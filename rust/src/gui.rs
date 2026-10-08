//! Native desktop shell: a tao window hosting a wry WebView pointed at the
//! embedded WebGUI, plus the Win32 tray icon.

use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

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

    let webview = wry::WebViewBuilder::new()
        .with_url(&shell.url)
        .build(&window)
        .map_err(|err| format!("创建 WebView 失败：{err}"))?;

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
    let mut next_poll = Instant::now();

    event_loop.run(move |event, _target, control| {
        *control = ControlFlow::WaitUntil(next_poll);
        match event {
            Event::NewEvents(StartCause::Init) => {
                let _ = &webview;
            }
            Event::WindowEvent { event, .. } => match event {
                WindowEvent::CloseRequested => {
                    if quitting || !close_to_tray {
                        *control = ControlFlow::Exit;
                    } else {
                        window.set_visible(false);
                        set_window_visible(false);
                    }
                }
                WindowEvent::Destroyed => *control = ControlFlow::Exit,
                _ => {}
            },
            Event::UserEvent(action) => match action {
                AppEvent::Show => {
                    window.set_visible(true);
                    set_window_visible(true);
                    window.set_focus();
                }
                AppEvent::Hide => {
                    window.set_visible(false);
                    set_window_visible(false);
                }
                AppEvent::Minimize => window.set_minimized(true),
                AppEvent::Maximize => window.set_maximized(!window.is_maximized()),
                AppEvent::Close => {
                    window.set_visible(false);
                    set_window_visible(false);
                }
                AppEvent::Quit => {
                    quitting = true;
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
                        window.set_visible(true);
                        set_window_visible(true);
                        window.set_focus();
                    }
                    TrayCommand::ToggleCore => {
                        let _ = PROXY
                            .get()
                            .and_then(|slot| slot.lock().ok())
                            .and_then(|guard| guard.as_ref().map(|p| p.send_event(AppEvent::ToggleCore)));
                    }
                    TrayCommand::Quit => {
                        quitting = true;
                        *control = ControlFlow::Exit;
                    }
                }
            }
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
