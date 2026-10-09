// 桌面应用不应该弹出黑色控制台窗口（Python 版靠 PyInstaller 的 --windowed
// 做到这一点）。GUI 子系统下 stdout 无处可去，所以 headless 调试模式会显式
// 挂回父进程的控制台，详见 attach_console()。
#![cfg_attr(windows, windows_subsystem = "windows")]

mod core;
mod credentials;
mod deploy;
// 原生界面只在 Windows 上编（tao + wry）；Linux 的形态是常驻服务，用同名的
// 空实现顶住，这样命令层照旧调用 crate::gui::*，不需要到处写 cfg。
#[cfg(windows)]
mod gui;
#[cfg(not(windows))]
#[path = "gui_stub.rs"]
mod gui;
mod gui_common;
mod importers;
mod methods;
mod mihomo;
mod models;
mod paths;
mod platform;
mod portable;
mod server;
mod sshclient;
mod ssh;
mod startup;
mod state;
mod store;
mod tray;

use std::sync::Arc;
use tokio::sync::Mutex;

use crate::core::AppState;
use crate::server::{display_url, serve, Shared};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone)]
struct Options {
    host: String,
    port: u16,
    username: String,
    password: String,
    start_core: bool,
    open_browser: bool,
    headless: bool,
    start_hidden: bool,
}

impl Options {
    fn parse() -> Self {
        let mut options = Options {
            host: std::env::var("NETWORK_MANAGER_WEB_HOST").unwrap_or_else(|_| "127.0.0.1".into()),
            port: std::env::var("NETWORK_MANAGER_WEB_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(9091),
            username: std::env::var("NETWORK_MANAGER_WEB_USERNAME")
                .unwrap_or_else(|_| "admin".into()),
            password: std::env::var("NETWORK_MANAGER_WEB_PASSWORD").unwrap_or_default(),
            start_core: false,
            open_browser: true,
            headless: false,
            start_hidden: false,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--listen" => options.host = args.next().unwrap_or_default(),
                "--port" => {
                    options.port = args
                        .next()
                        .and_then(|value| value.parse().ok())
                        .unwrap_or(options.port)
                }
                "--username" => options.username = args.next().unwrap_or_default(),
                "--password" => options.password = args.next().unwrap_or_default(),
                "--start-core" => options.start_core = true,
                "--no-browser" => options.open_browser = false,
                "--headless" => options.headless = true,
                "--startup" => options.start_hidden = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                _ => {}
            }
        }
        if options.host.is_empty() {
            options.host = "127.0.0.1".into();
        }
        if options.username.is_empty() {
            options.username = "admin".into();
        }
        options
    }
}

fn print_help() {
    println!(
        "Network Manager {VERSION} (Rust WebGUI)\n\
         \n\
         用法：\n\
         \x20 --listen <host>     监听地址（默认 127.0.0.1，环境变量 NETWORK_MANAGER_WEB_HOST）\n\
         \x20 --port <port>       监听端口（默认 9091，环境变量 NETWORK_MANAGER_WEB_PORT）\n\
         \x20 --username <name>   Basic 认证用户名（默认 admin）\n\
         \x20 --password <secret> Basic 认证密码（环境变量 NETWORK_MANAGER_WEB_PASSWORD）\n\
         \x20 --start-core        启动 WebGUI 时自动拉起内核\n\
         \x20 --headless          仅运行 HTTP 服务，不创建原生窗口（Windows）\n\
         \x20 --startup           随系统启动：隐藏窗口，仅驻留托盘（Windows）\n\
         \n\
         Linux 上没有原生界面，程序始终以常驻服务方式运行，管理页面用浏览器打开\n\
         上面打印的地址；用 SIGTERM（systemctl stop）结束。\n"
    );
}

/// 端口上已经有服务在响应，说明另一个实例正在运行。
async fn existing_instance_running(options: &Options) -> bool {
    let Ok(client) = reqwest::Client::builder().no_proxy().build() else {
        return false;
    };
    let Ok(response) = client
        .get(format!("http://{}:{}/", options.host, options.port))
        .send()
        .await
    else {
        return false;
    };
    // 200/401/403 都说明有服务在监听；连接被拒才是没有。
    response.status().as_u16() != 404
}

