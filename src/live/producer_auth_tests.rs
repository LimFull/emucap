use super::*;
use std::net::TcpListener;
use std::thread;

const LAUNCH: &str = "launch-01arz3ndektsv4rrffq69g5fav";

#[test]
fn keys_are_bound_to_launch_port_and_boundary() {
    let root = "reclaim-01arz3ndektsv4rrffq69g5fav01arz3ndektsv4rrffq69g5fav";
    let direct = derive_front_key(root, Boundary::Direct, 47800, LAUNCH).unwrap();
    assert_eq!(Boundary::from_front_key(&direct), Some(Boundary::Direct));
    assert_ne!(
        direct,
        derive_front_key(root, Boundary::Direct, 47801, LAUNCH).unwrap()
    );
    assert_ne!(
        direct,
        derive_front_key(root, Boundary::BrokerProducer, 47800, LAUNCH).unwrap()
    );
    assert_ne!(
        direct,
        derive_front_key(
            root,
            Boundary::Direct,
            47800,
            "launch-01arz3ndektsv4rrffq69g5faw"
        )
        .unwrap()
    );
}

#[test]
fn proofs_cannot_cross_root_challenge_port_boundary_or_direction() {
    let root = "direct-root";
    let port = 47800;
    let challenge = new_listener_challenge(Boundary::Direct, port).unwrap();
    let front = derive_front_key(root, Boundary::Direct, port, LAUNCH).unwrap();
    let (producer, _) = producer_proof_record(&challenge, &front, Boundary::Direct, LAUNCH);
    let auth = verify_producer_proof(root, &challenge, Boundary::Direct, port, &producer).unwrap();
    verify_producer_commit(&auth, &proof_record(PRODUCER_COMMIT, &auth, b"commit")).unwrap();

    let fresh = new_listener_challenge(Boundary::Direct, port).unwrap();
    assert!(verify_producer_proof(root, &fresh, Boundary::Direct, port, &producer).is_err());
    assert!(verify_producer_proof(
        "replacement-root",
        &challenge,
        Boundary::Direct,
        port,
        &producer,
    )
    .is_err());
    assert!(
        verify_producer_proof(root, &challenge, Boundary::Direct, port + 1, &producer).is_err()
    );
    assert!(
        verify_producer_proof(root, &challenge, Boundary::BrokerProducer, port, &producer).is_err()
    );
    assert!(verify_producer_commit(&auth, &listener_proof_record(&auth)).is_err());
}

#[test]
fn legitimate_producer_and_listener_complete() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let root = "broker-producer-root".to_string();
    let front = derive_front_key(&root, Boundary::BrokerProducer, port, LAUNCH).unwrap();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        authenticate_listener_stream(
            stream,
            &root,
            Boundary::BrokerProducer,
            port,
            Instant::now() + Duration::from_secs(1),
            None,
        )
        .map(|(_, launch)| launch)
    });
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    authenticate_producer_stream(
        stream,
        &front,
        Boundary::BrokerProducer,
        port,
        LAUNCH,
        Duration::from_secs(1),
    )
    .unwrap();
    assert_eq!(server.join().unwrap().unwrap(), LAUNCH);
}

#[test]
fn counterfeit_listener_gets_no_commit() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let front = derive_front_key("real-root", Boundary::Direct, port, LAUNCH).unwrap();
    let fake = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let challenge = new_listener_challenge(Boundary::Direct, port).unwrap();
        stream.write_all(challenge_bytes(&challenge)).unwrap();
        let mut producer = [0u8; PRODUCER_PROOF_SIZE];
        stream.read_exact(&mut producer).unwrap();
        let wrong = AuthenticatedTranscript {
            boundary: Boundary::Direct,
            launch_id: LAUNCH.into(),
            front_key: derive_front_key("wrong-root", Boundary::Direct, port, LAUNCH).unwrap(),
            transcript: vec![0; CHALLENGE_SIZE + LAUNCH_ID_SIZE + NONCE_SIZE],
        };
        stream.write_all(&listener_proof_record(&wrong)).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(1)))
            .unwrap();
        let mut byte = [0u8; 1];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
    });
    let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
    assert!(authenticate_producer_stream(
        stream,
        &front,
        Boundary::Direct,
        port,
        LAUNCH,
        Duration::from_secs(1),
    )
    .is_err());
    fake.join().unwrap();
}
