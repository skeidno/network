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
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 起一条不弹控制台窗口的后台命令。
///
/// 本程序是 GUI 子系统（`windows_subsystem = "windows"`），没有自己的控制台。
/// Windows 给控制台子进程（nvm / node --version / git / cmd / winget …）默认
/// 新建一个窗口，于是每次体检都闪一串黑框。CREATE_NO_WINDOW 只隐藏窗口，
/// stdout/stderr 照常能捕获，不影响探测结果。非 Windows 平台原样返回。
fn command(program: &str) -> std::process::Command {
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

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
    /// 分组：`core` 是本程序真正依赖的运行环境与工具，`language` 是开发语言工具链。
    /// 前端按它分两栏，且「一键部署缺失项」只管 core —— 否则点一下会把
    /// .NET SDK / PHP / Ruby 这些几百 MB 的大件一口气全装上。
    pub group: &'static str,
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
            group: "core",
        }
    }

    /// 开发语言工具链：项目本身不依赖它们，所以**不设最低版本**。
    /// 版本新旧交给独立的「最新稳定版」查询去比，不污染 core 的
    /// 就绪 / 缺失 / 版本过旧三态语义。
    fn new_lang(id: &'static str, name: &'static str) -> Self {
        Self {
            group: "language",
            ..Self::new(id, name, "")
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
            "group": self.group,
            // 「更新」按钮能不能用：Rust 要有 rustup，其余要有 winget。
            // Node 特意不给 —— 它归 nvm 管，winget 那个系统级包会跟 nvm 打架。
            "canUpdate": can_update(self.id),
            // 卸载白名单：WebView2 / VC++ 这类系统共用组件不给卸载入口。
            // 缺失项也保留这个按钮，方便清掉「装了一半」留下的残留目录。
            "canUninstall": can_uninstall(self.id),
        })
    }
}

