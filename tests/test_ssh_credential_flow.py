import json
from threading import Lock
from types import SimpleNamespace

from network_manager.models import ImportedNode, SshServerProfile, default_config
from network_manager.server_deployer import deployment_source_id
from network_manager.ui.web_window import WebBridge


class FakeCredentialStore:
    def __init__(self) -> None:
        self.saved: list[tuple[str, str]] = []

    def set(self, profile_id: str, credential: str) -> None:
        self.saved.append((profile_id, credential))

    def delete(self, _profile_id: str) -> None:
        return


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
        bridge, profile.profile_id, True, "deployed", deployment_payload(profile), "secret"
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


def test_editing_deployed_endpoint_removes_old_node_and_reloads_core() -> None:
    profile = SshServerProfile(
        "server-1",
        "Test server",
        "192.0.2.1",
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
            "host": "192.0.2.2",
            "port": 22,
            "username": "root",
            "authMethod": "password",
            "rememberPassword": False,
            "proxyPort": 24444,
        }
    )

    WebBridge.saveSshServer(bridge, payload, "")

    updated = bridge.window.config.ssh_servers[0]
    assert updated.host == "192.0.2.2"
    assert updated.proxy_port == 24444
    assert updated.deployed_node_id == ""
    assert bridge.window.config.imported_nodes == []
    assert bridge.window.config.selected_node == ""
    assert bridge.window.applied == 1
    assert bridge.notifications == [("success", "服务器登录配置已保存")]


def test_fallback_rule_target_is_persisted() -> None:
    profile = SshServerProfile("server-1", "Test server", "192.0.2.1")
    bridge = FakeBridge(profile)
    bridge.window._refresh_rules_table = lambda: None

    WebBridge.setDefaultTarget(bridge, "V2RAY")

    assert bridge.window.config.default_target == "V2RAY"
    assert bridge.window.applied == 1
    assert bridge.notifications == [("success", "强制保底规则已更新")]
