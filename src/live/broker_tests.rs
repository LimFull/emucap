use super::broker;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::time::Duration;

const TEST_SESSION_TOKEN: &str = "broker-test-session-token";
const TEST_PRODUCER_LAUNCH: &str = "launch-01arz3ndektsv4rrffq69g5fav";

fn test_launch_id(label: &str) -> String {
    let mut suffix: String = label
        .bytes()
        .map(|byte| match byte.to_ascii_lowercase() {
            byte @ (b'0'..=b'9'
            | b'a'..=b'h'
            | b'j'
            | b'k'
            | b'm'
            | b'n'
            | b'p'..=b't'
            | b'v'..=b'z') => char::from(byte),
            _ => '0',
        })
        .take(26)
        .collect();
    suffix.extend(std::iter::repeat_n('0', 26 - suffix.len()));
    format!("launch-{suffix}")
}

fn connect_producer_as(addr: impl std::net::ToSocketAddrs, launch_id: &str) -> TcpStream {
    let stream = TcpStream::connect(addr).unwrap();
    let port = stream.peer_addr().unwrap().port();
    let root = broker::producer_token_from_controller(TEST_SESSION_TOKEN);
    let front_key = super::producer_auth::derive_front_key(
        &root,
        super::producer_auth::Boundary::BrokerProducer,
        port,
        launch_id,
    )
    .unwrap();
    super::producer_auth::authenticate_producer_stream(
        stream,
        &front_key,
        super::producer_auth::Boundary::BrokerProducer,
        port,
        launch_id,
        Duration::from_secs(2),
    )
    .unwrap()
}

fn connect_producer(addr: impl std::net::ToSocketAddrs) -> TcpStream {
    connect_producer_as(addr, TEST_PRODUCER_LAUNCH)
}

#[test]
fn broker_readiness_preserves_an_attached_controller() {
    use super::link::EmulatorLink;
    let (ea, sa) = start_broker();
    let emulator = fake_emu(ea, Some("ready-owner".into()));
    std::thread::sleep(Duration::from_millis(100));
    let mut owner = super::broker_link::connect_with_token(
        &sa,
        Some("ready-owner".into()),
        TEST_SESSION_TOKEN,
        Duration::from_secs(1),
    )
    .unwrap();
    super::broker_link::probe_session_with_token(&sa, TEST_SESSION_TOKEN, Duration::from_secs(1))
        .unwrap();
    assert_eq!(
        owner.call("status", serde_json::json!({})).unwrap()["connected"],
        true
    );
    emulator.join().unwrap();
}

#[test]
fn producer_proofs_cannot_enumerate_attach_or_replace_controller() {
    let (ea, sa) = start_broker();
    let mut producer = controlled_producer(&ea, Some("role-target"), "role-generation");
    producer
        .set_read_timeout(Some(Duration::from_millis(100)))
        .unwrap();
    let token = broker::producer_token_from_controller(TEST_SESSION_TOKEN);
    assert_ne!(token, TEST_SESSION_TOKEN);
    assert!(!broker::generated_session_token_is_current(&token));
    for mut params in [
        serde_json::json!({}),
        serde_json::json!({"name":"role-target"}),
        serde_json::json!({"name":"role-target","expected_registration_id":1,"expected_launch_id":"role-generation"}),
    ] {
        let mut writer = TcpStream::connect(&sa).unwrap();
        writer
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut reader = BufReader::new(writer.try_clone().unwrap());
        let (client, server, proof) = begin_session_auth(&mut writer, &mut reader);
        assert!(!broker::auth_proof_matches(
            &broker::session_server_proof(&token, &client, &server),
            &proof
        ));
        let binding = broker::session_attach_binding(params.as_object().unwrap());
        params["auth_proof"] =
            broker::session_client_proof(&token, &client, &server, &binding).into();
        writeln!(
            writer,
            "{}",
            serde_json::json!({"v":1,"id":1,"method":"attach","params":params})
        )
        .unwrap();
        let mut reply = String::new();
        reader.read_line(&mut reply).unwrap();
        let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
        assert_eq!(reply["error"]["kind"], "identity_mismatch");
        assert!(reply.get("result").is_none());
    }
    let mut byte = [0];
    assert!(
        matches!(producer.read(&mut byte), Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
    );
}

#[test]
fn controller_secret_is_not_a_producer_bearer() {
    let (ea, sa) = start_broker();
    let mut rejected = submit_producer_hello(
        &ea,
        serde_json::json!({
            "id":0,"ok":true,"result":{
                "protocol_version":1,"session_token":TEST_SESSION_TOKEN,
                "name":"wrong-role","methods":["status"]
            }
        }),
    );
    rejected
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(rejected.read(&mut [0]).unwrap(), 0);
    let (_, reply) = attach(&sa, Some("wrong-role"));
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["error"]["kind"], "no_such_emulator");
}

#[test]
fn producer_visible_credential_cannot_attach_to_another_emulator() {
    use super::link::{EmulatorLink, LinkError};
    let (ea, sa) = start_broker();
    let _source = controlled_producer(&ea, Some("role-source"), "source-generation");
    let _target = controlled_producer(&ea, Some("role-target"), "target-generation");
    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    let source_controller = loop {
        match super::broker_link::connect_with_token(
            &sa,
            Some("role-source".into()),
            TEST_SESSION_TOKEN,
            Duration::from_secs(1),
        ) {
            Ok(link) => break link,
            Err(LinkError::NoSuchEmulator { .. }) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(5));
            }
            result => panic!("source controller failed: {result:?}"),
        }
    };
    let exposed = source_controller.session_token().unwrap();
    let attack = super::broker_link::connect_with_token(
        &sa,
        Some("role-target".into()),
        exposed,
        Duration::from_secs(1),
    );
    assert!(
        matches!(attack, Err(LinkError::IdentityMismatch { .. })),
        "producer role escalated or returned an unexpected error: {attack:?}"
    );
    let target = super::broker_link::connect_with_token(
        &sa,
        Some("role-target".into()),
        TEST_SESSION_TOKEN,
        Duration::from_secs(1),
    )
    .unwrap();
    assert!(target.has_exclusive_control());
}

// Start the broker on two ephemeral ports and return (emulator_addr, session_addr).
fn start_broker() -> (String, String) {
    let emu = TcpListener::bind("127.0.0.1:0").unwrap();
    let sess = TcpListener::bind("127.0.0.1:0").unwrap();
    let ea = emu.local_addr().unwrap().to_string();
    let sa = sess.local_addr().unwrap().to_string();
    std::thread::spawn(move || broker::serve(emu, sess, TEST_SESSION_TOKEN.to_string()));
    (ea, sa)
}

