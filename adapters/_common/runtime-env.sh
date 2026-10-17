emucap_windows_path() {
  if command -v cygpath >/dev/null 2>&1; then
    cygpath -u "$1"
  else
    printf '%s\n' "$1"
  fi
}

emucap_runtime_base() {
  local base user_home os windows=0
  os="$(uname -s)" || return
  if [ "${EMUCAP_EMU_HOME+x}" = x ]; then
    base="$EMUCAP_EMU_HOME"
  else
    case "$os" in
      MINGW*|MSYS*|CYGWIN*)
        if [ -z "${LOCALAPPDATA:-}" ]; then
          echo 'LOCALAPPDATA is unavailable; set EMUCAP_EMU_HOME to an absolute private directory' >&2
          return 1
        fi
        base="$LOCALAPPDATA/emucap"
        ;;
      *)
        if [ "$os" != Darwin ] && [ "${XDG_DATA_HOME+x}" = x ]; then
          # Match Rust: an explicitly empty or relative root is an error.
          case "$XDG_DATA_HOME" in
            /*) base="$XDG_DATA_HOME/emucap" ;;
            *) echo 'XDG_DATA_HOME must be absolute' >&2; return 1 ;;
          esac
        else
          user_home="${HOME:-}"
          if [ -z "$user_home" ]; then
            # Bash uses the current OS account's home for ~ when HOME is unset.
            user_home="$(unset HOME; printf '%s\n' ~)" || return
          fi
          case "$user_home" in
            /*) ;;
            *) echo 'user home is unavailable; set EMUCAP_EMU_HOME' >&2; return 1 ;;
          esac
          if [ "$os" = Darwin ]; then
            base="$user_home/Library/Application Support/emucap"
          else
            base="$user_home/.local/share/emucap"
          fi
        fi
        ;;
    esac
  fi
  case "$os" in
    MINGW*|MSYS*|CYGWIN*) windows=1; base="$(emucap_windows_path "$base")" || return ;;
  esac
  case "$base" in
    /*) printf '%s\n' "$base"; return 0 ;;
    [A-Za-z]:[\\/]*) if [ "$windows" = 1 ]; then printf '%s\n' "$base"; return 0; fi ;;
  esac
  echo 'emucap data directory must be absolute; set EMUCAP_EMU_HOME' >&2
  return 1
}

# Compatibility-path discovery remains available to diagnostics. Managed producer launchers must
# receive a derived front key and do not read this root file directly.
emucap_session_token_file() {
  if [ -n "${EMUCAP_SESSION_TOKEN_FILE:-}" ]; then
    printf '%s\n' "$EMUCAP_SESSION_TOKEN_FILE"
    return 0
  fi
  local base
  base="$(emucap_runtime_base)" || return
  printf '%s/sessions/compatibility/session-token-%s\n' "${base%/}" "$1"
}

emucap_require_front_identity() {
  local key="$1" launch_id="$2" digest ulid
  case "$key" in
    front-direct-*) digest="${key#front-direct-}" ;;
    front-broker-*) digest="${key#front-broker-}" ;;
    *) echo 'EMUCAP_SESSION_TOKEN must be a managed launch-bound front key' >&2; return 1 ;;
  esac
  if [ "${#digest}" -ne 64 ]; then
    echo 'EMUCAP_SESSION_TOKEN must be a managed launch-bound front key' >&2
    return 1
  fi
  case "$digest" in
    *[!0-9a-fA-F]*) echo 'EMUCAP_SESSION_TOKEN must be a managed launch-bound front key' >&2; return 1 ;;
  esac
  case "$launch_id" in
    launch-*) ulid="${launch_id#launch-}" ;;
    *) echo 'EMUCAP_LAUNCH_ID must be a managed launch identity' >&2; return 1 ;;
  esac
  if [ "${#ulid}" -ne 26 ]; then
    echo 'EMUCAP_LAUNCH_ID must be a managed launch identity' >&2
    return 1
  fi
  case "$ulid" in
    *[!0-9abcdefghjkmnpqrstvwxyz]*) echo 'EMUCAP_LAUNCH_ID must be a managed launch identity' >&2; return 1 ;;
  esac
}
