import json
from pathlib import Path

import paramiko
import pytest

from network_manager.importers import parse_share_link
from network_manager.models import SshServerProfile
from network_manager.server_deployer import (
    SHADOWSOCKS_METHOD,
    DeploymentResult,
    ServerDeploymentError,
    ServerProxyDeployer,
    build_shadowsocks_node,
    looks_like_connection_drop,
    shadowsocks_share_link,
)


def test_generated_server_node_round_trips_through_share_link_parser() -> None:
    profile = SshServerProfile(
        profile_id="server-1",
        name="Tokyo server",
        host="203.0.113.10",
        proxy_port=24443,
    )
    node = build_shadowsocks_node(profile, "bW9jay1zZWNyZXQ=")

    parsed = parse_share_link(shadowsocks_share_link(node))

    assert parsed == node
    assert parsed["cipher"] == SHADOWSOCKS_METHOD


def test_server_config_uses_encrypted_shadowsocks_on_tcp_and_udp() -> None:
    config = ServerProxyDeployer._server_config(24443, "secret")
    inbound = config["inbounds"][0]

    assert inbound["type"] == "shadowsocks"
    assert inbound["listen_port"] == 24443
    assert inbound["method"] == SHADOWSOCKS_METHOD
    assert "network" not in inbound
    assert inbound["multiplex"] == {"enabled": True}


def test_existing_remote_config_can_restore_a_node() -> None:
    profile = SshServerProfile("server-1", "Private server", "192.0.2.1", proxy_port=443)
    raw_config = json.dumps(ServerProxyDeployer._server_config(443, "existing-password"))

    node = ServerProxyDeployer._node_from_remote_config(profile, raw_config)

    assert node is not None
    assert node["server"] == profile.host
    assert node["port"] == 443
    assert node["password"] == "existing-password"


def test_remote_config_on_another_port_is_not_reused() -> None:
    profile = SshServerProfile("server-1", "Private server", "192.0.2.1", proxy_port=443)
    raw_config = json.dumps(ServerProxyDeployer._server_config(24443, "existing-password"))

    assert ServerProxyDeployer._node_from_remote_config(profile, raw_config) is None


def test_install_script_restarts_existing_service_and_keeps_rollback() -> None:
    script = ServerProxyDeployer._install_script("amd64", "/tmp/config", "/tmp/service")

    assert "systemctl restart network-manager-proxy" in script
    assert "config.backup" in script
    assert "service.backup" in script


class _FakeChannel:
    def __init__(self) -> None:
        self.exit_status = 0

    def recv_exit_status(self) -> int:
        return self.exit_status

    def shutdown_write(self) -> None:
        pass


class _FakeStdin:
    def __init__(self, channel: _FakeChannel) -> None:
        self.channel = channel
        self.written: bytes = b""

    def write(self, data: bytes) -> None:
        self.written = data


class _FakeStdout:
    def __init__(self, channel: _FakeChannel) -> None:
        self.channel = channel

    def read(self) -> bytes:
        return b""


class _FakeStderr:
    def read(self) -> bytes:
        return b""


class _FakeTransport:
    def __init__(self, active: bool) -> None:
        self._active = active

    def is_active(self) -> bool:
        return self._active


class _FakeSftp:
    def __enter__(self) -> "_FakeSftp":
        return self

    def __exit__(self, *args: object) -> bool:
        return False

    def putfo(self, fo: object, path: str) -> None:
        raise AssertionError("putfo should not be called when open fails")

    def chmod(self, path: str, mode: int) -> None:
        pass


class _FakeClient:
    def __init__(self, sftp_error: Exception | None, transport_active: bool) -> None:
        self._sftp_error = sftp_error
        self._transport_active = transport_active
        self.commands: list[str] = []
        self.stdins: list[_FakeStdin] = []

    def open_sftp(self) -> _FakeSftp:
        if self._sftp_error is not None:
            raise self._sftp_error
        return _FakeSftp()

    def get_transport(self) -> _FakeTransport:
        return _FakeTransport(self._transport_active)

    def exec_command(self, command: str, timeout: int | None = None):
        self.commands.append(command)
        channel = _FakeChannel()
        stdin = _FakeStdin(channel)
        self.stdins.append(stdin)
        return stdin, _FakeStdout(channel), _FakeStderr()


def test_upload_falls_back_to_shell_when_sftp_breaks_but_connection_alive() -> None:
    client = _FakeClient(
        sftp_error=paramiko.SSHException("Server connection dropped: "),
        transport_active=True,
    )

    ServerProxyDeployer(Path("kh"))._upload(client, "/tmp/nm.json", "content", 0o600)

    assert len(client.commands) == 1
    assert "cat > /tmp/nm.json" in client.commands[0]
    assert "chmod 600 /tmp/nm.json" in client.commands[0]
    assert client.stdins[0].written == b"content"


def test_upload_reports_failure_when_connection_is_dead() -> None:
    client = _FakeClient(
        sftp_error=paramiko.SSHException("Server connection dropped: "),
        transport_active=False,
    )

    with pytest.raises(ServerDeploymentError, match="上传远端配置失败"):
        ServerProxyDeployer(Path("kh"))._upload(client, "/tmp/nm.json", "content", 0o600)

    assert client.commands == []


def test_deploy_retries_once_after_connection_drop(monkeypatch) -> None:
    deployer = ServerProxyDeployer(Path("unused-known-hosts"))
    profile = SshServerProfile("server-1", "US server", "198.12.77.187", proxy_port=23261)
    sentinel = DeploymentResult(
        node_config={},
        share_link="ss://test",
        version="sing-box test",
        deployed_at="2026-10-07T00:00:00+00:00",
        firewall="unmanaged",
    )
    calls: list[str] = []

    def fake_deploy_once(profile, credential, report):
        calls.append(credential)
        report("正在上传代理配置")
        if len(calls) == 1:
            raise ServerDeploymentError(
                "上传远端配置失败：Server connection dropped: "
            )
        return sentinel

    monkeypatch.setattr(deployer, "_deploy_once", fake_deploy_once)
    stages: list[str] = []

    result = deployer.deploy(profile, "credential", stages.append)

    assert result is sentinel
    assert calls == ["credential", "credential"]
    assert any("重试" in stage for stage in stages)


def test_deploy_does_not_retry_auth_failure(monkeypatch) -> None:
    deployer = ServerProxyDeployer(Path("unused-known-hosts"))
    profile = SshServerProfile("server-1", "US server", "198.12.77.187", proxy_port=23261)
    calls: list[str] = []

    def fake_deploy_once(profile, credential, report):
        calls.append(credential)
        raise ServerDeploymentError("SSH 认证失败，请检查用户名、密码或私钥")

    monkeypatch.setattr(deployer, "_deploy_once", fake_deploy_once)

    with pytest.raises(ServerDeploymentError, match="认证失败"):
        deployer.deploy(profile, "credential", lambda _stage: None)

    assert len(calls) == 1


def test_looks_like_connection_drop_matches_network_errors_only() -> None:
    assert looks_like_connection_drop("上传远端配置失败：Server connection dropped: ")
    assert looks_like_connection_drop("SSH 连接失败：Connection timed out")
    assert looks_like_connection_drop("执行远端命令失败：Socket is closed")
    assert not looks_like_connection_drop("SSH 认证失败，请检查用户名、密码或私钥")
    assert not looks_like_connection_drop("自动部署目前仅支持 Linux 服务器")
