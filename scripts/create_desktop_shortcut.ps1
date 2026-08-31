param(
  [string]$TargetPath = "",
  [string]$ShortcutName = "Network Manager"
)

$ErrorActionPreference = "Stop"
$ProjectRoot = Split-Path -Parent $PSScriptRoot
if (-not $TargetPath) {
  $TargetPath = Join-Path $ProjectRoot "dist\NetworkManager\NetworkManager.exe"
}
$TargetPath = [IO.Path]::GetFullPath($TargetPath)
if (-not (Test-Path -LiteralPath $TargetPath -PathType Leaf)) {
  throw "Network Manager executable was not found: $TargetPath"
}

$desktop = [Environment]::GetFolderPath([Environment+SpecialFolder]::DesktopDirectory)
$shortcutPath = Join-Path $desktop ($ShortcutName + ".lnk")
$shell = New-Object -ComObject WScript.Shell
$shortcut = $shell.CreateShortcut($shortcutPath)
$shortcut.TargetPath = $TargetPath
$shortcut.WorkingDirectory = Split-Path -Parent $TargetPath
$shortcut.IconLocation = "$TargetPath,0"
$shortcut.Description = "Network Manager"
$shortcut.Save()

Write-Host "Desktop shortcut ready: $shortcutPath"
