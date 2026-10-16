package com.skeidno.networkmanager.data

import android.content.ContentValues
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.MediaStore
import android.security.KeyChain
import java.io.File
import java.math.BigInteger
import java.security.KeyFactory
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.MessageDigest
import java.security.PrivateKey
import java.security.SecureRandom
import java.security.cert.CertificateFactory
import java.security.cert.X509Certificate
import java.security.spec.ECGenParameterSpec
import java.security.spec.PKCS8EncodedKeySpec
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneOffset
import java.time.format.DateTimeFormatter
import java.util.Base64
import java.util.Date
import org.bouncycastle.asn1.DEROctetString
import org.bouncycastle.asn1.x500.X500Name
import org.bouncycastle.asn1.x509.BasicConstraints
import org.bouncycastle.asn1.x509.ExtendedKeyUsage
import org.bouncycastle.asn1.x509.Extension
import org.bouncycastle.asn1.x509.GeneralName
import org.bouncycastle.asn1.x509.GeneralNames
import org.bouncycastle.asn1.x509.KeyPurposeId
import org.bouncycastle.asn1.x509.KeyUsage
import org.bouncycastle.cert.jcajce.JcaX509CertificateConverter
import org.bouncycastle.cert.jcajce.JcaX509ExtensionUtils
import org.bouncycastle.cert.jcajce.JcaX509v3CertificateBuilder
import org.bouncycastle.operator.jcajce.JcaContentSignerBuilder
import org.json.JSONArray
import org.json.JSONObject

/** 叶证书有效期（天）。 */
const val LOCAL_CERT_LEAF_VALIDITY_DAYS = 397L

/** 私有根有效期（天）。 */
const val LOCAL_CERT_CA_VALIDITY_DAYS = 3650L

/** 叶证书剩余不足这么多天就重签。 */
const val LOCAL_CERT_RENEW_BEFORE_DAYS = 30L

const val LOCAL_CERT_CA_COMMON_NAME = "Network Manager Local CA"
private const val LOCAL_CERT_LEAF_COMMON_NAME = "Network Manager Local Dev"
private const val LOCAL_CERT_ORGANIZATION = "Network Manager"

/**
 * 本地证书当前状态。全部是「看文件 + meta 就能知道」的事实，运行态、不进配置：
 * 证书文件才是事实来源，把状态存进 SharedPreferences 反而会跟磁盘打架。
 */
data class LocalCertStatus(
    val dir: String = "",
    val caExists: Boolean = false,
    val leafExists: Boolean = false,
    val caFingerprint: String = "",
    /** 按当前映射应当覆盖的名字。 */
    val names: List<String> = emptyList(),
    /** 证书里实际写着的名字（来自落盘的 meta，不解析证书）。 */
    val certifiedNames: List<String> = emptyList(),
    val leafNotAfter: String = "",
    val leafDaysLeft: Long = 0,
) {
    val namesInSync: Boolean get() = names == certifiedNames
    val caValidityDays: Long get() = LOCAL_CERT_CA_VALIDITY_DAYS
    val leafValidityDays: Long get() = LOCAL_CERT_LEAF_VALIDITY_DAYS
}

/** 生成出来的一整套 PEM 与摘要。 */
data class LocalCertBundle(
    val caCertPem: String,
    val caKeyPem: String,
    val leafCertPem: String,
    val leafKeyPem: String,
    val caFingerprint: String,
    val leafFingerprint: String,
    val leafNotAfter: String,
    val leafNotAfterMillis: Long,
)

/**
 * 本地 CA 与叶证书的生成。
 *
 * 与桌面端（rust/src/localca.rs）同一套约束：
 * 1. **叶证书 397 天** —— Chrome Root Store 对任何 TLS 证书（含本机私有根签的）执行
 *    398 天上限，超一天就是 `ERR_CERT_VALIDITY_TOO_LONG`，实测 825 / 800 天都会被拦；
 * 2. **SAN 逐个列名字**：通配 `*.baidu.com` 不覆盖裸域，两个都要写；映射里的 IP 也写进去。
 *
 * 根证书只在缺失时生成 —— 它一旦被信任，换根就意味着用户要重装一次。
 */
object LocalCertificate {
    private val IPV4 = Regex("""^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$""")
    private val ISO_DATE: DateTimeFormatter = DateTimeFormatter.ISO_LOCAL_DATE

