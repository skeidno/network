package com.skeidno.networkmanager.data

import java.nio.charset.StandardCharsets
import java.util.Base64
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class SubscriptionParserTest {
    @Test
    fun parsesClashYamlUsingStructuredYamlParser() {
        val nodes = SubscriptionParser.parse(
            """
            proxies:
              - name: Singapore
                type: hysteria2
                server: sg.example.com
                port: 8443
                password: secret
                sni: example.com
            """.trimIndent(),
            sourceId = "subscription",
            sourceName = "Test",
        )

        assertEquals(1, nodes.size)
        assertEquals("HYSTERIA2", nodes.single().protocol)
        assertEquals("sg.example.com", nodes.single().server)
    }

    @Test
    fun parsesBase64SubscriptionAndUrlSafeCredentials() {
        val uri = "vless://12345678-1234-1234-1234-123456789abc@example.com:443" +
            "?security=tls&sni=example.com#Tokyo"
        val body = Base64.getEncoder().encodeToString(uri.toByteArray(StandardCharsets.UTF_8))

        val nodes = SubscriptionParser.parse(body, "subscription", "Test")

        assertEquals(1, nodes.size)
        assertEquals("Tokyo", nodes.single().name)
        assertTrue(nodes.single().raw.optBoolean("tls"))
    }

    @Test
    fun defaultOverseasGroupContainsExpectedDomains() {
        assertEquals(67, DEFAULT_PROXY_DOMAINS.size)
        assertTrue("arcteryx.com" in DEFAULT_PROXY_DOMAINS)
        assertTrue("chatgpt.com" in DEFAULT_PROXY_DOMAINS)
        assertTrue("claude.ai" in DEFAULT_PROXY_DOMAINS)
    }
}
