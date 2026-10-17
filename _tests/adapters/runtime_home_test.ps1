$ErrorActionPreference = "Stop"
. (Join-Path $PSScriptRoot "..\..\adapters\_common\runtime-env.ps1")
$keys = @("EMUCAP_EMU_HOME", "LOCALAPPDATA", "TEMP")
$saved = @{}
foreach ($key in $keys) { $saved[$key] = [Environment]::GetEnvironmentVariable($key) }
function Assert-Root([string]$expected) {
  $actual = Get-EmucapRuntimeBase
  if ($actual -ne $expected) { throw "wrong runtime root: $actual" }
}
function Assert-Rejected {
  $rejected = $false
  try { $null = Get-EmucapRuntimeBase } catch { $rejected = $true }
  if (-not $rejected) { throw "unsafe runtime root accepted" }
}
try {
  Remove-Item Env:EMUCAP_EMU_HOME -ErrorAction SilentlyContinue
  Remove-Item Env:LOCALAPPDATA -ErrorAction SilentlyContinue
  $env:TEMP = 'C:\shared-temp'
  Assert-Rejected
  $env:EMUCAP_EMU_HOME = 'C:\owned\emucap'
  Assert-Root 'C:\owned\emucap'
  $env:LOCALAPPDATA = 'C:\Users\test\AppData\Local'
  Assert-Root 'C:\owned\emucap'
  Remove-Item Env:EMUCAP_EMU_HOME
  Assert-Root 'C:\Users\test\AppData\Local\emucap'
  foreach ($value in @('relative', '..\emucap', '\emucap', 'C:emucap')) {
    $env:EMUCAP_EMU_HOME = $value
    Assert-Rejected
  }
  $env:EMUCAP_EMU_HOME = '\\server\share\emucap'
  Assert-Root '\\server\share\emucap'
  # Parse both entry points without starting either emulator.
  foreach ($relative in @('mesen2\launch.ps1', 'dolphin\launch-native.ps1')) {
    $path = Join-Path $PSScriptRoot "..\..\adapters\$relative"
    $tokens = $null; $errors = $null
    $null = [System.Management.Automation.Language.Parser]::ParseFile($path, [ref]$tokens, [ref]$errors)
    if ($errors.Count) { throw "$relative parse failure: $errors" }
  }
  Write-Output 'runtime home: missing, override, default, relative, drive-relative, UNC and launcher syntax passed'
} finally {
  foreach ($key in $keys) { [Environment]::SetEnvironmentVariable($key, $saved[$key]) }
}