    /** 只认字面量 IPv4，绝不走 DNS：给主机名就会真的发起一次查询。 */
    fun parseIpV4(text: String): ByteArray? {
        val match = IPV4.find(text.trim()) ?: return null
        val octets = match.groupValues.drop(1).map { it.toInt() }
        if (octets.any { it !in 0..255 }) return null
        return octets.map { it.toByte() }.toByteArray()
    }

    /**
     * 从映射表推出叶证书必须覆盖的名字。通配条目连裸域一起，地址是字面量 IP 的
     * 也要写进去（直连 IP 访问时同样校验 SAN）。
     */
    fun requiredNames(entries: List<LocalDnsEntry>): List<String> {
        val names = LinkedHashSet<String>()
        for (entry in entries) {
            val suffix = entry.wildcardSuffix?.trimEnd('.')
            if (suffix != null) {
                names.add("*.$suffix".lowercase())
                names.add(suffix.lowercase())
            } else {
                names.add(entry.domain.trim().trimEnd('.').lowercase())
            }
            if (parseIpV4(entry.address) != null) names.add(entry.address.trim())
        }
        return names.filter { it.isNotEmpty() }.distinct().sorted()
    }

    /** 生成根证书与叶证书。`names` 至少要有一个，否则签出来的证书没有任何用处。 */
    fun generate(names: List<String>): LocalCertBundle {
        require(names.isNotEmpty()) { "还没有本地 DNS 映射，先加一条映射再生成证书" }
        val (caCert, caKeys) = buildCa()
        return signLeaf(names, caCert, caKeys)
    }

    /** 只重签叶证书：根证书已经装进过信任库，换根等于让用户重装一次。 */
    fun generateLeaf(names: List<String>, caCertPem: String, caKeyPem: String): LocalCertBundle {
        require(names.isNotEmpty()) { "还没有本地 DNS 映射，先加一条映射再生成证书" }
        val caCert = parseCertificate(caCertPem)
        val caKey = decodePrivateKey(caKeyPem)
        return signLeaf(names, caCert, KeyPair(caCert.publicKey, caKey))
    }

    fun parseCertificate(pem: String): X509Certificate {
        val der = decodePemBody(pem)
        return CertificateFactory.getInstance("X.509")
            .generateCertificate(der.inputStream()) as X509Certificate
    }

    /**
     * OpenSSL 风格的旧版主题哈希（`subject_hash_old`）：对主题 DER 做 MD5 取前 4 字节
     * 倒序。往 `/system/etc/security/cacerts/<hash>.0` 放根证书时用的就是这个名字。
     */
    fun subjectHashOld(cert: X509Certificate): String {
        val digest = MessageDigest.getInstance("MD5").digest(cert.subjectX500Principal.encoded)
        return "%02x%02x%02x%02x".format(digest[3], digest[2], digest[1], digest[0])
    }

    fun fingerprint(pem: String): String =
        MessageDigest.getInstance("SHA-256").digest(parseCertificate(pem).encoded)
            .joinToString(":") { "%02X".format(it) }

    private fun buildCa(): Pair<X509Certificate, KeyPair> {
        val keys = generateKeyPair()
        val name = X500Name("CN=$LOCAL_CERT_CA_COMMON_NAME,O=$LOCAL_CERT_ORGANIZATION")
        val cert = buildCertificate(
            issuer = name,
            subject = name,
            publicKey = keys.public,
            signerKey = keys.private,
            validityDays = LOCAL_CERT_CA_VALIDITY_DAYS,
            isCa = true,
            names = emptyList(),
            issuerCertificate = null,
        )
        return cert to keys
    }

