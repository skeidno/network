package com.skeidno.networkmanager.data

import org.json.JSONObject
import java.net.InetAddress
import java.net.URI

enum class RoutingMode(val label: String) {
    Rule("规则"),
    Global("全局"),
    Smart("智能"),
    Direct("直连"),
}

enum class FallbackTarget(val label: String) {
    Proxy("内置节点"),
    Direct("直连"),
}

enum class LatencyStatus {
    Idle,
    Testing,
    Available,
    Error,
}

enum class ExitCheckStatus {
    Idle,
    Checking,
    Done,
    Failed,
}

/** 本地代理端口默认值，和桌面端 7890 的习惯保持一致。 */
const val DEFAULT_LOCAL_PROXY_PORT = 7890
const val LOCAL_PROXY_PORT_MIN = 1024
const val LOCAL_PROXY_PORT_MAX = 65535

/// 一次出口检测的结果：IP 加能查到的归属地（查不到就留空，不编造）。
data class ExitInfo(
    val ip: String = "",
    val location: String = "",
) {
    val available: Boolean get() = ip.isNotBlank()
}

data class ProxyNode(
    val id: String,
    val name: String,
    val sourceId: String,
    val sourceName: String,
    val rawJson: String,
    val group: String = "",
    val latencyMs: Int? = null,
    val latencyStatus: LatencyStatus = LatencyStatus.Idle,
) {
    val raw: JSONObject get() = JSONObject(rawJson)
    val protocol: String get() = raw.optString("type", "unknown").uppercase()
    val server: String get() = raw.optString("server")
    val port: Int get() = raw.optInt("port")
}

data class Subscription(
    val id: String,
    val name: String,
    val url: String,
    val updatedAt: String,
    val nodeCount: Int,
    val group: String = "",
)

data class RuleGroup(
    val id: String,
    val name: String,
    val domains: List<String>,
    val enabled: Boolean = true,
)

data class PortableRule(
    val type: String,
    val value: String,
    val target: FallbackTarget,
    val enabled: Boolean = true,
    val note: String = "",
)

data class InstalledApp(
    val label: String,
    val packageName: String,
)

/**
 * 本地 DNS 映射：把自定义域名直接指到固定 IP。
 *
 * 用途是在局域网里用域名访问自己的服务（NAS、路由器、自建站点），不用每台
 * 设备各记一套 IP。域名对上之后，服务端证书里的名字才有机会和访问地址一致，
 * HTTPS 才可能通过校验（自签证书还要把 CA 装进设备信任库，见工具箱的证书检测）。
 *
 * 只做**精确域名**匹配：sing-box 的 hosts 服务器就是查表，没有通配。
 */
data class LocalDnsEntry(
    val domain: String,
    val address: String,
)

val PORTABLE_RULE_TYPES = setOf(
    "package_name",
    "domain",
    "domain_suffix",
    "domain_keyword",
    "ip_cidr",
)

fun portableRulesFromValues(
    type: String,
    values: List<String>,
    target: FallbackTarget,
    enabled: Boolean = true,
    note: String = "",
    limit: Int = 500,
): List<PortableRule> {
    val normalizedType = type.trim().lowercase()
    require(normalizedType in PORTABLE_RULE_TYPES) { "不支持的规则类型" }
    require(values.isNotEmpty() && values.size <= limit) { "每次必须填写 1 到 $limit 条匹配内容" }
    val seen = mutableSetOf<String>()
    return buildList {
        values.forEachIndexed { index, rawValue ->
            val value = normalizePortableRuleValue(normalizedType, rawValue)
            validatePortableRuleValue(normalizedType, value)?.let { message ->
                throw IllegalArgumentException("第 ${index + 1} 行：$message")
            }
            if (seen.add(value.lowercase())) {
                add(
                    PortableRule(
                        type = normalizedType,
                        value = value,
                        target = target,
                        enabled = enabled,
                        note = note.trim(),
                    ),
                )
            }
        }
    }
}

fun ruleGroupDomainsFromValues(values: List<String>, limit: Int = 500): List<String> =
    portableRulesFromValues(
        type = "domain_suffix",
        values = values,
        target = FallbackTarget.Proxy,
        limit = limit,
    ).map(PortableRule::value)

private fun normalizePortableRuleValue(type: String, rawValue: String): String {
    var value = rawValue.trim()
    if (type in setOf("domain", "domain_suffix", "domain_keyword")) {
        if ("://" in value) {
            value = runCatching { URI(value).host }.getOrNull() ?: value
        }
        value = value.substringBefore('/').trim().lowercase().trimEnd('.')
        if (type == "domain_suffix") value = value.removePrefix("*.").removePrefix(".")
    }
    return value
}

