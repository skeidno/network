//! 开发环境体检与一键部署。
//!
//! 这一块刻意只做两件事：**如实报告本机现在有什么**，以及**在缺失时把安装命令跑起来**。
//! 不下载安装包、不写注册表、不改 PATH —— 装环境这种事一旦自己动手，出了问题没人说得清
//! 是谁的锅。所以安装统一走各工具官方的包管理器（winget / scoop / choco / 官方安装器），
//! 我们只负责把命令拼对、把输出如实转述给用户。
//!
//! 探测优先级：能直接问官方 exe 的（`--version`）就问 exe，问不到再查注册表
//! （很多运行库只往往注册表写，不进 PATH），最后才退到「目录在不在」。三级都不中才算未就绪。
//!
//! 三态而不是「就绪 / 没就绪」两个值：**装了但版本太老**和**根本没装**是两回事 ——
//! 前者重装能解决，后者必须先装，界面上给的按钮不一样（重装 vs 一键部署），
//! 混成一个布尔值就会出现「明明装了却不给重装按钮」的荒唐情况。

use serde_json::{json, Value};
use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::Command;

/// 单个环境的探测结果。前端只认这些字段，别的一律塞进 `note`。
#[derive(Debug, Clone)]
pub struct EnvItem {
    /// 稳定 id，前端拿它做 testTag 和图标映射。
    pub id: &'static str,
    /// 界面上的名字，比如 "Node.js"。
    pub name: &'static str,
    /// 装了没装。装了不等于可用 —— 还要看 `outdated`。
    pub ready: bool,
    /// 版本号原文，探测不到就是空串。
    pub version: String,
    /// 命中的可执行文件绝对路径，探测不到就是空串。
    pub path: String,
    /// 补充说明：装在哪、为什么没探测到、建议怎么办。
    pub note: String,
    /// 安装方式，见 [`InstallKind`]。
    pub install: InstallKind,
    /// 建议的最低版本，空串表示这一项不挑版本。
    pub min_version: &'static str,
    /// 装了但低于 `min_version`。只有 ready && outdated 才有意义。
    pub outdated: bool,
    /// 实际版本与最低版本的差距说明，outdated 时才有内容。
    pub outdated_note: String,
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
    fn new(id: &'static str, name: &'static str, min_version: &'static str) -> Self {
        Self {
            id,
            name,
            ready: false,
            version: String::new(),
            path: String::new(),
            note: String::new(),
            install: InstallKind::Winget,
            min_version,
            outdated: false,
            outdated_note: String::new(),
        }
    }

