$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
Set-Location -LiteralPath $ProjectRoot

python scripts\download_mihomo.py
python -m PyInstaller `
  --noconfirm `
  --clean `
  --windowed `
  --uac-admin `
  --name NetworkManager `
  --paths src `
  --add-data "src\network_manager\style.qss;network_manager" `
  --add-data "src\network_manager\web;network_manager\web" `
  --add-data "THIRD_PARTY_NOTICES.md;." `
  --add-binary "vendor\mihomo.exe;vendor" `
  src\network_manager\__main__.py

Write-Host "Build ready: $ProjectRoot\dist\NetworkManager\NetworkManager.exe"
