package com.skeidno.networkmanager.data

import com.skeidno.networkmanager.vpn.SingBoxConfigBuilder
import org.json.JSONObject
import java.net.HttpURLConnection
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.Proxy
import java.net.Socket
import java.net.URL
import java.net.URLDecoder
import java.net.URLEncoder
import java.nio.charset.StandardCharsets
import java.util.Base64
import java.util.UUID

/**
 * 「工具箱」页面的能力实现。
 *
 * 这里只放**可以直接单测**的逻辑：编解码、TCP 探活、DNS 查询、路由命中预判。
 * 不碰 VPN 内核、不改配置——纯诊断，出问题也影响不到代理本身。
 *
 * 路由预判刻意和 [SingBoxConfigBuilder.build] 的规则顺序一一对应（含局域网、
 * 常用站点、自定义规则、各模式兜底），这样界面上给出的结论才和内核实际行为一致。
 * 两者若哪天出现分歧，以内核为准，这里要跟着改。
 */
object NetworkTools {
    private const val userAgent = "NetworkManager-Android"

    private val ipv4Pattern = Regex("""^\d{1,3}(?:\.\d{1,3}){3}$""")

    /** dns.google 的 JSON 接口用数字表示记录类型，这里做双向映射。 */
    private val dnsTypeCodes = mapOf(
        "A" to 1,
        "NS" to 2,
        "CNAME" to 5,
        "SOA" to 6,
        "PTR" to 12,
        "MX" to 15,
        "TXT" to 16,
        "AAAA" to 28,
        "SRV" to 33,
    )
    private val dnsTypeNames = dnsTypeCodes.entries.associate { (name, code) -> code to name }

    val dnsRecordTypes: List<String> get() = listOf("A", "AAAA", "CNAME", "MX", "TXT", "NS", "PTR")

    private val noNodesNote = listOf("当前没有可用节点，配置里的兜底出站会退化成直连")

    // ------------------------------------------------------------------
    // 编解码
    // ------------------------------------------------------------------

    fun encodeBase64(input: String): String =
        Base64.getEncoder().encodeToString(input.toByteArray(StandardCharsets.UTF_8))

    /**
     * 宽容解码：补 padding、兼容 URL-safe 的 `-` `_`，并忽略换行与空格。
     * 解析失败返回 null 而不是抛异常——用户在输入框里边打边解，报错体验很差。
     */
    fun decodeBase64(input: String): String? = runCatching {
        val normalized = input.trim()
            .filterNot(Char::isWhitespace)
            .replace('-', '+')
            .replace('_', '/')
        if (normalized.isEmpty()) return null
        val padded = normalized + "=".repeat((4 - normalized.length % 4) % 4)
        String(Base64.getDecoder().decode(padded), StandardCharsets.UTF_8)
    }.getOrNull()

    fun encodeUrl(input: String): String = URLEncoder.encode(input, StandardCharsets.UTF_8.name())

    fun decodeUrl(input: String): String? =
        runCatching { URLDecoder.decode(input, StandardCharsets.UTF_8.name()) }.getOrNull()

    fun randomUuid(): String = UUID.randomUUID().toString()

    // ------------------------------------------------------------------
    // TCP 探活
    // ------------------------------------------------------------------

    data class TcpProbeResult(
        val host: String,
        val port: Int,
        val reachable: Boolean,
        val elapsedMs: Int,
        val detail: String,
    )

    /**
     * 直连 TCP 握手探测（不走代理），用来回答「这个 host:port 到底通不通」。
     * 域名会先做一次系统解析，解析失败也能直接区分出来。
     */
    fun probeTcp(host: String, port: Int, timeoutMs: Int = 4_000): TcpProbeResult {
        val target = host.trim().removeSurrounding("[", "]")
        val started = System.nanoTime()
        fun elapsed() = ((System.nanoTime() - started) / 1_000_000).toInt().coerceAtLeast(1)

        if (target.isBlank()) {
            return TcpProbeResult(target, port, false, 0, "请填写主机名或 IP")
        }
        if (port !in 1..65_535) {
            return TcpProbeResult(target, port, false, 0, "端口要在 1..65535 之间")
        }
        return try {
            Socket().use { socket -> socket.connect(InetSocketAddress(target, port), timeoutMs) }
            val ms = elapsed()
            TcpProbeResult(target, port, true, ms, "端口可连接，TCP 握手 $ms ms")
        } catch (error: Exception) {
            val ms = elapsed()
            val readable = when (error) {
                is java.net.UnknownHostException -> "域名解析失败"
                is java.net.SocketTimeoutException -> "连接超时（${timeoutMs} ms）"
                is java.net.ConnectException -> "连接被拒绝"
                else -> error.message ?: error.javaClass.simpleName
            }
            TcpProbeResult(target, port, false, ms, readable)
        }
    }

