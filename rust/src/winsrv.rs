//! Windows 服务器的远程管理：通过 SSH 在目标机器上执行 PowerShell。
//!
//! # 为什么走 SSH
//!
//! Windows Server 2019 及以后内置了 OpenSSH 服务端（可选功能，
//! 「添加功能 → OpenSSH 服务器」），讲的是标准 SSH 协议。所以本项目
//! 那套内置 SSH 客户端**不需要任何改动**就能连上 Windows —— 复用同一套
//! 客户端、同一份凭据、同一份 known_hosts。
//!
//! 相比 WinRM（`Invoke-Command`）走 SSH 的好处：
//! - 客户端不用额外配证书与 HTTPS 监听；
//! - 跟 Linux 侧的连接方式完全一致，用户心智统一；
//! - 不依赖域环境，工作组机器也能用。
//!
//! # 命令为什么都要加前缀
//!
//! SSH 的 exec 通道在 Windows 上默认跑的是 **cmd.exe**，不是 PowerShell。
//! 所以每条命令都显式写成 `powershell -NoProfile -NonInteractive -Command "..."`，
//! 不依赖远端的默认 shell 配置。
//!
//! # 输出编码
//!
//! 远端 PowerShell 的输出编码由 `$OutputEncoding` 与控制台代码页决定，
//! 各版本不一致。所以所有命令都显式设 `$OutputEncoding = [Console]::OutputEncoding =
//! [Text.Encoding]::UTF8`，并且**只输出 ASCII 标记**（如 `NMOK:xxx`），
//! 中间结果用 JSON 传回，避免编码问题把结果搅乱。

use serde_json::{json, Value};
use std::process::Command;

/// 远端执行一条 PowerShell 命令，返回标准输出。
///
/// 走 `crate::ssh::Runner` —— 它已经处理了连接复用、超时、错误分类
/// （认证失败会翻译成中文提示），这里不重复造。
pub async fn run_powershell(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    script: &str,
) -> Result<String, String> {
    // 显式设 UTF-8 输出编码，并把 CRLF 换成 LF —— 前端展示时 \r 是噪音
    let wrapped = format!(
        "$OutputEncoding = [Console]::OutputEncoding = [Text.Encoding]::UTF8; {script}"
    );
    // 用单引号包住整个 -Command 参数，内部的单引号写成 '\'' 是 PowerShell 的转义法。
    // 但 Windows 的 cmd 对引号处理更麻烦 —— 改用双引号包外层、内部避免双引号。
    let escaped = wrapped.replace('"', "`\"");
    let command = format!("powershell -NoProfile -NonInteractive -Command \"{escaped}\"");

    let mut runner = crate::ssh::Runner::new(profile, credential)?;
    let result = runner.run(&command, 60).await?;
    Ok(result.replace("\r\n", "\n"))
}

/// 在远端跑一条 shell 命令（Linux 侧用）。
///
/// Windows 的 exec 通道默认跑 cmd.exe，Linux 的默认跑 sh —— 这两套语法完全不同，
/// 所以分成两个函数，不试图用一套模板兼容两边。
pub async fn run_shell(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    script: &str,
) -> Result<String, String> {
    let mut runner = crate::ssh::Runner::new(profile, credential)?;
    let result = runner.run(script, 60).await?;
    Ok(result.replace("\r\n", "\n"))
}

