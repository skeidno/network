package com.skeidno.networkmanager.data

import org.json.JSONObject

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

data class AppState(
    val running: Boolean = false,
    val busy: Boolean = false,
    val statusMessage: String = "已停止",
    val mode: RoutingMode = RoutingMode.Rule,
    val fallbackTarget: FallbackTarget = FallbackTarget.Direct,
    val selectedNodeId: String = "",
    val nodes: List<ProxyNode> = emptyList(),
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
