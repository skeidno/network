import json
import os
from pathlib import Path
from threading import Lock
from types import SimpleNamespace
from unittest.mock import patch

import pytest

from network_manager.credential_store import CredentialStore, CredentialStoreError
from network_manager.models import ImportedNode, SshServerProfile, default_config
from network_manager.server_deployer import (
    DeploymentResult,
    ServerDeploymentError,
    deployment_source_id,
    shadowsocks_share_link,
)
from network_manager.ui.web_window import WebBridge


class FakeCredentialStore:
    def __init__(self) -> None:
        self.saved: list[tuple[str, str]] = []
        self.values: dict[str, str] = {}

    def set(self, profile_id: str, credential: str) -> None:
        self.saved.append((profile_id, credential))
        self.values[profile_id] = credential

    def get(self, profile_id: str) -> str:
        return self.values.get(profile_id, "")

    def has(self, profile_id: str) -> bool:
        return bool(self.values.get(profile_id))

    def delete(self, profile_id: str) -> None:
        self.values.pop(profile_id, None)


class FakeConfigStore:
    def __init__(self) -> None:
        self.saved = 0

    def save(self, _config: object) -> None:
        self.saved += 1


class FakeWindow:
    def __init__(self, profile: SshServerProfile, core_running: bool = False) -> None:
        self.credential_store = FakeCredentialStore()
        self.store = FakeConfigStore()
        self.config = default_config()
        self.config.ssh_servers = [profile]
        self.config.imported_nodes = []
        self.config.selected_node = ""
        self.config.selected_ssh_server = ""
        self.core = SimpleNamespace(is_running=core_running)
        self.applied = 0

    def _save_and_apply(self, _message: str) -> bool:
        self.applied += 1
        return True


class FakeBridge:
    # Reuse the real helpers so the deployment flow under test behaves like production.
    _server_deployment_candidate = staticmethod(WebBridge._server_deployment_candidate)
    _node_matches_profile_endpoint = staticmethod(
        WebBridge._node_matches_profile_endpoint
    )
    _with_public_access_check = staticmethod(WebBridge._with_public_access_check)
    _deploy_server_if_needed = WebBridge._deploy_server_if_needed
    _finish_server_deploy_future = WebBridge._finish_server_deploy_future

    def __init__(self, profile: SshServerProfile, core_running: bool = False) -> None:
        self._bridge_closed = False
        self._deployment_lock = Lock()
        self._deployment_states: dict[str, dict[str, str]] = {}
        self.window = FakeWindow(profile, core_running)
        self.notifications: list[tuple[str, str]] = []

    def _ssh_profile(self, profile_id: str) -> SshServerProfile | None:
        return next(
            (
                profile
                for profile in self.window.config.ssh_servers
                if profile.profile_id == profile_id
            ),
            None,
        )

    def _deployed_node(self, profile: SshServerProfile):
        source_id = deployment_source_id(profile.profile_id)
        return next(
            (
                node
                for node in self.window.config.imported_nodes
                if node.source_id == source_id
            ),
            None,
        )

    def _notify(self, kind: str, message: str) -> None:
        self.notifications.append((kind, message))


def deployment_payload(profile: SshServerProfile) -> str:
    return json.dumps(
        {
            "node": {
                "name": profile.name,
                "type": "ss",
                "server": profile.host,
                "port": profile.proxy_port,
                "cipher": "2022-blake3-aes-128-gcm",
                "password": "base64-password",
                "udp": True,
            },
            "version": "sing-box version 1.13.20",
            "deployedAt": "2026-08-30T12:00:00+08:00",
            "firewall": "ufw",
        }
    )


def test_successful_server_deployment_persists_credential_and_node() -> None:
    profile = SshServerProfile("server-1", "Test server", "192.0.2.1")
    bridge = FakeBridge(profile)

    WebBridge._server_deploy_finished(
        bridge,
        profile.profile_id,
        True,
        "deployed",
        deployment_payload(profile),
        "secret",
    )

    assert bridge.window.credential_store.saved == [(profile.profile_id, "secret")]
    assert profile.remember_password is True
    assert profile.deployed_node_id
    assert bridge.window.config.selected_node == profile.name
    assert bridge.window.config.imported_nodes[0].source_id == deployment_source_id(
        profile.profile_id
    )
    assert bridge.window.store.saved == 1
    assert bridge.window.applied == 0
    assert bridge.notifications[0][0] == "success"


