package com.skeidno.networkmanager.data

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.File

/**
 * 档案隔离的「形状」测试。
 *
 * 这里只测纯函数（路径推导、偏好文件名、名字归一），因为它们是隔离能否成立的根：
 * 只要默认档案与其它档案的落点不重叠、偏好文件名不撞，档案之间就不可能互相看见对方的数据。
 * ProfileStore 的增删改查要走 SharedPreferences，放模拟器上实测。
 */
class ProfilePathsTest {

    private val filesBase = File("/data/user/0/com.skeidno.networkmanager/files")
    private val workingBase = File("/storage/emulated/0/Android/data/pkg/files")
    private val tempBase = File("/data/user/0/com.skeidno.networkmanager/cache")

    private fun paths(id: String) = profilePaths(filesBase, workingBase, tempBase, id)

    @Test
    fun `默认档案沿用老路径，升级不需要搬数据`() {
        val paths = paths(DEFAULT_PROFILE_ID)
        assertEquals(filesBase, paths.dataRoot)
        assertEquals(workingBase, paths.workingRoot)
        assertEquals(tempBase, paths.tempRoot)
    }

    @Test
    fun `其它档案落在 profiles 子目录下`() {
        val paths = paths("a1b2c3d4e5f6")
        assertEquals(File(filesBase, "profiles/a1b2c3d4e5f6"), paths.dataRoot)
        assertEquals(File(workingBase, "profiles/a1b2c3d4e5f6"), paths.workingRoot)
        assertEquals(File(tempBase, "profiles/a1b2c3d4e5f6"), paths.tempRoot)
    }

    @Test
    fun `默认档案与普通档案的落点不重叠`() {
        val normal = paths("a1b2c3d4e5f6")
        val default = paths(DEFAULT_PROFILE_ID)
        // profiles/ 是普通档案的家，它必须待在默认档案根目录里面 —— 只要不相等，
        // 默认档案就不会被当成普通档案覆盖掉。
        assertNotEquals(default.dataRoot, normal.dataRoot)
        assertTrue(normal.dataRoot.path.startsWith(default.dataRoot.path))
    }

    @Test
    fun `默认档案沿用老偏好文件名，其余按 id 分开`() {
        assertEquals("network-manager", profilePrefsName(DEFAULT_PROFILE_ID))
        assertEquals("network-manager:a1b2c3d4e5f6", profilePrefsName("a1b2c3d4e5f6"))
        assertNotEquals(
            profilePrefsName(DEFAULT_PROFILE_ID),
            profilePrefsName("a1b2c3d4e5f6"),
        )
    }

    @Test
    fun `档案名归一：去空白、合并空格、截断`() {
        assertEquals("工作", sanitizeProfileName("  工作  "))
        assertEquals("工作 账号 A", sanitizeProfileName("工作   账号\tA"))
        assertEquals(PROFILE_NAME_MAX_LENGTH, sanitizeProfileName("x".repeat(80)).length)
    }

    @Test
    fun `档案名为空或重复时给出可读原因`() {
        assertEquals("档案名称不能为空", profileNameError("   ", emptyList()))
        assertEquals("已有同名档案：工作", profileNameError("工作", listOf("工作")))
        // 大小写不算区分：两个「WORK」在列表里看起来一模一样，必须挡住。
        assertEquals("已有同名档案：Work", profileNameError("work", listOf("Work")))
        assertNull(profileNameError("家里", listOf("工作", "机场 A")))
    }

    @Test
    fun `生成的档案 id 只含十六进制字符且互不相同`() {
        val ids = List(200) { newProfileId() }
        assertEquals(200, ids.toSet().size)
        ids.forEach { id ->
            assertEquals(12, id.length)
            assertTrue("id 里出现了不适合做路径的字符：$id", id.all { it in '0'..'9' || it in 'a'..'f' })
        }
    }

    @Test
    fun `App 名单归一：去空、去重、排序、剔除自身`() {
        assertEquals(
            listOf("a.b", "c.d"),
            sanitizePackages(listOf("c.d", "a.b", "c.d")),
        )
        assertEquals(
            listOf("a.b"),
            sanitizePackages(listOf("  a.b  ", "", "   ")),
        )
        // 自身包名必须剔掉：Android 的 addAllowedApplication 不接受 VPN 应用自己，
        // 混进去会直接抛异常，TUN 就建不起来了。
        assertEquals(
            listOf("a.b"),
            sanitizePackages(listOf("a.b", "com.skeidno.networkmanager"), selfPackage = "com.skeidno.networkmanager"),
        )
        assertTrue(sanitizePackages(emptyList()).isEmpty())
    }
}
