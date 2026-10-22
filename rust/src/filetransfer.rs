//! 文件快传与同步：基于已有 SSH exec 通道的传输引擎。
//!
//! ## 为什么走 exec 而不是 SFTP
//!
//! 本项目的 SSH 客户端是手写实现（`sshclient.rs`），只实现了 session 通道的
//! `exec` 请求。要走 SFTP 得再加一个 subsystem 通道并完整实现 `SSH_FXP`
//! 协议（OPEN/READ/WRITE/CLOSE/STAT…），工作量大且边界情况容易出错。
//!
//! 走 exec 的代价是**二进制不能直传** —— exec 的 stdout 是
//! `String::from_utf8_lossy`，二进制会被有损转换彻底破坏（一个 `\u{FFFD}`
//! 就代表原字节再也回不来了）。所以所有内容统一 base64。
//!
//! ## 四个硬约束（都来自 sshclient 的实现，不是猜的）
//!
//! 1. **stdout 是 String** → 远端必须用 ASCII（base64）把数据吐回来。
//! 2. **stdin 一次性写入**：`exec` 拿到 channel success 后把整个 stdin 打包发出，
//!    不支持流式喂。所以做不到「边读文件边发」，得整块备好在内存里再发。
//! 3. **单包 32KB**：`CHANNEL_MAX_PACKET`，写侧按这个切包；一块几百 KB 没问题。
//! 4. **deadline 默认是连接握手那个 20 秒**：一次往返传几百 KB，在慢链上很容易超时。
//!    所以这里全部走 `exec_with_deadline`，按调用类型分别给超时。
//!
//! 综合下来策略是**分块 + 每块一次 exec**：每块 256KB 原始字节（base64 后约 350KB），
//! 传完一块再传下一块。
//!
//! ## 为什么不打 tar 包
//!
//! 最初的写法是把整个目录在本地打成 tar、整体传过去再解开，看起来「一次搞定」。
//! 实际有三个坑：
//! - tar 是二进制，**必须**再套一层 base64 才能过 exec，等于数据两次膨胀；
//! - 整包意味着**没有 per-file 进展**，中途断开就全部重来；
//! - 增量同步本来就要先列两边的文件清单做比对，既然清单已经有了，按文件传是顺路的事。
//!
//! 所以改成逐文件传输：一个文件一个 Tar，文件自己再分块。多几次往返，换来
//! 「断在哪儿都知道、重跑只补差的那部分」。
//!
//! ## 远端命令的跨平台问题
//!
//! Linux 走 sh + coreutils（`base64` / `dd` / `stat` / `find`）；
//! Windows 的 SSH exec 默认跑的是 **cmd.exe**，每条命令都要显式包成
//! `powershell -NoProfile -NonInteractive -Command "…"`，
//! 并且整条命令**不能含换行**（cmd 扛不住）。两边按服务器 `kind` 分开生成。

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// 单次传输的块大小（**未** base64 编码前的原始字节数）。
///
/// 256KB 原始 → base64 后约 350KB。再大（比如 4MB）单次往返的峰值内存翻十几倍，
/// 多台并发时更明显；再小（比如 32KB）往返次数暴增，每个 SSH 往返都摊一次 RTT，
/// 传 100MB 要几千次往返，慢得没法用。
pub const CHUNK_SIZE: usize = 256 * 1024;

/// 列表／建目录这类"短命令"的超时。
const SHORT_TIMEOUT: Duration = Duration::from_secs(30);
/// 单个块的超时。留得宽 —— 慢链上 350KB 也可能要几十秒。
const CHUNK_TIMEOUT: Duration = Duration::from_secs(180);
/// 列出整个远端目录树（递归）的超时。
const LIST_TIMEOUT: Duration = Duration::from_secs(60);
/// 结果里最多保留多少条说明。大目录动辄上万个文件，全塞进 JSON 会把响应撑爆。
const MAX_NOTES: usize = 200;

// ===================================================================
// 路径处理
// ===================================================================

/// 判断一个字符能不能安全地出现在远端路径里。
///
/// 这些字符最终会被拼进远端的 shell 命令 / PowerShell 脚本。
/// 收紧白名单比"想方设法转义"可靠 —— 真实世界的路径几乎不会用到下面任何一个被拒的字符，
/// 而漏掉一个就可能变成远端命令注入。
///
/// `is_windows` 为真时额外禁掉 `$` 与 `` ` ``：PowerShell 双引号串会做变量替换，
/// 而 cmd 即使在引号里也照样展开 `%VAR%`，所以 `%` 两边都禁。
fn path_char_ok(ch: char) -> bool {
    if ch.is_alphanumeric() {
        return true;
    }
    if (ch as u32) > 0x7f {
        // 中文、日文等非 ASCII 路径 —— 用户目录里很常见，放行
        return true;
    }
    // 白名单之外的 ASCII 一律拒。这里没有列到的而又是危险的：`$`（两边都做替换）、
    // `;` `|` `&` `>` `<`（shell 拼接）、`` ` `` `"`（PowerShell 字符串）、
    // `%`（cmd 即使在引号里也照样展开 %VAR%）、`*` `?`（通配符）。
    matches!(
        ch,
        '/' | '\\'
            | '.'
            | '_'
            | '-'
            | ':'
            | ','
            | '@'
            | '+'
            | '='
            | '~'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '#'
            | ' '
            | '\''
    )
}

/// 校验远端路径可以把控。
pub fn validate_remote_path(path: &str) -> Result<String, String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return Err("远端路径为空".into());
    }
    if trimmed.len() > 4096 {
        return Err("远端路径太长了".into());
    }
    for ch in trimmed.chars() {
        if !path_char_ok(ch) {
            return Err(format!(
                "路径里含有不能安全传输的字符 {ch:?}，请改名后再传"
            ));
        }
        if ch == '\n' || ch == '\r' {
            return Err("路径里不能有换行".into());
        }
    }
    Ok(trimmed.to_string())
}

/// 给远端命令用的带引号路径。
///
/// Linux → 单引号包裹，内部单引号按 POSIX 写成 `'\''`；
/// Windows → 双引号包裹（外层 cmd 用双引号分组），内容已在
/// [`validate_remote_path`] 里排除了 `$`、`` ` ``、`"` 这些会被吃掉或截断的字符。
pub fn quote_path(path: &str, is_windows: bool) -> Result<String, String> {
    let trimmed = validate_remote_path(path)?;
    if is_windows {
        Ok(format!("\"{trimmed}\""))
    } else {
        Ok(format!("'{}'", trimmed.replace('\'', "'\\''")))
    }
}

