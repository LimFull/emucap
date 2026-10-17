use super::*;
use crate::pc98_bridge::events::{PendingEvents, MAX_EVENT_BYTES};

#[test]
fn event_count_overflow_keeps_newest_and_reports_loss_once() {
    let fake = FakeGdb::with(&[
        ("?", "S05"),
        ("qEmucap,pollreset", "NONE"),
        ("qEmucap,pollreset", "NONE"),
    ]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    for index in 0u32..257 {
        bridge.note_stop(format!("T05reset:{:08x};", index.swap_bytes()), false);
    }
    assert_eq!(bridge.events.len(), 256);
    assert!(bridge.frozen);
    let result = bridge.poll_events(&json!({})).unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), 256);
    assert_eq!(result["events"][0]["pc"], 1);
    assert_eq!(result["events"][255]["pc"], 256);
    assert_eq!(result["dropped"], 1);
    assert_eq!(
        bridge.poll_events(&json!({})).unwrap(),
        json!({"events":[],"dropped":0})
    );
}

#[test]
fn event_bytes_account_for_escaping_and_filtered_retention() {
    let fake = FakeGdb::with(&[
        ("?", "S05"),
        ("qEmucap,pollreset", "NONE"),
        ("qEmucap,pollreset", "NONE"),
    ]);
    let mut bridge = Bridge::new(fake, GdbBridgeEnv::default());
    for index in 1u32..=3 {
        bridge.note_stop(
            format!(
                "T05reset:{:08x};pad:{}",
                index.swap_bytes(),
                "\u{1}".repeat(100_000)
            ),
            false,
        );
    }
    assert_eq!(
        bridge.events.len(),
        1,
        "escaped JSON bytes exceed the budget before the count cap"
    );
    let filtered = bridge.poll_events(&json!({"breakpoint_id":99})).unwrap();
    assert_eq!(filtered, json!({"events":[],"dropped":2}));
    assert_eq!(bridge.events.len(), 1);
    let result = bridge.poll_events(&json!({})).unwrap();
    assert_eq!(result["events"][0]["pc"], 3);
    assert_eq!(result["dropped"], 0);
}

#[test]
fn exact_event_byte_limit_and_single_oversize_preserve_backlog() {
    let mut pending = PendingEvents::default();
    let exact = Value::String("x".repeat(MAX_EVENT_BYTES - 2));
    pending.push(exact.clone());
    pending.push(Value::String("x".repeat(MAX_EVENT_BYTES - 1)));
    assert_eq!(pending.len(), 1);
    assert_eq!(pending.take_dropped(), 1);
    assert_eq!(pending.take_matching(None), vec![exact]);
    pending.push(json!({"id":1}));
    pending.push(json!({"id":2}));
    pending.push(json!({"id":1,"sequence":3}));
    assert_eq!(
        pending.take_matching(Some(1)),
        vec![json!({"id":1}), json!({"id":1,"sequence":3})]
    );
    assert_eq!(pending.take_matching(None), vec![json!({"id":2})]);
    assert_eq!(pending.take_dropped(), 0);
}

#[test]
fn enriched_snapshots_are_readmitted_before_filtered_retention() {
    let mut replies = vec![
        ("?".into(), "S05".into()),
        (
            format!("qEmucap,setpoint,{}", hex::encode("0|a0000|1|1|")),
            "BP:1".into(),
        ),
        ("qEmucap,pollreset".into(), "NONE".into()),
    ];
    replies.extend((0..60).map(|_| ("ma0000,4000".into(), "ab".repeat(0x4000))));
    replies.push(("qEmucap,pollreset".into(), "NONE".into()));
    let mut bridge = Bridge::new(FakeGdb::from_pairs(replies), GdbBridgeEnv::default());
    bridge
        .set_breakpoint(&json!({"memory_type":"tvram","start":0,"snapshot":["tvram:0:0x4000"]}))
        .unwrap();
    let regs = i386_regs_hex(&[("eip", 0x1234)]);
    for _ in 0..60 {
        bridge.note_stop(format!("T05hwbreak:00000a00;idx:1;regs:{regs};"), false);
    }
    let filtered = bridge.poll_events(&json!({"breakpoint_id":99})).unwrap();
    let event_bytes = serde_json::to_vec(&bridge.events[0]).unwrap().len();
    let retained = MAX_EVENT_BYTES / event_bytes;
    assert_eq!(bridge.events.len(), retained);
    assert!(retained < 60);
    assert_eq!(filtered, json!({"events":[],"dropped":60-retained}));
    let result = bridge.poll_events(&json!({"breakpoint_id":1})).unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), retained);
    assert_eq!(
        result["events"][0]["snapshot"][0]["hex"],
        "ab".repeat(0x4000)
    );
    assert_eq!(result["dropped"], 0);
    assert!(serde_json::to_vec(&result).unwrap().len() < 8 * 1024 * 1024);
}

