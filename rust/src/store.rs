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
        Ok(value) => match serde_json::from_value::<AppConfig>(value) {
            Ok(mut config) => {
                config.version = crate::models::CONFIG_VERSION;
                config.mode = config.mode.to_uppercase();
                config.default_target = config.default_target.to_uppercase();
                config
            }
            Err(_) => {
                quarantine(&path);
                let config = AppConfig::default();
                save(&config);
                config
            }
        },
        Err(_) => {
            quarantine(&path);
            let config = AppConfig::default();
            save(&config);
            config
        }
    }
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
