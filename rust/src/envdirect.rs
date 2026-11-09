//! 没有 winget 时的官方直连通道。
//!
//! winget 是随 Microsoft Store 的「应用安装程序」(App Installer) 下发的。全新
//! Windows、LTSC、Server 上这个包还没注册，`%LOCALAPPDATA%\Microsoft\WindowsApps\winget.exe`
//! 根本不存在，微软官方 Q&A 也明确写了「WinGet 在全新 Windows 实例上不可用」。
//! 原先所有安装动作都挂在 winget 上，于是在这类机器上一键部署 / 更新 / 版本管理
//! 整列失效，只剩一句「去官网下载」——这正是用户在新电脑上实际遇到的情况。
//!
//! 这里不引入第三方包管理器、不改注册表：只做三件事 —— 从官方 API 问版本、从
//! 官方下载地址取安装包、用各工具公开的静默参数装好。所有 URL 都做过 HEAD 实测。
//! 版本号只接受数字和点，下载落到临时目录，失败时把官方安装器的原始输出带回来。
//! 装完由调用方 `scan_fresh()` 复核，不靠「退出码 0」就宣称成功。
//!
//! 只覆盖有稳定官方直链的条目。没有稳定直链的项目宁可明确说「请修复 winget」，
//! 也不猜一个 URL 把用户环境装坏。