private fun validatePortableRuleValue(type: String, value: String): String? {
    if (value.isBlank()) return "匹配内容不能为空"
    if (value.any { it == ',' || it == '\n' || it == '\r' }) return "匹配内容不能包含逗号或换行"
    if (type in setOf("domain", "domain_suffix")) {
        if (value.any(Char::isWhitespace) || '.' !in value) return "请输入有效域名，例如 example.com"
    }
    if (type == "package_name" && !value.matches(Regex("[A-Za-z0-9_]+(?:\\.[A-Za-z0-9_]+)+"))) {
        return "请输入有效应用包名，例如 com.example.app"
    }
    if (type == "ip_cidr" && !isValidCidr(value)) return "请输入有效 IP 或 CIDR"
    return null
}

private fun isValidCidr(value: String): Boolean {
    val parts = value.split('/', limit = 2)
    val address = parts[0]
    if (address.isBlank() || address.any { !it.isDigit() && it.lowercaseChar() !in 'a'..'f' && it !in ".:" }) {
        return false
    }
    val parsed = runCatching { InetAddress.getByName(address) }.getOrNull() ?: return false
    if (parts.size == 1) return true
    val prefix = parts[1].toIntOrNull() ?: return false
    return prefix in 0..if (parsed.address.size == 4) 32 else 128
}

// ---------------------------------------------------------------------------
// 本地 DNS 映射
// ---------------------------------------------------------------------------

/** 映射条数上限：够盖住一个局域网，也不至于把 DNS 规则撑到没法看。 */
const val LOCAL_DNS_MAX_ENTRIES = 200

private val ipv4LiteralPattern = Regex("""^\d{1,3}(?:\.\d{1,3}){3}$""")
private val ipv6LiteralPattern = Regex("""^[0-9A-Fa-f:.]+$""")
private val whitespacePattern = Regex("""[\s,]+""")

/**
 * 只解析**字面量** IP。
 *
 * 绝不用 [InetAddress.getByName] 去碰主机名——它收到 `example.com` 会真的发起
 * DNS 查询，让一个纯字符串判断变成一次网络往返。所以先用字符集把候选卡死。
 */
fun parseIpLiteral(text: String): ByteArray? {
    val value = text.trim().removeSurrounding("[", "]").substringBefore('%')
    if (value.isEmpty()) return null
    val looksLikeIp = if (':' in value) {
        ipv6LiteralPattern.matches(value)
    } else {
        ipv4LiteralPattern.matches(value)
    }
    if (!looksLikeIp) return null
    if (':' !in value && value.split('.').any { (it.toIntOrNull() ?: 256) > 255 }) return null
    return runCatching { InetAddress.getByName(value).address }.getOrNull()
}

fun isIpLiteral(text: String): Boolean = parseIpLiteral(text) != null

/** 域名归一：能贴 `https://nas.lan/admin` 也能贴 `nas.lan`，末尾的点去掉，统一小写。 */
fun normalizeLocalDnsDomain(raw: String): String {
    var value = raw.trim()
    if ("://" in value) {
        value = runCatching { URI(value).host }.getOrNull() ?: value
    }
    return value.substringBefore('/').trim().lowercase().trimEnd('.')
}

/** 返回 null 表示合法，否则是直接给用户看的失败原因。 */
fun localDnsDomainError(rawDomain: String): String? {
    val domain = normalizeLocalDnsDomain(rawDomain)
    if (domain.isEmpty()) return "域名不能为空"
    if (domain.startsWith("*.") || '*' in domain) {
        return "只支持精确域名，不支持通配（sing-box 的 hosts 是查表匹配，通配不会生效）"
    }
    if (domain.any(Char::isWhitespace)) return "域名里不能有空格"
    if (':' in domain) return "这里只填域名，不要带端口——端口由访问方决定"
    if ('.' !in domain) return "请输入完整域名，例如 nas.lan"
    if (!domain.matches(domainPattern)) return "域名里有非法字符"
    return null
}

/** 返回 null 表示合法，否则是直接给用户看的失败原因。 */
fun localDnsAddressError(rawAddress: String): String? {
    val address = rawAddress.trim()
    if (address.isEmpty()) return "IP 不能为空"
    if (!isIpLiteral(address)) return "「$address」不是合法的 IP（只支持 IPv4 / IPv6 字面量）"
    return null
}

private val domainPattern = Regex("""^[A-Za-z0-9](?:[A-Za-z0-9_-]{0,61}[A-Za-z0-9])?(?:\.[A-Za-z0-9](?:[A-Za-z0-9_-]{0,61}[A-Za-z0-9])?)+$""")

