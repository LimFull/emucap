use crate::analysis::bisect::{self, CmpOp, Predicate};
use crate::analysis::regression;
use crate::live::link::{EmulatorLink, LinkError};
use crate::live::tools;
use std::path::{Path, PathBuf};
use std::time::Instant;

pub use crate::analysis::regression::{load_case, load_suite, CaseResult, Summary};

#[cfg(test)]
#[path = "live_regression_tests.rs"]
mod tests;

/// 결정론 재현 판정. 측정 무효(MeasurementInvalid)와 진짜 재현 불가(NotReproducible)를
/// 절대 섞지 않는다.
#[derive(Debug, Clone, PartialEq)]
pub enum DetOutcome {
    Reproducible,
    NotReproducible,
    Unsupported(String),
    MeasurementInvalid(String),
}
impl DetOutcome {
    pub fn passed(&self) -> Option<bool> {
        match self {
            DetOutcome::Reproducible => Some(true),
            DetOutcome::NotReproducible => Some(false),
            DetOutcome::Unsupported(_) | DetOutcome::MeasurementInvalid(_) => None,
        }
    }
    pub fn code(&self) -> String {
        match self {
            DetOutcome::Reproducible => "reproducible".into(),
            DetOutcome::NotReproducible => "not_reproducible".into(),
            DetOutcome::Unsupported(m) => format!("unsupported:{m}"),
            DetOutcome::MeasurementInvalid(m) => format!("measurement_invalid:{m}"),
        }
    }
}

pub struct DetResult {
    pub outcome: DetOutcome,
    pub observe_kind: String,
    pub replays: u32,
    pub hashes: Vec<String>,
}

/// Immutable, filesystem-preflighted execution input. Movie bytes are read, parsed, and bounded
/// before any emulator call; state members are resolved to validated regular-file paths.
#[derive(Debug, Clone)]
pub struct PreparedCase {
    case: regression::Case,
    state_path: Option<PathBuf>,
    replay_start_path: Option<PathBuf>,
    movie: Option<regression::Movie>,
    advances: u64,
}

impl PreparedCase {
    pub fn case(&self) -> &regression::Case {
        &self.case
    }

    pub fn advances(&self) -> u64 {
        self.advances
    }
}

#[derive(Debug, Clone)]
pub struct PlannedCase {
    pub id: String,
    pub prepared: Result<PreparedCase, regression::Verdict>,
}

pub fn regression_deadline() -> Instant {
    Instant::now() + crate::live::temporal::MAX_SYNC_OPERATION_TIME
}

#[derive(Debug)]
enum CaseMemberError {
    Missing,
    Unsafe(String),
}

fn case_member_path(dir: &Path, file_name: &str) -> Result<PathBuf, CaseMemberError> {
    if !crate::path_safety::is_portable_file_name(file_name, 128) {
        return Err(CaseMemberError::Unsafe(
            "case member must be a portable file name".into(),
        ));
    }
    let root = std::fs::canonicalize(dir)
        .map_err(|error| CaseMemberError::Unsafe(format!("invalid case directory: {error}")))?;
    let path = dir.join(file_name);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(CaseMemberError::Missing)
        }
        Err(error) => {
            return Err(CaseMemberError::Unsafe(format!(
                "cannot inspect case member: {error}"
            )))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(CaseMemberError::Unsafe(
            "case member is not a regular owned file".into(),
        ));
    }
    let canonical = std::fs::canonicalize(&path)
        .map_err(|error| CaseMemberError::Unsafe(format!("invalid case member: {error}")))?;
    if !canonical.starts_with(&root) {
        return Err(CaseMemberError::Unsafe(
            "case member escapes the case directory".into(),
        ));
    }
    Ok(canonical)
}

fn state_member_path(dir: &Path, state_id: &str) -> Result<PathBuf, CaseMemberError> {
    case_member_path(dir, &format!("{state_id}.mss"))
}

fn member_error_verdict(error: CaseMemberError) -> regression::Verdict {
    match error {
        CaseMemberError::Missing => regression::Verdict::MissingPayload,
        CaseMemberError::Unsafe(reason) => regression::Verdict::Invalid(reason),
    }
}

