//! Native desktop shell: a tao window hosting a wry WebView pointed at the
//! embedded WebGUI, plus the Win32 tray icon.

use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(windows)]
use wry::WebViewBuilderExtWindows;

use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
use tao::platform::run_return::EventLoopExtRunReturn;
use tao::window::WindowBuilder;

use crate::server::Shared;
use crate::tray::TrayCommand;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppEvent {
    Show,
    Hide,
    Minimize,
    Maximize,
    Close,
    Quit,
    /// 托盘线程发来的命令（打开界面 / 启停内核 / 退出）。
    Tray(TrayCommand),
}

pub struct Shell {
    pub url: String,
    pub title: String,
    pub width: f64,
    pub height: f64,
    pub close_to_tray: bool,
    pub start_hidden: bool,
}

static PROXY: OnceLock<Mutex<Option<EventLoopProxy<AppEvent>>>> = OnceLock::new();

/// 事件循环内部出现异常（被 catch_unwind 收住）时的返回标记，
/// main 据此收掉内核后退出，而不是退回「开浏览器 + 等 Ctrl-C」的兜底路径。
pub const LOOP_CRASHED: &str = "event-loop-crashed";

/// Bridge used by the HTTP command layer to drive the desktop shell.
pub fn send(event: AppEvent) {
    if let Ok(guard) = PROXY.get_or_init(|| Mutex::new(None)).lock() {
        if let Some(proxy) = guard.as_ref() {
            let _ = proxy.send_event(event);
        }
    }
}

/// 创建承载界面的 WebView。
///
/// --disable-gpu-sandbox：本机（火绒 HIPS 驱动 + WebView2 Runtime 154 组合）
/// 会拦截 GPU 子进程的沙箱令牌创建，GPU 进程在 ~40ms 内静默退出（exit_code=7，
/// 无任何日志），连续失败 6 次后浏览器主进程自杀：
///   FATAL:content\browser\gpu\gpu_data_manager_impl_private.cc:436]
///   GPU process isn't usable. Goodbye.
/// 整个 WebView2 死掉，窗口呈现整块白屏/黑屏。只关 GPU 进程的沙箱即可绕过
/// （渲染进程沙箱保留），--no-sandbox 能修但影响面更大，不采用。
/// 佐证：--enable-logging 日志显示 GPU 进程连续 exit_code=7；--disable-gpu
/// 无效（GPU 信息收集进程仍会启动并失败）；Windows SearchHost 等系统宿主
/// 不受影响。
fn build_webview(window: &tao::window::Window, url: &str) -> Result<wry::WebView, String> {
    // 额外参数分两部分：
    //
    // 1) --disable-gpu-sandbox：本机（火绒 HIPS 驱动 + WebView2 Runtime 154 组合）
    //    会拦截 GPU 子进程的沙箱令牌创建，GPU 进程在 ~40ms 内静默退出（exit_code=7，
    //    无任何日志），连续失败 6 次后浏览器主进程自杀，窗口呈现整块白屏/黑屏。
    //    只关 GPU 进程的沙箱即可绕过（渲染进程沙箱保留）。
    //
    // 2) --disable-features=CalculateNativeWinOcclusion：Chromium 的窗口遮挡计算。
    //    本机的窗口常被其他全屏应用盖住，一旦被判定为「被遮挡」，渲染进程会被
    //    冻结：界面停在最后一帧、JS 定时器不再运行、鼠标点击完全没反应，
    //    而且重新置前后这个状态不会自动解除。禁用后按常规窗口处理，不再冻结。
    //    注意：additional browser args 会覆盖 wry 的默认参数，因此把默认值
    //    （msWebOOUI,msPdfOOUI,msSmartScreenProtection）一并写上。
    wry::WebViewBuilder::new()
        .with_url(url)
        .with_additional_browser_args(
            "--disable-gpu-sandbox --disable-features=msWebOOUI,msPdfOOUI,msSmartScreenProtection,CalculateNativeWinOcclusion",
        )
        .build(window)
        .map_err(|err| format!("创建 WebView 失败：{err}"))
}