    // ------------------------------------------------------------------
    // DNS 查询
    // ------------------------------------------------------------------

    data class DnsRecord(val type: String, val value: String, val ttl: Int)

    data class DnsResult(
        val name: String,
        val type: String,
        val records: List<DnsRecord>,
        val elapsedMs: Int,
        val detail: String,
    )

    /**
     * 走 dns.google 的 DoH JSON 接口查询。
     *
     * [proxyPort] 非 0 时请求从本机 sing-box 的 mixed 入站出去，这样查到的就是
     * 节点侧的解析结果——和「代理出口」是同一个道理，能看出 DNS 有没有跟着代理走。
     */
    fun queryDns(name: String, type: String, proxyPort: Int? = null): DnsResult {
        val cleanName = name.trim().removeSuffix(".")
        require(cleanName.isNotBlank()) { "请输入要查询的域名" }
        val upperType = type.trim().uppercase()
        require(dnsTypeCodes.containsKey(upperType)) { "不支持的记录类型：$type" }

        val url = "https://dns.google/resolve?name=${encodeUrl(cleanName)}&type=$upperType"
        val started = System.nanoTime()
        val body = httpGet(url, proxyPort)
        val elapsed = ((System.nanoTime() - started) / 1_000_000).toInt().coerceAtLeast(1)

        val json = JSONObject(body)
        val status = json.optInt("Status", 0)
        val answers = json.optJSONArray("Answer")
        val records = buildList {
            for (index in 0 until (answers?.length() ?: 0)) {
                val item = answers?.optJSONObject(index) ?: continue
                val data = item.optString("data").trim()
                if (data.isEmpty()) continue
                add(
                    DnsRecord(
                        type = dnsTypeNames[item.optInt("type")] ?: "TYPE${item.optInt("type")}",
                        value = data,
                        ttl = item.optInt("TTL"),
                    ),
                )
            }
        }
        val detail = when {
            status == 0 && records.isEmpty() -> "查询成功，但没有返回记录"
            status == 0 -> "查询成功，${records.size} 条记录，耗时 $elapsed ms"
            status == 3 -> "域名不存在（NXDOMAIN）"
            status == 2 -> "DNS 服务器解析失败（SERVFAIL）"
            else -> "DNS 返回状态码 $status"
        }
        return DnsResult(cleanName, upperType, records, elapsed, detail)
    }

    private fun httpGet(url: String, proxyPort: Int?): String {
        val connection = (if (proxyPort != null && proxyPort > 0) {
            URL(url).openConnection(
                Proxy(Proxy.Type.HTTP, InetSocketAddress("127.0.0.1", proxyPort)),
            )
        } else {
            URL(url).openConnection(Proxy.NO_PROXY)
        }) as HttpURLConnection
        connection.connectTimeout = 8_000
        connection.readTimeout = 8_000
        connection.instanceFollowRedirects = true
        connection.setRequestProperty("User-Agent", userAgent)
        connection.setRequestProperty("Accept", "application/dns-json, application/json")
        return try {
            if (connection.responseCode !in 200..299) error("HTTP ${connection.responseCode}")
            connection.inputStream.bufferedReader().use { it.readText() }
        } finally {
            connection.disconnect()
        }
    }

    // ------------------------------------------------------------------
    // 分享链接解析
    // ------------------------------------------------------------------

