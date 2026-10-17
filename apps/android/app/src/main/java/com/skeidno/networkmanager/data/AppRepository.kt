package com.skeidno.networkmanager.data

import android.content.Context
import android.content.SharedPreferences
import com.skeidno.networkmanager.vpn.SingBoxConfigBuilder
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.coroutineScope
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.withContext
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Proxy
import java.net.ServerSocket
import java.net.Socket
import java.net.URL
import java.time.OffsetDateTime
import java.util.UUID

private fun normalizeNodeGroup(value: String): String =
    value.trim().replace(Regex("\\s+"), " ").take(40)

/**
 * 出口检测端点。
 *
 * 直连优先用国内的 ipip / 3322：它们不在默认代理域名表里，接管状态下也是直连出去，
 * 测到的才是本机真实 IP（ip-api 作兜底）。
 * 代理优先用境外的 ip-api（带 lang=zh-CN 拿中文归属地）：反过来让节点去访问国内站点
 * 经常超时，实测通过节点打开 ipip 会卡住。
 */
private val DIRECT_EXIT_ENDPOINTS = listOf(
    "http://myip.ipip.net",
    "http://ip.3322.net",
    "http://ip-api.com/json/?lang=zh-CN&fields=status,message,query,country,regionName,city",
)

private val PROXY_EXIT_ENDPOINTS = listOf(
    "http://ip-api.com/json/?lang=zh-CN&fields=status,message,query,country,regionName,city",
    "http://myip.ipip.net",
)

private val IPV4 = Regex("""\d{1,3}(?:\.\d{1,3}){3}""")

class AppRepository private constructor(private val context: Context) {
    private val profileStore = ProfileStore.get(context)

    /** 当前生效的档案。所有配置读写都发生在它自己的偏好文件里。 */
    @Volatile
    private var profileId: String = profileStore.activeId()

    @Volatile
    private var preferences: SharedPreferences =
        context.getSharedPreferences(profilePrefsName(profileId), Context.MODE_PRIVATE)

    private val mutableState = MutableStateFlow(load())
    val state: StateFlow<AppState> = mutableState.asStateFlow()

    fun activeProfileId(): String = profileId

    /**
     * 切换档案：偏好文件与数据目录整体换根，然后重新从新档案读书。
     *
     * 调用方必须**先停掉接管**——正在跑的 sing-box 还握着旧档案的配置与 working 目录，
     * 换根之后再去 reload 会读到一半新一半旧的东西。
     */
    fun switchProfile(id: String) {
        if (id == profileId) return
        if (!profileStore.setActive(id)) return
        profileId = id
        preferences = context.getSharedPreferences(profilePrefsName(id), Context.MODE_PRIVATE)
        probePort = 0
        mutableState.value = load()
    }

    fun setMode(mode: RoutingMode) = update(mutableState.value.copy(mode = mode))

    fun setFallback(target: FallbackTarget) = update(mutableState.value.copy(fallbackTarget = target))

    fun setRuleGroupEnabled(enabled: Boolean) {
        val current = mutableState.value
        update(current.copy(ruleGroup = current.ruleGroup.copy(enabled = enabled)))
    }

    fun setRuleGroupDomains(values: List<String>): Int {
        val domains = ruleGroupDomainsFromValues(values)
        val current = mutableState.value
        update(current.copy(ruleGroup = current.ruleGroup.copy(domains = domains)))
        return domains.size
    }

    fun setCommonRuleTarget(target: FallbackTarget) =
        update(mutableState.value.copy(commonRuleTarget = target))

    fun setLocalProxyEnabled(enabled: Boolean) =
        update(mutableState.value.copy(localProxyEnabled = enabled))

    fun setLocalProxyPort(port: Int) {
        require(port in LOCAL_PROXY_PORT_MIN..LOCAL_PROXY_PORT_MAX) {
            "端口需要在 $LOCAL_PROXY_PORT_MIN 到 $LOCAL_PROXY_PORT_MAX 之间"
        }
        update(mutableState.value.copy(localProxyPort = port))
    }

    /**
     * 保存本地 DNS 映射。
     *
     * 落盘的只有域名和地址：这是**设备本地**配置（同一个域名在公司和在家要指到
     * 不同机器），所以刻意不进跨设备导入导出的那份文件。
     */
    fun setLocalDnsEntries(entries: List<LocalDnsEntry>): Int {
        val sanitized = sanitizeLocalDnsEntries(entries)
        update(mutableState.value.copy(localDnsEntries = sanitized))
        return sanitized.size
    }

    /** 收编辑器里的多行文本：解析 → 校验 → 落盘，有非法行就抛出去让界面提示。 */
    fun setLocalDnsFromText(text: String): Int = setLocalDnsEntries(localDnsEntriesFromText(text))

    /** 本地证书状态是运行态（local-certs/ 下的文件才是事实来源），不进配置。 */
    fun setLocalCertStatus(status: LocalCertStatus) =
        update(mutableState.value.copy(localCert = status), persist = false)

