//! 开发环境体检与一键部署。
//!
//! 这一块刻意只做两件事：**如实报告本机现在有什么**，以及**在缺失时把安装命令跑起来**。
//! 不下载安装包、不写注册表、不改 PATH —— 装环境这种事一旦自己动手，出了问题没人说得清
//! 是谁的锅。所以安装统一走各工具官方的包管理器（winget / scoop / choco / 官方安装器），
//! 我们只负责把命令拼对、把输出如实转述给用户。
//!
//! 探测优先级：能直接问官方 exe 的（`--version`）就问 exe，问不到再查注册表
//! （很多运行库只往注册表写，不进 PATH），最后才退到「目录在不在」。三级都不中才算未就绪。

use serde_json::{json, Value};
use std::path::PathBuf;
use std::process::Command;

/// 单个环境的探测结果。前端只认这五个字段，别的一律塞进 `note`。
#[derive(Debug, Clone)]
pub struct EnvItem {
    /// 稳定 id，前端拿它做 testTag 和图标映射。
    pub id: &'static str,
    /// 界面上的名字，比如 "Node.js"。
    pub name: &'static str,
    /// 是否就绪。
    pub ready: bool,
    /// 版本号原文，探测不到就是空串。
    pub version: String,
    /// 命中的可执行文件绝对路径，探测不到就是空串。
    pub path: String,
    /// 补充说明：装在哪、为什么没探测到、建议怎么办。
    pub note: String,
    /// 安装方式，见 [`InstallKind`]。
    pub install: InstallKind,
}

/// 安装方式。决定「一键部署」按钮点下去到底干什么。
///
/// 只有 winget 一种：这一版刻意不支持静默安装包，也不改 PATH。
/// 将来真要加内置安装器时再扩枚举，别提前留没人用的分支。
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InstallKind {
    /// 走 winget。
    Winget,
}

impl EnvItem {
    fn new(id: &'static str, name: &'static str, install: InstallKind) -> Self {
        Self {
            id,
            name,
            ready: false,
            version: String::new(),
            path: String::new(),
            note: String::new(),
            install,
        }
    }

    /// 组装成给前端的 JSON。
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id,
            "name": self.name,
            "ready": self.ready,
            "version": self.version,
            "path": self.path,
            "note": self.note,
"install": match self.install {
InstallKind::Winget => "winget",
            },
            // 一键部署能不能点：取决于这台机器有没有 winget。前端拿它决定按钮灰不灰，
            // 比自己再判断一遍可靠。
            "canInstall": can_auto_install(self.id),
        })
    }
}

/// 跑一条命令拿 stdout，失败返回空串。
///
/// 刻意不隐藏输出错误：这里拿到的就是我们要转述给用户的东西，
/// 吞掉错误只会让「为什么探测不到」变成玄学。
fn run_capture(program: &str, args: &[&str]) -> String {
    let out = Command::new(program)
        .args(args)
        .output();
    match out {
        Ok(result) if result.status.success() => {
            let mut text = String::from_utf8_lossy(&result.stdout).trim().to_string();
            if text.is_empty() {
                text = String::from_utf8_lossy(&result.stderr).trim().to_string();
            }
            text
        }
        _ => String::new(),
    }
}

