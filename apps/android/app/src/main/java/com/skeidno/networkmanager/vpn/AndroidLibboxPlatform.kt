package com.skeidno.networkmanager.vpn

import android.net.ConnectivityManager
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Build
import android.os.Process
import android.system.OsConstants
import android.util.Base64
import io.nekohasekai.libbox.ConnectionOwner
import io.nekohasekai.libbox.InterfaceUpdateListener
import io.nekohasekai.libbox.Libbox
import io.nekohasekai.libbox.LocalDNSTransport
import io.nekohasekai.libbox.NetworkInterfaceIterator
import io.nekohasekai.libbox.Notification
import io.nekohasekai.libbox.PlatformInterface
import io.nekohasekai.libbox.StringIterator
import io.nekohasekai.libbox.TunOptions
import io.nekohasekai.libbox.WIFIState
import java.net.Inet6Address
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.security.KeyStore
import java.security.cert.X509Certificate
import io.nekohasekai.libbox.NetworkInterface as BoxNetworkInterface

class AndroidLibboxPlatform(private val service: NetworkVpnService) : PlatformInterface {
    private val connectivity = service.getSystemService(ConnectivityManager::class.java)
    private var networkCallback: ConnectivityManager.NetworkCallback? = null

    override fun usePlatformAutoDetectInterfaceControl(): Boolean = true

    override fun autoDetectInterfaceControl(fd: Int) {
        check(service.protect(fd)) { "无法保护代理核心套接字" }
    }

    override fun openTun(options: TunOptions): Int = service.openTun(options)

    override fun useProcFS(): Boolean = Build.VERSION.SDK_INT < Build.VERSION_CODES.Q

    override fun findConnectionOwner(
        ipProtocol: Int,
        sourceAddress: String,
        sourcePort: Int,
        destinationAddress: String,
        destinationPort: Int,
    ): ConnectionOwner {
        check(Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) { "当前系统不支持进程识别" }
        val uid = connectivity.getConnectionOwnerUid(
            ipProtocol,
            InetSocketAddress(sourceAddress, sourcePort),
            InetSocketAddress(destinationAddress, destinationPort),
        )
        check(uid != Process.INVALID_UID) { "未找到连接所属应用" }
        return ConnectionOwner().apply {
            userId = uid
            val packages = service.packageManager.getPackagesForUid(uid).orEmpty()
            userName = packages.firstOrNull().orEmpty()
            setAndroidPackageNames(StringArray(packages.iterator()))
        }
    }

    override fun startDefaultInterfaceMonitor(listener: InterfaceUpdateListener) {
        closeDefaultInterfaceMonitor(listener)
        val callback = object : ConnectivityManager.NetworkCallback() {
            override fun onAvailable(network: Network) = notifyDefaultNetwork(listener, network)
            override fun onLinkPropertiesChanged(network: Network, linkProperties: android.net.LinkProperties) =
                notifyDefaultNetwork(listener, network)
            override fun onLost(network: Network) = listener.updateDefaultInterface("", -1, false, false)
        }
        networkCallback = callback
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            connectivity.registerDefaultNetworkCallback(callback)
        } else {
            connectivity.registerNetworkCallback(
                NetworkRequest.Builder().addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET).build(),
                callback,
            )
        }
        connectivity.activeNetwork?.let { notifyDefaultNetwork(listener, it) }
    }

    override fun closeDefaultInterfaceMonitor(listener: InterfaceUpdateListener) {
        networkCallback?.let { runCatching { connectivity.unregisterNetworkCallback(it) } }
        networkCallback = null
    }

    private fun notifyDefaultNetwork(listener: InterfaceUpdateListener, network: Network) {
        val name = connectivity.getLinkProperties(network)?.interfaceName.orEmpty()
        val index = runCatching { NetworkInterface.getByName(name)?.index ?: -1 }.getOrDefault(-1)
        listener.updateDefaultInterface(name, index, false, false)
    }

    override fun getInterfaces(): NetworkInterfaceIterator {
        val interfaces = mutableListOf<BoxNetworkInterface>()
        val javaInterfaces = NetworkInterface.getNetworkInterfaces()?.toList().orEmpty()
        connectivity.allNetworks.forEach { network ->
            val properties = connectivity.getLinkProperties(network) ?: return@forEach
            val capabilities = connectivity.getNetworkCapabilities(network) ?: return@forEach
            val javaInterface = javaInterfaces.firstOrNull { it.name == properties.interfaceName } ?: return@forEach
            interfaces += BoxNetworkInterface().apply {
                name = properties.interfaceName
                index = javaInterface.index
                mtu = runCatching { javaInterface.mtu }.getOrDefault(1500)
                type = when {
                    capabilities.hasTransport(NetworkCapabilities.TRANSPORT_WIFI) -> Libbox.InterfaceTypeWIFI
                    capabilities.hasTransport(NetworkCapabilities.TRANSPORT_CELLULAR) -> Libbox.InterfaceTypeCellular
                    capabilities.hasTransport(NetworkCapabilities.TRANSPORT_ETHERNET) -> Libbox.InterfaceTypeEthernet
                    else -> Libbox.InterfaceTypeOther
                }
                flags = if (capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)) {
                    OsConstants.IFF_UP or OsConstants.IFF_RUNNING
                } else 0
                metered = !capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_METERED)
                addresses = StringArray(javaInterface.interfaceAddresses.map { address ->
                    val host = if (address.address is Inet6Address) {
                        Inet6Address.getByAddress(address.address.address).hostAddress
                    } else address.address.hostAddress
                    "$host/${address.networkPrefixLength}"
                }.iterator())
                dnsServer = StringArray(properties.dnsServers.mapNotNull { it.hostAddress }.iterator())
            }
        }
        return InterfaceArray(interfaces.iterator())
    }

    override fun underNetworkExtension(): Boolean = false
    override fun includeAllNetworks(): Boolean = false
    override fun clearDNSCache() = Unit
    override fun readWIFIState(): WIFIState? = null
    override fun localDNSTransport(): LocalDNSTransport? = null
    override fun sendNotification(notification: Notification) = Unit

    override fun systemCertificates(): StringIterator {
        val certificates = runCatching {
            val store = KeyStore.getInstance("AndroidCAStore").apply { load(null) }
            store.aliases().toList().mapNotNull { alias ->
                val certificate = store.getCertificate(alias) as? X509Certificate ?: return@mapNotNull null
                val body = Base64.encodeToString(certificate.encoded, Base64.NO_WRAP)
                "-----BEGIN CERTIFICATE-----\n$body\n-----END CERTIFICATE-----"
            }
        }.getOrDefault(emptyList())
        return StringArray(certificates.iterator())
    }

    private class InterfaceArray(
        private val iterator: Iterator<BoxNetworkInterface>,
    ) : NetworkInterfaceIterator {
        override fun hasNext(): Boolean = iterator.hasNext()
        override fun next(): BoxNetworkInterface = iterator.next()
    }

    class StringArray(private val iterator: Iterator<String>) : StringIterator {
        override fun hasNext(): Boolean = iterator.hasNext()
        override fun len(): Int = 0
        override fun next(): String = iterator.next()
    }
}
