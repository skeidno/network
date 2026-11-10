package com.skeidno.networkmanager.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class PortableRuleTest {
    @Test
    fun multipleValuesAreNormalizedAndDeduplicated() {
        val rules = portableRulesFromValues(
            type = "domain_suffix",
            values = listOf("*.Example.com", "example.com", "other.example"),
            target = FallbackTarget.Proxy,
        )

        assertEquals(listOf("example.com", "other.example"), rules.map(PortableRule::value))
    }

    @Test
    fun invalidValueRejectsTheWholeBatch() {
        val error = assertThrows(IllegalArgumentException::class.java) {
            portableRulesFromValues(
                type = "package_name",
                values = listOf("com.discord", "not-a-package"),
                target = FallbackTarget.Proxy,
            )
        }

        assertEquals("第 2 行：请输入有效应用包名，例如 com.example.app", error.message)
    }

    @Test
    fun topLevelDomainSuffixIsAcceptedInsteadOfRejected() {
        // 想让整个 .cn 直连时会写 .cn。归一化把前导点剥掉成 cn —— 这是合法的单段
        // 后缀，不是忘了填域名。以前拿「必须有点」卡它，这类规则一条都导不进来。
        val rules = portableRulesFromValues(
            type = "domain_suffix",
            values = listOf(".cn"),
            target = FallbackTarget.Direct,
        )

        assertEquals(listOf("cn"), rules.map(PortableRule::value))
    }

    @Test
    fun exactDomainStillRequiresADot() {
        val error = assertThrows(IllegalArgumentException::class.java) {
            portableRulesFromValues(
                type = "domain",
                values = listOf("localhost"),
                target = FallbackTarget.Direct,
            )
        }

        assertEquals("第 1 行：请输入有效域名，例如 example.com", error.message)
    }

    @Test
    fun commonRuleDomainsUseTheSameNormalizationAsPortableRules() {
        val domains = ruleGroupDomainsFromValues(
            listOf("*.Example.com", "custom.example", "EXAMPLE.COM"),
        )

        assertEquals(listOf("example.com", "custom.example"), domains)
    }

    @Test
    fun defaultProxyKeywordsCoverApiAndCdnDomains() {
        val rules = defaultProxyKeywordRules()

        assertEquals(listOf("claude", "anthropic"), rules.map(PortableRule::value))
        assertTrue(rules.all { it.type == "domain_keyword" })
        assertTrue(rules.all { it.target == FallbackTarget.Proxy && it.enabled })
        // 只列后缀会漏掉这些，正是用户遇到的 `api.anthropic.com timed out`。
        for (domain in listOf(
            "api.anthropic.com",
            "console.anthropic.com",
            "claudeusercontent.com",
            "claude.com",
        )) {
            assertTrue(
                "$domain 必须被某个关键字命中",
                rules.any { domain.contains(it.value) },
            )
        }
    }

    @Test
    fun keywordSeedingKeepsUserRulesAndIsIdempotent() {
        val userEdited = PortableRule(
            type = "domain_keyword",
            value = "claude",
            target = FallbackTarget.Direct,
            enabled = false,
            note = "手动改过",
        )
        val custom = PortableRule("domain_suffix", "example.org", FallbackTarget.Proxy)
        val seeded = withDefaultProxyKeywords(listOf(userEdited, custom))

        assertEquals(3, seeded.size)
        assertEquals("用户改过的同名规则必须原样保留", userEdited, seeded[0])
        assertEquals(1, seeded.count { it.value == "anthropic" })
        assertEquals(1, seeded.count { it.value == "claude" })
        assertEquals("重复播种不能加两遍", seeded, withDefaultProxyKeywords(seeded))
    }

    @Test
    fun azureOpenaiIsInFreshDefaultOverseasGroup() {
        val domains = defaultOverseasRuleGroup().domains
        assertTrue("内置海外组必须含 Azure OpenAI 后缀", "openai.azure.com" in domains)
        assertEquals(domains.size, domains.toSet().size)
    }

    @Test
    fun overseasDomainSeedingOnlyAddsNewDomainsAndIsIdempotent() {
        // 模拟用户精简过的老清单：只补本次新增的一条，不能把整份默认清单塞回去，
        // 也不能把用户删掉的旧域名变回来。
        val old = listOf("openai.com", "chatgpt.com")
        val seeded = withDefaultOverseasDomainSeeds(old)

        assertEquals(old + "openai.azure.com", seeded)
        assertEquals("重复播种不能加两遍", seeded, withDefaultOverseasDomainSeeds(seeded))
        assertEquals("不该塞回整份默认清单", 3, seeded.size)
    }

    @Test
    fun overseasDomainSeedingIsCaseInsensitive() {
        val seeded = withDefaultOverseasDomainSeeds(listOf("OPENAI.AZURE.COM"))
        assertEquals(1, seeded.size)
        assertEquals("OPENAI.AZURE.COM", seeded.single())
    }
}
