package com.skeidno.networkmanager.data

import java.security.cert.X509Certificate
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test

class LocalCertificateTest {
    private fun entry(domain: String, address: String) = LocalDnsEntry(domain = domain, address = address)

    // -----------------------------------------------------------------------
    // 名字推导：和桌面端 required_sans 同一套规则，两端签出来的覆盖面必须一致
    // -----------------------------------------------------------------------

    @Test
    fun `wildcard entry also covers the apex and the mapped ip`() {
        val names = LocalCertificate.requiredNames(
            listOf(entry("*.baidu.com", "192.168.1.10")),
        )
        assertTrue(names.contains("*.baidu.com"))
        assertTrue(names.contains("baidu.com"))
        assertTrue(names.contains("192.168.1.10"))
    }

    @Test
    fun `exact entry stays single and drops the trailing dot`() {
        val names = LocalCertificate.requiredNames(listOf(entry("nas.lan", "10.0.0.2")))
        assertEquals(listOf("10.0.0.2", "nas.lan"), names)
    }

    @Test
    fun `hostname addresses are not turned into san entries`() {
        val names = LocalCertificate.requiredNames(listOf(entry("nas.lan", "another.host")))
        assertEquals(listOf("nas.lan"), names)
    }

    @Test
    fun `names are lowercased and deduplicated`() {
        val names = LocalCertificate.requiredNames(
            listOf(entry("NAS.LAN", "10.0.0.2"), entry("nas.lan", "10.0.0.2")),
        )
        assertEquals(listOf("10.0.0.2", "nas.lan"), names)
    }

    @Test
    fun `ip parser rejects hostnames and malformed octets`() {
        assertEquals(null, LocalCertificate.parseIpV4("nas.lan"))
        assertEquals(null, LocalCertificate.parseIpV4("10.0.300.2"))
        assertArrayEquals(byteArrayOf(10, 0, 2, 2), LocalCertificate.parseIpV4("10.0.2.2"))
    }

    // -----------------------------------------------------------------------
    // 生成：证书结构、有效期上限、链关系
    // -----------------------------------------------------------------------

    private fun bundle() = LocalCertificate.generate(
        listOf("*.baidu.com", "baidu.com", "10.0.2.2", "nas.lan"),
    )

    private fun leaf(bundle: LocalCertBundle): X509Certificate =
        LocalCertificate.parseCertificate(bundle.leafCertPem)

    private fun ca(bundle: LocalCertBundle): X509Certificate =
        LocalCertificate.parseCertificate(bundle.caCertPem)

    @Test
    fun `leaf validity stays under the chrome ceiling`() {
        val cert = leaf(bundle())
        val days = (cert.notAfter.time - cert.notBefore.time) / 86_400_000.0
        assertTrue("叶证书有效期 $days 天超过 398 天上限", days < 398)
        assertEquals(LOCAL_CERT_LEAF_VALIDITY_DAYS.toDouble(), days, 0.5)
    }

    @Test
    fun `ca validity is long and marked as a ca`() {
        val cert = ca(bundle())
        assertTrue(cert.basicConstraints != -1)
        // KeyUsage 位序：0=digitalSignature, 5=keyCertSign, 6=cRLSign。
        assertTrue("CA 应当有 keyCertSign", cert.keyUsage[5])
        assertTrue("CA 应当有 cRLSign", cert.keyUsage[6])
        val days = (cert.notAfter.time - cert.notBefore.time) / 86_400_000.0
        assertEquals(LOCAL_CERT_CA_VALIDITY_DAYS.toDouble(), days, 0.5)
    }

    @Test
    fun `leaf chains to the generated ca and carries every name`() {
        val generated = bundle()
        val certificate = leaf(generated)
        certificate.verify(ca(generated).publicKey)

        // 不同 CertificateFactory 实现给出来的 SAN 值类型不一样（String / IA5String /
        // 字节数组都有可能），统一转成文本或字节数组再比对。
        val dnsNames = sanTexts(certificate, tag = 2).toSet()
        assertEquals(setOf("*.baidu.com", "baidu.com", "nas.lan"), dnsNames)

        val ips = sanTexts(certificate, tag = 7).map { value ->
            when {
                value.length == 4 -> value.map { it.code.toByte() }.toByteArray()
                else -> java.net.InetAddress.getByName(value).address
            }
        }
        assertTrue(ips.any { it.contentEquals(byteArrayOf(10, 0, 2, 2)) })

        assertFalse(certificate.subjectX500Principal.name.contains("Local CA"))
    }

    @Test
    fun `leaf only claims digital signature and carries an authority key id`() {
        val generated = bundle()
        val certificate = leaf(generated)

        // KeyUsage 位序：0=digitalSignature、2=keyEncipherment。EC 密钥只签名，
        // 声明 keyEncipherment 违反 RFC 5480，严格校验的客户端会报错。
        assertTrue("叶证书应当有 digitalSignature", certificate.keyUsage[0])
        assertFalse("EC 叶证书不该声明 keyEncipherment", certificate.keyUsage[2])
        // RFC 5280 要求 CA 签发的证书带 AKI，否则链构建认不出签发者。
        // 2.5.29.35 = authorityKeyIdentifier，用公开 API 读扩展本身（JDK 的
        // X509Certificate 没有对应的属性访问器）。
        assertNotNull(
            "叶证书缺 Authority Key Identifier",
            certificate.getExtensionValue("2.5.29.35"),
        )
    }

    private fun sanTexts(certificate: X509Certificate, tag: Int): List<String> =
        certificate.subjectAlternativeNames
            .filter { (it[0] as Number).toInt() == tag }
            .map { entry ->
                when (val value = entry[1]) {
                    is ByteArray -> String(value, Charsets.ISO_8859_1)
                    is org.bouncycastle.asn1.ASN1Primitive -> value.toString()
                    else -> value.toString()
                }
            }

    @Test
    fun `regenerating the leaf keeps the same ca`() {
        val first = bundle()
        val second = LocalCertificate.generateLeaf(
            listOf("nas.lan"),
            first.caCertPem,
            first.caKeyPem,
        )
        leaf(second).verify(ca(first).publicKey)
        assertEquals(first.caFingerprint, second.caFingerprint)
        assertEquals(
            listOf("nas.lan"),
            sanTexts(leaf(second), tag = 2),
        )
    }

    @Test
    fun `empty names are refused`() {
        assertThrows(IllegalArgumentException::class.java) { LocalCertificate.generate(emptyList()) }
    }

    @Test
    fun `subject hash old is eight lowercase hex characters and stable`() {
        val hash = LocalCertificate.subjectHashOld(ca(bundle()))
        assertTrue("主题哈希应该是 8 位小写十六进制：$hash", hash.matches(Regex("^[0-9a-f]{8}$")))
        assertEquals(hash, LocalCertificate.subjectHashOld(ca(bundle())))
    }
}
