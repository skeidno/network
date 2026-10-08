use std::path::PathBuf;

pub const APP_NAME: &str = "NetWorkManger";

fn base_dir() -> PathBuf {
    if let Ok(override_dir) = std::env::var("NETWORK_MANAGER_DATA_DIR") {
        return PathBuf::from(override_dir);
    }
    let base = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join(APP_NAME)
}

pub fn app_data_dir() -> PathBuf {
    let path = base_dir();
    let _ = std::fs::create_dir_all(&path);
    path
}

pub fn settings_path() -> PathBuf {
    app_data_dir().join("settings.json")
}

pub fn generated_config_path() -> PathBuf {
    app_data_dir().join("mihomo-config.yaml")
}

pub fn logs_dir() -> PathBuf {
    let path = app_data_dir().join("logs");
    let _ = std::fs::create_dir_all(&path);
    path
}

pub fn core_log_path() -> PathBuf {
    logs_dir().join("mihomo.log")
}

pub fn ssh_credentials_path() -> PathBuf {
    app_data_dir().join("ssh-credentials.json")
}

pub fn ssh_known_hosts_path() -> PathBuf {
    app_data_dir().join("ssh-known-hosts")
}

pub fn core_binary() -> PathBuf {
    if let Ok(override_path) = std::env::var("NETWORK_MANAGER_CORE") {
        return PathBuf::from(override_path);
    }
    let name = if cfg!(windows) { "mihomo.exe" } else { "mihomo" };
    // 发布包把内核放在 exe 同级目录，因此优先在那里找，开发环境再回落到 vendor/。
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let beside = dir.join(&name);
            if beside.exists() {
                return beside;
            }
            let nested = dir.join("vendor").join(&name);
            if nested.exists() {
                return nested;
            }
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("vendor")
        .join(name)
}