/// 重新显示窗口。
///
/// 窗口被隐藏（关闭到托盘、开机自启隐藏启动）时，WebView2 会销毁承载画面与鼠标
/// 输入的渲染窗口（Chrome_RenderWidgetHostHWND），并且不会自动重建。此时界面看
/// 起来是正常的，但所有点击都没有反应。wry 只在创建时设置一次可见性，没有运行期
/// 的 set_visible，无法通知控制器恢复，因此这里在重新显示时重建 WebView。
fn show_window(
    window: &tao::window::Window,
    webview: &mut Option<wry::WebView>,
    url: &str,
    hidden: &mut bool,
) {
    window.set_minimized(false);
    window.set_visible(true);
    window.set_focus();
    if *hidden {
        // 只有重建成功才清掉标记：失败时保留 hidden，下一次再试，避免窗口
        // 永远停在「看得见但点不动」的状态。
        if let Ok(rebuilt) = build_webview(window, url) {
            *webview = Some(rebuilt);
            *hidden = false;
        }
    }
    set_window_visible(true);
}

fn set_window_visible(visible: bool) {
    if let Ok(mut guard) = WINDOW_VISIBLE.get_or_init(|| Mutex::new(true)).lock() {
        *guard = visible;
    }
}

static WINDOW_VISIBLE: OnceLock<Mutex<bool>> = OnceLock::new();

fn default_icon_path() -> Option<String> {
    let dir = crate::paths::app_data_dir();
    let target = dir.join("network-manager.ico");
    if target.exists() {
        return Some(target.to_string_lossy().to_string());
    }
    let bytes = crate::server::asset_bytes("icons/network-manager.ico")?;
    let _ = std::fs::create_dir_all(&dir);
    std::fs::write(&target, bytes).ok()?;
    Some(target.to_string_lossy().to_string())
}

pub fn run(shell: Shell, shared: Shared) -> Result<(), String> {
    // 用 run_return 而不是 run：run 内部会直接 std::process::exit()，
    // 事件循环一结束进程就没了，托盘图标来不及摘、内核也来不及收。
    // run_return 会把控制权交回来，我们能在循环外从容做收尾。
    let mut event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    PROXY
        .set(Mutex::new(Some(proxy)))
        .map_err(|_| "事件循环初始化失败".to_string())?;

    let icon_path = default_icon_path();
    let mut builder = WindowBuilder::new()
        .with_title(&shell.title)
        .with_inner_size(LogicalSize::new(shell.width, shell.height))
        .with_min_inner_size(LogicalSize::new(980.0, 660.0))
        .with_visible(!shell.start_hidden)
        .with_decorations(true);
    #[cfg(windows)]
    if let Some(path) = icon_path.as_deref() {
        use tao::platform::windows::IconExtWindows;
        use tao::window::Icon;
        if let Ok(icon) = Icon::from_path(path, None) {
            builder = builder.with_window_icon(Some(icon));
        }
    }
    let window = builder
        .build(&event_loop)
        .map_err(|err| format!("创建窗口失败：{err}"))?;
    set_window_visible(!shell.start_hidden);

    let url = shell.url.clone();
    let mut webview = Some(build_webview(&window, &url)?);

    if let Some(path) = icon_path.as_deref() {
        if let Err(err) = crate::tray::spawn(path) {
            eprintln!("{err}");
        }
    }

    let close_to_tray = shell.close_to_tray;
    let runtime = tokio::runtime::Handle::current();
    let mut quitting = false;
    let mut window_hidden = shell.start_hidden;
    // 运行期每 2 分钟检查一次日志体积，超过上限就只保留最新的部分。
    // 这是事件循环唯一的定时唤醒源：托盘命令改走 EventLoopProxy 即时唤醒，
    // 不再需要每 200ms 轮询一次（既耗电，又会被 tao 的定时器反复唤醒）。
    let mut next_log_trim = Instant::now() + Duration::from_secs(120);

    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        event_loop.run_return(move |event, _target, control| {
            *control = ControlFlow::WaitUntil(next_log_trim);
            match event {
                Event::NewEvents(StartCause::Init) => {
                    let _ = &webview;
                }
                Event::WindowEvent { event, .. } => match event {
                    WindowEvent::CloseRequested => {
                        if quitting || !close_to_tray {
                            request_exit(control);
                        } else {
                            window.set_visible(false);
                            set_window_visible(false);
                            window_hidden = true;
                        }
                    }
                    WindowEvent::Destroyed => request_exit(control),
                    _ => {}
                },
                // tao 收到 WM_ENDSESSION（注销 / 关机 / 重启）时会把内部状态直接置为
                // Destroyed；此后只要再派发任意一条事件就会
                // panic!("cannot move state from Destroyed")，进程当场 abort ——
                // 托盘图标来不及摘（变成点了没反应的死图标），内核也只剩作业对象兜底。
                //
                // 这里立刻投递 WM_QUIT 让消息泵停下：后续事件不会再被派发，
                // 循环随即结束，控制权回到 run()，由它在循环外做有序收尾。
                // 注意处理函数里绝不能做阻塞操作（比如等托盘线程摘图标）：
                // 那会让消息泵在处理函数执行期间继续派发事件，造成事件处理器重入，
                // 触发 "either event handler is re-entrant" 恐慌。
                Event::LoopDestroyed => stop_message_pump(),
                Event::UserEvent(action) => match action {
                    AppEvent::Show => show_window(&window, &mut webview, &url, &mut window_hidden),
                    AppEvent::Hide | AppEvent::Close => {
                        window.set_visible(false);
                        set_window_visible(false);
                        window_hidden = true;
                    }
                    AppEvent::Minimize => window.set_minimized(true),
                    AppEvent::Maximize => window.set_maximized(!window.is_maximized()),
                    AppEvent::Quit => {
                        quitting = true;
                        request_exit(control);
                    }
                    AppEvent::Tray(command) => match command {
                        TrayCommand::Open => {
                            show_window(&window, &mut webview, &url, &mut window_hidden)
                        }
                        TrayCommand::ToggleCore => toggle_core(&runtime, &shared),
                        TrayCommand::Quit => {
                            quitting = true;
                            request_exit(control);
                        }
                    },
                },
                _ => {}
            }

            if Instant::now() >= next_log_trim {
                crate::core::trim_core_log();
                next_log_trim = Instant::now() + Duration::from_secs(120);
            }
        });
    }));

    // 消息泵已经停了，这里做阻塞清理才是安全的（不会造成事件处理器重入）。
    crate::tray::request_close();
    if result.is_err() {
        return Err(LOOP_CRASHED.into());
    }
    Ok(())
}

