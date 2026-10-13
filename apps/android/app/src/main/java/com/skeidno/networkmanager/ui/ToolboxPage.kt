package com.skeidno.networkmanager.ui

import android.widget.Toast
import androidx.compose.foundation.background
import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.ColumnScope
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.ContentCopy
import androidx.compose.material.icons.filled.PlayArrow
import androidx.compose.material.icons.filled.Refresh
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.FilterChip
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.runtime.saveable.rememberSaveable
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.unit.dp
import com.skeidno.networkmanager.data.AppState
import com.skeidno.networkmanager.data.ExitCheckStatus
import com.skeidno.networkmanager.data.ExitInfo
import com.skeidno.networkmanager.data.LocalDnsEntry
import com.skeidno.networkmanager.data.NetworkTools
import com.skeidno.networkmanager.data.normalizeLocalDnsDomain
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

/**
 * 工具箱：把排障时要用的诊断能力集中在一页。
 *
 * 这里全是**只读的本地 / 网络探测**——不改配置、不碰内核、不动订阅，
 * 所以随便点都不会把正在跑的代理弄坏。
 */
@Composable
internal fun ToolboxPage(
    state: AppState,
    onTestExit: () -> Unit,
) {
    Column(
        modifier = Modifier
            .fillMaxSize()
            .verticalScroll(rememberScrollState())
            .padding(horizontal = 16.dp),
        verticalArrangement = Arrangement.spacedBy(14.dp),
    ) {
        Spacer(Modifier.height(12.dp))
        ExitCard(state = state, onTestExit = onTestExit)
        RoutePreviewCard(state = state)
        DnsCard(state = state)
        CertificateCard(state = state)
        TcpCard()
        TextToolCard()
        Spacer(Modifier.height(16.dp))
    }
}

/** 工具箱统一的卡片外壳：标题 + 一句说明 + 内容。 */
@Composable
private fun ToolCard(
    title: String,
    subtitle: String,
    content: @Composable ColumnScope.() -> Unit,
) {
    Card(
        modifier = Modifier.fillMaxWidth(),
        colors = CardDefaults.cardColors(containerColor = MaterialTheme.colorScheme.surface),
    ) {
        Column(
            modifier = Modifier.fillMaxWidth().padding(16.dp),
            verticalArrangement = Arrangement.spacedBy(10.dp),
        ) {
            Text(title, style = MaterialTheme.typography.titleMedium, fontWeight = FontWeight.Bold)
            Text(
                subtitle,
                style = MaterialTheme.typography.bodySmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant,
            )
            content()
        }
    }
}

@Composable
private fun Hint(text: String, warn: Boolean = false) {
    Text(
        text,
        style = MaterialTheme.typography.bodySmall,
        color = if (warn) {
            MaterialTheme.colorScheme.tertiary
        } else {
            MaterialTheme.colorScheme.onSurfaceVariant
        },
    )
}

/** 正在跑的按钮内容：转圈 + 文案，避免用户重复点。 */
@Composable
private fun BusyButtonContent(busy: Boolean, idle: String, working: String) {
    if (busy) {
        CircularProgressIndicator(
            modifier = Modifier.size(16.dp),
            strokeWidth = 2.dp,
            color = MaterialTheme.colorScheme.onPrimary,
        )
        Spacer(Modifier.width(6.dp))
    }
    Text(if (busy) working else idle)
}

// ---------------------------------------------------------------------------
// 出口检测
// ---------------------------------------------------------------------------

@Composable
private fun ExitCard(state: AppState, onTestExit: () -> Unit) {
    val checking = state.exitStatus == ExitCheckStatus.Checking
    ToolCard(
        title = "出口检测",
        subtitle = "分别从本机直连和所选节点出去各查一次出口 IP，用来确认节点到底有没有生效。",
    ) {
        ExitLine("直连出口", state.directExit)
        ExitLine("代理出口", state.proxyExit)
        if (state.proxyExit.ip.isBlank() && !checking) {
            Hint("代理出口为空：需要先启动接管并选中一个节点。")
        }
        if (state.exitMessage.isNotBlank()) Hint(state.exitMessage, warn = true)
        HorizontalDivider()
        Button(
            onClick = onTestExit,
            enabled = !checking,
            modifier = Modifier.testTag("toolbox-exit-test"),
        ) {
            Icon(Icons.Default.Refresh, contentDescription = null, modifier = Modifier.size(18.dp))
            Spacer(Modifier.width(6.dp))
            Text(if (checking) "检测中…" else "重新检测")
        }
    }
}