fn read_prepared_movie(
    dir: &Path,
    movie_name: &str,
    aggregate_movie_bytes: &mut u64,
) -> Result<regression::Movie, regression::Verdict> {
    // Preserve the established missing/unsafe verdicts for callers, then acquire the bytes again
    // through the descriptor-relative no-follow helper that supplies the actual enforcement.
    case_member_path(dir, movie_name).map_err(member_error_verdict)?;
    let bytes = crate::path_safety::read_bounded_regular_member(
        dir,
        movie_name,
        regression::MAX_REGRESSION_MOVIE_BYTES,
    )
    .map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            regression::Verdict::MissingPayload
        } else {
            regression::Verdict::Invalid(format!("invalid movie payload: {error}"))
        }
    })?;
    let byte_len = u64::try_from(bytes.len())
        .map_err(|_| regression::Verdict::Invalid("movie byte count overflow".into()))?;
    *aggregate_movie_bytes = aggregate_movie_bytes
        .checked_add(byte_len)
        .unwrap_or(u64::MAX);
    let text = std::str::from_utf8(&bytes)
        .map_err(|error| regression::Verdict::Invalid(format!("movie is not UTF-8: {error}")))?;
    let movie =
        crate::input_movie::parse_movie_bounded(text, regression::MAX_REGRESSION_MOVIE_ROWS)
            .map_err(regression::Verdict::Invalid)?;
    validate_movie_frames(&movie).map_err(regression::Verdict::Invalid)?;
    Ok(movie)
}

/// Resolve and bound every filesystem-controlled input for one case without contacting an
/// emulator. The returned value owns the parsed movie so execution cannot reopen a changed file.
pub fn prepare_case(
    dir: &Path,
    case: &regression::Case,
) -> Result<PreparedCase, regression::Verdict> {
    let mut movie_bytes = 0u64;
    prepare_case_counted(dir, case, &mut movie_bytes)
}

fn prepare_case_counted(
    dir: &Path,
    case: &regression::Case,
    aggregate_movie_bytes: &mut u64,
) -> Result<PreparedCase, regression::Verdict> {
    let dir_name = dir.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if dir_name != case.id {
        return Err(regression::Verdict::Invalid(format!(
            "case id ({}) does not match directory name ({dir_name})",
            case.id
        )));
    }
    if case.predicate.length == 0 || case.predicate.length > 8 {
        return Err(regression::Verdict::Invalid(format!(
            "invalid predicate length: {}",
            case.predicate.length
        )));
    }

    let mut prepared = PreparedCase {
        case: case.clone(),
        state_path: None,
        replay_start_path: None,
        movie: None,
        advances: 0,
    };
    match &case.repro {
        regression::Repro::Savestate {
            state_sha1,
            advance_frames,
        } => {
            if *advance_frames > regression::MAX_REGRESSION_ADVANCES {
                return Err(regression::Verdict::Invalid(format!(
                    "savestate advance count {advance_frames} exceeds {}",
                    regression::MAX_REGRESSION_ADVANCES
                )));
            }
            prepared.state_path =
                Some(state_member_path(dir, state_sha1).map_err(member_error_verdict)?);
            prepared.advances = *advance_frames;
        }
        regression::Repro::InputReplay { start, movie, .. } => {
            let parsed = read_prepared_movie(dir, movie, aggregate_movie_bytes)?;
            let advances = match parsed.frames.last() {
                Some(frame) => frame.frame.checked_add(1).ok_or_else(|| {
                    regression::Verdict::Invalid("movie frame count overflow".into())
                })?,
                None => 0,
            };
            if start != "reset" {
                prepared.replay_start_path =
                    Some(state_member_path(dir, start).map_err(member_error_verdict)?);
            }
            prepared.movie = Some(parsed);
            prepared.advances = advances;
        }
    }
    Ok(prepared)
}

/// Preflight the complete suite before link acquisition. Invalid individual cases remain result
/// entries, while aggregate resource-limit failures reject the request as a whole.
pub fn prepare_suite(cases: &[(PathBuf, regression::Case)]) -> Result<Vec<PlannedCase>, String> {
    let mut movie_bytes = 0u64;
    let mut advances = 0u64;
    let mut planned = Vec::with_capacity(cases.len());
    for (dir, case) in cases {
        let prepared = prepare_case_counted(dir, case, &mut movie_bytes);
        if movie_bytes > regression::MAX_SUITE_MOVIE_BYTES {
            return Err(format!(
                "regression suite movies exceed {} aggregate bytes",
                regression::MAX_SUITE_MOVIE_BYTES
            ));
        }
        if let Ok(ready) = &prepared {
            advances = advances
                .checked_add(ready.advances)
                .ok_or_else(|| "regression suite advance count overflow".to_string())?;
            if advances > regression::MAX_REGRESSION_ADVANCES {
                return Err(format!(
                    "regression suite advances exceed {}",
                    regression::MAX_REGRESSION_ADVANCES
                ));
            }
        }
        planned.push(PlannedCase {
            id: case.id.clone(),
            prepared,
        });
    }
    Ok(planned)
}

