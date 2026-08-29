from __future__ import annotations

import os
import socket

import pytest
import yaml

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
        for _iteration in range(5):
            manager.start(path)
            assert manager.is_running
            manager.stop()
            assert not manager.is_running
    finally:
        manager.stop()
