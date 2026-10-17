use super::*;
use emucap::live::broker_link;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::sync::Mutex;
use std::time::Duration;

static ENV_LOCK: Mutex<()> = Mutex::new(());

#[test]
fn unknown_bind_conflicts_fail_and_release_the_partial_election() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvRestore::new(&["EMUCAP_EMU_HOME", "EMUCAP_PORT"]);
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", home.path());
    for occupied_producer in [true, false] {
        let producer = TcpListener::bind("127.0.0.1:0").unwrap();
        let emu_port = producer.local_addr().unwrap().port();
        let session = TcpListener::bind("127.0.0.1:0").unwrap();
        let sess_port = session.local_addr().unwrap().port();
        std::env::set_var("EMUCAP_PORT", emu_port.to_string());
        let held = if occupied_producer {
            drop(session);
            producer
        } else {
            drop(producer);
            session
        };
        let started = std::time::Instant::now();
        assert_eq!(
            elect_broker(emu_port, sess_port).unwrap_err().kind(),
            std::io::ErrorKind::AddrInUse
        );
        assert!(started.elapsed() < Duration::from_secs(2));
        if !occupied_producer {
            TcpListener::bind(("127.0.0.1", emu_port)).unwrap();
        }
        assert!(RuntimeStore::discover()
            .unwrap()
            .read_broker_controller_token(emu_port)
            .unwrap()
            .is_none());
        drop(held);
    }
}

#[test]
fn duplicate_election_accepts_an_authenticated_empty_incumbent_without_rotating() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvRestore::new(&["EMUCAP_EMU_HOME", "EMUCAP_PORT"]);
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", home.path());
    let producer = TcpListener::bind("127.0.0.1:0").unwrap();
    let session = TcpListener::bind("127.0.0.1:0").unwrap();
    let emu_port = producer.local_addr().unwrap().port();
    let sess_port = session.local_addr().unwrap().port();
    std::env::set_var("EMUCAP_PORT", emu_port.to_string());
    let token = mint_and_publish_session_token(emu_port).unwrap();
    let serving_token = token.clone();
    std::thread::spawn(move || broker::serve(producer, session, serving_token));
    assert!(elect_broker(emu_port, sess_port).unwrap().is_none());
    assert_eq!(
        RuntimeStore::discover()
            .unwrap()
            .read_broker_controller_token(emu_port)
            .unwrap(),
        Some(token)
    );
}

#[test]
fn election_reloads_credentials_while_a_concurrent_winner_becomes_ready() {
    let _lock = ENV_LOCK.lock().unwrap_or_else(|p| p.into_inner());
    let _env = EnvRestore::new(&["EMUCAP_EMU_HOME", "EMUCAP_PORT"]);
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", home.path());
    let producer = TcpListener::bind("127.0.0.1:0").unwrap();
    let session = TcpListener::bind("127.0.0.1:0").unwrap();
    let emu_port = producer.local_addr().unwrap().port();
    let sess_port = session.local_addr().unwrap().port();
    std::env::set_var("EMUCAP_PORT", emu_port.to_string());
    let store = RuntimeStore::discover().unwrap();
    store
        .write_broker_controller_token(emu_port, &broker::new_session_token())
        .unwrap();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        let current = broker::new_session_token();
        store
            .write_broker_controller_token(emu_port, &current)
            .unwrap();
        broker::serve(producer, session, current);
    });
    assert!(elect_broker(emu_port, sess_port).unwrap().is_none());
}

