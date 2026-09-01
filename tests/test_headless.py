from __future__ import annotations

import json
from pathlib import Path

import requests

from network_manager.headless import HEADLESS_METHODS, HeadlessController, _sanitize_headless_config
from network_manager.local_web_server import LocalWebServer
from network_manager.models import ImportedNode, RoutingRule, default_config


class _DirectBridge:
    def __init__(self) -> None:
        self.value = "initial"

    def getState(self) -> str:
        return json.dumps({"value": self.value})

    def setValue(self, value: str) -> None:
        self.value = value


def _web_root(tmp_path: Path) -> Path:
    root = tmp_path / "web"
    root.mkdir()
    (root / "index.html").write_text(
        '<meta name="network-session-token" content="__SESSION_TOKEN__">',
        encoding="utf-8",
    )
    return root


def test_direct_web_server_requires_basic_auth(tmp_path) -> None:
    bridge = _DirectBridge()
    server = LocalWebServer(
        _web_root(tmp_path),
        bridge,
        {"getState", "setValue"},
        direct_dispatch=True,
        access_username="operator",
        access_password="strong-password",
    )
    server.start()
    auth = ("operator", "strong-password")
    headers = {"X-Network-Session": server.token}
    try:
        assert requests.get(server.url, timeout=3).status_code == 401
        page = requests.get(server.url, auth=auth, timeout=3)
        assert page.status_code == 200
        assert server.token in page.text

        state = requests.get(server.url + "api/state", auth=auth, headers=headers, timeout=3)
        assert state.json() == {"value": "initial"}
        changed = requests.post(
            server.url + "api/call",
            auth=auth,
            headers=headers,
            json={"method": "setValue", "args": ["updated"]},
            timeout=3,
        )
        assert changed.json()["ok"] is True
        assert bridge.value == "updated"
    finally:
        server.close()


def test_remote_listener_rejects_missing_password(tmp_path) -> None:
    try:
        LocalWebServer(
            _web_root(tmp_path),
            _DirectBridge(),
            {"getState"},
            direct_dispatch=True,
            host="0.0.0.0",
        )
    except ValueError as exc:
        assert "必须设置" in str(exc)
    else:  # pragma: no cover - security regression guard
        raise AssertionError("remote WebGUI must not allow an empty password")


def test_headless_controller_exposes_linux_capabilities(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))
    controller = HeadlessController(start_core=False)
    try:
        state = json.loads(controller.getState())
        assert state["capabilities"] == {
            "platform": "linux",
            "headless": True,
            "sshDeployment": False,
            "browserFiles": True,
        }
        assert state["core"]["running"] is False
        assert state["sshServers"] == []

        exported = json.loads(controller.exportPortableConfigText())
        assert exported["format"] == "network-manager-config"
    finally:
        controller.close()


def test_headless_method_allowlist_matches_controller() -> None:
    assert not (HEADLESS_METHODS - set(dir(HeadlessController)))


def test_headless_rule_editor_saves_multiple_values_atomically(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))
    controller = HeadlessController(start_core=False)
    try:
        controller.config.rules = []
        controller.saveRule(
            json.dumps(
                {
                    "index": -1,
                    "ruleType": "DOMAIN-SUFFIX",
                    "values": ["Example.com", "example.org", "example.com"],
                    "target": "DIRECT",
                    "enabled": True,
                    "note": "batch",
                }
            )
        )
        assert [rule.value for rule in controller.config.rules] == [
            "example.com",
            "example.org",
        ]

        previous_rules = list(controller.config.rules)
        controller.saveRule(
            json.dumps(
                {
                    "index": 0,
                    "ruleType": "DOMAIN-SUFFIX",
                    "values": ["valid.example", "invalid domain"],
                    "target": "DIRECT",
                }
            )
        )
        assert controller.config.rules == previous_rules
    finally:
        controller.close()


def test_headless_state_detects_running_process_names(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))

    class FakeProcess:
        def __init__(self, name: str) -> None:
            self.info = {"name": name}

    monkeypatch.setattr(
        "network_manager.headless.psutil.process_iter",
        lambda _attrs: [
            FakeProcess("zeta.exe"),
            FakeProcess("Discord.exe"),
            FakeProcess("discord.exe"),
        ],
    )
    controller = HeadlessController(start_core=False)
    try:
        state = json.loads(controller.getState())
        assert state["runningProcesses"] == ["Discord.exe", "zeta.exe"]
    finally:
        controller.close()


