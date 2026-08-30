from network_manager.ui.main_window import MainWindow


class FakeCore:
    def __init__(self, running: bool) -> None:
        self.is_running = running


class FakeWindow:
    def __init__(self, *, busy: bool, running: bool) -> None:
        self._operation_active = busy
        self.core = FakeCore(running)
        self.started = 0
        self.stopped = 0

    def start_core(self) -> None:
        self.started += 1
        self._operation_active = True

    def stop_core(self) -> None:
        self.stopped += 1
        self._operation_active = True


def test_toggle_core_rejects_a_second_request_while_busy() -> None:
    window = FakeWindow(busy=True, running=False)

    assert MainWindow.toggle_core(window) is False
    assert window.started == 0
    assert window.stopped == 0


def test_toggle_core_accepts_only_one_start_request() -> None:
    window = FakeWindow(busy=False, running=False)

    assert MainWindow.toggle_core(window) is True
    assert MainWindow.toggle_core(window) is False
    assert window.started == 1

