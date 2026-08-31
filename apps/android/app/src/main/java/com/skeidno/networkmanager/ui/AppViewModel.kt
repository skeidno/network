package com.skeidno.networkmanager.ui

import android.app.Application
import android.content.Intent
import android.net.TrafficStats
import android.net.Uri
import android.os.Process
import androidx.core.content.ContextCompat
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.viewModelScope
import com.skeidno.networkmanager.data.AppRepository
import com.skeidno.networkmanager.data.FallbackTarget
import com.skeidno.networkmanager.data.RoutingMode
import com.skeidno.networkmanager.vpn.NetworkVpnService
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class AppViewModel(application: Application) : AndroidViewModel(application) {
    private val repository = AppRepository.get(application)
    val state = repository.state

    private val mutableMessages = MutableSharedFlow<String>(extraBufferCapacity = 4)
    val messages = mutableMessages.asSharedFlow()

    init {
        viewModelScope.launch { sampleTraffic() }
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

    fun setCommonRuleTarget(target: FallbackTarget) {
        repository.setCommonRuleTarget(target)
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

    fun deleteErrorNodes() {
        val removed = repository.deleteErrorNodes()
        if (removed == 0) return
        applyConfigurationIfRunning()
        viewModelScope.launch {
            mutableMessages.emit("已删除 $removed 个 Error 节点；刷新原订阅可恢复")
        }
    }

    fun importText(content: String) {
        viewModelScope.launch {
            runCatching { withContext(Dispatchers.Default) { repository.importText(content) } }
                .onSuccess { mutableMessages.emit("已导入 $it 个节点") }
                .onFailure { mutableMessages.emit(it.message ?: "导入失败") }
        }
    }

    fun addSubscription(name: String, url: String) {
        viewModelScope.launch {
            runCatching { repository.addSubscription(name, url) }
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
                        input.readNBytes(10 * 1024 * 1024 + 1)
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
