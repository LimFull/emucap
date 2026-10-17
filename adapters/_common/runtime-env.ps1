# Windows launchers and the Rust runtime use the same absolute per-user data root.
function Get-EmucapRuntimeBase {
  $explicit = [Environment]::GetEnvironmentVariable("EMUCAP_EMU_HOME")
  if ($null -ne $explicit) {
    $base = $explicit
  } elseif ($env:LOCALAPPDATA) {
    $base = Join-Path $env:LOCALAPPDATA "emucap"
  } else {
    throw "LOCALAPPDATA is unavailable; set EMUCAP_EMU_HOME to an absolute private directory"
  }
  # IsPathRooted alone accepts drive-relative C:dir and current-drive-relative \dir.
  if ($base -notmatch '^(?:[A-Za-z]:[\\/]|[\\/]{2}[^\\/]+[\\/][^\\/]+(?:[\\/]|$))') {
    throw "emucap data directory must be absolute; set EMUCAP_EMU_HOME"
  }
  return $base
}

function Assert-EmucapFrontIdentity {
  if ($env:EMUCAP_SESSION_TOKEN -notmatch '^front-(?:direct|broker)-[0-9A-Fa-f]{64}$') {
    throw "EMUCAP_SESSION_TOKEN must be a managed launch-bound front key"
  }
  if ($env:EMUCAP_LAUNCH_ID -notmatch '^launch-[0-9abcdefghjkmnpqrstvwxyz]{26}$') {
    throw "EMUCAP_LAUNCH_ID must be a managed launch identity"
  }
}