    /**
     * 把一条分享链接拆成可读的字段。刻意复用订阅导入用的 [SubscriptionParser.parseUri]，
     * 保证「工具箱里看到的」和「真正导入进去的」是同一套解析结果，不会两边对不上。
     */
    fun describeShareLink(link: String): String {
        val text = link.trim()
        if (!text.contains("://")) {
            return "无法解析：这不像一条分享链接（需要带 ss:// / vmess:// 之类的前缀）"
        }
        val node = SubscriptionParser.parseUri(text, sourceId = "toolbox", sourceName = "工具箱")
            ?: return "无法解析：支持 ss / vmess / vless / trojan / hysteria2 的分享链接"
        val raw = node.raw
        val lines = mutableListOf(
            "名称：${node.name}",
            "协议：${node.protocol}",
            "服务器：${node.server}",
            "端口：${node.port}",
        )
        fun put(label: String, key: String) {
            raw.optString(key).takeIf { it.isNotBlank() }?.let { lines += "$label：$it" }
        }
        put("加密方式", "cipher")
        put("UUID", "uuid")
        put("密码", "password")
        put("用户名", "username")
        put("SNI", "sni")
        put("serverName", "servername")
        put("传输", "network")
        put("流控", "flow")
        put("混淆", "obfs")
        if (raw.optBoolean("tls")) lines += "TLS：开启"
        if (raw.has("reality-opts")) lines += "REALITY：开启"
        return lines.joinToString("\n")
    }

    // ------------------------------------------------------------------
    // 路由命中预判
    // ------------------------------------------------------------------

    /** 预判结论用的出站标识，与配置里的 outbound tag 对齐，方便对照。 */
    enum class Outbound(val label: String) {
        Proxy("代理节点"),
        Smart("智能选路"),
        Direct("直连"),
    }

    data class RoutePreview(
        val query: String,
        val outbound: Outbound,
        /** 命中了哪条规则（就是界面上的「为什么」）。 */
        val reason: String,
        /** 需要额外提醒的点，比如「lan 是字符串后缀匹配，比想象中激进」。 */
        val notes: List<String> = emptyList(),
    )

    /**
     * 按当前配置预判某个域名/IP 会走哪条出站。
     *
     * 顺序严格照抄 [SingBoxConfigBuilder.build] 里 rules 的排列：局域网 →
     * （节点服务器地址直连）→ 常用站点规则 → 自定义规则 → 模式兜底。
     * 唯一没覆盖的是 probe-in 入站那条强制规则，它只作用于出口检测流量，
     * 普通流量不会命中。
     */
    fun previewRoute(state: AppState, query: String): RoutePreview {
        val target = query.trim().removeSuffix(".").lowercase()
        val hasNodes = state.nodes.isNotEmpty()
        val isIp = ipv4Pattern.matches(target) || target.contains(':')

        if (target.isBlank()) {
            return RoutePreview(query, Outbound.Direct, "请输入域名或 IP")
        }

        // 1) 局域网：域名按 domain_suffix 字符串后缀匹配，IP 按 ip_cidr 匹配。
        if (isIp) {
            if (SingBoxConfigBuilder.lanCidrs.any { ipInCidr(target, it) }) {
                return RoutePreview(query, Outbound.Direct, "命中内置局域网规则：ip_cidr $target 属于内网段")
            }
        } else if (SingBoxConfigBuilder.lanDomainSuffixes.any { target.endsWith(it) }) {
            val suffix = SingBoxConfigBuilder.lanDomainSuffixes.first { target.endsWith(it) }
            return RoutePreview(
                query = query,
                outbound = Outbound.Direct,
                reason = "命中内置局域网规则：domain_suffix 里有一条 \"$suffix\"",
                notes = listOf("domain_suffix 是纯字符串后缀匹配，不含标签边界，\"$suffix\" 会连 \"xxx$suffix\" 一起命中"),
            )
        }

        // 2) 节点服务器地址本身要直连，否则就回环了。
        if (isIp && state.nodes.any { it.server.equals(target, ignoreCase = true) }) {
            return RoutePreview(query, Outbound.Direct, "目标是节点服务器地址，按配置强制直连")
        }

        // 3) 常用站点规则（ruleGroup）排在自定义规则前面，先命中先算。
        if (state.ruleGroup.enabled && !isIp) {
            val hit = state.ruleGroup.domains.firstOrNull { target.endsWith(it.trim().lowercase()) }
            if (hit != null) {
                // 组目标写的是「内置节点」但没有节点时，内核会把这条规则降级成 direct，
                // 这里必须把降级原因说清楚，不然「目标内置节点 → 直连」看起来像算错了。
                val downgraded = state.commonRuleTarget == FallbackTarget.Proxy && !hasNodes
                return RoutePreview(
                    query = query,
                    outbound = resolveTarget(state.commonRuleTarget, hasNodes),
                    reason = "命中「常用站点」规则：domain_suffix 包含 \"$hit\"（该组目标：${state.commonRuleTarget.label}）",
                    notes = if (downgraded) noNodesNote else emptyList(),
                )
            }
        }

        // 4) 自定义规则。package_name 只对「哪个应用」成立，域名/IP 查询命中不了。
        val matched = state.portableRules
            .filter { it.enabled && it.type != "package_name" }
            .firstOrNull { matchesPortableRule(it, target, isIp) }
        if (matched != null) {
            val downgraded = matched.target == FallbackTarget.Proxy && !hasNodes
            return RoutePreview(
                query = query,
                outbound = resolveTarget(matched.target, hasNodes),
                reason = "命中自定义规则：${matched.type} = \"${matched.value}\"（目标：${matched.target.label}）",
                notes = if (downgraded) noNodesNote else emptyList(),
            )
        }

        // 5) 都没命中，落到模式决定的兜底出站。没有节点时全部退化为直连。
        return when (state.mode) {
            RoutingMode.Global -> if (hasNodes) {
                RoutePreview(query, Outbound.Proxy, "全局模式：所有流量走节点")
            } else {
                RoutePreview(query, Outbound.Direct, "全局模式，但没有节点，只能直连", noNodesNote)
            }
            RoutingMode.Smart -> if (hasNodes) {
                RoutePreview(query, Outbound.Smart, "智能模式：由 urltest 选延迟最低的节点")
            } else {
                RoutePreview(query, Outbound.Direct, "智能模式，但没有节点，只能直连", noNodesNote)
            }
            RoutingMode.Direct -> RoutePreview(query, Outbound.Direct, "直连模式：所有流量都不走节点")
            RoutingMode.Rule -> {
                // 规则模式下没命中任何规则时，按 fallbackTarget 决定兜底。
                if (state.fallbackTarget == FallbackTarget.Proxy && hasNodes) {
                    RoutePreview(query, Outbound.Proxy, "规则模式：未命中任何规则，按兜底设置走节点")
                } else {
                    RoutePreview(query, Outbound.Direct, "规则模式：未命中任何规则，按兜底设置直连")
                }
            }
        }
    }

