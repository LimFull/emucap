use super::super::*;
use super::memory_region;

pub(in crate::pc98_bridge) fn open_pc98_state_archive(
    path: &Path,
) -> BridgeResult<ZipArchive<File>> {
    const EOCD_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x05, 0x06];
    const ZIP64_LOCATOR_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x06, 0x07];
    const CENTRAL_DIRECTORY_SIGNATURE: [u8; 4] = [0x50, 0x4b, 0x01, 0x02];
    const CENTRAL_DIRECTORY_HEADER_LEN: usize = 46;
    const EOCD_LEN: usize = 22;
    const MAX_COMMENT_LEN: usize = u16::MAX as usize;

    let mut file = crate::path_safety::open_regular_file_no_follow(path)
        .map_err(|error| BridgeError::BadParams(format!("invalid PC-98 state archive: {error}")))?;
    let file_len = file.metadata()?.len();
    if file_len > MAX_STATE_ARCHIVE_BYTES {
        return Err(BridgeError::BadParams(format!(
            "PC-98 state archive exceeds {MAX_STATE_ARCHIVE_BYTES} bytes"
        )));
    }
    if file_len < EOCD_LEN as u64 {
        return Err(BridgeError::BadParams(
            "PC-98 state archive has no ZIP end record".into(),
        ));
    }

    let tail_len = file_len.min((EOCD_LEN + MAX_COMMENT_LEN) as u64) as usize;
    let tail_offset = file_len - tail_len as u64;
    file.seek(SeekFrom::Start(tail_offset))?;
    let mut tail = vec![0u8; tail_len];
    file.read_exact(&mut tail)?;
    let eocd_relative = (0..=tail_len - EOCD_LEN)
        .rev()
        .find(|&offset| {
            if tail[offset..offset + 4] != EOCD_SIGNATURE {
                return false;
            }
            let comment_len = u16::from_le_bytes([tail[offset + 20], tail[offset + 21]]) as usize;
            offset
                .checked_add(EOCD_LEN + comment_len)
                .is_some_and(|end| end == tail_len)
        })
        .ok_or_else(|| {
            BridgeError::BadParams("PC-98 state archive has no bounded ZIP end record".into())
        })?;
    let eocd = &tail[eocd_relative..eocd_relative + EOCD_LEN];
    let read_u16 = |offset: usize| u16::from_le_bytes([eocd[offset], eocd[offset + 1]]);
    let read_u32 = |offset: usize| {
        u32::from_le_bytes([
            eocd[offset],
            eocd[offset + 1],
            eocd[offset + 2],
            eocd[offset + 3],
        ])
    };
    let disk = read_u16(4);
    let central_directory_disk = read_u16(6);
    let entries_on_disk = read_u16(8);
    let entries = read_u16(10);
    let central_directory_size = read_u32(12);
    let central_directory_offset = read_u32(16);
    if disk == u16::MAX
        || central_directory_disk == u16::MAX
        || entries_on_disk == u16::MAX
        || entries == u16::MAX
        || central_directory_size == u32::MAX
        || central_directory_offset == u32::MAX
    {
        return Err(BridgeError::BadParams(
            "Zip64 PC-98 state archives are not supported".into(),
        ));
    }
    if disk != 0 || central_directory_disk != 0 || entries_on_disk != entries {
        return Err(BridgeError::BadParams(
            "multi-disk PC-98 state archives are not supported".into(),
        ));
    }
    if u64::from(entries) > MAX_STATE_ARCHIVE_ENTRIES {
        return Err(BridgeError::BadParams(format!(
            "PC-98 state archive entry count exceeds {MAX_STATE_ARCHIVE_ENTRIES}"
        )));
    }
    if u64::from(central_directory_size) > MAX_STATE_CENTRAL_DIRECTORY_BYTES {
        return Err(BridgeError::BadParams(format!(
            "PC-98 state archive central directory exceeds {MAX_STATE_CENTRAL_DIRECTORY_BYTES} bytes"
        )));
    }
    if (entries == 0) != (central_directory_size == 0) {
        return Err(BridgeError::BadParams(
            "PC-98 state archive has inconsistent central directory metadata".into(),
        ));
    }
    let eocd_offset = tail_offset + eocd_relative as u64;
    let central_directory_end = u64::from(central_directory_offset)
        .checked_add(u64::from(central_directory_size))
        .ok_or_else(|| {
            BridgeError::BadParams("PC-98 state archive central directory overflows".into())
        })?;
    if central_directory_end > eocd_offset || central_directory_end > file_len {
        return Err(BridgeError::BadParams(
            "PC-98 state archive central directory is out of bounds".into(),
        ));
    }
    // A prepended executable or other prefix shifts the physical central directory by the
    // difference between the relative end and the EOCD. Validate the selected EOCD's bounded
    // directory itself so zip-rs cannot reject a forged trailing EOCD and fall back to an older,
    // unbounded one earlier in the file.
    let archive_offset = eocd_offset - central_directory_end;
    let physical_directory_offset = archive_offset
        .checked_add(u64::from(central_directory_offset))
        .ok_or_else(|| {
            BridgeError::BadParams("PC-98 state archive central directory overflows".into())
        })?;
    let mut central_directory = vec![0u8; central_directory_size as usize];
    file.seek(SeekFrom::Start(physical_directory_offset))?;
    file.read_exact(&mut central_directory)?;
    let mut cursor = 0usize;
    for _ in 0..entries {
        let header_end = cursor
            .checked_add(CENTRAL_DIRECTORY_HEADER_LEN)
            .filter(|end| *end <= central_directory.len())
            .ok_or_else(|| {
                BridgeError::BadParams("PC-98 state archive central directory is truncated".into())
            })?;
        let header = &central_directory[cursor..header_end];
        if header[..4] != CENTRAL_DIRECTORY_SIGNATURE {
            return Err(BridgeError::BadParams(
                "PC-98 state archive central directory has an invalid entry".into(),
            ));
        }
        let compressed_size = u32::from_le_bytes(header[20..24].try_into().unwrap());
        let uncompressed_size = u32::from_le_bytes(header[24..28].try_into().unwrap());
        let flags = u16::from_le_bytes(header[8..10].try_into().unwrap());
        let compression_method = u16::from_le_bytes(header[10..12].try_into().unwrap());
        let name_len = u16::from_le_bytes(header[28..30].try_into().unwrap()) as usize;
        let extra_len = u16::from_le_bytes(header[30..32].try_into().unwrap()) as usize;
        let comment_len = u16::from_le_bytes(header[32..34].try_into().unwrap()) as usize;
        let disk_start = u16::from_le_bytes(header[34..36].try_into().unwrap());
        let local_header_offset = u32::from_le_bytes(header[42..46].try_into().unwrap());
        if disk_start != 0 {
            return Err(BridgeError::BadParams(
                "multi-disk PC-98 state archives are not supported".into(),
            ));
        }
        if compressed_size == u32::MAX
            || uncompressed_size == u32::MAX
            || local_header_offset == u32::MAX
        {
            return Err(BridgeError::BadParams(
                "Zip64 PC-98 state archives are not supported".into(),
            ));
        }
        // zip-rs retries an older EOCD whenever parsing this selected central directory fails.
        // PC-98 state bundles are produced without central extra fields or encryption, so reject
        // both before invoking that fallback-capable parser. In particular, malformed Zip64/AES
        // extras and method 99 must not make an earlier, unbounded directory authoritative.
        if extra_len != 0 {
            return Err(BridgeError::BadParams(
                "PC-98 state archive central directory extra fields are not supported".into(),
            ));
        }
        if flags & 0x0001 != 0 || compression_method == 99 {
            return Err(BridgeError::BadParams(
                "encrypted PC-98 state archive entries are not supported".into(),
            ));
        }
        let physical_local_header = archive_offset
            .checked_add(u64::from(local_header_offset))
            .filter(|offset| *offset < physical_directory_offset)
            .and_then(|offset| u32::try_from(offset).ok())
            .ok_or_else(|| {
                BridgeError::BadParams(
                    "PC-98 state archive local header offset is out of bounds".into(),
                )
            })?;
        // The isolated metadata view below has archive offset zero. Rewrite this metadata copy to
        // the physical offset in the original, already-open file; archive payload bytes are never
        // copied or interpreted here.
        central_directory[cursor + 42..cursor + 46]
            .copy_from_slice(&physical_local_header.to_le_bytes());
        cursor = header_end
            .checked_add(name_len)
            .and_then(|end| end.checked_add(extra_len))
            .and_then(|end| end.checked_add(comment_len))
            .filter(|end| *end <= central_directory.len())
            .ok_or_else(|| {
                BridgeError::BadParams(
                    "PC-98 state archive central directory entry is truncated".into(),
                )
            })?;
    }
    if cursor != central_directory.len() {
        return Err(BridgeError::BadParams(
            "PC-98 state archive central directory size is inconsistent".into(),
        ));
    }
    if eocd_offset >= 20 {
        file.seek(SeekFrom::Start(eocd_offset - 20))?;
        let mut signature = [0u8; 4];
        file.read_exact(&mut signature)?;
        if signature == ZIP64_LOCATOR_SIGNATURE {
            return Err(BridgeError::BadParams(
                "Zip64 PC-98 state archives are not supported".into(),
            ));
        }
    }

    // Parse metadata from only the selected, bounded central directory and a synthetic EOCD. The
    // dependency's ordinary reader retries older EOCDs after any directory parse error; this view
    // contains no earlier EOCD, so such an error is terminal instead of selecting attacker-chosen
    // metadata outside the admitted directory.
    let mut metadata_view = central_directory;
    metadata_view.extend_from_slice(&EOCD_SIGNATURE);
    metadata_view.extend_from_slice(&0u16.to_le_bytes());
    metadata_view.extend_from_slice(&0u16.to_le_bytes());
    metadata_view.extend_from_slice(&entries.to_le_bytes());
    metadata_view.extend_from_slice(&entries.to_le_bytes());
    metadata_view.extend_from_slice(&central_directory_size.to_le_bytes());
    metadata_view.extend_from_slice(&0u32.to_le_bytes());
    metadata_view.extend_from_slice(&0u16.to_le_bytes());
    let isolated = ZipArchive::with_config(
        zip::read::Config {
            archive_offset: zip::read::ArchiveOffset::Known(0),
        },
        std::io::Cursor::new(metadata_view),
    )?;
    if isolated.len() as u64 != u64::from(entries) || isolated.central_directory_start() != 0 {
        return Err(BridgeError::BadParams(
            "PC-98 state archive parser selected inconsistent directory metadata".into(),
        ));
    }
    if isolated.len() as u64 > MAX_STATE_ARCHIVE_ENTRIES {
        return Err(BridgeError::BadParams(format!(
            "PC-98 state archive entry count exceeds {MAX_STATE_ARCHIVE_ENTRIES}"
        )));
    }
    let metadata = isolated.metadata();
    file.seek(SeekFrom::Start(0))?;
    // SAFETY: `metadata` was parsed from the exact selected central-directory bytes read from this
    // same file descriptor. Its local-header offsets were rewritten above from ZIP-relative to
    // physical positions for the zero-offset metadata view. No path reopen or alternate archive
    // is involved; later local-header mismatches remain ordinary zip errors.
    Ok(unsafe { ZipArchive::unsafe_new_with_metadata(file, metadata) })
}

