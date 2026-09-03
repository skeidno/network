param(
  [string]$Version = "",
  [string]$IsccPath = "",
  [string]$OutputDir = "",
  [switch]$SkipWindowsBuild
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $ProjectRoot

if (-not $Version) {
  $project = Get-Content -LiteralPath (Join-Path $ProjectRoot "pyproject.toml") -Raw
  $match = [regex]::Match($project, '(?m)^version\s*=\s*"([^"]+)"')
  if (-not $match.Success) {
    throw "Project version was not found in pyproject.toml"
  }
  $Version = $match.Groups[1].Value
}

if ($Version -notmatch '^\d+\.\d+\.\d+$') {
  throw "Installer version must use MAJOR.MINOR.PATCH: $Version"
}

if (-not $SkipWindowsBuild) {
  & (Join-Path $PSScriptRoot "build_windows.ps1") -SkipDesktopShortcut
}

$sourceDir = Join-Path $ProjectRoot "dist\NetworkManager"
$executable = Join-Path $sourceDir "NetworkManager.exe"
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
  throw "Windows build was not found: $executable"
}

if (-not $OutputDir) {
  $OutputDir = Join-Path $ProjectRoot "release-assets\v$Version"
}
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null

$compilerCandidates = @(
  $IsccPath,
  (Join-Path $ProjectRoot ".cache\innosetup7\ISCC.exe"),
  (Join-Path ${env:ProgramFiles} "Inno Setup 7\ISCC.exe"),
  (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 7\ISCC.exe")
) | Where-Object { $_ }
$compiler = $compilerCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (-not $compiler) {
  throw "Inno Setup 7 compiler was not found. Install Inno Setup 7 or pass -IsccPath."
}

$script = Join-Path $ProjectRoot "apps\windows\NetworkManager.iss"
& $compiler "/DMyAppVersion=$Version" "/DSourceDir=$sourceDir" "/DOutputDir=$OutputDir" $script
if ($LASTEXITCODE -ne 0) {
  throw "Inno Setup compilation failed with exit code $LASTEXITCODE"
}

$installer = Join-Path $OutputDir "NetworkManager-Setup-x64-v$Version.exe"
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
  throw "Installer was not created: $installer"
}

Write-Host "Installer ready: $installer"
