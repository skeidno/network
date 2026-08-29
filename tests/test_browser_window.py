from pathlib import Path

from network_manager.browser_window import BrowserWindowController


def test_browser_window_controller_ignores_unknown_window(tmp_path: Path) -> None:
    controller = BrowserWindowController(
        "Network Manager test window that does not exist",
        tmp_path / "missing.ico",
    )

    assert controller.attach() is False
    assert controller.show_existing() is False
    assert controller.perform("unknown") is False
