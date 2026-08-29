from __future__ import annotations

import pytest

from network_manager.traffic_monitor import (
    TrafficRateTracker,
    TrafficSnapshot,
    format_bytes,
    format_rate,
    parse_connection_snapshot,
)


def test_parse_connection_snapshot() -> None:
    snapshot = parse_connection_snapshot(
        {
            "downloadTotal": 4096,
            "uploadTotal": "2048",
            "connections": [{"id": "one"}, {"id": "two"}],
        }
    )
    assert snapshot == TrafficSnapshot(4096, 2048, 2)


def test_parse_connection_snapshot_rejects_non_object() -> None:
    with pytest.raises(ValueError, match="流量统计响应不是对象"):
        parse_connection_snapshot([])


def test_rate_tracker_calculates_deltas_and_handles_counter_reset() -> None:
    tracker = TrafficRateTracker()
    first = tracker.update(TrafficSnapshot(1000, 500, 1), timestamp=10.0)
    second = tracker.update(TrafficSnapshot(3048, 1524, 3), timestamp=12.0)
    reset = tracker.update(TrafficSnapshot(100, 50, 0), timestamp=13.0)
    after_reset = tracker.update(TrafficSnapshot(1124, 562, 2), timestamp=14.0)

    assert first.download_rate == 0
    assert first.upload_rate == 0
    assert second.download_rate == 1024
    assert second.upload_rate == 512
    assert second.active_connections == 3
    assert reset.download_rate == 0
    assert reset.upload_rate == 0
    assert after_reset.download_rate == 1024
    assert after_reset.upload_rate == 512


def test_traffic_units() -> None:
    assert format_bytes(0) == "0 B"
    assert format_bytes(1536) == "1.5 KB"
    assert format_rate(1024 * 1024) == "1.0 MB/s"
