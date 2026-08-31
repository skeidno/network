package com.skeidno.networkmanager

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.net.VpnService
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.activity.viewModels
import androidx.core.content.ContextCompat
import com.skeidno.networkmanager.ui.AppViewModel
import com.skeidno.networkmanager.ui.NetworkManagerApp
import com.skeidno.networkmanager.ui.NetworkManagerTheme
import com.skeidno.networkmanager.vpn.NetworkVpnService

class MainActivity : ComponentActivity() {
    private val viewModel by viewModels<AppViewModel>()

    private val vpnPermission = registerForActivityResult(
        ActivityResultContracts.StartActivityForResult(),
    ) { result ->
        if (result.resultCode == RESULT_OK) startVpnService()
    }

    private val notificationPermission = registerForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { prepareVpnStart() }

    private val importConfiguration = registerForActivityResult(
        ActivityResultContracts.OpenDocument(),
    ) { uri -> uri?.let(viewModel::importConfiguration) }

    private val exportConfiguration = registerForActivityResult(
        ActivityResultContracts.CreateDocument("application/json"),
    ) { uri -> uri?.let(viewModel::exportConfiguration) }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        handleDeepLink(intent)
        setContent {
            NetworkManagerTheme {
                NetworkManagerApp(
                    viewModel = viewModel,
                    onStartVpn = ::requestVpnStart,
                    onStopVpn = viewModel::stopVpn,
                    onImportConfiguration = {
                        importConfiguration.launch(arrayOf("application/json", "text/plain"))
                    },
                    onExportConfiguration = {
                        exportConfiguration.launch("NetworkManager-config.json")
                    },
                )
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleDeepLink(intent)
    }

    private fun requestVpnStart() {
        if (
            Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(this, Manifest.permission.POST_NOTIFICATIONS) !=
            PackageManager.PERMISSION_GRANTED
        ) {
            notificationPermission.launch(Manifest.permission.POST_NOTIFICATIONS)
            return
        }
        prepareVpnStart()
    }

    private fun prepareVpnStart() {
        viewModel.prepareForStart {
            val intent = VpnService.prepare(this)
            if (intent == null) startVpnService() else vpnPermission.launch(intent)
        }
    }

    private fun startVpnService() {
        ContextCompat.startForegroundService(
            this,
            Intent(this, NetworkVpnService::class.java).setAction(NetworkVpnService.ACTION_START),
        )
    }

    private fun handleDeepLink(intent: Intent?) {
        val uri = intent?.data ?: return
        if (uri.scheme in setOf("ss", "vmess", "vless", "trojan", "hysteria2", "hy2")) {
            viewModel.importText(uri.toString())
        }
    }

}
