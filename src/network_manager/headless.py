from __future__ import annotations

import argparse
from collections import deque
from concurrent.futures import Future, ThreadPoolExecutor
from dataclasses import replace
from datetime import datetime, timezone
import json
import os
from pathlib import Path
import signal
import threading
import time
from typing import Any
from urllib.parse import quote, urlsplit

import psutil
import requests

from network_manager import __version__
from network_manager.config_store import ConfigStore
from network_manager.core_manager import CoreManager
from network_manager.importers import (
    ImportContentError,
    fetch_subscription,
    parse_import_content,
    prepare_imported_nodes,
    subscription_from_url,
)
from network_manager.local_web_server import LocalWebServer
from network_manager.mihomo_config import write_mihomo_config
from network_manager.models import (
    COMMON_OVERSEAS_GROUP,
    DEFAULT_PROXY_DOMAINS,
    NODE_DIALER_POLICY_KEY,
    NODE_DIALER_PROXY_KEY,
    AppConfig,
    RoutingRule,
    SubscriptionSource,
    Upstream,
    apply_automatic_node_dialers,
    clear_node_dialer_references,
    common_overseas_rules_from_values,
    is_common_overseas_rule,
    normalize_node_group_name,
    normalize_proxy_endpoint_host,
    normalize_rule_value,
    routing_rules_from_values,
    validate_config,
)
from network_manager.network_probe import (
    diagnose_authenticated_proxy,
    exit_ip_through_proxy,
    port_is_open,
    proxy_url,
    test_upstream,
)
from network_manager.paths import app_data_dir, bundle_root, core_path, generated_config_path
from network_manager.portable_config import export_portable_config, import_portable_config
from network_manager.traffic_monitor import (
    TrafficRateTracker,
    format_bytes,
    format_rate,
    parse_connection_snapshot,
)


MODE_LABELS = {
    "RULE": "规则分流",
    "GLOBAL_CLASH": "全局 Clash",
    "GLOBAL_V2RAY": "全局 v2ray",
    "GLOBAL_BUILTIN": "全局节点",
    "SMART": "智能节点",
    "DIRECT": "全局直连",
}
RULE_LABELS = {
    "PROCESS-NAME": "程序",
    "DOMAIN": "完整域名",
    "DOMAIN-SUFFIX": "域名后缀",
    "DOMAIN-KEYWORD": "域名关键字",
    "IP-CIDR": "IPv4 网段",
}
TARGET_LABELS = {
    "CLASH": "Clash 本地端口",
    "V2RAY": "v2ray 本地端口",
    "BUILTIN": "内置节点组",
    "DIRECT": "直连",
}
HEADLESS_METHODS = {
    "getState",
    "getLogs",
    "toggleCore",
    "setMode",
    "saveRule",
    "ruleAction",
    "saveRuleGroup",
    "ruleGroupAction",
    "setDefaultTarget",
    "saveSources",
    "saveSettings",
    "addSubscription",
    "importPaste",
    "importFileText",
    "exportPortableConfigText",
    "importPortableConfigText",
    "refreshSubscription",
    "refreshAllSubscriptions",
    "deleteSubscription",
    "deleteNode",
    "deleteErrorNodes",
    "createNodeGroup",
    "assignNodeGroup",
    "setNodeDialerProxy",
    "saveProxyRelayRules",
    "deleteNodeGroup",
    "selectNode",
    "testExit",
    "testSource",
    "testAllNodes",
    "testNode",
    "clearLogs",
    "openLogs",
    "windowAction",
}