    private fun resolveTarget(target: FallbackTarget, hasNodes: Boolean): Outbound =
        if (target == FallbackTarget.Proxy && hasNodes) Outbound.Proxy else Outbound.Direct

    private fun matchesPortableRule(rule: PortableRule, target: String, isIp: Boolean): Boolean {
        val value = rule.value.trim().lowercase()
        if (value.isBlank()) return false
        return when (rule.type) {
            "domain" -> !isIp && target == value
            "domain_suffix" -> !isIp && target.endsWith(value)
            "domain_keyword" -> !isIp && target.contains(value)
            "ip_cidr" -> isIp && ipInCidr(target, value)
            else -> false
        }
    }

    /** IPv4/IPv6 的 CIDR 归属判断；地址族不一致直接判否。 */
    fun ipInCidr(ip: String, cidr: String): Boolean {
        val slash = cidr.indexOf('/')
        if (slash < 0) return ip.equals(cidr.trim(), ignoreCase = true)
        val network = cidr.substring(0, slash).trim()
        val prefix = cidr.substring(slash + 1).trim().toIntOrNull() ?: return false
        val address = parseLiteralIp(ip) ?: return false
        val base = parseLiteralIp(network) ?: return false
        if (address.size != base.size) return false
        if (prefix !in 0..address.size * 8) return false

        val fullBytes = prefix / 8
        val remainder = prefix % 8
        for (index in 0 until fullBytes) {
            if (address[index] != base[index]) return false
        }
        if (remainder > 0) {
            val mask = (0xFF shl (8 - remainder)) and 0xFF
            if ((address[fullBytes].toInt() and mask) != (base[fullBytes].toInt() and mask)) return false
        }
        return true
    }

    /** 只解析字面量 IP，绝不触发 DNS（getByName 对主机名会发查询）。 */
    private fun parseLiteralIp(text: String): ByteArray? {
        val value = text.trim().removeSurrounding("[", "]").substringBefore('%')
        if (value.isEmpty()) return null
        val looksLikeIp = ipv4Pattern.matches(value) || value.contains(':')
        if (!looksLikeIp) return null
        val isIpv4 = ipv4Pattern.matches(value)
        if (isIpv4 && value.split('.').any { (it.toIntOrNull() ?: 256) > 255 }) return null
        return runCatching { InetAddress.getByName(value).address }.getOrNull()
    }
}
