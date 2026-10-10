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

/// 服务器代理的部署/检查进度。
///
/// 「检查服务」在远端可能要跑好几秒，界面得能显示「检查中」，否则用户会以为
/// 点了没反应。状态存在这里而不是服务器配置里：它是一次运行态，不该落盘。
#[derive(Debug, Clone, Default)]
pub struct DeployTask {
    /// idle / deploying / error
    pub status: String,
    pub stage: String,
    pub error: String,
}


pub struct CoreProcess {
    child: Option<Child>,
    /// Windows 作业对象句柄：把内核进程挂进来，父进程无论正常退出、崩溃还是被
    /// 强杀，系统都会连同作业里的内核一起终止（JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE）。
    // 存原始句柄值而不是 HANDLE：HANDLE 内含裸指针，不满足 Send，
    // 会让整个 AppState 无法跨线程共享。
    #[cfg(windows)]
    job: Option<isize>,
}

impl Default for CoreProcess {
    fn default() -> Self {
        Self {
            child: None,
            #[cfg(windows)]
            job: None,
        }
    }
}

/// 清理上次异常退出残留的内核进程。
///
/// 内核以 TUN + auto-route 运行：只要进程还活着，它就接管本机路由表和 DNS。
/// 一旦管理进程崩溃或被强杀（release 配置是 panic = "abort"，没有任何清理机会），
/// 内核会变成孤儿继续占用 TUN 设备；再启动一次就会多一个实例，几个实例互相踩
/// 路由表，表现就是「关掉界面后整台机器上不了网，只能手动杀进程重启」。
/// 这里按可执行文件路径精确匹配我们自己的内核，避免误伤用户另外跑的 mihomo。
pub fn kill_orphan_cores(exclude_pid: Option<u32>) {
    let target = core_binary();
    let mut system = sysinfo::System::new_all();
    system.refresh_processes(sysinfo::ProcessesToUpdate::All);
    let self_pid = sysinfo::Pid::from_u32(std::process::id());
    for (pid, process) in system.processes() {
        let Some(path) = process.exe() else {
            continue;
        };
        if path != target {
            continue;
        }
        if *pid == self_pid || process.parent() == Some(self_pid) {
            continue;
        }
        // 只清理真正的孤儿：父进程已经不存在的内核。父进程还活着说明它属于
        // 另一个正在运行的实例，杀了会把人家的代理直接打掉。
        let parent_alive = process
            .parent()
            .map(|parent| system.process(parent).is_some())
            .unwrap_or(false);
        if parent_alive {
            continue;
        }
        if let Some(keep) = exclude_pid {
            if pid.as_u32() == keep {
                continue;
            }
        }
        process.kill();
        eprintln!("清理残留内核进程：{pid}");
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
        // 先把上一次异常退出留下的内核清掉：多个内核实例会同时抢 TUN 设备与路由表。
        kill_orphan_cores(None);
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
        #[cfg(windows)]
        {
            // 挂进「进程随父进程一起终止」的作业对象，避免父进程崩溃/被杀后内核变孤儿。
            if let Some(job) = create_kill_on_close_job() {
                if !assign_to_job(job, &child) {
                    eprintln!("内核未能加入作业对象，退出时可能需要手动清理");
                }
                self.job = Some(job);
            } else {
                eprintln!("创建作业对象失败，内核将不随程序退出");
            }
        }
        self.child = Some(child);
        Ok(())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        #[cfg(windows)]
        if let Some(job) = self.job.take() {
            unsafe {
                let _ = windows::Win32::Foundation::CloseHandle(
                    windows::Win32::Foundation::HANDLE(job as *mut std::ffi::c_void),
                );
            }
        }
        Ok(())
    }
}

/// 创建一个「句柄全部关闭即终止组内进程」的作业对象。
#[cfg(windows)]
fn create_kill_on_close_job() -> Option<isize> {
    use windows::Win32::System::JobObjects::{
        CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
        JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };

    unsafe {
        let job = CreateJobObjectW(None, None).ok()?;
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const std::ffi::c_void,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        )
        .is_ok();
        if !ok {
            let _ = windows::Win32::Foundation::CloseHandle(job);
            return None;
        }
        Some(job.0 as isize)
    }
}

#[cfg(windows)]
fn assign_to_job(job: isize, child: &Child) -> bool {
    use std::os::windows::io::AsRawHandle;
    use windows::Win32::Foundation::HANDLE;
    use windows::Win32::System::JobObjects::AssignProcessToJobObject;

    let job = HANDLE(job as *mut std::ffi::c_void);
    let process = HANDLE(child.as_raw_handle());
    unsafe { AssignProcessToJobObject(job, process).is_ok() }
}

pub struct AppState {
    pub config: AppConfig,
    pub core: CoreProcess,
    pub node_delays: HashMap<String, NodeDelay>,
    /// profile_id -> 正在进行的部署/检查任务。
    pub deployments: HashMap<String, DeployTask>,
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
            deployments: HashMap::new(),
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
