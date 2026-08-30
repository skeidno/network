from __future__ import annotations

import json
from datetime import datetime, timezone
from pathlib import Path
from typing import Any

from PySide6.QtCore import QSize, QThreadPool, QTimer, Qt, QUrl, Signal
from PySide6.QtGui import QAction, QColor, QCloseEvent, QDesktopServices, QIcon, QPainter
from PySide6.QtNetwork import QNetworkAccessManager, QNetworkReply, QNetworkRequest
from PySide6.QtWidgets import (
    QAbstractItemView,
    QApplication,
    QButtonGroup,
    QCheckBox,
    QComboBox,
    QDialog,
    QFormLayout,
    QFrame,
    QGridLayout,
    QGroupBox,
    QHBoxLayout,
    QHeaderView,
    QLabel,
    QListWidget,
    QListWidgetItem,
    QMainWindow,
    QMenu,
    QMessageBox,
    QPlainTextEdit,
    QPushButton,
    QScrollArea,
    QSpinBox,
    QStackedWidget,
    QStyle,
    QSystemTrayIcon,
    QTabWidget,
    QTableWidget,
    QTableWidgetItem,
    QVBoxLayout,
    QWidget,
)

from network_manager.config_store import ConfigStore
from network_manager.core_manager import CoreManager
from network_manager.discovery import subscriptions_from_v2rayn_db
from network_manager.importers import (
    ImportContentError,
    fetch_subscription,
    import_file,
    parse_import_content,
    prepare_imported_nodes,
    subscription_from_url,
)
from network_manager.mihomo_config import write_mihomo_config
from network_manager.models import SubscriptionSource, Upstream, validate_config
from network_manager.network_probe import (
    detect_download_proxy,
    exit_ip_through_proxy,
    port_is_open,
    test_upstream,
)
from network_manager.paths import (
    app_data_dir,
    core_path,
    generated_config_path,
    logs_dir,
    settings_path,
)
from network_manager.traffic_monitor import (
    TrafficRateTracker,
    format_bytes,
    format_rate,
    parse_connection_snapshot,
)
from network_manager.ui.common import Task
from network_manager.ui.dialogs import ImportDialog, RULE_LABELS, RuleDialog, TARGET_LABELS
from network_manager.ui.traffic_chart import TrafficChart
from network_manager.windows_startup import (
    get_start_with_windows,
    is_admin,
    set_start_with_windows,
)


MODE_LABELS = {
    "RULE": "规则分流",
    "GLOBAL_CLASH": "全局 Clash",
    "GLOBAL_V2RAY": "全局 v2ray",
    "GLOBAL_SSH": "全局 SSH",
    "GLOBAL_BUILTIN": "全局内置节点",
    "DIRECT": "全部直连",
}

MODE_DESCRIPTIONS = {
    "RULE": "按分流规则选择出口",
    "GLOBAL_CLASH": "所有流量经 Clash 转发",
    "GLOBAL_V2RAY": "所有流量经 v2ray 转发",
    "GLOBAL_SSH": "所有流量经当前 SSH 服务器转发",
    "GLOBAL_BUILTIN": "所有流量经当前内置节点转发",
    "DIRECT": "绕过代理，所有流量直接连接",
}


class EmptyStateTableWidget(QTableWidget):
    def __init__(self, rows: int, columns: int, empty_text: str) -> None:
        super().__init__(rows, columns)
        self.empty_text = empty_text

    def paintEvent(self, event: Any) -> None:
        super().paintEvent(event)
        if self.rowCount() > 0:
            return
        painter = QPainter(self.viewport())
        painter.setPen(QColor("#8a949e"))
        painter.drawText(self.viewport().rect(), Qt.AlignCenter, self.empty_text)