@Composable
private fun ExitLine(label: String, info: ExitInfo) {
    Row(verticalAlignment = Alignment.Top) {
        Text(
            label,
            modifier = Modifier.width(76.dp),
            style = MaterialTheme.typography.bodyMedium,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Column(Modifier.weight(1f)) {
            Text(
                info.ip.ifBlank { "—" },
                style = MaterialTheme.typography.bodyMedium,
                fontFamily = FontFamily.Monospace,
                fontWeight = FontWeight.Medium,
            )
            if (info.location.isNotBlank()) {
                Text(
                    info.location,
                    style = MaterialTheme.typography.bodySmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                )
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 规则命中预判
// ---------------------------------------------------------------------------

@Composable
private fun RoutePreviewCard(state: AppState) {
    var query by rememberSaveable { mutableStateOf("") }
    var preview by remember { mutableStateOf<NetworkTools.RoutePreview?>(null) }

    ToolCard(
        title = "规则命中预判",
        subtitle = "按当前模式、常用站点与自定义规则的匹配顺序，判断某个域名 / IP 会从哪条出站走。",
    ) {
        OutlinedTextField(
            value = query,
            onValueChange = { query = it },
            modifier = Modifier.fillMaxWidth().testTag("toolbox-route-input"),
            label = { Text("域名或 IP") },
            placeholder = { Text("chatgpt.com") },
            singleLine = true,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Button(
                onClick = { preview = NetworkTools.previewRoute(state, query) },
                modifier = Modifier.testTag("toolbox-route-run"),
            ) {
                Icon(Icons.Default.PlayArrow, contentDescription = null, modifier = Modifier.size(18.dp))
                Spacer(Modifier.width(6.dp))
                Text("预判")
            }
            OutlinedButton(
                onClick = { query = ""; preview = null },
                enabled = query.isNotBlank() || preview != null,
            ) { Text("清空") }
        }
        preview?.let { result ->
            OutboundBadge(result.outbound)
            Text(result.reason, style = MaterialTheme.typography.bodySmall)
            result.notes.forEach { Hint("注意：$it", warn = true) }
        }
    }
}

@Composable
private fun OutboundBadge(outbound: NetworkTools.Outbound) {
    val background = when (outbound) {
        NetworkTools.Outbound.Proxy -> MaterialTheme.colorScheme.secondaryContainer
        NetworkTools.Outbound.Smart -> MaterialTheme.colorScheme.primaryContainer
        NetworkTools.Outbound.Direct -> MaterialTheme.colorScheme.surfaceVariant
    }
    val foreground = when (outbound) {
        NetworkTools.Outbound.Proxy -> MaterialTheme.colorScheme.onSecondaryContainer
        NetworkTools.Outbound.Smart -> MaterialTheme.colorScheme.onPrimaryContainer
        NetworkTools.Outbound.Direct -> MaterialTheme.colorScheme.onSurfaceVariant
    }
    Text(
        "→ ${outbound.label}",
        modifier = Modifier
            .testTag("toolbox-route-outbound")
            .background(background, RoundedCornerShape(6.dp))
            .padding(horizontal = 10.dp, vertical = 5.dp),
        color = foreground,
        fontWeight = FontWeight.Bold,
        style = MaterialTheme.typography.bodyMedium,
    )
}

// ---------------------------------------------------------------------------
// DNS 查询
// ---------------------------------------------------------------------------

@Composable
private fun DnsCard(state: AppState) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var name by rememberSaveable { mutableStateOf("") }
    var type by rememberSaveable { mutableStateOf("A") }
    var viaProxy by rememberSaveable { mutableStateOf(false) }
    var busy by remember { mutableStateOf(false) }
    var result by remember { mutableStateOf<NetworkTools.DnsResult?>(null) }
    val proxyPort = state.localProxyActualPort.takeIf { it > 0 }

    ToolCard(
        title = "DNS 查询",
        subtitle = "走 DoH（dns.google）查询，可勾选从本地代理端口出去，用来对比直连与代理的解析结果。",
    ) {
        OutlinedTextField(
            value = name,
            onValueChange = { name = it },
            modifier = Modifier.fillMaxWidth().testTag("toolbox-dns-input"),
            label = { Text("域名") },
            placeholder = { Text("example.com") },
            singleLine = true,
        )
        // 命中本地映射时先把话说清楚：这张卡片是直接问 dns.google 的，
        // 走的是 DoH，不经过内核，所以结果**不会**体现本地映射——免得用户
        // 看到 DoH 和浏览器里的解析结果不一样，以为映射没生效。
        localMappingFor(state, name)?.let { entry ->
            Hint(
                "本地 DNS 映射：${entry.domain} → ${entry.address}。" +
                    "注意本卡片是直连 dns.google 查询，不受映射影响；映射只作用于内核（被 TUN 接管的应用）。",
            )
        }
        Row(
            modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            NetworkTools.dnsRecordTypes.forEach { item ->
                FilterChip(
                    selected = type == item,
                    onClick = { type = item },
                    label = { Text(item) },
                    modifier = Modifier.testTag("toolbox-dns-type-$item"),
                )
            }
        }
        Row(
            modifier = Modifier.fillMaxWidth(),
            verticalAlignment = Alignment.CenterVertically,
            horizontalArrangement = Arrangement.SpaceBetween,
        ) {
            Column(Modifier.weight(1f)) {
                Text("经本地代理查询", style = MaterialTheme.typography.bodyMedium)
                Hint(
                    if (proxyPort != null) {
                        "从 127.0.0.1:$proxyPort 出去，解析结果来自节点侧"
                    } else {
                        "本地代理未监听，先在「设置」里打开本地代理端口"
                    },
                )
            }
            Switch(
                checked = viaProxy && proxyPort != null,
                enabled = proxyPort != null && !busy,
                onCheckedChange = { viaProxy = it },
                modifier = Modifier.testTag("toolbox-dns-via-proxy"),
            )
        }
        Button(
            onClick = {
                busy = true
                result = null
                scope.launch {
                    val outcome = withContext(Dispatchers.IO) {
                        runCatching {
                            NetworkTools.queryDns(
                                name = name,
                                type = type,
                                proxyPort = if (viaProxy) proxyPort else null,
                            )
                        }
                    }
                    busy = false
                    outcome
                        .onSuccess { result = it }
                        .onFailure {
                            Toast.makeText(context, it.message ?: "DNS 查询失败", Toast.LENGTH_SHORT).show()
                        }
                }
            },
            enabled = !busy && name.isNotBlank(),
            modifier = Modifier.testTag("toolbox-dns-run"),
        ) {
            BusyButtonContent(busy = busy, idle = "查询", working = "查询中…")
        }
        result?.let { outcome ->
            HorizontalDivider()
            Hint(outcome.detail)
            outcome.records.forEach { record ->
                Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    Text(
                        record.type,
                        modifier = Modifier.width(56.dp),
                        style = MaterialTheme.typography.bodySmall,
                        color = MaterialTheme.colorScheme.secondary,
                        fontWeight = FontWeight.Bold,
                    )
                    Text(
                        record.value,
                        modifier = Modifier.weight(1f),
                        style = MaterialTheme.typography.bodySmall,
                        fontFamily = FontFamily.Monospace,
                    )
                    if (record.ttl > 0) {
                        Text(
                            "${record.ttl}s",
                            style = MaterialTheme.typography.bodySmall,
                            color = MaterialTheme.colorScheme.onSurfaceVariant,
                        )
                    }
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 证书检测
// ---------------------------------------------------------------------------

/**
 * 「局域网里用域名访问、证书还能过校验」由两件事组成：域名解析要对（本地 DNS
 * 映射），证书链要被信任且名字要对得上。这张卡片负责确认后半段——判定用的是
 * 平台默认信任库，结论等同于「浏览器打开会不会报红」。
 */
@Composable
private fun CertificateCard(state: AppState) {
    val context = LocalContext.current
    val scope = rememberCoroutineScope()
    var target by rememberSaveable { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var result by remember { mutableStateOf<NetworkTools.CertificateProbe?>(null) }

    ToolCard(
        title = "证书检测",
        subtitle = "对「域名:端口」做一次真实 TLS 握手，看系统会不会认这张证书。",
    ) {
        OutlinedTextField(
            value = target,
            onValueChange = { target = it },
            modifier = Modifier.fillMaxWidth().testTag("toolbox-cert-input"),
            label = { Text("域名或 域名:端口") },
            placeholder = { Text("nas.lan 或 nas.lan:8443") },
            singleLine = true,
        )
        localMappingFor(state, NetworkTools.parseHostPort(target).first)?.let { entry ->
            Hint("该域名有本地映射：${entry.domain} → ${entry.address}")
        }
        Button(
            onClick = {
                busy = true
                result = null
                val (host, port) = NetworkTools.parseHostPort(target)
                scope.launch {
                    val outcome = withContext(Dispatchers.IO) {
                        runCatching { NetworkTools.probeCertificate(host, port) }
                    }
                    busy = false
                    outcome
                        .onSuccess { result = it }
                        .onFailure {
                            Toast.makeText(context, it.message ?: "证书检测失败", Toast.LENGTH_SHORT)
                                .show()
                        }
                }
            },
            enabled = !busy && target.isNotBlank(),
            modifier = Modifier.testTag("toolbox-cert-run"),
        ) {
            BusyButtonContent(busy = busy, idle = "检测", working = "检测中…")
        }
        result?.let { probe ->
            HorizontalDivider()
            if (probe.resolvedAddresses.isNotEmpty()) {
                Text(
                    "${probe.host}:${probe.port} → ${probe.resolvedAddresses.joinToString(", ")}",
                    style = MaterialTheme.typography.bodySmall,
                    fontFamily = FontFamily.Monospace,
                )
            }
            CertVerdict("系统信任", probe.trustedBySystem)
            if (probe.reachable) CertVerdict("域名匹配", probe.hostnameVerified)
            Hint(probe.detail, warn = !probe.trustedBySystem || !probe.hostnameVerified)
            probe.chain.forEachIndexed { index, certificate ->
                CertDetail(index = index, certificate = certificate)
            }
        }
    }
}

/** 查询名归一后去映射表里精确匹配——和内核那边 hosts 服务器的匹配方式一致。 */
internal fun localMappingFor(state: AppState, rawName: String): LocalDnsEntry? {
    val target = normalizeLocalDnsDomain(rawName)
    if (target.isEmpty()) return null
    return state.localDnsEntries.firstOrNull { it.domain == target }
}

@Composable
private fun CertVerdict(label: String, ok: Boolean) {
    Row(
        modifier = Modifier.fillMaxWidth(),
        horizontalArrangement = Arrangement.spacedBy(8.dp),
        verticalAlignment = Alignment.CenterVertically,
    ) {
        Text(
            if (ok) "✓" else "✗",
            fontWeight = FontWeight.Bold,
            color = if (ok) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.error,
        )
        Text(label, style = MaterialTheme.typography.bodyMedium)
    }
}

@Composable
private fun CertDetail(index: Int, certificate: NetworkTools.CertificateInfo) {
    Column(
        modifier = Modifier.fillMaxWidth(),
        verticalArrangement = Arrangement.spacedBy(2.dp),
    ) {
        Text(
            if (index == 0) "服务器证书" else "中间证书 $index",
            style = MaterialTheme.typography.bodySmall,
            fontWeight = FontWeight.SemiBold,
        )
        CertField("主体", certificate.subject)
        CertField("签发者", certificate.issuer + if (certificate.selfSigned) "（自签）" else "")
        CertField("有效期", "${certificate.notBefore} ~ ${certificate.notAfter}")
        CertField("签名算法", certificate.signatureAlgorithm)
        if (certificate.alternativeNames.isNotEmpty()) {
            CertField("覆盖域名", certificate.alternativeNames.joinToString(", "))
        }
    }
}

@Composable
private fun CertField(label: String, value: String) {
    Row(modifier = Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        Text(
            label,
            modifier = Modifier.width(64.dp),
            style = MaterialTheme.typography.bodySmall,
            color = MaterialTheme.colorScheme.onSurfaceVariant,
        )
        Text(
            value,
            modifier = Modifier.weight(1f),
            style = MaterialTheme.typography.bodySmall,
        )
    }
}

// ---------------------------------------------------------------------------
// TCP 连通性
// ---------------------------------------------------------------------------

@Composable
private fun TcpCard() {
    val scope = rememberCoroutineScope()
    var host by rememberSaveable { mutableStateOf("") }
    var port by rememberSaveable { mutableStateOf("") }
    var busy by remember { mutableStateOf(false) }
    var result by remember { mutableStateOf<NetworkTools.TcpProbeResult?>(null) }

    ToolCard(
        title = "连通性测试",
        subtitle = "对 host:port 做一次直连 TCP 握手（不经代理），用来区分「端口不通」和「代理没生效」。",
    ) {
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp), verticalAlignment = Alignment.CenterVertically) {
            OutlinedTextField(
                value = host,
                onValueChange = { host = it },
                modifier = Modifier.weight(1f).testTag("toolbox-tcp-host"),
                label = { Text("主机") },
                placeholder = { Text("1.1.1.1") },
                singleLine = true,
            )
            OutlinedTextField(
                value = port,
                onValueChange = { value -> port = value.filter(Char::isDigit).take(5) },
                modifier = Modifier.width(104.dp).testTag("toolbox-tcp-port"),
                label = { Text("端口") },
                singleLine = true,
                keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Number),
            )
        }
        Button(
            onClick = {
                busy = true
                result = null
                scope.launch {
                    val outcome = withContext(Dispatchers.IO) {
                        NetworkTools.probeTcp(host, port.toIntOrNull() ?: -1)
                    }
                    busy = false
                    result = outcome
                }
            },
            enabled = !busy && host.isNotBlank() && port.isNotBlank(),
            modifier = Modifier.testTag("toolbox-tcp-run"),
        ) {
            BusyButtonContent(busy = busy, idle = "测试", working = "测试中…")
        }
        result?.let { outcome ->
            HorizontalDivider()
            Text(
                if (outcome.reachable) "✓ 可连接" else "✗ 不可连接",
                style = MaterialTheme.typography.bodyMedium,
                fontWeight = FontWeight.Bold,
                color = if (outcome.reachable) {
                    MaterialTheme.colorScheme.primary
                } else {
                    MaterialTheme.colorScheme.error
                },
            )
            Hint(outcome.detail)
        }
    }
}

// ---------------------------------------------------------------------------
// 编解码 / 链接解析
// ---------------------------------------------------------------------------

internal enum class TextTool(val label: String) {
    Base64Encode("Base64 编码"),
    Base64Decode("Base64 解码"),
    UrlEncode("URL 编码"),
    UrlDecode("URL 解码"),
    Uuid("UUID"),
    ParseLink("解析节点链接"),
}

@Composable
private fun TextToolCard() {
    val context = LocalContext.current
    var tool by rememberSaveable { mutableStateOf(TextTool.Base64Encode) }
    var input by rememberSaveable { mutableStateOf("") }
    var output by remember { mutableStateOf("") }

    ToolCard(
        title = "编解码与链接解析",
        subtitle = "处理订阅里常见的文本：Base64 / URL 编解码、生成 UUID，以及把 ss:// vmess:// 这类分享链接拆成字段。",
    ) {
        Row(
            modifier = Modifier.fillMaxWidth().horizontalScroll(rememberScrollState()),
            horizontalArrangement = Arrangement.spacedBy(8.dp),
        ) {
            TextTool.entries.forEach { item ->
                FilterChip(
                    selected = tool == item,
                    onClick = { tool = item; output = "" },
                    label = { Text(item.label) },
                    modifier = Modifier.testTag("toolbox-text-${item.name.lowercase()}"),
                )
            }
        }
        if (tool != TextTool.Uuid) {
            OutlinedTextField(
                value = input,
                onValueChange = { input = it },
                modifier = Modifier.fillMaxWidth().testTag("toolbox-text-input"),
                label = { Text(if (tool == TextTool.ParseLink) "分享链接" else "内容") },
                minLines = if (tool == TextTool.ParseLink) 2 else 1,
                maxLines = 5,
            )
        }
        Row(horizontalArrangement = Arrangement.spacedBy(10.dp)) {
            Button(
                onClick = { output = runTextTool(tool, input) },
                enabled = tool == TextTool.Uuid || input.isNotBlank(),
                modifier = Modifier.testTag("toolbox-text-run"),
            ) { Text(if (tool == TextTool.Uuid) "生成" else "执行") }
            OutlinedButton(
                onClick = { input = ""; output = "" },
                enabled = input.isNotBlank() || output.isNotBlank(),
            ) { Text("清空") }
        }
        if (output.isNotBlank()) {
            HorizontalDivider()
            Text(
                output,
                modifier = Modifier.fillMaxWidth().testTag("toolbox-text-output"),
                style = MaterialTheme.typography.bodySmall,
                fontFamily = FontFamily.Monospace,
            )
            TextButton(onClick = { copyToClipboard(context, output) }) {
                Icon(Icons.Default.ContentCopy, contentDescription = null, modifier = Modifier.size(16.dp))
                Spacer(Modifier.width(6.dp))
                Text("复制结果")
            }
        }
    }
}

/** 纯函数，方便单测；失败时返回可读的说明而不是抛异常。 */
internal fun runTextTool(tool: TextTool, input: String): String = when (tool) {
    TextTool.Base64Encode -> NetworkTools.encodeBase64(input)
    TextTool.Base64Decode -> NetworkTools.decodeBase64(input)
        ?: "解码失败：不是合法的 Base64（已兼容 URL-safe 与缺失 padding）"
    TextTool.UrlEncode -> NetworkTools.encodeUrl(input)
    TextTool.UrlDecode -> NetworkTools.decodeUrl(input) ?: "解码失败：不是合法的 URL 编码"
    TextTool.Uuid -> NetworkTools.randomUuid()
    TextTool.ParseLink -> NetworkTools.describeShareLink(input)
}