def test_successful_server_deployment_reloads_running_core() -> None:
    profile = SshServerProfile("server-1", "Test server", "192.0.2.1")
    bridge = FakeBridge(profile, core_running=True)

    WebBridge._server_deploy_finished(
        bridge, profile.profile_id, True, "deployed", deployment_payload(profile), ""
    )

    assert bridge.window.applied == 1


def test_server_deployment_warns_when_remote_service_has_no_public_port() -> None:
    profile = SshServerProfile("server-1", "Test server", "192.0.2.1")
    bridge = FakeBridge(profile)
    payload = json.loads(deployment_payload(profile))
    payload["publicReachable"] = False
    payload["publicError"] = "公网连接超时"

    WebBridge._server_deploy_finished(
        bridge,
        profile.profile_id,
        True,
        "deployed",
        json.dumps(payload),
        "",
    )

    assert profile.proxy_reachable is False
    assert profile.proxy_reachability_error == "公网连接超时"
    assert bridge._deployment_states[profile.profile_id]["status"] == "warning"
    assert bridge.notifications[-1][0] == "error"
    assert "云安全组" in bridge.notifications[-1][1]


def test_reused_remote_service_restores_missing_local_node() -> None:
    profile = SshServerProfile("server-1", "Test server", "192.0.2.1", proxy_port=443)
    bridge = FakeBridge(profile, core_running=True)
    payload = json.loads(deployment_payload(profile))
    payload["reused"] = True

    WebBridge._server_deploy_finished(
        bridge,
        profile.profile_id,
        True,
        "remote service reused",
        json.dumps(payload),
        "",
    )

    assert profile.deployed_node_id
    assert bridge.window.config.imported_nodes[0].config["port"] == 443
    assert bridge.window.config.selected_node == profile.name
    assert bridge.window.applied == 1


def test_successful_port_repair_updates_profile_and_node_together() -> None:
    profile = SshServerProfile(
        "server-1",
        "Test server",
        "192.0.2.1",
        proxy_port=443,
        deployed_node_id="node-1",
    )
    bridge = FakeBridge(profile)
    payload = json.loads(deployment_payload(profile))
    payload["node"]["port"] = 35123

    WebBridge._server_deploy_finished(
        bridge,
        profile.profile_id,
        True,
        "旧端口 443 已自动调整为 35123",
        json.dumps(payload),
        "",
    )

    assert profile.proxy_port == 35123
    assert bridge.window.config.imported_nodes[0].config["port"] == 35123


def test_deployed_low_port_uses_configured_high_port_as_repair_candidate() -> None:
    profile = SshServerProfile(
        "server-1",
        "Test server",
        "192.0.2.1",
        proxy_port=443,
        deployed_node_id="node-1",
    )

    candidate, previous_port = WebBridge._server_deployment_candidate(profile, 35123)

    assert previous_port == 443
    assert candidate.proxy_port == 35123
    assert profile.proxy_port == 443


def test_deployed_port_matching_default_remains_check_only() -> None:
    profile = SshServerProfile(
        "server-1",
        "Test server",
        "192.0.2.1",
        proxy_port=24443,
        deployed_node_id="node-1",
    )

    candidate, previous_port = WebBridge._server_deployment_candidate(profile, 24443)

    assert candidate is profile
    assert previous_port == 0


def test_deployed_different_high_port_uses_configured_port_as_repair_candidate() -> (
    None
):
    profile = SshServerProfile(
        "server-1",
        "Test server",
        "192.0.2.1",
        proxy_port=24443,
        deployed_node_id="node-1",
    )

    candidate, previous_port = WebBridge._server_deployment_candidate(profile, 35123)

    assert previous_port == 24443
    assert candidate.proxy_port == 35123
    assert profile.proxy_port == 24443


def test_failed_server_deployment_does_not_persist_credential_or_node() -> None:
    profile = SshServerProfile("server-1", "Test server", "192.0.2.1")
    bridge = FakeBridge(profile)

    WebBridge._server_deploy_finished(
        bridge, profile.profile_id, False, "failed", "", "secret"
    )

    assert bridge.window.credential_store.saved == []
    assert profile.remember_password is False
    assert bridge.window.config.imported_nodes == []
    assert bridge.window.store.saved == 0
    assert bridge.notifications == [("error", "failed")]


