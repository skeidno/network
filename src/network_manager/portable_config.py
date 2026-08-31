from __future__ import annotations

from copy import deepcopy
from datetime import datetime, timezone
from typing import Any

from network_manager.models import (
    DEFAULT_PROXY_DOMAINS,
    AppConfig,
    ImportedNode,
    RoutingRule,
    SubscriptionSource,
    default_routing_rules,
    normalize_rule_value,
    validate_config,
    validate_rule,
)

PORTABLE_FORMAT = "network-manager-config"
PORTABLE_VERSION = 1
PORTABLE_RULE_TYPES = {
    "domain": "DOMAIN",
    "domain_suffix": "DOMAIN-SUFFIX",
    "domain_keyword": "DOMAIN-KEYWORD",
    "ip_cidr": "IP-CIDR",
}


def export_portable_config(config: AppConfig) -> dict[str, Any]:
    common_keys = {("DOMAIN-SUFFIX", domain) for domain, _label in DEFAULT_PROXY_DOMAINS}
    common_rules = [
        rule
        for rule in config.rules
        if (rule.rule_type, normalize_rule_value(rule.rule_type, rule.value)) in common_keys
    ]
    custom_rules = [
        _export_rule(rule)
        for rule in config.rules
        if rule.rule_type != "PROCESS-NAME"
        and (rule.rule_type, normalize_rule_value(rule.rule_type, rule.value)) not in common_keys
        and rule.rule_type in PORTABLE_RULE_TYPES.values()
    ]
    mode = "rule"
    if config.mode == "GLOBAL_BUILTIN":
        mode = "global"
    elif config.mode == "SMART":
        mode = "smart"
    elif config.mode == "DIRECT":
        mode = "direct"
    selected_id = next(
        (node.node_id for node in config.imported_nodes if node.name == config.selected_node),
        "",
    )
    return {
        "format": PORTABLE_FORMAT,
        "version": PORTABLE_VERSION,
        "exportedAt": datetime.now(timezone.utc).isoformat(),
        "routing": {
            "mode": mode,
            "fallback": _portable_target(config.default_target),
            "commonOverseas": {
                "enabled": bool(common_rules) and all(rule.enabled for rule in common_rules),
                "target": _portable_target(common_rules[0].target if common_rules else "BUILTIN"),
            },
        },
        "selectedNodeId": selected_id,
        "nodes": [
            {
                "id": node.node_id,
                "sourceId": node.source_id,
                "sourceName": node.source,
                "config": deepcopy(node.config),
            }
            for node in config.imported_nodes
        ],
        "subscriptions": [
            {
                "id": source.source_id,
                "name": source.name,
                "url": source.url,
                "updatedAt": source.last_updated,
            }
            for source in config.subscriptions
        ],
        "rules": custom_rules,
    }


