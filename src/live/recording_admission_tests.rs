use super::*;
use std::io::{Read, Write};
use std::net::Shutdown;
use std::sync::Arc;

#[test]
fn bad_and_stalled_peers_do_not_own_sink_or_consume_payload() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let worker_stop = stop.clone();
    let worker = std::thread::spawn(move || {
        let mut reader = accept_authenticated(
            &listener,
            "secret",
            "capture",
            Instant::now() + Duration::from_secs(3),
            &worker_stop,
            Duration::from_millis(50),
        )
        .unwrap();
        let mut payload = String::new();
        reader.read_to_string(&mut payload).unwrap();
        payload
    });
    let mut peers = Vec::new();
    for data in [
        b"bad json\n".as_slice(),
        b"{\"token\":\"wrong\",\"capture_id\":\"capture\"}\n",
        b"{\"token\":\"secret\",\"capture_id\":\"wrong\"}\n",
        b"{",
        b"",
    ] {
        let mut peer = TcpStream::connect(address).unwrap();
        peer.write_all(data).unwrap();
        peers.push(peer);
    }
    let mut oversized = TcpStream::connect(address).unwrap();
    oversized
        .write_all(&vec![b'x'; HANDSHAKE_BYTES + 1])
        .unwrap();
    drop(TcpStream::connect(address).unwrap()); // early EOF
                                                // More than the entire candidate budget must not permanently occupy admission.
    for _ in 0..MAX_CANDIDATES + 4 {
        peers.push(TcpStream::connect(address).unwrap());
    }
    let mut producer = TcpStream::connect(address).unwrap();
    producer
        .write_all(b"{\"token\":\"secret\",\"capture_id\":\"capture\"}\npayload\n")
        .unwrap();
    producer.shutdown(Shutdown::Write).unwrap();
    assert_eq!(worker.join().unwrap(), "payload\n");
}

#[test]
fn candidate_activity_cannot_extend_deadline_and_stop_is_prompt() {
    for cancel in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let _peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let stop = AtomicBool::new(cancel);
        let start = Instant::now();
        assert!(accept_authenticated(
            &listener,
            "secret",
            "capture",
            start + Duration::from_millis(40),
            &stop,
            Duration::from_millis(20)
        )
        .is_err());
        assert!(start.elapsed() < Duration::from_millis(500));
    }
}
