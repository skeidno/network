//! Remote sing-box deployment.
//!
//! The connection layer drives the platform OpenSSH client instead of an
//! in-process SSH library: the crypto is the operating system's, and uploads go
//! through the shell channel (`cat > file`), which is the path that survives
//! unreliable cross-border links.

use base64::Engine;
use serde_json::{json, Value};
use std::path::PathBuf;
use std::time::Duration;

use crate::models::{server_proxy_port_error, SshServerProfile};
use crate::paths::ssh_known_hosts_path;

pub const SING_BOX_VERSION: &str = "1.13.20";
const SHADOWSOCKS_METHOD: &str = "2022-blake3-aes-128-gcm";
const SERVICE_NAME: &str = "network-manager-proxy";
const REMOTE_ROOT: &str = "/etc/network-manager-proxy";
const REMOTE_BINARY: &str = "/usr/local/lib/network-manager-proxy/sing-box";

const SING_BOX_SHA256: &[(&str, &str)] = &[
    (
        "386",
        "9614c2cb8a13ea745db07afb1c84f8233e40782de5c1f1770249e51ffc3fb63a",
    ),
    (
        "amd64",
        "646bc01bf128c32a12eb50d8690e387bba7504da7b1d65c704bd53916e38595a",
    ),
    (
        "arm64",
        "7f8187b1d1d30258cd4fa70892eaa232649f8f28b294078eeac719579e14cf42",
    ),
    (
        "armv7",
        "8740c42726b2de78cea3f9258249c839cf1dee6ddf654389574e94d9aebd7ab7",
    ),
];

const DROP_MARKERS: &[&str] = &[
    "连接失败",
    "连接中断",
    "连接已断开",
    "connection dropped",
    "connection reset",
    "connection refused",
    "connection timed out",
    "connection is closed",
    "socket exception",
    "socket is closed",
    "broken pipe",
    "eof",
    "10054",
    "远程主机强迫关闭",
    "ssh session",
    "connection closed",
    "timeout",
    "kex_exchange_identification",
];

pub fn looks_like_connection_drop(message: &str) -> bool {
    let lowered = message.to_lowercase();
    DROP_MARKERS.iter().any(|marker| lowered.contains(marker))
}

fn host_key_changed(message: &str) -> bool {
    let lowered = message.to_lowercase();
    lowered.contains("remote host identification has changed")
        || lowered.contains("host key verification failed")
        || lowered.contains("offending")
}

pub fn deployment_source_id(profile_id: &str) -> String {
    format!("server-deployment:{profile_id}")
}

pub fn build_shadowsocks_node(profile: &SshServerProfile, password: &str) -> Value {
    json!({
        "name": profile.name,
        "type": "ss",
        "server": profile.host,
        "port": profile.proxy_port,
        "cipher": SHADOWSOCKS_METHOD,
        "password": password,
        "udp": true,
    })
}

pub fn shadowsocks_share_link(node: &Value) -> String {
    let method = node
        .get("cipher")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let password = node
        .get("password")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let encoded = base64::engine::general_purpose::URL_SAFE
        .encode(format!("{method}:{password}"))
        .trim_end_matches('=')
        .to_string();
    let mut host = node
        .get("server")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    if host.contains(':') && !host.starts_with('[') {
        host = format!("[{host}]");
    }
    let port = node.get("port").and_then(|v| v.as_i64()).unwrap_or(0);
    let name = node
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or_default();
    let encoded_name = percent_encoding(&name);
    format!("ss://{encoded}@{host}:{port}#{encoded_name}")
}