    /**
     * 改写当前档案接管的 App 名单（空 = 接管全部 App）。
     *
     * 名单的落点在**档案清单**里（跟着档案走），不属于这份档案偏好，所以这里只刷新运行态快照。
     * 内核侧要重启接管才会按新名单建立 TUN —— 名单是 `addAllowedApplication` 级别的，
     * 建接口时定死，运行时改不了。
     */
    fun setTunPackages(packages: List<String>): List<String> {
        profileStore.setPackages(profileId, packages)
        val applied = profileStore.profiles().firstOrNull { it.id == profileId }?.packages.orEmpty()
        update(mutableState.value.copy(tunPackages = applied), persist = false)
        return applied
    }

    fun savePortableRules(
        index: Int?,
        type: String,
        values: List<String>,
        target: FallbackTarget,
    ): Int {
        val current = mutableState.value
        if (index != null) require(index in current.portableRules.indices) { "规则不存在或已被删除" }
        val existing = index?.let(current.portableRules::get)
        val replacements = portableRulesFromValues(
            type = type,
            values = values,
            target = target,
            enabled = existing?.enabled ?: true,
            note = existing?.note.orEmpty(),
        )
        val rules = current.portableRules.toMutableList().apply {
            if (index == null) {
                addAll(replacements)
            } else {
                removeAt(index)
                addAll(index, replacements)
            }
        }
        update(current.copy(portableRules = rules))
        return replacements.size
    }

    fun deletePortableRule(index: Int) {
        val current = mutableState.value
        if (index !in current.portableRules.indices) return
        update(current.copy(portableRules = current.portableRules.filterIndexed { itemIndex, _ -> itemIndex != index }))
    }

    fun selectNode(id: String) {
        if (mutableState.value.nodes.none { it.id == id }) return
        update(mutableState.value.copy(selectedNodeId = id))
    }

    fun deleteNode(id: String) {
        val current = mutableState.value
        val nodes = current.nodes.filterNot { it.id == id }
        val selected = if (current.selectedNodeId == id) nodes.firstOrNull()?.id.orEmpty() else current.selectedNodeId
        val mode = if (nodes.isEmpty() && current.mode in setOf(RoutingMode.Global, RoutingMode.Smart)) {
            RoutingMode.Rule
        } else {
            current.mode
        }
        update(current.copy(nodes = nodes, selectedNodeId = selected, mode = mode))
    }

    fun createNodeGroup(name: String) {
        val group = normalizeNodeGroup(name)
        require(group.isNotBlank()) { "分组名称不能为空" }
        val current = mutableState.value
        if (group in current.nodeGroups) return
        update(current.copy(nodeGroups = current.nodeGroups + group))
    }

    fun assignNodeGroup(id: String, name: String) {
        val group = normalizeNodeGroup(name)
        val current = mutableState.value
        require(group.isBlank() || group in current.nodeGroups) { "分组不存在" }
        if (current.nodes.none { it.id == id }) return
        update(current.copy(nodes = current.nodes.map { if (it.id == id) it.copy(group = group) else it }))
    }

    fun deleteNodeGroup(name: String) {
        val group = normalizeNodeGroup(name)
        val current = mutableState.value
        if (group !in current.nodeGroups) return
        update(
            current.copy(
                nodeGroups = current.nodeGroups.filterNot { it == group },
                nodes = current.nodes.map { if (it.group == group) it.copy(group = "") else it },
                subscriptions = current.subscriptions.map {
                    if (it.group == group) it.copy(group = "") else it
                },
            ),
        )
    }

    fun deleteErrorNodes(): Int {
        val current = mutableState.value
        val failedIds = current.nodes
            .filter { it.latencyStatus == LatencyStatus.Error }
            .mapTo(mutableSetOf()) { it.id }
        if (failedIds.isEmpty()) return 0
        val nodes = current.nodes.filterNot { it.id in failedIds }
        val selected = current.selectedNodeId.takeIf { id -> nodes.any { it.id == id } }
            ?: nodes.firstOrNull()?.id.orEmpty()
        val mode = if (nodes.isEmpty() && current.mode in setOf(RoutingMode.Global, RoutingMode.Smart)) {
            RoutingMode.Rule
        } else {
            current.mode
        }
        update(current.copy(nodes = nodes, selectedNodeId = selected, mode = mode))
        return failedIds.size
    }

    fun importText(
        content: String,
        sourceName: String = "粘贴导入",
        group: String = "",
    ): Int {
        val normalizedGroup = normalizeNodeGroup(group)
        require(normalizedGroup.isBlank() || normalizedGroup in mutableState.value.nodeGroups) {
            "导入分组不存在"
        }
        val sourceId = "manual-${UUID.randomUUID()}"
        val imported = SubscriptionParser.parse(content, sourceId, sourceName)
            .map { it.copy(group = normalizedGroup) }
        if (imported.isEmpty()) error("没有识别到可用节点")
        mergeNodes(imported)
        return imported.size
    }

