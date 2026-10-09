param(
  [string]$Version = "",
  [string]$IsccPath = "",
  [string]$OutputDir = "",
  [switch]$SkipBuild,
  # Offline builds only work when the crates are already vendored locally.
  # CI starts from an empty cargo registry cache, so this stays opt-in.
  [switch]$Offline
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $ProjectRoot

# Fetch the pinned official Mihomo core: download the zip, verify its SHA-256,
# then extract the single exe into vendor\. This used to be scripts/download_mihomo.py;
# folding it in means packaging never stops on a missing core.
function Ensure-MihomoCore {
  param([string]$VendorDir)

  $target = Join-Path $VendorDir "mihomo.exe"
  if (Test-Path -LiteralPath $target -PathType Leaf) {
    Write-Host "Mihomo is already available: $target"
    return $target
  }

  $metadataPath = Join-Path $VendorDir "mihomo.version.json"
  if (-not (Test-Path -LiteralPath $metadataPath -PathType Leaf)) {
    throw "Mihomo metadata was not found: $metadataPath"
  }
  $metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
  $coreVersion = [string]$metadata.version
  $asset = [string]$metadata.asset
  $expected = ([string]$metadata.sha256).ToLower()

  $archive = Join-Path $VendorDir ($asset + ".zip.tmp")
  $extractDir = Join-Path $VendorDir "mihomo-extract.tmp"
  $url = "https://github.com/MetaCubeX/mihomo/releases/download/$coreVersion/$asset"
  Write-Host "Downloading $url"
  try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $url -OutFile $archive -UserAgent "NetworkManager-Build" -UseBasicParsing
    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $expected) {
      throw "Mihomo checksum mismatch: expected $expected, got $actual"
    }
    if (Test-Path -LiteralPath $extractDir) {
      Remove-Item -LiteralPath $extractDir -Recurse -Force
    }
    Expand-Archive -LiteralPath $archive -DestinationPath $extractDir -Force
    $exe = @(Get-ChildItem -LiteralPath $extractDir -Recurse -Filter *.exe -File)
    if ($exe.Count -ne 1) {
      throw "Mihomo package did not contain exactly one executable (found $($exe.Count))"
    }
    Move-Item -LiteralPath $exe[0].FullName -Destination $target -Force
  }
  finally {
    if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
    if (Test-Path -LiteralPath $extractDir) { Remove-Item -LiteralPath $extractDir -Recurse -Force }
  }
  Write-Host "Installed Mihomo $coreVersion -> $target"
  return $target
}

if (-not $Version) {
  # The version lives in rust\Cargo.toml now: Rust is the only product code.
  $cargo = Get-Content -LiteralPath (Join-Path $ProjectRoot "rust\Cargo.toml")
  $match = [regex]::Match(($cargo -join "`n"), '(?m)^version\s*=\s*"([^"]+)"')
  if (-not $match.Success) {
    throw "Project version was not found in rust\Cargo.toml"
  }
  $Version = $match.Groups[1].Value
}

if ($Version -notmatch '^\d+\.\d+\.\d+$') {
  throw "Installer version must use MAJOR.MINOR.PATCH: $Version"
}

# The Windows desktop app has a single source: the Rust single-file exe,
# with mihomo.exe sitting next to it. (The old PyInstaller branch for the
# PySide6 GUI is gone; that interface was replaced by the Rust build.)
$sourceDir = Join-Path $ProjectRoot "dist-rs\NetworkManager"

if (-not $SkipBuild) {
  $buildArgs = @("build", "--release")
  if ($Offline) { $buildArgs += "--offline" }
  $buildArgs += @("--manifest-path", (Join-Path $ProjectRoot "rust\Cargo.toml"))
  & cargo @buildArgs
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
  $vendored = Ensure-MihomoCore -VendorDir (Join-Path $ProjectRoot "vendor")
  Copy-Item -LiteralPath $vendored -Destination $core -Force
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
