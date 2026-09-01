from __future__ import annotations

from concurrent.futures import Future
from threading import Lock
from types import SimpleNamespace

from network_manager.importers import prepare_imported_nodes
from network_manager.models import default_config
from network_manager.ui.web_window import WebBridge


def test_individual_node_tests_complete_independently() -> None:
    notifications: list[tuple[str, str]] = []
    bridge = SimpleNamespace(
        _node_delay_lock=Lock(),
        _node_delays={
            "One": {"status": "testing", "delay": None},
            "Two": {"status": "testing", "delay": None},
        },
        _node_test_pending={"One": 101, "Two": 102},
        _node_batch_generation=None,
        _node_batch_pending=set(),
        _notify=lambda level, message: notifications.append((level, message)),
    )
    second: Future[dict[str, object]] = Future()
    second.set_result({"status": "ok", "delay": 86})
    first: Future[dict[str, object]] = Future()
    first.set_result({"status": "ok", "delay": 104})

    WebBridge._node_delay_finished(bridge, 102, "Two", second)
    WebBridge._node_delay_finished(bridge, 101, "One", first)

    assert bridge._node_test_pending == {}
    assert bridge._node_delays["One"] == {"status": "ok", "delay": 104}
    assert bridge._node_delays["Two"] == {"status": "ok", "delay": 86}
    assert notifications == [("success", "Two：86 ms"), ("success", "One：104 ms")]


def test_failed_node_test_surfaces_actionable_diagnostic() -> None:
    notifications: list[tuple[str, str]] = []
    bridge = SimpleNamespace(
        _node_delay_lock=Lock(),
        _node_delays={"Proxy": {"status": "testing", "delay": None}},
        _node_test_pending={"Proxy": 7},
        _node_batch_generation=None,
        _node_batch_pending=set(),
        _notify=lambda level, message: notifications.append((level, message)),
    )
    result: Future[dict[str, object]] = Future()
    result.set_result(
        {
            "status": "error",
            "delay": None,
            "message": "代理供应商拒绝当前公网 IP，请添加白名单",
        }
    )

    WebBridge._node_delay_finished(bridge, 7, "Proxy", result)

    assert notifications == [
        ("error", "Proxy：代理供应商拒绝当前公网 IP，请添加白名单")
    ]


def test_delete_error_nodes_preserves_subscription_and_selects_remaining_node() -> None:
    config = default_config()
    config.imported_nodes = prepare_imported_nodes(
        [
            {"name": "Fast", "type": "socks5", "server": "127.0.0.1", "port": 1001},
            {"name": "Broken", "type": "socks5", "server": "127.0.0.1", "port": 1002},
        ],
        "subscription",
        source_id="source-one",
    )
    config.selected_node = "Broken"
    config.mode = "SMART"
    subscription_marker = object()
    config.subscriptions = [subscription_marker]  # type: ignore[list-item]
    applied: list[str] = []
    notifications: list[tuple[str, str]] = []
    window = SimpleNamespace(
        config=config,
        _save_and_apply=lambda message: applied.append(message),
        _refresh_nodes_table=lambda: None,
        _refresh_subscriptions_table=lambda: None,
    )
    bridge = SimpleNamespace(
        window=window,
        _node_delay_lock=Lock(),
        _node_test_pending={},
        _node_delays={
            "Fast": {"status": "ok", "delay": 80},
            "Broken": {"status": "error", "delay": None},
        },
        _notify=lambda level, message: notifications.append((level, message)),
    )

    WebBridge.deleteErrorNodes(bridge)

    assert [node.name for node in config.imported_nodes] == ["Fast"]
    assert config.selected_node == "Fast"
    assert config.mode == "SMART"
    assert config.subscriptions == [subscription_marker]
    assert bridge._node_delays == {"Fast": {"status": "ok", "delay": 80}}
    assert applied == ["Error 节点已批量删除"]
    assert notifications[-1] == (
        "success",
        "已删除 1 个 Error 节点；刷新原订阅可恢复",
    )
