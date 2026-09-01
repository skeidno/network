package com.skeidno.networkmanager.vpn

import android.net.ConnectivityManager
import android.net.DnsResolver
import android.net.Network
import android.net.NetworkCapabilities
import android.net.NetworkRequest
import android.os.Build
import android.os.CancellationSignal
import android.os.Process
import android.system.ErrnoException
import android.system.OsConstants
import android.util.Base64
import androidx.annotation.RequiresApi
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
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.asExecutor
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.suspendCancellableCoroutine
import java.net.Inet6Address
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.security.KeyStore
import java.security.cert.X509Certificate
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import io.nekohasekai.libbox.NetworkInterface as BoxNetworkInterface

class AndroidLibboxPlatform(private val service: NetworkVpnService) : PlatformInterface {
    private val connectivity = service.getSystemService(ConnectivityManager::class.java)
    private var networkCallback: ConnectivityManager.NetworkCallback? = null
    @Volatile private var underlyingNetwork: Network? = null
    private val localResolver = AndroidLocalResolver(::currentUnderlyingNetwork)

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
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
            error("当前系统不支持进程识别")
        }
        return findConnectionOwnerQ(
            ipProtocol,
            sourceAddress,
            sourcePort,
            destinationAddress,
            destinationPort,
        )
    }

    @RequiresApi(Build.VERSION_CODES.Q)
    private fun findConnectionOwnerQ(
        ipProtocol: Int,
        sourceAddress: String,
        sourcePort: Int,
        destinationAddress: String,
        destinationPort: Int,
    ): ConnectionOwner {
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
            override fun onAvailable(network: Network) {
                underlyingNetwork = chooseUnderlyingNetwork(network)
                underlyingNetwork?.let { notifyDefaultNetwork(listener, it) }
            }

            override fun onCapabilitiesChanged(
                network: Network,
                networkCapabilities: NetworkCapabilities,
            ) {
                if (!isUnderlyingNetwork(networkCapabilities)) return
                underlyingNetwork = chooseUnderlyingNetwork(network)
                underlyingNetwork?.let { notifyDefaultNetwork(listener, it) }
            }

            override fun onLinkPropertiesChanged(network: Network, linkProperties: android.net.LinkProperties) =
                if (network == underlyingNetwork) notifyDefaultNetwork(listener, network) else Unit

            override fun onLost(network: Network) {
                if (network != underlyingNetwork) return
                underlyingNetwork = findUnderlyingNetwork()
                underlyingNetwork?.let { notifyDefaultNetwork(listener, it) }
                    ?: listener.updateDefaultInterface("", -1, false, false)
            }
        }
        networkCallback = callback
        connectivity.registerNetworkCallback(
            NetworkRequest.Builder()
                .addCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET)
                .addCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN)
                .build(),
            callback,
        )
        underlyingNetwork = findUnderlyingNetwork()
        underlyingNetwork?.let { notifyDefaultNetwork(listener, it) }
    }

    override fun closeDefaultInterfaceMonitor(listener: InterfaceUpdateListener) {
        networkCallback?.let { runCatching { connectivity.unregisterNetworkCallback(it) } }
        networkCallback = null
        underlyingNetwork = null
    }

    private fun currentUnderlyingNetwork(): Network =
        underlyingNetwork?.takeIf { network ->
            connectivity.getNetworkCapabilities(network)?.let(::isUnderlyingNetwork) == true
        } ?: findUnderlyingNetwork()?.also { underlyingNetwork = it }
        ?: error("没有可用的底层网络")

    private fun chooseUnderlyingNetwork(candidate: Network): Network =
        findUnderlyingNetwork() ?: candidate

    private fun findUnderlyingNetwork(): Network? = connectivity.allNetworks
        .filter { network ->
            connectivity.getNetworkCapabilities(network)?.let(::isUnderlyingNetwork) == true
        }
        .maxByOrNull { network ->
            val capabilities = connectivity.getNetworkCapabilities(network) ?: return@maxByOrNull 0
            when {
                capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_VALIDATED) -> 2
                capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) -> 1
                else -> 0
            }
        }

    private fun isUnderlyingNetwork(capabilities: NetworkCapabilities): Boolean =
        capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_INTERNET) &&
            capabilities.hasCapability(NetworkCapabilities.NET_CAPABILITY_NOT_VPN) &&
            !capabilities.hasTransport(NetworkCapabilities.TRANSPORT_VPN)

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
    override fun localDNSTransport(): LocalDNSTransport = localResolver
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

    class StringArray(iterator: Iterator<String>) : StringIterator {
        private val values = iterator.asSequence().toList()
        private var index = 0

        override fun hasNext(): Boolean = index < values.size
        override fun len(): Int = values.size
        override fun next(): String = values[index++]
    }

    private class AndroidLocalResolver(
        private val networkProvider: () -> Network,
    ) : LocalDNSTransport {
        override fun raw(): Boolean = Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q

        override fun exchange(context: io.nekohasekai.libbox.ExchangeContext, message: ByteArray) {
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
                context.errorCode(RCODE_NXDOMAIN)
                return
            }
            exchangeQ(context, message)
        }

        @RequiresApi(Build.VERSION_CODES.Q)
        private fun exchangeQ(
            context: io.nekohasekai.libbox.ExchangeContext,
            message: ByteArray,
        ) {
            val network = networkProvider()
            runBlocking {
                suspendCancellableCoroutine { continuation ->
                    val signal = CancellationSignal()
                    context.onCancel {
                        signal.cancel()
                        continuation.cancel()
                    }
                    continuation.invokeOnCancellation { signal.cancel() }
                    DnsResolver.getInstance().rawQuery(
                        network,
                        message,
                        DnsResolver.FLAG_NO_RETRY,
                        Dispatchers.IO.asExecutor(),
                        signal,
                        object : DnsResolver.Callback<ByteArray> {
                            override fun onAnswer(answer: ByteArray, rcode: Int) {
                                if (rcode == 0) context.rawSuccess(answer) else context.errorCode(rcode)
                                if (continuation.isActive) continuation.resume(Unit)
                            }

                            override fun onError(error: DnsResolver.DnsException) {
                                val cause = error.cause
                                if (cause is ErrnoException) {
                                    context.errnoCode(cause.errno)
                                    if (continuation.isActive) continuation.resume(Unit)
                                } else if (continuation.isActive) {
                                    continuation.resumeWithException(error)
                                }
                            }
                        },
                    )
                }
            }
        }

        override fun lookup(
            context: io.nekohasekai.libbox.ExchangeContext,
            networkName: String,
            domain: String,
        ) {
            val network = networkProvider()
            if (Build.VERSION.SDK_INT < Build.VERSION_CODES.Q) {
                runCatching { network.getAllByName(domain) }
                    .onSuccess { addresses ->
                        context.success(addresses.mapNotNull { it.hostAddress }.joinToString("\n"))
                    }
                    .onFailure { context.errorCode(RCODE_NXDOMAIN) }
                return
            }
            runBlocking {
                suspendCancellableCoroutine { continuation ->
                    val signal = CancellationSignal()
                    context.onCancel {
                        signal.cancel()
                        continuation.cancel()
                    }
                    continuation.invokeOnCancellation { signal.cancel() }
                    val callback = object : DnsResolver.Callback<Collection<java.net.InetAddress>> {
                        override fun onAnswer(answer: Collection<java.net.InetAddress>, rcode: Int) {
                            if (rcode == 0) {
                                context.success(answer.mapNotNull { it.hostAddress }.joinToString("\n"))
                            } else {
                                context.errorCode(rcode)
                            }
                            if (continuation.isActive) continuation.resume(Unit)
                        }

                        override fun onError(error: DnsResolver.DnsException) {
                            val cause = error.cause
                            if (cause is ErrnoException) {
                                context.errnoCode(cause.errno)
                                if (continuation.isActive) continuation.resume(Unit)
                            } else if (continuation.isActive) {
                                continuation.resumeWithException(error)
                            }
                        }
                    }
                    val type = when {
                        networkName.endsWith("4") -> DnsResolver.TYPE_A
                        networkName.endsWith("6") -> DnsResolver.TYPE_AAAA
                        else -> null
                    }
                    if (type == null) {
                        DnsResolver.getInstance().query(
                            network,
                            domain,
                            DnsResolver.FLAG_NO_RETRY,
                            Dispatchers.IO.asExecutor(),
                            signal,
                            callback,
                        )
                    } else {
                        DnsResolver.getInstance().query(
                            network,
                            domain,
                            type,
                            DnsResolver.FLAG_NO_RETRY,
                            Dispatchers.IO.asExecutor(),
                            signal,
                            callback,
                        )
                    }
                }
            }
        }

        companion object {
            private const val RCODE_NXDOMAIN = 3
        }
    }
}
