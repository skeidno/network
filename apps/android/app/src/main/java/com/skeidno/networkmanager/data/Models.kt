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
