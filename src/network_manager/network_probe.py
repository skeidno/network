from __future__ import annotations

import socket

import requests

from network_manager.models import Upstream


IP_CHECK_URL = "https://api.ipify.org?format=json"


def port_is_open(host: str, port: int, timeout: float = 0.35) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def proxy_url(upstream: Upstream) -> str:
    scheme = "socks5h" if upstream.protocol == "socks5" else "http"
    return f"{scheme}://{upstream.host}:{upstream.port}"


def exit_ip_through_proxy(url: str, timeout: float = 12.0) -> str:
    session = requests.Session()
    session.trust_env = False
    response = session.get(
        IP_CHECK_URL,
        timeout=timeout,
        proxies={"http": url, "https": url},
    )
    response.raise_for_status()
    payload = response.json()
    return str(payload["ip"])


def test_upstream(upstream: Upstream) -> tuple[bool, str]:
    if not port_is_open(upstream.host, upstream.port):
        return False, "端口未监听"
    try:
        return True, exit_ip_through_proxy(proxy_url(upstream))
    except (requests.RequestException, ValueError, KeyError) as exc:
        return False, f"端口可用，但联网测试失败：{exc}"


def detect_download_proxy(clash: Upstream, v2ray: Upstream) -> str | None:
    for upstream in (clash, v2ray):
        if upstream.enabled and port_is_open(upstream.host, upstream.port):
            return proxy_url(upstream)
    return None