pub(in crate::pc98_bridge) fn read_state_manifest<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
) -> BridgeResult<Value> {
    let mut file = archive.by_name("state.json")?;
    if file.size() > MAX_STATE_MANIFEST_BYTES {
        return Err(BridgeError::BadParams(format!(
            "PC-98 state manifest exceeds {MAX_STATE_MANIFEST_BYTES} bytes"
        )));
    }
    let mut text = String::new();
    Read::by_ref(&mut file)
        .take(MAX_STATE_MANIFEST_BYTES + 1)
        .read_to_string(&mut text)?;
    if text.len() as u64 > MAX_STATE_MANIFEST_BYTES {
        return Err(BridgeError::BadParams(format!(
            "PC-98 state manifest exceeds {MAX_STATE_MANIFEST_BYTES} bytes"
        )));
    }
    Ok(serde_json::from_str(&text)?)
}

pub(in crate::pc98_bridge) fn state_format(manifest: &Value) -> BridgeResult<String> {
    let format = manifest
        .get("format")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if format != STATE_FORMAT && format != LEGACY_STATE_FORMAT {
        return Err(BridgeError::BadParams(format!(
            "unsupported PC-98 state format: {format}"
        )));
    }
    Ok(format.into())
}

pub(in crate::pc98_bridge) fn validate_register_packet(regs: &str) -> BridgeResult<()> {
    if regs.len() != I386_REGS.len() * 8 || !regs.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(BridgeError::BadParams(
            "PC-98 state requires a complete 128-character hexadecimal i386 register packet".into(),
        ));
    }
    Ok(())
}

