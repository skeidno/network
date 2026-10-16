//! 本地 CA 与叶证书的自动生成。
//!
//! 「局域网里用域名访问、HTTPS 还不报红」由两件事组成：域名解析要对（本地 DNS 映射），
//! 证书要被信任、名字还要对得上。后者以前得用户自己开 openssl 签一张，还要自己记着
//! 装根证书。这里把它做进程序：按当前映射表自动维护一套「私有根 CA + 叶证书」，
//! 域名一改就重签叶证书，用户只需要决定「要不要信任这个根」。
//!
//! 两条硬约束来自 Chrome Root Store（Edge / Chrome 同源，Android 上的 Chromium 同理）：
//!
//! 1. **叶证书有效期不得超过 398 天**，而且这条对「本机私有根签发的证书」一样生效 ——
//!    超一天就是 `NET::ERR_CERT_VALIDITY_TOO_LONG`。实测 825 天、800 天都会被拦，
//!    所以这里取 397 天，留一天余量。
//! 2. **SAN 必须逐个列名字**。X.509 的通配只吃最左一个标签，`*.baidu.com` 不匹配裸域
//!    `baidu.com`；映射里写了通配就把两个都写进 SAN，否则访问裸域时报名字不匹配。
//!
//! 根证书本身不在 398 天限制内（限制只作用于叶证书），所以给十年，省得用户反复重装信任。
//! 私钥只落在本机数据目录里，不参与导出、不写进「跨设备配置」——它是给这台机器上的
//! 本地服务用的，跟着配置满世界走反而是风险。

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[cfg(windows)]
use std::sync::{Mutex, OnceLock};
#[cfg(windows)]
use std::time::{Duration as StdDuration, Instant};

use base64::Engine as _;
use chrono::{Datelike, Duration, Utc};
use rcgen::{
    date_time_ymd, BasicConstraints, CertificateParams, DistinguishedName, DnType,
    ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair, KeyUsagePurpose,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::models::AppConfig;
use crate::paths;

/// 叶证书有效期。上限是 398 天，留一天余量。
pub const LEAF_VALIDITY_DAYS: i64 = 397;

/// 私有根有效期。398 天限制只作用于叶证书，根给十年。
pub const CA_VALIDITY_DAYS: i64 = 3650;

/// 叶证书剩余不足这么多天就提前重签，避免用户用着用着突然报过期。
const RENEW_BEFORE_DAYS: i64 = 30;

/// 叶证书里「扩展怎么写」的版本号。
///
/// 改了扩展的写法（比如补上 AKI、去掉 EC 不该有的 Key Encipherment）就加一，
/// 老证书会被认成过期形态重签一次 —— 光看名字和到期时间是发现不了这种变化的。
/// 落盘字段缺省为 0，所以升级前签的证书自然会被重签。
const LEAF_SCHEMA: u32 = 1;

/// 根证书的名字。装进系统信任库后，用户在证书列表里看到的就是这个名字。
pub const CA_COMMON_NAME: &str = "Network Manager Local CA";

const CA_ORGANIZATION: &str = "Network Manager";
const LEAF_COMMON_NAME: &str = "Network Manager Local Dev";
const META_FILE: &str = "meta.json";

// ---------------------------------------------------------------------------
// 路径
// ---------------------------------------------------------------------------

/// 证书目录：`<数据目录>/local-certs/`。
pub fn cert_dir() -> PathBuf {
    paths::app_data_dir().join("local-certs")
}

pub fn ca_cert_path() -> PathBuf {
    cert_dir().join("ca.crt")
}

pub fn ca_key_path() -> PathBuf {
    cert_dir().join("ca.key")
}

pub fn leaf_cert_path() -> PathBuf {
    cert_dir().join("server.crt")
}

pub fn leaf_key_path() -> PathBuf {
    cert_dir().join("server.key")
}

fn meta_path() -> PathBuf {
    cert_dir().join(META_FILE)
}

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path)
        .ok()
        .filter(|text| !text.trim().is_empty())
}