/// 拼接远端目录与相对文件名。
///
/// 统一用 `/` —— Windows 的 PowerShell 与 .NET API 都认正斜杠，
/// 按平台切换分隔符只会让两边的清单对不上。
pub fn join_remote(base: &str, relative: &str) -> String {
    let base = base.trim_end_matches(['/', '\\']);
    let relative = relative.trim().trim_start_matches(['/', '\\']);
    if base.is_empty() {
        return relative.to_string();
    }
    if relative.is_empty() {
        return base.to_string();
    }
    format!("{base}/{relative}")
}

/// 取远端路径的父目录（用于界面上的「上一级」）。
pub fn parent_of(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(0) => "/".to_string(),
        Some(index) => trimmed[..index].to_string(),
        None => "".to_string(),
    }
}

// ===================================================================
// 远端命令模板
//
// 全部用 `__NM_XXX__` 这样的占位符，而不是 format! 的花括号：
// PowerShell 脚本里到处是 { }（if 块、数组初始化），写进 Rust 的格式串
// 必须逐个转义成 {{ }}，漏一个就是编译期炸或者运行时命令缺字。
// 占位符替换没有这个问题。
// ===================================================================

/// 把模板里的占位符换成实际值。
fn fill(template: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = template.to_string();
    for (key, value) in pairs {
        out = out.replace(key, value);
    }
    out
}

/// 包成远端可执行的 PowerShell（Windows 侧）。
///
/// 必须显式走 `powershell -Command`，因为 SSH 的 exec 在 Windows 上默认进 cmd.exe。
/// 整条命令**不能含换行**，否则 cmd 会把后半句当成新命令 —— 模板里统一用 `; ` 分隔。
fn wrap_powershell(script: &str) -> String {
    let escaped = script.replace('"', "`\"");
    format!(
        "powershell -NoProfile -NonInteractive -Command \"$OutputEncoding = [Console]::OutputEncoding = [Text.Encoding]::UTF8; {escaped}\""
    )
}

/// 把相对路径解析成 .NET 认的绝对路径。
///
/// `[IO.File]::ReadAllBytes` 这类 API 用的是**进程当前目录**，不是 PowerShell 的
/// 当前位置（两者在 PS 里可以不一样）。所以凡是用 .NET 读写文件的地方，
/// 都要先过这道转换。
fn ps_resolve(variable: &str, quoted: &str) -> String {
    fill(
        "$__V__ = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath(__Q__); ",
        &[("__V__", variable), ("__Q__", quoted)],
    )
}

/// Linux：列目录下所有文件的命令行。
const LINUX_LIST: &str = "cd __Q__ && find . -type f -printf '%s\\t%T@\\t%P\\n' 2>/dev/null";

/// Linux：`find -printf` 是 GNU 扩展，BusyBox 的 find 没有 —— 这条是退路。
const LINUX_LIST_FALLBACK: &str =
    "cd __Q__ && find . -type f -exec stat -c '%s\\t%Y\\t%n' {} + 2>/dev/null";

/// Windows：递归列文件。用 `-LiteralPath` 而不是 `-Path`，避免路径里的方括号被当通配符。
const WINDOWS_LIST: &str = concat!(
    "__R__",
    "Get-ChildItem -LiteralPath $r -Recurse -File -ErrorAction SilentlyContinue | ForEach-Object { ",
    "$rel = $_.FullName.Substring($r.Length).TrimStart('\\','/'); ",
    "$ts = [int][math]::Floor(($_.LastWriteTimeUtc - ([datetime]'1970-01-01')).TotalSeconds); ",
    "Write-Output ([string]$_.Length + [string][char]9 + [string]$ts + [string][char]9 + $rel) }",
);

/// Linux 单块下载。`dd` 跳到偏移处读一块，管道的另一端直接 base64。
const LINUX_READ_CHUNK: &str =
    "dd if=__Q__ bs=__SIZE__ skip=__SKIP__ count=1 2>/dev/null | base64 -w 0";

/// Windows 单块下载。不能用 `Get-Content -Encoding Byte` —— PS 7 已经把这个参数砍了。
const WINDOWS_READ_CHUNK: &str = concat!(
    "__R__",
    "$f = [IO.File]::OpenRead($r); ",
    "$null = $f.Seek(__OFFSET__, [IO.SeekOrigin]::Begin); ",
    "$buf = New-Object byte[] __SIZE__; ",
    "$n = $f.Read($buf, 0, __SIZE__); ",
    "$f.Close(); ",
    "if ($n -gt 0) { [Console]::Out.Write([Convert]::ToBase64String($buf, 0, $n)) }",
);

/// Linux 单块上传：stdin 是 base64 文本，解码后写文件。第一块截断，其余追加。
const LINUX_WRITE_CHUNK: &str = "__DECODE__ __REDIRECT__ __Q__";

/// Windows 单块上传。`Append` 是 filemode，第一块用 `Create` 才能让文件重新来过
/// —— 否则中断后重传会拼出「半个旧文件 + 半个新文件」。
const WINDOWS_WRITE_CHUNK: &str = concat!(
    "__R__",
    "$parent = [IO.Path]::GetDirectoryName($r); ",
    "if ($parent -and !(Test-Path -LiteralPath $parent)) { New-Item -ItemType Directory -Force -Path $parent | Out-Null }; ",
    "$b = [Convert]::FromBase64String([Console]::In.ReadToEnd().Trim()); ",
    "$f = [IO.File]::Open($r, [IO.FileMode]::__MODE__, [IO.FileAccess]::Write); ",
    "$f.Write($b, 0, $b.Length); ",
    "$f.Close(); ",
    "[Console]::Out.Write('NMOK:' + $b.Length)",
);

// ===================================================================
// 远端执行
// ===================================================================

/// 判断这台服务器是 Windows 还是 Linux。
///
/// 靠 `SshServerProfile.kind`（linux/windows）。老配置没有这个字段时一律当 Linux，
/// 与项目其它地方的约定一致。
pub fn is_windows_server(profile: &crate::models::SshServerProfile) -> bool {
    profile.kind == "windows"
}

