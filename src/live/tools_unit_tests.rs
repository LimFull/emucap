use std::collections::VecDeque;

use super::{power_cycle, reset, watch_register, LinkError, ToolOutput};
use crate::live::link::{Capabilities, EmulatorLink, FakeLink};
use serde_json::json;

struct ResetReconnectLink {
    caps: Capabilities,
    status_results: VecDeque<Result<serde_json::Value, LinkError>>,
    prepared: bool,
    calls: Vec<String>,
    operation: &'static str,
    replacement_launch_id: Option<&'static str>,
}

impl EmulatorLink for ResetReconnectLink {
    fn capabilities(&self) -> &Capabilities {
        &self.caps
    }

    fn call(
        &mut self,
        method: &str,
        _params: serde_json::Value,
    ) -> Result<serde_json::Value, LinkError> {
        self.calls.push(method.to_string());
        match method {
            method if method == self.operation => Ok(json!({
                self.operation: true,
                "reconnect": true
            })),
            "status" => {
                assert!(self.prepared, "old transport must be discarded first");
                let result = self
                    .status_results
                    .pop_front()
                    .unwrap_or(Err(LinkError::NotConnected));
                if result.as_ref().ok().is_some_and(|status| {
                    status.get("connected").and_then(serde_json::Value::as_bool) == Some(true)
                }) {
                    if let Some(launch_id) = self.replacement_launch_id {
                        self.caps.identity.launch_id = Some(launch_id.into());
                    }
                }
                result
            }
            other => panic!("unexpected method: {other}"),
        }
    }

    fn prepare_reconnect(&mut self) {
        self.prepared = true;
    }
}

#[test]
fn reset_waits_for_replacement_session() {
    let mut link = ResetReconnectLink {
        caps: Capabilities::empty(),
        status_results: VecDeque::from([
            Err(LinkError::NotConnected),
            Ok(json!({"connected": true, "state": "running"})),
        ]),
        prepared: false,
        calls: Vec::new(),
        operation: "reset",
        replacement_launch_id: None,
    };

    let result = reset(&mut link).unwrap();
    assert_eq!(
        result,
        ToolOutput::Json(json!({
            "reset": true,
            "reconnected": true,
            "state": "running"
        }))
    );
    assert_eq!(link.calls, ["reset", "status", "status"]);
}

#[test]
fn power_cycle_waits_for_replacement_session() {
    let mut link = ResetReconnectLink {
        caps: Capabilities::empty(),
        status_results: VecDeque::from([
            Err(LinkError::NotConnected),
            Ok(json!({"connected": true, "state": "running"})),
        ]),
        prepared: false,
        calls: Vec::new(),
        operation: "power_cycle",
        replacement_launch_id: None,
    };

    let result = power_cycle(&mut link).unwrap();
    assert_eq!(
        result,
        ToolOutput::Json(json!({
            "power_cycle": true,
            "reconnected": true,
            "state": "running"
        }))
    );
    assert_eq!(link.calls, ["power_cycle", "status", "status"]);
}

#[test]
fn reconnecting_operation_rejects_a_replacement_launch_generation() {
    let mut caps = Capabilities::empty();
    caps.identity.launch_id = Some("launch-original".into());
    let mut link = ResetReconnectLink {
        caps,
        status_results: VecDeque::from([Ok(json!({
            "connected": true,
            "state": "running"
        }))]),
        prepared: false,
        calls: Vec::new(),
        operation: "power_cycle",
        replacement_launch_id: Some("launch-replacement"),
    };

    let error = power_cycle(&mut link).unwrap_err();
    assert!(matches!(error, LinkError::Emulator { ref kind, .. } if kind == "identity_changed"));
}

#[test]
fn reset_without_reconnect_marker_stays_single_call() {
    let mut link = FakeLink::ok(json!({"reset": true}));
    assert_eq!(
        reset(&mut link).unwrap(),
        ToolOutput::Json(json!({"reset": true}))
    );
    assert_eq!(link.last_method.as_deref(), Some("reset"));
}

#[test]
fn watch_register_rejects_over_budget() {
    // 과대 max_instructions는 매 명령 getState 플러드를 오래 돌려 emu 스레드를 굶긴다 — 실행 전 거부.
    let mut link = FakeLink::ok(json!({ "id": 1 }));
    let r = watch_register(&mut link, "sp", 0, 0xffff, true, Some(u64::MAX));
    assert!(
        matches!(r, Err(LinkError::Emulator { ref kind, .. }) if kind == "bad_params"),
        "과대 max_instructions는 bad_params로 거부해야: {r:?}"
    );
    let mut link2 = FakeLink::ok(json!({ "id": 1 }));
    assert!(
        watch_register(&mut link2, "sp", 0, 0xffff, true, Some(1000)).is_ok(),
        "상한 이내 예산은 통과해야"
    );
}

#[test]
fn dump_memory_refuses_file_destination() {
    // 요청 경로가 일반 파일이면 브리지 덤프·스테이징 전에 거부하고 파일을 보존한다(fail-fast) —
    // 어댑터를 호출하지 않는다.
    let tmp = tempfile::tempdir().unwrap();
    let dst = tmp.path().join("dump");
    std::fs::write(&dst, b"user-file").unwrap();
    let mut link = FakeLink::ok(json!({}));
    let err = super::dump_memory(&mut link, dst.to_str().unwrap()).unwrap_err();
    assert!(
        matches!(err, LinkError::Protocol(_)),
        "가드는 Protocol 에러"
    );
    assert_eq!(
        std::fs::read(&dst).unwrap(),
        b"user-file",
        "거부 시 사용자 파일은 그대로여야"
    );
    assert!(
        link.last_method.is_none(),
        "가드가 브리지 dump_memory 호출 전에 거부해야"
    );
}