/// 在一段文本里找最长的那个「像版本号」的片段。
///
/// 各家 `--version` 的输出格式差别很大：
/// `Python 3.13.14`、`v22.19.0`、`git version 2.50.1.windows.1`、`openjdk version "21.0.11"`、
/// `Version 37.0.1-15733141`…… 与其给每家写一套正则，不如统一按字符类别扫：
/// 数字之间可以用 `.` 连接，也允许夹一个**单字母段**（Git 的 `.windows.1` 发行标记
/// 就是这样，去掉它就丢了用户要看的信息）。
///
/// 两条排除规则，都是被真实输出逼出来的：
/// - 必须至少有一个 `x.y` 形态的连接点，否则 `built for 64` 里的架构号 64 会被当版本；
/// - 遇到 `-` `+` 这类分隔符就断开，否则 `37.0.1-15733141` 会连成一坨还带上构建号。
fn extract_version(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut best = String::new();
    let mut i = 0usize;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let start = i;
        // dots：扫到的 `.` 连接点个数。至少 1 个才像版本号（x.y 起步）。
        let mut dots = 0usize;
        while i < chars.len() {
            let c = chars[i];
            if c.is_ascii_digit() {
                i += 1;
            } else if c == '.' {
                let follows_digit = chars.get(i + 1).map(|n| n.is_ascii_digit()).unwrap_or(false);
                // `.windows.` 这种：点后接一串字母，再点，再数字。字母段长度不限
                //（Git 的 "windows" 是 7 个字母，不是单个字符）。
                let mut word_end = i + 1;
                while matches!(chars.get(word_end), Some(a) if a.is_ascii_alphabetic()) {
                    word_end += 1;
                }
                let follows_word = word_end > i + 1
                    && chars.get(word_end) == Some(&'.')
                    && chars.get(word_end + 1).map(|n| n.is_ascii_digit()).unwrap_or(false);
                if follows_digit {
                    dots += 1;
                    i += 1;
                } else if follows_word {
                    dots += 1;
                    // 吃掉 ".字母."，停在后面那个数字上
                    i = word_end + 1;
                } else {
                    break;
                }
            } else {
                break;
            }
        }
        if dots >= 1 {
            let candidate: String = chars[start..i].iter().collect();
            if candidate.len() > best.len() {
                best = candidate;
            }
        }
        if i == start {
            i += 1;
        }
    }
    best
}

