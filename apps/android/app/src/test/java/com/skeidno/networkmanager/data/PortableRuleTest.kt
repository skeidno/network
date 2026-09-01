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
}
