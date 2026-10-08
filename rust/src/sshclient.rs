//! Minimal SSH-2 client: binary packet protocol, password authentication and
//! exec channels.
//!
//! Why this exists instead of shelling out to `ssh.exe`:
//! the OpenSSH build shipped with Windows cannot create the pipe it needs for
//! `SSH_ASKPASS` on some machines, failing with
//! `ssh_askpass: pipe: Unknown error` before the helper is ever spawned. The
//! session then falls through to an empty password and surfaces as a bogus
//! "密码错误". Driving the protocol directly removes that failure mode (and the
//! dependency on an installed ssh client) entirely.

use std::cmp::min;
use std::path::Path;
use std::time::Duration;

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockEncrypt, KeyInit};
use hmac::{Hmac, Mac};
use num_bigint_dig::BigUint;
use sha2::{Digest, Sha256, Sha512};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const CLIENT_VERSION: &str = "SSH-2.0-NetworkManagerRs_0.6";
const MAC_LEN: usize = 32;
/// OpenSSH reports a 16-byte block size for the AES-CTR ciphers, so packets are
/// padded to a multiple of 16 (which also satisfies the 8-byte minimum).
const BLOCK_SIZE: usize = 16;
const MAX_PACKET: usize = 35000;
const CHANNEL_WINDOW: u32 = 4 << 20;
const CHANNEL_MAX_PACKET: u32 = 32 << 10;

// Message numbers.
const MSG_DISCONNECT: u8 = 1;
const MSG_IGNORE: u8 = 2;
const MSG_UNIMPLEMENTED: u8 = 3;
const MSG_DEBUG: u8 = 4;
const MSG_SERVICE_REQUEST: u8 = 5;
const MSG_SERVICE_ACCEPT: u8 = 6;
const MSG_KEXINIT: u8 = 20;
const MSG_NEWKEYS: u8 = 21;
const MSG_KEX_ECDH_INIT: u8 = 30;
const MSG_KEX_ECDH_REPLY: u8 = 31;
const MSG_USERAUTH_REQUEST: u8 = 50;
const MSG_USERAUTH_FAILURE: u8 = 51;
const MSG_USERAUTH_SUCCESS: u8 = 52;
const MSG_USERAUTH_BANNER: u8 = 53;
const MSG_USERAUTH_INFO_REQUEST: u8 = 60;
const MSG_USERAUTH_INFO_RESPONSE: u8 = 61;
const MSG_GLOBAL_REQUEST: u8 = 80;
const MSG_REQUEST_FAILURE: u8 = 82;
const MSG_CHANNEL_OPEN: u8 = 90;
const MSG_CHANNEL_OPEN_CONFIRMATION: u8 = 91;
const MSG_CHANNEL_OPEN_FAILURE: u8 = 92;
const MSG_CHANNEL_WINDOW_ADJUST: u8 = 93;
const MSG_CHANNEL_DATA: u8 = 94;
const MSG_CHANNEL_EXTENDED_DATA: u8 = 95;
const MSG_CHANNEL_EOF: u8 = 96;
const MSG_CHANNEL_CLOSE: u8 = 97;
const MSG_CHANNEL_REQUEST: u8 = 98;
const MSG_CHANNEL_SUCCESS: u8 = 99;
const MSG_CHANNEL_FAILURE: u8 = 100;

// ---------------------------------------------------------------------------
// wire encoding
// ---------------------------------------------------------------------------

struct Writer {
    buf: Vec<u8>,
}

impl Writer {
    fn new() -> Self {
        Self { buf: Vec::new() }
    }
    fn byte(&mut self, value: u8) {
        self.buf.push(value);
    }
    fn u32(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_be_bytes());
    }
    fn raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }
    fn string(&mut self, bytes: &[u8]) {
        self.u32(bytes.len() as u32);
        self.raw(bytes);
    }
    fn names(&mut self, list: &[&str]) {
        self.string(list.join(",").as_bytes());
    }
    fn into_inner(self) -> Vec<u8> {
        self.buf
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

type ReadResult<T> = Result<T, String>;

impl<'a> Reader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }
    fn take(&mut self, count: usize) -> ReadResult<&'a [u8]> {
        if self.pos + count > self.data.len() {
            return Err("SSH 报文长度异常".to_string());
        }
        let slice = &self.data[self.pos..self.pos + count];
        self.pos += count;
        Ok(slice)
    }
    fn byte(&mut self) -> ReadResult<u8> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> ReadResult<u32> {
        Ok(u32::from_be_bytes(
            self.take(4)?.try_into().map_err(|_| "SSH 报文长度异常".to_string())?,
        ))
    }
    fn string(&mut self) -> ReadResult<&'a [u8]> {
        let len = self.u32()? as usize;
        if len > self.data.len() {
            return Err("SSH 报文长度异常".to_string());
        }
        self.take(len)
    }
    fn string_owned(&mut self) -> ReadResult<Vec<u8>> {
        Ok(self.string()?.to_vec())
    }
    fn rest(&self) -> &'a [u8] {
        &self.data[self.pos..]
    }
}

