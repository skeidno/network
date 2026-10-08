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
    /// 强制退出：连内核一起结束，整个进程立即消失。
    /// 普通「退出」走的是事件循环的正常收尾，界面若卡住就退不掉，这时用它兜底。
    ForceQuit,
}

#[cfg(windows)]
mod windows_impl {
    use super::TrayCommand;
    use std::sync::atomic::{AtomicBool, AtomicIsize, AtomicPtr, AtomicUsize, Ordering};
    use std::time::Duration;

    use windows::Win32::Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::Shell::{
        Shell_NotifyIconW, NOTIFYICONDATAW, NOTIFYICON_VERSION_4, NIF_ICON, NIF_MESSAGE, NIF_TIP,
        NIM_ADD, NIM_DELETE, NIM_SETVERSION,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyMenu, DestroyWindow,
        DispatchMessageW, GetCursorPos, GetMessageW, IsIconic, LoadImageW, PostMessageW,
        PostQuitMessage, RegisterClassW, RegisterWindowMessageW,
        SetForegroundWindow, ShowWindow, TrackPopupMenu, TranslateMessage,
        HICON, IMAGE_ICON, LR_DEFAULTSIZE, LR_LOADFROMFILE, MSG, SW_RESTORE,
        TRACK_POPUP_MENU_FLAGS, WM_CLOSE, WM_CONTEXTMENU, WM_DESTROY, WM_HOTKEY,
        WM_LBUTTONDBLCLK, WM_LBUTTONUP, WM_LBUTTONDOWN, WM_RBUTTONUP, WNDCLASSW,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        RegisterHotKey, UnregisterHotKey, MOD_ALT, MOD_CONTROL,
    };

    /// 回调消息号用 WM_APP 段：WM_USER 段会和窗口自身的消息号打架，
    /// 而且 0x401 恰好等于 NIN_KEYSELECT 的数值，排查时极易混淆。
    const TRAY_MESSAGE: u32 = 0x8000 + 1; // WM_APP + 1
    const MENU_OPEN: usize = 1001;
    const MENU_TOGGLE: usize = 1002;
    const MENU_QUIT: usize = 1003;
    const MENU_FORCE_QUIT: usize = 1004;
    const TPM_RIGHTBUTTON: u32 = 0x0002;
    const TPM_RETURNCMD: u32 = 0x0100;
    /// NOTIFYICON_VERSION_4 风格的通知码（explorer 版本/主题不同会走这套）。
    const NIN_SELECT: u32 = 0x0400;
    const NIN_KEYSELECT: u32 = 0x0401;
    const NIN_DOUBLECLK: u32 = 0x0403;
    /// 全局热键 Ctrl+Alt+?：唤回界面的兜底通道。
    ///
    /// Windows 11 会把新托盘图标默认收进「隐藏的图标」溢出区，用户很可能
    /// 根本看不到它（看到的反而是旧进程留下的死图标），这时托盘就完全指望不上。
    /// 热键由系统直接投递，不经过 explorer，是最可靠的兜底。
    ///
    /// 用户机器上的热键占用情况无法预知（这次 Ctrl+Alt+M 就被别的软件占了，
    /// 报 ERROR_HOTKEY_ALREADY_REGISTERED），所以按顺序试，注册上哪个用哪个。
    const HOTKEY_ID: i32 = 1;
    /// N / M / P / K / W —— Network Manager 的首字母优先。
    const HOTKEY_VKS: [u32; 5] = [0x4E, 0x4D, 0x50, 0x4B, 0x57];

    /// 托盘消息窗口句柄，供退出时投递 WM_CLOSE。
    static TRAY_WINDOW: AtomicUsize = AtomicUsize::new(0);
    /// 托盘线程是否已经完成 NIM_DELETE（优雅退出等待用）。
    static TRAY_REMOVED: AtomicBool = AtomicBool::new(false);
    /// NOTIFYICONDATA 常驻副本，explorer 重启（TaskbarCreated）后重新挂图标用。
    static TRAY_DATA: AtomicPtr<NOTIFYICONDATAW> = AtomicPtr::new(std::ptr::null_mut());
    /// RegisterWindowMessageW("TaskbarCreated") 的消息号。
    static TASKBAR_CREATED: AtomicUsize = AtomicUsize::new(0);
    /// 主窗口句柄：托盘线程收到点击后要自己把它拉到前台。
    /// 托盘线程此刻刚处理完 explorer 的输入，具备抢夺前台的资格；
    /// 交给界面线程去做则经常因为前台锁而失败（窗口显示了但在别的窗口后面）。
    static MAIN_WINDOW: AtomicIsize = AtomicIsize::new(0);

