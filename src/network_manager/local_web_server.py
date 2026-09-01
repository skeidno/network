from __future__ import annotations

import base64
from dataclasses import dataclass, field
from http import HTTPStatus
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import mimetypes
from pathlib import Path
import secrets
import threading
from typing import Any
from urllib.parse import unquote, urlsplit


@dataclass(slots=True)
class BridgeCall:
    method: str
    args: list[Any]
    completed: threading.Event = field(default_factory=threading.Event)
    result: Any = None
    error: str = ""


class DirectBridgeDispatcher:
    """Dispatch requests for a thread-safe headless controller without Qt."""

    def __init__(self, bridge: Any, allowed_methods: set[str]) -> None:
        self.bridge = bridge
        self.allowed_methods = allowed_methods

    def dispatch(self, method: str, args: list[Any], timeout: float = 30.0) -> Any:
        if method not in self.allowed_methods:
            raise ValueError("不支持的操作")
        return getattr(self.bridge, method)(*args)


def _qt_dispatcher(bridge: Any, allowed_methods: set[str]) -> Any:
    from PySide6.QtCore import QObject, Qt, Signal, Slot

    class QtBridgeDispatcher(QObject):
        """Move HTTP worker requests onto the Qt GUI thread."""

        call_requested = Signal(object)

        def __init__(self) -> None:
            super().__init__(bridge)
            self.call_requested.connect(self._execute, Qt.ConnectionType.QueuedConnection)

        def dispatch(self, method: str, args: list[Any], timeout: float = 30.0) -> Any:
            if method not in allowed_methods:
                raise ValueError("不支持的操作")
            call = BridgeCall(method=method, args=args)
            self.call_requested.emit(call)
            if not call.completed.wait(timeout):
                raise TimeoutError("应用响应超时")
            if call.error:
                raise RuntimeError(call.error)
            return call.result

        @Slot(object)
        def _execute(self, call: BridgeCall) -> None:
            try:
                method = getattr(bridge, call.method)
                call.result = method(*call.args)
            except Exception as exc:  # pragma: no cover - final IPC boundary
                call.error = str(exc) or exc.__class__.__name__
            finally:
                call.completed.set()

    return QtBridgeDispatcher()


