package com.skeidno.networkmanager.ui

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.Image
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.BoxWithConstraints
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.PaddingValues
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxHeight
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.heightIn
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.layout.widthIn
import androidx.compose.foundation.layout.wrapContentHeight
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.lazy.grid.GridCells
import androidx.compose.foundation.lazy.grid.GridItemSpan
import androidx.compose.foundation.lazy.grid.LazyVerticalGrid
import androidx.compose.foundation.lazy.grid.items
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.Rule
import androidx.compose.material.icons.filled.AccountTree
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.filled.CheckCircle
import androidx.compose.material.icons.filled.ContentPaste
import androidx.compose.material.icons.filled.DeleteOutline
import androidx.compose.material.icons.filled.DeleteSweep
import androidx.compose.material.icons.filled.Dns
import androidx.compose.material.icons.filled.FileOpen
import androidx.compose.material.icons.filled.Home
import androidx.compose.material.icons.filled.Link
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material.icons.filled.Settings
import androidx.compose.material.icons.filled.Save
import androidx.compose.material.icons.filled.Speed
import androidx.compose.material.icons.filled.Stop
import androidx.compose.material.icons.filled.Wifi
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationRail
import androidx.compose.material3.NavigationRailItem
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.VerticalDivider
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.Path
import androidx.compose.ui.graphics.StrokeCap
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.res.painterResource
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import com.skeidno.networkmanager.R
import com.skeidno.networkmanager.data.AppState
import com.skeidno.networkmanager.data.FallbackTarget
import com.skeidno.networkmanager.data.LatencyStatus
import com.skeidno.networkmanager.data.ProxyNode
import com.skeidno.networkmanager.data.RoutingMode
import com.skeidno.networkmanager.data.Subscription
import kotlin.math.max

private enum class AppPage(val label: String, val icon: ImageVector) {
    Overview("概览", Icons.Default.Home),
    Rules("规则", Icons.AutoMirrored.Filled.Rule),
    Nodes("节点", Icons.Default.Dns),
    Settings("设置", Icons.Default.Settings),
}

@Composable
fun NetworkManagerApp(
    viewModel: AppViewModel,
    onStartVpn: () -> Unit,
    onStopVpn: () -> Unit,
    onImportConfiguration: () -> Unit,
    onExportConfiguration: () -> Unit,
) {
    val state by viewModel.state.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    var page by rememberSaveable { mutableStateOf(AppPage.Overview) }

    LaunchedEffect(viewModel) {
        viewModel.messages.collect { snackbar.showSnackbar(it) }
    }
    LaunchedEffect(state.error) {
        if (state.error.isNotBlank()) snackbar.showSnackbar(state.error)
    }

    BoxWithConstraints(Modifier.fillMaxSize()) {
        val wideLayout = maxWidth >= 720.dp
        Scaffold(
            snackbarHost = { SnackbarHost(snackbar) },
            bottomBar = {
                if (!wideLayout) {
                    NavigationBar(containerColor = MaterialTheme.colorScheme.surface) {
                        AppPage.entries.forEach { item ->
                            NavigationBarItem(
                                modifier = Modifier.testTag("nav-${item.name.lowercase()}"),
                                selected = page == item,
                                onClick = { page = item },
                                icon = { Icon(item.icon, contentDescription = item.label) },
                                label = { Text(item.label) },
                            )
                        }
                    }
                }
            },
        ) { scaffoldPadding ->
            Row(Modifier.fillMaxSize().padding(scaffoldPadding)) {
                if (wideLayout) {
                    AppNavigationRail(selected = page, onSelected = { page = it })
                    VerticalDivider(Modifier.fillMaxHeight())
                }
                Column(Modifier.fillMaxSize()) {
                    AppHeader(state = state, page = page, onStartVpn = onStartVpn, onStopVpn = onStopVpn)
                    HorizontalDivider()
                    when (page) {
                        AppPage.Overview -> OverviewPage(
                            state = state,
                            onMode = viewModel::setMode,
                            onShowNodes = { page = AppPage.Nodes },
                        )
                        AppPage.Rules -> RulesPage(
                            state = state,
                            onRuleEnabled = viewModel::setRuleGroupEnabled,
                            onCommonTarget = viewModel::setCommonRuleTarget,
                            onFallback = viewModel::setFallback,
                            onShowNodes = { page = AppPage.Nodes },
                        )
                        AppPage.Nodes -> NodesPage(
                            state = state,
                            onSelect = viewModel::selectNode,
                            onDelete = viewModel::deleteNode,
                            onDeleteErrors = viewModel::deleteErrorNodes,
                            onImportText = viewModel::importText,
                            onAddSubscription = viewModel::addSubscription,
                            onRefreshSubscription = viewModel::refreshSubscription,
                            onDeleteSubscription = viewModel::deleteSubscription,
                            onTest = viewModel::testAllNodes,
                            onTestNode = viewModel::testNode,
                        )
                        AppPage.Settings -> SettingsPage(
                            onImportConfiguration = onImportConfiguration,
                            onExportConfiguration = onExportConfiguration,
                        )
                    }
                }
            }
        }
    }
}

