from __future__ import annotations

import os
import queue
import socket
import subprocess
import threading
import time
from pathlib import Path

import requests
import yaml

from network_manager.windows_job import WindowsJob


CREATE_NO_WINDOW = 0x08000000 if os.name == "nt" else 0


class CoreStartError(RuntimeError):
    pass


class CoreManager:
    def __init__(self, executable: Path, work_dir: Path, controller_port: int) -> None:
        self.executable = executable
        self.work_dir = work_dir
        self.controller_port = controller_port
        self.process: subprocess.Popen[str] | None = None
        self._logs: queue.Queue[str] = queue.Queue(maxsize=3000)
        self._reader: threading.Thread | None = None
        self._job: WindowsJob | None = None
        self._operation_lock = threading.RLock()
        self._tun_ready = threading.Event()

    @property
    def is_running(self) -> bool:
        return self.process is not None and self.process.poll() is None

    @property
    def is_healthy(self) -> bool:
        return self.is_running and _port_is_open("127.0.0.1", self.controller_port, 0.2)

    @property
    def exit_code(self) -> int | None:
        return self.process.poll() if self.process is not None else None

    def validate(self, config_path: Path, timeout: float = 15.0) -> tuple[bool, str]:
        if not self.executable.is_file():
            return False, f"找不到内置核心：{self.executable}"
        command = [
            str(self.executable),
            "-t",
            "-d",
            str(self.work_dir),
            "-f",
            str(config_path),
        ]
        try:
            result = subprocess.run(
                command,
                capture_output=True,
                text=True,
                encoding="utf-8",
                errors="replace",
                timeout=timeout,
                creationflags=CREATE_NO_WINDOW,
            )
        except (OSError, subprocess.TimeoutExpired) as exc:
            return False, f"核心配置检查失败：{exc}"
        output = "\n".join(part.strip() for part in (result.stdout, result.stderr) if part.strip())
        return result.returncode == 0, output

    def start(self, config_path: Path, timeout: float = 35.0) -> None:
        with self._operation_lock:
            if self.is_running:
                return
            valid, output = self.validate(config_path)
            if not valid:
                raise CoreStartError(output or "Mihomo 配置检查未通过")
            self.work_dir.mkdir(parents=True, exist_ok=True)
            command = [
                str(self.executable),
                "-d",
                str(self.work_dir),
                "-f",
                str(config_path),
            ]
            self._tun_ready.clear()
            tun_required = _config_uses_tun(config_path)
            try:
                self.process = subprocess.Popen(
                    command,
                    stdout=subprocess.PIPE,
                    stderr=subprocess.STDOUT,
                    text=True,
                    encoding="utf-8",
                    errors="replace",
                    bufsize=1,
                    creationflags=CREATE_NO_WINDOW,
                )
            except OSError as exc:
                self.process = None
                raise CoreStartError(f"无法启动 Mihomo：{exc}") from exc

            self._job = WindowsJob()
            process_handle = getattr(self.process, "_handle", None)
            if process_handle is not None:
                self._job.assign(process_handle)
            self._reader = threading.Thread(target=self._read_output, daemon=True)
            self._reader.start()

            deadline = time.monotonic() + timeout
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    logs = "\n".join(self.drain_logs(80))
                    self.stop()
                    raise CoreStartError(logs or "Mihomo 启动后立即退出")
                controller_ready = _port_is_open("127.0.0.1", self.controller_port, 0.15)
                if controller_ready and (not tun_required or self._tun_ready.is_set()):
                    self._put_log("[Manager] TUN 核心已就绪")
                    return
                time.sleep(0.1)
            self.stop()
            if tun_required and not self._tun_ready.is_set():
                raise CoreStartError("Mihomo 启动超时，TUN 网卡没有就绪")
            raise CoreStartError("Mihomo 启动超时，控制端口没有就绪")

    def restart(self, config_path: Path) -> None:
        with self._operation_lock:
            self.stop()
            self.start(config_path)

    def reload(self, config_path: Path, secret: str, timeout: float = 20.0) -> None:
        with self._operation_lock:
            if not self.is_running:
                raise CoreStartError("Mihomo 核心未运行，无法热加载配置")
            valid, output = self.validate(config_path)
            if not valid:
                raise CoreStartError(output or "Mihomo 配置检查未通过")
            try:
                response = requests.put(
                    f"http://127.0.0.1:{self.controller_port}/configs",
                    headers={"Authorization": f"Bearer {secret}"},
                    params={"force": "true"},
                    json={"path": str(config_path.resolve())},
                    timeout=(3, timeout),
                )
                response.raise_for_status()
            except requests.RequestException as exc:
                raise CoreStartError(f"Mihomo 热加载失败：{exc}") from exc
            if not self.is_healthy:
                raise CoreStartError("配置已提交，但 Mihomo 控制端口未恢复")
            self._put_log("[Manager] 配置已热加载，TUN 网卡保持运行")

    def stop(self) -> None:
        with self._operation_lock:
            process = self.process
            self.process = None
            if process is not None and process.poll() is None:
                try:
                    process.terminate()
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    process.kill()
                    process.wait(timeout=3)
                except OSError:
                    pass
            if self._job is not None:
                self._job.close()
                self._job = None
            self._tun_ready.clear()
            self._put_log("[Manager] TUN 核心已停止")

    def drain_logs(self, limit: int = 300) -> list[str]:
        lines: list[str] = []
        for _ in range(limit):
            try:
                lines.append(self._logs.get_nowait())
            except queue.Empty:
                break
        return lines

    def log_event(self, line: str) -> None:
        self._put_log(f"[Manager] {line}")

    def _read_output(self) -> None:
        process = self.process
        if process is None or process.stdout is None:
            return
        for line in process.stdout:
            clean = line.rstrip()
            if "[TUN] Tun adapter listening at:" in clean:
                self._tun_ready.set()
            self._put_log(clean)

    def _put_log(self, line: str) -> None:
        if not line:
            return
        try:
            self._logs.put_nowait(line)
        except queue.Full:
            try:
                self._logs.get_nowait()
            except queue.Empty:
                pass
            try:
                self._logs.put_nowait(line)
            except queue.Full:
                pass


def _port_is_open(host: str, port: int, timeout: float) -> bool:
    try:
        with socket.create_connection((host, port), timeout=timeout):
            return True
    except OSError:
        return False


def _config_uses_tun(config_path: Path) -> bool:
    try:
        payload = yaml.safe_load(config_path.read_text(encoding="utf-8"))
    except (OSError, UnicodeError, yaml.YAMLError):
        return True
    if not isinstance(payload, dict):
        return True
    return bool(payload.get("tun", {}).get("enable", False))
