package com.skeidno.networkmanager.ui

import android.app.Application
import android.content.Intent
import android.content.pm.PackageManager
import android.net.TrafficStats
import android.net.Uri
import android.os.Process
import androidx.core.content.ContextCompat
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.skeidno.networkmanager.NetworkManagerApplication
import com.skeidno.networkmanager.data.AppRepository
import com.skeidno.networkmanager.data.ExitCheckStatus
import com.skeidno.networkmanager.data.FallbackTarget
import com.skeidno.networkmanager.data.InstalledApp
import com.skeidno.networkmanager.data.LOCAL_CERT_LEAF_VALIDITY_DAYS
import com.skeidno.networkmanager.data.LocalCertStore
import com.skeidno.networkmanager.data.ProfileStore
import com.skeidno.networkmanager.data.ProfileSummary
import com.skeidno.networkmanager.data.RoutingMode
import com.skeidno.networkmanager.data.localDnsEntriesFromText
import com.skeidno.networkmanager.vpn.NetworkVpnService
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.flow.distinctUntilChanged
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import java.io.InputStream

/** 切换档案时等内核停机的上限。内核停不下来就不换根，免得读到串档的数据。 */
private const val PROFILE_STOP_TIMEOUT_MS = 15_000L

private fun InputStream.readAtMost(limit: Int): ByteArray {
    val output = ByteArrayOutputStream(minOf(limit, 8 * 1024))
    val buffer = ByteArray(8 * 1024)
    while (output.size() < limit) {
        val count = read(buffer, 0, minOf(buffer.size, limit - output.size()))
        if (count < 0) break
        output.write(buffer, 0, count)
    }
    return output.toByteArray()
}

class AppViewModel(application: Application) : AndroidViewModel(application) {
    private val repository = AppRepository.get(application)
    val state = repository.state

    private val profileStore = ProfileStore.get(application)
    private val mutableProfiles = MutableStateFlow<List<ProfileSummary>>(emptyList())
    val profiles = mutableProfiles.asStateFlow()
    private val mutableActiveProfileId = MutableStateFlow(repository.activeProfileId())
    val activeProfileId = mutableActiveProfileId.asStateFlow()

    private val mutableMessages = MutableSharedFlow<String>(extraBufferCapacity = 4)
    val messages = mutableMessages.asSharedFlow()
    private val mutableInstalledApps = MutableStateFlow<List<InstalledApp>>(emptyList())
    val installedApps = mutableInstalledApps.asStateFlow()

    init {
        viewModelScope.launch(Dispatchers.Default) { sampleTraffic() }
        viewModelScope.launch(Dispatchers.Default) {
            mutableInstalledApps.value = loadLaunchableApps()
        }
        refreshLocalCert()
        refreshProfiles()
    }

    // -----------------------------------------------------------------------
    // 用户档案（数据隔离）
    // -----------------------------------------------------------------------

    /** 重新算一遍档案列表（节点数、目录这些要读各档案自己的偏好，放 IO）。 */
    fun refreshProfiles() {
        viewModelScope.launch {
            runCatching { withContext(Dispatchers.IO) { profileStore.summaries() } }
                .onSuccess { mutableProfiles.value = it }
        }
    }

    fun createProfile(name: String, packages: List<String> = emptyList()) {
        runProfileAction {
            val created = profileStore.create(name, packages)
            val scope = if (created.packages.isEmpty()) {
                "接管全部 App"
            } else {
                "只接管 ${created.packages.size} 个 App"
            }
            "已新建档案「${created.name}」（$scope），从空白配置开始"
        }
    }

    /**
     * 改写**当前档案**接管的 App 名单（空 = 接管全部 App）。
     *
     * 名单是 `addAllowedApplication` 级别的：TUN 接口建立那一刻就定死了，运行中改不了，
     * 所以正在接管时要提醒重启才生效（和切换档案同一个道理，只是不用换数据根）。
     */
    fun setActiveProfilePackages(packages: List<String>) {
        val running = state.value.running || state.value.busy
        runProfileAction {
            val applied = repository.setTunPackages(packages)
            val scope = if (applied.isEmpty()) "接管全部 App" else "只接管 ${applied.size} 个 App"
            "这个档案改成$scope" + if (running) "，重启接管后生效" else ""
        }
    }

