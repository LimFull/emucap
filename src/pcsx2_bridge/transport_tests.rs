use super::*;
#[cfg(unix)]
use std::os::unix::net::UnixListener;

fn read_wire_packet(stream: &mut impl Read) -> Vec<u8> {
    let mut header = [0u8; 4];
    stream.read_exact(&mut header).unwrap();
    let length = u32::from_le_bytes(header) as usize;
    assert!(length >= 4);
    let mut payload = vec![0u8; length - 4];
    stream.read_exact(&mut payload).unwrap();
    payload
}

fn write_wire_reply(stream: &mut impl Write, payload: &[u8]) {
    let length = u32::try_from(5 + payload.len()).unwrap();
    stream.write_all(&length.to_le_bytes()).unwrap();
    stream.write_all(&[0]).unwrap();
    stream.write_all(payload).unwrap();
}

#[cfg(unix)]
fn connected_pair() -> (PineSocket, UnixStream) {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("pine.sock");
    let listener = UnixListener::bind(&path).unwrap();
    let pine = PineSocket::connect(1, Some(&path), Duration::from_secs(1)).unwrap();
    (pine, listener.accept().unwrap().0)
}

#[cfg(windows)]
fn connected_pair() -> (PineSocket, TcpStream) {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let pine = PineSocket::connect(
        listener.local_addr().unwrap().port(),
        None,
        Duration::from_secs(1),
    )
    .unwrap();
    (pine, listener.accept().unwrap().0)
}

#[test]
fn long_authentication_token_matches_the_native_vector() {
    let token = format!("native-{}", "0123456789abcdef".repeat(4));
    let client_nonce: [u8; AUTH_NONCE_SIZE] = std::array::from_fn(|index| index as u8);
    let server_nonce: [u8; AUTH_NONCE_SIZE] =
        std::array::from_fn(|index| (index + AUTH_NONCE_SIZE) as u8);
    let actual = auth_hmac(token.as_bytes(), b"server", &client_nonce, &server_nonce);
    let actual_hex = actual
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    assert_eq!(
        actual_hex,
        "ee7012c007b5c3587ec126e6d0d78c253029c883883c8509b2f3fce63317ce82"
    );
}

#[test]
fn mutual_authentication_precedes_ordinary_pine_traffic() {
    let token = format!("native-{}", "0123456789abcdef".repeat(4));
    let (mut pine, mut stream) = connected_pair();
    let server_token = token.clone();
    let host = std::thread::spawn(move || {
        let challenge = read_wire_packet(&mut stream);
        assert_eq!(challenge.len(), 1 + AUTH_NONCE_SIZE);
        assert_eq!(challenge[0], AUTH_CHALLENGE_COMMAND);
        let client_nonce: [u8; AUTH_NONCE_SIZE] = challenge[1..].try_into().unwrap();
        let server_nonce: [u8; AUTH_NONCE_SIZE] =
            std::array::from_fn(|index| (index + AUTH_NONCE_SIZE) as u8);
        let server_proof = auth_hmac(
            server_token.as_bytes(),
            b"server",
            &client_nonce,
            &server_nonce,
        );
        let mut reply = Vec::from(server_nonce);
        reply.extend_from_slice(&server_proof);
        write_wire_reply(&mut stream, &reply);

        let proof = read_wire_packet(&mut stream);
        assert_eq!(proof.len(), 1 + AUTH_PROOF_SIZE);
        assert_eq!(proof[0], AUTH_PROOF_COMMAND);
        let expected = auth_hmac(
            server_token.as_bytes(),
            b"client",
            &client_nonce,
            &server_nonce,
        );
        assert!(constant_time_equal(&proof[1..], &expected));
        write_wire_reply(&mut stream, &[]);

        assert_eq!(read_wire_packet(&mut stream), [MSG_STATUS]);
        write_wire_reply(&mut stream, &[]);
    });
    pine.authenticate_mutual(&token).unwrap();
    assert!(!pine.is_terminal());
    assert_eq!(pine.transact(&[MSG_STATUS]).unwrap(), Vec::<u8>::new());
    host.join().unwrap();
}