/// SSH `mpint`: two's complement big-endian, with a leading zero when the high
/// bit of the first byte is set.
fn mpint_from(value: &[u8]) -> Vec<u8> {
    let mut bytes = value.to_vec();
    while bytes.first() == Some(&0) && bytes.len() > 1 {
        bytes.remove(0);
    }
    if bytes.first().map(|b| b & 0x80 != 0).unwrap_or(false) {
        bytes.insert(0, 0);
    }
    bytes
}

fn biguint_from_mpint(value: &[u8]) -> BigUint {
    let mut bytes = value.to_vec();
    while bytes.first() == Some(&0) {
        bytes.remove(0);
    }
    BigUint::from_bytes_be(&bytes)
}

// ---------------------------------------------------------------------------
// AES-CTR
// ---------------------------------------------------------------------------

enum Aes {
    Aes128(aes::Aes128Enc),
    Aes256(aes::Aes256Enc),
}

impl Aes {
    fn new(key: &[u8]) -> ReadResult<Self> {
        match key.len() {
            16 => Ok(Aes::Aes128(
                aes::Aes128Enc::new_from_slice(key).map_err(|_| "AES-128 密钥无效".to_string())?,
            )),
            32 => Ok(Aes::Aes256(
                aes::Aes256Enc::new_from_slice(key).map_err(|_| "AES-256 密钥无效".to_string())?,
            )),
            _ => Err("不支持的 SSH 加密密钥长度".to_string()),
        }
    }
    fn encrypt_block(&self, block: &mut [u8; 16]) {
        let mut generic = GenericArray::clone_from_slice(block);
        match self {
            Aes::Aes128(cipher) => cipher.encrypt_block(&mut generic),
            Aes::Aes256(cipher) => cipher.encrypt_block(&mut generic),
        }
        block.copy_from_slice(&generic);
    }
}

struct CtrStream {
    cipher: Aes,
    base: u128,
    pos: u64,
    cache_index: u64,
    cache_valid: bool,
    cache: [u8; 16],
}

impl CtrStream {
    fn new(cipher: Aes, iv: &[u8]) -> Self {
        let mut initial = [0u8; 16];
        let copy = min(16, iv.len());
        initial[..copy].copy_from_slice(&iv[..copy]);
        Self {
            cipher,
            base: u128::from_be_bytes(initial),
            pos: 0,
            cache_index: 0,
            cache_valid: false,
            cache: [0u8; 16],
        }
    }

    fn fill(&mut self, index: u64) {
        self.cache = self.base.wrapping_add(index as u128).to_be_bytes();
        self.cipher.encrypt_block(&mut self.cache);
        self.cache_index = index;
        self.cache_valid = true;
    }

    fn apply(&mut self, data: &mut [u8]) {
        let mut offset = 0usize;
        while offset < data.len() {
            let index = self.pos / 16;
            let within = (self.pos % 16) as usize;
            if !self.cache_valid || index != self.cache_index {
                self.fill(index);
            }
            let count = min(16 - within, data.len() - offset);
            for slot in 0..count {
                data[offset + slot] ^= self.cache[within + slot];
            }
            self.pos += count as u64;
            offset += count;
        }
    }
}

// ---------------------------------------------------------------------------
// key derivation
// ---------------------------------------------------------------------------

fn kdf(shared: &[u8], hash: &[u8], letter: u8, session_id: &[u8], need: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let mut block = {
        let mut digest = Sha256::new();
        digest.update(shared);
        digest.update(hash);
        digest.update([letter]);
        digest.update(session_id);
        digest.finalize().to_vec()
    };
    out.extend_from_slice(&block);
    while out.len() < need {
        let mut digest = Sha256::new();
        digest.update(shared);
        digest.update(hash);
        digest.update(&out);
        block = digest.finalize().to_vec();
        out.extend_from_slice(&block);
    }
    out.truncate(need);
    out
}

// ---------------------------------------------------------------------------
// host key verification
// ---------------------------------------------------------------------------

struct RsaHostKey {
    exponent: BigUint,
    modulus: BigUint,
}

fn parse_rsa_host_key(blob: &[u8]) -> ReadResult<RsaHostKey> {
    let mut reader = Reader::new(blob);
    let algorithm = String::from_utf8_lossy(reader.string()?).to_string();
    if !matches!(algorithm.as_str(), "ssh-rsa" | "rsa-sha2-256" | "rsa-sha2-512") {
        return Err(format!("远端使用了暂不支持的主机密钥类型：{algorithm}"));
    }
    let exponent = biguint_from_mpint(reader.string()?);
    let modulus = biguint_from_mpint(reader.string()?);
    let zero = BigUint::from(0u8);
    if modulus == zero || exponent == zero {
        return Err("远端主机密钥无效".to_string());
    }
    Ok(RsaHostKey { exponent, modulus })
}

