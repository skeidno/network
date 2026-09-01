from __future__ import annotations

import os
import socket

import pytest
import yaml

import network_manager.core_manager as core_manager_module
from network_manager.core_manager import CoreManager
from network_manager.mihomo_config import build_mihomo_config
from network_manager.models import default_config
from network_manager.paths import core_path


def _free_port() -> int:
    with socket.socket() as server:
        server.bind(("127.0.0.1", 0))
        return int(server.getsockname()[1])


@pytest.mark.skipif(os.name != "nt", reason="the pinned test core is the Windows build")
def test_core_manager_repeated_start_stop_without_tun(tmp_path) -> None:
    config = default_config()
    config.mixed_port = _free_port()
    config.controller_port = _free_port()
    config.dns_port = _free_port()
    generated = build_mihomo_config(config)
    generated["tun"]["enable"] = False
    path = tmp_path / "config.yaml"
    path.write_text(yaml.safe_dump(generated, allow_unicode=True), encoding="utf-8")
    manager = CoreManager(core_path(), tmp_path, config.controller_port)

    try:
        for iteration in range(5):
            manager.start(path)
            assert manager.is_running
            assert manager.is_healthy
            if iteration == 0:
                manager.reload(path, config.controller_secret)
                assert manager.is_healthy
            manager.stop()
            assert not manager.is_running
    finally:
        manager.stop()


def test_linux_ssh_ports_are_normalized(monkeypatch) -> None:
    monkeypatch.setenv("NETWORK_MANAGER_SSH_PORTS", "22, 2222 invalid 22 0 65536 2200")

    assert core_manager_module._linux_ssh_ports() == [22, 2222, 2200]


def test_linux_ssh_route_guard_is_installed_and_removed(monkeypatch) -> None:
    commands: list[list[str]] = []

    def run(command: list[str]) -> core_manager_module.subprocess.CompletedProcess[str]:
        commands.append(command)
        return core_manager_module.subprocess.CompletedProcess(command, 0, "", "")

    monkeypatch.setattr(core_manager_module.sys, "platform", "linux")
    monkeypatch.setattr(core_manager_module.shutil, "which", lambda _name: "/usr/sbin/ip")
    monkeypatch.setattr(core_manager_module, "_linux_ssh_ports", lambda: [22, 2222])
    monkeypatch.setattr(core_manager_module, "_run_ip_rule", run)

    rules = core_manager_module._install_linux_ssh_route_guard()
    core_manager_module._remove_linux_ssh_route_guard(rules)

    assert rules == [
        ("-4", 8990, 22),
        ("-4", 8991, 2222),
        ("-6", 8990, 22),
        ("-6", 8991, 2222),
    ]
    add_commands = [command for command in commands if "add" in command]
    delete_commands = [command for command in commands if "del" in command]
    assert len(add_commands) == 4
    assert len(delete_commands) == 8
    assert add_commands[0][-5:] == ["tcp", "sport", "22", "lookup", "main"]
