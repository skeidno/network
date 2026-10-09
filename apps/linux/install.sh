#!/usr/bin/env bash
set -Eeuo pipefail

INSTALL_ROOT="/opt/network-manager"
CONFIG_ROOT="/etc/network-manager"
DATA_ROOT="/var/lib/network-manager"
SERVICE_PATH="/etc/systemd/system/network-manager.service"
SCRIPT_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_ROOT}/../.." && pwd)"
MIHOMO_VERSION="v1.19.30"
BASE_URL="https://github.com/MetaCubeX/mihomo/releases/download/${MIHOMO_VERSION}"

if [[ "${EUID}" -ne 0 ]]; then
  echo "Run this installer as root: sudo bash apps/linux/install.sh" >&2
  exit 1
fi

# Python 那套已经换成 Rust 单文件二进制，安装不再需要 python3 / pip / venv。
for command_name in curl sha256sum gzip systemctl install; do
  if ! command -v "${command_name}" >/dev/null 2>&1; then
    echo "Missing required command: ${command_name}" >&2
    exit 1
  fi
done

case "$(uname -m)" in
  x86_64|amd64)
    ARCH_KEY="amd64"
    MIHOMO_ASSET="mihomo-linux-amd64-compatible-v1.19.30.gz"
    MIHOMO_SHA256="db214c7a2517e63c150d123178d16d102e03a241ccdae4e5e07ffbe9cf56c6f9"
    ;;
  aarch64|arm64)
    ARCH_KEY="arm64"
    MIHOMO_ASSET="mihomo-linux-arm64-v1.19.30.gz"
    MIHOMO_SHA256="58896873736d28628f66de3677c8654fa0f180662523148e136cff4f6e890069"
    ;;
  *)
    echo "Unsupported CPU architecture: $(uname -m)" >&2
    exit 1
    ;;
esac

# 发布包里带的是已经编译好的二进制（按架构命名或统一命名）。
binary_candidates=(
  "${SCRIPT_ROOT}/network-manager-rs"
  "${SCRIPT_ROOT}/network-manager-rs-linux-${ARCH_KEY}"
)
APP_BINARY=""
for candidate in "${binary_candidates[@]}"; do
  if [[ -f "${candidate}" ]]; then
    APP_BINARY="${candidate}"
    break
  fi
done
if [[ -z "${APP_BINARY}" ]]; then
  # 从源码目录安装时现场编译：需要 rust 工具链，服务器上一般走发布包而不是这条路。
  if command -v cargo >/dev/null 2>&1 && [[ -f "${PROJECT_ROOT}/rust/Cargo.toml" ]]; then
    echo "No bundled binary found; building from source with cargo."
    (cd "${PROJECT_ROOT}/rust" && cargo build --release)
    APP_BINARY="${PROJECT_ROOT}/rust/target/release/network-manager-rs"
  else
    echo "Installer package is incomplete: network-manager-rs binary not found." >&2
    echo "Use a release archive, or install a Rust toolchain and run from a source checkout." >&2
    exit 1
  fi
fi

if [[ -f "${SCRIPT_ROOT}/network-manager.service" ]]; then
  SERVICE_SOURCE="${SCRIPT_ROOT}/network-manager.service"
else
  SERVICE_SOURCE="${PROJECT_ROOT}/apps/linux/network-manager.service"
fi
if [[ ! -f "${SERVICE_SOURCE}" ]]; then
  echo "Installer package is incomplete: network-manager.service not found." >&2
  exit 1
fi

install -d -m 0755 "${INSTALL_ROOT}/bin" "${CONFIG_ROOT}" "${DATA_ROOT}"

install -m 0755 "${APP_BINARY}" "${INSTALL_ROOT}/bin/network-manager-rs"

temp_root="$(mktemp -d)"
trap 'rm -rf -- "${temp_root}"' EXIT
core_marker="${INSTALL_ROOT}/bin/mihomo.version"
expected_marker="${MIHOMO_VERSION}:${ARCH_KEY}:${MIHOMO_SHA256}"
installed_marker="$(cat "${core_marker}" 2>/dev/null || true)"
if [[ ! -x "${INSTALL_ROOT}/bin/mihomo" || "${installed_marker}" != "${expected_marker}" ]]; then
  if [[ -f "${SCRIPT_ROOT}/${MIHOMO_ASSET}" ]]; then
    cp "${SCRIPT_ROOT}/${MIHOMO_ASSET}" "${temp_root}/${MIHOMO_ASSET}"
    echo "Using bundled Mihomo ${MIHOMO_VERSION} (${ARCH_KEY})."
  else
    curl --fail --location --retry 3 --output "${temp_root}/${MIHOMO_ASSET}" "${BASE_URL}/${MIHOMO_ASSET}"
  fi
  echo "${MIHOMO_SHA256}  ${temp_root}/${MIHOMO_ASSET}" | sha256sum --check --status
  gzip --decompress --stdout "${temp_root}/${MIHOMO_ASSET}" > "${temp_root}/mihomo"
  install -m 0755 "${temp_root}/mihomo" "${INSTALL_ROOT}/bin/mihomo"
  printf '%s\n' "${expected_marker}" > "${temp_root}/mihomo.version"
  install -m 0644 "${temp_root}/mihomo.version" "${core_marker}"
else
  echo "Mihomo ${MIHOMO_VERSION} (${ARCH_KEY}) is already installed; skipping download."
fi

env_file="${CONFIG_ROOT}/network-manager.env"
created_credentials="false"
if [[ ! -f "${env_file}" ]]; then
  umask 077
  # 不再依赖 python3 生成口令：优先 openssl，退回 /dev/urandom。
  if command -v openssl >/dev/null 2>&1; then
    web_password="$(openssl rand -base64 24 | tr -d '\n')"
  else
    web_password="$(head -c 32 /dev/urandom | base64 | tr -d '\n')"
  fi
  {
    echo "NETWORK_MANAGER_WEB_HOST=127.0.0.1"
    echo "NETWORK_MANAGER_WEB_PORT=9091"
    echo "NETWORK_MANAGER_WEB_USERNAME=admin"
    echo "NETWORK_MANAGER_WEB_PASSWORD=${web_password}"
  } > "${env_file}"
  created_credentials="true"
fi
chmod 0600 "${env_file}"

install -m 0644 "${SERVICE_SOURCE}" "${SERVICE_PATH}"
systemctl daemon-reload
systemctl enable network-manager.service >/dev/null
systemctl restart network-manager.service

# 老版本留下的 Python 虚拟环境已经没人用了，服务起成功后清掉，省几十 MB。
if [[ -d "${INSTALL_ROOT}/venv" ]]; then
  rm -rf -- "${INSTALL_ROOT}/venv"
  echo "Removed the obsolete Python virtualenv at ${INSTALL_ROOT}/venv."
fi

echo
echo "Network Manager Linux service is installed and running (Rust binary)."
echo "Import and test a proxy configuration before starting TUN interception."
echo "Local WebGUI: http://127.0.0.1:9091/"
echo "Remote access (recommended): ssh -L 9091:127.0.0.1:9091 <user>@<server>"
if [[ "${created_credentials}" == "true" ]]; then
  echo "Username: admin"
  echo "Password: ${web_password}"
else
  echo "Existing WebGUI credentials were preserved in ${env_file}."
fi
echo "Status: systemctl status network-manager --no-pager"