fn digest_info_prefix(algorithm: &str) -> Option<(Vec<u8>, Vec<u8>)> {
    match algorithm {
        "rsa-sha2-256" => Some((
            vec![
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x01, 0x05, 0x00, 0x04, 0x20,
            ],
            Sha256::digest([]).to_vec(),
        )),
        "rsa-sha2-512" => Some((
            vec![
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x03, 0x05, 0x00, 0x04, 0x40,
            ],
            Sha512::digest([]).to_vec(),
        )),
        _ => None,
    }
}

fn rsa_verify(key: &RsaHostKey, algorithm: &str, signed: &[u8], signature: &[u8]) -> ReadResult<()> {
    let expected = match algorithm {
        "rsa-sha2-256" => Sha256::digest(signed).to_vec(),
        "rsa-sha2-512" => Sha512::digest(signed).to_vec(),
        other => return Err(format!("不支持的主机密钥签名算法：{other}")),
    };
    let (prefix, _) = digest_info_prefix(algorithm).ok_or("不支持的主机密钥签名算法")?;

    let value = biguint_from_mpint(signature);
    if value >= key.modulus {
        return Err("主机签名校验失败".to_string());
    }
    let decoded = value.modpow(&key.exponent, &key.modulus).to_bytes_be();
    let modulus_len = (key.modulus.bits() + 7) / 8;
    let mut encoded = vec![0u8; modulus_len];
    let copy_at = modulus_len.saturating_sub(decoded.len());
    let copy_len = min(decoded.len(), modulus_len);
    encoded[copy_at..copy_at + copy_len].copy_from_slice(&decoded[decoded.len() - copy_len..]);

    // PKCS#1 v1.5: 0x00 0x01 0xFF... 0x00 DigestInfo
    if encoded.len() < 11 || encoded[0] != 0x00 || encoded[1] != 0x01 {
        return Err("主机签名格式无效".to_string());
    }
    let mut index = 2;
    let mut padding = 0usize;
    while index < encoded.len() && encoded[index] == 0xff {
        padding += 1;
        index += 1;
    }
    if padding < 8 || index >= encoded.len() || encoded[index] != 0x00 {
        return Err("主机签名格式无效".to_string());
    }
    let mut expected_block = prefix.clone();
    expected_block.extend_from_slice(&expected);
    if encoded[index + 1..] != expected_block[..] {
        return Err("主机签名校验失败".to_string());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// known hosts
// ---------------------------------------------------------------------------

fn host_entry(host: &str, port: u16) -> String {
    if port == 22 {
        host.to_string()
    } else {
        format!("[{host}]:{port}")
    }
}

fn known_hosts_lookup(path: &Path, entry: &str) -> Option<Vec<u8>> {
    let content = std::fs::read_to_string(path).ok()?;
    for line in content.lines() {
        let mut parts = line.split_whitespace();
        let recorded = parts.next()?;
        let payload = parts.next()?;
        if recorded == entry {
            return base64_decode(payload).ok();
        }
    }
    None
}

fn known_hosts_remember(path: &Path, entry: &str, key_blob: &[u8]) {
    use base64::Engine;
    let encoded = base64::engine::general_purpose::STANDARD.encode(key_blob);
    let mut lines: Vec<String> = std::fs::read_to_string(path)
        .unwrap_or_default()
        .lines()
        .filter(|line| line.split_whitespace().next() != Some(entry))
        .map(|line| line.to_string())
        .collect();
    lines.push(format!("{entry} {encoded}"));
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(path, lines.join("\n") + "\n");
}

pub fn purge_host_key(path: &Path, host: &str, port: u16) {
    let entry = host_entry(host, port);
    let Ok(content) = std::fs::read_to_string(path) else {
        return;
    };
    let kept: Vec<&str> = content
        .lines()
        .filter(|line| line.split_whitespace().next() != Some(entry.as_str()))
        .collect();
    let _ = std::fs::write(path, kept.join("\n") + "\n");
}

fn base64_decode(value: &str) -> ReadResult<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| "凭据内容损坏".to_string())
}

fn fingerprint(blob: &[u8]) -> String {
    use base64::Engine;
    let digest = Sha256::digest(blob);
    base64::engine::general_purpose::STANDARD
        .encode(digest)
        .trim_end_matches('=')
        .to_string()
}

// ---------------------------------------------------------------------------
// client
// ---------------------------------------------------------------------------

pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub status: i32,
}

pub struct Client {
    stream: TcpStream,
    timeout: Duration,
    outgoing: Option<CtrStream>,
    incoming: Option<CtrStream>,
    mac_out: Vec<u8>,
    mac_in: Vec<u8>,
    out_seq: u32,
    in_seq: u32,
    next_channel: u32,
}

impl Client {
    pub async fn connect(
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        known_hosts: &Path,
        timeout: Duration,
    ) -> ReadResult<Self> {
        let address = format!("{host}:{port}");
        let stream = tokio::time::timeout(timeout, TcpStream::connect(address))
            .await
            .map_err(|_| format!("连接 {host}:{port} 超时"))?
            .map_err(|err| format!("连接 {host}:{port} 失败：{err}"))?;
        stream
            .set_nodelay(true)
            .map_err(|err| format!("配置 SSH 连接失败：{err}"))?;

        let mut client = Self {
            stream,
            timeout,
            outgoing: None,
            incoming: None,
            mac_out: Vec::new(),
            mac_in: Vec::new(),
            out_seq: 0,
            in_seq: 0,
            next_channel: 0,
        };

        client.handshake(host, port, user, password, known_hosts).await?;
        Ok(client)
    }

