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

/// Re-launch the current executable with an administrator token, keeping the
/// current command line (so `--startup` / `--headless` survive the relaunch).
///
/// Returns `Ok(true)` when a new elevated process was really started — the
/// caller should then exit itself. `Ok(false)` means the UAC prompt was
/// declined or elevation is unavailable, so the caller keeps running as-is.
///
/// Note: `ShellExecuteW` alone can't tell these apart (a cancelled UAC prompt
/// reports 1223, which is > 32 and looks like success), hence `ShellExecuteExW`
/// with an explicit process handle.
pub fn restart_as_admin() -> Result<bool, String> {
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows::core::{w, PCWSTR};
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::UI::Shell::ShellExecuteExW;
        use windows::Win32::UI::Shell::{SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
        use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        fn wide(value: &str) -> Vec<u16> {
            value.encode_utf16().chain(std::iter::once(0)).collect()
        }

        let exe = std::env::current_exe().map_err(|err| format!("无法定位自身路径：{err}"))?;
        let exe_wide: Vec<u16> = exe
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        // 原样带上这次的启动参数（--startup 时提权重启后仍要回到托盘）。
        let params: Vec<String> = std::env::args().skip(1).collect();
        let params = params
            .iter()
            .map(|arg| {
                if arg.contains(' ') {
                    format!("\"{arg}\"")
                } else {
                    arg.clone()
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let params_wide = wide(&params);

        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: w!("runas"),
            lpFile: PCWSTR(exe_wide.as_ptr()),
            lpParameters: if params.is_empty() {
                PCWSTR::null()
            } else {
                PCWSTR(params_wide.as_ptr())
            },
            nShow: SW_SHOWNORMAL.0,
            ..Default::default()
        };

        let ok = unsafe { ShellExecuteExW(&mut info) };
        if let Err(err) = ok {
            return Err(format!("提权重启失败：{err}"));
        }
        // 用户点了「否」/无法提权时 hProcess 是空的：这时候不该把当前实例退掉，
        // 否则表现就是双击之后什么都没发生。
        if info.hProcess.is_invalid() {
            return Ok(false);
        }
        unsafe {
            let _ = CloseHandle(info.hProcess);
        }
        Ok(true)
    }
    #[cfg(not(windows))]
    {
        Err("仅 Windows 支持提权重启".to_string())
    }
}
