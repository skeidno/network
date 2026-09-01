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
    val error: String = "",
)

val DEFAULT_PROXY_DOMAINS = listOf(
    "discord.com",
    "discordapp.com",
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
    "github.com",
    "githubassets.com",
    "githubusercontent.com",
    "gitlab.com",
    "stackoverflow.com",
    "stackexchange.com",
    "docker.com",
    "docker.io",
    "npmjs.com",
    "pypi.org",
    "pythonhosted.org",
    "huggingface.co",
    "perplexity.ai",
    "poe.com",
    "midjourney.com",
    "x.ai",
    "x.com",
    "twitter.com",
    "twimg.com",
    "facebook.com",
    "fbcdn.net",
    "instagram.com",
    "cdninstagram.com",
    "reddit.com",
    "redd.it",
    "redditstatic.com",
    "telegram.org",
    "telegram.me",
    "t.me",
    "whatsapp.com",
    "whatsapp.net",
    "linkedin.com",
    "netflix.com",
    "nflximg.net",
    "nflxvideo.net",
    "spotify.com",
    "scdn.co",
    "twitch.tv",
    "twitchcdn.net",
    "vimeo.com",
    "wikipedia.org",
    "wikimedia.org",
    "medium.com",
    "notion.so",
    "slack.com",
    "dropbox.com",
    "duckduckgo.com",
    "quora.com",
    "steamcommunity.com",
    "steampowered.com",
)

fun defaultOverseasRuleGroup() = RuleGroup(
    id = "common-overseas",
    name = "常用海外站点",
    domains = DEFAULT_PROXY_DOMAINS,
)