    pub fn spawn(icon_path: &str) -> Result<(), String> {
        let icon_path = icon_path.to_string();
        std::thread::Builder::new()
            .name("network-manager-tray".into())
            .spawn(move || {
                if let Err(err) = run(&icon_path) {
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
    /// 界面线程创建窗口后把句柄交给托盘线程，供点击时抢前台用。
    pub fn set_main_window(hwnd: isize) {
        MAIN_WINDOW.store(hwnd, Ordering::SeqCst);
    }

    /// 把主窗口拉到前台：先还原最小化，再 SetForegroundWindow。
    ///
    /// 必须在托盘线程里做。Windows 只允许「前台进程」或「刚收到输入的进程」
    /// 抢前台；托盘线程此刻正在处理 explorer 转发来的鼠标消息，资格是有的，
    /// 而界面线程往往没有，结果就是窗口显示了却缩在别的窗口后面。
    fn bring_main_to_front() {
        let raw = MAIN_WINDOW.load(Ordering::SeqCst);
        if raw == 0 {
            return;
        }
        unsafe {
            let window = HWND(raw as *mut _);
            if IsIconic(window).as_bool() {
                let _ = ShowWindow(window, SW_RESTORE);
            }
            let _ = SetForegroundWindow(window);
        }
    }

    /// 把 explorer 实际发来的通知码记进 logs/tray.log。
    ///
    /// 托盘「点了没反应」的排查难点在于看不到系统到底发了什么：自己 PostMessage
    /// 伪造的消息和系统真实通知完全可能不是同一个码。有了这份日志，用户点一次
    /// 就能知道 explorer 走的是 WM_LBUTTONUP 还是 NIN_SELECT。
    fn log_line(text: &str) {
        const MAX_BYTES: u64 = 16 * 1024;
        let dir = crate::paths::logs_dir();
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("tray.log");
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() >= MAX_BYTES {
                let _ = std::fs::remove_file(&path);
            }
        }
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let _ = writeln!(file, "{now} {text}");
        }
    }

    fn log_event(event: u32) {
        const MAX_BYTES: u64 = 16 * 1024;
        let Ok(dir) = std::panic::catch_unwind(crate::paths::logs_dir) else {
            return;
        };
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("tray.log");
        if let Ok(meta) = std::fs::metadata(&path) {
            if meta.len() >= MAX_BYTES {
                let _ = std::fs::remove_file(&path);
            }
        }
        use std::io::Write;
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0);
            let name = match event {
                WM_LBUTTONDOWN => "WM_LBUTTONDOWN",
                WM_LBUTTONUP => "WM_LBUTTONUP",
                WM_LBUTTONDBLCLK => "WM_LBUTTONDBLCLK",
                WM_RBUTTONUP => "WM_RBUTTONUP",
                WM_CONTEXTMENU => "WM_CONTEXTMENU",
                NIN_SELECT => "NIN_SELECT",
                NIN_KEYSELECT => "NIN_KEYSELECT",
                NIN_DOUBLECLK => "NIN_DOUBLECLK",
                _ => "unknown",
            };
            let _ = writeln!(file, "{now} 0x{event:04x} {name}");
        }
    }

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
            // 窗口已经销毁：清掉句柄，避免之后拿到失效句柄重复投递。
            TRAY_WINDOW.store(0, Ordering::SeqCst);
        }
    }

    fn run(icon_path: &str) -> Result<(), String> {
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
            // 必须切到 NOTIFYICON_VERSION_4：Windows 11 的 XAML 托盘只对 V4 图标
            // 稳定派发通知，不设置版本时图标照样显示，但单击收不到任何消息
            // （表现就是「图标在，点了没反应」）。V4 下左键是 NIN_SELECT、
            // 右键是 WM_CONTEXTMENU，两者上面都已处理。
            data.Anonymous.uVersion = NOTIFYICON_VERSION_4;
            let _ = Shell_NotifyIconW(NIM_SETVERSION, &data);

            // 热键注册失败（被别的软件占用）不致命：只是少一条唤回通道。
            for vk in HOTKEY_VKS {
                match RegisterHotKey(Some(window), HOTKEY_ID, MOD_CONTROL | MOD_ALT, vk) {
                    Ok(()) => {
                        let ch = char::from_u32(vk).unwrap_or('?');
                        log_line(&format!("hotkey Ctrl+Alt+{ch} 注册成功"));
                        break;
                    }
                    Err(err) => log_line(&format!("hotkey Ctrl+Alt+{} 被占用: {err}",
                        char::from_u32(vk).unwrap_or('?'))),
                }
            }
            // 常驻副本供 TaskbarCreated 重挂；窗口销毁前一直有效。
            TRAY_DATA.store(Box::into_raw(Box::new(data)), Ordering::SeqCst);

            let mut message: MSG = std::mem::zeroed();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                if message.message == TRAY_MESSAGE {
                    handle_tray_event(message.lParam);
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
    /// 把命令交给界面线程。
    ///
    /// 走 tao 的 EventLoopProxy（内部是 PostMessage 到事件循环的消息窗口），
    /// 界面线程会被立刻唤醒；以前用 mpsc 通道 + 每 200ms 轮询一次，既费电
    /// 也让点击有明显延迟。
    fn dispatch(command: TrayCommand) {
        crate::gui::send(crate::gui::AppEvent::Tray(command));
    }

    fn handle_tray_event(lparam: LPARAM) {
        let event = lparam.0 as u32;
        log_event(event);
        match event {
            // 两套通知码都认：
            // - 传统（uVersion=0）：explorer 转发原始鼠标消息；
            // - NOTIFYICON_VERSION_4 风格：转发 NIN_SELECT / NIN_KEYSELECT。
            // 不同 Windows 版本、不同 explorer 主题会走不同的一套，只认一套时
            // 表现就是「图标在，点了没反应」。
            WM_LBUTTONDOWN | WM_LBUTTONUP | WM_LBUTTONDBLCLK | NIN_SELECT | NIN_KEYSELECT
            | NIN_DOUBLECLK => {
                // 抢前台必须在托盘线程做，见 bring_main_to_front 的说明。
                bring_main_to_front();
                dispatch(TrayCommand::Open);
            }
            // 右键只认抬起和 WM_CONTEXTMENU：都认 WM_RBUTTONDOWN 的话一次点击会弹两次菜单。
            WM_RBUTTONUP | WM_CONTEXTMENU => {
                unsafe {
                    show_menu();
                }
            }
            _ => {}
        }
    }

    unsafe fn show_menu() {
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
        let force_label = wide("强制退出进程");
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
        let _ = AppendMenuW(
            menu,
            Default::default(),
            MENU_FORCE_QUIT,
            windows::core::PCWSTR::from_raw(force_label.as_ptr()),
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
                    bring_main_to_front();
                    dispatch(TrayCommand::Open);
                }
                MENU_TOGGLE => dispatch(TrayCommand::ToggleCore),
                MENU_QUIT => dispatch(TrayCommand::Quit),
                MENU_FORCE_QUIT => dispatch(TrayCommand::ForceQuit),
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
                let _ = DestroyWindow(window);
                LRESULT(0)
            }
            WM_HOTKEY => {
                // Ctrl+Alt+M：和托盘左键一样唤回界面（系统直接投递，不经 explorer）。
                log_line("收到 WM_HOTKEY");
                bring_main_to_front();
                dispatch(TrayCommand::Open);
                LRESULT(0)
            }
            WM_DESTROY => {
                let _ = UnregisterHotKey(Some(window), HOTKEY_ID);
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
    pub fn spawn(_icon_path: &str) -> Result<(), String> {
        Ok(())
    }

    pub fn request_close() {}

    pub fn set_main_window(_hwnd: isize) {}
}
