package com.skeidno.networkmanager.vpn

import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Intent
import android.content.pm.PackageManager.NameNotFoundException
import android.net.VpnService
import android.os.Build
import android.os.IBinder
import android.os.ParcelFileDescriptor
import android.util.Log
import androidx.core.app.NotificationCompat
import com.skeidno.networkmanager.MainActivity
import com.skeidno.networkmanager.R
import com.skeidno.networkmanager.data.AppRepository
import io.nekohasekai.libbox.CommandServer
import io.nekohasekai.libbox.CommandServerHandler
import io.nekohasekai.libbox.OverrideOptions
import io.nekohasekai.libbox.SystemProxyStatus
import io.nekohasekai.libbox.TunOptions
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicBoolean

class NetworkVpnService : VpnService(), CommandServerHandler {
    private val executor = Executors.newSingleThreadExecutor()
    private lateinit var repository: AppRepository
    private lateinit var platform: AndroidLibboxPlatform
    private var commandServer: CommandServer? = null
    private var tunDescriptor: ParcelFileDescriptor? = null
    private val operationPending = AtomicBoolean(false)

    override fun onCreate() {
        super.onCreate()
        repository = AppRepository.get(this)
        platform = AndroidLibboxPlatform(this)
        createNotificationChannel()
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            stopCore()
            return START_NOT_STICKY
        }
        if (intent?.action == ACTION_RELOAD && commandServer != null) {
            reloadCore()
            return START_NOT_STICKY
        }
        if (commandServer != null) {
            repository.updateRuntime(running = true, busy = false, message = "接管中")
            return START_STICKY
        }
        if (!operationPending.compareAndSet(false, true)) return START_NOT_STICKY
        startForeground(NOTIFICATION_ID, notification("正在启动"))
        repository.updateRuntime(running = false, busy = true, message = "正在启动")
        executor.execute {
            try {
                startCore()
            } finally {
                operationPending.set(false)
            }
        }
        return START_NOT_STICKY
    }

    override fun onBind(intent: Intent): IBinder? = super.onBind(intent)

    private fun startCore() {
        try {
            val config = repository.runtimeConfigFile().readText(Charsets.UTF_8)
            val server = CommandServer(this, platform)
            server.start()
            server.startOrReloadService(config, OverrideOptions())
            commandServer = server
            repository.updateRuntime(running = true, busy = false, message = "接管中")
            getSystemService(NotificationManager::class.java).notify(
                NOTIFICATION_ID,
                notification("接管中"),
            )
        } catch (error: Exception) {
            Log.e(LOG_TAG, "Unable to start VPN core", error)
            commandServer?.close()
            commandServer = null
            closeTun()
            repository.updateRuntime(
                running = false,
                busy = false,
                message = "启动失败",
                error = error.message.orEmpty(),
            )
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        }
    }

    private fun stopCore() {
        if (!operationPending.compareAndSet(false, true)) return
        repository.updateRuntime(running = false, busy = true, message = "正在停止")
        executor.execute {
            try {
                runCatching { commandServer?.closeService() }
                runCatching { commandServer?.close() }
                commandServer = null
                closeTun()
                repository.updateRuntime(running = false, busy = false, message = "已停止")
                stopForeground(STOP_FOREGROUND_REMOVE)
                stopSelf()
            } finally {
                operationPending.set(false)
            }
        }
    }

    private fun reloadCore() {
        if (!operationPending.compareAndSet(false, true)) return
        repository.updateRuntime(running = true, busy = true, message = "正在应用配置")
        executor.execute {
            try {
                serviceReload()
                repository.updateRuntime(running = true, busy = false, message = "接管中")
            } catch (error: Exception) {
                repository.updateRuntime(
                    running = true,
                    busy = false,
                    message = "配置未应用",
                    error = error.message.orEmpty(),
                )
            } finally {
                operationPending.set(false)
            }
        }
    }

    internal fun openTun(options: TunOptions): Int {
        check(prepare(this) == null) { "Android VPN 权限未授予" }
        val builder = Builder().setSession("Network Manager").setMtu(options.mtu)
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) builder.setMetered(false)

        val inet4 = options.inet4Address.toList()
        val inet6 = options.inet6Address.toList()
        (inet4 + inet6).forEach { builder.addAddress(it.address(), it.prefix()) }
        options.dnsServerAddress?.value?.takeIf(String::isNotBlank)?.let(builder::addDnsServer)

        if (options.autoRoute) {
            val route4 = options.inet4RouteRange.toList()
            val route6 = options.inet6RouteRange.toList()
            if (route4.isEmpty()) builder.addRoute("0.0.0.0", 0)
            else route4.forEach { builder.addRoute(it.address(), it.prefix()) }
            if (route6.isNotEmpty()) route6.forEach { builder.addRoute(it.address(), it.prefix()) }
        }

        options.includePackage.toList().forEach { packageName ->
            try {
                builder.addAllowedApplication(packageName)
            } catch (_: NameNotFoundException) {
            }
        }
        options.excludePackage.toList().forEach { packageName ->
            try {
                builder.addDisallowedApplication(packageName)
            } catch (_: NameNotFoundException) {
            }
        }

        val descriptor = builder.establish() ?: error("Android 未能建立 VPN 接口")
        tunDescriptor = descriptor
        return descriptor.fd
    }

    private fun closeTun() {
        runCatching { tunDescriptor?.close() }
        tunDescriptor = null
    }

    override fun serviceStop() = stopCore()

    override fun serviceReload() {
        val config = repository.runtimeConfigFile().readText(Charsets.UTF_8)
        commandServer?.startOrReloadService(config, OverrideOptions())
    }

    override fun getSystemProxyStatus(): SystemProxyStatus = SystemProxyStatus().apply {
        available = false
        enabled = false
    }

    override fun setSystemProxyEnabled(enabled: Boolean) = Unit
    override fun writeDebugMessage(message: String?) = Unit

    override fun onRevoke() {
        stopCore()
        super.onRevoke()
    }

    override fun onDestroy() {
        runCatching { commandServer?.close() }
        closeTun()
        executor.shutdownNow()
        super.onDestroy()
    }

    private fun notification(status: String): android.app.Notification {
        val pendingIntent = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return NotificationCompat.Builder(this, CHANNEL_ID)
            .setSmallIcon(R.mipmap.ic_launcher)
            .setContentTitle(getString(R.string.app_name))
            .setContentText(status)
            .setContentIntent(pendingIntent)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setCategory(NotificationCompat.CATEGORY_SERVICE)
            .build()
    }

    private fun createNotificationChannel() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        getSystemService(NotificationManager::class.java).createNotificationChannel(
            NotificationChannel(
                CHANNEL_ID,
                getString(R.string.vpn_channel_name),
                NotificationManager.IMPORTANCE_LOW,
            ),
        )
    }

    private fun io.nekohasekai.libbox.RoutePrefixIterator.toList(): List<io.nekohasekai.libbox.RoutePrefix> =
        buildList { while (hasNext()) add(next()) }

    private fun io.nekohasekai.libbox.StringIterator.toList(): List<String> =
        buildList { while (hasNext()) add(next()) }

    companion object {
        const val ACTION_START = "com.skeidno.networkmanager.START"
        const val ACTION_STOP = "com.skeidno.networkmanager.STOP"
        const val ACTION_RELOAD = "com.skeidno.networkmanager.RELOAD"
        private const val CHANNEL_ID = "network-manager-vpn"
        private const val NOTIFICATION_ID = 1101
        private const val LOG_TAG = "NetworkVpnService"
    }
}
