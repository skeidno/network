from __future__ import annotations

import base64
import json
from unittest.mock import MagicMock, patch

import requests

from network_manager.importers import (
    fetch_subscription,
    parse_import_content,
    parse_share_link,
    prepare_imported_nodes,
)


def _b64(value: str) -> str:
    return base64.urlsafe_b64encode(value.encode()).decode().rstrip("=")


def test_parse_vless_reality_websocket_link() -> None:
    node = parse_share_link(
        "vless://00000000-0000-0000-0000-000000000001@example.com:443"
        "?security=reality&sni=edge.example.com&fp=chrome&pbk=public-key&sid=abcd"
        "&type=ws&host=cdn.example.com&path=%2Fsocket#VLESS%20Demo"
    )
    assert node["name"] == "VLESS Demo"
    assert node["type"] == "vless"
    assert node["tls"] is True
    assert node["reality-opts"] == {"public-key": "public-key", "short-id": "abcd"}
    assert node["ws-opts"]["headers"]["Host"] == "cdn.example.com"


def test_parse_vmess_link() -> None:
    payload = {
        "v": "2",
        "ps": "VMess Demo",
        "add": "vm.example.com",
        "port": "443",
        "id": "00000000-0000-0000-0000-000000000002",
        "aid": "0",
        "scy": "auto",
        "net": "ws",
        "host": "cdn.example.com",
        "path": "/vmess",
        "tls": "tls",
        "sni": "vm.example.com",
    }
    node = parse_share_link("vmess://" + _b64(json.dumps(payload)))
    assert node["type"] == "vmess"
    assert node["port"] == 443
    assert node["ws-opts"]["path"] == "/vmess"


def test_parse_trojan_and_ss_links() -> None:
    trojan = parse_share_link(
        "trojan://secret@example.com:443?sni=edge.example.com&type=grpc&serviceName=demo#Trojan"
    )
    assert trojan["password"] == "secret"
    assert trojan["grpc-opts"]["grpc-service-name"] == "demo"

    userinfo = _b64("aes-128-gcm:password")
    shadowsocks = parse_share_link(f"ss://{userinfo}@1.2.3.4:8388#SS%20Demo")
    assert shadowsocks["cipher"] == "aes-128-gcm"
    assert shadowsocks["password"] == "password"


def test_parse_clash_yaml_and_base64_subscription() -> None:
    yaml_text = """
proxies:
  - name: YAML Demo
    type: socks5
    server: 127.0.0.1
    port: 1080
"""
    nodes, errors = parse_import_content(yaml_text)
    assert errors == []
    assert nodes[0]["name"] == "YAML Demo"

    content = "trojan://secret@example.com:443#One\nss://" + _b64(
        "aes-128-gcm:password@1.2.3.4:8388"
    )
    encoded = base64.b64encode(content.encode()).decode()
    nodes, errors = parse_import_content(encoded)
    assert len(nodes) == 2
    assert errors == []


def test_prepare_nodes_renames_collisions() -> None:
    first = prepare_imported_nodes(
        [{"name": "Same", "type": "socks5", "server": "127.0.0.1", "port": 1001}],
        "A",
    )
    second = prepare_imported_nodes(
        [{"name": "Same", "type": "socks5", "server": "127.0.0.1", "port": 1002}],
        "B",
        first,
    )
    assert first[0].name == "Same"
    assert second[0].name == "Same (2)"


def test_fetch_subscription_uses_clash_compatible_user_agent() -> None:
    yaml_content = b"""proxies:
  - name: Demo
    type: socks5
    server: 127.0.0.1
    port: 1080
"""
    response = MagicMock()
    response.content = yaml_content
    response.encoding = "utf-8"
    response.text = yaml_content.decode()

    with patch("network_manager.importers.requests.Session") as session_factory:
        session_factory.return_value.get.return_value = response
        content = fetch_subscription("https://example.com/subscription", "http://127.0.0.1:7897")

    assert content == yaml_content.decode()
    assert session_factory.return_value.trust_env is False
    session_factory.return_value.get.assert_called_once_with(
        "https://example.com/subscription",
        timeout=(8, 30),
        proxies={"http": "http://127.0.0.1:7897", "https": "http://127.0.0.1:7897"},
        headers={"User-Agent": "Clash.Meta"},
    )
    response.raise_for_status.assert_called_once_with()


def test_fetch_subscription_falls_back_to_v2rayn_format() -> None:
    forbidden = MagicMock()
    forbidden.raise_for_status.side_effect = requests.HTTPError("403 Forbidden")
    encoded = base64.b64encode(b"trojan://secret@example.com:443#Demo")
    v2ray_response = MagicMock(content=encoded, encoding="utf-8", text=encoded.decode())

    with patch("network_manager.importers.requests.Session") as session_factory:
        session_factory.return_value.get.side_effect = [forbidden, v2ray_response]
        content = fetch_subscription("https://example.com/subscription")

    nodes, errors = parse_import_content(content)
    assert errors == []
    assert [node["type"] for node in nodes] == ["trojan"]
    calls = session_factory.return_value.get.call_args_list
    assert calls[0].kwargs["headers"] == {"User-Agent": "Clash.Meta"}
    assert calls[1].kwargs["headers"] == {"User-Agent": "v2rayN/7.12.5"}


def test_fetch_subscription_resolves_clash_proxy_provider() -> None:
    parent_content = b"""proxies:
proxy-providers:
  remote:
    type: http
    url: /provider.yaml
"""
    provider_content = b"""proxies:
  - name: Provider Demo
    type: socks5
    server: 127.0.0.1
    port: 1080
"""
    parent = MagicMock(content=parent_content, encoding="utf-8", text=parent_content.decode())
    invalid = MagicMock(content=b"unsupported", encoding="utf-8", text="unsupported")
    provider = MagicMock(
        content=provider_content, encoding="utf-8", text=provider_content.decode()
    )

    with patch("network_manager.importers.requests.Session") as session_factory:
        session_factory.return_value.get.side_effect = [parent, invalid, invalid, provider]
        content = fetch_subscription("https://example.com/subscription")

    nodes, errors = parse_import_content(content)
    assert errors == []
    assert [node["name"] for node in nodes] == ["Provider Demo"]
    assert session_factory.return_value.get.call_args_list[-1].args[0] == (
        "https://example.com/provider.yaml"
    )