    private fun signLeaf(
        names: List<String>,
        caCert: X509Certificate,
        caKeys: KeyPair,
    ): LocalCertBundle {
        val leafKeys = generateKeyPair()
        val issuer = X500Name.getInstance(caCert.issuerX500Principal.encoded)
        val leafCert = buildCertificate(
            issuer = issuer,
            subject = X500Name("CN=$LOCAL_CERT_LEAF_COMMON_NAME,O=$LOCAL_CERT_ORGANIZATION"),
            publicKey = leafKeys.public,
            signerKey = caKeys.private,
            validityDays = LOCAL_CERT_LEAF_VALIDITY_DAYS,
            isCa = false,
            names = names,
            issuerCertificate = caCert,
        )
        val notAfter = LocalDate.now(ZoneOffset.UTC).plusDays(LOCAL_CERT_LEAF_VALIDITY_DAYS)
        val notAfterInstant = notAfter.atStartOfDay(ZoneOffset.UTC).toInstant()
        return LocalCertBundle(
            caCertPem = toPem("CERTIFICATE", caCert.encoded),
            caKeyPem = toPem("PRIVATE KEY", caKeys.private.encoded),
            leafCertPem = toPem("CERTIFICATE", leafCert.encoded),
            leafKeyPem = toPem("PRIVATE KEY", leafKeys.private.encoded),
            caFingerprint = fingerprint(caCert.encoded),
            leafFingerprint = fingerprint(leafCert.encoded),
            leafNotAfter = notAfter.format(ISO_DATE),
            leafNotAfterMillis = notAfterInstant.toEpochMilli(),
        )
    }

    private fun buildCertificate(
        issuer: X500Name,
        subject: X500Name,
        publicKey: java.security.PublicKey,
        signerKey: PrivateKey,
        validityDays: Long,
        isCa: Boolean,
        names: List<String>,
        issuerCertificate: X509Certificate?,
    ): X509Certificate {
        val now = Instant.now()
        // notBefore 回拨一分钟，容忍设备之间常见的时钟偏移。
        val notBefore = Date.from(now.minusSeconds(60))
        val notAfter = Date.from(now.plusSeconds(validityDays * 86_400))
        val builder = JcaX509v3CertificateBuilder(
            issuer,
            BigInteger(159, SecureRandom()),
            notBefore,
            notAfter,
            subject,
            publicKey,
        )
        val extensions = JcaX509ExtensionUtils()
        if (isCa) {
            builder.addExtension(Extension.basicConstraints, true, BasicConstraints(true))
            builder.addExtension(
                Extension.keyUsage,
                true,
                KeyUsage(KeyUsage.keyCertSign or KeyUsage.cRLSign or KeyUsage.digitalSignature),
            )
        } else {
            builder.addExtension(Extension.basicConstraints, true, BasicConstraints(false))
            // EC 密钥只做数字签名（ECDHE_ECDSA）。RFC 5480 禁止 EC 证书声明
            // Key Encipherment，严格校验的客户端（OpenSSL 的 -x509_strict、
            // Python 3.13 的 ssl 默认严格模式）会盯这类越界用途。
            builder.addExtension(
                Extension.keyUsage,
                true,
                KeyUsage(KeyUsage.digitalSignature),
            )
            builder.addExtension(
                Extension.extendedKeyUsage,
                false,
                ExtendedKeyUsage(KeyPurposeId.id_kp_serverAuth),
            )
        }
        if (names.isNotEmpty()) {
            builder.addExtension(
                Extension.subjectAlternativeName,
                false,
                GeneralNames(
                    names.map { name ->
                        // iPAddress 的 GeneralName 是 4 字节原始编码，不是点分文本；
                        // DNS 名才是可见字符串。
                        LocalCertificate.parseIpV4(name)
                            ?.let { bytes -> GeneralName(GeneralName.iPAddress, DEROctetString(bytes)) }
                            ?: GeneralName(GeneralName.dNSName, name)
                    }.toTypedArray(),
                ),
            )
        }
        issuerCertificate?.let {
            builder.addExtension(
                Extension.authorityKeyIdentifier,
                false,
                extensions.createAuthorityKeyIdentifier(it.publicKey),
            )
        }
        builder.addExtension(
            Extension.subjectKeyIdentifier,
            false,
            extensions.createSubjectKeyIdentifier(publicKey),
        )
        val signer = JcaContentSignerBuilder("SHA256withECDSA").build(signerKey)
        return JcaX509CertificateConverter().getCertificate(builder.build(signer))
    }

    private fun generateKeyPair(): KeyPair =
        KeyPairGenerator.getInstance("EC").apply {
            initialize(ECGenParameterSpec("secp256r1"), SecureRandom())
        }.generateKeyPair()

    private fun decodePrivateKey(pem: String): PrivateKey =
        KeyFactory.getInstance("EC")
            .generatePrivate(PKCS8EncodedKeySpec(decodePemBody(pem)))