fn begin_session_auth(
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
) -> (String, String, String) {
    let client_nonce = broker::new_auth_nonce();
    writeln!(
        writer,
        "{}",
        serde_json::json!({
            "v": 1,
            "id": 0,
            "method": "broker_authenticate",
            "params": {"protocol_version": 1, "client_nonce": client_nonce.clone()},
        })
    )
    .unwrap();
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    let response: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    assert_eq!(response["id"], 0);
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["protocol_version"], 1);
    let server_nonce = response["result"]["server_nonce"]
        .as_str()
        .unwrap()
        .to_string();
    let server_proof = response["result"]["server_proof"]
        .as_str()
        .unwrap()
        .to_string();
    (client_nonce, server_nonce, server_proof)
}

fn authenticated_attach(
    writer: &mut TcpStream,
    reader: &mut BufReader<TcpStream>,
    mut params: serde_json::Value,
) -> String {
    let (client_nonce, server_nonce, server_proof) = begin_session_auth(writer, reader);
    let expected_server =
        broker::session_server_proof(TEST_SESSION_TOKEN, &client_nonce, &server_nonce);
    assert!(broker::auth_proof_matches(&expected_server, &server_proof));
    let params = params.as_object_mut().expect("broker attach params object");
    let binding = broker::session_attach_binding(params);
    params.insert(
        "auth_proof".into(),
        serde_json::Value::String(broker::session_client_proof(
            TEST_SESSION_TOKEN,
            &client_nonce,
            &server_nonce,
            &binding,
        )),
    );
    writeln!(
        writer,
        "{}",
        serde_json::json!({"v": 1, "id": 1, "method": "attach", "params": params})
    )
    .unwrap();
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    response
}

// 가짜 에뮬레이터: 접속→hello 응답(name, methods)→이후 명령 echo 응답.
fn fake_emu(addr: String, name: Option<String>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let s = connect_producer(addr);
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut hello = String::new();
        r.read_line(&mut hello).unwrap(); // hello
        let nm = name
            .map(|n| format!(r#","name":"{n}""#))
            .unwrap_or_default();
        writeln!(
            w,
            r#"{{"id":0,"ok":true,"result":{{"protocol_version":1,"methods":["status"]{nm}}}}}"#
        )
        .unwrap();
        // 명령 하나: status → working keepalive + completed(양방향 펌프 검증)
        let mut cmd = String::new();
        if r.read_line(&mut cmd).unwrap() > 0 {
            let id = serde_json::from_str::<serde_json::Value>(cmd.trim()).unwrap()["id"].clone();
            writeln!(
                w,
                r#"{{"id":{id},"ok":true,"result":{{"status":"working"}}}}"#
            )
            .unwrap();
            writeln!(
                w,
                r#"{{"id":{id},"ok":true,"result":{{"connected":true}}}}"#
            )
            .unwrap();
        }
        std::thread::sleep(Duration::from_millis(200));
    })
}

fn fake_mesen_emu(
    addr: String,
    name: &str,
    host_features: &[&str],
    hold_ms: u64,
) -> std::thread::JoinHandle<()> {
    let name = name.to_string();
    let host_features: Vec<String> = host_features
        .iter()
        .map(|value| value.to_string())
        .collect();
    std::thread::spawn(move || {
        let stream = connect_producer(addr);
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        writeln!(
            writer,
            "{}",
            serde_json::json!({
                "id": 0,
                "ok": true,
                "result": {
                    "protocol_version": 1,
                    "methods": ["status", "record_window", "abort_recording"],
                    "name": name,
                    "adapter": "mesen2-live",
                    "mesen_host_api": 1,
                    "host_features": host_features,
                    "memory_types": ["workram"],
                    "memory_regions": [{"memory_type": "workram", "size": 131072}],
                    "breakpoint_kinds": [{
                        "kind": "device_boundary",
                        "range_unit": "scanline",
                        "memory_type_used": false,
                        "snapshot": true
                    }],
                    "host_build": {
                        "upstream_commit": "0123456789abcdef0123456789abcdef01234567",
                        "patchset_sha256": "abcdef0123456789abcdef0123456789abcdef0123456789abcdef0123456789"
                    },
                    "recording": {
                        "revision": super::recording_capability::INITIAL_RECORDING_CAPABILITY_REVISION,
                        "origins": ["next_frame_boundary"],
                        "units": ["frames"],
                        "default_event_classes": ["frame_boundary"],
                        "event_classes": [{
                            "id": "frame_boundary",
                            "contract_sha256": "498fcd52f2fa2327e0af9e9730b4314f0854a6047f57dcde16961b8a4ecb80cd",
                            "clock_domains": ["frame"],
                            "exact": true
                        }],
                        "limits": {
                            "max_frames": 300,
                            "max_events": 100000,
                            "max_bytes": 67108864,
                            "max_line_bytes": 65536,
                            "max_host_ms": 30000,
                            "progress_interval_ms": 250
                        }
                    }
                }
            })
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(hold_ms));
    })
}