struct EventGdb {
    scripted: FakeGdb,
    incoming: VecDeque<String>,
}
impl GdbTransport for EventGdb {
    fn send(&mut self, request: &str) -> GdbResult<String> {
        self.scripted.send(request)
    }
    fn send_with_timeout(&mut self, request: &str, budget: Duration) -> GdbResult<String> {
        self.scripted.send_with_timeout(request, budget)
    }
    fn recv_reply_with_timeout(&mut self, _: Duration) -> GdbResult<String> {
        Ok(self.incoming.pop_front().expect("unexpected receive"))
    }
    fn interrupt(&mut self) -> GdbResult<String> {
        self.scripted.interrupt()
    }
    fn send_no_reply(&mut self, request: &str) -> GdbResult<()> {
        self.scripted.send_no_reply(request)
    }
}

#[test]
fn stops_arriving_during_enrichment_survive_in_fifo_order() {
    let gdb = EventGdb {
        scripted: FakeGdb::with(&[
            ("?", "S05"),
            ("qEmucap,pollreset", "NONE"),
            ("g", "T05reset:02000000;"),
            ("qEmucap,pollreset", "NONE"),
        ]),
        incoming: VecDeque::from([i386_regs_hex(&[("eip", 0x1234)])]),
    };
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    bridge.note_stop("T05reset:01000000;".into(), false);
    bridge.note_stop("S06".into(), false);
    assert_eq!(
        bridge.poll_events(&json!({"breakpoint_id":99})).unwrap(),
        json!({"events":[],"dropped":0})
    );
    let events = bridge.poll_events(&json!({})).unwrap();
    let events = events["events"].as_array().unwrap();
    assert_eq!(events.len(), 3);
    assert_eq!(events[0]["pc"], 1);
    assert_eq!(events[1]["raw"], "S06");
    assert_eq!(events[2]["pc"], 2);
}

#[test]
fn reset_suppression_does_not_count_as_budget_loss() {
    let mut bridge = Bridge::new(
        FakeGdb::with(&[("?", "S05"), ("qEmucap,pollreset", "RESET:01000000|")]),
        GdbBridgeEnv::default(),
    );
    bridge.note_stop("S05".into(), false);
    let result = bridge.poll_events(&json!({})).unwrap();
    assert_eq!(result["events"].as_array().unwrap().len(), 1);
    assert_eq!(result["events"][0]["type"], "reset");
    assert_eq!(result["dropped"], 0);
}

#[test]
fn owned_exchange_streams_oversized_stops_through_admission() {
    let oversized = format!("T05reset:01000000;pad:{}", "x".repeat(MAX_EVENT_BYTES));
    let command = format!("qEmucap,framepoll,{}", hex::encode("owner"));
    let gdb = EventGdb {
        scripted: FakeGdb::from_pairs(vec![
            ("?".into(), "S05".into()),
            (command, oversized.clone()),
        ]),
        incoming: VecDeque::from([oversized, "T05reset:02000000;".into(), "terminal".into()]),
    };
    let mut bridge = Bridge::new(gdb, GdbBridgeEnv::default());
    bridge.frozen = false;
    assert_eq!(
        bridge.frame_exchange("framepoll", "owner").unwrap(),
        "terminal"
    );
    assert!(bridge.frozen);
    assert_eq!(bridge.events.len(), 1);
    assert_eq!(bridge.events[0]["pc"], 2);
    assert_eq!(bridge.events.take_dropped(), 2);
}
