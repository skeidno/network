from __future__ import annotations

import socket
import socketserver
import struct
import threading

from network_manager.models import SshServerProfile
from network_manager.ssh_tunnel import SshTunnelManager, _SocksServer


class _EchoHandler(socketserver.BaseRequestHandler):
    def handle(self) -> None:
        while data := self.request.recv(4096):
            self.request.sendall(data)


class _Transport:
    def is_active(self) -> bool:
        return True

    def open_channel(self, _kind, destination, _source):
        return socket.create_connection(destination, timeout=3)


class _Client:
    def __init__(self) -> None:
        self.transport = _Transport()

    def get_transport(self):
        return self.transport

    def close(self) -> None:
        return


def test_socks5_tunnel_forwards_tcp_data(tmp_path) -> None:
    echo = socketserver.ThreadingTCPServer(("127.0.0.1", 0), _EchoHandler)
    echo_thread = threading.Thread(target=echo.serve_forever, daemon=True)
    echo_thread.start()
    manager = SshTunnelManager(tmp_path / "known-hosts")
    manager._profile = SshServerProfile("test", "test", "example.com")
    manager._client = _Client()  # type: ignore[assignment]
    socks = _SocksServer(("127.0.0.1", 0), manager)
    manager._server = socks
    socks_thread = threading.Thread(target=socks.serve_forever, daemon=True)
    socks_thread.start()
    try:
        with socket.create_connection(socks.server_address, timeout=3) as client:
            client.sendall(b"\x05\x01\x00")
            assert client.recv(2) == b"\x05\x00"
            host = socket.inet_aton("127.0.0.1")
            client.sendall(
                b"\x05\x01\x00\x01" + host + struct.pack("!H", echo.server_address[1])
            )
            assert client.recv(10)[:2] == b"\x05\x00"
            client.sendall(b"network-manager")
            assert client.recv(15) == b"network-manager"
    finally:
        manager.stop()
        echo.shutdown()
        echo.server_close()
        echo_thread.join(timeout=2)
