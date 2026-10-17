use super::*;
use std::net::TcpListener;
use std::thread;
use std::time::Duration;
use tungstenite::handshake::server::{
    ErrorResponse as WsErrorResponse, Request as WsRequest, Response as WsResponse,
};
use tungstenite::{accept_hdr, Message};

const TEST_NATIVE_DEBUG_TOKEN: &str = "transport-test-token";

#[allow(clippy::result_large_err)]
fn select_ppsspp_subprotocol(
    request: &WsRequest,
    mut response: WsResponse,
) -> Result<WsResponse, WsErrorResponse> {
    let expected_authorization = format!("Bearer {TEST_NATIVE_DEBUG_TOKEN}");
    assert_eq!(
        request
            .headers()
            .get("authorization")
            .and_then(|value| value.to_str().ok()),
        Some(expected_authorization.as_str())
    );
    assert_eq!(
        request
            .headers()
            .get("origin")
            .and_then(|value| value.to_str().ok()),
        Some(super::transport::PPSSPP_MANAGED_ORIGIN)
    );
    response.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        tungstenite::http::HeaderValue::from_static(PPSSPP_SUBPROTOCOL),
    );
    Ok(response)
}

#[test]
fn empty_native_debug_token_is_rejected_before_tcp_connect() {
    let error = match TungsteniteWs::connect(1, Duration::from_millis(10), "") {
        Ok(_) => panic!("empty native debugger token unexpectedly connected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("EMUCAP_NATIVE_DEBUG_TOKEN"));
}

fn websocket_server(
    exchange: impl FnOnce(&mut tungstenite::WebSocket<std::net::TcpStream>) + Send + 'static,
) -> (u16, thread::JoinHandle<()>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut socket = accept_hdr(stream, select_ppsspp_subprotocol).unwrap();
        exchange(&mut socket);
    });
    (port, handle)
}

fn read_json(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) -> Value {
    match socket.read().unwrap() {
        Message::Text(text) => serde_json::from_str(text.as_str()).unwrap(),
        other => panic!("expected a text request, got {other:?}"),
    }
}

#[test]
fn delayed_error_cannot_become_the_next_calls_error() {
    let (port, server) = websocket_server(|socket| {
        let first = read_json(socket);
        assert_eq!(first["event"], "slow.command");
        let first_ticket = first["ticket"].as_str().unwrap().to_owned();

        thread::sleep(Duration::from_millis(80));
        socket
            .send(Message::Text(
                json!({
                    "event": "error",
                    "ticket": first_ticket,
                    "message": "late failure from the timed-out command",
                })
                .to_string()
                .into(),
            ))
            .unwrap();

        let second = read_json(socket);
        assert_eq!(second["event"], "version");
        let second_ticket = second["ticket"].as_str().unwrap().to_owned();
        socket
            .send(Message::Text(
                json!({
                    "event": "version",
                    "ticket": second_ticket,
                    "version": "test",
                })
                .to_string()
                .into(),
            ))
            .unwrap();
    });

    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    let first = ws.call_with_timeout("slow.command", json!({}), Duration::from_millis(30));
    assert!(first.as_ref().is_err_and(is_timeout_error));
    assert!(
        !ws.is_terminal(),
        "a ticketed read timeout remains recoverable"
    );

    let second = ws.call("version", json!({})).unwrap();
    assert_eq!(second["version"], "test");
    assert!(!ws.is_terminal());

    let queued = ws.drain_events();
    assert!(queued.iter().any(|event| {
        event["event"] == "error" && event["message"] == "late failure from the timed-out command"
    }));
    server.join().unwrap();
}

