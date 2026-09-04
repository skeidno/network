from __future__ import annotations

import json
import threading
import time

from PySide6.QtCore import QCoreApplication, QObject
import requests

from network_manager.local_web_server import LocalWebServer


class _Bridge(QObject):
    def __init__(self) -> None:
        super().__init__()
        self.value = "initial"

    def getState(self) -> str:
        return json.dumps({"value": self.value})

    def getLogs(self) -> str:
        return "one\ntwo"

    def setValue(self, value: str) -> None:
        self.value = value


def _run_with_events(callback):
    app = QCoreApplication.instance() or QCoreApplication([])
    result: dict[str, object] = {}

    def worker() -> None:
        try:
            result["value"] = callback()
        except Exception as exc:  # pragma: no cover - surfaced by assertion
            result["error"] = exc

    thread = threading.Thread(target=worker)
    thread.start()
    deadline = time.monotonic() + 10
    while thread.is_alive() and time.monotonic() < deadline:
        app.processEvents()
        time.sleep(0.005)
    thread.join(timeout=1)
    assert not thread.is_alive()
    if "error" in result:
        raise result["error"]  # type: ignore[misc]
    return result.get("value")


def test_loopback_web_server_auth_and_bridge(tmp_path) -> None:
    web_root = tmp_path / "web"
    web_root.mkdir()
    (web_root / "index.html").write_text(
        '<meta name="network-session-token" content="__SESSION_TOKEN__">',
        encoding="utf-8",
    )
    bridge = _Bridge()
    server = LocalWebServer(web_root, bridge, {"getState", "getLogs", "setValue"})
    server.start()
    headers = {"X-Network-Session": server.token}
    try:
        page = requests.get(server.url, timeout=3)
        assert page.status_code == 200
        assert page.raw.version == 11
        assert server.token in page.text
        assert "__SESSION_TOKEN__" not in page.text

        unauthorized = requests.get(server.url + "api/state", timeout=3)
        assert unauthorized.status_code == 403

        state = _run_with_events(
            lambda: requests.get(server.url + "api/state", headers=headers, timeout=3)
        )
        assert state.json() == {"value": "initial"}

        response = _run_with_events(
            lambda: requests.post(
                server.url + "api/call",
                headers=headers,
                json={"method": "setValue", "args": ["updated"]},
                timeout=3,
            )
        )
        assert response.json()["ok"] is True
        assert bridge.value == "updated"
    finally:
        server.close()
