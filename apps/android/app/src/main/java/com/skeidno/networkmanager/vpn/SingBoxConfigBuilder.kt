package com.skeidno.networkmanager.vpn

import com.skeidno.networkmanager.data.AppState
import com.skeidno.networkmanager.data.FallbackTarget
import com.skeidno.networkmanager.data.ProxyNode
import com.skeidno.networkmanager.data.RoutingMode
import org.json.JSONArray
import org.json.JSONObject

object SingBoxConfigBuilder {
    private val lanDomainSuffixes = listOf("lan", "local", "home.arpa")
    private val lanCidrs = listOf(
        "0.0.0.0/8",
        "10.0.0.0/8",
        "100.64.0.0/10",
        "127.0.0.0/8",
        "169.254.0.0/16",
        "172.16.0.0/12",
        "192.168.0.0/16",
        "224.0.0.0/4",
        "240.0.0.0/4",
        "::/128",
        "::1/128",
        "fc00::/7",
        "fe80::/10",
        "ff00::/8",
    )

    fun build(state: AppState): String {
        val selected = state.nodes.firstOrNull { it.id == state.selectedNodeId }
            ?: state.nodes.firstOrNull()
        val orderedNodes = state.nodes.sortedByDescending { it.id == selected?.id }
        val outbounds = JSONArray()
        orderedNodes.forEach { outbounds.put(toOutbound(it)) }
        if (orderedNodes.isNotEmpty()) {
            outbounds.put(
                JSONObject()
                    .put("type", "selector")
                    .put("tag", "proxy")
                    .put("outbounds", JSONArray(orderedNodes.map { it.id }))
                    .put("default", selected?.id ?: orderedNodes.first().id),
            )
            outbounds.put(
                JSONObject()
                    .put("type", "urltest")
                    .put("tag", "smart")
                    .put("outbounds", JSONArray(orderedNodes.map { it.id }))
                    .put("url", "https://www.gstatic.com/generate_204")
                    .put("interval", "1m")
                    .put("tolerance", 120)
                    .put("interrupt_exist_connections", false),
            )
        }
        outbounds.put(JSONObject().put("type", "direct").put("tag", "direct"))

        val rules = JSONArray()
            .put(JSONObject().put("action", "sniff"))
            .put(JSONObject().put("protocol", "dns").put("action", "hijack-dns"))
            .put(
                JSONObject()
                    .put("domain_suffix", JSONArray(lanDomainSuffixes))
                    .put("outbound", "direct"),
            )
            .put(
                JSONObject()
                    .put("ip_cidr", JSONArray(lanCidrs))
                    .put("outbound", "direct"),
            )
        if (state.ruleGroup.enabled) {
            val commonOutbound = if (
                state.commonRuleTarget == FallbackTarget.Proxy && orderedNodes.isNotEmpty()
            ) "proxy" else "direct"
            rules.put(
                JSONObject()
                    .put("domain_suffix", JSONArray(state.ruleGroup.domains))
                    .put("outbound", commonOutbound),
            )
        }
        state.portableRules.filter { it.enabled }.forEach { rule ->
            val field = when (rule.type) {
                "domain" -> "domain"
                "domain_suffix" -> "domain_suffix"
                "domain_keyword" -> "domain_keyword"
                "ip_cidr" -> "ip_cidr"
                else -> return@forEach
            }
            val target = if (
                rule.target == FallbackTarget.Proxy && orderedNodes.isNotEmpty()
            ) "proxy" else "direct"
            rules.put(
                JSONObject()
                    .put(field, JSONArray().put(rule.value))
                    .put("outbound", target),
            )
        }
        val finalOutbound = when (state.mode) {
            RoutingMode.Global -> if (orderedNodes.isEmpty()) "direct" else "proxy"
            RoutingMode.Smart -> if (orderedNodes.isEmpty()) "direct" else "smart"
            RoutingMode.Direct -> "direct"
            RoutingMode.Rule -> if (
                state.fallbackTarget == FallbackTarget.Proxy && orderedNodes.isNotEmpty()
            ) "proxy" else "direct"
        }

        return JSONObject()
            .put("log", JSONObject().put("level", "info").put("timestamp", true))
            .put(
                "dns",
                JSONObject()
                    .put(
                        "servers",
                        JSONArray().put(JSONObject().put("type", "local").put("tag", "local")),
                    )
                    .put("final", "local")
                    .put("strategy", "prefer_ipv4"),
            )
            .put(
                "inbounds",
                JSONArray().put(
                    JSONObject()
                        .put("type", "tun")
                        .put("tag", "tun-in")
                        .put("address", JSONArray().put("172.19.0.1/30"))
                        .put("mtu", 9000)
                        .put("auto_route", true)
                        .put("route_exclude_address", JSONArray(lanCidrs))
                        .put("strict_route", false)
                        .put("stack", "mixed"),
                ),
            )
            .put("outbounds", outbounds)
            .put(
                "route",
                JSONObject()
                    .put("rules", rules)
                    .put("final", finalOutbound)
                    .put("auto_detect_interface", true),
            )
            .toString(2)
    }