/// 采集性能与运行状态。
///
/// 刻意用 PowerShell 的 WMI/CIM 类而不是 `typeperf` 或任务管理器 ——
/// 一次调用能拿到全部指标，输出结构稳定，跨 Windows 版本一致。
pub async fn collect_metrics(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<Value, String> {
    // 分段采集，每段之间用标记隔开 —— 比拼 JSON 字符串再解析稳得多
    let script = r#"
$os = Get-CimInstance Win32_OperatingSystem
$cpu = (Get-CimInstance Win32_Processor | Measure-Object -Property LoadPercentage -Average).Average
$memTotal = $os.TotalVisibleMemorySize
$memFree = $os.FreePhysicalMemory
$uptime = (Get-Date) - $os.LastBootUpTime
$disk = Get-CimInstance Win32_LogicalDisk -Filter "DeviceID='C:'" -ErrorAction SilentlyContinue
$cpuCores = (Get-CimInstance Win32_Processor | Measure-Object -Property NumberOfLogicalProcessors -Sum).Sum
$hostName = $env:COMPUTERNAME
[Console]::Out.Write("NMHOST:" + $hostName)
[Console]::Out.Write("NMOS:" + $os.Caption)
[Console]::Out.Write("NMVER:" + $os.Version)
[Console]::Out.Write("NMUPTIME:" + [int]$uptime.TotalSeconds)
[Console]::Out.Write("NMCPU:" + [int]$cpu)
[Console]::Out.Write("NMCORES:" + $cpuCores)
[Console]::Out.Write("NMMEMTOT:" + $memTotal)
[Console]::Out.Write("NMMEMFREE:" + $memFree)
if ($disk) {
  [Console]::Out.Write("NMDISKTOT:" + $disk.Size)
  [Console]::Out.Write("NMDISKFREE:" + $disk.FreeSpace)
}
"#;
    let out = run_powershell(profile, credential, script).await?;
    parse_metrics(&out)
}

/// 解析远端输出的标记行。
///
/// 用 `NMxxx:值` 这样的标记而不是 JSON —— 远端 PowerShell 的 JSON 序列化
/// 在不同版本上格式差异大（缩进、大小写、浮点数精度），反而难解析。
/// 标记行是纯 ASCII，跨版本绝对可靠。
fn parse_metrics(output: &str) -> Result<Value, String> {
    let mut map: Vec<(String, String)> = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some((key, value)) = line.split_once(':') {
            let key = key.trim();
            if !key.starts_with("NM") || key.len() <= 2 {
                continue;
            }
            map.push((key[2..].to_string(), value.trim().to_string()));
        }
    }
    let get = |key: &str| -> String {
        map.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    if map.is_empty() {
        return Err(format!(
            "远端没有返回可识别的状态数据。原始输出：\n{}",
            output.trim()
        ));
    }

    let number = |key: &str| -> f64 {
        get(key).parse::<f64>().unwrap_or(0.0)
    };
    let uptime_seconds = number("UPTIME");
    let mem_total = number("MEMTOT");
    let mem_free = number("MEMFREE");
    let disk_total = number("DISKTOT");
    let disk_free = number("DISKFREE");

    Ok(json!({
        "hostName": get("HOST"),
        "os": get("OS"),
        "osVersion": get("VER"),
        "uptimeSeconds": uptime_seconds as i64,
        "uptimeText": format_uptime(uptime_seconds),
        "cpuPercent": number("CPU"),
        "cpuCores": number("CORES") as i64,
        "memoryTotalBytes": mem_total as i64 * 1024.0 as i64,
        "memoryUsedBytes": ((mem_total - mem_free) * 1024.0) as i64,
        "memoryPercent": if mem_total > 0.0 {
            ((mem_total - mem_free) / mem_total * 100.0 * 10.0).round() / 10.0
        } else { 0.0 },
        "diskTotalBytes": disk_total as i64,
        "diskUsedBytes": (disk_total - disk_free) as i64,
        "diskPercent": if disk_total > 0.0 {
            ((disk_total - disk_free) / disk_total * 100.0 * 10.0).round() / 10.0
        } else { 0.0 },
    }))
}

/// 秒数转成人话。
fn format_uptime(seconds: f64) -> String {
    let total = seconds.max(0.0) as i64;
    let days = total / 86400;
    let hours = (total % 86400) / 3600;
    let minutes = (total % 3600) / 60;
    if days > 0 {
        format!("{days} 天 {hours} 小时")
    } else if hours > 0 {
        format!("{hours} 小时 {minutes} 分")
    } else {
        format!("{minutes} 分钟")
    }
}

/// 采集电源与远程桌面相关的配置项（只读）。
///
/// 每项输出一个 `NMPWR:key=value`，用 `=` 而不是 `:` 是因为
/// 有些值本身含冒号（时间、路径）。
pub async fn collect_power_settings(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<Value, String> {
    let script = r#"
function Read-PowerValue($sub, $setting) {
  $out = powercfg /query SCHEME_CURRENT $sub $setting 2>$null | Out-String
  if ($out -match '0x([0-9a-fA-F]+)') { return [int64]('0x' + $Matches[1]) }
  return -1
}
function Read-RegValue($path, $name) {
  try {
    $item = Get-ItemProperty -Path $path -Name $name -ErrorAction Stop
    return [string]$item.$name
  } catch { return 'NA' }
}
$sleep = Read-PowerValue 'SUB_SLEEP' 'STANDBYIDLE'
$hib   = Read-PowerValue 'SUB_SLEEP' 'HIBERNATEIDLE'
$video = Read-PowerValue 'SUB_VIDEO' 'VIDEOIDLE'
$hibFile = Read-RegValue 'HKLM:\SYSTEM\CurrentControlSet\Control\Power' 'HibernateEnabled'
$single = Read-RegValue 'HKLM:\SYSTEM\CurrentControlSet\Control\Terminal Server' 'fSingleSessionPerUser'
$saver  = Read-RegValue 'HKCU:\Control Panel\Desktop' 'ScreenSaveActive'
$scheme = (powercfg /getactivescheme 2>$null | Out-String)
if ($scheme -match '([0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12})') {
  $schemeGuid = $Matches[1]
} else { $schemeGuid = '' }
if ($scheme -match '\(([^)]+)\)') { $schemeName = $Matches[1] } else { $schemeName = '' }
[Console]::Out.Write("NMPRW:sleep=$sleep")
[Console]::Out.Write("NMPRW:hibernate=$hib")
[Console]::Out.Write("NMPRW:video=$video")
[Console]::Out.Write("NMPRW:hibFile=$hibFile")
[Console]::Out.Write("NMPRW:single=$single")
[Console]::Out.Write("NMPRW:saver=$saver")
[Console]::Out.Write("NMPRW:schemeGuid=$schemeGuid")
[Console]::Out.Write("NMPRW:schemeName=$schemeName")
"#;
    let out = run_powershell(profile, credential, script).await?;
    parse_power_settings(&out)
}

/// 解析电源设置输出。
fn parse_power_settings(output: &str) -> Result<Value, String> {
    let mut map: Vec<(String, String)> = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        // 标记是 NMPRW:key=value
        if !line.starts_with("NMPRW:") {
            continue;
        }
        let body = &line[6..];
        if let Some((key, value)) = body.split_once('=') {
            map.push((key.trim().to_string(), value.trim().to_string()));
        }
    }
    if map.is_empty() {
        return Err(format!(
            "远端没有返回电源设置。原始输出：\n{}",
            output.trim()
        ));
    }
    let get = |key: &str| -> String {
        map.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let number = |key: &str| -> i64 {
        get(key).parse::<i64>().unwrap_or(-1)
    };

    let sleep = number("sleep");
    let hibernate = number("hibernate");
    let video = number("video");
    let hib_file = get("hibFile");
    let single = get("single");
    let saver = get("saver");

    Ok(json!({
        "sleepSeconds": sleep,
        "hibernateSeconds": hibernate,
        "videoSeconds": video,
        "hibernateEnabled": hib_file == "1",
        "hibernateFilePresent": hib_file == "1",
        "singleSessionPerUser": single == "1",
        "screenSaverActive": saver == "1",
        "schemeGuid": get("schemeGuid"),
        "schemeName": get("schemeName"),
    }))
}

/// 一条可执行的修复动作（远程执行）。
#[derive(Debug, Clone)]
pub struct RemoteFix {
    pub id: &'static str,
    pub title: &'static str,
    pub reason: &'static str,
    /// 远端要执行的 PowerShell。
    pub script: &'static str,
    pub need_admin: bool,
}

pub fn remote_fixes() -> Vec<RemoteFix> {
    vec![
        RemoteFix {
            id: "sleep",
            title: "睡眠：设为「从不」",
            reason: "RDP 断开后没人动鼠标，超时就进睡眠，远程直接断。",
            script: "powercfg /change standby-timeout-ac 0; powercfg /change standby-timeout-dc 0",
            need_admin: true,
        },
        RemoteFix {
            id: "video",
            title: "关闭显示器超时：设为「从不」",
            reason: "这就是「息屏」的来源，跟睡眠是两套设置，要分开治。",
            script: "powercfg /change monitor-timeout-ac 0; powercfg /change monitor-timeout-dc 0",
            need_admin: true,
        },
        RemoteFix {
            id: "hdd",
            title: "硬盘休眠超时：设为「从不」",
            reason: "有些电源方案里它独立于睡眠，超时后先关硬盘再睡。",
            script: "powercfg /change disk-timeout-ac 0; powercfg /change disk-timeout-dc 0",
            need_admin: true,
        },
        RemoteFix {
            id: "screensaver",
            title: "关闭屏幕保护程序",
            reason: "屏保会遮住画面，让人误以为远程掉线了。",
            script: "Set-ItemProperty -Path 'HKCU:\\Control Panel\\Desktop' -Name 'ScreenSaveActive' -Value '' -Type String; Set-ItemProperty -Path 'HKCU:\\Control Panel\\Desktop' -Name 'ScreenSaveTimeOut' -Value '0' -Type String",
            need_admin: false,
        },
        RemoteFix {
            id: "hibernateFile",
            title: "关闭休眠文件（顺带关掉快速启动）",
            reason: "开着休眠文件时「关机」其实是休眠，重启远程也救不回来。",
            script: "powercfg -hibernate off",
            need_admin: true,
        },
        RemoteFix {
            id: "schemeHigh",
            title: "切到「高性能」电源计划",
            reason: "平衡计划在无人操作时更激进，休眠阈值更短。",
            script: "powercfg /setactive 8c5e7fda-e8bf-4a96-9a85-a6e23a8c635c",
            need_admin: true,
        },
        RemoteFix {
            id: "singleSession",
            title: "允许同一用户多个远程会话",
            reason: "为 1 时同名用户重连会踢掉上一个会话 —— 这就是「必须用子用户」的原因。改成 0 才能同用户多会话（需重启 TermService）。",
            script: "Set-ItemProperty -Path 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Terminal Server' -Name 'fSingleSessionPerUser' -Value 0 -Type DWord",
            need_admin: true,
        },
        RemoteFix {
            id: "keepSession",
            title: "断开连接时保留会话",
            reason: "默认就是保留，但被组策略改过的话断连等于注销，程序全没了。",
            script: "Set-ItemProperty -Path 'HKLM:\\SOFTWARE\\Policies\\Microsoft\\Windows NT\\Terminal Services' -Name 'MaxDisconnectionTime' -Value 0 -Type DWord -Force; Set-ItemProperty -Path 'HKLM:\\SOFTWARE\\Policies\\Microsoft\\Windows NT\\Terminal Services' -Name 'fSingleSessionPerUser' -Value 0 -Type DWord -Force",
            need_admin: true,
        },
    ]
}

/// 在远端执行一条修复，返回执行输出。
pub async fn apply_remote_fix(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    id: &str,
) -> Result<String, String> {
    let Some(fix) = remote_fixes().into_iter().find(|f| f.id == id) else {
        return Err(format!("未知的修复项：{id}"));
    };
    // 每条命令后面跟一句标记，方便确认「跑完了」而不是「跑到一半断了」
    let script = format!("{}; [Console]::Out.Write('NMDONE')", fix.script);
    let out = run_powershell(profile, credential, &script).await?;
    if out.contains("NMDONE") {
        Ok(format!("{} 已执行。\n{}", fix.title, out.trim()))
    } else {
        Err(format!(
            "{} 未能确认执行完成（可能权限不足或命令被拦截）。\n远端输出：\n{}",
            fix.title,
            out.trim()
        ))
    }
}

/// 在远端列出用户，并标出谁在「远程桌面用户」组里。
pub async fn list_remote_users(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<Value, String> {
    let script = r#"
$users = Get-LocalUser -ErrorAction SilentlyContinue
if (-not $users) { $users = Get-WmiObject Win32_UserAccount -Filter "LocalAccount=True" }
$rdp = @()
try { $rdp = (Get-LocalGroupMember -Group 'Remote Desktop Users' -ErrorAction Stop | Select-Object -ExpandProperty Name) } catch { }
foreach ($u in $users) {
  $name = $u.Name
  if (-not $name) { continue }
  $inGroup = if ($rdp -contains $name) { 'Y' } else { 'N' }
  [Console]::Out.Write("NMUSR:$name|$inGroup")
}
"#;
    let out = run_powershell(profile, credential, script).await?;
    let mut users = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if !line.starts_with("NMUSR:") {
            continue;
        }
        let body = &line[6..];
        if let Some((name, member)) = body.split_once('|') {
            users.push(json!({
                "name": name.trim(),
                "rdpMember": member.trim() == "Y",
            }));
        }
    }
    if users.is_empty() {
        return Err(format!("远端没有读到用户列表。\n原始输出：\n{}", out.trim()));
    }
    Ok(json!({ "users": users }))
}

/// 在远端创建一个 RDP 用的子用户。
///
/// 显式加进「远程桌面用户」组 —— 这是最常见的坑：新用户默认不在组里，
/// 连远程桌面会被直接拒绝，用户往往不知道为什么。
pub async fn create_remote_user(
    profile: &crate::models::SshServerProfile,
    credential: &str,
    name: &str,
    password: &str,
    admin: bool,
) -> Result<String, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("用户名不能为空".into());
    }
    if password.is_empty() {
        return Err("密码不能为空（Windows 禁用空密码远程登录）".into());
    }
    // 用户名里带引号会破坏命令，先挡掉
    if name.chars().any(|c| "[]/\\:|=;,\"<>?".contains(c) || c.is_whitespace()) {
        return Err(format!("用户名「{name}」含非法字符"));
    }
    if password.chars().any(|c| c == '"' || c == '`' || c == '$' || c.is_control()) {
        return Err("密码里不能包含引号、反引号、美元符号或控制字符（会破坏命令解析）".into());
    }

    // 用 PowerShell 的 API 而不是 net user —— net user 的命令行引号规则
    // 在 SSH exec 通道里套一层再套一层，很容易被吃掉。
    //
    // 管理员组那一段单独拼，不放进 format! 的模板里 ——
    // format! 里的 `{{` `}}` 是转义后的花括号，混进来很难读，
    // 上一次写就是因为把 Rust 字符串和 PowerShell 变量混在一处才出的错。
    let admin_line = if admin {
        format!(
            "try {{ Add-LocalGroupMember -Group 'Administrators' -Member '{name}' -ErrorAction Stop }} catch {{ }}"
        )
    } else {
        String::new()
    };
    let script = format!(
        r#"
$ErrorActionPreference = 'Stop'
$pw = ConvertTo-SecureString '{password}' -AsPlainText -Force
if (Get-LocalUser -Name '{name}' -ErrorAction SilentlyContinue) {{
  Set-LocalUser -Name '{name}' -Password $pw
  [Console]::Out.Write('NMACT:updated')
}} else {{
  New-LocalUser -Name '{name}' -Password $pw -PasswordNeverExpires -AccountNeverExpires | Out-Null
  [Console]::Out.Write('NMACT:created')
}}
try {{ Add-LocalGroupMember -Group 'Remote Desktop Users' -Member '{name}' -ErrorAction Stop }} catch {{ }}
{admin_line}
"#,
        password = password,
        name = name,
        admin_line = admin_line,
    );
    let out = run_powershell(profile, credential, &script).await?;
    // 远端用 NMACT: 标记回报动作，Rust 侧不猜
    let action = if out.contains("NMACT:updated") {
        "已重置密码"
    } else if out.contains("NMACT:created") {
        "已创建"
    } else {
        "已处理"
    };
    Ok(format!(
        "用户 {name} {action}（已加入「远程桌面用户」组）。\n{}",
        out.trim()
    ))
}