#[test]
fn asynchronous_ack_must_echo_the_requests_ticket() {
    let (port, server) = websocket_server(|socket| {
        let request = read_json(socket);
        assert_eq!(request["event"], "cpu.stepInto");
        socket
            .send(Message::Text(
                json!({
                    "event": "cpu.stepping",
                    "ticket": request["ticket"],
                    "pc": 0x0880_4004u64,
                })
                .to_string()
                .into(),
            ))
            .unwrap();
    });

    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    let response = ws
        .call_and_wait_for("cpu.stepInto", json!({}), "cpu.stepping")
        .unwrap();
    assert_eq!(response["pc"], 0x0880_4004u64);
    server.join().unwrap();
}

#[test]
fn websocket_close_marks_the_transport_terminal() {
    let (port, server) = websocket_server(|socket| {
        let request = read_json(socket);
        assert_eq!(request["event"], "version");
        socket.close(None).unwrap();
    });

    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    assert!(ws.call("version", json!({})).is_err());
    assert!(ws.is_terminal());
    server.join().unwrap();
}

fn frame_protocol_capability(socket: &mut tungstenite::WebSocket<std::net::TcpStream>) {
    let request = read_json(socket);
    assert_eq!(request["event"], "emucap.frameStep.capability");
    socket
        .send(Message::Text(
            json!({
                "event":"emucap.frameStep.capability", "ticket":request["ticket"],
                "version":1, "memory_park_version":1, "connection_owned":true
            })
            .to_string()
            .into(),
        ))
        .unwrap();
}

#[test]
fn frame_cancel_drains_both_tickets_in_either_order_and_keeps_breakpoint_event() {
    for terminal_first in [false, true] {
        let cancellation = crate::live::link::RequestCancellation::default();
        let signal = cancellation.clone();
        let (port, server) = websocket_server(move |socket| {
            frame_protocol_capability(socket);
            let step = read_json(socket);
            assert_eq!(step["event"], "emucap.frameStep");
            signal.cancel();
            let cancel = read_json(socket);
            assert_eq!(cancel["event"], "emucap.frameStep.cancel");
            assert_eq!(cancel["operation_id"], step["operation_id"]);
            assert_ne!(cancel["ticket"], step["ticket"]);
            let terminal = json!({"event":"emucap.frameStep", "ticket":step["ticket"],
                "operation_id":step["operation_id"],"unit":"frames","count":120,
                "completed":3,"status":"interrupted","state":"frozen"});
            let ack = if terminal_first {
                json!({"event":"error","ticket":cancel["ticket"],"message":"already completed"})
            } else {
                json!({"event":"emucap.frameStep.cancel","ticket":cancel["ticket"],"status":"requested"})
            };
            socket
                .send(Message::Text(
                    json!({"event":"cpu.stepping","reason":"cpu.breakpoint"})
                        .to_string()
                        .into(),
                ))
                .unwrap();
            for reply in if terminal_first {
                [terminal, ack]
            } else {
                [ack, terminal]
            } {
                socket
                    .send(Message::Text(reply.to_string().into()))
                    .unwrap();
            }
            let next = read_json(socket);
            assert_eq!(next["event"], "version");
            socket
                .send(Message::Text(
                    json!({"event":"version","ticket":next["ticket"],"version":"after-cancel"})
                        .to_string()
                        .into(),
                ))
                .unwrap();
        });
        let mut ws =
            TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
        let result = ws
            .frame_step_cancellable(120, Duration::from_secs(3), &cancellation)
            .unwrap();
        assert_eq!(result["completed"], 3);
        assert!(!ws.is_terminal());
        assert_eq!(
            ws.call("version", json!({})).unwrap()["version"],
            "after-cancel"
        );
        assert!(ws
            .drain_events()
            .iter()
            .any(|v| v["reason"] == "cpu.breakpoint"));
        server.join().unwrap();
    }
}