fn deadline_error() -> LinkError {
    LinkError::Protocol(format!(
        "regression operation exceeded {} ms",
        crate::live::temporal::MAX_SYNC_OPERATION_MS
    ))
}

fn call_before_deadline(
    link: &mut dyn EmulatorLink,
    deadline: Instant,
    method: &str,
    params: serde_json::Value,
) -> Result<serde_json::Value, LinkError> {
    if Instant::now() >= deadline {
        return Err(deadline_error());
    }
    link.call(method, params)
}

/// 한 번의 재생 + 관측. InputReplay는 frozen observe_hash, Savestate+Memory는 원자 probe.
fn run_repro_observe(
    link: &mut dyn EmulatorLink,
    prepared: &PreparedCase,
    observe: &crate::track::observe::ObserveSpec,
    deadline: Instant,
) -> Result<crate::track::observe::ObserveOutcome, DetOutcome> {
    use crate::track::observe::{observe_hash, sha256_hex, ObserveOutcome, ObserveSpec};
    match &prepared.case.repro {
        regression::Repro::Savestate { advance_frames, .. } => {
            // 광역 관측치는 savestate 원자 경로(probe)가 못 싣는다 → 강등.
            let ObserveSpec::Memory {
                memory_type,
                address,
                length,
            } = observe
            else {
                return Err(DetOutcome::Unsupported("savestate_broad_observe".into()));
            };
            if !has_method(link, "probe") {
                return Err(DetOutcome::Unsupported("probe".into()));
            }
            let Some(mss) = prepared.state_path.as_ref() else {
                return Err(DetOutcome::MeasurementInvalid(
                    "invalid_payload: missing preflighted state".into(),
                ));
            };
            if Instant::now() >= deadline {
                return Err(DetOutcome::MeasurementInvalid(format!(
                    "repro_error: {}",
                    deadline_error()
                )));
            }
            // probe_bytes는 length 1~8 제약 없음 — dummy op/value Predicate.
            let pred = Predicate {
                memory_type: memory_type.clone(),
                address: *address,
                length: *length,
                op: CmpOp::Eq,
                value: 0,
            };
            match bisect::probe_bytes(link, &mss.to_string_lossy(), *advance_frames, &pred) {
                Ok(bytes) => Ok(ObserveOutcome {
                    kind_used: "memory".into(),
                    sha256: sha256_hex(&bytes),
                    byte_len: bytes.len(),
                }),
                Err(e) => Err(DetOutcome::MeasurementInvalid(format!("repro_error: {e}"))),
            }
        }
        regression::Repro::InputReplay { start, anchor, .. } => {
            // 재생 필수 메서드와 observe 요구 메서드를 재생 전에 확인한다.
            // read_memory는 재생 자체엔 안 쓴다(observe=Memory일 때만 필요 → observe_missing이 검사).
            // 무조건 포함하면 screenshot/state-only 어댑터를 거짓 Unsupported로 거부한다(강등 위반).
            let mut required = vec!["pause", "set_input", "step", "clear_all_breakpoints"];
            if start == "reset" {
                required.push("reset");
            } else {
                required.push("load_state");
            }
            if anchor.is_some() {
                required.extend(["set_breakpoint", "poll_events"]);
            }
            let observe_missing = match observe {
                ObserveSpec::Auto => {
                    !has_method(link, "screenshot") && !has_method(link, "get_state")
                }
                ObserveSpec::Memory { .. } => !has_method(link, "read_memory"),
                ObserveSpec::Screenshot => !has_method(link, "screenshot"),
                ObserveSpec::State => !has_method(link, "get_state"),
            };
            let missing = missing_methods(link, &required);
            if !missing.is_empty() || observe_missing {
                return Err(DetOutcome::Unsupported(format!(
                    "missing methods: {missing:?} observe_missing={observe_missing}"
                )));
            }
            if start != "reset" {
                match load_state_replay_supported(link, deadline) {
                    Ok(true) => {}
                    Ok(false) => {
                        return Err(DetOutcome::Unsupported(
                            "load_state_nondeterministic".into(),
                        ))
                    }
                    Err(e) => {
                        return Err(DetOutcome::MeasurementInvalid(format!("repro_error: {e}")))
                    }
                }
            }
            let Some(movie) = prepared.movie.as_ref() else {
                return Err(DetOutcome::MeasurementInvalid(
                    "invalid_payload: missing preflighted movie".into(),
                ));
            };
            validate_movie_axes(link, movie, deadline).map_err(DetOutcome::MeasurementInvalid)?;
            // 시작 복원(케이스 격리)
            let start_res = if start == "reset" {
                call_before_deadline(link, deadline, "reset", serde_json::json!({})).map(|_| ())
            } else {
                let Some(mss) = prepared.replay_start_path.as_ref() else {
                    return Err(DetOutcome::MeasurementInvalid(
                        "invalid_payload: missing preflighted replay state".into(),
                    ));
                };
                call_before_deadline(
                    link,
                    deadline,
                    "load_state",
                    serde_json::json!({"path": mss.to_string_lossy()}),
                )
                .map(|_| ())
            };
            if let Err(e) = start_res {
                return Err(DetOutcome::MeasurementInvalid(format!("repro_error: {e}")));
            }
            // frozen 재생 → 종점 observe_hash(capability 선검사 통과 → 여기 에러는 측정 무효)
            let mut obs =
                |l: &mut dyn EmulatorLink| observe_hash(l, observe).map_err(|e| e.to_string());
            match replay_movie_observe(link, movie, anchor.as_ref(), &mut obs, deadline) {
                Ok(Some(o)) => Ok(o),
                Ok(None) => Err(DetOutcome::MeasurementInvalid("anchor_miss".into())),
                Err(e) => Err(DetOutcome::MeasurementInvalid(format!("repro_error: {e}"))),
            }
        }
    }
}

