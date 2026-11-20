# 从 sing-box 源码重新编译 libbox.aar，并把 libbox-patches/ 下的补丁打进去。
#
# 为什么需要这个脚本：官方 CI 产出的 libbox.aar 里 v2raywebsocket.(*WebsocketConn).Close
# 没有 nil 保护，vmess/vless/trojan 走 WebSocket 拨号失败时会把空指针传进去，Go runtime
# 直接 SIGABRT 整个进程 —— 表现就是「点开始接管就闪退」。上游 v1.12 ~ v1.14 都没修。
#
# 依赖（本机一次性准备）：
#   go install github.com/sagernet/gomobile/cmd/gomobile@v0.1.12
#   go install github.com/sagernet/gomobile/cmd/gobind@v0.1.12
#   Android NDK（r27+），用 ANDROID_NDK_HOME 或 SDK/ndk/<latest> 指过去
#
# 用法：
#   .\build_libbox_patched.ps1                 # 编 arm64，写回 app\libs\libbox.aar
#   .\build_libbox_patched.ps1 -Abi amd64      # x86_64 模拟器（合并进同一个 AAR 需另行处理）
#
# 两个 Windows 上的坑（都已在本脚本里处理）：
#   1. gomobile 中断后会留下 build 目录，下次报 "go_libboxmain.go: The file exists"
#   2. with_naive_outbound 会拉 cronet-go 的 cgo，clang 在 Windows 上会挂死，别加这个 tag
param(
    [string]$SingBoxTag = "v1.13.20",
    [string]$Abi = "arm64",
    [string]$CacheRoot = ""
)

$ErrorActionPreference = "Stop"

$scriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$projectRoot = Split-Path -Parent $scriptDir          # apps\android
$repoRoot = Split-Path -Parent (Split-Path -Parent $projectRoot)
if (-not $CacheRoot) { $CacheRoot = Join-Path $repoRoot ".cache" }

$sourceDir = Join-Path $CacheRoot "sing-box"
$outputAar = Join-Path $projectRoot "app\libs\libbox.aar"

# ---- 环境 ----
$sdk = $env:ANDROID_HOME
if (-not $sdk) {
    $localProps = Join-Path $projectRoot "local.properties"
    if (Test-Path $localProps) {
        $line = Get-Content $localProps | Where-Object { $_ -match '^sdk\.dir=' } | Select-Object -First 1
        if ($line) { $sdk = ($line -replace '^sdk\.dir=', '') -replace '\\:', ':' -replace '\\', '\' }
    }
}
if (-not $sdk -or -not (Test-Path $sdk)) { throw "找不到 Android SDK，设置 ANDROID_HOME 或在 local.properties 写 sdk.dir" }

$ndk = $env:ANDROID_NDK_HOME
if (-not $ndk) {
    $ndkRoot = Join-Path $sdk "ndk"
    if (Test-Path $ndkRoot) {
        $ndk = Get-ChildItem $ndkRoot -Directory | Sort-Object Name -Descending | Select-Object -First 1 -ExpandProperty FullName
    }
}
if (-not $ndk -or -not (Test-Path (Join-Path $ndk "source.properties"))) { throw "找不到 Android NDK，设置 ANDROID_NDK_HOME" }

$gomobile = Join-Path (Join-Path $env:GOPATH "bin") "gomobile.exe"
if (-not (Test-Path $gomobile)) { throw "找不到 gomobile，先 go install github.com/sagernet/gomobile/cmd/gomobile@v0.1.12" }

Write-Output "SDK       : $sdk"
Write-Output "NDK       : $ndk"
Write-Output "gomobile  : $gomobile"
Write-Output "sing-box  : $SingBoxTag ($Abi)"

# ---- 源码 ----
if (-not (Test-Path (Join-Path $sourceDir ".git"))) {
    New-Item -ItemType Directory -Path $CacheRoot -Force | Out-Null
    git clone --depth 1 --branch $SingBoxTag https://github.com/SagerNet/sing-box.git $sourceDir
}
else {
    git -C $sourceDir fetch --depth 1 origin "refs/tags/${SingBoxTag}:refs/tags/${SingBoxTag}"
    git -C $sourceDir checkout --force $SingBoxTag
}

# ---- 补丁 ----
$patchDir = Join-Path $scriptDir "libbox-patches"
foreach ($patch in (Get-ChildItem $patchDir -Filter "*.patch" | Sort-Object Name)) {
    git -C $sourceDir apply --check $patch.FullName
    if ($LASTEXITCODE -ne 0) { throw "补丁打不上（源码版本变了？）：$($patch.Name)" }
    git -C $sourceDir apply $patch.FullName
    Write-Output "applied   : $($patch.Name)"
}

# ---- 编译 ----
$env:ANDROID_HOME = $sdk
$env:ANDROID_SDK_ROOT = $sdk
$env:ANDROID_NDK_HOME = $ndk
$env:PATH = (Join-Path $env:GOPATH "bin") + ";" + $env:PATH

Push-Location $sourceDir
try {
    # 残留的 build 目录会让 gomobile 直接失败
    if (Test-Path "build") { Remove-Item -Recurse -Force "build" }
    if (Test-Path "libbox.aar") { Remove-Item -Force "libbox.aar" }

    $tags = "with_gvisor,with_quic,with_wireguard,with_utls,with_clash_api,badlinkname,tfogo_checklinkname0"
    $ldflags = "-X github.com/sagernet/sing-box/constant.Version=$SingBoxTag -X internal/godebug.defaultGODEBUG=multipathtcp=0 -s -w -buildid= -checklinkname=0"

    & $gomobile bind -v -o libbox.aar `
        -target "android/$Abi" `
        -androidapi 23 `
        -javapkg io.nekohasekai `
        -libname box `
        -trimpath -buildvcs=false `
        -ldflags $ldflags `
        -tags $tags `
        ./experimental/libbox
    if ($LASTEXITCODE -ne 0) { throw "gomobile bind 失败" }

    Copy-Item -Force libbox.aar $outputAar
    $hash = (Get-FileHash -Algorithm SHA256 $outputAar).Hash
    Write-Output "written   : $outputAar"
    Write-Output "sha256    : $hash"
}
finally {
    Pop-Location
}
