from __future__ import annotations

from collections import deque
from concurrent.futures import Future, ThreadPoolExecutor
import json
import os
from pathlib import Path
from threading import Lock
from typing import Callable
from urllib.parse import quote, urlsplit

import psutil
import requests
from PySide6.QtCore import QObject, Qt, QUrl, Signal, Slot
from PySide6.QtGui import QDesktopServices
from PySide6.QtWebEngineWidgets import QWebEngineView
from PySide6.QtWidgets import QFileDialog

from network_manager.local_web_server import LocalWebServer
from network_manager.credential_store import CredentialStore, CredentialStoreError
from network_manager.models import (
    ImportedNode,
    RoutingRule,
    SshServerProfile,
    Upstream,
    default_routing_rules,
    normalize_rule_value,
    validate_config,
    validate_rule,
)
from network_manager.server_deployer import (
    DeploymentResult,
    ServerDeploymentError,
    ServerProxyDeployer,
    deployment_source_id,
    shadowsocks_share_link,
)
from network_manager.paths import (
    logs_dir,
    ssh_credentials_path,
    ssh_known_hosts_path,
)
from network_manager.ui.dialogs import RULE_LABELS, TARGET_LABELS
from network_manager.ui.main_window import MODE_LABELS, MainWindow as NativeMainWindow
from network_manager.windows_startup import is_admin

COMMON_OVERSEAS_GROUP = "common-overseas"