/// 打开远程桌面并预填地址。
///
/// 直接调 `mstsc.exe <host>` 而不是生成 `.rdp` 文件 —— 文件要写盘、
/// 还要处理 NLA 与凭据缓存的字段，而 mstsc 命令行已经能做预填，
/// 其余（凭据、剪贴板、多显示器）交给客户端自己的处理更靠谱。
pub fn open_rdp(host: &str) -> Result<(), String> {
    #[cfg(windows)]
    {
        // mstsc 常常不在 PATH（它在 System32 下，GUI 进程的 PATH 可能不含它）
        let candidates = [
            "mstsc.exe".to_string(),
            r"C:\Windows\System32\mstsc.exe".to_string(),
        ];
        let exe = candidates
            .iter()
            .find(|name| {
                if name.contains('\\') {
                    std::path::PathBuf::from(name).is_file()
                } else {
                    find_in_path(name)
                }
            })
            .cloned()
            .ok_or_else(|| "找不到 mstsc.exe，远程桌面客户端可能未安装".to_string())?;
        Command::new(exe)
            .arg(host)
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("启动远程桌面失败：{error}"))
    }
    #[cfg(not(windows))]
    {
        let _ = host;
        Err("远程桌面连接只在 Windows 上可用".into())
    }
}

#[cfg(windows)]
fn find_in_path(name: &str) -> bool {
    std::env::var_os("PATH")
        .map(|path_var| {
            std::env::split_paths(&path_var).any(|dir| dir.join(name).is_file())
        })
        .unwrap_or(false)
}

