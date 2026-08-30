from network_manager.importers import parse_share_link
from network_manager.models import SshServerProfile
from network_manager.server_deployer import (
    SHADOWSOCKS_METHOD,
    ServerProxyDeployer,
    build_shadowsocks_node,
    shadowsocks_share_link,
)


def test_generated_server_node_round_trips_through_share_link_parser() -> None:
    profile = SshServerProfile(
        profile_id="server-1",
        name="Tokyo server",
        host="203.0.113.10",
        proxy_port=24443,
    )
    node = build_shadowsocks_node(profile, "bW9jay1zZWNyZXQ=")

    parsed = parse_share_link(shadowsocks_share_link(node))

    assert parsed == node
    assert parsed["cipher"] == SHADOWSOCKS_METHOD


def test_server_config_uses_encrypted_shadowsocks_on_tcp_and_udp() -> None:
    config = ServerProxyDeployer._server_config(24443, "secret")
    inbound = config["inbounds"][0]

    assert inbound["type"] == "shadowsocks"
    assert inbound["listen_port"] == 24443
    assert inbound["method"] == SHADOWSOCKS_METHOD
    assert "network" not in inbound
    assert inbound["multiplex"] == {"enabled": True}


def test_install_script_restarts_existing_service_and_keeps_rollback() -> None:
    script = ServerProxyDeployer._install_script("amd64", "/tmp/config", "/tmp/service")

    assert "systemctl restart network-manager-proxy" in script
    assert "config.backup" in script
    assert "service.backup" in script
