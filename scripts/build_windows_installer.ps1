param(
  [string]$Version = "",
  [string]$IsccPath = "",
  [string]$OutputDir = "",
  [switch]$SkipBuild
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

# Windows 桌面端只有一个来源：Rust 编出来的单个 exe，旁边放一份 mihomo.exe。
# 以前这里还有一条 PyInstaller 打包 Python 桌面 GUI 的分支（scripts/build_windows.ps1），
# 那套 PySide6 界面已经被 Rust 版取代并删除。
$sourceDir = Join-Path $ProjectRoot "dist-rs\NetworkManager"

if (-not $SkipBuild) {
  & cargo build --release --offline --manifest-path (Join-Path $ProjectRoot "rust\Cargo.toml")
  if ($LASTEXITCODE -ne 0) {
    throw "cargo build failed with exit code $LASTEXITCODE"
  }
}

$built = Join-Path $ProjectRoot "rust\target\release\network-manager-rs.exe"
if (-not (Test-Path -LiteralPath $built -PathType Leaf)) {
  throw "Rust build was not found: $built"
}

if (-not (Test-Path -LiteralPath $sourceDir -PathType Container)) {
  New-Item -ItemType Directory -Path $sourceDir -Force | Out-Null
}
Copy-Item -LiteralPath $built -Destination (Join-Path $sourceDir "NetworkManager.exe") -Force

$core = Join-Path $sourceDir "mihomo.exe"
if (-not (Test-Path -LiteralPath $core -PathType Leaf)) {
  $vendored = Join-Path $ProjectRoot "vendor\mihomo.exe"
  if (Test-Path -LiteralPath $vendored -PathType Leaf) {
    Copy-Item -LiteralPath $vendored -Destination $core -Force
  }
  else {
    throw "mihomo.exe was not found beside the app or under vendor\"
  }
}

$executable = Join-Path $sourceDir "NetworkManager.exe"
if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
  throw "Windows build was not found: $executable"
}

if (-not $OutputDir) {
  $OutputDir = Join-Path $ProjectRoot "release-assets\v$Version"
}
$OutputDir = [IO.Path]::GetFullPath($OutputDir)
if (-not (Test-Path -LiteralPath $OutputDir -PathType Container)) {
  New-Item -ItemType Directory -Path $OutputDir -Force | Out-Null
}

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