    suspend fun addSubscription(
        name: String,
        url: String,
        group: String = "",
    ): Int = withContext(Dispatchers.IO) {
        val current = mutableState.value
        val existing = current.subscriptions.firstOrNull { it.url == url }
        val selectedGroup = normalizeNodeGroup(group).ifBlank { existing?.group.orEmpty() }
        require(selectedGroup.isBlank() || selectedGroup in current.nodeGroups) {
            "订阅分组不存在"
        }
        val sourceId = existing?.id ?: UUID.randomUUID().toString()
        val content = SubscriptionParser.fetch(url)
        val displayName = name.trim().ifBlank { URLName.from(url) }
        val imported = SubscriptionParser.parse(content, sourceId, displayName)
        if (imported.isEmpty()) error("订阅中没有识别到可用节点")
        val previousGroups = current.nodes
            .filter { it.sourceId == sourceId && it.group.isNotBlank() }
            .associate { it.name to it.group }
        val groupedImported = imported.map {
            it.copy(group = previousGroups[it.name] ?: selectedGroup)
        }
        val retained = current.nodes.filterNot { it.sourceId == sourceId }
        val nodes = uniqueNames(retained + groupedImported)
        val subscription = Subscription(
            id = sourceId,
            name = displayName,
            url = url,
            updatedAt = OffsetDateTime.now().toString(),
            nodeCount = imported.size,
            group = selectedGroup,
        )
        val subscriptions = current.subscriptions.filterNot { it.id == sourceId } + subscription
        val selected = current.selectedNodeId.takeIf { id -> nodes.any { it.id == id } } ?: nodes.first().id
        update(current.copy(nodes = nodes, subscriptions = subscriptions, selectedNodeId = selected))
        imported.size
    }

    suspend fun refreshSubscription(id: String): Int {
        val subscription = mutableState.value.subscriptions.firstOrNull { it.id == id }
            ?: error("订阅不存在")
        return addSubscription(subscription.name, subscription.url, subscription.group)
    }

    fun deleteSubscription(id: String) {
        val current = mutableState.value
        val nodes = current.nodes.filterNot { it.sourceId == id }
        val selected = current.selectedNodeId.takeIf { selectedId -> nodes.any { it.id == selectedId } }
            ?: nodes.firstOrNull()?.id.orEmpty()
        update(
            current.copy(
                nodes = nodes,
                subscriptions = current.subscriptions.filterNot { it.id == id },
                selectedNodeId = selected,
            ),
        )
    }

    suspend fun testAllNodes() = coroutineScope {
        val current = mutableState.value
        update(
            current.copy(
                nodes = current.nodes.map { it.copy(latencyMs = null, latencyStatus = LatencyStatus.Testing) },
            ),
            persist = false,
        )
        val tested = current.nodes.map { node -> async(Dispatchers.IO) { testEndpoint(node) } }.awaitAll()
            .sortedWith(compareBy<ProxyNode> { it.latencyStatus == LatencyStatus.Error }.thenBy { it.latencyMs ?: Int.MAX_VALUE })
        update(mutableState.value.copy(nodes = tested))
    }

    /**
     * 检测两个出口：直连（本机真实 IP）与代理（经过所选节点的出口）。
     *
     * 代理检测必须走 [SingBoxConfigBuilder.probeInboundTag] 那个入站 —— 内核规则会把它强制
     * 指向节点出站，否则检测请求会按普通分流规则走直连，"代理出口"就永远等于"直连出口"。
     */
    suspend fun checkExit() {
        val current = mutableState.value
        update(
            current.copy(exitStatus = ExitCheckStatus.Checking, exitMessage = ""),
            persist = false,
        )
        val direct = withContext(Dispatchers.IO) {
            runCatching { probeExit(proxyPort = null, endpoints = DIRECT_EXIT_ENDPOINTS) }
        }
        val proxy = withContext(Dispatchers.IO) {
            when {
                current.nodes.isEmpty() -> Result.failure(IllegalStateException("还没有节点，先导入订阅或分享链接"))
                !current.running -> Result.failure(IllegalStateException("请先启动接管"))
                probePort <= 0 -> Result.failure(IllegalStateException("检测端口未就绪，请重新启动接管"))
                else -> runCatching { probeExit(proxyPort = probePort, endpoints = PROXY_EXIT_ENDPOINTS) }
            }
        }

        val directInfo = direct.getOrNull() ?: ExitInfo()
        val proxyInfo = proxy.getOrNull() ?: ExitInfo()
        val message = buildList {
            direct.exceptionOrNull()?.let { add("直连出口检测失败（${it.message ?: "未知错误"}）") }
            proxy.exceptionOrNull()?.let { add("代理出口检测失败（${it.message ?: "未知错误"}）") }
            if (direct.isSuccess && proxy.isSuccess && directInfo.ip == proxyInfo.ip) {
                add("直连与代理出口相同（${proxyInfo.ip}），所选节点可能没有生效")
            }
        }.joinToString("；")
        val now = mutableState.value
        update(
            now.copy(
                exitStatus = if (proxy.isSuccess) ExitCheckStatus.Done else ExitCheckStatus.Failed,
                directExit = directInfo,
                proxyExit = proxyInfo,
                exitMessage = message,
            ),
            persist = false,
        )
    }

    private fun allocateLoopbackPort(): Int = runCatching {
        ServerSocket(0, 1, InetAddress.getByName("127.0.0.1")).use { it.localPort }
    }.getOrDefault(0)