/// 在远端跑一条命令。
///
/// `script` 是**平台原生**脚本：Linux 直接给 sh，Windows 给 PowerShell 语句
/// （函数内部负责套 `powershell -Command` 外壳）。
async fn run_remote(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    script: &str,
    stdin: &[u8],
    timeout: Duration,
) -> Result<String, String> {
    let command = if is_windows {
        wrap_powershell(script)
    } else {
        script.to_string()
    };
    let client = runner.connected().await?;
    let result = client.exec_with_deadline(&command, stdin, timeout).await?;
    if result.status != 0 {
        let detail = if !result.stderr.trim().is_empty() {
            result.stderr.trim().to_string()
        } else {
            result.stdout.trim().to_string()
        };
        // PowerShell 的错误是一大段带缩进的红字，取最有信息的那行
        let message = detail
            .lines()
            .map(|line| line.trim())
            .find(|line| !line.is_empty())
            .unwrap_or("");
        if message.is_empty() {
            return Err(format!("远端命令失败（退出码 {}）", result.status));
        }
        return Err(format!("远端命令失败：{}", truncate(message)));
    }
    Ok(result.stdout.replace("\r\n", "\n"))
}

fn truncate(message: &str) -> String {
    if message.chars().count() > 300 {
        message.chars().take(300).collect()
    } else {
        message.to_string()
    }
}

/// 建远端目录（含父目录）。
async fn ensure_remote_dir(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_dir: &str,
) -> Result<(), String> {
    let quoted = quote_path(remote_dir, is_windows)?;
    let script = if is_windows {
        fill(
            "New-Item -ItemType Directory -Force -Path __Q__ | Out-Null",
            &[("__Q__", &quoted)],
        )
    } else {
        fill("mkdir -p __Q__", &[("__Q__", &quoted)])
    };
    run_remote(runner, is_windows, &script, &[], SHORT_TIMEOUT).await?;
    Ok(())
}

/// 删远端文件（不存在也不算错 —— 镜像模式下目标可能本来就没这个文件）。
async fn remove_remote_file(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_path: &str,
) -> Result<(), String> {
    let quoted = quote_path(remote_path, is_windows)?;
    let script = if is_windows {
        fill(
            "__R__Remove-Item -LiteralPath $r -Force -ErrorAction SilentlyContinue",
            &[("__R__", &ps_resolve("r", &quoted))],
        )
    } else {
        fill("rm -f __Q__", &[("__Q__", &quoted)])
    };
    run_remote(runner, is_windows, &script, &[], SHORT_TIMEOUT).await?;
    Ok(())
}

// ===================================================================
// 清单采集
// ===================================================================

/// 本地目录下的文件清单：`(相对路径, 字节数, mtime 秒)`。
///
/// 相对路径统一用 `/` 分隔 —— 远端清单也是这个形态，比对才对得上。
pub fn scan_local_dir(root: &Path) -> Result<Vec<(String, u64, i64)>, String> {
    let mut out = Vec::new();
    if !root.is_dir() {
        return Err(format!("{} 不是目录", root.display()));
    }
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|error| format!("读取目录 {} 失败：{error}", dir.display()))?;
        for entry in entries.flatten() {
            let path = entry.path();
            let meta = match entry.metadata() {
                Ok(meta) => meta,
                // 符号链接指向的目标不存在时会报这个错，跳过而不是整体失败
                Err(_) => continue,
            };
            if meta.is_dir() {
                stack.push(path);
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            out.push((
                relative.to_string_lossy().replace('\\', "/"),
                meta.len(),
                mtime_of(&meta),
            ));
        }
    }
    // 排序：两次扫描顺序一致，增量比对才靠得住
    out.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(out)
}

/// 把远端形态的相对路径（`/` 分隔）落成本机路径。
///
/// `Path::join` 在 Windows 上其实也认 `/`，但显式换成主分隔符能让报错信息里的
/// 路径与用户系统的实际形态一致（`C://a//b` 而不是 `C://a/b`）。
fn local_join(root: &Path, relative: &str) -> PathBuf {
    root.join(relative.replace('/', std::path::MAIN_SEPARATOR_STR))
}

fn mtime_of(meta: &std::fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0)
}

/// 解析 `size<TAB>mtime<TAB>path` 形态的远端清单。
///
/// 用制表符分隔是刻意的 —— 路径里可能有空格，但基本不会有制表符。
pub fn parse_remote_listing(text: &str) -> Vec<(String, u64, i64)> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        if trimmed.is_empty() {
            continue;
        }
        let mut parts = trimmed.splitn(3, '\t');
        let (Some(size_text), Some(mtime_text), Some(path)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let Ok(size) = size_text.trim().parse::<u64>() else {
            continue;
        };
        let mtime = parse_epoch(mtime_text.trim());
        // 退路（`find -exec stat`）输出的是 `./rel` 形态，去掉前缀
        let path = path.trim().replace('\\', "/");
        let path = path.strip_prefix("./").unwrap_or(&path).to_string();
        if path.is_empty() {
            continue;
        }
        out.push((path, size, mtime));
    }
    out.sort_by(|left, right| left.0.cmp(&right.0));
    out
}

/// `%T@` 输出的是小数形式（如 `1700000000.1234567890`），`stat -c %Y` 是整数。
/// 两种都要认，认不出来按 0 处理（0 在比对里代表「未知 → 一律重传」）。
fn parse_epoch(text: &str) -> i64 {
    if let Ok(value) = text.parse::<i64>() {
        return value;
    }
    if let Ok(value) = text.parse::<f64>() {
        return value as i64;
    }
    0
}

/// 判断一个文件是否需要重传。
///
/// 规则：**大小不同必传**；大小相同再看修改时间，远端不比本地新才传。
/// 只比 mtime 会漏掉「内容改了大小没变」的编辑；只比 size 会让 mtime 乱跳的机器
/// 每次都全量重传。两个都比才稳。
///
/// mtime 为 0 表示「某一端读不出时间」→ 按未知处理，直接传。
pub fn needs_transfer(local_size: u64, local_mtime: i64, remote_size: u64, remote_mtime: i64) -> bool {
    if local_size != remote_size {
        return true;
    }
    if local_mtime <= 0 || remote_mtime <= 0 {
        return true;
    }
    remote_mtime < local_mtime
}