#[cfg(windows)]
mod imp {
    use serde_json::{json, Value};
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Output};

    const EOL_API: &str = "https://endoflife.date/api";
    const UA: &str = "NetWorkManger-EnvDirect/1.0";

    /// 有稳定官方直链的条目。和 `plan()` 的 match 分支一一对应。
    const SUPPORTED: &[&str] = &[
        "python", "node", "go", "git", "jdk", "webview2", "vcredist", "adb", "rust",
    ];

    pub fn supports(id: &str) -> bool {
        SUPPORTED.contains(&id)
    }

    /// 起一条不弹控制台窗口的命令。本程序是 GUI 子系统，没有自己的控制台，
    /// 不给 CREATE_NO_WINDOW 的话每下载一个包都会闪黑框。
    /// 泛型是为了让 winget 修复模块能直接传 `PathBuf`（应用执行别名的绝对路径）。
    pub(crate) fn hidden_command<P: AsRef<std::ffi::OsStr>>(program: P) -> Command {
        let mut cmd = Command::new(program);
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd
    }

    fn command(program: &str) -> Command {
        hidden_command(program)
    }

    fn system_exe(relative: &str) -> String {
        let root = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".to_string());
        let path = PathBuf::from(root).join("System32").join(relative);
        if path.is_file() {
            path.to_string_lossy().to_string()
        } else {
            Path::new(relative)
                .file_name()
                .map(|name| name.to_string_lossy().to_string())
                .unwrap_or_else(|| relative.to_string())
        }
    }

    fn curl_exe() -> String {
        system_exe("curl.exe")
    }

    fn powershell_exe() -> String {
        system_exe("WindowsPowerShell\\v1.0\\powershell.exe")
    }

    fn output_text(output: &Output) -> String {
        format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .trim()
        .to_string()
    }

    /// 同步 GET 一段文本。用系统自带 curl.exe 而不是 reqwest：这些调用都发生在
    /// `spawn_blocking` 的同步上下文里，起个子进程比在阻塞线程里 block_on 干净。
    fn http_text(url: &str, timeout: u64) -> Result<String, String> {
        let seconds = timeout.to_string();
        let output = command(&curl_exe())
            .args([
                "-sSL",
                "--fail",
                "--connect-timeout",
                "10",
                "--max-time",
                &seconds,
                "-A",
                UA,
                url,
            ])
            .output()
            .map_err(|error| format!("调用 curl.exe 失败：{error}"))?;
        if !output.status.success() {
            return Err(format!(
                "请求 {url} 失败（退出码 {:?}）：{}",
                output.status.code(),
                output_text(&output)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// PowerShell 一律走 `-EncodedCommand`（UTF-16LE + base64）。
    /// 临时目录可能含中文用户名，直接 `-Command` 传中文路径会按系统 ANSI 码页
    /// 解错，脚本里的引号也会被引擎拆坏。编码后没有这两种问题。
    pub(crate) fn powershell(script: &str) -> Result<String, String> {
        let utf16: Vec<u8> = script
            .encode_utf16()
            .flat_map(|unit| unit.to_le_bytes())
            .collect();
        let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &utf16);
        let output = command(&powershell_exe())
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-ExecutionPolicy",
                "Bypass",
                "-EncodedCommand",
                &encoded,
            ])
            .output()
            .map_err(|error| format!("调用 PowerShell 失败：{error}"))?;
        let text = output_text(&output);
        if !output.status.success() {
            return Err(format!("PowerShell 执行失败：{text}"));
        }
        Ok(text)
    }

    pub(crate) fn quote(value: &str) -> String {
        value.replace('\'', "''")
    }

    /// 逐段比数字，避免 "3.9" 排到 "3.10" 后面。
    pub(super) fn version_key(text: &str) -> Vec<u64> {
        text.split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect()
    }

    pub(super) fn clean_version(text: &str) -> String {
        text.trim()
            .trim_start_matches(['v', 'V'])
            .chars()
            .take_while(|c| c.is_ascii_digit() || *c == '.')
            .collect::<String>()
            .trim_end_matches('.')
            .to_string()
    }

    fn eol_name(id: &str) -> Option<&'static str> {
        match id {
            "python" => Some("python"),
            "node" => Some("nodejs"),
            "go" => Some("go"),
            "rust" => Some("rust"),
            "dotnet" => Some("dotnet"),
            "php" => Some("php"),
            "ruby" => Some("ruby"),
            _ => None,
        }
    }

    fn eol_entries(id: &str) -> Result<Vec<Value>, String> {
        let Some(name) = eol_name(id) else {
            return Ok(Vec::new());
        };
        let body = http_text(&format!("{EOL_API}/{name}.json"), 10)?;
        serde_json::from_str(&body).map_err(|error| format!("解析官方版本列表失败：{error}"))
    }

    /// 下拉里只摆还有维护意义的系列，别把 Python 3.0 混进来。
    pub(super) fn min_series(id: &str) -> (u64, u64) {
        match id {
            "python" => (3, 9),
            "node" => (20, 0),
            "go" => (1, 22),
            "php" => (8, 0),
            "ruby" => (3, 0),
            "dotnet" => (8, 0),
            "rust" => (1, 70),
            _ => (0, 0),
        }
    }

    /// winget 缺失时的版本下拉数据源：直接问官方发布周期，不再返回
    /// 「本机没有 winget，无法查询可安装版本」这种死胡同错误。
    pub fn versions(id: &str) -> Result<Value, String> {
        if eol_name(id).is_none() {
            return Ok(json!({
                "id": id,
                "versions": [],
                "versionKind": "latest",
                "source": "official",
                "note": "这一项由官方安装器固定提供最新稳定版，无需选择版本；点「一键部署」即可。",
            }));
        }
        let entries = eol_entries(id)?;
        let (min_major, min_minor) = min_series(id);
        let mut list: Vec<String> = entries
            .iter()
            .filter_map(|entry| entry.get("cycle").and_then(Value::as_str))
            .filter(|cycle| {
                cycle.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
                    && !cycle.contains('-')
            })
            .map(str::to_string)
            .collect();
        list.retain(|cycle| {
            let key = version_key(cycle);
            (key.first().copied().unwrap_or(0), key.get(1).copied().unwrap_or(0))
                >= (min_major, min_minor)
        });
        list.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
        list.dedup();
        list.truncate(12);
        Ok(json!({
            "id": id,
            "versions": list,
            "versionKind": "series",
            "source": "official",
            "note": "本机没有 winget，版本列表来自官方发布源；安装会走官方安装包直连。",
        }))
    }

    /// 把下拉选中的「大版本系列」换成可下载安装包的完整版本号。
    /// endoflife.date 的 `latest` 就是该系列的最新补丁号（Python 3.14 → 3.14.0）。
    fn latest_patch(id: &str, series: &str) -> Result<String, String> {
        let entries = eol_entries(id)?;
        let picked = if series.is_empty() {
            entries.first()
        } else {
            entries
                .iter()
                .find(|entry| entry.get("cycle").and_then(Value::as_str) == Some(series))
        };
        let entry = picked.ok_or_else(|| format!("官方发布源里没有 {id} {series} 这个系列"))?;
        let raw = entry
            .get("latest")
            .and_then(Value::as_str)
            .or_else(|| entry.get("cycle").and_then(Value::as_str))
            .ok_or_else(|| format!("官方发布源里没有 {id} 的版本号"))?;
        let version = clean_version(raw);
        if version.is_empty() {
            return Err(format!("官方发布源返回了无法识别的版本号：{raw}"));
        }
        Ok(version)
    }

    pub(super) fn series_of(version: &str, parts: usize) -> String {
        if version.is_empty() {
            return String::new();
        }
        let sliced: Vec<&str> = version.split('.').take(parts).collect();
        if sliced.len() == parts {
            sliced.join(".")
        } else {
            version.to_string()
        }
    }

    enum Kind {
        Msi,
        Exe,
        Zip,
    }

    struct Plan {
        url: String,
        file: String,
        kind: Kind,
        args: Vec<String>,
        note: String,
    }

    /// Git for Windows 走国内可直连的镜像，但**绝不按 tag 猜文件名**。
    ///
    /// 实测（2026-11-09）：GitHub tag `v2.56.0.windows.4` 对应的镜像文件名是
    /// `Git-2.56.0.4-64-bit.exe`，把 `.windows.` 压成了 `.`。按 tag 直拼会得到
    /// `Git-2.56.0.windows.4-64-bit.exe`，HEAD 实测 404。所以从镜像目录列表里
    /// 真正读出文件名和它自带的 url 字段，顺带也就不依赖 GitHub API 了。
    pub(super) fn git_plan() -> Result<(String, String), String> {
        const INDEX: &str = "https://registry.npmmirror.com/-/binary/git-for-windows/";
        let body = http_text(INDEX, 10)?;
        let entries: Vec<Value> = serde_json::from_str(&body)
            .map_err(|error| format!("解析 Git 镜像目录失败：{error}"))?;
        // 目录按版本升序返回；倒序取第一个正式版，跳过 -rc 预览版。
        let tag = entries
            .iter()
            .rev()
            .filter_map(|entry| entry.get("name").and_then(Value::as_str))
            .map(|name| name.trim_end_matches('/'))
            .find(|name| {
                name.starts_with('v') && name.contains(".windows.") && !name.contains("-rc")
            })
            .ok_or("Git 镜像里没有找到稳定版目录")?
            .to_string();
        let body = http_text(&format!("{INDEX}{tag}/"), 10)?;
        let files: Vec<Value> = serde_json::from_str(&body)
            .map_err(|error| format!("解析 Git 版本目录失败：{error}"))?;
        let file = files
            .iter()
            .find(|entry| {
                entry
                    .get("name")
                    .and_then(Value::as_str)
                    .map(|name| name.starts_with("Git-") && name.ends_with("-64-bit.exe"))
                    .unwrap_or(false)
            })
            .ok_or_else(|| format!("Git 镜像目录 {tag} 里没有 64 位安装包"))?;
        let name = file
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or("Git-64-bit.exe")
            .to_string();
        let url = file
            .get("url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{INDEX}{tag}/{name}"));
        Ok((url, name))
    }

    fn plan(id: &str, version: &str) -> Result<Plan, String> {
        let v = clean_version(version);
        if !v.chars().all(|c| c.is_ascii_digit() || c == '.') || v.contains("..") {
            return Err(format!("版本号不合法：{version}"));
        }
        let (url, file, kind, args, note) = match id {
            "python" => {
                let full = latest_patch("python", &series_of(&v, 2))?;
                (
                    format!("https://www.python.org/ftp/python/{full}/python-{full}-amd64.exe"),
                    format!("python-{full}-amd64.exe"),
                    Kind::Exe,
                    vec![
                        "/quiet".into(),
                        "InstallAllUsers=0".into(),
                        "PrependPath=1".into(),
                        "Include_test=0".into(),
                    ],
                    format!("Python {full}（官方 python.org 安装包，仅当前用户，不覆盖系统 Python）"),
                )
            }
            "node" => {
                let full = latest_patch("node", &series_of(&v, 1))?;
                (
                    format!("https://nodejs.org/dist/v{full}/node-v{full}-x64.msi"),
                    format!("node-v{full}-x64.msi"),
                    Kind::Msi,
                    vec![],
                    format!("Node.js {full}（官方 nodejs.org MSI）"),
                )
            }
            "go" => {
                let full = latest_patch("go", &series_of(&v, 2))?;
                (
                    format!("https://go.dev/dl/go{full}.windows-amd64.msi"),
                    format!("go{full}.windows-amd64.msi"),
                    Kind::Msi,
                    vec![],
                    format!("Go {full}（官方 go.dev MSI）"),
                )
            }
            "git" => {
                let (url, file) = git_plan()?;
                (
                    url,
                    file,
                    Kind::Exe,
                    vec![
                        "/VERYSILENT".into(),
                        "/NORESTART".into(),
                        "/NOCANCEL".into(),
                        "/SP-".into(),
                    ],
                    "Git for Windows 最新稳定版（官方安装包，经国内镜像下载）".into(),
                )
            }
            "jdk" => (
                "https://aka.ms/download-jdk/microsoft-jdk-21.0.5-windows-x64.msi".into(),
                "microsoft-jdk-21.0.5-windows-x64.msi".into(),
                Kind::Msi,
                vec![],
                "Microsoft OpenJDK 21 LTS（官方 aka.ms MSI）".into(),
            ),
            "webview2" => (
                "https://go.microsoft.com/fwlink/p/?LinkId=2124703".into(),
                "MicrosoftEdgeWebview2Setup.exe".into(),
                Kind::Exe,
                vec!["/silent".into(), "/install".into()],
                "WebView2 Evergreen Runtime（微软官方引导安装器）".into(),
            ),
            "vcredist" => (
                "https://aka.ms/vs/17/release/vc_redist.x64.exe".into(),
                "vc_redist.x64.exe".into(),
                Kind::Exe,
                vec!["/install".into(), "/quiet".into(), "/norestart".into()],
                "VC++ 2015-2022 运行库（微软官方 aka.ms）".into(),
            ),
            "adb" => (
                "https://dl.google.com/android/repository/platform-tools-latest-windows.zip".into(),
                "platform-tools-latest-windows.zip".into(),
                Kind::Zip,
                vec![],
                "Android Platform Tools（Google 官方 zip，解压到用户目录）".into(),
            ),
            "rust" => (
                "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe".into(),
                "rustup-init.exe".into(),
                Kind::Exe,
                vec![
                    "-y".into(),
                    "--profile".into(),
                    "minimal".into(),
                    "--default-toolchain".into(),
                    "stable".into(),
                ],
                "Rust stable（官方 rustup-init）".into(),
            ),
            _ => return Err(format!("{id} 暂不支持官方直连安装；请点「修复 winget」恢复完整安装能力")),
        };
        Ok(Plan { url, file, kind, args, note })
    }

    pub(crate) fn work_dir() -> Result<PathBuf, String> {
        let base = std::env::var_os("TEMP")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join("AppData").join("Local").join("Temp")))
            .ok_or("找不到临时目录")?;
        let dir = base.join("network-manager-env");
        fs::create_dir_all(&dir).map_err(|error| format!("创建临时目录失败：{error}"))?;
        Ok(dir)
    }

    fn tools_dir() -> Result<PathBuf, String> {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join("AppData").join("Local")))
            .ok_or("找不到用户数据目录")?;
        Ok(base.join("NetworkManager").join("tools"))
    }

    pub(crate) fn download(url: &str, dest: &Path) -> Result<u64, String> {
        let part = dest.with_extension("part");
        let part_text = part.to_string_lossy().to_string();
        let output = command(&curl_exe())
            .args([
                "-sSL",
                "--fail",
                "--retry",
                "2",
                "--connect-timeout",
                "15",
                "--max-time",
                "900",
                "-A",
                UA,
                "-o",
                &part_text,
                url,
            ])
            .output()
            .map_err(|error| format!("调用 curl.exe 失败：{error}"))?;
        if !output.status.success() {
            let _ = fs::remove_file(&part);
            return Err(format!(
                "下载失败：{url}\n退出码 {:?}：{}",
                output.status.code(),
                output_text(&output)
            ));
        }
        let size = fs::metadata(&part)
            .map_err(|error| format!("下载文件不存在：{error}"))?
            .len();
        if size < 1024 {
            let _ = fs::remove_file(&part);
            return Err(format!("下载内容异常（{size} 字节）：{url}"));
        }
        fs::rename(&part, dest).map_err(|error| format!("保存安装包失败：{error}"))?;
        Ok(size)
    }

    /// 把解压目录写进用户 PATH，并同步当前进程 PATH。
    /// 不同步的话 `scan_fresh()` 里的 find_exe 仍按旧 PATH 找，会误报没装上。
    fn add_user_path(dir: &Path) -> Result<(), String> {
        let text = dir.to_string_lossy().to_string();
        let escaped = quote(&text);
        powershell(&format!(
            "$d='{escaped}'; $p=[Environment]::GetEnvironmentVariable('Path','User'); \
             if (-not $p) {{ $p='' }}; \
             if ($p -notlike \"*$d*\") {{ [Environment]::SetEnvironmentVariable('Path', ($p.TrimEnd(';') + ';' + $d), 'User') }}"
        ))?;
        let current = std::env::var("PATH").unwrap_or_default();
        if !current.contains(&text) {
            std::env::set_var("PATH", format!("{};{}", current.trim_end_matches(';'), text));
        }
        Ok(())
    }

    fn install_zip(dest: &Path, id: &str) -> Result<String, String> {
        let target = tools_dir()?.join(id);
        fs::create_dir_all(&target).map_err(|error| format!("创建工具目录失败：{error}"))?;
        powershell(&format!(
            "Expand-Archive -LiteralPath '{}' -DestinationPath '{}' -Force",
            quote(&dest.to_string_lossy()),
            quote(&target.to_string_lossy())
        ))?;
        // platform-tools 的 zip 里还套了一层同名目录，PATH 要指到真正放 exe 的那层
        let nested = target.join("platform-tools");
        let bin = if nested.is_dir() { nested } else { target };
        add_user_path(&bin)?;
        Ok(format!("已解压到 {}，并写入用户 PATH", bin.display()))
    }

    fn run_installer(dest: &Path, plan: &Plan, id: &str) -> Result<String, String> {
        let path = dest.to_string_lossy().to_string();
        let output = match plan.kind {
            Kind::Msi => command("msiexec.exe")
                .args(["/i", &path, "/quiet", "/norestart"])
                .output(),
            Kind::Exe => command(&path).args(&plan.args).output(),
            Kind::Zip => return install_zip(dest, id),
        }
        .map_err(|error| format!("启动安装器失败：{error}"))?;
        let text = output_text(&output);
        if !output.status.success() {
            return Err(format!(
                "官方安装器退出码 {:?}：\n{}\n安装包留在 {} 便于排查",
                output.status.code(),
                text,
                dest.display()
            ));
        }
        Ok(text)
    }

    /// 直连安装一个条目。`version` 为空 = 官方最新稳定版；非空 = 下拉选中的系列。
    pub fn install(id: &str, version: &str) -> Result<String, String> {
        if !supports(id) {
            return Err(format!(
                "{id} 没有稳定的官方直连安装包。请点环境页的「修复 winget」，\n\
                 装好 App Installer 后即可用完整安装能力；也可以从卡片上的官网链接手动安装。"
            ));
        }
        let plan = plan(id, version)?;
        let dir = work_dir()?;
        let dest = dir.join(&plan.file);
        let size = download(&plan.url, &dest)?;
        let detail = run_installer(&dest, &plan, id);
        // 成功就把几十 MB 的安装包删掉；失败保留原始文件，错误信息里已经给了路径。
        if detail.is_ok() {
            let _ = fs::remove_file(&dest);
        }
        let output = detail?;
        Ok(format!(
            "官方直连安装：{}\n来源：{}\n下载：{:.1} MB\n\n{}",
            plan.note,
            plan.url,
            size as f64 / 1024.0 / 1024.0,
            if output.is_empty() { "安装器已静默完成（无输出）" } else { &output }
        ))
    }
}

