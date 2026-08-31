package com.skeidno.networkmanager

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createAndroidComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
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
    }

    @Test
    fun settingsExposePortableImportAndExport() {
        composeRule.onNodeWithTag("nav-settings").performClick()
        composeRule.onNodeWithText("跨设备配置").assertIsDisplayed()
        composeRule.onNodeWithText("导入").assertIsDisplayed()
        composeRule.onNodeWithText("导出").assertIsDisplayed()
    }
}