class HeadlessController:
    def __init__(self, *, start_core: bool = False) -> None:
        self.store = ConfigStore(app_data_dir() / "settings.json")
        self.config = self.store.load()
        if _sanitize_headless_config(self.config):
            self.store.save(self.config)
        self.core = CoreManager(core_path(), app_data_dir() / "mihomo", self.config.controller_port)
        self._config_lock = threading.RLock()
        self._state_lock = threading.RLock()
        self._node_lock = threading.RLock()
        self._logs: deque[str] = deque(maxlen=3000)
        self._toasts: deque[dict[str, str]] = deque(maxlen=40)
        self._download_samples: deque[float] = deque([0.0] * 60, maxlen=60)
        self._upload_samples: deque[float] = deque([0.0] * 60, maxlen=60)
        self._traffic_tracker = TrafficRateTracker()
        self._traffic = {
            "downloadRate": "0 B/s",
            "uploadRate": "0 B/s",
            "downloadTotal": "0 B",
            "uploadTotal": "0 B",
            "connections": "0",
        }
        self._exit_ip = "尚未检测"
        self._busy = False
        self._importing = False
        self._desired_running = bool(start_core or self.config.start_on_launch)
        self._unhealthy_polls = 0
        self._recovery_attempts = 0
        self._next_recovery_at = 0.0
        self._last_monitor_running = False
        self._closed = False
        self._stop_event = threading.Event()
        self._operations = ThreadPoolExecutor(max_workers=4, thread_name_prefix="headless-op")
        self._node_executor = ThreadPoolExecutor(max_workers=4, thread_name_prefix="node-delay")
        self._node_delays: dict[str, dict[str, object]] = {}
        self._node_pending: set[str] = set()
        self._running_process_cache: list[str] = []
        self._running_process_cache_at = 0.0
        write_mihomo_config(self.config, generated_config_path())
        self._monitor = threading.Thread(
            target=self._monitor_loop, name="headless-monitor", daemon=True
        )
        self._monitor.start()
        if self._desired_running:
            self._queue_core_action("start")

    def getState(self) -> str:
        with self._config_lock, self._state_lock, self._node_lock:
            config = self.config
            running = self.core.is_running
            nodes = []
            for index, node in enumerate(config.imported_nodes):
                server = str(node.config.get("server", ""))
                port = node.config.get("port")
                delay = self._node_delays.get(node.name, {"status": "idle", "delay": None})
                is_server_deployment = node.source_id.startswith("server-deployment:")
                nodes.append(
                    {
                        "index": index,
                        "name": node.name,
                        "protocol": node.protocol,
                        "server": f"{server}:{port}" if port else server,
                        "source": node.source,
                        "group": node.group
                        or ("服务器部署" if is_server_deployment else node.source)
                        or "手动导入",
                        "customGroup": node.group,
                        "selected": node.name == config.selected_node,
                        "latencyStatus": delay.get("status", "idle"),
                        "latency": delay.get("delay"),
                        "latencyMessage": delay.get("message", ""),
                        "dialerProxy": node.dialer_proxy,
                        "dialerPolicy": node.dialer_policy,
                    }
                )
            subscriptions = [
                {
                    "index": index,
                    "name": source.name,
                    "host": urlsplit(source.url).netloc or "订阅地址",
                    "nodeCount": sum(
                        node.source_id == source.source_id for node in config.imported_nodes
                    ),
                    "group": source.group,
                    "lastUpdated": source.last_updated or "未更新",
                }
                for index, source in enumerate(config.subscriptions)
            ]
            toasts = list(self._toasts)
            self._toasts.clear()
            payload = {
                "capabilities": {
                    "platform": "linux",
                    "headless": True,
                    "sshDeployment": False,
                    "browserFiles": True,
                },
                "core": {
                    "running": running,
                    "busy": self._busy,
                    "status": "处理中" if self._busy else "接管中" if running else "已停止",
                    "admin": _has_tun_privilege(),
                    "mode": config.mode,
                    "modeLabel": MODE_LABELS.get(config.mode, config.mode),
                    "mixedPort": config.mixed_port,
                },
                "summary": {
                    "processRules": sum(
                        rule.enabled and rule.rule_type == "PROCESS-NAME"
                        for rule in config.rules
                    ),
                    "networkRules": sum(
                        rule.enabled and rule.rule_type != "PROCESS-NAME"
                        for rule in config.rules
                    ),
                    "nodes": len(config.imported_nodes),
                    "defaultTarget": TARGET_LABELS.get(
                        config.default_target, config.default_target
                    ),
                },
                "sources": {
                    "clash": self._source_state(config.clash),
                    "v2ray": self._source_state(config.v2ray),
                    "ssh": {
                        "enabled": False,
                        "endpoint": "Linux 版不提供",
                        "status": "不可用",
                        "available": False,
                    },
                },
                "traffic": {
                    "status": "实时更新" if running else "接管停止",
                    **self._traffic,
                    "downloadSamples": list(self._download_samples),
                    "uploadSamples": list(self._upload_samples),
                    "memoryMb": round(psutil.Process().memory_info().rss / 1024 / 1024),
                },
                "exitIp": self._exit_ip,
                "rules": self._rule_states(config.rules),
                "fallbackRule": {
                    "target": config.default_target,
                    "targetLabel": TARGET_LABELS.get(
                        config.default_target, config.default_target
                    ),
                },
                "nodes": nodes,
                "nodeGroups": list(config.node_groups),
                "subscriptions": subscriptions,
                "sshServers": [],
                "selectedNode": config.selected_node,
                "runningProcesses": self._running_process_names(),
                "importing": self._importing,
                "settings": {
                    "mixedPort": config.mixed_port,
                    "controllerPort": config.controller_port,
                    "dnsPort": config.dns_port,
                    "serverProxyPort": config.server_proxy_port,
                    "strictRoute": config.strict_route,
                    "startOnLaunch": config.start_on_launch,
                    "closeToTray": False,
                    "startWithWindows": False,
                },
                "version": __version__,
                "toasts": toasts,
            }
        return json.dumps(payload, ensure_ascii=False)

    def getLogs(self) -> str:
        with self._state_lock:
            return "\n".join(self._logs)

    def _running_process_names(self) -> list[str]:
        now = time.monotonic()
        if now - self._running_process_cache_at < 5:
            return list(self._running_process_cache)
        names: dict[str, str] = {}
        for process in psutil.process_iter(["name"]):
            try:
                name = str(process.info.get("name") or "").strip()
            except (psutil.NoSuchProcess, psutil.AccessDenied, psutil.ZombieProcess):
                continue
            if name:
                names.setdefault(name.casefold(), name)
        self._running_process_cache = sorted(names.values(), key=str.casefold)[:600]
        self._running_process_cache_at = now
        return list(self._running_process_cache)

    def toggleCore(self) -> bool:
        with self._state_lock:
            if self._busy:
                return False
            action = "stop" if self.core.is_running else "start"
            self._desired_running = action == "start"
        return self._queue_core_action(action)

    def setMode(self, mode: str) -> None:
        if mode not in MODE_LABELS:
            self._notify("error", "运行模式无效")
            return
        with self._config_lock:
            if mode in {"GLOBAL_BUILTIN", "SMART"} and not self.config.imported_nodes:
                self._notify("error", "请先导入至少一个节点")
                return
            self.config.mode = mode
            self._save_and_apply(f"已切换为{MODE_LABELS[mode]}")

    def saveRule(self, payload_json: str) -> None:
        try:
            payload = json.loads(payload_json)
            rule_type = str(payload["ruleType"])
            raw_values = payload.get("values")
            if raw_values is None:
                raw_values = str(payload["value"]).splitlines()
            rules = routing_rules_from_values(
                rule_type,
                raw_values,
                payload["target"],
                enabled=bool(payload.get("enabled", True)),
                note=str(payload.get("note", "")).strip(),
            )
            index = int(payload.get("index", -1))
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
            self._notify("error", str(exc) or "规则内容无效")
            return
        with self._config_lock:
            previous_rules = list(self.config.rules)
            if 0 <= index < len(self.config.rules):
                self.config.rules[index : index + 1] = rules
                message = f"规则已更新，共 {len(rules)} 条"
            else:
                self.config.rules.extend(rules)
                message = f"规则已添加，共 {len(rules)} 条"
            if not self._save_and_apply(message):
                self.config.rules = previous_rules

    def ruleAction(self, index: int, action: str) -> None:
        with self._config_lock:
            if not 0 <= index < len(self.config.rules):
                return
            if action == "toggle":
                self.config.rules[index].enabled = not self.config.rules[index].enabled
                message = "规则状态已更新"
            elif action == "delete":
                self.config.rules.pop(index)
                message = "规则已删除"
            elif action in {"up", "down"}:
                target = index + (-1 if action == "up" else 1)
                if not 0 <= target < len(self.config.rules):
                    return
                self.config.rules[index], self.config.rules[target] = (
                    self.config.rules[target],
                    self.config.rules[index],
                )
                message = "规则顺序已更新"
            else:
                return
            self._save_and_apply(message)

    def saveRuleGroup(self, payload_json: str) -> None:
        try:
            payload = json.loads(payload_json)
            if payload["groupId"] != COMMON_OVERSEAS_GROUP:
                raise ValueError("规则组无效")
            target = str(payload["target"])
            if target not in TARGET_LABELS:
                raise ValueError("规则组去向无效")
            values = payload.get("values")
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
            self._notify("error", str(exc) or "规则组内容无效")
            return
        with self._config_lock:
            members = self._common_rules()
            if not members:
                self._notify("error", "规则组已经不存在")
                return
            previous_rules = list(self.config.rules)
            if values is None:
                member_indexes = {index for index, _rule in members}
                self.config.rules = [
                    replace(rule, target=target) if index in member_indexes else rule
                    for index, rule in enumerate(self.config.rules)
                ]
                message = "常用海外站点去向已更新"
            else:
                labels = {
                    normalize_rule_value(rule.rule_type, rule.value): rule.note
                    for _index, rule in members
                    if rule.note
                }
                try:
                    replacements = common_overseas_rules_from_values(
                        values,
                        target,
                        enabled=all(rule.enabled for _index, rule in members),
                        existing_labels=labels,
                    )
                except ValueError as exc:
                    self._notify("error", str(exc))
                    return
                first_index = min(index for index, _rule in members)
                member_indexes = {index for index, _rule in members}
                remaining = [
                    rule
                    for index, rule in enumerate(self.config.rules)
                    if index not in member_indexes
                ]
                remaining[first_index:first_index] = replacements
                self.config.rules = remaining
                message = f"常用海外站点已更新，共 {len(replacements)} 条"
            if not self._save_and_apply(message):
                self.config.rules = previous_rules

    def ruleGroupAction(self, group_id: str, action: str) -> None:
        if group_id != COMMON_OVERSEAS_GROUP:
            return
        with self._config_lock:
            members = self._common_rules()
            if action == "toggle":
                enabled = not all(rule.enabled for _index, rule in members)
                for _index, rule in members:
                    rule.enabled = enabled
                message = "常用海外站点规则组状态已更新"
            elif action == "delete":
                indexes = {index for index, _rule in members}
                self.config.rules = [
                    rule for index, rule in enumerate(self.config.rules) if index not in indexes
                ]
                message = "常用海外站点规则组已删除"
            else:
                return
            self._save_and_apply(message)

    def setDefaultTarget(self, target: str) -> None:
        if target not in TARGET_LABELS:
            self._notify("error", "保底规则去向无效")
            return
        with self._config_lock:
            self.config.default_target = target
            self._save_and_apply("强制保底规则已更新")

    def saveSources(self, payload_json: str) -> None:
        try:
            payload = json.loads(payload_json)
            sources: dict[str, Upstream] = {}
            for key in ("clash", "v2ray"):
                item = payload[key]
                port = int(item["port"])
                protocol = str(item["protocol"])
                if protocol not in {"socks5", "http"} or not 1 <= port <= 65535:
                    raise ValueError("代理源协议或端口无效")
                sources[key] = Upstream(
                    name="Clash 7897" if key == "clash" else "v2ray 10808",
                    host=str(item["host"]).strip(),
                    port=port,
                    protocol=protocol,
                    enabled=bool(item.get("enabled", True)),
                )
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
            self._notify("error", str(exc) or "代理源设置无效")
            return
        with self._config_lock:
            self.config.clash = sources["clash"]
            self.config.v2ray = sources["v2ray"]
            self._save_and_apply("本地代理源已保存")

    def saveSettings(self, payload_json: str) -> None:
        try:
            payload = json.loads(payload_json)
            candidate = AppConfig.from_dict(self.config.to_dict())
            candidate.mixed_port = int(payload["mixedPort"])
            candidate.controller_port = int(payload["controllerPort"])
            candidate.dns_port = int(payload["dnsPort"])
            candidate.server_proxy_port = int(
                payload.get("serverProxyPort", candidate.server_proxy_port)
            )
            candidate.strict_route = bool(payload["strictRoute"])
            candidate.start_on_launch = bool(payload["startOnLaunch"])
            errors = validate_config(candidate)
            if errors:
                raise ValueError(errors[0])
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
            self._notify("error", str(exc) or "设置内容无效")
            return
        with self._config_lock:
            old_runtime = (
                self.config.mixed_port,
                self.config.controller_port,
                self.config.dns_port,
                self.config.strict_route,
            )
            self.config = candidate
            self.core.controller_port = candidate.controller_port
            new_runtime = (
                candidate.mixed_port,
                candidate.controller_port,
                candidate.dns_port,
                candidate.strict_route,
            )
            self._save_and_apply("设置已保存", full_restart=old_runtime != new_runtime)

    def addSubscription(self, name: str, url: str, group_name: str = "") -> None:
        if not url.strip().lower().startswith(("http://", "https://")):
            self._notify("error", "订阅地址必须以 http:// 或 https:// 开头")
            return
        group = normalize_node_group_name(group_name)
        with self._config_lock:
            if group and group not in self.config.node_groups:
                self._notify("error", "订阅分组不存在")
                return
        self._submit_import(lambda: self._download_subscription(name, url, None, group))

    def importPaste(self, name: str, content: str, group_name: str = "") -> None:
        self.importFileText(name, content, group_name)

    def importFileText(self, name: str, content: str, group_name: str = "") -> None:
        try:
            nodes, errors = parse_import_content(content)
        except (ImportContentError, ValueError) as exc:
            self._notify("error", f"配置导入失败：{exc}")
            return
        with self._config_lock:
            group = normalize_node_group_name(group_name)
            if group and group not in self.config.node_groups:
                self._notify("error", "导入分组不存在")
                return
            imported = prepare_imported_nodes(
                nodes, name.strip() or "文件导入", self.config.imported_nodes
            )
            for node in imported:
                node.group = group
            self.config.imported_nodes.extend(imported)
            if imported and not self.config.selected_node:
                self.config.selected_node = imported[0].name
            self._save_and_apply("节点已导入")
        kind = "success" if imported else "info"
        self._notify(kind, f"已导入 {len(imported)} 个节点")
        if errors:
            self._notify("info", f"另有 {len(errors)} 个链接未识别")

    def exportPortableConfigText(self) -> str:
        with self._config_lock:
            return json.dumps(export_portable_config(self.config), ensure_ascii=False, indent=2)

    def importPortableConfigText(self, content: str) -> None:
        if self.core.is_running or self._busy:
            self._notify("error", "请先停止接管，再导入跨设备配置")
            return
        try:
            if len(content.encode("utf-8")) > 10 * 1024 * 1024:
                raise ValueError("配置文件不能超过 10 MB")
            payload = json.loads(content)
            imported = import_portable_config(self.config, payload)
        except (TypeError, ValueError, json.JSONDecodeError) as exc:
            self._notify("error", f"配置导入失败：{exc}")
            return
        with self._config_lock:
            self.config = imported
            self.store.save(imported)
            write_mihomo_config(imported, generated_config_path())
        self._notify("success", "跨设备配置已导入，本机端口保持不变")

    def refreshSubscription(self, index: int) -> None:
        with self._config_lock:
            if not 0 <= index < len(self.config.subscriptions):
                return
            source = self.config.subscriptions[index]
        self._submit_import(lambda: self._download_subscription(source.name, source.url, source))

    def refreshAllSubscriptions(self) -> None:
        with self._config_lock:
            sources = list(self.config.subscriptions)
        if not sources:
            self._notify("info", "没有可刷新的订阅")
            return

        def work() -> None:
            failures = 0
            for source in sources:
                try:
                    self._download_subscription(source.name, source.url, source, notify=False)
                except Exception as exc:
                    failures += 1
                    self._append_log(f"[Manager] 刷新 {source.name} 失败：{exc}")
            if failures:
                self._notify("error", f"订阅刷新完成，{failures} 个失败")
            else:
                self._notify("success", "全部订阅已刷新")

        self._submit_import(work)

    def deleteSubscription(self, index: int) -> None:
        with self._config_lock:
            if not 0 <= index < len(self.config.subscriptions):
                return
            source = self.config.subscriptions.pop(index)
            removed = {
                node.name for node in self.config.imported_nodes if node.source_id == source.source_id
            }
            self.config.imported_nodes = [
                node for node in self.config.imported_nodes if node.source_id != source.source_id
            ]
            clear_node_dialer_references(self.config.imported_nodes, removed)
            self._repair_selected_node(removed)
            self._save_and_apply("订阅及其节点已删除")

    def deleteNode(self, index: int) -> None:
        with self._config_lock:
            if not 0 <= index < len(self.config.imported_nodes):
                return
            removed = self.config.imported_nodes.pop(index)
            clear_node_dialer_references(
                self.config.imported_nodes, {removed.name}
            )
            self._repair_selected_node({removed.name})
            self._save_and_apply("节点已删除")

    def deleteErrorNodes(self) -> None:
        with self._node_lock:
            if self._node_pending:
                self._notify("info", "请等待当前节点测速完成")
                return
            failed = {
                name for name, result in self._node_delays.items()
                if result.get("status") == "error"
            }
        if not failed:
            self._notify("info", "当前没有 Error 节点")
            return
        with self._config_lock:
            before = len(self.config.imported_nodes)
            self.config.imported_nodes = [
                node for node in self.config.imported_nodes if node.name not in failed
            ]
            clear_node_dialer_references(self.config.imported_nodes, failed)
            removed = before - len(self.config.imported_nodes)
            self._repair_selected_node(failed)
            self._save_and_apply("Error 节点已批量删除")
        with self._node_lock:
            for name in failed:
                self._node_delays.pop(name, None)
        self._notify("success", f"已删除 {removed} 个 Error 节点；刷新订阅可恢复")

    def selectNode(self, name: str) -> None:
        with self._config_lock:
            if not any(node.name == name for node in self.config.imported_nodes):
                return
            self.config.selected_node = name
            self._save_and_apply("当前节点已切换")

    def createNodeGroup(self, name: str) -> None:
        group = normalize_node_group_name(name)
        if not group:
            self._notify("error", "分组名称不能为空")
            return
        with self._config_lock:
            if group in self.config.node_groups:
                self._notify("info", "分组已经存在")
                return
            self.config.node_groups.append(group)
            self.store.save(self.config)
        self._notify("success", f"已创建分组：{group}")

    def assignNodeGroup(self, name: str, group_name: str) -> None:
        group = normalize_node_group_name(group_name)
        with self._config_lock:
            if group and group not in self.config.node_groups:
                self._notify("error", "分组不存在")
                return
            node = next(
                (item for item in self.config.imported_nodes if item.name == name), None
            )
            if node is None:
                self._notify("error", "节点不存在")
                return
            node.group = group
            self.store.save(self.config)
        self._notify("success", "节点分组已更新")

    def setNodeDialerProxy(self, name: str, dialer_name: str) -> None:
        with self._config_lock:
            node = next(
                (item for item in self.config.imported_nodes if item.name == name), None
            )
            dialer = dialer_name.strip()
            if node is None:
                self._notify("error", "节点不存在")
                return
            if dialer and not any(
                item.name == dialer for item in self.config.imported_nodes
            ):
                self._notify("error", "中转节点不存在")
                return
            previous = node.config.get(NODE_DIALER_PROXY_KEY)
            previous_policy = node.config.get(NODE_DIALER_POLICY_KEY)
            if dialer:
                node.config[NODE_DIALER_PROXY_KEY] = dialer
                node.config[NODE_DIALER_POLICY_KEY] = "manual"
            else:
                node.config.pop(NODE_DIALER_PROXY_KEY, None)
                node.config[NODE_DIALER_POLICY_KEY] = "direct"
            if not self._save_and_apply(
                f"节点中转已设置为 {dialer}" if dialer else "节点已改为直接连接"
            ):
                if previous is None:
                    node.config.pop(NODE_DIALER_PROXY_KEY, None)
                else:
                    node.config[NODE_DIALER_PROXY_KEY] = previous
                if previous_policy is None:
                    node.config.pop(NODE_DIALER_POLICY_KEY, None)
                else:
                    node.config[NODE_DIALER_POLICY_KEY] = previous_policy

    def saveProxyRelayRules(self, payload_json: str) -> None:
        with self._config_lock:
            previous_configs = {
                node.node_id: dict(node.config) for node in self.config.imported_nodes
            }
            try:
                payload = json.loads(payload_json)
                assignments = payload.get("assignments", [])
                if not isinstance(assignments, list) or len(assignments) > 500:
                    raise ValueError("代理域名前置配置无效")
                names = {node.name for node in self.config.imported_nodes}
                nodes_by_name = {node.name: node for node in self.config.imported_nodes}
                for item in assignments:
                    if not isinstance(item, dict):
                        raise ValueError("代理域名前置配置无效")
                    node = nodes_by_name.get(str(item.get("node", "")))
                    mode = str(item.get("mode", "")).lower()
                    dialer = str(item.get("dialer", "")).strip()
                    if node is None or node.protocol.lower() != "http":
                        raise ValueError("代理入口节点不存在")
                    server = normalize_proxy_endpoint_host(
                        item.get("server", node.config.get("server", ""))
                    )
                    try:
                        port = int(item.get("port", node.config.get("port", 0)))
                    except (TypeError, ValueError):
                        raise ValueError(
                            "代理入口端口必须是 1 到 65535 之间的整数"
                        ) from None
                    if not 1 <= port <= 65535:
                        raise ValueError("代理入口端口必须是 1 到 65535 之间的整数")
                    node.config["server"] = server
                    node.config["port"] = port
                    if mode == "auto":
                        node.config.pop(NODE_DIALER_PROXY_KEY, None)
                        node.config.pop(NODE_DIALER_POLICY_KEY, None)
                    elif mode == "direct":
                        node.config.pop(NODE_DIALER_PROXY_KEY, None)
                        node.config[NODE_DIALER_POLICY_KEY] = "direct"
                    elif mode == "manual" and dialer in names and dialer != node.name:
                        node.config[NODE_DIALER_PROXY_KEY] = dialer
                        node.config[NODE_DIALER_POLICY_KEY] = "manual"
                    else:
                        raise ValueError("请选择有效的前置节点")
                apply_automatic_node_dialers(self.config.imported_nodes)
                errors = validate_config(self.config)
                if errors:
                    raise ValueError(errors[0])
                if not self._save_and_apply("代理域名前置规则已更新"):
                    raise ValueError("代理域名前置规则未能应用")
            except (TypeError, ValueError, json.JSONDecodeError) as exc:
                for node in self.config.imported_nodes:
                    if node.node_id in previous_configs:
                        node.config = previous_configs[node.node_id]
                self._notify("error", str(exc) or "代理域名前置规则无效")

    def deleteNodeGroup(self, name: str) -> None:
        group = normalize_node_group_name(name)
        with self._config_lock:
            if group not in self.config.node_groups:
                return
            self.config.node_groups.remove(group)
            for source in self.config.subscriptions:
                if source.group == group:
                    source.group = ""
            for node in self.config.imported_nodes:
                if node.group == group:
                    node.group = ""
            self.store.save(self.config)
        self._notify("success", "分组已删除，节点恢复按来源归类")

    def testExit(self) -> None:
        if not self.core.is_running:
            self._notify("error", "请先启动接管核心")
            return

        def work() -> None:
            try:
                address = exit_ip_through_proxy(
                    f"http://127.0.0.1:{self.config.mixed_port}"
                )
            except (requests.RequestException, ValueError) as exc:
                self._notify("error", f"出口检测失败：{exc}")
                return
            with self._state_lock:
                self._exit_ip = address
            self._notify("success", f"当前出口：{address}")

        self._operations.submit(work)

    def testSource(self, key: str) -> None:
        source = self.config.clash if key == "clash" else self.config.v2ray if key == "v2ray" else None
        if source is None:
            return

        def work() -> None:
            ok, detail = test_upstream(source)
            self._notify("success" if ok else "error", f"{source.name}：{detail}")

        self._operations.submit(work)

    def testAllNodes(self) -> None:
        with self._config_lock:
            names = [node.name for node in self.config.imported_nodes]
        if not self.core.is_running:
            self._notify("error", "请先启动接管核心再测速")
            return
        if not names:
            self._notify("error", "没有可测试的内置节点")
            return
        with self._node_lock:
            for name in names:
                if name in self._node_pending:
                    continue
                self._node_pending.add(name)
                self._node_delays[name] = {"status": "testing", "delay": None}
                self._submit_node_test(name)
        self._notify("info", f"正在并发测试 {len(names)} 个节点")

    def testNode(self, name: str) -> None:
        if not self.core.is_running:
            self._notify("error", "请先启动接管核心再测速")
            return
        with self._config_lock:
            exists = any(node.name == name for node in self.config.imported_nodes)
        if not exists:
            self._notify("error", "节点不存在")
            return
        with self._node_lock:
            if name in self._node_pending:
                return
            self._node_pending.add(name)
            self._node_delays[name] = {"status": "testing", "delay": None}
        self._submit_node_test(name)

    def clearLogs(self) -> None:
        with self._state_lock:
            self._logs.clear()

    def openLogs(self) -> None:
        self._notify("info", "Linux 日志请使用 journalctl -u network-manager")

    def windowAction(self, _action: str) -> bool:
        return False

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        self._desired_running = False
        self._stop_event.set()
        self.core.stop()
        self._monitor.join(timeout=3)
        self._operations.shutdown(wait=False, cancel_futures=True)
        self._node_executor.shutdown(wait=False, cancel_futures=True)

    def _queue_core_action(self, action: str) -> bool:
        with self._state_lock:
            if self._busy:
                return False
            self._busy = True

        def work() -> None:
            try:
                if action == "stop":
                    self.core.stop()
                    self._notify("success", "接管已停止")
                else:
                    write_mihomo_config(self.config, generated_config_path())
                    if action == "restart":
                        self.core.restart(generated_config_path())
                    else:
                        self.core.start(generated_config_path())
                    self._recovery_attempts = 0
                    self._notify("success", "TUN 接管已启动")
            except Exception as exc:
                self._append_log(f"[Manager] 核心{action}失败：{exc}")
                self._notify("error", str(exc) or "核心操作失败")
                if action != "stop" and self._desired_running:
                    self._recovery_attempts += 1
                    self._next_recovery_at = time.monotonic() + min(
                        60, 2 ** min(self._recovery_attempts, 6)
                    )
            finally:
                with self._state_lock:
                    self._busy = False

        self._operations.submit(work)
        return True

    def _save_and_apply(self, message: str, *, full_restart: bool = False) -> bool:
        apply_automatic_node_dialers(self.config.imported_nodes)
        errors = validate_config(self.config)
        if errors:
            self._notify("error", errors[0])
            return False
        self.store.save(self.config)
        write_mihomo_config(self.config, generated_config_path())
        if self.core.is_running:
            if full_restart:
                self._queue_core_action("restart")
            else:
                controller_secret = self.config.controller_secret

                def reload_core() -> None:
                    try:
                        self.core.reload(generated_config_path(), controller_secret)
                    except Exception as exc:
                        self._append_log(f"[Manager] 热加载失败，准备重启：{exc}")
                        self._queue_core_action("restart")

                self._operations.submit(reload_core)
        self._notify("success", message)
        return True

    def _monitor_loop(self) -> None:
        while True:
            interval = 1.0 if self.core.is_running or self._desired_running or self._busy else 5.0
            if self._stop_event.wait(interval):
                return
            for line in self.core.drain_logs(300):
                self._append_log(line)
            running = self.core.is_running
            if running:
                if self._poll_traffic() or self.core.is_healthy:
                    self._unhealthy_polls = 0
                else:
                    self._unhealthy_polls += 1
            elif self._last_monitor_running:
                self._traffic_tracker.reset()
                self._download_samples.append(0.0)
                self._upload_samples.append(0.0)
                with self._state_lock:
                    self._traffic.update(
                        downloadRate="0 B/s",
                        uploadRate="0 B/s",
                        connections="0",
                    )
            self._last_monitor_running = running
            if (
                self._desired_running
                and not self._busy
                and (not self.core.is_running or self._unhealthy_polls >= 3)
                and time.monotonic() >= self._next_recovery_at
            ):
                if self.core.is_running:
                    self.core.stop()
                self._unhealthy_polls = 0
                self._queue_core_action("start")

    def _poll_traffic(self) -> bool:
        try:
            response = requests.get(
                f"http://127.0.0.1:{self.config.controller_port}/connections",
                headers={"Authorization": f"Bearer {self.config.controller_secret}"},
                timeout=(0.5, 0.8),
            )
            response.raise_for_status()
            sample = self._traffic_tracker.update(
                parse_connection_snapshot(response.json())
            )
        except (requests.RequestException, TypeError, ValueError):
            return False
        with self._state_lock:
            self._download_samples.append(sample.download_rate)
            self._upload_samples.append(sample.upload_rate)
            self._traffic = {
                "downloadRate": format_rate(sample.download_rate),
                "uploadRate": format_rate(sample.upload_rate),
                "downloadTotal": format_bytes(sample.download_total),
                "uploadTotal": format_bytes(sample.upload_total),
                "connections": str(sample.active_connections),
            }
        return True

    def _source_state(self, upstream: Upstream) -> dict[str, object]:
        available = upstream.enabled and port_is_open(upstream.host, upstream.port)
        return {
            "enabled": upstream.enabled,
            "protocol": upstream.protocol,
            "host": upstream.host,
            "port": upstream.port,
            "endpoint": f"{upstream.host}:{upstream.port}",
            "status": "正在监听" if available else "已禁用" if not upstream.enabled else "未监听",
            "available": available,
        }

    def _rule_key(self, rule: RoutingRule) -> tuple[str, str]:
        return rule.rule_type, normalize_rule_value(rule.rule_type, rule.value)

    def _common_rules(self) -> list[tuple[int, RoutingRule]]:
        return [
            (index, rule)
            for index, rule in enumerate(self.config.rules)
            if is_common_overseas_rule(rule)
        ]

    def _rule_states(self, rules: list[RoutingRule]) -> list[dict[str, object]]:
        common = [
            (index, rule)
            for index, rule in enumerate(rules)
            if is_common_overseas_rule(rule)
        ]
        common_indexes = {index for index, _rule in common}
        states: list[dict[str, object]] = []
        group_added = False
        for index, rule in enumerate(rules):
            if index in common_indexes:
                if group_added:
                    continue
                group_added = True
                targets = {item.target for _item_index, item in common}
                target = next(iter(targets)) if len(targets) == 1 else "MIXED"
                enabled_count = sum(item.enabled for _item_index, item in common)
                states.append(
                    {
                        "kind": "group",
                        "groupId": COMMON_OVERSEAS_GROUP,
                        "enabled": enabled_count == len(common),
                        "partiallyEnabled": 0 < enabled_count < len(common),
                        "ruleType": "GROUP",
                        "ruleTypeLabel": "预置规则组",
                        "value": f"{len(common)} 条匹配",
                        "detail": "、".join(item.value for _item_index, item in common),
                        "entries": [
                            {
                                "domain": item.value,
                                "label": item.note,
                                "enabled": item.enabled,
                            }
                            for _item_index, item in common
                        ],
                        "defaultEntries": [domain for domain, _label in DEFAULT_PROXY_DOMAINS],
                        "target": target,
                        "targetLabel": TARGET_LABELS.get(target, "多个去向"),
                        "note": "常用海外站点",
                        "count": len(common),
                    }
                )
                continue
            states.append(
                {
                    "kind": "rule",
                    "index": index,
                    "enabled": rule.enabled,
                    "partiallyEnabled": False,
                    "ruleType": rule.rule_type,
                    "ruleTypeLabel": RULE_LABELS.get(rule.rule_type, rule.rule_type),
                    "value": rule.value,
                    "target": rule.target,
                    "targetLabel": TARGET_LABELS.get(rule.target, rule.target),
                    "note": rule.note,
                }
            )
        relay_entries = [
            {
                "node": node.name,
                "endpoint": str(node.config.get("server", "")),
                "port": node.config.get("port", 0),
                "relay": node.dialer_proxy,
                "policy": node.dialer_policy
                or ("manual" if node.dialer_proxy else "direct"),
            }
            for node in self.config.imported_nodes
            if node.protocol.lower() == "http"
            and not node.source_id.startswith("server-deployment:")
            and str(node.config.get("server", "")).strip()
        ]
        if relay_entries:
            relays = list(
                dict.fromkeys(
                    str(entry["relay"]) for entry in relay_entries if entry["relay"]
                )
            )
            configured_count = len([entry for entry in relay_entries if entry["relay"]])
            automatic_count = len(
                [entry for entry in relay_entries if entry["policy"] == "auto"]
            )
            states.append(
                {
                    "kind": "relay",
                    "enabled": configured_count == len(relay_entries),
                    "partiallyEnabled": 0 < configured_count < len(relay_entries),
                    "automatic": automatic_count == len(relay_entries),
                    "ruleType": "PROXY-ENDPOINT",
                    "ruleTypeLabel": "代理域名前置",
                    "value": f"{len(relay_entries)} 个代理入口",
                    "detail": "、".join(str(entry["endpoint"]) for entry in relay_entries),
                    "entries": relay_entries,
                    "target": (
                        relays[0]
                        if len(relays) == 1
                        else "MULTIPLE" if relays else "DIRECT"
                    ),
                    "targetLabel": (
                        f"经 {relays[0]}"
                        if len(relays) == 1
                        else f"经 {len(relays)} 个前置节点" if relays else "直连"
                    ),
                    "note": "第 2 阶段；可编辑，代码直接使用代理域名时同样生效",
                    "count": len(relay_entries),
                }
            )
        return states

    def _download_proxy(self) -> str | None:
        if self.core.is_running:
            return f"http://127.0.0.1:{self.config.mixed_port}"
        for source in (self.config.clash, self.config.v2ray):
            if source.enabled and port_is_open(source.host, source.port):
                return proxy_url(source)
        return None

    def _download_subscription(
        self,
        name: str,
        url: str,
        existing: SubscriptionSource | None,
        group: str = "",
        *,
        notify: bool = True,
    ) -> None:
        content = fetch_subscription(url, self._download_proxy())
        nodes, errors = parse_import_content(content)
        source = subscription_from_url(name, url, existing.group if existing else group)
        if existing is not None:
            source.source_id = existing.source_id
        with self._config_lock:
            previous_groups = {
                node.name: node.group
                for node in self.config.imported_nodes
                if node.source_id == source.source_id and node.group
            }
            previous_dialers = {
                node.name: node.dialer_proxy
                for node in self.config.imported_nodes
                if node.source_id == source.source_id and node.dialer_proxy
            }
            previous_dialer_policies = {
                node.name: node.dialer_policy
                for node in self.config.imported_nodes
                if node.source_id == source.source_id and node.dialer_policy
            }
            previous_names = {
                node.name
                for node in self.config.imported_nodes
                if node.source_id == source.source_id
            }
            remaining = [
                node for node in self.config.imported_nodes if node.source_id != source.source_id
            ]
            imported = prepare_imported_nodes(
                nodes, source.name, remaining, source.source_id
            )
            for node in imported:
                node.group = previous_groups.get(node.name, source.group)
                if node.name in previous_dialers:
                    node.config[NODE_DIALER_PROXY_KEY] = previous_dialers[node.name]
                if node.name in previous_dialer_policies:
                    node.config[NODE_DIALER_POLICY_KEY] = previous_dialer_policies[
                        node.name
                    ]
            self.config.imported_nodes = remaining + imported
            clear_node_dialer_references(
                self.config.imported_nodes,
                previous_names - {node.name for node in imported},
            )
            source.last_updated = datetime.now(timezone.utc).isoformat(timespec="seconds")
            replaced = False
            for index, item in enumerate(self.config.subscriptions):
                if item.source_id == source.source_id:
                    self.config.subscriptions[index] = source
                    replaced = True
                    break
            if not replaced:
                self.config.subscriptions.append(source)
            if self.config.selected_node not in {
                node.name for node in self.config.imported_nodes
            }:
                self.config.selected_node = imported[0].name if imported else ""
            self._save_and_apply("订阅已更新")
        if notify:
            self._notify("success", f"{source.name}：已导入 {len(imported)} 个节点")
            if errors:
                self._notify("info", f"另有 {len(errors)} 个链接未识别")

    def _submit_import(self, work: Any) -> None:
        with self._state_lock:
            if self._importing:
                self._notify("info", "已有导入或订阅任务正在执行")
                return
            self._importing = True

        def wrapped() -> None:
            try:
                work()
            except Exception as exc:
                self._notify("error", str(exc) or "订阅操作失败")
            finally:
                with self._state_lock:
                    self._importing = False

        self._operations.submit(wrapped)

    def _repair_selected_node(self, removed_names: set[str]) -> None:
        if self.config.selected_node in removed_names:
            self.config.selected_node = (
                self.config.imported_nodes[0].name if self.config.imported_nodes else ""
            )
        if self.config.mode in {"GLOBAL_BUILTIN", "SMART"} and not self.config.imported_nodes:
            self.config.mode = "RULE"

    def _measure_node_delay(self, name: str) -> dict[str, object]:
        try:
            response = requests.get(
                f"http://127.0.0.1:{self.config.controller_port}"
                f"/proxies/{quote(name, safe='')}/delay",
                headers={"Authorization": f"Bearer {self.config.controller_secret}"},
                params={"timeout": 6000, "url": "https://www.gstatic.com/generate_204"},
                timeout=(3, 9),
            )
            response.raise_for_status()
            delay = int(response.json().get("delay", 0))
            if delay <= 0:
                raise ValueError("测速没有返回有效延迟")
            return {"status": "ok", "delay": delay}
        except (requests.RequestException, TypeError, ValueError):
            node = next(
                (item for item in self.config.imported_nodes if item.name == name), None
            )
            diagnostic = ""
            if node and node.dialer_proxy:
                diagnostic = (
                    f"经 {node.dialer_proxy} 中转连接失败，请先单独测试中转节点"
                )
            elif node:
                diagnostic = diagnose_authenticated_proxy(node.config)
            return {
                "status": "error",
                "delay": None,
                "message": diagnostic or "节点连接或测速端点不可用",
            }

    def _submit_node_test(self, name: str) -> None:
        future = self._node_executor.submit(self._measure_node_delay, name)
        future.add_done_callback(lambda result: self._node_test_finished(name, result))

    def _node_test_finished(
        self, name: str, future: Future[dict[str, object]]
    ) -> None:
        try:
            result = future.result()
        except Exception as exc:  # pragma: no cover - executor boundary
            result = {"status": "error", "delay": None, "message": str(exc)}
        with self._node_lock:
            self._node_pending.discard(name)
            self._node_delays[name] = result
            pending = len(self._node_pending)
        if result.get("status") == "ok":
            self._notify("success", f"{name}：{result['delay']} ms")
        else:
            detail = str(result.get("message", "")).strip() or "测速失败"
            self._notify("error", f"{name}：{detail}")
        if pending == 0:
            self._notify("info", "节点测速已完成")

    def _notify(self, kind: str, message: str) -> None:
        with self._state_lock:
            self._toasts.append({"kind": kind, "message": message})

    def _append_log(self, line: str) -> None:
        if not line:
            return
        with self._state_lock:
            self._logs.append(line)
        print(line, flush=True)


