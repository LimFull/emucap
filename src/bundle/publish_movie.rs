use super::error::PublishError;
use super::publish::valid_hex_digest;
use crate::input_movie::canonical_recording_movie;
use sha2::{Digest, Sha256};
use std::fs;
use std::path::Path;
const INPUT_MOVIE_MEMBER: &str = "input.movie";

pub(super) fn validate_input_movie_member(
    bundle_path: &Path,
    request: &super::recording_manifest::RecordingRequest,
) -> Result<(), PublishError> {
    let path = bundle_path.join(INPUT_MOVIE_MEMBER);
    let Some(identity) = &request.input_movie else {
        if fs::symlink_metadata(&path).is_ok() {
            return Err(PublishError::InvalidIdentity(
                "unexpected input movie member".into(),
            ));
        }
        return Ok(());
    };
    if identity.format != crate::input_movie::INPUT_MOVIE_FORMAT
        || identity.port != 0
        || identity.bytes > crate::live::recording_capability::CORE_MAX_INPUT_MOVIE_BYTES
        || identity.frames != request.frames.saturating_add(request.warmup_frames)
        || !valid_hex_digest(&identity.sha256, 32)
    {
        return Err(PublishError::InvalidIdentity(
            "input movie request identity is invalid".into(),
        ));
    }
    let bytes = crate::path_safety::read_bounded_regular_member(
        bundle_path,
        INPUT_MOVIE_MEMBER,
        identity.bytes,
    )?;
    if bytes.len() as u64 != identity.bytes {
        return Err(PublishError::InvalidIdentity(
            "input movie member size mismatch".into(),
        ));
    }
    if identity.sha256 != hex::encode(Sha256::digest(&bytes)) {
        return Err(PublishError::InvalidIdentity(
            "input movie member digest mismatch".into(),
        ));
    }
    let text = std::str::from_utf8(&bytes).map_err(|error| {
        PublishError::InvalidIdentity(format!("input movie member is not UTF-8: {error}"))
    })?;
    let canonical = canonical_recording_movie(
        text,
        request.frames.saturating_add(request.warmup_frames),
        crate::live::recording_capability::CORE_MAX_INPUT_BUTTONS_PER_FRAME,
    )
    .map_err(PublishError::InvalidIdentity)?;
    if canonical.bytes != bytes {
        return Err(PublishError::InvalidIdentity(
            "input movie member is not canonical".into(),
        ));
    }
    Ok(())
}