    /** 分身：把当前档案整套复制成新档案（配置 + 本地证书），两边从此互不影响。 */
    fun duplicateActiveProfile() {
        runProfileAction {
            val sourceId = repository.activeProfileId()
            val sourceName = profileStore.profiles().firstOrNull { it.id == sourceId }?.name.orEmpty()
            val created = profileStore.duplicate(sourceId, "$sourceName 副本")
            "已分身出「${created.name}」，配置与本地证书各一份"
        }
    }

    fun renameProfile(id: String, name: String) {
        runProfileAction {
            profileStore.rename(id, name)
            val renamed = profileStore.profiles().firstOrNull { it.id == id }?.name.orEmpty()
            "档案已重命名为「$renamed」"
        }
    }

    fun deleteProfile(id: String) {
        runProfileAction {
            val name = profileStore.profiles().firstOrNull { it.id == id }?.name.orEmpty()
            profileStore.delete(id)
            "档案「$name」已删除（它的配置目录与证书一起清掉了）"
        }
    }

    /**
     * 切换档案。
     *
     * 顺序是有讲究的：接管中先停机并**等它真的停**（running / busy 都落下去），
     * 再换数据根与偏好，然后重新 setup libbox 的路径，最后刷新证书状态。
     * 少了哪一步都会读到「一半新档案一半旧档案」的东西。
     */
    fun switchProfile(id: String) {
        if (id == repository.activeProfileId()) return
        viewModelScope.launch {
            if (state.value.running || state.value.busy) {
                mutableMessages.emit("切换档案要先停止接管，正在停止…")
                stopVpn()
                val deadline = System.currentTimeMillis() + PROFILE_STOP_TIMEOUT_MS
                while ((state.value.running || state.value.busy) && System.currentTimeMillis() < deadline) {
                    delay(150)
                }
                if (state.value.running || state.value.busy) {
                    mutableMessages.emit("内核还没停下来，稍后再试切换")
                    return@launch
                }
            }
            val targetName = mutableProfiles.value.firstOrNull { it.id == id }?.name ?: "新档案"
            val switched = withContext(Dispatchers.IO) {
                repository.switchProfile(id)
                repository.activeProfileId()
            }
            if (switched != id) {
                mutableMessages.emit("档案不存在或已被删除")
                refreshProfiles()
                return@launch
            }
            mutableActiveProfileId.value = switched
            val libbox = NetworkManagerApplication.setupLibbox(getApplication())
            refreshLocalCert()
            refreshProfiles()
            mutableMessages.emit(
                if (libbox.isFailure) {
                    "已切到「$targetName」，但内核路径没初始化成功——重启一次 App 再启动接管"
                } else {
                    "已切换到档案「$targetName」，数据目录与内核缓存都换成了它自己那份"
                },
            )
        }
    }

    private fun runProfileAction(block: () -> String) {
        viewModelScope.launch {
            runCatching { withContext(Dispatchers.IO) { block() } }
                .onSuccess {
                    mutableMessages.emit(it)
                    refreshProfiles()
                }
                .onFailure { mutableMessages.emit(it.message ?: "档案操作失败") }
        }
    }

    fun setMode(mode: RoutingMode) {
        repository.setMode(mode)
        applyConfigurationIfRunning()
    }

    fun setFallback(target: FallbackTarget) {
        repository.setFallback(target)
        applyConfigurationIfRunning()
    }

    /** detour 是写进出站里的，改完必须重载内核才生效，所以这里和改分流模式一样要 apply。 */
    fun setRelay(enabled: Boolean, nodeId: String): Boolean = runCatching {
        repository.setRelay(enabled, nodeId)
    }.fold(
        onSuccess = {
            applyConfigurationIfRunning()
            true
        },
        onFailure = {
            mutableMessages.tryEmit(it.message ?: "统一中转设置失败")
            false
        },
    )

    fun setRuleGroupEnabled(enabled: Boolean) {
        repository.setRuleGroupEnabled(enabled)
        applyConfigurationIfRunning()
    }

    fun setRuleGroupDomains(values: List<String>): Boolean = runCatching {
        repository.setRuleGroupDomains(values)
    }.fold(
        onSuccess = { count ->
            mutableMessages.tryEmit("已保存 $count 条常用站点规则")
            applyConfigurationIfRunning()
            true
        },
        onFailure = {
            mutableMessages.tryEmit(it.message ?: "匹配内容保存失败")
            false
        },
    )

