from __future__ import annotations

import hashlib
import select
import socket
import socketserver
import struct
import threading
import time
from pathlib import Path
from typing import Any

import paramiko

from network_manager.models import SshServerProfile


class SshTunnelError(RuntimeError):
    pass


def _read_exact(stream: socket.socket, size: int) -> bytes:
    chunks = bytearray()
    while len(chunks) < size:
        chunk = stream.recv(size - len(chunks))
        if not chunk:
            raise ConnectionError("连接已关闭")
        chunks.extend(chunk)
    return bytes(chunks)


class _SocksHandler(socketserver.BaseRequestHandler):
    server: "_SocksServer"

    def handle(self) -> None:
        client = self.request
        client.settimeout(15)
        try:
            version, method_count = struct.unpack("!BB", _read_exact(client, 2))
            if version != 5:
                return
            methods = _read_exact(client, method_count)
            if 0 not in methods:
                client.sendall(b"\x05\xff")
                return
            client.sendall(b"\x05\x00")
            version, command, _reserved, address_type = struct.unpack(
                "!BBBB", _read_exact(client, 4)
            )
            if version != 5 or command != 1:
                self._reply(7)
                return
            if address_type == 1:
                host = socket.inet_ntoa(_read_exact(client, 4))
            elif address_type == 3:
                host = _read_exact(client, _read_exact(client, 1)[0]).decode("idna")
            elif address_type == 4:
                host = socket.inet_ntop(socket.AF_INET6, _read_exact(client, 16))
            else:
                self._reply(8)
                return
            port = struct.unpack("!H", _read_exact(client, 2))[0]
            channel = self.server.manager.open_channel(host, port, self.client_address)
            if channel is None:
                self._reply(5)
                return
            self._reply(0)
            client.settimeout(None)
            self._relay(client, channel)
        except (OSError, EOFError, ConnectionError, UnicodeError, struct.error):
            return

    def _reply(self, code: int) -> None:
        self.request.sendall(b"\x05" + bytes([code]) + b"\x00\x01\x00\x00\x00\x00\x00\x00")

    @staticmethod
    def _relay(client: socket.socket, channel: paramiko.Channel) -> None:
        try:
            while True:
                readable, _writable, _errors = select.select([client, channel], [], [], 30)
                if not readable:
                    continue
                if client in readable:
                    data = client.recv(65536)
                    if not data:
                        break
                    channel.sendall(data)
                if channel in readable:
                    data = channel.recv(65536)
                    if not data:
                        break
                    client.sendall(data)
        finally:
            channel.close()


class _SocksServer(socketserver.ThreadingTCPServer):
    allow_reuse_address = True
    daemon_threads = True

    def __init__(self, address: tuple[str, int], manager: "SshTunnelManager") -> None:
        self.manager = manager
        super().__init__(address, _SocksHandler)