    private fun isLoopbackPortFree(port: Int): Boolean = runCatching {
        ServerSocket(port, 1, InetAddress.getByName("127.0.0.1")).use { true }
    }.getOrDefault(false)

    /**
     * 挑本地代理实际监听的端口。
     *
     * 用户填的端口可能被别的进程占着，而 sing-box 碰到端口占用是**整个内核启动失败**，
     * 不是跳过这个入站 —— 所以这里先探测，占了就往后顺延，并把实际端口回写到状态里
     * 让界面显示真实地址。
     */
    private fun resolveLocalProxyPort(preferred: Int, reservedProbePort: Int): Int {
        var candidate = preferred
        repeat(16) {
            if (candidate > LOCAL_PROXY_PORT_MAX) candidate = LOCAL_PROXY_PORT_MIN
            if (candidate != reservedProbePort && isLoopbackPortFree(candidate)) return candidate
            candidate += 1
        }
        val allocated = allocateLoopbackPort()
        if (allocated > 0 && allocated != reservedProbePort) return allocated
        return preferred
    }

    private fun probeExit(proxyPort: Int?, endpoints: List<String>): ExitInfo {
        var lastError: Exception? = null
        endpoints.forEach { endpoint ->
            runCatching { fetchExit(endpoint, proxyPort) }
                .onSuccess { return it }
                .onFailure { lastError = it as? Exception }
        }
        throw lastError ?: IllegalStateException("所有检测端点都失败")
    }

    private fun fetchExit(endpoint: String, proxyPort: Int?): ExitInfo {
        val connection = if (proxyPort != null) {
            URL(endpoint).openConnection(
                Proxy(Proxy.Type.HTTP, InetSocketAddress("127.0.0.1", proxyPort)),
            )
        } else {
            URL(endpoint).openConnection(Proxy.NO_PROXY)
        } as HttpURLConnection
        connection.connectTimeout = 8_000
        connection.readTimeout = 10_000
        connection.setRequestProperty("User-Agent", "NetworkManager-Android")
        return try {
            if (connection.responseCode !in 200..299) error("HTTP ${connection.responseCode}")
            val body = connection.inputStream.bufferedReader().use { it.readText() }
            parseExit(body)
        } finally {
            connection.disconnect()
        }
    }

    private fun parseExit(body: String): ExitInfo {
        val text = body.trim()
        if (text.startsWith("{")) {
            val json = JSONObject(text)
            if (json.optString("status") == "fail") {
                error(json.optString("message").ifBlank { "检测端点返回失败" })
            }
            val ip = json.optString("query").ifBlank { error("检测端点没有返回 IP") }
            val location = listOf(
                json.optString("country"),
                json.optString("regionName"),
                json.optString("city"),
            ).filter { it.isNotBlank() && it != "null" }.joinToString(" ")
            return ExitInfo(ip, location)
        }
        // ipip 的文本格式：当前 IP：1.2.3.4  来自于：中国 浙江 杭州  电信
        val ip = IPV4.find(text)?.value ?: error("检测端点没有返回 IP")
        val location = text.substringAfter("来自于：", "").replace(Regex("\\s+"), " ").trim()
        return ExitInfo(ip, location)
    }

    suspend fun testNode(id: String): ProxyNode {        val current = mutableState.value
        val node = current.nodes.firstOrNull { it.id == id } ?: error("节点不存在或已被删除")
        update(
            current.copy(
                nodes = current.nodes.map {
                    if (it.id == id) it.copy(latencyMs = null, latencyStatus = LatencyStatus.Testing) else it
                },
            ),
            persist = false,
        )
        val tested = withContext(Dispatchers.IO) { testEndpoint(node) }
        val nodes = mutableState.value.nodes.map { if (it.id == id) tested else it }
            .sortedWith(
                compareBy<ProxyNode> { it.latencyStatus == LatencyStatus.Error }
                    .thenBy { it.latencyStatus == LatencyStatus.Idle }
                    .thenBy { it.latencyMs ?: Int.MAX_VALUE },
            )
        update(mutableState.value.copy(nodes = nodes))
        return tested
    }

    /** 内核配置落在当前档案自己的目录里 —— 换档案就等于换了一份 runtime.json。 */
    fun runtimeConfigFile(): File =
        File(context.profilePaths(profileId).dataRoot, PROFILE_RUNTIME_FILE)

    /** 出口检测专用入站当前监听的端口，0 表示没有（配置里没写或还没有节点）。 */
    @Volatile
    private var probePort: Int = 0

    /** 内核控制接口当前监听的端口，0 表示没开（没有节点或没分配成功）。 */
    @Volatile
    private var clashApiPort: Int = 0