    private fun decodePemBody(pem: String): ByteArray =
        Base64.getMimeDecoder().decode(
            pem.lineSequence()
                .filterNot { it.trimStart().startsWith("-----") }
                .joinToString(""),
        )

    private fun fingerprint(der: ByteArray): String =
        MessageDigest.getInstance("SHA-256").digest(der).joinToString(":") { "%02X".format(it) }

    private fun toPem(label: String, der: ByteArray): String {
        val body = Base64.getMimeEncoder(64, "\n".toByteArray()).encodeToString(der)
        return "-----BEGIN $label-----\n$body\n-----END $label-----\n"
    }
}

/**
 * 落盘、读取与安装入口。
 *
 * 证书挂在**当前档案**的数据目录下（默认档案是应用私有目录根，其余档案是 `profiles/<id>/`），
 * 所以每个档案有自己的 CA 与叶证书：A 档案装进系统信任库的根，不会让 B 档案的证书跟着变"可信"。
 * 实例都是随用随建，路径每次现算，切换档案后重新 new 一个即可。
 */
class LocalCertStore(private val context: Context) {
    private val caCert: File get() = File(dir, "ca.crt")
    private val caKey: File get() = File(dir, "ca.key")
    private val leafCert: File get() = File(dir, "server.crt")
    private val leafKey: File get() = File(dir, "server.key")
    private val metaFile: File get() = File(dir, "meta.json")

    private val dir: File get() =
        File(context.profilePaths().dataRoot, PROFILE_CERT_DIR)

    /** 当前状态。只读小文件，不解析证书。 */
    fun status(entries: List<LocalDnsEntry>): LocalCertStatus {
        val meta = readMeta()
        val now = Instant.now().toEpochMilli()
        val notAfter = meta?.optLong("leafNotAfterMillis") ?: 0L
        return LocalCertStatus(
            dir = dir.path,
            caExists = caCert.isFile && caKey.isFile,
            leafExists = leafCert.isFile && leafKey.isFile,
            caFingerprint = meta?.optString("caFingerprint").orEmpty(),
            names = LocalCertificate.requiredNames(entries),
            certifiedNames = meta?.optJSONArray("names")?.let { array ->
                List(array.length()) { index -> array.optString(index) }
            }.orEmpty(),
            leafNotAfter = meta?.optString("leafNotAfter").orEmpty(),
            leafDaysLeft = when {
                notAfter <= 0L -> 0
                else -> (((notAfter - now) / 86_400_000.0).let { value ->
                    if (value < 0) 0 else value.toLong()
                })
            },
        )
    }

    /**
     * 把证书准备到位：根证书只在缺失时生成，叶证书在「缺失 / 名字变了 / 快到期」时重签。
     * 映射清空时调用方应直接跳过 —— 没有名字就没什么可签的。
     */
    fun ensure(entries: List<LocalDnsEntry>): LocalCertStatus {
        val names = LocalCertificate.requiredNames(entries)
        require(names.isNotEmpty()) { "还没有本地 DNS 映射，先加一条映射再生成证书" }

        dir.mkdirs()
        val previous = readMeta()
        val caReady = caCert.isFile && caKey.isFile
        val fullBundle = if (caReady) null else LocalCertificate.generate(names)
        val caFingerprint = fullBundle?.caFingerprint
            ?: previous?.optString("caFingerprint")?.takeIf { it.isNotEmpty() }
            ?: LocalCertificate.fingerprint(caCert.readText())

        if (!caReady) {
            caCert.writeText(fullBundle!!.caCertPem)
            caKey.writeText(fullBundle.caKeyPem)
        }

        val inSync = previous?.optJSONArray("names")?.let { array ->
            List(array.length()) { index -> array.optString(index) }
        } == names && previous.optString("caFingerprint") == caFingerprint
        val expiring = (previous?.optLong("leafNotAfterMillis") ?: 0L) -
            Instant.now().toEpochMilli() < LOCAL_CERT_RENEW_BEFORE_DAYS * 86_400_000
        val onDisk = leafCert.isFile && leafKey.isFile

        if (!(inSync && !expiring && onDisk)) {
            val leaf = fullBundle
                ?: LocalCertificate.generateLeaf(names, caCert.readText(), caKey.readText())
            leafCert.writeText(leaf.leafCertPem)
            leafKey.writeText(leaf.leafKeyPem)
            writeMeta(caFingerprint, leaf, names)
        }
        return status(entries)
    }