pub fn parse_observe_spec(
    observe: Option<&str>,
    memory_type: Option<String>,
    address: Option<u64>,
    length: Option<u64>,
) -> Result<crate::track::observe::ObserveSpec, String> {
    use crate::track::observe::ObserveSpec;
    match observe.unwrap_or("auto") {
        "auto" => Ok(ObserveSpec::Auto),
        "screenshot" => Ok(ObserveSpec::Screenshot),
        "state" => Ok(ObserveSpec::State),
        "memory" => {
            let memory_type = memory_type.ok_or("observe=memory requires memory_type")?;
            let address = address.ok_or("observe=memory requires address")?;
            let length = length.ok_or("observe=memory requires length")?;
            if length == 0 {
                return Err("observe=memory length must be at least 1".into());
            }
            Ok(ObserveSpec::Memory {
                memory_type,
                address,
                length,
            })
        }
        other => Err(format!(
            "unknown observe value: {other} (expected auto|memory|screenshot|state)"
        )),
    }
}

/// 재현 레시피를 replays회 재생해 관측치 해시 일치를 판정한다. 어느 재생이든 Err면 그
/// outcome으로 즉시 중단한다. 재생 구간 내 로깅 없음.
pub fn verify_determinism_core(
    link: &mut dyn EmulatorLink,
    dir: &std::path::Path,
    case: &regression::Case,
    observe: &crate::track::observe::ObserveSpec,
    replays: u32,
) -> DetResult {
    let prepared = match prepare_case(dir, case) {
        Ok(prepared) => prepared,
        Err(verdict) => return determinism_preflight_failure(verdict, replays),
    };
    verify_determinism_prepared_core(link, &prepared, observe, replays, regression_deadline())
}

pub fn determinism_preflight_failure(verdict: regression::Verdict, replays: u32) -> DetResult {
    let outcome = match verdict {
        regression::Verdict::Unsupported => DetOutcome::Unsupported("preflight".into()),
        regression::Verdict::MissingPayload => {
            DetOutcome::MeasurementInvalid("missing_payload".into())
        }
        regression::Verdict::Invalid(reason) => {
            DetOutcome::MeasurementInvalid(format!("invalid_payload: {reason}"))
        }
        other => DetOutcome::MeasurementInvalid(format!("preflight: {}", other.code())),
    };
    DetResult {
        outcome,
        observe_kind: String::new(),
        replays,
        hashes: vec![],
    }
}