    fun writeRuntimeConfig(): File {
        val state = mutableState.value
        // 每次生成配置时重新挑一个空闲端口：sing-box 碰到端口占用会直接启动失败，
        // 而 App 挑完端口立刻就用它发请求，不存在「配置里写的」和「实际监听的」不一致的问题。
        probePort = if (state.nodes.isEmpty()) 0 else allocateLoopbackPort()
        clashApiPort = if (state.nodes.isEmpty()) 0 else allocateLoopbackPort()
        val localProxyPort = if (state.localProxyEnabled) {
            resolveLocalProxyPort(state.localProxyPort, probePort)
        } else {
            0
        }
        val file = runtimeConfigFile()
        val temporary = File(file.parentFile, "runtime.tmp")
        temporary.writeText(
            SingBoxConfigBuilder.build(state, probePort, localProxyPort, clashApiPort),
            Charsets.UTF_8,
        )
        if (!temporary.renameTo(file)) {
            temporary.copyTo(file, overwrite = true)
            temporary.delete()
        }
        if (mutableState.value.localProxyActualPort != localProxyPort) {
            update(mutableState.value.copy(localProxyActualPort = localProxyPort), persist = false)
        }
        return file
    }

    fun exportPortableConfig(): String {
        val state = mutableState.value
        val nodes = JSONArray()
        state.nodes.forEach { node ->
            nodes.put(
                JSONObject()
                    .put("id", node.id)
                    .put("sourceId", node.sourceId)
                    .put("sourceName", node.sourceName)
                    .put("group", node.group)
                    .put("config", node.raw),
            )
        }
        val subscriptions = JSONArray()
        state.subscriptions.forEach { item ->
            subscriptions.put(
                JSONObject()
                    .put("id", item.id)
                    .put("name", item.name)
                    .put("url", item.url)
                    .put("updatedAt", item.updatedAt)
                    .put("group", item.group),
            )
        }
        val rules = JSONArray()
        state.portableRules.forEach { rule ->
            rules.put(
                JSONObject()
                    .put("type", rule.type)
                    .put("value", rule.value)
                    .put("target", rule.target.portableValue())
                    .put("enabled", rule.enabled)
                    .put("note", rule.note),
            )
        }
        return JSONObject()
            .put("format", "network-manager-config")
            .put("version", 1)
            .put("exportedAt", OffsetDateTime.now().toString())
            .put(
                "routing",
                JSONObject()
                    .put("mode", state.mode.portableValue())
                    .put("fallback", state.fallbackTarget.portableValue())
                    .put(
                        "commonOverseas",
                        JSONObject()
                            .put("enabled", state.ruleGroup.enabled)
                            .put("target", state.commonRuleTarget.portableValue())
                            .put("domains", JSONArray(state.ruleGroup.domains)),
                    ),
            )
            .put("selectedNodeId", state.selectedNodeId)
            .put("nodeGroups", JSONArray(state.nodeGroups))
            .put("nodes", nodes)
            .put("subscriptions", subscriptions)
            .put("rules", rules)
            .toString(2)
    }

