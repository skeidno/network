package com.skeidno.networkmanager.data

import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONObject
import java.io.File
import java.time.Instant
import java.util.UUID

/**
 * 用户档案（数据隔离）。
 *
 * 想解决的问题：同一个 App 里要放好几套互不相干的配置（多个机场账号、工作 / 家里两套规则），
 * 以前只能靠多开 App —— 手机上装两份包，或者电脑上开好几个模拟器实例，切换还费资源。
 *
 * 这里按浏览器的 userdir 思路做：**每个档案是一整套独立的用户数据**，落在自己的文件夹里，
 * 档案之间零共享。一个档案包含：
 *
 * - 全部配置（节点、订阅、分组、分流规则、模式、本地代理端口、本地 DNS 映射）—— 各自一份偏好文件；
 * - 本地 CA 与叶证书（`local-certs/`）—— 各一份，所以 A 档案装进系统信任库的 CA 不影响 B；
 * - 内核运行数据（`runtime.json` 与 sing-box 的 working / temp 目录，geoip、cache 等缓存）—— 各一份。
 *
 * [DEFAULT_PROFILE_ID] 这个档案**刻意复用升级前的老路径**（`filesDir` 根 + 老的偏好文件名），
 * 这样老用户升级上来时数据原地不动：不需要搬迁文件，已经装进系统信任库的 CA 依然有效。
 * 其余档案放在 `profiles/<id>/` 下。
 */

/** 默认档案的 id。它对应升级前的老目录与老偏好文件，不做迁移。 */
const val DEFAULT_PROFILE_ID = "default"

const val DEFAULT_PROFILE_NAME = "默认"

/** 档案数量上限：够用，也不至于让「档案」列表变成第二个节点列表。 */
const val MAX_PROFILES = 8

/** 档案显示名长度上限。 */
const val PROFILE_NAME_MAX_LENGTH = 24

private const val ROOT_PREFS = "network-manager-profiles"
private const val PROFILES_KEY = "profiles"
private const val ACTIVE_KEY = "active"
private const val PROFILES_DIR = "profiles"

/** 档案内部的固定文件名：与 AppRepository / LocalCertStore 共用。 */
const val PROFILE_RUNTIME_FILE = "runtime.json"
const val PROFILE_CERT_DIR = "local-certs"

private val whitespacePattern = Regex("""\s+""")

data class Profile(
    val id: String,
    val name: String,
    val createdAt: String,
    val lastUsedAt: String,
    /**
     * 这个档案接管的 App（包名）。**空 = 接管全部 App**（老行为）。
     *
     * 之所以把名单挂在档案上而不是做成全局设置：一台手机同一时刻只能有一个 VPN，
     * 所以「机场 A 只管 Chrome、机场 B 只管银行 App」这种用法本质上就是**切档案时
     * 连被接管的 App 一起换**。名单放在档案里，切过去就自动是对的。
     */
    val packages: List<String> = emptyList(),
)

/**
 * App 名单归一：去掉空白、自身包名与重复项，排序后返回。
 *
 * 自身包名必须剔除 —— Android 的 `addAllowedApplication` 不接受 VPN 应用自己，
 * 名单里混进它会直接抛异常。
 */
fun sanitizePackages(values: Collection<String>, selfPackage: String = ""): List<String> =
    values.map(String::trim)
        .filter { it.isNotEmpty() && it != selfPackage }
        .distinct()
        .sorted()

/**
 * 一个档案在磁盘上的三处落点，对应 libbox 的 basePath / workingPath / tempPath。
 *
 * 做成纯数据 + 纯函数（见 [profilePaths]），盘上的布局才能在单测里直接断言，
 * 不用把 Android 的 Context 拖进测试。
 */
data class ProfilePaths(
    val dataRoot: File,
    val workingRoot: File,
    val tempRoot: File,
)

/** 档案列表里显示的一行：配置摘要 + 数据目录，让「隔离」看得见。 */
data class ProfileSummary(
    val id: String,
    val name: String,
    val active: Boolean,
    val nodeCount: Int,
    val subscriptionCount: Int,
    val ruleCount: Int,
    val dataDir: String,
    val lastUsedAt: String,
    /** 接管的 App 包名；空表示接管全部 App。 */
    val packages: List<String> = emptyList(),
)

/**
 * 每个档案一份偏好文件。
 *
 * 默认档案沿用老名字 `network-manager`——这是「老用户数据原地不动」的另一半：
 * 路径没搬，偏好文件也不能换名字。
 */
