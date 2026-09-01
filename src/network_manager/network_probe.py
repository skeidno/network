from __future__ import annotations

import socket
from typing import Any, Mapping
from urllib.parse import quote

import requests

from network_manager.models import Upstream


IP_CHECKS = (
    ("https://1.1.1.1/cdn-cgi/trace", "trace"),
    ("https://api.ipify.org?format=json", "json"),
    ("http://api.ipify.org?format=json", "json"),
)


def port_is_open(host: str, port: int, timeout: float = 0.35) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def check_public_tcp_endpoint(
    host: str, port: int, timeout: float = 4.0
) -> tuple[bool, str]:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True, ""
    except socket.gaierror:
        return False, "服务器域名解析失败"
    except (TimeoutError, socket.timeout):
        return False, "公网连接超时"
    except ConnectionRefusedError:
        return False, "公网连接被拒绝"
    except OSError:
        return False, "公网端口不可达"


def diagnose_authenticated_proxy(
    node: Mapping[str, Any], timeout: float = 10.0
) -> str:
    if str(node.get("type", "")).lower() != "http":
        return ""
    host = str(node.get("server", "")).strip()
    username = str(node.get("username", ""))
    password = str(node.get("password", ""))
    try:
        port = int(node.get("port", 0))
    except (TypeError, ValueError):
        return "代理端口无效"
    if not host or not 1 <= port <= 65535:
        return "代理地址或端口无效"
    if not username or not password:
        return "代理缺少用户名或密码"

    proxy = (
        f"http://{quote(username, safe='')}:{quote(password, safe='')}@{host}:{port}"
    )
    session = requests.Session()
    session.trust_env = False
    try:
        response = session.get(
            "http://www.gstatic.com/generate_204",
            proxies={"http": proxy, "https": proxy},
            timeout=(min(timeout, 6.0), timeout),
            allow_redirects=False,
        )
    except requests.ConnectTimeout:
        return "代理端口连接超时"
    except requests.ProxyError:
        return "HTTP 代理握手失败，请确认供应商要求的代理协议"
    except requests.RequestException:
        return "认证代理连接失败"

    body = response.text[:1024].lower()
    if response.status_code == 407:
        return "代理账号或密码认证失败"
    if response.status_code == 403 and (
        "ip forbidden" in body or "ip banned" in body or "ip whitelist" in body
    ):
        return "代理供应商拒绝当前公网 IP，请在供应商后台添加白名单或解除封禁"
    if response.status_code == 403:
        return "代理服务器返回 403，请检查账号权限、IP 白名单和套餐状态"
    if response.status_code not in {200, 204}:
        return f"代理服务器返回 HTTP {response.status_code}"
    return "测速端点暂时不可用，请稍后重试"


def proxy_url(upstream: Upstream) -> str:
    scheme = "socks5h" if upstream.protocol == "socks5" else "http"
    return f"{scheme}://{upstream.host}:{upstream.port}"


def exit_ip_through_proxy(url: str, timeout: float = 12.0) -> str:
    session = requests.Session()
    session.trust_env = False
    proxies = {"http": url, "https": url}
    failures: list[str] = []
    for endpoint, response_type in IP_CHECKS:
        try:
            response = session.get(endpoint, timeout=timeout, proxies=proxies)
            response.raise_for_status()
            if response_type == "json":
                address = str(response.json()["ip"]).strip()
            else:
                address = next(
                    line.split("=", 1)[1].strip()
                    for line in response.text.splitlines()
                    if line.startswith("ip=")
                )
            if address:
                return address
        except (requests.RequestException, ValueError, KeyError, StopIteration) as exc:
            failures.append(str(exc))
    detail = failures[-1] if failures else "检测端点没有返回 IP"
    raise requests.RequestException(f"所有出口检测端点均失败：{detail}")


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
