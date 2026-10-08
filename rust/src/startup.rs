//! Windows "start with Windows" registration (HKCU Run key).

#[cfg(windows)]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

pub fn set_enabled(enabled: bool) -> Result<(), String> {
    #[cfg(windows)]
    {
        use windows::core::PCWSTR;
        use windows::Win32::Foundation::{ERROR_SUCCESS, WIN32_ERROR};
        use windows::Win32::System::Registry::{
            RegCloseKey, RegDeleteValueW, RegOpenKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
            KEY_SET_VALUE, REG_SAM_FLAGS, REG_VALUE_TYPE,
        };

        let key_path = wide("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
        let value_name = wide("NetWorkManger");
        unsafe {
            let mut key: HKEY = Default::default();
            let access = REG_SAM_FLAGS(KEY_SET_VALUE.0 | 0x0002_0000);
            let status: WIN32_ERROR = RegOpenKeyExW(
                HKEY_CURRENT_USER,
                PCWSTR::from_raw(key_path.as_ptr()),
                Some(0),
                access,
                &mut key,
            );
            if status != ERROR_SUCCESS {
                return Err(format!("打开启动项注册表失败：{status:?}"));
            }
            let result = if enabled {
                let command = wide(&format!(
                    "\"{}\" --startup",
                    std::env::current_exe()
                        .unwrap_or_default()
                        .to_string_lossy()
                ));
                let status = RegSetValueExW(
                    key,
                    PCWSTR::from_raw(value_name.as_ptr()),
                    Some(0),
                    REG_VALUE_TYPE(1), // REG_SZ
                    Some(std::slice::from_raw_parts(
                        command.as_ptr() as *const u8,
                        command.len() * 2,
                    )),
                );
                if status == ERROR_SUCCESS {
                    Ok(())
                } else {
                    Err(format!("写入启动项失败：{status:?}"))
                }
            } else {
                // Absence is not an error.
                let _ = RegDeleteValueW(key, PCWSTR::from_raw(value_name.as_ptr()));
                Ok(())
            };
            let _ = RegCloseKey(key);
            result
        }
    }
    #[cfg(not(windows))]
    {
        let _ = enabled;
        Ok(())
    }
}