/// 落到本地状态文件里的信息。
///
/// 不解析证书本身（那要额外拉一个 X.509 解析库），而是把「签的时候就知道」的
/// 事实记下来：指纹、覆盖的名字、到期时间。判断要不要重签只看这个文件就够了。
#[derive(serde::Serialize, serde::Deserialize, Default)]
struct Meta {
    ca_fingerprint: String,
    leaf_fingerprint: String,
    /// 叶证书里实际写进 SAN 的名字（DNS 名与 IP 字面量）。
    sans: Vec<String>,
    /// 同上，Unix 时间戳，用来算剩余天数。
    leaf_not_after_unix: i64,
    /// 叶证书扩展的写法版本，见 [`LEAF_SCHEMA`]。老文件里没有这个字段，按 0 读。
    #[serde(default)]
    leaf_schema: u32,
    /// 叶证书到期日，给人看的写法。
    leaf_not_after: String,
    generated_at: String,
}

fn read_meta() -> Option<Meta> {
    let raw = read_text(&meta_path())?;
    serde_json::from_str(&raw).ok()
}

fn write_secret(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|err| format!("写入 {} 失败：{err}", path.display()))?;
    restrict(path);
    Ok(())
}

fn write_public(path: &Path, contents: &str) -> Result<(), String> {
    fs::write(path, contents).map_err(|err| format!("写入 {} 失败：{err}", path.display()))
}

/// 私钥文件在类 Unix 上收到 0600。
///
/// Windows 上没有对应的位，用的是 ACL —— 数据目录本来就只在当前用户配置目录下，
/// 这里不额外动 ACL，免得给用户留下一个看不懂的安全描述符。
#[cfg(unix)]
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict(_path: &Path) {}

// ---------------------------------------------------------------------------
// 名字推导
// ---------------------------------------------------------------------------

fn push_unique(names: &mut Vec<String>, raw: &str) {
    let value = raw.trim().trim_end_matches('.').to_lowercase();
    if !value.is_empty() && !names.contains(&value) {
        names.push(value);
    }
}

/// 从映射表推出叶证书必须覆盖的名字。
///
/// 通配条目补上裸域：`*.baidu.com` 与 `baidu.com` 在 X.509 里是两个不同的名字，
/// 写通配不会顺带覆盖裸域。地址是字面量 IP 的也塞进去 —— 有的客户端直连 IP 访问时
/// 同样校验 SAN，本地调试经常这么用。
fn required_sans(config: &AppConfig) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for entry in &config.local_dns {
        match entry.wildcard_suffix() {
            Some(suffix) => {
                push_unique(&mut names, &format!("*.{}", suffix.trim_end_matches('.')));
                push_unique(&mut names, suffix);
            }
            None => push_unique(&mut names, &entry.domain),
        }
        if crate::models::parse_ip_literal(&entry.address).is_some() {
            push_unique(&mut names, &entry.address);
        }
    }
    names.sort();
    names
}

// ---------------------------------------------------------------------------
// 生成
// ---------------------------------------------------------------------------

fn fingerprint(der: &[u8]) -> String {
    Sha256::digest(der)
        .iter()
        .map(|byte| format!("{byte:02X}"))
        .collect::<Vec<_>>()
        .join(":")
}

/// PEM → DER。用来给「从磁盘读回来的」CA 算指纹（我们只存 PEM，不存原始 DER）。
fn pem_to_der(pem: &str) -> Option<Vec<u8>> {
    let body: String = pem
        .lines()
        .filter(|line| !line.trim_start().starts_with("-----"))
        .collect();
    if body.trim().is_empty() {
        return None;
    }
    base64::engine::general_purpose::STANDARD
        .decode(body.trim())
        .ok()
}

fn begin_params(common_name: &str) -> CertificateParams {
    let mut params = CertificateParams::default();
    // 默认值会带一个 "rcgen self signed cert" 的 CN，先清掉再写自己的 ——
    // 否则证书里会挂两个 CN，装进信任库后在列表里看着很怪。
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, common_name);
    params
        .distinguished_name
        .push(DnType::OrganizationName, CA_ORGANIZATION);
    params
}

fn ca_params() -> CertificateParams {
    let mut params = begin_params(CA_COMMON_NAME);
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    apply_window(&mut params, CA_VALIDITY_DAYS);
    params
}