pub fn verify_determinism_prepared_core(
    link: &mut dyn EmulatorLink,
    prepared: &PreparedCase,
    observe: &crate::track::observe::ObserveSpec,
    replays: u32,
    deadline: Instant,
) -> DetResult {
    debug_assert!(
        replays >= 2,
        "verify_determinism_core requires replays >= 2"
    );
    if replays < 2 {
        return DetResult {
            outcome: DetOutcome::MeasurementInvalid("replays_lt_2".into()),
            observe_kind: String::new(),
            replays,
            hashes: vec![],
        };
    }
    let total_advances = prepared.advances.checked_mul(u64::from(replays));
    let max_advances = regression::MAX_DETERMINISM_ADVANCES;
    if !matches!(total_advances, Some(advances) if advances <= max_advances) {
        return DetResult {
            outcome: DetOutcome::MeasurementInvalid(format!(
                "determinism advances exceed {max_advances}"
            )),
            observe_kind: String::new(),
            replays,
            hashes: vec![],
        };
    }
    let case = &prepared.case;
    // rom 대조(get_rom_info 가용 시). 불일치는 측정 무효.
    if has_method(link, "get_rom_info") {
        if let Ok(info) =
            call_before_deadline(link, deadline, "get_rom_info", serde_json::json!({}))
        {
            // Mednafen은 content_md5를 우선하고(run_start에 그 값을 넘김), Mesen/PC-98은 sha1을 쓴다.
            // 저장된 키와 같은 우선순위로 대조한다.
            let h = info
                .get("content_md5")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| info.get("sha1").and_then(|s| s.as_str()))
                .unwrap_or("");
            if !h.is_empty() && h != "skipped:too_large" && !rom_hash_matches(h, &case.rom.sha1) {
                return DetResult {
                    outcome: DetOutcome::MeasurementInvalid("rom_mismatch".into()),
                    observe_kind: String::new(),
                    replays,
                    hashes: vec![],
                };
            }
        }
    }
    let mut hashes = Vec::new();
    let mut observe_kind = String::new();
    for _ in 0..replays {
        match run_repro_observe(link, prepared, observe, deadline) {
            Ok(o) => {
                observe_kind = o.kind_used.clone();
                hashes.push(o.sha256);
            }
            Err(outcome) => {
                return DetResult {
                    outcome,
                    observe_kind,
                    replays,
                    hashes,
                }
            }
        }
    }
    let all_equal = hashes.windows(2).all(|w| w[0] == w[1]);
    let outcome = if all_equal {
        DetOutcome::Reproducible
    } else {
        DetOutcome::NotReproducible
    };
    DetResult {
        outcome,
        observe_kind,
        replays,
        hashes,
    }
}

fn rom_hash_matches(observed: &str, expected: &str) -> bool {
    observed.eq_ignore_ascii_case(expected)
}

/// 한 케이스를 현재 link으로 재현·판정한다. savestate는 원자적 probe로, 읽기 검증은 evaluate가.
pub fn run_one_case(
    link: &mut dyn EmulatorLink,
    dir: &std::path::Path,
    case: &regression::Case,
) -> regression::Verdict {
    let prepared = match prepare_case(dir, case) {
        Ok(prepared) => prepared,
        Err(verdict) => return verdict,
    };
    run_prepared_case(link, &prepared, regression_deadline())
}

