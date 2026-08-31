param(
  [switch]$SkipDesktopShortcut
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $ProjectRoot

python scripts\download_mihomo.py
python -m PyInstaller `
  --noconfirm `
  --clean `
  --windowed `
  --uac-admin `
  --version-file "NetworkManager.version.txt" `
  --icon "src\network_manager\web\icons\network-manager.ico" `
  --name NetworkManager `
  --paths src `
  --add-data "src\network_manager\style.qss;network_manager" `
  --add-data "src\network_manager\web;network_manager\web" `
  --add-data "THIRD_PARTY_NOTICES.md;." `
  --add-binary "vendor\mihomo.exe;vendor" `
  src\network_manager\__main__.py

$executable = Join-Path $ProjectRoot "dist\NetworkManager\NetworkManager.exe"
if (-not $SkipDesktopShortcut) {
  & "$PSScriptRoot\create_desktop_shortcut.ps1" -TargetPath $executable
}

Write-Host "Build ready: $ProjectRoot\dist\NetworkManager\NetworkManager.exe"