    // -- raw io ------------------------------------------------------------

    async fn read_exact(&mut self, buffer: &mut [u8]) -> ReadResult<()> {
        tokio::time::timeout(self.timeout, self.stream.read_exact(buffer))
            .await
            .map_err(|_| "SSH 连接超时".to_string())?
            .map_err(|err| format!("SSH 连接中断：{err}"))?;
        Ok(())
    }

    async fn write_all(&mut self, bytes: &[u8]) -> ReadResult<()> {
        tokio::time::timeout(self.timeout, self.stream.write_all(bytes))
            .await
            .map_err(|_| "SSH 写入超时".to_string())?
            .map_err(|err| format!("SSH 写入失败：{err}"))?;
        let _ = self.stream.flush().await;
        Ok(())
    }

    fn mac(&self, key: &[u8], seq: u32, packet: &[u8]) -> Vec<u8> {
        let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(key).expect("HMAC 密钥长度固定");
        mac.update(&seq.to_be_bytes());
        mac.update(packet);
        mac.finalize().into_bytes().to_vec()
    }

    async fn send_packet(&mut self, payload: &[u8]) -> ReadResult<()> {
        let mut padding = BLOCK_SIZE - ((payload.len() + 5) % BLOCK_SIZE);
        if padding < 4 {
            padding += BLOCK_SIZE;
        }
        let body_len = 1 + payload.len() + padding;
        if body_len > MAX_PACKET + 1024 {
            return Err("SSH 报文过长".to_string());
        }
        let mut packet = Vec::with_capacity(4 + body_len);
        packet.extend_from_slice(&(body_len as u32).to_be_bytes());
        packet.push(padding as u8);
        packet.extend_from_slice(payload);
        packet.resize(4 + body_len, 0u8);

        let mac = if self.outgoing.is_some() {
            let seq = self.out_seq;
            Some(self.mac(&self.mac_out.clone(), seq, &packet))
        } else {
            None
        };
        if let Some(cipher) = self.outgoing.as_mut() {
            cipher.apply(&mut packet);
        }
        if let Some(mac) = mac {
            packet.extend_from_slice(&mac);
        }
        self.out_seq = self.out_seq.wrapping_add(1);
        self.write_all(&packet).await
    }

    async fn read_packet(&mut self) -> ReadResult<(u8, Vec<u8>)> {
        let mut header = [0u8; 4];
        self.read_exact(&mut header).await?;
        if self.incoming.is_some() {
            let mut cipher = self.incoming.take().unwrap();
            cipher.apply(&mut header);
            self.incoming = Some(cipher);
        }
        let body_len = u32::from_be_bytes(header) as usize;
        if body_len < 2 || body_len > 4 * MAX_PACKET {
            return Err("SSH 报文长度异常".to_string());
        }
        let mut body = vec![0u8; body_len];
        self.read_exact(&mut body).await?;

        let encrypted = self.incoming.is_some();
        if encrypted {
            {
                let mut cipher = self.incoming.take().unwrap();
                cipher.apply(&mut body);
                self.incoming = Some(cipher);
            }
            let mut mac = vec![0u8; MAC_LEN];
            self.read_exact(&mut mac).await?;
            let mut plain = Vec::with_capacity(4 + body_len);
            plain.extend_from_slice(&header);
            plain.extend_from_slice(&body);
            let expected = self.mac(&self.mac_in.clone(), self.in_seq, &plain);
            if expected != mac {
                return Err("SSH 报文校验失败".to_string());
            }
        }
        self.in_seq = self.in_seq.wrapping_add(1);

        let padding_len = body[0] as usize;
        if padding_len < 4 || padding_len + 1 > body.len() {
            return Err("SSH 报文填充异常".to_string());
        }
        let payload = body[1..body.len() - padding_len].to_vec();
        let kind = payload[0];
                Ok((kind, payload[1..].to_vec()))
    }

    /// Read packets until one matches `accept`, discarding noise (ignore /
    /// debug / unimplemented) and surfacing disconnects.
    async fn read_until(&mut self, accept: &[u8]) -> ReadResult<(u8, Vec<u8>)> {
        loop {
            let (kind, payload) = self.read_packet().await?;
            if accept.contains(&kind) {
                return Ok((kind, payload));
            }
            match kind {
                MSG_IGNORE | MSG_DEBUG | MSG_UNIMPLEMENTED => continue,
                // OpenSSH probes with global requests (hostkeys-prove,
                // no-more-sessions) right after authentication.
                MSG_GLOBAL_REQUEST => {
                    self.send_packet(&[MSG_REQUEST_FAILURE]).await?;
                    continue;
                }
                MSG_DISCONNECT => {
                    let mut description = String::new();
                    let mut reader = Reader::new(&payload);
                    if let Ok(code) = reader.u32() {
                        let _ = code;
                        description = String::from_utf8_lossy(reader.rest()).to_string();
                    }
                    return Err(if description.trim().is_empty() {
                        "远端主动断开连接".to_string()
                    } else {
                        format!("远端主动断开连接：{}", description.trim())
                    });
                }
                other => return Err(format!("SSH 协议异常：收到消息 {other}")),
            }
        }
    }