/**
 * 解析「域名 IP」多行文本。做成纯函数，界面与单测共用同一套规则。
 *
 * - 空行、`#` 注释行忽略；行尾 `#` 之后也算注释（所以能把 hosts 文件直接粘进来）
 * - 一行的两个字段顺序随意：`nas.lan 192.168.1.10` 和 `192.168.1.10 nas.lan` 等价，
 *   但两个都不像 IP 时会按「第几个是域名」的自然顺序理解
 * - 同一域名重复出现时后写的覆盖先写的
 * - 任何一行有问题都直接抛 [IllegalArgumentException]，并带行号，避免存半截配置
 */
fun localDnsEntriesFromText(
    text: String,
    limit: Int = LOCAL_DNS_MAX_ENTRIES,
): List<LocalDnsEntry> {
    val entries = LinkedHashMap<String, LocalDnsEntry>()
    text.lineSequence().forEachIndexed { index, rawLine ->
        val line = rawLine.substringBefore('#').trim()
        if (line.isEmpty()) return@forEachIndexed
        val parts = line.split(whitespacePattern).filter(String::isNotBlank)
        if (parts.size < 2) {
            throw IllegalArgumentException(
                "第 ${index + 1} 行：一行要写「域名 IP」两段内容，例如 nas.lan 192.168.1.10",
            )
        }
        // 字段顺序随意：hosts 文件是「IP 域名」，命令习惯是「域名 IP」，两种都收。
        val (domainPart, addressPart) = when {
            !isIpLiteral(parts[0]) && isIpLiteral(parts[1]) -> parts[0] to parts[1]
            isIpLiteral(parts[0]) && !isIpLiteral(parts[1]) -> parts[1] to parts[0]
            isIpLiteral(parts[0]) && isIpLiteral(parts[1]) ->
                throw IllegalArgumentException("第 ${index + 1} 行：两段都是 IP，没有域名")
            else -> parts[0] to parts[1]
        }
        localDnsDomainError(domainPart)?.let { throw IllegalArgumentException("第 ${index + 1} 行：$it") }
        localDnsAddressError(addressPart)?.let { throw IllegalArgumentException("第 ${index + 1} 行：$it") }
        val domain = normalizeLocalDnsDomain(domainPart)
        entries.remove(domain)
        entries[domain] = LocalDnsEntry(domain = domain, address = addressPart.trim())
    }
    if (entries.size > limit) {
        throw IllegalArgumentException("最多 $limit 条映射，当前 ${entries.size} 条")
    }
    return entries.values.toList()
}

/** 解析结果回写成编辑框文本，保证「打开编辑器不改动」时是等价的一行一条。 */
fun localDnsEntriesToText(entries: List<LocalDnsEntry>): String =
    entries.joinToString("\n") { "${it.domain} ${it.address}" }

/** 只保留仍然合法的那几条：配置是外部文本存下来的，读回来时不该信任。 */
fun sanitizeLocalDnsEntries(
    entries: List<LocalDnsEntry>,
    limit: Int = LOCAL_DNS_MAX_ENTRIES,
): List<LocalDnsEntry> {
    val result = LinkedHashMap<String, LocalDnsEntry>()
    entries.forEach { entry ->
        val domain = normalizeLocalDnsDomain(entry.domain)
        val address = entry.address.trim()
        if (localDnsDomainError(domain) != null || localDnsAddressError(address) != null) return@forEach
        result.remove(domain)
        result[domain] = LocalDnsEntry(domain, address)
    }
    return result.values.take(limit).toList()
}

data class AppState(
    val running: Boolean = false,
    val busy: Boolean = false,
    val statusMessage: String = "已停止",
    val mode: RoutingMode = RoutingMode.Rule,
    val fallbackTarget: FallbackTarget = FallbackTarget.Direct,
    val selectedNodeId: String = "",
    val nodes: List<ProxyNode> = emptyList(),
    val nodeGroups: List<String> = emptyList(),
    val subscriptions: List<Subscription> = emptyList(),
    val ruleGroup: RuleGroup = defaultOverseasRuleGroup(),
    val commonRuleTarget: FallbackTarget = FallbackTarget.Proxy,
    val portableRules: List<PortableRule> = emptyList(),
    val downloadBytesPerSecond: Long = 0,
    val uploadBytesPerSecond: Long = 0,
    val totalDownloadBytes: Long = 0,
    val totalUploadBytes: Long = 0,
    val downloadSamples: List<Long> = List(30) { 0L },
    val uploadSamples: List<Long> = List(30) { 0L },
    // 出口检测是运行态，不落盘：进程重启后回到未检测。
    val exitStatus: ExitCheckStatus = ExitCheckStatus.Idle,
    val directExit: ExitInfo = ExitInfo(),
    val proxyExit: ExitInfo = ExitInfo(),
    val exitMessage: String = "",
    // 本地代理：一个 mixed 入站同时接受 HTTP 与 SOCKS5，给浏览器 / Telegram 这类
    // 手动填代理的应用用。[localProxyPort] 是用户填的偏好端口，[localProxyActualPort]
    // 是内核实际监听的端口（偏好端口被占用时自动顺延），属于运行态、不落盘。
    val localProxyEnabled: Boolean = false,
    val localProxyPort: Int = DEFAULT_LOCAL_PROXY_PORT,
    val localProxyActualPort: Int = 0,
    // 本地 DNS 映射（自定义域名 → IP）：写进内核的 hosts DNS 服务器，
    // 让 TUN 接管的所有应用都能按域名找到局域网里的服务。
    val localDnsEntries: List<LocalDnsEntry> = emptyList(),
    val error: String = "",
)

