from __future__ import annotations

from network_manager.importers import prepare_imported_nodes
from network_manager.models import (
    NODE_DIALER_PROXY_KEY,
    ImportedNode,
    RoutingRule,
    SshServerProfile,
    SubscriptionSource,
    default_config,
)
from network_manager.portable_config import export_portable_config, import_portable_config


def _node() -> ImportedNode:
    return ImportedNode(
        node_id="node-1",
        source="我的订阅",
        source_id="sub-1",
        config={
            "name": "Tokyo",
            "type": "vless",
            "server": "example.com",
            "port": 443,
            "uuid": "12345678-1234-1234-1234-123456789abc",
            "tls": True,
        },
    )


def test_portable_export_excludes_ssh_and_windows_only_settings() -> None:
    config = default_config()
    config.imported_nodes = [_node()]
    config.selected_node = "Tokyo"
    config.subscriptions = [SubscriptionSource("sub-1", "我的订阅", "https://example.com/sub")]
    config.ssh_servers = [SshServerProfile("server-1", "服务器", "192.0.2.1")]
    config.selected_ssh_server = "server-1"

    payload = export_portable_config(config)

    text = str(payload).lower()
    assert payload["format"] == "network-manager-config"
    assert payload["selectedNodeId"] == "node-1"
    assert "ssh" not in text
    assert "controller_secret" not in text
    assert "mixed_port" not in text


def test_portable_import_preserves_ssh_and_process_rules() -> None:
    current = default_config()
    current.ssh_servers = [SshServerProfile("server-1", "服务器", "192.0.2.1")]
    current.selected_ssh_server = "server-1"
    current.rules.append(RoutingRule("DOMAIN-SUFFIX", "example.net", "DIRECT", note="Custom"))
    source = default_config()
    source.imported_nodes = [_node()]
    source.selected_node = "Tokyo"
    source.mode = "GLOBAL_BUILTIN"
    source.default_target = "BUILTIN"
    source.rules.append(RoutingRule("DOMAIN-SUFFIX", "example.org", "BUILTIN", note="Portable"))

    imported = import_portable_config(current, export_portable_config(source))

    assert imported.mode == "GLOBAL_BUILTIN"
    assert imported.default_target == "BUILTIN"
    assert imported.selected_node == "Tokyo"
    assert imported.ssh_servers == current.ssh_servers
    assert imported.selected_ssh_server == "server-1"
    assert any(rule.rule_type == "PROCESS-NAME" for rule in imported.rules)
    assert any(rule.value == "example.org" and rule.target == "BUILTIN" for rule in imported.rules)
    assert all(rule.value != "example.net" for rule in imported.rules)


def test_smart_mode_round_trips_between_devices() -> None:
    source = default_config()
    source.imported_nodes = prepare_imported_nodes(
        [{"name": "One", "type": "socks5", "server": "127.0.0.1", "port": 1001}],
        "test",
    )
    source.mode = "SMART"

    payload = export_portable_config(source)
    imported = import_portable_config(default_config(), payload)

    assert payload["routing"]["mode"] == "smart"
    assert imported.mode == "SMART"


def test_custom_node_groups_round_trip_between_devices() -> None:
    source = default_config()
    node = _node()
    node.group = "巴西住宅代理"
    source.imported_nodes = [node]
    source.node_groups = ["巴西住宅代理", "备用节点"]
    source.subscriptions = [
        SubscriptionSource(
            "sub-1",
            "我的订阅",
            "https://example.com/sub",
            group="巴西住宅代理",
        )
    ]

    payload = export_portable_config(source)
    imported = import_portable_config(default_config(), payload)

    assert payload["nodeGroups"] == ["巴西住宅代理", "备用节点"]
    assert payload["nodes"][0]["group"] == "巴西住宅代理"
    assert payload["subscriptions"][0]["group"] == "巴西住宅代理"
    assert imported.node_groups == ["巴西住宅代理", "备用节点"]
    assert imported.imported_nodes[0].group == "巴西住宅代理"
    assert imported.subscriptions[0].group == "巴西住宅代理"


def test_node_dialer_round_trips_between_devices() -> None:
    source = default_config()
    target = _node()
    relay = ImportedNode(
        "node-2",
        "Server deployment",
        {"name": "Relay", "type": "socks5", "server": "127.0.0.1", "port": 1080},
    )
    target.config[NODE_DIALER_PROXY_KEY] = relay.name
    source.imported_nodes = [target, relay]

    imported = import_portable_config(default_config(), export_portable_config(source))

    assert imported.imported_nodes[0].dialer_proxy == "Relay"


def test_android_shaped_payload_imports_on_windows() -> None:
    payload = {
        "format": "network-manager-config",
        "version": 1,
        "routing": {
            "mode": "rule",
            "fallback": "direct",
            "commonOverseas": {"enabled": True, "target": "proxy"},
        },
        "selectedNodeId": "android-node",
        "nodes": [
            {
                "id": "android-node",
                "sourceId": "sub-a",
                "sourceName": "Android",
                "config": {
                    "name": "Singapore",
                    "type": "hysteria2",
                    "server": "sg.example.com",
                    "port": 8443,
                    "password": "secret",
                },
            }
        ],
        "subscriptions": [],
        "rules": [],
    }

    imported = import_portable_config(default_config(), payload)

    assert imported.selected_node == "Singapore"
    assert imported.imported_nodes[0].protocol == "hysteria2"
    assert all(
        rule.target == "BUILTIN"
        for rule in imported.rules
        if rule.rule_type == "DOMAIN-SUFFIX"
    )
