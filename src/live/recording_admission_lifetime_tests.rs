use super::*;
use std::io::Write;
use std::sync::{mpsc, Arc};

#[test]
fn fragmented_authentication_and_cancellation_during_wait() {
    for cancel in [false, true] {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = stop.clone();
        let (tx, rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let result = accept_authenticated(
                &listener,
                "t",
                "c",
                Instant::now() + Duration::from_secs(10),
                &worker_stop,
                Duration::from_millis(20),
            );
            tx.send(result.is_ok()).unwrap();
        });
        let mut peer = TcpStream::connect(addr).unwrap();
        peer.write_all(b"{\"token\":").unwrap();
        assert!(rx.recv_timeout(Duration::from_millis(30)).is_err());
        if cancel {
            stop.store(true, Ordering::Release);
        } else {
            peer.write_all(b"\"t\",\"capture_id\":\"c\"}\n").unwrap();
        }
        assert_eq!(rx.recv_timeout(Duration::from_secs(1)).unwrap(), !cancel);
        worker.join().unwrap();
    }
}