def test_changing_deployed_443_port_removes_old_node_before_redeploy() -> None:
    profile = SshServerProfile(
        "server-1",
        "Test server",
        "192.0.2.1",
        proxy_port=443,
        deployed_node_id="node-1",
        deployed_at="2026-08-30T12:00:00+08:00",
        deployed_version="sing-box version 1.13.20",
    )
    bridge = FakeBridge(profile, core_running=True)
    node = ImportedNode(
        node_id="node-1",
        source="server deployment",
        source_id=deployment_source_id(profile.profile_id),
        config={
            "name": profile.name,
            "type": "ss",
            "server": profile.host,
            "port": profile.proxy_port,
            "cipher": "2022-blake3-aes-128-gcm",
            "password": "base64-password",
        },
    )
    bridge.window.config.imported_nodes = [node]
    bridge.window.config.selected_node = node.name
    payload = json.dumps(
        {
            "profileId": profile.profile_id,
            "name": profile.name,
            "host": "192.0.2.1",
            "port": 22,
            "username": "root",
            "authMethod": "password",
            "rememberPassword": False,
            "proxyPort": 24444,
        }
    )

    WebBridge.saveSshServer(bridge, payload, "")

    updated = bridge.window.config.ssh_servers[0]
    assert updated.host == "192.0.2.1"
    assert updated.proxy_port == 24444
    assert updated.deployed_node_id == ""
    assert bridge.window.config.imported_nodes == []
    assert bridge.window.config.selected_node == ""
    assert bridge.window.applied == 1
    assert bridge.notifications == [("success", "服务器登录配置已保存")]


def test_saving_explicit_common_server_proxy_port_is_allowed() -> None:
    profile = SshServerProfile("server-1", "Test server", "192.0.2.1")
    bridge = FakeBridge(profile)
    payload = json.dumps(
        {
            "profileId": profile.profile_id,
            "name": profile.name,
            "host": profile.host,
            "port": 22,
            "username": "root",
            "authMethod": "password",
            "rememberPassword": False,
            "proxyPort": 443,
        }
    )

    WebBridge.saveSshServer(bridge, payload, "")

    updated = bridge.window.config.ssh_servers[0]
    assert updated.proxy_port == 443
    assert updated.deployed_node_id == ""
    assert bridge.window.applied == 1
    assert bridge.notifications[-1] == ("success", "服务器登录配置已保存")


class ImmediateFuture:
    def __init__(
        self, result: object = None, error: BaseException | None = None
    ) -> None:
        self._result = result
        self._error = error

    def add_done_callback(self, callback) -> None:
        callback(self)

    def result(self) -> object:
        if self._error is not None:
            raise self._error
        return self._result


class ImmediateExecutor:
    def submit(self, function, *args, **kwargs) -> ImmediateFuture:
        try:
            return ImmediateFuture(result=function(*args, **kwargs))
        except Exception as exc:  # mirrors the real Future error handover
            return ImmediateFuture(error=exc)


class RecordingDeployer:
    """Report a healthy remote service and remember the credential it was given."""

    def __init__(self, node_config: dict[str, object]) -> None:
        self.node_config = node_config
        self.credentials: list[str] = []

    def inspect(self, _profile, credential: str) -> dict[str, object]:
        self.credentials.append(credential)
        return {
            "status": "active",
            "version": "sing-box version 1.13.20",
            "nodeConfig": self.node_config,
        }

    def deploy(self, *_args, **_kwargs):
        raise AssertionError("an active remote service must not be redeployed")


class RotatingDeployer(RecordingDeployer):
    """Same healthy remote service, but able to run a real redeploy on demand."""

    def __init__(self, node_config: dict[str, object], rotated_password: str) -> None:
        super().__init__(node_config)
        self.rotated_password = rotated_password
        self.deploy_calls = 0

    def deploy(self, profile, _credential, _progress) -> DeploymentResult:
        self.deploy_calls += 1
        node = dict(self.node_config)
        node["password"] = self.rotated_password
        node["port"] = profile.proxy_port
        return DeploymentResult(
            node_config=node,
            share_link=shadowsocks_share_link(node),
            version="sing-box version 1.13.20",
            deployed_at="2026-10-06T10:00:00+08:00",
            firewall="ufw",
        )


def deploy_bridge(
    profile: SshServerProfile, deployer: RecordingDeployer, saved: str
) -> FakeBridge:
    bridge = FakeBridge(profile)
    bridge.window.credential_store.set(profile.profile_id, saved)
    bridge.window.credential_store.saved.clear()
    bridge._ssh_executor = ImmediateExecutor()
    bridge.window.server_deployer = deployer
    bridge.server_deploy_completed = SimpleNamespace(
        emit=lambda *args: WebBridge._server_deploy_finished(bridge, *args)
    )
    return bridge