    fun importPortableConfig(content: String): Int {
        val root = JSONObject(content)
        require(root.optString("format") == "network-manager-config") {
            "不是 Network Manager 跨设备配置"
        }
        require(root.optInt("version") == 1) { "暂不支持这个配置版本" }
        val groupsArray = root.optJSONArray("nodeGroups") ?: JSONArray()
        val nodeGroups = buildList {
            repeat(groupsArray.length()) { index ->
                normalizeNodeGroup(groupsArray.optString(index)).takeIf(String::isNotBlank)?.let {
                    if (it !in this) add(it)
                }
            }
        }.toMutableList()
        val nodesArray = root.optJSONArray("nodes") ?: JSONArray()
        require(nodesArray.length() <= 5_000) { "节点数量超过限制" }
        val nodes = List(nodesArray.length()) { index ->
            val item = nodesArray.getJSONObject(index)
            val config = item.getJSONObject("config")
            val name = config.optString("name").ifBlank { "导入节点 ${index + 1}" }
            config.put("name", name)
            require(config.optString("type").isNotBlank()) { "节点 $name 缺少协议类型" }
            require(config.optString("server").isNotBlank()) { "节点 $name 缺少服务器" }
            require(config.optInt("port") in 1..65535) { "节点 $name 端口无效" }
            val group = normalizeNodeGroup(item.optString("group"))
            if (group.isNotBlank() && group !in nodeGroups) nodeGroups.add(group)
            ProxyNode(
                id = item.optString("id").ifBlank { UUID.randomUUID().toString() },
                name = name,
                sourceId = item.optString("sourceId"),
                sourceName = item.optString("sourceName").ifBlank { "跨设备导入" },
                rawJson = config.toString(),
                group = group,
            )
        }.let(::uniqueNames)
        val subscriptionsArray = root.optJSONArray("subscriptions") ?: JSONArray()
        require(subscriptionsArray.length() <= 500) { "订阅数量超过限制" }
        val subscriptions = List(subscriptionsArray.length()) { index ->
            val item = subscriptionsArray.getJSONObject(index)
            val group = normalizeNodeGroup(item.optString("group"))
            if (group.isNotBlank() && group !in nodeGroups) nodeGroups.add(group)
            Subscription(
                id = item.optString("id").ifBlank { UUID.randomUUID().toString() },
                name = item.optString("name").ifBlank { "订阅" },
                url = item.optString("url"),
                updatedAt = item.optString("updatedAt"),
                nodeCount = nodes.count { node -> node.sourceId == item.optString("id") },
                group = group,
            )
        }.filter { it.url.isNotBlank() }
        val rulesArray = root.optJSONArray("rules") ?: JSONArray()
        require(rulesArray.length() <= 5_000) { "规则数量超过限制" }
        val rules = List(rulesArray.length()) { index ->
            val item = rulesArray.getJSONObject(index)
            val type = item.optString("type").lowercase()
            val value = item.optString("value").trim()
            portableRulesFromValues(
                type = type,
                values = listOf(value),
                target = item.optString("target").toFallbackTarget(),
                enabled = item.optBoolean("enabled", true),
                note = item.optString("note"),
            ).singleOrNull() ?: error("第 ${index + 1} 条规则无效")
        }
        val routing = root.optJSONObject("routing") ?: JSONObject()
        // 桌面端会把「常用国内站点」整组折叠进 commonChina。端上不认识这一段的话，
        // 这一组会整个丢掉 —— 国内域名跟着保底出口走，等于白配。这里展开成普通的
        // 直连规则，至少规则还在，用户也能自己增删。
        val china = routing.optJSONObject("commonChina")
        val chinaRules: List<PortableRule> = china?.optJSONArray("domains")?.let { array ->
            val values = List(array.length()) { index -> array.optString(index) }
            val enabled = china.optBoolean("enabled", true)
            buildList {
                // 一次最多 500 条，超了就分批展开，别把整份配置拒掉。
                values.chunked(500).forEach { chunk ->
                    addAll(
                        portableRulesFromValues(
                            type = "domain_suffix",
                            values = chunk,
                            target = FallbackTarget.Direct,
                            enabled = enabled,
                            note = "常用国内站点",
                        ),
                    )
                }
            }
        }.orEmpty()
        val common = routing.optJSONObject("commonOverseas") ?: JSONObject()
        val commonDomainsArray = common.optJSONArray("domains")
        val commonDomains = if (commonDomainsArray == null) {
            DEFAULT_PROXY_DOMAINS
        } else {
            ruleGroupDomainsFromValues(
                List(commonDomainsArray.length()) { index -> commonDomainsArray.optString(index) },
            )
        }
        val selected = root.optString("selectedNodeId")
            .takeIf { id -> nodes.any { it.id == id } } ?: nodes.firstOrNull()?.id.orEmpty()
        update(
            mutableState.value.copy(
                mode = when (routing.optString("mode").lowercase()) {
                    "global" -> RoutingMode.Global
                    "smart" -> RoutingMode.Smart
                    "direct" -> RoutingMode.Direct
                    else -> RoutingMode.Rule
                },
                fallbackTarget = routing.optString("fallback").toFallbackTarget(),
                selectedNodeId = selected,
                nodes = nodes,
                nodeGroups = nodeGroups,
                subscriptions = subscriptions,
                ruleGroup = defaultOverseasRuleGroup().copy(
                    domains = commonDomains,
                    enabled = common.optBoolean("enabled", true),
                ),
                commonRuleTarget = common.optString("target", "proxy").toFallbackTarget(),
                // 国内直连排在最前：它是「国内的就别绕出去」，要先于其它自定义规则命中。
                portableRules = chinaRules + rules,
            ),
        )
        return nodes.size
    }

    fun updateRuntime(running: Boolean, busy: Boolean, message: String, error: String = "") {
        update(
            mutableState.value.copy(
                running = running,
                busy = busy,
                statusMessage = message,
                error = error,
            ),
            persist = false,
        )
    }

    fun updateTraffic(download: Long, upload: Long, totalDownload: Long, totalUpload: Long) {
        val current = mutableState.value
        update(
            current.copy(
                downloadBytesPerSecond = download,
                uploadBytesPerSecond = upload,
                totalDownloadBytes = totalDownload,
                totalUploadBytes = totalUpload,
                downloadSamples = (current.downloadSamples + download).takeLast(30),
                uploadSamples = (current.uploadSamples + upload).takeLast(30),
            ),
            persist = false,
        )
    }

    private fun mergeNodes(imported: List<ProxyNode>) {
        val current = mutableState.value
        val nodes = uniqueNames(current.nodes + imported)
        val selected = current.selectedNodeId.ifBlank { nodes.first().id }
        update(current.copy(nodes = nodes, selectedNodeId = selected))
    }

    private fun uniqueNames(nodes: List<ProxyNode>): List<ProxyNode> {
        val counts = mutableMapOf<String, Int>()
        return nodes.map { node ->
            val count = (counts[node.name] ?: 0) + 1
            counts[node.name] = count
            if (count == 1) node else node.copy(name = "${node.name} ($count)")
        }
    }