pub fn run_prepared_case(
    link: &mut dyn EmulatorLink,
    prepared: &PreparedCase,
    deadline: Instant,
) -> regression::Verdict {
    let case = &prepared.case;
    if let Err(e) = ensure_capabilities_loaded_before(link, deadline) {
        return regression::Verdict::ReproError(format!("{e}"));
    }
    // rom 대조(get_rom_info를 advertise하는 어댑터만 — capability 체크로 분기). 불일치는 합산 제외.
    if link
        .capabilities()
        .methods
        .iter()
        .any(|m| m == "get_rom_info")
    {
        if let Ok(info) =
            call_before_deadline(link, deadline, "get_rom_info", serde_json::json!({}))
        {
            // Mednafen은 content_md5를 우선하고(run_start에 그 값을 넘김), Mesen/PC-98은 sha1을 쓴다.
            // 저장된 키와 같은 우선순위로 대조한다.
            let h = info
                .get("content_md5")
                .and_then(|s| s.as_str())
                .filter(|s| !s.is_empty())
                .or_else(|| info.get("sha1").and_then(|s| s.as_str()))
                .unwrap_or("");
            if !h.is_empty() && h != "skipped:too_large" && !rom_hash_matches(h, &case.rom.sha1) {
                return regression::Verdict::RomMismatch;
            }
        }
    }
    match &case.repro {
        regression::Repro::Savestate { advance_frames, .. } => {
            if !has_method(link, "probe") {
                return regression::Verdict::Unsupported;
            }
            let Some(mss) = prepared.state_path.as_ref() else {
                return regression::Verdict::Invalid("missing preflighted savestate path".into());
            };
            if Instant::now() >= deadline {
                return regression::Verdict::ReproError(deadline_error().to_string());
            }
            // 원자적 probe(load→진행→읽기). 비결정론·네트워크 갭 없음. 바이트로 invalid_read 검증.
            match bisect::probe_bytes(
                link,
                &mss.to_string_lossy(),
                *advance_frames,
                &case.predicate,
            ) {
                Ok(bytes) => regression::evaluate(&bytes, &case.predicate, case.expect),
                Err(e) => regression::Verdict::ReproError(format!("{e}")),
            }
        }
        regression::Repro::InputReplay { start, anchor, .. } => {
            let mut required = vec![
                "pause",
                "set_input",
                "step",
                "read_memory",
                "clear_all_breakpoints",
            ];
            if start == "reset" {
                required.push("reset");
            } else {
                required.push("load_state");
            }
            if anchor.is_some() {
                required.extend(["set_breakpoint", "poll_events"]);
            }
            let missing = missing_methods(link, &required);
            if !missing.is_empty() {
                return regression::Verdict::Unsupported;
            }
            if start != "reset" {
                match load_state_replay_supported(link, deadline) {
                    Ok(true) => {}
                    Ok(false) => return regression::Verdict::Unsupported,
                    Err(e) => return regression::Verdict::ReproError(e),
                }
            }
            let Some(movie) = prepared.movie.as_ref() else {
                return regression::Verdict::Invalid("missing preflighted movie".into());
            };
            if let Err(error) = validate_movie_axes(link, movie, deadline) {
                return regression::Verdict::Invalid(error);
            }
            // 시작점 복원(케이스 격리)
            let start_res = if start == "reset" {
                call_before_deadline(link, deadline, "reset", serde_json::json!({})).map(|_| ())
            } else {
                let Some(mss) = prepared.replay_start_path.as_ref() else {
                    return regression::Verdict::Invalid("missing preflighted replay state".into());
                };
                call_before_deadline(
                    link,
                    deadline,
                    "load_state",
                    serde_json::json!({"path": mss.to_string_lossy()}),
                )
                .map(|_| ())
            };
            if let Err(e) = start_res {
                return regression::Verdict::ReproError(format!("{e}"));
            }
            // frozen에서 무비를 프레임별로 적용(자유 실행 누수 차단)
            match replay_movie_and_read(link, movie, anchor.as_ref(), &case.predicate, deadline) {
                Ok(Some(bytes)) => regression::evaluate(&bytes, &case.predicate, case.expect),
                Ok(None) => regression::Verdict::DriftSuspected, // 앵커 미히트
                Err(e) => regression::Verdict::ReproError(e),
            }
        }
    }
}

pub fn ensure_capabilities_loaded(link: &mut dyn EmulatorLink) -> Result<(), LinkError> {
    ensure_capabilities_loaded_before(link, regression_deadline())
}

pub fn ensure_capabilities_loaded_before(
    link: &mut dyn EmulatorLink,
    deadline: Instant,
) -> Result<(), LinkError> {
    if link.capabilities().methods.is_empty() {
        call_before_deadline(link, deadline, "status", serde_json::json!({}))?;
    }
    Ok(())
}

fn has_method(link: &dyn EmulatorLink, method: &str) -> bool {
    if method == "probe" {
        tools::probe_available(link)
    } else {
        link.capabilities().methods.iter().any(|m| m == method)
    }
}

fn missing_methods(link: &dyn EmulatorLink, methods: &[&str]) -> Vec<String> {
    methods
        .iter()
        .copied()
        .filter(|m| !has_method(link, m))
        .map(str::to_string)
        .collect()
}

fn load_state_replay_supported(
    link: &mut dyn EmulatorLink,
    deadline: Instant,
) -> Result<bool, String> {
    if !has_method(link, "status") {
        return Ok(true);
    }
    let status = call_before_deadline(link, deadline, "status", serde_json::json!({}))
        .map_err(|e| e.to_string())?;
    let Some(state_restore) = status.get("state_restore") else {
        return Ok(true);
    };
    for key in [
        "deterministic_replay",
        "hidden_device_state",
        "post_restore_instruction_exact",
    ] {
        if state_restore.get(key).and_then(|v| v.as_bool()) == Some(false) {
            return Ok(false);
        }
    }
    Ok(true)
}