    fun setCommonRuleTarget(target: FallbackTarget) {
        repository.setCommonRuleTarget(target)
        applyConfigurationIfRunning()
    }

    fun setLocalProxyEnabled(enabled: Boolean) {
        repository.setLocalProxyEnabled(enabled)
        applyConfigurationIfRunning()
    }

    fun setLocalProxyPort(port: Int): Boolean = runCatching { repository.setLocalProxyPort(port) }
        .fold(
            onSuccess = {
                applyConfigurationIfRunning()
                true
            },
            onFailure = {
                mutableMessages.tryEmit(it.message ?: "端口保存失败")
                false
            },
        )

    /**
     * 保存本地 DNS 映射（自定义域名 → IP）。
     *
     * 有非法行时 [localDnsEntriesFromText] 会抛异常，这里翻成提示语并把编辑框留着
     * 不给关——半截配置比没配置更难查。
     */
    fun setLocalDnsText(text: String): Boolean = runCatching {
        repository.setLocalDnsFromText(text)
    }.fold(
        onSuccess = { count ->
            mutableMessages.tryEmit(
                if (count == 0) "本地 DNS 映射已清空" else "已保存 $count 条本地 DNS 映射",
            )
            applyConfigurationIfRunning()
            // 映射一改，叶证书该覆盖的名字就变了，顺手同步到位 —— 否则新加的域名
            // 一访问就报「名字对不上」。清空映射时不动证书：那多半只是编辑中途，
            // 而且用户可能正拿这套证书跑本地服务。
            if (count > 0) syncLocalCerts()
            true
        },
        onFailure = {
            mutableMessages.tryEmit(it.message ?: "本地 DNS 映射保存失败")
            false
        },
    )

    /** 刷新证书状态快照（不生成），让卡片显示的和磁盘一致。 */
    fun refreshLocalCert() {
        val entries = state.value.localDnsEntries
        viewModelScope.launch(Dispatchers.IO) {
            runCatching { LocalCertStore(getApplication()).status(entries) }
                .onSuccess { repository.setLocalCertStatus(it) }
        }
    }

    /** 生成或重签证书。映射为空时 [LocalCertStore.ensure] 会给出可读的错误。 */
    fun generateLocalCerts() {
        val entries = state.value.localDnsEntries
        viewModelScope.launch(Dispatchers.IO) {
            runCatching { LocalCertStore(getApplication()).ensure(entries) }
                .onSuccess { status ->
                    repository.setLocalCertStatus(status)
                    mutableMessages.emit(
                        "已生成根证书与叶证书，覆盖 ${status.names.size} 个名字；" +
                            "叶证书有效期 $LOCAL_CERT_LEAF_VALIDITY_DAYS 天",
                    )
                }
                .onFailure { mutableMessages.emit(it.message ?: "证书生成失败") }
        }
    }

    /** 保存映射后同步证书：在独立协程里做，EC 密钥生成别卡住界面。 */
    private fun syncLocalCerts() {
        val entries = state.value.localDnsEntries
        viewModelScope.launch(Dispatchers.IO) {
            runCatching { LocalCertStore(getApplication()).ensure(entries) }
                .onSuccess { status ->
                    repository.setLocalCertStatus(status)
                    if (status.namesInSync) {
                        mutableMessages.emit("证书已同步，覆盖 ${status.names.size} 个名字")
                    }
                }
                .onFailure { mutableMessages.emit("本地 DNS 已保存，但证书更新失败：${it.message}") }
        }
    }

    /** 把根证书装进系统信任库（root 路线）。 */
    fun installLocalCaWithRoot() {
        viewModelScope.launch(Dispatchers.IO) {
            runCatching { LocalCertStore(getApplication()).installWithRoot() }
                .onSuccess { mutableMessages.emit(it) }
                .onFailure { mutableMessages.emit(it.message ?: "root 安装失败") }
        }
    }

    /** 把根证书导出到下载目录，方便拿到别的设备 / 电脑上装。 */
    fun exportLocalCa() {
        viewModelScope.launch(Dispatchers.IO) {
            runCatching { LocalCertStore(getApplication()).exportCa() }
                .onSuccess { mutableMessages.emit(it) }
                .onFailure { mutableMessages.emit(it.message ?: "导出失败") }
        }
    }

