from __future__ import annotations

import time
from dataclasses import dataclass
from typing import Any


@dataclass(frozen=True, slots=True)
class TrafficSnapshot:
    download_total: int
    upload_total: int
    active_connections: int


@dataclass(frozen=True, slots=True)
class TrafficSample:
    download_rate: float
    upload_rate: float
    download_total: int
    upload_total: int
    active_connections: int


def _non_negative_int(value: Any) -> int:
    if isinstance(value, bool):
        return 0
    try:
        return max(0, int(value))
    except (TypeError, ValueError):
        return 0


def parse_connection_snapshot(payload: object) -> TrafficSnapshot:
    if not isinstance(payload, dict):
        raise ValueError("流量统计响应不是对象")
    connections = payload.get("connections")
    return TrafficSnapshot(
        download_total=_non_negative_int(payload.get("downloadTotal")),
        upload_total=_non_negative_int(payload.get("uploadTotal")),
        active_connections=len(connections) if isinstance(connections, list) else 0,
    )


class TrafficRateTracker:
    def __init__(self) -> None:
        self._previous: TrafficSnapshot | None = None
        self._previous_at: float | None = None

    def reset(self) -> None:
        self._previous = None
        self._previous_at = None

    def update(
        self, snapshot: TrafficSnapshot, timestamp: float | None = None
    ) -> TrafficSample:
        sampled_at = time.monotonic() if timestamp is None else timestamp
        download_rate = 0.0
        upload_rate = 0.0
        if self._previous is not None and self._previous_at is not None:
            elapsed = sampled_at - self._previous_at
            if elapsed > 0:
                download_delta = snapshot.download_total - self._previous.download_total
                upload_delta = snapshot.upload_total - self._previous.upload_total
                if download_delta >= 0:
                    download_rate = download_delta / elapsed
                if upload_delta >= 0:
                    upload_rate = upload_delta / elapsed
        self._previous = snapshot
        self._previous_at = sampled_at
        return TrafficSample(
            download_rate=download_rate,
            upload_rate=upload_rate,
            download_total=snapshot.download_total,
            upload_total=snapshot.upload_total,
            active_connections=snapshot.active_connections,
        )


def format_bytes(value: float | int) -> str:
    amount = max(0.0, float(value))
    units = ("B", "KB", "MB", "GB", "TB")
    unit_index = 0
    while amount >= 1024 and unit_index < len(units) - 1:
        amount /= 1024
        unit_index += 1
    if unit_index == 0 or amount >= 100:
        return f"{amount:.0f} {units[unit_index]}"
    return f"{amount:.1f} {units[unit_index]}"


def format_rate(value: float | int) -> str:
    return f"{format_bytes(value)}/s"
