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
import com.skeidno.networkmanager.data.AppRepository
import com.skeidno.networkmanager.data.FallbackTarget
import com.skeidno.networkmanager.data.InstalledApp
import com.skeidno.networkmanager.data.RoutingMode
import com.skeidno.networkmanager.vpn.NetworkVpnService
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import java.io.ByteArrayOutputStream
import java.io.InputStream

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

    private val mutableMessages = MutableSharedFlow<String>(extraBufferCapacity = 4)
    val messages = mutableMessages.asSharedFlow()
    private val mutableInstalledApps = MutableStateFlow<List<InstalledApp>>(emptyList())
    val installedApps = mutableInstalledApps.asStateFlow()

    init {
        viewModelScope.launch { sampleTraffic() }
        viewModelScope.launch(Dispatchers.Default) {
            mutableInstalledApps.value = loadLaunchableApps()
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
            mutableMessages.emit("批量测速完成，$available 个节点可连接")
        }
    }

    fun testNode(id: String) {
        viewModelScope.launch {
            runCatching { repository.testNode(id) }
                .onSuccess { node ->
                    val result = node.latencyMs?.let { "${it} ms" } ?: "测速失败"
                    mutableMessages.emit("${node.name}：$result")
                }
                .onFailure { mutableMessages.emit(it.message ?: "节点测速失败") }
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
        var lastRx = TrafficStats.getUidRxBytes(Process.myUid()).coerceAtLeast(0)
        var lastTx = TrafficStats.getUidTxBytes(Process.myUid()).coerceAtLeast(0)
        while (true) {
            delay(1_000)
            val rx = TrafficStats.getUidRxBytes(Process.myUid()).coerceAtLeast(0)
            val tx = TrafficStats.getUidTxBytes(Process.myUid()).coerceAtLeast(0)
            val current = state.value
            val down = if (current.running) (rx - lastRx).coerceAtLeast(0) else 0
            val up = if (current.running) (tx - lastTx).coerceAtLeast(0) else 0
            repository.updateTraffic(
                download = down,
                upload = up,
                totalDownload = current.totalDownloadBytes + down,
                totalUpload = current.totalUploadBytes + up,
            )
            lastRx = rx
            lastTx = tx
        }
    }
}
