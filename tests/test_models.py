from __future__ import annotations

from network_manager.models import (
    CONFIG_VERSION,
    RoutingRule,
    default_config,
    migrate_config,
    normalize_rule_value,
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


def test_invalid_rule_is_reported() -> None:
    errors = validate_rule(RoutingRule("DOMAIN", "not a domain", "CLASH"))
    assert errors


def test_builtin_target_requires_imported_nodes() -> None:
    config = default_config()
    config.rules.append(RoutingRule("DOMAIN-SUFFIX", "example.com", "BUILTIN"))
    assert any("BUILTIN" in error for error in validate_config(config))
