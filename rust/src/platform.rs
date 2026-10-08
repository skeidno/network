pub fn is_admin() -> bool {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Shell::IsUserAnAdmin;
        unsafe { IsUserAnAdmin().as_bool() }
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// Re-launch the current executable with an administrator token.
///
/// TUN mode creates a virtual network adapter, which Windows only allows from an
/// elevated process. The app starts un-elevated on purpose (no UAC prompt on
/// every launch), so the UI offers this as an explicit one-time action.
pub fn restart_as_admin() -> Result<(), String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::{w, PCWSTR};
        use windows::Win32::UI::Shell::ShellExecuteW;
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let exe = std::env::current_exe().map_err(|err| format!("无法定位自身路径：{err}"))?;
        // ShellExecuteW 需要以 NUL 结尾的宽字符串。
        let wide: Vec<u16> = exe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let operation = w!("runas");
        let handle = unsafe {
            ShellExecuteW(
                None,
                operation,
                PCWSTR(wide.as_ptr()),
                PCWSTR(std::ptr::null()),
                PCWSTR(std::ptr::null()),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecuteW 返回值 > 32 表示成功。
        if handle.0 as isize > 32 {
            Ok(())
        } else {
            Err(format!("提权重启被取消或失败（代码 {}）", handle.0 as isize))
        }
    }
    #[cfg(not(windows))]
    {
        Err("仅 Windows 支持提权重启".to_string())
    }
}
