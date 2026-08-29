from __future__ import annotations

import os
import sqlite3
from dataclasses import dataclass
from pathlib import Path

import psutil
import yaml

from network_manager.models import SubscriptionSource


@dataclass(frozen=True, slots=True)
class DiscoveredConfig:
    product: str
    path: Path
    kind: str


def _has_clash_nodes(path: Path) -> bool:
    try:
        data = yaml.safe_load(path.read_text(encoding="utf-8-sig"))
    except (OSError, UnicodeDecodeError, yaml.YAMLError):
        return False
    return isinstance(data, dict) and isinstance(data.get("proxies"), list) and bool(
        data["proxies"]
    )


def _known_clash_roots() -> list[Path]:
    appdata = Path(os.environ.get("APPDATA", ""))
    userprofile = Path(os.environ.get("USERPROFILE", ""))
    return [
        appdata / "io.github.clash-verge-rev.clash-verge-rev",
        appdata / "clash-verge",
        appdata / "Clash Verge",
        userprofile / ".config" / "clash",
        userprofile / ".config" / "mihomo",
    ]


def _v2ray_roots_from_processes() -> list[Path]:
    roots: list[Path] = []
    for process in psutil.process_iter(["name", "exe"]):
        try:
            name = (process.info.get("name") or "").lower()
            executable = process.info.get("exe")
        except (psutil.AccessDenied, psutil.NoSuchProcess):
            continue
        if not executable:
            continue
        path = Path(executable)
        if name == "v2rayn.exe":
            roots.append(path.parent)
        elif name in {"xray.exe", "v2ray.exe"} and len(path.parents) >= 3:
            roots.append(path.parents[2])
    return roots


def discover_configs() -> list[DiscoveredConfig]:
    found: dict[Path, DiscoveredConfig] = {}
    for root in _known_clash_roots():
        if not root.is_dir():
            continue
        candidates = [root / "clash-verge.yaml", root / "config.yaml"]
        profiles = root / "profiles"
        if profiles.is_dir():
            candidates.extend(profiles.glob("*.yaml"))
            candidates.extend(profiles.glob("*.yml"))
        for path in candidates:
            if (
                path.is_file()
                and 32 <= path.stat().st_size <= 10 * 1024 * 1024
                and _has_clash_nodes(path)
            ):
                found[path.resolve()] = DiscoveredConfig("Clash", path.resolve(), "yaml")

    for root in _v2ray_roots_from_processes():
        database = root / "guiConfigs" / "guiNDB.db"
        if database.is_file():
            found[database.resolve()] = DiscoveredConfig("v2rayN", database.resolve(), "database")
    return sorted(found.values(), key=lambda item: (item.product, str(item.path).lower()))


def subscriptions_from_v2rayn_db(path: Path) -> list[SubscriptionSource]:
    uri = f"file:{path.as_posix()}?mode=ro"
    subscriptions: list[SubscriptionSource] = []
    with sqlite3.connect(uri, uri=True) as database:
        rows = database.execute(
            'SELECT Id, Remarks, Url, UpdateTime FROM SubItem WHERE Enabled = 1 AND Url != ""'
        ).fetchall()
    for source_id, remarks, url, updated in rows:
        subscriptions.append(
            SubscriptionSource(
                source_id=str(source_id),
                name=str(remarks or "v2rayN 订阅"),
                url=str(url),
                last_updated=str(updated or ""),
            )
        )
    return subscriptions
