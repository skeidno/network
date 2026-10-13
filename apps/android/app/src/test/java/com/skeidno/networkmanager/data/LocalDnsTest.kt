package com.skeidno.networkmanager.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * 本地 DNS 映射的解析与校验。
 *
 * 这一层的每一条都会直接进内核配置，所以宁可啰嗦也要把边界钉死：
 * 域名通配、端口、不是 IP 的地址，都得在保存之前拦住。
 */
class LocalDnsTest {

    @Test
    fun acceptsBothFieldOrders() {
        // hosts 文件是「IP 域名」，命令行习惯是「域名 IP」，两种都收。
        val entries = localDnsEntriesFromText(
            """
            nas.lan 192.168.1.10
            192.168.1.11 router.lan
            """.trimIndent(),
        )
        assertEquals(2, entries.size)
        assertEquals(LocalDnsEntry("nas.lan", "192.168.1.10"), entries[0])
        assertEquals(LocalDnsEntry("router.lan", "192.168.1.11"), entries[1])
    }

    @Test
    fun ignoresCommentsAndBlankLines() {
        val entries = localDnsEntriesFromText(
            """
            # 家里这台 NAS
            nas.lan 192.168.1.10   # 固定 IP

            """.trimIndent(),
        )
        assertEquals(listOf(LocalDnsEntry("nas.lan", "192.168.1.10")), entries)
    }

    @Test
    fun normalizesPastedUrlAndCase() {
        val entries = localDnsEntriesFromText("https://NAS.lan/admin 192.168.1.10")
        assertEquals("nas.lan", entries.single().domain)
    }

    @Test
    fun laterDuplicateReplacesEarlierOne() {
        val entries = localDnsEntriesFromText("nas.lan 192.168.1.10\nnas.lan 192.168.1.20")
        assertEquals(1, entries.size)
        assertEquals("192.168.1.20", entries.single().address)
    }

    @Test
    fun acceptsIpv6Address() {
        assertEquals("fd00::10", localDnsEntriesFromText("nas.lan fd00::10").single().address)
    }

    @Test
    fun acceptsBracketedIpv6ButStoresItWithoutBrackets() {
        // 粘 URL 写法要认，但落盘只留一种：hosts 表里需要的是 `fd00::10`。
        // 桌面端（rust/src/models.rs 的 normalize_address_literal）是同一套规则。
        assertEquals("fd00::10", localDnsEntriesFromText("nas.lan [fd00::10]").single().address)
    }

    @Test
    fun rejectsWildcardAndSaysWhy() {
        val error = assertThrows(IllegalArgumentException::class.java) {
            localDnsEntriesFromText("*.lan 192.168.1.10")
        }
        assertTrue(error.message.orEmpty(), error.message.orEmpty().contains("通配"))
    }

    @Test
    fun rejectsLineWithOnlyOneField() {
        val error = assertThrows(IllegalArgumentException::class.java) {
            localDnsEntriesFromText("nas.lan")
        }
        assertTrue(error.message.orEmpty(), error.message.orEmpty().contains("第 1 行"))
    }

    @Test
    fun rejectsInvalidAddressWithLineNumber() {
        val error = assertThrows(IllegalArgumentException::class.java) {
            localDnsEntriesFromText("nas.lan 192.168.1.10\nbad.lan 999.1.1.1")
        }
        assertTrue(error.message.orEmpty(), error.message.orEmpty().contains("第 2 行"))
        assertTrue(error.message.orEmpty(), error.message.orEmpty().contains("999.1.1.1"))
    }

    @Test
    fun rejectsPortInsideDomain() {
        val error = assertThrows(IllegalArgumentException::class.java) {
            localDnsEntriesFromText("nas.lan:443 192.168.1.10")
        }
        assertTrue(error.message.orEmpty(), error.message.orEmpty().contains("不要带端口"))
    }

    @Test
    fun rejectsLineWithTwoAddresses() {
        val error = assertThrows(IllegalArgumentException::class.java) {
            localDnsEntriesFromText("192.168.1.10 192.168.1.11")
        }
        assertTrue(error.message.orEmpty(), error.message.orEmpty().contains("没有域名"))
    }

    @Test
    fun textRoundTripIsStable() {
        val entries = listOf(
            LocalDnsEntry("nas.lan", "192.168.1.10"),
            LocalDnsEntry("wiki.lan", "10.0.0.8"),
        )
        assertEquals(entries, localDnsEntriesFromText(localDnsEntriesToText(entries)))
    }

    @Test
    fun sanitizeDropsEntriesThatWouldBreakTheCoreConfig() {
        val sanitized = sanitizeLocalDnsEntries(
            listOf(
                LocalDnsEntry("nas.lan", "192.168.1.10"),
                LocalDnsEntry("bad.lan", "not-an-ip"),
                LocalDnsEntry("", "192.168.1.1"),
                LocalDnsEntry("*.lan", "192.168.1.1"),
            ),
        )
        assertEquals(listOf(LocalDnsEntry("nas.lan", "192.168.1.10")), sanitized)
    }

    @Test
    fun entryLimitIsEnforced() {
        val text = (1..5).joinToString("\n") { "h$it.lan 192.168.1.$it" }
        assertEquals(5, localDnsEntriesFromText(text).size)
        val error = assertThrows(IllegalArgumentException::class.java) {
            localDnsEntriesFromText(text, limit = 3)
        }
        assertTrue(error.message.orEmpty(), error.message.orEmpty().contains("最多 3 条"))
    }

    @Test
    fun ipLiteralDetectionNeverTreatsHostsAsAddresses() {
        assertTrue(isIpLiteral("192.168.1.10"))
        assertTrue(isIpLiteral("fd00::10"))
        assertTrue(isIpLiteral("::1"))
        // 这几条看着像 IP 但不是，必须判否——否则会拿去做 DNS 查询。
        assertFalse(isIpLiteral("192.168.1.256"))
        assertFalse(isIpLiteral("nas.lan"))
        assertFalse(isIpLiteral("nas.lan:443"))
        assertNull(parseIpLiteral("example.com"))
        assertNull(parseIpLiteral(""))
    }
}