pub(in crate::pc98_bridge) fn state_registers(
    manifest: &Value,
    required: bool,
) -> BridgeResult<Option<&str>> {
    let legacy = state_format(manifest)? == LEGACY_STATE_FORMAT;
    let regs = match manifest.get("registers_hex") {
        None if legacy && !required => return Ok(None),
        Some(Value::String(regs)) if legacy && !required && regs.is_empty() => return Ok(None),
        Some(Value::String(regs)) => regs.as_str(),
        _ => {
            return Err(BridgeError::BadParams(
                "PC-98 state is missing registers_hex".into(),
            ))
        }
    };
    validate_register_packet(regs)?;
    Ok(Some(regs))
}

#[derive(Debug)]
struct SaveItemManifestEntry {
    filename: String,
    bytes_len: u64,
}

fn parse_save_item_manifest(bytes: &[u8]) -> BridgeResult<Vec<SaveItemManifestEntry>> {
    let text = std::str::from_utf8(bytes)
        .map_err(|_| BridgeError::BadParams("PC-98 save item manifest is not UTF-8".into()))?;
    let mut entries = Vec::new();
    let mut indexes = BTreeSet::new();
    let mut filenames = BTreeSet::new();
    let mut total_bytes = 0u64;

    for (line_index, line) in text.split_terminator('\n').enumerate() {
        let line_number = line_index + 1;
        if line.is_empty() {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest line {line_number} is empty"
            )));
        }
        if entries.len() >= MAX_SAVE_ITEM_MEMBERS.saturating_sub(1) {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest exceeds {} entries",
                MAX_SAVE_ITEM_MEMBERS.saturating_sub(1)
            )));
        }
        let fields = line.split('|').collect::<Vec<_>>();
        if fields.len() != 5 {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest line {line_number} must contain five fields"
            )));
        }
        let parse_decimal = |field: &str, name: &str| -> BridgeResult<u64> {
            if field.is_empty() || !field.bytes().all(|byte| byte.is_ascii_digit()) {
                return Err(BridgeError::BadParams(format!(
                    "PC-98 save item manifest line {line_number} has invalid {name}"
                )));
            }
            let value = field.parse::<u64>().map_err(|_| {
                BridgeError::BadParams(format!(
                    "PC-98 save item manifest line {line_number} has overflowing {name}"
                ))
            })?;
            if value > MAX_SAVE_ITEM_INTEGER {
                return Err(BridgeError::BadParams(format!(
                    "PC-98 save item manifest line {line_number} has out-of-range {name}"
                )));
            }
            Ok(value)
        };
        let index = parse_decimal(fields[0], "index")?;
        let size = parse_decimal(fields[1], "size")?;
        let count = parse_decimal(fields[2], "count")?;
        let bytes_len = parse_decimal(fields[3], "byte length")?;
        let filename = fields[4];
        let expected_len = size.checked_mul(count).ok_or_else(|| {
            BridgeError::BadParams(format!(
                "PC-98 save item manifest line {line_number} byte length overflows"
            ))
        })?;
        if bytes_len != expected_len {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest line {line_number} byte length does not equal size * count"
            )));
        }
        if bytes_len > MAX_SAVE_ITEM_MEMBER_BYTES {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest line {line_number} exceeds {MAX_SAVE_ITEM_MEMBER_BYTES} bytes"
            )));
        }
        if !crate::path_safety::is_portable_file_name(filename, MAX_SAVE_ITEM_FILENAME_BYTES)
            || filename.eq_ignore_ascii_case("manifest.txt")
        {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest line {line_number} has an unsafe filename"
            )));
        }
        if !indexes.insert(index) {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest repeats index {index}"
            )));
        }
        let folded_filename = filename.to_ascii_lowercase();
        if !filenames.insert(folded_filename) {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest repeats filename {filename}"
            )));
        }
        total_bytes = total_bytes.checked_add(bytes_len).ok_or_else(|| {
            BridgeError::BadParams("PC-98 save item manifest byte count overflow".into())
        })?;
        if total_bytes > MAX_SAVE_ITEM_TOTAL_BYTES {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest bytes exceed {MAX_SAVE_ITEM_TOTAL_BYTES}"
            )));
        }
        entries.push(SaveItemManifestEntry {
            filename: filename.into(),
            bytes_len,
        });
    }
    Ok(entries)
}