#[cfg(windows)]
pub use imp::{install, supports, versions};

// 给 winget 修复模块复用同一套「隐藏窗口的子进程 + curl 下载 + 编码后的 PowerShell」，
// 避免两个模块各写一份、以后只修一边。
#[cfg(windows)]
pub(crate) use imp::{
    download as download_file, hidden_command, powershell as run_powershell, quote as ps_quote,
    work_dir as temp_work_dir,
};

#[cfg(not(windows))]
pub fn supports(_id: &str) -> bool {
    false
}

#[cfg(not(windows))]
pub fn versions(_id: &str) -> Result<serde_json::Value, String> {
    Err("官方直连版本查询只支持 Windows".into())
}

#[cfg(not(windows))]
pub fn install(_id: &str, _version: &str) -> Result<String, String> {
    Err("官方直连安装只支持 Windows".into())
}

#[cfg(test)]
mod tests {
    #[test]
    fn direct_channel_is_disabled_outside_windows() {
        // Linux 上环境页本来就是 Windows 专属能力，编译通过即可，
        // 不能让直连通道在非 Windows 平台上悄悄返回「成功」。
        #[cfg(not(windows))]
        {
            assert!(!super::supports("python"));
            assert!(super::install("python", "").is_err());
        }
    }
}

#[cfg(all(test, windows))]
mod win_tests {
    use super::imp::{clean_version, git_plan, hidden_command, min_series, series_of, supports, version_key};

