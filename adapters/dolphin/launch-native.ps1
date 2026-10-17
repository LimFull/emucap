# Manual launcher for the native Dolphin adapter on Windows.
#
#   launch-native.ps1 <ISO> <EMUCAP_PORT> [NAME] [-Dolphin <forked exe>] [-User <dir>]
#
# Prefer the MCP launch tool. This low-level launcher requires the prepared launch ID and front key
# in its environment. It does not create a managed generation or start a GDB relay.
param(
  [Parameter(Mandatory=$true)][string]$Iso,
  [Parameter(Mandatory=$true)][ValidateRange(1, 65535)][int]$EmucapPort,
  [string]$Name = "dolphin",
  [ValidateSet("gamecube", "wii")][string]$System = "gamecube",
  [string]$Dolphin = "C:\dolphin-build\dolphin-src\Binary\x64\Dolphin.exe",
  [string]$User = ""
)
$ErrorActionPreference = "Stop"

. (Join-Path $PSScriptRoot "..\_common\runtime-env.ps1")
$runtimeBase = Get-EmucapRuntimeBase
if (-not $User) { $User = Join-Path $runtimeBase "dolphin\user" }

New-Item -ItemType Directory -Force -Path "$User\Config" | Out-Null
@"
[Core]
[Interface]
ConfirmStop = False
UsePanicHandlers = False
[DSP]
Backend = No Audio Output
Volume = 0
[Analytics]
Enabled = False
PermissionAsked = True
"@ | Out-File -Encoding ascii "$User\Config\Dolphin.ini"

$env:EMUCAP_PORT = "$EmucapPort"
if (-not $env:EMUCAP_SESSION_TOKEN -and $env:EMUCAP_SESSION_TOKEN_FILE) {
  $env:EMUCAP_SESSION_TOKEN = (Get-Content -LiteralPath $env:EMUCAP_SESSION_TOKEN_FILE -TotalCount 1).Trim()
}
Assert-EmucapFrontIdentity
$env:EMUCAP_NAME = $Name
$env:EMUCAP_CONTENT = $Iso
$env:EMUCAP_SYSTEM = $System

$p = Start-Process -FilePath $Dolphin `
  -ArgumentList @("--user", "`"$User`"", "--exec", "`"$Iso`"", "--batch") -PassThru
$p.Id | Out-File -Encoding ascii "$User\dolphin.pid"
Write-Output "[launch-native] Dolphin fork pid=$($p.Id), Control MCP port=$EmucapPort"