fn leaf_params(sans: &[String]) -> Result<CertificateParams, String> {
    // `CertificateParams::new` 会把「长得像 IP」的字符串自动归到 IP SAN，
    // 其余按 DNS 名处理，正好省掉一层手写判断。
    let mut params = CertificateParams::new(sans.to_vec())
        .map_err(|err| format!("证书名字不合法：{err}"))?;
    params.distinguished_name = DistinguishedName::new();
    params
        .distinguished_name
        .push(DnType::CommonName, LEAF_COMMON_NAME);
    params
        .distinguished_name
        .push(DnType::OrganizationName, CA_ORGANIZATION);
    params.is_ca = IsCa::NoCa;
    // EC 密钥只做数字签名。RFC 5480 明确禁止 EC 证书声明 Key Encipherment，
    // 而严格校验的实现（Python 3.13 的 ssl 默认开 VERIFY_X509_STRICT、OpenSSL
    // 的 -x509_strict）会盯着这类越界用途。
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    // RFC 5280 要求 CA 签发的证书带 Authority Key Identifier：链构建要靠它认出
    // 「这张证书是不是这把根签的」。rcgen 默认不写 —— 浏览器宽容，但 OpenSSL /
    // Python 请求库的严格模式会直接判 `Missing Authority Key Identifier`。
    params.use_authority_key_identifier_extension = true;
    apply_window(&mut params, LEAF_VALIDITY_DAYS);
    Ok(params)
}

/// 起止时间：起点取当天 00:00 UTC（对任何时区都已经是过去），终点按天数往后推。
/// 这样有效期正好是 `days` 天，不会因为"起点也是当下"而少算半天。
fn apply_window(params: &mut CertificateParams, days: i64) {
    let today = Utc::now().date_naive();
    let end = today + Duration::days(days);
    params.not_before = date_time_ymd(today.year(), today.month() as u8, today.day() as u8);
    params.not_after = date_time_ymd(end.year(), end.month() as u8, end.day() as u8);
}

/// 叶证书的到期日：给人看的写法 + 用来算剩余天数的时间戳。
fn leaf_not_after() -> (String, i64) {
    let end = Utc::now().date_naive() + Duration::days(LEAF_VALIDITY_DAYS);
    let stamp = date_time_ymd(end.year(), end.month() as u8, end.day() as u8);
    (end.to_string(), stamp.unix_timestamp())
}

/// 生成结果：谁是新造的，谁被重签了。
pub struct Ensured {
    pub ca_created: bool,
    pub leaf_generated: bool,
    pub status: Value,
}

