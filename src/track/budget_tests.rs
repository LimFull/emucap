use super::budget::{self, ScanBudget};
use super::{id::UlidGen, index, mcp_ops, model::*, ops, query, store, summary};
use std::io::{Cursor, Read};
use std::path::Path;

fn run(root: &Path, rom: &str) -> Run {
    ops::create_run(
        root,
        &UlidGen,
        "t0",
        rom,
        None,
        None,
        vec![],
        Some("connection".into()),
    )
    .unwrap()
}
fn limited(error: store::TrackError, resource: &str) {
    assert!(matches!(error, store::TrackError::ResourceLimit { resource: r, .. } if r == resource));
}

#[test]
fn scan_exact_limits_and_ignored_entries() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let r = run(root, "sha-a");
    let size = std::fs::metadata(
        store::run_dir(root, "sha-a", &r.id)
            .unwrap()
            .join("run.json"),
    )
    .unwrap()
    .len();
    let mut b = ScanBudget {
        entries: 2,
        records: 1,
        bytes: size,
        elements: 0,
    };
    assert_eq!(store::walk_runs_budgeted(root, &mut b).unwrap().len(), 1);
    assert_eq!((b.entries, b.records, b.bytes), (0, 0, 0));
    std::fs::write(root.join("roms/ignored"), "").unwrap();
    let mut b = ScanBudget {
        entries: 2,
        ..Default::default()
    };
    limited(
        store::walk_runs_lenient_budgeted(root, &mut b).unwrap_err(),
        "directory entries",
    );
    let mut b = ScanBudget {
        records: 0,
        ..Default::default()
    };
    limited(
        store::walk_runs_lenient_budgeted(root, &mut b).unwrap_err(),
        "candidate records",
    );
}

#[test]
fn corrupt_records_consume_bytes_and_limit_errors_are_not_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    std::fs::create_dir_all(root.join("findings")).unwrap();
    std::fs::write(root.join("findings/a.json"), "invalid").unwrap();
    std::fs::write(root.join("findings/b.json"), "invalid").unwrap();
    let mut b = ScanBudget {
        bytes: 14,
        ..Default::default()
    };
    let (records, skipped) = store::walk_findings_lenient_budgeted(root, &mut b).unwrap();
    assert!(records.is_empty());
    assert_eq!(skipped.len(), 2);
    assert_eq!(b.bytes, 0);
    let mut b = ScanBudget {
        bytes: 13,
        ..Default::default()
    };
    limited(
        store::walk_findings_lenient_budgeted(root, &mut b).unwrap_err(),
        "input bytes",
    );
}

#[test]
fn actual_read_growth_and_partial_io_count_against_allowance() {
    let path = Path::new("fixture.json");
    let mut b = ScanBudget {
        bytes: 4,
        ..Default::default()
    };
    let mut source = Cursor::new(b"abcdefg");
    limited(b.read(&mut source, 1, path).unwrap_err(), "input bytes");
    assert_eq!(source.position(), 5); // Remaining allowance plus one detection byte.
    struct FailAfterBytes(bool);
    impl Read for FailAfterBytes {
        fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
            if self.0 {
                return Err(std::io::Error::other("injected"));
            }
            self.0 = true;
            output[..2].copy_from_slice(b"{}");
            Ok(2)
        }
    }
    let mut b = ScanBudget {
        bytes: 4,
        ..Default::default()
    };
    assert!(matches!(
        b.read(FailAfterBytes(false), 0, path),
        Err(store::TrackError::Io(_))
    ));
    assert_eq!(b.bytes, 2);
    assert_eq!(b.read(Cursor::new(b"{}"), 2, path).unwrap(), b"{}");
    assert_eq!(b.bytes, 0);
}

