package com.skeidno.networkmanager.vpn

import com.skeidno.networkmanager.data.AppState
import com.skeidno.networkmanager.data.DEFAULT_PROXY_DOMAINS
import com.skeidno.networkmanager.data.FallbackTarget
import com.skeidno.networkmanager.data.LocalDnsEntry
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
        // 别写死条数：内置清单会持续扩充，写死的数字每次扩充都会假失败。
        assertEquals(DEFAULT_PROXY_DOMAINS.size, domainRule.getJSONArray("domain_suffix").length())
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
    fun localProxyInboundAcceptsBothHttpAndSocks5OnOnePort() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(mode = RoutingMode.Global, selectedNodeId = node.id, nodes = listOf(node)),
                probePort = 45001,
                localProxyPort = 7890,
            ),
        )

        val inbounds = config.getJSONArray("inbounds")
        val local = (0 until inbounds.length())
            .map { inbounds.getJSONObject(it) }
            .first { it.optString("tag") == SingBoxConfigBuilder.localInboundTag }

        // sing-box 的 mixed 入站按首字节自动分辨 HTTP 与 SOCKS5，不需要开两个端口。
        assertEquals("mixed", local.getString("type"))
        assertEquals(7890, local.getInt("listen_port"))
        assertEquals("127.0.0.1", local.getString("listen"))
        // 本地代理不加强制规则，走的是当前代理模式下的正常分流。
        val forced = config.getJSONObject("route").getJSONArray("rules").let { rules ->
            (0 until rules.length())
                .map { rules.getJSONObject(it) }
                .filter { it.optJSONArray("inbound")?.optString(0) == SingBoxConfigBuilder.localInboundTag }
        }
        assertEquals(0, forced.size)
        assertEquals("proxy", config.getJSONObject("route").getString("final"))
    }

    @Test
    fun localProxyInboundIsAbsentWhenPortIsNotAllocated() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(mode = RoutingMode.Rule, selectedNodeId = node.id, nodes = listOf(node)),
                probePort = 45002,
            ),
        )

        val tags = config.getJSONArray("inbounds").let { inbounds ->
            (0 until inbounds.length()).map { inbounds.getJSONObject(it).optString("tag") }
        }
        assertTrue(SingBoxConfigBuilder.localInboundTag !in tags)
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

    @Test
    fun localDnsMappingBecomesHostsServerThatBeatsTheLanSuffixRule() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(
                    mode = RoutingMode.Global,
                    selectedNodeId = node.id,
                    nodes = listOf(node),
                    localDnsEntries = listOf(
                        LocalDnsEntry("nas.lan", "192.168.1.10"),
                        LocalDnsEntry("wiki.internal-lab.test", "10.0.0.8"),
                    ),
                ),
            ),
        )

        val dns = config.getJSONObject("dns")
        val hosts = dns.getJSONArray("servers").getJSONObject(0)
        assertEquals("hosts", hosts.getString("type"))
        assertEquals(SingBoxConfigBuilder.localDnsServerTag, hosts.getString("tag"))
        val predefined = hosts.getJSONObject("predefined")
        assertEquals("192.168.1.10", predefined.getString("nas.lan"))
        assertEquals("10.0.0.8", predefined.getString("wiki.internal-lab.test"))

        // 顺序是关键：nas.lan 同样命中内置的 .lan 后缀规则，映射规则必须排在它前面。
        val rules = dns.getJSONArray("rules")
        val first = rules.getJSONObject(0)
        assertEquals("route", first.getString("action"))
        assertEquals(SingBoxConfigBuilder.localDnsServerTag, first.getString("server"))
        assertEquals("nas.lan", first.getJSONArray("domain").getString(0))
        val lanRule = rules.getJSONObject(1)
        assertEquals("local", lanRule.getString("server"))
        assertTrue(lanRule.getJSONArray("domain_suffix").toString().contains("lan"))
    }

    @Test
    fun wildcardMappingBecomesPredefinedDnsRecords() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(
                    mode = RoutingMode.Global,
                    selectedNodeId = node.id,
                    nodes = listOf(node),
                    localDnsEntries = listOf(LocalDnsEntry("*.baidu.com", "10.0.2.2")),
                ),
            ),
        )

        val dns = config.getJSONObject("dns")
        // 通配条目进不了 hosts 服务器（那是精确查表），所以有它就不该建这台服务器。
        assertEquals("local", dns.getJSONArray("servers").getJSONObject(0).getString("tag"))

        val rules = dns.getJSONArray("rules")
        // 第一条管顶级域自身：`domain_suffix` 是含自身的，但 `*.` 记录换名只对子域生效，
        // 少了这条 https://baidu.com 会漏出去。
        val apex = rules.getJSONObject(0)
        assertEquals("predefined", apex.getString("action"))
        assertEquals("baidu.com", apex.getJSONArray("domain").getString(0))
        assertEquals("baidu.com. IN A 10.0.2.2", apex.getJSONArray("answer").getString(0))
        // 第二条管子域：记录名带 `*`，sing-box 会把它换成实际查询的域名。
        val suffix = rules.getJSONObject(1)
        assertEquals("predefined", suffix.getString("action"))
        assertEquals("baidu.com", suffix.getJSONArray("domain_suffix").getString(0))
        assertEquals("*.baidu.com. IN A 10.0.2.2", suffix.getJSONArray("answer").getString(0))
    }

    @Test
    fun wildcardMappingToIpv6UsesAaaaRecords() {
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(
                    mode = RoutingMode.Global,
                    localDnsEntries = listOf(LocalDnsEntry("*.baidu.com", "fd00::5")),
                ),
            ),
        )
        val rules = config.getJSONObject("dns").getJSONArray("rules")
        assertEquals("baidu.com. IN AAAA fd00::5", rules.getJSONObject(0).getJSONArray("answer").getString(0))
    }

    @Test
    fun exactMappingStillWinsOverWildcard() {
        // 精确条目生成的规则排在通配条目前面，先命中先算。
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(
                    mode = RoutingMode.Global,
                    localDnsEntries = listOf(
                        LocalDnsEntry("*.baidu.com", "10.0.2.2"),
                        LocalDnsEntry("www.baidu.com", "10.0.2.3"),
                    ),
                ),
            ),
        )
        val rules = config.getJSONObject("dns").getJSONArray("rules")
        assertEquals("route", rules.getJSONObject(0).getString("action"))
        assertEquals("www.baidu.com", rules.getJSONObject(0).getJSONArray("domain").getString(0))
        assertEquals("predefined", rules.getJSONObject(1).getString("action"))
    }

    @Test
    fun dnsServersStayUntouchedWithoutLocalDnsMapping() {
        // 没配映射时不能凭空多出一台 DNS 服务器，否则原有配置的语义就变了。
        val config = JSONObject(SingBoxConfigBuilder.build(AppState(nodes = listOf(node))))
        val dns = config.getJSONObject("dns")
        assertEquals("local", dns.getJSONArray("servers").getJSONObject(0).getString("tag"))
        assertEquals("local", dns.getJSONArray("rules").getJSONObject(0).getString("server"))
    }

    @Test
    fun tunInboundCarriesIncludePackageForProfileAllowList() {
        // 档案限定了「只接管这几个 App」时，白名单必须落在 tun inbound 上 ——
        // libbox 靠它调用 addAllowedApplication，名单外的 App 才真的不进 TUN。
        val config = JSONObject(
            SingBoxConfigBuilder.build(
                AppState(nodes = listOf(node), tunPackages = listOf("com.android.chrome", "org.telegram.messenger")),
            ),
        )
        val tun = config.getJSONArray("inbounds").getJSONObject(0)
        assertEquals("tun", tun.getString("type"))
        val include = tun.getJSONArray("include_package")
        assertEquals(2, include.length())
        assertEquals("com.android.chrome", include.getString(0))
        assertTrue(!tun.has("exclude_package"))
    }

    @Test
    fun tunInboundOmitsIncludePackageWhenProfileTakesEverything() {
        // 空名单不能写成空数组：空数组的含义是「谁都不接管」，和「接管全部」正好相反。
        val config = JSONObject(SingBoxConfigBuilder.build(AppState(nodes = listOf(node))))
        val tun = config.getJSONArray("inbounds").getJSONObject(0)
        assertTrue(!tun.has("include_package"))
        assertTrue(!tun.has("exclude_package"))
    }
}