    // -- handshake ---------------------------------------------------------

    async fn handshake(
        &mut self,
        host: &str,
        port: u16,
        user: &str,
        password: &str,
        known_hosts: &Path,
    ) -> ReadResult<()> {
        // Version exchange.
        self.write_all(format!("{CLIENT_VERSION}\r\n").as_bytes())
            .await?;
                let server_version = self.read_version_line().await?;
                let client_version = CLIENT_VERSION.to_string();

        // KEXINIT.
        let kexinit = {
            let mut writer = Writer::new();
            writer.byte(MSG_KEXINIT);
            writer.raw(&[0u8; 16]);
            writer.names(&["curve25519-sha256", "curve25519-sha256@libssh.org"]);
            writer.names(&["rsa-sha2-512", "rsa-sha2-256"]);
            writer.names(&["aes128-ctr", "aes256-ctr"]);
            writer.names(&["aes128-ctr", "aes256-ctr"]);
            writer.names(&["hmac-sha2-256"]);
            writer.names(&["hmac-sha2-256"]);
            writer.names(&["none"]);
            writer.names(&["none"]);
            writer.names(&[] as &[&str]);
            writer.names(&[] as &[&str]);
            writer.byte(0);
            writer.u32(0);
            writer.into_inner()
        };
        self.send_packet(&kexinit).await?;

        let (_, server_kexinit_payload) = self.read_until(&[MSG_KEXINIT]).await?;
        let mut server_kexinit = Vec::new();
        server_kexinit.push(MSG_KEXINIT);
        server_kexinit.extend_from_slice(&server_kexinit_payload);

        let mut reader = Reader::new(&server_kexinit_payload);
        let _cookie = reader.take(16)?;
        let server_kex = split_names(reader.string()?);
        let server_host_keys = split_names(reader.string()?);
        let server_cipher_c2s = split_names(reader.string()?);
        let server_cipher_s2c = split_names(reader.string()?);
        let server_mac_c2s = split_names(reader.string()?);
        let server_mac_s2c = split_names(reader.string()?);

        if !server_kex
            .iter()
            .any(|name| name == "curve25519-sha256" || name == "curve25519-sha256@libssh.org")
        {
            return Err("远端不支持 curve25519-sha256 密钥交换".to_string());
        }
        let host_key_algorithm = ["rsa-sha2-512", "rsa-sha2-256"]
            .iter()
            .find(|name| server_host_keys.contains(&name.to_string()))
            .ok_or_else(|| "远端不支持 RSA 主机密钥（rsa-sha2-256/512）".to_string())?
            .to_string();
        let cipher_name = ["aes128-ctr", "aes256-ctr"]
            .iter()
            .find(|name| {
                server_cipher_c2s.contains(&name.to_string())
                    && server_cipher_s2c.contains(&name.to_string())
            })
            .ok_or_else(|| "远端不支持 AES-CTR 加密".to_string())?
            .to_string();
        if !server_mac_c2s.iter().any(|name| name == "hmac-sha2-256")
            || !server_mac_s2c.iter().any(|name| name == "hmac-sha2-256")
        {
            return Err("远端不支持 hmac-sha2-256 校验".to_string());
        }

        // ECDH.
        let rng = ring::rand::SystemRandom::new();
        let private_key = ring::agreement::EphemeralPrivateKey::generate(
            &ring::agreement::X25519,
            &rng,
        )
        .map_err(|_| "生成 SSH 密钥交换参数失败".to_string())?;
        let public_key = private_key
            .compute_public_key()
            .map_err(|_| "生成 SSH 密钥交换参数失败".to_string())?;
        let client_public: Vec<u8> = public_key.as_ref().to_vec();

        let mut init = Writer::new();
        init.byte(MSG_KEX_ECDH_INIT);
        init.string(&client_public);
        self.send_packet(&init.into_inner()).await?;

        let (_, reply) = self.read_until(&[MSG_KEX_ECDH_REPLY]).await?;
        let mut reader = Reader::new(&reply);
        let key_blob = reader.string_owned()?;
        let server_public = reader.string_owned()?;
        let signature_blob = reader.string_owned()?;

        let shared: Vec<u8> = ring::agreement::agree_ephemeral(
            private_key,
            &ring::agreement::UnparsedPublicKey::new(&ring::agreement::X25519, &server_public),
            |secret| secret.to_vec(),
        )
        .map_err(|_| "SSH 密钥交换失败".to_string())?;

        let host_key = parse_rsa_host_key(&key_blob)?;
        let entry = host_entry(host, port);
        match known_hosts_lookup(known_hosts, &entry) {
            Some(recorded) if recorded != key_blob => {
                return Err(format!(
                    "主机密钥已变更（{}），已阻止连接以免中间人攻击；请在服务器上确认后清除记录重试",
                    fingerprint(&key_blob)
                ));
            }
            Some(_) => {}
            None => known_hosts_remember(known_hosts, &entry, &key_blob),
        }

        let mut exchange_hash = Writer::new();
        exchange_hash.string(client_version.as_bytes());
        exchange_hash.string(server_version.as_bytes());
        exchange_hash.string(&kexinit);
        exchange_hash.string(&server_kexinit);
        exchange_hash.string(&key_blob);
        exchange_hash.string(&client_public);
        exchange_hash.string(&server_public);
        exchange_hash.string(&mpint_from(&shared));
        let hash = Sha256::digest(exchange_hash.into_inner()).to_vec();

        let mut signature_reader = Reader::new(&signature_blob);
        let signature_algorithm = String::from_utf8_lossy(signature_reader.string()?).to_string();
        let signature = signature_reader.string_owned()?;
        rsa_verify(&host_key, &signature_algorithm, &hash, &signature)
            .map_err(|err| format!("{err}（{host_key_algorithm}）"))?;

        let session_id = hash.clone();
        // The key derivation feeds K as a length-prefixed mpint (K || H || X ||
        // session_id), not as the raw X25519 output.
        let shared_mpint = mpint_from(&shared);
        let mut shared_encoded = (shared_mpint.len() as u32).to_be_bytes().to_vec();
        shared_encoded.extend_from_slice(&shared_mpint);
                        let key_len = if cipher_name == "aes256-ctr" { 32 } else { 16 };
        let iv_c = kdf(&shared_encoded, &hash, b'A', &session_id, 16);
        let iv_s = kdf(&shared_encoded, &hash, b'B', &session_id, 16);
        let key_c = kdf(&shared_encoded, &hash, b'C', &session_id, key_len);
        let key_s = kdf(&shared_encoded, &hash, b'D', &session_id, key_len);
        let mac_c = kdf(&shared_encoded, &hash, b'E', &session_id, 32);
        let mac_s = kdf(&shared_encoded, &hash, b'F', &session_id, 32);

        self.send_packet(&[MSG_NEWKEYS]).await?;
        self.outgoing = Some(CtrStream::new(Aes::new(&key_c)?, &iv_c));
        self.mac_out = mac_c;
        self.read_until(&[MSG_NEWKEYS]).await?;
        self.incoming = Some(CtrStream::new(Aes::new(&key_s)?, &iv_s));
        self.mac_in = mac_s;

        
        // Service + userauth.
        let mut service = Writer::new();
        service.byte(MSG_SERVICE_REQUEST);
        service.string(b"ssh-userauth");
        self.send_packet(&service.into_inner()).await?;
                self.read_until(&[MSG_SERVICE_ACCEPT]).await?;
        
        self.authenticate(user, password).await
    }

