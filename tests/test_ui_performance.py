from __future__ import annotations

from threading import Lock
import time
from types import SimpleNamespace

from network_manager.ui.main_window import MainWindow
from network_manager.ui.web_window import WebBridge


class _CoreWithoutSynchronousHealth:
    is_running = True

    @property
    def is_healthy(self) -> bool:
        raise AssertionError("GUI polling must not perform a synchronous health probe")

    @staticmethod
    def drain_logs() -> list[str]:
        return []


class _StatusBar:
    @staticmethod
    def showMessage(_message: str, _timeout: int) -> None:
        return


def test_gui_poll_uses_cached_controller_health() -> None:
    window = SimpleNamespace(
        core=_CoreWithoutSynchronousHealth(),
        _last_running=True,
        _operation_active=False,
        _core_desired_running=True,
        _next_auto_recovery_at=0.0,
        _controller_healthy=True,
        _core_unhealthy_polls=2,
        _auto_recovery_attempts=0,
        _last_status_signature=None,
        statusBar=lambda: _StatusBar(),
        _update_status=lambda: None,
    )

    MainWindow._poll_core(window)

    assert window._core_unhealthy_polls == 0


def test_port_probe_is_dispatched_without_blocking_gui(monkeypatch) -> None:
    probed: list[tuple[str, int]] = []
    task: dict[str, object] = {}

    def fake_probe(host: str, port: int) -> bool:
        probed.append((host, port))
        return True

    def capture_task(function, success, _error_title, finished, failed) -> None:
        task.update(function=function, success=success, finished=finished, failed=failed)

    monkeypatch.setattr("network_manager.ui.main_window.port_is_open", fake_probe)
    window = SimpleNamespace(
        _port_probe_active=False,
        config=SimpleNamespace(
            clash=SimpleNamespace(enabled=True, host="127.0.0.1", port=17897),
            v2ray=SimpleNamespace(enabled=True, host="127.0.0.1", port=10808),
        ),
        _run_task=capture_task,
    )

    MainWindow._refresh_port_status(window)

    assert window._port_probe_active is True
    assert probed == []
    result = task["function"]()
    assert result["clash"][2:] == ("正在监听", "ok")
    assert probed == [("127.0.0.1", 17897), ("127.0.0.1", 10808)]
    task["finished"]()
    assert window._port_probe_active is False


class _DeferredFuture:
    def __init__(self) -> None:
        self.callback = None
        self.value: list[str] = []

    def add_done_callback(self, callback) -> None:
        self.callback = callback

    def result(self) -> list[str]:
        return self.value

    def complete(self, value: list[str]) -> None:
        self.value = value
        assert self.callback is not None
        self.callback(self)


class _DeferredExecutor:
    def __init__(self) -> None:
        self.future = _DeferredFuture()
        self.submitted = None

    def submit(self, function):
        self.submitted = function
        return self.future


def test_running_process_refresh_returns_cached_value_immediately() -> None:
    executor = _DeferredExecutor()
    bridge = SimpleNamespace(
        _process_cache_lock=Lock(),
        _running_process_cache=["cached.exe"],
        _running_process_cache_at=time.monotonic() - 20,
        _running_process_refresh_pending=False,
        _bridge_closed=False,
        _node_executor=executor,
        _scan_running_process_names=lambda: ["fresh.exe"],
    )
    bridge._running_process_names_finished = (
        lambda future: WebBridge._running_process_names_finished(bridge, future)
    )

    result = WebBridge._running_process_names(bridge)

    assert result == ["cached.exe"]
    assert bridge._running_process_refresh_pending is True
    assert executor.submitted is bridge._scan_running_process_names
    executor.future.complete(["fresh.exe"])
    assert bridge._running_process_refresh_pending is False
    assert bridge._running_process_cache == ["fresh.exe"]
