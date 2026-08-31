package com.skeidno.networkmanager.data

import org.json.JSONArray
import org.json.JSONObject
import org.yaml.snakeyaml.Yaml
import java.net.HttpURLConnection
import java.net.URI
import java.net.URL
import java.net.URLDecoder
import java.nio.charset.StandardCharsets
import java.util.Base64
import java.util.UUID

object SubscriptionParser {
    private val supportedSchemes = setOf("ss", "vmess", "vless", "trojan", "hysteria2", "hy2")
    private val userAgents = listOf(
        "ClashMetaForAndroid/2.11",
        "clash-verge/v2.4",
        "v2rayN/7.15",
        "Mozilla/5.0 (Linux; Android 11) NetworkManager/0.1",
    )

    fun fetch(url: String): String {
        var lastError = "订阅下载失败"
        for (userAgent in userAgents) {
            val connection = URL(url).openConnection() as HttpURLConnection
            try {
                connection.connectTimeout = 12_000
                connection.readTimeout = 20_000
                connection.instanceFollowRedirects = true
                connection.setRequestProperty("User-Agent", userAgent)
                connection.setRequestProperty("Accept", "text/yaml,text/plain,application/yaml,*/*")
                val status = connection.responseCode
                if (status in 200..299) {
                    return connection.inputStream.bufferedReader().use { it.readText() }
                }
                lastError = "订阅服务器返回 $status"
                if (status != HttpURLConnection.HTTP_FORBIDDEN) break
            } finally {
                connection.disconnect()
            }
        }
        error(lastError)
    }

    fun parse(content: String, sourceId: String, sourceName: String): List<ProxyNode> {
        val text = content.trim().removePrefix("\uFEFF")
        if (text.isBlank()) return emptyList()
        parseYaml(text, sourceId, sourceName).takeIf { it.isNotEmpty() }?.let { return it }
        val decoded = decodeSubscriptionBody(text)
        return decoded.lineSequence()
            .map(String::trim)
            .filter { line -> supportedSchemes.any { line.startsWith("$it://", ignoreCase = true) } }
            .mapNotNull { parseUri(it, sourceId, sourceName) }
            .toList()
    }

    private fun parseYaml(text: String, sourceId: String, sourceName: String): List<ProxyNode> {
        if (!text.contains("proxies:")) return emptyList()
        val root = runCatching { Yaml().load<Any>(text) }.getOrNull() as? Map<*, *> ?: return emptyList()
        val proxies = root["proxies"] as? List<*> ?: return emptyList()
        return proxies.mapNotNull { item ->
            val map = item as? Map<*, *> ?: return@mapNotNull null
            val raw = mapToJson(map)
            val name = raw.optString("name").trim()
            val server = raw.optString("server").trim()
            val port = raw.optInt("port")
            if (name.isEmpty() || server.isEmpty() || port !in 1..65535) return@mapNotNull null
            ProxyNode(
                id = UUID.randomUUID().toString(),
                name = name,
                sourceId = sourceId,
                sourceName = sourceName,
                rawJson = raw.toString(),
            )
        }
    }

    private fun mapToJson(map: Map<*, *>): JSONObject {
        val result = JSONObject()
        map.forEach { (key, value) -> result.put(key.toString(), toJsonValue(value)) }
        return result
    }

    private fun toJsonValue(value: Any?): Any? = when (value) {
        null -> JSONObject.NULL
        is Map<*, *> -> mapToJson(value)
        is List<*> -> JSONArray(value.map(::toJsonValue))
        else -> value
    }

    private fun decodeSubscriptionBody(text: String): String {
        if (supportedSchemes.any { text.contains("$it://", ignoreCase = true) }) return text
        return runCatching { decodeBase64(text.filterNot(Char::isWhitespace)) }.getOrDefault(text)
    }

    fun parseUri(uriText: String, sourceId: String, sourceName: String): ProxyNode? {
        val scheme = uriText.substringBefore(":").lowercase()
        return runCatching {
            val raw = when (scheme) {
                "ss" -> parseShadowsocks(uriText)
                "vmess" -> parseVmess(uriText)
                "vless", "trojan", "hysteria2", "hy2" -> parseStandardUri(uriText, scheme)
                else -> return null
            }
            val name = raw.optString("name").ifBlank { raw.optString("server", "未命名节点") }
            ProxyNode(
                id = UUID.randomUUID().toString(),
                name = name,
                sourceId = sourceId,
                sourceName = sourceName,
                rawJson = raw.toString(),
            )
        }.getOrNull()
    }

