use super::{
    evidence_identity::runtime_identity,
    link::EmulatorIdentity,
    runtime::{CurrentManifest, ProcessIdentity},
};
use crate::content_identity::{
    ContentHashAlgorithm, ContentIdentity as GraphIdentity, ContentIdentityScope,
    ContentMemberIdentity,
};
use serde_json::json;
use sha1::Sha1;
use sha2::{Digest, Sha256};
use std::fs;

fn emulator_identity() -> EmulatorIdentity {
    EmulatorIdentity {
        adapter: Some("mednafen".into()),
        build: Some("adapter-build".into()),
        host_build: Some(json!({
            "upstream": "Mednafen",
            "commit": "upstream-revision",
            "patchset_sha256": "11".repeat(32),
            "binary_sha256": "22".repeat(32),
        })),
        ..EmulatorIdentity::default()
    }
}

fn current(content: String) -> CurrentManifest {
    CurrentManifest {
        schema_version: 1,
        launch_id: "launch-test".into(),
        port: 47_999,
        adapter: "mednafen".into(),
        system: "psx".into(),
        content,
        content_identity: None,
        build: None,
        emulator: ProcessIdentity {
            pid: 1,
            start_identity: None,
        },
        bridge: None,
        backend_endpoint: None,
        execution_profile: None,
        start_frozen: true,
        created_at_unix_ms: 1,
    }
}

#[test]
fn composite_evidence_uses_the_bound_graph_without_reopening_the_entry() {
    let missing = std::env::temp_dir().join(format!(
        "emucap-missing-composite-{}.cue",
        ulid::Ulid::generate()
    ));
    let mut current = current(missing.display().to_string());
    current.content_identity = Some(GraphIdentity {
        scope: ContentIdentityScope::CueGraph,
        algorithm: ContentHashAlgorithm::Sha256,
        digest: "ab".repeat(32),
        size: 734_003_200,
        members: vec![ContentMemberIdentity {
            declared_name: "disc.cue".into(),
            size: 48,
            digest: "cd".repeat(32),
        }],
    });

    let runtime = runtime_identity(&emulator_identity(), &current, "capability", 64).unwrap();

    assert_eq!(runtime.content.sha1, None);
    assert_eq!(runtime.content.sha256, Some("ab".repeat(32)));
    assert_eq!(runtime.content.bytes, 734_003_200);
    assert_eq!(
        runtime.content.path_hint.as_deref(),
        missing.file_name().unwrap().to_str()
    );
}

#[test]
fn single_file_evidence_still_hashes_the_exact_file() {
    let directory = tempfile::tempdir().unwrap();
    let content = directory.path().join("game.rom");
    fs::write(&content, b"exact bytes").unwrap();

    let runtime = runtime_identity(
        &emulator_identity(),
        &current(content.display().to_string()),
        "capability",
        64,
    )
    .unwrap();

    assert_eq!(runtime.content.bytes, 11);
    assert_eq!(
        runtime.content.sha1,
        Some(hex::encode(Sha1::digest(b"exact bytes")))
    );
    assert_eq!(
        runtime.content.sha256,
        Some(hex::encode(Sha256::digest(b"exact bytes")))
    );
}