pub(in crate::pc98_bridge) fn extract_save_items<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &Value,
    target_root: &Path,
) -> BridgeResult<Option<PathBuf>> {
    let Some(save_items) = manifest.get("save_items").and_then(Value::as_object) else {
        return Ok(None);
    };
    let directory = save_items
        .get("dir")
        .and_then(Value::as_str)
        .unwrap_or(SAVE_ITEMS_DIR)
        .trim_matches('/');
    if directory != SAVE_ITEMS_DIR {
        return Err(BridgeError::BadParams(format!(
            "unsupported PC-98 save item directory: {directory}"
        )));
    }
    let names = archive
        .file_names()
        .filter(|name| name.starts_with(&format!("{SAVE_ITEMS_DIR}/")))
        .map(str::to_string)
        .collect::<Vec<_>>();
    if names.len() > MAX_SAVE_ITEM_MEMBERS {
        return Err(BridgeError::BadParams(format!(
            "PC-98 save item count exceeds {MAX_SAVE_ITEM_MEMBERS}"
        )));
    }
    if !names.iter().any(|name| name == SAVE_ITEMS_MANIFEST) {
        return Err(BridgeError::BadParams(
            "PC-98 save item manifest is missing".into(),
        ));
    }
    let mut exact_names = BTreeSet::new();
    let mut folded_names = BTreeSet::new();
    for name in &names {
        if !exact_names.insert(name.clone()) {
            return Err(BridgeError::BadParams(format!(
                "duplicate PC-98 save item member: {name}"
            )));
        }
        if !folded_names.insert(name.to_ascii_lowercase()) {
            return Err(BridgeError::BadParams(format!(
                "case-colliding PC-98 save item member: {name}"
            )));
        }
    }
    let manifest_entries = {
        let mut manifest_file = archive.by_name(SAVE_ITEMS_MANIFEST)?;
        let manifest_size = manifest_file.size();
        if manifest_size > MAX_SAVE_ITEM_MANIFEST_BYTES {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest exceeds {MAX_SAVE_ITEM_MANIFEST_BYTES} bytes"
            )));
        }
        let mut bytes = Vec::with_capacity(manifest_size as usize);
        Read::by_ref(&mut manifest_file)
            .take(MAX_SAVE_ITEM_MANIFEST_BYTES + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_SAVE_ITEM_MANIFEST_BYTES {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest exceeds {MAX_SAVE_ITEM_MANIFEST_BYTES} bytes"
            )));
        }
        if bytes.len() as u64 != manifest_size {
            return Err(BridgeError::BadParams(
                "PC-98 save item manifest size changed while reading".into(),
            ));
        }
        parse_save_item_manifest(&bytes)?
    };
    for entry in &manifest_entries {
        let member = format!("{SAVE_ITEMS_DIR}/{}", entry.filename);
        if !exact_names.contains(&member) {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest references missing member: {member}"
            )));
        }
        let file = archive.by_name(&member)?;
        if file.size() != entry.bytes_len {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item manifest length does not match {member}"
            )));
        }
    }
    let approved_names = std::iter::once(SAVE_ITEMS_MANIFEST.to_string())
        .chain(
            manifest_entries
                .iter()
                .map(|entry| format!("{SAVE_ITEMS_DIR}/{}", entry.filename)),
        )
        .collect::<Vec<_>>();
    let approved_set = approved_names.iter().cloned().collect::<BTreeSet<_>>();
    if exact_names != approved_set {
        let unexpected = exact_names
            .difference(&approved_set)
            .next()
            .cloned()
            .unwrap_or_else(|| "missing manifest member".into());
        return Err(BridgeError::BadParams(format!(
            "unexpected PC-98 save item member: {unexpected}"
        )));
    }
    let out_dir = target_root.join(SAVE_ITEMS_DIR);
    crate::private_fs::create_private_dir(&out_dir)?;
    let mut total_bytes = 0u64;
    for name in approved_names {
        let rel = &name[SAVE_ITEMS_DIR.len() + 1..];
        if !crate::path_safety::is_portable_file_name(rel, MAX_SAVE_ITEM_FILENAME_BYTES) {
            return Err(BridgeError::BadParams(format!(
                "unsafe PC-98 save item member: {name}"
            )));
        }
        let dest = out_dir.join(rel);
        let mut src = archive.by_name(&name)?;
        if src.size() > MAX_SAVE_ITEM_MEMBER_BYTES {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item exceeds {MAX_SAVE_ITEM_MEMBER_BYTES} bytes: {name}"
            )));
        }
        total_bytes = total_bytes
            .checked_add(src.size())
            .ok_or_else(|| BridgeError::BadParams("PC-98 save item byte count overflow".into()))?;
        if total_bytes > MAX_SAVE_ITEM_TOTAL_BYTES {
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item bytes exceed {MAX_SAVE_ITEM_TOTAL_BYTES}"
            )));
        }
        let mut output = crate::private_fs::create_private_file(&dest)?;
        let copied = std::io::copy(
            &mut Read::by_ref(&mut src).take(MAX_SAVE_ITEM_MEMBER_BYTES + 1),
            &mut output,
        )?;
        if copied != src.size() || copied > MAX_SAVE_ITEM_MEMBER_BYTES {
            let _ = fs::remove_file(&dest);
            return Err(BridgeError::BadParams(format!(
                "PC-98 save item size changed during extraction: {name}"
            )));
        }
    }
    Ok(Some(out_dir))
}

