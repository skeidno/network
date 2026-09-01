from __future__ import annotations

from pathlib import Path

import psutil
from PySide6.QtWidgets import (
    QCheckBox,
    QComboBox,
    QDialog,
    QDialogButtonBox,
    QFileDialog,
    QFormLayout,
    QHBoxLayout,
    QInputDialog,
    QLabel,
    QLineEdit,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QTabWidget,
    QVBoxLayout,
    QWidget,
)

from network_manager.discovery import DiscoveredConfig, discover_configs
from network_manager.models import RoutingRule, normalize_rule_value, validate_rule


RULE_LABELS = {
    "PROCESS-NAME": "程序",
    "DOMAIN": "完整域名",
    "DOMAIN-SUFFIX": "域名后缀",
    "DOMAIN-KEYWORD": "域名关键词",
    "IP-CIDR": "IP / CIDR",
}
TARGET_LABELS = {
    "CLASH": "Clash 本地端口",
    "V2RAY": "v2ray 本地端口",
    "SSH": "SSH 服务器出口",
    "BUILTIN": "内置节点组",
    "DIRECT": "直连",
}


class RuleDialog(QDialog):
    def __init__(self, parent: QWidget | None = None, rule: RoutingRule | None = None) -> None:
        super().__init__(parent)
        self.setWindowTitle("编辑规则" if rule else "添加规则")
        self.setMinimumWidth(480)
        self.rule_type = QComboBox()
        for value, label in RULE_LABELS.items():
            self.rule_type.addItem(label, value)
        self.value = QLineEdit()
        self.value.setPlaceholderText("例如 Discord.exe 或 example.com")
        self.pick_process = QPushButton("选择运行程序")
        self.pick_file = QPushButton("选择 EXE")
        pick_layout = QHBoxLayout()
        pick_layout.setContentsMargins(0, 0, 0, 0)
        pick_layout.addWidget(self.value, 1)
        pick_layout.addWidget(self.pick_process)
        pick_layout.addWidget(self.pick_file)
        pick_widget = QWidget()
        pick_widget.setLayout(pick_layout)
        self.target = QComboBox()
        for value, label in TARGET_LABELS.items():
            self.target.addItem(label, value)
        self.note = QLineEdit()
        self.note.setPlaceholderText("可选")
        self.enabled = QCheckBox("启用这条规则")
        self.enabled.setChecked(True)

        form = QFormLayout()
        form.setFieldGrowthPolicy(QFormLayout.AllNonFixedFieldsGrow)
        form.addRow("匹配类型", self.rule_type)
        form.addRow("匹配内容", pick_widget)
        form.addRow("流量去向", self.target)
        form.addRow("备注", self.note)
        form.addRow("", self.enabled)
        buttons = QDialogButtonBox(QDialogButtonBox.Save | QDialogButtonBox.Cancel)
        buttons.button(QDialogButtonBox.Save).setText("保存")
        buttons.button(QDialogButtonBox.Cancel).setText("取消")

        layout = QVBoxLayout(self)
        layout.addLayout(form)
        layout.addWidget(buttons)
        buttons.accepted.connect(self.accept)
        buttons.rejected.connect(self.reject)
        self.rule_type.currentIndexChanged.connect(self._sync_process_buttons)
        self.pick_process.clicked.connect(self._pick_running_process)
        self.pick_file.clicked.connect(self._pick_executable)

        if rule:
            self.rule_type.setCurrentIndex(self.rule_type.findData(rule.rule_type))
            self.value.setText(rule.value)
            self.target.setCurrentIndex(self.target.findData(rule.target))
            self.note.setText(rule.note)
            self.enabled.setChecked(rule.enabled)
        self._sync_process_buttons()

    def _sync_process_buttons(self) -> None:
        visible = self.rule_type.currentData() == "PROCESS-NAME"
        self.pick_process.setVisible(visible)
        self.pick_file.setVisible(visible)

    def _pick_running_process(self) -> None:
        names: set[str] = set()
        for process in psutil.process_iter(["name"]):
            try:
                name = process.info.get("name")
            except (psutil.AccessDenied, psutil.NoSuchProcess):
                continue
            if name:
                names.add(str(name))
        choices = sorted(names, key=str.lower)
        value, accepted = QInputDialog.getItem(
            self, "选择运行程序", "程序", choices, editable=False
        )
        if accepted and value:
            self.value.setText(value)

    def _pick_executable(self) -> None:
        path, _ = QFileDialog.getOpenFileName(self, "选择程序", "", "Windows 程序 (*.exe)")
        if path:
            self.value.setText(Path(path).name)

    def result_rule(self) -> RoutingRule:
        rule_type = str(self.rule_type.currentData())
        return RoutingRule(
            rule_type=rule_type,
            value=normalize_rule_value(rule_type, self.value.text()),
            target=str(self.target.currentData()),
            enabled=self.enabled.isChecked(),
            note=self.note.text().strip(),
        )

    def accept(self) -> None:
        errors = validate_rule(self.result_rule())
        if errors:
            QMessageBox.warning(self, "规则无效", "\n".join(errors))
            return
        super().accept()