val DEFAULT_PROXY_DOMAINS = listOf(
    "discord.com",
    "discordapp.com",
    "discord.gg",
    "discordapp.net",
    "discordcdn.com",
    "discord.media",
    "google.com",
    "googleapis.com",
    "gstatic.com",
    "googleusercontent.com",
    "arcteryx.com",
    "youtube.com",
    "youtu.be",
    "ytimg.com",
    "googlevideo.com",
    "openai.com",
    "chatgpt.com",
    "oaistatic.com",
    "oaiusercontent.com",
    "claude.ai",
    "anthropic.com",
    "x.ai",
    "poe.com",
    "midjourney.com",
    "stability.ai",
    "runwayml.com",
    "suno.com",
    "cursor.com",
    "cursor.sh",
    "deepseek.com",
    "github.com",
    "githubassets.com",
    "githubusercontent.com",
    "gitlab.com",
    "stackoverflow.com",
    "stackexchange.com",
    "docker.com",
    "docker.io",
    "npmjs.com",
    "registry.npmjs.org",
    "pypi.org",
    "pythonhosted.org",
    "files.pythonhosted.org",
    "crates.io",
    "rust-lang.org",
    "jetbrains.com",
    "vercel.com",
    "netlify.com",
    "cloudflare.com",
    "digitalocean.com",
    "vultr.com",
    "linode.com",
    "herokuapp.com",
    "render.com",
    "huggingface.co",
    "perplexity.ai",
    "x.com",
    "twitter.com",
    "twimg.com",
    "facebook.com",
    "fbcdn.net",
    "instagram.com",
    "cdninstagram.com",
    "threads.net",
    "reddit.com",
    "redd.it",
    "redditstatic.com",
    "telegram.org",
    "telegram.me",
    "cdn-telegram.org",
    "telesco.pe",
    "telegra.ph",
    "telegram.dog",
    "telega.one",
    "t.me",
    "whatsapp.com",
    "whatsapp.net",
    "line.me",
    "line-apps.com",
    "linkedin.com",
    "pinterest.com",
    "tumblr.com",
    "quora.com",
    "medium.com",
    "substack.com",
    "mastodon.social",
    "netflix.com",
    "nflximg.net",
    "nflxvideo.net",
    "disneyplus.com",
    "hulu.com",
    "max.com",
    "paramountplus.com",
    "primevideo.com",
    "spotify.com",
    "scdn.co",
    "soundcloud.com",
    "twitch.tv",
    "twitchcdn.net",
    "vimeo.com",
    "dailymotion.com",
    "binance.com",
    "binance.me",
    "binance.us",
    "okx.com",
    "okex.com",
    "coinbase.com",
    "bybit.com",
    "kucoin.com",
    "gate.io",
    "bitget.com",
    "kraken.com",
    "bitfinex.com",
    "crypto.com",
    "coingecko.com",
    "coinmarketcap.com",
    "tradingview.com",
    "dexscreener.com",
    "gmgn.ai",
    "metamask.io",
    "etherscan.io",
    "wikipedia.org",
    "wikimedia.org",
    "notion.so",
    "notion.site",
    "deepl.com",
    "grammarly.com",
    "canva.com",
    "figma.com",
    "miro.com",
    "dropbox.com",
    "bitwarden.com",
    "1password.com",
    "proton.me",
    "protonmail.com",
    "zoom.us",
    "outlook.com",
    "live.com",
    "office.com",
    "slack.com",
    "duckduckgo.com",
    "bloomberg.com",
    "reuters.com",
    "wsj.com",
    "ft.com",
    "economist.com",
    "nytimes.com",
    "bbc.com",
    "bbc.co.uk",
    "amazon.com",
    "ebay.com",
    "shopify.com",
    "bestbuy.com",
    "iherb.com",
    "steamcommunity.com",
    "steampowered.com",
    "epicgames.com",
    "roblox.com",
    "minecraft.net",
    "playstation.com",
    "nintendo.com",
    "blizzard.com",
    "battle.net",
    "riotgames.com",
    "archive.org",
)

fun defaultOverseasRuleGroup() = RuleGroup(
    id = "common-overseas",
    name = "常用海外站点",
    domains = DEFAULT_PROXY_DOMAINS,
)
