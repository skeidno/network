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

for command_name in python3 curl sha256sum gzip systemctl; do
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

wheel_candidates=("${SCRIPT_ROOT}"/network_manager-*.whl)
if [[ -f "${wheel_candidates[0]}" ]]; then
  PYTHON_PACKAGE="${wheel_candidates[0]}"
elif [[ -f "${PROJECT_ROOT}/pyproject.toml" ]]; then
  PYTHON_PACKAGE="${PROJECT_ROOT}"
else
  echo "Installer package is incomplete: Python wheel or source checkout not found." >&2
  exit 1
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

if [[ ! -x "${INSTALL_ROOT}/venv/bin/python" ]]; then
  if ! python3 -m venv "${INSTALL_ROOT}/venv"; then
    if command -v apt-get >/dev/null 2>&1; then
      echo "Python venv support is missing; installing python3-venv."
      apt-get update
      DEBIAN_FRONTEND=noninteractive apt-get install -y python3-venv
      python3 -m venv --clear "${INSTALL_ROOT}/venv"
    else
      echo "Python venv support is missing. Install the Python venv package and run again." >&2
      exit 1
    fi
  fi
fi
"${INSTALL_ROOT}/venv/bin/python" -m pip install \
  --disable-pip-version-check \
  --upgrade \
  --force-reinstall \
  "${PYTHON_PACKAGE}"

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
  web_password="$(python3 -c 'import secrets; print(secrets.token_urlsafe(24))')"
  {
    echo "NETWORK_MANAGER_WEB_HOST=127.0.0.1"
    echo "NETWORK_MANAGER_WEB_PORT=9091"
    echo "NETWORK_MANAGER_WEB_USERNAME=admin"
    echo "NETWORK_MANAGER_WEB_PASSWORD=${web_password}"
  } > "${env_file}"
  created_credentials="true"
fi
if ! grep -q '^NETWORK_MANAGER_SSH_PORTS=' "${env_file}"; then
  echo "NETWORK_MANAGER_SSH_PORTS=22" >> "${env_file}"
fi
chmod 0600 "${env_file}"

install -m 0644 "${SERVICE_SOURCE}" "${SERVICE_PATH}"
systemctl daemon-reload
systemctl enable network-manager.service >/dev/null
systemctl restart network-manager.service

echo
echo "Network Manager Linux WebGUI service is installed and running."
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
