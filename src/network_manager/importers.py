from __future__ import annotations

import base64
import binascii
import hashlib
import json
import re
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Iterable
from urllib.parse import parse_qs, unquote, urljoin, urlsplit

import requests
import yaml

from network_manager.models import ImportedNode, SubscriptionSource


SUPPORTED_SCHEMES = ("vmess", "vless", "trojan", "ss", "hysteria2", "hy2")
LINK_PATTERN = re.compile(
    r"(?:(?:vmess|vless|trojan|ss|hysteria2|hy2)://)[^\s]+", re.IGNORECASE
)
MAX_IMPORT_BYTES = 10 * 1024 * 1024
MAX_PROVIDER_SOURCES = 8
RESERVED_NAMES = {"DIRECT", "REJECT", "UPSTREAM-CLASH", "UPSTREAM-V2RAY", "IMPORTED-NODES"}
SUBSCRIPTION_USER_AGENTS = ("Clash.Meta", "v2rayN/7.12.5", "Shadowrocket/2.2")


class ImportContentError(ValueError):
    pass


def _padded_base64(value: str) -> str:
    return value + "=" * (-len(value) % 4)


def _decode_base64(value: str) -> bytes:
    compact = "".join(value.split())
    try:
        return base64.urlsafe_b64decode(_padded_base64(compact))
    except (binascii.Error, ValueError) as exc:
        raise ImportContentError("Base64 内容无效") from exc


def _bool(value: str | None) -> bool:
    return str(value or "").strip().lower() in {"1", "true", "yes", "on"}


def _first(query: dict[str, list[str]], key: str, default: str = "") -> str:
    values = query.get(key)
    return values[0] if values else default


def _name(fragment: str, fallback: str) -> str:
    return unquote(fragment).strip() or fallback


def _transport_options(
    node: dict[str, Any], network: str, query: dict[str, list[str]]
) -> None:
    network = (network or "tcp").lower()
    if network == "raw":
        network = "tcp"
    if network not in {"tcp", "ws", "http", "h2", "grpc", "xhttp"}:
        network = "tcp"
    node["network"] = network
    host = _first(query, "host")
    path = unquote(_first(query, "path", "/"))
    if network == "ws":
        options: dict[str, Any] = {"path": path}
        if host:
            options["headers"] = {"Host": host}
        node["ws-opts"] = options
    elif network == "grpc":
        service_name = _first(query, "serviceName") or path.lstrip("/")
        node["grpc-opts"] = {"grpc-service-name": service_name}
    elif network == "h2":
        options = {"path": path}
        if host:
            options["host"] = [part.strip() for part in host.split(",") if part.strip()]
        node["h2-opts"] = options
    elif network == "http":
        options = {"path": [path]}
        if host:
            options["headers"] = {"Host": [host]}
        node["http-opts"] = options
    elif network == "xhttp":
        options = {"path": path}
        if host:
            options["host"] = host
        mode = _first(query, "mode")
        if mode:
            options["mode"] = mode
        node["xhttp-opts"] = options


def _tls_options(node: dict[str, Any], query: dict[str, list[str]], security: str) -> None:
    if security not in {"tls", "reality"}:
        return
    node["tls"] = True
    servername = _first(query, "sni") or _first(query, "peer")
    if servername:
        node["servername"] = servername
    fingerprint = _first(query, "fp")
    if fingerprint:
        node["client-fingerprint"] = fingerprint
    alpn = _first(query, "alpn")
    if alpn:
        node["alpn"] = [part.strip() for part in alpn.split(",") if part.strip()]
    insecure = _first(query, "allowInsecure") or _first(query, "insecure")
    if insecure:
        node["skip-cert-verify"] = _bool(insecure)
    if security == "reality":
        public_key = _first(query, "pbk")
        short_id = _first(query, "sid")
        reality: dict[str, Any] = {}
        if public_key:
            reality["public-key"] = public_key
        if short_id:
            reality["short-id"] = short_id
        if reality:
            node["reality-opts"] = reality


def parse_vless_link(link: str) -> dict[str, Any]:
    parsed = urlsplit(link)
    query = parse_qs(parsed.query, keep_blank_values=True)
    if not parsed.hostname or not parsed.port or not parsed.username:
        raise ImportContentError("VLESS 链接缺少服务器、端口或 UUID")
    node: dict[str, Any] = {
        "name": _name(parsed.fragment, f"VLESS {parsed.hostname}:{parsed.port}"),
        "type": "vless",
        "server": parsed.hostname,
        "port": parsed.port,
        "uuid": unquote(parsed.username),
        "udp": True,
    }
    flow = _first(query, "flow")
    if flow:
        node["flow"] = flow
    encryption = _first(query, "encryption")
    if encryption and encryption != "none":
        node["encryption"] = encryption
    _tls_options(node, query, _first(query, "security").lower())
    _transport_options(node, _first(query, "type", "tcp"), query)
    return node


