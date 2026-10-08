//! Win32 system tray icon (no third-party tray dependency).
//!
//! The icon owns a message-only window on a dedicated thread so its clicks never
//! block the WebGUI event loop.
//!
//! 交互约定：
//! - 左键单击（或双击）→ 打开主窗口；
//! - 右键 → 弹出菜单（打开界面 / 启停内核 / 退出）；
//! - 程序退出前必须调用 [`request_close`]，让托盘线程走 `NIM_DELETE`，
//!   否则托盘会留下一个点了没反应的「死图标」。

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
    use std::sync::atomic::{AtomicBool, AtomicPtr, AtomicUsize, Ordering};
    use std::sync::mpsc::Sender;
    use std::time::Duration;

    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NOTIFYICONDATAW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, GetCursorPos, GetMessageW, LoadImageW, PostMessageW, PostQuitMessage,
        RegisterClassW, RegisterWindowMessageW, SetForegroundWindow, TrackPopupMenu,
        TranslateMessage, HICON, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE, MSG,
        TRACK_POPUP_MENU_FLAGS, WM_CLOSE, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
        WM_LBUTTONDOWN, WM_RBUTTONUP, WNDCLASSW,
    };

    const TRAY_MESSAGE: u32 = 0x0400 + 1; // WM_USER + 1
    const MENU_OPEN: usize = 1001;
    const MENU_TOGGLE: usize = 1002;
    const MENU_QUIT: usize = 1003;
    const TPM_RIGHTBUTTON: u32 = 0x0002;
    const TPM_RETURNCMD: u32 = 0x0100;

    /// 托盘消息窗口句柄，供退出时投递 WM_CLOSE。
    static TRAY_WINDOW: AtomicUsize = AtomicUsize::new(0);
    /// 托盘线程是否已经完成 NIM_DELETE（优雅退出等待用）。
    static TRAY_REMOVED: AtomicBool = AtomicBool::new(false);
    /// NOTIFYICONDATA 常驻副本，explorer 重启（TaskbarCreated）后重新挂图标用。
    static TRAY_DATA: AtomicPtr<NOTIFYICONDATAW> = AtomicPtr::new(std::ptr::null_mut());
    /// RegisterWindowMessageW("TaskbarCreated") 的消息号。
    static TASKBAR_CREATED: AtomicUsize = AtomicUsize::new(0);

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

    /// 请求托盘线程清理图标并退出，最多等待 ~600ms。
    ///
    /// 进程退出时若直接杀掉线程，Shell_NotifyIconW(NIM_DELETE) 没机会执行，
    /// 托盘区就会残留一个点了没反应的死图标，只能等鼠标划过才被系统回收。
    pub fn request_close() {
        let hwnd = TRAY_WINDOW.load(Ordering::SeqCst);
        if hwnd != 0 {
            unsafe {
                let _ = PostMessageW(
                    Some(HWND(hwnd as *mut _)),
                    WM_CLOSE,
                    WPARAM(0),
                    LPARAM(0),
                );
            }
            for _ in 0..60 {
                if TRAY_REMOVED.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
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
            TRAY_WINDOW.store(window.0 as usize, Ordering::SeqCst);

            // explorer 重启后托盘会被整体清空，系统会广播 TaskbarCreated，
            // 收到后用保存的 NOTIFYICONDATA 重新挂一次即可。
            let taskbar_name = wide("TaskbarCreated");
            let taskbar = RegisterWindowMessageW(windows::core::PCWSTR::from_raw(
                taskbar_name.as_ptr(),
            ));
            if taskbar != 0 {
                TASKBAR_CREATED.store(taskbar as usize, Ordering::SeqCst);
            }

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
            // 常驻副本供 TaskbarCreated 重挂；窗口销毁前一直有效。
            TRAY_DATA.store(Box::into_raw(Box::new(data)), Ordering::SeqCst);

            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                if message.message == TRAY_MESSAGE {
                    handle_tray_event(message.lParam, sender);
                    continue;
                }
                if TASKBAR_CREATED.load(Ordering::SeqCst) != 0
                    && message.message == TASKBAR_CREATED.load(Ordering::SeqCst) as u32
                {
                    let data = TRAY_DATA.load(Ordering::SeqCst);
                    if !data.is_null() {
                        let _ = Shell_NotifyIconW(NIM_ADD, &*data);
                    }
                    continue;
                }
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
            let _ = Shell_NotifyIconW(NIM_DELETE, &data);
            TRAY_REMOVED.store(true, Ordering::SeqCst);
        }
        Ok(())
    }

    /// 托盘回调：lParam 是鼠标消息。
    ///
    /// 左键单击/双击都打开窗口（此前只认双击，单击毫无反应，体验像「摆设」）；
    /// 右键弹菜单，同时兼容新版 explorer 的 WM_CONTEXTMENU。
    fn handle_tray_event(lparam: LPARAM, sender: &Sender<TrayCommand>) {
        let event = lparam.0 as u32;
        match event {
            // 按下也响应：某些 explorer 版本/主题下抬起消息可能不到达，
            // 而 Open 是幂等的（窗口已经可见时不会重复重建 WebView）。
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                let _ = sender.send(TrayCommand::Open);
            }
            WM_RBUTTONUP => {
                unsafe {
                    show_menu(sender);
                }
            }
            _ => {}
        }
    }

    unsafe fn show_menu(sender: &Sender<TrayCommand>) {
        // 菜单挂在托盘消息窗口上，需要它的句柄来 SetForegroundWindow /
        // TrackPopupMenu，托盘线程里读一次即可。
        let window = HWND(TRAY_WINDOW.load(Ordering::SeqCst) as *mut _);
        if window.is_invalid() {
            return;
        }
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
            WM_CLOSE => {
                DestroyWindow(window);
                LRESULT(0)
            }
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

    pub fn request_close() {}
}