fun profilePrefsName(id: String): String =
    if (id == DEFAULT_PROFILE_ID) "network-manager" else "network-manager:$id"

/**
 * 档案落点。默认档案直接用老的三处根目录，其余走 `profiles/<id>/` 子目录。
 *
 * 依赖注入式的签名（自己传三个根目录）是为了单测能拿临时目录直接跑。
 */
fun profilePaths(
    dataBase: File,
    workingBase: File,
    tempBase: File,
    id: String,
): ProfilePaths = if (id == DEFAULT_PROFILE_ID) {
    ProfilePaths(dataRoot = dataBase, workingRoot = workingBase, tempRoot = tempBase)
} else {
    ProfilePaths(
        dataRoot = File(File(dataBase, PROFILES_DIR), id),
        workingRoot = File(File(workingBase, PROFILES_DIR), id),
        tempRoot = File(File(tempBase, PROFILES_DIR), id),
    )
}

/** 只做「归一」，不判空——名字为空由 [profileNameError] 负责给用户看的话。 */
fun sanitizeProfileName(raw: String): String =
    raw.trim().replace(whitespacePattern, " ").take(PROFILE_NAME_MAX_LENGTH)

/** 返回 null 表示名字可用，否则是直接显示给用户的原因。 */
fun profileNameError(raw: String, existingNames: Collection<String>): String? {
    val name = sanitizeProfileName(raw)
    if (name.isEmpty()) return "档案名称不能为空"
    // 重名提示回显**列表里真实那个名字**：用户输的是 "work"、列表里写着 "Work" 时，
    // 把 "Work" 摆出来才对得上他屏幕上看到的东西。
    val clash = existingNames.firstOrNull { it.equals(name, ignoreCase = true) }
    return clash?.let { "已有同名档案：$it" }
}

/** 档案目录名 / 偏好文件名都要拼进路径，所以只允许十六进制字符。 */
fun newProfileId(): String = UUID.randomUUID().toString().replace("-", "").take(12)

/** 当前生效档案的 id。 */
fun Context.activeProfileId(): String = ProfileStore.get(this).activeId()

/** 当前生效档案（或指定档案）在磁盘上的三处落点。 */
fun Context.profilePaths(id: String = activeProfileId()): ProfilePaths = profilePaths(
    dataBase = this.filesDir,
    workingBase = getExternalFilesDir(null) ?: this.filesDir,
    tempBase = this.cacheDir,
    id = id,
)

/**
 * 档案的增删改查。
 *
 * 档案清单本身存在一个**不属于任何档案**的根偏好里（[ROOT_PREFS]）——否则「当前是哪个档案」
 * 这个信息会跟着档案被一起切走，切完就不知道自己是谁了。
 *
 * 全进程只有一个实例（[get]）：`Context.profilePaths()` 这类高频调用会一路走到
 * [activeId]，每次新建实例既浪费又会把清单解析重复一遍。清单也因此在实例内缓存，
 * 只有走本类的写方法才会更新——所有写入都在这里，缓存不会过期。
 */
class ProfileStore private constructor(context: Context) {
    private val appContext = context.applicationContext
    private val preferences: SharedPreferences =
        appContext.getSharedPreferences(ROOT_PREFS, Context.MODE_PRIVATE)

    /** 解析过的档案清单。null 表示还没读过。 */
    @Volatile
    private var cachedProfiles: List<Profile>? = null

    init {
        synchronized(lock) { ensureDefaultProfile() }
    }

    fun profiles(): List<Profile> = synchronized(lock) { readProfiles() }

    /** 当前档案 id。指向的档案被删掉了就退回第一个，保证永远有一个可用档案。 */
    fun activeId(): String = synchronized(lock) {
        val list = readProfiles()
        val stored = preferences.getString(ACTIVE_KEY, null)
        stored?.takeIf { id -> list.any { it.id == id } } ?: list.first().id
    }

    fun setActive(id: String): Boolean = synchronized(lock) {
        val list = readProfiles()
        if (list.none { it.id == id }) return false
        preferences.edit().putString(ACTIVE_KEY, id).apply()
        writeProfiles(list.map { if (it.id == id) it.copy(lastUsedAt = Instant.now().toString()) else it })
        true
    }