def parse_trojan_link(link: str) -> dict[str, Any]:
    parsed = urlsplit(link)
    query = parse_qs(parsed.query, keep_blank_values=True)
    if not parsed.hostname or not parsed.port or not parsed.username:
        raise ImportContentError("Trojan 链接缺少服务器、端口或密码")
    node: dict[str, Any] = {
        "name": _name(parsed.fragment, f"Trojan {parsed.hostname}:{parsed.port}"),
        "type": "trojan",
        "server": parsed.hostname,
        "port": parsed.port,
        "password": unquote(parsed.username),
        "udp": True,
    }
    sni = _first(query, "sni") or _first(query, "peer")
    if sni:
        node["sni"] = sni
    fingerprint = _first(query, "fp")
    if fingerprint:
        node["client-fingerprint"] = fingerprint
    if _first(query, "allowInsecure") or _first(query, "insecure"):
        node["skip-cert-verify"] = _bool(
            _first(query, "allowInsecure") or _first(query, "insecure")
        )
    alpn = _first(query, "alpn")
    if alpn:
        node["alpn"] = [part.strip() for part in alpn.split(",") if part.strip()]
    if _first(query, "security").lower() == "reality":
        reality = {
            key: value
            for key, value in {
                "public-key": _first(query, "pbk"),
                "short-id": _first(query, "sid"),
            }.items()
            if value
        }
        if reality:
            node["reality-opts"] = reality
    _transport_options(node, _first(query, "type", "tcp"), query)
    return node