@Composable
private fun AppNavigationRail(selected: AppPage, onSelected: (AppPage) -> Unit) {
    NavigationRail(
        modifier = Modifier.width(92.dp),
        header = {
            Image(
                painter = painterResource(R.mipmap.ic_launcher),
                contentDescription = null,
                modifier = Modifier.padding(vertical = 16.dp).size(42.dp).clip(RoundedCornerShape(8.dp)),
            )
        },
    ) {
        AppPage.entries.forEach { page ->
            NavigationRailItem(
                modifier = Modifier.testTag("nav-${page.name.lowercase()}"),
                selected = selected == page,
                onClick = { onSelected(page) },
                icon = { Icon(page.icon, contentDescription = page.label) },
                label = { Text(page.label) },
            )
        }
    }
}

@Composable
private fun AppHeader(state: AppState, page: AppPage, onStartVpn: () -> Unit, onStopVpn: () -> Unit) {
    Surface(color = MaterialTheme.colorScheme.surface) {
        Row(
            modifier = Modifier.fillMaxWidth().height(72.dp).padding(horizontal = 20.dp),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Column {
                Text(
                    page.label,
                    modifier = Modifier.testTag("page-title"),
                    style = MaterialTheme.typography.headlineSmall,
                    fontWeight = FontWeight.Bold,
                )
                Text("Network Manager", style = MaterialTheme.typography.labelMedium, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            Button(
                onClick = if (state.running) onStopVpn else onStartVpn,
                enabled = !state.busy,
                colors = if (state.running) {
                    ButtonDefaults.buttonColors(
                        containerColor = MaterialTheme.colorScheme.surfaceVariant,
                        contentColor = MaterialTheme.colorScheme.onSurface,
                    )
                } else ButtonDefaults.buttonColors(),
            ) {
                Icon(if (state.running) Icons.Default.Stop else Icons.Default.PlayArrow, contentDescription = null)
                Spacer(Modifier.width(6.dp))
                Text(if (state.busy) state.statusMessage else if (state.running) "停止" else "接管")
            }
        }
    }
}

@Composable
private fun OverviewPage(state: AppState, onMode: (RoutingMode) -> Unit, onShowNodes: () -> Unit) {
    val selected = state.nodes.firstOrNull { it.id == state.selectedNodeId }
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        StatusPanel(state)
        SectionCard(title = "代理模式", icon = Icons.Default.AccountTree) {
            SegmentedChoice(
                options = RoutingMode.entries,
                selected = state.mode,
                label = { it.label },
                onSelected = onMode,
            )
        }
        SectionCard(title = "当前节点", icon = Icons.Default.Dns) {
            Row(
                modifier = Modifier.fillMaxWidth(),
                verticalAlignment = Alignment.CenterVertically,
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Column(Modifier.weight(1f)) {
                    Text(selected?.name ?: "尚未选择", fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Text(
                        selected?.let { "${it.protocol} · ${it.server}:${it.port}" } ?: "导入订阅或节点后即可接管",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        style = MaterialTheme.typography.bodySmall,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                TextButton(onClick = onShowNodes) { Text("选择") }
            }
        }
        TrafficPanel(state)
        Spacer(Modifier.height(8.dp))
    }
}

@Composable
private fun StatusPanel(state: AppState) {
    val accent = if (state.running) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurfaceVariant
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(8.dp),
        color = if (state.running) MaterialTheme.colorScheme.primaryContainer else MaterialTheme.colorScheme.surface,
        border = CardDefaults.outlinedCardBorder(),
    ) {
        Row(Modifier.padding(16.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(if (state.running) Icons.Default.CheckCircle else Icons.Default.Stop, contentDescription = null, tint = accent)
            Spacer(Modifier.width(12.dp))
            Column {
                Text(if (state.running) "全流量接管运行中" else "全流量接管已停止", fontWeight = FontWeight.Bold)
                Text(state.statusMessage, color = MaterialTheme.colorScheme.onSurfaceVariant, style = MaterialTheme.typography.bodySmall)
            }
        }
    }
}

@Composable
private fun TrafficPanel(state: AppState) {
    SectionCard(title = "实时流量", icon = Icons.Default.Speed) {
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Metric("下载", formatRate(state.downloadBytesPerSecond), MaterialTheme.colorScheme.secondary, Modifier.weight(1f))
            Metric("上传", formatRate(state.uploadBytesPerSecond), MaterialTheme.colorScheme.tertiary, Modifier.weight(1f))
        }
        Spacer(Modifier.height(8.dp))
        Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Metric("累计下载", formatBytes(state.totalDownloadBytes), MaterialTheme.colorScheme.onSurfaceVariant, Modifier.weight(1f))
            Metric("累计上传", formatBytes(state.totalUploadBytes), MaterialTheme.colorScheme.onSurfaceVariant, Modifier.weight(1f))
        }
        Spacer(Modifier.height(14.dp))
        TrafficChart(state.downloadSamples, state.uploadSamples)
    }
}

@Composable
private fun Metric(label: String, value: String, color: Color, modifier: Modifier = Modifier) {
    Surface(modifier = modifier.height(64.dp), color = MaterialTheme.colorScheme.surfaceVariant, shape = RoundedCornerShape(6.dp)) {
        Column(Modifier.padding(horizontal = 12.dp, vertical = 9.dp)) {
            Text(value, color = color, fontWeight = FontWeight.Bold, fontSize = 18.sp, maxLines = 1)
            Text(label, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
    }
}

@Composable
private fun TrafficChart(download: List<Long>, upload: List<Long>) {
    val downColor = MaterialTheme.colorScheme.secondary
    val upColor = MaterialTheme.colorScheme.tertiary
    val gridColor = MaterialTheme.colorScheme.outline.copy(alpha = 0.45f)
    Canvas(Modifier.fillMaxWidth().height(150.dp)) {
        repeat(3) { index ->
            val y = size.height * index / 2f
            drawLine(gridColor, Offset(0f, y), Offset(size.width, y), strokeWidth = 1f)
        }
        val sampleMax = max(1024L, max(download.maxOrNull() ?: 0, upload.maxOrNull() ?: 0)).toFloat()
        fun path(values: List<Long>): Path {
            val result = Path()
            values.forEachIndexed { index, value ->
                val x = if (values.size <= 1) 0f else size.width * index / (values.size - 1)
                val y = size.height - (value / sampleMax * size.height)
                if (index == 0) result.moveTo(x, y) else result.lineTo(x, y)
            }
            return result
        }
        drawPath(path(download), downColor, style = Stroke(width = 3f, cap = StrokeCap.Round))
        drawPath(path(upload), upColor, style = Stroke(width = 3f, cap = StrokeCap.Round))
    }
}

@Composable
private fun RulesPage(
    state: AppState,
    onRuleEnabled: (Boolean) -> Unit,
    onCommonTarget: (FallbackTarget) -> Unit,
    onFallback: (FallbackTarget) -> Unit,
    onShowNodes: () -> Unit,
) {
    var showDomains by rememberSaveable { mutableStateOf(false) }
    var showPortableRules by rememberSaveable { mutableStateOf(false) }
    val selected = state.nodes.firstOrNull { it.id == state.selectedNodeId }
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        SectionCard(title = "内网与局域网", icon = Icons.Default.CheckCircle) {
            Text("系统强制直连，不进入代理或保底出口", color = MaterialTheme.colorScheme.onSurfaceVariant)
            Text(
                "私有 IP、回环、链路本地、.lan、.local 与 home.arpa",
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
        }
        SectionCard(title = state.ruleGroup.name, icon = Icons.AutoMirrored.Filled.Rule) {
            Row(Modifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
                Column(Modifier.weight(1f)) {
                    Text("${state.ruleGroup.domains.size} 条域名共用一个出口", color = MaterialTheme.colorScheme.onSurfaceVariant)
                    Text(
                        selected?.name ?: "尚未选择代理节点",
                        fontWeight = FontWeight.SemiBold,
                        maxLines = 1,
                        overflow = TextOverflow.Ellipsis,
                    )
                }
                Switch(checked = state.ruleGroup.enabled, onCheckedChange = onRuleEnabled)
            }
            Spacer(Modifier.height(8.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = { showDomains = true }) { Text("查看全部域名") }
                TextButton(onClick = onShowNodes) { Text("更换出口") }
            }
            Spacer(Modifier.height(10.dp))
            SegmentedChoice(
                options = FallbackTarget.entries,
                selected = state.commonRuleTarget,
                label = { if (it == FallbackTarget.Proxy) "当前节点" else "直连" },
                onSelected = onCommonTarget,
            )
        }
        if (state.portableRules.isNotEmpty()) {
            SectionCard(title = "跨设备规则", icon = Icons.AutoMirrored.Filled.Rule) {
                Text("${state.portableRules.size} 条域名 / IP 规则", color = MaterialTheme.colorScheme.onSurfaceVariant)
                TextButton(onClick = { showPortableRules = true }) { Text("查看规则") }
            }
        }
        SectionCard(title = "强制保底规则", icon = Icons.Default.CheckCircle) {
            Text("其他规则未匹配时始终执行", color = MaterialTheme.colorScheme.onSurfaceVariant)
            Spacer(Modifier.height(10.dp))
            SegmentedChoice(
                options = FallbackTarget.entries,
                selected = state.fallbackTarget,
                label = { it.label },
                onSelected = onFallback,
            )
        }
    }
    if (showDomains) {
        AlertDialog(
            onDismissRequest = { showDomains = false },
            title = { Text("常用海外站点") },
            text = {
                LazyColumn(Modifier.heightIn(max = 440.dp)) {
                    items(state.ruleGroup.domains) { domain ->
                        Text(domain, Modifier.fillMaxWidth().padding(vertical = 9.dp))
                        HorizontalDivider()
                    }
                }
            },
            confirmButton = { TextButton(onClick = { showDomains = false }) { Text("完成") } },
        )
    }
    if (showPortableRules) {
        AlertDialog(
            onDismissRequest = { showPortableRules = false },
            title = { Text("跨设备规则") },
            text = {
                LazyColumn(Modifier.heightIn(max = 440.dp)) {
                    items(state.portableRules) { rule ->
                        Column(Modifier.fillMaxWidth().padding(vertical = 9.dp)) {
                            Text(rule.value, fontWeight = FontWeight.SemiBold)
                            Text(
                                "${rule.type} · ${rule.target.label}",
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                        HorizontalDivider()
                    }
                }
            },
            confirmButton = { TextButton(onClick = { showPortableRules = false }) { Text("完成") } },
        )
    }
}

@Composable
private fun NodesPage(
    state: AppState,
    onSelect: (String) -> Unit,
    onDelete: (String) -> Unit,
    onDeleteErrors: () -> Unit,
    onImportText: (String) -> Unit,
    onAddSubscription: (String, String) -> Unit,
    onRefreshSubscription: (String) -> Unit,
    onDeleteSubscription: (String) -> Unit,
    onTest: () -> Unit,
    onTestNode: (String) -> Unit,
) {
    var importDialog by rememberSaveable { mutableStateOf(false) }
    var deleteNodeId by rememberSaveable { mutableStateOf<String?>(null) }
    var deleteErrorsDialog by rememberSaveable { mutableStateOf(false) }
    val errorCount = state.nodes.count { it.latencyStatus == LatencyStatus.Error }
    val testing = state.nodes.any { it.latencyStatus == LatencyStatus.Testing }
    LazyVerticalGrid(
        columns = GridCells.Adaptive(168.dp),
        modifier = Modifier.fillMaxSize(),
        contentPadding = PaddingValues(16.dp),
        horizontalArrangement = Arrangement.spacedBy(10.dp),
        verticalArrangement = Arrangement.spacedBy(10.dp),
    ) {
        item(span = { GridItemSpan(maxLineSpan) }) {
            Row(
                modifier = Modifier.fillMaxWidth().padding(bottom = 4.dp),
                horizontalArrangement = Arrangement.spacedBy(8.dp),
            ) {
                Button(onClick = { importDialog = true }) {
                    Icon(Icons.Default.Add, contentDescription = null)
                    Spacer(Modifier.width(6.dp))
                    Text("导入")
                }
                OutlinedButton(onClick = onTest, enabled = state.nodes.isNotEmpty() && !testing) {
                    Icon(Icons.Default.Speed, contentDescription = null)
                    Spacer(Modifier.width(6.dp))
                    Text("批量测速")
                }
                IconButton(
                    onClick = { deleteErrorsDialog = true },
                    enabled = errorCount > 0 && !testing,
                ) {
                    Icon(
                        Icons.Default.DeleteSweep,
                        contentDescription = "删除 Error 节点",
                        tint = if (errorCount > 0) {
                            MaterialTheme.colorScheme.error
                        } else {
                            MaterialTheme.colorScheme.onSurfaceVariant
                        },
                    )
                }
            }
        }
        if (state.subscriptions.isNotEmpty()) {
            item(span = { GridItemSpan(maxLineSpan) }) {
                Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text("订阅", fontWeight = FontWeight.Bold, modifier = Modifier.padding(top = 4.dp))
                    state.subscriptions.forEach { subscription ->
                        SubscriptionRow(subscription, onRefreshSubscription, onDeleteSubscription)
                    }
                    Text("节点", fontWeight = FontWeight.Bold, modifier = Modifier.padding(top = 8.dp))
                }
            }
        }
        items(state.nodes, key = { it.id }) { node ->
            NodeCard(
                node = node,
                selected = node.id == state.selectedNodeId,
                onClick = { onSelect(node.id) },
                onTest = { onTestNode(node.id) },
                onDelete = { deleteNodeId = node.id },
            )
        }
        if (state.nodes.isEmpty()) {
            item(span = { GridItemSpan(maxLineSpan) }) {
                EmptyNodes(onImport = { importDialog = true })
            }
        }
    }
    if (importDialog) {
        ImportDialog(
            onDismiss = { importDialog = false },
            onImportText = {
                importDialog = false
                onImportText(it)
            },
            onAddSubscription = { name, url ->
                importDialog = false
                onAddSubscription(name, url)
            },
        )
    }
    deleteNodeId?.let { id ->
        AlertDialog(
            onDismissRequest = { deleteNodeId = null },
            title = { Text("删除节点") },
            text = { Text("节点将从本机移除；刷新对应订阅时可能再次出现。") },
            confirmButton = {
                TextButton(onClick = { onDelete(id); deleteNodeId = null }) { Text("删除", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = { TextButton(onClick = { deleteNodeId = null }) { Text("取消") } },
        )
    }
    if (deleteErrorsDialog) {
        AlertDialog(
            onDismissRequest = { deleteErrorsDialog = false },
            title = { Text("删除测速失败节点") },
            text = { Text("确定删除 $errorCount 个 Error 节点？订阅记录会保留，之后刷新原订阅即可恢复。") },
            confirmButton = {
                TextButton(
                    onClick = {
                        deleteErrorsDialog = false
                        onDeleteErrors()
                    },
                ) { Text("删除", color = MaterialTheme.colorScheme.error) }
            },
            dismissButton = {
                TextButton(onClick = { deleteErrorsDialog = false }) { Text("取消") }
            },
        )
    }
}

@Composable
private fun SubscriptionRow(
    subscription: Subscription,
    onRefresh: (String) -> Unit,
    onDelete: (String) -> Unit,
) {
    Surface(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(8.dp),
        color = MaterialTheme.colorScheme.surface,
        border = CardDefaults.outlinedCardBorder(),
    ) {
        Row(Modifier.padding(start = 12.dp, end = 4.dp, top = 7.dp, bottom = 7.dp), verticalAlignment = Alignment.CenterVertically) {
            Icon(Icons.Default.Link, contentDescription = null, tint = MaterialTheme.colorScheme.secondary)
            Spacer(Modifier.width(10.dp))
            Column(Modifier.weight(1f)) {
                Text(subscription.name, fontWeight = FontWeight.SemiBold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text("${subscription.nodeCount} 个节点", style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
            IconButton(onClick = { onRefresh(subscription.id) }) {
                Icon(Icons.Default.Refresh, contentDescription = "刷新订阅")
            }
            IconButton(onClick = { onDelete(subscription.id) }) {
                Icon(Icons.Default.DeleteOutline, contentDescription = "删除订阅", tint = MaterialTheme.colorScheme.error)
            }
        }
    }
}

@Composable
private fun NodeCard(
    node: ProxyNode,
    selected: Boolean,
    onClick: () -> Unit,
    onTest: () -> Unit,
    onDelete: () -> Unit,
) {
    val borderColor = if (selected) MaterialTheme.colorScheme.secondary else MaterialTheme.colorScheme.outline
    val background = if (selected) MaterialTheme.colorScheme.secondaryContainer else MaterialTheme.colorScheme.surface
    Card(
        modifier = Modifier.fillMaxWidth().height(130.dp).border(1.dp, borderColor, RoundedCornerShape(8.dp)).clickable(onClick = onClick),
        shape = RoundedCornerShape(8.dp),
        colors = CardDefaults.cardColors(containerColor = background),
    ) {
        Column(Modifier.fillMaxSize().padding(12.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Text(node.name, modifier = Modifier.weight(1f), fontWeight = FontWeight.Bold, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Latency(node)
                IconButton(
                    onClick = onTest,
                    enabled = node.latencyStatus != LatencyStatus.Testing,
                    modifier = Modifier.size(32.dp),
                ) {
                    Icon(Icons.Default.Wifi, contentDescription = "测试该节点", modifier = Modifier.size(19.dp))
                }
                IconButton(onClick = onDelete, modifier = Modifier.size(32.dp)) {
                    Icon(Icons.Default.DeleteOutline, contentDescription = "删除节点", tint = MaterialTheme.colorScheme.error, modifier = Modifier.size(19.dp))
                }
            }
            AssistChip(onClick = {}, label = { Text(node.protocol, fontSize = 11.sp) })
            Spacer(Modifier.weight(1f))
            Text(node.sourceName, style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
            Text("${node.server}:${node.port}", style = MaterialTheme.typography.labelSmall, color = MaterialTheme.colorScheme.onSurfaceVariant, maxLines = 1, overflow = TextOverflow.Ellipsis)
        }
    }
}

@Composable
private fun Latency(node: ProxyNode) {
    val text = when (node.latencyStatus) {
        LatencyStatus.Idle -> ""
        LatencyStatus.Testing -> "..."
        LatencyStatus.Available -> "${node.latencyMs} ms"
        LatencyStatus.Error -> "Error"
    }
    val color = when {
        node.latencyStatus == LatencyStatus.Error -> MaterialTheme.colorScheme.error
        (node.latencyMs ?: 0) > 500 -> MaterialTheme.colorScheme.tertiary
        else -> MaterialTheme.colorScheme.primary
    }
    Text(text, color = color, style = MaterialTheme.typography.labelSmall)
}

@Composable
private fun EmptyNodes(onImport: () -> Unit) {
    Column(
        modifier = Modifier.fillMaxWidth().padding(vertical = 72.dp),
        horizontalAlignment = Alignment.CenterHorizontally,
    ) {
        Icon(Icons.Default.Dns, contentDescription = null, modifier = Modifier.size(42.dp), tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Spacer(Modifier.height(12.dp))
        Text("还没有节点", fontWeight = FontWeight.Bold)
        TextButton(onClick = onImport) { Text("导入订阅或分享链接") }
    }
}

private enum class ImportMode { Subscription, Text }

@Composable
private fun ImportDialog(
    onDismiss: () -> Unit,
    onImportText: (String) -> Unit,
    onAddSubscription: (String, String) -> Unit,
) {
    var mode by rememberSaveable { mutableStateOf(ImportMode.Subscription) }
    var name by rememberSaveable { mutableStateOf("") }
    var value by rememberSaveable { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("导入节点") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                SegmentedChoice(
                    options = ImportMode.entries,
                    selected = mode,
                    label = { if (it == ImportMode.Subscription) "订阅链接" else "粘贴内容" },
                    onSelected = { mode = it },
                )
                if (mode == ImportMode.Subscription) {
                    OutlinedTextField(
                        value = name,
                        onValueChange = { name = it },
                        modifier = Modifier.fillMaxWidth(),
                        label = { Text("名称（可选）") },
                        singleLine = true,
                    )
                }
                OutlinedTextField(
                    value = value,
                    onValueChange = { value = it },
                    modifier = Modifier.fillMaxWidth().heightIn(min = if (mode == ImportMode.Text) 150.dp else 56.dp),
                    label = { Text(if (mode == ImportMode.Subscription) "订阅地址" else "节点链接、Base64 或 Clash YAML") },
                    leadingIcon = { Icon(if (mode == ImportMode.Subscription) Icons.Default.Link else Icons.Default.ContentPaste, contentDescription = null) },
                    singleLine = mode == ImportMode.Subscription,
                )
            }
        },
        confirmButton = {
            Button(
                enabled = value.isNotBlank(),
                onClick = {
                    if (mode == ImportMode.Subscription) onAddSubscription(name, value.trim())
                    else onImportText(value.trim())
                },
            ) { Text("导入") }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("取消") } },
    )
}

@Composable
private fun SettingsPage(
    onImportConfiguration: () -> Unit,
    onExportConfiguration: () -> Unit,
) {
    Column(
        modifier = Modifier.fillMaxSize().verticalScroll(rememberScrollState()).padding(16.dp),
        verticalArrangement = Arrangement.spacedBy(12.dp),
    ) {
        SectionCard(title = "应用信息", icon = Icons.Default.Settings) {
            InfoRow("版本", "Android 0.4.0")
            HorizontalDivider()
            InfoRow("代理核心", "sing-box 1.13.20 · libbox")
            HorizontalDivider()
            InfoRow("VPN 模式", "Android VpnService")
        }
        SectionCard(title = "本机数据", icon = Icons.Default.Dns) {
            Text("订阅、节点和规则仅保存在应用私有目录。", color = MaterialTheme.colorScheme.onSurfaceVariant)
        }
        SectionCard(title = "跨设备配置", icon = Icons.Default.Link) {
            Text(
                "节点、订阅和分流规则可与其他平台互通。导出文件可能包含节点密码，请妥善保管。",
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            Spacer(Modifier.height(12.dp))
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedButton(onClick = onImportConfiguration) {
                    Icon(Icons.Default.FileOpen, contentDescription = null)
                    Spacer(Modifier.width(6.dp))
                    Text("导入")
                }
                OutlinedButton(onClick = onExportConfiguration) {
                    Icon(Icons.Default.Save, contentDescription = null)
                    Spacer(Modifier.width(6.dp))
                    Text("导出")
                }
            }
        }
    }
}

@Composable
private fun InfoRow(label: String, value: String) {
    Row(Modifier.fillMaxWidth().padding(vertical = 11.dp), horizontalArrangement = Arrangement.SpaceBetween) {
        Text(label, color = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(value, fontWeight = FontWeight.SemiBold)
    }
}

@Composable
private fun SectionCard(title: String, icon: ImageVector, content: @Composable ColumnScope.() -> Unit) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        shape = RoundedCornerShape(8.dp),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
        border = CardDefaults.outlinedCardBorder(),
    ) {
        Column(Modifier.fillMaxWidth().padding(16.dp)) {
            Row(verticalAlignment = Alignment.CenterVertically) {
                Surface(shape = RoundedCornerShape(6.dp), color = MaterialTheme.colorScheme.secondaryContainer) {
                    Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.secondary, modifier = Modifier.padding(7.dp).size(20.dp))
                }
                Spacer(Modifier.width(10.dp))
                Text(title, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
            }
            Spacer(Modifier.height(14.dp))
            content()
        }
    }
}

@Composable
private fun <T> SegmentedChoice(
    options: List<T>,
    selected: T,
    label: (T) -> String,
    onSelected: (T) -> Unit,
) {
    Row(
        modifier = Modifier.fillMaxWidth().height(44.dp).clip(RoundedCornerShape(7.dp)).border(1.dp, MaterialTheme.colorScheme.outline, RoundedCornerShape(7.dp)),
    ) {
        options.forEachIndexed { index, item ->
            Box(
                modifier = Modifier.weight(1f).fillMaxHeight()
                    .background(if (item == selected) MaterialTheme.colorScheme.secondary else Color.Transparent)
                    .clickable { onSelected(item) },
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    label(item),
                    color = if (item == selected) MaterialTheme.colorScheme.onSecondary else MaterialTheme.colorScheme.onSurfaceVariant,
                    fontWeight = if (item == selected) FontWeight.Bold else FontWeight.Normal,
                    maxLines = 1,
                )
            }
            if (index < options.lastIndex) VerticalDivider(Modifier.fillMaxHeight())
        }
    }
}

private fun formatRate(bytes: Long): String = when {
    bytes >= 1024L * 1024 -> "%.1f MB/s".format(bytes / 1024f / 1024f)
    bytes >= 1024 -> "%.1f KB/s".format(bytes / 1024f)
    else -> "$bytes B/s"
}

private fun formatBytes(bytes: Long): String = when {
    bytes >= 1024L * 1024 * 1024 -> "%.2f GB".format(bytes / 1024f / 1024f / 1024f)
    bytes >= 1024L * 1024 -> "%.1f MB".format(bytes / 1024f / 1024f)
    bytes >= 1024 -> "%.1f KB".format(bytes / 1024f)
    else -> "$bytes B"
}
