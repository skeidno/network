$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
$env:PYTHONPATH = "$ProjectRoot\src"
$Python = (Get-Command python).Source
$Pythonw = Join-Path (Split-Path -Parent $Python) "pythonw.exe"
if (-not (Test-Path -LiteralPath $Pythonw)) {
  $Pythonw = $Python
}
Start-Process -FilePath $Pythonw -ArgumentList "-m", "network_manager" -WorkingDirectory $ProjectRoot -Verb RunAs -WindowStyle Hidden
