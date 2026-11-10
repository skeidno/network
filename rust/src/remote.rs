//! Linux 服务器的「远程界面」后端：Web 终端、远端文件树、系统监控。
//!
//! 三样东西都走项目里那套手写的 SSH exec 通道（`ssh::Runner`），不引入新依赖，
//! 也不要求目标机装 agent。Windows 侧已有的 `winsrv` 是 PowerShell 语义，
//! 这里专门写 Linux/POSIX 版本，避免把两套 shell 语法揉在一块。
//!
//! 所有函数都是 async 且**不碰 AppState 锁**：调用方（`methods::run_remote_action`）
//! 只在首尾短暂取锁拿服务器凭据，SSH 往返期间状态轮询照常跑。

use serde_json::{json, Value};

use crate::models::SshServerProfile;

/// 终端输出上限。远端 `cat` 一个大文件能把 JSON 撑到几十 MB，
/// 前端也渲染不动，这里统一截断并留一行提示。
const MAX_OUTPUT_BYTES: usize = 64 * 1024;

/// 一次 SSH exec 能拿到的最长命令（防止误贴一整篇文章进去）。
const MAX_COMMAND_BYTES: usize = 4096;

/// 在远端执行一条命令，stdout 与 stderr 合并回显，末尾附退出码。
///
/// 包一层 `sh -c` 是为了两件事：让失败命令（非零退出码）也能把输出带回来，
/// 而不是被 `Runner::run` 直接判成错误；以及把 stderr 并进 stdout，
/// 终端里看到的顺序才跟真实终端一致。
pub async fn run_shell(
    profile: &SshServerProfile,
    credential: &str,
    command: &str,
) -> Result<Value, String> {
    let command = command.trim();
    if command.is_empty() {
        return Err("请输入要执行的命令".into());
    }
    if command.len() > MAX_COMMAND_BYTES {
        return Err(format!(
            "命令太长（{} 字节，上限 {}）",
            command.len(),
            MAX_COMMAND_BYTES
        ));
    }
    // 单引号转义：`'` → `'\''`，这是 POSIX sh 里在单引号串内嵌单引号的唯一写法。
    let escaped = command.replace('\'', "'\\''");
    let script = format!(
        "sh -c '{}' 2>&1; printf '\\n[NM_EXIT:%d]\\n' \"$?\"",
        escaped
    );
    let mut runner = crate::ssh::Runner::new(profile, credential)?;
    let result = runner.run(&script, 30).await;
    runner.shutdown().await;
    let (output, truncated) = clip_output(&result?);
    Ok(json!({
        "output": output,
        "truncated": truncated,
    }))
}

fn clip_output(text: &str) -> (String, bool) {
    if text.len() <= MAX_OUTPUT_BYTES {
        return (text.to_string(), false);
    }
    // 按字符边界截，别把一个中文字劈成半个导致 JSON 里出现替换符。
    let mut end = MAX_OUTPUT_BYTES;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (
        format!(
            "{}…\n[输出已截断：只显示前 {} 字节]",
            &text[..end],
            MAX_OUTPUT_BYTES
        ),
        true,
    )
}

/// 远端目录列表。直接复用文件快传那条已经处理过 Windows/Linux 差异的通道。
pub async fn browse(
    profile: &SshServerProfile,
    credential: &str,
    path: &str,
) -> Result<Value, String> {
    crate::filetransfer::browse_remote(profile, credential, path).await
}

/// 采集 Linux 主机的系统信息。
///
/// 只用 `/proc`、`uname`、`df` 这些基础工具，不依赖 `top`/`vmstat`/`bc`
/// （精简镜像里经常没有）。CPU 用两次 `/proc/stat` 快照算差值，
/// 输出统一是 `NMKEY:value` 行，便于解析和单测。
const METRICS_SCRIPT: &str = r#"
echo "NMHOST:$(hostname 2>/dev/null || uname -n)"
if [ -f /etc/os-release ]; then
  . /etc/os-release 2>/dev/null
  echo "NMOS:${PRETTY_NAME:-Linux}"
else
  echo "NMOS:$(uname -s)"
fi
echo "NMKERNEL:$(uname -r 2>/dev/null)"
echo "NMARCH:$(uname -m 2>/dev/null)"
echo "NMUPTIME:$(awk '{print $1}' /proc/uptime 2>/dev/null)"
echo "NMCORES:$(getconf _NPROCESSORS_ONLN 2>/dev/null || nproc 2>/dev/null || echo 1)"
echo "NMLOAD:$(awk '{print $1, $2, $3}' /proc/loadavg 2>/dev/null)"
awk '/^MemTotal:/ {t=$2} /^MemAvailable:/ {a=$2} END {print "NMMEMTOT:" t+0; print "NMMEMAVAIL:" a+0}' /proc/meminfo 2>/dev/null
df -kP / 2>/dev/null | awk 'NR==2 {print "NMDISKTOT:" $2; print "NMDISKFREE:" $4}'
c1=$(awk '/^cpu / {print $2+$3+$4+$5+$6+$7+$8, $5}' /proc/stat 2>/dev/null)
sleep 1
c2=$(awk '/^cpu / {print $2+$3+$4+$5+$6+$7+$8, $5}' /proc/stat 2>/dev/null)
echo "$c1 $c2" | awk '{t=$3-$1; i=$4-$2; if (t>0) printf "NMCPU:%.1f\n", (1-i/t)*100; else print "NMCPU:0"}'
"#;