    #[test]
    fn version_key_sorts_numeric_segments() {
        // 字符串排序会把 3.9 排到 3.10 后面，下拉必须按数字段倒序。
        let mut list = vec!["3.9", "3.10", "3.14", "4.0"];
        list.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
        assert_eq!(list, vec!["4.0", "3.14", "3.10", "3.9"]);
    }

    #[test]
    fn clean_version_strips_prefix_and_suffix() {
        assert_eq!(clean_version("v3.14.0-rc1"), "3.14.0");
        assert_eq!(clean_version(" 1.25.4 "), "1.25.4");
        assert_eq!(clean_version("3.11"), "3.11");
    }

    #[test]
    fn series_of_maps_full_version_to_series() {
        assert_eq!(series_of("3.14.0", 2), "3.14");
        assert_eq!(series_of("22.22.0", 1), "22");
        assert_eq!(series_of("", 2), "");
    }

    #[test]
    fn min_series_rejects_unmaintained_python() {
        let (major, minor) = min_series("python");
        let above = |text: &str| {
            let key = version_key(text);
            (key[0], key[1]) >= (major, minor)
        };
        assert!(above("3.14"));
        assert!(above("3.9"));
        assert!(!above("3.8"));
    }

    #[test]
    fn supported_ids_match_the_documented_whitelist() {
        for id in [
            "python", "node", "go", "git", "jdk", "webview2", "vcredist", "adb", "rust",
        ] {
            assert!(supports(id), "{id} 应当有官方直连方案");
        }
        // 这几项没有稳定直链，宁可明确提示「修复 winget」，也不猜 URL 装坏环境。
        for id in ["php", "ruby", "dotnet", "nvm"] {
            assert!(!supports(id), "{id} 不该被标成可直连");
        }
    }

    /// 联网自证：CI 不依赖外网，本机手动 `cargo test -- --ignored` 跑。
    #[test]
    #[ignore]
    fn official_version_sources_return_real_series() {
        let python = super::versions("python").expect("官方 Python 版本列表应可取");
        let list = python["versions"].as_array().expect("versions 应为数组");
        assert!(!list.is_empty(), "Python 系列不应为空");
        assert_eq!(python["source"], "official");
        let go = super::versions("go").expect("官方 Go 版本列表应可取");
        assert!(!go["versions"].as_array().unwrap().is_empty());

        // 回归：按 GitHub tag 直拼镜像文件名会得到 404，必须列目录拿真实文件名。
        let (url, name) = git_plan().expect("Git 镜像应能列出 64 位安装包");
        assert!(name.starts_with("Git-") && name.ends_with("-64-bit.exe"), "{name}");
        let probe = hidden_command("curl.exe")
            .args([
                "-sIL", "--max-time", "15", "-o", "NUL", "-w", "%{http_code}", &url,
            ])
            .output()
            .expect("应能调用 curl.exe");
        assert_eq!(
            String::from_utf8_lossy(&probe.stdout).trim(),
            "200",
            "镜像里的安装包 URL 必须真实可达：{url}"
        );
    }
}
