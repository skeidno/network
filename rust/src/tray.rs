//! Win32 system tray icon (no third-party tray dependency).
//!
//! The icon owns a message-only window on a dedicated thread so its clicks never
//! block the WebGUI event loop.

#[cfg(windows)]
pub use windows_impl::*;

#[cfg(not(windows))]
pub use stub::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Open,
    ToggleCore,
    Quit,
}

#[cfg(windows)]
mod windows_impl {
    use super::TrayCommand;
    use std::sync::mpsc::Sender;

    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NOTIFYICONDATAW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu,
        DispatchMessageW, GetCursorPos, GetMessageW, LoadImageW, PostQuitMessage, RegisterClassW,
        SetForegroundWindow, TrackPopupMenu, TranslateMessage, HICON, IMAGE_ICON, LR_DEFAULTSIZE,
        LR_LOADFROMFILE, MSG, TRACK_POPUP_MENU_FLAGS, WM_DESTROY, WM_LBUTTONDBLCLK, WM_RBUTTONUP,
        WNDCLASSW,
    };

    const TRAY_MESSAGE: u32 = 0x0400 + 1; // WM_USER + 1
    const MENU_OPEN: usize = 1001;
    const MENU_TOGGLE: usize = 1002;
    const MENU_QUIT: usize = 1003;
    const TPM_RIGHTBUTTON: u32 = 0x0002;
    const TPM_RETURNCMD: u32 = 0x0100;

    pub fn spawn(icon_path: &str, sender: Sender<TrayCommand>) -> Result<(), String> {
        let icon_path = icon_path.to_string();
        std::thread::Builder::new()
            .name("network-manager-tray".into())
            .spawn(move || {
                if let Err(err) = run(&icon_path, &sender) {
                    eprintln!("托盘启动失败：{err}");
                }
            })
            .map_err(|err| format!("创建托盘线程失败：{err}"))?;
        Ok(())
    }

    fn run(icon_path: &str, sender: &Sender<TrayCommand>) -> Result<(), String> {
        unsafe {
            let module = GetModuleHandleW(None).map_err(|err| err.to_string())?;
            let instance = HINSTANCE(module.0);
            let class_name = wide("NetworkManagerTrayWindow");
            let mut class: WNDCLASSW = std::mem::zeroed();
            class.lpfnWndProc = Some(procedure);
            class.hInstance = instance;
            class.lpszClassName = windows::core::PCWSTR::from_raw(class_name.as_ptr());
            if RegisterClassW(&class) == 0 {
                return Err("注册托盘窗口类失败".into());
            }

            let window_name = wide("Network Manager Tray");
            let window = CreateWindowExW(
                Default::default(),
                windows::core::PCWSTR::from_raw(class_name.as_ptr()),
                windows::core::PCWSTR::from_raw(window_name.as_ptr()),
                Default::default(),
                0,
                0,
                0,
                0,
                None,
                None,
                Some(instance),
                None,
            )
            .map_err(|err| format!("创建托盘窗口失败：{err}"))?;

            let icon_path_wide = wide(icon_path);
            let icon = LoadImageW(
                None,
                windows::core::PCWSTR::from_raw(icon_path_wide.as_ptr()),
                IMAGE_ICON,
                0,
                0,
                LR_LOADFROMFILE | LR_DEFAULTSIZE,
            )
            .map_err(|err| format!("加载托盘图标失败：{err}"))?;
            let icon = HICON(icon.0);

            let mut tip = [0u16; 128];
            let label = wide("Network Manager");
            let copy = label.len().min(tip.len());
            tip[..copy].copy_from_slice(&label[..copy]);

            let mut data: NOTIFYICONDATAW = std::mem::zeroed();
            data.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
            data.hWnd = window;
            data.uID = 1;
            data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
            data.uCallbackMessage = TRAY_MESSAGE;
            data.hIcon = icon;
            data.szTip = tip;

            if !Shell_NotifyIconW(NIM_ADD, &data).as_bool() {
                return Err("添加托盘图标失败".into());
            }

            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                if message.message == TRAY_MESSAGE {
                    let event = message.lParam.0 as u32;
                    if event == WM_LBUTTONDBLCLK {
                        let _ = sender.send(TrayCommand::Open);
                    } else if event == WM_RBUTTONUP {
                        show_menu(window, sender);
                    }
                    continue;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
        }
        Ok(())
    }

    unsafe fn show_menu(window: HWND, sender: &Sender<TrayCommand>) {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let open_label = wide("打开界面");
        let toggle_label = wide("启动 / 停止内核");
        let quit_label = wide("退出");
        let _ = AppendMenuW(
            menu,
            Default::default(),
            MENU_OPEN,
            windows::core::PCWSTR::from_raw(open_label.as_ptr()),
        );
        let _ = AppendMenuW(
            menu,
            Default::default(),
            MENU_TOGGLE,
            windows::core::PCWSTR::from_raw(toggle_label.as_ptr()),
        );
        let _ = AppendMenuW(
            menu,
            Default::default(),
            MENU_QUIT,
            windows::core::PCWSTR::from_raw(quit_label.as_ptr()),
        );
        let mut point = POINT::default();
        let _ = GetCursorPos(&mut point);
        let _ = SetForegroundWindow(window);
        let chosen = TrackPopupMenu(
            menu,
            TRACK_POPUP_MENU_FLAGS(TPM_RIGHTBUTTON | TPM_RETURNCMD),
            point.x,
            point.y,
            Some(0),
            window,
            None,
        );
        if chosen.as_bool() {
            match chosen.0 as usize {
                MENU_OPEN => {
                    let _ = sender.send(TrayCommand::Open);
                }
                MENU_TOGGLE => {
                    let _ = sender.send(TrayCommand::ToggleCore);
                }
                MENU_QUIT => {
                    let _ = sender.send(TrayCommand::Quit);
                }
                _ => {}
            }
        }
        let _ = DestroyMenu(menu);
    }

    unsafe extern "system" fn procedure(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match message {
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(window, message, wparam, lparam),
        }
    }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }
}

#[cfg(not(windows))]
mod stub {
    use super::TrayCommand;
    use std::sync::mpsc::Sender;

    pub fn spawn(_icon_path: &str, _sender: Sender<TrayCommand>) -> Result<(), String> {
        Ok(())
    }
}