    /** 系统安装 CA 对话框的意图；返回 null 表示还没有根证书（提示已在消息里）。 */
    fun localCaInstallIntent(): Intent? = runCatching {
        LocalCertStore(getApplication()).installIntent()
    }.onFailure { mutableMessages.tryEmit(it.message ?: "无法安装 CA") }.getOrNull()

    fun savePortableRules(
        index: Int?,
        type: String,
        values: List<String>,
        target: FallbackTarget,
    ): Boolean = runCatching {
        repository.savePortableRules(index, type, values, target)
    }.fold(
        onSuccess = { count ->
            mutableMessages.tryEmit("已保存 $count 条规则")
            applyConfigurationIfRunning()
            true
        },
        onFailure = {
            mutableMessages.tryEmit(it.message ?: "规则保存失败")
            false
        },
    )

    fun deletePortableRule(index: Int) {
        repository.deletePortableRule(index)
        applyConfigurationIfRunning()
    }

    fun selectNode(id: String) {
        repository.selectNode(id)
        applyConfigurationIfRunning()
    }

    fun deleteNode(id: String) {
        repository.deleteNode(id)
        applyConfigurationIfRunning()
    }

    fun createNodeGroup(name: String) {
        runCatching { repository.createNodeGroup(name) }
            .onFailure { mutableMessages.tryEmit(it.message ?: "创建分组失败") }
    }

    fun assignNodeGroup(id: String, name: String) {
        runCatching { repository.assignNodeGroup(id, name) }
            .onFailure { mutableMessages.tryEmit(it.message ?: "更新分组失败") }
    }

    fun deleteNodeGroup(name: String) = repository.deleteNodeGroup(name)

    fun deleteErrorNodes() {
        val removed = repository.deleteErrorNodes()
        if (removed == 0) return
        applyConfigurationIfRunning()
        viewModelScope.launch {
            mutableMessages.emit("已删除 $removed 个 Error 节点；刷新原订阅可恢复")
        }
    }