/// broker 세션 포트 기본값. emu_port가 높아도 u16 wrap(→저포트)하지 않는다.
pub fn default_session_port(emu_port: u16) -> u16 {
    emu_port.saturating_add(100)
}

/// 무비 프레임이 단조 증가하고 합리적 상한 안인지 검증한다 — 신뢰할 수 없는 무비의
/// 거대/역행 frame 번호가 무제한 step·네트워크 호출 루프를 일으키지 않게 한다.
fn validate_movie_axes(
    link: &mut dyn EmulatorLink,
    movie: &regression::Movie,
    deadline: Instant,
) -> Result<(), String> {
    if movie.frames.iter().all(|f| f.axes.is_empty()) {
        return Ok(());
    }
    let status = call_before_deadline(link, deadline, "status", serde_json::json!({}))
        .map_err(|e| e.to_string())?;
    for frame in &movie.frames {
        crate::live::controller_input::validate_controller_axes(&status, 0, &frame.axes)?;
    }
    Ok(())
}

fn validate_movie_frames(movie: &regression::Movie) -> Result<(), String> {
    for w in movie.frames.windows(2) {
        // 엄격 증가(중복 frame 거부): 같은 frame이 둘이면 재생 루프가 첫 버튼셋을 조용히 덮어쓰고
        // 프레임을 하나 더 소비해 회귀 판정이 틀린 시점에 일어난다 — 조용한 오류 대신 드러낸다.
        if w[1].frame <= w[0].frame {
            return Err(format!(
                "movie frames are not strictly increasing: {} followed by {}",
                w[0].frame, w[1].frame
            ));
        }
    }
    if movie.frames.len() > regression::MAX_REGRESSION_MOVIE_ROWS {
        return Err(format!(
            "movie exceeds {} frame rows",
            regression::MAX_REGRESSION_MOVIE_ROWS
        ));
    }
    if let Some(last) = movie.frames.last() {
        let advances = last
            .frame
            .checked_add(1)
            .ok_or_else(|| "movie frame count overflow".to_string())?;
        if advances > regression::MAX_REGRESSION_ADVANCES {
            return Err(format!(
                "movie advances {advances} exceed the limit {}",
                regression::MAX_REGRESSION_ADVANCES
            ));
        }
    }
    Ok(())
}

/// 무비를 frozen 프레임별로 적용하고, 종점(또는 anchor 히트)에서 `observe`를 1회 호출한다.
/// regression(predicate read)·결정론 재현 판정(observe_hash)이 공유한다. anchor 미히트면 Ok(None).
fn replay_movie_observe<T>(
    link: &mut dyn EmulatorLink,
    movie: &regression::Movie,
    anchor: Option<&Predicate>,
    observe: &mut dyn FnMut(&mut dyn EmulatorLink) -> Result<T, String>,
    deadline: Instant,
) -> Result<Option<T>, String> {
    validate_movie_frames(movie)?;
    validate_movie_axes(link, movie, deadline)?;
    call_before_deadline(link, deadline, "pause", serde_json::json!({}))
        .map_err(|e| e.to_string())?;
    let launch_id = link.capabilities().identity.launch_id.clone();
    let outcome = replay_movie_body(link, movie, anchor, observe, deadline);
    let cleanup = cleanup_replay(link, launch_id.as_deref());
    crate::live::temporal::finish_with_cleanup(outcome, cleanup, |primary, cleanup| match primary {
        Some(primary) => format!("{primary}; {cleanup}"),
        None => cleanup,
    })
}

