from __future__ import annotations

import base64
import ctypes
from ctypes import wintypes
import json
import os
from pathlib import Path


class CredentialStoreError(RuntimeError):
    pass


if os.name == "nt":
    class _DataBlob(ctypes.Structure):
        _fields_ = [("cbData", wintypes.DWORD), ("pbData", ctypes.POINTER(ctypes.c_byte))]


def _protect_windows(value: str) -> str:
    raw = value.encode("utf-8")
    buffer = ctypes.create_string_buffer(raw)
    source = _DataBlob(len(raw), ctypes.cast(buffer, ctypes.POINTER(ctypes.c_byte)))
    protected = _DataBlob()
    crypt32 = ctypes.windll.crypt32
    if not crypt32.CryptProtectData(
        ctypes.byref(source),
        "Network Manager SSH",
        None,
        None,
        None,
        0,
        ctypes.byref(protected),
    ):
        raise CredentialStoreError("Windows 凭据加密失败")
    try:
        payload = ctypes.string_at(protected.pbData, protected.cbData)
        return base64.b64encode(payload).decode("ascii")
    finally:
        ctypes.windll.kernel32.LocalFree(protected.pbData)


def _unprotect_windows(value: str) -> str:
    try:
        raw = base64.b64decode(value, validate=True)
    except (ValueError, TypeError) as exc:
        raise CredentialStoreError("SSH 凭据内容损坏") from exc
    buffer = ctypes.create_string_buffer(raw)
    source = _DataBlob(len(raw), ctypes.cast(buffer, ctypes.POINTER(ctypes.c_byte)))
    clear = _DataBlob()
    crypt32 = ctypes.windll.crypt32
    if not crypt32.CryptUnprotectData(
        ctypes.byref(source), None, None, None, None, 0, ctypes.byref(clear)
    ):
        raise CredentialStoreError("Windows 凭据解密失败")
    try:
        return ctypes.string_at(clear.pbData, clear.cbData).decode("utf-8")
    finally:
        ctypes.windll.kernel32.LocalFree(clear.pbData)


class CredentialStore:
    """Store SSH secrets encrypted for the current Windows user."""

    def __init__(self, path: Path) -> None:
        self.path = path

    @property
    def can_persist(self) -> bool:
        return os.name == "nt"

    def set(self, profile_id: str, password: str) -> None:
        if not self.can_persist:
            raise CredentialStoreError("当前系统暂不支持保存密码，请使用 SSH 密钥")
        if not password:
            # An empty value must never erase a stored secret by accident. Removing a
            # credential is an explicit decision, so callers have to use delete().
            return
        values = self._load()
        values[profile_id] = _protect_windows(password)
        self._save(values)

    def get(self, profile_id: str) -> str:
        if not self.can_persist:
            return ""
        protected = self._load().get(profile_id)
        if not protected:
            return ""
        return _unprotect_windows(protected)

    def has(self, profile_id: str) -> bool:
        if not self.can_persist:
            return False
        return bool(self._load().get(profile_id))

    def delete(self, profile_id: str) -> None:
        values = self._load()
        if profile_id in values:
            values.pop(profile_id, None)
            self._save(values)

    def _load(self) -> dict[str, str]:
        """Read the encrypted store.

        A file that exists but cannot be parsed is treated as an error instead of an
        empty mapping: callers write back whatever they loaded, so silently returning
        ``{}`` here would drop every other saved password on the next save.
        """
        try:
            payload = json.loads(self.path.read_text(encoding="utf-8"))
        except FileNotFoundError:
            return {}
        except (OSError, ValueError, TypeError, json.JSONDecodeError) as exc:
            self._quarantine()
            raise CredentialStoreError(
                "SSH 凭据文件无法读取，已备份为 .corrupt 文件，请重新输入密码"
            ) from exc
        if not isinstance(payload, dict):
            self._quarantine()
            raise CredentialStoreError(
                "SSH 凭据文件格式异常，已备份为 .corrupt 文件，请重新输入密码"
            )
        return {str(key): str(value) for key, value in payload.items()}

    def _quarantine(self) -> None:
        """Move an unreadable store aside so nothing overwrites it."""
        try:
            self.path.replace(self.path.with_suffix(self.path.suffix + ".corrupt"))
        except OSError:
            pass

    def _save(self, values: dict[str, str]) -> None:
        self.path.parent.mkdir(parents=True, exist_ok=True)
        temporary = self.path.with_suffix(".tmp")
        temporary.write_text(
            json.dumps(values, ensure_ascii=True, indent=2) + "\n", encoding="utf-8"
        )
        temporary.replace(self.path)
