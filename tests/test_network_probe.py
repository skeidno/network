from unittest.mock import Mock, patch

import requests

from network_manager.network_probe import (
    check_public_tcp_endpoint,
    diagnose_authenticated_proxy,
    exit_ip_through_proxy,
)


def test_exit_ip_falls_back_when_first_endpoint_fails() -> None:
    failed = Mock()
    failed.raise_for_status.side_effect = requests.RequestException("unavailable")
    success = Mock()
    success.raise_for_status.return_value = None
    success.json.return_value = {"ip": "203.0.113.8"}

    with patch("network_manager.network_probe.requests.Session") as session_type:
        session_type.return_value.get.side_effect = [failed, success]
        address = exit_ip_through_proxy("http://127.0.0.1:17897")

    assert address == "203.0.113.8"
    assert session_type.return_value.trust_env is False


def test_authenticated_proxy_reports_provider_ip_block_without_leaking_credentials() -> None:
    response = Mock(status_code=403, text="errorMsg: ip forbidden, ip banned access")
    node = {
        "type": "http",
        "server": "proxy.example.com",
        "port": 4600,
        "username": "account-region-br",
        "password": "private-password",
    }

    with patch("network_manager.network_probe.requests.Session") as session_type:
        session_type.return_value.get.return_value = response
        message = diagnose_authenticated_proxy(node)

    assert message == "代理供应商拒绝当前公网 IP，请在供应商后台添加白名单或解除封禁"
    assert node["username"] not in message
    assert node["password"] not in message
    assert session_type.return_value.trust_env is False


def test_authenticated_proxy_reports_authentication_failure() -> None:
    response = Mock(status_code=407, text="Proxy Authentication Required")

    with patch("network_manager.network_probe.requests.Session") as session_type:
        session_type.return_value.get.return_value = response
        message = diagnose_authenticated_proxy(
            {
                "type": "http",
                "server": "proxy.example.com",
                "port": 4600,
                "username": "account",
                "password": "password",
            }
        )

    assert message == "代理账号或密码认证失败"


def test_public_tcp_endpoint_reports_timeout() -> None:
    with patch(
        "network_manager.network_probe.socket.create_connection",
        side_effect=TimeoutError,
    ):
        assert check_public_tcp_endpoint("proxy.example.com", 24443) == (
            False,
            "公网连接超时",
        )