def create_headless_server(
    controller: HeadlessController,
    *,
    host: str,
    port: int,
    username: str,
    password: str,
) -> LocalWebServer:
    web_root = bundle_root() / "network_manager" / "web"
    if not web_root.is_dir():
        web_root = Path(__file__).with_name("web")
    return LocalWebServer(
        web_root,
        controller,
        HEADLESS_METHODS,
        direct_dispatch=True,
        host=host,
        port=port,
        access_username=username,
        access_password=password,
    )


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description="Network Manager Linux headless WebGUI")
    parser.add_argument("--listen", default=os.environ.get("NETWORK_MANAGER_WEB_HOST", "127.0.0.1"))
    parser.add_argument(
        "--port", type=int, default=int(os.environ.get("NETWORK_MANAGER_WEB_PORT", "9091"))
    )
    parser.add_argument(
        "--username", default=os.environ.get("NETWORK_MANAGER_WEB_USERNAME", "admin")
    )
    parser.add_argument("--password", default=os.environ.get("NETWORK_MANAGER_WEB_PASSWORD", ""))
    parser.add_argument("--start-core", action="store_true")
    args = parser.parse_args(argv)

    controller = HeadlessController(start_core=args.start_core)
    try:
        server = create_headless_server(
            controller,
            host=args.listen,
            port=args.port,
            username=args.username,
            password=args.password,
        )
        url = server.start()
    except Exception:
        controller.close()
        raise

    stopping = threading.Event()

    def stop(_signum: int, _frame: Any) -> None:
        stopping.set()

    signal.signal(signal.SIGTERM, stop)
    signal.signal(signal.SIGINT, stop)
    print(f"Network Manager {__version__} WebGUI: {url}", flush=True)
    try:
        while not stopping.wait(1):
            pass
    finally:
        server.close()
        controller.close()
    return 0


def _has_tun_privilege() -> bool:
    return os.name != "posix" or not hasattr(os, "geteuid") or os.geteuid() == 0


def _sanitize_headless_config(config: AppConfig) -> bool:
    changed = False
    replacement = "BUILTIN" if config.imported_nodes else "DIRECT"
    if config.mode == "GLOBAL_SSH":
        config.mode = "GLOBAL_BUILTIN" if config.imported_nodes else "RULE"
        changed = True
    if config.default_target == "SSH":
        config.default_target = replacement
        changed = True
    for rule in config.rules:
        if rule.target == "SSH":
            rule.target = replacement
            changed = True
    return changed


if __name__ == "__main__":
    raise SystemExit(main())
