//! Node.js 版本管理，走 nvm-windows。
//!
//! 为什么单独一个模块而不是塞进 envmgr：Node 是唯一一个**同时装多个版本**的项。
//! 其它工具装一个就够，Node 项目要求什么版本就用什么版本，nvm 就是干这个的。
//! 把它铺在 envmgr 的 winget 逻辑里会让两边互相拖累，所以拆开。
//!
//! 一个必须知道的坑：`nvm root` 的输出**不是纯路径**，是
//! `Current Root: C:\Users\...\AppData\Local\nvm`，直接拿来拼路径必然失败。
//! 同理 `nvm current` 输出带 `v` 前缀（v22.22.2）而 `nvm list` 不带（22.19.0），
//! 而且当前版本**可能根本不在 nvm 管辖内**（系统里另装了一份 node 时就是），
//! 这时 `nvm list` 里根本不会出现当前版本。解析时都得当这种情况。

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

/// nvm-windows 本体对应的 winget 包。只装最新版，不参与版本管理 ——
/// nvm 自己升级用 `nvm upgrade` 就够了，给它开版本选择反而添乱。
pub const NVM_WINGET_ID: &str = "CoreyButler.NVMforWindows";

/// 跑一条 nvm 命令拿输出。失败返回空串。
///
/// 必须设 `NVM_SYMLINK`：nvm-windows 用这个环境变量决定软链接放在哪，
/// 不设的话它会去读一个可能不存在的全局配置并直接报错退出。
fn run_nvm(args: &[&str]) -> String {
    let bindir = PathBuf::from(r"C:\Program Files\nodejs");
    let result = Command::new("nvm")
        .args(args)
        .env("NVM_SYMLINK", bindir)
        .env("NVM_HOME", nvm_home())
        .output();
    match result {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).to_string();
            if stdout.trim().is_empty() {
                String::from_utf8_lossy(&out.stderr).to_string()
            } else {
                stdout
            }
        }
        Err(_) => String::new(),
    }
}

/// nvm 的家目录。装在当前用户下时是 `%LOCALAPPDATA%\nvm`。
///
/// 不去解析 `nvm root` 的输出来当路径 —— 那行输出带前缀，还得防它哪天变了格式。
/// 直接问环境变量更稳，`NVM_HOME` 是 nvm 自己装的时候写进用户环境的。
pub fn nvm_home_pub() -> String {
    nvm_home().to_string_lossy().to_string()
}

/// 已经装了几个 Node 版本。查不到返回 0。
pub fn installed_count() -> usize {
    if !installed() {
        return 0;
    }
    parse_list(&run_nvm(&["list"])).len()
}

fn nvm_home() -> PathBuf {
    if let Some(home) = std::env::var_os("NVM_HOME") {
        return PathBuf::from(home);
    }
    let local = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_default();
    local.join("nvm")
}

/// nvm 本体在不在。装了 nvm，Node 才有得管版本。
pub fn installed() -> bool {
    find_nvm_exe().is_some()
}

fn find_nvm_exe() -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join("nvm.exe");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    // nvm 装在用户目录但没进 PATH 时，靠 NVM_HOME 找
    let candidate = nvm_home().join("nvm.exe");
    if candidate.is_file() {
        return Some(candidate);
    }
    None
}

/// nvm 本体版本（`nvm --version`）。
pub fn version() -> String {
    let out = run_nvm(&["version"]);
    let text = out.trim();
    if text.is_empty() {
        return String::new();
    }
    // 输出形如 "1.2.2"，取第一段数字点分即可
    text.split_whitespace()
        .next()
        .filter(|token| token.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false))
        .unwrap_or(text)
        .to_string()
}

/// 解析 `nvm list` 的输出。
///
/// 实测 1.2.2 的真实输出（注意没有 v 前缀、没有星号，说明当前 node
/// 根本不在 nvm 管辖内 —— 见 [`parse_current`] 的说明）：
/// ```text
///     22.19.0
///     22.17.1
///     20.19.4
/// ```
/// 但 `nvm use` 过之后会长成 `v22.22.2 (64-bit)`，当前那个前面加 `*`，
/// 所以两种形态都得认。
fn parse_list(output: &str) -> Vec<String> {
    output
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            // 去掉行首的 v（v20.19.4 → 20.19.4）
            let body = trimmed.trim_start_matches(['v', 'V']);
            // 去掉标记当前版本的 * 号
            let body = body.trim_start_matches('*').trim();
            if !body.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                return None;
            }
            // 只要第一段版本号，后面跟着的 " (64-bit)" 之类的不要
            let mut version: String = body
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            while version.ends_with('.') {
                version.pop();
            }
            if version.is_empty() {
                None
            } else {
                Some(version)
            }
        })
        .collect()
}