    private fun parseVmess(text: String): JSONObject {
        val payload = JSONObject(decodeBase64(text.substringAfter("vmess://")))
        val raw = JSONObject()
            .put("name", payload.optString("ps", "VMess"))
            .put("type", "vmess")
            .put("server", payload.getString("add"))
            .put("port", payload.optString("port").toInt())
            .put("uuid", payload.getString("id"))
            .put("alterId", payload.optString("aid", "0").toInt())
            .put("cipher", payload.optString("scy", "auto"))
            .put("network", payload.optString("net", "tcp"))
        if (payload.optString("tls").equals("tls", true)) raw.put("tls", true)
        payload.optString("sni").takeIf(String::isNotBlank)?.let { raw.put("servername", it) }
        if (payload.optString("net") == "ws") {
            raw.put("ws-opts", JSONObject().put("path", payload.optString("path", "/")))
        }
        return raw
    }

    private fun parseStandardUri(text: String, scheme: String): JSONObject {
        val uri = URI(text)
        val query = parseQuery(uri.rawQuery.orEmpty())
        val type = if (scheme == "hy2") "hysteria2" else scheme
        val raw = JSONObject()
            .put("name", decode(uri.rawFragment ?: type.uppercase()))
            .put("type", type)
            .put("server", uri.host ?: error("节点缺少服务器"))
            .put("port", uri.port.takeIf { it > 0 } ?: error("节点缺少端口"))
        val user = decode(uri.rawUserInfo.orEmpty())
        when (type) {
            "vless" -> raw.put("uuid", user)
            "trojan", "hysteria2" -> raw.put("password", user)
        }
        query["sni"]?.let { raw.put("sni", it) }
        query["peer"]?.let { raw.put("sni", it) }
        query["insecure"]?.let { raw.put("skip-cert-verify", it == "1" || it.equals("true", true)) }
        query["allowInsecure"]?.let { raw.put("skip-cert-verify", it == "1" || it.equals("true", true)) }
        query["type"]?.let { raw.put("network", it) }
        query["security"]?.let { raw.put("tls", it != "none") }
        query["flow"]?.let { raw.put("flow", it) }
        query["obfs"]?.let { raw.put("obfs", it) }
        query["obfs-password"]?.let { raw.put("obfs-password", it) }
        query["obfsParam"]?.let { raw.put("obfs-password", it) }
        if (query["type"] == "ws") {
            raw.put(
                "ws-opts",
                JSONObject()
                    .put("path", query["path"] ?: "/")
                    .put("headers", JSONObject().put("Host", query["host"] ?: "")),
            )
        }
        if (query["security"] == "reality") {
            raw.put(
                "reality-opts",
                JSONObject()
                    .put("public-key", query["pbk"] ?: "")
                    .put("short-id", query["sid"] ?: ""),
            )
        }
        return raw
    }

    private fun parseShadowsocks(text: String): JSONObject {
        val withoutScheme = text.substringAfter("ss://")
        val fragment = withoutScheme.substringAfter("#", "")
        val main = withoutScheme.substringBefore("#").substringBefore("?")
        val decodedMain = if (main.contains("@")) main else decodeBase64(main)
        val userPart = decodedMain.substringBeforeLast("@")
        val endpoint = decodedMain.substringAfterLast("@")
        val credentials = if (userPart.contains(":")) userPart else decodeBase64(userPart)
        val host = endpoint.substringBeforeLast(":").removeSurrounding("[", "]")
        val port = endpoint.substringAfterLast(":").toInt()
        return JSONObject()
            .put("name", decode(fragment).ifBlank { "Shadowsocks" })
            .put("type", "ss")
            .put("server", host)
            .put("port", port)
            .put("cipher", decode(credentials.substringBefore(":")))
            .put("password", decode(credentials.substringAfter(":")))
    }

    private fun parseQuery(query: String): Map<String, String> = query.split("&")
        .filter(String::isNotBlank)
        .associate { part ->
            val key = decode(part.substringBefore("="))
            key to decode(part.substringAfter("=", ""))
        }

    private fun decode(value: String): String = URLDecoder.decode(value, StandardCharsets.UTF_8.name())

    private fun decodeBase64(value: String): String {
        val normalized = value.trim().replace('-', '+').replace('_', '/')
        val padded = normalized + "=".repeat((4 - normalized.length % 4) % 4)
        return String(Base64.getDecoder().decode(padded), StandardCharsets.UTF_8)
    }
}
