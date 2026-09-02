param(
    [Parameter(Mandatory = $true)]
    [string]$ApkPath,

    [string]$Device = "",
    [string]$AdbPath = "adb"
)

$ErrorActionPreference = "Stop"
$resolvedApk = (Resolve-Path -LiteralPath $ApkPath).Path
$adbArgs = if ([string]::IsNullOrWhiteSpace($Device)) { @() } else { @("-s", $Device) }
$outputDirectory = Join-Path $PSScriptRoot "..\app\build\outputs"
$stdout = Join-Path $outputDirectory "release-install.stdout.txt"
$stderr = Join-Path $outputDirectory "release-install.stderr.txt"
Remove-Item -LiteralPath $stdout, $stderr -Force -ErrorAction SilentlyContinue

$process = Start-Process -FilePath $AdbPath `
    -ArgumentList ($adbArgs + @("install", "-r", "-d", $resolvedApk)) `
    -PassThru `
    -WindowStyle Hidden `
    -RedirectStandardOutput $stdout `
    -RedirectStandardError $stderr

$deadline = (Get-Date).AddMinutes(2)
$primaryLabels = @(
    "Continue",
    "Install",
    "Allow",
    "OK",
    "Confirm",
    (([string][char]0x7EE7) + ([string][char]0x7EED) + ([string][char]0x5B89) + ([string][char]0x88C5)),
    (([string][char]0x7EE7) + ([string][char]0x7EED)),
    (([string][char]0x5B89) + ([string][char]0x88C5)),
    (([string][char]0x5141) + ([string][char]0x8BB8)),
    (([string][char]0x786E) + ([string][char]0x8BA4)),
    (([string][char]0x5B8C) + ([string][char]0x6210))
)

while (-not $process.HasExited -and (Get-Date) -lt $deadline) {
    & $AdbPath @adbArgs shell uiautomator dump /sdcard/window_dump.xml | Out-Null
    $xml = (& $AdbPath @adbArgs shell cat /sdcard/window_dump.xml 2>$null) -join "`n"
    $pattern = if ($xml -match 'package="com\.miui\.securitycenter"') {
        'resource-id="android:id/button2"[\s\S]*?bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"'
    } else {
        $labels = ($primaryLabels | ForEach-Object { [regex]::Escape($_) }) -join "|"
        'text="(?:' + $labels + ')"[\s\S]*?bounds="\[(\d+),(\d+)\]\[(\d+),(\d+)\]"'
    }
    if ($xml -match $pattern) {
        $x = [int](([int]$matches[1] + [int]$matches[3]) / 2)
        $y = [int](([int]$matches[2] + [int]$matches[4]) / 2)
        & $AdbPath @adbArgs shell input tap $x $y | Out-Null
        Start-Sleep -Milliseconds 700
    }
    Start-Sleep -Milliseconds 900
    $process.Refresh()
}

if (-not $process.HasExited) {
    $process.Kill()
    throw "APK install timed out: $resolvedApk"
}

$process.WaitForExit()
$output = if (Test-Path $stdout) { Get-Content $stdout -Raw } else { "" }
$errorOutput = if (Test-Path $stderr) { Get-Content $stderr -Raw } else { "" }
$installOutput = "$output`n$errorOutput"
if ($installOutput -notmatch '(?m)^Success\s*$' -or $installOutput -match '(?m)^Failure\b') {
    throw "APK install failed: $resolvedApk`n$output`n$errorOutput"
}
Write-Output $output.Trim()