/// 按当前映射表把证书准备到位。
///
/// - 根证书**只在缺失时生成**。它一旦装进过系统信任库，换根就意味着用户要重装一次，
///   所以绝不主动轮换。
/// - 叶证书在「文件缺失 / 覆盖名字变了 / 快到期了 / 不是当前这个根签的」时重签。
pub fn ensure(config: &AppConfig) -> Result<Ensured, String> {
    let dir = cert_dir();
    fs::create_dir_all(&dir).map_err(|err| format!("创建 {} 失败：{err}", dir.display()))?;

    let sans = required_sans(config);
    if sans.is_empty() {
        // 没有映射就没有要覆盖的名字，签一张没有 SAN 的证书毫无意义 ——
        // 浏览器不认，用户也没法拿它服务任何域名。
        return Err("还没有本地 DNS 映射，先加一条映射再生成证书".into());
    }

    let mut ca_created = false;
    let ca_key = match (read_text(&ca_cert_path()), read_text(&ca_key_path())) {
        (Some(_), Some(key_pem)) => {
            KeyPair::from_pem(&key_pem).map_err(|err| format!("读取已有 CA 私钥失败：{err}"))?
        }
        _ => {
            let key = KeyPair::generate().map_err(|err| format!("生成 CA 私钥失败：{err}"))?;
            let cert = ca_params()
                .self_signed(&key)
                .map_err(|err| format!("自签 CA 失败：{err}"))?;
            let key_pem = key.serialize_pem();
            write_public(&ca_cert_path(), &cert.pem())?;
            write_secret(&ca_key_path(), &key_pem)?;
            ca_created = true;
            key
        }
    };

    // 根证书的公钥指纹：从磁盘上的 PEM 还原 DER 再算（新造的也一样走这条路，
    // 免得同一个算法在两处各写一份）。
    let ca_fingerprint = pem_to_der(&read_text(&ca_cert_path()).unwrap_or_default())
        .map(|der| fingerprint(&der))
        .unwrap_or_default();

    let meta = read_meta();
    let now = Utc::now();
    let now_unix = now.timestamp();

    let in_sync = meta
        .as_ref()
        .map(|old| {
            old.sans == sans
                && old.ca_fingerprint == ca_fingerprint
                && old.leaf_schema == LEAF_SCHEMA
        })
        .unwrap_or(false);
    let expiring = meta
        .as_ref()
        .map(|old| old.leaf_not_after_unix - now_unix < RENEW_BEFORE_DAYS * 86_400)
        .unwrap_or(true);
    let on_disk = leaf_cert_path().is_file() && leaf_key_path().is_file();

    let mut leaf_generated = false;
    if !(in_sync && !expiring && on_disk) {
        let params = leaf_params(&sans)?;
        // Issuer 只用来取签发者 DN / KeyIdMethod / 签发密钥，参数与磁盘上那张根证书
        // 完全一致（同一份 ca_params()），所以签出来的链能对上。
        let issuer: Issuer<'static, KeyPair> = Issuer::new(ca_params(), ca_key);
        let key = KeyPair::generate().map_err(|err| format!("生成叶证书私钥失败：{err}"))?;
        let cert = params
            .signed_by(&key, &issuer)
            .map_err(|err| format!("签发叶证书失败：{err}"))?;
        write_public(&leaf_cert_path(), &cert.pem())?;
        write_secret(&leaf_key_path(), &key.serialize_pem())?;

        let (not_after, not_after_unix) = leaf_not_after();
        let meta = Meta {
            ca_fingerprint,
            leaf_fingerprint: fingerprint(cert.der()),
            sans,
            leaf_schema: LEAF_SCHEMA,
            leaf_not_after: not_after,
            leaf_not_after_unix: not_after_unix,
            generated_at: now.to_rfc3339(),
        };
        let raw = serde_json::to_string_pretty(&meta)
            .map_err(|err| format!("序列化证书信息失败：{err}"))?;
        write_public(&meta_path(), &raw)?;
        leaf_generated = true;
    }

    Ok(Ensured {
        ca_created,
        leaf_generated,
        status: status(config),
    })
}

// ---------------------------------------------------------------------------
// 状态
// ---------------------------------------------------------------------------

/// 当前状态快照。只读小文件 + 一次带缓存的信任查询，状态轮询里也能随便调。
pub fn status(config: &AppConfig) -> Value {
    let meta = read_meta();
    let names = required_sans(config);
    let certified = meta.as_ref().map(|m| m.sans.clone()).unwrap_or_default();
    let now_unix = Utc::now().timestamp();
    let not_after_unix = meta.as_ref().map(|m| m.leaf_not_after_unix).unwrap_or(0);
    let days_left = if not_after_unix > 0 {
        ((not_after_unix - now_unix) as f64 / 86_400.0).floor() as i64
    } else {
        0
    };

    json!({
        "dir": cert_dir().to_string_lossy(),
        "caCert": ca_cert_path().to_string_lossy(),
        "caKey": ca_key_path().to_string_lossy(),
        "leafCert": leaf_cert_path().to_string_lossy(),
        "leafKey": leaf_key_path().to_string_lossy(),
        "caExists": ca_cert_path().is_file(),
        "leafExists": leaf_cert_path().is_file(),
        "caFingerprint": meta.as_ref().map(|m| m.ca_fingerprint.clone()).unwrap_or_default(),
        "leafFingerprint": meta.as_ref().map(|m| m.leaf_fingerprint.clone()).unwrap_or_default(),
        "caCommonName": CA_COMMON_NAME,
        "leafNotAfter": meta.as_ref().map(|m| m.leaf_not_after.clone()).unwrap_or_default(),
        "leafDaysLeft": days_left,
        "leafValidityDays": LEAF_VALIDITY_DAYS,
        "caValidityDays": CA_VALIDITY_DAYS,
        "renewBeforeDays": RENEW_BEFORE_DAYS,
        // names 是「按当前映射应该覆盖的名字」，certified 是「证书里实际写着的」，
        // 两者不一致界面就提示需要更新 —— 用户改完映射还没重签时正好看到这个。
        "names": names,
        "certifiedNames": certified,
        "namesInSync": meta.as_ref().map(|m| m.sans == names).unwrap_or(false),
        "caTrusted": trust_state(),
        "canInstallTrust": cfg!(windows),
        "platform": std::env::consts::OS,
    })
}