class LocalWebServer:
    """Serve the WebGUI and its authenticated loopback API."""

    def __init__(
        self,
        web_root: Path,
        bridge: Any,
        allowed_methods: set[str],
        *,
        direct_dispatch: bool = False,
        host: str = "127.0.0.1",
        port: int = 0,
        access_username: str = "admin",
        access_password: str = "",
    ) -> None:
        self.web_root = web_root.resolve()
        self.token = secrets.token_urlsafe(32)
        self.host = host.strip() or "127.0.0.1"
        self.port = int(port)
        self.access_username = access_username.strip() or "admin"
        self.access_password = access_password
        if not _is_loopback_host(self.host) and not self.access_password:
            raise ValueError("远程监听必须设置 WebGUI 管理密码")
        self.dispatcher = (
            DirectBridgeDispatcher(bridge, allowed_methods)
            if direct_dispatch
            else _qt_dispatcher(bridge, allowed_methods)
        )
        self._httpd: ThreadingHTTPServer | None = None
        self._thread: threading.Thread | None = None

    @property
    def url(self) -> str:
        if self._httpd is None:
            raise RuntimeError("WebGUI 服务尚未启动")
        display_host = "127.0.0.1" if self.host in {"0.0.0.0", "::"} else self.host
        return f"http://{display_host}:{self._httpd.server_port}/"

    def start(self) -> str:
        if self._httpd is not None:
            return self.url

        owner = self

        class RequestHandler(BaseHTTPRequestHandler):
            server_version = "NetworkManagerWeb/1.0"

            def do_GET(self) -> None:  # noqa: N802
                path = urlsplit(self.path).path
                if path == "/health":
                    self._send_json({"ok": True})
                    return
                if path == "/api/state":
                    self._handle_read("getState", "application/json; charset=utf-8")
                    return
                if path == "/api/logs":
                    self._handle_read("getLogs", "text/plain; charset=utf-8")
                    return
                self._serve_static(path)

            def do_POST(self) -> None:  # noqa: N802
                if urlsplit(self.path).path != "/api/call":
                    self.send_error(HTTPStatus.NOT_FOUND)
                    return
                if not self._authorized():
                    self._send_json({"ok": False, "error": "请求未授权"}, HTTPStatus.FORBIDDEN)
                    return
                try:
                    size = int(self.headers.get("Content-Length", "0"))
                    if size < 1 or size > 12 * 1024 * 1024:
                        raise ValueError("请求大小无效")
                    payload = json.loads(self.rfile.read(size).decode("utf-8"))
                    method = str(payload["method"])
                    args = payload.get("args", [])
                    if not isinstance(args, list):
                        raise ValueError("参数格式无效")
                    result = owner.dispatcher.dispatch(method, args)
                    self._send_json({"ok": True, "result": result})
                except (KeyError, TypeError, ValueError, RuntimeError, TimeoutError) as exc:
                    self._send_json(
                        {"ok": False, "error": str(exc) or "请求失败"},
                        HTTPStatus.BAD_REQUEST,
                    )

            def do_OPTIONS(self) -> None:  # noqa: N802
                self.send_response(HTTPStatus.NO_CONTENT)
                self.send_header("Allow", "GET, POST, OPTIONS")
                self.send_header("Content-Length", "0")
                self.end_headers()

            def _handle_read(self, method: str, content_type: str) -> None:
                if not self._authorized():
                    self._send_json({"ok": False, "error": "请求未授权"}, HTTPStatus.FORBIDDEN)
                    return
                try:
                    result = owner.dispatcher.dispatch(method, [])
                except (RuntimeError, TimeoutError, ValueError) as exc:
                    self._send_json(
                        {"ok": False, "error": str(exc)}, HTTPStatus.SERVICE_UNAVAILABLE
                    )
                    return
                self._send_bytes(str(result).encode("utf-8"), content_type)

            def _authorized(self) -> bool:
                return owner._basic_authorized(self.headers.get("Authorization", "")) and secrets.compare_digest(
                    self.headers.get("X-Network-Session", ""), owner.token
                )

            def _serve_static(self, request_path: str) -> None:
                if not owner._basic_authorized(self.headers.get("Authorization", "")):
                    self._request_basic_auth()
                    return
                relative = unquote(request_path).lstrip("/") or "index.html"
                candidate = (owner.web_root / relative).resolve()
                try:
                    candidate.relative_to(owner.web_root)
                except ValueError:
                    self.send_error(HTTPStatus.NOT_FOUND)
                    return
                if not candidate.is_file():
                    self.send_error(HTTPStatus.NOT_FOUND)
                    return
                try:
                    body = candidate.read_bytes()
                    if candidate.name == "index.html":
                        body = body.replace(b"__SESSION_TOKEN__", owner.token.encode("ascii"))
                except OSError:
                    self.send_error(HTTPStatus.INTERNAL_SERVER_ERROR)
                    return
                content_type = mimetypes.guess_type(candidate.name)[0] or "application/octet-stream"
                if content_type.startswith("text/") or content_type in {
                    "application/javascript",
                    "image/svg+xml",
                }:
                    content_type += "; charset=utf-8"
                self._send_bytes(body, content_type)

            def _send_json(
                self, payload: object, status: HTTPStatus = HTTPStatus.OK
            ) -> None:
                self._send_bytes(
                    json.dumps(payload, ensure_ascii=False).encode("utf-8"),
                    "application/json; charset=utf-8",
                    status,
                )

            def _send_bytes(
                self,
                body: bytes,
                content_type: str,
                status: HTTPStatus = HTTPStatus.OK,
            ) -> None:
                self.send_response(status)
                self.send_header("Content-Type", content_type)
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Cache-Control", "no-store")
                self.send_header("X-Content-Type-Options", "nosniff")
                self.send_header("X-Frame-Options", "DENY")
                self.send_header("Referrer-Policy", "no-referrer")
                self.send_header(
                    "Content-Security-Policy",
                    "default-src 'self'; style-src 'self' 'unsafe-inline'; "
                    "img-src 'self'; connect-src 'self'",
                )
                self.end_headers()
                try:
                    self.wfile.write(body)
                except (BrokenPipeError, ConnectionAbortedError, ConnectionResetError):
                    # Browser tabs can close while a polling response is in flight.
                    return

            def _request_basic_auth(self) -> None:
                body = "需要 WebGUI 管理凭据".encode("utf-8")
                self.send_response(HTTPStatus.UNAUTHORIZED)
                self.send_header("WWW-Authenticate", 'Basic realm="Network Manager"')
                self.send_header("Content-Type", "text/plain; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Cache-Control", "no-store")
                self.end_headers()
                self.wfile.write(body)

            def log_message(self, _format: str, *_args: object) -> None:
                return

        self._httpd = ThreadingHTTPServer((self.host, self.port), RequestHandler)
        self._httpd.daemon_threads = True
        self._thread = threading.Thread(
            target=self._httpd.serve_forever,
            name="local-webgui",
            daemon=True,
        )
        self._thread.start()
        return self.url

    def close(self) -> None:
        server = self._httpd
        self._httpd = None
        if server is None:
            return
        server.shutdown()
        server.server_close()
        thread = self._thread
        self._thread = None
        if thread is not None and thread is not threading.current_thread():
            thread.join(timeout=2)

    def _basic_authorized(self, authorization: str) -> bool:
        if not self.access_password:
            return True
        if not authorization.startswith("Basic "):
            return False
        try:
            decoded = base64.b64decode(authorization[6:], validate=True).decode("utf-8")
            username, password = decoded.split(":", 1)
        except (ValueError, UnicodeDecodeError):
            return False
        return secrets.compare_digest(username, self.access_username) and secrets.compare_digest(
            password, self.access_password
        )


def _is_loopback_host(host: str) -> bool:
    return host.lower() in {"127.0.0.1", "localhost", "::1"}