/// 递归列出远端目录下的所有文件。
async fn list_remote_files(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_dir: &str,
) -> Result<Vec<(String, u64, i64)>, String> {
    let quoted = quote_path(remote_dir, is_windows)?;
    let script = if is_windows {
        fill(WINDOWS_LIST, &[("__R__", &ps_resolve("r", &quoted))])
    } else {
        LINUX_LIST.replace("__Q__", &quoted)
    };
    match run_remote(runner, is_windows, &script, &[], LIST_TIMEOUT).await {
        Ok(text) => Ok(parse_remote_listing(&text)),
        Err(error) if !is_windows => {
            // GNU find 的 -printf 在 BusyBox / toybox 上不支持，换 -exec stat 再试一次
            let fallback = LINUX_LIST_FALLBACK.replace("__Q__", &quoted);
            match run_remote(runner, is_windows, &fallback, &[], LIST_TIMEOUT).await {
                Ok(text) => Ok(parse_remote_listing(&text)),
                Err(_) => Err(error),
            }
        }
        Err(error) => Err(error),
    }
}

// ===================================================================
// 单文件传输
// ===================================================================

/// 上传一个本地文件的字节到远端路径。
async fn upload_file_bytes(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_path: &str,
    data: &[u8],
) -> Result<u64, String> {
    let quoted = quote_path(remote_path, is_windows)?;
    let chunks: Vec<&[u8]> = data.chunks(CHUNK_SIZE).collect();
    // 空文件也要在远端建出来： chunks(0) 是空数组，补一次空写
    let rounds = if chunks.is_empty() {
        vec![&[][..]]
    } else {
        chunks.clone()
    };

    for (index, chunk) in rounds.iter().enumerate() {
        let encoded = base64_encode(chunk);
        let script = if is_windows {
            fill(
                WINDOWS_WRITE_CHUNK,
                &[
                    ("__R__", &ps_resolve("r", &quoted)),
                    (
                        "__MODE__",
                        if index == 0 { "Create" } else { "Append" },
                    ),
                ],
            )
        } else {
            // Linux 的 `>` 不会自动建父目录，目录传输时子目录是一个个新建的，
            // 所以在第一块的写命令前面挂一条 mkdir -p —— 同一个 exec 里做完，
            // 不额外多一次往返。`&&` 保证 mkdir 失败时不会写出一个残缺文件。
            let mkdir = match (index, remote_parent(remote_path, false)) {
                (0, Some(parent)) => format!("mkdir -p {} && ", quote_path(&parent, false)?),
                _ => String::new(),
            };
            format!(
                "{mkdir}{}",
                fill(
                    LINUX_WRITE_CHUNK,
                    &[
                        ("__DECODE__", "base64 -d"),
                        ("__REDIRECT__", if index == 0 { ">" } else { ">>" }),
                        ("__Q__", &quoted),
                    ],
                )
            )
        };
        run_remote(runner, is_windows, &script, encoded.as_bytes(), CHUNK_TIMEOUT).await?;
    }
    Ok(data.len() as u64)
}

/// 远端文件大小。读不出来按 0 处理。
async fn remote_file_size(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_path: &str,
) -> Result<u64, String> {
    let quoted = quote_path(remote_path, is_windows)?;
    let script = if is_windows {
        fill(
            "__R__(Get-Item -LiteralPath $r -ErrorAction SilentlyContinue).Length",
            &[("__R__", &ps_resolve("r", &quoted))],
        )
    } else {
        fill("stat -c %s __Q__", &[("__Q__", &quoted)])
    };
    let text = run_remote(runner, is_windows, &script, &[], SHORT_TIMEOUT).await?;
    Ok(text.trim().lines().last().and_then(|l| l.trim().parse().ok()).unwrap_or(0))
}

/// 远端路径是否存在、是文件还是目录。
async fn remote_stat(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_path: &str,
) -> Result<Option<(bool, u64)>, String> {
    let quoted = quote_path(remote_path, is_windows)?;
    // 输出 `d<TAB>size`（目录）或 `f<TAB>size`（文件）或空（不存在）
    let script = if is_windows {
        fill(
            "__R__$i = Get-Item -LiteralPath $r -ErrorAction SilentlyContinue; if ($i) { $k = if ($i.PSIsContainer) {'d'} else {'f'}; $s = if ($i.PSIsContainer) {0} else {$i.Length}; [Console]::Out.Write($k + [string][char]9 + [string]$s) }",
            &[("__R__", &ps_resolve("r", &quoted))],
        )
    } else {
        fill(
            "if [ -d __Q__ ]; then printf 'd\\t0\\n'; elif [ -f __Q__ ]; then printf 'f\\t%s\\n' \"$(stat -c %s __Q__)\"; fi",
            &[("__Q__", &quoted)],
        )
    };
    let text = run_remote(runner, is_windows, &script, &[], SHORT_TIMEOUT).await?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    let mut parts = trimmed.split('\t');
    let kind = parts.next().unwrap_or("");
    let size = parts.next().and_then(|s| s.trim().parse().ok()).unwrap_or(0);
    Ok(Some((kind == "d", size)))
}

/// 从远端读一个文件的全部内容。
async fn download_file_bytes(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_path: &str,
) -> Result<Vec<u8>, String> {
    let size = remote_file_size(runner, is_windows, remote_path).await?;
    if size == 0 {
        return Ok(Vec::new());
    }
    let quoted = quote_path(remote_path, is_windows)?;
    let total = size.div_ceil(CHUNK_SIZE as u64);
    let mut out = Vec::with_capacity(size as usize);

    for index in 0..total {
        let script = if is_windows {
            fill(
                WINDOWS_READ_CHUNK,
                &[
                    ("__R__", &ps_resolve("r", &quoted)),
                    ("__OFFSET__", &(index * CHUNK_SIZE as u64).to_string()),
                    ("__SIZE__", &CHUNK_SIZE.to_string()),
                ],
            )
        } else {
            fill(
                LINUX_READ_CHUNK,
                &[
                    ("__Q__", &quoted),
                    ("__SIZE__", &CHUNK_SIZE.to_string()),
                    ("__SKIP__", &index.to_string()),
                ],
            )
        };
        let text = run_remote(runner, is_windows, &script, &[], CHUNK_TIMEOUT).await?;
        let trimmed = text.trim();
        if trimmed.is_empty() {
            break;
        }
        let chunk = base64_decode(trimmed)?;
        out.extend_from_slice(&chunk);
        if chunk.len() < CHUNK_SIZE {
            break;
        }
    }
    Ok(out)
}

