from __future__ import annotations

import base64
from dataclasses import dataclass
from datetime import datetime, timezone
from io import BytesIO
import json
from pathlib import Path
import secrets
import shlex
import time
from typing import Callable
from urllib.parse import quote

import paramiko

from network_manager.models import SshServerProfile, server_proxy_port_error


SING_BOX_VERSION = "1.13.20"
SING_BOX_SHA256 = {
    "386": "9614c2cb8a13ea745db07afb1c84f8233e40782de5c1f1770249e51ffc3fb63a",
    "amd64": "646bc01bf128c32a12eb50d8690e387bba7504da7b1d65c704bd53916e38595a",
    "arm64": "7f8187b1d1d30258cd4fa70892eaa232649f8f28b294078eeac719579e14cf42",
    "armv7": "8740c42726b2de78cea3f9258249c839cf1dee6ddf654389574e94d9aebd7ab7",
}
SHADOWSOCKS_METHOD = "2022-blake3-aes-128-gcm"
SERVICE_NAME = "network-manager-proxy"
REMOTE_ROOT = "/etc/network-manager-proxy"
REMOTE_BINARY = "/usr/local/lib/network-manager-proxy/sing-box"


class ServerDeploymentError(RuntimeError):
    pass


@dataclass(frozen=True, slots=True)
class DeploymentResult:
    node_config: dict[str, object]
    share_link: str
    version: str
    deployed_at: str
    firewall: str
    reused: bool = False
    public_reachable: bool | None = None
    public_error: str = ""


def deployment_source_id(profile_id: str) -> str:
    return f"server-deployment:{profile_id}"


def build_shadowsocks_node(
    profile: SshServerProfile, password: str, node_name: str | None = None
) -> dict[str, object]:
    return {
        "name": node_name or profile.name,
        "type": "ss",
        "server": profile.host,
        "port": profile.proxy_port,
        "cipher": SHADOWSOCKS_METHOD,
        "password": password,
        "udp": True,
    }


def shadowsocks_share_link(node: dict[str, object]) -> str:
    method = str(node["cipher"])
    password = str(node["password"])
    userinfo = base64.urlsafe_b64encode(f"{method}:{password}".encode()).decode().rstrip("=")
    host = str(node["server"])
    if ":" in host and not host.startswith("["):
        host = f"[{host}]"
    return f"ss://{userinfo}@{host}:{int(node['port'])}#{quote(str(node['name']))}"