def reused_node_config(profile: SshServerProfile) -> dict[str, object]:
    return {
        "name": profile.name,
        "type": "ss",
        "server": profile.host,
        "port": profile.proxy_port,
        "cipher": "2022-blake3-aes-128-gcm",
        "password": "base64-password",
        "udp": True,
    }


def test_deploying_with_new_password_overrides_saved_credential() -> None:
    profile = SshServerProfile(
        "server-1", "Test server", "192.0.2.1", remember_password=True
    )
    deployer = RecordingDeployer(reused_node_config(profile))
    bridge = deploy_bridge(profile, deployer, "old-password")

    with patch(
        "network_manager.ui.web_window.check_public_tcp_endpoint",
        return_value=(True, ""),
    ):
        WebBridge.deploySshServer(bridge, profile.profile_id, "new-password", False)

    assert deployer.credentials == ["new-password"]
    assert bridge.window.credential_store.saved == [("server-1", "new-password")]
    assert bridge.window.credential_store.get("server-1") == "new-password"
    assert profile.remember_password is True
    assert any("覆盖更新" in message for _kind, message in bridge.notifications)


def test_deploying_without_password_still_uses_saved_credential() -> None:
    profile = SshServerProfile(
        "server-1", "Test server", "192.0.2.1", remember_password=True
    )
    deployer = RecordingDeployer(reused_node_config(profile))
    bridge = deploy_bridge(profile, deployer, "old-password")

    with patch(
        "network_manager.ui.web_window.check_public_tcp_endpoint",
        return_value=(True, ""),
    ):
        WebBridge.deploySshServer(bridge, profile.profile_id, "", False)

    assert deployer.credentials == ["old-password"]
    assert bridge.window.credential_store.saved == []
    assert bridge.window.credential_store.get("server-1") == "old-password"


def test_forced_redeploy_rotates_node_password() -> None:
    profile = SshServerProfile(
        "server-1", "Test server", "192.0.2.1", remember_password=True
    )
    deployer = RotatingDeployer(reused_node_config(profile), "rotated-password")
    bridge = deploy_bridge(profile, deployer, "old-password")

    with patch(
        "network_manager.ui.web_window.check_public_tcp_endpoint",
        return_value=(True, ""),
    ):
        WebBridge.deploySshServer(bridge, profile.profile_id, "", False, True)

    assert deployer.deploy_calls == 1
    assert bridge.window.config.imported_nodes[0].config["password"] == "rotated-password"
    assert profile.deployed_node_id
    assert any("轮换" in message for _kind, message in bridge.notifications)


def test_check_service_without_force_keeps_running_service_password() -> None:
    profile = SshServerProfile(
        "server-1", "Test server", "192.0.2.1", remember_password=True
    )
    deployer = RotatingDeployer(reused_node_config(profile), "rotated-password")
    bridge = deploy_bridge(profile, deployer, "old-password")

    with patch(
        "network_manager.ui.web_window.check_public_tcp_endpoint",
        return_value=(True, ""),
    ):
        WebBridge.deploySshServer(bridge, profile.profile_id, "", False, False)

    assert deployer.deploy_calls == 0
    assert bridge.window.config.imported_nodes[0].config["password"] == "base64-password"


def test_failed_deployment_with_new_password_keeps_old_credential() -> None:
    profile = SshServerProfile(
        "server-1", "Test server", "192.0.2.1", remember_password=True
    )

    class FailingDeployer:
        def inspect(self, _profile, _credential):
            raise ServerDeploymentError("SSH 认证失败，请检查用户名、密码或私钥")

    bridge = FakeBridge(profile)
    bridge.window.credential_store.set(profile.profile_id, "old-password")
    bridge.window.credential_store.saved.clear()
    bridge._ssh_executor = ImmediateExecutor()
    bridge.window.server_deployer = FailingDeployer()
    bridge.server_deploy_completed = SimpleNamespace(
        emit=lambda *args: WebBridge._server_deploy_finished(bridge, *args)
    )

    with patch(
        "network_manager.ui.web_window.check_public_tcp_endpoint",
        return_value=(True, ""),
    ):
        WebBridge.deploySshServer(bridge, profile.profile_id, "typed-wrong", True)

    assert bridge.window.credential_store.get("server-1") == "old-password"