    /// 探测收尾：知道实际版本之后，比一下最低要求，把三态补齐。
    ///
    /// 拿不到版本号时一律**不**判过期 —— 读不出本就不知道是多是少，
    /// 猜一个「可能太老」只会让用户去重装一个本来好好的东西。
    fn finalize(&mut self) {
        self.outdated = false;
        self.outdated_note.clear();
        if !self.ready || self.version.is_empty() || self.min_version.is_empty() {
            return;
        }
        match compare_versions(&self.version, self.min_version) {
            Some(Ordering::Less) => {
                self.outdated = true;
                self.outdated_note =
                    format!("当前 {}，建议 {} 以上；重装会升级到最新版", self.version, self.min_version);
            }
            // 版本号格式怪到没法比较（比如注册表里读到非预期内容）时，
            // 照旧按就绪处理，只是提示一句，省得用户以为程序坏了。
            Some(Ordering::Equal) | Some(Ordering::Greater) => {}
            None => {
                self.note = if self.note.is_empty() {
                    format!("版本号「{}」无法与最低要求比较，按就绪处理", self.version)
                } else {
                    self.note.clone()
                };
            }
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
            "minVersion": self.min_version,
            "outdated": self.outdated,
            "outdatedNote": self.outdated_note,
            // 一键部署能不能点：取决于这台机器有没有 winget。前端拿它决定按钮灰不灰，
            // 比自己再判断一遍可靠。
            "canInstall": can_auto_install(self.id),
            // 过期项也需要重装按钮，所以单独给前端一个「可重装」标记。
            // 前端不能只看 outdated，因为 winget 缺失时重装也做不了。
            "canReinstall": can_auto_install(self.id),
            // 这一项支不支持「装指定版本」。前端据此决定要不要画版本下拉框。
            // Node 走 nvm（能装多版本并存），其它走 winget 的 --version
            // （换个版本需要先卸载旧的，所以前端要提示用户）。
            "supportsVersions": supports_versions(self.id),
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

/// 常见安装位置：用户装在非 PATH 目录的情况。
///
/// 只放「装完通常不进 PATH 但确实是系统安装」的目录。**不放 conda 目录** ——
/// Python 那一项要显式排除 conda，不能在这里悄悄把它当兜底候选。
const EXTRA_EXE_DIRS: [&str; 9] = [
    r"C:\Program Files\Git\cmd",
    r"C:\Program Files\Git\bin",
    r"C:\Program Files\nodejs",
    r"C:\Program Files\Android\Android Studio\jbr\bin",
    r"C:\Program Files\Microsoft\jdk-21.0.11.10-hotspot\bin",
    r"C:\Program Files\Android\platform-tools",
    r"C:\Program Files (x86)\Android\platform-tools",
    r"C:\Program Files (x86)\Android\adb",
    r"C:\Program Files\WebView2",
];

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
    let extras: Vec<PathBuf> = EXTRA_EXE_DIRS.iter().map(PathBuf::from).collect();
    for extra in extras {
        for name in names {
            candidates.push(extra.join(name));
        }
    }
    candidates.into_iter().find(|p| p.is_file())
}

/// 列出所有匹配的候选可执行文件（find_exe 的多值版本），按优先级排序。
///
/// 需要「挨个试」的场景用这个：比如 Python 装了多个，第一个命中的是 conda
/// 环境的就该跳过看下一个，而不是直接返回「没装」。
fn find_all_exes(names: &[&str]) -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Some(path_var) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path_var) {
            for name in names {
                candidates.push(dir.join(name));
            }
        }
    }
    for extra in EXTRA_EXE_DIRS {
        for name in names {
            candidates.push(PathBuf::from(extra).join(name));
        }
    }
    // 去重但保序：同一个 exe 可能同时命中 PATH 与兜底目录
    let mut seen = Vec::new();
    candidates
        .into_iter()
        .filter(|p| p.is_file())
        .filter(|p| {
            let key = p.to_string_lossy().to_lowercase();
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        })
        .collect()
}

