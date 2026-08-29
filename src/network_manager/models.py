from __future__ import annotations

import ipaddress
import re
import secrets
from dataclasses import asdict, dataclass, field
from typing import Any
from urllib.parse import urlsplit


RULE_TYPES = ("PROCESS-NAME", "DOMAIN", "DOMAIN-SUFFIX", "DOMAIN-KEYWORD", "IP-CIDR")
TARGETS = ("CLASH", "V2RAY", "SSH", "BUILTIN", "DIRECT")
MODES = (
    "RULE",
    "GLOBAL_CLASH",
    "GLOBAL_V2RAY",
    "GLOBAL_SSH",
    "GLOBAL_BUILTIN",
    "DIRECT",
)
CONFIG_VERSION = 4

DEFAULT_PROXY_DOMAINS = (
    ("discord.com", "Discord"),
    ("discordapp.com", "Discord"),
    ("google.com", "Google"),
    ("googleapis.com", "Google"),
    ("gstatic.com", "Google"),
    ("googleusercontent.com", "Google"),
    ("youtube.com", "YouTube"),
    ("youtu.be", "YouTube"),
    ("ytimg.com", "YouTube"),
    ("googlevideo.com", "YouTube"),
    ("openai.com", "ChatGPT / OpenAI"),
    ("chatgpt.com", "ChatGPT / OpenAI"),
    ("oaistatic.com", "ChatGPT / OpenAI"),
    ("oaiusercontent.com", "ChatGPT / OpenAI"),
    ("claude.ai", "Claude / Anthropic"),
    ("anthropic.com", "Claude / Anthropic"),
    ("github.com", "GitHub"),
    ("githubassets.com", "GitHub"),
    ("githubusercontent.com", "GitHub"),
    ("gitlab.com", "GitLab"),
    ("stackoverflow.com", "Stack Overflow"),
    ("stackexchange.com", "Stack Exchange"),
    ("docker.com", "Docker"),
    ("docker.io", "Docker"),
    ("npmjs.com", "npm"),
    ("pypi.org", "PyPI"),
    ("pythonhosted.org", "PyPI"),
    ("huggingface.co", "Hugging Face"),
    ("perplexity.ai", "Perplexity"),
    ("poe.com", "Poe"),
    ("midjourney.com", "Midjourney"),
    ("x.ai", "xAI"),
    ("x.com", "X / Twitter"),
    ("twitter.com", "X / Twitter"),
    ("twimg.com", "X / Twitter"),
    ("facebook.com", "Facebook"),
    ("fbcdn.net", "Facebook"),
    ("instagram.com", "Instagram"),
    ("cdninstagram.com", "Instagram"),
    ("reddit.com", "Reddit"),
    ("redd.it", "Reddit"),
    ("redditstatic.com", "Reddit"),
    ("telegram.org", "Telegram"),
    ("telegram.me", "Telegram"),
    ("t.me", "Telegram"),
    ("whatsapp.com", "WhatsApp"),
    ("whatsapp.net", "WhatsApp"),
    ("linkedin.com", "LinkedIn"),
    ("netflix.com", "Netflix"),
    ("nflximg.net", "Netflix"),
    ("nflxvideo.net", "Netflix"),
    ("spotify.com", "Spotify"),
    ("scdn.co", "Spotify"),
    ("twitch.tv", "Twitch"),
    ("twitchcdn.net", "Twitch"),
    ("vimeo.com", "Vimeo"),
    ("wikipedia.org", "Wikipedia"),
    ("wikimedia.org", "Wikimedia"),
    ("medium.com", "Medium"),
    ("notion.so", "Notion"),
    ("slack.com", "Slack"),
    ("dropbox.com", "Dropbox"),
    ("duckduckgo.com", "DuckDuckGo"),
    ("quora.com", "Quora"),
    ("steamcommunity.com", "Steam"),
    ("steampowered.com", "Steam"),
)


@dataclass(slots=True)
class Upstream:
    name: str
    host: str
    port: int
    protocol: str = "socks5"
    enabled: bool = True

    @classmethod
    def from_dict(cls, data: dict[str, Any], fallback: "Upstream") -> "Upstream":
        return cls(
            name=str(data.get("name", fallback.name)),
            host=str(data.get("host", fallback.host)),
            port=int(data.get("port", fallback.port)),
            protocol=str(data.get("protocol", fallback.protocol)).lower(),
            enabled=bool(data.get("enabled", fallback.enabled)),
        )