    async fn read_version_line(&mut self) -> ReadResult<String> {
        let mut buffer: Vec<u8> = Vec::new();
        let mut byte = [0u8; 1];
        loop {
            self.read_exact(&mut byte).await?;
            if byte[0] == b'\n' {
                let line = String::from_utf8_lossy(&buffer).trim_end().to_string();
                if line.starts_with("SSH-2.0-") || line.starts_with("SSH-1.99-") {
                    return Ok(line);
                }
                if buffer.len() > 4096 {
                    return Err("SSH 协议握手失败：未收到版本标识".to_string());
                }
                buffer.clear();
                continue;
            }
            if byte[0] != b'\r' {
                buffer.push(byte[0]);
            }
            if buffer.len() > 4096 {
                return Err("SSH 协议握手失败：未收到版本标识".to_string());
            }
        }
    }

    async fn authenticate(&mut self, user: &str, password: &str) -> ReadResult<()> {
        let mut request = Writer::new();
        request.byte(MSG_USERAUTH_REQUEST);
        request.string(user.as_bytes());
        request.string(b"ssh-connection");
        request.string(b"password");
        request.byte(0);
        request.string(password.as_bytes());
        self.send_packet(&request.into_inner()).await?;

        loop {
            let (kind, payload) = self
                .read_until(&[
                    MSG_USERAUTH_SUCCESS,
                    MSG_USERAUTH_FAILURE,
                    MSG_USERAUTH_BANNER,
                    MSG_USERAUTH_INFO_REQUEST,
                ])
                .await?;
            match kind {
                MSG_USERAUTH_SUCCESS => return Ok(()),
                MSG_USERAUTH_BANNER => continue,
                MSG_USERAUTH_FAILURE => {
                    let mut reader = Reader::new(&payload);
                    let methods = String::from_utf8_lossy(reader.string()?).to_string();
                    if methods.contains("keyboard-interactive") {
                        return self.authenticate_interactive(user, password).await;
                    }
                    return Err("SSH 认证失败，请检查用户名、密码或私钥".to_string());
                }
                MSG_USERAUTH_INFO_REQUEST => {
                    self.answer_info_request(&payload, password).await?;
                    return self.authenticate_finish().await;
                }
                _ => unreachable!(),
            }
        }
    }

