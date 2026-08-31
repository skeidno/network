param(
    [string]$GitHubToken = $env:GITHUB_TOKEN
)

$ErrorActionPreference = "Stop"

$artifacts = @(
    @{
        Abi = "x86_64"
        Folder = "amd64"
        Id = "9714103682"
        ZipSha256 = "EF8753F1BBCBA9E54F13492B769A0E633FF2700327461762D66E567D8A5D843E"
        AarSha256 = "FE7E94C777A110E713EF5A8AA0E513282341633A9AA39784043D38B17E3F1D99"
    },
    @{
        Abi = "arm64-v8a"
        Folder = "arm64"
        Id = "9714076630"
        ZipSha256 = "0E416F3E4970B6E5D2D6D270AD1A4A80797DB49E4026BC395917613F37B080F9"
        AarSha256 = "0C717A710D53C7E09C6C0C05E9A16E00581E6CC62E68FA9FFED5D0B1AF7A5D0D"
    }
)

if (-not $GitHubToken) {
    $credentialInput = "protocol=https`nhost=github.com`n`n"
    $credentialOutput = $credentialInput | git credential fill 2>$null
    foreach ($line in $credentialOutput) {
        $pair = $line -split "=", 2
        if ($pair.Count -eq 2 -and $pair[0] -eq "password") {
            $GitHubToken = $pair[1]
            break
        }
    }
}
if (-not $GitHubToken) {
    throw "GitHub Actions artifact download requires GITHUB_TOKEN or a saved git credential."
}

$scriptRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = Split-Path -Parent $scriptRoot
$outputDir = Join-Path $projectRoot "app\libs"
$outputAar = Join-Path $outputDir "libbox.aar"
$temporaryRoot = Join-Path ([IO.Path]::GetTempPath()) ("network-manager-libbox-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null

function Assert-Sha256([string]$Path, [string]$Expected) {
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash
    if ($actual -ne $Expected) {
        throw "SHA-256 mismatch for $Path. Expected $Expected, got $actual."
    }
}

try {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $expandedAars = @{}
    foreach ($artifact in $artifacts) {
        $zipPath = Join-Path $temporaryRoot ("artifact-" + $artifact.Abi + ".zip")
        $artifactDir = Join-Path $temporaryRoot ("artifact-" + $artifact.Abi)
        $curlArgs = @(
            "--silent", "--show-error", "--fail-with-body", "--location",
            "-H", ("Authorization: Bearer " + $GitHubToken),
            "-H", "Accept: application/vnd.github+json",
            "-H", "User-Agent: NetworkManager-Android",
            "--output", $zipPath,
            ("https://api.github.com/repos/SagerNet/sing-box/actions/artifacts/" + $artifact.Id + "/zip")
        )
        & curl.exe @curlArgs
        if ($LASTEXITCODE -ne 0) {
            throw "Failed to download libbox artifact for $($artifact.Abi)."
        }
        Assert-Sha256 $zipPath $artifact.ZipSha256
        Expand-Archive -LiteralPath $zipPath -DestinationPath $artifactDir
        $aarPath = Join-Path $artifactDir ($artifact.Folder + "\libbox.aar")
        Assert-Sha256 $aarPath $artifact.AarSha256

        $expandedDir = Join-Path $temporaryRoot ("aar-" + $artifact.Abi)
        [IO.Compression.ZipFile]::ExtractToDirectory($aarPath, $expandedDir)
        $expandedAars[$artifact.Abi] = $expandedDir
    }

    $baseDir = $expandedAars["x86_64"]
    $armLibrary = Join-Path $expandedAars["arm64-v8a"] "jni\arm64-v8a\libbox.so"
    $armTargetDir = Join-Path $baseDir "jni\arm64-v8a"
    New-Item -ItemType Directory -Path $armTargetDir -Force | Out-Null
    Copy-Item -LiteralPath $armLibrary -Destination (Join-Path $armTargetDir "libbox.so") -Force

    New-Item -ItemType Directory -Path $outputDir -Force | Out-Null
    if (Test-Path -LiteralPath $outputAar) {
        [IO.File]::Delete($outputAar)
    }
    $outputStream = [IO.File]::Open($outputAar, [IO.FileMode]::CreateNew)
    try {
        $archive = [IO.Compression.ZipArchive]::new(
            $outputStream,
            [IO.Compression.ZipArchiveMode]::Create,
            $false
        )
        try {
            foreach ($file in [IO.Directory]::EnumerateFiles($baseDir, "*", [IO.SearchOption]::AllDirectories)) {
                $entryName = $file.Substring($baseDir.Length).TrimStart("\", "/").Replace("\", "/")
                $entry = $archive.CreateEntry($entryName, [IO.Compression.CompressionLevel]::Optimal)
                $entryStream = $entry.Open()
                try {
                    $inputStream = [IO.File]::OpenRead($file)
                    try {
                        $inputStream.CopyTo($entryStream)
                    }
                    finally {
                        $inputStream.Dispose()
                    }
                }
                finally {
                    $entryStream.Dispose()
                }
            }
        }
        finally {
            $archive.Dispose()
        }
    }
    finally {
        $outputStream.Dispose()
    }
    $outputHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $outputAar).Hash
    Write-Output "libbox 1.13.20 ready: $outputAar"
    Write-Output "ABIs: x86_64, arm64-v8a"
    Write-Output "SHA-256: $outputHash"
}
finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        [IO.Directory]::Delete($temporaryRoot, $true)
    }
}