/// 跑一条命令拿 stdout，失败返回空串。
///
/// 刻意不隐藏输出错误：这里拿到的就是我们要转述给用户的东西，
/// 吞掉错误只会让「为什么探测不到」变成玄学。
fn run_capture(program: &str, args: &[&str]) -> String {
    let out = command(program)
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
const EXTRA_EXE_DIRS: &[&str] = &[
    r"C:\Program Files\Git\cmd",
    r"C:\Program Files\Git\bin",
    r"C:\Program Files\nodejs",
    r"C:\Program Files\Android\Android Studio\jbr\bin",
    r"C:\Program Files\Microsoft\jdk-21.0.11.10-hotspot\bin",
    r"C:\Program Files\Android\platform-tools",
    r"C:\Program Files (x86)\Android\platform-tools",
    r"C:\Program Files (x86)\Android\adb",
    r"C:\Program Files\WebView2",
    // 语言工具链常见的默认安装位置（很多时候不会写进 PATH）
    r"C:\Program Files\Go\bin",
    r"C:\Program Files\dotnet",
    r"C:\Program Files\PHP",
];

/// 用户目录下的工具链 bin（rustup / go 默认都装在这里）。
/// PATH 里通常有，但有些机器是手动配的，补一层兜底能少报几个假的「缺失」。
fn home_exe_dirs() -> Vec<PathBuf> {
    let mut dirs_out = Vec::new();
    if let Some(home) = dirs::home_dir() {
        for rel in [
            r".cargo\bin",
            r"go\bin",
            r"AppData\Local\Programs\Go\bin",
            r"AppData\Local\Programs\PHP",
        ] {
            dirs_out.push(home.join(rel));
        }
    }
    dirs_out
}

/// 在 PATH 和几个常见安装位置里找一个可执行文件。
///
/// 顺序刻意是「PATH 优先，常见目录兜底」：PATH 里的那个才是用户真正在用的，
/// 而注册表/默认目录里可能躺着多个版本，挑错版本比找不到更麻烦。
fn find_exe(names: &[&str]) -> Option<PathBuf> {
    find_all_exes(names).into_iter().next()
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
    let mut extras: Vec<PathBuf> = EXTRA_EXE_DIRS.iter().map(PathBuf::from).collect();
    extras.extend(home_exe_dirs());
    for extra in extras {
        for name in names {
            candidates.push(extra.join(name));
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
/// `nvm_state` 由 scan_items 统一取一次传进来（status_local，不联网）。
/// 早先这里自己调 `nvm::status()`，每体检一遍就顺手联网拉一次全量版本表。
fn probe_node(item: &mut EnvItem, nvm_state: &Value) {
    if nvm_state
        .get("installed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
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

// ---------------------------------------------------------------------------
// 开发语言工具链
//
// 这一组只回答「装没装、什么版本」：项目本身不依赖它们，所以不设最低版本，
// 也不进「一键部署缺失项」的批量流程。新旧对比交给下面的「最新稳定版」查询，
// 不复用 core 的 outdated 三态 —— 那套语义是「本项目能不能跑」，不是「是否最新」。
// ---------------------------------------------------------------------------

/// Rust：`rustc --version` 输出 `rustc 1.99.1 (hash date)`。
/// 额外记一下 rustup / cargo 在不在 —— 没有 rustup 就没法用 `rustup update`。
fn probe_rust(item: &mut EnvItem) {
    if probe_by_version(item, &["rustc.exe", "rustc"], &["--version"]) {
        let mut tools = Vec::new();
        if find_exe(&["rustup.exe", "rustup"]).is_some() {
            tools.push("rustup");
        }
        if find_exe(&["cargo.exe", "cargo"]).is_some() {
            tools.push("cargo");
        }
        item.note = if tools.is_empty() {
            "找到 rustc，但没找到 rustup / cargo；建议改用 rustup 安装以便更新工具链"
                .to_string()
        } else {
            format!("工具链组件：{}", tools.join("、"))
        };
    } else {
        item.note = "未找到 rustc；官方推荐用 rustup 安装".to_string();
    }
}

/// Go：`go version` 输出 `go version go1.27.3 windows/amd64`。
fn probe_go(item: &mut EnvItem) {
    if probe_by_version(item, &["go.exe", "go"], &["version"]) {
        return;
    }
    item.note = "未找到 go；也可从 go.dev/dl 手动安装".to_string();
}

/// .NET SDK：`dotnet --version` 只报 SDK；只装了运行时时会问不出来。
fn probe_dotnet(item: &mut EnvItem) {
    if probe_by_version(item, &["dotnet.exe", "dotnet"], &["--version"]) {
        item.note = "dotnet --version 返回当前 SDK 版本".to_string();
        return;
    }
    item.note = "未找到 .NET SDK（只有运行时不算 SDK）".to_string();
}

/// PHP：`php -v` 是多行 banner，extract_version 会挑出首行的版本号。
fn probe_php(item: &mut EnvItem) {
    if probe_by_version(item, &["php.exe", "php"], &["-v"]) {
        return;
    }
    item.note = "未找到 php.exe；PHP 常装在自定义目录，没进 PATH 时会显示为缺失".to_string();
}

/// Ruby：`ruby -v` 输出 `ruby 4.0.9 (date) [x64-mingw-ucrt]`。
fn probe_ruby(item: &mut EnvItem) {
    if probe_by_version(item, &["ruby.exe", "ruby"], &["-v"]) {
        return;
    }
    item.note = "未找到 ruby.exe；Windows 上通常用 RubyInstaller 安装".to_string();
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
    let out = command("cmd")
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
/// 体检结果的短期缓存。
///
/// 前端曾经把「进环境页」写成每轮状态轮询都调一次，结果每 2.5 秒就全量
/// 重扫十几个工具、起一堆控制台子进程。前端已改成只在切页时拉一次，
/// 这里再兜一层：即便调用方以后又写错，同一进程内 15 秒内也只真扫一次，
/// 不会把整机 CPU 和一堆黑窗口拖起来。装完 / 切完版本走 scan_fresh 强制重扫。
static SCAN_CACHE: Mutex<Option<(Instant, Value)>> = Mutex::new(None);
const SCAN_MIN_INTERVAL: Duration = Duration::from_secs(15);

/// 常规体检：命中短期缓存就直接复用上次的快照。
pub fn scan() -> Value {
    scan_cached(false)
}

/// 安装 / 切换 / 卸载之后强制重扫：这时环境真的变了，缓存必须失效。
pub fn scan_fresh() -> Value {
    scan_cached(true)
}

fn scan_cached(force: bool) -> Value {
    let mut cache = SCAN_CACHE.lock().unwrap_or_else(|err| err.into_inner());
    if !force {
        if let Some((at, ref value)) = *cache {
            if at.elapsed() < SCAN_MIN_INTERVAL {
                return value.clone();
            }
        }
    }
    let value = build_scan();
    *cache = Some((Instant::now(), value.clone()));
    value
}

fn build_scan() -> Value {
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
        // winget 有没有。没有时不再把整列按钮灰掉：installMode=direct 表示
        // 自动切到官方直连安装，前端会额外给一个「修复 winget」的可选入口。
        "winget": winget_available(),
        "installMode": install_mode(),
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

    // nvm 状态只取一次，Node 卡片和 nvm 卡片共用：纯本地、不联网。
    // 复用同一份快照，也免得两处各起一遍 nvm 子进程。
    let nvm_state = crate::nvm::status_local();
    let nvm_installed = nvm_state
        .get("installed")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let mut python = EnvItem::new("python", "Python", MIN_PYTHON);
    probe_python(&mut python);
    python.finalize();
    items.push(python);

    let mut node = EnvItem::new("node", "Node.js", MIN_NODE);
    probe_node(&mut node, &nvm_state);
    node.finalize();
    items.push(node);

    // nvm 本体单列一项，排在 Node 后面。
    // 它只在「要装指定版本 Node」时才必需，所以不设最低版本 —— 老 nvm 也能用。
    // 只装最新版：nvm 自己有 `nvm upgrade`，给它开版本选择纯属添乱。
    let mut nvm = EnvItem::new("nvm", "nvm（Node 版本管理）", "");
    if nvm_installed {
        nvm.ready = true;
        nvm.version = nvm_state
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        nvm.path = crate::nvm::nvm_home_pub();
        let count = nvm_state
            .get("installedVersions")
            .and_then(|v| v.as_array())
            .map(|a| a.len())
            .unwrap_or(0);
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

    // 开发语言工具链。顺序就是卡片顺序：Rust / Go 是这次点名要加的，放最前。
    // 每一项都只起一个子进程问版本号，全部跟着 scan 的 15 秒缓存走。
    let mut rust = EnvItem::new_lang("rust", "Rust");
    probe_rust(&mut rust);
    rust.finalize();
    items.push(rust);

    let mut go = EnvItem::new_lang("go", "Go");
    probe_go(&mut go);
    go.finalize();
    items.push(go);

    let mut dotnet = EnvItem::new_lang("dotnet", ".NET SDK");
    probe_dotnet(&mut dotnet);
    dotnet.finalize();
    items.push(dotnet);

    let mut php = EnvItem::new_lang("php", "PHP");
    probe_php(&mut php);
    php.finalize();
    items.push(php);

    let mut ruby = EnvItem::new_lang("ruby", "Ruby");
    probe_ruby(&mut ruby);
    ruby.finalize();
    items.push(ruby);

    items
}

/// winget 的完整路径。
///
/// winget 是「应用执行别名」：真实入口在
/// `%LOCALAPPDATA%\Microsoft\WindowsApps\winget.exe`（一个 0 字节重解析点），
/// 不少机器不会把这个目录写进 PATH。只靠 `find_exe` 会误判成「本机没有 winget」，
/// 界面上所有「一键部署 / 更新」按钮被整列灰掉，用户只能看到「去官网下载」。
/// 所以先按常规路径找，再补应用别名目录。
fn winget_exe() -> Option<PathBuf> {
    if let Some(path) = find_exe(&["winget.exe"]) {
        return Some(path);
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA")
            .map(PathBuf::from)
            .or_else(|| dirs::home_dir().map(|home| home.join("AppData").join("Local")))?;
        let alias = base.join("Microsoft").join("WindowsApps").join("winget.exe");
        if alias.is_file() {
            return Some(alias);
        }
    }
    None
}

/// 给修复模块用的别名路径。不重新实现一遍探测，避免两边判断不一致。
pub fn winget_path() -> Option<PathBuf> {
    winget_exe()
}

/// winget 在不在。在的话走包管理器；不在也不代表装不了 ——
/// `envdirect` 会从官方源直连安装，见 `install_mode()`。
pub fn winget_available() -> bool {
    // 自证测试开关：全新电脑没有 App Installer，本机有，没法真把系统组件卸了验证。
    // 设了这个环境变量就强制走直连分支，用来对照验证 scanEnv / 版本下拉 / 安装
    // 按钮在「无 winget」时的真实行为。普通用户不会设置，也不影响鉴权与配置。
    if std::env::var_os("NETWORK_MANAGER_ENV_FORCE_DIRECT").is_some() {
        return false;
    }
    winget_exe().is_some()
}

/// 环境页当前的安装通道。前端据此决定摘要文案和「修复 winget」按钮显不显示。
pub fn install_mode() -> &'static str {
    if winget_available() {
        "winget"
    } else {
        "direct"
    }
}

/// 用绝对路径起 winget，别让 PATH 决定功能在不在。
///
/// 光修检测不够：调用写成 `command("winget")` 时，CreateProcess 依然按 PATH 找，
/// 检测通过、按钮亮了，一点下去还是「调用 winget 失败」。
fn winget_command() -> Result<std::process::Command, String> {
    let exe = winget_exe().ok_or_else(|| "本机没有 winget，无法自动安装。".to_string())?;
    Ok(command(&exe.to_string_lossy().to_string()))
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
        // 语言工具链。带大版本号的包 id 是 winget 的惯例（PHP 8.5 / Ruby 4.0 /
        // .NET 10 各自一个包），所以「更新」只升同一系列，跨大版本要手动装。
        // 这些 id 都在 winget 官方源里核对过。
        "rust" => Some("Rustlang.Rustup"),
        "go" => Some("GoLang.Go"),
        "dotnet" => Some("Microsoft.DotNet.SDK.10"),
        "php" => Some("PHP.PHP.8.5"),
        "ruby" => Some("RubyInstallerTeam.Ruby.4.0"),
        _ => None,
    }
}

/// winget 安装 / 重装 / 更新共用的参数。
const WINGET_COMMON: [&str; 4] = [
    "--exact",
    "--source",
    "winget",
    "--accept-package-agreements",
];

/// 跑 winget 安装。`version` 传空串表示装最新版。
///
/// 刻意同步执行而不是后台异步：安装过程要花几十秒到几分钟，
/// 界面上一个转圈的按钮比一个不知道在干嘛的 toast 诚实得多。
///
/// Node 走 nvm 而不是 winget：Node 要能同时装多个版本并存，winget 装的是
/// 单一系统级安装，装第二个版本会跟第一个打架。nvm 天然解决这件事。
pub fn install(id: &str, version: &str) -> Result<String, String> {
    // 全新系统没有 nvm，指定版本时也别硬转 nvm —— 交给官方 MSI 直连装这个系列。
    if id == "node" && !version.is_empty() && winget_available() {
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
    if id == "node" && !version.is_empty() && winget_available() {
        return crate::nvm::install_version(version);
    }
    run_winget(id, true, version)
}

/// 这一项的「更新」按钮能不能点。
///
/// Rust 用 rustup，不需要 winget；Node 归 nvm 管，winget 那个系统级包会跟
/// nvm 打架，所以只给「版本管理」，不给一键更新；其余都要有 winget。
pub fn can_update(id: &str) -> bool {
    match id {
        "rust" => find_exe(&["rustup.exe", "rustup"]).is_some(),
        // Node 有 nvm 就能装并切到最新版；没 nvm 时仍走卡片里的「版本管理」。
        // installed() 只查文件、不起子进程，逐项放进 to_json 也不拖慢体检。
        "node" => crate::nvm::installed(),
        _ => can_auto_install(id),
    }
}

/// 按大版本分包的 winget 项，从「最新稳定版」推出它对应的包 id。
///
/// winget 把 Python 3.13 / 3.14、PHP 8.4 / 8.5 这类大版本拆成不同的包，
/// `winget upgrade` 只会在同一个包里升补丁号。要装到官方最新版，
/// 就得拼出最新大版本的那个包 id。拼出来的串只可能是固定前缀 + 数字点，
/// 这里再过一道字符白名单才允许进命令行。
///
/// 不覆盖的项返回 None：GoLang.Go / Rustlang.Rustup 本来就只有一个包，
/// nvm 与 rustup 各有自己的更新通道。
fn cross_major_package(id: &str, latest: &str) -> Option<String> {
    let series = |count: usize| -> Option<String> {
        let parts: Vec<&str> = latest
            .trim()
            .trim_start_matches(['v', 'V'])
            .split('.')
            .take(count)
            .collect();
        if parts.len() != count
            || parts
                .iter()
                .any(|part| part.is_empty() || !part.chars().all(|c| c.is_ascii_digit()))
        {
            return None;
        }
        Some(parts.join("."))
    };
    let candidate = match id {
        "python" => format!("Python.Python.{}", series(2)?),
        "php" => format!("PHP.PHP.{}", series(2)?),
        "ruby" => format!("RubyInstallerTeam.Ruby.{}", series(2)?),
        "dotnet" => format!("Microsoft.DotNet.SDK.{}", series(1)?),
        _ => return None,
    };
    if candidate
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
    {
        Some(candidate)
    } else {
        None
    }
}

/// 把已装的工具更新到官方最新稳定版。
///
/// 三条路径，按 id 选：
/// - Node → `nvm install <latest>` + 切换（nvm 自己管多版本，不碰 winget）
/// - Rust → `rustup update stable`（一次把 rustc / cargo / std 全升到 stable）
/// - 其余 → winget。若最新版属于另一个大版本包（Python 3.13 → 3.15），
///   直接 `winget install` 那个新包，**旧包保留、不卸载**；新系列还没进
///   winget 源（实测 Python 3.15 就还没有）就退回同系列 upgrade 并如实说明。
///
/// 全程不碰 `--force`：更新不该有「卸了旧的却没装回来」的风险。
/// `latest` 由前端把查到的最新稳定版传进来，空串表示没查到，走保守的同系列升级。
pub fn update(id: &str, latest: &str) -> Result<String, String> {
    // 没有 winget 时 nvm 通常也不存在，Node 的「更新」改走官方 MSI 直连。
    if id == "node" && winget_available() {
        if latest.is_empty() {
            return Err("没拿到 Node 最新版本号，请先点「检查最新版本」".into());
        }
        return crate::nvm::install_version(latest);
    }

    // Rust 走官方工具链：rustup 能一次把 rustc / cargo / std 全部升到 stable，
    // winget 只会升级 rustup 这个安装器本身。
    if id == "rust" {
        let Some(rustup) = find_exe(&["rustup.exe", "rustup"]) else {
            if !winget_available() {
                return crate::envdirect::install("rust", "")
                    .map(|detail| format!("本机没有 rustup，已通过官方直连安装 Rust。\n{detail}"));
            }
            return Err("没找到 rustup，无法更新 Rust 工具链；请先到 rustup.rs 安装".into());
        };
        let output = command(&rustup.to_string_lossy())
            .args(["update", "stable"])
            .output()
            .map_err(|error| format!("调用 rustup 失败：{error}"))?;
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .trim()
        .to_string();
        return if output.status.success() {
            Ok(format!("rustup update stable 完成。\n{text}"))
        } else {
            Err(format!(
                "rustup update stable 退出码 {:?}：\n{text}",
                output.status.code()
            ))
        };
    }

    let Some(package) = winget_id(id) else {
        return Err(format!("这一项没有可用的 winget 包：{id}"));
    };
    if !winget_available() {
        // 全新系统：把「更新」等价成「直连装官方最新稳定版」，并如实说明通道。
        return crate::envdirect::install(id, latest)
            .map(|detail| format!("本机没有 winget，已通过官方直连安装最新稳定版。\n{detail}"));
    }

    // 跨大版本：装最新版对应的那个 winget 包，旧版本保留。
    if let Some(target) = cross_major_package(id, latest) {
        if target != package {
            match run_winget_command("install", &target, &WINGET_COMMON, None, None) {
                Ok(detail) => {
                    return Ok(format!(
                        "官方最新稳定版 {latest} 属于 {target}，与当前 {package} 不是同一个 winget 包。\n\
                         已自动安装新系列（旧版本保留、没有卸载）；PATH / 版本选择器指向哪个由安装器决定。\n{detail}"
                    ));
                }
                Err(install_error) => {
                    // 官方发了新版但 winget 源还没跟上，很常见。退回同系列升级，
                    // 并把「这次没到官方最新」说清楚，别假装成功。
                    let upgrade =
                        run_winget_command("upgrade", package, &WINGET_COMMON, None, None)
                            .map_err(|upgrade_error| {
                                format!(
                                    "安装最新系列 {target} 失败：{install_error}\n\
                                     同系列升级也失败：{upgrade_error}"
                                )
                            })?;
                    return Ok(format!(
                        "winget 源里暂时还没有 {target}（官方最新 {latest}），已改为升级当前系列 {package}；\
                         这次可能达不到官方最新版。\n{upgrade}"
                    ));
                }
            }
        }
    }

    let detail = run_winget_command("upgrade", package, &WINGET_COMMON, None, None)?;
    if detail.contains("No applicable upgrade") || detail.contains("没有可用的升级") {
        return Ok(format!(
            "{package} 已是 winget 源里的最新版，无需重复更新。\n{detail}"
        ));
    }
    Ok(format!("{package} 已更新。\n{detail}"))
}

/// 组装并执行 winget 命令。
fn run_winget(id: &str, force: bool, version: &str) -> Result<String, String> {
    let Some(package) = winget_id(id) else {
        return Err(format!("未知的环境条目：{id}"));
    };
    if !winget_available() {
        // 全新系统没有 App Installer：不让用户去官网，也不弹「无法安装」，
        // 直接走官方直连（版本为空时自动取官方最新稳定版）。
        return crate::envdirect::install(id, version)
            .map(|detail| format!("本机没有 winget，已自动切换为官方直连安装。\n{detail}"));
    }

    // 下拉里选的是「大版本系列」（Python 3.11 / .NET 10），不是补丁号。
    // winget 把每个大版本拆成独立包 id，--version 只认补丁号，
    // 所以这里换成对应包、装该系列最新补丁。不同包可并存，不会卸掉现有版本。
    if !version.is_empty()
        && series_search_prefix(id).is_some()
        && version.split('.').count() <= 2
    {
        let Some(target) = cross_major_package(id, version) else {
            return Err(format!("无法从版本 {version} 推导出 {id} 的 winget 包 id"));
        };
        let detail = run_winget_command(
            "install",
            &target,
            &WINGET_COMMON,
            if force { Some("--force") } else { None },
            None,
        )?;
        return Ok(format!(
            "已安装 {target}（{id} {version} 系列的最新补丁）。\n\
             winget 里各系列是独立包，可与现有版本并存；命令行默认指向哪个由 PATH / 版本选择器决定。\n{detail}"
        ));
    }

    if force {
        // 指定了版本就只能强制安装到那个版本：upgrade 不接受「降到某个旧版本」
        if !version.is_empty() {
            let detail =
                run_winget_command("install", package, &WINGET_COMMON, Some("--force"), Some(version))?;
            return Ok(format!(
                "{package} 已重装到指定版本 {version}（先卸载了旧版本）。\n{detail}"
            ));
        }
        // 先试升级：非破坏性，失败也不动用户现有环境
        match run_winget_command("upgrade", package, &WINGET_COMMON, None, None) {
            Ok(detail) => return Ok(format!("{package} 已升级。\n{detail}")),
            Err(upgrade_error) => {
                // 升级走不通（源里没新版 / 包不支持升级）才强制重装
                // 强制安装会先卸载，所以文案必须说明这一步发生了什么
                let detail = run_winget_command("install", package, &WINGET_COMMON, Some("--force"), None)
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
        run_winget_command("install", package, &WINGET_COMMON, extra, if version.is_empty() { None } else { Some(version) })?;
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

    let output = winget_command()?
        .args(&args)
        .output()
        .map_err(|error| format!("调用 winget 失败：{error}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let combined = format!("{stdout}\n{stderr}").trim().to_string();

    if output.status.success() {
        Ok(combined)
    } else if winget_no_op(&combined) {
        // winget 对「已经装了、又没有可用升级」也返回非零（0x8A15002B）。
        // 这不是安装失败：卡片还没刷新时用户会再点一次「一键部署」，
        // 第二次不能因为「其实早就装好了」而报错。
        Ok(format!("{package} 已是最新，无需重复安装。\n{combined}"))
    } else {
        Err(format!(
            "winget {action} 退出码 {:?}：\n{combined}",
            output.status.code()
        ))
    }
}

/// 识别 winget 的「无需操作」输出。必须同时出现「已安装」和「没有可用升级」两层意思，
/// 免得把下载失败、哈希校验失败这类真错误也当成成功。
fn winget_no_op(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    let says_installed = output.contains("已安装")
        || lower.contains("already installed")
        || lower.contains("existing package");
    let says_nothing_to_do = output.contains("找不到可用的升级")
        || output.contains("没有可用的较新的包版本")
        || lower.contains("no applicable upgrade")
        || lower.contains("no newer package versions")
        || lower.contains("no available upgrade");
    says_installed && says_nothing_to_do
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
        "rust" => "https://rustup.rs/",
        "go" => "https://go.dev/dl/",
        "dotnet" => "https://dotnet.microsoft.com/download",
        "php" => "https://www.php.net/downloads.php",
        "ruby" => "https://rubyinstaller.org/",
        _ => return Err(format!("未知的环境条目：{id}")),
    };
    open_url(url)
}

// ---------------------------------------------------------------------------
// 一键卸载：官方卸载 + 残留清理
// ---------------------------------------------------------------------------
//
// 「卸载干净」不是一句 winget uninstall 就完事：Go 这类工具会同时在
// Program Files 留主程序、在 %LOCALAPPDATA% 留编译缓存、在用户目录留 GOPATH，
// 还要往 PATH / GOROOT / GOPATH 写环境变量。winget 的 --purge 只管它自己
// 认得的那个包目录，且包不在 winget 里时（手动装的）整个卸载会失败。
//
// 所以这里分四步：官方卸载 → 删白名单残留目录 → 清 PATH 与环境变量 → 复核体检。
// 官方卸载失败**不中断**：残留目录照样清，报告里如实写明哪步没成功。
// 所有路径都由下面的常量白名单拼出来，绝不接收前端传进来的任意路径。

/// 能不能卸载。WebView2 / VC++ 运行库是系统组件，卸了会打坏别的程序
/// （甚至本程序自己的界面），明确不给这个入口。
pub fn can_uninstall(id: &str) -> bool {
    matches!(
        id,
        "go" | "rust" | "python" | "node" | "git" | "jdk" | "adb" | "dotnet" | "php" | "ruby" | "nvm"
    )
}

fn display_name(id: &str) -> &'static str {
    match id {
        "go" => "Go",
        "rust" => "Rust",
        "python" => "Python",
        "node" => "Node.js",
        "git" => "Git",
        "jdk" => "JDK",
        "adb" => "ADB Platform Tools",
        "dotnet" => ".NET SDK",
        "php" => "PHP",
        "ruby" => "Ruby",
        "nvm" => "nvm-windows",
        "webview2" => "WebView2 Runtime",
        "vcredist" => "VC++ 运行库",
        _ => "未知组件",
    }
}

#[derive(Debug, Clone)]
struct CleanupTarget {
    /// install = 程序本体；cache = 缓存/配置；userdata = 可能含用户自己的东西
    kind: &'static str,
    path: PathBuf,
    note: &'static str,
}

fn os_dir(key: &str) -> Option<PathBuf> {
    std::env::var_os(key)
        .map(PathBuf::from)
        .filter(|path| path.is_dir())
}

fn user_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| PathBuf::from("C:\\"))
}

/// 枚举 base 下名字以 prefix 开头的子目录（Python3xx / jdk-21 / Ruby 4.0 这类）。
fn prefixed_dirs(base: Option<PathBuf>, prefix: &str) -> Vec<PathBuf> {
    let Some(base) = base else { return Vec::new() };
    let Ok(entries) = std::fs::read_dir(base) else {
        return Vec::new();
    };
    let needle = prefix.to_ascii_lowercase();
    entries
        .flatten()
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .map(|name| name.to_ascii_lowercase().starts_with(&needle))
                .unwrap_or(false)
        })
        .collect()
}

/// 该文件全部由白名单目录拼成；前端只能传 id 与一个 deep 布尔值。
fn cleanup_targets(id: &str) -> Vec<CleanupTarget> {
    let home = user_home();
    let local = os_dir("LOCALAPPDATA");
    let appdata = os_dir("APPDATA");
    let pf = os_dir("ProgramFiles");
    let pf86 = os_dir("ProgramFiles(x86)");
    let drive = std::env::var_os("SystemDrive")
        .map(|value| PathBuf::from(format!("{}\\", value.to_string_lossy())))
        .filter(|path| path.is_dir());
    let mut out: Vec<CleanupTarget> = Vec::new();
    macro_rules! add {
        ($kind:expr, $path:expr, $note:expr) => {
            if let Some(path) = $path {
                out.push(CleanupTarget { kind: $kind, path, note: $note });
            }
        };
    }
    let join = |base: &Option<PathBuf>, rel: &str| base.as_ref().map(|dir| dir.join(rel));

    match id {
        "go" => {
            add!("install", join(&pf, "Go"), "Go 主安装目录");
            add!("install", join(&pf86, "Go"), "Go 主安装目录（备用路径）");
            add!("install", join(&local, r"Programs\Go"), "Go 用户级安装目录");
            add!("cache", join(&local, "go-build"), "Go 编译缓存");
            add!("cache", join(&appdata, "go"), "go env 配置与遥测");
            add!("userdata", Some(home.join("go")), "GOPATH（可能含你自己写的项目或模块缓存）");
        }
        "rust" => {
            add!("install", Some(home.join(".cargo")), "Cargo 工具链与 registry 缓存");
            add!("install", Some(home.join(".rustup")), "rustup 工具链");
        }
        "python" => {
            for dir in prefixed_dirs(local.as_ref().map(|p| p.join("Programs").join("Python")), "Python3") {
                out.push(CleanupTarget { kind: "install", path: dir, note: "Python 安装目录" });
            }
            for dir in prefixed_dirs(drive.clone(), "Python3") {
                out.push(CleanupTarget { kind: "install", path: dir, note: "Python 根目录安装" });
            }
            add!("cache", join(&local, r"pip\Cache"), "pip 下载缓存");
            add!("cache", join(&appdata, "Python"), "Python 用户配置");
        }
        "node" => {
            add!("install", join(&pf, "nodejs"), "Node 系统级安装目录");
            add!("install", join(&local, r"Programs\nodejs"), "Node 用户级安装目录");
            add!("cache", join(&appdata, "npm-cache"), "npm 缓存");
            add!("cache", join(&local, "npm-cache"), "npm 缓存（本地）");
            add!("userdata", join(&appdata, "npm"), "npm 全局安装的包");
        }
        "git" => {
            add!("install", join(&pf, "Git"), "Git 安装目录");
            add!("install", join(&pf86, "Git"), "Git 安装目录（备用路径）");
        }
        "jdk" => {
            for dir in prefixed_dirs(pf.as_ref().map(|p| p.join("Microsoft")), "jdk-") {
                out.push(CleanupTarget { kind: "install", path: dir, note: "Microsoft JDK" });
            }
            for dir in prefixed_dirs(pf.as_ref().map(|p| p.join("Eclipse Adoptium")), "jdk-") {
                out.push(CleanupTarget { kind: "install", path: dir, note: "Eclipse Temurin JDK" });
            }
            for dir in prefixed_dirs(pf.as_ref().map(|p| p.join("Java")), "jdk") {
                out.push(CleanupTarget { kind: "install", path: dir, note: "Java JDK" });
            }
        }
        "adb" => {
            add!("install", join(&local, r"Android\Sdk\platform-tools"), "Android platform-tools");
            add!("install", drive.as_ref().map(|p| p.join("platform-tools")), "platform-tools 根目录");
            add!("install", join(&pf, r"Android\platform-tools"), "platform-tools（Program Files）");
        }
        "dotnet" => {
            add!("install", join(&pf, "dotnet"), ".NET SDK 安装目录");
            add!("install", join(&local, r"Programs\dotnet"), ".NET SDK 用户级安装目录");
            add!("cache", Some(home.join(".dotnet")), ".NET 用户级工具与缓存");
            add!("cache", Some(home.join(".templateengine")), ".NET 模板缓存");
        }
        "php" => {
            add!("install", join(&pf, "PHP"), "PHP 安装目录");
            add!("install", drive.as_ref().map(|p| p.join("php")), "PHP 根目录安装");
            add!("install", join(&local, r"Programs\PHP"), "PHP 用户级安装目录");
        }
        "ruby" => {
            for dir in prefixed_dirs(drive.clone(), "Ruby") {
                out.push(CleanupTarget { kind: "install", path: dir, note: "RubyInstaller 安装目录" });
            }
            add!("install", join(&local, r"Programs\Ruby"), "Ruby 用户级安装目录");
        }
        "nvm" => {
            add!("install", join(&appdata, "nvm"), "nvm 版本库");
            add!("install", join(&local, r"Programs\nvm"), "nvm 用户级安装目录");
        }
        _ => {}
    }
    out
}

/// PATH 条目里属于这个工具的片段。用带反斜杠的片段，避免把名字里碰巧含 go 的路径误删。
fn path_needles(id: &str) -> Vec<&'static str> {
    match id {
        "go" => vec![r"\go\bin"],
        "rust" => vec![r"\.cargo\bin", r"\.rustup"],
        "python" => vec![r"\python3", r"\python\scripts"],
        "node" => vec![r"\nodejs", r"\npm"],
        "git" => vec![r"\git\cmd", r"\git\bin"],
        "jdk" => vec![r"\jdk-", r"\java\jdk"],
        "adb" => vec![r"\platform-tools"],
        "dotnet" => vec![r"\dotnet"],
        "php" => vec![r"\php"],
        "ruby" => vec![r"\ruby"],
        "nvm" => vec![r"\nvm"],
        _ => vec![],
    }
}

fn env_names(id: &str) -> Vec<&'static str> {
    match id {
        "go" => vec!["GOROOT", "GOPATH"],
        "rust" => vec!["CARGO_HOME", "RUSTUP_HOME"],
        "python" => vec!["PYTHONHOME", "PYTHONPATH"],
        "jdk" => vec!["JAVA_HOME"],
        "dotnet" => vec!["DOTNET_ROOT", "DOTNET_CLI_HOME"],
        "node" => vec!["NODE_PATH", "npm_config_prefix"],
        "nvm" => vec!["NVM_HOME", "NVM_SYMLINK"],
        _ => vec![],
    }
}

fn path_hit(id: &str, entry: &str) -> bool {
    let lower = entry
        .trim_end_matches(['\\', '/'])
        .to_ascii_lowercase();
    path_needles(id)
        .iter()
        .any(|needle| lower.contains(needle))
}

/// 当前进程 PATH 里会被清掉的条目，只用于卸载前的清单展示。
fn current_path_entries(id: &str) -> Vec<String> {
    let Some(path) = std::env::var_os("PATH") else {
        return Vec::new();
    };
    std::env::split_paths(&path)
        .map(|dir| dir.to_string_lossy().to_string())
        .filter(|dir| path_hit(id, dir))
        .collect()
}

/// 只统计个大概：目录可能有上万文件，全量递归会让预览卡住状态轮询。
fn dir_stats(path: &Path) -> (usize, u64) {
    fn walk(path: &Path, depth: u8, files: &mut usize, bytes: &mut u64) {
        if depth > 4 || *files > 3000 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            if *files > 3000 {
                break;
            }
            let child = entry.path();
            if child.is_dir() {
                walk(&child, depth + 1, files, bytes);
            } else {
                *files += 1;
                *bytes += entry.metadata().map(|meta| meta.len()).unwrap_or(0);
            }
        }
    }
    let mut files = 0;
    let mut bytes = 0;
    walk(path, 0, &mut files, &mut bytes);
    (files, bytes)
}

/// 卸载预览：纯只读，把「会删什么」摊给用户看，一个字节都不动。
pub fn uninstall_preview(id: &str) -> Value {
    let allowed = can_uninstall(id);
    let reason = if allowed {
        ""
    } else if matches!(id, "webview2" | "vcredist") {
        "WebView2 / VC++ 运行库是系统共用组件，卸载会破坏其它程序，本页不提供卸载"
    } else {
        "未知的环境条目"
    };
    let paths: Vec<Value> = cleanup_targets(id)
        .iter()
        .map(|target| {
            let exists = target.path.exists();
            let (files, bytes) = if exists {
                dir_stats(&target.path)
            } else {
                (0, 0)
            };
            json!({
                "kind": target.kind,
                "path": target.path.to_string_lossy(),
                "exists": exists,
                "files": files,
                "bytes": bytes,
                "note": target.note,
            })
        })
        .collect();
    let env_vars: Vec<Value> = env_names(id)
        .iter()
        .map(|name| {
            json!({
                "name": name,
                "value": std::env::var(name).unwrap_or_default(),
            })
        })
        .collect();
    json!({
        "id": id,
        "name": display_name(id),
        "package": winget_id(id).unwrap_or(""),
        "allowed": allowed,
        "reason": reason,
        "paths": paths,
        "pathEntries": current_path_entries(id),
        "envVars": env_vars,
    })
}

/// 官方卸载命令。包不在 winget 里不算致命错误，后面还会清残留目录。
fn official_uninstall(id: &str) -> Result<String, String> {
    if id == "rust" {
        let exe = find_exe(&["rustup.exe", "rustup"])
            .ok_or_else(|| "没找到 rustup，跳过 rustup 自卸载".to_string())?;
        let output = command(&exe.to_string_lossy())
            .args(["self", "uninstall", "-y"])
            .output()
            .map_err(|error| format!("调用 rustup 失败：{error}"))?;
        let text = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .trim()
        .to_string();
        return if output.status.success() {
            Ok(text)
        } else {
            Err(format!("rustup self uninstall 退出码 {:?}：\n{text}", output.status.code()))
        };
    }
    let package = winget_id(id).ok_or_else(|| format!("未知的环境条目：{id}"))?;
    let output = winget_command()?
        .args([
            "uninstall",
            "--id",
            package,
            "--exact",
            "--silent",
            "--purge",
            "--disable-interactivity",
        ])
        .output()
        .map_err(|error| format!("调用 winget 失败：{error}"))?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .trim()
    .to_string();
    if output.status.success() {
        Ok(text)
    } else if uninstall_not_found(&text) {
        Ok(format!("winget 里没有这个包（可能是手动安装的），跳过官方卸载：\n{text}"))
    } else {
        Err(format!(
            "winget uninstall 退出码 {:?}：\n{text}",
            output.status.code()
        ))
    }
}

/// winget「没找到已安装的包」也是非零退出，但它不代表失败：
/// 用户可能手动装过，残留目录仍然要清。
fn uninstall_not_found(output: &str) -> bool {
    let lower = output.to_ascii_lowercase();
    output.contains("找不到已安装的包")
        || output.contains("没有找到与输入条件匹配的已安装包")
        || lower.contains("no installed package found")
        || lower.contains("no package found matching")
        || lower.contains("no applicable")
}

#[cfg(windows)]
const CLEAN_ENV_SCRIPT: &str = r#"
$needles = @(__NEEDLES__)
$names = @(__NAMES__)
foreach ($scope in @('User','Machine')) {
  try {
    $p = [Environment]::GetEnvironmentVariable('Path', $scope)
    if ($p) {
      $parts = @()
      foreach ($entry in ($p -split ';')) {
        $t = $entry.TrimEnd('\')
        if (-not $t) { continue }
        $hit = $false
        foreach ($n in $needles) { if ($t.ToLower().Contains($n.ToLower())) { $hit = $true; break } }
        if (-not $hit) { $parts += $entry }
      }
      $new = ($parts -join ';')
      if ($new -ne $p) {
        [Environment]::SetEnvironmentVariable('Path', $new, $scope)
        Write-Output ("PATH(" + $scope + "): removed matching entries")
      } else {
        Write-Output ("PATH(" + $scope + "): nothing to remove")
      }
    }
    foreach ($name in $names) {
      if ([Environment]::GetEnvironmentVariable($name, $scope)) {
        [Environment]::SetEnvironmentVariable($name, $null, $scope)
        Write-Output ("ENV(" + $scope + "): removed " + $name)
      }
    }
  } catch {
    Write-Output ("ENV(" + $scope + "): skipped - " + $_.Exception.Message)
  }
}
"#;

#[cfg(windows)]
fn ps_literal(items: &[&str]) -> String {
    items
        .iter()
        .map(|item| format!("'{}'", item.replace('\'', "''")))
        .collect::<Vec<_>>()
        .join(",")
}

#[cfg(windows)]
fn clean_environment(id: &str) -> String {
    let needles = path_needles(id);
    let names = env_names(id);
    if needles.is_empty() && names.is_empty() {
        return "  该项没有需要清理的 PATH / 环境变量".to_string();
    }
    let script = CLEAN_ENV_SCRIPT
        .replace("__NEEDLES__", &ps_literal(&needles))
        .replace("__NAMES__", &ps_literal(&names));
    match command("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", &script])
        .output()
    {
        Ok(output) => {
            let text = format!(
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let lines: Vec<String> = text
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty())
                .map(|line| format!("  {line}"))
                .collect();
            if lines.is_empty() {
                "  PowerShell 没有返回输出".to_string()
            } else {
                lines.join("\n")
            }
        }
        Err(error) => format!("  调用 PowerShell 失败：{error}"),
    }
}

#[cfg(not(windows))]
fn clean_environment(_id: &str) -> String {
    "  非 Windows 平台跳过环境变量清理".to_string()
}

/// 进程内的 PATH 是启动时读进来的，改注册表不会自动刷新。
/// 不把它同步清掉，装完立刻复核时 find_exe 还会从旧 PATH 找到刚删掉的 exe。
fn scrub_current_process_path(id: &str) {
    let Some(path) = std::env::var_os("PATH") else {
        return;
    };
    let kept: Vec<PathBuf> = std::env::split_paths(&path)
        .filter(|dir| !path_hit(id, &dir.to_string_lossy()))
        .collect();
    if let Ok(joined) = std::env::join_paths(kept) {
        std::env::set_var("PATH", joined);
    }
}

/// 一键卸载 + 清理。`deep` 为 true 才删 userdata 目录（GOPATH / npm 全局包等）。
pub fn uninstall(id: &str, deep: bool) -> Result<String, String> {
    if !cfg!(windows) {
        return Err("卸载与清理目前只在 Windows 上可用".into());
    }
    if !can_uninstall(id) {
        return Err(format!("{} 不允许在这里卸载", display_name(id)));
    }
    let mut report = vec![format!(
        "卸载 {}（深度清理：{}）",
        display_name(id),
        if deep { "开" } else { "关（保留用户数据目录）" }
    )];

    crate::envtask::set_phase("调用官方卸载程序（winget 卸载可能需要几分钟）");
    match official_uninstall(id) {
        Ok(detail) => report.push(format!("[1/4] 官方卸载\n{}", indent(&detail))),
        // 官方卸载失败照样往下清残留：winget 里没这个包时，目录才是真正要处理的东西
        Err(error) => report.push(format!("[1/4] 官方卸载未完成，继续清理残留\n{}", indent(&error))),
    }

    crate::envtask::set_phase("清理残留目录");
    let mut removed = 0;
    let mut kept = 0;
    let mut failed = 0;
    for target in cleanup_targets(id) {
        if !target.path.exists() {
            continue;
        }
        if target.kind == "userdata" && !deep {
            kept += 1;
            report.push(format!("  保留 {}: {}（{}）", target.kind, target.path.display(), target.note));
            continue;
        }
        match std::fs::remove_dir_all(&target.path) {
            Ok(()) => {
                removed += 1;
                report.push(format!("  已删除 {}: {}", target.kind, target.path.display()));
            }
            Err(error) => {
                failed += 1;
                report.push(format!("  删除失败 {}: {} — {error}", target.kind, target.path.display()));
            }
        }
    }
    report.push(format!(
        "[2/4] 残留目录：已删 {removed} 个，保留 {kept} 个，失败 {failed} 个"
    ));

    crate::envtask::set_phase("清理 PATH 与环境变量");
    report.push(format!("[3/4] PATH / 环境变量\n{}", clean_environment(id)));
    scrub_current_process_path(id);

    crate::envtask::set_phase("复核体检结果");
    let env = scan_fresh();
    let still_ready = env
        .get("items")
        .and_then(Value::as_array)
        .and_then(|items| {
            items
                .iter()
                .find(|item| item.get("id").and_then(Value::as_str) == Some(id))
        })
        .and_then(|item| item.get("ready").and_then(Value::as_bool))
        .unwrap_or(false);
    report.push(format!(
        "[4/4] 复核：{}",
        if still_ready {
            "仍能检测到这个工具，可能还有别的安装位置；已打开的终端需要重开才会读到新的 PATH"
        } else {
            "已检测不到，环境清理完成（已打开的终端需重开才生效）"
        }
    ));
    Ok(report.join("\n"))
}

fn indent(text: &str) -> String {
    text.lines()
        .map(|line| format!("  {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 用系统默认浏览器打开一个链接。
fn open_url(url: &str) -> Result<(), String> {
    #[cfg(windows)]
    let result = command("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(target_os = "macos")]
    let result = command("open").arg(url).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let result = command("xdg-open").arg(url).spawn();
    result
        .map(|_| ())
        .map_err(|err| format!("打开链接失败：{err}"))
}

/// 这个条目是不是「一键部署」能搞定的。
///
/// 全新系统没有 winget 时改看官方直连通道支不支持这一项；没有稳定直链的
/// 项目仍然灰掉，但界面会把「修复 winget」摆出来，不是无解的死胡同。
pub fn can_auto_install(id: &str) -> bool {
    if winget_available() {
        winget_id(id).is_some()
    } else {
        crate::envdirect::supports(id)
    }
}

/// 这一项支不支持指定版本安装。
///
/// Node 特殊：它走 nvm，能同时并存多个版本，切版本不动其它项目。
/// python / php / ruby / dotnet 在 winget 里按大版本拆成多个包，
/// 下拉给的是「系列」（3.10 / 3.11 / 3.12…），装不同系列可以并存；
/// git / jdk / go 只有一个包，下拉给的是补丁号，换版本会覆盖现有安装。
fn supports_versions(id: &str) -> bool {
    matches!(
        id,
        "node" | "python" | "php" | "ruby" | "dotnet" | "git" | "jdk" | "go"
    )
}

/// winget 里按大版本分包的项目：包 id 的公共前缀。
/// 这些项不能用 `show --versions`（那只给某一个包里的补丁号），
/// 要改用 `search` 把源里所有系列列出来。
fn series_search_prefix(id: &str) -> Option<&'static str> {
    match id {
        "python" => Some("Python.Python."),
        "php" => Some("PHP.PHP."),
        "ruby" => Some("RubyInstallerTeam.Ruby."),
        "dotnet" => Some("Microsoft.DotNet.SDK."),
        _ => None,
    }
}

/// 把版本号拆成数字段用于倒序排序，避免 "3.9" 排在 "3.10" 后面。
fn version_key(text: &str) -> Vec<u64> {
    text.split('.')
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

/// 列出某个项能装的版本（走 winget 的包）。
/// Node 不在这里 —— 它的版本来自 nvm，见 crate::nvm::available_versions。
pub fn available_versions(id: &str) -> Result<Value, String> {
    let Some(package) = winget_id(id) else {
        return Err(format!("未知的环境条目：{id}"));
    };
    if !winget_available() {
        // 全新电脑没有 App Installer。以前这里直接报错，用户点开下拉只看到一句
        // 「本机没有 winget」，什么也做不了。改成问官方发布源，安装动作由 envdirect
        // 直连完成，版本下拉不再是死胡同。
        return crate::envdirect::versions(id);
    }
    if let Some(prefix) = series_search_prefix(id) {
        return series_versions(id, prefix);
    }
    patch_versions(id, package)
}

/// 按大版本分包的项：从 `winget search` 的真实结果里提取系列。
///
/// 实测 `winget search --id Python.Python` 会列出 3.0 ~ 3.14 全部包，
/// 每行形如 `Python 3.11   Python.Python.3.11   3.11.9`，第二列就是包 id。
/// 不硬编码版本清单：源里新增 3.15 时下拉自动跟上。
fn series_versions(id: &str, prefix: &str) -> Result<Value, String> {
    let output = winget_command()?
        .args([
            "search",
            "--id",
            prefix.trim_end_matches('.'),
            "--source",
            "winget",
            "--disable-interactivity",
            "--accept-source-agreements",
        ])
        .output()
        .map_err(|error| format!("调用 winget 失败：{error}"))?;
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    if !output.status.success() {
        return Err(format!("查询 {id} 的大版本列表失败：\n{text}"));
    }
    let series = parse_series_versions(&text, prefix, id);
    Ok(json!({
        "id": id,
        "versions": series,
        // 前端据此把文案从「会卸载现有版本」改成「各系列可并存」
        "versionKind": "series",
    }))
}

/// 从 winget search 的输出里挑出大版本系列，倒序去重。
/// 抽成纯函数是为了能拿真实输出片段做单测：字符串排序会把 "3.9" 排在
/// "3.10" 后面，必须按数字段比。
fn parse_series_versions(text: &str, prefix: &str, id: &str) -> Vec<String> {
    let mut series: Vec<String> = text
        .lines()
        .filter_map(|line| {
            let token = line.split_whitespace().find(|part| part.starts_with(prefix))?;
            let tail = &token[prefix.len()..];
            let valid = !tail.is_empty()
                && tail.split('.').count() <= 2
                && tail.chars().all(|c| c.is_ascii_digit() || c == '.');
            valid.then(|| tail.to_string())
        })
        .collect();
    // 过滤掉早就没有维护必要的老系列，别让下拉里混进 Python 3.0。
    let (min_major, min_minor) = match id {
        "python" => (3, 9),
        "php" => (8, 0),
        "ruby" => (3, 0),
        "dotnet" => (8, 0),
        _ => (0, 0),
    };
    series.retain(|value| {
        let key = version_key(value);
        (key.first().copied().unwrap_or(0), key.get(1).copied().unwrap_or(0))
            >= (min_major, min_minor)
    });
    series.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
    series.dedup();
    series
}

/// 单包项目：列这个包自己的历史补丁版本。
fn patch_versions(id: &str, package: &str) -> Result<Value, String> {
    let output = winget_command()?
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
    Ok(json!({ "id": id, "versions": versions, "versionKind": "patch" }))
}

// ---------------------------------------------------------------------------
// 最新稳定版查询
//
// 这是环境页里唯一会联网的动作，且**只由用户点「检查最新版本」触发**：
// 前端不自动调、不跟随状态轮询，后端再加 10 分钟缓存。之前把联网探测塞进
// 每轮体检，结果停在环境页就每 2.5 秒拉一次全量版本表 —— 别再走回头路。
//
// 查询本身必须在 state 锁外执行（见 server.rs），否则几秒的网络等待会把
// 状态轮询和所有其它 API 一起卡住。
// ---------------------------------------------------------------------------

static LATEST_CACHE: Mutex<Option<(Instant, Value)>> = Mutex::new(None);
const LATEST_TTL: Duration = Duration::from_secs(10 * 60);

/// id →（数据源 URL，来源名）。endoflife.date 的 API 返回「最新周期在前」的数组，
/// 第一项的 `latest` 就是该系列最新稳定补丁版。
const LATEST_SOURCES: &[(&'static str, &'static str, &'static str)] = &[
    ("python", "https://endoflife.date/api/python.json", "endoflife.date"),
    ("node", "https://endoflife.date/api/nodejs.json", "endoflife.date"),
    ("go", "https://endoflife.date/api/go.json", "endoflife.date"),
    ("rust", "https://endoflife.date/api/rust.json", "endoflife.date"),
    ("dotnet", "https://endoflife.date/api/dotnet.json", "endoflife.date"),
    ("php", "https://endoflife.date/api/php.json", "endoflife.date"),
    ("ruby", "https://endoflife.date/api/ruby.json", "endoflife.date"),
];

/// 从各数据源的 JSON 里挑出版本号。数组取首项的 latest / cycle，对象取 version。
fn parse_latest(body: &str) -> Option<String> {
    let value: Value = serde_json::from_str(body).ok()?;
    let raw = if let Some(list) = value.as_array() {
        let first = list.first()?;
        first
            .get("latest")
            .and_then(Value::as_str)
            .or_else(|| first.get("cycle").and_then(Value::as_str))
    } else {
        value.get("version").and_then(Value::as_str)
    }?;
    let version = raw.trim().trim_start_matches(['v', 'V']).to_string();
    // 挡住 "unknown" / 空串这类脏数据：显示出来比不显示更糟
    if !version.chars().next()?.is_ascii_digit() {
        return None;
    }
    Some(version)
}

async fn fetch_latest(client: reqwest::Client, url: &'static str) -> Result<String, String> {
    let request = async {
        let response = client
            .get(url)
            .header("User-Agent", "NetWorkManger-EnvCheck")
            .send()
            .await
            .map_err(|error| format!("请求失败：{error}"))?;
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        response
            .text()
            .await
            .map_err(|error| format!("读取响应失败：{error}"))
    };
    let body = tokio::time::timeout(Duration::from_secs(10), request)
        .await
        .map_err(|_| "查询超时（10 秒）".to_string())??;
    parse_latest(&body).ok_or_else(|| "返回内容里没有可识别的版本号".to_string())
}

/// 查一次所有语言工具链的最新稳定版。单项失败不影响其它项，错误原样回给前端。
pub async fn latest_versions(force: bool) -> Value {
    if !force {
        let cache = LATEST_CACHE.lock().unwrap_or_else(|error| error.into_inner());
        if let Some((at, ref value)) = *cache {
            if at.elapsed() < LATEST_TTL {
                return value.clone();
            }
        }
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let mut tasks = tokio::task::JoinSet::new();
    for &(id, url, source) in LATEST_SOURCES {
        let client = client.clone();
        tasks.spawn(async move {
            let result = fetch_latest(client, url).await;
            (id, source, url, result)
        });
    }

    let mut items = serde_json::Map::new();
    let mut ok = 0usize;
    let mut failed = 0usize;
    while let Some(joined) = tasks.join_next().await {
        let Ok((id, source, url, result)) = joined else {
            failed += 1;
            continue;
        };
        match result {
            Ok(latest) => {
                ok += 1;
                items.insert(
                    id.to_string(),
                    json!({ "latest": latest, "source": source, "url": url }),
                );
            }
            Err(error) => {
                failed += 1;
                items.insert(
                    id.to_string(),
                    json!({ "error": error, "source": source, "url": url }),
                );
            }
        }
    }

    let value = json!({
        "items": Value::Object(items),
        "checkedAt": chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string(),
        "ok": ok,
        "failed": failed,
    });
    *LATEST_CACHE
        .lock()
        .unwrap_or_else(|error| error.into_inner()) = Some((Instant::now(), value.clone()));
    value
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
    fn 版本号提取覆盖语言工具链格式() {
        assert_eq!(
            extract_version("rustc 1.99.1 (58cf8f9c3 2026-10-13)"),
            "1.99.1"
        );
        assert_eq!(extract_version("go version go1.27.3 windows/amd64"), "1.27.3");
        assert_eq!(extract_version("10.0.403"), "10.0.403");
        assert_eq!(
            extract_version("PHP 8.5.12 (cli) (built: Nov  4 2026 12:00:00)"),
            "8.5.12"
        );
        assert_eq!(
            extract_version("ruby 4.0.9 (2026-03-14) [x64-mingw-ucrt]"),
            "4.0.9"
        );
    }

    #[test]
    fn 最新稳定版解析覆盖两类数据源() {
        // endoflife.date：最新周期在前的数组
        assert_eq!(
            parse_latest(r#"[{"cycle":"3.15","latest":"3.15.2","lts":false}]"#),
            Some("3.15.2".to_string())
        );
        // 没有 latest 就退回 cycle，别把整项丢掉
        assert_eq!(parse_latest(r#"[{"cycle":"1.99"}]"#), Some("1.99".to_string()));
        // 防御性兼容：以后若接回对象形数据源也能解析
        assert_eq!(
            parse_latest(r#"{"version":"2.0.1"}"#),
            Some("2.0.1".to_string())
        );
        assert_eq!(parse_latest(r#"{"version":"v26.13.0"}"#), Some("26.13.0".to_string()));
        assert_eq!(parse_latest(r#"{"version":"unknown"}"#), None);
        assert_eq!(parse_latest("not json"), None);
        assert_eq!(parse_latest("[]"), None);
    }

    #[test]
    fn 新增语言项都有安装源与官网链接() {
        for id in ["rust", "go", "dotnet", "php", "ruby"] {
            assert!(winget_id(id).is_some(), "{id} 缺 winget 包 id");
        }
    }

    #[test]
    fn 跨大版本包id从最新稳定版推导() {
        assert_eq!(
            cross_major_package("python", "3.15.2").as_deref(),
            Some("Python.Python.3.15")
        );
        assert_eq!(
            cross_major_package("php", "8.5.12").as_deref(),
            Some("PHP.PHP.8.5")
        );
        assert_eq!(
            cross_major_package("ruby", "4.0.9").as_deref(),
            Some("RubyInstallerTeam.Ruby.4.0")
        );
        assert_eq!(
            cross_major_package("dotnet", "10.0.13").as_deref(),
            Some("Microsoft.DotNet.SDK.10")
        );
        // 只有一个包的项不需要推导；脏版本号不能拼出命令参数
        assert_eq!(cross_major_package("go", "1.27.3"), None);
        assert_eq!(cross_major_package("rust", "1.99.1"), None);
        assert_eq!(cross_major_package("python", "garbage"), None);
        assert_eq!(cross_major_package("python", ""), None);
    }

    #[test]
    fn 语言工具链单列一组且不设最低版本() {
        let items = scan_items();
        let langs: Vec<&EnvItem> = items.iter().filter(|item| item.group == "language").collect();
        assert_eq!(langs.len(), 5, "语言工具链应有 5 项");
        assert!(
            langs
                .iter()
                .all(|item| item.min_version.is_empty() && !item.outdated),
            "语言项不该被判「版本过旧」，新旧交给最新稳定版查询"
        );
        for id in ["rust", "go", "dotnet", "php", "ruby"] {
            assert!(items.iter().any(|item| item.id == id), "体检结果缺 {id}");
        }
        assert!(!items.iter().any(|item| item.id == "bun"), "Bun 已按要求移除");
        assert_eq!(items.iter().filter(|item| item.group == "core").count(), 8);
    }

    /// 真实联网跑一次，验证外部数据源的格式没变。CI 默认跳过（可能没有外网）。
    /// 本地手动验证：`cargo test --release 实查最新稳定版 -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "需要访问外部数据源"]
    async fn 实查最新稳定版() {
        let value = latest_versions(true).await;
        let items = value
            .get("items")
            .and_then(Value::as_object)
            .expect("返回里要有 items");
        assert!(
            value.get("ok").and_then(Value::as_u64).unwrap_or(0) >= 5,
            "至少 5 个数据源要成功：{value}"
        );
        for (id, entry) in items {
            assert!(
                entry.get("latest").is_some() || entry.get("error").is_some(),
                "{id} 既没版本也没错误"
            );
        }
        for id in ["rust", "go", "python", "node", "dotnet", "php", "ruby"] {
            let latest = items
                .get(id)
                .and_then(|entry| entry.get("latest"))
                .and_then(Value::as_str)
                .unwrap_or("");
            assert!(
                latest.chars().next().map(|c| c.is_ascii_digit()).unwrap_or(false),
                "{id} 没拿到版本：{latest}"
            );
        }
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
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

    /// 本机自证：PATH 里没有 WindowsApps 时，winget 依旧能通过应用执行别名
    /// 被找到并真的跑起来（不只是「文件存在」）。依赖机器装了 App Installer，
    /// 所以标 #[ignore]，不进 CI；手工跑：
    /// `cargo test --release winget别名 -- --ignored`
    #[test]
    #[ignore = "依赖本机安装 winget"]
    fn winget别名能被找到并执行() {
        #[cfg(windows)]
        {
            let exe = winget_exe().expect("应当通过应用执行别名找到 winget.exe");
            let output = winget_command()
                .expect("winget_command 应当构造成功")
                .arg("--version")
                .output()
                .expect("winget --version 应当能执行");
            assert!(
                output.status.success(),
                "winget --version 失败：{}",
                String::from_utf8_lossy(&output.stderr)
            );
            println!("winget path = {}", exe.display());
            println!(
                "winget version = {}",
                String::from_utf8_lossy(&output.stdout).trim()
            );
        }
    }

    #[test]
    fn winget_已装且无可用升级不算失败() {
        // 实测 Go 已装 1.27.3 后再点「一键部署」，winget 返回 -1978335189
        // 和这段中文输出；以前被当成安装失败。
        let zh = "找到已安装的现有包。正在尝试升级已安装的包...\r\n\
                  找不到可用的升级。\r\n配置的源中没有可用的较新的包版本。";
        assert!(winget_no_op(zh), "中文 no-op 输出应当识别为成功");
        let en = "Found an existing package already installed. No applicable upgrade found.";
        assert!(winget_no_op(en), "英文 no-op 输出应当识别为成功");
        assert!(!winget_no_op("已安装程序哈希校验失败；下载被中断"));
        assert!(!winget_no_op("已安装"), "只说装了、没说没得升，不能判成功");
        assert!(!winget_no_op(""));
    }

    #[test]
    fn 大版本系列按数字倒序且过滤老版本() {
        // 实测 winget search --id Python.Python 的输出片段。注意 3.10 不能排在 3.9 后面。
        let lines = [
            "名称                           ID                                  版本",
            "-----------------------------------------------------------------------------",
            "Python Embeddable Package 3.14 Python.PythonEmbeddablePackage.3.14 3.14.6",
            "Python 2                       Python.Python.2                     2.7.18150",
            "Python 3.8                     Python.Python.3.8                   3.8.10",
            "Python 3.9                     Python.Python.3.9                   3.9.13",
            "Python 3.10                    Python.Python.3.10                  3.10.11",
            "Python 3.11                    Python.Python.3.11                  3.11.9",
            "Python 3.12                    Python.Python.3.12                  3.12.10",
            "Python 3.13                    Python.Python.3.13                  3.13.17",
            "Python 3.14                    Python.Python.3.14                  3.14.10",
        ];
        let versions = parse_series_versions(&lines.join("\n"), "Python.Python.", "python");
        assert_eq!(versions, vec!["3.14", "3.13", "3.12", "3.11", "3.10", "3.9"]);
    }

    #[test]
    fn 选大版本会推导出对应的_winget_包id() {
        assert_eq!(
            cross_major_package("python", "3.11").as_deref(),
            Some("Python.Python.3.11")
        );
        assert_eq!(
            cross_major_package("dotnet", "10").as_deref(),
            Some("Microsoft.DotNet.SDK.10")
        );
        assert!(cross_major_package("git", "2.45.0").is_none());
        assert!(cross_major_package("python", "3.11; rm -rf /").is_none());
    }
}