fn percent_encoding(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

pub struct DeployResult {
    pub node_config: Value,
    /// 部署完成后的 ss:// 分享链接，供前端一键复制。
    #[allow(dead_code)]
    pub share_link: String,
    pub version: String,
    pub deployed_at: String,
    /// 远端防火墙处理情况（ufw / firewalld / unmanaged）。
    #[allow(dead_code)]
    pub firewall: String,
    pub reused: bool,
    pub public_reachable: Option<bool>,
    pub public_error: String,
}

impl Default for DeployResult {
    fn default() -> Self {
        Self {
            node_config: Value::Null,
            share_link: String::new(),
            version: String::new(),
            deployed_at: String::new(),
            firewall: String::new(),
            reused: false,
            public_reachable: None,
            public_error: String::new(),
        }
    }
}

/// A live SSH session against one server profile.
///
/// Backed by the in-process SSH-2 client in `sshclient`, so password
/// authentication never depends on `SSH_ASKPASS` (which the Windows OpenSSH
/// build cannot always spawn).
struct Runner {
    host: String,
    port: u16,
    user: String,
    credential: String,
    known_hosts: PathBuf,
    client: Option<crate::sshclient::Client>,
}

impl Runner {
    fn new(profile: &SshServerProfile, credential: &str) -> Result<Self, String> {
        if profile.host.trim().is_empty() {
            return Err("SSH 服务器地址为空".to_string());
        }
        if profile.username.trim().is_empty() {
            return Err("SSH 用户名为空".to_string());
        }
        Ok(Self {
            host: profile.host.trim().to_string(),
            port: if profile.port > 0 && profile.port < 65536 {
                profile.port as u16
            } else {
                22
            },
            user: profile.username.trim().to_string(),
            credential: credential.to_string(),
            known_hosts: ssh_known_hosts_path(),
            client: None,
        })
    }

    async fn connected(&mut self) -> Result<&mut crate::sshclient::Client, String> {
        if self.client.is_none() {
            let client = crate::sshclient::Client::connect(
                &self.host,
                self.port,
                &self.user,
                &self.credential,
                &self.known_hosts,
                Duration::from_secs(20),
            )
            .await?;
            self.client = Some(client);
        }
        Ok(self.client.as_mut().expect("刚刚建立的连接"))
    }

    async fn run(&mut self, remote: &str, timeout: u64) -> Result<String, String> {
        let client = self.connected().await?;
        let result = client.exec(remote, &[]).await?;
        if result.status != 0 {
            let detail = if !result.stderr.trim().is_empty() {
                result.stderr.trim().to_string()
            } else {
                result.stdout.trim().to_string()
            };
            let message = detail.lines().last().unwrap_or("").trim();
            if message.is_empty() {
                return Err(format!("远端命令失败（退出码 {}）", result.status));
            }
            let lowered = detail.to_lowercase();
            if lowered.contains("permission denied") || lowered.contains("authentication failed") {
                return Err("SSH 认证失败，请检查用户名、密码或私钥".to_string());
            }
            return Err(format!("远端命令失败：{}", truncate_message(message)));
        }
        let _ = timeout;
        Ok(result.stdout)
    }

    /// Write a file through the exec channel: no SFTP session to lose.
    async fn upload(&mut self, path: &str, content: &str, mode: u32) -> Result<(), String> {
        let command = format!(
            "cat > {} && chmod {mode:o} {}",
            shell_quote(path),
            shell_quote(path)
        );
        let client = self.connected().await?;
        let result = client.exec(&command, content.as_bytes()).await?;
        if result.status != 0 {
            let message = result
                .stderr
                .trim()
                .lines()
                .last()
                .unwrap_or("远端写入失败")
                .trim();
            return Err(format!("上传远端配置失败：{}", truncate_message(message)));
        }
        Ok(())
    }

    fn purge_host_key(&self) {
        crate::sshclient::purge_host_key(&self.known_hosts, &self.host, self.port);
    }

    async fn shutdown(&mut self) {
        if let Some(client) = self.client.as_mut() {
            client.close().await;
        }
        self.client = None;
    }
}

fn truncate_message(message: &str) -> String {
    if message.len() > 300 {
        message[..300].to_string()
    } else {
        message.to_string()
    }
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn server_config(port: i32, password: &str) -> Value {
    json!({
        "log": {"level": "info", "timestamp": true},
        "inbounds": [{
            "type": "shadowsocks",
            "tag": "ss-in",
            "listen": "::",
            "listen_port": port,
            "method": SHADOWSOCKS_METHOD,
            "password": password,
            "multiplex": {"enabled": true},
        }],
        "outbounds": [{"type": "direct", "tag": "direct"}],
        "route": {"final": "direct"},
    })
}

fn service_unit() -> String {
    format!(
        "[Unit]\n\
         Description=Network Manager managed proxy\n\
         Documentation=https://sing-box.sagernet.org/\n\
         After=network-online.target\n\
         Wants=network-online.target\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={REMOTE_BINARY} run -c {REMOTE_ROOT}/config.json\n\
         Restart=on-failure\n\
         RestartSec=5s\n\
         LimitNOFILE=1048576\n\
         NoNewPrivileges=true\n\
         PrivateTmp=true\n\
         ProtectHome=true\n\
         ProtectSystem=strict\n\
         \n\
         [Install]\n\
         WantedBy=multi-user.target\n"
    )
}

fn install_script(architecture: &str, config_tmp: &str, service_tmp: &str) -> Result<String, String> {
    let checksum = SING_BOX_SHA256
        .iter()
        .find(|(arch, _)| *arch == architecture)
        .map(|(_, value)| *value)
        .ok_or_else(|| format!("不支持的服务器架构：{architecture}"))?;
    let archive = format!("sing-box-{SING_BOX_VERSION}-linux-{architecture}.tar.gz");
    let release = format!("https://github.com/SagerNet/sing-box/releases/download/v{SING_BOX_VERSION}");
    Ok(format!(
        "#!/bin/sh\n\
         set -eu\n\
         VERSION={version}\n\
         ARCHIVE={archive}\n\
         WORK_DIR=$(mktemp -d)\n\
         trap 'rm -rf \"$WORK_DIR\"' EXIT\n\
         cd \"$WORK_DIR\"\n\
         if command -v curl >/dev/null 2>&1; then\n\
         \x20 curl --proto '=https' --tlsv1.2 -fL --retry 3 -o \"$ARCHIVE\" {url}\n\
         else\n\
         \x20 wget -qO \"$ARCHIVE\" {url}\n\
         fi\n\
         EXPECTED={checksum}\n\
         ACTUAL=$(sha256sum \"$ARCHIVE\" | awk '{{print $1}}')\n\
         test \"$EXPECTED\" = \"$ACTUAL\"\n\
         tar -xzf \"$ARCHIVE\"\n\
         install -d -m 0755 /usr/local/lib/network-manager-proxy {root}\n\
         install -m 0755 \"sing-box-$VERSION-linux-{architecture}/sing-box\" {binary}\n\
         {binary} check -c {config_tmp}\n\
         HAD_CONFIG=0\n\
         HAD_SERVICE=0\n\
         if test -f {root}/config.json; then\n\
         \x20 cp -p {root}/config.json \"$WORK_DIR/config.backup\"\n\
         \x20 HAD_CONFIG=1\n\
         fi\n\
         if test -f /etc/systemd/system/{service}.service; then\n\
         \x20 cp -p /etc/systemd/system/{service}.service \"$WORK_DIR/service.backup\"\n\
         \x20 HAD_SERVICE=1\n\
         fi\n\
         install -m 0600 {config_tmp} {root}/config.json\n\
         install -m 0644 {service_tmp} /etc/systemd/system/{service}.service\n\
         if ! systemctl daemon-reload || ! systemctl enable {service} || ! systemctl restart {service}; then\n\
         \x20 if test \"$HAD_CONFIG\" = 1; then cp -p \"$WORK_DIR/config.backup\" {root}/config.json; else rm -f {root}/config.json; fi\n\
         \x20 if test \"$HAD_SERVICE\" = 1; then cp -p \"$WORK_DIR/service.backup\" /etc/systemd/system/{service}.service; else rm -f /etc/systemd/system/{service}.service; fi\n\
         \x20 systemctl daemon-reload || true\n\
         \x20 if test \"$HAD_SERVICE\" = 1; then systemctl restart {service} || true; else systemctl stop {service} || true; fi\n\
         \x20 exit 1\n\
         fi\n\
         sleep 1\n\
         systemctl is-active --quiet {service}\n",
        version = SING_BOX_VERSION,
        archive = archive,
        url = format!("{release}/{archive}"),
        root = REMOTE_ROOT,
        binary = REMOTE_BINARY,
        service = SERVICE_NAME,
        config_tmp = config_tmp,
        service_tmp = service_tmp,
    ))
}

fn configure_firewall_script(port: i32) -> String {
    format!(
        "if command -v ufw >/dev/null 2>&1 && ufw status | grep -q '^Status: active'; then\n\
         \x20 ufw allow {port}/tcp >/dev/null\n\
         \x20 ufw allow {port}/udp >/dev/null\n\
         \x20 printf ufw\n\
         elif command -v firewall-cmd >/dev/null 2>&1 && firewall-cmd --state >/dev/null 2>&1; then\n\
         \x20 firewall-cmd --permanent --add-port={port}/tcp >/dev/null\n\
         \x20 firewall-cmd --permanent --add-port={port}/udp >/dev/null\n\
         \x20 firewall-cmd --reload >/dev/null\n\
         \x20 printf firewalld\n\
         else\n\
         \x20 printf unmanaged\n\
         fi\n"
    )
}

fn random_token() -> String {
    use rand::RngCore;
    let mut buffer = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut buffer);
    buffer.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn random_password() -> String {
    use rand::RngCore;
    let mut buffer = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut buffer);
    base64::engine::general_purpose::STANDARD.encode(buffer)
}

fn now_iso() -> String {
    let now = chrono::Local::now();
    now.format("%Y-%m-%dT%H:%M:%S%:z").to_string()
}

/// Run a single deploy attempt; returns the result or a deploy error.
async fn deploy_once(
    profile: &SshServerProfile,
    credential: &str,
    report: &mut (dyn FnMut(String) + Send),
) -> Result<DeployResult, String> {
    report("正在连接 SSH".into());
    let mut runner = Runner::new(profile, credential)?;

    report("正在检查服务器环境".into());
    let preflight = runner
        .run(
            "printf '%s\\n' \"$(uname -s)\" \"$(uname -m)\" \"$(id -u)\"; \
             command -v systemctl >/dev/null; command -v tar >/dev/null; \
             command -v sha256sum >/dev/null; command -v base64 >/dev/null; \
             command -v curl >/dev/null || command -v wget >/dev/null",
            30,
        )
        .await?;
    let lines: Vec<&str> = preflight.lines().collect();
    if lines.len() < 3 {
        return Err("无法识别服务器系统环境".into());
    }
    if lines[0].trim() != "Linux" {
        return Err("自动部署目前仅支持 Linux 服务器".into());
    }
    if lines[2].trim() != "0" {
        return Err("自动部署需要 root 账号；普通账号暂不执行远端提权".into());
    }
    let architecture = lines[1].trim();
    let archive_arch = match architecture {
        "x86_64" | "amd64" => "amd64",
        "aarch64" | "arm64" => "arm64",
        "armv7l" => "armv7",
        "i386" | "i686" => "386",
        other => return Err(format!("不支持的服务器架构：{other}")),
    };

    let proxy_password = random_password();
    let node = build_shadowsocks_node(profile, &proxy_password);
    let config = serde_json::to_string_pretty(&server_config(profile.proxy_port, &proxy_password))
        .map_err(|_| "生成远端配置失败".to_string())?;
    let service = service_unit();
    let token = random_token();
    let config_tmp = format!("/tmp/network-manager-proxy-{token}.json");
    let service_tmp = format!("/tmp/network-manager-proxy-{token}.service");
    let install_tmp = format!("/tmp/network-manager-proxy-{token}.sh");

    report("正在上传代理配置".into());
    runner
        .upload(&config_tmp, &format!("{config}\n"), 0o600)
        .await?;
    runner.upload(&service_tmp, &service, 0o600).await?;
    runner
        .upload(
            &install_tmp,
            &install_script(archive_arch, &config_tmp, &service_tmp)?,
            0o700,
        )
        .await?;

    report(format!("正在安装 sing-box {SING_BOX_VERSION}"));
    runner.run(&shell_quote(&install_tmp), 300).await?;
    let _ = runner
        .run(
            &format!("rm -f {} {} {}", shell_quote(&config_tmp), shell_quote(&service_tmp), shell_quote(&install_tmp)),
            15,
        )
        .await;

    report("正在检查服务状态".into());
    let version = runner
        .run(&format!("{REMOTE_BINARY} version | head -n 1"), 30)
        .await
        .unwrap_or_default();
    let active = runner
        .run(&format!("systemctl is-active {SERVICE_NAME}"), 30)
        .await?;
    if active.trim() != "active" {
        return Err("远端代理服务未能保持运行".into());
    }
    let firewall = runner
        .run(&configure_firewall_script(profile.proxy_port), 60)
        .await
        .unwrap_or_default();
    report("部署完成".into());
    let share_link = shadowsocks_share_link(&node);
    runner.shutdown().await;

    Ok(DeployResult {
        share_link,
        node_config: node,
        version: if version.trim().is_empty() {
            format!("sing-box {SING_BOX_VERSION}")
        } else {
            version.trim().to_string()
        },
        deployed_at: now_iso(),
        firewall: if firewall.trim().is_empty() {
            "unmanaged".to_string()
        } else {
            firewall.trim().to_string()
        },
        reused: false,
        public_reachable: None,
        public_error: String::new(),
    })
}

pub async fn deploy(
    profile: &SshServerProfile,
    credential: &str,
    report: &mut (dyn FnMut(String) + Send),
) -> Result<DeployResult, String> {
    let port_error = server_proxy_port_error(profile.proxy_port, profile.port);
    if !port_error.is_empty() {
        return Err(port_error);
    }
    let mut last = String::new();
    for attempt in 0..2 {
        match deploy_once(profile, credential, report).await {
            Ok(result) => return Ok(result),
            Err(err) => {
                let droppable = looks_like_connection_drop(&err);
                if host_key_changed(&err) && attempt == 0 {
                    report("检测到服务器主机密钥已更新，正在刷新本地记录并重连".into());
                    if let Ok(runner) = Runner::new(profile, credential) {
                        runner.purge_host_key();
                    }
                    continue;
                }
                last = err;
                if attempt == 0 && droppable {
                    report("SSH 连接被中断，正在重新连接并重试部署".into());
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    continue;
                }
                return Err(last);
            }
        }
    }
    Err(last)
}

pub async fn inspect(profile: &SshServerProfile, credential: &str) -> Result<Value, String> {
    let mut last = String::new();
    for attempt in 0..2 {
        match inspect_once(profile, credential).await {
            Ok(value) => return Ok(value),
            Err(err) => {
                if host_key_changed(&err) && attempt == 0 {
                    if let Ok(runner) = Runner::new(profile, credential) {
                        runner.purge_host_key();
                    }
                    continue;
                }
                let droppable = looks_like_connection_drop(&err);
                last = err;
                if attempt == 0 && droppable {
                    tokio::time::sleep(Duration::from_millis(1500)).await;
                    continue;
                }
                return Err(last);
            }
        }
    }
    Err(last)
}

async fn inspect_once(profile: &SshServerProfile, credential: &str) -> Result<Value, String> {
    let mut runner = Runner::new(profile, credential)?;
    let status = runner
        .run(&format!("systemctl is-active {SERVICE_NAME} 2>/dev/null || true"), 30)
        .await
        .unwrap_or_default();
    let version = runner
        .run(
            &format!("test -x {REMOTE_BINARY} && {REMOTE_BINARY} version | head -n 1 || true"),
            30,
        )
        .await
        .unwrap_or_default();
    let status = status.trim().to_string();
    let version = version.trim().to_string();
    let mut result = json!({
        "status": if status.is_empty() { "not-installed".to_string() } else { status.clone() },
        "version": version,
    });
    if status == "active" {
        let raw = runner
            .run(
                &format!("test -r {REMOTE_ROOT}/config.json && cat {REMOTE_ROOT}/config.json || true"),
                30,
            )
            .await
            .unwrap_or_default();
        if let Some(node) = node_from_remote_config(profile, &raw) {
            result["nodeConfig"] = node;
        }
    }
    runner.shutdown().await;
    Ok(result)
}

fn node_from_remote_config(profile: &SshServerProfile, raw: &str) -> Option<Value> {
    let config: Value = serde_json::from_str(raw).ok()?;
    let inbounds = config.get("inbounds")?.as_array()?;
    for inbound in inbounds {
        let listen_port = inbound.get("listen_port")?.as_i64()?;
        let password = inbound.get("password")?.as_str()?.to_string();
        if inbound.get("type")?.as_str() == Some("shadowsocks")
            && inbound.get("method")?.as_str() == Some(SHADOWSOCKS_METHOD)
            && listen_port == profile.proxy_port as i64
            && !password.is_empty()
        {
            return Some(build_shadowsocks_node(profile, &password));
        }
    }
    None
}

/// Probe whether the deployed port answers from the public internet.
pub async fn probe_public(profile: &SshServerProfile) -> (bool, String) {
    let endpoint = format!("{}:{}", profile.host, profile.proxy_port);
    let address = match tokio::net::lookup_host(&endpoint).await {
        Ok(mut addresses) => match addresses.next() {
            Some(address) => Some(address),
            None => None,
        },
        Err(_) => None,
    };
    let Some(address) = address else {
        return (false, "公网地址解析失败".to_string());
    };
    match tokio::time::timeout(
        Duration::from_secs(4),
        tokio::net::TcpStream::connect(address),
    )
    .await
    {
        Ok(Ok(_)) => (true, String::new()),
        Ok(Err(err)) => (false, format!("公网连接失败：{err}")),
        Err(_) => (false, "公网连接超时".to_string()),
    }
}
