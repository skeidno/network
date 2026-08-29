from __future__ import annotations

import sys
from pathlib import Path

from PySide6.QtCore import QLockFile
from PySide6.QtGui import QIcon
from PySide6.QtWidgets import QApplication, QMessageBox, QStyleFactory

from network_manager.paths import app_data_dir
from network_manager.ui.web_window import WebMainWindow as MainWindow


def _load_style() -> str:
    path = Path(__file__).with_name("style.qss")
    try:
        return path.read_text(encoding="utf-8")
    except OSError:
        return ""


def main() -> int:
    app = QApplication(sys.argv)
    app.setApplicationName("NetWorkManger")
    app.setOrganizationName("NetWorkManger")
    icon_path = Path(__file__).with_name("web") / "icons" / "network-manager.ico"
    app.setWindowIcon(QIcon(str(icon_path)))
    app.setQuitOnLastWindowClosed(False)
    app.setStyle(QStyleFactory.create("Fusion"))
    app.setStyleSheet(_load_style())

    lock = QLockFile(str(app_data_dir() / "instance.lock"))
    lock.setStaleLockTime(0)
    if not lock.tryLock(100):
        QMessageBox.information(None, "Network Manager", "程序已经在运行，请查看系统托盘。")
        return 0

    startup_launch = "--startup" in sys.argv
    window = MainWindow(startup_launch=startup_launch)
    app.aboutToQuit.connect(window.shutdown)
    if not startup_launch:
        window.open_web_ui()
    exit_code = app.exec()
    lock.unlock()
    return exit_code


if __name__ == "__main__":
    raise SystemExit(main())