/// `nvm list` 里哪些版本是当前在用的。
///
/// 注意这里**不能**假设 `nvm current` 的输出出现在 `nvm list` 里：
/// 系统里另装了一份 node（不走 nvm）时，`nvm current` 会报
/// 「Now using node v22.22.2 (64-bit)」但那版本不在 nvm 的目录中，
/// `nvm list` 里根本没有它。这种情况下 nvm 自己认为的「当前」就取 `*` 标记。
fn parse_current(output: &str) -> Option<String> {
    output
        .lines()
        .find(|line| line.trim_start().starts_with('*'))
        .and_then(|line| {
            let trimmed = line.trim().trim_start_matches('*').trim();
            let body = trimmed.trim_start_matches(['v', 'V']);
            let version: String = body
                .chars()
                .take_while(|c| c.is_ascii_digit() || *c == '.')
                .collect();
            if version.is_empty() { None } else { Some(version) }
        })
}

/// 解析 `nvm list available` 的四列表格。
///
/// 表格形如（实测）：
/// ```text
/// |   CURRENT    |     LTS      |  OLD STABLE  | OLD UNSTABLE |
/// |--------------|--------------|--------------|--------------|
/// |   26.12.1    |   24.22.1    |   0.12.18    |   0.11.16    |
/// ```
/// 只取 CURRENT 与 LTS 两列 —— OLD STABLE 那些是 0.10/0.12 时代的老货，
/// 界面上给用户选它们毫无意义。
fn parse_available(output: &str) -> Vec<(String, &'static str)> {
    let mut rows: Vec<(String, &'static str)> = Vec::new();
    let mut seen_header = false;
    for line in output.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with('|') && trimmed.contains("CURRENT") {
            seen_header = true;
            continue;
        }
        // 分隔行 |------|------|
        if trimmed.starts_with('|') && trimmed.contains('-') && !seen_header {
            continue;
        }
        if !seen_header || !trimmed.starts_with('|') {
            continue;
        }
        // 去掉首尾两个 | 后按 | 切
        let inner = trimmed.trim_start_matches('|').trim_end_matches('|');
        let cells: Vec<&str> = inner.split('|').map(|cell| cell.trim()).collect();
        // 前两列是 CURRENT / LTS
        for (index, channel) in [(0usize, "CURRENT"), (1usize, "LTS")] {
            if let Some(cell) = cells.get(index) {
                let version: String = cell
                    .chars()
                    .take_while(|c| c.is_ascii_digit() || *c == '.')
                    .collect();
                if version.is_empty() {
                    continue;
                }
                let channel: &'static str = match channel {
                    "CURRENT" => "CURRENT",
                    _ => "LTS",
                };
                // 同一个版本可能同时出现在两列（current 恰好也是 lts），去重
                if !rows.iter().any(|(v, _)| *v == version) {
                    rows.push((version, channel));
                }
            }
        }
    }
    rows
}

/// nvm 的完整状态，给前端下拉框用。
pub fn status() -> Value {
    let has_nvm = installed();
    if !has_nvm {
        return json!({
            "installed": false,
            "version": "",
            "root": "",
            "current": "",
            "installedVersions": [],
            "availableVersions": [],
            // 没装 nvm 时前端要把「先装 nvm」讲清楚，而不是给一个空的版本下拉
            "hint": "未检测到 nvm，安装指定 Node 版本需要先装它",
        });
    }

    let list_out = run_nvm(&["list"]);
    let installed_versions = parse_list(&list_out);
    let current = parse_current(&list_out);

    // available 要联网拉，几秒延迟。查不到不该影响已装版本的使用，所以失败即空列表
    let available = parse_available(&run_nvm(&["list", "available"]));

    json!({
        "installed": true,
        "version": version(),
        "root": nvm_home().to_string_lossy(),
        "current": current.unwrap_or_default(),
        "installedVersions": installed_versions,
        "availableVersions": available
            .into_iter()
            .map(|(version, channel)| json!({ "version": version, "channel": channel }))
            .collect::<Vec<Value>>(),
        "hint": "",
    })
}

/// 用 nvm 装指定版本并切过去。
///
/// 两步都要做：`install` 只把版本下载解压到 nvm 目录，不改软链接，
/// 不 `use` 的话用户命令行里跑的还是原来那个。
pub fn install_version(version: &str) -> Result<String, String> {
    let version = version.trim();
    if version.is_empty() {
        return Err("请指定要安装的 Node 版本".into());
    }
    if !installed() {
        return Err("本机没有 nvm，无法用 nvm 管理 Node 版本。\n请先在「环境管理」里安装 nvm（只装最新版即可）。".into());
    }
    // 只放行像版本号的参数。命令虽然不经过 shell，但 nvm 的参数会进自己的
    // 配置与日志，脏输入没必要往下传。
    if !version
        .chars()
        .all(|c| c.is_ascii_digit() || c == '.')
    {
        return Err(format!("{version} 不像是 Node 版本号"));
    }

    let mut chunks = Vec::new();
    let install_out = run_nvm(&["install", version]);
    // nvm 失败时输出里是 "error installing ..."，退出码不可靠（部分版本恒 0）
    if install_out.to_lowercase().contains("error") {
        chunks.push(format!("### nvm install {version}\n{install_out}"));
        return Err(format!(
            "nvm 安装 {version} 失败：\n{install_out}"
        ));
    }
    chunks.push(format!("### nvm install {version}\n{install_out}"));

    // 装完切过去
    let use_out = run_nvm(&["use", version]);
    if use_out.to_lowercase().contains("error") {
        chunks.push(format!("### nvm use {version}\n{use_out}"));
        return Err(format!(
            "{version} 装好了，但切换失败：\n{use_out}"
        ));
    }
    chunks.push(format!("### nvm use {version}\n{use_out}"));

    Ok(chunks.join("\n\n"))
}