class WebBridge(QObject):
    server_deploy_completed = Signal(str, bool, str, str, str)

    def __init__(self, window: NativeMainWindow) -> None:
        super().__init__(window)
        self.window = window
        self.process = psutil.Process()
        self._node_delay_lock = Lock()
        self._node_delays: dict[str, dict[str, object]] = {}
        self._node_test_generation = 0
        self._node_test_remaining = 0
        self._node_executor = ThreadPoolExecutor(
            max_workers=8, thread_name_prefix="node-delay"
        )
        self._ssh_executor = ThreadPoolExecutor(max_workers=1, thread_name_prefix="server-deploy")
        self._deployment_lock = Lock()
        self._deployment_states: dict[str, dict[str, str]] = {}
        self._bridge_closed = False
        self._toast_lock = Lock()
        self._toasts: deque[dict[str, str]] = deque(maxlen=40)
        self.common_rule_keys = {
            (rule.rule_type, normalize_rule_value(rule.rule_type, rule.value))
            for rule in default_routing_rules()
            if rule.rule_type != "PROCESS-NAME"
        }
        self.server_deploy_completed.connect(self._server_deploy_finished)

    @Slot(result=str)
    def getState(self) -> str:
        config = self.window.config
        running = self.window.core.is_running
        selected_ssh = next(
            (
                profile
                for profile in config.ssh_servers
                if profile.profile_id == config.selected_ssh_server
            ),
            None,
        )
        with self._deployment_lock:
            deployment_states = {
                profile_id: dict(value)
                for profile_id, value in self._deployment_states.items()
            }
        if self.window._operation_active:
            core_status = "处理中"
        elif running:
            core_status = "接管中"
        else:
            core_status = "已停止"

        rules = self._rule_states(config.rules)
        with self._node_delay_lock:
            node_delays = {name: dict(result) for name, result in self._node_delays.items()}
        nodes = []
        for index, node in enumerate(config.imported_nodes):
            server = str(node.config.get("server", ""))
            port = node.config.get("port")
            delay = node_delays.get(node.name, {"status": "idle", "delay": None})
            nodes.append(
                {
                    "index": index,
                    "name": node.name,
                    "protocol": node.protocol,
                    "server": f"{server}:{port}" if port else server,
                    "source": node.source,
                    "selected": node.name == config.selected_node,
                    "latencyStatus": delay.get("status", "idle"),
                    "latency": delay.get("delay"),
                }
            )
        subscriptions = []
        for index, source in enumerate(config.subscriptions):
            count = sum(1 for node in config.imported_nodes if node.source_id == source.source_id)
            subscriptions.append(
                {
                    "index": index,
                    "name": source.name,
                    "host": urlsplit(source.url).netloc or "订阅地址",
                    "nodeCount": count,
                    "lastUpdated": source.last_updated or "未更新",
                }
            )

        state = {
            "core": {
                "running": running,
                "busy": self.window._operation_active,
                "status": core_status,
                "admin": is_admin(),
                "mode": config.mode,
                "modeLabel": MODE_LABELS.get(config.mode, config.mode),
                "mixedPort": config.mixed_port,
            },
            "summary": {
                "processRules": sum(
                    1
                    for rule in config.rules
                    if rule.enabled and rule.rule_type == "PROCESS-NAME"
                ),
                "networkRules": sum(
                    1
                    for rule in config.rules
                    if rule.enabled and rule.rule_type != "PROCESS-NAME"
                ),
                "nodes": len(config.imported_nodes),
                "defaultTarget": TARGET_LABELS.get(
                    config.default_target, config.default_target
                ),
            },
            "sources": {
                "clash": self._source_state("clash", config.clash),
                "v2ray": self._source_state("v2ray", config.v2ray),
                "ssh": {
                    "enabled": bool(selected_ssh and selected_ssh.deployed_node_id),
                    "endpoint": (
                        f"{selected_ssh.host}:{selected_ssh.proxy_port}"
                        if selected_ssh and selected_ssh.deployed_node_id
                        else "尚未配置"
                    ),
                    "status": (
                        "已部署" if selected_ssh and selected_ssh.deployed_node_id else "未部署"
                    ),
                    "available": bool(selected_ssh and selected_ssh.deployed_node_id),
                },
            },
            "traffic": {
                "status": self.window.traffic_monitor_status.text(),
                "downloadRate": self.window.download_rate_label.text(),
                "uploadRate": self.window.upload_rate_label.text(),
                "downloadTotal": self.window.download_total_label.text().replace(
                    "累计下载  ", ""
                ),
                "uploadTotal": self.window.upload_total_label.text().replace(
                    "累计上传  ", ""
                ),
                "connections": self.window.active_connections_label.text(),
                "downloadSamples": list(self.window.traffic_chart.download_rates),
                "uploadSamples": list(self.window.traffic_chart.upload_rates),
                "memoryMb": self._memory_usage_mb(),
            },
            "exitIp": self.window.exit_ip_label.text(),
            "rules": rules,
            "fallbackRule": {
                "target": config.default_target,
                "targetLabel": TARGET_LABELS.get(
                    config.default_target, config.default_target
                ),
            },
            "nodes": nodes,
            "subscriptions": subscriptions,
            "sshServers": [
                {
                    "profileId": profile.profile_id,
                    "name": profile.name,
                    "host": profile.host,
                    "port": profile.port,
                    "username": profile.username,
                    "proxyPort": profile.proxy_port,
                    "authMethod": profile.auth_method,
                    "keyPath": profile.key_path,
                    "rememberPassword": profile.remember_password,
                    "hasCredential": self._has_credential(profile),
                    "selected": profile.profile_id == config.selected_ssh_server,
                    "deployed": bool(profile.deployed_node_id),
                    "deployedAt": profile.deployed_at,
                    "deployedVersion": profile.deployed_version,
                    "nodeName": (
                        self._deployed_node(profile).name if self._deployed_node(profile) else ""
                    ),
                    "shareLink": self._deployed_share_link(profile),
                    "deployment": deployment_states.get(
                        profile.profile_id,
                        {"status": "idle", "stage": "", "error": ""},
                    ),
                }
                for profile in config.ssh_servers
            ],
            "selectedNode": config.selected_node,
            "importing": not self.window.import_button.isEnabled(),
            "settings": {
                "mixedPort": config.mixed_port,
                "controllerPort": config.controller_port,
                "dnsPort": config.dns_port,
                "strictRoute": config.strict_route,
                "startOnLaunch": config.start_on_launch,
                "closeToTray": config.close_to_tray,
                "startWithWindows": config.start_with_windows,
            },
            "toasts": self._drain_toasts(),
        }
        return json.dumps(state, ensure_ascii=False)

    def _memory_usage_mb(self) -> int:
        try:
            return round(self.process.memory_info().rss / (1024 * 1024))
        except psutil.Error:
            return 0

    def _has_credential(self, profile: SshServerProfile) -> bool:
        if not profile.remember_password:
            return False
        try:
            return bool(self.window.credential_store.get(profile.profile_id))
        except CredentialStoreError:
            return False

    def _deployed_node(self, profile: SshServerProfile) -> ImportedNode | None:
        source_id = deployment_source_id(profile.profile_id)
        return next(
            (node for node in self.window.config.imported_nodes if node.source_id == source_id),
            None,
        )

    def _deployed_share_link(self, profile: SshServerProfile) -> str:
        node = self._deployed_node(profile)
        if node is None or node.protocol != "ss":
            return ""
        try:
            return shadowsocks_share_link(node.config)
        except (KeyError, TypeError, ValueError):
            return ""

    def _rule_key(self, rule: RoutingRule) -> tuple[str, str]:
        return (rule.rule_type, normalize_rule_value(rule.rule_type, rule.value))

    def _common_overseas_rules(self) -> list[tuple[int, RoutingRule]]:
        return [
            (index, rule)
            for index, rule in enumerate(self.window.config.rules)
            if self._rule_key(rule) in self.common_rule_keys
        ]

    def _rule_states(self, rules: list[RoutingRule]) -> list[dict[str, object]]:
        common = [
            (index, rule)
            for index, rule in enumerate(rules)
            if self._rule_key(rule) in self.common_rule_keys
        ]
        common_indexes = {index for index, _rule in common}
        common_added = False
        states: list[dict[str, object]] = []
        for index, rule in enumerate(rules):
            if index in common_indexes:
                if common_added:
                    continue
                common_added = True
                targets = {item.target for _item_index, item in common}
                enabled_count = sum(item.enabled for _item_index, item in common)
                target = next(iter(targets)) if len(targets) == 1 else "MIXED"
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
                        "target": target,
                        "targetLabel": (
                            TARGET_LABELS.get(target, target)
                            if target != "MIXED"
                            else "多个去向"
                        ),
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
        return states

    def _source_state(self, key: str, upstream: Upstream) -> dict[str, object]:
        status = self.window.source_widgets[key]["status"].text()
        return {
            "enabled": upstream.enabled,
            "protocol": upstream.protocol,
            "host": upstream.host,
            "port": upstream.port,
            "endpoint": f"{upstream.host}:{upstream.port}",
            "status": status,
            "available": status.startswith(("正在监听", "可用")),
        }

    def _measure_node_delay(self, node_name: str) -> dict[str, object]:
        endpoint = (
            f"http://127.0.0.1:{self.window.config.controller_port}"
            f"/proxies/{quote(node_name, safe='')}/delay"
        )
        headers = {"Authorization": f"Bearer {self.window.config.controller_secret}"}
        try:
            response = requests.get(
                endpoint,
                headers=headers,
                params={"timeout": 8000, "url": "http://www.gstatic.com/generate_204"},
                timeout=(3, 11),
            )
            response.raise_for_status()
            delay = int(response.json().get("delay", 0))
            if delay <= 0:
                raise ValueError("测速没有返回有效延迟")
            return {"status": "ok", "delay": delay}
        except (requests.RequestException, TypeError, ValueError) as exc:
            return {"status": "error", "delay": None, "message": str(exc)}

    def _node_delay_finished(
        self, generation: int, node_name: str, future: Future[dict[str, object]]
    ) -> None:
        try:
            result = future.result()
        except Exception as exc:  # pragma: no cover - executor boundary
            result = {"status": "error", "delay": None, "message": str(exc)}
        with self._node_delay_lock:
            if generation != self._node_test_generation:
                return
            self._node_delays[node_name] = result
            self._node_test_remaining -= 1
            finished = self._node_test_remaining == 0
            success_count = sum(
                item.get("status") == "ok" for item in self._node_delays.values()
            )
        if finished:
            self._notify("success", f"批量测速完成，{success_count} 个节点可用")

    def close(self) -> None:
        if self._bridge_closed:
            return
        self._bridge_closed = True
        with self._node_delay_lock:
            self._node_test_generation += 1
            self._node_test_remaining = 0
        self._node_executor.shutdown(wait=False, cancel_futures=True)
        self._ssh_executor.shutdown(wait=False, cancel_futures=True)

    @Slot(result=str)
    def getLogs(self) -> str:
        lines = self.window.log_view.toPlainText().splitlines()
        return "\n".join(lines[-800:])

    @Slot(result=bool)
    def toggleCore(self) -> bool:
        return self.window.toggle_core()

    @Slot(str)
    def setMode(self, mode: str) -> None:
        if mode not in MODE_LABELS:
            self._notify("error", "运行模式无效")
            return
        self.window._set_mode(mode)

    @Slot(str)
    def saveRule(self, payload_json: str) -> None:
        try:
            payload = json.loads(payload_json)
            rule_type = str(payload["ruleType"])
            rule = RoutingRule(
                rule_type=rule_type,
                value=normalize_rule_value(rule_type, str(payload["value"])),
                target=str(payload["target"]),
                enabled=bool(payload.get("enabled", True)),
                note=str(payload.get("note", "")).strip(),
            )
            errors = validate_rule(rule)
            if errors:
                raise ValueError("；".join(errors))
            index = int(payload.get("index", -1))
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
            self._notify("error", str(exc) or "规则内容无效")
            return
        if 0 <= index < len(self.window.config.rules):
            self.window.config.rules[index] = rule
            message = "规则已更新"
        else:
            self.window.config.rules.append(rule)
            message = "规则已添加"
        if self.window._save_and_apply(message):
            self.window._refresh_rules_table()
            self._notify("success", message)

    @Slot(int, str)
    def ruleAction(self, index: int, action: str) -> None:
        rules = self.window.config.rules
        if not 0 <= index < len(rules):
            return
        if action == "toggle":
            rules[index].enabled = not rules[index].enabled
            message = "规则状态已更新"
        elif action == "delete":
            rules.pop(index)
            message = "规则已删除"
        elif action in {"up", "down"}:
            target = index + (-1 if action == "up" else 1)
            if not 0 <= target < len(rules):
                return
            rules[index], rules[target] = rules[target], rules[index]
            message = "规则顺序已更新"
        else:
            return
        if self.window._save_and_apply(message):
            self.window._refresh_rules_table()
            self._notify("success", message)

    @Slot(str)
    def saveRuleGroup(self, payload_json: str) -> None:
        try:
            payload = json.loads(payload_json)
            group_id = str(payload["groupId"])
            target = str(payload["target"])
            if group_id != COMMON_OVERSEAS_GROUP or target not in TARGET_LABELS:
                raise ValueError("规则组去向无效")
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
            self._notify("error", str(exc) or "规则组内容无效")
            return
        members = self._common_overseas_rules()
        if not members:
            self._notify("error", "规则组已经不存在")
            return
        for _index, rule in members:
            rule.target = target
        if self.window._save_and_apply("常用海外站点去向已更新"):
            self.window._refresh_rules_table()
            self._notify("success", "规则组已统一切换")

    @Slot(str)
    def setDefaultTarget(self, target: str) -> None:
        if target not in {"CLASH", "V2RAY", "BUILTIN", "DIRECT"}:
            self._notify("error", "保底规则去向无效")
            return
        previous = self.window.config.default_target
        self.window.config.default_target = target
        errors = validate_config(self.window.config)
        if errors:
            self.window.config.default_target = previous
            self._notify("error", errors[0])
            return
        if self.window._save_and_apply("强制保底规则已更新"):
            self.window._refresh_rules_table()
            self._notify("success", "强制保底规则已更新")
        else:
            self.window.config.default_target = previous

    @Slot(str, str)
    def ruleGroupAction(self, group_id: str, action: str) -> None:
        if group_id != COMMON_OVERSEAS_GROUP:
            return
        members = self._common_overseas_rules()
        if not members:
            return
        if action == "toggle":
            enabled = not all(rule.enabled for _index, rule in members)
            for _index, rule in members:
                rule.enabled = enabled
            message = "常用海外站点规则组已启用" if enabled else "常用海外站点规则组已停用"
        elif action == "delete":
            member_indexes = {index for index, _rule in members}
            self.window.config.rules = [
                rule
                for index, rule in enumerate(self.window.config.rules)
                if index not in member_indexes
            ]
            message = "常用海外站点规则组已删除"
        else:
            return
        if self.window._save_and_apply(message):
            self.window._refresh_rules_table()
            self._notify("success", message)

    @Slot(str)
    def saveSources(self, payload_json: str) -> None:
        parsed_sources: dict[str, Upstream] = {}
        try:
            payload = json.loads(payload_json)
            for key in ("clash", "v2ray"):
                source = payload[key]
                protocol = str(source["protocol"])
                port = int(source["port"])
                if protocol not in {"socks5", "http"} or not 1 <= port <= 65535:
                    raise ValueError("代理源协议或端口无效")
                upstream = Upstream(
                    name="Clash 7897" if key == "clash" else "v2ray 10808",
                    host=str(source["host"]).strip(),
                    port=port,
                    protocol=protocol,
                    enabled=bool(source.get("enabled", True)),
                )
                parsed_sources[key] = upstream
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
            self._notify("error", str(exc) or "代理源设置无效")
            return
        previous_sources = {
            "clash": self.window.config.clash,
            "v2ray": self.window.config.v2ray,
        }
        for key, upstream in parsed_sources.items():
            setattr(self.window.config, key, upstream)
        errors = validate_config(self.window.config)
        if errors:
            for key, upstream in previous_sources.items():
                setattr(self.window.config, key, upstream)
            self.window._load_config_into_ui()
            self.window._refresh_port_status()
            self._notify("error", errors[0])
            return
        if self.window._save_and_apply("本地代理源已保存"):
            self.window._load_config_into_ui()
            self.window._refresh_port_status()
            self._notify("success", "本地代理源已保存")
            return
        for key, upstream in previous_sources.items():
            setattr(self.window.config, key, upstream)
        self.window._load_config_into_ui()
        self.window._refresh_port_status()

    @Slot(str)
    def saveSettings(self, payload_json: str) -> None:
        try:
            payload = json.loads(payload_json)
            self.window.mixed_port_spin.setValue(int(payload["mixedPort"]))
            self.window.controller_port_spin.setValue(int(payload["controllerPort"]))
            self.window.dns_port_spin.setValue(int(payload["dnsPort"]))
            self.window.strict_route_check.setChecked(bool(payload["strictRoute"]))
            self.window.start_on_launch_check.setChecked(bool(payload["startOnLaunch"]))
            self.window.close_to_tray_check.setChecked(bool(payload["closeToTray"]))
            self.window.start_with_windows_check.setChecked(bool(payload["startWithWindows"]))
        except (KeyError, TypeError, ValueError, json.JSONDecodeError) as exc:
            self._notify("error", str(exc) or "设置内容无效")
            return
        self.window._save_settings()
        self._notify("success", "设置已保存")

    @Slot(str, str)
    def addSubscription(self, name: str, url: str) -> None:
        if not url.strip().lower().startswith(("http://", "https://")):
            self._notify("error", "订阅地址必须以 http:// 或 https:// 开头")
            return
        self.window._execute_import(
            {"kind": "subscription", "name": name.strip() or "我的订阅", "url": url.strip()}
        )
        self._notify("info", "正在下载并解析订阅")

    @Slot(str, str)
    def importPaste(self, name: str, content: str) -> None:
        if not content.strip():
            self._notify("error", "请先粘贴订阅或节点内容")
            return
        self.window._execute_import(
            {"kind": "paste", "name": name.strip() or "手动导入", "content": content}
        )
        self._notify("info", "正在解析导入内容")

    @Slot()
    def importFile(self) -> None:
        path, _filter = QFileDialog.getOpenFileName(
            self.window,
            "选择代理配置",
            "",
            "代理配置 (*.yaml *.yml *.txt *.db);;所有文件 (*)",
        )
        if path:
            self.window._execute_import({"kind": "file", "path": path})

    @Slot(int)
    def refreshSubscription(self, index: int) -> None:
        source = self.window._subscription_at_row(index)
        if source:
            self.window._refresh_sources([source])
            self._notify("info", "正在刷新订阅")

    @Slot()
    def refreshAllSubscriptions(self) -> None:
        self.window._refresh_all_subscriptions()
        self._notify("info", "正在刷新全部订阅")

    @Slot(int)
    def deleteSubscription(self, index: int) -> None:
        source = self.window._subscription_at_row(index)
        if source is None:
            return
        removed_names = {
            node.name
            for node in self.window.config.imported_nodes
            if node.source_id == source.source_id
        }
        self.window.config.subscriptions.pop(index)
        self.window.config.imported_nodes = [
            node
            for node in self.window.config.imported_nodes
            if node.source_id != source.source_id
        ]
        if self.window.config.selected_node in removed_names:
            self.window.config.selected_node = (
                self.window.config.imported_nodes[0].name
                if self.window.config.imported_nodes
                else ""
            )
        self.window._save_and_apply("订阅已删除")
        self.window._refresh_nodes_table()
        self.window._refresh_subscriptions_table()
        self._notify("success", "订阅及其节点已删除")

    @Slot(int)
    def deleteNode(self, index: int) -> None:
        if not 0 <= index < len(self.window.config.imported_nodes):
            return
        removed = self.window.config.imported_nodes.pop(index)
        if self.window.config.selected_node == removed.name:
            self.window.config.selected_node = (
                self.window.config.imported_nodes[0].name
                if self.window.config.imported_nodes
                else ""
            )
        if self.window.config.mode == "GLOBAL_BUILTIN" and not self.window.config.imported_nodes:
            self.window.config.mode = "RULE"
        self.window._save_and_apply("节点已删除")
        self.window._refresh_nodes_table()
        self.window._refresh_subscriptions_table()
        self._notify("success", "节点已删除")

    @Slot(str)
    def selectNode(self, name: str) -> None:
        if any(node.name == name for node in self.window.config.imported_nodes):
            self.window.config.selected_node = name
            self.window._save_and_apply("内置节点已切换")
            self.window._refresh_nodes_table()
            self._notify("success", "当前节点已切换")

    @Slot()
    def testExit(self) -> None:
        self.window._test_managed_exit()

    @Slot(str)
    def testSource(self, key: str) -> None:
        if key in {"clash", "v2ray"}:
            self.window._test_source(key)

    @Slot()
    def testAllNodes(self) -> None:
        if not self.window.core.is_running:
            self._notify("error", "请先启动接管核心再进行节点测速")
            return
        nodes = list(self.window.config.imported_nodes)
        if not nodes:
            self._notify("error", "没有可测试的内置节点")
            return
        with self._node_delay_lock:
            if self._node_test_remaining:
                self._notify("info", "节点测速正在进行")
                return
            self._node_test_generation += 1
            generation = self._node_test_generation
            self._node_test_remaining = len(nodes)
            self._node_delays = {
                node.name: {"status": "testing", "delay": None} for node in nodes
            }
        self._notify("info", f"正在并发测试 {len(nodes)} 个节点")
        for node in nodes:
            future = self._node_executor.submit(self._measure_node_delay, node.name)
            future.add_done_callback(
                lambda completed, name=node.name: self._node_delay_finished(
                    generation, name, completed
                )
            )

    @Slot(str, str)
    def saveSshServer(self, payload_json: str, password: str) -> None:
        try:
            payload = json.loads(payload_json)
            profile_id = str(payload.get("profileId", "")).strip()
            existing = next(
                (
                    profile
                    for profile in self.window.config.ssh_servers
                    if profile.profile_id == profile_id
                ),
                None,
            )
            with self._deployment_lock:
                if self._deployment_states.get(profile_id, {}).get("status") == "deploying":
                    raise ValueError("服务器正在部署，请等待当前任务完成")
            profile = SshServerProfile(
                profile_id=profile_id or __import__("secrets").token_hex(8),
                name=str(payload.get("name", "SSH 服务器")).strip() or "SSH 服务器",
                host=str(payload["host"]).strip(),
                port=int(payload.get("port", 22)),
                username=str(payload.get("username", "root")).strip(),
                auth_method=str(payload.get("authMethod", "password")).lower(),
                key_path=str(payload.get("keyPath", "")).strip(),
                remember_password=bool(payload.get("rememberPassword", False)),
                proxy_port=int(payload.get("proxyPort", 24443)),
                deployed_node_id=existing.deployed_node_id if existing else "",
                deployed_at=existing.deployed_at if existing else "",
                deployed_version=existing.deployed_version if existing else "",
            )
            if profile.auth_method == "key" and not Path(profile.key_path).is_file():
                raise ValueError("SSH 私钥文件不存在")
            endpoint_changed = bool(
                existing
                and (existing.host != profile.host or existing.proxy_port != profile.proxy_port)
            )
            if endpoint_changed:
                profile.deployed_node_id = ""
                profile.deployed_at = ""
                profile.deployed_version = ""
            candidate_profiles = list(self.window.config.ssh_servers)
            if existing is None:
                candidate_profiles.append(profile)
            else:
                candidate_profiles[candidate_profiles.index(existing)] = profile
            previous_profiles = self.window.config.ssh_servers
            previous_nodes = self.window.config.imported_nodes
            previous_selected_node = self.window.config.selected_node
            self.window.config.ssh_servers = candidate_profiles
            removed_names: set[str] = set()
            if endpoint_changed:
                source_id = deployment_source_id(profile.profile_id)
                removed_names = {
                    node.name
                    for node in self.window.config.imported_nodes
                    if node.source_id == source_id
                }
                self.window.config.imported_nodes = [
                    node
                    for node in self.window.config.imported_nodes
                    if node.source_id != source_id
                ]
                if self.window.config.selected_node in removed_names:
                    self.window.config.selected_node = (
                        self.window.config.imported_nodes[0].name
                        if self.window.config.imported_nodes
                        else ""
                    )
            errors = validate_config(self.window.config)
            if errors:
                self.window.config.ssh_servers = previous_profiles
                self.window.config.imported_nodes = previous_nodes
                self.window.config.selected_node = previous_selected_node
                raise ValueError(errors[0])
            if profile.remember_password and password:
                self.window.credential_store.set(profile.profile_id, password)
            elif not profile.remember_password:
                self.window.credential_store.delete(profile.profile_id)
            if endpoint_changed:
                self.window._save_and_apply("服务器地址已更新，旧节点已移除")
            else:
                self.window.store.save(self.window.config)
            self._notify("success", "服务器登录配置已保存")
        except (
            KeyError,
            TypeError,
            ValueError,
            json.JSONDecodeError,
            CredentialStoreError,
        ) as exc:
            self._notify("error", str(exc) or "服务器配置无效")

    @Slot(str, str, bool)
    def deploySshServer(
        self, profile_id: str, password: str, remember_credential: bool = False
    ) -> None:
        profile = self._ssh_profile(profile_id)
        if profile is None:
            self._notify("error", "SSH 服务器不存在")
            return
        with self._deployment_lock:
            if any(
                state.get("status") == "deploying"
                for state in self._deployment_states.values()
            ):
                self._notify("info", "已有服务器部署任务正在执行")
                return
        if not password and profile.remember_password:
            try:
                password = self.window.credential_store.get(profile.profile_id)
            except CredentialStoreError as exc:
                self._notify("error", str(exc))
                return
        self.window.config.selected_ssh_server = profile.profile_id
        self.window.store.save(self.window.config)
        with self._deployment_lock:
            self._deployment_states[profile_id] = {
                "status": "deploying",
                "stage": "等待部署任务",
                "error": "",
            }

        def progress(stage: str) -> None:
            with self._deployment_lock:
                self._deployment_states[profile_id] = {
                    "status": "deploying",
                    "stage": stage,
                    "error": "",
                }

        existing_node = self._deployed_node(profile)
        existing_config = dict(existing_node.config) if existing_node else None
        self._notify(
            "info",
            f"正在检查 {profile.name}" if existing_config else f"正在通过 SSH 部署 {profile.name}",
        )
        future = self._ssh_executor.submit(
            self._deploy_server_if_needed,
            profile,
            password,
            existing_config,
            profile.deployed_at,
            progress,
        )
        future.add_done_callback(
            lambda completed: self._finish_server_deploy_future(
                profile.profile_id,
                completed,
                password if remember_credential else "",
            )
        )

    def _deploy_server_if_needed(
        self,
        profile: SshServerProfile,
        credential: str,
        existing_node_config: dict[str, object] | None,
        deployed_at: str,
        progress: Callable[[str], None],
    ) -> DeploymentResult:
        if existing_node_config:
            progress("正在检查远端代理服务")
            inspection = self.window.server_deployer.inspect(profile, credential)
            if inspection.get("status") == "active":
                node = dict(existing_node_config)
                return DeploymentResult(
                    node_config=node,
                    share_link=shadowsocks_share_link(node),
                    version=inspection.get("version") or profile.deployed_version,
                    deployed_at=deployed_at,
                    firewall="unchanged",
                    reused=True,
                )
            progress("远端服务未运行，正在修复部署")
        return self.window.server_deployer.deploy(profile, credential, progress)

    @Slot(str)
    def copyServerNode(self, profile_id: str) -> None:
        profile = self._ssh_profile(profile_id)
        link = self._deployed_share_link(profile) if profile else ""
        if not link:
            self._notify("error", "该服务器尚未生成可复制的代理节点")
            return
        from PySide6.QtWidgets import QApplication

        QApplication.clipboard().setText(link)
        self._notify("success", "代理节点链接已复制，可在其他设备导入")

    @Slot(str)
    def deleteSshServer(self, profile_id: str) -> None:
        with self._deployment_lock:
            if self._deployment_states.get(profile_id, {}).get("status") == "deploying":
                self._notify("error", "服务器正在部署，暂时不能删除")
                return
        profile = self._ssh_profile(profile_id)
        if profile is None:
            return
        previous = list(self.window.config.ssh_servers)
        previous_selected = self.window.config.selected_ssh_server
        previous_nodes = list(self.window.config.imported_nodes)
        previous_selected_node = self.window.config.selected_node
        self.window.config.ssh_servers = [
            item for item in previous if item.profile_id != profile_id
        ]
        if self.window.config.selected_ssh_server == profile_id:
            self.window.config.selected_ssh_server = ""
        source_id = deployment_source_id(profile_id)
        removed_names = {
            node.name
            for node in self.window.config.imported_nodes
            if node.source_id == source_id
        }
        self.window.config.imported_nodes = [
            node for node in self.window.config.imported_nodes if node.source_id != source_id
        ]
        if self.window.config.selected_node in removed_names:
            self.window.config.selected_node = (
                self.window.config.imported_nodes[0].name
                if self.window.config.imported_nodes
                else ""
            )
        errors = validate_config(self.window.config)
        if errors:
            self.window.config.ssh_servers = previous
            self.window.config.selected_ssh_server = previous_selected
            self.window.config.imported_nodes = previous_nodes
            self.window.config.selected_node = previous_selected_node
            self._notify("error", errors[0])
            return
        self.window.credential_store.delete(profile_id)
        self.window.store.save(self.window.config)
        with self._deployment_lock:
            self._deployment_states.pop(profile_id, None)
        if self.window.core.is_running and removed_names:
            self.window._save_and_apply("服务器节点已从本地移除")
        self._notify("success", "本地服务器记录和对应内置节点已删除；远端服务未卸载")

    @Slot(result=str)
    def pickSshKey(self) -> str:
        path, _filter = QFileDialog.getOpenFileName(
            self.window,
            "选择 SSH 私钥",
            str(Path.home() / ".ssh"),
            "SSH 私钥 (id_* *.pem *.key);;所有文件 (*)",
        )
        return path

    def _ssh_profile(self, profile_id: str) -> SshServerProfile | None:
        return next(
            (
                profile
                for profile in self.window.config.ssh_servers
                if profile.profile_id == profile_id
            ),
            None,
        )

    def _finish_server_deploy_future(
        self,
        profile_id: str,
        future: Future[DeploymentResult],
        credential: str = "",
    ) -> None:
        if self._bridge_closed:
            return
        try:
            result = future.result()
            success = True
            message = (
                "远端代理已存在并正常运行，无需重复部署"
                if result.reused
                else "服务器代理部署完成"
            )
            result_json = json.dumps(
                {
                    "node": result.node_config,
                    "shareLink": result.share_link,
                    "version": result.version,
                    "deployedAt": result.deployed_at,
                    "firewall": result.firewall,
                    "reused": result.reused,
                },
                ensure_ascii=False,
            )
        except (ServerDeploymentError, OSError, ValueError) as exc:
            success = False
            message = str(exc) or "服务器代理部署失败"
            result_json = ""
        self.server_deploy_completed.emit(
            profile_id,
            success,
            message,
            result_json,
            credential if success else "",
        )

    @Slot(str, bool, str, str, str)
    def _server_deploy_finished(
        self,
        profile_id: str,
        success: bool,
        message: str,
        result_json: str,
        credential: str,
    ) -> None:
        if self._bridge_closed:
            return
        profile = self._ssh_profile(profile_id)
        if not success or profile is None:
            with self._deployment_lock:
                self._deployment_states[profile_id] = {
                    "status": "error",
                    "stage": "部署失败",
                    "error": message,
                }
            self._notify("error", message)
            return
        payload = json.loads(result_json)
        if payload.get("reused"):
            profile.deployed_version = str(payload.get("version", profile.deployed_version))
            if credential:
                try:
                    self.window.credential_store.set(profile_id, credential)
                    profile.remember_password = True
                    message += "，SSH 凭据已安全保存"
                except CredentialStoreError as exc:
                    message += f"；凭据保存失败：{exc}"
            self.window.store.save(self.window.config)
            with self._deployment_lock:
                self._deployment_states[profile_id] = {
                    "status": "ready",
                    "stage": "远端服务已存在并运行中",
                    "error": "",
                }
            self._notify("success", message)
            return
        node_config = dict(payload["node"])
        source_id = deployment_source_id(profile_id)
        current = self._deployed_node(profile)
        other_names = {
            node.name
            for node in self.window.config.imported_nodes
            if node.source_id != source_id
        }
        base_name = str(node_config.get("name") or profile.name)
        node_name = base_name
        suffix = 2
        while node_name in other_names:
            node_name = f"{base_name} ({suffix})"
            suffix += 1
        node_config["name"] = node_name
        node = ImportedNode(
            node_id=current.node_id if current else __import__("secrets").token_hex(8),
            source=f"服务器部署 · {profile.name}",
            config=node_config,
            source_id=source_id,
        )
        self.window.config.imported_nodes = [
            item for item in self.window.config.imported_nodes if item.source_id != source_id
        ]
        self.window.config.imported_nodes.append(node)
        self.window.config.selected_node = node.name
        self.window.config.selected_ssh_server = profile_id
        profile.deployed_node_id = node.node_id
        profile.deployed_at = str(payload.get("deployedAt", ""))
        profile.deployed_version = str(payload.get("version", ""))
        if credential:
            try:
                self.window.credential_store.set(profile_id, credential)
                profile.remember_password = True
                message += "，SSH 凭据已安全保存"
            except CredentialStoreError as exc:
                message += f"；凭据保存失败：{exc}"
        self.window.store.save(self.window.config)
        if self.window.core.is_running:
            self.window._save_and_apply("服务器代理节点已加入内置节点")
        with self._deployment_lock:
            self._deployment_states[profile_id] = {
                "status": "ready",
                "stage": "已部署并加入内置节点",
                "error": "",
            }
        if payload.get("firewall") == "unmanaged":
            message += "；请确认云服务器安全组已放行代理 TCP/UDP 端口"
        self._notify("success", message)

    @Slot()
    def clearLogs(self) -> None:
        self.window.log_view.clear()

    @Slot()
    def openLogs(self) -> None:
        QDesktopServices.openUrl(QUrl.fromLocalFile(str(logs_dir())))

    @Slot(str, result=bool)
    def windowAction(self, action: str) -> bool:
        return self.window.perform_window_action(action)

    def _notify(self, kind: str, message: str) -> None:
        with self._toast_lock:
            self._toasts.append({"kind": kind, "message": message})

    def _drain_toasts(self) -> list[dict[str, str]]:
        with self._toast_lock:
            items = list(self._toasts)
            self._toasts.clear()
        return items


