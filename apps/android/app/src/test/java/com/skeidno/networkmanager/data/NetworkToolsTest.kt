package com.skeidno.networkmanager.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class NetworkToolsTest {

    private fun node(server: String = "203.0.113.10", port: Int = 8388) = ProxyNode(
        id = "n-$server",
        name = "测试节点 $server",
        sourceId = "src",
        sourceName = "测试",
        rawJson = """{"type":"ss","server":"$server","port":$port}""",
    )

    private fun state(
        nodes: List<ProxyNode> = listOf(node()),
        mode: RoutingMode = RoutingMode.Rule,
        fallback: FallbackTarget = FallbackTarget.Direct,
    ) = AppState(
        nodes = nodes,
        mode = mode,
        fallbackTarget = fallback,
        ruleGroup = defaultOverseasRuleGroup(),
    )

    // ------------------------------------------------------------------
    // 编解码
    // ------------------------------------------------------------------

    @Test
    fun base64RoundTrip() {
        assertEquals("aGVsbG8=", NetworkTools.encodeBase64("hello"))
        assertEquals("hello", NetworkTools.decodeBase64("aGVsbG8="))
    }

    @Test
    fun base64DecodeAcceptsUrlSafeAndMissingPadding() {
        // URL-safe 的 - _ 和缺 padding 都要能解出来：订阅里的链接两种写法都有。
        val encoded = NetworkTools.encodeBase64("aes-128-gcm:password")
        val urlSafe = encoded.replace('+', '-').replace('/', '_').trimEnd('=')
        assertEquals("aes-128-gcm:password", NetworkTools.decodeBase64(urlSafe))
    }

    @Test
    fun base64DecodeRejectsGarbage() {
        assertNull(NetworkTools.decodeBase64("!!!not base64!!!"))
        assertNull(NetworkTools.decodeBase64("   "))
    }

    @Test
    fun urlCodecRoundTrip() {
        assertEquals("a%3Db%26c", NetworkTools.encodeUrl("a=b&c"))
        assertEquals("a=b&c", NetworkTools.decodeUrl("a%3Db%26c"))
        // 非法百分号转义要返回 null 而不是抛出去
        assertNull(NetworkTools.decodeUrl("bad%2"))
    }

    @Test
    fun uuidLooksLikeUuid() {
        val uuid = NetworkTools.randomUuid()
        assertTrue(
            uuid,
            Regex("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$").matches(uuid),
        )
    }

    // ------------------------------------------------------------------
    // CIDR
    // ------------------------------------------------------------------

    @Test
    fun ipv4CidrMatching() {
        assertTrue(NetworkTools.ipInCidr("10.1.2.3", "10.0.0.0/8"))
        assertFalse(NetworkTools.ipInCidr("11.1.2.3", "10.0.0.0/8"))
        assertTrue(NetworkTools.ipInCidr("192.168.1.1", "192.168.0.0/16"))
        assertTrue(NetworkTools.ipInCidr("127.0.0.1", "127.0.0.0/8"))
        assertTrue(NetworkTools.ipInCidr("172.20.0.1", "172.16.0.0/12"))
        assertFalse(NetworkTools.ipInCidr("172.32.0.1", "172.16.0.0/12"))
        // /32 等价于精确匹配
        assertTrue(NetworkTools.ipInCidr("8.8.8.8", "8.8.8.8/32"))
        assertFalse(NetworkTools.ipInCidr("8.8.8.9", "8.8.8.8/32"))
    }

    @Test
    fun ipv6CidrMatching() {
        assertTrue(NetworkTools.ipInCidr("::1", "::1/128"))
        assertFalse(NetworkTools.ipInCidr("::2", "::1/128"))
        assertTrue(NetworkTools.ipInCidr("fc00::1", "fc00::/7"))
        assertTrue(NetworkTools.ipInCidr("fd12::1", "fc00::/7"))
        assertTrue(NetworkTools.ipInCidr("fe80::abcd", "fe80::/10"))
    }

    @Test
    fun cidrRejectsAddressFamilyMismatch() {
        assertFalse(NetworkTools.ipInCidr("10.0.0.1", "::1/128"))
        assertFalse(NetworkTools.ipInCidr("::1", "10.0.0.0/8"))
    }

    // ------------------------------------------------------------------
    // TCP 探活（不联网，只测参数校验分支）
    // ------------------------------------------------------------------

    @Test
    fun tcpProbeValidatesArgumentsWithoutTouchingNetwork() {
        assertFalse(NetworkTools.probeTcp("", 443).reachable)
        assertFalse(NetworkTools.probeTcp("example.com", 0).reachable)
        assertFalse(NetworkTools.probeTcp("example.com", 70_000).reachable)
        assertTrue(NetworkTools.probeTcp("example.com", 0).detail.contains("端口"))
    }

    // ------------------------------------------------------------------
    // 规则命中预判
    // ------------------------------------------------------------------

    @Test
    fun lanDomainGoesDirect() {
        val verdict = NetworkTools.previewRoute(state(), "router.lan")
        assertEquals(NetworkTools.Outbound.Direct, verdict.outbound)
        assertTrue(verdict.reason, verdict.reason.contains("局域网"))
    }

    @Test
    fun lanSuffixIsPlainStringMatchSoItAlsoCatchesSubstrings() {
        // 这是 sing-box domain_suffix 的真实语义，不是 bug：没有标签边界。
        val verdict = NetworkTools.previewRoute(state(), "huanglan")
        assertEquals(NetworkTools.Outbound.Direct, verdict.outbound)
        assertTrue(verdict.notes.isNotEmpty())
    }

    @Test
    fun privateIpGoesDirect() {
        val verdict = NetworkTools.previewRoute(state(), "192.168.31.7")
        assertEquals(NetworkTools.Outbound.Direct, verdict.outbound)
        assertTrue(verdict.reason, verdict.reason.contains("局域网"))
    }

    @Test
    fun commonRuleGroupDomainGoesProxyWhenNodesExist() {
        val verdict = NetworkTools.previewRoute(state(), "chatgpt.com")
        assertEquals(NetworkTools.Outbound.Proxy, verdict.outbound)
        assertTrue(verdict.reason, verdict.reason.contains("常用站点"))
    }

    @Test
    fun commonRuleGroupDegradesToDirectWithoutNodes() {
        val verdict = NetworkTools.previewRoute(state(nodes = emptyList()), "chatgpt.com")
        assertEquals(NetworkTools.Outbound.Direct, verdict.outbound)
    }

    @Test
    fun unmatchedDomainFallsBackToMode() {
        // 规则模式 + 兜底直连（默认）
        assertEquals(
            NetworkTools.Outbound.Direct,
            NetworkTools.previewRoute(state(), "no-such-rule.example").outbound,
        )
        // 规则模式 + 兜底走节点
        assertEquals(
            NetworkTools.Outbound.Proxy,
            NetworkTools.previewRoute(
                state(nodes = listOf(node()), fallback = FallbackTarget.Proxy),
                "no-such-rule.example",
            ).outbound,
        )
        // 全局模式
        assertEquals(
            NetworkTools.Outbound.Proxy,
            NetworkTools.previewRoute(state(mode = RoutingMode.Global), "no-such-rule.example").outbound,
        )
        // 直连模式
        assertEquals(
            NetworkTools.Outbound.Direct,
            NetworkTools.previewRoute(state(mode = RoutingMode.Direct), "no-such-rule.example").outbound,
        )
    }

    @Test
    fun globalModeWithoutNodesDegradesToDirect() {
        val verdict = NetworkTools.previewRoute(
            state(nodes = emptyList(), mode = RoutingMode.Global),
            "no-such-rule.example",
        )
        assertEquals(NetworkTools.Outbound.Direct, verdict.outbound)
        assertTrue(verdict.notes.isNotEmpty())
    }

    @Test
    fun customPortableRulesAreHonoured() {
        // 查询刻意选一个不会先被「常用站点」规则吃掉的域名。
        val keywordRule = PortableRule(
            type = "domain_keyword",
            value = "internal-lab",
            target = FallbackTarget.Proxy,
        )
        val verdict = NetworkTools.previewRoute(
            state().copy(portableRules = listOf(keywordRule)),
            "media.internal-lab.test",
        )
        assertEquals(NetworkTools.Outbound.Proxy, verdict.outbound)
        assertTrue(verdict.reason, verdict.reason.contains("自定义规则"))
    }

    @Test
    fun disabledPortableRuleIsIgnored() {
        val disabled = PortableRule(
            type = "domain_suffix",
            value = "internal-lab.test",
            target = FallbackTarget.Proxy,
            enabled = false,
        )
        val verdict = NetworkTools.previewRoute(
            state().copy(portableRules = listOf(disabled)),
            "media.internal-lab.test",
        )
        assertEquals(NetworkTools.Outbound.Direct, verdict.outbound)
    }

    @Test
    fun commonRuleGroupIsAlsoAPlainSuffixMatch() {
        // 内置表里有 x.com，于是 notx.com 也会被判成「常用站点」——
        // 记录的正是 domain_suffix 不带标签边界这个语义。
        val verdict = NetworkTools.previewRoute(state(), "notx.com")
        assertEquals(NetworkTools.Outbound.Proxy, verdict.outbound)
        assertTrue(verdict.reason, verdict.reason.contains("常用站点"))
    }

    @Test
    fun packageNameRuleNeverMatchesDomainQueries() {
        // package_name 是「哪个应用」的规则，按域名查不该命中。
        val packageRule = PortableRule(
            type = "package_name",
            value = "com.example.app",
            target = FallbackTarget.Proxy,
        )
        val verdict = NetworkTools.previewRoute(
            state().copy(portableRules = listOf(packageRule)),
            "com.example.app",
        )
        assertFalse(verdict.reason, verdict.reason.contains("自定义规则"))
    }

    @Test
    fun nodeServerAddressIsForcedDirect() {
        val verdict = NetworkTools.previewRoute(state(), "203.0.113.10")
        assertEquals(NetworkTools.Outbound.Direct, verdict.outbound)
        assertTrue(verdict.reason, verdict.reason.contains("节点服务器"))
    }

    @Test
    fun blankQueryIsRejectedGracefully() {
        val verdict = NetworkTools.previewRoute(state(), "   ")
        assertEquals(NetworkTools.Outbound.Direct, verdict.outbound)
        assertTrue(verdict.reason, verdict.reason.contains("请输入"))
    }

    // ------------------------------------------------------------------
    // 分享链接解析
    // ------------------------------------------------------------------

    @Test
    fun describesShadowsocksLink() {
        val link = "ss://${NetworkTools.encodeBase64("aes-128-gcm:password")}@1.2.3.4:8388#%E8%8A%82%E7%82%B9A"
        val text = NetworkTools.describeShareLink(link)
        assertTrue(text, text.contains("服务器：1.2.3.4"))
        assertTrue(text, text.contains("端口：8388"))
        assertTrue(text, text.contains("加密方式：aes-128-gcm"))
        assertTrue(text, text.contains("密码：password"))
        assertTrue(text, text.contains("节点A"))
    }

    @Test
    fun describesVlessLink() {
        val link = "vless://11111111-2222-3333-4444-555555555555@example.com:443" +
            "?security=tls&type=ws&path=%2Fws&sni=cdn.example.com#%E6%B5%8B%E8%AF%95"
        val text = NetworkTools.describeShareLink(link)
        assertTrue(text, text.contains("协议：VLESS"))
        assertTrue(text, text.contains("服务器：example.com"))
        assertTrue(text, text.contains("UUID：11111111-2222-3333-4444-555555555555"))
        assertTrue(text, text.contains("SNI：cdn.example.com"))
        assertTrue(text, text.contains("传输：ws"))
        assertTrue(text, text.contains("TLS：开启"))
    }

    @Test
    fun rejectsInputThatIsNotAShareLink() {
        assertTrue(NetworkTools.describeShareLink("hello world").contains("不像一条分享链接"))
        assertTrue(NetworkTools.describeShareLink("ftp://example.com").contains("无法解析"))
    }

    @Test
    fun parsedLinkFieldsMatchTheImporter() {
        // 工具箱显示的和真正导入的必须来自同一套解析结果。
        val link = "ss://${NetworkTools.encodeBase64("2022-blake3-aes-128-gcm:secret")}@5.6.7.8:9443#Node"
        val node = SubscriptionParser.parseUri(link, "toolbox", "工具箱")
        assertNotNull(node)
        assertEquals("5.6.7.8", node!!.server)
        assertEquals(9443, node.port)
        assertEquals("2022-blake3-aes-128-gcm", node.raw.optString("cipher"))
        assertTrue(NetworkTools.describeShareLink(link).contains("2022-blake3-aes-128-gcm"))
    }
}