/// 根证书是否已被本机信任；查不到（非 Windows、或 certutil 不在）返回 null。
pub fn trust_state() -> Value {
    #[cfg(windows)]
    {
        json!(windows_trust_cached())
    }
    #[cfg(not(windows))]
    {
        Value::Null
    }
}

#[cfg(windows)]
fn hide_console(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    command.creation_flags(0x0800_0000);
}

#[cfg(not(windows))]
#[allow(dead_code)]
fn hide_console(_command: &mut Command) {}

#[cfg(windows)]
fn certutil_has_ca(user_store: bool) -> bool {
    let mut command = Command::new("certutil");
    if user_store {
        command.arg("-user");
    }
    command.args(["-store", "Root", CA_COMMON_NAME]);
    hide_console(&mut command);
    match command.output() {
        Ok(out) => {
            // 退出码为准（提示文案会跟着系统语言变，不能拿去匹配），
            // 再要求输出里真的出现了这个名字，防止"查到 0 条"也算成功。
            let mut text = String::from_utf8_lossy(&out.stdout).into_owned();
            text.push_str(&String::from_utf8_lossy(&out.stderr));
            out.status.success() && text.contains(CA_COMMON_NAME)
        }
        Err(_) => false,
    }
}

/// 证书列表是每轮状态都会问一次的，但 `certutil` 起进程要几十毫秒 ——
/// 加个短缓存，装 / 卸之后主动失效。
///
/// 缓存必须是**模块级**的一份：查询和失效如果各自持有函数内的 `static`，
/// 那就是两个互不相干的变量，装完证书还读到旧值。
#[cfg(windows)]
static TRUST_CACHE: OnceLock<Mutex<Option<(Instant, bool)>>> = OnceLock::new();

#[cfg(windows)]
fn trust_cell() -> &'static Mutex<Option<(Instant, bool)>> {
    TRUST_CACHE.get_or_init(|| Mutex::new(None))
}

#[cfg(windows)]
fn windows_trust_cached() -> bool {
    let cell = trust_cell();
    let mut guard = match cell.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some((at, value)) = *guard {
        if at.elapsed() < StdDuration::from_secs(20) {
            return value;
        }
    }
    let value = ca_cert_path().is_file() && (certutil_has_ca(true) || certutil_has_ca(false));
    *guard = Some((Instant::now(), value));
    value
}

fn forget_trust_cache() {
    #[cfg(windows)]
    {
        let cell = trust_cell();
        if let Ok(mut guard) = cell.lock() {
            *guard = None;
        }
    }
}

// ---------------------------------------------------------------------------
// 信任安装
// ---------------------------------------------------------------------------