#[test]
fn frame_protocol_rejects_old_native_before_guest_dispatch() {
    let (port, server) = websocket_server(|socket| {
        let query = read_json(socket);
        assert_eq!(query["event"], "emucap.frameStep.capability");
        socket
            .send(Message::Text(
                json!({"event":"error","ticket":query["ticket"],"message":"unknown event"})
                    .to_string()
                    .into(),
            ))
            .unwrap();
        let next = read_json(socket);
        assert_eq!(next["event"], "version");
        socket
            .send(Message::Text(
                json!({"event":"version","ticket":next["ticket"]})
                    .to_string()
                    .into(),
            ))
            .unwrap();
    });
    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    assert!(ws
        .frame_step_cancellable(120, Duration::from_secs(3), &Default::default())
        .is_err());
    assert!(!ws.is_terminal());
    ws.call("version", json!({})).unwrap();
    server.join().unwrap();
}

#[test]
fn unverified_frame_terminal_retires_native_channel() {
    for fault in ["wrong_operation", "running", "too_many", "false_completed"] {
        let (port, server) = websocket_server(move |socket| {
            frame_protocol_capability(socket);
            let step = read_json(socket);
            let mut result = json!({"event":"emucap.frameStep","ticket":step["ticket"],
                "operation_id":step["operation_id"],"unit":"frames","count":120,
                "completed":3,"status":"interrupted","state":"frozen"});
            match fault {
                "wrong_operation" => result["operation_id"] = json!("stale"),
                "running" => result["state"] = json!("running"),
                "too_many" => result["completed"] = json!(121),
                _ => result["status"] = json!("completed"),
            }
            socket
                .send(Message::Text(result.to_string().into()))
                .unwrap();
            assert!(
                socket.read().is_err(),
                "uncertain producer channel must close"
            );
        });
        let mut ws =
            TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
        assert!(
            ws.frame_step_cancellable(120, Duration::from_secs(3), &Default::default())
                .is_err(),
            "{fault}"
        );
        assert!(ws.is_terminal(), "{fault}");
        server.join().unwrap();
    }
}

#[test]
fn cancellation_ack_without_native_terminal_cannot_certify_stop() {
    let cancellation = crate::live::link::RequestCancellation::default();
    let signal = cancellation.clone();
    let (port, server) = websocket_server(move |socket| {
        frame_protocol_capability(socket);
        let _step = read_json(socket);
        signal.cancel();
        let cancel = read_json(socket);
        socket.send(Message::Text(json!({"event":"emucap.frameStep.cancel", "ticket":cancel["ticket"], "status":"requested"}).to_string().into())).unwrap();
        assert!(
            socket.read().is_err(),
            "unverified stop must close the native connection"
        );
    });
    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    let result = ws.frame_step_cancellable(120, Duration::from_secs(5), &cancellation);
    assert!(result
        .unwrap_err()
        .to_string()
        .contains("stop was not verified"));
    assert!(ws.is_terminal());
    server.join().unwrap();
}

#[test]
fn cancelled_before_admission_does_not_send_native_request() {
    let (port, server) = websocket_server(|socket| {
        let next = read_json(socket);
        assert_eq!(next["event"], "version");
        socket
            .send(Message::Text(
                json!({"event":"version","ticket":next["ticket"]})
                    .to_string()
                    .into(),
            ))
            .unwrap();
    });
    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    let cancellation = crate::live::link::RequestCancellation::default();
    cancellation.cancel();
    assert!(ws
        .frame_step_cancellable(120, Duration::from_secs(3), &cancellation)
        .is_err());
    assert!(!ws.is_terminal());
    ws.call("version", json!({})).unwrap();
    server.join().unwrap();
}

