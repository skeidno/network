package com.skeidno.networkmanager

import android.app.Application
import android.util.Log
import com.skeidno.networkmanager.data.AppRepository
import io.nekohasekai.libbox.Libbox
import io.nekohasekai.libbox.SetupOptions
import java.util.Locale

class NetworkManagerApplication : Application() {
    override fun onCreate() {
        super.onCreate()
        AppRepository.get(this)
        runCatching {
            Libbox.setLocale(Locale.getDefault().toLanguageTag())
            Libbox.setup(
                SetupOptions().apply {
                    basePath = filesDir.path
                    workingPath = (getExternalFilesDir(null) ?: filesDir).path
                    tempPath = cacheDir.path
                    fixAndroidStack = true
                    logMaxLines = 2_000
                    debug = BuildConfig.DEBUG
                },
            )
        }.onFailure { error ->
            Log.e("NetworkManager", "libbox initialization failed", error)
            AppRepository.get(this).updateRuntime(
                running = false,
                busy = false,
                message = "内核初始化失败",
                error = error.message.orEmpty(),
            )
        }
    }
}