@dataclass(slots=True)
class RoutingRule:
    rule_type: str
    value: str
    target: str
    enabled: bool = True
    note: str = ""

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "RoutingRule":
        return cls(
            rule_type=str(data.get("rule_type", "DOMAIN-SUFFIX")).upper(),
            value=str(data.get("value", "")).strip(),
            target=str(data.get("target", "DIRECT")).upper(),
            enabled=bool(data.get("enabled", True)),
            note=str(data.get("note", "")).strip(),
        )


@dataclass(slots=True)
class ImportedNode:
    node_id: str
    source: str
    config: dict[str, Any]
    source_id: str = ""

    @property
    def name(self) -> str:
        return str(self.config.get("name", "未命名节点"))

    @property
    def protocol(self) -> str:
        return str(self.config.get("type", "unknown"))

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "ImportedNode":
        raw_config = data.get("config", {})
        return cls(
            node_id=str(data.get("node_id") or secrets.token_hex(8)),
            source=str(data.get("source", "手动导入")),
            config=dict(raw_config) if isinstance(raw_config, dict) else {},
            source_id=str(data.get("source_id", "")),
        )


@dataclass(slots=True)
class SubscriptionSource:
    source_id: str
    name: str
    url: str
    last_updated: str = ""

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SubscriptionSource":
        return cls(
            source_id=str(data.get("source_id") or secrets.token_hex(8)),
            name=str(data.get("name", "订阅")),
            url=str(data.get("url", "")),
            last_updated=str(data.get("last_updated", "")),
        )


@dataclass(slots=True)
class SshServerProfile:
    profile_id: str
    name: str
    host: str
    port: int = 22
    username: str = "root"
    local_port: int = 10888
    auth_method: str = "password"
    key_path: str = ""
    remember_password: bool = False
    auto_connect: bool = False

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "SshServerProfile":
        return cls(
            profile_id=str(data.get("profile_id") or secrets.token_hex(8)),
            name=str(data.get("name", "SSH 服务器")).strip() or "SSH 服务器",
            host=str(data.get("host", "")).strip(),
            port=int(data.get("port", 22)),
            username=str(data.get("username", "root")).strip(),
            local_port=int(data.get("local_port", 10888)),
            auth_method=str(data.get("auth_method", "password")).lower(),
            key_path=str(data.get("key_path", "")).strip(),
            remember_password=bool(data.get("remember_password", False)),
            auto_connect=bool(data.get("auto_connect", False)),
        )


