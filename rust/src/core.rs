use serde_json::{json, Value};
use std::collections::HashMap;
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::sleep;

use crate::mihomo::render_yaml;
use crate::models::AppConfig;
use crate::paths::{core_binary, core_log_path, generated_config_path};

/// 界面上那条曲线是「最近 60 秒」，采样窗口就得对上。
const TRAFFIC_WINDOW: usize = 60;
/// 推进一格曲线所需的最小间隔。界面每秒来拉一次状态，间隔会抖；
/// 不设这道闸，同一个计数会被切成忽长忽短的格子，曲线抖成锯齿。
const TRAFFIC_STEP: Duration = Duration::from_millis(900);

#[derive(Debug, Clone)]
pub struct NodeDelay {
    pub status: String,
    pub delay: Option<i64>,
    pub message: String,
}

/// 内核累计流量快照（mihomo `/connections` 的汇总字段）。
#[derive(Debug, Clone, Copy, Default)]
pub struct TrafficSnapshot {
    pub download: u64,
    pub upload: u64,
    pub connections: u32,
}

fn push_traffic_sample(samples: &mut Vec<f64>, value: f64) {
    if samples.len() >= TRAFFIC_WINDOW {
        samples.remove(0);
    }
    samples.push(value);
}

/// 把内核返回的 HTTP 响应体读出来。
///
/// mihomo 的接口既可能给 Content-Length，也可能走 chunked；只按其中一种读，
/// 另一种就会解析失败，看着像「接口不返回东西」。
fn read_http_body(mut stream: std::net::TcpStream) -> Result<String, String> {
    let mut raw = Vec::new();
    stream
        .read_to_end(&mut raw)
        .map_err(|err| format!("读取内核响应失败：{err}"))?;
    let Some(split) = raw.windows(4).position(|window| window == b"\r\n\r\n") else {
        return Err("内核响应不完整".into());
    };
    let head = String::from_utf8_lossy(&raw[..split]).to_lowercase();
    if !head.starts_with("http/1.1 200") && !head.starts_with("http/1.0 200") {
        let status = head.lines().next().unwrap_or("").trim().to_string();
        return Err(format!("内核拒绝请求：{status}"));
    }
    let body = &raw[split + 4..];
    if head.contains("transfer-encoding: chunked") {
        return decode_chunked(body);
    }
    Ok(String::from_utf8_lossy(body).into_owned())
}