#[test]
fn broker_routes_and_pumps_keepalive() {
    let (ea, sa) = start_broker();
    let h = fake_emu(ea, None);
    std::thread::sleep(Duration::from_millis(100)); // 등록 여유

    // 세션: attach(이름 없음, 단일) → status → working 건너뛰고 completed 받기
    let s = TcpStream::connect(sa).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s;
    let ar = authenticated_attach(&mut w, &mut r, serde_json::json!({}));
    assert!(ar.contains("attached_name"), "attach 응답: {ar}");

    writeln!(w, r#"{{"v":1,"id":2,"method":"status","params":{{}}}}"#).unwrap();
    // working + completed 두 줄이 와야 함(양방향 펌프)
    let mut l1 = String::new();
    r.read_line(&mut l1).unwrap();
    let mut l2 = String::new();
    r.read_line(&mut l2).unwrap();
    assert!(l1.contains("working"), "첫 줄 working: {l1}");
    assert!(l2.contains("connected"), "둘째 줄 completed: {l2}");
    h.join().unwrap();
}

#[test]
fn broker_attach_preserves_contract_advertisement() {
    let (emulator_addr, session_addr) = start_broker();
    let emulator = std::thread::spawn(move || {
        let stream = connect_producer(emulator_addr);
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        writeln!(
            writer,
            "{}",
            serde_json::json!({
                "id": 0,
                "ok": true,
                "result": {
                    "protocol_version": 1,
                    "name": "nds-contracts",
                    "adapter": "desmume-nds-rust-gdb",
                    "system": "nds",
                    "methods": ["status", "step", "step_instructions", "call_stack", "cancel_operation"],
                    "temporal_cancellation_capability": {"methods":["step"],"control_service_ms":25,"stop_host_ms":1000},
                    "producer_extension": {"nested":[1,"two",{"three":true}]},
                    "attached_name":"producer-forged-route",
                    "broker_registration_id":999999,
                    "contracts": crate::contracts::advertisement_value(&[
                        "nds.execution.frame-step-vblank",
                        "nds.call-stack.best-effort",
                    ]),
                }
            })
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
    });
    std::thread::sleep(Duration::from_millis(100));

    let stream = TcpStream::connect(session_addr).unwrap();
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut writer = stream;
    let response = authenticated_attach(
        &mut writer,
        &mut reader,
        serde_json::json!({"name": "nds-contracts"}),
    );
    let value: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    assert_eq!(
        value["result"]["contracts"]["catalog"],
        crate::contracts::CATALOG_ID
    );
    assert_eq!(
        value["result"]["contracts"]["active_exceptions"],
        serde_json::json!([
            "nds.execution.frame-step-vblank",
            "nds.call-stack.best-effort"
        ])
    );
    assert_eq!(
        value["result"]["temporal_cancellation_capability"],
        serde_json::json!({"methods":["step"],"control_service_ms":25,"stop_host_ms":1000})
    );
    assert_eq!(
        value["result"]["producer_extension"],
        serde_json::json!({"nested":[1,"two",{"three":true}]})
    );
    assert_eq!(value["result"]["attached_name"], "nds-contracts");
    assert_ne!(value["result"]["broker_registration_id"], 999999);
    assert!(value["result"].get("session_token").is_none());
    emulator.join().unwrap();
}

#[test]
fn broker_rejects_mesen_without_native_halt_features() {
    let (emu_addr, session_addr) = start_broker();
    let emulator = fake_mesen_emu(emu_addr, "unpatched", &["code_break_idle"], 200);
    std::thread::sleep(Duration::from_millis(100));

    let (_session, response) = attach(&session_addr, Some("unpatched"));
    assert!(
        response.contains("no_such_emulator"),
        "incompatible Mesen must not enter the broker registry: {response}"
    );
    emulator.join().unwrap();
}

#[test]
fn broker_forwards_mesen_native_halt_identity() {
    let (emu_addr, session_addr) = start_broker();
    let emulator = fake_mesen_emu(
        emu_addr,
        "patched",
        &["code_break_idle", "native_halt_service"],
        500,
    );
    std::thread::sleep(Duration::from_millis(100));

    let (_session, response) = attach(&session_addr, Some("patched"));
    let response: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    let result = &response["result"];
    assert_eq!(result["mesen_host_api"], 1);
    assert_eq!(
        result["host_features"],
        serde_json::json!(["code_break_idle", "native_halt_service"])
    );
    assert_eq!(
        result["memory_regions"],
        serde_json::json!([{"memory_type": "workram", "size": 131072}])
    );
    assert_eq!(
        result["breakpoint_kinds"],
        serde_json::json!([{
            "kind": "device_boundary",
            "range_unit": "scanline",
            "memory_type_used": false,
            "snapshot": true
        }])
    );
    assert_eq!(
        result["host_build"]["upstream_commit"],
        "0123456789abcdef0123456789abcdef01234567"
    );
    assert_eq!(
        result["recording"]["default_event_classes"],
        serde_json::json!(["frame_boundary"])
    );
    emulator.join().unwrap();
}

#[test]
fn broker_atomic_two_port_bind() {
    // 한 포트를 미리 점유 → 같은 포트 이중 바인드가 AddrInUse임을 확인(bind-as-lock 전제).
    // emucap-broker는 에뮬레이터 포트를 먼저 바인드하고, 세션 포트 실패 시 에뮬레이터 포트를
    // drop해 해제한다(부분 점유 없음). 이 테스트는 OS 레벨 guard가 작동하는 전제를 검증.
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap();
    let second = std::net::TcpListener::bind(addr);
    assert!(
        second.is_err(),
        "이중 바인드는 실패해야 bind-as-lock 선출이 성립"
    );
}

fn attach(sa: &str, name: Option<&str>) -> (TcpStream, String) {
    let params = name
        .map(|name| serde_json::json!({"name": name}))
        .unwrap_or_else(|| serde_json::json!({}));
    attach_with_params(sa, params)
}

fn attach_with_params(sa: &str, params: serde_json::Value) -> (TcpStream, String) {
    let s = TcpStream::connect(sa).unwrap();
    let mut r = BufReader::new(s.try_clone().unwrap());
    let mut w = s.try_clone().unwrap();
    let ar = authenticated_attach(&mut w, &mut r, params);
    (s, ar)
}

fn submit_producer_hello(ea: &str, response: serde_json::Value) -> TcpStream {
    let advertised_launch = response
        .get("result")
        .and_then(|result| result.get("launch_id"))
        .and_then(serde_json::Value::as_str)
        .filter(|launch_id| {
            super::producer_auth::derive_front_key(
                "test",
                super::producer_auth::Boundary::BrokerProducer,
                1,
                launch_id,
            )
            .is_ok()
        })
        .unwrap_or(TEST_PRODUCER_LAUNCH);
    let mut stream = connect_producer_as(ea, advertised_launch);
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut hello = String::new();
    reader.read_line(&mut hello).unwrap();
    let hello: serde_json::Value = serde_json::from_str(hello.trim()).unwrap();
    assert_eq!(hello["id"], 0);
    assert_eq!(hello["method"], "hello");
    assert_eq!(hello["params"], serde_json::json!({}));
    writeln!(stream, "{response}").unwrap();
    stream
}

fn controlled_producer(ea: &str, name: Option<&str>, launch_id: &str) -> TcpStream {
    let launch_id = test_launch_id(launch_id);
    let mut result = serde_json::json!({
        "protocol_version": 1,
        "methods": ["status"], "launch_id": launch_id,
    });
    if let Some(name) = name {
        result["name"] = serde_json::json!(name);
    }
    submit_producer_hello(ea, serde_json::json!({"id":0,"ok":true,"result":result}))
}

fn disconnect_producer(producer: &TcpStream, session: &TcpStream) {
    session
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    producer.shutdown(std::net::Shutdown::Both).unwrap();
    let mut notice = String::new();
    BufReader::new(session.try_clone().unwrap())
        .read_line(&mut notice)
        .unwrap();
    assert!(
        notice.contains("emulator gone"),
        "EOF cleanup must finish: {notice}"
    );
}

#[test]
fn broker_authenticated_duplicate_cannot_evict_owner() {
    let (ea, sa) = start_broker();
    let mut original = controlled_producer(&ea, Some("g"), "launch-original");
    std::thread::sleep(Duration::from_millis(100));
    let (mut session, first) = attach(&sa, Some("g"));
    let first: serde_json::Value = serde_json::from_str(&first).unwrap();
    for launch in ["launch-attacker", "launch-original"] {
        let mut candidate = controlled_producer(&ea, Some("g"), launch);
        candidate
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut byte = [0];
        match candidate.read(&mut byte) {
            Ok(0) => (),
            Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => (),
            other => panic!("duplicate must close without replacing owner: {other:?}"),
        }
    }
    session
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    original
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    writeln!(
        session,
        "{}",
        serde_json::json!({"v":1,"id":42,"method":"status","params":{}})
    )
    .unwrap();
    let mut command = String::new();
    BufReader::new(original.try_clone().unwrap())
        .read_line(&mut command)
        .unwrap();
    let command: serde_json::Value = serde_json::from_str(&command).unwrap();
    writeln!(
        original,
        "{}",
        serde_json::json!({"id":command["id"],"ok":true,"result":{"owner":true}})
    )
    .unwrap();
    let mut reply = String::new();
    BufReader::new(session.try_clone().unwrap())
        .read_line(&mut reply)
        .unwrap();
    let reply: serde_json::Value = serde_json::from_str(&reply).unwrap();
    assert_eq!(reply["id"], 42);
    assert_eq!(reply["result"]["owner"], true);
    let (_, resumed) = attach_with_params(
        &sa,
        serde_json::json!({"name":"g", "expected_registration_id":first["result"]["broker_registration_id"]}),
    );
    let resumed: serde_json::Value = serde_json::from_str(&resumed).unwrap();
    assert_eq!(
        resumed["result"]["broker_registration_id"],
        first["result"]["broker_registration_id"]
    );
}

#[test]
fn broker_anonymous_registration_skips_named_slots() {
    let (ea, sa) = start_broker();
    let _named = controlled_producer(&ea, Some("emu1"), "named");
    std::thread::sleep(Duration::from_millis(100));
    let (_owner, first) = attach(&sa, Some("emu1"));
    let _anonymous = controlled_producer(&ea, None, "anonymous");
    std::thread::sleep(Duration::from_millis(100));
    let (_, second) = attach(&sa, Some("emu2"));
    let second: serde_json::Value = serde_json::from_str(&second).unwrap();
    assert_eq!(second["result"]["launch_id"], test_launch_id("anonymous"));
    assert!(first.contains("named"));
    let (_, busy) = attach(&sa, Some("emu1"));
    assert!(busy.contains("busy"));
}

#[test]
fn broker_concurrent_duplicate_admission_has_one_owner() {
    let (ea, sa) = start_broker();
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let attempts: Vec<_> = ["first", "second"]
        .into_iter()
        .map(|launch| {
            let ea = ea.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                (launch, controlled_producer(&ea, Some("g"), launch))
            })
        })
        .collect();
    let candidates: Vec<_> = attempts
        .into_iter()
        .map(|attempt| attempt.join().unwrap())
        .collect();
    std::thread::sleep(Duration::from_millis(100));
    let (_session, response) = attach(&sa, Some("g"));
    let response: serde_json::Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["ok"], true);
    let winner = response["result"]["launch_id"].as_str().unwrap();
    assert!([test_launch_id("first"), test_launch_id("second")]
        .iter()
        .any(|launch_id| launch_id == winner));
    for (launch, mut socket) in candidates {
        if test_launch_id(launch) != winner {
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            match socket.read(&mut [0]) {
                Ok(0) => (),
                Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => (),
                other => panic!("losing registration stayed open: {other:?}"),
            }
        }
    }
}

