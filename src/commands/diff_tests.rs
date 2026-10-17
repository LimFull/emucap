use super::*;

#[test]
fn largest_complete_report_remains_a_readable_baseline() {
    let report = diff::DiffReport {
        regions: vec![diff::RegionDiff {
            name: "ram".into(),
            compared: 1_000_000,
            differing: 1_000_000,
            first_divergence: None,
            a_len: 1_000_000,
            b_len: 1_000_000,
            divergences: (1_000_000_000..1_001_000_000).collect(),
        }],
        unmatched: vec![],
    };
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), diff::render_json(&report)).unwrap();
    assert!(load_baseline(file.path()).is_ok());
}