    /**
     * 测一个节点到底能不能用。
     *
     * 内核在跑就一定要问内核：它的延迟测试是真的发一个 HTTP 请求走完整链路
     * （建隧道 → 认证 → 取回响应）。本机自己连只能做到 TCP 握手，中间差着
     * 「账号被拒」「对端 reset」「节点根本不转发」这一整段 —— 用户看到的
     * 「批量测试全通还很快，一用就废」就是这么差出来的。
     *
     * 内核没接管时才退回握手：那时候没有链路可问，握手至少能看出端口开没开。
     */
    private fun testEndpoint(node: ProxyNode): ProxyNode {
        if (mutableState.value.running) {
            val delay = runCatching { requestCoreDelay(node.id) }.getOrNull()
            return if (delay == null) {
                node.copy(latencyMs = null, latencyStatus = LatencyStatus.Error)
            } else {
                node.copy(latencyMs = delay, latencyStatus = LatencyStatus.Available)
            }
        }
        val started = System.nanoTime()
        return try {
            Socket().use { socket -> socket.connect(InetSocketAddress(node.server, node.port), 4_000) }
            node.copy(
                latencyMs = ((System.nanoTime() - started) / 1_000_000).toInt().coerceAtLeast(1),
                latencyStatus = LatencyStatus.Available,
            )
        } catch (_: Exception) {
            node.copy(latencyMs = null, latencyStatus = LatencyStatus.Error)
        }
    }

    /** 借内核的延迟测试：它走的是节点自己的完整链路（含 detour 中转链）。 */
    private fun requestCoreDelay(outboundTag: String): Int? {
        val port = clashApiPort
        if (port <= 0) return null
        val target = java.net.URLEncoder.encode(
            "http://www.gstatic.com/generate_204",
            Charsets.UTF_8.name(),
        )
        val tag = java.net.URLEncoder.encode(outboundTag, Charsets.UTF_8.name())
        val connection = URL(
            "http://127.0.0.1:$port/proxies/$tag/delay?url=$target&timeout=5000",
        ).openConnection(Proxy.NO_PROXY) as HttpURLConnection
        return try {
            connection.requestMethod = "GET"
            connection.setRequestProperty(
                "Authorization",
                "Bearer ${SingBoxConfigBuilder.CLASH_API_SECRET}",
            )
            connection.connectTimeout = 6_000
            connection.readTimeout = 6_000
            val code = connection.responseCode
            if (code !in 200..299) return null
            val body = connection.inputStream.bufferedReader().readText()
            JSONObject(body).optInt("delay", -1).takeIf { it >= 0 }
        } finally {
            connection.disconnect()
        }
    }

    private fun update(value: AppState, persist: Boolean = true) {
        mutableState.value = value
        if (persist) save(value)
    }

    private fun save(state: AppState) {
        val nodes = JSONArray()
        state.nodes.forEach { node ->
            nodes.put(
                JSONObject()
                    .put("id", node.id)
                    .put("name", node.name)
                    .put("sourceId", node.sourceId)
                    .put("sourceName", node.sourceName)
                    .put("rawJson", node.rawJson)
                    .put("group", node.group)
                    .put("latencyMs", node.latencyMs ?: JSONObject.NULL)
                    .put("latencyStatus", node.latencyStatus.name),
            )
        }
        val subscriptions = JSONArray()
        state.subscriptions.forEach { item ->
            subscriptions.put(
                JSONObject()
                    .put("id", item.id)
                    .put("name", item.name)
                    .put("url", item.url)
                    .put("updatedAt", item.updatedAt)
                    .put("nodeCount", item.nodeCount)
                    .put("group", item.group),
            )
        }
        preferences.edit()
            .putString("nodes", nodes.toString())
            .putString("subscriptions", subscriptions.toString())
            .putString("selectedNodeId", state.selectedNodeId)
            .putString("nodeGroups", JSONArray(state.nodeGroups).toString())
            .putString("mode", state.mode.name)
            .putString("fallback", state.fallbackTarget.name)
            .putBoolean("ruleGroupEnabled", state.ruleGroup.enabled)
            .putString("ruleGroupDomains", JSONArray(state.ruleGroup.domains).toString())
            .putString("commonRuleTarget", state.commonRuleTarget.name)
            .putBoolean("localProxyEnabled", state.localProxyEnabled)
            .putInt("localProxyPort", state.localProxyPort)
            .putString(
                "localDns",
                JSONArray().apply {
                    state.localDnsEntries.forEach { entry ->
                        put(
                            JSONObject()
                                .put("domain", entry.domain)
                                .put("address", entry.address),
                        )
                    }
                }.toString(),
            )
            .putString(
                "portableRules",
                JSONArray().apply {
                    state.portableRules.forEach { rule ->
                        put(
                            JSONObject()
                                .put("type", rule.type)
                                .put("value", rule.value)
                                .put("target", rule.target.name)
                                .put("enabled", rule.enabled)
                                .put("note", rule.note),
                        )
                    }
                }.toString(),
            )
            .apply()
    }