fn decode_chunked(mut body: &[u8]) -> Result<String, String> {
    let mut out = Vec::new();
    loop {
        let Some(line_end) = body.windows(2).position(|window| window == b"\r\n") else {
            break;
        };
        let size_line = String::from_utf8_lossy(&body[..line_end]);
        let size_text = size_line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| "内核响应分块长度非法".to_string())?;
        body = &body[line_end + 2..];
        if size == 0 || body.len() < size {
            break;
        }
        out.extend_from_slice(&body[..size]);
        body = &body[(size + 2).min(body.len())..];
    }
    Ok(String::from_utf8_lossy(&out).into_owned())
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
    /// 本次内核启动时日志文件的字节长度。之后的日志才算「这次启动产生的」——
    /// 日志是追加写的，不记这个位置就分不清一条报错是这次的还是上次的。
    log_mark: Option<u64>,
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
            log_mark: None,
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
        // 记下此刻的日志长度，之后新增的内容才算这次启动产生的。
        let log_mark = std::fs::metadata(core_log_path())
            .map(|meta| meta.len())
            .unwrap_or(0);
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
        self.log_mark = Some(log_mark);
        Ok(())
    }

    /// 通知运行中的内核重新读一遍配置文件。
    ///
    /// 内核只在启动时读配置，之后并不监听文件变化。切节点时配置文件改了、
    /// 界面也显示选中了，实际生效的还是旧节点，表现就是「换到日本节点，
    /// 出口 IP 还是美国的」。这里直接打它自己的 controller 接口触发重载：
    /// 比重启内核轻，也不会断流。
    pub fn reload(&self, config: &AppConfig) -> Result<(), String> {
        use std::io::Write;
        use std::net::TcpStream;

        let address = format!("127.0.0.1:{}", config.controller_port);
        let mut stream = TcpStream::connect(&address)
            .map_err(|err| format!("连不上内核控制接口 {address}：{err}"))?;
        // 重载要重建全部出站，节点多时可能要几秒；不能让调用方无限等。
        let _ = stream.set_read_timeout(Some(Duration::from_secs(8)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(8)));

        let body = json!({ "path": generated_config_path().to_string_lossy() }).to_string();
        let request = format!(
            "PUT /configs?force=true HTTP/1.1\r\n\
             Host: {address}\r\n\
             Authorization: Bearer {secret}\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {length}\r\n\
             Connection: close\r\n\r\n{body}",
            secret = config.controller_secret,
            length = body.len(),
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|err| format!("发送重载请求失败：{err}"))?;

        let mut response = Vec::new();
        let _ = stream.read_to_end(&mut response);
        let text = String::from_utf8_lossy(&response);
        let status = text.lines().next().unwrap_or("").trim().to_string();
        if status.contains(" 204") || status.contains(" 200") {
            Ok(())
        } else if status.is_empty() {
            Err("内核没有回应重载请求".into())
        } else {
            Err(format!("内核拒绝了重载：{status}"))
        }
    }

    /// 混合端口和控制端口是否还在监听。
    ///
    /// 光看进程活着看不出这个：mihomo 热重载配置时 `listeners` 段会整体丢掉，
    /// 重载完 TUN 还在转发、日志也照常打，但 mixed-port 和 external-controller
    /// 全没了——本地代理用不了，控制接口也调不动。必须真的连一下才知道。
    pub fn listeners_alive(&self, config: &AppConfig) -> bool {
        use std::net::TcpStream;
        self.child.is_some()
            && TcpStream::connect(format!("127.0.0.1:{}", config.mixed_port)).is_ok()
            && TcpStream::connect(format!("127.0.0.1:{}", config.controller_port)).is_ok()
    }

    /// 读内核的累计流量与连接数。
    ///
    /// 流量只有内核自己知道：它按连接记 uploads / downloads 累计，我们在外面
    /// 既看不到每条连接，也不该自己去抓包，只能问它。之前界面上的速率和曲线
    /// 是写死的 0，看着就像「图表没了」。
    pub fn traffic(&self, config: &AppConfig) -> Result<TrafficSnapshot, String> {
        let body = self.controller_get(config, "/connections")?;
        let value: Value =
            serde_json::from_str(&body).map_err(|err| format!("内核流量数据无法解析：{err}"))?;
        Ok(TrafficSnapshot {
            download: value
                .get("downloadTotal")
                .and_then(Value::as_u64)
                .unwrap_or(0),
            upload: value.get("uploadTotal").and_then(Value::as_u64).unwrap_or(0),
            connections: value
                .get("connections")
                .and_then(Value::as_array)
                .map(|list| list.len() as u32)
                .unwrap_or(0),
        })
    }

    /// 向内核控制接口发一个 GET，返回响应体。
    fn controller_get(&self, config: &AppConfig, path: &str) -> Result<String, String> {
        use std::io::Write;
        use std::net::TcpStream;

        if self.child.is_none() {
            return Err("内核没在运行".into());
        }
        let address = format!("127.0.0.1:{}", config.controller_port);
        let mut stream = TcpStream::connect(&address)
            .map_err(|err| format!("连不上内核控制接口 {address}：{err}"))?;
        // 状态页每秒都会问一次，问不通就得赶紧返回，不能把整页渲染拖住。
        let _ = stream.set_read_timeout(Some(Duration::from_secs(3)));
        let _ = stream.set_write_timeout(Some(Duration::from_secs(3)));
        let request = format!(
            "GET {path} HTTP/1.1\r\n\
             Host: {address}\r\n\
             Authorization: Bearer {secret}\r\n\
             Accept: application/json\r\n\
             Connection: close\r\n\r\n",
            secret = config.controller_secret,
        );
        stream
            .write_all(request.as_bytes())
            .map_err(|err| format!("发送内核请求失败：{err}"))?;
        read_http_body(stream)
    }

    /// 本次内核启动之后写进日志的新内容。
    fn log_since_start(&self) -> String {
        let path = core_log_path();
        let Ok(meta) = std::fs::metadata(&path) else {
            return String::new();
        };
        let mark = self.log_mark.unwrap_or(0).min(meta.len());
        let Ok(mut file) = std::fs::File::open(&path) else {
            return String::new();
        };
        if file.seek(SeekFrom::Start(mark)).is_err() {
            return String::new();
        }
        let mut buf = Vec::new();
        if file.read_to_end(&mut buf).is_err() {
            return String::new();
        }
        // 崩溃栈里可能夹着非法 UTF-8 字节，丢掉它们也要拿到能看的部分。
        String::from_utf8_lossy(&buf).to_string()
    }

    /// 内核起来了但 TUN 没起来时的原因。
    ///
    /// 这是最容易骗过所有健康检查的一种故障：mihomo 起 TUN 失败只打一条
    /// `level=error msg="Start TUN listening error: ..."`，**进程不退出**、端口照常监听，
    /// 于是「进程活着」这个判据一路绿灯，界面显示接管中，实际一条流量都没被接管——
    /// 用户看到的就是「代理明明开着，出口 IP 还是本地的」。
    /// 除了翻日志没有别的判据，所以这里必须查。
    pub fn tun_start_error(&self) -> Option<String> {
        self.log_since_start()
            .lines()
            .rev()
            .find(|line| line.contains("Start TUN listening error"))
            .and_then(|line| line.split("msg=").nth(1))
            .map(|message| message.trim().trim_matches('"').to_string())
            .filter(|message| !message.is_empty())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.log_mark = None;
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

/// 删掉上次异常退出残留的 wintun 网卡。
///
/// 内核崩溃或被强杀时 wintun 适配器常常没释放干净，下次启动就报
/// `configure tun interface: set ipv4 address: The object already exists`，
/// 然后安静地不带 TUN 继续跑。重启内核之前必须先把残留设备清掉，否则
/// 重启只会原样再失败一次。
#[cfg(windows)]
pub fn clear_stale_tun_adapter() {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let script = concat!(
        "$ErrorActionPreference='SilentlyContinue';",
        "Get-PnpDevice -Class Net | Where-Object {",
        "  $_.FriendlyName -like 'NetWorkManger*' -or $_.FriendlyName -like 'Wintun*'",
        "} | ForEach-Object {",
        "  & pnputil.exe /remove-device \"$($_.InstanceId)\" | Out-Null",
        "}"
    );
    let _ = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
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
    /// 用户「想要内核跑着」的意图：无论是开机自启、界面里手动开启，还是托盘
    /// 切换开启，都置为 true；手动关闭置为 false。自愈守护用它来区分「内核该跑却
    /// 崩了」和「用户故意关掉」，只在前一种情况下自动拉起，绝不跟用户的停止操作打架。
    pub core_desired_running: bool,
    pub node_delays: HashMap<String, NodeDelay>,
    /// profile_id -> 正在进行的部署/检查任务。
    pub deployments: HashMap<String, DeployTask>,
    pub session_token: String,
    pub access_username: String,
    pub access_password: String,
    pub busy: bool,
    pub importing: bool,
    pub headless: bool,
    /// 上次套用到内核的配置快照，用来判断这次是不是真的变了。
    pub applied_snapshot: String,
    /// 上回读到的内核累计流量与时刻，用来算瞬时速率。
    pub traffic_mark: Option<(Instant, u64, u64)>,
    pub traffic_download_rate: f64,
    pub traffic_upload_rate: f64,
    pub traffic_download_total: u64,
    pub traffic_upload_total: u64,
    pub traffic_connections: u32,
    /// 最近 60 个采样点（字节/秒），界面直接当曲线数据用。
    pub traffic_download_samples: Vec<f64>,
    pub traffic_upload_samples: Vec<f64>,
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
            core_desired_running: false,
            node_delays: HashMap::new(),
            deployments: HashMap::new(),
            session_token: crate::server::random_session_token(),
            access_username: "admin".into(),
            access_password: std::env::var("NETWORK_MANAGER_WEB_PASSWORD").unwrap_or_default(),
            busy: false,
            importing: false,
            headless: false,
            applied_snapshot: String::new(),
            traffic_mark: None,
            traffic_download_rate: 0.0,
            traffic_upload_rate: 0.0,
            traffic_download_total: 0,
            traffic_upload_total: 0,
            traffic_connections: 0,
            traffic_download_samples: vec![0.0; TRAFFIC_WINDOW],
            traffic_upload_samples: vec![0.0; TRAFFIC_WINDOW],
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

    /// 采一次内核流量，顺便推进那一格曲线。
    ///
    /// 计数只有内核知道，所以每次都是问它要累计值，再由我们两次差分出速率：
    /// 这样即使某次状态没拉到，下一格也只是变长一点，不会凭空少一段流量。
    pub fn sample_traffic(&mut self) {
        if !self.core.is_running() {
            self.traffic_mark = None;
            self.traffic_download_rate = 0.0;
            self.traffic_upload_rate = 0.0;
            self.traffic_connections = 0;
            return;
        }
        let Ok(snapshot) = self.core.traffic(&self.config) else {
            return;
        };
        self.traffic_download_total = snapshot.download;
        self.traffic_upload_total = snapshot.upload;
        self.traffic_connections = snapshot.connections;

        let now = Instant::now();
        let Some((last_at, last_down, last_up)) = self.traffic_mark else {
            self.traffic_mark = Some((now, snapshot.download, snapshot.upload));
            return;
        };
        let elapsed = now.duration_since(last_at);
        if elapsed < TRAFFIC_STEP {
            return;
        }
        let seconds = elapsed.as_secs_f64().max(0.1);
        // 内核重启会把计数清零，减上一次会得到负数，用饱和减法当 0 处理。
        let download = snapshot.download.saturating_sub(last_down) as f64 / seconds;
        let upload = snapshot.upload.saturating_sub(last_up) as f64 / seconds;
        self.traffic_mark = Some((now, snapshot.download, snapshot.upload));
        self.traffic_download_rate = download;
        self.traffic_upload_rate = upload;
        push_traffic_sample(&mut self.traffic_download_samples, download);
        push_traffic_sample(&mut self.traffic_upload_samples, upload);
    }

    pub fn apply_config(&mut self) -> Result<(), String> {
        crate::store::save(&self.config);
        // 只有配置真的变了才去动内核。存盘/巡检这类周期性调用配置是一样的，
        // 不设这道闸就会每隔一两分钟把内核重启一次 —— 每次重启 TUN 都要重建，
        // 路由表跟着抖，表现就是「接管一会儿有一会儿没有」。
        let snapshot = serde_json::to_string(&self.config).unwrap_or_default();
        let unchanged = !snapshot.is_empty() && snapshot == self.applied_snapshot;
        self.applied_snapshot = snapshot;
        if self.core.is_running() && !unchanged {
            self.core.write_config(&self.config)?;
            // 光把新配置写到磁盘，内核是不知道的：它只在启动时读一次，之后
            // 并不监听文件变化。少了这一步，「切换节点」就变成只有界面和配置
            // 文件变了，实际跑的还是旧节点——换到日本、出口还是美国。
            let reloaded = self.core.reload(&self.config).is_ok();
            // 重载完还要确认监听端口没丢：mihomo 的 listeners 不支持热重载，
            // 重载后 TUN 照常转发、日志照常打，但两个端口都没了。这时候只剩
            // 重启能把它们带回来。
            if !reloaded || !self.core.listeners_alive(&self.config) {
                eprintln!("内核热重载不可用，改为重启内核以套用新配置");
                let _ = self.core.stop();
                // 立刻重启会撞上还没释放干净的端口和网卡。
                std::thread::sleep(Duration::from_millis(1200));
                self.core.start(&self.config)?;
            }
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

/// 从内核日志尾部捞最后一条 fatal / error，用来说明「为什么起不来」。
///
/// mihomo 配置有错时是 `level=fatal msg="Parse config error: ..."` 然后立刻退出，
/// 只提示「核心异常退出」等于什么都没说，用户只能猜。
fn last_core_error() -> String {
    let text = read_log_tail(&core_log_path(), 16 * 1024, 80);
    text.lines()
        .rev()
        .find(|line| line.contains("level=fatal") || line.contains("level=error"))
        .and_then(|line| {
            let rest = line.split("msg=").nth(1)?.trim();
            Some(rest.trim_matches('"').to_string())
        })
        .filter(|message| !message.is_empty())
        .unwrap_or_default()
}

/// 可被 shutdown 打断的休眠；返回 true 表示收到退出信号。
async fn sleep_watch(shutdown: &Arc<AtomicBool>, total: Duration) -> bool {
    let deadline = Instant::now() + total;
    while Instant::now() < deadline {
        if shutdown.load(Ordering::SeqCst) {
            return true;
        }
        let step = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_millis(250));
        sleep(step).await;
    }
    shutdown.load(Ordering::SeqCst)
}

/// 接管核心自愈守护。
///
/// 内核（mihomo）是个独立子进程，目前除了手动开关没有任何守护：它一旦因为崩溃、
/// 被系统回收、资源不足之类的原因自己死掉，就再也没人把它拉起来。而「关到托盘」
/// 只是把窗口藏起来，内核本就该继续跑——于是就出现用户描述的故障：托盘还在，
/// 但 git 之类走代理的流量全断了，打开界面（手动重启）才恢复。
///
/// 这个任务跑在 tokio 运行时上，和 GUI 事件循环完全解耦，所以窗口藏起来也照常巡检。
/// 只要 `core_desired_running` 为 true（说明用户是要它跑着的）而内核实际没在跑，
/// 就重新拉起。`shutdown` 置位后立刻退出，避免主程序收尾停内核时被它又拉起来。
///
/// 两个容易踩的坑（都真实发生过）：
/// 1. `start()` 成功只代表**进程创建成功**。配置有错时 mihomo 读配置就 fatal 退出，
///    于是「拉起成功」每 15 秒重演一次，界面不停弹同一句「核心曾异常退出，已自动恢复」。
///    所以拉起后要等一小会儿确认真活着，死掉就把日志里的 fatal 原因带出来。
/// 2. 同一条提示不要反复弹：失败只在第一次提示，之后安静退避；恢复成功也要隔开
///    一段时间才再提示，避免「崩—恢复」循环刷屏。
pub async fn supervise_core(shared: crate::server::Shared, shutdown: Arc<AtomicBool>) {
    const RESTART_INTERVAL: Duration = Duration::from_secs(15);
    const MAX_BACKOFF: Duration = Duration::from_secs(120);
    /// 拉起后等这么久再确认它不是「刚起来就死」。
    const VERIFY_DELAY: Duration = Duration::from_millis(800);
    /// 两次「已自动恢复」提示之间至少隔这么久，避免循环刷屏。
    const NOTIFY_GAP: Duration = Duration::from_secs(120);
    /// TUN 起不来时，先等这么久再重启内核，给系统时间释放上一张网卡。
    const TUN_RETRY_DELAY: Duration = Duration::from_millis(1500);

    let mut backoff = RESTART_INTERVAL;
    let mut failures: u32 = 0;
    let mut last_notice: Option<Instant> = None;
    let mut last_tun_notice: Option<Instant> = None;

    loop {
        if shutdown.load(Ordering::SeqCst) {
            return;
        }

        // 先短促持锁判断，再在锁外等待，避免长期占着状态锁挡住界面操作。
        let needs_restart = {
            let mut state = shared.lock().await;
            state.core_desired_running
                && !state.core.is_running()
                && !shutdown.load(Ordering::SeqCst)
        };
        if !needs_restart {
            // 用户本来就是关着的，或者内核正跑着：恢复正常节奏。
            failures = 0;

            // 「进程活着」不等于「接管生效」。TUN 起不来时内核只打一条 error 就
            // 继续跑：端口在听、界面显示接管中，实际一条流量都没被接管，用户看到
            // 的就是「节点都挂着，出口 IP 还是本地的」。这是唯一能发现它的地方。
            let tun_error = {
                let state = shared.lock().await;
                if state.core_desired_running {
                    state.core.tun_start_error()
                } else {
                    None
                }
            };
            if let Some(reason) = tun_error {
                eprintln!("接管未真正生效：{reason}");
                // 残留的 wintun 网卡不先清掉，重启内核只会原样再失败一次。
                #[cfg(windows)]
                clear_stale_tun_adapter();
                let pending = {
                    let mut state = shared.lock().await;
                    if state.core_desired_running && !shutdown.load(Ordering::SeqCst) {
                        let _ = state.core.stop();
                        Some(state.config.clone())
                    } else {
                        None
                    }
                };
                if let Some(config) = pending {
                    // 立刻重启会撞上还没释放干净的网卡，等一下再拉起来。
                    if sleep_watch(&shutdown, TUN_RETRY_DELAY).await {
                        return;
                    }
                    let mut state = shared.lock().await;
                    if let Err(err) = state.core.start(&config) {
                        eprintln!("重启内核失败：{err}");
                    }
                }
                let should_notify = last_tun_notice
                    .map(|when| when.elapsed() >= NOTIFY_GAP)
                    .unwrap_or(true);
                if should_notify {
                    let mut state = shared.lock().await;
                    state.notify(
                        "error",
                        format!("接管没有真正生效（{reason}），已重启内核重试"),
                    );
                    last_tun_notice = Some(Instant::now());
                }
                if sleep_watch(&shutdown, RESTART_INTERVAL).await {
                    return;
                }
                continue;
            }

            if sleep_watch(&shutdown, RESTART_INTERVAL).await {
                return;
            }
            continue;
        }

        let started = {
            let mut state = shared.lock().await;
            if !state.core_desired_running || shutdown.load(Ordering::SeqCst) {
                None
            } else {
                let config = state.config.clone();
                Some(state.core.start(&config))
            }
        };
        let started = match started {
            Some(result) => result,
            None => continue,
        };

        if let Err(err) = started {
            failures += 1;
            eprintln!("自动恢复接管核心失败：{err}");
            if failures == 1 {
                let mut state = shared.lock().await;
                state.notify("error", format!("接管核心启动失败：{err}"));
            }
            backoff = (backoff * 2).min(MAX_BACKOFF);
            if sleep_watch(&shutdown, backoff).await {
                return;
            }
            continue;
        }

        // 确认它不是「刚起来就死」：配置有错时这里会立刻发现。
        if sleep_watch(&shutdown, VERIFY_DELAY).await {
            return;
        }
        let alive = {
            let mut state = shared.lock().await;
            state.core.is_running()
        };
        if alive {
            let should_notify = last_notice
                .map(|when| when.elapsed() >= NOTIFY_GAP)
                .unwrap_or(true);
            if should_notify {
                let mut state = shared.lock().await;
                state.notify("warning", "接管核心曾异常退出，已自动恢复");
                last_notice = Some(Instant::now());
            }
            failures = 0;
            backoff = RESTART_INTERVAL;
            if sleep_watch(&shutdown, RESTART_INTERVAL).await {
                return;
            }
            continue;
        }

        // 起来了又立刻死：多半是配置被内核拒绝，重试也只会重复同样结局。
        failures += 1;
        let reason = last_core_error();
        eprintln!("接管核心启动后立即退出（第 {failures} 次）：{reason}");
        if failures == 1 {
            let mut state = shared.lock().await;
            if reason.is_empty() {
                state.notify("error", "接管核心启动后立即退出，请查看内核日志");
            } else {
                state.notify("error", format!("接管核心启动失败：{reason}"));
            }
            last_notice = Some(Instant::now());
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
        if sleep_watch(&shutdown, backoff).await {
            return;
        }
    }
}