pub async fn metrics(
    profile: &SshServerProfile,
    credential: &str,
) -> Result<Value, String> {
    let mut runner = crate::ssh::Runner::new(profile, credential)?;
    let result = runner.run(METRICS_SCRIPT, 30).await;
    runner.shutdown().await;
    parse_metrics(&result?)
}

/// 把标记行解析成前端用的结构。字段名跟 `winsrv::parse_metrics` 对齐，
/// 这样界面可以共用一套卡片渲染。
fn parse_metrics(output: &str) -> Result<Value, String> {
    let mut fields: Vec<(String, String)> = Vec::new();
    for line in output.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("NM") {
            if let Some((key, value)) = rest.split_once(':') {
                if !key.is_empty() {
                    fields.push((key.to_string(), value.trim().to_string()));
                }
            }
        }
    }
    if fields.is_empty() {
        return Err(format!(
            "远端没有返回可识别的状态数据（可能不是 Linux 主机）。原始输出：\n{}",
            output.trim()
        ));
    }
    let get = |key: &str| -> String {
        fields
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let number = |key: &str| -> f64 { get(key).parse::<f64>().unwrap_or(0.0) };

    let uptime_seconds = number("UPTIME");
    // /proc/meminfo 与 df -kP 都是 KB，统一换算成字节
    let mem_total = number("MEMTOT") * 1024.0;
    let mem_available = number("MEMAVAIL") * 1024.0;
    let mem_used = (mem_total - mem_available).max(0.0);
    let disk_total = number("DISKTOT") * 1024.0;
    let disk_free = number("DISKFREE") * 1024.0;
    let disk_used = (disk_total - disk_free).max(0.0);
    let percent = |used: f64, total: f64| {
        if total > 0.0 {
            (used / total * 1000.0).round() / 10.0
        } else {
            0.0
        }
    };

    Ok(json!({
        "hostName": get("HOST"),
        "os": get("OS"),
        "osVersion": get("KERNEL"),
        "arch": get("ARCH"),
        "uptimeSeconds": uptime_seconds as i64,
        "uptimeText": format_uptime(uptime_seconds),
        "cpuPercent": number("CPU"),
        "cpuCores": number("CORES") as i64,
        "load": get("LOAD"),
        "memoryTotalBytes": mem_total as i64,
        "memoryUsedBytes": mem_used as i64,
        "memoryPercent": percent(mem_used, mem_total),
        "diskTotalBytes": disk_total as i64,
        "diskUsedBytes": disk_used as i64,
        "diskPercent": percent(disk_used, disk_total),
    }))
}

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

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "NMHOST:web-01\n\
NMOS:Ubuntu 22.04.5 LTS\n\
NMKERNEL:5.15.0-119-generic\n\
NMARCH:x86_64\n\
NMUPTIME:604800.5\n\
NMCORES:2\n\
NMLOAD:0.12 0.08 0.03\n\
NMMEMTOT:16384000\n\
NMMEMAVAIL:8388608\n\
NMDISKTOT:41152000\n\
NMDISKFREE:27160000\n\
NMCPU:3.5\n";

    #[test]
    fn parse_linux_metrics_and_convert_units() {
        let value = parse_metrics(SAMPLE).expect("应解析成功");
        assert_eq!(value["hostName"], "web-01");
        assert_eq!(value["os"], "Ubuntu 22.04.5 LTS");
        assert_eq!(value["osVersion"], "5.15.0-119-generic");
        assert_eq!(value["uptimeText"], "7 天 0 小时");
        assert_eq!(value["cpuCores"], 2);
        assert_eq!(value["cpuPercent"], 3.5);
        assert_eq!(value["memoryTotalBytes"], 16384000_i64 * 1024);
        assert_eq!(value["memoryUsedBytes"], (16384000_i64 - 8388608) * 1024);
        assert_eq!(value["diskPercent"], 34.0);
    }

    #[test]
    fn values_with_colons_are_not_truncated() {
        let value = parse_metrics("NMHOST:a:b\nNMOS:Debian:12\n").unwrap();
        assert_eq!(value["hostName"], "a:b");
        assert_eq!(value["os"], "Debian:12");
    }

    #[test]
    fn non_linux_output_reports_an_error() {
        let err = parse_metrics("some random output\n").unwrap_err();
        assert!(err.contains("没有返回可识别"), "{err}");
    }

    #[test]
    fn missing_fields_default_to_zero() {
        let value = parse_metrics("NMCPU:0.0\n").unwrap();
        assert_eq!(value["memoryPercent"], 0.0);
        assert_eq!(value["diskTotalBytes"], 0);
    }

    #[test]
    fn long_output_is_clipped_at_char_boundary() {
        let text = "中".repeat(MAX_OUTPUT_BYTES);
        let (clipped, truncated) = clip_output(&text);
        assert!(truncated);
        assert!(clipped.contains("输出已截断"));
        assert!(clipped.len() < text.len());
    }

    #[test]
    fn single_quote_escaping_follows_posix() {
        // 这里只验证转义串本身可拼装；真正执行要连远端，放到端到端验证。
        let cmd = "echo 'hi'";
        let escaped = cmd.replace('\'', "'\\''");
        assert_eq!(escaped, "echo '\\''hi'\\''");
    }
}