    fun create(rawName: String, packages: Collection<String> = emptyList()): Profile = synchronized(lock) {
        val list = readProfiles().toMutableList()
        check(list.size < MAX_PROFILES) { "最多只能有 $MAX_PROFILES 个档案" }
        val now = Instant.now().toString()
        val profile = Profile(
            id = newProfileId(),
            name = uniqueName(rawName, list.map(Profile::name)),
            createdAt = now,
            lastUsedAt = now,
            packages = sanitizePackages(packages, appContext.packageName),
        )
        // 目录先建出来：档案卡片要显示路径，空档案也该有个真实存在的目录。
        pathsOf(profile.id).dataRoot.mkdirs()
        writeProfiles(list + profile)
        profile
    }

    /**
     * 分身：把源档案整个复制成一个新档案（配置 + 本地证书 + 内核配置）。
     *
     * 只拷「属于档案的东西」——cache 之类的目录不拷：那部分要么能重建，要么拷了反而是垃圾。
     * 接管的 App 名单也一并继承：分身的语义就是「和源档案一模一样，只是换套账号」。
     */
    fun duplicate(sourceId: String, rawName: String): Profile = synchronized(lock) {
        val list = readProfiles().toMutableList()
        check(list.size < MAX_PROFILES) { "最多只能有 $MAX_PROFILES 个档案" }
        val source = list.firstOrNull { it.id == sourceId } ?: error("源档案不存在")
        val now = Instant.now().toString()
        val profile = Profile(
            id = newProfileId(),
            name = uniqueName(rawName, list.map(Profile::name)),
            createdAt = now,
            lastUsedAt = now,
            packages = source.packages,
        )
        copyProfileData(sourceId, profile.id)
        writeProfiles(list + profile)
        profile
    }

    /** 改写某个档案接管的 App 名单。空列表表示回到「接管全部 App」。 */
    fun setPackages(id: String, packages: Collection<String>) = synchronized(lock) {
        val list = readProfiles()
        check(list.any { it.id == id }) { "档案不存在" }
        val sanitized = sanitizePackages(packages, appContext.packageName)
        writeProfiles(list.map { if (it.id == id) it.copy(packages = sanitized) else it })
    }

    fun rename(id: String, rawName: String) = synchronized(lock) {
        val list = readProfiles()
        check(list.any { it.id == id }) { "档案不存在" }
        val name = uniqueName(rawName, list.filterNot { it.id == id }.map(Profile::name))
        writeProfiles(list.map { if (it.id == id) it.copy(name = name) else it })
    }

    fun delete(id: String) = synchronized(lock) {
        val list = readProfiles()
        check(list.size > 1) { "至少要保留一个档案" }
        check(id != DEFAULT_PROFILE_ID) { "默认档案不能删除" }
        check(id != activeId()) { "不能删除正在使用的档案，先切到别的档案" }
        check(list.any { it.id == id }) { "档案不存在" }

        val paths = pathsOf(id)
        paths.dataRoot.deleteRecursively()
        paths.workingRoot.deleteRecursively()
        paths.tempRoot.deleteRecursively()
        appContext.deleteSharedPreferences(profilePrefsName(id))
        writeProfiles(list.filterNot { it.id == id })
    }

    /** 档案卡片要用的摘要。读的是各个档案自己的偏好文件，所以放 IO 线程调用。 */
    fun summaries(): List<ProfileSummary> {
        val active = activeId()
        return profiles().map { profile ->
            val prefs = appContext.getSharedPreferences(profilePrefsName(profile.id), Context.MODE_PRIVATE)
            ProfileSummary(
                id = profile.id,
                name = profile.name,
                active = profile.id == active,
                nodeCount = jsonArrayLength(prefs.getString("nodes", null)),
                subscriptionCount = jsonArrayLength(prefs.getString("subscriptions", null)),
                ruleCount = jsonArrayLength(prefs.getString("portableRules", null)),
                dataDir = pathsOf(profile.id).dataRoot.path,
                lastUsedAt = profile.lastUsedAt,
                packages = profile.packages,
            )
        }
    }

    private fun pathsOf(id: String): ProfilePaths = appContext.profilePaths(id)

    private fun jsonArrayLength(raw: String?): Int {
        if (raw.isNullOrBlank()) return 0
        return runCatching { JSONArray(raw).length() }.getOrDefault(0)
    }