pub(in crate::pc98_bridge) fn extract_presented_framebuffer<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &Value,
    target_root: &Path,
) -> BridgeResult<Option<PathBuf>> {
    let Some(framebuffer) = manifest.get("framebuffer").and_then(Value::as_object) else {
        return Ok(None);
    };
    let member = framebuffer
        .get("file")
        .and_then(Value::as_str)
        .ok_or_else(|| BridgeError::BadParams("PC-98 state framebuffer is missing file".into()))?;
    if member != FRAMEBUFFER_MEMBER {
        return Err(BridgeError::BadParams(format!(
            "unsupported PC-98 state framebuffer member: {member}"
        )));
    }
    let encoding = framebuffer
        .get("encoding")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            BridgeError::BadParams("PC-98 state framebuffer is missing encoding".into())
        })?;
    if encoding != "mame_rgb32_native" {
        return Err(BridgeError::BadParams(format!(
            "unsupported PC-98 state framebuffer encoding: {encoding}"
        )));
    }
    let width = framebuffer
        .get("width")
        .and_then(Value::as_u64)
        .ok_or_else(|| BridgeError::BadParams("PC-98 state framebuffer is missing width".into()))?;
    let height = framebuffer
        .get("height")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
            BridgeError::BadParams("PC-98 state framebuffer is missing height".into())
        })?;
    let expected = framebuffer
        .get("bytes")
        .and_then(Value::as_u64)
        .ok_or_else(|| BridgeError::BadParams("PC-98 state framebuffer is missing bytes".into()))?;
    if expected == 0 || expected > 16 * 1024 * 1024 {
        return Err(BridgeError::BadParams(format!(
            "PC-98 state framebuffer byte count is invalid: {expected}"
        )));
    }
    if width
        .checked_mul(height)
        .and_then(|pixels| pixels.checked_mul(4))
        != Some(expected)
    {
        return Err(BridgeError::BadParams(format!(
            "PC-98 state framebuffer geometry differs from byte count: {width}x{height}, bytes={expected}"
        )));
    }
    let mut member_file = archive.by_name(FRAMEBUFFER_MEMBER)?;
    if member_file.size() != expected {
        return Err(BridgeError::BadParams(format!(
            "PC-98 state framebuffer size differs: manifest={expected}, archive={}",
            member_file.size()
        )));
    }
    let output = target_root.join("framebuffer.rgb32");
    let mut file = crate::private_fs::create_private_file(&output)?;
    std::io::copy(&mut member_file, &mut file)?;
    Ok(Some(output))
}