#[test]
fn broker_rejects_unauthenticated_same_name_producer_without_replacement() {
    let (ea, sa) = start_broker();
    let _legitimate = fake_emu_alive(ea.clone(), "g", 3000);
    std::thread::sleep(Duration::from_millis(100));

    let attacker = TcpStream::connect(&ea).unwrap();
    let port = attacker.peer_addr().unwrap().port();
    let wrong_front = super::producer_auth::derive_front_key(
        "wrong-producer-root",
        super::producer_auth::Boundary::BrokerProducer,
        port,
        TEST_PRODUCER_LAUNCH,
    )
    .unwrap();
    assert!(super::producer_auth::authenticate_producer_stream(
        attacker,
        &wrong_front,
        super::producer_auth::Boundary::BrokerProducer,
        port,
        TEST_PRODUCER_LAUNCH,
        Duration::from_secs(1),
    )
    .is_err());
    std::thread::sleep(Duration::from_millis(100));

    let (_session, response) = attach(&sa, Some("g"));
    let response: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["methods"], serde_json::json!(["status"]));
}

#[test]
fn broker_rejects_malformed_producer_envelopes_before_registration() {
    let (ea, sa) = start_broker();
    let _legitimate = fake_emu_alive(ea.clone(), "g", 3000);
    std::thread::sleep(Duration::from_millis(100));

    let malformed = [
        serde_json::json!({
            "id": 1,
            "ok": true,
            "result": {"protocol_version": 1, "session_token": broker::producer_token_from_controller(TEST_SESSION_TOKEN), "name": "g", "methods": ["wrong_id"]},
        }),
        serde_json::json!({
            "id": 0,
            "ok": false,
            "result": {"protocol_version": 1, "session_token": broker::producer_token_from_controller(TEST_SESSION_TOKEN), "name": "g", "methods": ["not_ok"]},
        }),
        serde_json::json!({"id": 0, "ok": true, "result": [TEST_SESSION_TOKEN]}),
        serde_json::json!({
            "id": 0,
            "ok": true,
            "result": {"protocol_version": 2, "session_token": broker::producer_token_from_controller(TEST_SESSION_TOKEN), "name": "g", "methods": ["wrong_protocol"]},
        }),
    ];
    let _invalid_connections: Vec<_> = malformed
        .into_iter()
        .map(|response| submit_producer_hello(&ea, response))
        .collect();
    std::thread::sleep(Duration::from_millis(100));

    let (_session, response) = attach(&sa, Some("g"));
    let response: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    assert_eq!(response["ok"], true);
    assert_eq!(response["result"]["methods"], serde_json::json!(["status"]));
}

