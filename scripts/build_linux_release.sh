#!/usr/bin/env bash
# 打包可直接安装的 Linux 发布包（二进制 + 内核 + install.sh + systemd 单元）。
#
# 产物是 NetworkManager-Linux-<arch>-v<version>.tar.gz，解压后跑里面的 install.sh 即可。
# 二进制是架构相关的，在哪个机器上编的就只打那个架构（用 --arch 指定）。
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
LINUX_ROOT="$ROOT/apps/linux"
CARGO_TOML="$ROOT/rust/Cargo.toml"
CORE_MANIFEST="$LINUX_ROOT/mihomo.version.json"
CORE_BASE_URL_BASE="https://github.com/MetaCubeX/mihomo/releases/download"

OUTPUT="$ROOT/release"
CORE_DIR="${TMPDIR:-/tmp}"
BINARY=""
ARCH_FILTER=""

usage() {
  cat <<'USAGE'
用法: build_linux_release.sh [选项]

  --output DIR      输出目录（默认 <仓库>/release）
  --core-dir DIR    内核下载缓存（默认 $TMPDIR，已有且校验通过就复用）
  --binary PATH     已经编好的 network-manager-rs（不给就在这里调 cargo build）
  --arch ARCH       只打指定架构，可重复：amd64 / arm64（默认两个都打）
  -h, --help        显示本说明

注意：Rust 二进制必须在本机（或 CI 的 ubuntu runner）上用 cargo 编，
Windows 上交叉编译缺 glibc 目标，编不出来。
USAGE
}

die() {
  echo "build_linux_release: $*" >&2
  exit 1
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --output) OUTPUT="${2:?--output 需要参数}"; shift 2 ;;
    --core-dir) CORE_DIR="${2:?--core-dir 需要参数}"; shift 2 ;;
    --binary) BINARY="${2:?--binary 需要参数}"; shift 2 ;;
    --arch)
      case "$2" in
        amd64 | arm64) ARCH_FILTER="$ARCH_FILTER $2" ;;
        *) die "--arch 只支持 amd64 / arm64，收到: $2" ;;
      esac
      shift 2
      ;;
    -h | --help) usage; exit 0 ;;
    *) echo "未知参数: $1" >&2; usage; exit 2 ;;
  esac
done

command -v curl >/dev/null || die "需要 curl"
command -v tar >/dev/null || die "需要 tar"