// ===================================================================
// 目录传输
// ===================================================================

/// 推一个本地目录到远端。
///
/// `incremental` 为真时先拉远端清单做比对，只传变化的文件；
/// `mirror` 为真时额外删掉远端多出来的文件。
async fn push_directory(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    local_dir: &Path,
    remote_dir: &str,
    incremental: bool,
    mirror: bool,
) -> Result<TransferStats, String> {
    let mut stats = TransferStats::default();
    let local_files = scan_local_dir(local_dir)?;
    ensure_remote_dir(runner, is_windows, remote_dir).await?;

    let remote_files = if incremental || mirror {
        list_remote_files(runner, is_windows, remote_dir)
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // 远端清单做成索引，避免每个文件都线性扫一遍
    let remote_index: Vec<(&str, u64, i64)> = remote_files
        .iter()
        .map(|(path, size, mtime)| (path.as_str(), *size, *mtime))
        .collect();

    for (relative, _size, mtime) in &local_files {
        let local_path = local_join(local_dir, relative);
        let data = std::fs::read(&local_path)
            .map_err(|error| format!("读取 {} 失败：{error}", local_path.display()))?;
        let actual_size = data.len() as u64;

        if incremental {
            let skip = remote_index
                .iter()
                .find(|(path, _, _)| *path == relative.as_str())
                .map(|(_, remote_size, remote_mtime)| {
                    !needs_transfer(actual_size, *mtime, *remote_size, *remote_mtime)
                })
                .unwrap_or(false);
            if skip {
                stats.skipped += 1;
                stats.push_note(format!("跳过（未变化）：{relative}"));
                continue;
            }
        }
        let remote_path = join_remote(remote_dir, relative);
        upload_file_bytes(runner, is_windows, &remote_path, &data).await?;
        stats.files += 1;
        stats.bytes += actual_size;
        stats.push_note(format!("已同步：{relative}（{}）", human_bytes(actual_size)));
    }

    if mirror {
        for (path, _, _) in &remote_files {
            if local_files.iter().any(|(local, _, _)| path == local) {
                continue;
            }
            let full = join_remote(remote_dir, path);
            if remove_remote_file(runner, is_windows, &full).await.is_err() {
                stats.push_note(format!("删除失败（跳过）：{path}"));
                continue;
            }
            stats.removed += 1;
            stats.push_note(format!("已删除（远端多余）：{path}"));
        }
    }
    Ok(stats)
}

/// 拉一个远端目录到本机。
async fn pull_directory(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_dir: &str,
    local_dir: &Path,
    incremental: bool,
    mirror: bool,
) -> Result<TransferStats, String> {
    let mut stats = TransferStats::default();
    let remote_files = list_remote_files(runner, is_windows, remote_dir).await?;
    let local_files = if incremental || mirror {
        scan_local_dir(local_dir).unwrap_or_default()
    } else {
        Vec::new()
    };
    std::fs::create_dir_all(local_dir)
        .map_err(|error| format!("创建 {} 失败：{error}", local_dir.display()))?;

    for (relative, remote_size, remote_mtime) in &remote_files {
        let target = local_join(local_dir, relative);
        if incremental {
            if let Ok(meta) = std::fs::metadata(&target) {
                if !needs_transfer(
                    *remote_size,
                    *remote_mtime,
                    meta.len(),
                    mtime_of(&meta),
                ) {
                    stats.skipped += 1;
                    stats.push_note(format!("跳过（未变化）：{relative}"));
                    continue;
                }
            }
        }
        let remote_path = join_remote(remote_dir, relative);
        let data = download_file_bytes(runner, is_windows, &remote_path).await?;
        if let Some(parent) = target.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&target, &data)
            .map_err(|error| format!("写入 {} 失败：{error}", target.display()))?;
        stats.files += 1;
        stats.bytes += data.len() as u64;
        stats.push_note(format!("已下载：{relative}（{}）", human_bytes(data.len() as u64)));
    }

    if mirror {
        for (relative, _, _) in &local_files {
            if remote_files.iter().any(|(remote, _, _)| remote == relative) {
                continue;
            }
            let target = local_join(local_dir, relative);
            if std::fs::remove_file(&target).is_err() {
                stats.push_note(format!("删除失败（跳过）：{relative}"));
                continue;
            }
            stats.removed += 1;
            stats.push_note(format!("已删除（本地多余）：{relative}"));
        }
    }
    Ok(stats)
}

// ===================================================================
// 对外入口
// ===================================================================

/// 一次传输的统计结果。
#[derive(Debug, Clone, Default)]
pub struct TransferStats {
    /// 实际传输的字节数（原始字节，不是 base64 后的）。
    pub bytes: u64,
    pub files: u32,
    /// 跳过的文件个数（增量模式下大小与修改时间都没变的）。
    pub skipped: u32,
    /// 删除的文件个数（镜像模式）。
    pub removed: u32,
    pub notes: Vec<String>,
}

impl TransferStats {
    /// 说明最多留 [`MAX_NOTES`] 条 —— 上万个文件的目录全塞进 JSON 会把响应撑到几十 MB。
    fn push_note(&mut self, note: String) {
        if self.notes.len() < MAX_NOTES {
            self.notes.push(note);
        } else if self.notes.len() == MAX_NOTES {
            self.notes.push("…其余文件的明细已省略".to_string());
        }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "bytes": self.bytes,
            "bytesText": human_bytes(self.bytes),
            "files": self.files,
            "skipped": self.skipped,
            "removed": self.removed,
            "notes": self.notes,
        })
    }
}

