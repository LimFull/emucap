//! Per-operation admission for ledger traversal and complete query projections.
use std::io::{Read, Write};
use std::path::Path;

use serde::Serialize;

use super::store::TrackError;

pub(super) const MAX_SCAN_ENTRIES: u64 = 65_536;
pub(super) const MAX_SCAN_RECORDS: u64 = 8_192;
pub(super) const MAX_SCAN_BYTES: u64 = 32 * 1024 * 1024;
pub(super) const MAX_SCAN_ELEMENTS: u64 = 262_144;
pub(super) const MAX_RESULT_BYTES: u64 = 16 * 1024 * 1024;
pub(super) const MAX_RECORD_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug)]
pub(super) struct ScanBudget {
    pub(super) entries: u64,
    pub(super) records: u64,
    pub(super) bytes: u64,
    pub(super) elements: u64,
}

impl Default for ScanBudget {
    fn default() -> Self {
        Self {
            entries: MAX_SCAN_ENTRIES,
            records: MAX_SCAN_RECORDS,
            bytes: MAX_SCAN_BYTES,
            elements: MAX_SCAN_ELEMENTS,
        }
    }
}

pub(super) fn limit(resource: &'static str, maximum: u64) -> TrackError {
    TrackError::ResourceLimit {
        resource,
        limit: maximum,
    }
}

fn charge(
    remaining: &mut u64,
    amount: u64,
    resource: &'static str,
    maximum: u64,
) -> Result<(), TrackError> {
    *remaining = remaining
        .checked_sub(amount)
        .ok_or_else(|| limit(resource, maximum))?;
    Ok(())
}

impl ScanBudget {
    pub(super) fn entry(&mut self) -> Result<(), TrackError> {
        charge(&mut self.entries, 1, "directory entries", MAX_SCAN_ENTRIES)
    }
    pub(super) fn record(&mut self) -> Result<(), TrackError> {
        charge(&mut self.records, 1, "candidate records", MAX_SCAN_RECORDS)
    }
    pub(super) fn elements(&mut self, count: usize) -> Result<(), TrackError> {
        charge(
            &mut self.elements,
            u64::try_from(count).unwrap_or(u64::MAX),
            "record elements",
            MAX_SCAN_ELEMENTS,
        )
    }
    pub(super) fn admit_size(&self, size: u64) -> Result<(), TrackError> {
        if size > self.bytes {
            return Err(limit("input bytes", MAX_SCAN_BYTES));
        }
        Ok(())
    }
    pub(super) fn read(
        &mut self,
        reader: impl Read,
        size_hint: u64,
        path: &Path,
    ) -> Result<Vec<u8>, TrackError> {
        let max = MAX_RECORD_BYTES.min(self.bytes);
        let mut bytes = Vec::with_capacity(size_hint.min(max) as usize);
        let result = reader.take(max + 1).read_to_end(&mut bytes);
        // Account failed reads and malformed records too. One overflow detection byte is bounded.
        charge(
            &mut self.bytes,
            bytes.len() as u64,
            "input bytes",
            MAX_SCAN_BYTES,
        )?;
        result?;
        if bytes.len() as u64 > MAX_RECORD_BYTES {
            return Err(TrackError::TooLarge {
                path: path.to_owned(),
                limit: MAX_RECORD_BYTES,
            });
        }
        Ok(bytes)
    }
}

struct ResultCounter {
    remaining: u64,
    exceeded: bool,
}
impl Write for ResultCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            self.exceeded = true;
            return Err(std::io::Error::other("tracking result exceeds byte budget"));
        }
        self.remaining -= bytes.len() as u64;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Counts without creating a JSON string/Value. Pretty form also bounds output_path publication.
pub(super) fn check_result<T: Serialize>(value: &T) -> Result<(), TrackError> {
    check_result_with_limit(value, MAX_RESULT_BYTES)
}
pub(super) fn check_result_with_limit<T: Serialize>(value: &T, max: u64) -> Result<(), TrackError> {
    let mut counter = ResultCounter {
        remaining: max,
        exceeded: false,
    };
    let result = serde_json::to_writer_pretty(&mut counter, value);
    if counter.exceeded {
        return Err(limit("result bytes", max));
    }
    result.map_err(TrackError::Serialize)
}
