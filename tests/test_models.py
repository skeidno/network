from __future__ import annotations

from network_manager.models import (
    AppConfig,
    COMMON_OVERSEAS_GROUP,
    CONFIG_VERSION,
    MIN_RANDOM_SERVER_PROXY_PORT,
    NODE_DIALER_POLICY_KEY,
    NODE_DIALER_PROXY_KEY,
    ImportedNode,
    RoutingRule,
    SshServerProfile,
    apply_automatic_node_dialers,
    common_overseas_rules_from_values,
    default_config,
    is_common_overseas_rule,
    migrate_config,
    normalize_rule_value,
    server_proxy_port_error,
    validate_config,
    validate_rule,
)


def test_default_config_contains_discord_process_rule() -> None:
    config = default_config()
    assert config.rules[0] == RoutingRule(
        "PROCESS-NAME", "Discord.exe", "CLASH", True, "Discord 全部流量"
    )
    assert any(rule.value == "google.com" for rule in config.rules)
    assert any(rule.value == "chatgpt.com" for rule in config.rules)
    assert any(rule.value == "claude.ai" for rule in config.rules)
    assert any(rule.value == "arcteryx.com" for rule in config.rules)
    assert validate_config(config) == []


def test_default_server_deployment_port_is_random_high_port_and_round_trips() -> None:
    config = default_config()

    assert MIN_RANDOM_SERVER_PROXY_PORT <= config.server_proxy_port <= 65535
    restored = AppConfig.from_dict(config.to_dict())
    assert restored.server_proxy_port == config.server_proxy_port


def test_default_server_deployment_port_rejects_low_ports() -> None:
    config = default_config()
    config.server_proxy_port = 9999

    assert any("默认服务器部署端口" in error for error in validate_config(config))


def test_config_migration_adds_missing_defaults_without_overriding_user_rule() -> None:
    config = default_config()
    config.version = CONFIG_VERSION - 1
    config.rules = [RoutingRule("DOMAIN-SUFFIX", "google.com", "DIRECT")]

    assert migrate_config(config)
    google_rules = [rule for rule in config.rules if rule.value == "google.com"]
    assert google_rules == [RoutingRule("DOMAIN-SUFFIX", "google.com", "DIRECT")]
    assert any(rule.value == "chatgpt.com" for rule in config.rules)
    assert any(rule.value == "arcteryx.com" for rule in config.rules)
    assert not migrate_config(config)


def test_normalize_domain_and_process_values() -> None:
    assert normalize_rule_value("DOMAIN-SUFFIX", "https://*.Example.COM/path") == "example.com"
    assert normalize_rule_value("PROCESS-NAME", r"C:\Apps\Discord.exe") == "Discord.exe"


def test_custom_common_rule_domains_keep_their_group_identity() -> None:
    rules = common_overseas_rules_from_values(
        ["*.Example.com", "example.org", "EXAMPLE.COM"],
        "DIRECT",
    )

    assert [rule.value for rule in rules] == ["example.com", "example.org"]
    assert all(rule.group == COMMON_OVERSEAS_GROUP for rule in rules)
    assert all(is_common_overseas_rule(rule) for rule in rules)
    assert rules[0].note == "自定义"


def test_invalid_rule_is_reported() -> None:
    errors = validate_rule(RoutingRule("DOMAIN", "not a domain", "CLASH"))
    assert errors


def test_builtin_target_requires_imported_nodes() -> None:
    config = default_config()
    config.rules.append(RoutingRule("DOMAIN-SUFFIX", "example.com", "BUILTIN"))
    assert any("BUILTIN" in error for error in validate_config(config))


def test_server_deployment_allows_standard_tls_port() -> None:
    config = default_config()
    config.ssh_servers = [
        SshServerProfile("server-1", "Server", "198.51.100.10", proxy_port=443)
    ]

    assert validate_config(config) == []