/// 把根证书装进信任库。
///
/// Windows 走 `certutil -user`，装进**当前用户**的受信任根 —— 不需要管理员，
/// 也不会污染其他账户；Edge / Chrome 都会认。其他平台没有免提权的等价操作，
/// 这里返回的是照抄就能用的手工步骤。
pub fn install_ca() -> Result<String, String> {
    let path = ca_cert_path();
    if !path.is_file() {
        return Err("还没有生成根证书，先点「生成或更新证书」".into());
    }

    #[cfg(windows)]
    {
        let mut command = Command::new("certutil");
        command
            .args(["-user", "-f", "-addstore", "Root"])
            .arg(&path);
        hide_console(&mut command);
        let out = command
            .output()
            .map_err(|err| format!("调用 certutil 失败：{err}"))?;
        forget_trust_cache();
        let text = String::from_utf8_lossy(&out.stdout);
        if out.status.success() {
            Ok(format!("已把「{CA_COMMON_NAME}」装入当前用户的受信任根证书颁发机构"))
        } else {
            let tail = text
                .lines()
                .rev()
                .take(3)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>()
                .join(" / ");
            Err(format!("certutil 未成功（{tail}）"))
        }
    }

    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(format!(
            "当前平台需要手动信任：把 {} 导入系统根证书库（Linux 拷到 /usr/local/share/ca-certificates/ 后执行 update-ca-certificates；macOS 用「钥匙串访问」导入并设为始终信任）",
            ca_cert_path().display()
        ))
    }
}

/// 取消信任。装错了、或者换过手工证书时用得上。
pub fn uninstall_ca() -> Result<String, String> {
    #[cfg(windows)]
    {
        let mut command = Command::new("certutil");
        command
            .args(["-user", "-delstore", "Root", CA_COMMON_NAME]);
        hide_console(&mut command);
        let out = command
            .output()
            .map_err(|err| format!("调用 certutil 失败：{err}"))?;
        forget_trust_cache();
        if out.status.success() {
            Ok(format!("已从当前用户的受信任根中移除「{CA_COMMON_NAME}」"))
        } else {
            Ok(format!(
                "当前用户的受信任根里没有找到「{CA_COMMON_NAME}」，无需移除"
            ))
        }
    }

    #[cfg(not(windows))]
    {
        Ok(format!(
            "当前平台请手动移除：从系统根证书库里删除「{CA_COMMON_NAME}」"
        ))
    }
}