#[test]
fn pacing_reply_timeout_keeps_bridge_retired_after_late_native_success() {
    let (release, wait) = std::sync::mpsc::channel();
    let (sent, delivered) = std::sync::mpsc::channel();
    let (port, server) = websocket_server(move |socket| {
        let probe = read_json(socket);
        socket
            .send(Message::Text(
                json!({"event":"emucap.pacing","ticket":probe["ticket"],
            "percent":100,"fast_forward":false,"fps_limit":0,"network_forced":false,
            "revision":1,"vblank":7,"transaction_version":1})
                .to_string()
                .into(),
            ))
            .unwrap();
        let setter = read_json(socket);
        assert_eq!(setter["event"], "emucap.pacing");
        assert_eq!(setter["percent"], 50);
        // Controlled native commit precedes the lost reply; no native rollback is attempted.
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        socket
            .send(Message::Text(
                json!({"event":"emucap.pacing","ticket":setter["ticket"],
            "percent":50,"fast_forward":false,"fps_limit":0,"network_forced":false,
            "revision":2,"vblank":7,"transaction_version":1,"clock_domain":"psp_vblank",
            "begin_vblank":7,"outcome":"completed",
            "previous":{"percent":100,"fast_forward":false,"fps_limit":0,"network_forced":false,
                        "revision":1,"vblank":7}})
                .to_string()
                .into(),
            ))
            .unwrap();
        sent.send(()).unwrap();
    });
    let ws =
        TungsteniteWs::connect(port, Duration::from_millis(100), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    let mut bridge = PpssppBridge::new(ws);
    let result = bridge.execution_speed(&json!({"mode":"limited","percent":50}));
    assert!(result.is_err());
    assert!(bridge.control_unverified && bridge.backend_terminal());
    release.send(()).unwrap();
    delivered.recv_timeout(Duration::from_secs(2)).unwrap();
    let _ = bridge.ws.drain_events();
    assert!(
        bridge.control_unverified && bridge.backend_terminal(),
        "late success cannot restore healthy control"
    );
    server.join().unwrap();
}

#[test]
fn frame_poll_retains_partial_reply_and_restores_blocking_calls() {
    use std::io::Write;
    let (port, server) = websocket_server(|socket| {
        frame_protocol_capability(socket);
        let step = read_json(socket);
        assert_eq!(step["event"], "emucap.frameStep");
        let payload = json!({"event":"emucap.frameStep", "ticket":step["ticket"],
            "operation_id":step["operation_id"], "unit":"frames", "count":120,
            "completed":120, "status":"completed", "state":"frozen"})
        .to_string();
        let mut frame = vec![0x81, 126];
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        frame.extend_from_slice(payload.as_bytes());
        socket.get_mut().write_all(&frame[..3]).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        socket.get_mut().write_all(&frame[3..17]).unwrap();
        std::thread::sleep(Duration::from_millis(80));
        socket.get_mut().write_all(&frame[17..]).unwrap();
        let next = read_json(socket);
        assert_eq!(next["event"], "version");
        std::thread::sleep(Duration::from_millis(80));
        socket
            .send(Message::Text(
                json!({"event":"version", "ticket":next["ticket"],
            "version":"after-partial-frame"})
                .to_string()
                .into(),
            ))
            .unwrap();
    });
    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    let result = ws
        .frame_step_cancellable(120, Duration::from_secs(3), &Default::default())
        .unwrap();
    assert_eq!(result["completed"], 120);
    assert!(!ws.is_terminal());
    assert_eq!(
        ws.call("version", json!({})).unwrap()["version"],
        "after-partial-frame"
    );
    server.join().unwrap();
}
#[test]
fn continuous_notifications_do_not_extend_a_request_deadline() {
    let (port, server) = websocket_server(|socket| {
        let _ = read_json(socket);
        for i in 0..60 {
            if socket
                .send(Message::Text(
                    json!({"event":"notice","i":i}).to_string().into(),
                ))
                .is_err()
            {
                break;
            }
            thread::sleep(Duration::from_millis(5));
        }
    });
    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    let start = std::time::Instant::now();
    let result = ws.call_with_timeout("version", json!({}), Duration::from_millis(40));
    assert!(result.as_ref().is_err_and(is_timeout_error));
    assert!(start.elapsed() < Duration::from_millis(200));
    assert!(!ws.is_terminal());
    drop(ws);
    server.join().unwrap();
}

#[test]
fn fragmented_frame_deadline_preserves_late_ticket_recovery() {
    use std::io::Write;
    let (port, server) = websocket_server(|socket| {
        let first = read_json(socket);
        let payload =
            json!({"event":"slow","ticket":first["ticket"],"padding":"x".repeat(300)}).to_string();
        let mut frame = vec![0x81, 126];
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        frame.extend_from_slice(payload.as_bytes());
        for chunk in frame.chunks(20) {
            socket.get_mut().write_all(chunk).unwrap();
            thread::sleep(Duration::from_millis(5));
        }
        let second = read_json(socket);
        socket
            .send(Message::Text(
                json!({"event":"version","ticket":second["ticket"]})
                    .to_string()
                    .into(),
            ))
            .unwrap();
    });
    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    let result = ws.call_with_timeout("slow", json!({}), Duration::from_millis(30));
    assert!(result.as_ref().is_err_and(is_timeout_error));
    assert!(!ws.is_terminal());
    assert!(ws.call("version", json!({})).is_ok());
    assert!(ws
        .drain_events()
        .iter()
        .any(|event| event["event"] == "slow"));
    server.join().unwrap();
}

#[test]
fn pending_events_have_count_and_byte_budgets() {
    for bytes in [0, 1024 * 1024] {
        let (port, server) = websocket_server(move |socket| {
            let _ = read_json(socket);
            let event = json!({"event":"notice","data":"x".repeat(bytes)}).to_string();
            let sends = if bytes == 0 {
                transport::MAX_PENDING_EVENTS + 1
            } else {
                transport::MAX_PENDING_BYTES / event.len() + 2
            };
            for _ in 0..sends {
                if socket.send(Message::Text(event.clone().into())).is_err() {
                    break;
                }
            }
        });
        let mut ws =
            TungsteniteWs::connect(port, Duration::from_secs(5), TEST_NATIVE_DEBUG_TOKEN).unwrap();
        let error = ws.call("version", json!({})).unwrap_err();
        assert!(
            error.to_string().contains("pending event budget"),
            "{error}"
        );
        assert!(ws.is_terminal());
        assert!(ws.drain_events().len() <= if bytes == 0 { 1024 } else { 8 });
        assert!(ws.call("version", json!({})).is_err());
        server.join().unwrap();
    }
}

#[test]
fn oversized_websocket_frame_is_rejected_before_payload() {
    use std::io::Write;
    let (port, server) = websocket_server(|socket| {
        let _ = read_json(socket);
        let mut header = vec![0x81, 127];
        header.extend_from_slice(&(8 * 1024 * 1024 + 1u64).to_be_bytes());
        socket.get_mut().write_all(&header).unwrap();
    });
    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    assert!(ws.call("version", json!({})).is_err());
    assert!(ws.is_terminal());
    server.join().unwrap();
}
#[test]
fn host_deadline_allows_cancellation_grace_and_next_request() {
    let (port, server) =
        websocket_server(|socket| {
            frame_protocol_capability(socket);
            let step = read_json(socket);
            let cancel = read_json(socket);
            assert_eq!(cancel["event"], "emucap.frameStep.cancel");
            socket.send(Message::Text(json!({
            "event":"emucap.frameStep", "ticket":step["ticket"],
            "operation_id":step["operation_id"], "unit":"frames", "count":20,
            "completed":2, "status":"interrupted", "reason":"cancelled", "state":"frozen"
        }).to_string().into())).unwrap();
            socket.send(Message::Text(json!({
            "event":"emucap.frameStep.cancel", "ticket":cancel["ticket"], "status":"requested"
        }).to_string().into())).unwrap();
            let next = read_json(socket);
            socket
                .send(Message::Text(
                    json!({"event":"version", "ticket":next["ticket"]})
                        .to_string()
                        .into(),
                ))
                .unwrap();
        });
    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    let result = ws
        .frame_step_cancellable(
            20,
            Duration::from_millis(80),
            &crate::live::link::RequestCancellation::default(),
        )
        .unwrap();
    assert_eq!(result["reason"], "host_deadline");
    assert!(!ws.is_terminal());
    assert!(ws.call("version", json!({})).is_ok());
    server.join().unwrap();
}

#[test]
fn draining_events_has_a_finite_batch_and_preserves_the_remainder() {
    let (sent, received) = std::sync::mpsc::channel();
    let (release, wait) = std::sync::mpsc::channel();
    let (port, server) = websocket_server(move |socket| {
        for i in 0..1100 {
            socket
                .send(Message::Text(
                    json!({"event":"notice","i":i}).to_string().into(),
                ))
                .unwrap();
        }
        sent.send(()).unwrap();
        wait.recv_timeout(Duration::from_secs(3)).unwrap();
    });
    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    received.recv_timeout(Duration::from_secs(3)).unwrap();
    let started = std::time::Instant::now();
    let mut all = ws.drain_events();
    assert!(all.len() <= 1024);
    assert!(started.elapsed() < Duration::from_secs(1));
    for _ in 0..20 {
        if all.len() == 1100 {
            break;
        }
        all.extend(ws.drain_events());
    }
    assert_eq!(all.len(), 1100);
    for (i, event) in all.iter().enumerate() {
        assert_eq!(event["i"], i);
    }
    assert!(!ws.is_terminal());
    release.send(()).unwrap();
    server.join().unwrap();
}
#[test]
fn cancellation_interrupts_a_continuously_readable_fragmented_message() {
    use std::io::Write;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    let (started, ready) = std::sync::mpsc::channel();
    let (port, server) = websocket_server(move |socket| {
        frame_protocol_capability(socket);
        let step = read_json(socket);
        socket.get_mut().write_all(&[0x01, 0]).unwrap();
        let mut stream = socket.get_ref().try_clone().unwrap();
        let running = Arc::new(AtomicBool::new(true));
        let writer_running = running.clone();
        let writer = thread::spawn(move || {
            let continuations = [0u8; 4096]; // Non-final, empty continuation frames.
            while writer_running.load(Ordering::Relaxed) {
                if stream.write_all(&continuations).is_err() {
                    break;
                }
            }
        });
        started.send(()).unwrap();
        let cancel = read_json(socket);
        assert_eq!(cancel["event"], "emucap.frameStep.cancel");
        running.store(false, Ordering::Relaxed);
        writer.join().unwrap();
        let payload = json!({
            "event":"emucap.frameStep", "ticket":step["ticket"],
            "operation_id":step["operation_id"], "unit":"frames", "count":20,
            "completed":1, "status":"interrupted", "reason":"cancelled", "state":"frozen"
        })
        .to_string();
        let mut terminal = vec![0x80, 126]; // Finish the outstanding text message.
        terminal.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        terminal.extend_from_slice(payload.as_bytes());
        socket.get_mut().write_all(&terminal).unwrap();
        socket.send(Message::Text(json!({
            "event":"emucap.frameStep.cancel", "ticket":cancel["ticket"], "status":"requested"
        }).to_string().into())).unwrap();
    });
    let cancellation = crate::live::link::RequestCancellation::default();
    let cancel = cancellation.clone();
    let trigger = thread::spawn(move || {
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        thread::sleep(Duration::from_millis(30));
        cancel.cancel();
    });
    let mut ws =
        TungsteniteWs::connect(port, Duration::from_secs(1), TEST_NATIVE_DEBUG_TOKEN).unwrap();
    let start = std::time::Instant::now();
    let result = ws
        .frame_step_cancellable(20, Duration::from_secs(3), &cancellation)
        .unwrap();
    assert_eq!(result["reason"], "cancelled");
    assert!(start.elapsed() < Duration::from_secs(1));
    trigger.join().unwrap();
    server.join().unwrap();
}
