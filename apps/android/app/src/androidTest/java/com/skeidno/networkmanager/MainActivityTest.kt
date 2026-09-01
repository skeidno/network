package com.skeidno.networkmanager

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.ExperimentalTestApi
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithContentDescription
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import androidx.compose.ui.test.waitUntilAtLeastOneExists
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class MainActivityTest {
    @get:Rule
    val composeRule = createAndroidComposeRule<MainActivity>()

    @Test
    fun mobileNavigationHasNodesAndNoSshScreen() {
        composeRule.onNodeWithTag("page-title").assertIsDisplayed()
        composeRule.onNodeWithTag("nav-nodes").performClick()
        composeRule.onNodeWithText("批量测速").assertIsDisplayed()
        composeRule.onNodeWithText("SSH 服务器").assertDoesNotExist()
    }

    @Test
    fun rulesExposeGroupedDomainsAndMandatoryFallback() {
        composeRule.onNodeWithTag("nav-rules").performClick()
        composeRule.onNodeWithText("内网与局域网").assertIsDisplayed()
        composeRule.onNodeWithText("常用海外站点").assertIsDisplayed()
        composeRule.onNodeWithText("强制保底规则").assertIsDisplayed()
        composeRule.onNodeWithTag("common-rules-edit").performClick()
        composeRule.onNodeWithTag("common-rule-values").assertIsDisplayed()
        composeRule.onNodeWithText("整理去重").assertIsDisplayed()
        composeRule.onNodeWithText("恢复默认").assertIsDisplayed()
    }

    @Test
    fun ruleEditorSupportsInstalledApplicationsAndMultilineValues() {
        composeRule.onNodeWithTag("nav-rules").performClick()
        composeRule.onNodeWithTag("rules-add").performClick()
        composeRule.onNodeWithTag("rule-values").assertIsDisplayed()
        composeRule.onNodeWithTag("rule-type-selector").performClick()
        composeRule.onNodeWithText("应用程序").performClick()
        composeRule.onNodeWithText("选择已安装应用（可多选）").assertIsDisplayed()
        composeRule.onNodeWithTag("rule-app-search").assertIsDisplayed()
        composeRule.onNodeWithTag("rule-package-values").assertIsDisplayed()
    }

    @Test
    fun settingsExposePortableImportAndExport() {
        composeRule.onNodeWithTag("nav-settings").performClick()
        composeRule.onNodeWithText("跨设备配置").assertIsDisplayed()
        composeRule.onNodeWithText("导入").assertIsDisplayed()
        composeRule.onNodeWithText("导出").assertIsDisplayed()
    }

    @Test
    @OptIn(ExperimentalTestApi::class)
    fun importedNodeCanBeMovedIntoCustomGroup() {
        composeRule.onNodeWithTag("nav-nodes").performClick()
        composeRule.onNodeWithTag("nodes-import").performClick()
        composeRule.onNodeWithText("粘贴内容").performClick()
        composeRule.onNodeWithTag("import-value")
            .performTextInput("proxy.example.com:18080:demo-user:demo-password")
        composeRule.onNodeWithTag("import-confirm").performClick()

        composeRule.waitUntilAtLeastOneExists(
            hasText("HTTP Proxy proxy.example.com:18080", substring = true),
            timeoutMillis = 5_000,
        )
        composeRule.onNodeWithText("HTTP Proxy proxy.example.com:18080")
            .performScrollTo()
            .assertIsDisplayed()
        composeRule.onNodeWithText("分组").performClick()
        composeRule.onNodeWithText("新分组名称").performTextInput("住宅代理")
        composeRule.onNodeWithText("新建").performClick()
        composeRule.onNodeWithTag("managed-node-group-住宅代理").assertIsDisplayed()
        composeRule.onNodeWithText("完成").performClick()

        composeRule.onNodeWithContentDescription("移动到分组").performClick()
        composeRule.onNodeWithTag("assign-node-group-住宅代理").performClick()
        composeRule.onNodeWithTag("node-group-住宅代理").assertIsDisplayed()
    }
}