class ImportDialog(QDialog):
    def __init__(self, parent: QWidget | None = None) -> None:
        super().__init__(parent)
        self.setWindowTitle("导入代理节点")
        self.resize(700, 500)
        self.tabs = QTabWidget()
        self.paste_name = QLineEdit("手动导入")
        self.paste_content = QPlainTextEdit()
        self.paste_content.setPlaceholderText(
            "粘贴 Clash YAML、分享链接，或每行一个 主机:端口:用户名:密码 代理 IP"
        )
        paste_page = QWidget()
        paste_layout = QVBoxLayout(paste_page)
        paste_form = QFormLayout()
        paste_form.addRow("来源名称", self.paste_name)
        paste_layout.addLayout(paste_form)
        paste_layout.addWidget(self.paste_content, 1)

        self.file_path = QLineEdit()
        self.file_path.setReadOnly(True)
        self.browse_button = QPushButton("浏览")
        file_row = QHBoxLayout()
        file_row.addWidget(self.file_path, 1)
        file_row.addWidget(self.browse_button)
        file_widget = QWidget()
        file_widget.setLayout(file_row)
        self.detected = QComboBox()
        self.refresh_detected = QPushButton("重新扫描")
        detected_row = QHBoxLayout()
        detected_row.addWidget(self.detected, 1)
        detected_row.addWidget(self.refresh_detected)
        detected_widget = QWidget()
        detected_widget.setLayout(detected_row)
        file_page = QWidget()
        file_layout = QFormLayout(file_page)
        file_layout.addRow("配置文件", file_widget)
        file_layout.addRow("自动发现", detected_widget)
        file_tip = QLabel("Clash YAML 将导入 proxies；v2rayN 数据库将读取已启用订阅。")
        file_tip.setWordWrap(True)
        file_tip.setProperty("muted", True)
        file_layout.addRow("", file_tip)

        self.subscription_name = QLineEdit("我的订阅")
        self.subscription_url = QLineEdit()
        self.subscription_url.setPlaceholderText("https://...")
        subscription_page = QWidget()
        subscription_layout = QFormLayout(subscription_page)
        subscription_layout.addRow("订阅名称", self.subscription_name)
        subscription_layout.addRow("订阅地址", self.subscription_url)

        self.tabs.addTab(paste_page, "粘贴内容")
        self.tabs.addTab(file_page, "配置文件")
        self.tabs.addTab(subscription_page, "订阅地址")
        buttons = QDialogButtonBox(QDialogButtonBox.Ok | QDialogButtonBox.Cancel)
        buttons.button(QDialogButtonBox.Ok).setText("导入")
        buttons.button(QDialogButtonBox.Cancel).setText("取消")
        layout = QVBoxLayout(self)
        layout.addWidget(self.tabs, 1)
        layout.addWidget(buttons)

        buttons.accepted.connect(self.accept)
        buttons.rejected.connect(self.reject)
        self.browse_button.clicked.connect(self._browse)
        self.refresh_detected.clicked.connect(self._discover)
        self.detected.currentIndexChanged.connect(self._select_detected)
        self._discover()

    def _browse(self) -> None:
        path, _ = QFileDialog.getOpenFileName(
            self,
            "选择代理配置",
            "",
            "代理配置 (*.yaml *.yml *.txt *.db);;所有文件 (*)",
        )
        if path:
            self.file_path.setText(path)

    def _discover(self) -> None:
        self.detected.clear()
        configs = discover_configs()
        if not configs:
            self.detected.addItem("未发现配置", None)
            return
        self.detected.addItem("选择已发现的配置", None)
        for item in configs:
            self.detected.addItem(f"{item.product} · {item.path.name}", item)

    def _select_detected(self) -> None:
        item = self.detected.currentData()
        if isinstance(item, DiscoveredConfig):
            self.file_path.setText(str(item.path))

    def import_request(self) -> dict[str, str]:
        if self.tabs.currentIndex() == 0:
            return {
                "kind": "paste",
                "name": self.paste_name.text().strip() or "手动导入",
                "content": self.paste_content.toPlainText(),
            }
        if self.tabs.currentIndex() == 1:
            return {"kind": "file", "path": self.file_path.text().strip()}
        return {
            "kind": "subscription",
            "name": self.subscription_name.text().strip() or "我的订阅",
            "url": self.subscription_url.text().strip(),
        }

    def accept(self) -> None:
        request = self.import_request()
        if request["kind"] == "paste" and not request["content"].strip():
            QMessageBox.warning(self, "缺少内容", "请粘贴配置或分享链接。")
            return
        if request["kind"] == "file" and not Path(request["path"]).is_file():
            QMessageBox.warning(self, "文件无效", "请选择存在的配置文件。")
            return
        if request["kind"] == "subscription" and not request["url"].lower().startswith(
            ("http://", "https://")
        ):
            QMessageBox.warning(self, "地址无效", "订阅地址必须以 http:// 或 https:// 开头。")
            return
        super().accept()