    private fun load(): AppState {
        val nodes = runCatching {
            val array = JSONArray(preferences.getString("nodes", "[]"))
            List(array.length()) { index ->
                val item = array.getJSONObject(index)
                ProxyNode(
                    id = item.getString("id"),
                    name = item.getString("name"),
                    sourceId = item.getString("sourceId"),
                    sourceName = item.getString("sourceName"),
                    rawJson = item.getString("rawJson"),
                    group = normalizeNodeGroup(item.optString("group")),
                    latencyMs = item.optInt("latencyMs").takeIf { !item.isNull("latencyMs") },
                    latencyStatus = runCatching { LatencyStatus.valueOf(item.optString("latencyStatus")) }
                        .getOrDefault(LatencyStatus.Idle),
                )
            }
        }.getOrDefault(emptyList())
        val subscriptions = runCatching {
            val array = JSONArray(preferences.getString("subscriptions", "[]"))
            List(array.length()) { index ->
                val item = array.getJSONObject(index)
                Subscription(
                    id = item.getString("id"),
                    name = item.getString("name"),
                    url = item.getString("url"),
                    updatedAt = item.optString("updatedAt"),
                    nodeCount = item.optInt("nodeCount"),
                    group = normalizeNodeGroup(item.optString("group")),
                )
            }
        }.getOrDefault(emptyList())
        val selected = preferences.getString("selectedNodeId", "").orEmpty()
            .takeIf { id -> nodes.any { it.id == id } } ?: nodes.firstOrNull()?.id.orEmpty()
        val nodeGroups = runCatching {
            val array = JSONArray(preferences.getString("nodeGroups", "[]"))
            List(array.length()) { index -> normalizeNodeGroup(array.optString(index)) }
                .filter(String::isNotBlank)
                .distinct()
        }.getOrDefault(emptyList())
        val portableRules = runCatching {
            val array = JSONArray(preferences.getString("portableRules", "[]"))
            List(array.length()) { index ->
                val item = array.getJSONObject(index)
                PortableRule(
                    type = item.getString("type"),
                    value = item.getString("value"),
                    target = runCatching { FallbackTarget.valueOf(item.getString("target")) }
                        .getOrDefault(FallbackTarget.Direct),
                    enabled = item.optBoolean("enabled", true),
                    note = item.optString("note"),
                )
            }
        }.getOrDefault(emptyList())
        val ruleGroupDomains = runCatching {
            val array = JSONArray(preferences.getString("ruleGroupDomains", null))
            ruleGroupDomainsFromValues(List(array.length()) { index -> array.optString(index) })
        }.getOrDefault(DEFAULT_PROXY_DOMAINS)
        // 存的是外面来的文本，读回来时再挑一遍：手改过配置文件也不该让内核拿到脏数据。
        val localDnsEntries = runCatching {
            val array = JSONArray(preferences.getString("localDns", "[]"))
            sanitizeLocalDnsEntries(
                List(array.length()) { index ->
                    val item = array.getJSONObject(index)
                    LocalDnsEntry(domain = item.optString("domain"), address = item.optString("address"))
                },
            )
        }.getOrDefault(emptyList())
        return AppState(
            mode = enumPreference("mode", RoutingMode.Rule),
            fallbackTarget = enumPreference("fallback", FallbackTarget.Direct),
            selectedNodeId = selected,
            nodes = nodes,
            nodeGroups = (
                nodeGroups +
                    nodes.mapNotNull { it.group.takeIf(String::isNotBlank) } +
                    subscriptions.mapNotNull { it.group.takeIf(String::isNotBlank) }
            ).distinct(),
            subscriptions = subscriptions,
            ruleGroup = defaultOverseasRuleGroup().copy(
                domains = ruleGroupDomains,
                enabled = preferences.getBoolean("ruleGroupEnabled", true),
            ),
            commonRuleTarget = enumPreference("commonRuleTarget", FallbackTarget.Proxy),
            portableRules = portableRules,
            localProxyEnabled = preferences.getBoolean("localProxyEnabled", false),
            localProxyPort = preferences.getInt("localProxyPort", DEFAULT_LOCAL_PROXY_PORT)
                .takeIf { it in LOCAL_PROXY_PORT_MIN..LOCAL_PROXY_PORT_MAX }
                ?: DEFAULT_LOCAL_PROXY_PORT,
            localDnsEntries = localDnsEntries,
            tunPackages = profileStore.profiles()
                .firstOrNull { it.id == profileId }
                ?.packages
                .orEmpty(),
        )
    }

    private inline fun <reified T : Enum<T>> enumPreference(key: String, fallback: T): T =
        runCatching { enumValueOf<T>(preferences.getString(key, fallback.name) ?: fallback.name) }
            .getOrDefault(fallback)

    companion object {
        @Volatile private var instance: AppRepository? = null

        fun get(context: Context): AppRepository = instance ?: synchronized(this) {
            instance ?: AppRepository(context.applicationContext).also { instance = it }
        }
    }
}

private fun RoutingMode.portableValue(): String = when (this) {
    RoutingMode.Rule -> "rule"
    RoutingMode.Global -> "global"
    RoutingMode.Smart -> "smart"
    RoutingMode.Direct -> "direct"
}

private fun FallbackTarget.portableValue(): String =
    if (this == FallbackTarget.Proxy) "proxy" else "direct"

private fun String.toFallbackTarget(): FallbackTarget =
    if (equals("proxy", ignoreCase = true)) FallbackTarget.Proxy else FallbackTarget.Direct

private object URLName {
    fun from(url: String): String = runCatching { java.net.URL(url).host }
        .getOrDefault("订阅")
        .ifBlank { "订阅" }
}