def import_portable_config(current: AppConfig, payload: dict[str, Any]) -> AppConfig:
    _validate_root(payload)
    imported = deepcopy(current)
    nodes_data = _object_list(payload.get("nodes", []), "nodes", 5_000)
    subscriptions_data = _object_list(payload.get("subscriptions", []), "subscriptions", 500)
    rules_data = _object_list(payload.get("rules", []), "rules", 5_000)

    nodes: list[ImportedNode] = []
    used_names: set[str] = set()
    for index, item in enumerate(nodes_data, start=1):
        raw_config = item.get("config")
        if not isinstance(raw_config, dict):
            raise ValueError(f"第 {index} 个节点缺少 config")
        config = deepcopy(raw_config)
        name = str(config.get("name", "")).strip() or f"导入节点 {index}"
        base_name = name
        suffix = 2
        while name in used_names:
            name = f"{base_name} ({suffix})"
            suffix += 1
        used_names.add(name)
        config["name"] = name
        nodes.append(
            ImportedNode(
                node_id=str(item.get("id", "")).strip() or __import__("secrets").token_hex(8),
                source=str(item.get("sourceName", "跨设备导入")).strip() or "跨设备导入",
                source_id=str(item.get("sourceId", "")).strip(),
                config=config,
            )
        )
    imported.imported_nodes = nodes
    imported.subscriptions = [
        SubscriptionSource(
            source_id=str(item.get("id", "")).strip() or __import__("secrets").token_hex(8),
            name=str(item.get("name", "订阅")).strip() or "订阅",
            url=str(item.get("url", "")).strip(),
            last_updated=str(item.get("updatedAt", "")).strip(),
        )
        for item in subscriptions_data
        if str(item.get("url", "")).strip()
    ]

    selected_id = str(payload.get("selectedNodeId", ""))
    imported.selected_node = next(
        (node.name for node in nodes if node.node_id == selected_id),
        nodes[0].name if nodes else "",
    )
    routing = payload.get("routing", {})
    if not isinstance(routing, dict):
        raise ValueError("routing 必须是对象")
    imported.mode = {
        "rule": "RULE",
        "global": "GLOBAL_BUILTIN" if nodes else "RULE",
        "smart": "SMART" if nodes else "RULE",
        "direct": "DIRECT",
    }.get(str(routing.get("mode", "rule")).lower(), "RULE")
    imported.default_target = _windows_target(routing.get("fallback"), bool(nodes))

    process_rules = [rule for rule in imported.rules if rule.rule_type == "PROCESS-NAME"]
    common_settings = routing.get("commonOverseas", {})
    if not isinstance(common_settings, dict):
        common_settings = {}
    common_enabled = bool(common_settings.get("enabled", True))
    common_target = _windows_target(common_settings.get("target"), bool(nodes))
    common_rules = [
        rule
        for rule in default_routing_rules()
        if rule.rule_type == "DOMAIN-SUFFIX"
    ]
    for rule in common_rules:
        rule.enabled = common_enabled
        rule.target = common_target

    portable_rules: list[RoutingRule] = []
    for item in rules_data:
        rule_type = PORTABLE_RULE_TYPES.get(str(item.get("type", "")).lower())
        if not rule_type:
            continue
        rule = RoutingRule(
            rule_type=rule_type,
            value=normalize_rule_value(rule_type, str(item.get("value", ""))),
            target=_windows_target(item.get("target"), bool(nodes)),
            enabled=bool(item.get("enabled", True)),
            note=str(item.get("note", "")).strip(),
        )
        errors = validate_rule(rule)
        if errors:
            raise ValueError("；".join(errors))
        portable_rules.append(rule)
    imported.rules = process_rules + common_rules + portable_rules

    errors = validate_config(imported)
    if errors:
        raise ValueError(errors[0])
    return imported


def _export_rule(rule: RoutingRule) -> dict[str, Any]:
    portable_type = next(
        key for key, value in PORTABLE_RULE_TYPES.items() if value == rule.rule_type
    )
    return {
        "type": portable_type,
        "value": normalize_rule_value(rule.rule_type, rule.value),
        "target": _portable_target(rule.target),
        "enabled": rule.enabled,
        "note": rule.note,
    }


def _portable_target(target: str) -> str:
    return "direct" if str(target).upper() == "DIRECT" else "proxy"


def _windows_target(target: object, has_nodes: bool) -> str:
    return "BUILTIN" if str(target).lower() == "proxy" and has_nodes else "DIRECT"


def _validate_root(payload: dict[str, Any]) -> None:
    if not isinstance(payload, dict):
        raise ValueError("配置文件根节点必须是对象")
    if payload.get("format") != PORTABLE_FORMAT:
        raise ValueError("不是 Network Manager 跨设备配置")
    if payload.get("version") != PORTABLE_VERSION:
        raise ValueError("暂不支持这个配置版本")


def _object_list(value: object, name: str, limit: int) -> list[dict[str, Any]]:
    if not isinstance(value, list) or any(not isinstance(item, dict) for item in value):
        raise ValueError(f"{name} 必须是对象数组")
    if len(value) > limit:
        raise ValueError(f"{name} 数量超过限制")
    return value