#[test]
fn broker_rejects_unauthenticated_registration_takeover_without_name_disclosure() {
    let (ea, sa) = start_broker();
    let _emulator = fake_emu_alive(ea, "g", 3000);
    std::thread::sleep(Duration::from_millis(100));
    let (mut legitimate, attached) = attach(&sa, Some("g"));
    let attached: serde_json::Value = serde_json::from_str(attached.trim()).unwrap();
    let registration = attached["result"]["broker_registration_id"]
        .as_u64()
        .unwrap();

    let mut attacker = TcpStream::connect(sa).unwrap();
    let mut attacker_reader = BufReader::new(attacker.try_clone().unwrap());
    let (_client_nonce, _server_nonce, _server_proof) =
        begin_session_auth(&mut attacker, &mut attacker_reader);
    writeln!(
        attacker,
        "{}",
        serde_json::json!({
            "v": 1,
            "id": 1,
            "method": "attach",
            "params": {
                "name": "g",
                "expected_registration_id": registration,
                "auth_proof": "wrong-proof",
            },
        })
    )
    .unwrap();
    let mut rejected = String::new();
    attacker_reader.read_line(&mut rejected).unwrap();
    let rejected: serde_json::Value = serde_json::from_str(rejected.trim()).unwrap();
    assert_eq!(rejected["error"]["kind"], "identity_mismatch");
    assert!(rejected["error"].get("name").is_none());
    assert!(rejected["error"].get("names").is_none());

    legitimate
        .set_read_timeout(Some(Duration::from_millis(150)))
        .unwrap();
    let mut byte = [0u8; 1];
    let still_open = matches!(
        legitimate.read(&mut byte),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            )
    );
    assert!(
        still_open,
        "unauthenticated exact-registration attach replaced the owner"
    );
}

#[test]
fn broker_times_out_silent_admission_connections() {
    let (ea, sa) = start_broker();
    let producer = TcpStream::connect(ea).unwrap();
    let mut producer_reader = BufReader::new(producer);
    let mut challenge = [0u8; 25];
    producer_reader.read_exact(&mut challenge).unwrap();
    assert_eq!(&challenge[..4], b"ECFA");

    let session = TcpStream::connect(sa).unwrap();
    let mut session_reader = BufReader::new(session);
    std::thread::sleep(Duration::from_millis(5300));
    producer_reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    session_reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut line = String::new();
    assert_eq!(producer_reader.read_line(&mut line).unwrap(), 0);
    assert_eq!(session_reader.read_line(&mut line).unwrap(), 0);
}

#[test]
fn broker_admission_deadline_cannot_be_extended_by_slow_drip() {
    let (ea, sa) = start_broker();
    let producer = TcpStream::connect(ea).unwrap();
    producer
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut producer_reader = BufReader::new(producer.try_clone().unwrap());
    let mut challenge = [0u8; 25];
    producer_reader.read_exact(&mut challenge).unwrap();
    assert_eq!(&challenge[..4], b"ECFA");
    let session = TcpStream::connect(sa).unwrap();
    session
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut session_reader = BufReader::new(session.try_clone().unwrap());

    let producer_drip = std::thread::spawn(move || {
        let mut producer = producer;
        for _ in 0..8 {
            if producer.write_all(b"{").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(750));
        }
    });
    let session_drip = std::thread::spawn(move || {
        let mut session = session;
        for _ in 0..8 {
            if session.write_all(b"{").is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(750));
        }
    });

    std::thread::sleep(Duration::from_millis(5300));
    // A peer still writing at the deadline may observe FIN or RST. Both prove
    // admission ended; a timeout or another byte does not.
    for reader in [&mut producer_reader, &mut session_reader] {
        let result = reader.read_line(&mut String::new());
        assert!(
            matches!(&result, Ok(0))
                || matches!(&result, Err(error) if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                )),
            "admission did not close: {result:?}"
        );
    }
    producer_drip.join().unwrap();
    session_drip.join().unwrap();
}

#[test]
fn broker_admits_controller_behind_full_silent_producer_queue() {
    let (ea, sa) = start_broker();
    let mut held = Vec::new();
    for _ in 0..broker::MAX_PENDING_ADMISSIONS {
        let stream = TcpStream::connect(&ea).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut challenge = [0u8; 25];
        reader.read_exact(&mut challenge).unwrap();
        assert_eq!(&challenge[..4], b"ECFA");
        held.push(reader);
    }
    super::broker_link::probe_session_with_token(
        &sa,
        TEST_SESSION_TOKEN,
        Duration::from_millis(500),
    )
    .expect("silent producers must not prevent controller authentication");
}

#[test]
fn broker_admits_producer_behind_full_silent_producer_queue() {
    let (ea, sa) = start_broker();
    let mut held = Vec::new();
    for _ in 0..broker::MAX_PENDING_ADMISSIONS {
        let stream = TcpStream::connect(&ea).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut reader = BufReader::new(stream);
        let mut challenge = [0u8; 25];
        reader.read_exact(&mut challenge).unwrap();
        assert_eq!(&challenge[..4], b"ECFA");
        held.push(reader);
    }
    let mut producer = connect_producer(ea);
    producer
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut hello = String::new();
    BufReader::new(producer.try_clone().unwrap())
        .read_line(&mut hello)
        .unwrap();
    assert!(hello.contains("hello"));
    writeln!(
        producer,
        "{}",
        serde_json::json!({"id":0,"ok":true,"result":{
            "protocol_version":1,
            "name":"after-silent","methods":["status"]
        }})
    )
    .unwrap();
    let _owner = connect_registered(&sa, "after-silent");
    assert_eq!(
        held[0].read(&mut [0]).unwrap(),
        0,
        "oldest silent peer was not evicted"
    );
}

fn connect_registered(sa: &str, name: &str) -> super::broker_link::BrokerLink {
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        match super::broker_link::connect_with_token(
            sa,
            Some(name.into()),
            TEST_SESSION_TOKEN,
            Duration::from_secs(1),
        ) {
            Ok(link) => return link,
            Err(super::link::LinkError::NoSuchEmulator { .. })
                if std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(Duration::from_millis(5));
            }
            result => panic!("authenticated attachment failed: {result:?}"),
        }
    }
}