fn runner_for(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<crate::ssh::Runner, String> {
    crate::ssh::Runner::new(profile, credential)
}

/// 浏览远端目录的一层（给界面做目录导航用）。
pub async fn browse_remote(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    remote_path: &str,
) -> Result<Value, String> {
    let is_windows = is_windows_server(profile);
    let path = validate_remote_path(remote_path)?;
    let mut runner = runner_for(profile, credential)?;
    let result = browse_with_runner(&mut runner, is_windows, &path).await;
    runner.shutdown().await;
    result
}

async fn browse_with_runner(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_path: &str,
) -> Result<Value, String> {
    let quoted = quote_path(remote_path, is_windows)?;
    // 输出 `kind<TAB>size<TAB>mtime<TAB>name`，每行一项
    let script = if is_windows {
        fill(
            "__R__Get-ChildItem -LiteralPath $r -Force -ErrorAction SilentlyContinue | ForEach-Object { ",
            &[("__R__", &ps_resolve("r", &quoted))],
        ) + concat!(
            "$k = if ($_.PSIsContainer) {'d'} else {'f'}; ",
            "$s = if ($_.PSIsContainer) {0} else {$_.Length}; ",
            "$ts = [int][math]::Floor(($_.LastWriteTimeUtc - ([datetime]'1970-01-01')).TotalSeconds); ",
            "Write-Output ($k + [string][char]9 + [string]$s + [string][char]9 + [string]$ts + [string][char]9 + $_.Name) }",
        )
    } else {
        fill(
            "cd __Q__ && find . -maxdepth 1 -mindepth 1 -printf '%y\\t%s\\t%T@\\t%f\\n' 2>/dev/null",
            &[("__Q__", &quoted)],
        )
    };
    let text = run_remote(runner, is_windows, &script, &[], LIST_TIMEOUT).await?;
    Ok(json!({
        "path": remote_path,
        "parent": parent_of(remote_path),
        "entries": parse_browse(&text),
    }))
}

fn parse_browse(text: &str) -> Vec<Value> {
    let mut out = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_end_matches(['\r', '\n']);
        let mut parts = trimmed.splitn(4, '\t');
        let (Some(kind), Some(size), Some(mtime), Some(name)) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if name.trim().is_empty() {
            continue;
        }
        out.push(json!({
            "kind": if kind.trim() == "d" { "dir" } else { "file" },
            "name": name,
            "size": size.trim().parse::<u64>().unwrap_or(0),
            "mtime": parse_epoch(mtime.trim()),
        }));
    }
    out.sort_by_key(|item| {
        (
            item.get("kind").and_then(|v| v.as_str()) != Some("dir"),
            item.get("name").and_then(|v| v.as_str()).unwrap_or("").to_lowercase(),
        )
    });
    out
}

/// 上传本机文件或目录到远端。
///
/// 本地是目录就走目录逻辑，是文件就走单文件逻辑 —— 不让用户在界面上再选一次。
pub async fn upload(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    local_path: &Path,
    remote_path: &str,
    sync: bool,
    mirror: bool,
) -> Result<TransferStats, String> {
    let is_windows = is_windows_server(profile);
    let mut runner = runner_for(profile, credential)?;
    let result = upload_with_runner(&mut runner, is_windows, local_path, remote_path, sync, mirror).await;
    runner.shutdown().await;
    result
}

async fn upload_with_runner(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    local_path: &Path,
    remote_path: &str,
    sync: bool,
    mirror: bool,
) -> Result<TransferStats, String> {
    let mut stats = TransferStats::default();
    if local_path.is_dir() {
        return push_directory(runner, is_windows, local_path, remote_path, sync, mirror).await;
    }
    if !local_path.is_file() {
        return Err(format!("{} 不存在", local_path.display()));
    }
    let data = std::fs::read(local_path)
        .map_err(|error| format!("读取 {} 失败：{error}", local_path.display()))?;

    // 远端路径给的是目录时（比如以 / 结尾或已存在），落到该目录下的同名文件
    let target = match remote_stat(runner, is_windows, remote_path).await? {
        Some((true, _)) => join_remote(remote_path, &file_name_of(local_path)),
        _ => validate_remote_path(remote_path)?,
    };
    if let Some(parent) = remote_parent(&target, is_windows) {
        ensure_remote_dir(runner, is_windows, &parent).await?;
    }

    let sent = upload_file_bytes(runner, is_windows, &target, &data).await?;
    stats.bytes = sent;
    stats.files = 1;
    stats.push_note(format!(
        "已上传：{} → {}（{}）",
        file_name_of(local_path),
        target,
        human_bytes(sent)
    ));
    Ok(stats)
}

fn file_name_of(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string())
}

fn remote_parent(path: &str, is_windows: bool) -> Option<String> {
    let trimmed = path.trim_end_matches(['/', '\\']);
    let index = trimmed.rfind(['/', '\\'])?;
    let parent = if index == 0 {
        "/"
    } else {
        &trimmed[..index]
    };
    // Windows 的 `C:` 这类盘符根不要当成目录去 mkdir
    if is_windows && parent.len() <= 2 && parent.ends_with(':') {
        return None;
    }
    if parent.is_empty() {
        return None;
    }
    Some(parent.to_string())
}

/// 从远端下载文件或目录到本机。
pub async fn download(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    remote_path: &str,
    local_path: &Path,
    sync: bool,
    mirror: bool,
) -> Result<TransferStats, String> {
    let is_windows = is_windows_server(profile);
    let mut runner = runner_for(profile, credential)?;
    let result =
        download_with_runner(&mut runner, is_windows, remote_path, local_path, sync, mirror).await;
    runner.shutdown().await;
    result
}

async fn download_with_runner(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_path: &str,
    local_path: &Path,
    sync: bool,
    mirror: bool,
) -> Result<TransferStats, String> {
    let mut stats = TransferStats::default();
    let Some((is_dir, _)) = remote_stat(runner, is_windows, remote_path).await? else {
        return Err(format!("远端 {remote_path} 不存在"));
    };

    if is_dir {
        return pull_directory(runner, is_windows, remote_path, local_path, sync, mirror).await;
    }

    // 目标给的是已存在的目录时，落到该目录下的同名文件
    let target = if local_path.is_dir() {
        local_path.join(file_name_of(Path::new(remote_path)))
    } else {
        local_path.to_path_buf()
    };
    if sync {
        if let Ok(meta) = std::fs::metadata(&target) {
            let size = remote_file_size(runner, is_windows, remote_path).await?;
            // 远端 mtime 要自己问一次：拉回来再判断就晚了一步，白传数据
            let remote_mtime = remote_mtime_of(runner, is_windows, remote_path).await?;
            if !needs_transfer(size, remote_mtime, meta.len(), mtime_of(&meta)) {
                stats.skipped = 1;
                stats.push_note("跳过（本机已是最新）".to_string());
                return Ok(stats);
            }
        }
    }
    let data = download_file_bytes(runner, is_windows, remote_path).await?;
    if let Some(parent) = target.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    std::fs::write(&target, &data)
        .map_err(|error| format!("写入 {} 失败：{error}", target.display()))?;
    stats.bytes = data.len() as u64;
    stats.files = 1;
    stats.push_note(format!(
        "已下载：{remote_path} → {}（{}）",
        target.display(),
        human_bytes(stats.bytes)
    ));
    Ok(stats)
}

