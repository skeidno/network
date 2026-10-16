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

# TUN 模式要 wintun.dll，而且必须和内核放在同一目录：mihomo 用
# LoadLibrary("wintun.dll") 加载它，Windows 的搜索顺序里「可执行文件所在目录」
# 排在最前面，所以放在内核旁边最稳（这也是各 Clash 客户端的通行做法）。
# 不放的话 mihomo 只能在运行时联网自行下载，离线机器上 TUN 直接起不来。
#
# 许可：wintun 预编译 DLL 的授权（随 zip 提供的 LICENSE.txt）第 3.d 条允许
# 「与仅通过其 API 使用它的软件一起分发」，mihomo 正是这种用法，因此随包分发
# 合规 —— 前提是不修改 DLL、不删版权声明，所以 LICENSE.txt 也一并带上。
function Ensure-Wintun {
  param([string]$VendorDir)

  $target = Join-Path $VendorDir "wintun.dll"
  $licenseTarget = Join-Path $VendorDir "wintun-LICENSE.txt"
  if ((Test-Path -LiteralPath $target -PathType Leaf) -and (Test-Path -LiteralPath $licenseTarget -PathType Leaf)) {
    Write-Host "Wintun is already available: $target"
    return $target
  }

  $metadataPath = Join-Path $VendorDir "wintun.version.json"
  if (-not (Test-Path -LiteralPath $metadataPath -PathType Leaf)) {
    throw "Wintun metadata was not found: $metadataPath"
  }
  $metadata = Get-Content -LiteralPath $metadataPath -Raw | ConvertFrom-Json
  $asset = [string]$metadata.asset
  $expected = ([string]$metadata.sha256).ToLower()
  $dllEntry = [string]$metadata.dll
  $licenseEntry = [string]$metadata.license

  $archive = Join-Path $VendorDir ($asset + ".tmp")
  $extractDir = Join-Path $VendorDir "wintun-extract.tmp"
  $url = "https://www.wintun.net/builds/$asset"
  Write-Host "Downloading $url"
  try {
    [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest -Uri $url -OutFile $archive -UserAgent "NetworkManager-Build" -UseBasicParsing
    $actual = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLower()
    if ($actual -ne $expected) {
      throw "Wintun checksum mismatch: expected $expected, got $actual"
    }
    if (Test-Path -LiteralPath $extractDir) {
      Remove-Item -LiteralPath $extractDir -Recurse -Force
    }
    Expand-Archive -LiteralPath $archive -DestinationPath $extractDir -Force
    $dll = Join-Path $extractDir ($dllEntry -replace '/', '\')
    if (-not (Test-Path -LiteralPath $dll -PathType Leaf)) {
      throw "Wintun package did not contain $dllEntry"
    }
    Copy-Item -LiteralPath $dll -Destination $target -Force
    $license = Join-Path $extractDir ($licenseEntry -replace '/', '\')
    if (Test-Path -LiteralPath $license -PathType Leaf) {
      Copy-Item -LiteralPath $license -Destination $licenseTarget -Force
    }
  }
  finally {
    if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
    if (Test-Path -LiteralPath $extractDir) { Remove-Item -LiteralPath $extractDir -Recurse -Force }
  }
  Write-Host "Installed Wintun $($metadata.version) -> $target"
  return $target
}

# WebView2 运行时（原生界面 wry 的硬依赖）的在线安装器：内置进安装包后，
# 「目标机器没装 WebView2」这条路上安装器能自己把运行时补上，不用用户去官网找。
#
# 官方 fwlink 永远指向「当前最新版」，SHA-256 每次发布都变，没法固定校验值，
# 所以改成校验 Authenticode 签名：签名必须有效且签发者是 Microsoft。
function Ensure-WebView2Bootstrapper {
  param([string]$VendorDir)

  $target = Join-Path $VendorDir "MicrosoftEdgeWebview2Setup.exe"
  if (Test-Path -LiteralPath $target -PathType Leaf) {
    Write-Host "WebView2 bootstrapper is already available: $target"
    return $target
  }

  $url = "https://go.microsoft.com/fwlink/p/?LinkId=2124703"
  Write-Host "Downloading $url"
  [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
  Invoke-WebRequest -Uri $url -OutFile $target -UserAgent "NetworkManager-Build" -UseBasicParsing -MaximumRedirection 10

  $signature = Get-AuthenticodeSignature -LiteralPath $target
  $subject = if ($signature.SignerCertificate) { $signature.SignerCertificate.Subject } else { "" }
  if (($signature.Status -ne "Valid") -or ($subject -notlike "*O=Microsoft Corporation*")) {
    Remove-Item -LiteralPath $target -Force -ErrorAction SilentlyContinue
    throw "WebView2 bootstrapper signature check failed: status=$($signature.Status) signer=$subject"
  }
  Write-Host "WebView2 bootstrapper ready: $target"
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

# wintun.dll 必须和内核同级（TUN 模式要它，见 Ensure-Wintun 的说明）。
$wintunDll = Join-Path $sourceDir "wintun.dll"
if (-not (Test-Path -LiteralPath $wintunDll -PathType Leaf)) {
  $vendorDir = Join-Path $ProjectRoot "vendor"
  $vendoredWintun = Ensure-Wintun -VendorDir $vendorDir
  Copy-Item -LiteralPath $vendoredWintun -Destination $wintunDll -Force
  $vendoredWintunLicense = Join-Path $vendorDir "wintun-LICENSE.txt"
  if (Test-Path -LiteralPath $vendoredWintunLicense -PathType Leaf) {
    Copy-Item -LiteralPath $vendoredWintunLicense -Destination (Join-Path $sourceDir "wintun-LICENSE.txt") -Force
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

# 6 和 7 都能编（脚本没有用 7 才有的语法），探测顺序与 release.yml 保持一致：
# CI 上 choco 装的是 6，本地开发装 7 的也不少，先 6 后 7 谁在就用谁。
$compilerCandidates = @(
  $IsccPath,
  (Join-Path $ProjectRoot ".cache\innosetup7\ISCC.exe"),
  (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
  (Join-Path ${env:ProgramFiles} "Inno Setup 6\ISCC.exe"),
  (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 7\ISCC.exe"),
  (Join-Path ${env:ProgramFiles} "Inno Setup 7\ISCC.exe")
) | Where-Object { $_ }
$compiler = $compilerCandidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
if (-not $compiler) {
  throw "Inno Setup compiler was not found. Install Inno Setup 6 or 7, or pass -IsccPath."
}

$script = Join-Path $ProjectRoot "apps\windows\NetworkManager.iss"
$isccArgs = @("/DMyAppVersion=$Version", "/DSourceDir=$sourceDir", "/DOutputDir=$OutputDir")

# WebView2 在线安装器是「锦上添花」：拿到了就内置，缺失时安装包退化为提示用户
# 手动安装（见 .iss 的 PrepareToInstall），不能因为它拿不到就让整个打包失败。
try {
  $vendoredBootstrapper = Ensure-WebView2Bootstrapper -VendorDir (Join-Path $ProjectRoot "vendor")
  Copy-Item -LiteralPath $vendoredBootstrapper -Destination (Join-Path $sourceDir "MicrosoftEdgeWebview2Setup.exe") -Force
  $isccArgs += "/DHasWebView2Bootstrapper=1"
  Write-Host "WebView2 bootstrapper will be bundled into the installer."
} catch {
  Write-Warning "WebView2 在线安装器不可用，安装包将退化为提示用户手动安装：$_"
}

& $compiler @isccArgs $script
if ($LASTEXITCODE -ne 0) {
  throw "Inno Setup compilation failed with exit code $LASTEXITCODE"
}

$installer = Join-Path $OutputDir "NetworkManager-Setup-x64-v$Version.exe"
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
  throw "Installer was not created: $installer"
}

Write-Host "Installer ready: $installer"