def test_headless_node_groups_fall_back_to_source(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))
    controller = HeadlessController(start_core=False)
    try:
        controller.config.imported_nodes = [
            ImportedNode(
                "node-id",
                "我的订阅",
                {"name": "Tokyo", "type": "socks5", "server": "127.0.0.1", "port": 1080},
            )
        ]
        controller.store.save(controller.config)

        state = json.loads(controller.getState())
        assert state["nodeGroups"] == []
        assert state["nodes"][0]["group"] == "我的订阅"
        assert state["nodes"][0]["customGroup"] == ""

        controller.createNodeGroup("  亚洲   备用  ")
        controller.assignNodeGroup("Tokyo", "亚洲 备用")
        state = json.loads(controller.getState())
        assert state["nodeGroups"] == ["亚洲 备用"]
        assert state["nodes"][0]["group"] == "亚洲 备用"
        assert state["nodes"][0]["customGroup"] == "亚洲 备用"

        controller.deleteNodeGroup("亚洲 备用")
        state = json.loads(controller.getState())
        assert state["nodeGroups"] == []
        assert state["nodes"][0]["group"] == "我的订阅"
        assert state["nodes"][0]["customGroup"] == ""
    finally:
        controller.close()


def test_headless_server_deployments_share_one_source_group(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))
    controller = HeadlessController(start_core=False)
    try:
        controller.config.imported_nodes = [
            ImportedNode(
                "server-a",
                "服务器部署 · 新加坡服务器",
                {"name": "Singapore", "type": "ss", "server": "192.0.2.1", "port": 443},
                source_id="server-deployment:server-a",
            ),
            ImportedNode(
                "server-b",
                "服务器部署 · 日本服务器",
                {"name": "Tokyo", "type": "ss", "server": "192.0.2.2", "port": 443},
                source_id="server-deployment:server-b",
            ),
        ]

        state = json.loads(controller.getState())

        assert {node["group"] for node in state["nodes"]} == {"服务器部署"}
    finally:
        controller.close()


def test_headless_node_dialer_is_saved_and_exposed(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))
    controller = HeadlessController(start_core=False)
    try:
        controller.config.imported_nodes = [
            ImportedNode(
                "target",
                "manual",
                {"name": "HTTP target", "type": "http", "server": "proxy.example.com", "port": 4600},
            ),
            ImportedNode(
                "relay",
                "server",
                {"name": "Stable relay", "type": "socks5", "server": "127.0.0.1", "port": 1080},
            ),
        ]

        controller.setNodeDialerProxy("HTTP target", "Stable relay")
        state = json.loads(controller.getState())

        assert state["nodes"][0]["dialerProxy"] == "Stable relay"
        assert state["nodes"][0]["dialerPolicy"] == "manual"
        generated = (tmp_path / "data" / "mihomo-config.yaml").read_text(encoding="utf-8")
        assert "dialer-proxy: Stable relay" in generated
    finally:
        controller.close()


def test_headless_auto_relay_rule_is_visible_and_applies_to_code(monkeypatch, tmp_path) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))
    controller = HeadlessController(start_core=False)
    try:
        controller.config.imported_nodes = [
            ImportedNode(
                "relay",
                "服务器部署 · 海外服务器",
                {
                    "name": "海外服务器",
                    "type": "ss",
                    "server": "198.51.100.20",
                    "port": 24443,
                    "cipher": "aes-128-gcm",
                    "password": "relay-secret",
                },
                source_id="server-deployment:relay",
            ),
            ImportedNode(
                "target",
                "手动导入",
                {
                    "name": "Brazil HTTP",
                    "type": "http",
                    "server": "proxy.example.com",
                    "port": 4600,
                    "username": "user",
                    "password": "proxy-secret",
                },
            ),
        ]

        assert controller._save_and_apply("saved") is True
        state = json.loads(controller.getState())

        assert state["nodes"][1]["dialerProxy"] == "海外服务器"
        assert state["nodes"][1]["dialerPolicy"] == "auto"
        relay_rule = next(rule for rule in state["rules"] if rule["kind"] == "relay")
        assert relay_rule["targetLabel"] == "经 海外服务器"
        assert "代码直接使用代理域名" in relay_rule["note"]
    finally:
        controller.close()


def test_headless_proxy_relay_rule_can_switch_between_direct_manual_and_auto(
    monkeypatch, tmp_path
) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))
    controller = HeadlessController(start_core=False)
    try:
        controller.config.imported_nodes = [
            ImportedNode(
                "relay",
                "服务器部署 · 海外服务器",
                {
                    "name": "海外服务器",
                    "type": "ss",
                    "server": "198.51.100.20",
                    "port": 24443,
                    "cipher": "aes-128-gcm",
                    "password": "relay-secret",
                },
                source_id="server-deployment:relay",
            ),
            ImportedNode(
                "target",
                "手动导入",
                {
                    "name": "Brazil HTTP",
                    "type": "http",
                    "server": "proxy.example.com",
                    "port": 4600,
                    "username": "user",
                    "password": "proxy-secret",
                },
            ),
        ]
        controller._save_and_apply("saved")

        controller.saveProxyRelayRules(
            json.dumps(
                {
                    "assignments": [
                        {"node": "Brazil HTTP", "mode": "direct", "dialer": ""}
                    ]
                }
            )
        )
        state = json.loads(controller.getState())
        relay_rule = next(rule for rule in state["rules"] if rule["kind"] == "relay")
        assert relay_rule["targetLabel"] == "直连"
        assert relay_rule["entries"][0]["policy"] == "direct"

        controller.saveProxyRelayRules(
            json.dumps(
                {
                    "assignments": [
                        {
                            "node": "Brazil HTTP",
                            "server": "new-proxy.example.com",
                            "port": 4700,
                            "mode": "manual",
                            "dialer": "海外服务器",
                        }
                    ]
                }
            )
        )
        state = json.loads(controller.getState())
        assert state["nodes"][1]["dialerProxy"] == "海外服务器"
        assert state["nodes"][1]["dialerPolicy"] == "manual"
        assert state["nodes"][1]["server"] == "new-proxy.example.com:4700"
        relay_rule = next(rule for rule in state["rules"] if rule["kind"] == "relay")
        assert relay_rule["entries"][0]["endpoint"] == "new-proxy.example.com"
        assert relay_rule["entries"][0]["port"] == 4700

        controller.saveProxyRelayRules(
            json.dumps(
                {
                    "assignments": [
                        {"node": "Brazil HTTP", "mode": "auto", "dialer": ""}
                    ]
                }
            )
        )
        state = json.loads(controller.getState())
        assert state["nodes"][1]["dialerPolicy"] == "auto"
    finally:
        controller.close()