sha256_file() {
  # 取输出里的 64 位十六进制串，而不是第一列：某些 coreutils 在文件名需要转义时
  # 会给整行加前导反斜杠（Git Bash 上就复现过），直接取 $1 会把反斜杠带进校验值。
  local tool
  if command -v sha256sum >/dev/null; then
    tool="sha256sum"
  elif command -v shasum >/dev/null; then
    tool="shasum -a 256"
  else
    die "需要 sha256sum 或 shasum"
  fi
  # 用 length() 而不是 {64}：mawk 不支持区间量词，写了会静默匹配失败。
  # 还要先剥掉可能的前导反斜杠——coreutils 转义文件名时会加它，会让长度变成 65。
  local hash
  hash=$($tool "$1" | awk '
    { for (i = 1; i <= NF; i++) {
        f = $i
        sub(/^\\/, "", f)
        if (length(f) == 64 && f ~ /^[0-9a-f]+$/) { print f; exit }
      } }')
  [[ -n "$hash" ]] || die "算不出校验值: $1"
  printf '%s' "$hash"
}

# 版本号以 rust/Cargo.toml 为准：Rust 是唯一的产品代码。
app_version() {
  awk '
    /^\[/ { in_pkg = ($0 ~ /^\[package\]/) ; next }
    in_pkg && /^version[[:space:]]*=/ {
      line = $0
      gsub(/["[:space:]]/, "", line)
      sub(/^version=/, "", line)
      print line
      exit
    }
  ' "$CARGO_TOML"
}

# 从 mihomo.version.json 取内核版本，以及指定架构的产物名与校验值。
core_version() {
  awk '/"version"[[:space:]]*:/ { v=$2; gsub(/[",]/, "", v); print v; exit }' "$CORE_MANIFEST"
}

core_asset() {
  local want="$1"
  awk -v want="$want" '
    $1 ~ /^"(amd64|arm64)":$/ { a=$1; gsub(/[":]/, "", a) }
    $1 ~ /^"name":$/          { n=$2; gsub(/[",]/, "", n); if (a==want) name=n }
    $1 ~ /^"sha256":$/        { s=$2; gsub(/[",]/, "", s); if (a==want) sha=s }
    END { if (name != "" && sha != "") print name, sha }
  ' "$CORE_MANIFEST"
}

fetch_core() {
  local name="$1" expected="$2" dir="$3" url path actual
  mkdir -p "$dir"
  path="$dir/$name"
  if [[ -f "$path" ]] && [[ "$(sha256_file "$path")" == "$expected" ]]; then
    echo "复用已缓存的内核: $path" >&2
  else
    url="$CORE_BASE_URL_BASE/$(core_version)/$name"
    echo "下载 $name ..." >&2
    curl -fsSL --retry 3 -o "$path" "$url" || die "下载失败: $url"
    actual="$(sha256_file "$path")"
    [[ "$actual" == "$expected" ]] || die "$name 校验值不符：期望 $expected 实际 $actual"
  fi
  printf '%s' "$path"
}

build_binary() {
  local dir="$1" built="$ROOT/rust/target/release/network-manager-rs"
  mkdir -p "$dir"
  # cargo 的输出走 stderr：本函数的 stdout 是给调用方当返回值用的。
  cargo build --release --manifest-path "$CARGO_TOML" >&2
  [[ -f "$built" ]] || die "cargo 没有产出 $built"
  cp -f "$built" "$dir/network-manager-rs"
  printf '%s' "$dir/network-manager-rs"
}

APP_VERSION="$(app_version)"
[[ -n "$APP_VERSION" ]] || die "没从 $CARGO_TOML 读到版本号"
CORE_VERSION="$(core_version)"
[[ -n "$CORE_VERSION" ]] || die "没从 $CORE_MANIFEST 读到内核版本"

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/network-manager-linux-build-XXXXXX")"
# 清理失败不能影响退出码：产物已经落地，临时目录留着顶多占点空间。
trap 'rm -rf "$TMP_ROOT" 2>/dev/null || true' EXIT

if [[ -n "$BINARY" ]]; then
  [[ -f "$BINARY" ]] || die "--binary 指定的文件不存在: $BINARY"
  cp -f "$BINARY" "$TMP_ROOT/network-manager-rs"
  BINARY_PATH="$TMP_ROOT/network-manager-rs"
else
  BINARY_PATH="$(build_binary "$TMP_ROOT")"
fi

mkdir -p "$OUTPUT"
ARCHIVES=()

for arch in amd64 arm64; do
  if [[ -n "$ARCH_FILTER" ]] && [[ " $ARCH_FILTER " != *" $arch "* ]]; then
    continue
  fi

  read -r core_name core_sha <<<"$(core_asset "$arch")"
  [[ -n "${core_name:-}" ]] || die "$CORE_MANIFEST 里没有 $arch 的产物"

  core_path="$(fetch_core "$core_name" "$core_sha" "$CORE_DIR")"

  package_name="NetworkManager-Linux-$arch-v$APP_VERSION"
  pkg="$TMP_ROOT/$package_name"
  mkdir -p "$pkg"

  # 统一成 LF：即使在 Windows 上打包，解压到 Linux 也不会带着 CRLF。
  for f in install.sh network-manager.service README.md; do
    tr -d '\r' <"$LINUX_ROOT/$f" >"$pkg/$f"
  done
  cp -f "$BINARY_PATH" "$pkg/network-manager-rs"
  cp -f "$core_path" "$pkg/$core_name"
  # install.sh 用 install -m 0755 落地二进制，所以这里保持 0644 是安全的。
  chmod 0755 "$pkg/install.sh"
  chmod 0644 "$pkg/network-manager.service" "$pkg/README.md" \
    "$pkg/network-manager-rs" "$pkg/$core_name"

  bin_sha="$(sha256_file "$pkg/network-manager-rs")"
  core_sha_actual="$(sha256_file "$pkg/$core_name")"

  cat >"$pkg/manifest.json" <<JSON
{
  "applicationVersion": "$APP_VERSION",
  "architecture": "$arch",
  "mihomoVersion": "$CORE_VERSION",
  "files": {
    "network-manager-rs": "$bin_sha",
    "$core_name": "$core_sha_actual"
  }
}
JSON

  {
    printf '%s  %s\n' "$bin_sha" "network-manager-rs"
    printf '%s  %s\n' "$core_sha_actual" "$core_name"
  } >"$pkg/SHA256SUMS"

  archive="$OUTPUT/$package_name.tar.gz"
  tar --owner=root --group=root -czf "$archive" -C "$TMP_ROOT" "$package_name"
  ARCHIVES+=("$archive")
  echo "已生成 $archive ($(wc -c <"$archive" | tr -d ' ') 字节)"
done

[[ ${#ARCHIVES[@]} -gt 0 ]] || die "没有产出任何架构的包，检查 --arch"

for archive in "${ARCHIVES[@]}"; do
  printf '%s  %s\n' "$(sha256_file "$archive")" "$(basename "$archive")"
done >"$OUTPUT/SHA256SUMS-Linux.txt"

echo "校验清单: $OUTPUT/SHA256SUMS-Linux.txt"
