//! Native desktop shell: a tao window hosting a wry WebView pointed at the
//! embedded WebGUI, plus the Win32 tray icon.

use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[cfg(windows)]
use wry::WebViewBuilderExtWindows;

use tao::dpi::LogicalSize;
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder, EventLoopProxy};
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
    ToggleCore,
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
        if let Ok(rebuilt) = build_webview(window, url) {
            *webview = Some(rebuilt);
        }
    }
    *hidden = false;
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
    let event_loop = EventLoopBuilder::<AppEvent>::with_user_event().build();
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

    let tray_receiver: Option<Receiver<TrayCommand>> = icon_path.as_deref().and_then(|path| {
        let (sender, receiver) = channel();
        match crate::tray::spawn(path, sender) {
            Ok(()) => Some(receiver),
            Err(err) => {
                eprintln!("{err}");
                None
            }
        }
    });

    let close_to_tray = shell.close_to_tray;
    let runtime = tokio::runtime::Handle::current();
    let mut quitting = false;
    let mut window_hidden = shell.start_hidden;
    let mut next_poll = Instant::now();
    // 运行期每 2 分钟检查一次日志体积，超过上限就只保留最新的部分。
    let mut next_log_trim = Instant::now() + Duration::from_secs(120);

    event_loop.run(move |event, _target, control| {
        *control = ControlFlow::WaitUntil(next_poll);
        match event {
            Event::NewEvents(StartCause::Init) => {
                let _ = &webview;
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    if quitting || !close_to_tray {
                        // 真正退出：先让托盘线程摘掉图标，避免留下点了没反应的死图标。
                        crate::tray::request_close();
                        *control = ControlFlow::Exit;
                    } else {
                        window.set_visible(false);
                        set_window_visible(false);
                        window_hidden = true;
                    }
                }
                WindowEvent::Destroyed => {
                    crate::tray::request_close();
                    *control = ControlFlow::Exit;
                }
                _ => {}
            },
            Event::UserEvent(action) => match action {
                AppEvent::Show => show_window(&window, &mut webview, &url, &mut window_hidden),
                AppEvent::Hide => {
                    window.set_visible(false);
                    set_window_visible(false);
                    window_hidden = true;
                }
                AppEvent::Minimize => window.set_minimized(true),
                AppEvent::Maximize => window.set_maximized(!window.is_maximized()),
                AppEvent::Close => {
                    window.set_visible(false);
                    set_window_visible(false);
                    window_hidden = true;
                }
                AppEvent::Quit => {
                    quitting = true;
                    crate::tray::request_close();
                    *control = ControlFlow::Exit;
                }
                AppEvent::ToggleCore => {
                    let state = Arc::clone(&shared);
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
            },
            _ => {}
        }

        if let Some(receiver) = tray_receiver.as_ref() {
            while let Ok(command) = receiver.try_recv() {
                match command {
                    TrayCommand::Open => {
                        show_window(&window, &mut webview, &url, &mut window_hidden);
                    }
                    TrayCommand::ToggleCore => {
                        let _ = PROXY
                            .get()
                            .and_then(|slot| slot.lock().ok())
                            .and_then(|guard| guard.as_ref().map(|p| p.send_event(AppEvent::ToggleCore)));
                    }
                    TrayCommand::Quit => {
                        quitting = true;
                        crate::tray::request_close();
                        *control = ControlFlow::Exit;
                    }
                }
            }
        }
        if Instant::now() >= next_log_trim {
            crate::core::trim_core_log();
            next_log_trim = Instant::now() + Duration::from_secs(120);
        }

        next_poll = Instant::now() + Duration::from_millis(200);
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