/// 在文件管理器里打开证书目录，方便用户自己取 `ca.crt`。
pub fn open_dir() -> Result<(), String> {
    let dir = cert_dir();
    let _ = fs::create_dir_all(&dir);
    #[cfg(windows)]
    let result = Command::new("explorer").arg(&dir).spawn();
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(&dir).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(&dir).spawn();
    result.map(|_| ()).map_err(|err| format!("打开目录失败：{err}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::LocalDnsEntry;

    fn config_with(pairs: &[(&str, &str)]) -> AppConfig {
        let mut config = AppConfig::default();
        config.local_dns = pairs
            .iter()
            .map(|(domain, address)| LocalDnsEntry {
                domain: (*domain).to_string(),
                address: (*address).to_string(),
            })
            .collect();
        config
    }

    #[test]
    fn wildcard_entries_also_cover_the_apex() {
        let names = required_sans(&config_with(&[("*.baidu.com", "192.168.1.10")]));
        assert!(names.contains(&"*.baidu.com".to_string()));
        assert!(names.contains(&"baidu.com".to_string()));
        assert!(names.contains(&"192.168.1.10".to_string()));
    }

    #[test]
    fn exact_entries_stay_single() {
        let names = required_sans(&config_with(&[("nas.lan", "10.0.0.2")]));
        assert!(names.contains(&"nas.lan".to_string()));
        assert!(!names.iter().any(|name| name.contains('*')));
    }

    #[test]
    fn hostname_addresses_are_not_turned_into_san_ips() {
        // 地址写的是域名时不该塞进 SAN —— 那不是 IP，塞进去会被当成 DNS 名。
        let names = required_sans(&config_with(&[("nas.lan", "another.host")]));
        assert_eq!(names, vec!["nas.lan".to_string()]);
    }

    #[test]
    fn names_are_lowercased_and_deduplicated() {
        let names = required_sans(&config_with(&[
            ("NAS.LAN", "10.0.0.2"),
            ("nas.lan", "10.0.0.2"),
        ]));
        assert_eq!(names, vec!["10.0.0.2".to_string(), "nas.lan".to_string()]);
    }

    #[test]
    fn leaf_validity_stays_under_the_chrome_ceiling() {
        assert!(LEAF_VALIDITY_DAYS < 398);
        assert!(CA_VALIDITY_DAYS > 398);
    }

    #[test]
    fn signed_leaf_actually_carries_the_names() {
        let names = vec![
            "baidu.com".to_string(),
            "*.baidu.com".to_string(),
            "10.0.2.2".to_string(),
        ];
        let params = leaf_params(&names).expect("params");
        assert_eq!(params.subject_alt_names.len(), 3, "参数里 SAN 数量不对");

        let ca_key = KeyPair::generate().expect("ca key");
        let issuer = Issuer::new(ca_params(), ca_key);
        let leaf_key = KeyPair::generate().expect("leaf key");
        let cert = params.signed_by(&leaf_key, &issuer).expect("sign");
        let der = pem_to_der(&cert.pem()).expect("der");
        let text = String::from_utf8_lossy(&der).into_owned();

        // DNS 名在 DER 里就是可见的 IA5 文本；IP 走的是 4 字节原始编码，
        // 拿字符串找是找不到的，得按字节比对。
        assert!(text.contains("baidu.com"), "DNS 名没写进 SAN");
        assert!(text.contains("*.baidu.com"), "通配名没写进 SAN");
        assert!(
            der.windows(4).any(|window| window == [10, 0, 2, 2]),
            "IP 没写进 SAN"
        );
    }

    #[test]
    fn signed_leaf_carries_authority_key_identifier() {
        // 严格校验的实现（Python 3.13 的 ssl 默认开 VERIFY_X509_STRICT）会拒绝
        // 没有 AKI 的叶证书：`Missing Authority Key Identifier`。
        let names = vec!["nas.lan".to_string()];
        let ca_key = KeyPair::generate().expect("ca key");
        let ca_cert = ca_params().self_signed(&ca_key).expect("ca");
        let issuer = Issuer::new(ca_params(), ca_key);
        let leaf_key = KeyPair::generate().expect("leaf key");
        let cert = leaf_params(&names)
            .expect("params")
            .signed_by(&leaf_key, &issuer)
            .expect("sign");

        let leaf_der = pem_to_der(&cert.pem()).expect("leaf der");
        let ca_der = pem_to_der(&ca_cert.pem()).expect("ca der");

        // OID 2.5.29.35 = 55 1D 23（AKI）、2.5.29.14 = 55 1D 0E（SKI）。
        assert!(
            leaf_der.windows(3).any(|window| window == [0x55, 0x1D, 0x23]),
            "叶证书缺 Authority Key Identifier"
        );
        assert!(
            ca_der.windows(3).any(|window| window == [0x55, 0x1D, 0x0E]),
            "根证书缺 Subject Key Identifier"
        );
    }

    #[test]
    fn ec_leaf_only_claims_digital_signature() {
        // RFC 5480：EC 证书不得声明 Key Encipherment。rcgen 把 KeyUsage 编成
        // 「只有 Digital Signature」时是 03 02 07 80，带上 Key Encipherment 会变成
        // 03 02 06 C0 —— 直接用这个字节对来盯住用途位。
        let ca_key = KeyPair::generate().expect("ca key");
        let issuer = Issuer::new(ca_params(), ca_key);
        let leaf_key = KeyPair::generate().expect("leaf key");
        let cert = leaf_params(&["nas.lan".to_string()])
            .expect("params")
            .signed_by(&leaf_key, &issuer)
            .expect("sign");
        let der = pem_to_der(&cert.pem()).expect("der");

        assert!(
            der.windows(4).any(|window| window == [0x03, 0x02, 0x07, 0x80]),
            "叶证书的 Key Usage 不是「只有数字签名」"
        );
        assert!(
            !der.windows(4).any(|window| window == [0x03, 0x02, 0x06, 0xC0]),
            "EC 叶证书不该声明 Key Encipherment"
        );
    }

    #[test]
    fn ensure_refuses_to_sign_a_certificate_without_names() {
        let mut config = AppConfig::default();
        config.local_dns.clear();
        let result = ensure(&config);
        assert!(result.is_err(), "没有映射时应当拒绝");
        let err = result.err().unwrap_or_default();
        assert!(err.contains("本地 DNS 映射"), "错误信息不友好：{err}");
    }
}