@dataclass(slots=True)
class AppConfig:
    version: int = CONFIG_VERSION
    mode: str = "RULE"
    default_target: str = "DIRECT"
    mixed_port: int = 17897
    controller_port: int = 19090
    dns_port: int = 11053
    controller_secret: str = field(default_factory=lambda: secrets.token_urlsafe(24))
    strict_route: bool = True
    start_on_launch: bool = False
    close_to_tray: bool = True
    start_with_windows: bool = False
    clash: Upstream = field(
        default_factory=lambda: Upstream("Clash 7897", "127.0.0.1", 7897, "socks5")
    )
    v2ray: Upstream = field(
        default_factory=lambda: Upstream("v2ray 10808", "127.0.0.1", 10808, "socks5")
    )
    selected_node: str = ""
    imported_nodes: list[ImportedNode] = field(default_factory=list)
    subscriptions: list[SubscriptionSource] = field(default_factory=list)
    ssh_servers: list[SshServerProfile] = field(default_factory=list)
    selected_ssh_server: str = ""
    rules: list[RoutingRule] = field(default_factory=list)

    def to_dict(self) -> dict[str, Any]:
        return asdict(self)

    @classmethod
    def from_dict(cls, data: dict[str, Any]) -> "AppConfig":
        defaults = default_config()
        rules_data = data.get("rules", defaults.rules)
        rules = [
            item if isinstance(item, RoutingRule) else RoutingRule.from_dict(item)
            for item in rules_data
            if isinstance(item, (dict, RoutingRule))
        ]
        imported_nodes = [
            item if isinstance(item, ImportedNode) else ImportedNode.from_dict(item)
            for item in data.get("imported_nodes", [])
            if isinstance(item, (dict, ImportedNode))
        ]
        subscriptions = [
            item if isinstance(item, SubscriptionSource) else SubscriptionSource.from_dict(item)
            for item in data.get("subscriptions", [])
            if isinstance(item, (dict, SubscriptionSource))
        ]
        ssh_servers = [
            item if isinstance(item, SshServerProfile) else SshServerProfile.from_dict(item)
            for item in data.get("ssh_servers", [])
            if isinstance(item, (dict, SshServerProfile))
        ]
        return cls(
            version=int(data.get("version", 1)),
            mode=str(data.get("mode", defaults.mode)).upper(),
            default_target=str(data.get("default_target", defaults.default_target)).upper(),
            mixed_port=int(data.get("mixed_port", defaults.mixed_port)),
            controller_port=int(data.get("controller_port", defaults.controller_port)),
            dns_port=int(data.get("dns_port", defaults.dns_port)),
            controller_secret=str(data.get("controller_secret", defaults.controller_secret)),
            strict_route=bool(data.get("strict_route", defaults.strict_route)),
            start_on_launch=bool(data.get("start_on_launch", defaults.start_on_launch)),
            close_to_tray=bool(data.get("close_to_tray", defaults.close_to_tray)),
            start_with_windows=bool(
                data.get("start_with_windows", defaults.start_with_windows)
            ),
            clash=Upstream.from_dict(data.get("clash", {}), defaults.clash),
            v2ray=Upstream.from_dict(data.get("v2ray", {}), defaults.v2ray),
            selected_node=str(data.get("selected_node", "")),
            imported_nodes=imported_nodes,
            subscriptions=subscriptions,
            ssh_servers=ssh_servers,
            selected_ssh_server=str(data.get("selected_ssh_server", "")),
            rules=rules,
        )


def default_routing_rules() -> list[RoutingRule]:
    rules = [
        RoutingRule(
            rule_type="PROCESS-NAME",
            value="Discord.exe",
            target="CLASH",
            note="Discord 全部流量",
        )
    ]
    rules.extend(
        RoutingRule(
            rule_type="DOMAIN-SUFFIX",
            value=domain,
            target="CLASH",
            note=label,
        )
        for domain, label in DEFAULT_PROXY_DOMAINS
    )
    return rules


def default_config() -> AppConfig:
    return AppConfig(rules=default_routing_rules())


def normalize_rule_value(rule_type: str, value: str) -> str:
    rule_type = rule_type.upper()
    text = value.strip()
    if rule_type in {"DOMAIN", "DOMAIN-SUFFIX", "DOMAIN-KEYWORD"}:
        if "://" in text:
            text = urlsplit(text).hostname or text
        text = text.split("/", 1)[0].strip().lower().rstrip(".")
        if rule_type == "DOMAIN-SUFFIX":
            text = text.removeprefix("*.").removeprefix(".")
    elif rule_type == "PROCESS-NAME":
        text = text.replace("/", "\\").rsplit("\\", 1)[-1]
    return text


def migrate_config(config: AppConfig) -> bool:
    if config.version >= CONFIG_VERSION:
        return False

    existing = {
        (rule.rule_type, normalize_rule_value(rule.rule_type, rule.value))
        for rule in config.rules
    }
    for rule in default_routing_rules():
        key = (rule.rule_type, normalize_rule_value(rule.rule_type, rule.value))
        if key not in existing:
            config.rules.append(rule)
            existing.add(key)
    config.version = CONFIG_VERSION
    return True


def validate_rule(rule: RoutingRule) -> list[str]:
    errors: list[str] = []
    if rule.rule_type not in RULE_TYPES:
        errors.append("不支持的规则类型")
    if rule.target not in TARGETS:
        errors.append("不支持的代理目标")
    value = normalize_rule_value(rule.rule_type, rule.value)
    if not value:
        errors.append("匹配内容不能为空")
        return errors
    if "," in value or "\n" in value or "\r" in value:
        errors.append("匹配内容不能包含逗号或换行")
    if rule.rule_type in {"DOMAIN", "DOMAIN-SUFFIX"}:
        if " " in value or "." not in value:
            errors.append("请输入有效域名，例如 example.com")
    if rule.rule_type == "PROCESS-NAME" and not re.fullmatch(r"[^<>:\"/\\|?*]+", value):
        errors.append("请输入程序文件名，例如 Discord.exe")
    if rule.rule_type == "IP-CIDR":
        try:
            ipaddress.ip_network(value, strict=False)
        except ValueError:
            errors.append("请输入有效 IP 或 CIDR，例如 1.2.3.0/24")
    return errors