/// 在 PATH 和几个常见安装位置里找一个可执行文件。
///
/// 顺序刻意是「PATH 优先，常见目录兜底」：PATH 里的那个才是用户真正在用的，
/// 而注册表/默认目录里可能躺着多个版本，挑错版本比找不到更麻烦。
fn find_exe(names: &[&str]) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let mut candidates: Vec<PathBuf> = Vec::new();
    for dir in std::env::split_paths(&path_var) {
        for name in names {
            candidates.push(dir.join(name));
        }
    }
    // 常见安装位置（用户装在非 PATH 目录的情况）
    let extras: Vec<PathBuf> = vec![
        PathBuf::from(r"C:\Program Files\Git\cmd"),
        PathBuf::from(r"C:\Program Files\Git\bin"),
        PathBuf::from(r"C:\Program Files\nodejs"),
        PathBuf::from(r"C:\Program Files\Android\Android Studio\jbr\bin"),
        PathBuf::from(r"C:\Program Files\Microsoft\jdk-21.0.11.10-hotspot\bin"),
        PathBuf::from(r"C:\Program Files\Android\platform-tools"),
        PathBuf::from(r"C:\Program Files (x86)\Android\platform-tools"),
        PathBuf::from(r"C:\Program Files (x86)\Android\adb"),
        PathBuf::from(r"C:\Program Files\WebView2"),
    ];
    for extra in extras {
        for name in names {
            candidates.push(extra.join(name));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

// ---------------------------------------------------------------------------
// 单项探测
// ---------------------------------------------------------------------------

/// 用 `--version` 问一个 exe。拿到版本就算就绪。
fn probe_by_version(item: &mut EnvItem, exe_names: &[&str], args: &[&str]) -> bool {
    let Some(exe) = find_exe(exe_names) else {
        return false;
    };
    let exe_str = exe.to_string_lossy().to_string();
    let out = run_capture(&exe_str, args);
    let version = extract_version(&out);
    if version.is_empty() {
        // 找到了文件但问不出版本：算「装了但状态不明」，仍然按就绪处理但如实说明。
        item.ready = true;
        item.path = exe_str;
        item.note = "找到了可执行文件，但没能读出版本号".to_string();
        return true;
    }
    item.ready = true;
    item.version = version;
    item.path = exe_str;
    true
}

/// Python：优先用 launcher（py -3），没有再找 python。
fn probe_python(item: &mut EnvItem) {
    if probe_by_version(item, &["py.exe"], &["-3", "--version"]) {
        // launcher 报的是 "Python 3.13.14"，路径补成真实的解释器
        if let Some(real) = find_exe(&["python.exe"]) {
            item.path = real.to_string_lossy().to_string();
        }
        item.note = "通过 Python Launcher 探测".to_string();
        return;
    }
    if probe_by_version(item, &["python.exe"], &["--version"]) {
        return;
    }
    item.note = "未在 PATH 中找到 py.exe 或 python.exe".to_string();
}

/// Node.js：node --version 输出 v22.19.0，extract_version 会去掉前缀 v。
fn probe_node(item: &mut EnvItem) {
    if probe_by_version(item, &["node.exe"], &["--version"]) {
        return;
    }
    item.note = "未在 PATH 中找到 node.exe".to_string();
}

/// Git：输出形如 "git version 2.50.1.windows.1"。
fn probe_git(item: &mut EnvItem) {
    if probe_by_version(item, &["git.exe"], &["--version"]) {
        return;
    }
    item.note = "未在 PATH 中找到 git.exe".to_string();
}

/// ADB：Android 调试桥，Android 工具链要用。
fn probe_adb(item: &mut EnvItem) {
    if probe_by_version(item, &["adb.exe"], &["version"]) {
        // adb version 首行是 "Android Debug Bridge version 1.0.41"，
        // 真正有用的是第二行的 "Version 37.0.1-15733141"，整段存进去给界面显示。
        let out = run_capture(&item.path.clone(), &["version"]);
        let tail = out
            .lines()
            .find(|line| line.to_lowercase().contains("version "))
            .unwrap_or("")
            .trim();
        if !tail.is_empty() {
            let replaced = tail.replace("Version ", "");
            item.version = replaced;
            item.note = "Platform Tools".to_string();
        }
        return;
    }
    item.note = "未在 PATH 中找到 adb.exe".to_string();
}

/// JDK：javac -version 输出 javac 21.0.11；
/// 另有 Microsoft/Oracle 的 OpenJDK 装在非标准目录，所以多找几个。
fn probe_jdk(item: &mut EnvItem) {
    if probe_by_version(item, &["javac.exe"], &["-version"]) {
        if let Some(home) = java_home() {
            item.note = format!("JAVA_HOME 指向 {home}");
        }
        return;
    }
    if probe_by_version(item, &["java.exe"], &["-version"]) {
        return;
    }
    item.note = "未在 PATH 中找到 javac.exe；装了 JDK 记得配 JAVA_HOME".to_string();
}

#[cfg(windows)]
fn java_home() -> Option<String> {
    let out = Command::new("cmd")
        .args(["/C", "echo", "%JAVA_HOME%"])
        .output()
        .ok()?;
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if value.is_empty() || value == "%JAVA_HOME%" {
        None
    } else {
        Some(value)
    }
}

#[cfg(not(windows))]
fn java_home() -> Option<String> {
    std::env::var("JAVA_HOME").ok().filter(|v| !v.is_empty())
}

/// WebView2 运行时：它是本程序桌面端自己的渲染依赖，装不上桌面端直接起不来，
/// 所以优先级最高。走注册表读版本（运行时不一定有 exe 在 PATH）。
fn probe_webview2(item: &mut EnvItem) {
    #[cfg(windows)]
    {
        // EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5} 的 pv 就是版本号
        let key = r"HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
        let key2 = r"HKEY_CURRENT_USER\Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
        let key3 = r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}";
        for k in [key, key2, key3] {
            let out = run_capture("reg", &["query", k, "/v", "pv"]);
            // 精确按值名 pv 取，不要用「找像版本号的 token」那种粗办法
            if let Some(version) = reg_value(&out, "pv") {
                item.ready = true;
                item.version = version;
                item.path = k.to_string();
                item.note = "从 EdgeUpdate 注册表读取".to_string();
                return;
            }
        }
        // 注册表没有但默认目录在：装了 Evergreen 版。
        // 该目录常驻多个版本目录，取版本号最大的那个（比较段数与逐段数值）。
        let dir = PathBuf::from(r"C:\Program Files (x86)\Microsoft\EdgeWebView\Application");
        if let Ok(entries) = std::fs::read_dir(&dir) {
            let mut best: Option<String> = None;
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                if !name.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false) {
                    continue;
                }
                let better = match &best {
                    None => true,
                    Some(current) => compare_version(&name, current),
                };
                if better {
                    best = Some(name);
                }
            }
            if let Some(version) = best {
                item.ready = true;
                item.version = version;
                item.path = dir.to_string_lossy().to_string();
                item.note = "Evergreen 安装，未写入注册表版本键".to_string();
                return;
            }
        }
        item.note = "注册表与默认目录均未找到；桌面端界面依赖它，缺失时程序无法启动".to_string();
    }
    #[cfg(not(windows))]
    {
        let _ = item;
    }
}

