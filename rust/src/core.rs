use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::OpenOptions;
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