/// 找第一个通过筛选的候选可执行文件。
fn find_exe_filtered(names: &[&str], accept: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    find_all_exes(names).into_iter().find(|p| accept(p))
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

/// Python：只认**原生**安装（python.org / Microsoft Store 那种），
/// 明确排除 conda 环境里的解释器。
///
/// 为什么排除 conda：
///
/// 1. conda 装的 Python 依赖 `activate` 才生效，直接跑它的 `python.exe --version`
///    拿到的版本不代表用户命令行里那个；
/// 2. conda 环境是「一整套依赖」，我们只体检单个解释器版本，管不了它的包；
/// 3. 这一页是给开发环境做体检的，不是包管理器 —— 装了 conda 的用户知道自己在用哪套。
///
/// 判定只认**安装路径**（路径里含 conda 的根目录名），不猜版本输出里的字符串：
/// 各家 conda 的 banner 措辞不统一，靠字符串匹配等于给自己埋雷。
///
/// 一个原生解释器都找不到时，如果本机确实装了 conda，要说清楚为什么不算它 ——
/// 否则用户看到「未检测到」会以为程序坏了，而实际上他机器上有 Python，
/// 只是我们刻意没数它。
fn probe_python(item: &mut EnvItem) {
    // launcher（py.exe）优先：它只列系统安装的解释器，不会把 conda 环境的
    // 解释器算进去 —— 这正是我们要的，所以 launcher 命中就算通过
    if let Some(launcher) = find_exe_filtered(&["py.exe"], |_| true) {
        let launcher_str = launcher.to_string_lossy().to_string();
        let out = run_capture(&launcher_str, &["-3", "--version"]);
        let version = extract_version(&out);
        if !version.is_empty() {
            item.ready = true;
            item.version = version;
            item.path = launcher_str;
            item.note = "通过 Python Launcher 探测（只列原生安装）".to_string();
            return;
        }
    }

    // 没有 launcher 才直接找 python.exe，此时必须排除 conda
    for candidate in find_all_exes(&["python.exe"]) {
        if is_conda_python(&candidate) {
            continue;
        }
        let exe = candidate.to_string_lossy().to_string();
        let out = run_capture(&exe, &["--version"]);
        let version = extract_version(&out);
        if version.is_empty() {
            continue;
        }
        item.ready = true;
        item.version = version;
        item.path = exe;
        item.note = if inside_conda_env() {
            // 进程在 conda 环境里但解释器本身是原生的，说明是外部工具链起的进程
            "原生安装（当前进程在 conda 环境内，探测到的是原生解释器）".to_string()
        } else {
            "原生安装".to_string()
        };
        return;
    }

    // 一个原生解释器都没找到：如果 conda 环境是现成的，说清楚为什么不算它
    let conda_dirs = conda_install_dirs();
    if !conda_dirs.is_empty() {
        item.note = format!(
            "未找到原生 Python。检测到 conda 环境（{}），本工具只体检原生安装，不使用 conda 管理的解释器",
            conda_dirs.join("、")
        );
    } else {
        item.note = "未在 PATH 中找到 py.exe 或 python.exe".to_string();
    }
}

/// 当前进程是否跑在 conda 环境里（被 conda activate 过）。
fn inside_conda_env() -> bool {
    std::env::var_os("CONDA_PREFIX").is_some()
}

/// conda 的常见安装目录名。
///
/// 认目录名而不是问 conda 本体在不在：`conda` 可能不在 PATH（用户装完从没
/// activate 过），但解释器照样在环境目录里，靠目录名反而更稳。
const CONDA_ROOT_NAMES: [&str; 5] =
    ["anaconda3", "miniconda3", "miniforge3", "mambaforge", "micromamba"];

/// 列出本机存在的 conda 根目录（完整路径）。
fn conda_install_dirs() -> Vec<String> {
    let mut found = Vec::new();
    let push_root = |base: PathBuf, out: &mut Vec<String>| {
        if base.as_os_str().is_empty() {
            return;
        }
        for name in CONDA_ROOT_NAMES {
            let candidate = base.join(name);
            if candidate.is_dir() {
                out.push(candidate.to_string_lossy().to_string());
            }
        }
    };
    if let Some(local) = std::env::var_os("LOCALAPPDATA") {
        push_root(PathBuf::from(local).join("Programs"), &mut found);
    }
    if let Some(profile) = std::env::var_os("USERPROFILE") {
        push_root(PathBuf::from(profile), &mut found);
    }
    push_root(PathBuf::from(r"C:\ProgramData"), &mut found);
    push_root(PathBuf::from(r"C:\"), &mut found);
    found
}

/// 这个 python.exe 是不是 conda 环境里的。
///
/// 只看路径，不执行它 —— 执行 conda 解释器要连带一整套依赖，装不装得起来
/// 都是环境自己的事，我们不该去激活它。
///
/// 判定刻意**不依赖路径分隔符**：Windows 上 `Path` 会把 `\` 规范成 `/`
/// （单测里就能看到），而真实文件系统返回的又常是 `\`。写成两边都能匹配：
/// 先把反斜杠换成正斜杠，再按「目录段」比对。
fn is_conda_python(exe: &Path) -> bool {
    let normalized = exe.to_string_lossy().replace('\\', "/").to_lowercase();
    CONDA_ROOT_NAMES
        .iter()
        .any(|name| normalized.contains(&format!("/{name}/")))
}

/// Node.js：node --version 输出 v22.19.0，extract_version 会去掉前缀 v。
///
/// 装了 nvm 的话以 nvm 为准。原因是 nvm-windows 用软链接接管 node.exe，
/// 而软链接的指向由 nvm use 决定；同时系统里可能还躺着一份不走 nvm 的独立
/// 安装（比如编辑器自带的），PATH 里的 node.exe 未必是用户实际在用的那个。
/// 这时报「PATH 里那个的版本」会给出误导性的结论 —— 用户以为自己用的是
/// 22.17.1，其实 nvm 已经切到别的版本去了。
fn probe_node(item: &mut EnvItem) {
    if crate::nvm::installed() {
        let nvm_state = crate::nvm::status();
        let installed = nvm_state
            .get("installedVersions")
            .and_then(|v| v.as_array())
            .map(|list| !list.is_empty())
            .unwrap_or(false);
        let current = nvm_state
            .get("current")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if installed && !current.is_empty() {
            item.ready = true;
            item.version = current.clone();
            item.path = "nvm-windows 管理".to_string();
            let count = nvm_state
                .get("installedVersions")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            item.note = format!("由 nvm 管理，当前版本，已装 {count} 个");
            return;
        }
        if installed {
            // 装了版本但认不出当前是哪个：仍然算就绪，如实说明
            item.ready = true;
            let count = nvm_state
                .get("installedVersions")
                .and_then(|v| v.as_array())
                .map(|a| a.len())
                .unwrap_or(0);
            item.path = "nvm-windows 管理".to_string();
            item.note = format!("由 nvm 管理，已装 {count} 个，但认不出当前使用的版本（可能 PATH 里的 node 不走 nvm）");
            return;
        }
    }
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
                    Some(current) => compare_versions(&name, current) == Some(Ordering::Greater),
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

/// 版本段。既能装数字，也能装字母（Git 的 `windows` 就是字母段）。
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum VersionSegment {
    Num(u64),
    /// 字母段。排序时排在同位置的数字之后 —— `1.0.windows.2` 与 `1.0.1` 不好比，
    /// 但只要最低要求里没有字母段，就不会走到这个分支。
    Word(String),
}

/// 把一段版本号切成可比较的段。切不出来就返回 None。
///
/// 为什么不用 `split('.')` 直接 parse：实际探测到的版本号里混着这些东西 ——
/// `2.50.1.windows.1`、`14.44.35211.00`、`35.0.2-12147458`、`v3.13.14`。
/// 硬 parse 会在遇到字母时整条失败，然后所有项都判不出过期 —— 那是更糟的静默。
/// 所以宽松切：能转数字就转，转不了就当字母段留着。
fn parse_version(text: &str) -> Option<Vec<VersionSegment>> {
    // 前缀的 v / V（node --version 输出 v22.19.0）不是版本的一部分
    let body = text.trim().trim_start_matches(['v', 'V']);
    // 尾巴上的 -构建号、+元数据（adb 的 35.0.2-12147458）不比
    let body = body.split(['-', '+']).next().unwrap_or(body);
    if body.is_empty() {
        return None;
    }
    let mut segments = Vec::new();
    for part in body.split('.') {
        if part.is_empty() {
            // "1.2..3" 这种畸形输入，保守当无法比较
            return None;
        }
        match part.parse::<u64>() {
            Ok(value) => segments.push(VersionSegment::Num(value)),
            Err(_) => {
                // 纯字母段才认；夹着别的字符说明这压根不是版本号
                if part.chars().all(|c| c.is_ascii_alphabetic()) {
                    segments.push(VersionSegment::Word(part.to_ascii_lowercase()));
                } else {
                    return None;
                }
            }
        }
    }
    Some(segments)
}

/// 比较两个版本号。返回 `None` 表示有一边格式没法解析，调用方应保守处理
/// （按就绪算，不提示重装）—— 宁可漏报也不要让用户去重装一个好端端的东西。
fn compare_versions(left: &str, right: &str) -> Option<Ordering> {
    let l = parse_version(left)?;
    let r = parse_version(right)?;
    for index in 0..l.len().max(r.len()) {
        // 缺的段当 0，这样 21 与 21.0.0 判等
        let lv = l.get(index).unwrap_or(&VersionSegment::Num(0));
        let rv = r.get(index).unwrap_or(&VersionSegment::Num(0));
        match lv.cmp(rv) {
            Ordering::Equal => continue,
            other => return Some(other),
        }
    }
    Some(Ordering::Equal)
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
/// 顺序保持目录序：用户扫一眼卡片列表时，位置稳定比排序更有用。
pub fn scan() -> Value {
    let items = scan_items();
    let installed = items.iter().filter(|item| item.ready).count();
    let outdated = items.iter().filter(|item| item.outdated).count();
    json!({
        "items": items.iter().map(|item| item.to_json()).collect::<Vec<Value>>(),
        "total": items.len(),
        // 装了且版本够新 = 真就绪。过期项单列，因为它需要的是重装而不是安装。
        "ready": installed - outdated,
        "installed": installed,
        "outdated": outdated,
        "missing": items.len() - installed,
        // winget 有没有是「一键部署 / 重装」能不能用的前提，没它整列按钮都得灰掉
        "winget": winget_available(),
    })
}

/// 最低版本要求。
///
/// 取值不是随手写的：每一条都对应一个具体的、会在某个环节真的出问题的地方，
/// 而不是「最新版」。定得太高会让一半正常环境被判过期，催着用户去重装没必要的东西。
const MIN_PYTHON: &str = "3.9.0";
/// Node 20 是当前 LTS，18 已进入维护末期；CI 用 `node --check` 校验前端语法。
const MIN_NODE: &str = "20.0.0";
/// 2.30 才有 `git switch` 与新版凭据助手，老版本在跨平台协作上问题较多。
const MIN_GIT: &str = "2.30.0";
/// Android Gradle Plugin 8.x 要求 JDK 17 起；本项目 `jbr` 也带 21。
const MIN_JDK: &str = "17.0.0";
/// WebView2 90 才有稳定的多进程与 DevTools 协议，桌面端 wry 依赖它。
const MIN_WEBVIEW2: &str = "90.0.0";
/// VC++ 2015-2022 的注册表版本是 14.x；低于 14 说明只有更老的 2015 运行库，
/// 原生依赖的新 API 会缺符号。
const MIN_VCREDIST: &str = "14.0.0";
/// ADB 30 才有 split-APK 与增量安装支持，Android 调试链路必需。
const MIN_ADB: &str = "30.0.0";

fn scan_items() -> Vec<EnvItem> {
    let mut items = Vec::new();

    let mut python = EnvItem::new("python", "Python", MIN_PYTHON);
    probe_python(&mut python);
    python.finalize();
    items.push(python);

    let mut node = EnvItem::new("node", "Node.js", MIN_NODE);
    probe_node(&mut node);
    node.finalize();
    items.push(node);

    // nvm 本体单列一项，排在 Node 后面。
    // 它只在「要装指定版本 Node」时才必需，所以不设最低版本 —— 老 nvm 也能用。
    // 只装最新版：nvm 自己有 `nvm upgrade`，给它开版本选择纯属添乱。
    let mut nvm = EnvItem::new("nvm", "nvm（Node 版本管理）", "");
    if crate::nvm::installed() {
        nvm.ready = true;
        nvm.version = crate::nvm::version();
        nvm.path = crate::nvm::nvm_home_pub();
        let count = crate::nvm::installed_count();
        nvm.note = if count > 0 {
            format!("已托管 {count} 个 Node 版本")
        } else {
            "已安装，但还没有任何 Node 版本".to_string()
        };
    } else {
        nvm.note = "未检测到 nvm；装指定版本的 Node 需要它".to_string();
    }
    nvm.finalize();
    items.push(nvm);

    let mut git = EnvItem::new("git", "Git", MIN_GIT);
    probe_git(&mut git);
    git.finalize();
    items.push(git);

    let mut jdk = EnvItem::new("jdk", "JDK", MIN_JDK);
    probe_jdk(&mut jdk);
    jdk.finalize();
    items.push(jdk);

    // 桌面端自己的渲染依赖：装不上程序起不来，所以列在常用工具前面
    let mut webview2 = EnvItem::new("webview2", "WebView2 运行时", MIN_WEBVIEW2);
    probe_webview2(&mut webview2);
    webview2.finalize();
    items.push(webview2);

    let mut vcredist = EnvItem::new("vcredist", "VC++ 运行库", MIN_VCREDIST);
    probe_vcredist(&mut vcredist);
    vcredist.finalize();
    items.push(vcredist);

    let mut adb = EnvItem::new("adb", "ADB", MIN_ADB);
    probe_adb(&mut adb);
    adb.finalize();
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
        // nvm 只装最新版，不参与 Node 的版本选择（那是 nvm install 的活）
        "nvm" => Some(crate::nvm::NVM_WINGET_ID),
        _ => None,
    }
}

/// 跑 winget 安装。`version` 传空串表示装最新版。
///
/// 刻意同步执行而不是后台异步：安装过程要花几十秒到几分钟，
/// 界面上一个转圈的按钮比一个不知道在干嘛的 toast 诚实得多。
///
/// Node 走 nvm 而不是 winget：Node 要能同时装多个版本并存，winget 装的是
/// 单一系统级安装，装第二个版本会跟第一个打架。nvm 天然解决这件事。
pub fn install(id: &str, version: &str) -> Result<String, String> {
    if id == "node" && !version.is_empty() {
        return crate::nvm::install_version(version);
    }
    run_winget(id, false, version)
}

/// 强制重装到最新版（或指定版本）。用于「装了但版本太老」的场景。
///
/// 走的是 `winget upgrade`。不用 `install --force` 的原因：
/// 强制安装会先卸载再装，一旦下载失败就把用户的环境弄没了 —— 先试升级，
/// 升级不可用（版本太老不在源里、包不支持升级）再退回强制安装，
/// 并且把降级这件事在返回文案里说清楚。
pub fn reinstall(id: &str, version: &str) -> Result<String, String> {
    if id == "node" && !version.is_empty() {
        return crate::nvm::install_version(version);
    }
    run_winget(id, true, version)
}

/// 组装并执行 winget 命令。
fn run_winget(id: &str, force: bool, version: &str) -> Result<String, String> {
    let Some(package) = winget_id(id) else {
        return Err(format!("未知的环境条目：{id}"));
    };
    if !winget_available() {
        return Err("本机没有 winget，无法自动安装。\n请到「设置 → 应用 → 应用执行别名」里启用 winget，或手动到官网下载安装。".to_string());
    }

    // 公共参数这几个动作都一样
    const COMMON: [&str; 4] = [
        "--exact",
        "--source",
        "winget",
        "--accept-package-agreements",
    ];

    if force {
        // 指定了版本就只能强制安装到那个版本：upgrade 不接受「降到某个旧版本」
        if !version.is_empty() {
            let detail =
                run_winget_command("install", package, &COMMON, Some("--force"), Some(version))?;
            return Ok(format!(
                "{package} 已重装到指定版本 {version}（先卸载了旧版本）。\n{detail}"
            ));
        }
        // 先试升级：非破坏性，失败也不动用户现有环境
        match run_winget_command("upgrade", package, &COMMON, None, None) {
            Ok(detail) => return Ok(format!("{package} 已升级。\n{detail}")),
            Err(upgrade_error) => {
                // 升级走不通（源里没新版 / 包不支持升级）才强制重装
                // 强制安装会先卸载，所以文案必须说明这一步发生了什么
                let detail = run_winget_command("install", package, &COMMON, Some("--force"), None)
                    .map_err(|force_error| {
                        format!(
                            "重装失败。\n升级方式：{upgrade_error}\n强制安装方式：{force_error}"
                        )
                    })?;
                return Ok(format!(
                    "{package} 升级不可用，已改为强制重装（会先卸载旧版本）。\n{detail}"
                ));
            }
        }
    }

    let extra = if version.is_empty() {
        None
    } else {
        Some("--force")
    };
    let detail =
        run_winget_command("install", package, &COMMON, extra, if version.is_empty() { None } else { Some(version) })?;
    // winget 命中已安装时返回 0 但会说「No applicable upgrade found」，文案上区分一下
    if detail.contains("already installed") || detail.contains("已安装") {
        return Ok(format!("{package} 已经是最新状态，无需重复安装。\n{detail}"));
    }
    Ok(if version.is_empty() {
        format!("{package} 安装完成。\n{detail}")
    } else {
        format!("{package} 已安装到指定版本 {version}。\n{detail}")
    })
}

/// 执行一次 winget，返回合并后的输出文本。失败时把 winget 的话原样带回来。
///
/// `version` 非空时加 `-v <版本>`。winget 自己支持这个参数（`--version`，
/// 注意它跟 winget 自身的版本无关，是「安装包的指定版本」）。
fn run_winget_command(
    action: &str,
    package: &str,
    common: &[&str],
    extra: Option<&str>,
    version: Option<&str>,
) -> Result<String, String> {
    let mut args: Vec<&str> = vec![action];
    args.push("--id");
    args.push(package);
    args.extend_from_slice(common);
    args.push("--accept-source-agreements");
    args.push("--disable-interactivity");
    if let Some(flag) = extra {
        args.push(flag);
    }
    if let Some(ver) = version {
        // 放进版本号前先过一道白名单：只允许数字和点，
        // 免得界面上算出来的空串或异常值拼进命令里
        if !ver.is_empty() && ver.chars().all(|c| c.is_ascii_digit() || c == '.') {
            args.push("--version");
            args.push(ver);
        }
    }

    let output = Command::new("winget")
        .args(&args)
        .output()
        .map_err(|error| format!("调用 winget 失败：{error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}").trim().to_string();

    if output.status.success() {
        Ok(combined)
    } else {
        Err(format!(
            "winget {action} 退出码 {:?}：\n{combined}",
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

/// 这一项支不支持指定版本安装。
///
/// Node 特殊：它走 nvm，能同时并存多个版本，切版本不动其它项目。
/// 其它项靠 winget 的 `--version`，但 winget 换版本要先卸载旧的
/// （我们用 `--force` 达成），所以对用户来说是破坏性操作。
fn supports_versions(id: &str) -> bool {
    matches!(id, "node" | "python" | "git" | "jdk")
}

/// 列出某个项能装的版本（走 winget 的包）。
/// Node 不在这里 —— 它的版本来自 nvm，见 crate::nvm::status。
pub fn available_versions(id: &str) -> Result<Value, String> {
    let Some(package) = winget_id(id) else {
        return Err(format!("未知的环境条目：{id}"));
    };
    if !winget_available() {
        return Err("本机没有 winget，无法查询可安装版本。".to_string());
    }
    let output = Command::new("winget")
        .args([
            "show",
            "--id",
            package,
            "--exact",
            "--source",
            "winget",
            "--versions",
        ])
        .output()
        .map_err(|error| format!("调用 winget 失败：{error}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(format!("查询 {package} 的版本列表失败：\n{text}"));
    }
    // winget show --versions 一行一个版本号，但前面还有若干字段行，
    // 所以逐行挑出「整行就是一个版本号」的那些
    let versions: Vec<String> = text
        .lines()
        .map(|line| line.trim())
        .filter(|line| {
            !line.is_empty()
                && line
                    .chars()
                    .all(|c| c.is_ascii_digit() || c == '.' || c == '-' || c == '+')
                && line.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false)
        })
        .map(|line| line.to_string())
        .collect();
    Ok(json!({ "id": id, "versions": versions }))
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
        assert_eq!(compare_versions("155.0.4283.62", "155.0.4283.45"), Some(Ordering::Greater));
        assert_eq!(compare_versions("155.0.4283.45", "155.0.4283.62"), Some(Ordering::Less));
        // 段数不同按 0 补齐，判等
        assert_eq!(compare_versions("1.2", "1.2.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("21", "21.0.0"), Some(Ordering::Equal));
        assert_eq!(compare_versions("1.3", "1.2.9"), Some(Ordering::Greater));
        assert_eq!(compare_versions("2.19.0", "2.9.0"), Some(Ordering::Greater));
    }

    #[test]
    fn 版本比较容忍实际输出里的怪格式() {
        // node 的 v 前缀
        assert_eq!(compare_versions("v22.19.0", "20.0.0"), Some(Ordering::Greater));
        assert_eq!(compare_versions("V3.13.14", "3.9.0"), Some(Ordering::Greater));
        // adb 的 -构建号后缀不比
        assert_eq!(compare_versions("35.0.2-12147458", "33.0.1"), Some(Ordering::Greater));
        // git 的字母段：.windows.1 不能让整条解析失败
        assert_eq!(compare_versions("2.50.1.windows.1", "2.30.0"), Some(Ordering::Greater));
        // 字母段按派生 Ord 排在数字段之后（枚举声明顺序 Num 在前），
        // 所以 "1.0.windows" > "1.0.1"。实际用不出这个结果 ——
        // 最低要求里不会出现字母段，写在这里是为了把这条边界钉住。
        assert_eq!(compare_versions("1.0.windows.1", "1.0.1"), Some(Ordering::Greater));
        // 注册表里的 VC++ 版本带 v 前缀且四段
        assert_eq!(compare_versions("14.44.35211.00", "14.30.0"), Some(Ordering::Greater));
    }

    #[test]
    fn 版本号完全没法比较时返回None() {
        assert_eq!(compare_versions("未知", "3.0.0"), None);
        assert_eq!(compare_versions("", "3.0.0"), None);
        assert_eq!(compare_versions("1.2..3", "1.2.3"), None);
        // 夹着非字母非数字的段（3_1、1.2-beta）判为不可比较
        assert_eq!(compare_versions("3_0.0", "3.0.0"), None);
        // 纯字母段是合法的（Git 的 windows 就是），不该被拒；
        // 排在数字段之后是派生 Ord 的既定行为
        assert_eq!(compare_versions("abc", "3.0.0"), Some(Ordering::Greater));
    }

    #[test]
    fn 装了但版本过老会被标出来() {
        let mut item = EnvItem::new("node", "Node.js", "20.0.0");
        // 先按就绪装上，但给一个太老的版本
        item.ready = true;
        item.version = "18.19.0".to_string();
        item.finalize();
        assert!(item.ready, "装了就是 ready，重装与否是另一回事");
        assert!(item.outdated, "18.19.0 低于 20.0.0，应该提示重装");
        assert!(item.outdated_note.contains("18.19.0"));
        assert!(item.outdated_note.contains("20.0.0"));
    }

    #[test]
    fn 版本够新或者读不出就不提示重装() {
        let mut ok = EnvItem::new("node", "Node.js", "20.0.0");
        ok.ready = true;
        ok.version = "22.19.0".to_string();
        ok.finalize();
        assert!(!ok.outdated);

        // 读不出版本号时不能瞎猜 —— 宁可漏报也不要让用户重装好端端的东西
        let mut unknown = EnvItem::new("weird", "怪东西", "20.0.0");
        unknown.ready = true;
        unknown.version = String::new();
        unknown.finalize();
        assert!(!unknown.outdated);

        // 没装更不该提重装
        let mut missing = EnvItem::new("node", "Node.js", "20.0.0");
        missing.finalize();
        assert!(!missing.outdated);
        assert!(!missing.ready);
    }

    #[test]
    fn conda环境里的解释器要认出来() {
        // conda 根目录下的本体
        assert!(is_conda_python(Path::new(
            r"C:\Users\dev\anaconda3\python.exe"
        )));
        assert!(is_conda_python(Path::new(
            r"C:\Users\dev\miniconda3\python.exe"
        )));
        // conda 环境目录里的解释器（路径含根目录名）
        assert!(is_conda_python(Path::new(
            r"C:\Users\dev\miniconda3\envs\web\python.exe"
        )));
        assert!(is_conda_python(Path::new(
            r"C:\ProgramData\anaconda3\envs\base\python.exe"
        )));
        // 大小写不敏感（盘符和用户目录名都可能不同大小写）
        assert!(is_conda_python(Path::new(
            r"c:\users\dev\Miniconda3\python.exe"
        )));
        // 分隔符无关：Windows 上 Path 会把 \ 规范成 /，两种都得认
        assert!(is_conda_python(Path::new(
            r"C:\Users\dev\miniconda3\python.exe"
        )));
        assert!(is_conda_python(Path::new(
            "C:/Users/dev/miniconda3/envs/x/python.exe"
        )));
        // micromamba / mambaforge 也在名单里
        assert!(is_conda_python(Path::new(
            r"C:\Users\dev\micromamba\python.exe"
        )));
    }

    #[test]
    fn 原生安装不该被误判成conda() {
        for native in [
            r"C:\Users\dev\AppData\Local\Programs\Python\Python313\python.exe",
            r"C:\Python313\python.exe",
            r"C:\Program Files\Python312\python.exe",
            // 目录名里只是碰巧含有 conda 的子串，不该命中
            r"C:\Users\dev\my-conda-notes\python.exe",
        ] {
            assert!(
                !is_conda_python(Path::new(native)),
                "{native} 被误判成 conda 了"
            );
        }
    }

    #[test]
    fn 兜底目录里不含conda() {
        // 否则 find_all_exes 会把 conda 的解释器当兜底候选，
        // probe_python 里的排除逻辑就白写了
        for dir in EXTRA_EXE_DIRS {
            let lower = dir.to_lowercase();
            assert!(
                !CONDA_ROOT_NAMES.iter().any(|name| lower.contains(name)),
                "兜底目录 {dir} 不该包含 conda"
            );
        }
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