/// 比较两个点分版本号，`true` 表示 `left` 更新。
/// 逐段按数值比，段数不同时多的那段补 0，所以 `1.2` 与 `1.2.0` 判等。
fn compare_version(left: &str, right: &str) -> bool {
    let parse = |text: &str| -> Vec<u64> {
        text.split('.')
            .map(|part| part.parse::<u64>().unwrap_or(0))
            .collect()
    };
    let l = parse(left);
    let r = parse(right);
    for index in 0..l.len().max(r.len()) {
        let lv = l.get(index).copied().unwrap_or(0);
        let rv = r.get(index).copied().unwrap_or(0);
        if lv != rv {
            return lv > rv;
        }
    }
    false
}

/// 从 `reg query` 的输出里取「值名 → 值」中某个值名的值。
///
/// `reg query` 的输出是三列：值名 / 类型 / 值，而且值本身可能带空格。
/// 不用 split_whitespace 全切 —— 注册表路径 `...\VisualStudio\14.0\VC\Runtimes\x64`
/// 自己也含点，按「含点就算版本」的粗办法会把这个路径当成版本号抓回来。
fn reg_value(output: &str, value_name: &str) -> Option<String> {
    output.lines().find_map(|line| {
        let mut parts = line.split_whitespace();
        let name = parts.next()?;
        if !name.eq_ignore_ascii_case(value_name) {
            return None;
        }
        // 类型列之后剩下的全部（含空格）才是值
        let _kind = parts.next()?;
        let value: Vec<&str> = parts.collect();
        if value.is_empty() {
            None
        } else {
            Some(value.join(" "))
        }
    })
}

/// VC++ 运行库：只写注册表，没有 exe 可问。
///
/// 两个要点：版本值形如 `v14.44.35211.00`（带 v 前缀），且光有版本键**不等于装了**
/// —— `Installed` 必须是 1。只看版本键会把卸载残留的也判成就绪。
fn probe_vcredist(item: &mut EnvItem) {
    #[cfg(windows)]
    {
        // 64 位 Windows 上 32 位注册表视图在 WOW6432Node 下，两个架构都要看
        let keys = [
            r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x64",
            r"HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\VisualStudio\14.0\VC\Runtimes\x64",
            r"HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\VisualStudio\14.0\VC\Runtimes\x86",
            r"HKEY_LOCAL_MACHINE\SOFTWARE\WOW6432Node\Microsoft\VisualStudio\14.0\VC\Runtimes\x86",
        ];
        let mut best_version = String::new();
        let mut best_key = String::new();
        for key in keys {
            let out = run_capture("reg", &["query", key, "/v", "Version"]);
            let Some(version) = reg_value(&out, "Version") else {
                continue;
            };
            // Installed=1 才算真装了；reg 输出的是十六进制 0x1
            let installed = run_capture("reg", &["query", key, "/v", "Installed"]);
            let flag = reg_value(&installed, "Installed").unwrap_or_default();
            let is_installed = flag.eq_ignore_ascii_case("0x1") || flag == "1";
            if !is_installed {
                continue;
            }
            // 版本号带 v 前缀，去掉更好看；x86 通常比 x64 低，留更高的那个
            let cleaned = version.trim_start_matches(['v', 'V']).to_string();
            if cleaned.len() > best_version.len() {
                best_version = cleaned;
                best_key = key.to_string();
            }
        }
        if !best_version.is_empty() {
            item.ready = true;
            item.version = best_version;
            item.path = best_key;
            item.note = "VC++ 2015-2022 Redistributable".to_string();
        } else {
            item.note = "注册表里没有已安装的 VC++ 2015-2022 运行库；依赖原生组件的程序可能起不来".to_string();
        }
    }
    #[cfg(not(windows))]
    {
        let _ = item;
    }
}

