from __future__ import annotations

import subprocess

from network_manager.importers import prepare_imported_nodes
from network_manager.mihomo_config import build_mihomo_config, write_mihomo_config
from network_manager.models import (
    NODE_DIALER_POLICY_KEY,
    NODE_DIALER_PROXY_KEY,
    ImportedNode,
    RoutingRule,
    SshServerProfile,
    apply_automatic_node_dialers,
    default_config,
)
from network_manager.paths import core_path


def test_process_exclusions_precede_user_rules() -> None:
    config = default_config()
    generated = build_mihomo_config(config)
    rules = generated["rules"]
    xray_index = rules.index("PROCESS-NAME,xray.exe,DIRECT")
    discord_index = rules.index("PROCESS-NAME,Discord.exe,UPSTREAM-CLASH")
    assert "PROCESS-NAME,sshd,DIRECT" in rules
    assert "DST-PORT,22,DIRECT" in rules
    assert "PROCESS-NAME,python.exe,DIRECT" not in rules
    assert "PROCESS-NAME,node.exe,DIRECT" not in rules
    assert xray_index < discord_index
    assert rules[-1] == "MATCH,DIRECT"


def test_lan_traffic_always_bypasses_proxy_and_fallback() -> None:
    config = default_config()
    config.mode = "GLOBAL_CLASH"
    config.default_target = "CLASH"

    generated = build_mihomo_config(config)
    rules = generated["rules"]

    assert "DOMAIN-SUFFIX,local,DIRECT" in rules
    assert "DOMAIN-SUFFIX,home.arpa,DIRECT" in rules
    assert "IP-CIDR,10.0.0.0/8,DIRECT,no-resolve" in rules
    assert "IP-CIDR,192.168.0.0/16,DIRECT,no-resolve" in rules
    assert "IP-CIDR6,fc00::/7,DIRECT,no-resolve" in rules
    assert rules.index("IP-CIDR,192.168.0.0/16,DIRECT,no-resolve") < rules.index(
        "MATCH,UPSTREAM-CLASH"
    )
    assert "192.168.0.0/16" in generated["tun"]["route-exclude-address"]


def test_imported_node_group_puts_selected_node_first() -> None:
    config = default_config()
    config.imported_nodes = prepare_imported_nodes(
        [
            {"name": "One", "type": "socks5", "server": "127.0.0.1", "port": 1001},
            {"name": "Two", "type": "socks5", "server": "127.0.0.1", "port": 1002},
        ],
        "test",
    )
    config.selected_node = "Two"
    config.rules.append(RoutingRule("DOMAIN-SUFFIX", "example.com", "BUILTIN"))
    generated = build_mihomo_config(config)
    assert generated["proxy-groups"][0]["proxies"] == ["Two", "One"]
    assert "DOMAIN-SUFFIX,example.com,IMPORTED-NODES" in generated["rules"]
    smart = next(group for group in generated["proxy-groups"] if group["name"] == "SMART-NODES")
    assert smart["lazy"] is True


def test_imported_node_can_use_another_node_as_dialer_proxy() -> None:
    config = default_config()
    config.imported_nodes = prepare_imported_nodes(
        [
            {
                "name": "Stable relay",
                "type": "ss",
                "server": "198.51.100.20",
                "port": 24443,
                "cipher": "aes-128-gcm",
                "password": "relay-secret",
            },
            {
                "name": "Residential HTTP",
                "type": "http",
                "server": "proxy.example.com",
                "port": 4600,
                "username": "user",
                "password": "proxy-secret",
                NODE_DIALER_PROXY_KEY: "Stable relay",
            },
        ],
        "test",
    )

    generated = build_mihomo_config(config)
    target = next(
        proxy for proxy in generated["proxies"] if proxy["name"] == "Residential HTTP"
    )

    assert target["dialer-proxy"] == "Stable relay"
    assert NODE_DIALER_PROXY_KEY not in target