    fun importText(content: String, group: String = "") {
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.Default) { repository.importText(content, group = group) }
            }
                .onSuccess { mutableMessages.emit("已导入 $it 个节点") }
                .onFailure { mutableMessages.emit(it.message ?: "导入失败") }
        }
    }

    fun addSubscription(name: String, url: String, group: String = "") {
        viewModelScope.launch {
            runCatching { repository.addSubscription(name, url, group) }
                .onSuccess { mutableMessages.emit("订阅更新完成，共 $it 个节点") }
                .onFailure { mutableMessages.emit(it.message ?: "订阅导入失败") }
        }
    }

    fun refreshSubscription(id: String) {
        viewModelScope.launch {
            runCatching { repository.refreshSubscription(id) }
                .onSuccess { mutableMessages.emit("订阅已刷新，共 $it 个节点") }
                .onFailure { mutableMessages.emit(it.message ?: "订阅刷新失败") }
        }
    }

    fun deleteSubscription(id: String) = repository.deleteSubscription(id)

    fun testAllNodes() {
        viewModelScope.launch {
            repository.testAllNodes()
            val available = state.value.nodes.count { it.latencyMs != null }
            mutableMessages.emit("批量入口检测完成，$available 个节点端口可连接")
        }
    }

    fun testNode(id: String) {
        viewModelScope.launch {
            runCatching { repository.testNode(id) }
                .onSuccess { node ->
                    val result = node.latencyMs?.let { "入口 ${it} ms" } ?: "入口检测失败"
                    mutableMessages.emit("${node.name}：$result")
                }
                .onFailure { mutableMessages.emit(it.message ?: "节点入口检测失败") }
        }
    }

    fun testExit() {
        if (state.value.exitStatus == ExitCheckStatus.Checking) return
        viewModelScope.launch {
            runCatching { repository.checkExit() }
                .onFailure { mutableMessages.emit(it.message ?: "出口检测失败") }
            state.value.exitMessage.takeIf { it.isNotBlank() }?.let { mutableMessages.emit(it) }
        }
    }

    fun prepareForStart(onPrepared: () -> Unit) {
        if (state.value.busy) return
        viewModelScope.launch {
            runCatching { withContext(Dispatchers.IO) { repository.writeRuntimeConfig() } }
                .onSuccess { onPrepared() }
                .onFailure { mutableMessages.emit(it.message ?: "配置生成失败") }
        }
    }

    fun exportConfiguration(uri: Uri) {
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    val resolver = getApplication<Application>().contentResolver
                    resolver.openOutputStream(uri, "wt")?.bufferedWriter()?.use { writer ->
                        writer.write(repository.exportPortableConfig())
                        writer.newLine()
                    } ?: error("无法写入所选文件")
                }
            }.onSuccess {
                mutableMessages.emit("跨设备配置已导出")
            }.onFailure {
                mutableMessages.emit(it.message ?: "配置导出失败")
            }
        }
    }

    fun importConfiguration(uri: Uri) {
        viewModelScope.launch {
            runCatching {
                withContext(Dispatchers.IO) {
                    val resolver = getApplication<Application>().contentResolver
                    val bytes = resolver.openInputStream(uri)?.buffered()?.use { input ->
                        input.readAtMost(10 * 1024 * 1024 + 1)
                    } ?: error("无法读取所选文件")
                    require(bytes.size <= 10 * 1024 * 1024) { "配置文件不能超过 10 MB" }
                    repository.importPortableConfig(bytes.toString(Charsets.UTF_8))
                }
            }.onSuccess { count ->
                mutableMessages.emit("跨设备配置已导入，共 $count 个节点")
                applyConfigurationIfRunning()
            }.onFailure {
                mutableMessages.emit(it.message ?: "配置导入失败")
            }
        }
    }

    fun stopVpn() {
        val context = getApplication<Application>()
        context.startService(
            Intent(context, NetworkVpnService::class.java).setAction(NetworkVpnService.ACTION_STOP),
        )
    }

    private fun applyConfigurationIfRunning() {
        if (!state.value.running) return
        viewModelScope.launch(Dispatchers.IO) {
            runCatching {
                repository.writeRuntimeConfig()
                val context = getApplication<Application>()
                ContextCompat.startForegroundService(
                    context,
                    Intent(context, NetworkVpnService::class.java).setAction(NetworkVpnService.ACTION_RELOAD),
                )
            }.onFailure { mutableMessages.emit(it.message ?: "配置应用失败") }
        }
    }

    private fun loadLaunchableApps(): List<InstalledApp> {
        val application = getApplication<Application>()
        val packageManager = application.packageManager
        val intent = Intent(Intent.ACTION_MAIN).addCategory(Intent.CATEGORY_LAUNCHER)
        return packageManager.queryIntentActivities(intent, PackageManager.MATCH_ALL)
            .map { resolveInfo ->
                InstalledApp(
                    label = resolveInfo.loadLabel(packageManager).toString()
                        .ifBlank { resolveInfo.activityInfo.packageName },
                    packageName = resolveInfo.activityInfo.packageName,
                )
            }
            .filterNot { it.packageName == application.packageName }
            .distinctBy(InstalledApp::packageName)
            .sortedBy { it.label.lowercase() }
    }

    private suspend fun sampleTraffic() {
        state.map { it.running }.distinctUntilChanged().collectLatest { running ->
            if (!running) {
                val current = state.value
                if (current.downloadBytesPerSecond != 0L || current.uploadBytesPerSecond != 0L) {
                    repository.updateTraffic(
                        download = 0,
                        upload = 0,
                        totalDownload = current.totalDownloadBytes,
                        totalUpload = current.totalUploadBytes,
                    )
                }
                return@collectLatest
            }

            var lastRx = TrafficStats.getUidRxBytes(Process.myUid()).coerceAtLeast(0)
            var lastTx = TrafficStats.getUidTxBytes(Process.myUid()).coerceAtLeast(0)
            while (true) {
                delay(1_000)
                val rx = TrafficStats.getUidRxBytes(Process.myUid()).coerceAtLeast(0)
                val tx = TrafficStats.getUidTxBytes(Process.myUid()).coerceAtLeast(0)
                val current = state.value
                if (!current.running) return@collectLatest
                val down = (rx - lastRx).coerceAtLeast(0)
                val up = (tx - lastTx).coerceAtLeast(0)
                val chartSettled = current.downloadSamples.all { it == 0L } &&
                    current.uploadSamples.all { it == 0L }
                if (
                    down != 0L || up != 0L || !chartSettled ||
                    current.downloadBytesPerSecond != 0L || current.uploadBytesPerSecond != 0L
                ) {
                    repository.updateTraffic(
                        download = down,
                        upload = up,
                        totalDownload = current.totalDownloadBytes + down,
                        totalUpload = current.totalUploadBytes + up,
                    )
                }
                lastRx = rx
                lastTx = tx
            }
        }
    }
}
