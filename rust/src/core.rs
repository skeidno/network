use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};

use crate::mihomo::render_yaml;
use crate::models::AppConfig;
use crate::paths::{core_binary, core_log_path, generated_config_path};

#[derive(Debug, Clone)]
pub struct NodeDelay {
    pub status: String,
    pub delay: Option<i64>,
    pub message: String,
}


pub struct CoreProcess {
    child: Option<Child>,
}

impl Default for CoreProcess {
    fn default() -> Self {
        Self { child: None }
    }
}

impl CoreProcess {
    pub fn is_running(&mut self) -> bool {
        match self.child.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }

    pub fn write_config(&self, config: &AppConfig) -> Result<(), String> {
        let path = generated_config_path();
        let yaml = render_yaml(config)?;
        std::fs::write(&path, yaml).map_err(|err| format!("写入核心配置失败：{err}"))
    }

    pub fn start(&mut self, config: &AppConfig) -> Result<(), String> {
        if self.is_running() {
            self.write_config(config)?;
            return Ok(());
        }
        self.write_config(config)?;
        // 启动前先裁剪一次：此时还没有句柄持有日志文件，重写最安全。
        trim_core_log();
        let binary = core_binary();
        if !binary.exists() {
            return Err(format!(
                "未找到内核程序：{}（可用 NETWORK_MANAGER_CORE 指定）",
                binary.display()
            ));
        }
        let log = OpenOptions::new()
            .create(true)
            .append(true)
            .open(core_log_path())
            .map_err(|err| format!("打开内核日志失败：{err}"))?;
        let log_err = log
            .try_clone()
            .map_err(|err| format!("打开内核日志失败：{err}"))?;
        let mut command = Command::new(&binary);
        command
            .arg("-f")
            .arg(generated_config_path())
            .arg("-d")
            .arg(crate::paths::app_data_dir())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(log_err));
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let child = command
            .spawn()
            .map_err(|err| format!("启动内核失败：{err}"))?;
        self.child = Some(child);
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        Ok(())
    }
}

pub struct AppState {
    pub config: AppConfig,
    pub core: CoreProcess,
    pub node_delays: HashMap<String, NodeDelay>,
    pub session_token: String,
    pub access_username: String,
    pub access_password: String,
    pub busy: bool,
    pub importing: bool,
    pub headless: bool,
    pub exit_ip: String,
    pub exit_ip_location: String,
    pub local_ip: String,
    pub local_ip_location: String,
    pub toasts: Vec<Value>,
}

impl AppState {
    pub fn new() -> Self {
        Self {
            config: crate::store::load(),
            core: CoreProcess::default(),
            node_delays: HashMap::new(),
            session_token: crate::server::random_session_token(),
            access_username: "admin".into(),
            access_password: std::env::var("NETWORK_MANAGER_WEB_PASSWORD").unwrap_or_default(),
            busy: false,
            importing: false,
            headless: false,
            exit_ip: "尚未检测".into(),
            exit_ip_location: String::new(),
            local_ip: "尚未检测".into(),
            local_ip_location: String::new(),
            toasts: Vec::new(),
        }
    }

    pub fn notify(&mut self, kind: &str, message: impl Into<String>) {
        self.toasts.push(json!({"kind": kind, "message": message.into()}));
        if self.toasts.len() > 40 {
            self.toasts.remove(0);
        }
    }

    pub fn apply_config(&mut self) -> Result<(), String> {
        crate::store::save(&self.config);
        if self.core.is_running() {
            self.core.write_config(&self.config)?;
        }
        Ok(())
    }
}

/// 本地内核日志的体积上限（3 MB）。超过后只保留最新的一段，旧内容直接丢弃，
/// 避免长期运行把日志堆到几十上百 MB。
pub const MAX_LOG_BYTES: u64 = 3 * 1024 * 1024;
/// 触发裁剪后保留的字节数，留出余量，避免频繁重写文件。
const LOG_KEEP_BYTES: u64 = MAX_LOG_BYTES - 384 * 1024;

/// 只读取日志尾部（最多 max_bytes 字节），并按行对齐后返回最后 max_lines 行。
///
/// 解码一律走「UTF-8 宽松」：内核日志里常出现中文节点名（海外服务器 等），
/// 只要文件中出现一个被截断的字节，严格的 read_to_string 就会整段失败并返回空，
/// 界面上表现为空白或乱码。宽松解码只会把个别坏字节替换成占位符，不会整段丢失。
pub fn read_log_tail(path: &Path, max_bytes: u64, max_lines: usize) -> String {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return String::new(),
    };
    let size = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    let start = size.saturating_sub(max_bytes);
    let mut buffer = Vec::new();
    if file
        .seek(SeekFrom::Start(start))
        .and_then(|_| file.read_to_end(&mut buffer))
        .is_err()
    {
        return String::new();
    }
    // 从文件中间开始读时，第一行通常是被截断的半行，直接丢掉。
    let skip = if start > 0 {
        match buffer.iter().position(|byte| *byte == b'\n') {
            Some(index) => index + 1,
            None => 0,
        }
    } else {
        0
    };
    let text = String::from_utf8_lossy(&buffer[skip..]).replace('\r', "");
    let lines: Vec<&str> = text.lines().collect();
    let from = lines.len().saturating_sub(max_lines);
    lines[from..].join("\n")
}

/// 日志超过上限时，只保留最新的内容，多出来的部分删除。
pub fn trim_core_log() {
    let path = core_log_path();
    let Ok(meta) = std::fs::metadata(&path) else {
        return;
    };
    if meta.len() <= MAX_LOG_BYTES {
        return;
    }
    let keep = read_log_tail(&path, LOG_KEEP_BYTES, usize::MAX);
    let Ok(mut file) = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(&path)
    else {
        return;
    };
    if file.seek(SeekFrom::Start(0)).is_err() {
        return;
    }
    if file.write_all(keep.as_bytes()).is_err() {
        return;
    }
    let _ = file.set_len(keep.len() as u64);
    let _ = file.flush();
}

pub fn memory_mb() -> f64 {
    use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, RefreshKind, System};
    let mut system = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::new().with_memory()),
    );
    let pid = Pid::from_u32(std::process::id());
    system.refresh_processes(ProcessesToUpdate::Some(&[pid]));
    system
        .process(pid)
        .map(|process| process.memory() as f64 / 1024.0 / 1024.0)
        .unwrap_or(0.0)
}