def test_authenticated_proxy_uses_deployed_relay_for_core_and_explicit_code() -> None:
    config = default_config()
    config.imported_nodes = [
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
    config.rules.append(RoutingRule("DOMAIN-SUFFIX", "target.example", "BUILTIN"))

    assert apply_automatic_node_dialers(config.imported_nodes) is True
    generated = build_mihomo_config(config)
    target = next(proxy for proxy in generated["proxies"] if proxy["name"] == "Brazil HTTP")

    assert target["dialer-proxy"] == "海外服务器"
    assert NODE_DIALER_POLICY_KEY not in target
    endpoint_rule = "DOMAIN,proxy.example.com,海外服务器"
    destination_rule = "DOMAIN-SUFFIX,target.example,IMPORTED-NODES"
    assert endpoint_rule in generated["rules"]
    assert generated["rules"].index(endpoint_rule) < generated["rules"].index(
        destination_rule
    )


def test_chained_ip_proxy_is_routed_to_relay_instead_of_excluded_from_tun() -> None:
    config = default_config()
    config.imported_nodes = prepare_imported_nodes(
        [
            {
                "name": "Relay",
                "type": "ss",
                "server": "198.51.100.20",
                "port": 24443,
                "cipher": "aes-128-gcm",
                "password": "relay-secret",
            },
            {
                "name": "IP target",
                "type": "http",
                "server": "203.0.113.9",
                "port": 4600,
                "username": "user",
                "password": "proxy-secret",
                NODE_DIALER_PROXY_KEY: "Relay",
            },
        ],
        "test",
    )

    generated = build_mihomo_config(config)

    assert "203.0.113.9/32" not in generated["tun"]["route-exclude-address"]
    assert "198.51.100.20/32" in generated["tun"]["route-exclude-address"]
    assert "IP-CIDR,203.0.113.9/32,Relay,no-resolve" in generated["rules"]


def test_ip_proxy_servers_are_excluded_from_the_tun_route() -> None:
    config = default_config()
    config.imported_nodes = prepare_imported_nodes(
        [
            {
                "name": "IPv4 proxy",
                "type": "socks5",
                "server": "198.51.100.10",
                "port": 1001,
            },
            {
                "name": "IPv6 proxy",
                "type": "socks5",
                "server": "2001:db8::10",
                "port": 1002,
            },
            {
                "name": "Domain proxy",
                "type": "socks5",
                "server": "proxy.example.com",
                "port": 1003,
            },
        ],
        "test",
    )

    exclusions = build_mihomo_config(config)["tun"]["route-exclude-address"]

    assert "198.51.100.10/32" in exclusions
    assert "2001:db8::10/128" in exclusions
    assert all("proxy.example.com" not in item for item in exclusions)
    assert len(exclusions) == len(set(exclusions))


def test_smart_mode_uses_health_checked_low_latency_group() -> None:
    config = default_config()
    config.imported_nodes = prepare_imported_nodes(
        [
            {"name": "One", "type": "socks5", "server": "127.0.0.1", "port": 1001},
            {"name": "Two", "type": "socks5", "server": "127.0.0.1", "port": 1002},
        ],
        "test",
    )
    config.mode = "SMART"

    generated = build_mihomo_config(config)
    smart = next(group for group in generated["proxy-groups"] if group["name"] == "SMART-NODES")

    assert generated["rules"][-1] == "MATCH,SMART-NODES"
    assert smart["type"] == "url-test"
    assert smart["interval"] == 60
    assert smart["tolerance"] == 120
    assert smart["lazy"] is False
    assert smart["timeout"] == 6000
    assert smart["max-failed-times"] == 2
    assert smart["expected-status"] == 204


def test_connection_stability_options_are_enabled() -> None:
    generated = build_mihomo_config(default_config())

    assert generated["tcp-concurrent"] is True
    assert generated["keep-alive-interval"] == 15
    assert generated["keep-alive-idle"] == 15
    assert generated["disable-keep-alive"] is False


def test_generated_default_config_passes_mihomo_validation(tmp_path) -> None:
    executable = core_path()
    assert executable.exists(), "vendor/mihomo.exe is required for this test"
    config_path = write_mihomo_config(default_config(), tmp_path / "config.yaml")
    result = subprocess.run(
        [str(executable), "-t", "-d", str(tmp_path), "-f", str(config_path)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=20,
    )
    assert result.returncode == 0, result.stdout + result.stderr


def test_generated_smart_config_passes_mihomo_validation(tmp_path) -> None:
    executable = core_path()
    assert executable.exists(), "vendor/mihomo.exe is required for this test"
    config = default_config()
    config.imported_nodes = prepare_imported_nodes(
        [
            {"name": "One", "type": "socks5", "server": "127.0.0.1", "port": 1001},
            {"name": "Two", "type": "socks5", "server": "127.0.0.1", "port": 1002},
        ],
        "test",
    )
    config.mode = "SMART"
    config_path = write_mihomo_config(config, tmp_path / "smart.yaml")

    result = subprocess.run(
        [str(executable), "-t", "-d", str(tmp_path), "-f", str(config_path)],
        capture_output=True,
        text=True,
        encoding="utf-8",
        errors="replace",
        timeout=20,
    )

    assert result.returncode == 0, result.stdout + result.stderr


def test_ssh_server_is_available_as_global_target() -> None:
    config = default_config()
    profile = SshServerProfile("ssh-one", "Server", "203.0.113.10", local_port=10888)
    config.ssh_servers = [profile]
    config.selected_ssh_server = profile.profile_id
    config.mode = "GLOBAL_SSH"

    generated = build_mihomo_config(config)

    assert any(proxy["name"] == "UPSTREAM-SSH" for proxy in generated["proxies"])
    assert generated["rules"][-1] == "MATCH,UPSTREAM-SSH"