/// 体检全部条目。
///
/// 结果按「就绪的排前面，且按名字排」——不对，顺序保持目录序，
/// 因为用户扫一眼卡片列表时，稳定的位置比排序更有用。
pub fn scan() -> Value {
    let items = scan_items();
    let ready = items.iter().filter(|item| item.ready).count();
    json!({
        "items": items.iter().map(|item| item.to_json()).collect::<Vec<Value>>(),
        "total": items.len(),
        "ready": ready,
        "missing": items.len() - ready,
        // winget 有没有是「一键部署」能不能用的前提，没它整列按钮都得灰掉
        "winget": winget_available(),
    })
}

fn scan_items() -> Vec<EnvItem> {
    let mut items = Vec::new();

    let mut python = EnvItem::new("python", "Python", InstallKind::Winget);
    probe_python(&mut python);
    items.push(python);

    let mut node = EnvItem::new("node", "Node.js", InstallKind::Winget);
    probe_node(&mut node);
    items.push(node);

    let mut git = EnvItem::new("git", "Git", InstallKind::Winget);
    probe_git(&mut git);
    items.push(git);

    let mut jdk = EnvItem::new("jdk", "JDK", InstallKind::Winget);
    probe_jdk(&mut jdk);
    items.push(jdk);

    // 桌面端自己的渲染依赖：装不上程序起不来，所以列在常用工具前面
    let mut webview2 = EnvItem::new("webview2", "WebView2 运行时", InstallKind::Winget);
    probe_webview2(&mut webview2);
    items.push(webview2);

    let mut vcredist = EnvItem::new("vcredist", "VC++ 运行库", InstallKind::Winget);
    probe_vcredist(&mut vcredist);
    items.push(vcredist);

    let mut adb = EnvItem::new("adb", "ADB", InstallKind::Winget);
    probe_adb(&mut adb);
    items.push(adb);

    items
}