def test_headless_proxy_relay_rule_rejects_invalid_node_without_partial_save(
    monkeypatch, tmp_path
) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))
    controller = HeadlessController(start_core=False)
    try:
        controller.config.imported_nodes = [
            ImportedNode(
                "target",
                "手动导入",
                {
                    "name": "HTTP target",
                    "type": "http",
                    "server": "proxy.example.com",
                    "port": 4600,
                },
            )
        ]
        original = dict(controller.config.imported_nodes[0].config)

        controller.saveProxyRelayRules(
            json.dumps(
                {
                    "assignments": [
                        {
                            "node": "HTTP target",
                            "mode": "manual",
                            "dialer": "missing",
                        }
                    ]
                }
            )
        )

        assert controller.config.imported_nodes[0].config == original
    finally:
        controller.close()


def test_headless_proxy_relay_rule_rejects_invalid_endpoint_without_partial_save(
    monkeypatch, tmp_path
) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))
    controller = HeadlessController(start_core=False)
    try:
        controller.config.imported_nodes = [
            ImportedNode(
                "target",
                "手动导入",
                {
                    "name": "HTTP target",
                    "type": "http",
                    "server": "proxy.example.com",
                    "port": 4600,
                },
            )
        ]
        original = dict(controller.config.imported_nodes[0].config)

        controller.saveProxyRelayRules(
            json.dumps(
                {
                    "assignments": [
                        {
                            "node": "HTTP target",
                            "server": "https://invalid.example/path",
                            "port": 70000,
                            "mode": "direct",
                            "dialer": "",
                        }
                    ]
                }
            )
        )

        assert controller.config.imported_nodes[0].config == original
    finally:
        controller.close()


def test_headless_config_removes_legacy_ssh_routes() -> None:
    config = default_config()
    config.mode = "GLOBAL_SSH"
    config.default_target = "SSH"
    config.rules.append(RoutingRule("DOMAIN-SUFFIX", "example.com", "SSH"))
    assert _sanitize_headless_config(config) is True
    assert config.mode == "RULE"
    assert config.default_target == "DIRECT"
    assert config.rules[-1].target == "DIRECT"

    config.imported_nodes.append(
        ImportedNode("node-id", "test", {"name": "node", "type": "ss"})
    )
    config.mode = "GLOBAL_SSH"
    config.default_target = "SSH"
    config.rules[-1].target = "SSH"
    assert _sanitize_headless_config(config) is True
    assert config.mode == "GLOBAL_BUILTIN"
    assert config.default_target == "BUILTIN"
    assert config.rules[-1].target == "BUILTIN"


def test_subscription_default_group_survives_refresh_and_node_restore(
    monkeypatch, tmp_path
) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_DATA_DIR", str(tmp_path / "data"))
    monkeypatch.setenv("NETWORK_MANAGER_CORE", str(tmp_path / "missing-mihomo"))
    monkeypatch.setattr("network_manager.headless.fetch_subscription", lambda *_args: "data")
    monkeypatch.setattr(
        "network_manager.headless.parse_import_content",
        lambda _content: (
            [
                {
                    "name": "Singapore",
                    "type": "socks5",
                    "server": "127.0.0.1",
                    "port": 1080,
                }
            ],
            [],
        ),
    )
    controller = HeadlessController(start_core=False)
    try:
        controller.createNodeGroup("亚洲节点")
        controller._download_subscription(
            "我的订阅", "https://example.com/sub", None, "亚洲节点"
        )
        source = controller.config.subscriptions[0]
        assert source.group == "亚洲节点"
        assert controller.config.imported_nodes[0].group == "亚洲节点"

        controller.config.imported_nodes.clear()
        controller._download_subscription(source.name, source.url, source)
        assert controller.config.imported_nodes[0].group == "亚洲节点"

        controller.deleteNodeGroup("亚洲节点")
        assert controller.config.subscriptions[0].group == ""
        assert controller.config.imported_nodes[0].group == ""
    finally:
        controller.close()
