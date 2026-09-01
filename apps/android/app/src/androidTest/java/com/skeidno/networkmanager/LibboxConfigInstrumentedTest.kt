package com.skeidno.networkmanager

import androidx.test.ext.junit.runners.AndroidJUnit4
import com.skeidno.networkmanager.data.AppState
import com.skeidno.networkmanager.data.ProxyNode
import com.skeidno.networkmanager.data.PortableRule
import com.skeidno.networkmanager.data.FallbackTarget
import com.skeidno.networkmanager.data.RoutingMode
import com.skeidno.networkmanager.vpn.SingBoxConfigBuilder
import io.nekohasekai.libbox.Libbox
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class LibboxConfigInstrumentedTest {
    @Test
    fun generatedDirectConfigIsAcceptedByNativeCore() {
        val config = SingBoxConfigBuilder.build(AppState(mode = RoutingMode.Direct))

        Libbox.checkConfig(config)

        assertTrue(Libbox.version().startsWith("1.13"))
    }

    @Test
    fun generatedSmartConfigIsAcceptedByNativeCore() {
        val node = ProxyNode(
            id = "node-1",
            name = "Test",
            sourceId = "test",
            sourceName = "Test",
            rawJson = """{"type":"vless","server":"example.com","port":443,"uuid":"id","tls":true}""",
        )
        val config = SingBoxConfigBuilder.build(
            AppState(mode = RoutingMode.Smart, selectedNodeId = node.id, nodes = listOf(node)),
        )

        Libbox.checkConfig(config)
    }

    @Test
    fun generatedApplicationRuleIsAcceptedByNativeCore() {
        val config = SingBoxConfigBuilder.build(
            AppState(
                mode = RoutingMode.Direct,
                portableRules = listOf(
                    PortableRule("package_name", "com.android.chrome", FallbackTarget.Direct),
                ),
            ),
        )

        Libbox.checkConfig(config)
    }
}