fn replay_movie_body<T>(
    link: &mut dyn EmulatorLink,
    movie: &regression::Movie,
    anchor: Option<&Predicate>,
    observe: &mut dyn FnMut(&mut dyn EmulatorLink) -> Result<T, String>,
    deadline: Instant,
) -> Result<Option<T>, String> {
    let to_e = |e: LinkError| e.to_string();
    if let Some(a) = anchor {
        // arming 전에 직전 활동(예: 회귀 루프의 이전 케이스·verify_determinism 반복)이 남긴 stale BP·
        // 이벤트를 비운다 — 실패를 무시하면 옛 이벤트를 현재 anchor로 오인할 수 있으므로 fail-loud다.
        call_before_deadline(
            link,
            deadline,
            "clear_all_breakpoints",
            serde_json::json!({}),
        )
        .map_err(to_e)?;
        call_before_deadline(link, deadline, "poll_events", serde_json::json!({})).map_err(to_e)?;
        call_before_deadline(
            link,
            deadline,
            "set_breakpoint",
            serde_json::json!({
                "kind": "exec", "memory_type": a.memory_type,
                "start": a.address, "end": a.address, "pause_on_hit": true
            }),
        )
        .map_err(to_e)?;
        if let Some(v) = poll_anchor_observe(link, observe, deadline)? {
            return Ok(Some(v));
        }
    }
    let empty: Vec<String> = vec![];
    let mut next = 0u64;
    for mf in &movie.frames {
        while next < mf.frame {
            // 사이 프레임은 입력 없이 진행
            call_before_deadline(
                link,
                deadline,
                "set_input",
                serde_json::json!({"buttons": empty}),
            )
            .map_err(to_e)?;
            call_before_deadline(link, deadline, "step", serde_json::json!({"frames": 1}))
                .map_err(to_e)?;
            next += 1;
            if anchor.is_some() {
                if let Some(v) = poll_anchor_observe(link, observe, deadline)? {
                    return Ok(Some(v));
                }
            }
        }
        let mut input = serde_json::json!({"buttons":mf.buttons});
        if !mf.axes.is_empty() {
            input["axes"] = serde_json::json!(mf.axes);
        }
        call_before_deadline(link, deadline, "set_input", input).map_err(to_e)?;
        call_before_deadline(link, deadline, "step", serde_json::json!({"frames": 1}))
            .map_err(to_e)?;
        next += 1;
        if anchor.is_some() {
            if let Some(v) = poll_anchor_observe(link, observe, deadline)? {
                return Ok(Some(v));
            }
        }
    }
    if anchor.is_some() {
        return Ok(None); // 앵커 미히트 = drift/측정 무효
    }
    if Instant::now() >= deadline {
        return Err(deadline_error().to_string());
    }
    Ok(Some(observe(link)?))
}

fn cleanup_replay(
    link: &mut dyn EmulatorLink,
    expected_launch_id: Option<&str>,
) -> Result<(), String> {
    let mut failures = Vec::new();
    let mut attempt = |method: &str, params: serde_json::Value| {
        if let Err(error) = tools::call_session_cleanup(link, method, params, expected_launch_id) {
            failures.push(format!("{method}: {error}"));
        }
    };
    attempt("set_input", serde_json::json!({"buttons": []}));
    attempt("clear_all_breakpoints", serde_json::json!({}));
    attempt("pause", serde_json::json!({}));
    if failures.is_empty() {
        Ok(())
    } else {
        Err(format!("temporal cleanup failed ({})", failures.join("; ")))
    }
}

fn poll_anchor_observe<T>(
    link: &mut dyn EmulatorLink,
    observe: &mut dyn FnMut(&mut dyn EmulatorLink) -> Result<T, String>,
    deadline: Instant,
) -> Result<Option<T>, String> {
    let ev = call_before_deadline(link, deadline, "poll_events", serde_json::json!({}))
        .map_err(|e| e.to_string())?;
    if ev
        .get("events")
        .and_then(|e| e.as_array())
        .map(|a| !a.is_empty())
        .unwrap_or(false)
    {
        if Instant::now() >= deadline {
            return Err(deadline_error().to_string());
        }
        let v = observe(link)?;
        return Ok(Some(v));
    }
    Ok(None)
}

fn replay_movie_and_read(
    link: &mut dyn EmulatorLink,
    movie: &regression::Movie,
    anchor: Option<&Predicate>,
    target: &Predicate,
    deadline: Instant,
) -> Result<Option<Vec<u8>>, String> {
    let mut obs = |l: &mut dyn EmulatorLink| read_target(l, target, deadline);
    replay_movie_observe(link, movie, anchor, &mut obs, deadline)
}

fn read_target(
    link: &mut dyn EmulatorLink,
    p: &Predicate,
    deadline: Instant,
) -> Result<Vec<u8>, String> {
    let r = call_before_deadline(
        link,
        deadline,
        "read_memory",
        serde_json::json!({
            "memory_type": p.memory_type, "address": p.address, "length": p.length
        }),
    )
    .map_err(|e| e.to_string())?;
    let hex = r
        .get("hex")
        .and_then(|h| h.as_str())
        .ok_or("read_memory response has no hex field")?;
    bisect::hex_to_bytes(hex)
}