/// 唤醒已经在运行的实例：先让它自己走一遍显示逻辑（会按需重建 WebView），
/// 失败再退回到直接显示它的窗口。
async fn wake_existing_instance(options: &Options) -> bool {
    let base = format!("http://{}:{}", options.host, options.port);
    if let Ok(client) = reqwest::Client::builder().no_proxy().build() {
        if let Ok(response) = client.get(format!("{base}/")).send().await {
            let html = response.text().await.unwrap_or_default();
            let token = html
                .split("name=\"network-session-token\" content=\"")
                .nth(1)
                .and_then(|rest| rest.split('"').next())
                .unwrap_or_default()
                .to_string();
            if !token.is_empty() {
                let body = serde_json::json!({"method": "windowAction", "args": ["show"]});
                let sent = client
                    .post(format!("{base}/api/call"))
                    .header("X-Network-Session", token)
                    .header("Content-Type", "application/json")
                    .body(serde_json::to_string(&body).unwrap_or_default())
                    .send()
                    .await;
                if matches!(sent, Ok(response) if response.status().is_success()) {
                    return true;
                }
            }
        }
    }
    show_existing_window()
}

/// 兜底：按照窗口标题找到已运行实例的主窗口并显示。
#[cfg(windows)]
fn show_existing_window() -> bool {
    use windows::core::BOOL;
    use windows::Win32::Foundation::{HWND, LPARAM};
    use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, SetForegroundWindow, ShowWindow};

    struct Finder {
        owner: u32,
        found: Option<HWND>,
    }

    unsafe extern "system" fn each(window: HWND, lparam: LPARAM) -> BOOL {
        let finder = lparam.0 as *mut Finder;
        let mut title = [0u16; 256];
        let length =
            windows::Win32::UI::WindowsAndMessaging::GetWindowTextW(window, &mut title);
        if length > 0 {
            let title = String::from_utf16_lossy(&title[..length as usize]);
            if title.starts_with("Network Manager") {
                let mut pid = 0u32;
                windows::Win32::UI::WindowsAndMessaging::GetWindowThreadProcessId(
                    window,
                    Some(&mut pid),
                );
                if pid != (*finder).owner {
                    (*finder).found = Some(window);
                    return BOOL(0);
                }
            }
        }
        BOOL(1)
    }

    unsafe {
        let mut finder = Finder {
            owner: std::process::id(),
            found: None,
        };
        let _ = EnumWindows(
            Some(each),
            LPARAM(&mut finder as *mut Finder as isize),
        );
        if let Some(window) = finder.found {
            let _ = ShowWindow(window, windows::Win32::UI::WindowsAndMessaging::SW_SHOW);
            let _ = SetForegroundWindow(window);
            return true;
        }
    }
    false
}

#[cfg(not(windows))]
fn show_existing_window() -> bool {
    false
}

fn is_loopback(host: &str) -> bool {
    matches!(host.to_lowercase().as_str(), "127.0.0.1" | "localhost" | "::1")
}

/// 等到进程被要求退出（systemd stop 发 SIGTERM，前台 Ctrl-C 发 SIGINT）。
///
/// 退出前必须回到 main 做收尾：内核是子进程，直接被杀会留下残留的转发规则。
async fn wait_for_exit_signal() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut term = signal(SignalKind::terminate())?;
        let mut int = signal(SignalKind::interrupt())?;
        tokio::select! {
            _ = term.recv() => {},
            _ = int.recv() => {},
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
    }
    Ok(())
}

#[cfg(windows)]
fn open_in_browser(url: &str) {
    let _ = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
}

/// GUI 子系统启动时没有控制台，`println!` 会被丢弃。
/// 显式以 `--headless` 运行时挂回父进程控制台，方便排查问题。
#[cfg(windows)]
fn attach_console() {
    use windows::Win32::System::Console::{AllocConsole, AttachConsole, ATTACH_PARENT_PROCESS};
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS).is_err() {
            let _ = AllocConsole();
        }
    }
}

