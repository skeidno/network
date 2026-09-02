package com.skeidno.networkmanager.vpn

import android.content.pm.ApplicationInfo
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
import android.util.Log
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
import java.io.File
import java.net.Inet6Address
import java.net.InetAddress
import java.net.InetSocketAddress
import java.net.NetworkInterface
import java.security.KeyStore
import java.security.cert.X509Certificate
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import io.nekohasekai.libbox.NetworkInterface as BoxNetworkInterface

private const val OWNER_LOG_TAG = "NetworkVpnOwner"

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

    override fun useProcFS(): Boolean = false

    override fun findConnectionOwner(
        ipProtocol: Int,
        sourceAddress: String,
        sourcePort: Int,
        destinationAddress: String,
        destinationPort: Int,
    ): ConnectionOwner {
        return try {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
                findConnectionOwnerQ(
                    ipProtocol,
                    sourceAddress,
                    sourcePort,
                    destinationAddress,
                    destinationPort,
                )
            } else {
                val uid = ProcfsConnectionOwnerResolver.findUid(
                    ipProtocol,
                    sourceAddress,
                    sourcePort,
                )
                check(uid != Process.INVALID_UID) { "未找到连接所属应用" }
                connectionOwner(uid)
            }
        } catch (error: Exception) {
            Log.e(OWNER_LOG_TAG, "Unable to resolve Android connection owner", error)
            throw error
        }
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
        return connectionOwner(uid)
    }

    private fun connectionOwner(uid: Int): ConnectionOwner = ConnectionOwner().apply {
            userId = uid
            val packages = service.packageManager.getPackagesForUid(uid).orEmpty()
            if (service.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0) {
                Log.d(OWNER_LOG_TAG, "Resolved connection owner uid=$uid packages=${packages.joinToString()}")
            }
            userName = packages.firstOrNull().orEmpty()
            setAndroidPackageNames(StringArray(packages.iterator()))
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
        val includedNames = interfaces.mapTo(mutableSetOf()) { it.name }
        javaInterfaces.filterNot { it.name in includedNames }.forEach { javaInterface ->
            interfaces += BoxNetworkInterface().apply {
                name = javaInterface.name
                index = javaInterface.index
                mtu = runCatching { javaInterface.mtu }.getOrDefault(1500)
                type = Libbox.InterfaceTypeOther
                flags = buildInterfaceFlags(javaInterface)
                metered = false
                addresses = StringArray(javaInterface.interfaceAddresses.map { address ->
                    val host = if (address.address is Inet6Address) {
                        Inet6Address.getByAddress(address.address.address).hostAddress
                    } else address.address.hostAddress
                    "$host/${address.networkPrefixLength}"
                }.iterator())
                dnsServer = StringArray(emptyList<String>().iterator())
            }
        }
        if (service.applicationInfo.flags and ApplicationInfo.FLAG_DEBUGGABLE != 0) {
            Log.d(
                OWNER_LOG_TAG,
                "Reported interfaces: " + javaInterfaces.joinToString { javaInterface ->
                    val addresses = javaInterface.interfaceAddresses.joinToString { address ->
                        "${address.address.hostAddress}/${address.networkPrefixLength}"
                    }
                    "${javaInterface.name}=[$addresses]"
                },
            )
        }
        return InterfaceArray(interfaces.iterator())
    }

    private fun buildInterfaceFlags(networkInterface: NetworkInterface): Int {
        var flags = 0
        if (runCatching { networkInterface.isUp }.getOrDefault(false)) {
            flags = flags or OsConstants.IFF_UP or OsConstants.IFF_RUNNING
        }
        if (networkInterface.isLoopback) flags = flags or OsConstants.IFF_LOOPBACK
        if (networkInterface.isPointToPoint) flags = flags or OsConstants.IFF_POINTOPOINT
        if (networkInterface.supportsMulticast()) flags = flags or OsConstants.IFF_MULTICAST
        return flags
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

internal object ProcfsConnectionOwnerResolver {
    fun findUid(ipProtocol: Int, sourceAddress: String, sourcePort: Int): Int {
        val tableNames = when (ipProtocol) {
            OsConstants.IPPROTO_TCP -> listOf("tcp", "tcp6")
            OsConstants.IPPROTO_UDP -> listOf("udp", "udp6")
            else -> return Process.INVALID_UID
        }
        val normalizedSource = normalizeAddress(sourceAddress) ?: return Process.INVALID_UID
        tableNames.forEach { tableName ->
            val uid = runCatching {
                File("/proc/net/$tableName").bufferedReader().useLines { lines ->
                    findUid(lines, normalizedSource, sourcePort)
                }
            }.getOrNull()
            if (uid != null) return uid
        }
        return Process.INVALID_UID
    }

    internal fun findUid(lines: Sequence<String>, sourceAddress: String, sourcePort: Int): Int? {
        val expectedPort = sourcePort.toString(16).padStart(4, '0')
        return lines.drop(1).mapNotNull { line ->
            val fields = line.trim().split(Regex("\\s+"))
            if (fields.size <= UID_COLUMN_INDEX) return@mapNotNull null
            val local = fields[LOCAL_ADDRESS_COLUMN_INDEX]
            val separator = local.lastIndexOf(':')
            if (separator <= 0 || !local.substring(separator + 1).equals(expectedPort, true)) {
                return@mapNotNull null
            }
            val address = decodeAddress(local.substring(0, separator)) ?: return@mapNotNull null
            if (address != sourceAddress) return@mapNotNull null
            fields[UID_COLUMN_INDEX].toIntOrNull()
        }.firstOrNull()
    }

    private fun decodeAddress(value: String): String? {
        if (value.length != 8 && value.length != 32) return null
        val raw = runCatching {
            ByteArray(value.length / 2) { index ->
                value.substring(index * 2, index * 2 + 2).toInt(16).toByte()
            }
        }.getOrNull() ?: return null
        for (offset in raw.indices step 4) {
            raw.reverse(offset, offset + 4)
        }
        return runCatching { InetAddress.getByAddress(raw).hostAddress }
            .getOrNull()
            ?.let(::normalizeAddress)
    }

    private fun normalizeAddress(value: String): String? = runCatching {
        InetAddress.getByName(value.substringBefore('%')).hostAddress
    }.getOrNull()?.substringBefore('%')

    private const val LOCAL_ADDRESS_COLUMN_INDEX = 1
    private const val UID_COLUMN_INDEX = 7
}
