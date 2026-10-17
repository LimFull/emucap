use super::*;
use std::io::{BufRead, Read};

const TOKEN: &str = "test-admission-token";
const PRODUCER_LAUNCH: &str = "launch-01arz3ndektsv4rrffq69g5fav";

fn pair(role: Role) -> (Candidate, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let peer = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
    peer.set_read_timeout(Some(Duration::from_secs(1))).unwrap();
    peer.set_nodelay(true).unwrap();
    let (stream, _) = listener.accept().unwrap();
    (Candidate::new(stream, role).unwrap(), peer)
}

fn poll_until(candidate: &mut Candidate, predicate: impl Fn(&Progress) -> bool) -> Progress {
    let end = Instant::now() + Duration::from_secs(1);
    loop {
        let progress = candidate.poll(TOKEN).unwrap();
        if predicate(&progress) {
            return progress;
        }
        assert!(Instant::now() < end, "candidate made no progress");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn producer_hello(padding: usize) -> String {
    json!({"id":0,"ok":true,"result":{
        "protocol_version":PROTOCOL_VERSION,
        "name":"large","methods":["status"],"extension":"x".repeat(padding)
    }})
    .to_string()
        + "\n"
}

fn finish_producer_auth(mut candidate: Candidate, peer: TcpStream) -> (Candidate, TcpStream) {
    let port = peer.peer_addr().unwrap().port();
    let front_key = crate::live::producer_auth::derive_front_key(
        TOKEN,
        crate::live::producer_auth::Boundary::BrokerProducer,
        port,
        PRODUCER_LAUNCH,
    )
    .unwrap();
    let producer = std::thread::spawn(move || {
        crate::live::producer_auth::authenticate_producer_stream(
            peer,
            &front_key,
            crate::live::producer_auth::Boundary::BrokerProducer,
            port,
            PRODUCER_LAUNCH,
            Duration::from_secs(1),
        )
        .unwrap()
    });
    let end = Instant::now() + Duration::from_secs(1);
    while !matches!(candidate.stage, Stage::ProducerHello { .. }) || candidate.output.is_some() {
        assert!(matches!(candidate.poll(TOKEN).unwrap(), Progress::Pending));
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    (candidate, producer.join().unwrap())
}

fn init(client: &str) -> String {
    json!({"v":PROTOCOL_VERSION,"id":0,"method":"broker_authenticate","params":{
        "protocol_version":PROTOCOL_VERSION,"client_nonce":client
    }})
    .to_string()
        + "\n"
}

fn finish_init(candidate: &mut Candidate, peer: &mut TcpStream) -> (String, String) {
    let client = super::super::new_auth_nonce();
    for fragment in init(&client).as_bytes().chunks(7) {
        peer.write_all(fragment).unwrap();
        assert!(matches!(candidate.poll(TOKEN).unwrap(), Progress::Pending));
    }
    let end = Instant::now() + Duration::from_secs(1);
    while !matches!(candidate.stage, Stage::ControllerAttach { .. }) || candidate.output.is_some() {
        assert!(matches!(candidate.poll(TOKEN).unwrap(), Progress::Pending));
        assert!(Instant::now() < end);
        std::thread::sleep(Duration::from_millis(1));
    }
    let mut response = String::new();
    BufReader::new(peer.try_clone().unwrap())
        .read_line(&mut response)
        .unwrap();
    let response: Value = serde_json::from_str(&response).unwrap();
    let server = response["result"]["server_nonce"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_eq!(
        response["result"]["server_proof"],
        super::super::session_server_proof(TOKEN, &client, &server)
    );
    (client, server)
}

fn attach(client: &str, server: &str) -> String {
    let mut params = json!({"name":"large"});
    let binding = super::super::session_attach_binding(params.as_object().unwrap());
    params["auth_proof"] =
        super::super::session_client_proof(TOKEN, client, server, &binding).into();
    json!({"v":PROTOCOL_VERSION,"id":1,"method":"attach","params":params}).to_string() + "\n"
}

#[test]
fn fragmented_maximum_hello_preserves_extensions_and_pipeline() {
    let (candidate, peer) = pair(Role::Producer);
    let (mut candidate, mut peer) = finish_producer_auth(candidate, peer);
    let mut hello_request = String::new();
    BufReader::new(peer.try_clone().unwrap())
        .read_line(&mut hello_request)
        .unwrap();
    assert!(hello_request.contains("hello"));
    let padding = MAX_ADMISSION_FRAME_BYTES - (producer_hello(0).len() - 1);
    let hello = producer_hello(padding);
    assert_eq!(hello.len(), MAX_ADMISSION_FRAME_BYTES + 1);
    let (last, prefix) = hello.as_bytes().split_last().unwrap();
    for part in prefix.chunks(997) {
        peer.write_all(part).unwrap();
        assert!(matches!(candidate.poll(TOKEN).unwrap(), Progress::Pending));
    }
    peer.write_all(&[*last]).unwrap();
    peer.write_all(b"{\"event\":\"following\"}\n").unwrap();
    let Progress::Authenticated(result) =
        poll_until(&mut candidate, |p| matches!(p, Progress::Authenticated(_)))
    else {
        panic!()
    };
    assert_eq!(result["extension"].as_str().unwrap().len(), padding);
    assert!(result.get("session_token").is_none());
    let mut reader = candidate.into_reader().unwrap();
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    assert_eq!(line, "{\"event\":\"following\"}\n");
}

#[test]
fn fragmented_two_flight_controller_preserves_pipelined_request() {
    let (mut candidate, mut peer) = pair(Role::Controller);
    let deadline = candidate.deadline;
    let (client, server) = finish_init(&mut candidate, &mut peer);
    let request = attach(&client, &server);
    for fragment in request.as_bytes()[..request.len() - 1].chunks(13) {
        peer.write_all(fragment).unwrap();
        assert!(matches!(candidate.poll(TOKEN).unwrap(), Progress::Pending));
    }
    peer.write_all(b"\n{\"v\":1,\"id\":2,\"method\":\"status\",\"params\":{}}\n")
        .unwrap();
    let Progress::Authenticated(value) =
        poll_until(&mut candidate, |p| matches!(p, Progress::Authenticated(_)))
    else {
        panic!()
    };
    assert_eq!(value["params"]["name"], "large");
    assert_eq!(candidate.deadline, deadline);
    let mut reader = candidate.into_reader().unwrap();
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    let mut following = String::new();
    reader.read_line(&mut following).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&following).unwrap()["method"],
        "status"
    );
}

#[test]
fn deadline_covers_second_flight_and_pending_writes() {
    let (mut candidate, mut peer) = pair(Role::Controller);
    let original_deadline = candidate.deadline;
    let (client, server) = finish_init(&mut candidate, &mut peer);
    assert_eq!(candidate.deadline, original_deadline);
    peer.write_all(attach(&client, &server).as_bytes()).unwrap();
    candidate.deadline = Instant::now();
    assert!(matches!(candidate.poll(TOKEN).unwrap(), Progress::Closed));
    assert!(candidate.into_reader().is_err());

    let (mut candidate, _nonreader) = pair(Role::Producer);
    assert!(candidate.output.is_some());
    candidate.deadline = Instant::now();
    assert!(matches!(candidate.poll(TOKEN).unwrap(), Progress::Closed));
    assert_eq!(candidate.output.unwrap().offset, 0);
}

#[test]
fn partial_output_and_backpressure_do_not_repeat_bytes_or_spin() {
    struct Writer {
        calls: usize,
        bytes: Vec<u8>,
    }
    impl Write for Writer {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.calls += 1;
            assert!(bytes.len() <= IO_CHUNK);
            match self.calls {
                1 => Err(io::ErrorKind::WouldBlock.into()),
                2 => Err(io::ErrorKind::Interrupted.into()),
                _ => {
                    let n = bytes.len().min(17);
                    self.bytes.extend_from_slice(&bytes[..n]);
                    Ok(n)
                }
            }
        }
        fn flush(&mut self) -> io::Result<()> {
            panic!("nonblocking output must not flush")
        }
    }
    let expected = "a".repeat(IO_CHUNK + 3) + "\n";
    let mut output = PendingWrite::line(expected.clone());
    let mut writer = Writer {
        calls: 0,
        bytes: vec![],
    };
    assert!(!output.poll(&mut writer).unwrap());
    assert_eq!(writer.calls, 1);
    assert_eq!(output.offset, 0);
    assert!(!output.poll(&mut writer).unwrap());
    assert_eq!(writer.calls, 2);
    while !output.poll(&mut writer).unwrap() {}
    assert_eq!(writer.bytes, expected.as_bytes());
}

#[test]
fn bounded_accept_turn_evicts_oldest_without_expanding_queue() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut held = Vec::new();
    let mut candidates = VecDeque::new();
    for _ in 0..PER_ROLE_LIMIT {
        held.push(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        accept_candidates(&listener, &mut candidates, Role::Controller);
    }
    let deadline = Instant::now() + Duration::from_secs(1);
    while candidates.len() < PER_ROLE_LIMIT {
        accept_candidates(&listener, &mut candidates, Role::Controller);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(candidates.len(), PER_ROLE_LIMIT);
    let newest_old = candidates
        .back()
        .unwrap()
        .reader
        .get_ref()
        .peer_addr()
        .unwrap();
    for _ in 0..ACCEPTS_PER_TURN + 1 {
        held.push(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
    }
    accept_candidates(&listener, &mut candidates, Role::Controller);
    assert_eq!(candidates.len(), PER_ROLE_LIMIT);
    let position = candidates
        .iter()
        .position(|c| c.reader.get_ref().peer_addr().unwrap() == newest_old)
        .unwrap();
    assert!(position >= PER_ROLE_LIMIT - ACCEPTS_PER_TURN - 1);
    let newest = held.last().unwrap().local_addr().unwrap();
    assert_ne!(
        candidates
            .back()
            .unwrap()
            .reader
            .get_ref()
            .peer_addr()
            .unwrap(),
        newest
    );
    let deadline = Instant::now() + Duration::from_secs(1);
    while candidates
        .back()
        .unwrap()
        .reader
        .get_ref()
        .peer_addr()
        .unwrap()
        != newest
    {
        accept_candidates(&listener, &mut candidates, Role::Controller);
        assert_eq!(candidates.len(), PER_ROLE_LIMIT);
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(1));
    }
    let first = &mut held[0];
    first
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    assert_eq!(first.read(&mut [0]).unwrap(), 0);
}

#[test]
fn complete_large_hello_survives_full_accept_turns() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut candidates = VecDeque::new();
    let mut held = Vec::new();
    for _ in 0..PER_ROLE_LIMIT - 1 {
        let (candidate, peer) = pair(Role::Producer);
        candidates.push_back(candidate);
        held.push(peer);
    }
    let (candidate, legitimate) = pair(Role::Producer);
    let (candidate, mut legitimate) = finish_producer_auth(candidate, legitimate);
    legitimate
        .write_all(producer_hello(60 * 1024).as_bytes())
        .unwrap();
    candidates.push_back(candidate);
    let reg = Arc::new(std::sync::Mutex::new(super::super::Registry::default()));
    // A finite four-turn burst, with the complete valid frame already available.
    // Serve ready candidates before each batch of eight new silent peers.
    for _ in 0..PER_ROLE_LIMIT / ACCEPTS_PER_TURN {
        service(&mut candidates, &reg, TOKEN, Role::Producer);
        for _ in 0..ACCEPTS_PER_TURN {
            held.push(TcpStream::connect(listener.local_addr().unwrap()).unwrap());
        }
        let newest = held.last().unwrap().local_addr().unwrap();
        let deadline = Instant::now() + Duration::from_secs(1);
        while candidates
            .back()
            .is_none_or(|c| c.reader.get_ref().peer_addr().unwrap() != newest)
        {
            accept_candidates(&listener, &mut candidates, Role::Producer);
            assert!(Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(candidates.len() <= PER_ROLE_LIMIT);
    }
    let deadline = Instant::now() + Duration::from_millis(100);
    while !super::super::lock(&reg).emus.contains_key("large") && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(
        super::super::lock(&reg).emus.contains_key("large"),
        "a complete valid hello was evicted by a finite burst before authentication"
    );
}

#[test]
fn unauthenticated_attach_rejection_stays_bounded_and_does_not_disclose_registry() {
    let (mut candidate, mut peer) = pair(Role::Controller);
    finish_init(&mut candidate, &mut peer);
    peer.write_all(b"{\"v\":1,\"id\":1,\"method\":\"attach\",\"params\":{\"name\":\"secret\"}}\n")
        .unwrap();
    poll_until(&mut candidate, |p| matches!(p, Progress::Closed));
    let mut response = String::new();
    BufReader::new(peer).read_line(&mut response).unwrap();
    let response: Value = serde_json::from_str(&response).unwrap();
    assert_eq!(response["error"]["kind"], "identity_mismatch");
    assert!(response.get("result").is_none());
    assert!(response["error"].get("names").is_none());
}
