from __future__ import annotations

import json
import runpy
from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]


def test_linux_mihomo_assets_match_installer() -> None:
    manifest = json.loads(
        (ROOT / "apps/linux/mihomo.version.json").read_text(encoding="utf-8")
    )
    installer = (ROOT / "apps/linux/install.sh").read_text(encoding="utf-8")
    assert manifest["version"] == "v1.19.30"
    for asset in manifest["assets"].values():
        assert asset["name"] in installer
        assert asset["sha256"] in installer


def test_linux_installer_prefers_bundled_release_files() -> None:
    installer = (ROOT / "apps/linux/install.sh").read_text(encoding="utf-8")
    # 程序本体是 Rust 单文件二进制，不再有 Python wheel / venv / pip。
    assert 'binary_candidates=(' in installer
    assert '"${SCRIPT_ROOT}/network-manager-rs"' in installer
    assert '"${SCRIPT_ROOT}/${MIHOMO_ASSET}"' in installer
    assert 'SERVICE_SOURCE="${SCRIPT_ROOT}/network-manager.service"' in installer
    assert "venv/bin/python" not in installer


def test_linux_release_builder_is_loadable() -> None:
    namespace = runpy.run_path(str(ROOT / "scripts/build_linux_release.py"), run_name="builder")
    assert namespace["project_version"]().count(".") == 2
    assert namespace["sha256"](ROOT / "pyproject.toml")


def test_linux_service_runs_rust_binary_with_tun_capability() -> None:
    service = (ROOT / "apps/linux/network-manager.service").read_text(encoding="utf-8")
    assert "ExecStart=/opt/network-manager/bin/network-manager-rs" in service
    assert "venv" not in service
    assert "EnvironmentFile=/etc/network-manager/network-manager.env" in service
    assert "CAP_NET_ADMIN" in service
    assert "CAP_SYS_PTRACE" in service
    assert "CAP_DAC_READ_SEARCH" in service
    assert "DeviceAllow=/dev/net/tun rw" in service
