from __future__ import annotations

import subprocess

from network_manager.importers import prepare_imported_nodes
from network_manager.mihomo_config import build_mihomo_config, write_mihomo_config
from network_manager.models import RoutingRule, SshServerProfile, default_config
from network_manager.paths import core_path


def test_process_exclusions_precede_user_rules() -> None:
    config = default_config()
    generated = build_mihomo_config(config)
    rules = generated["rules"]
    xray_index = rules.index("PROCESS-NAME,xray.exe,DIRECT")
    discord_index = rules.index("PROCESS-NAME,Discord.exe,UPSTREAM-CLASH")
    assert xray_index < discord_index
    assert rules[-1] == "MATCH,DIRECT"


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


def test_ssh_server_is_available_as_global_target() -> None:
    config = default_config()
    profile = SshServerProfile("ssh-one", "Server", "203.0.113.10", local_port=10888)
    config.ssh_servers = [profile]
    config.selected_ssh_server = profile.profile_id
    config.mode = "GLOBAL_SSH"

    generated = build_mihomo_config(config)

    assert any(proxy["name"] == "UPSTREAM-SSH" for proxy in generated["proxies"])
    assert generated["rules"][-1] == "MATCH,UPSTREAM-SSH"