/// winget 在不在。在的话一键部署才有意义。
pub fn winget_available() -> bool {
    #[cfg(windows)]
    {
        find_exe(&["winget.exe"]).is_some()
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// 每个条目对应的 winget 包 id。
fn winget_id(id: &str) -> Option<&'static str> {
    match id {
        "python" => Some("Python.Python.3.13"),
        "node" => Some("OpenJS.NodeJS.LTS"),
        "git" => Some("Git.Git"),
        "jdk" => Some("Microsoft.OpenJDK.21"),
        "webview2" => Some("Microsoft.EdgeWebView2Runtime"),
        "vcredist" => Some("Microsoft.VCRedist.2015.x64"),
        "adb" => Some("Google.PlatformTools"),
        _ => None,
    }
}

/// 跑 winget 安装。返回一段给用户看的话。
///
/// 刻意同步执行而不是后台异步：安装过程要花几十秒到几分钟，
/// 界面上一个转圈的按钮比一个不知道在干嘛的 toast 诚实得多。
pub fn install(id: &str) -> Result<String, String> {
    let Some(package) = winget_id(id) else {
        return Err(format!("未知的环境条目：{id}"));
    };
    if !winget_available() {
        return Err("本机没有 winget，无法自动安装。\n请到「设置 → 应用 → 应用执行别名」里启用 winget，或手动到官网下载安装。".to_string());
    }

    let args = [
        "install",
        "--id",
        package,
        "--exact",
        "--source",
        "winget",
        "--accept-package-agreements",
        "--accept-source-agreements",
        "--disable-interactivity",
    ];
    let output = Command::new("winget")
        .args(args)
        .output()
        .map_err(|error| format!("调用 winget 失败：{error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}");

    if output.status.success() {
        // winget 命中已安装时返回 0 但会说「No applicable upgrade found」，文案上区分一下
        let already = combined.contains("already installed")
            || combined.contains("已安装")
            || combined.contains("No applicable upgrade");
        Ok(if already {
            format!("{package} 已经是最新状态，无需重复安装。\n{stdout}")
        } else {
            format!("{package} 安装完成。\n{stdout}")
        })
    } else {
        // 失败时把 winget 自己的话原样带回来，它比我们的转述有用
        Err(format!(
            "{package} 安装失败（winget 退出码 {:?}）。\n{combined}",
            output.status.code()
        ))
    }
}

/// 打开某个工具的下载页。winget 不覆盖的版本走这里。
pub fn open_download(id: &str) -> Result<(), String> {
    let url = match id {
        "python" => "https://www.python.org/downloads/",
        "node" => "https://nodejs.org/en/download",
        "git" => "https://git-scm.com/download/win",
        "jdk" => "https://learn.microsoft.com/java/jdk/download",
        "webview2" => "https://developer.microsoft.com/microsoft-edge/webview2/",
        "vcredist" => "https://aka.ms/vs/17/release/vc_redist.x64.exe",
        "adb" => "https://developer.android.com/tools/releases/platform-tools",
        _ => return Err(format!("未知的环境条目：{id}")),
    };
    open_url(url)
}

/// 用系统默认浏览器打开一个链接。
fn open_url(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    let result = Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let result = Command::new("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = Command::new("xdg-open").arg(url).spawn();
    result
        .map(|_| ())
        .map_err(|err| format!("打开链接失败：{err}"))
}

/// 这个条目是不是「一键部署」能搞定的。
pub fn can_auto_install(id: &str) -> bool {
    winget_id(id).is_some() && winget_available()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 版本号提取覆盖各家格式() {
        assert_eq!(extract_version("Python 3.13.14"), "3.13.14");
        assert_eq!(extract_version("v22.19.0"), "22.19.0");
        // Git 的 .windows.1 是要给人看的发行标记，不能截断
        assert_eq!(extract_version("git version 2.50.1.windows.1"), "2.50.1.windows.1");
        assert_eq!(extract_version("openjdk version \"21.0.11\""), "21.0.11");
        // 结尾的孤立点不能算进版本
        assert_eq!(extract_version("node v20.1."), "20.1");
        // 只有一个数字段的不算版本（架构号 64 之类）
        assert_eq!(extract_version("built for 64"), "");
        assert_eq!(extract_version(""), "");
        // ADB 首行是桥接版本，第二行才是 Platform Tools 版本
        assert_eq!(extract_version("Android Debug Bridge version 1.0.41"), "1.0.41");
        assert_eq!(extract_version("Version 37.0.1-15733141"), "37.0.1");
        // 别把后面无关的词粘进版本
        assert_eq!(extract_version("node v22.19.0 (installed)"), "22.19.0");
    }

    #[test]
    fn 注册表取值按值名精确匹配() {
        // 真实 reg query 输出：路径那一行含点也有数字，不能被当成值抓回来
        let out = "HKEY_LOCAL_MACHINE\\SOFTWARE\\Microsoft\\VisualStudio\\14.0\\VC\\Runtimes\\x64\n    Version    REG_SZ    v14.44.35211.00\n";
        assert_eq!(reg_value(out, "Version"), Some("v14.44.35211.00".to_string()));
        // 取不存在的值名要返回 None，而不是退而求其次抓别的列
        assert_eq!(reg_value(out, "Installed"), None);
        // 大小写不敏感（reg 的输出列对齐在不同版本上不稳定）
        assert_eq!(reg_value(out, "version"), Some("v14.44.35211.00".to_string()));
        // Installed 是十六进制 0x1
        let inst = "HKEY_LOCAL_MACHINE\\SOFTWARE\\...\\x64\n    Installed    REG_DWORD    0x1\n";
        assert_eq!(reg_value(inst, "Installed"), Some("0x1".to_string()));
        assert_eq!(reg_value("", "Version"), None);
    }

    #[test]
    fn 版本比较逐段比数值() {
        assert!(compare_version("155.0.4283.62", "155.0.4283.45"));
        assert!(!compare_version("155.0.4283.45", "155.0.4283.62"));
        // 段数不同按 0 补齐，判等
        assert!(!compare_version("1.2", "1.2.0"));
        assert!(compare_version("1.3", "1.2.9"));
        assert!(!compare_version("1.2.1", "1.2.1"));
    }

    #[test]
    fn 每个条目都有对应的安装来源() {
        for id in ["python", "node", "git", "jdk", "webview2", "vcredist", "adb"] {
            assert!(winget_id(id).is_some(), "{id} 缺 winget 包 id");
        }
        assert!(winget_id("不存在的东西").is_none());
    }

    #[test]
    fn 体检至少覆盖截图里那七项() {
        let items = scan_items();
        let ids: Vec<&str> = items.iter().map(|item| item.id).collect();
        for expected in ["python", "jdk", "node", "git", "webview2", "vcredist", "adb"] {
            assert!(ids.contains(&expected), "体检漏了 {expected}");
        }
    }
}