    /** 走系统「安装 CA 证书」对话框：免 root，装进当前用户，需要锁屏凭据。 */
    fun installIntent(): Intent {
        require(caCert.isFile) { "还没有生成根证书，先点「生成或更新证书」" }
        val certificate = LocalCertificate.parseCertificate(caCert.readText())
        return KeyChain.createInstallIntent().apply {
            putExtra(KeyChain.EXTRA_CERTIFICATE, certificate.encoded)
            putExtra(KeyChain.EXTRA_NAME, LOCAL_CERT_CA_COMMON_NAME)
        }
    }

    /** 把 ca.crt 拷到公共下载目录，方便拿到别的设备 / 电脑上装。 */
    fun exportCa(): String {
        require(caCert.isFile) { "还没有生成根证书，先点「生成或更新证书」" }
        val name = "network-manager-local-ca.crt"
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            val values = ContentValues().apply {
                put(MediaStore.Downloads.DISPLAY_NAME, name)
                put(MediaStore.Downloads.MIME_TYPE, "application/x-x509-ca-cert")
            }
            val uri: Uri = context.contentResolver
                .insert(MediaStore.Downloads.EXTERNAL_CONTENT_URI, values)
                ?: error("无法写入下载目录")
            context.contentResolver.openOutputStream(uri)?.use { output ->
                output.write(caCert.readBytes())
            } ?: error("无法写入下载目录")
            return "ca.crt 已导出到下载目录（$name）"
        }
        // Android 9 及以下没有 MediaStore.Downloads：写应用专属的下载目录，
        // 不用申请存储权限，路径直接告诉用户去哪拿。
        val target = File(
            context.getExternalFilesDir(Environment.DIRECTORY_DOWNLOADS) ?: dir,
            name,
        )
        target.parentFile?.mkdirs()
        caCert.copyTo(target, overwrite = true)
        return "ca.crt 已导出到 ${target.path}"
    }

    /**
     * 有 root 时直接进系统信任库：`/system/etc/security/cacerts/<hash>.0`。
     * 需要可写的 /system，失败就抛出去让界面提示改用「安装 CA」或「导出」。
     */
    fun installWithRoot(): String {
        require(caCert.isFile) { "还没有生成根证书，先点「生成或更新证书」" }
        val certificate = LocalCertificate.parseCertificate(caCert.readText())
        val hash = LocalCertificate.subjectHashOld(certificate)
        val target = "/system/etc/security/cacerts/$hash.0"
        val script = "mount -o remount,rw /system && cp ${caCert.absolutePath} $target && " +
            "chmod 644 $target && chown root:root $target"
        // su 的语法各家不一样：Magisk 是 `su -c <cmd>`，AOSP/toybox 只认
        // `su 0 sh -c <cmd>`（`su -c` 会被当成 uid "-c" 报 invalid uid/gid）。
        // 两种都试一遍，谁先成功用谁。
        val attempts = listOf(
            listOf("su", "-c", script),
            listOf("su", "0", "sh", "-c", script),
        )
        var lastOutput = ""
        for (argv in attempts) {
            val process = ProcessBuilder(argv).redirectErrorStream(true).start()
            val output = process.inputStream.bufferedReader().use { it.readText() }
            val code = process.waitFor()
            if (code == 0) return "根证书已放进系统信任库（$target）"
            lastOutput = output.ifBlank { argv.joinToString(" ").take(60) }
        }
        throw IllegalStateException("root 安装失败（$lastOutput）。没有 root 就用「安装 CA」走系统对话框")
    }

    private fun readMeta(): JSONObject? = runCatching {
        if (!metaFile.isFile) return null
        JSONObject(metaFile.readText())
    }.getOrNull()

    private fun writeMeta(caFingerprint: String, bundle: LocalCertBundle, names: List<String>) {
        metaFile.writeText(
            JSONObject()
                .put("caFingerprint", caFingerprint)
                .put("leafFingerprint", bundle.leafFingerprint)
                .put("names", JSONArray(names))
                .put("leafNotAfter", bundle.leafNotAfter)
                .put("leafNotAfterMillis", bundle.leafNotAfterMillis)
                .put("generatedAt", Instant.now().toString())
                .toString(2),
        )
    }
}
