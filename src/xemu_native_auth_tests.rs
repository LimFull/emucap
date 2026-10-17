use super::*;
use std::net::TcpListener;
use std::sync::mpsc;
use std::thread;

const TOKEN: &str =
    "native-0123456789abcdefghijklmnopqrstuvwxyz-ABCDEFGHIJKLMNOPQRSTUVWXYZ-0123456789";

fn read_record(stream: &mut TcpStream, length: usize) -> Vec<u8> {
    let mut bytes = vec![0; length];
    stream.read_exact(&mut bytes).unwrap();
    bytes
}

fn valid_server(
    listener: TcpListener,
    endpoint: XemuNativeEndpoint,
    suffix: &'static [u8],
    fragment: bool,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let hello = read_record(&mut stream, CLIENT_HELLO_SIZE);
        assert_eq!(&hello[..7], &header(CLIENT_HELLO, endpoint));
        let client_nonce: [u8; NONCE_SIZE] = hello[7..].try_into().unwrap();
        let server_nonce = [0x5a; NONCE_SIZE];
        let mut challenge = Vec::from(header(SERVER_PROOF, endpoint));
        challenge.extend_from_slice(&server_nonce);
        challenge.extend_from_slice(&proof(
            TOKEN.as_bytes(),
            endpoint,
            b"server",
            &client_nonce,
            &server_nonce,
        ));
        if fragment {
            for byte in challenge {
                stream.write_all(&[byte]).unwrap();
            }
        } else {
            stream.write_all(&challenge).unwrap();
        }

        let response = read_record(&mut stream, CLIENT_PROOF_SIZE);
        assert_eq!(&response[..7], &header(CLIENT_PROOF, endpoint));
        assert_eq!(
            &response[7..],
            &proof(
                TOKEN.as_bytes(),
                endpoint,
                b"client",
                &client_nonce,
                &server_nonce,
            )
        );
        let mut final_flight = Vec::from(header(SERVER_OK, endpoint));
        final_flight.extend_from_slice(suffix);
        stream.write_all(&final_flight).unwrap();
    })
}

#[test]
fn proofs_are_endpoint_separated_and_match_pinned_long_key_vectors() {
    let client = [0x11; NONCE_SIZE];
    let server = [0x22; NONCE_SIZE];
    assert_eq!(
        hex::encode(proof(
            TOKEN.as_bytes(),
            XemuNativeEndpoint::Qmp,
            b"server",
            &client,
            &server,
        )),
        "9d4fd2492dcd5297951a3c8b9d3468356fe62468880b323be863fcef60d87dfb"
    );
    assert_eq!(
        hex::encode(proof(
            TOKEN.as_bytes(),
            XemuNativeEndpoint::Gdb,
            b"server",
            &client,
            &server,
        )),
        "18b949bd1c0bf8b02ea6f57833e1985b5d45c1f220b14801439ccf82fe5e3615"
    );
}

#[test]
fn fragmented_exchange_preserves_bytes_after_success() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = valid_server(listener, XemuNativeEndpoint::Qmp, b"{\"QMP\":{}}\n", true);
    let stream = TcpStream::connect(address).unwrap();
    let mut stream = authenticate_stream(
        stream,
        TOKEN,
        XemuNativeEndpoint::Qmp,
        Duration::from_secs(2),
    )
    .unwrap();
    let mut greeting = String::new();
    stream.read_to_string(&mut greeting).unwrap();
    assert_eq!(greeting, "{\"QMP\":{}}\n");
    server.join().unwrap();
}

#[test]
fn counterfeit_server_receives_no_client_proof() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (sent, received) = mpsc::channel();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let hello = read_record(&mut stream, CLIENT_HELLO_SIZE);
        let mut challenge = Vec::from(header(SERVER_PROOF, XemuNativeEndpoint::Gdb));
        challenge.extend_from_slice(&[0x44; NONCE_SIZE]);
        challenge.extend_from_slice(&[0xff; PROOF_SIZE]);
        stream.write_all(&challenge).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut byte = [0];
        let count = match stream.read(&mut byte) {
            Ok(count) => count,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                0
            }
            Err(error) => panic!("unexpected read error: {error}"),
        };
        sent.send((hello, count)).unwrap();
    });
    let stream = TcpStream::connect(address).unwrap();
    let error = authenticate_stream(
        stream,
        TOKEN,
        XemuNativeEndpoint::Gdb,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let (hello, count) = received.recv().unwrap();
    assert_eq!(&hello[..7], &header(CLIENT_HELLO, XemuNativeEndpoint::Gdb));
    assert_eq!(count, 0);
    server.join().unwrap();
}

#[test]
fn invalid_token_is_rejected_before_connect() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let error = connect_authenticated(
        "127.0.0.1",
        address.port(),
        "",
        XemuNativeEndpoint::Qmp,
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(
        listener.accept().unwrap_err().kind(),
        io::ErrorKind::WouldBlock
    );
}

#[test]
fn old_qmp_greeting_fails_without_a_client_proof() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let _hello = read_record(&mut stream, CLIENT_HELLO_SIZE);
        let prefix = b"{\"QMP\":{\"version\":{},\"capabilities\":[]}}\r\n";
        let mut greeting = [b'x'; SERVER_PROOF_SIZE];
        greeting[..prefix.len()].copy_from_slice(prefix);
        stream.write_all(&greeting).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_millis(300)))
            .unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap_or(0), 0);
    });
    let stream = TcpStream::connect(address).unwrap();
    let error = authenticate_stream(
        stream,
        TOKEN,
        XemuNativeEndpoint::Qmp,
        Duration::from_secs(1),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    server.join().unwrap();
}
