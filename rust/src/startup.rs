//! Windows 开机自启注册。
//!
//! ## 为什么用任务计划，而不是 HKCU\...\Run
//!
//! exe 的清单是 `requireAdministrator`（双击即提权，用户不用右键选）。而 UAC
//! 环境下 Windows 会**静默跳过** Run 键下需要提权的程序 —— 登录时没有交互式
//! 会话可以弹 UAC，表现就是勾了「开机自启」却什么都不发生。任务计划可以用
//! `/RL HIGHEST` 注册，登录后直接以最高令牌拉起且不弹窗，所以改用它。
//!
//! 顺带把旧版本写在 Run 键里的那条删掉：留着会出现「静默失败 + 任务计划重复
//! 拉起」两种状态混在一起的情况。

#[cfg(windows)]
const TASK_NAME: &str = "NetWorkManger";
#[cfg(windows)]
const LEGACY_RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
#[cfg(windows)]
const LEGACY_RUN_VALUE: &str = "NetWorkManger";

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        // 迁移：旧版本留下的 Run 键条目一律清掉，不管这次是开还是关。
        remove_legacy_run_entry();
        if enabled {
            create_task()
        } else {
            delete_task()
        }
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Ok(())
    }
}

/// 配置里说要自启、但任务计划里没有（旧版 Run 键迁移、被用户手删）时补建一次。
///
/// 只在任务确实缺失时才动手，正常启动路径上最多多跑一次 `schtasks /Query`。
/// 启动入口调用，失败不阻断程序：自启没注册上不该影响这一次的使用。
pub fn ensure_enabled() -> Result<(), String> {
    #[cfg(windows)]
    {
        if task_exists() {
            return Ok(());
        }
        remove_legacy_run_entry();
        create_task()
    }
    #[cfg(not(windows))]
    {
        Ok(())
    }
}

#[cfg(windows)]
fn exe_command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|err| format!("无法定位自身路径：{err}"))?;
    Ok(format!("\"{}\" --startup", exe.to_string_lossy()))
}

/// 静默跑 schtasks：GUI 子系统进程调控制台工具会闪一个黑框，屏蔽掉。
#[cfg(windows)]
fn schtasks(args: &[&str]) -> Result<std::process::Output, String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    std::process::Command::new("schtasks")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map_err(|err| format!("调用 schtasks 失败：{err}"))
}

#[cfg(windows)]
fn describe(output: &std::process::Output) -> String {
    let mut text = String::new();
    for chunk in [&output.stdout, &output.stderr] {
        let part = String::from_utf8_lossy(chunk);
        let part = part.trim();
        if !part.is_empty() {
            if !text.is_empty() {
                text.push(' ');
            }
            text.push_str(part);
        }
    }
    let code = output.status.code().unwrap_or(-1);
    if text.is_empty() {
        format!("schtasks 退出码 {code}")
    } else {
        format!("{text}（退出码 {code}）")
    }
}

#[cfg(windows)]
fn create_task() -> Result<(), String> {
    let command = exe_command()?;
    let output = schtasks(&[
        "/Create",
        "/TN",
        TASK_NAME,
        "/TR",
        &command,
        "/SC",
        "ONLOGON",
        "/RL",
        "HIGHEST",
        "/F",
    ])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("创建开机自启任务失败：{}", describe(&output)))
    }
}

#[cfg(windows)]
fn task_exists() -> bool {
    matches!(schtasks(&["/Query", "/TN", TASK_NAME]), Ok(output) if output.status.success())
}

#[cfg(windows)]
fn delete_task() -> Result<(), String> {
    if !task_exists() {
        // 本来就没有：不算错误（用户可能从来没开过自启）。
        return Ok(());
    }
    let output = schtasks(&["/Delete", "/TN", TASK_NAME, "/F"])?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!("删除开机自启任务失败：{}", describe(&output)))
    }
}

/// 清掉旧版本写进 HKCU Run 的条目，不存在也不报错。
#[cfg(windows)]
fn remove_legacy_run_entry() {
    use windows::core::PCWSTR;
    use windows::Win32::System::Registry::{
        RegCloseKey, RegDeleteValueW, RegOpenKeyExW, HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE,
        REG_SAM_FLAGS,
    };

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(std::iter::once(0)).collect()
    }

    let key_path = wide(LEGACY_RUN_KEY);
    let value_name = wide(LEGACY_RUN_VALUE);
    unsafe {
        let mut key: HKEY = Default::default();
        // 打开键只为删值，KEY_SET_VALUE 足够（掩码沿用写入时的那一份）。
        let access = REG_SAM_FLAGS(KEY_SET_VALUE.0 | 0x0002_0000);
        if RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR::from_raw(key_path.as_ptr()),
            Some(0),
            access,
            &mut key,
        )
        .is_ok()
        {
            let _ = RegDeleteValueW(key, PCWSTR::from_raw(value_name.as_ptr()));
            let _ = RegCloseKey(key);
        }
    }
}