def parse_vmess_link(link: str) -> dict[str, Any]:
    encoded = link.split("://", 1)[1].split("#", 1)[0]
    try:
        payload = json.loads(_decode_base64(encoded).decode("utf-8-sig"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise ImportContentError("VMess 链接内容无效") from exc
    try:
        port = int(payload.get("port"))
    except (TypeError, ValueError) as exc:
        raise ImportContentError("VMess 链接端口无效") from exc
    server = str(payload.get("add", "")).strip()
    uuid = str(payload.get("id", "")).strip()
    if not server or not uuid:
        raise ImportContentError("VMess 链接缺少服务器或 UUID")
    node: dict[str, Any] = {
        "name": str(payload.get("ps") or f"VMess {server}:{port}"),
        "type": "vmess",
        "server": server,
        "port": port,
        "uuid": uuid,
        "alterId": int(payload.get("aid") or 0),
        "cipher": str(payload.get("scy") or "auto"),
        "udp": True,
    }
    query = {
        "host": [str(payload.get("host") or "")],
        "path": [str(payload.get("path") or "/")],
        "sni": [str(payload.get("sni") or "")],
        "fp": [str(payload.get("fp") or "")],
        "alpn": [str(payload.get("alpn") or "")],
        "insecure": [str(payload.get("insecure") or "")],
    }
    security = str(payload.get("tls") or "").lower()
    _tls_options(node, query, security)
    _transport_options(node, str(payload.get("net") or "tcp"), query)
    return node


def _parse_ss_userinfo(value: str) -> tuple[str, str]:
    decoded = value
    if ":" not in decoded:
        try:
            decoded = _decode_base64(decoded).decode("utf-8")
        except (ImportContentError, UnicodeDecodeError):
            pass
    if ":" not in decoded:
        raise ImportContentError("Shadowsocks 链接缺少加密方式或密码")
    return decoded.split(":", 1)


def parse_ss_link(link: str) -> dict[str, Any]:
    raw = link.split("://", 1)[1]
    fragment = ""
    if "#" in raw:
        raw, fragment = raw.split("#", 1)
    if "@" not in raw:
        try:
            decoded = _decode_base64(raw.split("?", 1)[0]).decode("utf-8")
        except UnicodeDecodeError as exc:
            raise ImportContentError("Shadowsocks 链接内容无效") from exc
        raw = decoded
    parsed = urlsplit(f"ss://{raw}")
    if not parsed.hostname or not parsed.port or parsed.username is None:
        raise ImportContentError("Shadowsocks 链接缺少服务器或端口")
    userinfo = unquote(parsed.username)
    if parsed.password is not None:
        method, password = userinfo, unquote(parsed.password)
    else:
        method, password = _parse_ss_userinfo(userinfo)
    node: dict[str, Any] = {
        "name": _name(fragment, f"SS {parsed.hostname}:{parsed.port}"),
        "type": "ss",
        "server": parsed.hostname,
        "port": parsed.port,
        "cipher": method,
        "password": password,
        "udp": True,
    }
    query = parse_qs(parsed.query, keep_blank_values=True)
    plugin_value = _first(query, "plugin")
    if plugin_value:
        parts = unquote(plugin_value).split(";")
        plugin_name = parts[0]
        plugin_values: dict[str, str | bool] = {}
        for item in parts[1:]:
            if "=" in item:
                key, value = item.split("=", 1)
                plugin_values[key] = value
            elif item:
                plugin_values[item] = True
        if plugin_name in {"obfs-local", "simple-obfs"}:
            node["plugin"] = "obfs"
            node["plugin-opts"] = {
                "mode": plugin_values.get("obfs", "http"),
                "host": plugin_values.get("obfs-host", ""),
            }
        elif plugin_name == "v2ray-plugin":
            node["plugin"] = "v2ray-plugin"
            node["plugin-opts"] = {
                "mode": plugin_values.get("mode", "websocket"),
                "host": plugin_values.get("host", ""),
                "path": plugin_values.get("path", "/"),
                "tls": bool(plugin_values.get("tls", False)),
            }
    return node


def parse_hysteria2_link(link: str) -> dict[str, Any]:
    parsed = urlsplit(link)
    query = parse_qs(parsed.query, keep_blank_values=True)
    if not parsed.hostname or not parsed.port or not parsed.username:
        raise ImportContentError("Hysteria2 链接缺少服务器、端口或密码")
    password = unquote(parsed.username)
    if parsed.password:
        password = f"{password}:{unquote(parsed.password)}"
    node: dict[str, Any] = {
        "name": _name(parsed.fragment, f"Hysteria2 {parsed.hostname}:{parsed.port}"),
        "type": "hysteria2",
        "server": parsed.hostname,
        "port": parsed.port,
        "password": password,
    }
    sni = _first(query, "sni") or _first(query, "peer")
    if sni:
        node["sni"] = sni
    if _first(query, "insecure"):
        node["skip-cert-verify"] = _bool(_first(query, "insecure"))
    obfs = _first(query, "obfs")
    if obfs:
        node["obfs"] = obfs
        node["obfs-password"] = _first(query, "obfs-password")
    return node


def parse_share_link(link: str) -> dict[str, Any]:
    scheme = link.split("://", 1)[0].lower()
    parser = {
        "vmess": parse_vmess_link,
        "vless": parse_vless_link,
        "trojan": parse_trojan_link,
        "ss": parse_ss_link,
        "hysteria2": parse_hysteria2_link,
        "hy2": parse_hysteria2_link,
    }.get(scheme)
    if parser is None:
        raise ImportContentError(f"暂不支持 {scheme} 协议")
    return parser(link.strip())


def _yaml_nodes(text: str) -> list[dict[str, Any]]:
    try:
        data = yaml.safe_load(text)
    except yaml.YAMLError:
        return []
    if not isinstance(data, dict) or not isinstance(data.get("proxies"), list):
        return []
    return [dict(item) for item in data["proxies"] if isinstance(item, dict)]


def parse_import_content(text: str) -> tuple[list[dict[str, Any]], list[str]]:
    if len(text.encode("utf-8")) > MAX_IMPORT_BYTES:
        raise ImportContentError("导入内容超过 10 MB 限制")
    normalized = text.strip().lstrip("\ufeff")
    if not normalized:
        raise ImportContentError("没有可导入的内容")
    yaml_nodes = _yaml_nodes(normalized)
    if yaml_nodes:
        return yaml_nodes, []

    links = LINK_PATTERN.findall(normalized)
    if not links and "://" not in normalized:
        try:
            decoded = _decode_base64(normalized).decode("utf-8-sig")
        except (ImportContentError, UnicodeDecodeError):
            decoded = ""
        links = LINK_PATTERN.findall(decoded)
        if not links:
            yaml_nodes = _yaml_nodes(decoded)
            if yaml_nodes:
                return yaml_nodes, []
    if not links:
        raise ImportContentError("未找到 Clash proxies 或支持的分享链接")

    nodes: list[dict[str, Any]] = []
    errors: list[str] = []
    for index, link in enumerate(links, start=1):
        try:
            nodes.append(parse_share_link(link))
        except (ImportContentError, ValueError) as exc:
            errors.append(f"第 {index} 个链接：{exc}")
    if not nodes:
        raise ImportContentError(errors[0] if errors else "没有成功解析任何节点")
    return nodes, errors


def import_file(path: Path) -> tuple[list[dict[str, Any]], list[str]]:
    if path.stat().st_size > MAX_IMPORT_BYTES:
        raise ImportContentError("配置文件超过 10 MB 限制")
    return parse_import_content(path.read_text(encoding="utf-8-sig"))


def _node_fingerprint(node: dict[str, Any]) -> str:
    payload = json.dumps(node, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return hashlib.sha256(payload.encode("utf-8")).hexdigest()


def prepare_imported_nodes(
    raw_nodes: Iterable[dict[str, Any]],
    source: str,
    existing: Iterable[ImportedNode] = (),
    source_id: str = "",
) -> list[ImportedNode]:
    existing_names = {node.name for node in existing}
    existing_fingerprints = {_node_fingerprint(node.config) for node in existing}
    imported: list[ImportedNode] = []
    for index, raw_node in enumerate(raw_nodes, start=1):
        node = dict(raw_node)
        if not node.get("type") or not node.get("server"):
            continue
        if _node_fingerprint(node) in existing_fingerprints:
            continue
        base_name = str(node.get("name") or f"{node['type']} 节点 {index}").strip()
        if base_name in RESERVED_NAMES:
            base_name = f"节点 · {base_name}"
        name = base_name
        suffix = 2
        while name in existing_names:
            name = f"{base_name} ({suffix})"
            suffix += 1
        node["name"] = name
        existing_names.add(name)
        existing_fingerprints.add(_node_fingerprint(node))
        imported.append(
            ImportedNode(
                node_id=hashlib.sha256(
                    f"{source}\0{name}\0{_node_fingerprint(node)}".encode("utf-8")
                ).hexdigest()[:16],
                source=source,
                config=node,
                source_id=source_id,
            )
        )
    return imported


def _subscription_provider_urls(text: str, base_url: str) -> list[str]:
    try:
        data = yaml.safe_load(text)
    except yaml.YAMLError:
        return []
    if not isinstance(data, dict) or not isinstance(data.get("proxy-providers"), dict):
        return []

    urls: list[str] = []
    for provider in data["proxy-providers"].values():
        if not isinstance(provider, dict) or not provider.get("url"):
            continue
        provider_url = urljoin(base_url, str(provider["url"]).strip())
        if urlsplit(provider_url).scheme not in {"http", "https"}:
            continue
        if provider_url not in urls:
            urls.append(provider_url)
        if len(urls) >= MAX_PROVIDER_SOURCES:
            break
    return urls


def _subscription_response_text(response: requests.Response) -> str:
    try:
        return response.content.decode("utf-8-sig")
    except UnicodeDecodeError:
        response.encoding = response.encoding or "utf-8"
        return response.text


def _fetch_subscription(
    url: str,
    proxies: dict[str, str] | None,
    session: requests.Session,
    provider_depth: int,
) -> str:
    http_errors: list[requests.RequestException] = []
    parse_errors: list[ImportContentError] = []
    provider_urls: list[str] = []

    for user_agent in SUBSCRIPTION_USER_AGENTS:
        try:
            response = session.get(
                url,
                timeout=(8, 30),
                proxies=proxies,
                headers={"User-Agent": user_agent},
            )
            response.raise_for_status()
        except requests.RequestException as exc:
            http_errors.append(exc)
            continue
        if len(response.content) > MAX_IMPORT_BYTES:
            raise ImportContentError("订阅响应超过 10 MB 限制")

        content = _subscription_response_text(response)
        try:
            parse_import_content(content)
        except ImportContentError as exc:
            parse_errors.append(exc)
            if provider_depth == 0:
                for provider_url in _subscription_provider_urls(content, url):
                    if provider_url not in provider_urls:
                        provider_urls.append(provider_url)
        else:
            return content

    if provider_urls:
        provider_nodes: list[dict[str, Any]] = []
        for provider_url in provider_urls:
            try:
                content = _fetch_subscription(provider_url, proxies, session, provider_depth + 1)
                nodes, _errors = parse_import_content(content)
            except (ImportContentError, requests.RequestException):
                continue
            provider_nodes.extend(nodes)
        if provider_nodes:
            combined = yaml.safe_dump(
                {"proxies": provider_nodes}, allow_unicode=True, sort_keys=False
            )
            if len(combined.encode("utf-8")) > MAX_IMPORT_BYTES:
                raise ImportContentError("代理提供者节点超过 10 MB 限制")
            return combined

    if parse_errors:
        raise ImportContentError(
            "订阅已下载，但未识别到兼容的节点格式（已尝试 Clash、v2rayN 和 Shadowrocket）"
        ) from parse_errors[-1]
    if http_errors:
        raise http_errors[-1]
    raise ImportContentError("订阅下载失败")


def fetch_subscription(url: str, proxy_url: str | None = None) -> str:
    proxies = {"http": proxy_url, "https": proxy_url} if proxy_url else None
    session = requests.Session()
    session.trust_env = False
    return _fetch_subscription(url.strip(), proxies, session, provider_depth=0)


def subscription_from_url(name: str, url: str) -> SubscriptionSource:
    source_id = hashlib.sha256(url.encode("utf-8")).hexdigest()[:16]
    return SubscriptionSource(
        source_id=source_id,
        name=name.strip() or "订阅",
        url=url.strip(),
        last_updated=datetime.now(timezone.utc).isoformat(timespec="seconds"),
    )