#[test]
fn reindex_shares_mixed_record_budget_and_preserves_previous_index() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    run(root, "sha-a");
    store::save_rom(
        root,
        &Rom {
            sha1: "sha-a".into(),
            platform: "snes".into(),
            title: None,
            first_seen: "t".into(),
        },
    )
    .unwrap();
    store::save_finding(
        root,
        &Finding {
            id: "f1".into(),
            rom_sha1: "sha-a".into(),
            run_id: None,
            claim: "fact".into(),
            evidence_refs: vec![],
            promoted: false,
            created_at: "t".into(),
        },
    )
    .unwrap();
    let conn = index::open_index(&root.join("index.sqlite")).unwrap();
    let mut b = ScanBudget {
        records: 3,
        ..Default::default()
    };
    index::reindex_budgeted(root, &conn, &mut b).unwrap();
    assert_eq!(b.records, 0);
    conn.execute("UPDATE run SET goal='prior'", []).unwrap();
    for lenient in [false, true] {
        let mut b = ScanBudget {
            records: 2,
            ..Default::default()
        };
        let result = if lenient {
            index::reindex_lenient_budgeted(root, &conn, &mut b).map(|r| r.0)
        } else {
            index::reindex_budgeted(root, &conn, &mut b)
        };
        assert!(matches!(
            result,
            Err(index::IndexError::Track(
                store::TrackError::ResourceLimit { .. }
            ))
        ));
        let goal: String = conn
            .query_row("SELECT goal FROM run", [], |r| r.get(0))
            .unwrap();
        assert_eq!(goal, "prior");
    }
}

#[test]
fn nested_elements_are_admitted_before_index_and_summary_projection() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let mut r = run(root, "sha-a");
    r.tags = vec![String::new(); budget::MAX_SCAN_ELEMENTS as usize + 1];
    store::save_run(root, &r).unwrap();
    assert_eq!(
        mcp_ops::summarize_runs(root, summary::SummaryFilter::default())
            .err()
            .map(|e| e.code().to_owned()),
        Some("resource_limit".into())
    );
    assert_eq!(
        mcp_ops::query_runs(root, query::RunFilter::default())
            .err()
            .map(|e| e.code().to_owned()),
        Some("resource_limit".into())
    );
    let before = std::fs::read(
        store::run_dir(root, "sha-a", &r.id)
            .unwrap()
            .join("run.json"),
    )
    .unwrap();
    assert_eq!(
        mcp_ops::start_run(
            root,
            &UlidGen,
            "t1",
            "sha-b",
            Some("connection".into()),
            None,
            None,
            vec![]
        )
        .err()
        .map(|e| e.code().to_owned()),
        Some("resource_limit".into())
    );
    assert_eq!(
        std::fs::read(
            store::run_dir(root, "sha-a", &r.id)
                .unwrap()
                .join("run.json")
        )
        .unwrap(),
        before
    );
    assert!(!root.join("roms/sha-b").exists());
}

#[test]
fn projected_json_counts_escaping_and_pretty_indentation_without_allocating_output() {
    let value = serde_json::json!({"tags":["\u{0}\"\\", "text"]});
    let size = serde_json::to_vec_pretty(&value).unwrap().len() as u64;
    budget::check_result_with_limit(&value, size).unwrap();
    limited(
        budget::check_result_with_limit(&value, size - 1).unwrap_err(),
        "result bytes",
    );
}

#[test]
fn direct_sql_queries_admit_rows_and_strings_before_copying() {
    let dir = tempfile::tempdir().unwrap();
    let conn = index::open_index(&dir.path().join("index.sqlite")).unwrap();
    conn.execute(
        "INSERT INTO run (id,rom_sha1,status,started_at) VALUES ('r','rom','done','t')",
        [],
    )
    .unwrap();
    assert_eq!(
        query::query_runs(&conn, &Default::default()).unwrap().len(),
        1
    );
    conn.execute(
        "UPDATE run SET goal=zeroblob(?1)",
        [budget::MAX_RESULT_BYTES as i64 + 1],
    )
    .unwrap();
    conn.execute("UPDATE run SET goal=CAST(goal AS TEXT)", [])
        .unwrap();
    assert!(matches!(
        query::query_runs(&conn, &Default::default()),
        Err(query::QueryError::Track(
            store::TrackError::ResourceLimit { .. }
        ))
    ));
    conn.execute("DELETE FROM run", []).unwrap();
    conn.execute("WITH RECURSIVE n(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM n WHERE x < ?1) INSERT INTO run(id,rom_sha1,status,started_at) SELECT CAST(x AS TEXT),'rom','done','t' FROM n", [budget::MAX_SCAN_RECORDS as i64+1]).unwrap();
    assert!(matches!(
        query::query_runs(&conn, &Default::default()),
        Err(query::QueryError::Track(
            store::TrackError::ResourceLimit { .. }
        ))
    ));
}