/// 探测远端系统信息兼作探活。返回 (系统名, 版本)。
pub async fn probe_os(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<(String, String), String> {
    let out = probe_os_raw(profile, credential).await?;
    let mut parts = out.split('|');
    let caption = parts.next().unwrap_or("").trim().to_string();
    let version = parts.next().unwrap_or("").trim().to_string();
    Ok((caption, version))
}

async fn probe_os_raw(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<String, String> {
    run_powershell(
        profile,
        credential,
        "[Console]::Out.Write((Get-CimInstance Win32_OperatingSystem).Caption + '|' + [Environment]::OSVersion.Version.ToString())",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 解析性能指标() {
        let out = "NMHOST:SERVER01\nNMOS:Microsoft Windows Server 2022\nNMVER:10.0.20348\nNMUPTIME:93784\nNMCPU:23\nNMCORES:8\nNMMEMTOT:16777216\nNMMEMFREE:8388608\nNMDISKTOT:107374182400\nNMDISKFREE:53687091200\n";
        let value = parse_metrics(out).unwrap();
        assert_eq!(value["hostName"], "SERVER01");
        assert_eq!(value["os"], "Microsoft Windows Server 2022");
        assert_eq!(value["cpuPercent"], 23.0);
        assert_eq!(value["cpuCores"], 8);
        // 16 GB 总内存，8 GB 空闲 → 50% 已用
        assert_eq!(value["memoryPercent"], 50.0);
        assert_eq!(value["diskPercent"], 50.0);
        assert_eq!(value["uptimeSeconds"], 93784);
    }

    #[test]
    fn 运行时长转人话() {
        assert_eq!(format_uptime(60.0), "1 分钟");
        assert_eq!(format_uptime(3600.0), "1 小时 0 分");
        assert_eq!(format_uptime(93784.0), "1 天 2 小时");
        assert_eq!(format_uptime(0.0), "0 分钟");
    }

    #[test]
    fn 解析电源设置() {
        let out = "NMPRW:sleep=1800\nNMPRW:hibernate=3600\nNMPRW:video=1200\nNMPRW:hibFile=1\nNMPRW:single=1\nNMPRW:saver=1\nNMPRW:schemeGuid=381b4222-f694-41f0-9685-ff5bb260df2e\nNMPRW:schemeName=平衡\n";
        let value = parse_power_settings(out).unwrap();
        assert_eq!(value["sleepSeconds"], 1800);
        assert_eq!(value["videoSeconds"], 1200);
        assert_eq!(value["singleSessionPerUser"], true);
        assert_eq!(value["screenSaverActive"], true);
        assert_eq!(value["schemeName"], "平衡");
    }

    #[test]
    fn 远端没返回数据时要报错而不是给空值() {
        // 静默返回空指标比报错更糟 —— 界面上会显示「0% CPU」，
        // 用户以为服务器空载，其实是没连上
        assert!(parse_metrics("").is_err());
        assert!(parse_metrics("some random output").is_err());
        assert!(parse_power_settings("").is_err());
    }

    #[test]
    fn 修复清单覆盖电源与RDP两类() {
        let ids: Vec<&str> = remote_fixes().iter().map(|f| f.id).collect();
        assert!(ids.contains(&"sleep"), "缺睡眠设置");
        assert!(ids.contains(&"video"), "缺显示器超时");
        assert!(ids.contains(&"screensaver"), "缺屏保关闭");
        assert!(ids.contains(&"hibernateFile"), "缺休眠文件关闭");
        assert!(ids.contains(&"singleSession"), "缺单会话限制");
        assert!(ids.contains(&"keepSession"), "缺会话保留");
    }
}