#[test]
fn broker_admits_both_roles_behind_full_controller_queue_and_preserves_owner() {
    use super::link::EmulatorLink;
    let (ea, sa) = start_broker();
    let existing = fake_emu(ea.clone(), Some("existing-owner".into()));
    let mut owner = connect_registered(&sa, "existing-owner");
    let mut held = Vec::new();
    for _ in 0..broker::MAX_PENDING_ADMISSIONS {
        let mut stream = TcpStream::connect(&sa).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        begin_session_auth(&mut stream, &mut reader);
        held.push(stream);
    }
    let new_producer = fake_emu(ea, Some("new-owner".into()));
    let mut new_owner = connect_registered(&sa, "new-owner");
    assert_eq!(
        new_owner.call("status", serde_json::json!({})).unwrap()["connected"],
        true
    );
    assert_eq!(
        owner.call("status", serde_json::json!({})).unwrap()["connected"],
        true
    );
    assert_eq!(held[0].read(&mut [0]).unwrap(), 0);
    existing.join().unwrap();
    new_producer.join().unwrap();
}

#[test]
fn broker_progress_and_established_traffic_survive_bounded_invalid_churn() {
    use super::link::EmulatorLink;
    let (ea, sa) = start_broker();
    let existing = fake_emu(ea.clone(), Some("churn-owner".into()));
    let mut owner = connect_registered(&sa, "churn-owner");
    let (started, ready) = std::sync::mpsc::channel();
    let churn_ea = ea.clone();
    let churn_sa = sa.clone();
    let churn = std::thread::spawn(move || {
        let mut held = Vec::new();
        for index in 0..96 {
            for addr in [&churn_ea, &churn_sa] {
                let mut stream = TcpStream::connect(addr).unwrap();
                stream
                    .write_all(if index % 2 == 0 {
                        b"{partial"
                    } else {
                        b"invalid\n"
                    })
                    .unwrap();
                held.push(stream);
            }
            if index == 32 {
                started.send(()).unwrap();
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    });
    ready.recv_timeout(Duration::from_secs(2)).unwrap();
    let new_producer = fake_emu(ea, Some("during-churn".into()));
    let mut new_owner = connect_registered(&sa, "during-churn");
    assert_eq!(
        new_owner.call("status", serde_json::json!({})).unwrap()["connected"],
        true
    );
    assert_eq!(
        owner.call("status", serde_json::json!({})).unwrap()["connected"],
        true
    );
    churn.join().unwrap();
    existing.join().unwrap();
    new_producer.join().unwrap();
}

#[test]
fn broker_routes_by_name_and_ambiguous() {
    let (ea, sa) = start_broker();
    let _a = fake_emu(ea.clone(), Some("alpha".into()));
    let _b = fake_emu(ea, Some("beta".into()));
    std::thread::sleep(Duration::from_millis(150));
    // 이름 없이 → Ambiguous
    let (_s, ar) = attach(&sa, None);
    assert!(
        ar.contains("ambiguous"),
        "다중인데 이름 없으면 ambiguous: {ar}"
    );
    // 이름 beta → 성공
    let (_s2, ar2) = attach(&sa, Some("beta"));
    assert!(
        ar2.contains("attached_name") && ar2.contains("beta"),
        "beta 라우팅: {ar2}"
    );
}

#[test]
fn broker_second_session_busy() {
    let (ea, sa) = start_broker();
    let _a = fake_emu(ea, Some("g".into()));
    std::thread::sleep(Duration::from_millis(100));
    let (_s1, ar1) = attach(&sa, Some("g")); // 살아있는 첫 세션
    assert!(ar1.contains("attached_name"), "{ar1}");
    let (_s2, ar2) = attach(&sa, Some("g")); // 둘째 → Busy
    assert!(ar2.contains("busy"), "둘째 세션은 busy: {ar2}");
}

#[test]
fn broker_exact_registration_can_resume_the_same_application_link() {
    let (ea, sa) = start_broker();
    let _emulator = fake_emu_alive(ea, "g", 4000);
    std::thread::sleep(Duration::from_millis(100));
    let (old_session, first) = attach(&sa, Some("g"));
    let first: serde_json::Value = serde_json::from_str(first.trim()).unwrap();
    let registration_id = first["result"]["broker_registration_id"]
        .as_u64()
        .expect("broker registration identity");

    let (_new_session, resumed) = attach_with_params(
        &sa,
        serde_json::json!({
            "name": "g",
            "expected_registration_id": registration_id,
        }),
    );
    assert!(
        resumed.contains("attached_name") && resumed.contains("broker_registration_id"),
        "exact registration should resume the same application link: {resumed}"
    );
    drop(old_session);
}

#[test]
fn broker_rejects_a_reconnect_after_emulator_registration_changes() {
    let (ea, sa) = start_broker();
    let _emulator = fake_emu_alive(ea, "g", 4000);
    std::thread::sleep(Duration::from_millis(100));

    let (_session, response) = attach_with_params(
        &sa,
        serde_json::json!({
            "name": "g",
            "expected_registration_id": u64::MAX,
        }),
    );
    assert!(
        response.contains("identity_mismatch"),
        "a stale registration identity must not bind a different emulator: {response}"
    );
}

#[test]
fn broker_accepts_reregistration_only_for_the_same_returned_launch() {
    let (ea, sa) = start_broker();
    let first_emulator = controlled_producer(&ea, Some("g"), "launch-same");
    std::thread::sleep(Duration::from_millis(100));
    let (first_session, first) = attach(&sa, Some("g"));
    let first: serde_json::Value = serde_json::from_str(first.trim()).unwrap();
    let first_registration = first["result"]["broker_registration_id"].as_u64().unwrap();

    disconnect_producer(&first_emulator, &first_session);
    let _replacement_emulator = fake_managed_emu_alive(ea, "g", "launch-same", 4000);
    std::thread::sleep(Duration::from_millis(100));
    let (_continued_session, continued) = attach_with_params(
        &sa,
        serde_json::json!({
            "name": "g",
            "expected_registration_id": first_registration,
            "expected_launch_id": test_launch_id("launch-same"),
        }),
    );
    let continued: serde_json::Value = serde_json::from_str(continued.trim()).unwrap();
    assert_eq!(continued["ok"], true);
    assert_ne!(
        continued["result"]["broker_registration_id"], first_registration,
        "adapter re-registration must retain a distinct transport identity"
    );
    drop(first_session);
}

#[test]
fn broker_rejects_reregistration_for_a_different_launch() {
    let (ea, sa) = start_broker();
    let _emulator = fake_managed_emu_alive(ea, "g", "launch-new", 4000);
    std::thread::sleep(Duration::from_millis(100));

    let (_session, response) = attach_with_params(
        &sa,
        serde_json::json!({
            "name": "g",
            "expected_registration_id": u64::MAX,
            "expected_launch_id": test_launch_id("launch-old"),
        }),
    );
    assert!(
        response.contains("identity_mismatch"),
        "a durable launch mismatch must reject transport recovery: {response}"
    );
}

// hello만 응답하고 명령을 echo하지 않은 채 hold_ms 동안 살아있는 에뮬레이터.
fn fake_emu_alive(addr: String, name: &str, hold_ms: u64) -> std::thread::JoinHandle<()> {
    let name = name.to_string();
    std::thread::spawn(move || {
        let s = connect_producer(addr);
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut hello = String::new();
        r.read_line(&mut hello).unwrap();
        writeln!(
            w,
            r#"{{"id":0,"ok":true,"result":{{"protocol_version":1,"methods":["status"],"name":"{name}"}}}}"#
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(hold_ms));
    })
}

fn fake_managed_emu_alive(
    addr: String,
    name: &str,
    launch_id: &str,
    hold_ms: u64,
) -> std::thread::JoinHandle<()> {
    let name = name.to_string();
    let launch_id = test_launch_id(launch_id);
    std::thread::spawn(move || {
        let stream = connect_producer_as(addr, &launch_id);
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut writer = stream;
        let mut hello = String::new();
        reader.read_line(&mut hello).unwrap();
        writeln!(
            writer,
            "{}",
            serde_json::json!({
                "id": 0,
                "ok": true,
                "result": {
                    "protocol_version": 1,
                    "methods": ["status"],
                    "name": name,
                    "launch_id": launch_id,
                },
            })
        )
        .unwrap();
        std::thread::sleep(Duration::from_millis(hold_ms));
    })
}

#[test]
fn broker_old_session_reader_does_not_clobber_new_pairing() {
    // EOF 후 같은 EMUCAP_NAME 에뮬레이터가 등록된 뒤, 구 세션의 리더가 뒤늦게 종료하며
    // 신규 세션의 페어링을 unpair하면 안 된다(session generation 가드).
    let (ea, sa) = start_broker();
    let e1 = controlled_producer(&ea, Some("g"), "old");
    std::thread::sleep(Duration::from_millis(100));

    // 세션 A: 구 에뮬레이터에 페어링(소켓 보관 — 아직 종료 안 함).
    let (a_sock, ar_a) = attach(&sa, Some("g"));
    assert!(ar_a.contains("attached_name"), "A attach: {ar_a}");

    // EOF cleanup releases the name before a different producer registers.
    disconnect_producer(&e1, &a_sock);
    let _e2 = fake_emu_alive(ea.clone(), "g", 3000);
    std::thread::sleep(Duration::from_millis(200)); // 신규 등록 완료

    // 세션 B: 신규 에뮬레이터에 페어링.
    let (_b_sock, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("attached_name"),
        "B attach(신규 에뮬레이터): {ar_b}"
    );

    // 구 세션 A 종료 → 구 세션 리더가 EOF로 깨어나 정리한다.
    drop(a_sock);
    std::thread::sleep(Duration::from_millis(200));

    // B의 페어링이 유지되면 제3 세션 C는 busy여야 한다.
    // 가드가 없으면 A의 리더가 B를 unpair해 C가 에뮬레이터를 탈취한다.
    let (_c_sock, ar_c) = attach(&sa, Some("g"));
    assert!(
        ar_c.contains("busy"),
        "B 페어링이 유지되어야 C는 busy — 구 세션 리더가 신규 페어링을 clobber함: {ar_c}"
    );
}

#[test]
fn broker_does_not_transfer_an_open_session_after_elapsed_time() {
    // heartbeat 지연은 관측 신호일 뿐이며 열린 세션의 배타 제어권을 이전하지 않는다.
    let (ea, sa) = start_broker();
    let _e = fake_emu_alive(ea, "g", 4000);
    std::thread::sleep(Duration::from_millis(100));
    let (a_sock, ar_a) = attach(&sa, Some("g"));
    assert!(ar_a.contains("attached_name"), "A attach: {ar_a}");
    std::thread::sleep(Duration::from_millis(350));
    let (_b, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("busy"),
        "열린 세션은 경과 시간만으로 이전되면 안 된다: {ar_b}"
    );
    let _ = a_sock;
}

#[test]
fn broker_heartbeat_does_not_change_exclusive_control() {
    // _ping은 transport 관측일 뿐이며 열린 세션의 배타 제어권을 바꾸지 않는다.
    let (ea, sa) = start_broker();
    let _e = fake_emu_alive(ea, "g", 4000);
    std::thread::sleep(Duration::from_millis(100));
    let (mut a_sock, ar_a) = attach(&sa, Some("g"));
    assert!(ar_a.contains("attached_name"), "A attach: {ar_a}");
    // A가 heartbeat를 주기적으로 보내도 소유권 근거로 사용하지 않는다.
    for _ in 0..4 {
        writeln!(a_sock, r#"{{"v":1,"method":"_ping"}}"#).unwrap();
        std::thread::sleep(Duration::from_millis(60));
    }
    // A의 연결이 열려 있으므로 B는 busy.
    let (_b, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("busy"),
        "활동 중 세션은 busy 유지해야: {ar_b}"
    );
}

// hello 응답 후 명령 하나를 읽어 *받은 그대로의 id*를 기억하고, `go` 신호가 올 때까지 응답을 보류한다.
// The signal releases a late reply after a front-session handoff.
fn fake_emu_hold_reply(
    addr: String,
    name: &str,
    go: std::sync::mpsc::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    let name = name.to_string();
    std::thread::spawn(move || {
        let s = connect_producer(addr);
        let mut r = BufReader::new(s.try_clone().unwrap());
        let mut w = s;
        let mut hello = String::new();
        r.read_line(&mut hello).unwrap();
        writeln!(
            w,
            r#"{{"id":0,"ok":true,"result":{{"protocol_version":1,"methods":["status"],"name":"{name}"}}}}"#
        )
        .unwrap();
        // A의 요청 한 줄을 읽어 broker가 부여한(네임스페이스된) id를 그대로 보관.
        let mut cmd = String::new();
        r.read_line(&mut cmd).unwrap();
        let id = serde_json::from_str::<serde_json::Value>(cmd.trim()).unwrap()["id"].clone();
        // Hold the reply until the front-session handoff completes.
        let _ = go.recv();
        // A의 요청에 대한 뒤늦은 응답 — 펜싱되어 신규 소유자 B에게 배달되면 안 된다.
        writeln!(w, r#"{{"id":{id},"ok":true,"result":{{"stale":true}}}}"#).unwrap();
        std::thread::sleep(Duration::from_millis(300));
    })
}

#[test]
fn broker_fences_a_late_response_after_closed_session_reattach() {
    // 세션 A가 닫힌 뒤 명시적으로 붙은 B는 A의 늦은 응답을 자기 응답으로 받지 않아야 한다.
    let (ea, sa) = start_broker();
    let (go_tx, go_rx) = std::sync::mpsc::channel();
    let _e = fake_emu_hold_reply(ea, "g", go_rx);
    std::thread::sleep(Duration::from_millis(100)); // 등록 여유

    // 세션 A: attach 후 요청(id=7) 전송 — 에뮬레이터는 읽되 응답 보류.
    let (a_sock, ar_a) = attach(&sa, Some("g"));
    assert!(ar_a.contains("attached_name"), "A attach: {ar_a}");
    {
        let mut wa = a_sock.try_clone().unwrap();
        writeln!(wa, r#"{{"v":1,"id":7,"method":"status","params":{{}}}}"#).unwrap();
    }
    std::thread::sleep(Duration::from_millis(80)); // 에뮬레이터가 A 요청을 읽을 시간

    drop(a_sock);
    std::thread::sleep(Duration::from_millis(100));
    let (b_sock, ar_b) = attach(&sa, Some("g"));
    assert!(
        ar_b.contains("attached_name"),
        "A가 닫힌 뒤 B가 명시적으로 attach: {ar_b}"
    );
    // B도 같은 id=7로 자기 요청 전송(겹치는 id 공간 재현).
    {
        let mut wb = b_sock.try_clone().unwrap();
        writeln!(wb, r#"{{"v":1,"id":7,"method":"status","params":{{}}}}"#).unwrap();
    }

    // 이제 에뮬레이터가 A의 요청에 대한 뒤늦은 응답을 방출.
    go_tx.send(()).unwrap();
    std::thread::sleep(Duration::from_millis(150));

    // B는 A의 stale 응답(id=7, "stale":true)을 자기 것으로 받아선 안 된다.
    b_sock
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut rb = BufReader::new(b_sock);
    let mut got = String::new();
    let n = rb.read_line(&mut got).unwrap_or(0);
    assert!(
        !got.contains("stale"),
        "B가 옛 세션 A의 in-flight 응답을 받으면 안 됨(fence): n={n}, got={got:?}"
    );
}

#[test]
fn broker_persists_across_session() {
    let (ea, sa) = start_broker();
    let _a = fake_emu(ea, Some("g".into()));
    std::thread::sleep(Duration::from_millis(100));
    {
        let (s1, ar1) = attach(&sa, Some("g"));
        assert!(ar1.contains("attached_name"));
        drop(s1);
    }
    std::thread::sleep(Duration::from_millis(100)); // 언페어 여유
    let (_s2, ar2) = attach(&sa, Some("g")); // 재attach 같은 에뮬레이터
    assert!(
        ar2.contains("attached_name") && ar2.contains("g"),
        "재attach 복귀: {ar2}"
    );
}

#[test]
fn broker_frontend_eof_delivers_negotiated_attachment_detach() {
    let (ea, sa) = start_broker();
    let (sent, received) = std::sync::mpsc::channel();
    let producer = std::thread::spawn(move || {
        let launch_id = test_launch_id("live");
        let mut writer = connect_producer_as(ea, &launch_id);
        writer
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut reader = BufReader::new(writer.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        writeln!(writer,"{}",serde_json::json!({"id":0,"ok":true,"result":{"protocol_version":1,"name":"lifecycle","methods":["step"],"launch_id":launch_id,"control_session_lifecycle":true}})).unwrap();
        for _ in 0..3 {
            line.clear();
            assert_ne!(reader.read_line(&mut line).unwrap(), 0);
            sent.send(serde_json::from_str::<serde_json::Value>(&line).unwrap())
                .unwrap();
        }
    });
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    let mut frontend = loop {
        let (socket, result) = attach(&sa, Some("lifecycle"));
        if result.contains("attached_name") {
            break socket;
        }
        assert!(std::time::Instant::now() < deadline, "{result}");
        std::thread::yield_now();
    };
    writeln!(
        frontend,
        "{}",
        serde_json::json!({"v":1,"id":1,"method":"step","params":{"frames":2}})
    )
    .unwrap();
    let attached = received.recv_timeout(Duration::from_secs(3)).unwrap();
    let request = received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(attached["_control_session"]["kind"], "attach");
    assert_eq!(
        request["params"]["_control_attachment"],
        attached["_control_session"]["attachment"]
    );
    drop(frontend);
    let detached = received.recv_timeout(Duration::from_secs(3)).unwrap();
    assert_eq!(detached["_control_session"]["kind"], "detach");
    assert_eq!(
        detached["_control_session"]["attachment"],
        attached["_control_session"]["attachment"]
    );
    producer.join().unwrap();
}

#[test]
fn oversized_admission_is_closed_at_every_stage() {
    let (producer, session) = start_broker();
    for (address, stage) in [(&producer, 0), (&session, 1), (&session, 2)] {
        let mut stream = TcpStream::connect(address).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        if stage == 0 {
            let mut challenge = [0u8; 25];
            reader.read_exact(&mut challenge).unwrap();
        } else if stage == 2 {
            begin_session_auth(&mut stream, &mut reader);
        }
        // Fragment without a newline: admission must reject before EOF or parsing.
        let bytes = vec![b' '; broker::MAX_ADMISSION_FRAME_BYTES + 1];
        for chunk in bytes.chunks(1024) {
            if stream.write_all(chunk).is_err() {
                break;
            }
        }
        let mut byte = [0];
        let result = reader.read(&mut byte);
        assert!(
            matches!(result, Ok(0))
                || result.as_ref().is_err_and(|error| matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted
                )),
            "stage {stage}: {result:?}"
        );
    }
    // Rejected candidates release permits; ordinary admission remains available.
    let _producer = fake_emu(producer, Some("after-limit".into()));
    std::thread::sleep(Duration::from_millis(150));
    let (_, response) = attach(&session, Some("after-limit"));
    assert!(response.contains("after-limit"), "{response}");
}
