from __future__ import annotations

from pathlib import Path
from typing import Any

import yaml

from network_manager.models import AppConfig, RoutingRule, normalize_rule_value


TARGET_NAMES = {
    "CLASH": "UPSTREAM-CLASH",
    "V2RAY": "UPSTREAM-V2RAY",
    "SSH": "UPSTREAM-SSH",
    "BUILTIN": "IMPORTED-NODES",
    "DIRECT": "DIRECT",
}

UPSTREAM_PROCESSES = (
    "verge-mihomo.exe",
    "clash.exe",
    "clash-win64.exe",
    "v2rayN.exe",
    "xray.exe",
    "v2ray.exe",
    "NetworkManager.exe",
    "network-manager.exe",
    "python.exe",
    "pythonw.exe",
)


def _proxy_entry(name: str, host: str, port: int, protocol: str) -> dict[str, Any]:
    entry: dict[str, Any] = {
        "name": name,
        "type": protocol,
        "server": host,
        "port": port,
    }
    if protocol == "socks5":
        entry["udp"] = True
    return entry


def _rule_line(rule: RoutingRule) -> str:
    value = normalize_rule_value(rule.rule_type, rule.value)
    suffix = ",no-resolve" if rule.rule_type == "IP-CIDR" else ""
    return f"{rule.rule_type},{value},{TARGET_NAMES[rule.target]}{suffix}"


def build_mihomo_config(config: AppConfig) -> dict[str, Any]:
    proxies: list[dict[str, Any]] = []
    if config.clash.enabled:
        proxies.append(
            _proxy_entry(
                TARGET_NAMES["CLASH"],
                config.clash.host,
                config.clash.port,
                config.clash.protocol,
            )
        )
    if config.v2ray.enabled:
        proxies.append(
            _proxy_entry(
                TARGET_NAMES["V2RAY"],
                config.v2ray.host,
                config.v2ray.port,
                config.v2ray.protocol,
            )
        )
    ssh_profile = next(
        (
            profile
            for profile in config.ssh_servers
            if profile.profile_id == config.selected_ssh_server
        ),
        None,
    )
    if ssh_profile is not None:
        proxies.append(
            _proxy_entry(
                TARGET_NAMES["SSH"],
                "127.0.0.1",
                ssh_profile.local_port,
                "socks5",
            )
        )
        proxies[-1]["udp"] = False
    proxies.extend(dict(node.config) for node in config.imported_nodes)

    rules = [f"PROCESS-NAME,{name},DIRECT" for name in UPSTREAM_PROCESSES]
    if config.mode == "RULE":
        rules.extend(_rule_line(rule) for rule in config.rules if rule.enabled)
        final_target = TARGET_NAMES[config.default_target]
    elif config.mode == "GLOBAL_CLASH":
        final_target = TARGET_NAMES["CLASH"]
    elif config.mode == "GLOBAL_V2RAY":
        final_target = TARGET_NAMES["V2RAY"]
    elif config.mode == "GLOBAL_SSH":
        final_target = TARGET_NAMES["SSH"]
    elif config.mode == "GLOBAL_BUILTIN":
        final_target = TARGET_NAMES["BUILTIN"]
    else:
        final_target = "DIRECT"
    rules.append(f"MATCH,{final_target}")

    result: dict[str, Any] = {
        "mixed-port": config.mixed_port,
        "allow-lan": False,
        "bind-address": "127.0.0.1",
        "mode": "rule",
        "log-level": "info",
        "ipv6": False,
        "unified-delay": True,
        "find-process-mode": "strict",
        "external-controller": f"127.0.0.1:{config.controller_port}",
        "secret": config.controller_secret,
        "profile": {"store-selected": False, "store-fake-ip": True},
        "tun": {
            "enable": True,
            "stack": "mixed",
            "device": "NetWorkManger",
            "auto-route": True,
            "auto-detect-interface": True,
            "strict-route": config.strict_route,
            "dns-hijack": ["any:53", "tcp://any:53"],
            "route-exclude-address": [
                "10.0.0.0/8",
                "127.0.0.0/8",
                "169.254.0.0/16",
                "172.16.0.0/12",
                "192.168.0.0/16",
                "224.0.0.0/4",
            ],
        },
        "sniffer": {
            "enable": True,
            "force-dns-mapping": True,
            "parse-pure-ip": True,
            "override-destination": True,
            "sniff": {
                "HTTP": {"ports": [80, "8080-8880"]},
                "TLS": {"ports": [443, 8443]},
                "QUIC": {"ports": [443, 8443]},
            },
            "skip-domain": ["Mijia Cloud", "+.push.apple.com"],
        },
        "dns": {
            "enable": True,
            "listen": f"127.0.0.1:{config.dns_port}",
            "ipv6": False,
            "enhanced-mode": "fake-ip",
            "fake-ip-range": "198.18.0.1/16",
            "fake-ip-filter": [
                "+.lan",
                "+.local",
                "localhost.ptlogin2.qq.com",
                "time.windows.com",
                "time.nist.gov",
            ],
            "default-nameserver": ["223.5.5.5", "1.1.1.1"],
            "nameserver": [
                "https://dns.alidns.com/dns-query",
                "https://1.1.1.1/dns-query",
            ],
        },
        "proxies": proxies,
        "rules": rules,
    }
    if config.imported_nodes:
        node_names = [node.name for node in config.imported_nodes]
        if config.selected_node in node_names:
            node_names.remove(config.selected_node)
            node_names.insert(0, config.selected_node)
        result["proxy-groups"] = [
            {
                "name": TARGET_NAMES["BUILTIN"],
                "type": "select",
                "proxies": node_names,
            }
        ]
    return result


def write_mihomo_config(config: AppConfig, path: Path) -> Path:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_suffix(".tmp")
    temporary.write_text(
        yaml.safe_dump(
            build_mihomo_config(config),
            allow_unicode=True,
            sort_keys=False,
            default_flow_style=False,
        ),
        encoding="utf-8",
    )
    temporary.replace(path)
    return path