/// 切换到某个已安装的版本。
pub fn use_version(version: &str) -> Result<String, String> {
    let version = version.trim();
    if version.is_empty() {
        return Err("请指定要切换到的 Node 版本".into());
    }
    if !installed() {
        return Err("本机没有 nvm。".into());
    }
    let out = run_nvm(&["use", version]);
    if out.to_lowercase().contains("error") {
        return Err(format!("切换到 {version} 失败：\n{out}"));
    }
    Ok(format!("已切换到 Node {version}。\n{out}"))
}

/// 卸载某个版本。当前正在用的不让卸 —— nvm 自己会拒绝，这里先拦一道给出更好懂的话。
pub fn uninstall_version(version: &str) -> Result<String, String> {
    let version = version.trim();
    if version.is_empty() {
        return Err("请指定要卸载的 Node 版本".into());
    }
    if !installed() {
        return Err("本机没有 nvm。".into());
    }
    let list_out = run_nvm(&["list"]);
    if let Some(current) = parse_current(&list_out) {
        if current == version {
            return Err(format!(
                "{version} 正在使用中，先切到别的版本再卸载（nvm use <其他版本>）"
            ));
        }
    }
    let out = run_nvm(&["uninstall", version]);
    if out.to_lowercase().contains("error") {
        return Err(format!("卸载 {version} 失败：\n{out}"));
    }
    Ok(format!("已卸载 Node {version}。\n{out}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析已安装版本列表() {
        // 实测本机 nvm 1.2.2 的真实输出形态
        let out = "\n    v22.19.0\n    22.17.1\n  * 22.22.2\n";
        assert_eq!(
            parse_list(out),
            vec!["22.19.0".to_string(), "22.17.1".to_string(), "22.22.2".to_string()]
        );
    }

    #[test]
    fn 解析当前版本认星号() {
        let out = "\n    22.19.0\n    22.17.1\n  * 22.22.2\n";
        assert_eq!(parse_current(out), Some("22.22.2".to_string()));
        // 一个都没有时返回 None，而不是空串
        assert_eq!(parse_current("\n  22.19.0\n"), None);
    }

    #[test]
    fn 列表里带架构标注也要剥掉() {
        // nvm use 后 list 会带 (64-bit) 之类标注
        let out = "    v20.19.4 (64-bit)\n  * 22.22.2 (64-bit)\n";
        assert_eq!(
            parse_list(out),
            vec!["20.19.4".to_string(), "22.22.2".to_string()]
        );
        assert_eq!(parse_current(out), Some("22.22.2".to_string()));
    }

    #[test]
    fn 解析可安装版本表格只取当前与LTS() {
        // 实测输出：四列，OLD STABLE 是 0.x 老货，不该给用户选
        let out = "\n|   CURRENT    |     LTS      |  OLD STABLE  | OLD UNSTABLE |\n|--------------|--------------|--------------|--------------|\n|   26.12.1    |   24.22.1    |   0.12.18    |   0.11.16    |\n|   26.12.0    |   24.22.0    |   0.12.17    |   0.11.15    |\n";
        let rows = parse_available(out);
        assert_eq!(rows.len(), 4);
        assert!(rows.contains(&("26.12.1".to_string(), "CURRENT")));
        assert!(rows.contains(&("24.22.1".to_string(), "LTS")));
        // OLD STABLE 的 0.12.18 不该出现
        assert!(!rows.iter().any(|(v, _)| v == "0.12.18"));
        // 表头与分隔行不该被当成数据
        assert!(!rows.iter().any(|(v, _)| v.contains("CURRENT")));
    }

    #[test]
    fn 可安装列表为空时返回空而不是报错() {
        assert!(parse_available("").is_empty());
        assert!(parse_available("network unreachable").is_empty());
    }

    #[test]
    fn 脏版本号被拦在命令之外() {
        // install_version 里对参数的检查：只放行数字和点
        for bad in ["latest; rm -rf /", "20.0.0 && calc", "abc", ""] {
            assert!(!bad
                .chars()
                .all(|c| c.is_ascii_digit() || c == '.')
                || bad.is_empty());
        }
        assert!("20.0.0".chars().all(|c| c.is_ascii_digit() || c == '.'));
    }
}