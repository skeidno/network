from __future__ import annotations

import ctypes
from ctypes import wintypes
import os
from pathlib import Path


class BrowserWindowController:
    """Own the native frame of the dedicated Edge WebGUI window on Windows."""

    def __init__(self, title: str, icon_path: Path) -> None:
        self.title = title
        self.icon_path = icon_path
        self.launch_pid = 0
        self.window_handle = 0
        self._icon_handles: list[int] = []

    @property
    def supported(self) -> bool:
        return os.name == "nt"

    def attach(self, launch_pid: int = 0) -> bool:
        if not self.supported:
            return False
        if launch_pid:
            self.launch_pid = launch_pid
        handle = self._find_window()
        if not handle:
            return False
        self.window_handle = handle
        self._apply_custom_frame(handle)
        self._apply_icon(handle)
        return True

    def show_existing(self) -> bool:
        if not self.supported:
            return False
        handle = self._find_window()
        if not handle:
            return False
        self.window_handle = handle
        if _user32.IsIconic(handle):
            _user32.ShowWindow(handle, SW_RESTORE)
        else:
            _user32.ShowWindow(handle, SW_SHOW)
        _user32.SetForegroundWindow(handle)
        return True

    def perform(self, action: str) -> bool:
        if not self.supported:
            return False
        handle = self._find_window()
        if not handle:
            return False
        self.window_handle = handle
        if action == "minimize":
            _user32.ShowWindow(handle, SW_MINIMIZE)
        elif action == "maximize":
            command = SW_RESTORE if _user32.IsZoomed(handle) else SW_MAXIMIZE
            _user32.ShowWindow(handle, command)
        elif action == "drag":
            if _user32.IsZoomed(handle):
                _user32.ShowWindow(handle, SW_RESTORE)
            _user32.ReleaseCapture()
            _user32.PostMessageW(handle, WM_NCLBUTTONDOWN, HTCAPTION, 0)
        elif action == "close":
            _user32.PostMessageW(handle, WM_CLOSE, 0, 0)
        else:
            return False
        return True

    def close(self) -> None:
        if self.supported and self.window_handle and _user32.IsWindow(self.window_handle):
            _user32.PostMessageW(self.window_handle, WM_CLOSE, 0, 0)
        self.window_handle = 0

    def _find_window(self) -> int:
        if self.window_handle and _user32.IsWindow(self.window_handle):
            return self.window_handle

        candidates: list[tuple[int, int]] = []

        @ENUM_WINDOWS_PROC
        def collect(handle: int, _parameter: int) -> bool:
            if not _user32.IsWindowVisible(handle):
                return True
            length = _user32.GetWindowTextLengthW(handle)
            if length < 1:
                return True
            text = ctypes.create_unicode_buffer(length + 1)
            _user32.GetWindowTextW(handle, text, length + 1)
            if text.value != self.title:
                return True
            class_name = ctypes.create_unicode_buffer(128)
            _user32.GetClassNameW(handle, class_name, len(class_name))
            if not class_name.value.startswith("Chrome_WidgetWin"):
                return True
            process_id = wintypes.DWORD()
            _user32.GetWindowThreadProcessId(handle, ctypes.byref(process_id))
            candidates.append((int(handle), int(process_id.value)))
            return True

        _user32.EnumWindows(collect, 0)
        if not candidates:
            return 0
        return next(
            (handle for handle, process_id in candidates if process_id == self.launch_pid),
            candidates[0][0],
        )

    def _apply_custom_frame(self, handle: int) -> None:
        style = _user32.GetWindowLongPtrW(handle, GWL_STYLE)
        style = (style & ~WS_CAPTION) | WS_THICKFRAME | WS_SYSMENU
        style |= WS_MINIMIZEBOX | WS_MAXIMIZEBOX
        _user32.SetWindowLongPtrW(handle, GWL_STYLE, style)
        _user32.SetWindowPos(
            handle,
            0,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
        )

    def _apply_icon(self, handle: int) -> None:
        if not self.icon_path.is_file():
            return
        for size, kind in ((16, ICON_SMALL), (32, ICON_BIG)):
            icon = _user32.LoadImageW(
                0,
                str(self.icon_path),
                IMAGE_ICON,
                size,
                size,
                LR_LOADFROMFILE,
            )
            if icon:
                self._icon_handles.append(int(icon))
                _user32.SendMessageW(handle, WM_SETICON, kind, icon)


if os.name == "nt":
    _user32 = ctypes.WinDLL("user32", use_last_error=True)
    LONG_PTR = ctypes.c_ssize_t
    ENUM_WINDOWS_PROC = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)

    _user32.EnumWindows.argtypes = [ENUM_WINDOWS_PROC, wintypes.LPARAM]
    _user32.EnumWindows.restype = wintypes.BOOL
    _user32.GetWindowLongPtrW.argtypes = [wintypes.HWND, ctypes.c_int]
    _user32.GetWindowLongPtrW.restype = LONG_PTR
    _user32.SetWindowLongPtrW.argtypes = [wintypes.HWND, ctypes.c_int, LONG_PTR]
    _user32.SetWindowLongPtrW.restype = LONG_PTR
    _user32.SetWindowPos.argtypes = [
        wintypes.HWND,
        wintypes.HWND,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_int,
        wintypes.UINT,
    ]
    _user32.LoadImageW.restype = wintypes.HANDLE
else:  # pragma: no cover - exercised by macOS/Linux packages
    _user32 = None

    def ENUM_WINDOWS_PROC(callback):  # type: ignore[no-redef]
        return callback


GWL_STYLE = -16
WS_CAPTION = 0x00C00000
WS_THICKFRAME = 0x00040000
WS_SYSMENU = 0x00080000
WS_MINIMIZEBOX = 0x00020000
WS_MAXIMIZEBOX = 0x00010000
SW_SHOW = 5
SW_MINIMIZE = 6
SW_MAXIMIZE = 3
SW_RESTORE = 9
SWP_NOSIZE = 0x0001
SWP_NOMOVE = 0x0002
SWP_NOZORDER = 0x0004
SWP_NOACTIVATE = 0x0010
SWP_FRAMECHANGED = 0x0020
WM_CLOSE = 0x0010
WM_SETICON = 0x0080
WM_NCLBUTTONDOWN = 0x00A1
HTCAPTION = 2
ICON_SMALL = 0
ICON_BIG = 1
IMAGE_ICON = 1
LR_LOADFROMFILE = 0x0010
