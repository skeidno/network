use serde_json::Value;
use std::path::Path;

use crate::models::AppConfig;
use crate::paths::settings_path;

pub fn load() -> AppConfig {
    let path = settings_path();
    if !path.exists() {
        let config = AppConfig::default();
        save(&config);
        return config;
    }
    match std::fs::read_to_string(&path).and_then(|raw| {
        serde_json::from_str::<Value>(&raw)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
    }) {
        Ok(value) => {
            let stored_version = value.get("version").and_then(|v| v.as_i64()).unwrap_or(0);
            match serde_json::from_value::<AppConfig>(value) {
                Ok(mut config) => {
                    config.version = crate::models::CONFIG_VERSION;
                    config.mode = config.mode.to_uppercase();
                    config.default_target = config.default_target.to_uppercase();
                    // 迁移只在内存里改的话，每次启动都要重算一遍，配置文件里的
                    // 旧值也永远在那里；改了就落盘，让用户看到真实生效的值。
                    if migrate(&mut config, stored_version) {
                        save(&config);
                    }
                    config
                }
                Err(_) => {
                    quarantine(&path);
                    let config = AppConfig::default();
                    save(&config);
                    config
                }
            }
        }
        Err(_) => {
            quarantine(&path);
            let config = AppConfig::default();
            save(&config);
            config
        }
    }
}

/// 老配置文件里 `close_to_tray` 的一次性纠正。
///
/// v10 时把它统一改成了 false（关闭即退出），理由是「托盘图标被 Windows 11 收进
/// 溢出区、点不到」。那个结论是错的：托盘收不到消息的真实原因是通知走 SendMessage
/// 不进消息队列、且通知码在 lParam 低位字 —— 那两个 bug 已经修好了（见
/// [`crate::tray`]）。托盘现在能点能唤回，默认值也就回到托盘类程序该有的行为：
/// 关闭只收起界面，程序继续在后台跑。存量配置一并纠正，否则升级后行为没变化。
/// 返回是否真的改动了（决定是否要重新落盘）。
fn migrate(config: &mut AppConfig, stored_version: i64) -> bool {
    if stored_version >= 11 {
        return false;
    }
    let before = config.close_to_tray;
    config.close_to_tray = AppConfig::default().close_to_tray;
    config.close_to_tray != before
}

fn quarantine(path: &Path) {
    let mut backup = path.to_path_buf();
    backup.set_extension("invalid.json");
    let _ = std::fs::rename(path, backup);
}

pub fn save(config: &AppConfig) {
    let path = settings_path();
    let temporary = path.with_extension("tmp");
    let payload = serde_json::to_string_pretty(config).unwrap_or_else(|_| "{}".into());
    let _ = std::fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")));
    if std::fs::write(&temporary, format!("{payload}\n")).is_ok() {
        let _ = std::fs::rename(&temporary, &path);
    }
}
