from unittest.mock import Mock, patch

import requests

from network_manager.network_probe import exit_ip_through_proxy


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