    async fn authenticate_interactive(&mut self, user: &str, password: &str) -> ReadResult<()> {
        let mut request = Writer::new();
        request.byte(MSG_USERAUTH_REQUEST);
        request.string(user.as_bytes());
        request.string(b"ssh-connection");
        request.string(b"keyboard-interactive");
        request.string(b"");
        request.string(b"");
        self.send_packet(&request.into_inner()).await?;
        loop {
            let (kind, payload) = self
                .read_until(&[
                    MSG_USERAUTH_SUCCESS,
                    MSG_USERAUTH_FAILURE,
                    MSG_USERAUTH_BANNER,
                    MSG_USERAUTH_INFO_REQUEST,
                ])
                .await?;
            match kind {
                MSG_USERAUTH_SUCCESS => return Ok(()),
                MSG_USERAUTH_BANNER => continue,
                MSG_USERAUTH_FAILURE => {
                    return Err("SSH 认证失败，请检查用户名、密码或私钥".to_string())
                }
                MSG_USERAUTH_INFO_REQUEST => {
                    self.answer_info_request(&payload, password).await?;
                }
                _ => unreachable!(),
            }
        }
    }

    async fn answer_info_request(&mut self, payload: &[u8], password: &str) -> ReadResult<()> {
        let mut reader = Reader::new(payload);
        let _name = reader.string()?;
        let _instruction = reader.string()?;
        let _language = reader.string()?;
        let count = reader.u32()?;
        let mut response = Writer::new();
        response.byte(MSG_USERAUTH_INFO_RESPONSE);
        response.u32(count);
        for _ in 0..count {
            let _prompt = reader.string()?;
            let _echo = reader.byte()?;
            response.string(password.as_bytes());
        }
        self.send_packet(&response.into_inner()).await
    }

    async fn authenticate_finish(&mut self) -> ReadResult<()> {
        loop {
            let (kind, _payload) = self
                .read_until(&[
                    MSG_USERAUTH_SUCCESS,
                    MSG_USERAUTH_FAILURE,
                    MSG_USERAUTH_BANNER,
                    MSG_USERAUTH_INFO_REQUEST,
                ])
                .await?;
            match kind {
                MSG_USERAUTH_SUCCESS => return Ok(()),
                MSG_USERAUTH_BANNER => continue,
                MSG_USERAUTH_INFO_REQUEST => {
                    return Err("SSH 认证失败，请检查用户名、密码或私钥".to_string())
                }
                _ => return Err("SSH 认证失败，请检查用户名、密码或私钥".to_string()),
            }
        }
    }

    // -- channels ----------------------------------------------------------

    async fn open_channel(&mut self) -> ReadResult<(u32, u32)> {
        let local = self.next_channel;
        self.next_channel += 1;
        let mut open = Writer::new();
        open.byte(MSG_CHANNEL_OPEN);
        open.string(b"session");
        open.u32(local);
        open.u32(CHANNEL_WINDOW);
        open.u32(CHANNEL_MAX_PACKET);
        self.send_packet(&open.into_inner()).await?;

        let (kind, payload) = self
            .read_until(&[MSG_CHANNEL_OPEN_CONFIRMATION, MSG_CHANNEL_OPEN_FAILURE])
            .await?;
        if kind == MSG_CHANNEL_OPEN_FAILURE {
            let mut reader = Reader::new(&payload);
            let _recipient = reader.u32()?;
            let reason = reader.u32()?;
            return Err(format!("远端拒绝打开会话通道（代码 {reason}）"));
        }
        let mut reader = Reader::new(&payload);
        let _recipient = reader.u32()?;
        let remote = reader.u32()?;
        Ok((local, remote))
    }