class ServerProxyDeployer:
    def __init__(self, known_hosts_path: Path) -> None:
        self.known_hosts_path = known_hosts_path

    def deploy(
        self,
        profile: SshServerProfile,
        credential: str = "",
        progress: Callable[[str], None] | None = None,
    ) -> DeploymentResult:
        port_error = server_proxy_port_error(profile.proxy_port, profile.port)
        if port_error:
            raise ServerDeploymentError(port_error)
        report = progress or (lambda _stage: None)
        report("正在连接 SSH")
        client = self._connect(profile, credential)
        temporary_paths: list[str] = []
        try:
            report("正在检查服务器环境")
            system, architecture, uid = self._preflight(client)
            if system != "Linux":
                raise ServerDeploymentError("自动部署目前仅支持 Linux 服务器")
            if uid != "0":
                raise ServerDeploymentError("自动部署需要 root 账号；普通账号暂不执行远端提权")
            archive_arch = {
                "x86_64": "amd64",
                "amd64": "amd64",
                "aarch64": "arm64",
                "arm64": "arm64",
                "armv7l": "armv7",
                "i386": "386",
                "i686": "386",
            }.get(architecture)
            if not archive_arch:
                raise ServerDeploymentError(f"不支持的服务器架构：{architecture}")

            proxy_password = base64.b64encode(secrets.token_bytes(16)).decode("ascii")
            node = build_shadowsocks_node(profile, proxy_password)
            config = self._server_config(profile.proxy_port, proxy_password)
            service = self._service_unit()
            token = secrets.token_hex(6)
            config_tmp = f"/tmp/network-manager-proxy-{token}.json"
            service_tmp = f"/tmp/network-manager-proxy-{token}.service"
            install_tmp = f"/tmp/network-manager-proxy-{token}.sh"
            temporary_paths.extend((config_tmp, service_tmp, install_tmp))

            report("正在上传代理配置")
            self._upload(client, config_tmp, json.dumps(config, indent=2) + "\n", 0o600)
            self._upload(client, service_tmp, service, 0o600)
            self._upload(
                client,
                install_tmp,
                self._install_script(archive_arch, config_tmp, service_tmp),
                0o700,
            )

            report(f"正在安装 sing-box {SING_BOX_VERSION}")
            self._run(client, shlex.quote(install_tmp), timeout=180)
            report("正在检查服务状态")
            version = self._run(client, f"{REMOTE_BINARY} version | head -n 1").strip()
            active = self._run(client, f"systemctl is-active {SERVICE_NAME}").strip()
            if active != "active":
                raise ServerDeploymentError("远端代理服务未能保持运行")
            firewall = self._configure_firewall(client, profile.proxy_port)
            report("部署完成")
            deployed_at = datetime.now(timezone.utc).astimezone().isoformat(timespec="seconds")
            return DeploymentResult(
                node_config=node,
                share_link=shadowsocks_share_link(node),
                version=version or f"sing-box {SING_BOX_VERSION}",
                deployed_at=deployed_at,
                firewall=firewall,
            )
        finally:
            if temporary_paths:
                command = "rm -f " + " ".join(shlex.quote(path) for path in temporary_paths)
                try:
                    self._run(client, command, timeout=10)
                except ServerDeploymentError:
                    pass
            client.close()

    def inspect(self, profile: SshServerProfile, credential: str = "") -> dict[str, object]:
        client = self._connect(profile, credential)
        try:
            status = self._run(
                client,
                f"systemctl is-active {SERVICE_NAME} 2>/dev/null || true",
            ).strip()
            version = self._run(
                client,
                f"test -x {REMOTE_BINARY} && {REMOTE_BINARY} version | head -n 1 || true",
            ).strip()
            result: dict[str, object] = {
                "status": status or "not-installed",
                "version": version,
            }
            if status == "active":
                raw_config = self._run(
                    client,
                    f"test -r {REMOTE_ROOT}/config.json && "
                    f"cat {REMOTE_ROOT}/config.json || true",
                )
                node = self._node_from_remote_config(profile, raw_config)
                if node is not None:
                    result["nodeConfig"] = node
            return result
        finally:
            client.close()

    @staticmethod
    def _node_from_remote_config(
        profile: SshServerProfile, raw_config: str
    ) -> dict[str, object] | None:
        try:
            config = json.loads(raw_config)
            inbounds = config.get("inbounds", [])
        except (AttributeError, TypeError, ValueError, json.JSONDecodeError):
            return None
        for inbound in inbounds:
            if not isinstance(inbound, dict):
                continue
            try:
                listen_port = int(inbound.get("listen_port", 0))
            except (TypeError, ValueError):
                continue
            password = str(inbound.get("password", ""))
            if (
                inbound.get("type") == "shadowsocks"
                and inbound.get("method") == SHADOWSOCKS_METHOD
                and listen_port == profile.proxy_port
                and password
            ):
                return build_shadowsocks_node(profile, password)
        return None

    def _connect(self, profile: SshServerProfile, credential: str) -> paramiko.SSHClient:
        options: dict[str, object] = {
            "hostname": profile.host,
            "port": profile.port,
            "username": profile.username,
            "timeout": 12,
            "banner_timeout": 15,
            "auth_timeout": 20,
        }
        if profile.auth_method == "password":
            if not credential:
                raise ServerDeploymentError("未找到 SSH 密码，请编辑服务器后重新保存凭据")
            options.update(password=credential, allow_agent=False, look_for_keys=False)
        elif profile.auth_method == "key":
            options.update(
                key_filename=profile.key_path,
                passphrase=credential or None,
                allow_agent=False,
                look_for_keys=False,
            )
        else:
            options.update(allow_agent=True, look_for_keys=True)
        last_error: Exception | None = None
        for attempt in range(3):
            client = paramiko.SSHClient()
            client.load_system_host_keys()
            if self.known_hosts_path.is_file():
                client.load_host_keys(str(self.known_hosts_path))
            client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
            try:
                client.connect(**options)
                transport = client.get_transport()
                if (
                    transport is None
                    or not transport.is_active()
                    or not transport.is_authenticated()
                ):
                    raise paramiko.SSHException("SSH authentication session was not established")
                transport.set_keepalive(20)
                self.known_hosts_path.parent.mkdir(parents=True, exist_ok=True)
                client.save_host_keys(str(self.known_hosts_path))
                return client
            except paramiko.AuthenticationException as exc:
                client.close()
                raise ServerDeploymentError("SSH 认证失败，请检查用户名、密码或私钥") from exc
            except paramiko.BadHostKeyException as exc:
                client.close()
                raise ServerDeploymentError("SSH 主机密钥与已保存记录不一致") from exc
            except (EOFError, OSError, paramiko.SSHException) as exc:
                client.close()
                last_error = exc
                if attempt < 2:
                    time.sleep(0.8 * (attempt + 1))
        raise ServerDeploymentError(f"SSH 连接失败：{last_error}") from last_error

    def _preflight(self, client: paramiko.SSHClient) -> tuple[str, str, str]:
        output = self._run(
            client,
            "printf '%s\\n' \"$(uname -s)\" \"$(uname -m)\" \"$(id -u)\"; "
            "command -v systemctl >/dev/null; command -v tar >/dev/null; "
            "command -v sha256sum >/dev/null; command -v base64 >/dev/null; "
            "command -v curl >/dev/null || command -v wget >/dev/null",
        )
        lines = output.splitlines()
        if len(lines) < 3:
            raise ServerDeploymentError("无法识别服务器系统环境")
        return lines[0].strip(), lines[1].strip(), lines[2].strip()

    @staticmethod
    def _server_config(port: int, password: str) -> dict[str, object]:
        return {
            "log": {"level": "info", "timestamp": True},
            "inbounds": [
                {
                    "type": "shadowsocks",
                    "tag": "ss-in",
                    "listen": "::",
                    "listen_port": port,
                    "method": SHADOWSOCKS_METHOD,
                    "password": password,
                    "multiplex": {"enabled": True},
                }
            ],
            "outbounds": [{"type": "direct", "tag": "direct"}],
            "route": {"final": "direct"},
        }

    @staticmethod
    def _service_unit() -> str:
        return f"""[Unit]
Description=Network Manager managed proxy
Documentation=https://sing-box.sagernet.org/
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
ExecStart={REMOTE_BINARY} run -c {REMOTE_ROOT}/config.json
Restart=on-failure
RestartSec=5s
LimitNOFILE=1048576
NoNewPrivileges=true
PrivateTmp=true
ProtectHome=true
ProtectSystem=strict

[Install]
WantedBy=multi-user.target
"""

    @staticmethod
    def _install_script(architecture: str, config_tmp: str, service_tmp: str) -> str:
        archive = f"sing-box-{SING_BOX_VERSION}-linux-{architecture}.tar.gz"
        release = f"https://github.com/SagerNet/sing-box/releases/download/v{SING_BOX_VERSION}"
        checksum = SING_BOX_SHA256[architecture]
        return f"""#!/bin/sh
set -eu
VERSION={shlex.quote(SING_BOX_VERSION)}
ARCHIVE={shlex.quote(archive)}
WORK_DIR=$(mktemp -d)
trap 'rm -rf "$WORK_DIR"' EXIT
cd "$WORK_DIR"
if command -v curl >/dev/null 2>&1; then
  curl --proto '=https' --tlsv1.2 -fL --retry 3 -o "$ARCHIVE" {shlex.quote(f'{release}/{archive}')}
else
  wget -qO "$ARCHIVE" {shlex.quote(f'{release}/{archive}')}
fi
EXPECTED={shlex.quote(checksum)}
ACTUAL=$(sha256sum "$ARCHIVE" | awk '{{print $1}}')
test "$EXPECTED" = "$ACTUAL"
tar -xzf "$ARCHIVE"
install -d -m 0755 /usr/local/lib/network-manager-proxy {REMOTE_ROOT}
install -m 0755 "sing-box-$VERSION-linux-{architecture}/sing-box" {REMOTE_BINARY}
{REMOTE_BINARY} check -c {shlex.quote(config_tmp)}
HAD_CONFIG=0
HAD_SERVICE=0
if test -f {REMOTE_ROOT}/config.json; then
  cp -p {REMOTE_ROOT}/config.json "$WORK_DIR/config.backup"
  HAD_CONFIG=1
fi
if test -f /etc/systemd/system/{SERVICE_NAME}.service; then
  cp -p /etc/systemd/system/{SERVICE_NAME}.service "$WORK_DIR/service.backup"
  HAD_SERVICE=1
fi
install -m 0600 {shlex.quote(config_tmp)} {REMOTE_ROOT}/config.json
install -m 0644 {shlex.quote(service_tmp)} /etc/systemd/system/{SERVICE_NAME}.service
if ! systemctl daemon-reload || ! systemctl enable {SERVICE_NAME} || ! systemctl restart {SERVICE_NAME}; then
  if test "$HAD_CONFIG" = 1; then cp -p "$WORK_DIR/config.backup" {REMOTE_ROOT}/config.json; else rm -f {REMOTE_ROOT}/config.json; fi
  if test "$HAD_SERVICE" = 1; then cp -p "$WORK_DIR/service.backup" /etc/systemd/system/{SERVICE_NAME}.service; else rm -f /etc/systemd/system/{SERVICE_NAME}.service; fi
  systemctl daemon-reload || true
  if test "$HAD_SERVICE" = 1; then systemctl restart {SERVICE_NAME} || true; else systemctl stop {SERVICE_NAME} || true; fi
  exit 1
fi
sleep 1
systemctl is-active --quiet {SERVICE_NAME}
"""

    def _configure_firewall(self, client: paramiko.SSHClient, port: int) -> str:
        command = f"""
if command -v ufw >/dev/null 2>&1 && ufw status | grep -q '^Status: active'; then
  ufw allow {port}/tcp >/dev/null
  ufw allow {port}/udp >/dev/null
  printf ufw
elif command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then
  firewall-cmd --permanent --add-port={port}/tcp >/dev/null
  firewall-cmd --permanent --add-port={port}/udp >/dev/null
  firewall-cmd --reload >/dev/null
  printf firewalld
else
  printf unmanaged
fi
"""
        return self._run(client, command).strip() or "unmanaged"

    @staticmethod
    def _upload(client: paramiko.SSHClient, path: str, content: str, mode: int) -> None:
        try:
            with client.open_sftp() as sftp:
                sftp.putfo(BytesIO(content.encode("utf-8")), path)
                sftp.chmod(path, mode)
        except (OSError, paramiko.SSHException) as exc:
            raise ServerDeploymentError(f"上传远端配置失败：{exc}") from exc

    @staticmethod
    def _run(client: paramiko.SSHClient, command: str, timeout: int = 30) -> str:
        try:
            _stdin, stdout, stderr = client.exec_command(command, timeout=timeout)
            output = stdout.read().decode("utf-8", "replace")
            error = stderr.read().decode("utf-8", "replace")
            status = stdout.channel.recv_exit_status()
        except (OSError, paramiko.SSHException) as exc:
            raise ServerDeploymentError(f"执行远端命令失败：{exc}") from exc
        if status != 0:
            detail = (error or output).strip().splitlines()
            message = detail[-1] if detail else f"退出码 {status}"
            raise ServerDeploymentError(f"远端命令失败：{message[:300]}")
        return output