class WebMainWindow(NativeMainWindow):
    def __init__(self, startup_launch: bool = False) -> None:
        super().__init__(startup_launch=startup_launch)
        self.credential_store = CredentialStore(ssh_credentials_path())
        self.server_deployer = ServerProxyDeployer(ssh_known_hosts_path())
        self.web_bridge = WebBridge(self)
        web_root = Path(__file__).resolve().parents[1] / "web"
        allowed_methods = {
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
            "importFile",
            "refreshSubscription",
            "refreshAllSubscriptions",
            "deleteSubscription",
            "deleteNode",
            "selectNode",
            "testExit",
            "testSource",
            "testAllNodes",
            "clearLogs",
            "openLogs",
            "saveSshServer",
            "deploySshServer",
            "copyServerNode",
            "deleteSshServer",
            "pickSshKey",
            "windowAction",
        }
        self.web_server = LocalWebServer(web_root, self.web_bridge, allowed_methods)
        self.web_url = self.web_server.start()
        self._native_central_widget = self.takeCentralWidget()
        if self._native_central_widget is not None:
            self._native_central_widget.hide()
        self.statusBar().hide()
        self.setWindowFlag(Qt.WindowType.FramelessWindowHint, True)
        self.web_view = QWebEngineView(self)
        self.web_view.setObjectName("webGui")
        self.web_view.setContextMenuPolicy(Qt.ContextMenuPolicy.NoContextMenu)
        self.setCentralWidget(self.web_view)
        self.web_view.setUrl(QUrl(f"{self.web_url}?customFrame=1"))

    def open_web_ui(self) -> None:
        if os.environ.get("NETWORK_MANAGER_NO_BROWSER") == "1":
            return
        super().show_and_raise()

    def perform_window_action(self, action: str) -> bool:
        if action == "minimize":
            self.showMinimized()
        elif action == "maximize":
            self.showNormal() if self.isMaximized() else self.showMaximized()
        elif action == "drag":
            handle = self.windowHandle()
            return bool(handle and handle.startSystemMove())
        elif action == "close":
            self.close()
        else:
            return False
        return True

    def show_and_raise(self) -> None:
        self.open_web_ui()

    def shutdown(self) -> None:
        self.web_server.close()
        self.web_bridge.close()
        super().shutdown()
