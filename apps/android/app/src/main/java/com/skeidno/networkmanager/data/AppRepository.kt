package com.skeidno.networkmanager.data

import android.content.Context
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
import java.net.InetSocketAddress
import java.net.Socket
import java.time.OffsetDateTime
import java.util.UUID

class AppRepository private constructor(private val context: Context) {
    private val preferences = context.getSharedPreferences("network-manager", Context.MODE_PRIVATE)
    private val mutableState = MutableStateFlow(load())
    val state: StateFlow<AppState> = mutableState.asStateFlow()

    fun setMode(mode: RoutingMode) = update(mutableState.value.copy(mode = mode))

    fun setFallback(target: FallbackTarget) = update(mutableState.value.copy(fallbackTarget = target))

    fun setRuleGroupEnabled(enabled: Boolean) {
        val current = mutableState.value
        update(current.copy(ruleGroup = current.ruleGroup.copy(enabled = enabled)))
    }

    fun setCommonRuleTarget(target: FallbackTarget) =
        update(mutableState.value.copy(commonRuleTarget = target))

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

    fun importText(content: String, sourceName: String = "粘贴导入"): Int {
        val sourceId = "manual-${UUID.randomUUID()}"
        val imported = SubscriptionParser.parse(content, sourceId, sourceName)
        if (imported.isEmpty()) error("没有识别到可用节点")
        mergeNodes(imported)
        return imported.size
    }

    suspend fun addSubscription(name: String, url: String): Int = withContext(Dispatchers.IO) {
        val current = mutableState.value
        val existing = current.subscriptions.firstOrNull { it.url == url }
        val sourceId = existing?.id ?: UUID.randomUUID().toString()
        val content = SubscriptionParser.fetch(url)
        val displayName = name.trim().ifBlank { URLName.from(url) }
        val imported = SubscriptionParser.parse(content, sourceId, displayName)
        if (imported.isEmpty()) error("订阅中没有识别到可用节点")
        val retained = current.nodes.filterNot { it.sourceId == sourceId }
        val nodes = uniqueNames(retained + imported)
        val subscription = Subscription(
            id = sourceId,
            name = displayName,
            url = url,
            updatedAt = OffsetDateTime.now().toString(),
            nodeCount = imported.size,
        )
        val subscriptions = current.subscriptions.filterNot { it.id == sourceId } + subscription
        val selected = current.selectedNodeId.takeIf { id -> nodes.any { it.id == id } } ?: nodes.first().id
        update(current.copy(nodes = nodes, subscriptions = subscriptions, selectedNodeId = selected))
        imported.size
    }

    suspend fun refreshSubscription(id: String): Int {
        val subscription = mutableState.value.subscriptions.firstOrNull { it.id == id }
            ?: error("订阅不存在")
        return addSubscription(subscription.name, subscription.url)
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

    suspend fun testNode(id: String): ProxyNode {
        val current = mutableState.value
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

    fun runtimeConfigFile(): File = File(context.filesDir, "runtime.json")

    fun writeRuntimeConfig(): File {
        val file = runtimeConfigFile()
        val temporary = File(file.parentFile, "runtime.tmp")
        temporary.writeText(SingBoxConfigBuilder.build(mutableState.value), Charsets.UTF_8)
        if (!temporary.renameTo(file)) {
            temporary.copyTo(file, overwrite = true)
            temporary.delete()
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
                    .put("updatedAt", item.updatedAt),
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
                            .put("target", state.commonRuleTarget.portableValue()),
                    ),
            )
            .put("selectedNodeId", state.selectedNodeId)
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
            ProxyNode(
                id = item.optString("id").ifBlank { UUID.randomUUID().toString() },
                name = name,
                sourceId = item.optString("sourceId"),
                sourceName = item.optString("sourceName").ifBlank { "跨设备导入" },
                rawJson = config.toString(),
            )
        }.let(::uniqueNames)
        val subscriptionsArray = root.optJSONArray("subscriptions") ?: JSONArray()
        require(subscriptionsArray.length() <= 500) { "订阅数量超过限制" }
        val subscriptions = List(subscriptionsArray.length()) { index ->
            val item = subscriptionsArray.getJSONObject(index)
            Subscription(
                id = item.optString("id").ifBlank { UUID.randomUUID().toString() },
                name = item.optString("name").ifBlank { "订阅" },
                url = item.optString("url"),
                updatedAt = item.optString("updatedAt"),
                nodeCount = nodes.count { node -> node.sourceId == item.optString("id") },
            )
        }.filter { it.url.isNotBlank() }
        val rulesArray = root.optJSONArray("rules") ?: JSONArray()
        require(rulesArray.length() <= 5_000) { "规则数量超过限制" }
        val supportedTypes = setOf("domain", "domain_suffix", "domain_keyword", "ip_cidr")
        val rules = List(rulesArray.length()) { index ->
            val item = rulesArray.getJSONObject(index)
            val type = item.optString("type").lowercase()
            val value = item.optString("value").trim()
            require(type in supportedTypes && value.isNotBlank()) { "第 ${index + 1} 条规则无效" }
            PortableRule(
                type = type,
                value = value,
                target = item.optString("target").toFallbackTarget(),
                enabled = item.optBoolean("enabled", true),
                note = item.optString("note"),
            )
        }
        val routing = root.optJSONObject("routing") ?: JSONObject()
        val common = routing.optJSONObject("commonOverseas") ?: JSONObject()
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
                subscriptions = subscriptions,
                ruleGroup = defaultOverseasRuleGroup().copy(enabled = common.optBoolean("enabled", true)),
                commonRuleTarget = common.optString("target", "proxy").toFallbackTarget(),
                portableRules = rules,
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

    private fun testEndpoint(node: ProxyNode): ProxyNode {
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
                    .put("nodeCount", item.nodeCount),
            )
        }
        preferences.edit()
            .putString("nodes", nodes.toString())
            .putString("subscriptions", subscriptions.toString())
            .putString("selectedNodeId", state.selectedNodeId)
            .putString("mode", state.mode.name)
            .putString("fallback", state.fallbackTarget.name)
            .putBoolean("ruleGroupEnabled", state.ruleGroup.enabled)
            .putString("commonRuleTarget", state.commonRuleTarget.name)
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
                )
            }
        }.getOrDefault(emptyList())
        val selected = preferences.getString("selectedNodeId", "").orEmpty()
            .takeIf { id -> nodes.any { it.id == id } } ?: nodes.firstOrNull()?.id.orEmpty()
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
        return AppState(
            mode = enumPreference("mode", RoutingMode.Rule),
            fallbackTarget = enumPreference("fallback", FallbackTarget.Direct),
            selectedNodeId = selected,
            nodes = nodes,
            subscriptions = subscriptions,
            ruleGroup = defaultOverseasRuleGroup().copy(
                enabled = preferences.getBoolean("ruleGroupEnabled", true),
            ),
            commonRuleTarget = enumPreference("commonRuleTarget", FallbackTarget.Proxy),
            portableRules = portableRules,
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
