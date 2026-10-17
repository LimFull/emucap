use super::{broker, broker_link};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

const TOKEN: &str = "readiness-fixture-controller";

#[test]
fn readiness_only_verifies_server_and_never_sends_attach_even_to_a_valid_server() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(!line.contains(TOKEN));
        let request: serde_json::Value = serde_json::from_str(&line).unwrap();
        assert_eq!(request["method"], "broker_authenticate");
        let client = request["params"]["client_nonce"].as_str().unwrap();
        let nonce = broker::new_auth_nonce();
        let proof = broker::session_server_proof(TOKEN, client, &nonce);
        writeln!(
            stream,
            "{}",
            serde_json::json!({"id":0,"ok":true,"result":{
                "protocol_version":1,"server_nonce":nonce,"server_proof":proof
            }})
        )
        .unwrap();
        line.clear();
        assert_eq!(reader.read_line(&mut line).unwrap(), 0);
    });
    broker_link::probe_session_with_token(&address, TOKEN, Duration::from_secs(1)).unwrap();
    server.join().unwrap();
}

#[test]
fn readiness_rejects_wrong_key_replayed_proof_and_malformed_reply() {
    for reply in ["wrong-key", "replayed-proof", "malformed"] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let server = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(1)))
                .unwrap();
            let mut reader = BufReader::new(stream.try_clone().unwrap());
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let request: serde_json::Value = serde_json::from_str(&line).unwrap();
            let client = request["params"]["client_nonce"].as_str().unwrap();
            let nonce = broker::new_auth_nonce();
            let proof = if reply == "replayed-proof" {
                broker::session_server_proof(TOKEN, &broker::new_auth_nonce(), &nonce)
            } else {
                broker::session_server_proof("another-controller", client, &nonce)
            };
            if reply == "malformed" {
                writeln!(stream, "invalid response").unwrap();
            } else {
                writeln!(
                    stream,
                    "{}",
                    serde_json::json!({"id":0,"ok":true,"result":{
                        "protocol_version":1,"server_nonce":nonce,"server_proof":proof
                    }})
                )
                .unwrap();
            }
            line.clear();
            assert_eq!(reader.read_line(&mut line).unwrap(), 0);
        });
        assert!(
            broker_link::probe_session_with_token(&address, TOKEN, Duration::from_secs(1)).is_err()
        );
        server.join().unwrap();
    }
}

#[test]
fn readiness_silent_peer_is_bounded() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap().to_string();
    let started = Instant::now();
    assert!(
        broker_link::probe_session_with_token(&address, TOKEN, Duration::from_millis(60)).is_err()
    );
    assert!(started.elapsed() < Duration::from_secs(1));
}