pub(in crate::pc98_bridge) fn read_state_regions<R: Read + Seek>(
    archive: &mut ZipArchive<R>,
    manifest: &Value,
) -> BridgeResult<Vec<(String, Vec<u8>)>> {
    let Some(regions) = manifest.get("regions").and_then(Value::as_array) else {
        return Ok(Vec::new());
    };
    if regions.len() > MEMORY_REGIONS.len() {
        return Err(BridgeError::BadParams(
            "too many PC-98 state regions".into(),
        ));
    }
    let mut out = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for region in regions {
        let memory_type = region
            .get("memory_type")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BridgeError::BadParams("PC-98 state region missing memory_type".into())
            })?;
        if !seen.insert(memory_type) {
            return Err(BridgeError::BadParams(format!(
                "duplicate PC-98 state region: {memory_type}"
            )));
        }
        let declared = region
            .get("size")
            .and_then(Value::as_u64)
            .ok_or_else(|| BridgeError::BadParams("PC-98 state region missing size".into()))?;
        let known = memory_region(memory_type).ok_or_else(|| {
            BridgeError::BadParams(format!("unsupported PC-98 state region: {memory_type}"))
        })?;
        if declared == 0 || declared > known.size as u64 {
            return Err(BridgeError::BadParams(format!(
                "PC-98 state region size is invalid: {memory_type}={declared}"
            )));
        }
        let file_name = region
            .get("file")
            .and_then(Value::as_str)
            .ok_or_else(|| BridgeError::BadParams("PC-98 state region missing file".into()))?;
        let expected_name = format!("{memory_type}.bin");
        if file_name != expected_name {
            return Err(BridgeError::BadParams(format!(
                "PC-98 state region member must be {expected_name}: {file_name}"
            )));
        }
        let mut file = archive.by_name(file_name)?;
        if file.size() != declared {
            return Err(BridgeError::BadParams(format!(
                "PC-98 state region size differs: {memory_type} manifest={declared}, archive={}",
                file.size()
            )));
        }
        let mut bytes = Vec::new();
        Read::by_ref(&mut file)
            .take(declared + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 != declared {
            return Err(BridgeError::BadParams(format!(
                "PC-98 state region read length differs: {memory_type}"
            )));
        }
        out.push((memory_type.into(), bytes));
    }
    Ok(out)
}