def test_fallback_rule_target_is_persisted() -> None:
    profile = SshServerProfile("server-1", "Test server", "192.0.2.1")
    bridge = FakeBridge(profile)
    bridge.window._refresh_rules_table = lambda: None

    WebBridge.setDefaultTarget(bridge, "V2RAY")

    assert bridge.window.config.default_target == "V2RAY"
    assert bridge.window.applied == 1
    assert bridge.notifications == [("success", "强制保底规则已更新")]


def test_existing_active_server_service_is_reused_without_deploying() -> None:
    profile = SshServerProfile(
        "server-1",
        "Test server",
        "192.0.2.1",
        deployed_node_id="node-1",
        deployed_at="2026-08-30T12:00:00+08:00",
        deployed_version="sing-box version 1.13.20",
    )
    bridge = FakeBridge(profile)

    class ExistingServiceDeployer:
        def inspect(self, _profile, _credential):
            return {
                "status": "active",
                "version": "sing-box version 1.13.20",
                "nodeConfig": node_config,
            }

        def deploy(self, *_args, **_kwargs):
            raise AssertionError("active remote service must not be redeployed")

    bridge.window.server_deployer = ExistingServiceDeployer()
    node_config = {
        "name": profile.name,
        "type": "ss",
        "server": profile.host,
        "port": profile.proxy_port,
        "cipher": "2022-blake3-aes-128-gcm",
        "password": "base64-password",
    }
    stages: list[str] = []

    with patch(
        "network_manager.ui.web_window.check_public_tcp_endpoint",
        return_value=(True, ""),
    ):
        result = WebBridge._deploy_server_if_needed(
            bridge,
            profile,
            "credential",
            node_config,
            profile.deployed_at,
            stages.append,
        )

    assert result.reused is True
    assert result.node_config == node_config
    assert result.deployed_at == profile.deployed_at
    assert stages == ["正在检查远端代理服务", "正在验证公网代理端口"]
    assert result.public_reachable is True


def test_existing_active_server_is_discovered_without_local_node() -> None:
    profile = SshServerProfile("server-1", "Test server", "192.0.2.1", proxy_port=443)
    bridge = FakeBridge(profile)
    node_config = {
        "name": profile.name,
        "type": "ss",
        "server": profile.host,
        "port": profile.proxy_port,
        "cipher": "2022-blake3-aes-128-gcm",
        "password": "base64-password",
        "udp": True,
    }

    class ExistingServiceDeployer:
        def inspect(self, _profile, _credential):
            return {
                "status": "active",
                "version": "sing-box version 1.13.20",
                "nodeConfig": node_config,
            }

        def deploy(self, *_args, **_kwargs):
            raise AssertionError("discovered active service must not be redeployed")

    bridge.window.server_deployer = ExistingServiceDeployer()
    stages: list[str] = []

    with patch(
        "network_manager.ui.web_window.check_public_tcp_endpoint",
        return_value=(True, ""),
    ):
        result = WebBridge._deploy_server_if_needed(
            bridge,
            profile,
            "credential",
            None,
            "",
            stages.append,
        )

    assert result.reused is True
    assert result.node_config == node_config
    assert result.deployed_at
    assert stages == ["正在查找远端现有代理服务", "正在验证公网代理端口"]


def test_stale_local_node_does_not_block_remote_port_repair() -> None:
    profile = SshServerProfile(
        "server-1",
        "Test server",
        "192.0.2.1",
        proxy_port=35123,
        deployed_node_id="node-1",
    )
    bridge = FakeBridge(profile)
    old_node = {
        "name": profile.name,
        "type": "ss",
        "server": profile.host,
        "port": 443,
        "cipher": "2022-blake3-aes-128-gcm",
        "password": "old-password",
    }
    deployed_profiles: list[SshServerProfile] = []

    class RepairingDeployer:
        def inspect(self, _profile, _credential):
            return {"status": "active", "version": "sing-box version 1.13.20"}

        def deploy(self, deployed_profile, _credential, _progress):
            deployed_profiles.append(deployed_profile)
            node = {**old_node, "port": deployed_profile.proxy_port}
            return DeploymentResult(
                node_config=node,
                share_link="ss://repaired",
                version="sing-box version 1.13.20",
                deployed_at="2026-09-01T12:00:00+08:00",
                firewall="ufw",
            )

    bridge.window.server_deployer = RepairingDeployer()
    stages: list[str] = []

    with patch(
        "network_manager.ui.web_window.check_public_tcp_endpoint",
        return_value=(True, ""),
    ):
        result = WebBridge._deploy_server_if_needed(
            bridge,
            profile,
            "credential",
            old_node,
            "",
            stages.append,
        )

    assert deployed_profiles == [profile]
    assert result.reused is False
    assert result.node_config["port"] == 35123
    assert len(stages) == 3


