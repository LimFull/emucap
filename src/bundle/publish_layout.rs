use std::ffi::OsStr;
use std::fs;
use std::path::Path;

use super::error::PublishError;
use super::recording_manifest::RecordingRequest;

#[derive(Clone, Copy)]
enum MemberType {
    File,
    Directory,
}

fn exact_directory(path: &Path, expected: &[(&str, MemberType)]) -> Result<(), PublishError> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(PublishError::UnsafeMember(path.to_owned()));
    }
    crate::private_fs::verify_private_dir(path)
        .map_err(|_| PublishError::UnsafeMember(path.to_owned()))?;
    let mut seen = vec![false; expected.len()];
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(index) = expected
            .iter()
            .position(|(name_expected, _)| name == OsStr::new(name_expected))
        else {
            return Err(PublishError::UnsafeMember(entry.path()));
        };
        let kind = entry.file_type()?;
        let matches = match expected[index].1 {
            MemberType::File => kind.is_file(),
            MemberType::Directory => kind.is_dir(),
        };
        if seen[index] || kind.is_symlink() || !matches {
            return Err(PublishError::UnsafeMember(entry.path()));
        }
        let private = match expected[index].1 {
            MemberType::File => crate::private_fs::verify_private_file(&entry.path()),
            MemberType::Directory => crate::private_fs::verify_private_dir(&entry.path()),
        };
        private.map_err(|_| PublishError::UnsafeMember(entry.path()))?;
        seen[index] = true;
    }
    for (index, (name, _)) in expected.iter().enumerate() {
        if !seen[index] {
            return Err(PublishError::MemberMissing(path.join(name)));
        }
    }
    Ok(())
}

/// Check the final layout, after the staging owner marker has been removed.
/// Snapshot validators separately enforce their exact requested labels and types.
pub(super) fn validate_final_layout(
    root: &Path,
    request: &RecordingRequest,
) -> Result<(), PublishError> {
    use MemberType::{Directory, File};
    let mut members = vec![("manifest.json", File), ("events", Directory)];
    if request.input_movie.is_some() {
        members.push(("input.movie", File));
    }
    if request.initial_state.is_some() {
        members.push(("initial.state", File));
    }
    if !request.initial_snapshots.is_empty() {
        members.push(("initial-snapshots", Directory));
    }
    if !request.terminal_snapshots.is_empty() {
        members.push(("snapshots", Directory));
    }
    if request.terminal_state.is_some() {
        members.push(("terminal-state.json", File));
    }
    exact_directory(root, &members)?;
    exact_directory(&root.join("events"), &[("segment-000.ndjson", File)])
}