    internal fun toOutbound(node: ProxyNode): JSONObject {
        val raw = node.raw
        val type = raw.optString("type").lowercase()
        val outbound = JSONObject()
            .put("tag", node.id)
            .put("type", if (type == "ss") "shadowsocks" else type)
            .put("server", raw.getString("server"))
            .put("server_port", raw.getInt("port"))
        when (type) {
            "ss" -> outbound
                .put("method", raw.getString("cipher"))
                .put("password", raw.getString("password"))
            "vmess" -> outbound
                .put("uuid", raw.getString("uuid"))
                .put("security", raw.optString("cipher", "auto"))
                .put("alter_id", raw.optInt("alterId", 0))
            "vless" -> outbound
                .put("uuid", raw.getString("uuid"))
                .apply { raw.optString("flow").takeIf(String::isNotBlank)?.let { put("flow", it) } }
            "trojan", "hysteria2" -> outbound.put("password", raw.getString("password"))
            "socks", "http" -> {
                raw.optString("username").takeIf(String::isNotBlank)?.let { outbound.put("username", it) }
                raw.optString("password").takeIf(String::isNotBlank)?.let { outbound.put("password", it) }
            }
            else -> error("Android 暂不支持节点协议：${node.protocol}")
        }
        addTls(outbound, raw, type)
        addTransport(outbound, raw)
        if (type == "hysteria2") {
            raw.optString("obfs").takeIf(String::isNotBlank)?.let { obfsType ->
                outbound.put(
                    "obfs",
                    JSONObject()
                        .put("type", obfsType)
                        .put("password", raw.optString("obfs-password")),
                )
            }
        }
        return outbound
    }

    private fun addTls(outbound: JSONObject, raw: JSONObject, type: String) {
        val enabled = type in setOf("trojan", "hysteria2") || raw.optBoolean("tls") || raw.has("reality-opts")
        if (!enabled) return
        val tls = JSONObject()
            .put("enabled", true)
            .put("server_name", raw.optString("sni", raw.optString("servername", raw.optString("server"))))
            .put("insecure", raw.optBoolean("skip-cert-verify"))
        raw.optJSONObject("reality-opts")?.let { reality ->
            tls.put(
                "reality",
                JSONObject()
                    .put("enabled", true)
                    .put("public_key", reality.optString("public-key"))
                    .put("short_id", reality.optString("short-id")),
            )
        }
        outbound.put("tls", tls)
    }

    private fun addTransport(outbound: JSONObject, raw: JSONObject) {
        when (raw.optString("network")) {
            "ws" -> {
                val options = raw.optJSONObject("ws-opts") ?: JSONObject()
                val transport = JSONObject()
                    .put("type", "ws")
                    .put("path", options.optString("path", "/"))
                options.optJSONObject("headers")?.let { transport.put("headers", it) }
                outbound.put("transport", transport)
            }
            "grpc" -> outbound.put(
                "transport",
                JSONObject()
                    .put("type", "grpc")
                    .put("service_name", raw.optString("grpc-service-name")),
            )
        }
    }
}
