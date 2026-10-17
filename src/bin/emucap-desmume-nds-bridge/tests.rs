use super::parse_endpoint;

#[test]
fn gdb_endpoint_accepts_only_numeric_loopback_addresses() {
    assert_eq!(
        parse_endpoint("127.0.0.1:48100").unwrap(),
        ("127.0.0.1".to_string(), 48100)
    );
    assert_eq!(
        parse_endpoint("::1:48101").unwrap(),
        ("::1".to_string(), 48101)
    );

    for endpoint in ["192.0.2.10:48100", "localhost:48100", "0.0.0.0:48100"] {
        assert!(parse_endpoint(endpoint).is_err(), "accepted {endpoint}");
    }
}