class SshTunnelManager:
    def __init__(self, known_hosts_path: Path) -> None:
        self.known_hosts_path = known_hosts_path
        self._lock = threading.RLock()
        self._connect_lock = threading.Lock()
        self._client: paramiko.SSHClient | None = None
        self._server: _SocksServer | None = None
        self._server_thread: threading.Thread | None = None
        self._profile: SshServerProfile | None = None
        self._password = ""
        self._stopping = False
        self.status = "stopped"
        self.error = ""
        self.fingerprint = ""
        self.connected_at = 0.0

    @property
    def profile_id(self) -> str:
        with self._lock:
            return self._profile.profile_id if self._profile else ""

    @property
    def is_running(self) -> bool:
        with self._lock:
            transport = self._client.get_transport() if self._client else None
            return bool(self._server and transport and transport.is_active())

    def start(self, profile: SshServerProfile, password: str = "") -> None:
        self.stop()
        with self._lock:
            self.status = "connecting"
            self.error = ""
            self._profile = profile
            self._password = password
            self._stopping = False
        client: paramiko.SSHClient | None = None
        try:
            client = self._connect(profile, password)
            server = _SocksServer(("127.0.0.1", profile.local_port), self)
        except Exception as exc:
            with self._lock:
                self.status = "error"
                self.error = str(exc)
            if client is not None:
                client.close()
            raise SshTunnelError(str(exc)) from exc
        thread = threading.Thread(
            target=server.serve_forever,
            name=f"ssh-socks-{profile.local_port}",
            daemon=True,
        )
        with self._lock:
            self._client = client
            self._server = server
            self._server_thread = thread
            self.status = "connected"
            self.connected_at = time.time()
        thread.start()

    def stop(self) -> None:
        with self._lock:
            self._stopping = True
            server = self._server
            client = self._client
            thread = self._server_thread
            self._server = None
            self._client = None
            self._server_thread = None
            self.status = "stopped"
            self.error = ""
            self.connected_at = 0.0
        if server is not None:
            server.shutdown()
            server.server_close()
        if client is not None:
            client.close()
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=2)

    def open_channel(
        self, host: str, port: int, client_address: tuple[str, int]
    ) -> paramiko.Channel | None:
        transport = self._active_transport()
        if transport is None:
            try:
                self._reconnect()
            except SshTunnelError:
                return None
            transport = self._active_transport()
        if transport is None:
            return None
        try:
            return transport.open_channel("direct-tcpip", (host, port), client_address)
        except (OSError, paramiko.SSHException):
            return None

    def state(self) -> dict[str, Any]:
        with self._lock:
            profile = self._profile
            running = self.is_running
            status = self.status
            error = self.error
            if status == "connected" and not running:
                status = "error"
                error = error or "SSH 连接已断开，将在下次请求时重连"
            return {
                "status": status,
                "running": running,
                "profileId": profile.profile_id if profile else "",
                "localPort": profile.local_port if profile else 0,
                "fingerprint": self.fingerprint,
                "error": error,
                "connectedAt": self.connected_at,
            }

    def _active_transport(self) -> paramiko.Transport | None:
        with self._lock:
            transport = self._client.get_transport() if self._client else None
            return transport if transport and transport.is_active() else None

    def _reconnect(self) -> None:
        with self._connect_lock:
            if self._active_transport() is not None:
                return
            with self._lock:
                if self._stopping or self._profile is None:
                    raise SshTunnelError("SSH 隧道已停止")
                profile = self._profile
                password = self._password
                old_client = self._client
                self.status = "connecting"
            if old_client is not None:
                old_client.close()
            try:
                client = self._connect(profile, password)
            except Exception as exc:
                with self._lock:
                    self.status = "error"
                    self.error = str(exc)
                raise SshTunnelError(str(exc)) from exc
            with self._lock:
                self._client = client
                self.status = "connected"
                self.error = ""
                self.connected_at = time.time()

    def _connect(self, profile: SshServerProfile, password: str) -> paramiko.SSHClient:
        client = paramiko.SSHClient()
        client.load_system_host_keys()
        if self.known_hosts_path.is_file():
            client.load_host_keys(str(self.known_hosts_path))
        client.set_missing_host_key_policy(paramiko.AutoAddPolicy())
        options: dict[str, Any] = {
            "hostname": profile.host,
            "port": profile.port,
            "username": profile.username,
            "timeout": 12,
            "banner_timeout": 12,
            "auth_timeout": 15,
        }
        if profile.auth_method == "password":
            if not password:
                raise SshTunnelError("请输入 SSH 密码")
            options.update(password=password, allow_agent=False, look_for_keys=False)
        elif profile.auth_method == "key":
            options.update(
                key_filename=profile.key_path,
                passphrase=password or None,
                allow_agent=False,
                look_for_keys=False,
            )
        else:
            options.update(allow_agent=True, look_for_keys=True)
        client.connect(**options)
        transport = client.get_transport()
        if transport is None or not transport.is_active():
            client.close()
            raise SshTunnelError("SSH 连接没有建立")
        transport.set_keepalive(20)
        server_key = transport.get_remote_server_key()
        digest = hashlib.sha256(server_key.asbytes()).digest()
        self.fingerprint = "SHA256:" + __import__("base64").b64encode(digest).decode().rstrip("=")
        self.known_hosts_path.parent.mkdir(parents=True, exist_ok=True)
        client.save_host_keys(str(self.known_hosts_path))
        return client