def ssh_payload(profile: SshServerProfile, remember: bool, port: int = 24444) -> str:
    return json.dumps(
        {
            "profileId": profile.profile_id,
            "name": profile.name,
            "host": profile.host,
            "port": 22,
            "username": "root",
            "authMethod": "password",
            "rememberPassword": remember,
            "proxyPort": port,
        }
    )


def test_editing_server_without_password_keeps_stored_credential() -> None:
    profile = SshServerProfile(
        "server-1", "Test server", "192.0.2.1", remember_password=True
    )
    bridge = FakeBridge(profile)
    bridge.window.credential_store.set(profile.profile_id, "saved-password")

    WebBridge.saveSshServer(bridge, ssh_payload(profile, True), "")

    assert bridge.window.credential_store.get("server-1") == "saved-password"
    assert bridge.notifications[-1] == ("success", "服务器登录配置已保存")


def test_editing_server_with_new_password_replaces_stored_credential() -> None:
    profile = SshServerProfile(
        "server-1", "Test server", "192.0.2.1", remember_password=True
    )
    bridge = FakeBridge(profile)
    bridge.window.credential_store.set(profile.profile_id, "old-password")

    WebBridge.saveSshServer(bridge, ssh_payload(profile, True), "rotated-password")

    assert bridge.window.credential_store.get("server-1") == "rotated-password"


def test_turning_off_remember_reports_cleared_credential() -> None:
    profile = SshServerProfile(
        "server-1", "Test server", "192.0.2.1", remember_password=True
    )
    bridge = FakeBridge(profile)
    bridge.window.credential_store.set(profile.profile_id, "saved-password")

    WebBridge.saveSshServer(bridge, ssh_payload(profile, False), "")

    assert bridge.window.credential_store.get("server-1") == ""
    assert "清除" in bridge.notifications[-1][1]


def test_forgetting_credential_keeps_server_record() -> None:
    profile = SshServerProfile(
        "server-1", "Test server", "192.0.2.1", remember_password=True
    )
    bridge = FakeBridge(profile)
    bridge.window.credential_store.set(profile.profile_id, "saved-password")

    WebBridge.forgetSshCredential(bridge, profile.profile_id)

    assert bridge.window.credential_store.get("server-1") == ""
    assert profile.remember_password is False
    assert [item.profile_id for item in bridge.window.config.ssh_servers] == ["server-1"]
    assert any("清除" in message for _kind, message in bridge.notifications)


def test_forgetting_credential_without_stored_value_reports_info() -> None:
    profile = SshServerProfile("server-1", "Test server", "192.0.2.1")
    bridge = FakeBridge(profile)

    WebBridge.forgetSshCredential(bridge, profile.profile_id)

    assert bridge.notifications[-1][0] == "info"


@pytest.mark.skipif(os.name != "nt", reason="DPAPI 凭据存储仅在 Windows 上可用")
def test_credential_store_ignores_empty_password(tmp_path: Path) -> None:
    store = CredentialStore(tmp_path / "ssh-credentials.json")
    store.set("server-1", "saved-password")

    store.set("server-1", "")

    assert store.get("server-1") == "saved-password"
    assert store.has("server-1") is True


@pytest.mark.skipif(os.name != "nt", reason="DPAPI 凭据存储仅在 Windows 上可用")
def test_credential_store_keeps_other_servers_when_saving(tmp_path: Path) -> None:
    store = CredentialStore(tmp_path / "ssh-credentials.json")
    store.set("server-1", "first-password")

    store.set("server-2", "second-password")

    assert store.get("server-1") == "first-password"
    assert store.get("server-2") == "second-password"


@pytest.mark.skipif(os.name != "nt", reason="DPAPI 凭据存储仅在 Windows 上可用")
def test_corrupt_credential_file_is_quarantined_instead_of_overwritten(
    tmp_path: Path,
) -> None:
    path = tmp_path / "ssh-credentials.json"
    path.write_text("{ 这不是合法 JSON", encoding="utf-8")
    store = CredentialStore(path)

    with pytest.raises(CredentialStoreError):
        store.set("server-1", "new-password")

    assert (tmp_path / "ssh-credentials.json.corrupt").exists()