async fn remote_mtime_of(
    runner: &mut crate::ssh::Runner,
    is_windows: bool,
    remote_path: &str,
) -> Result<i64, String> {
    let quoted = quote_path(remote_path, is_windows)?;
    let script = if is_windows {
        fill(
            "__R__$i = Get-Item -LiteralPath $r -ErrorAction SilentlyContinue; if ($i) { [Console]::Out.Write([int][math]::Floor(($i.LastWriteTimeUtc - ([datetime]'1970-01-01')).TotalSeconds)) }",
            &[("__R__", &ps_resolve("r", &quoted))],
        )
    } else {
        fill("stat -c %Y __Q__", &[("__Q__", &quoted)])
    };
    let text = run_remote(runner, is_windows, &script, &[], SHORT_TIMEOUT).await?;
    Ok(parse_epoch(text.trim().lines().last().unwrap_or("0").trim()))
}

/// 批量推到多台服务器。
///
/// **并发但有上限**：一次开太多条 SSH 连接会打满本机的 fd，也容易被远端 sshd 的
/// `MaxSessions` 挡掉。默认一批 4 台。
pub async fn push_to_many(
    targets: &[(crate::models::SshServerProfile, String)],
    local_path: &Path,
    remote_path: &str,
    sync: bool,
    mirror: bool,
) -> Result<Value, String> {
    let mut results: Vec<Value> = Vec::new();
    // Profile 里含私钥等敏感串，套一层 tokio 任务前先把要用的字段摘出来，
    // 免得错误信息里把整份配置打出来。
    for chunk in targets.chunks(4) {
        let mut handles = Vec::new();
        for (profile, credential) in chunk {
            let local = local_path.to_path_buf();
            let remote = remote_path.to_string();
            let profile = profile.clone();
            let credential = credential.clone();
            handles.push(tokio::spawn(async move {
                let outcome =
                    upload(&profile, &credential, &local, &remote, sync, mirror).await;
                (profile.name.clone(), profile.profile_id.clone(), outcome)
            }));
        }
        for handle in handles {
            match handle.await {
                Ok((name, profile_id, outcome)) => results.push(json!({
                    "profileId": profile_id,
                    "name": name,
                    "ok": outcome.is_ok(),
                    "detail": match &outcome {
                        Ok(stats) => format!(
                            "{} 个文件，{}，跳过 {}",
                            stats.files,
                            human_bytes(stats.bytes),
                            stats.skipped
                        ),
                        Err(error) => error.clone(),
                    },
                    "bytes": outcome.as_ref().map(|s| s.bytes).unwrap_or(0),
                    "files": outcome.as_ref().map(|s| s.files).unwrap_or(0),
                    "skipped": outcome.as_ref().map(|s| s.skipped).unwrap_or(0),
                })),
                Err(error) => results.push(json!({
                    "ok": false,
                    "detail": format!("任务异常：{error}"),
                })),
            }
        }
    }
    let succeeded = results
        .iter()
        .filter(|item| item.get("ok").and_then(|v| v.as_bool()) == Some(true))
        .count();
    Ok(json!({
        "results": results,
        "total": results.len(),
        "succeeded": succeeded,
        "failed": results.len() - succeeded,
    }))
}

/// 服务器之间互传：先从源机拉到本机临时目录，再推到目标机。
///
/// **经本机中转**而不是让两台服务器直连 —— 直连要求源机能主动连上目标机，
/// 现实里大多不成立（内网、单向出网、没有对方凭据）。经本机只要求两端都能连。
/// 代价是大文件要在本机落一次盘，收益是零配置。
pub async fn move_between(
    source_profile: &crate::models::SshServerProfile,
    source_credential: &str,
    target_profile: &crate::models::SshServerProfile,
    target_credential: &str,
    remote_path: &str,
    mirror: bool,
) -> Result<TransferStats, String> {
    let staging = staging_dir();
    let _ = std::fs::remove_dir_all(&staging);
    std::fs::create_dir_all(&staging).map_err(|error| format!("创建暂存目录失败：{error}"))?;

    // 第一段：源 → 本机暂存。整棵子树都拉下来，不做增量 ——
    // 源机与目标机的内容可能本来就不一样，按源机的清单跳过反而漏文件。
    let source_is_windows = is_windows_server(source_profile);
    let pulled = {
        let mut runner = runner_for(source_profile, source_credential)?;
        let result =
            pull_directory(&mut runner, source_is_windows, remote_path, &staging, false, false)
                .await;
        runner.shutdown().await;
        result
    };
    // 拉不下来就别往下走 —— 半截内容推给目标机比什么都不做更糟。
    pulled?;

    // 第二段：暂存 → 目标。这里才做增量与镜像，依据是目标机自己的清单。
    let pushed = upload(target_profile, target_credential, &staging, remote_path, true, mirror).await?;
    let _ = std::fs::remove_dir_all(&staging);

    Ok(TransferStats {
        bytes: pushed.bytes,
        files: pushed.files,
        skipped: pushed.skipped,
        removed: pushed.removed,
        notes: pushed.notes,
    })
}

fn staging_dir() -> PathBuf {
    std::env::temp_dir().join("network-manager-filetransfer")
}