#[cfg(not(windows))]
fn attach_console() {}

/// 把恐慌现场写到日志文件。
///
/// 界面是 GUI 子系统程序，没有控制台，panic 信息默认随进程一起消失。落盘一份
/// （含 backtrace）后面才查得到「程序为什么自己没了」。
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let stamp = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
        let mut line = format!("[{stamp}] {info}\n");
        line.push_str(&format!("{}\n", std::backtrace::Backtrace::force_capture()));
        let path = crate::paths::logs_dir().join("app-crash.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            use std::io::Write;
            let _ = file.write_all(line.as_bytes());
        }
        previous(info);
    }));
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = Options::parse();
    install_panic_hook();
    if options.headless {
        attach_console();
    }
    if !is_loopback(&options.host) && options.password.is_empty() {
        return Err("远程监听必须设置 WebGUI 管理密码（--password）".into());
    }

    let mut app = AppState::new();
    app.access_username = options.username.clone();
    app.access_password = options.password.clone();
    app.session_token = crate::server::random_session_token();
    // Linux 上永远是常驻服务，没有界面可谈，等价于 headless。
    app.headless = options.headless || !cfg!(windows);

    // 先确认没有别的实例在跑：否则这一趟也会去启动/清理内核，
    // 把已经在跑的那个实例的内核打掉（表现为代理突然失效）。
    if existing_instance_running(&options).await {
        wake_existing_instance(&options).await;
        return Ok(());
    }

    let should_start = options.start_core || app.config.start_on_launch;
    if should_start && !app.core.is_running() {
        match app.core.start(&app.config.clone()) {
            Ok(()) => println!("内核已启动"),
            Err(err) => eprintln!("内核启动失败：{err}"),
        }
    }
    #[cfg(windows)]
    let close_to_tray = app.config.close_to_tray;

    let shared: Shared = Arc::new(Mutex::new(app));
    let bound = match serve(shared.clone(), &options.host, options.port).await {
        Ok(bound) => bound,
        Err(err) => {
            // 端口被占用 = 已经有一个实例在跑（通常是关到托盘了）。
            // 托盘里可能还混着旧实例被强杀后留下的死图标，点了没反应会让人误以为
            // 程序坏了，所以这里给一条不依赖托盘的唤醒通道：再启动一次就唤起窗口。
            if wake_existing_instance(&options).await {
                return Ok(());
            }
            return Err(err.into());
        }
    };
    let url = display_url(&options.host, bound.port());
    println!("Network Manager {VERSION} WebGUI: {url}");

    #[cfg(windows)]
    {
        if options.headless {
            wait_for_exit_signal().await?;
        } else {
            let shell = crate::gui::Shell {
                url: url.clone(),
                title: format!("Network Manager {VERSION}"),
                width: 1280.0,
                height: 860.0,
                close_to_tray,
                start_hidden: options.start_hidden,
            };
            let result = crate::gui::run(shell, shared.clone());
            crate::gui::trace(&format!("main: gui::run 返回 ok={}", result.is_ok()));
            if let Err(err) = result {
                eprintln!("界面退出：{err}");
                if err == crate::gui::LOOP_CRASHED {
                    // 事件循环崩了：收掉内核再退出，不要挂着一个没有界面的进程。
                    let mut state = shared.lock().await;
                    let _ = state.core.stop();
                    return Err(err.into());
                }
                // 其他情况（比如窗口创建失败）退回浏览器兜底。
                if options.open_browser {
                    open_in_browser(&url);
                }
                tokio::signal::ctrl_c().await?;
            }
        }
    }

    // Linux：没有原生界面，程序就是一条常驻服务（systemd 管着），管理页面用浏览器
    // 打开上面打印的地址。等到 SIGTERM/SIGINT 再往下走收尾。
    #[cfg(not(windows))]
    {
        wait_for_exit_signal().await?;
    }

    let mut state = shared.lock().await;
    crate::gui::trace("main: 拿到 state 锁，准备停内核");
    let _ = state.core.stop();
    crate::gui::trace("main: 内核已停，准备返回");
    Ok(())
}