    /** 同名时按「名字 2」「名字 3」往后排，避免两个档案看起来一模一样。 */
    private fun uniqueName(rawName: String, otherNames: Collection<String>): String {
        val base = sanitizeProfileName(rawName).ifEmpty { "新档案" }
        if (otherNames.none { it.equals(base, ignoreCase = true) }) return base
        var index = 2
        while (true) {
            val candidate = sanitizeProfileName("$base $index")
            if (otherNames.none { it.equals(candidate, ignoreCase = true) }) return candidate
            index += 1
        }
    }

    /**
     * 复制档案数据。只搬这三样：
     * - 偏好文件（全部配置）
     * - `local-certs/`（本地证书 —— 分身之后新档案的 HTTPS 也是通的）
     * - `runtime.json`（上次生成的内核配置，第一次启动接管前不至于空着）
     *
     * 刻意**不做整目录递归拷贝**：默认档案的根目录就是 `filesDir`，里面还躺着
     * `profiles/`（所有档案的家）—— 整拷会把它卷进去，越拷越大。
     */
    private fun copyProfileData(fromId: String, toId: String) {
        val source = appContext.profilePaths(fromId)
        val target = appContext.profilePaths(toId)
        target.dataRoot.mkdirs()

        val sourcePrefs = appContext.getSharedPreferences(profilePrefsName(fromId), Context.MODE_PRIVATE)
        val targetPrefs = appContext.getSharedPreferences(profilePrefsName(toId), Context.MODE_PRIVATE)
        val editor = targetPrefs.edit()
        sourcePrefs.all.forEach { (key, value) ->
            when (value) {
                is String -> editor.putString(key, value)
                is Int -> editor.putInt(key, value)
                is Long -> editor.putLong(key, value)
                is Float -> editor.putFloat(key, value)
                is Boolean -> editor.putBoolean(key, value)
                is Set<*> -> editor.putStringSet(key, value.filterIsInstance<String>().toSet())
                else -> Unit
            }
        }
        editor.apply()

        File(source.dataRoot, PROFILE_CERT_DIR)
            .takeIf(File::isDirectory)
            ?.copyRecursively(File(target.dataRoot, PROFILE_CERT_DIR), overwrite = true)
        File(source.dataRoot, PROFILE_RUNTIME_FILE)
            .takeIf(File::isFile)
            ?.copyTo(File(target.dataRoot, PROFILE_RUNTIME_FILE), overwrite = true)
    }

    private fun ensureDefaultProfile() {
        if (readProfiles().isNotEmpty()) return
        val now = Instant.now().toString()
        writeProfiles(listOf(Profile(DEFAULT_PROFILE_ID, DEFAULT_PROFILE_NAME, now, now)))
        preferences.edit().putString(ACTIVE_KEY, DEFAULT_PROFILE_ID).apply()
    }

    private fun readProfiles(): List<Profile> =
        cachedProfiles ?: parseProfiles().also { cachedProfiles = it }

    private fun parseProfiles(): List<Profile> = runCatching {
        val array = JSONArray(preferences.getString(PROFILES_KEY, null))
        List(array.length()) { index ->
            val item = array.getJSONObject(index)
            Profile(
                id = item.getString("id"),
                name = sanitizeProfileName(item.optString("name")).ifEmpty { DEFAULT_PROFILE_NAME },
                createdAt = item.optString("createdAt"),
                lastUsedAt = item.optString("lastUsedAt"),
                packages = sanitizePackages(
                    item.optJSONArray("packages")?.let { array ->
                        List(array.length()) { index -> array.optString(index) }
                    }.orEmpty(),
                    appContext.packageName,
                ),
            )
        }
    }.getOrDefault(emptyList())

    private fun writeProfiles(profiles: List<Profile>) {
        val array = JSONArray()
        profiles.forEach { profile ->
            array.put(
                JSONObject()
                    .put("id", profile.id)
                    .put("name", profile.name)
                    .put("createdAt", profile.createdAt)
                    .put("lastUsedAt", profile.lastUsedAt)
                    .put("packages", JSONArray(profile.packages)),
            )
        }
        preferences.edit().putString(PROFILES_KEY, array.toString()).apply()
        cachedProfiles = profiles
    }

    companion object {
        /** 跨实例的锁：`get` 之外的路径也可能并发进来。 */
        private val lock = Any()

        @Volatile
        private var instance: ProfileStore? = null

        fun get(context: Context): ProfileStore = instance ?: synchronized(lock) {
            instance ?: ProfileStore(context.applicationContext).also { instance = it }
        }
    }
}