class MainWindow(QMainWindow):
    request_quit = Signal()

    def __init__(self, startup_launch: bool = False) -> None:
        super().__init__()
        self.store = ConfigStore(settings_path())
        self.config = self.store.load()
        self.config.start_with_windows = get_start_with_windows()
        self.core = CoreManager(core_path(), app_data_dir(), self.config.controller_port)
        self.thread_pool = QThreadPool.globalInstance()
        self._tasks: set[Task] = set()
        self._operation_active = False
        self._force_close = False
        self._tray_notice_shown = False
        self._last_running = False
        self._startup_launch = startup_launch
        self.traffic_tracker = TrafficRateTracker()
        self.traffic_network = QNetworkAccessManager(self)
        self._traffic_reply: QNetworkReply | None = None
        self._traffic_core_active = False

        self.setWindowTitle("Network Manager")
        self.resize(1180, 780)
        self.setMinimumSize(940, 660)
        icon_path = Path(__file__).resolve().parents[1] / "web" / "icons" / "network-manager.ico"
        self.setWindowIcon(QIcon(str(icon_path)))
        self._build_ui()
        self._build_tray()
        self._load_config_into_ui()

        self.poll_timer = QTimer(self)
        self.poll_timer.setInterval(700)
        self.poll_timer.timeout.connect(self._poll_core)
        self.poll_timer.start()
        self.health_timer = QTimer(self)
        self.health_timer.setInterval(4000)
        self.health_timer.timeout.connect(self._refresh_port_status)
        self.health_timer.start()
        self.traffic_timer = QTimer(self)
        self.traffic_timer.setInterval(1000)
        self.traffic_timer.timeout.connect(self._request_traffic_update)
        self.traffic_timer.start()
        self._refresh_port_status()
        self._set_traffic_inactive()
        self._update_status()

        if self.config.start_on_launch:
            QTimer.singleShot(500, self.start_core)
        if startup_launch:
            QTimer.singleShot(0, self.hide)

    def _build_ui(self) -> None:
        root = QWidget()
        root.setObjectName("appRoot")
        root_layout = QHBoxLayout(root)
        root_layout.setContentsMargins(0, 0, 0, 0)
        root_layout.setSpacing(0)

        sidebar = QFrame()
        sidebar.setObjectName("sidebar")
        sidebar.setFixedWidth(218)
        sidebar_layout = QVBoxLayout(sidebar)
        sidebar_layout.setContentsMargins(16, 22, 16, 18)
        sidebar_layout.setSpacing(0)

        brand_row = QHBoxLayout()
        brand_row.setSpacing(10)
        brand_mark = QLabel()
        brand_mark.setObjectName("brandMark")
        brand_mark.setAlignment(Qt.AlignCenter)
        brand_mark.setFixedSize(38, 38)
        brand_mark.setPixmap(
            self.style().standardIcon(QStyle.SP_DriveNetIcon).pixmap(QSize(23, 23))
        )
        brand_text = QVBoxLayout()
        brand_text.setSpacing(1)
        title = QLabel("Network Manager")
        title.setObjectName("brandTitle")
        subtitle = QLabel("网络与分流")
        subtitle.setObjectName("brandSubtitle")
        brand_text.addWidget(title)
        brand_text.addWidget(subtitle)
        brand_row.addWidget(brand_mark)
        brand_row.addLayout(brand_text, 1)
        sidebar_layout.addLayout(brand_row)
        sidebar_layout.addSpacing(28)

        self.nav_list = QListWidget()
        self.nav_list.setObjectName("navList")
        self.nav_list.setIconSize(QSize(20, 20))
        self.nav_list.setSpacing(4)
        self.nav_list.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        nav_entries = (
            ("运行概览", QStyle.SP_DesktopIcon),
            ("分流规则", QStyle.SP_FileDialogDetailedView),
            ("代理与节点", QStyle.SP_DriveNetIcon),
            ("设置", QStyle.SP_FileDialogContentsView),
            ("运行日志", QStyle.SP_FileDialogInfoView),
        )
        for text, icon in nav_entries:
            item = QListWidgetItem(text)
            item.setIcon(self.style().standardIcon(icon))
            item.setSizeHint(QSize(0, 46))
            self.nav_list.addItem(item)
        sidebar_layout.addWidget(self.nav_list, 1)

        core_panel = QFrame()
        core_panel.setObjectName("sidebarCorePanel")
        core_layout = QVBoxLayout(core_panel)
        core_layout.setContentsMargins(12, 10, 12, 10)
        core_layout.setSpacing(3)
        core_name = QLabel("Mihomo TUN")
        core_name.setObjectName("sidebarCoreName")
        self.sidebar_core_status = QLabel("核心已停止")
        self.sidebar_core_status.setObjectName("sidebarCoreStatus")
        core_layout.addWidget(core_name)
        core_layout.addWidget(self.sidebar_core_status)
        sidebar_layout.addWidget(core_panel)
        sidebar_layout.addSpacing(12)
        version = QLabel("Network Manager  ·  v0.3.0")
        version.setObjectName("sidebarVersion")
        sidebar_layout.addWidget(version)

        content = QWidget()
        content.setObjectName("contentArea")
        content_layout = QVBoxLayout(content)
        content_layout.setContentsMargins(24, 18, 24, 18)
        content_layout.setSpacing(14)
        header = QHBoxLayout()
        header.setContentsMargins(2, 0, 2, 0)
        self.page_title = QLabel("运行概览")
        self.page_title.setObjectName("pageTitle")
        header.addWidget(self.page_title)
        header.addStretch()
        self.header_status = QLabel("已停止")
        self.header_status.setObjectName("statusBadge")
        header.addWidget(self.header_status)
        content_layout.addLayout(header)
        self.pages = QStackedWidget()
        self.pages.addWidget(self._build_overview_page())
        self.pages.addWidget(self._build_rules_page())
        self.pages.addWidget(self._build_nodes_page())
        self.pages.addWidget(self._build_settings_page())
        self.pages.addWidget(self._build_logs_page())
        content_layout.addWidget(self.pages, 1)

        root_layout.addWidget(sidebar)
        root_layout.addWidget(content, 1)
        self.setCentralWidget(root)
        self.nav_list.currentRowChanged.connect(self._change_page)
        self.nav_list.setCurrentRow(0)
        self.statusBar().showMessage("就绪")

    def _create_card(
        self, title: str, icon: QStyle.StandardPixmap, subtitle: str = ""
    ) -> tuple[QFrame, QVBoxLayout]:
        card = QFrame()
        card.setObjectName("card")
        card_layout = QVBoxLayout(card)
        card_layout.setContentsMargins(18, 16, 18, 16)
        card_layout.setSpacing(12)

        header = QHBoxLayout()
        header.setSpacing(11)
        icon_label = QLabel()
        icon_label.setObjectName("cardIcon")
        icon_label.setAlignment(Qt.AlignCenter)
        icon_label.setFixedSize(38, 38)
        icon_label.setPixmap(self.style().standardIcon(icon).pixmap(QSize(20, 20)))
        title_layout = QVBoxLayout()
        title_layout.setSpacing(1)
        title_label = QLabel(title)
        title_label.setObjectName("cardTitle")
        title_layout.addWidget(title_label)
        if subtitle:
            subtitle_label = QLabel(subtitle)
            subtitle_label.setObjectName("cardSubtitle")
            title_layout.addWidget(subtitle_label)
        header.addWidget(icon_label)
        header.addLayout(title_layout, 1)
        card_layout.addLayout(header)
        return card, card_layout

    def _build_overview_page(self) -> QWidget:
        scroll = QScrollArea()
        scroll.setObjectName("overviewScroll")
        scroll.setWidgetResizable(True)
        scroll.setFrameShape(QFrame.Shape.NoFrame)
        scroll.setHorizontalScrollBarPolicy(Qt.ScrollBarPolicy.ScrollBarAlwaysOff)
        page = QWidget()
        page.setObjectName("overviewBody")
        layout = QVBoxLayout(page)
        layout.setContentsMargins(0, 0, 4, 0)
        layout.setSpacing(12)

        if not is_admin():
            admin_banner = QLabel("当前未以管理员身份运行，TUN 接管无法启动。打包版会自动请求 UAC。")
            admin_banner.setObjectName("warningBanner")
            admin_banner.setWordWrap(True)
            layout.addWidget(admin_banner)

        self.status_panel = QFrame()
        self.status_panel.setObjectName("statusPanel")
        status_layout = QHBoxLayout(self.status_panel)
        status_layout.setContentsMargins(18, 16, 18, 16)
        status_layout.setSpacing(14)
        self.status_icon = QLabel()
        self.status_icon.setObjectName("statusIcon")
        self.status_icon.setAlignment(Qt.AlignCenter)
        self.status_icon.setFixedSize(44, 44)
        self.status_icon.setPixmap(
            self.style().standardIcon(QStyle.SP_ComputerIcon).pixmap(QSize(23, 23))
        )
        status_layout.addWidget(self.status_icon)
        status_text = QVBoxLayout()
        status_text.setSpacing(3)
        self.main_status = QLabel("全流量接管已停止")
        self.main_status.setObjectName("mainStatus")
        self.status_detail = QLabel("启动后，TCP、UDP 和 DNS 将按下面的规则处理。")
        self.status_detail.setObjectName("mutedText")
        status_text.addWidget(self.main_status)
        status_text.addWidget(self.status_detail)
        status_layout.addLayout(status_text, 1)
        self.start_button = QPushButton("启动接管")
        self.start_button.setObjectName("primaryButton")
        self.start_button.setIcon(self.style().standardIcon(QStyle.SP_MediaPlay))
        self.start_button.setMinimumSize(132, 42)
        self.start_button.clicked.connect(self.toggle_core)
        status_layout.addWidget(self.start_button)
        layout.addWidget(self.status_panel)

        mode_card, mode_card_layout = self._create_card(
            "代理模式", QStyle.SP_DirLinkIcon, "选择当前的流量处理方式"
        )
        mode_header = mode_card_layout.itemAt(0).layout()
        self.mode_caption = QLabel(MODE_DESCRIPTIONS["RULE"])
        self.mode_caption.setObjectName("modeCaption")
        mode_header.addWidget(self.mode_caption)
        mode_layout = QHBoxLayout()
        mode_layout.setSpacing(0)
        self.mode_group = QButtonGroup(self)
        self.mode_group.setExclusive(True)
        self.mode_buttons: dict[str, QPushButton] = {}
        for index, (mode, label) in enumerate(MODE_LABELS.items()):
            button = QPushButton(label)
            button.setCheckable(True)
            button.setProperty("segment", True)
            if index == 0:
                button.setProperty("segmentPosition", "first")
            elif index == len(MODE_LABELS) - 1:
                button.setProperty("segmentPosition", "last")
            else:
                button.setProperty("segmentPosition", "middle")
            button.setMinimumHeight(38)
            self.mode_group.addButton(button, index)
            self.mode_buttons[mode] = button
            mode_layout.addWidget(button, 1)
        self.mode_group.idClicked.connect(self._mode_clicked)
        mode_card_layout.addLayout(mode_layout)
        layout.addWidget(mode_card)

        grid = QGridLayout()
        grid.setSpacing(12)
        sources, sources_layout = self._create_card(
            "本地代理源", QStyle.SP_DriveNetIcon, "Clash 与 v2ray 兼容端口"
        )
        sources_table = QGridLayout()
        sources_table.setHorizontalSpacing(14)
        sources_table.setVerticalSpacing(10)
        for column, text in enumerate(("来源", "监听地址", "状态")):
            label = QLabel(text)
            label.setObjectName("tableEyebrow")
            sources_table.addWidget(label, 0, column)
        self.clash_overview_endpoint = QLabel()
        self.clash_overview_endpoint.setObjectName("endpointText")
        self.clash_overview_status = QLabel()
        self.v2ray_overview_endpoint = QLabel()
        self.v2ray_overview_endpoint.setObjectName("endpointText")
        self.v2ray_overview_status = QLabel()
        clash_name = QLabel("Clash")
        clash_name.setObjectName("sourceName")
        v2ray_name = QLabel("v2ray")
        v2ray_name.setObjectName("sourceName")
        sources_table.addWidget(clash_name, 1, 0)
        sources_table.addWidget(self.clash_overview_endpoint, 1, 1)
        sources_table.addWidget(self.clash_overview_status, 1, 2, Qt.AlignRight)
        sources_table.addWidget(v2ray_name, 2, 0)
        sources_table.addWidget(self.v2ray_overview_endpoint, 2, 1)
        sources_table.addWidget(self.v2ray_overview_status, 2, 2, Qt.AlignRight)
        sources_table.setColumnStretch(1, 1)
        sources_layout.addLayout(sources_table)

        summary, summary_layout = self._create_card(
            "规则摘要", QStyle.SP_FileDialogDetailedView, "当前生效配置"
        )
        summary_grid = QGridLayout()
        summary_grid.setContentsMargins(0, 0, 0, 0)
        summary_grid.setHorizontalSpacing(20)
        summary_grid.setVerticalSpacing(8)
        self.process_rule_count = QLabel("0")
        self.domain_rule_count = QLabel("0")
        self.node_count = QLabel("0")
        self.default_target_summary = QLabel("直连")
        summary_items = (
            ("程序规则", self.process_rule_count),
            ("域名 / IP", self.domain_rule_count),
            ("内置节点", self.node_count),
            ("默认出口", self.default_target_summary),
        )
        for index, (label_text, value_label) in enumerate(summary_items):
            item = QWidget()
            item_layout = QVBoxLayout(item)
            item_layout.setContentsMargins(0, 0, 0, 0)
            item_layout.setSpacing(1)
            value_label.setObjectName("metricValue")
            label = QLabel(label_text)
            label.setObjectName("metricLabel")
            item_layout.addWidget(value_label)
            item_layout.addWidget(label)
            summary_grid.addWidget(item, index // 2, index % 2)
        summary_layout.addLayout(summary_grid)
        grid.addWidget(sources, 0, 0)
        grid.addWidget(summary, 0, 1)
        grid.setColumnStretch(0, 3)
        grid.setColumnStretch(1, 2)
        layout.addLayout(grid)

        exit_group = QFrame()
        exit_group.setObjectName("card")
        exit_layout = QHBoxLayout(exit_group)
        exit_layout.setContentsMargins(18, 14, 18, 14)
        exit_layout.setSpacing(12)
        exit_icon = QLabel()
        exit_icon.setObjectName("cardIconWarm")
        exit_icon.setAlignment(Qt.AlignCenter)
        exit_icon.setFixedSize(38, 38)
        exit_icon.setPixmap(
            self.style().standardIcon(QStyle.SP_DialogApplyButton).pixmap(QSize(20, 20))
        )
        exit_text = QVBoxLayout()
        exit_text.setSpacing(0)
        exit_title = QLabel("当前出口")
        exit_title.setObjectName("cardTitle")
        self.exit_ip_label = QLabel("尚未检测")
        self.exit_ip_label.setObjectName("exitIp")
        exit_text.addWidget(exit_title)
        exit_text.addWidget(self.exit_ip_label)
        exit_layout.addWidget(exit_icon)
        exit_layout.addLayout(exit_text)
        exit_layout.addStretch()
        self.test_exit_button = QPushButton("检测出口 IP")
        self.test_exit_button.setIcon(self.style().standardIcon(QStyle.SP_BrowserReload))
        self.test_exit_button.clicked.connect(self._test_managed_exit)
        exit_layout.addWidget(self.test_exit_button)
        layout.addWidget(exit_group)
        layout.addWidget(self._build_traffic_card())
        layout.addStretch()
        scroll.setWidget(page)
        return scroll

    def _build_traffic_card(self) -> QFrame:
        card, card_layout = self._create_card(
            "实时流量", QStyle.SP_DriveNetIcon, "最近 60 秒"
        )
        header = card_layout.itemAt(0).layout()
        self.traffic_monitor_status = QLabel("接管停止")
        self.traffic_monitor_status.setObjectName("trafficMonitorStatus")
        header.addWidget(self.traffic_monitor_status)

        metrics = QHBoxLayout()
        metrics.setSpacing(28)

        def add_metric(label_text: str, swatch_name: str = "") -> QLabel:
            metric = QWidget()
            metric_layout = QHBoxLayout(metric)
            metric_layout.setContentsMargins(0, 0, 0, 0)
            metric_layout.setSpacing(8)
            if swatch_name:
                swatch = QLabel()
                swatch.setObjectName(swatch_name)
                swatch.setFixedSize(8, 28)
                metric_layout.addWidget(swatch)
            text_layout = QVBoxLayout()
            text_layout.setSpacing(0)
            value = QLabel("0 B/s" if swatch_name else "0")
            value.setObjectName("trafficRateValue")
            label = QLabel(label_text)
            label.setObjectName("trafficMetricLabel")
            text_layout.addWidget(value)
            text_layout.addWidget(label)
            metric_layout.addLayout(text_layout)
            metrics.addWidget(metric)
            return value

        self.download_rate_label = add_metric("下载", "downloadSwatch")
        self.upload_rate_label = add_metric("上传", "uploadSwatch")
        self.active_connections_label = add_metric("活跃连接")
        metrics.addStretch()
        card_layout.addLayout(metrics)

        self.traffic_chart = TrafficChart()
        card_layout.addWidget(self.traffic_chart)

        totals = QHBoxLayout()
        totals.setContentsMargins(0, 0, 0, 0)
        self.download_total_label = QLabel("累计下载  0 B")
        self.upload_total_label = QLabel("累计上传  0 B")
        for label in (self.download_total_label, self.upload_total_label):
            label.setObjectName("trafficTotal")
            totals.addWidget(label)
        totals.addStretch()
        card_layout.addLayout(totals)
        return card

    def _build_rules_page(self) -> QWidget:
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(12)
        toolbar_panel = QFrame()
        toolbar_panel.setObjectName("pageToolbar")
        toolbar = QHBoxLayout(toolbar_panel)
        toolbar.setContentsMargins(12, 9, 12, 9)
        toolbar.setSpacing(8)
        self.add_rule_button = QPushButton("添加规则")
        self.add_rule_button.setObjectName("primaryButton")
        self.add_rule_button.setIcon(self.style().standardIcon(QStyle.SP_FileDialogNewFolder))
        self.edit_rule_button = QPushButton("编辑")
        self.edit_rule_button.setIcon(self.style().standardIcon(QStyle.SP_FileDialogContentsView))
        self.toggle_rule_button = QPushButton("启用 / 停用")
        self.toggle_rule_button.setIcon(self.style().standardIcon(QStyle.SP_DialogApplyButton))
        self.remove_rule_button = QPushButton("删除")
        self.remove_rule_button.setObjectName("dangerButton")
        self.remove_rule_button.setIcon(self.style().standardIcon(QStyle.SP_TrashIcon))
        for button in (
            self.add_rule_button,
            self.edit_rule_button,
            self.toggle_rule_button,
            self.remove_rule_button,
        ):
            toolbar.addWidget(button)
        self.move_up_button = QPushButton()
        self.move_up_button.setToolTip("上移")
        self.move_up_button.setAccessibleName("上移")
        self.move_up_button.setFixedWidth(36)
        self.move_up_button.setIcon(self.style().standardIcon(QStyle.SP_ArrowUp))
        self.move_down_button = QPushButton()
        self.move_down_button.setToolTip("下移")
        self.move_down_button.setAccessibleName("下移")
        self.move_down_button.setFixedWidth(36)
        self.move_down_button.setIcon(self.style().standardIcon(QStyle.SP_ArrowDown))
        for button in (self.move_up_button, self.move_down_button):
            toolbar.addWidget(button)
        toolbar.addStretch()
        layout.addWidget(toolbar_panel)

        self.rules_table = EmptyStateTableWidget(0, 5, "暂无分流规则")
        self.rules_table.setHorizontalHeaderLabels(["状态", "类型", "匹配内容", "去向", "备注"])
        self.rules_table.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.rules_table.setSelectionMode(QAbstractItemView.SingleSelection)
        self.rules_table.setEditTriggers(QAbstractItemView.NoEditTriggers)
        self.rules_table.setAlternatingRowColors(True)
        self.rules_table.verticalHeader().setVisible(False)
        header = self.rules_table.horizontalHeader()
        header.setSectionResizeMode(0, QHeaderView.ResizeToContents)
        header.setSectionResizeMode(1, QHeaderView.ResizeToContents)
        header.setSectionResizeMode(2, QHeaderView.Stretch)
        header.setSectionResizeMode(3, QHeaderView.ResizeToContents)
        header.setSectionResizeMode(4, QHeaderView.Stretch)
        layout.addWidget(self.rules_table, 1)

        self.add_rule_button.clicked.connect(self._add_rule)
        self.edit_rule_button.clicked.connect(self._edit_rule)
        self.toggle_rule_button.clicked.connect(self._toggle_rule)
        self.remove_rule_button.clicked.connect(self._remove_rule)
        self.move_up_button.clicked.connect(lambda: self._move_rule(-1))
        self.move_down_button.clicked.connect(lambda: self._move_rule(1))
        self.rules_table.doubleClicked.connect(self._edit_rule)
        return page

    def _build_nodes_page(self) -> QWidget:
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(0, 0, 0, 0)
        self.node_tabs = QTabWidget()
        self.node_tabs.addTab(self._build_local_sources_tab(), "本地兼容")
        self.node_tabs.addTab(self._build_imported_nodes_tab(), "内置节点")
        self.node_tabs.addTab(self._build_subscriptions_tab(), "订阅")
        layout.addWidget(self.node_tabs)
        return page

    def _build_local_sources_tab(self) -> QWidget:
        page = QWidget()
        layout = QVBoxLayout(page)
        tip = QLabel(
            "兼容模式会把现有 Clash 和 v2ray 的本地端口作为上游；代理核心进程会被强制直连以避免回环。"
        )
        tip.setWordWrap(True)
        tip.setObjectName("mutedText")
        layout.addWidget(tip)
        self.source_widgets: dict[str, dict[str, QWidget]] = {}
        for key, title in (("clash", "Clash 本地端口"), ("v2ray", "v2ray 本地端口")):
            group = QGroupBox(title)
            grid = QGridLayout(group)
            enabled = QCheckBox("启用")
            protocol = QComboBox()
            protocol.addItem("SOCKS5", "socks5")
            protocol.addItem("HTTP / Mixed", "http")
            host = QComboBox()
            host.setEditable(True)
            host.addItems(["127.0.0.1", "localhost"])
            port = QSpinBox()
            port.setRange(1, 65535)
            status = QLabel("未检测")
            test_button = QPushButton("测试出口")
            save_button = QPushButton("保存")
            grid.addWidget(enabled, 0, 0)
            grid.addWidget(QLabel("协议"), 0, 1)
            grid.addWidget(protocol, 0, 2)
            grid.addWidget(QLabel("地址"), 0, 3)
            grid.addWidget(host, 0, 4)
            grid.addWidget(QLabel("端口"), 0, 5)
            grid.addWidget(port, 0, 6)
            grid.addWidget(status, 1, 0, 1, 5)
            grid.addWidget(test_button, 1, 5)
            grid.addWidget(save_button, 1, 6)
            self.source_widgets[key] = {
                "enabled": enabled,
                "protocol": protocol,
                "host": host,
                "port": port,
                "status": status,
            }
            test_button.clicked.connect(lambda _checked=False, source=key: self._test_source(source))
            save_button.clicked.connect(self._save_local_sources)
            layout.addWidget(group)
        layout.addStretch()
        return page

    def _build_imported_nodes_tab(self) -> QWidget:
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(14, 14, 14, 14)
        layout.setSpacing(12)
        toolbar_panel = QFrame()
        toolbar_panel.setObjectName("pageToolbar")
        toolbar = QHBoxLayout(toolbar_panel)
        toolbar.setContentsMargins(12, 9, 12, 9)
        toolbar.setSpacing(8)
        self.import_button = QPushButton("导入节点")
        self.import_button.setObjectName("primaryButton")
        self.import_button.setIcon(self.style().standardIcon(QStyle.SP_DialogOpenButton))
        self.remove_node_button = QPushButton("删除选中")
        self.remove_node_button.setObjectName("dangerButton")
        self.remove_node_button.setIcon(self.style().standardIcon(QStyle.SP_TrashIcon))
        toolbar.addWidget(self.import_button)
        toolbar.addWidget(self.remove_node_button)
        toolbar.addStretch()
        toolbar.addWidget(QLabel("当前内置节点"))
        self.selected_node_combo = QComboBox()
        self.selected_node_combo.setMinimumWidth(240)
        toolbar.addWidget(self.selected_node_combo)
        layout.addWidget(toolbar_panel)
        self.nodes_table = EmptyStateTableWidget(0, 4, "暂无已导入节点")
        self.nodes_table.setHorizontalHeaderLabels(["名称", "协议", "服务器", "来源"])
        self.nodes_table.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.nodes_table.setSelectionMode(QAbstractItemView.ExtendedSelection)
        self.nodes_table.setEditTriggers(QAbstractItemView.NoEditTriggers)
        self.nodes_table.setAlternatingRowColors(True)
        self.nodes_table.verticalHeader().setVisible(False)
        header = self.nodes_table.horizontalHeader()
        header.setSectionResizeMode(0, QHeaderView.Stretch)
        header.setSectionResizeMode(1, QHeaderView.ResizeToContents)
        header.setSectionResizeMode(2, QHeaderView.Stretch)
        header.setSectionResizeMode(3, QHeaderView.Stretch)
        layout.addWidget(self.nodes_table, 1)
        self.import_button.clicked.connect(self._open_import_dialog)
        self.remove_node_button.clicked.connect(self._remove_nodes)
        self.selected_node_combo.currentIndexChanged.connect(self._selected_node_changed)
        return page

    def _build_subscriptions_tab(self) -> QWidget:
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(14, 14, 14, 14)
        layout.setSpacing(12)
        toolbar_panel = QFrame()
        toolbar_panel.setObjectName("pageToolbar")
        toolbar = QHBoxLayout(toolbar_panel)
        toolbar.setContentsMargins(12, 9, 12, 9)
        toolbar.setSpacing(8)
        self.add_subscription_button = QPushButton("添加订阅")
        self.add_subscription_button.setObjectName("primaryButton")
        self.add_subscription_button.setIcon(
            self.style().standardIcon(QStyle.SP_FileDialogNewFolder)
        )
        self.refresh_subscription_button = QPushButton("刷新选中")
        self.refresh_subscription_button.setIcon(
            self.style().standardIcon(QStyle.SP_BrowserReload)
        )
        self.refresh_all_subscriptions_button = QPushButton("刷新全部")
        self.refresh_all_subscriptions_button.setIcon(
            self.style().standardIcon(QStyle.SP_DialogResetButton)
        )
        self.remove_subscription_button = QPushButton("删除订阅")
        self.remove_subscription_button.setObjectName("dangerButton")
        self.remove_subscription_button.setIcon(self.style().standardIcon(QStyle.SP_TrashIcon))
        toolbar.addWidget(self.add_subscription_button)
        toolbar.addWidget(self.refresh_subscription_button)
        toolbar.addWidget(self.refresh_all_subscriptions_button)
        toolbar.addWidget(self.remove_subscription_button)
        toolbar.addStretch()
        layout.addWidget(toolbar_panel)
        self.subscriptions_table = EmptyStateTableWidget(0, 3, "暂无订阅记录")
        self.subscriptions_table.setHorizontalHeaderLabels(["名称", "节点数", "最后更新"])
        self.subscriptions_table.setSelectionBehavior(QAbstractItemView.SelectRows)
        self.subscriptions_table.setSelectionMode(QAbstractItemView.SingleSelection)
        self.subscriptions_table.setEditTriggers(QAbstractItemView.NoEditTriggers)
        self.subscriptions_table.verticalHeader().setVisible(False)
        header = self.subscriptions_table.horizontalHeader()
        header.setSectionResizeMode(0, QHeaderView.Stretch)
        header.setSectionResizeMode(1, QHeaderView.ResizeToContents)
        header.setSectionResizeMode(2, QHeaderView.Stretch)
        layout.addWidget(self.subscriptions_table, 1)
        self.add_subscription_button.clicked.connect(self._open_subscription_dialog)
        self.refresh_subscription_button.clicked.connect(self._refresh_selected_subscription)
        self.refresh_all_subscriptions_button.clicked.connect(self._refresh_all_subscriptions)
        self.remove_subscription_button.clicked.connect(self._remove_subscription)
        return page

    def _build_settings_page(self) -> QWidget:
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(0, 0, 0, 0)
        network_group = QGroupBox("本地服务")
        network_form = QFormLayout(network_group)
        self.mixed_port_spin = QSpinBox()
        self.controller_port_spin = QSpinBox()
        self.dns_port_spin = QSpinBox()
        for spin in (self.mixed_port_spin, self.controller_port_spin, self.dns_port_spin):
            spin.setRange(1024, 65535)
        network_form.addRow("HTTP / SOCKS 入口端口", self.mixed_port_spin)
        network_form.addRow("核心控制端口", self.controller_port_spin)
        network_form.addRow("内部 DNS 端口", self.dns_port_spin)

        behavior_group = QGroupBox("运行行为")
        behavior_layout = QVBoxLayout(behavior_group)
        self.strict_route_check = QCheckBox("严格路由，减少 DNS 和流量泄漏")
        self.start_on_launch_check = QCheckBox("打开软件后自动启动接管")
        self.close_to_tray_check = QCheckBox("关闭窗口时继续在托盘运行")
        self.start_with_windows_check = QCheckBox("随 Windows 登录启动")
        for checkbox in (
            self.strict_route_check,
            self.start_on_launch_check,
            self.close_to_tray_check,
            self.start_with_windows_check,
        ):
            behavior_layout.addWidget(checkbox)
        self.save_settings_button = QPushButton("保存设置")
        self.save_settings_button.setObjectName("primaryButton")
        self.save_settings_button.setIcon(self.style().standardIcon(QStyle.SP_DialogSaveButton))
        self.save_settings_button.setMinimumWidth(118)
        self.save_settings_button.clicked.connect(self._save_settings)
        layout.addWidget(network_group)
        layout.addWidget(behavior_group)
        layout.addWidget(self.save_settings_button, 0, Qt.AlignLeft)
        layout.addStretch()
        return page

    def _build_logs_page(self) -> QWidget:
        page = QWidget()
        layout = QVBoxLayout(page)
        layout.setContentsMargins(0, 0, 0, 0)
        layout.setSpacing(12)
        toolbar_panel = QFrame()
        toolbar_panel.setObjectName("pageToolbar")
        toolbar = QHBoxLayout(toolbar_panel)
        toolbar.setContentsMargins(12, 9, 12, 9)
        toolbar.setSpacing(8)
        clear_button = QPushButton("清空显示")
        clear_button.setIcon(self.style().standardIcon(QStyle.SP_DialogResetButton))
        open_button = QPushButton("打开日志目录")
        open_button.setIcon(self.style().standardIcon(QStyle.SP_DirOpenIcon))
        toolbar.addWidget(clear_button)
        toolbar.addWidget(open_button)
        toolbar.addStretch()
        layout.addWidget(toolbar_panel)
        self.log_view = QPlainTextEdit()
        self.log_view.setObjectName("logView")
        self.log_view.setPlaceholderText("暂无运行日志")
        self.log_view.setReadOnly(True)
        self.log_view.setMaximumBlockCount(3000)
        self.log_view.setLineWrapMode(QPlainTextEdit.NoWrap)
        layout.addWidget(self.log_view, 1)
        clear_button.clicked.connect(self.log_view.clear)
        open_button.clicked.connect(
            lambda: QDesktopServices.openUrl(QUrl.fromLocalFile(str(logs_dir())))
        )
        return page

    def _build_tray(self) -> None:
        self.tray = QSystemTrayIcon(self.windowIcon(), self)
        self.tray.setToolTip("Network Manager")
        menu = QMenu()
        show_action = QAction("打开主窗口", self)
        self.tray_toggle_action = QAction("启动接管", self)
        mode_menu = menu.addMenu("运行模式")
        for mode, label in MODE_LABELS.items():
            action = QAction(label, self)
            action.triggered.connect(lambda _checked=False, value=mode: self._set_mode(value))
            mode_menu.addAction(action)
        exit_action = QAction("退出并停止接管", self)
        menu.insertAction(mode_menu.menuAction(), show_action)
        menu.insertAction(mode_menu.menuAction(), self.tray_toggle_action)
        menu.addSeparator()
        menu.addAction(exit_action)
        self.tray.setContextMenu(menu)
        show_action.triggered.connect(self.show_and_raise)
        self.tray_toggle_action.triggered.connect(self.toggle_core)
        exit_action.triggered.connect(self.quit_application)
        self.tray.activated.connect(self._tray_activated)
        self.tray.show()

    def _change_page(self, index: int) -> None:
        if index < 0:
            return
        self.pages.setCurrentIndex(index)
        self.page_title.setText(self.nav_list.item(index).text())

    def _load_config_into_ui(self) -> None:
        self._refresh_rules_table()
        self._refresh_nodes_table()
        self._refresh_subscriptions_table()
        for key, upstream in (("clash", self.config.clash), ("v2ray", self.config.v2ray)):
            widgets = self.source_widgets[key]
            widgets["enabled"].setChecked(upstream.enabled)
            widgets["protocol"].setCurrentIndex(
                widgets["protocol"].findData(upstream.protocol)
            )
            widgets["host"].setCurrentText(upstream.host)
            widgets["port"].setValue(upstream.port)
        self.mixed_port_spin.setValue(self.config.mixed_port)
        self.controller_port_spin.setValue(self.config.controller_port)
        self.dns_port_spin.setValue(self.config.dns_port)
        self.strict_route_check.setChecked(self.config.strict_route)
        self.start_on_launch_check.setChecked(self.config.start_on_launch)
        self.close_to_tray_check.setChecked(self.config.close_to_tray)
        self.start_with_windows_check.setChecked(self.config.start_with_windows)
        for mode, button in self.mode_buttons.items():
            button.setChecked(mode == self.config.mode)
        self.mode_caption.setText(
            MODE_DESCRIPTIONS.get(self.config.mode, MODE_DESCRIPTIONS["RULE"])
        )
        self._update_summary()

    def _refresh_rules_table(self) -> None:
        self.rules_table.setRowCount(len(self.config.rules))
        for row, rule in enumerate(self.config.rules):
            values = (
                "启用" if rule.enabled else "停用",
                RULE_LABELS.get(rule.rule_type, rule.rule_type),
                rule.value,
                TARGET_LABELS.get(rule.target, rule.target),
                rule.note,
            )
            for column, value in enumerate(values):
                item = QTableWidgetItem(value)
                if column == 0:
                    item.setTextAlignment(Qt.AlignCenter)
                self.rules_table.setItem(row, column, item)
        self._update_summary()

    def _refresh_nodes_table(self) -> None:
        self.nodes_table.setRowCount(len(self.config.imported_nodes))
        for row, node in enumerate(self.config.imported_nodes):
            server = str(node.config.get("server", ""))
            port = node.config.get("port")
            endpoint = f"{server}:{port}" if port else server
            for column, value in enumerate((node.name, node.protocol, endpoint, node.source)):
                self.nodes_table.setItem(row, column, QTableWidgetItem(str(value)))
        self.selected_node_combo.blockSignals(True)
        self.selected_node_combo.clear()
        for node in self.config.imported_nodes:
            self.selected_node_combo.addItem(node.name, node.name)
        selected_index = self.selected_node_combo.findData(self.config.selected_node)
        if selected_index < 0 and self.selected_node_combo.count():
            selected_index = 0
            self.config.selected_node = str(self.selected_node_combo.itemData(0))
        self.selected_node_combo.setCurrentIndex(selected_index)
        self.selected_node_combo.blockSignals(False)
        self.mode_buttons["GLOBAL_BUILTIN"].setEnabled(bool(self.config.imported_nodes))
        self._update_summary()

    def _refresh_subscriptions_table(self) -> None:
        self.subscriptions_table.setRowCount(len(self.config.subscriptions))
        for row, source in enumerate(self.config.subscriptions):
            count = sum(
                1 for node in self.config.imported_nodes if node.source_id == source.source_id
            )
            for column, value in enumerate((source.name, str(count), source.last_updated or "未更新")):
                self.subscriptions_table.setItem(row, column, QTableWidgetItem(value))

    def _selected_row(self, table: QTableWidget) -> int:
        rows = table.selectionModel().selectedRows()
        return rows[0].row() if rows else -1

    def _add_rule(self) -> None:
        dialog = RuleDialog(self)
        if dialog.exec() == QDialog.Accepted:
            self.config.rules.append(dialog.result_rule())
            self._save_and_apply("规则已添加")
            self._refresh_rules_table()

    def _edit_rule(self) -> None:
        row = self._selected_row(self.rules_table)
        if row < 0:
            self.statusBar().showMessage("请先选择一条规则", 3000)
            return
        dialog = RuleDialog(self, self.config.rules[row])
        if dialog.exec() == QDialog.Accepted:
            self.config.rules[row] = dialog.result_rule()
            self._save_and_apply("规则已更新")
            self._refresh_rules_table()

    def _toggle_rule(self) -> None:
        row = self._selected_row(self.rules_table)
        if row < 0:
            return
        self.config.rules[row].enabled = not self.config.rules[row].enabled
        self._save_and_apply("规则状态已更新")
        self._refresh_rules_table()
        self.rules_table.selectRow(row)

    def _remove_rule(self) -> None:
        row = self._selected_row(self.rules_table)
        if row < 0:
            return
        rule = self.config.rules[row]
        answer = QMessageBox.question(
            self, "删除规则", f"确定删除规则“{rule.value}”吗？", QMessageBox.Yes | QMessageBox.No
        )
        if answer == QMessageBox.Yes:
            self.config.rules.pop(row)
            self._save_and_apply("规则已删除")
            self._refresh_rules_table()

    def _move_rule(self, offset: int) -> None:
        row = self._selected_row(self.rules_table)
        target = row + offset
        if row < 0 or target < 0 or target >= len(self.config.rules):
            return
        self.config.rules[row], self.config.rules[target] = (
            self.config.rules[target],
            self.config.rules[row],
        )
        self._save_and_apply("规则顺序已更新")
        self._refresh_rules_table()
        self.rules_table.selectRow(target)

    def _source_from_form(self, key: str) -> Upstream:
        widgets = self.source_widgets[key]
        return Upstream(
            name="Clash 7897" if key == "clash" else "v2ray 10808",
            host=widgets["host"].currentText().strip(),
            port=widgets["port"].value(),
            protocol=str(widgets["protocol"].currentData()),
            enabled=widgets["enabled"].isChecked(),
        )

    def _save_local_sources(self) -> None:
        self.config.clash = self._source_from_form("clash")
        self.config.v2ray = self._source_from_form("v2ray")
        if self._save_and_apply("本地代理源已保存"):
            self._refresh_port_status()

    def _test_source(self, key: str) -> None:
        upstream = self._source_from_form(key)
        status = self.source_widgets[key]["status"]
        status.setText("正在测试...")

        def success(result: object) -> None:
            ok, message = result
            status.setText(f"可用 · 出口 {message}" if ok else f"不可用 · {message}")
            status.setProperty("state", "ok" if ok else "error")
            self._repolish(status)

        self._run_task(lambda: test_upstream(upstream), success, "代理测试失败")

    def _open_import_dialog(self) -> None:
        dialog = ImportDialog(self)
        if dialog.exec() == QDialog.Accepted:
            self._execute_import(dialog.import_request())

    def _open_subscription_dialog(self) -> None:
        dialog = ImportDialog(self)
        dialog.tabs.setCurrentIndex(2)
        if dialog.exec() == QDialog.Accepted:
            self._execute_import(dialog.import_request())

    def _execute_import(self, request: dict[str, str]) -> None:
        self.import_button.setEnabled(False)
        proxy = detect_download_proxy(self.config.clash, self.config.v2ray)

        def work() -> dict[str, Any]:
            kind = request["kind"]
            if kind == "paste":
                raw_nodes, errors = parse_import_content(request["content"])
                return {"batches": [(request["name"], "", raw_nodes)], "errors": errors}
            if kind == "subscription":
                source = subscription_from_url(request["name"], request["url"])
                content = fetch_subscription(source.url, proxy)
                raw_nodes, errors = parse_import_content(content)
                return {
                    "batches": [(source.name, source.source_id, raw_nodes)],
                    "subscriptions": [source],
                    "errors": errors,
                }
            path = Path(request["path"])
            if path.suffix.lower() == ".db":
                sources = subscriptions_from_v2rayn_db(path)
                if not sources:
                    raise ImportContentError("v2rayN 数据库中没有已启用订阅")
                batches = []
                errors: list[str] = []
                updated_sources = []
                for source in sources:
                    try:
                        content = fetch_subscription(source.url, proxy)
                        raw_nodes, parse_errors = parse_import_content(content)
                        source.last_updated = datetime.now(timezone.utc).isoformat(timespec="seconds")
                        batches.append((source.name, source.source_id, raw_nodes))
                        errors.extend(f"{source.name}：{item}" for item in parse_errors)
                        updated_sources.append(source)
                    except Exception as exc:
                        errors.append(f"{source.name}：{exc}")
                if not batches:
                    raise ImportContentError(errors[0] if errors else "订阅下载失败")
                return {
                    "batches": batches,
                    "subscriptions": updated_sources,
                    "errors": errors,
                }
            raw_nodes, errors = import_file(path)
            return {"batches": [(path.stem, "", raw_nodes)], "errors": errors}

        def success(result: object) -> None:
            self._accept_import_result(result)

        self._run_task(work, success, "导入失败", lambda: self.import_button.setEnabled(True))

    def _accept_import_result(self, result: object) -> None:
        payload = dict(result)
        added = 0
        for source_name, source_id, raw_nodes in payload.get("batches", []):
            if source_id:
                self.config.imported_nodes = [
                    node for node in self.config.imported_nodes if node.source_id != source_id
                ]
            prepared = prepare_imported_nodes(
                raw_nodes,
                source_name,
                self.config.imported_nodes,
                source_id=source_id,
            )
            self.config.imported_nodes.extend(prepared)
            added += len(prepared)
        for source in payload.get("subscriptions", []):
            self.config.subscriptions = [
                item for item in self.config.subscriptions if item.source_id != source.source_id
            ]
            self.config.subscriptions.append(source)
        if not self.config.selected_node and self.config.imported_nodes:
            self.config.selected_node = self.config.imported_nodes[0].name
        if not self._save_and_apply(f"已导入 {added} 个节点"):
            return
        self._refresh_nodes_table()
        self._refresh_subscriptions_table()
        errors = payload.get("errors", [])
        if errors:
            QMessageBox.warning(
                self,
                "部分内容未导入",
                f"成功导入 {added} 个节点，另有 {len(errors)} 项失败。\n\n"
                + "\n".join(errors[:8]),
            )
        else:
            self.statusBar().showMessage(f"成功导入 {added} 个节点", 5000)

    def _remove_nodes(self) -> None:
        rows = sorted({index.row() for index in self.nodes_table.selectionModel().selectedRows()})
        if not rows:
            return
        answer = QMessageBox.question(
            self,
            "删除节点",
            f"确定删除选中的 {len(rows)} 个节点吗？",
            QMessageBox.Yes | QMessageBox.No,
        )
        if answer != QMessageBox.Yes:
            return
        removed_names = {self.config.imported_nodes[row].name for row in rows}
        self.config.imported_nodes = [
            node for index, node in enumerate(self.config.imported_nodes) if index not in rows
        ]
        if self.config.selected_node in removed_names:
            self.config.selected_node = (
                self.config.imported_nodes[0].name if self.config.imported_nodes else ""
            )
        if self.config.mode == "GLOBAL_BUILTIN" and not self.config.imported_nodes:
            self.config.mode = "RULE"
        self._save_and_apply("节点已删除")
        self._refresh_nodes_table()
        self._refresh_subscriptions_table()
        self._load_mode_buttons()

    def _selected_node_changed(self) -> None:
        value = self.selected_node_combo.currentData()
        if value and value != self.config.selected_node:
            self.config.selected_node = str(value)
            self._save_and_apply("内置节点已切换")

    def _subscription_at_row(self, row: int) -> SubscriptionSource | None:
        if 0 <= row < len(self.config.subscriptions):
            return self.config.subscriptions[row]
        return None

    def _refresh_selected_subscription(self) -> None:
        source = self._subscription_at_row(self._selected_row(self.subscriptions_table))
        if source:
            self._refresh_sources([source])

    def _refresh_all_subscriptions(self) -> None:
        if self.config.subscriptions:
            self._refresh_sources(list(self.config.subscriptions))

    def _refresh_sources(self, sources: list[SubscriptionSource]) -> None:
        proxy = detect_download_proxy(self.config.clash, self.config.v2ray)

        def work() -> dict[str, Any]:
            batches = []
            errors: list[str] = []
            updated: list[SubscriptionSource] = []
            for source in sources:
                try:
                    content = fetch_subscription(source.url, proxy)
                    nodes, parse_errors = parse_import_content(content)
                    refreshed = SubscriptionSource(
                        source.source_id,
                        source.name,
                        source.url,
                        datetime.now(timezone.utc).isoformat(timespec="seconds"),
                    )
                    batches.append((source.name, source.source_id, nodes))
                    updated.append(refreshed)
                    errors.extend(f"{source.name}：{error}" for error in parse_errors)
                except Exception as exc:
                    errors.append(f"{source.name}：{exc}")
            if not batches:
                raise ImportContentError(errors[0] if errors else "没有订阅可刷新")
            return {"batches": batches, "subscriptions": updated, "errors": errors}

        self._run_task(work, self._accept_import_result, "订阅刷新失败")

    def _remove_subscription(self) -> None:
        row = self._selected_row(self.subscriptions_table)
        source = self._subscription_at_row(row)
        if source is None:
            return
        answer = QMessageBox.question(
            self,
            "删除订阅",
            f"删除“{source.name}”以及它导入的节点吗？",
            QMessageBox.Yes | QMessageBox.No,
        )
        if answer != QMessageBox.Yes:
            return
        removed_names = {
            node.name for node in self.config.imported_nodes if node.source_id == source.source_id
        }
        self.config.subscriptions.pop(row)
        self.config.imported_nodes = [
            node for node in self.config.imported_nodes if node.source_id != source.source_id
        ]
        if self.config.selected_node in removed_names:
            self.config.selected_node = (
                self.config.imported_nodes[0].name if self.config.imported_nodes else ""
            )
        self._save_and_apply("订阅已删除")
        self._refresh_nodes_table()
        self._refresh_subscriptions_table()

    def _save_settings(self) -> None:
        old_controller_port = self.config.controller_port
        self.config.mixed_port = self.mixed_port_spin.value()
        self.config.controller_port = self.controller_port_spin.value()
        self.config.dns_port = self.dns_port_spin.value()
        self.config.strict_route = self.strict_route_check.isChecked()
        self.config.start_on_launch = self.start_on_launch_check.isChecked()
        self.config.close_to_tray = self.close_to_tray_check.isChecked()
        self.config.start_with_windows = self.start_with_windows_check.isChecked()
        try:
            set_start_with_windows(self.config.start_with_windows)
        except OSError as exc:
            QMessageBox.warning(self, "启动项设置失败", str(exc))
            return
        if old_controller_port != self.config.controller_port:
            self.core.controller_port = self.config.controller_port
        self._save_and_apply("设置已保存")

    def _load_mode_buttons(self) -> None:
        for mode, button in self.mode_buttons.items():
            button.setChecked(mode == self.config.mode)

    def _mode_clicked(self, button_id: int) -> None:
        mode = list(MODE_LABELS)[button_id]
        self._set_mode(mode)

    def _set_mode(self, mode: str) -> None:
        if mode == "GLOBAL_BUILTIN" and not self.config.imported_nodes:
            QMessageBox.information(self, "没有内置节点", "请先导入至少一个节点。")
            self._load_mode_buttons()
            return
        if mode == "GLOBAL_SSH" and not self.config.selected_ssh_server:
            QMessageBox.information(self, "没有 SSH 服务器", "请先添加并连接 SSH 服务器。")
            self._load_mode_buttons()
            return
        self.config.mode = mode
        if not self._save_and_apply(f"已切换为{MODE_LABELS[mode]}"):
            return
        self._load_mode_buttons()
        self.mode_caption.setText(MODE_DESCRIPTIONS.get(mode, ""))

    def _save_and_apply(self, message: str) -> bool:
        errors = validate_config(self.config)
        if errors:
            QMessageBox.warning(self, "配置无效", "\n".join(errors[:12]))
            return False
        self.store.save(self.config)
        write_mihomo_config(self.config, generated_config_path())
        self.statusBar().showMessage(message, 4000)
        if self.core.is_running:
            self._restart_core_async()
        self._update_summary()
        return True

    def toggle_core(self) -> bool:
        if self._operation_active:
            return False
        if self.core.is_running:
            self.stop_core()
        else:
            self.start_core()
        return self._operation_active

    def start_core(self) -> None:
        if self._operation_active:
            return
        if not is_admin():
            QMessageBox.warning(self, "需要管理员权限", "TUN 接管必须以管理员身份运行。")
            return
        errors = validate_config(self.config)
        if errors:
            QMessageBox.warning(self, "配置无效", "\n".join(errors[:12]))
            return
        self.store.save(self.config)
        write_mihomo_config(self.config, generated_config_path())
        self._operation_active = True
        self._update_status("正在启动 TUN 核心...")

        def success(_result: object) -> None:
            self.statusBar().showMessage("全流量接管已启动", 5000)
            self._update_status()

        def finished() -> None:
            self._operation_active = False
            self._update_status()

        self._run_task(
            lambda: self.core.start(generated_config_path()),
            success,
            "启动失败",
            finished,
        )

    def stop_core(self) -> None:
        if self._operation_active:
            return
        self._operation_active = True
        self._update_status("正在停止并恢复网络...")

        def finished() -> None:
            self._operation_active = False
            self._update_status()

        self._run_task(self.core.stop, lambda _result: None, "停止失败", finished)

    def _restart_core_async(self) -> None:
        if self._operation_active:
            return
        self._operation_active = True
        self._update_status("正在应用配置...")

        def finished() -> None:
            self._operation_active = False
            self._update_status()

        self._run_task(
            lambda: self.core.restart(generated_config_path()),
            lambda _result: self.statusBar().showMessage("配置已应用", 3000),
            "应用配置失败",
            finished,
        )

    def _test_managed_exit(self) -> None:
        if not self.core.is_running:
            QMessageBox.information(self, "尚未启动", "请先启动全流量接管。")
            return
        self.exit_ip_label.setText("正在检测...")
        url = f"http://127.0.0.1:{self.config.mixed_port}"

        def success(result: object) -> None:
            self.exit_ip_label.setText(str(result))

        self._run_task(lambda: exit_ip_through_proxy(url), success, "出口检测失败")

    def _refresh_port_status(self) -> None:
        for key, upstream, endpoint_label, status_label in (
            ("clash", self.config.clash, self.clash_overview_endpoint, self.clash_overview_status),
            ("v2ray", self.config.v2ray, self.v2ray_overview_endpoint, self.v2ray_overview_status),
        ):
            endpoint_label.setText(f"{upstream.host}:{upstream.port}")
            if not upstream.enabled:
                status_label.setText("已禁用")
                state = "muted"
            elif port_is_open(upstream.host, upstream.port):
                status_label.setText("正在监听")
                state = "ok"
            else:
                status_label.setText("端口未监听")
                state = "error"
            status_label.setProperty("state", state)
            self._repolish(status_label)
            widgets = self.source_widgets.get(key)
            if widgets:
                widgets["status"].setText(status_label.text())
                widgets["status"].setProperty("state", state)
                self._repolish(widgets["status"])

    def _update_summary(self) -> None:
        if not hasattr(self, "process_rule_count"):
            return
        process_count = sum(
            1 for rule in self.config.rules if rule.enabled and rule.rule_type == "PROCESS-NAME"
        )
        domain_count = sum(
            1 for rule in self.config.rules if rule.enabled and rule.rule_type != "PROCESS-NAME"
        )
        self.process_rule_count.setText(str(process_count))
        self.domain_rule_count.setText(str(domain_count))
        self.node_count.setText(str(len(self.config.imported_nodes)))
        self.default_target_summary.setText(
            TARGET_LABELS.get(self.config.default_target, self.config.default_target)
        )

    def _poll_core(self) -> None:
        lines = self.core.drain_logs()
        if lines:
            self.log_view.appendPlainText("\n".join(lines))
            self._append_log_file(lines)
        running = self.core.is_running
        if self._last_running and not running and not self._operation_active:
            self.statusBar().showMessage("TUN 核心意外停止，请查看运行日志", 7000)
        self._last_running = running
        self._update_status()

    def _request_traffic_update(self) -> None:
        if not self.core.is_running:
            if self._traffic_core_active:
                self._set_traffic_inactive()
            return
        if not self._traffic_core_active:
            self._traffic_core_active = True
            self.traffic_tracker.reset()
            self.traffic_chart.reset(monitoring=True)
            self._set_traffic_status("正在连接", "waiting")
        if self._traffic_reply is not None:
            return

        request = QNetworkRequest(
            QUrl(f"http://127.0.0.1:{self.config.controller_port}/connections")
        )
        request.setRawHeader(
            b"Authorization", f"Bearer {self.config.controller_secret}".encode("utf-8")
        )
        request.setTransferTimeout(900)
        reply = self.traffic_network.get(request)
        self._traffic_reply = reply
        reply.finished.connect(lambda current=reply: self._traffic_reply_finished(current))

    def _traffic_reply_finished(self, reply: QNetworkReply) -> None:
        if self._traffic_reply is reply:
            self._traffic_reply = None
        try:
            if not self.core.is_running:
                return
            if reply.error() != QNetworkReply.NetworkError.NoError:
                self._set_traffic_status("数据暂不可用", "error")
                return
            payload = json.loads(bytes(reply.readAll()).decode("utf-8"))
            snapshot = parse_connection_snapshot(payload)
            sample = self.traffic_tracker.update(snapshot)
        except (UnicodeDecodeError, json.JSONDecodeError, ValueError):
            self._set_traffic_status("数据暂不可用", "error")
            return
        finally:
            reply.deleteLater()

        self.download_rate_label.setText(format_rate(sample.download_rate))
        self.upload_rate_label.setText(format_rate(sample.upload_rate))
        self.active_connections_label.setText(str(sample.active_connections))
        self.download_total_label.setText(
            f"累计下载  {format_bytes(sample.download_total)}"
        )
        self.upload_total_label.setText(f"累计上传  {format_bytes(sample.upload_total)}")
        self.traffic_chart.append_sample(sample.download_rate, sample.upload_rate)
        self._set_traffic_status("实时更新", "active")

    def _set_traffic_status(self, text: str, state: str) -> None:
        self.traffic_monitor_status.setText(text)
        self.traffic_monitor_status.setProperty("state", state)
        self._repolish(self.traffic_monitor_status)

    def _set_traffic_inactive(self) -> None:
        self._traffic_core_active = False
        self.traffic_tracker.reset()
        self.traffic_chart.reset(monitoring=False)
        self.download_rate_label.setText("0 B/s")
        self.upload_rate_label.setText("0 B/s")
        self.active_connections_label.setText("0")
        self.download_total_label.setText("累计下载  0 B")
        self.upload_total_label.setText("累计上传  0 B")
        self._set_traffic_status("接管停止", "idle")

    def _append_log_file(self, lines: list[str]) -> None:
        path = logs_dir() / "mihomo.log"
        try:
            if path.exists() and path.stat().st_size > 2 * 1024 * 1024:
                backup = path.with_suffix(".previous.log")
                if backup.exists():
                    backup.unlink()
                path.replace(backup)
            with path.open("a", encoding="utf-8") as handle:
                handle.write("\n".join(lines) + "\n")
        except OSError:
            pass

    def _update_status(self, detail_override: str | None = None) -> None:
        if not hasattr(self, "start_button"):
            return
        running = self.core.is_running
        if self._operation_active:
            status = "处理中"
            main = detail_override or "正在处理..."
            detail = "请稍候，不要重复操作。"
            state = "busy"
        elif running:
            status = "正在接管"
            main = "全流量接管运行中"
            detail = f"{MODE_LABELS.get(self.config.mode, self.config.mode)} · 本地入口 127.0.0.1:{self.config.mixed_port}"
            state = "running"
        else:
            status = "已停止"
            main = detail_override or "全流量接管已停止"
            detail = "当前不会修改系统路由。"
            state = "stopped"
        self.header_status.setText(status)
        self.header_status.setProperty("state", state)
        self._repolish(self.header_status)
        self.status_panel.setProperty("state", state)
        self._repolish(self.status_panel)
        self.status_icon.setProperty("state", state)
        self._repolish(self.status_icon)
        self.sidebar_core_status.setText("核心运行中" if running else "核心已停止")
        self.sidebar_core_status.setProperty("state", state)
        self._repolish(self.sidebar_core_status)
        self.main_status.setText(main)
        self.status_detail.setText(detail)
        self.start_button.setEnabled(not self._operation_active)
        self.start_button.setText("停止接管" if running else "启动接管")
        self.start_button.setProperty("running", running)
        self._repolish(self.start_button)
        self.start_button.setIcon(
            self.style().standardIcon(QStyle.SP_MediaStop if running else QStyle.SP_MediaPlay)
        )
        self.tray_toggle_action.setText("停止接管" if running else "启动接管")
        self.tray.setToolTip(f"Network Manager · {status}")

    def _run_task(
        self,
        function: Any,
        on_success: Any,
        error_title: str,
        on_finished: Any | None = None,
    ) -> None:
        task = Task(function)
        self._tasks.add(task)
        task.signals.result.connect(on_success)
        task.signals.error.connect(lambda message: QMessageBox.warning(self, error_title, message))

        def finished() -> None:
            self._tasks.discard(task)
            if on_finished:
                on_finished()

        task.signals.finished.connect(finished)
        self.thread_pool.start(task)

    @staticmethod
    def _repolish(widget: QWidget) -> None:
        widget.style().unpolish(widget)
        widget.style().polish(widget)

    def _tray_activated(self, reason: QSystemTrayIcon.ActivationReason) -> None:
        if reason in {QSystemTrayIcon.Trigger, QSystemTrayIcon.DoubleClick}:
            self.show_and_raise()

    def show_and_raise(self) -> None:
        self.showNormal()
        self.activateWindow()
        self.raise_()

    def closeEvent(self, event: QCloseEvent) -> None:
        if self._force_close or not self.config.close_to_tray or not self.tray.isVisible():
            self.core.stop()
            event.accept()
            QApplication.quit()
            return
        event.ignore()
        self.hide()
        if not self._tray_notice_shown:
            self.tray.showMessage(
                "Network Manager",
                "程序仍在托盘运行。选择“退出并停止接管”可完全退出。",
                QSystemTrayIcon.Information,
                3500,
            )
            self._tray_notice_shown = True

    def quit_application(self) -> None:
        self._force_close = True
        self.core.stop()
        self.tray.hide()
        QApplication.quit()

    def shutdown(self) -> None:
        if self._traffic_reply is not None:
            self._traffic_reply.abort()
        self.core.stop()