def test_server_deployment_allows_explicit_common_port_but_rejects_conflicts() -> None:
    assert server_proxy_port_error(443, 22) == ""
    assert "SSH 端口" in server_proxy_port_error(2222, 2222)
    assert "1 到 65535" in server_proxy_port_error(0, 22)
    assert server_proxy_port_error(24443, 22) == ""


def test_server_region_round_trips_through_config() -> None:
    profile = SshServerProfile("server-1", "Tokyo", "198.51.100.10", region="日本")

    restored = SshServerProfile.from_dict(
        {
            "profile_id": profile.profile_id,
            "name": profile.name,
            "host": profile.host,
            "region": profile.region,
            "proxy_reachable": False,
            "proxy_reachability_error": "公网连接超时",
        }
    )

    assert restored.region == "日本"
    assert restored.proxy_reachable is False
    assert restored.proxy_reachability_error == "公网连接超时"


def test_server_region_does_not_change_legacy_positional_port() -> None:
    profile = SshServerProfile("server-1", "Tokyo", "198.51.100.10", 2222)

    assert profile.port == 2222
    assert profile.region == ""


def test_node_dialer_rejects_missing_self_and_cycles() -> None:
    def node(name: str, dialer: str = "") -> ImportedNode:
        config = {"name": name, "type": "socks5", "server": "127.0.0.1", "port": 1080}
        if dialer:
            config[NODE_DIALER_PROXY_KEY] = dialer
        return ImportedNode(name, "test", config)

    config = default_config()
    config.imported_nodes = [node("A", "missing")]
    assert any("missing" in error for error in validate_config(config))

    config.imported_nodes = [node("A", "A")]
    assert any("A" in error for error in validate_config(config))

    config.imported_nodes = [node("A", "B"), node("B", "A")]
    assert any("A -> B -> A" in error for error in validate_config(config))


def test_authenticated_http_proxy_automatically_uses_preferred_deployed_relay() -> None:
    nodes = [
        ImportedNode(
            "private",
            "服务器部署 · 私人服务器",
            {"name": "私人服务器", "type": "ss", "server": "192.0.2.1", "port": 24443},
            source_id="server-deployment:private",
        ),
        ImportedNode(
            "overseas",
            "服务器部署 · 海外服务器",
            {"name": "海外服务器", "type": "ss", "server": "192.0.2.2", "port": 24443},
            source_id="server-deployment:overseas",
        ),
        ImportedNode(
            "target",
            "手动导入",
            {
                "name": "Residential HTTP",
                "type": "http",
                "server": "proxy.example.com",
                "port": 4600,
                "username": "user",
                "password": "secret",
            },
        ),
    ]

    assert apply_automatic_node_dialers(nodes) is True
    assert nodes[-1].dialer_proxy == "海外服务器"
    assert nodes[-1].dialer_policy == "auto"
    assert apply_automatic_node_dialers(nodes) is False


def test_direct_or_manual_node_dialer_policy_is_not_overridden() -> None:
    relay = ImportedNode(
        "relay",
        "服务器部署",
        {"name": "Relay", "type": "ss", "server": "192.0.2.1", "port": 24443},
        source_id="server-deployment:relay",
    )
    direct = ImportedNode(
        "direct",
        "manual",
        {
            "name": "Direct HTTP",
            "type": "http",
            "server": "direct.example.com",
            "port": 4600,
            "username": "user",
            "password": "secret",
            NODE_DIALER_POLICY_KEY: "direct",
        },
    )
    manual = ImportedNode(
        "manual",
        "manual",
        {
            "name": "Manual HTTP",
            "type": "http",
            "server": "manual.example.com",
            "port": 4600,
            "username": "user",
            "password": "secret",
            NODE_DIALER_PROXY_KEY: "Relay",
            NODE_DIALER_POLICY_KEY: "manual",
        },
    )

    assert apply_automatic_node_dialers([relay, direct, manual]) is False
    assert direct.dialer_proxy == ""
    assert manual.dialer_proxy == "Relay"
