use crate::live::runtime::RuntimeStore;

pub fn new_session_token() -> String {
    // 포맷 `{cwd_hash}-{session_anchor}-{pid}-{random}`. 세션 식별부는 앞 두 필드
    // `{cwd_hash}-{session_anchor}` — cwd는 세션마다 다르고, session_anchor는 제어 세션 id 해시라
    // 같은 세션의 서버 respawn(/mcp 재연결)엔 불변이면서 동시 실행되는 다른 세션과는 달라, 같은 cwd의
    // 형제 세션을 구별한다(reclaim 오인 방지). 부모 PID는 respawn이 다른 부모 아래로 재spawn되면 값이
    // 바뀌어, 자기 토큰을 foreign으로 오판해 실행 중 에뮬을 strand하므로 앵커로 쓰지 않는다.
    // 안정 세션 id가 없으면 앵커 필드는 0으로 적는다(값은 무의미) — 소유 판정은 저장된 앵커가 아니라
    // own_session_identity()가 Some일 때만 매칭하므로, 이 0은 형제 간 own 오판으로 이어지지 않는다.
    format!(
        "{:016x}-{:x}-{:x}-{}{}",
        cwd_hash(),
        session_anchor().unwrap_or(0),
        std::process::id(),
        ulid::Ulid::generate().to_string().to_lowercase(),
        ulid::Ulid::generate().to_string().to_lowercase(),
    )
}

/// 세션-안정·세션-고유 제어 lease label의 앵커. 명시적인 공통 ID를 우선하고 알려진 host session ID를
/// fallback으로 쓴다. 같은 세션은 /mcp 재연결·서버 respawn에도 값이 불변이고, 동시 실행되는 다른 세션은
/// 값이 달라 형제 에뮬레이터의 조용한 인계를 막는다. 부모 PID(respawn 시 변동)는 쓰지 않는다.
/// 안정된 per-session id가 없으면(plain shell·CI·테스트) `None` — 같은 cwd 형제를
/// 구별할 수 없으므로 fail-closed로 강등한다. `None`이면 어떤 기존 토큰/포트도 own으로 재사용하지 않아
/// (session_token_is_own·reusable_session_token·port_persist_path가 모두 거부/None) 형제가 살아있는
/// 에뮬을 조용히 이어받지 못한다. 옛 동작(앵커 0으로 강등)은 두 형제가 같은 식별부로 붕괴해 서로의
/// 토큰을 own으로 오판하는 인계 회귀를 냈다 — 그래서 0-강등이 아니라 None-거부로 일반화했다.
fn session_anchor() -> Option<u64> {
    for key in [
        "EMUCAP_SESSION_ID",
        "CODEX_THREAD_ID",
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_SESSION_ID",
    ] {
        if let Ok(v) = std::env::var(key) {
            let v = v.trim();
            if !v.is_empty() {
                return Some(fnv1a_64(v.as_bytes()));
            }
        }
    }
    None
}

// ── 세션당 포트 영속화 ───────────────────────────────────────
// 서버는 매 (재)시작 시 [base, base+RANGE)에서 가장 낮은 빈 포트를 잡는다. 그래서 서버가 재시작
// (/mcp 재연결·크래시 등)하면 더 낮은 포트가 비어 있을 때 그리로 옮겨가, 그 세션이 옛 포트로 띄워둔
// 에뮬레이터가 고아가 되고(freeze 화면 손실), 에이전트는 not connected를 본다. 이를 막기 위해 바인드한
// 포트를 파일에 적어두고, 다음 바인드 때 그 포트를 먼저 정확히 재바인드한다 — 서버가 죽으면 리스너가
// 닫혀 포트가 비므로 재시작 서버가 같은 포트를 되찾고, 에뮬레이터(옛 포트 고정)가 자동 재연결된다.
// 세션 구분 키는 세션 식별부(cwd+session id 앵커) — /mcp 재연결에도 유지되고 형제 세션과는 다르다.
// 안정 세션 id가 없으면 영속화 자체를 건너뛴다(port_persist_path=None) — 형제와 같은 포트 파일을
// 공유해 서로의 포트를 가로채는 창을 없앤다(fail-closed). 점유 중이면 스캔 폴백.
/// FNV-1a(64) — 프로세스·실행 무관 결정론적 해시(DefaultHasher의 시드 모호성 회피).
fn fnv1a_64(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

fn cwd_hash() -> u64 {
    let cwd = std::env::current_dir()
        .ok()
        .and_then(|p| p.to_str().map(String::from))
        .unwrap_or_default();
    fnv1a_64(cwd.as_bytes())
}

/// 이 세션(식별부 `{cwd_hash}-{session_anchor}`)+기준 포트의 포트 영속화 파일 경로. 세션 식별부로 키해
/// 형제 세션과 다른 파일을 쓴다(cwd만으로 키하면 형제가 같은 파일을 공유해 포트를 가로챈다). 안정 세션
/// id가 없거나(fail-closed) base==0(임시포트, 세션 고정 무의미)이면 `None` — 영속화를 건너뛴다.
#[cfg(test)]
pub(crate) fn port_persist_path(base: u16) -> Option<std::path::PathBuf> {
    port_persist_path_with_store(&RuntimeStore::discover().ok()?, base)
}

#[cfg(test)]
pub(crate) fn port_persist_path_with_store(
    store: &RuntimeStore,
    base: u16,
) -> Option<std::path::PathBuf> {
    if base == 0 {
        return None;
    }
    let identity = own_session_identity()?;
    store.persisted_port_path(&identity, base).ok()
}

pub(super) fn read_persisted_port(store: &RuntimeStore, base: u16) -> std::io::Result<Option<u16>> {
    let identity = own_session_identity().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "stable control session id is required for listener port persistence",
        )
    })?;
    store.read_persisted_port(&identity, base)
}