    pub async fn exec(&mut self, command: &str, stdin: &[u8]) -> ReadResult<ExecResult> {
        let (_local, remote) = self.open_channel().await?;

        let mut request = Writer::new();
        request.byte(MSG_CHANNEL_REQUEST);
        request.u32(remote);
        request.string(b"exec");
        request.byte(1);
        request.string(command.as_bytes());
        self.send_packet(&request.into_inner()).await?;

        // Some servers emit output before the "exec" success reply, so the
        // request handshake and the data pump share one loop.
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut status: Option<i32> = None;
        let mut closed = false;
        let mut started = false;
        let mut failed = false;
        let deadline = tokio::time::Instant::now() + self.timeout;

        while !(closed && status.is_some()) {
            let wait = deadline.saturating_duration_since(tokio::time::Instant::now());
            if wait.is_zero() {
                return Err("执行远端命令超时".to_string());
            }
            let (kind, payload) = match tokio::time::timeout(wait, self.read_packet()).await {
                Ok(result) => result?,
                Err(_) => return Err("执行远端命令超时".to_string()),
            };
            match kind {
                MSG_CHANNEL_SUCCESS => {
                    if !started {
                        started = true;
                        if !stdin.is_empty() {
                            let mut remaining = stdin;
                            while !remaining.is_empty() {
                                let chunk =
                                    min(CHANNEL_MAX_PACKET as usize - 1024, remaining.len());
                                let mut data = Writer::new();
                                data.byte(MSG_CHANNEL_DATA);
                                data.u32(remote);
                                data.string(&remaining[..chunk]);
                                self.send_packet(&data.into_inner()).await?;
                                remaining = &remaining[chunk..];
                            }
                        }
                        let mut eof = Writer::new();
                        eof.byte(MSG_CHANNEL_EOF);
                        eof.u32(remote);
                        self.send_packet(&eof.into_inner()).await?;
                    }
                }
                MSG_CHANNEL_FAILURE => failed = true,
                MSG_CHANNEL_DATA | MSG_CHANNEL_EXTENDED_DATA => {
                    let mut reader = Reader::new(&payload);
                    let _recipient = reader.u32()?;
                    if kind == MSG_CHANNEL_EXTENDED_DATA {
                        let _stream = reader.u32()?;
                    }
                    let data = reader.string_owned()?;
                    if kind == MSG_CHANNEL_DATA {
                        stdout.extend_from_slice(&data);
                    } else {
                        stderr.extend_from_slice(&data);
                    }
                    self.adjust_window(remote, data.len() as u32).await?;
                }
                MSG_CHANNEL_EOF => {}
                MSG_CHANNEL_CLOSE => {
                    closed = true;
                    if status.is_some() {
                        break;
                    }
                }
                MSG_CHANNEL_WINDOW_ADJUST => {}
                MSG_CHANNEL_REQUEST => {
                    let mut reader = Reader::new(&payload);
                    let _recipient = reader.u32()?;
                    let name = String::from_utf8_lossy(reader.string()?).to_string();
                    let _want_reply = reader.byte()?;
                    if name == "exit-status" {
                        status = Some(reader.u32()? as i32);
                    }
                }
                MSG_DISCONNECT => {
                    return Err("远端在命令执行期间断开连接".to_string());
                }
                MSG_IGNORE | MSG_DEBUG | MSG_UNIMPLEMENTED => {}
                MSG_GLOBAL_REQUEST => {
                    self.send_packet(&[MSG_REQUEST_FAILURE]).await?;
                }
                _ => {}
            }
        }

        let _ = self.close_channel(remote).await;
        if failed && stdout.is_empty() && status.unwrap_or(0) != 0 {
            return Err(format!("远端拒绝执行命令：{}", truncate(command)));
        }
        Ok(ExecResult {
            stdout: String::from_utf8_lossy(&stdout).to_string(),
            stderr: String::from_utf8_lossy(&stderr).to_string(),
            status: status.unwrap_or(0),
        })
    }

    async fn adjust_window(&mut self, remote: u32, consumed: u32) -> ReadResult<()> {
        if consumed < CHANNEL_WINDOW / 2 {
            return Ok(());
        }
        let mut adjust = Writer::new();
        adjust.byte(MSG_CHANNEL_WINDOW_ADJUST);
        adjust.u32(remote);
        adjust.u32(consumed);
        self.send_packet(&adjust.into_inner()).await
    }

    async fn close_channel(&mut self, remote: u32) -> ReadResult<()> {
        let mut close = Writer::new();
        close.byte(MSG_CHANNEL_CLOSE);
        close.u32(remote);
        self.send_packet(&close.into_inner()).await
    }

    pub async fn close(&mut self) {
        let _ = self.stream.shutdown().await;
    }
}

fn split_names(raw: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(raw)
        .split(',')
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string())
        .collect()
}

fn truncate(value: &str) -> String {
    if value.len() > 120 {
        format!("{}...", &value[..120])
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    /// 环境变量驱动的真机连通性测试，默认忽略：
    /// NETWORK_MANAGER_SSH_PROBE=host:port:user:password cargo test --offline probe_real -- --ignored
    #[tokio::test]
    #[ignore = "需要真实 SSH 服务器，通过 NETWORK_MANAGER_SSH_PROBE 环境变量启用"]
    async fn probe_real_server() {
        let target = std::env::var("NETWORK_MANAGER_SSH_PROBE").expect("设置 NETWORK_MANAGER_SSH_PROBE=host:port:user:password");
        let parts: Vec<&str> = target.splitn(4, ':').collect();
        assert_eq!(parts.len(), 4, "格式应为 host:port:user:password");
        let path = crate::paths::ssh_known_hosts_path();
        let started = std::time::Instant::now();
        let mut client = super::Client::connect(
            parts[0],
            parts[1].parse().unwrap_or(22),
            parts[2],
            parts[3],
            &path,
            std::time::Duration::from_secs(20),
        )
        .await
        .expect("连接/认证失败");
        println!("握手+认证耗时 {:?}", started.elapsed());
        let result = client.exec("echo HELLO; id -u", &[]).await.expect("exec 失败");
        println!(
            "exec status={} stdout={:?} stderr={:?}",
            result.status,
            result.stdout, result.stderr
        );
        client.close().await;
    }
}