def validate_config(config: AppConfig) -> list[str]:
    errors: list[str] = []
    if config.mode not in MODES:
        errors.append("运行模式无效")
    if config.default_target not in TARGETS:
        errors.append("默认目标无效")
    ports = (config.mixed_port, config.controller_port, config.dns_port)
    if any(port < 1 or port > 65535 for port in ports):
        errors.append("本地端口必须在 1 到 65535 之间")
    if len(set(ports)) != len(ports):
        errors.append("入口、控制器和 DNS 端口不能相同")
    for upstream in (config.clash, config.v2ray):
        if not upstream.host.strip():
            errors.append(f"{upstream.name} 地址不能为空")
        if upstream.port < 1 or upstream.port > 65535:
            errors.append(f"{upstream.name} 端口无效")
        if upstream.protocol not in {"socks5", "http"}:
            errors.append(f"{upstream.name} 仅支持 SOCKS5 或 HTTP")
    target_enabled = {
        "CLASH": config.clash.enabled,
        "V2RAY": config.v2ray.enabled,
        "SSH": any(
            profile.profile_id == config.selected_ssh_server
            for profile in config.ssh_servers
        ),
        "BUILTIN": bool(config.imported_nodes),
        "DIRECT": True,
    }
    active_targets = {config.default_target}
    if config.mode == "GLOBAL_CLASH":
        active_targets.add("CLASH")
    elif config.mode == "GLOBAL_V2RAY":
        active_targets.add("V2RAY")
    elif config.mode == "GLOBAL_SSH":
        active_targets.add("SSH")
    elif config.mode == "GLOBAL_BUILTIN":
        active_targets.add("BUILTIN")
    active_targets.update(rule.target for rule in config.rules if rule.enabled)
    for target in sorted(active_targets):
        if target in target_enabled and not target_enabled[target]:
            errors.append(f"规则使用了已禁用的 {target} 代理源")
    for index, rule in enumerate(config.rules, start=1):
        for message in validate_rule(rule):
            errors.append(f"第 {index} 条规则：{message}")
    names: set[str] = set()
    for index, node in enumerate(config.imported_nodes, start=1):
        node_name = node.name.strip()
        if not node_name:
            errors.append(f"第 {index} 个内置节点缺少名称")
        elif node_name in names:
            errors.append(f"内置节点名称重复：{node_name}")
        names.add(node_name)
        if not node.config.get("type"):
            errors.append(f"内置节点 {node_name or index} 缺少协议类型")
    if config.selected_node and config.selected_node not in names:
        errors.append("当前选择的内置节点不存在")
    profile_ids: set[str] = set()
    local_ports: set[int] = set(ports)
    for index, profile in enumerate(config.ssh_servers, start=1):
        if not profile.host:
            errors.append(f"第 {index} 个 SSH 服务器地址不能为空")
        if not profile.username:
            errors.append(f"第 {index} 个 SSH 服务器用户名不能为空")
        if not 1 <= profile.port <= 65535:
            errors.append(f"第 {index} 个 SSH 服务器端口无效")
        if not 1024 <= profile.local_port <= 65535:
            errors.append(f"第 {index} 个 SSH 本地端口必须在 1024 到 65535 之间")
        if profile.local_port in local_ports:
            errors.append(f"SSH 本地端口冲突：{profile.local_port}")
        local_ports.add(profile.local_port)
        if profile.auth_method not in {"password", "key", "agent"}:
            errors.append(f"第 {index} 个 SSH 认证方式无效")
        if profile.auth_method == "key" and not profile.key_path:
            errors.append(f"第 {index} 个 SSH 服务器未选择私钥")
        if profile.profile_id in profile_ids:
            errors.append("SSH 服务器标识重复")
        profile_ids.add(profile.profile_id)
    if config.selected_ssh_server and config.selected_ssh_server not in profile_ids:
        errors.append("当前选择的 SSH 服务器不存在")
    return errors
