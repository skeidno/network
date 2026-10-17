package com.skeidno.networkmanager.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
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
}