/// 结束事件循环：只置标志，不做任何阻塞操作。
///
/// 摘托盘图标要等托盘线程回话（最多 600ms），必须放到事件循环之外，
/// 否则处理函数执行期间消息泵继续派发事件，会导致事件处理器重入恐慌。
fn request_exit(control: &mut ControlFlow) {
    *control = ControlFlow::Exit;
}

/// 立刻停掉消息泵（投递 WM_QUIT）。
#[cfg(windows)]
fn stop_message_pump() {
    use windows::Win32::UI::WindowsAndMessaging::PostQuitMessage;
    unsafe {
        PostQuitMessage(0);
    }
}

#[cfg(not(windows))]
fn stop_message_pump() {}

/// 启停内核（丢给 tokio 异步执行，避免阻塞界面线程）。
fn toggle_core(runtime: &tokio::runtime::Handle, shared: &Shared) {
    let state = Arc::clone(shared);
    runtime.spawn(async move {
        let mut guard = state.lock().await;
        if guard.core.is_running() {
            let _ = guard.core.stop();
        } else {
            let config = guard.config.clone();
            if let Err(err) = guard.core.start(&config) {
                guard.notify("error", err);
            }
        }
    });
}

#[cfg(windows)]
pub fn pick_ssh_key_blocking() -> Option<String> {
    use windows::core::{PCWSTR, PWSTR};
    use windows::Win32::UI::Controls::Dialogs::{
            GetOpenFileNameW, OPEN_FILENAME_FLAGS, OPENFILENAMEW,
        };

    unsafe {
        let mut buffer = vec![0u16; 4096];
        let mut filter: Vec<u16> = Vec::new();
        for part in ["私钥文件\0*.pem;*.key;*id_rsa*\0所有文件\0*.*\0"] {
            filter.extend(part.encode_utf16());
        }
        filter.push(0);
        let mut info: OPENFILENAMEW = std::mem::zeroed();
        info.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
        info.lpstrFilter = PCWSTR::from_raw(filter.as_ptr());
        info.lpstrFile = PWSTR::from_raw(buffer.as_mut_ptr());
        info.nMaxFile = buffer.len() as u32;
        info.Flags = OPEN_FILENAME_FLAGS(0x0000_1000 | 0x0000_0004); // OFN_FILEMUSTEXIST | OFN_EXPLORER
        if GetOpenFileNameW(&mut info).as_bool() {
            let length = buffer.iter().position(|&c| c == 0).unwrap_or(buffer.len());
            Some(String::from_utf16_lossy(&buffer[..length]))
        } else {
            None
        }
    }
}

#[cfg(not(windows))]
pub fn pick_ssh_key_blocking() -> Option<String> {
    None
}