#[test]
fn counterfeit_server_never_receives_the_client_proof() {
    let (mut pine, mut stream) = connected_pair();
    let host = std::thread::spawn(move || {
        let challenge = read_wire_packet(&mut stream);
        assert_eq!(challenge[0], AUTH_CHALLENGE_COMMAND);
        write_wire_reply(&mut stream, &[0u8; AUTH_NONCE_SIZE + AUTH_PROOF_SIZE]);
        let mut remainder = Vec::new();
        stream.read_to_end(&mut remainder).unwrap();
        remainder
    });
    let error = pine.authenticate_mutual("correct-token").unwrap_err();
    assert!(matches!(error, Pcsx2BridgeError::Protocol(_)));
    assert!(pine.is_terminal());
    drop(pine);
    assert!(host.join().unwrap().is_empty());
}

#[test]
fn invalid_tokens_are_rejected_before_writing() {
    for token in [String::new(), "x".repeat(AUTH_TOKEN_MAX + 1)] {
        let (mut pine, mut stream) = connected_pair();
        let host = std::thread::spawn(move || {
            let mut received = Vec::new();
            stream.read_to_end(&mut received).unwrap();
            received
        });
        assert!(matches!(
            pine.authenticate_mutual(&token),
            Err(Pcsx2BridgeError::BadParams(_))
        ));
        assert!(!pine.is_terminal());
        drop(pine);
        assert!(host.join().unwrap().is_empty());
    }
}

#[test]
fn fragments_cannot_renew_the_whole_exchange_deadline() {
    for fragment_header in [false, true] {
        let (mut pine, mut stream) = connected_pair();
        let host = std::thread::spawn(move || {
            let mut request = [0; 5];
            stream.read_exact(&mut request).unwrap();
            let packet = [13, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8];
            let tail = if fragment_header {
                &packet[..]
            } else {
                stream.write_all(&packet[..5]).unwrap();
                &packet[5..]
            };
            for byte in tail {
                std::thread::sleep(Duration::from_millis(15));
                if stream.write_all(&[*byte]).is_err() {
                    break;
                }
            }
        });
        pine.stream
            .set_timeouts(Some(Duration::from_secs(1)), Some(Duration::from_secs(2)))
            .unwrap();
        let start = Instant::now();
        assert!(pine
            .transact_with_timeout(&[MSG_STATUS], Duration::from_millis(60))
            .is_err());
        assert!(start.elapsed() < Duration::from_secs(1));
        assert!(pine.is_terminal());
        assert_eq!(
            pine.stream.timeouts().unwrap(),
            (Some(Duration::from_secs(1)), Some(Duration::from_secs(2)))
        );
        assert!(pine.transact(&[MSG_STATUS]).is_err());
        drop(pine);
        host.join().unwrap();
    }
}
#[test]
fn invalid_budget_is_rejected_before_dispatch_and_keeps_transport_usable() {
    let (mut pine, mut stream) = connected_pair();
    let (done, finish) = std::sync::mpsc::channel();
    let host = std::thread::spawn(move || {
        let mut request = [0; 5];
        stream.read_exact(&mut request).unwrap();
        assert_eq!(request, [5, 0, 0, 0, MSG_STATUS]);
        stream.write_all(&[5, 0, 0, 0, 0]).unwrap();
        let _ = finish.recv_timeout(Duration::from_secs(2));
    });
    assert!(pine
        .transact_with_timeout(&[MSG_VERSION], Duration::ZERO)
        .is_err());
    assert!(!pine.is_terminal());
    let result = pine.transact_with_timeout(&[MSG_STATUS], Duration::from_millis(200));
    assert!(result.is_ok(), "{result:?}");
    done.send(()).unwrap();
    host.join().unwrap();
}
