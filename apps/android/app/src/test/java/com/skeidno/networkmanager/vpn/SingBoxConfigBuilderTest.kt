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
    }

    @Test
    fun globalModeUsesSelectedProxyAsFinalOutbound() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(mode = RoutingMode.Global, selectedNodeId = node.id, nodes = listOf(node)),
            ),
        )

        assertEquals("proxy", config.getJSONObject("route").getString("final"))
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
}
