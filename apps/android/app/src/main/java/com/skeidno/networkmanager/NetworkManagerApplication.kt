package com.skeidno.networkmanager

import android.app.Application
import android.content.Context
import android.util.Log
import com.skeidno.networkmanager.data.AppRepository
import com.skeidno.networkmanager.data.profilePaths
import io.nekohasekai.libbox.Libbox
import io.nekohasekai.libbox.SetupOptions
import java.util.Locale

class NetworkManagerApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        AppRepository.get(this)
        setupLibbox(this)
    }

    companion object {
        /**
         * 按**当前档案**的目录设置内核路径。
         *
         * libbox 的 basePath / workingPath / tempPath 是全局量，只在 setup 的时候读一次。
         * 所以切换档案之后必须重新 setup 一遍 —— 否则新档案跑接管时会继续读写旧档案的
         * working 目录，geoip / cache 这些缓存就串了，隔离等于没做。
         *
         * 把三处目录先 mkdirs 出来：setup 之后这些路径就要被内核直接写。
         */
        fun setupLibbox(context: Context): Result<Unit> = runCatching {
            val paths = context.profilePaths()
            paths.dataRoot.mkdirs()
            paths.workingRoot.mkdirs()
            paths.tempRoot.mkdirs()
            Libbox.setLocale(Locale.getDefault().toLanguageTag())
            Libbox.setup(
                SetupOptions().apply {
                    basePath = paths.dataRoot.path
                    workingPath = paths.workingRoot.path
                    tempPath = paths.tempRoot.path
                    fixAndroidStack = true
                    logMaxLines = 2_000
                    debug = BuildConfig.DEBUG
                },
            )
        }.onFailure { error ->
            Log.e("NetworkManager", "libbox initialization failed", error)
            AppRepository.get(context).updateRuntime(
                running = false,
                busy = false,
                message = "内核初始化失败",
                error = error.message.orEmpty(),
            )
        }
    }
}
