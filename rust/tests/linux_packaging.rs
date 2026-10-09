//! Linux 发布包的安装脚本 / systemd 单元契约。
//!
//! 这些断言原先是 Python 的 tests/test_linux_packaging.py。Python 那套删掉后
//! 它们仍然有价值：install.sh 和 .service 是发到客户服务器上的东西，改坏了
//! 编译期看不出来，只能靠这里兜住。

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("读不到 {}: {err}", path.display()))
}

/// 返回 (内核版本, [(产物文件名, sha256)])。
fn parse_mihomo_manifest() -> (String, Vec<(String, String)>) {
    let text = read(&repo_root().join("apps/linux/mihomo.version.json"));
    let value: serde_json::Value =
        serde_json::from_str(&text).expect("mihomo.version.json 不是合法 JSON");
    let version = value["version"].as_str().expect("manifest 缺 version").to_string();
    let assets = value["assets"]
        .as_object()
        .expect("manifest 缺 assets")
        .values()
        .map(|asset| {
            let name = asset["name"].as_str().expect("asset 缺 name").to_string();
            let sha256 = asset["sha256"].as_str().expect("asset 缺 sha256").to_string();
            (name, sha256)
        })
        .collect();
    (version, assets)
}

#[test]
fn mihomo_assets_match_installer() {
    let (version, assets) = parse_mihomo_manifest();
    assert_eq!(version, "v1.19.30");
    assert!(!assets.is_empty(), "manifest 里没有任何内核产物");
    let installer = read(&repo_root().join("apps/linux/install.sh"));
    for (name, sha256) in assets {
        assert!(installer.contains(&name), "install.sh 缺少内核产物 {name}");
        assert!(installer.contains(&sha256), "install.sh 缺少 {name} 的校验值");
    }
}

#[test]
fn installer_prefers_bundled_release_files() {
    let installer = read(&repo_root().join("apps/linux/install.sh"));
    // 程序本体是 Rust 单文件二进制，不再有 Python wheel / venv / pip。
    assert!(installer.contains("binary_candidates=("));
    assert!(installer.contains("\"${SCRIPT_ROOT}/network-manager-rs\""));
    assert!(installer.contains("\"${SCRIPT_ROOT}/${MIHOMO_ASSET}\""));
    assert!(installer.contains("SERVICE_SOURCE=\"${SCRIPT_ROOT}/network-manager.service\""));
    assert!(!installer.contains("venv/bin/python"), "install.sh 不该再碰 venv");
    assert!(!installer.contains("pip install"), "install.sh 不该再装 pip 包");
}

#[test]
fn service_runs_rust_binary_with_tun_capability() {
    let service = read(&repo_root().join("apps/linux/network-manager.service"));
    assert!(service.contains("ExecStart=/opt/network-manager/bin/network-manager-rs"));
    assert!(!service.contains("venv"), "service 不该再引用 venv");
    assert!(service.contains("EnvironmentFile=/etc/network-manager/network-manager.env"));
    for capability in [
        "CAP_NET_ADMIN",
        "CAP_SYS_PTRACE",
        "CAP_DAC_READ_SEARCH",
        "DeviceAllow=/dev/net/tun rw",
    ] {
        assert!(service.contains(capability), "service 缺少 {capability}");
    }
}

/// build.rs 把这份前端资源嵌进二进制，目录没了编译会直接失败，但报错信息很懵，
/// 这里给一条直白的断言。
#[test]
fn web_assets_are_embedded_source() {
    let web = repo_root().join("src/network_manager/web");
    assert!(web.join("index.html").is_file(), "缺少 web/index.html");
    assert!(web.join("app.js").is_file(), "缺少 web/app.js");
    assert!(web.join("icons").join("network-manager.ico").is_file());
}