/// 字节数转人类可读形态。
pub fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.0;
    let mut unit = 1;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if value >= 100.0 {
        format!("{:.0} {}", value, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_decode(text: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    let clean: String = text
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();
    base64::engine::general_purpose::STANDARD
        .decode(clean)
        .map_err(|error| format!("base64 解码失败：{error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64往返完整() {
        let cases: Vec<Vec<u8>> = vec![
            vec![0u8],
            vec![0u8, 1],
            vec![0u8, 1, 2],
            b"hello world".to_vec(),
            (0..=255u8).collect(),
            vec![0u8; 4096],
        ];
        for case in cases {
            let encoded = base64_encode(&case);
            let decoded = base64_decode(&encoded).expect("解码失败");
            assert_eq!(decoded, case, "长度 {} 的数据往返不一致", case.len());
        }
    }

    #[test]
    fn base64能吃带换行的文本() {
        let wrapped = "aGVsbG8g\nd29ybGQgaGVs\nbG8=";
        assert_eq!(base64_decode(wrapped).unwrap(), b"hello world hello");
    }

    #[test]
    fn linux路径引号按posix转义() {
        assert_eq!(quote_path("/tmp/a.txt", false).unwrap(), "'/tmp/a.txt'");
        assert_eq!(
            quote_path("/tmp/it's.txt", false).unwrap(),
            "'/tmp/it'\\''s.txt'"
        );
    }

    #[test]
    fn shell元字符一律拒绝() {
        assert!(quote_path("/tmp/a;rm -rf /", false).is_err());
        assert!(quote_path("/tmp/$(whoami)", false).is_err());
        assert!(quote_path("/tmp/a|b", false).is_err());
        assert!(quote_path("", false).is_err());
        assert!(validate_remote_path("/tmp/a`b").is_err());
    }

    #[test]
    fn 两边都禁掉会被shell或powershell吃掉的字符() {
        assert!(validate_remote_path("C:/tmp/a$b").is_err());
        assert!(validate_remote_path("C:/tmp/a`b").is_err());
        assert!(validate_remote_path("C:/tmp/a%b").is_err());
        assert!(validate_remote_path("/tmp/a;rm -rf /").is_err());
        assert!(validate_remote_path("/tmp/a`b").is_err());
    }

    #[test]
    fn 空格与中文路径要能过() {
        assert!(validate_remote_path("C:/Program Files/My App/a.txt").is_ok());
        assert!(validate_remote_path("C:/文档/配置.ini").is_ok());
        assert!(validate_remote_path("/srv/中文 目录/a.tar.gz").is_ok());
    }

    #[test]
    fn joins不拔掉盘符的根() {
        assert_eq!(join_remote("/var/www", "a/b.txt"), "/var/www/a/b.txt");
        assert_eq!(join_remote("/var/www/", "/a/b.txt"), "/var/www/a/b.txt");
        assert_eq!(join_remote("", "a.txt"), "a.txt");
        assert_eq!(join_remote("/var/www", ""), "/var/www");
    }

    #[test]
    fn 父目录计算() {
        assert_eq!(parent_of("/var/www/html"), "/var/www");
        assert_eq!(parent_of("/"), "");
        assert_eq!(parent_of("C:\\inetpub\\www"), "C:\\inetpub");
    }

    #[test]
    fn 本地目录不存在的父目录不会mkdir() {
        // 根目录下的文件：父目录就是 "/"，mkdir -p / 是幂等的，不算错
        assert_eq!(remote_parent("/index.html", false), Some("/".into()));
        assert_eq!(remote_parent("/var/www/index.html", false), Some("/var/www".into()));
        assert_eq!(remote_parent("C:\\index.html", true), None);
        assert_eq!(
            remote_parent("C:\\inetpub\\a.txt", true),
            Some("C:\\inetpub".into())
        );
    }

    #[test]
    fn 远端清单解析剥掉点与斜杠() {
        let text = "1024\t1700000000\tetc/nginx.conf\n2048\t1700000001.5\thost name.txt\n";
        let list = parse_remote_listing(text);
        assert_eq!(list.len(), 2);
        // 清单按路径升序：`etc/` 排在 `host` 前面
        assert_eq!(list[0], ("etc/nginx.conf".to_string(), 1024, 1700000000));
        assert_eq!(list[1], ("host name.txt".to_string(), 2048, 1700000001));
    }

    #[test]
    fn 远端清单忽略垃圾行() {
        let text = "total 123\n不是清单\n1024\t1700000000\tok.txt\n";
        let list = parse_remote_listing(text);
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].0, "ok.txt");
    }

    #[test]
    fn 增量判定规则() {
        assert!(needs_transfer(100, 100, 200, 100), "大小不同必传");
        assert!(!needs_transfer(100, 100, 100, 200), "远端更新 → 跳过");
        assert!(!needs_transfer(100, 100, 100, 100), "一样新 → 跳过");
        assert!(needs_transfer(100, 200, 100, 100), "远端更旧 → 传");
        assert!(needs_transfer(100, 100, 100, 0), "读不到远端时间 → 传");
    }

    #[test]
    fn 命令模板里不留占位符() {
        let script = fill(
            WINDOWS_WRITE_CHUNK,
            &[
                ("__R__", &ps_resolve("r", "\"C:/tmp/a.txt\"")),
                ("__MODE__", "Create"),
            ],
        );
        assert!(!script.contains("__R__"), "占位符没替换干净：{script}");
        assert!(!script.contains("__MODE__"));
        assert!(script.contains("$r = $ExecutionContext"));
        assert!(script.contains("[IO.FileMode]::Create"));
        assert!(!script.contains('\n'), "命令不能含换行，cmd 会截断");

        let read_script = fill(
            WINDOWS_READ_CHUNK,
            &[
                ("__R__", &ps_resolve("r", "\"C:/tmp/a.txt\"")),
                ("__OFFSET__", "0"),
                ("__SIZE__", &CHUNK_SIZE.to_string()),
            ],
        );
        assert!(!read_script.contains("__OFFSET__"));
        assert!(!read_script.contains("__SIZE__"));
        // PowerShell 的 if 块花括号必须原样到达远端 —— 用 format! 时这里会变成 {{ }}
        assert!(read_script.contains("if ($n -gt 0) {"));
        assert!(read_script.contains("[IO.SeekOrigin]::Begin"));
    }

    #[test]
    fn powershell外壳不掺库文件也不含换行() {
        let wrapped = wrap_powershell("Write-Output \"hi\"");
        assert!(wrapped.starts_with("powershell -NoProfile -NonInteractive -Command \""));
        assert!(wrapped.contains("[Text.Encoding]::UTF8"));
        // 内层双引号必须转义成反引号+引号，否则 PowerShell 会提前结束字符串
        assert!(wrapped.contains("`\"hi`\""));
    }

    #[test]
    fn 浏览目录结果排序目录在前() {
        let entries = parse_browse("f\t10\t1\tzeta.txt\nd\t0\t2\talpha\nf\t20\t3\tBeta.txt\n");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0]["kind"], "dir");
        assert_eq!(entries[1]["name"], "Beta.txt");
        assert_eq!(entries[2]["name"], "zeta.txt");
    }

    #[test]
    fn 人类可读字节() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(512), "512 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1536 * 1024), "1.5 MB");
        assert_eq!(human_bytes(200 * 1024 * 1024), "200 MB");
    }
}
