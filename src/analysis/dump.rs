use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::diff::RegionSet;

/// `regions.json`의 한 항목. 포맷 단일 진실 원천.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RegionMeta {
    pub name: String,
    pub memory_type: String,
    pub base_address: u64,
    pub size: u64,
}

#[derive(Debug, thiserror::Error)]
pub enum DumpError {
    #[error("regions.json 없음: {0}")]
    MetaNotFound(PathBuf),
    #[error("regions.json 파싱 실패: {0}")]
    MetaParse(#[from] serde_json::Error),
    #[error("입출력 오류: {0}")]
    Io(#[from] std::io::Error),
    #[error("리전 바이트 파일 없음: {0}")]
    BinNotFound(PathBuf),
    #[error("덤프 멤버가 안전한 일반 파일이 아님: {0}")]
    UnsafeMember(PathBuf),
    #[error("덤프 리전 크기가 매니페스트와 다름: {0}")]
    SizeMismatch(String),
}

const MAX_META_BYTES: u64 = 4 * 1024 * 1024;
const MAX_STATE_BYTES: u64 = 16 * 1024 * 1024;
const MAX_REGIONS: usize = 256;
const MAX_REGION_BYTES: u64 = 1024 * 1024 * 1024;
/// Aggregate retained bytes per dump; checked before opening any region.
pub const MAX_DUMP_BYTES: u64 = 1024 * 1024 * 1024;

/// 덤프 디렉토리(regions.json + <name>.bin)를 RegionSet으로 읽는다.
pub fn load(dir: &Path) -> Result<RegionSet, DumpError> {
    let meta_path = dir.join("regions.json");
    let root = crate::rooted_fs::RootedDirectory::open(dir)?;
    let meta = match root.read_bounded_regular_member(Path::new("regions.json"), MAX_META_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(DumpError::MetaNotFound(meta_path))
        }
        Err(_) => return Err(DumpError::UnsafeMember(meta_path)),
    };
    let metas: Vec<RegionMeta> = serde_json::from_slice(&meta)?;
    if metas.len() > MAX_REGIONS {
        return Err(DumpError::SizeMismatch(format!(
            "region count exceeds {MAX_REGIONS}"
        )));
    }
    let mut total = 0_u64;
    for meta in &metas {
        total = total
            .checked_add(meta.size)
            .filter(|size| *size <= MAX_DUMP_BYTES)
            .ok_or_else(|| DumpError::SizeMismatch("aggregate dump byte limit exceeded".into()))?;
    }
    let mut set = RegionSet::new();
    let mut names = std::collections::BTreeSet::new();
    for m in metas {
        let file_name = format!("{}.bin", m.name);
        let bin = dir.join(&file_name);
        if !crate::path_safety::is_portable_file_name(&file_name, 192)
            || !names.insert(m.name.clone())
        {
            return Err(DumpError::UnsafeMember(bin));
        }
        if m.size > MAX_REGION_BYTES {
            return Err(DumpError::SizeMismatch(m.name));
        }
        let bytes = match root.read_bounded_regular_member(Path::new(&file_name), m.size) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(DumpError::BinNotFound(bin))
            }
            Err(error) if error.kind() == std::io::ErrorKind::InvalidData => {
                return Err(DumpError::SizeMismatch(m.name))
            }
            Err(_) => return Err(DumpError::UnsafeMember(bin)),
        };
        if bytes.len() as u64 != m.size {
            return Err(DumpError::SizeMismatch(m.name));
        }
        set.insert(&m.name, m.base_address, bytes);
    }
    Ok(set)
}

/// 덤프 디렉토리의 `state.json`(레지스터/DMA/PPU 스냅샷)을 읽는다. 없으면 None.
pub fn load_state_map(dir: &Path) -> Result<Option<BTreeMap<String, Value>>, DumpError> {
    let p = dir.join("state.json");
    let root = crate::rooted_fs::RootedDirectory::open(dir)?;
    let bytes = match root.read_bounded_regular_member(Path::new("state.json"), MAX_STATE_BYTES) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(DumpError::UnsafeMember(p)),
    };
    let map: BTreeMap<String, Value> = serde_json::from_slice(&bytes)?;
    Ok(Some(map))
}