pub(crate) fn write_persisted_port(
    store: &RuntimeStore,
    base: u16,
    port: u16,
) -> std::io::Result<()> {
    let identity = own_session_identity().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "stable control session id is required for listener port persistence",
        )
    })?;
    store.write_persisted_port(&identity, base, port)
}

pub fn session_token_path(port: u16) -> std::io::Result<std::path::PathBuf> {
    Ok(RuntimeStore::discover()?.compatibility_token_path(port))
}

pub(crate) fn write_session_token(port: u16, token: &str) -> std::io::Result<()> {
    RuntimeStore::discover()?.write_compatibility_token(port, token)
}

/// 토큰 `{cwd_hash}-{session_anchor}-{pid}-{random}`의 세션 식별부 `{cwd_hash}-{session_anchor}`
/// (앞 두 필드)를 반환한다. 형식이 아니면(구분자 부족) None.
fn identity_of(token: &str) -> Option<&str> {
    let cwd_end = token.find('-')?;
    let anchor_end = token[cwd_end + 1..].find('-')?;
    Some(&token[..cwd_end + 1 + anchor_end])
}

fn is_current_session_token(token: &str) -> bool {
    let mut fields = token.split('-');
    let Some(cwd) = fields.next() else {
        return false;
    };
    let Some(anchor) = fields.next() else {
        return false;
    };
    let Some(pid) = fields.next() else {
        return false;
    };
    let Some(random) = fields.next() else {
        return false;
    };
    fields.next().is_none()
        && cwd.len() == 16
        && cwd.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !anchor.is_empty()
        && anchor.bytes().all(|byte| byte.is_ascii_hexdigit())
        && !pid.is_empty()
        && pid.bytes().all(|byte| byte.is_ascii_hexdigit())
        && is_current_random_capability(random)
}

pub(super) fn is_current_reclaim_token(token: &str) -> bool {
    token
        .strip_prefix("reclaim-")
        .is_some_and(is_current_random_capability)
}

pub(super) fn is_current_auth_token(token: &str) -> bool {
    is_current_session_token(token) || is_current_reclaim_token(token)
}

fn is_current_random_capability(random: &str) -> bool {
    random.len() == 52
        && random.bytes().all(|byte| {
            matches!(
                byte,
                b'0'..=b'9'
                    | b'a'..=b'h'
                    | b'j'
                    | b'k'
                    | b'm'
                    | b'n'
                    | b'p'..=b't'
                    | b'v'..=b'z'
            )
        })
}

/// 이 세션의 식별부 `{cwd_hash}-{session_anchor}` — 안정 세션 id가 있을 때만 Some. 없으면
/// None으로 fail-closed 신호를 전파해, 소유 판정·포트 영속화가 모두 거부된다.
pub(super) fn own_session_identity() -> Option<String> {
    session_anchor().map(|anchor| format!("{:016x}-{anchor:x}", cwd_hash()))
}

/// 토큰이 *이 세션* 소유인지 — cwd만이 아니라 세션 식별부(`{cwd_hash}-{session_anchor}`) 전체
/// 일치로 판정한다. 소유면 서버 respawn/재연결 시 토큰을 회전하지 않고 재사용해, 실행 중인 자기
/// 에뮬레이터를 strand하지 않는다(reclaim-own). 앵커가 session id라 부모 pid가 바뀌는 respawn에도
/// 자기 토큰은 계속 own이다. 같은 cwd라도 session id가 다른 형제 세션의 토큰은 own이 아니라, 그
/// 형제의 살아있는 에뮬레이터를 조용히 이어받지 않는다(identity-guard 충돌 방지).
pub fn session_token_is_own(existing: &str) -> bool {
    // 안정 세션 id가 없으면 own_session_identity()가 None → 어떤 토큰도 own이 아니다(fail-closed).
    // 이것이 두 형제 세션이 앵커 0으로 붕괴해 서로의 토큰을 own으로 오판(=조용한 인계)하던 회귀를 막는다.
    match own_session_identity() {
        Some(identity) => identity_of(existing) == Some(identity.as_str()),
        None => false,
    }
}

/// 두 세션 토큰이 같은 세션(식별부 `{cwd_hash}-{session_anchor}` 일치)인지 — 같은 cwd라도 session id가
/// 다르면 false. 형제 세션 간 reclaim 금지를 프로세스 밖에서(테스트 등) 검증할 때 쓴다.
pub fn same_session_identity(a: &str, b: &str) -> bool {
    match (identity_of(a), identity_of(b)) {
        (Some(x), Some(y)) => x == y,
        _ => false,
    }
}

/// 이 포트의 기존 토큰파일이 *이 세션 소유*면 재사용 후보로 반환한다(없거나 foreign이면 None →
/// 새 토큰 유지; foreign 에뮬은 여전히 mismatch → 진입점이 graceful 처리). 토큰파일은 포트별이나,
/// 같은 cwd의 형제 세션이 잠깐 이 포트를 놓쳐 이 세션이 바인드하더라도, 식별부(cwd+session id)가 달라
/// 형제의 살아있는 토큰은 재사용하지 않는다 — 그 형제 에뮬레이터를 조용히 이어받지 않는다.
pub(crate) fn reusable_session_token(port: u16) -> std::io::Result<Option<String>> {
    let existing = RuntimeStore::discover()?.read_compatibility_token(port)?;
    if existing
        .as_deref()
        .is_some_and(|token| session_token_is_own(token) && is_current_session_token(token))
    {
        Ok(existing)
    } else {
        Ok(None)
    }
}
