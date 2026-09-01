from __future__ import annotations

from network_manager.config_store import ConfigStore
from network_manager.models import CONFIG_VERSION, default_config


def test_config_round_trip(tmp_path) -> None:
    path = tmp_path / "settings.json"
    store = ConfigStore(path)
    config = default_config()
    config.mode = "GLOBAL_CLASH"
    store.save(config)
    loaded = store.load()
    assert loaded.mode == "GLOBAL_CLASH"
    assert loaded.rules[0].value == "Discord.exe"
    assert loaded.server_proxy_port == config.server_proxy_port


def test_invalid_config_is_backed_up(tmp_path) -> None:
    path = tmp_path / "settings.json"
    path.write_text("not-json", encoding="utf-8")
    loaded = ConfigStore(path).load()
    assert loaded.mode == "RULE"
    assert path.with_suffix(".invalid.json").exists()


def test_old_config_is_migrated_and_saved(tmp_path) -> None:
    path = tmp_path / "settings.json"
    path.write_text(
        '{"version": 1, "rules": [{"rule_type": "DOMAIN-SUFFIX", '
        '"value": "google.com", "target": "DIRECT"}]}',
        encoding="utf-8",
    )

    loaded = ConfigStore(path).load()

    assert loaded.version == CONFIG_VERSION
    assert sum(rule.value == "google.com" for rule in loaded.rules) == 1
    assert any(rule.value == "claude.ai" for rule in loaded.rules)
    assert 10000 <= loaded.server_proxy_port <= 65535
    assert '"server_proxy_port"' in path.read_text(encoding="utf-8")
    assert f'"version": {CONFIG_VERSION}' in path.read_text(encoding="utf-8")
