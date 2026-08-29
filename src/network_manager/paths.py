from __future__ import annotations

import os
import sys
from pathlib import Path


APP_NAME = "NetWorkManger"


def project_root() -> Path:
    return Path(__file__).resolve().parents[2]


def bundle_root() -> Path:
    frozen_root = getattr(sys, "_MEIPASS", None)
    return Path(frozen_root) if frozen_root else project_root()


def core_path() -> Path:
    return bundle_root() / "vendor" / "mihomo.exe"


def app_data_dir() -> Path:
    local_app_data = os.environ.get("LOCALAPPDATA")
    base = Path(local_app_data) if local_app_data else Path.home() / "AppData" / "Local"
    path = base / APP_NAME
    path.mkdir(parents=True, exist_ok=True)
    return path


def settings_path() -> Path:
    return app_data_dir() / "settings.json"


def generated_config_path() -> Path:
    return app_data_dir() / "mihomo-config.yaml"


def logs_dir() -> Path:
    path = app_data_dir() / "logs"
    path.mkdir(parents=True, exist_ok=True)
    return path


def ssh_credentials_path() -> Path:
    return app_data_dir() / "ssh-credentials.json"


def ssh_known_hosts_path() -> Path:
    return app_data_dir() / "ssh-known-hosts"