struct EnvRestore(Vec<(&'static str, Option<OsString>)>);

impl EnvRestore {
    fn new(keys: &[&'static str]) -> Self {
        Self(
            keys.iter()
                .map(|key| (*key, std::env::var_os(key)))
                .collect(),
        )
    }
}

impl Drop for EnvRestore {
    fn drop(&mut self) {
        for (key, value) in &self.0 {
            match value {
                Some(value) => std::env::set_var(key, value),
                None => std::env::remove_var(key),
            }
        }
    }
}

#[test]
fn launch_persists_fresh_auth_under_the_producer_port_for_every_generation() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::new(&[
        "EMUCAP_EMU_HOME",
        "EMUCAP_PORT",
        "EMUCAP_BROKER_SESSION_PORT",
        broker::SESSION_TOKEN_ENV,
    ]);
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", home.path());
    std::env::set_var("EMUCAP_PORT", "49323");
    std::env::set_var("EMUCAP_BROKER_SESSION_PORT", "49423");
    std::env::set_var(
        broker::SESSION_TOKEN_ENV,
        "stable-override-must-not-be-used",
    );
    let (producer_port, session_port) = configured_ports();
    assert_eq!(producer_port, 49323);
    assert_eq!(session_port, 49423);

    let store = RuntimeStore::discover().unwrap();
    let legacy = "0000000000000000-dead-beef";
    store
        .write_compatibility_token(producer_port, legacy)
        .unwrap();
    let first = mint_and_publish_session_token(producer_port).unwrap();
    assert_ne!(first, legacy);
    assert_ne!(first, "stable-override-must-not-be-used");
    assert!(broker::generated_session_token_is_current(&first));
    assert_eq!(
        store.read_broker_controller_token(producer_port).unwrap(),
        Some(first.clone())
    );
    assert_eq!(
        store.read_compatibility_token(producer_port).unwrap(),
        Some(broker::producer_token_from_controller(&first))
    );
    assert_eq!(store.read_compatibility_token(session_port).unwrap(), None);
    let second = mint_and_publish_session_token(producer_port).unwrap();
    assert_ne!(second, first, "every elected broker generation must rotate");
    assert_eq!(
        store.read_broker_controller_token(producer_port).unwrap(),
        Some(second.clone())
    );
    assert_ne!(
        broker::producer_token_from_controller(&first),
        broker::producer_token_from_controller(&second)
    );
    assert_eq!(
        store.read_compatibility_token(producer_port).unwrap(),
        Some(broker::producer_token_from_controller(&second))
    );
}

#[test]
fn prebound_session_cannot_impersonate_and_prior_token_cannot_authenticate_new_broker() {
    let _lock = ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _env = EnvRestore::new(&["EMUCAP_EMU_HOME", "EMUCAP_PORT", broker::SESSION_TOKEN_ENV]);
    let home = tempfile::tempdir().unwrap();
    std::env::set_var("EMUCAP_EMU_HOME", home.path());
    std::env::set_var(
        broker::SESSION_TOKEN_ENV,
        "stable-override-must-not-be-used",
    );

    let emu_probe = TcpListener::bind("127.0.0.1:0").unwrap();
    let emu_port = emu_probe.local_addr().unwrap().port();
    drop(emu_probe);
    std::env::set_var("EMUCAP_PORT", emu_port.to_string());
    let attacker = TcpListener::bind("127.0.0.1:0").unwrap();
    let session_addr = attacker.local_addr().unwrap();

    let store = RuntimeStore::discover().unwrap();
    let prior_generation = broker::new_session_token();
    store
        .write_broker_controller_token(emu_port, &prior_generation)
        .unwrap();

    // A broker candidate may acquire the producer port, but losing the session-port election
    // must leave the prior token untouched because provisioning happens only after both binds.
    let losing_emu = TcpListener::bind(("127.0.0.1", emu_port)).unwrap();
    assert!(TcpListener::bind(session_addr).is_err());
    drop(losing_emu);
    assert_eq!(
        store.read_broker_controller_token(emu_port).unwrap(),
        Some(prior_generation.clone())
    );

    // The pre-bound listener sees only a nonce and cannot fake broker ownership or solicit the
    // bearer. BrokerLink refuses to send attach after an invalid server proof.
    let fake = std::thread::spawn(move || {
        let (mut attacker_stream, _) = attacker.accept().unwrap();
        attacker_stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut attacker_reader = BufReader::new(attacker_stream.try_clone().unwrap());
        let mut auth_init = String::new();
        attacker_reader.read_line(&mut auth_init).unwrap();
        assert!(auth_init.contains("broker_authenticate"));
        assert!(!auth_init.contains(&prior_generation));
        writeln!(
            attacker_stream,
            "{}",
            serde_json::json!({"id":0,"ok":true,"result":{
                "protocol_version":1,
                "server_nonce":broker::new_auth_nonce(),
                "server_proof":"0000000000000000000000000000000000000000000000000000000000000000",
            }})
        )
        .unwrap();
        let mut forbidden_attach = String::new();
        assert_eq!(attacker_reader.read_line(&mut forbidden_attach).unwrap(), 0);
    });
    let fake_error =
        broker_link::connect(&session_addr.to_string(), None, Duration::from_secs(2)).unwrap_err();
    assert_eq!(fake_error.kind(), "identity_mismatch");
    fake.join().unwrap();

    let captured_token = store
        .read_broker_controller_token(emu_port)
        .unwrap()
        .unwrap();

    // Once the legitimate process wins both ports it rotates and atomically publishes a new
    // generation credential before accepting. The captured bearer is now stale.
    let emu = TcpListener::bind(("127.0.0.1", emu_port)).unwrap();
    let sess = TcpListener::bind(session_addr).unwrap();
    let current_generation = mint_and_publish_session_token(emu_port).unwrap();
    assert_ne!(current_generation, captured_token);
    std::thread::spawn(move || broker::serve(emu, sess, current_generation));

    let mut stale = TcpStream::connect(session_addr).unwrap();
    stale
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let client_nonce = broker::new_auth_nonce();
    writeln!(
        stale,
        "{}",
        serde_json::json!({"v":1,"id":0,"method":"broker_authenticate","params":{
            "protocol_version":1,"client_nonce":client_nonce.clone(),
        }})
    )
    .unwrap();
    let mut stale_reader = BufReader::new(stale.try_clone().unwrap());
    let mut auth_response = String::new();
    stale_reader.read_line(&mut auth_response).unwrap();
    let auth_response: serde_json::Value = serde_json::from_str(auth_response.trim()).unwrap();
    let server_nonce = auth_response["result"]["server_nonce"].as_str().unwrap();
    let stale_expected = broker::session_server_proof(&captured_token, &client_nonce, server_nonce);
    assert!(!broker::auth_proof_matches(
        &stale_expected,
        auth_response["result"]["server_proof"].as_str().unwrap()
    ));
    let mut params = serde_json::Map::new();
    let binding = broker::session_attach_binding(&params);
    params.insert(
        "auth_proof".into(),
        serde_json::Value::String(broker::session_client_proof(
            &captured_token,
            &client_nonce,
            server_nonce,
            &binding,
        )),
    );
    writeln!(
        stale,
        "{}",
        serde_json::json!({"v":1,"id":1,"method":"attach","params":params})
    )
    .unwrap();
    let mut rejected = String::new();
    stale_reader.read_line(&mut rejected).unwrap();
    let rejected: serde_json::Value = serde_json::from_str(rejected.trim()).unwrap();
    assert_eq!(rejected["error"]["kind"], "identity_mismatch");
}
