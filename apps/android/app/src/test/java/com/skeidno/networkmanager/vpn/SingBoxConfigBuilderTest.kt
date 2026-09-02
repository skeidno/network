package com.skeidno.networkmanager.vpn

import com.skeidno.networkmanager.data.AppState
import com.skeidno.networkmanager.data.FallbackTarget
import com.skeidno.networkmanager.data.ProxyNode
import com.skeidno.networkmanager.data.PortableRule
import com.skeidno.networkmanager.data.RoutingMode
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class SingBoxConfigBuilderTest {
    private val node = ProxyNode(
        id = "node-1",
        name = "Test node",
        sourceId = "test",
        sourceName = "Test",
        rawJson = """{"type":"vless","server":"example.com","port":443,"uuid":"id","tls":true}""",
    )

    @Test
    fun groupedDomainsUseOneSelectedProxyAndFallbackStaysDirect() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(
                    mode = RoutingMode.Rule,
                    fallbackTarget = FallbackTarget.Direct,
                    selectedNodeId = node.id,
                    nodes = listOf(node),
                ),
            ),
        )

        val route = config.getJSONObject("route")
        assertEquals("direct", route.getString("final"))
        val domainRule = route.getJSONArray("rules").getJSONObject(4)
        assertEquals("proxy", domainRule.getString("outbound"))
        assertEquals(67, domainRule.getJSONArray("domain_suffix").length())
        assertTrue(config.toString().contains("arcteryx.com"))
        val outbounds = config.getJSONArray("outbounds")
        assertTrue((0 until outbounds.length()).none {
            outbounds.getJSONObject(it).optString("type") == "urltest"
        })
    }

    @Test
    fun globalModeUsesSelectedProxyAsFinalOutbound() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(mode = RoutingMode.Global, selectedNodeId = node.id, nodes = listOf(node)),
            ),
        )

        assertEquals("proxy", config.getJSONObject("route").getString("final"))
        assertEquals("remote-proxy", config.getJSONObject("dns").getString("final"))
        assertEquals("local", config.getJSONObject("route").getString("default_domain_resolver"))
        assertEquals("node-1", config.getJSONArray("outbounds").getJSONObject(1).getString("default"))
    }

    @Test
    fun smartModeUsesHealthCheckedLowLatencyOutbound() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(mode = RoutingMode.Smart, selectedNodeId = node.id, nodes = listOf(node)),
            ),
        )

        assertEquals("smart", config.getJSONObject("route").getString("final"))
        val smart = config.getJSONArray("outbounds").getJSONObject(2)
        assertEquals("urltest", smart.getString("type"))
        assertEquals("1m", smart.getString("interval"))
        assertEquals(120, smart.getInt("tolerance"))
        val dns = config.getJSONObject("dns")
        assertEquals("remote-smart", dns.getString("final"))
        assertEquals("smart", dns.getJSONArray("servers").getJSONObject(2).getString("detour"))
    }

    @Test
    fun portableRulesAreAppliedBeforeFallback() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(
                    selectedNodeId = node.id,
                    nodes = listOf(node),
                    portableRules = listOf(
                        PortableRule("domain_suffix", "example.org", FallbackTarget.Proxy),
                    ),
                ),
            ),
        )

        val rules = config.getJSONObject("route").getJSONArray("rules")
        val portable = rules.getJSONObject(5)
        assertEquals("example.org", portable.getJSONArray("domain_suffix").getString(0))
        assertEquals("proxy", portable.getString("outbound"))
    }

    @Test
    fun androidApplicationRuleUsesPackageName() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(
                    selectedNodeId = node.id,
                    nodes = listOf(node),
                    portableRules = listOf(
                        PortableRule("package_name", "com.discord", FallbackTarget.Proxy),
                    ),
                ),
            ),
        )

        val rules = config.getJSONObject("route").getJSONArray("rules")
        val appRule = rules.getJSONObject(4)
        assertEquals("com.discord", appRule.getJSONArray("package_name").getString(0))
        assertEquals("proxy", appRule.getString("outbound"))
    }

    @Test
    fun applicationRulesTakePriorityOverAllDomainRules() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(
                    mode = RoutingMode.Rule,
                    fallbackTarget = FallbackTarget.Direct,
                    selectedNodeId = node.id,
                    nodes = listOf(node),
                    portableRules = listOf(
                        PortableRule("domain_suffix", "example.com", FallbackTarget.Direct),
                        PortableRule("package_name", "com.example.app", FallbackTarget.Proxy),
                        PortableRule("domain", "api.example.com", FallbackTarget.Direct),
                    ),
                ),
            ),
        )

        val rules = config.getJSONObject("route").getJSONArray("rules")
        val appRuleIndex = (0 until rules.length()).first {
            rules.getJSONObject(it).has("package_name")
        }
        val publicDomainRuleIndexes = (0 until rules.length()).filter { index ->
            val rule = rules.getJSONObject(index)
            val values = rule.optJSONArray("domain") ?: rule.optJSONArray("domain_suffix")
                ?: rule.optJSONArray("domain_keyword")
            values != null && (0 until values.length()).any { valueIndex ->
                values.getString(valueIndex) !in setOf("lan", "local", "home.arpa")
            }
        }
        val appRule = rules.getJSONObject(appRuleIndex)

        assertTrue(publicDomainRuleIndexes.all { appRuleIndex < it })
        assertEquals(2, appRule.length())
        assertEquals("com.example.app", appRule.getJSONArray("package_name").getString(0))
        assertEquals("proxy", appRule.getString("outbound"))
    }

    @Test
    fun applicationProxyRuleAlsoUsesRemoteDnsThroughSelectedNode() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(
                    mode = RoutingMode.Rule,
                    fallbackTarget = FallbackTarget.Direct,
                    selectedNodeId = node.id,
                    nodes = listOf(node),
                    portableRules = listOf(
                        PortableRule("package_name", "com.example.app", FallbackTarget.Proxy),
                    ),
                ),
            ),
        )

        val dns = config.getJSONObject("dns")
        val remote = dns.getJSONArray("servers").getJSONObject(1)
        val appRule = dns.getJSONArray("rules").getJSONObject(1)

        assertEquals("remote-proxy", remote.getString("tag"))
        assertEquals("proxy", remote.getString("detour"))
        assertEquals("com.example.app", appRule.getJSONArray("package_name").getString(0))
        assertEquals("remote-proxy", appRule.getString("server"))
        assertEquals("local", dns.getString("final"))
    }

    @Test
    fun lanTrafficBypassesProxyModeAndTunRoute() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(mode = RoutingMode.Global, selectedNodeId = node.id, nodes = listOf(node)),
            ),
        )

        val routeRules = config.getJSONObject("route").getJSONArray("rules")
        assertEquals("direct", routeRules.getJSONObject(2).getString("outbound"))
        assertTrue(routeRules.getJSONObject(2).getJSONArray("domain_suffix").toString().contains("local"))
        assertTrue(routeRules.getJSONObject(3).getJSONArray("ip_cidr").toString().contains("192.168.0.0/16"))
        val tun = config.getJSONArray("inbounds").getJSONObject(0)
        assertTrue(tun.getJSONArray("route_exclude_address").toString().contains("fc00::/7"))
    }

    @Test
    fun authenticatedHttpProxyMapsToSingBoxOutbound() {
        val httpNode = ProxyNode(
            id = "http-node",
            name = "HTTP Proxy proxy.example.com:4600",
            sourceId = "manual",
            sourceName = "Manual",
            rawJson = """{"type":"http","server":"proxy.example.com","port":4600,"username":"user","password":"secret"}""",
        )

        val outbound = SingBoxConfigBuilder.toOutbound(httpNode)

        assertEquals("http", outbound.getString("type"))
        assertEquals("proxy.example.com", outbound.getString("server"))
        assertEquals(4600, outbound.getInt("server_port"))
        assertEquals("user", outbound.getString("username"))
        assertEquals("secret", outbound.getString("password"))
    }

    @Test
    fun authenticatedHttpProxyCanDetourThroughAnotherNode() {
        val relay = ProxyNode(
            id = "relay-node",
            name = "Overseas server",
            sourceId = "server",
            sourceName = "Server deployment",
            rawJson = """{"type":"socks","server":"198.51.100.20","port":24443}""",
        )
        val target = ProxyNode(
            id = "http-node",
            name = "Residential HTTP",
            sourceId = "manual",
            sourceName = "Manual",
            rawJson = """{"type":"http","server":"proxy.example.com","port":4600,"username":"user","password":"secret","_network-manager-dialer-proxy":"Overseas server"}""",
        )

        val config = JSONObject(
            SingBoxConfigBuilder.build(AppState(nodes = listOf(target, relay))),
        )
        val targetOutbound = config.getJSONArray("outbounds").getJSONObject(0)

        assertEquals("relay-node", targetOutbound.getString("detour"))
        val endpointRule = config.getJSONObject("route").getJSONArray("rules").let { rules ->
            (0 until rules.length())
                .map { rules.getJSONObject(it) }
                .first { it.optJSONArray("domain")?.optString(0) == "proxy.example.com" }
        }
        assertEquals("relay-node", endpointRule.getString("outbound"))
    }

    @Test
    fun realityOutboundEnablesUtlsWithACompatibleFingerprint() {
        val realityNode = ProxyNode(
            id = "reality-node",
            name = "Reality",
            sourceId = "test",
            sourceName = "Test",
            rawJson = """{"type":"vless","server":"example.com","port":443,"uuid":"id","reality-opts":{"public-key":"key","short-id":"short"}}""",
        )

        val tls = SingBoxConfigBuilder.toOutbound(realityNode).getJSONObject("tls")

        assertTrue(tls.getJSONObject("reality").getBoolean("enabled"))
        assertTrue(tls.getJSONObject("utls").getBoolean("enabled"))
        assertEquals("chrome", tls.getJSONObject("utls").getString("fingerprint"))
    }
}
