//! 让 Windows 服务器在 RDP 断开后继续运行。
//!
//! # 用户的需求与它的真实含义
//!
//! 需求是「我的电脑会关机，不可能一直连着，所以重点是让 Windows 服务器
//! RDP 关闭后还能保持开机状态」。
//!
//! 这里要把「保持开机」拆成两件**不同**的事，很多人会混为一谈：
//!
//! 1. **不睡眠、不息屏、不休眠** —— 机器还在跑，只是别让它进省电模式。
//!    这是操作系统层面能解决的，靠电源计划与注册表。
//! 2. **不关机** —— 机器真的被人关了，或者断电了。操作系统层面**管不了**：
//!    系统一旦开始关机流程，任何软件都拦不住。这个只能靠 BIOS/UEFI 的
//!    「来电自动开机 / 断电恢复后自动开机」，或者云厂商控制台的
//!    「停止实例后自动重启」来管。
//!
//! 所以本模块分成两档：
//!
//! - **软件层（能远程做完的）**：断开后不掉电、不睡眠、不注销、会话保留、
//!   计划任务保活。
//! - **硬件层（必须去机房或云控制台做的）**：来电自启、断电恢复自启、
//!   看门狗。这里只给出**该去哪个菜单、开哪个选项**的指引，不假装能远程搞定。
//!
//! # 几个容易搞混的点
//!
//! - **「快速启动」会让「关机」变成「休眠」**。开着快速启动时，
//!   点了关机其实是休眠到磁盘（所以重启后原来的会话还在）。
//!   要真正关机就得关掉它 —— 也就是说，快速启动开着时
//!   「关机」反而更像「保持运行」，但它不省电且状态会怪。
//! - **`fSingleSessionPerUser=1`** 会让同名用户重连踢掉旧会话。
//!   这不是「机器睡眠」，是「会话被踢」，两个问题要分开治。
//! - **断开后自动注销**（组策略 MaxDisconnectionTime 配了有限值）
//!   会让服务器上跑的程序全没了 —— 用户看到的现象跟睡眠一样，
//!   但根因完全不同。

use serde_json::{json, Value};

/// 一条保活措施。
#[derive(Debug, Clone)]
pub struct KeepAliveItem {
    /// 稳定 id。
    pub id: &'static str,
    /// 显示名。
    pub title: &'static str,
    /// 这一条解决什么问题。
    pub reason: &'static str,
    /// 执行的命令（Windows 用 PowerShell，Linux 用 shell）。
    pub script: &'static str,
    /// 需要管理员权限。
    pub need_admin: bool,
    /// 这一层属于软件层还是硬件层。
    pub layer: &'static str,
    /// 做完要不要重启才生效。
    pub need_restart: bool,
}

/// Windows 的软件层保活措施。
pub fn windows_keep_alive() -> Vec<KeepAliveItem> {
    vec![
        KeepAliveItem {
            id: "sleep",
            title: "睡眠：设为「从不」",
            reason: "RDP 断开后没人动鼠标，超时就进睡眠，远程直接断。这是最主要的一条。",
            script: "powercfg /change standby-timeout-ac 0; powercfg /change standby-timeout-dc 0",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "video",
            title: "关闭显示器超时：设为「从不」",
            reason: "这就是「息屏」的来源，跟睡眠是两套设置，要分开治。",
            script: "powercfg /change monitor-timeout-ac 0; powercfg /change monitor-timeout-dc 0",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "hdd",
            title: "硬盘休眠超时：设为「从不」",
            reason: "有些电源计划里它独立于睡眠，超时后先关硬盘再睡。",
            script: "powercfg /change disk-timeout-ac 0; powercfg /change disk-timeout-dc 0",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "hibernateFile",
            title: "关闭休眠文件",
            reason: "开着时「关机」其实是休眠，机器看似关了其实在耗电，而且唤醒路径很怪。关掉后关机就是真关机。",
            script: "powercfg -hibernate off",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "fastStartup",
            title: "关闭快速启动",
            reason: "开着快速启动时「关机」实际是休眠+混合关机，重启后状态会串。想要「关机就是真关机」就得关。",
            script: "Set-ItemProperty -Path 'HKLM:\\SYSTEM\\CurrentControlSet\\Control\\Session Manager\\Power' -Name 'HiberbootEnabled' -Value 0 -Type DWord -Force",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "keepSession",
            title: "断开连接时保留会话（不注销）",
            reason: "如果被组策略改成「断开后注销」，服务器上跑的程序全没了 —— 现象跟睡眠一样，但根因完全不同。",
            script: "Set-ItemProperty -Path 'HKLM:\\SOFTWARE\\Policies\\Microsoft\\Windows NT\\Terminal Services' -Name 'MaxDisconnectionTime' -Value 0 -Type DWord -Force; Set-ItemProperty -Path 'HKLM:\\SOFTWARE\\Policies\\Microsoft\\Windows NT\\Terminal Services' -Name 'fSingleSessionPerUser' -Value 0 -Type DWord -Force",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "disableSleepLock",
            title: "禁用「休眠」按钮与睡眠锁",
            reason: "让误点的人无法把服务器弄睡着。",
            script: "Set-ItemProperty -Path 'HKLM:\\SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Policies\\Explorer' -Name 'NoSuspend' -Value 1 -Type DWord -Force; powercfg -hibernate off",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "rdpKeepAliveTask",
            title: "建一个每分钟的保活计划任务",
            reason: "有些云主机会因为「无流量」被判定为空闲而回收实例。每分钟做点事（写个时间戳）就能证明机器还在被使用 —— 这是云主机被自动关停时唯一的软件层对策。",
            script: "schtasks /create /tn \"NetworkManager-KeepAlive\" /tr \"cmd /c echo %date% %time% > %TEMP%\\\\nm-keepalive.txt\" /sc minute /mo 1 /ru SYSTEM /f",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "wakeOnLan",
            title: "启用网络唤醒（WOL）",
            reason: "让机器能被网络包唤醒 —— 关机状态下也能远程把它叫醒。注意：这个只管「唤醒」，管不了「保持开机」。",
            script: "powercfg -deviceenablewake \"$((Get-NetAdapter | Where-Object { $_.Status -eq 'Up' } | Select-Object -First 1).Name)\"; Set-NetAdapterPowerManagement -Name \"$((Get-NetAdapter | Where-Object { $_.Status -eq 'Up' } | Select-Object -First 1).Name)\" -WakeOnMagicPacket $true -WakeOnPattern $true",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
    ]
}

/// Linux 的软件层保活措施。
pub fn linux_keep_alive() -> Vec<KeepAliveItem> {
    vec![
        KeepAliveItem {
            id: "noSleep",
            title: "禁止挂起与休眠",
            reason: "笔记本合盖就睡、systemd-logind 的 IdleAction 会休眠 —— 服务器上这两条都要堵死。",
            script: "systemctl mask sleep.target suspend.target hibernate.target hybrid-sleep.target; systemctl set-property org.freedesktop.login1 handle-idle-session power-save=false",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "screenOff",
            title: "关闭屏幕保护与显示器超时",
            reason: "对应 Windows 的息屏。X11 上是 xset，GNOME 上是 gsettings，两边都设。",
            script: "export DISPLAY=:0; xset s off 2>/dev/null; xset s noblank 2>/dev/null; gsettings set org.gnome.desktop.session idle-delay 0 2>/dev/null; gsettings set org.gnome.desktop.screensaver lock-enabled false 2>/dev/null",
            need_admin: false,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "maskSleepConf",
            title: "清掉 logind 的休眠配置",
            reason: "有些镜像的 logind 配置里写着「空闲 10 分钟休眠」，mask 目标不够，得把配置也改掉。",
            script: "sed -i 's/^#*HandleLidSwitch=.*/HandleLidSwitch=ignore/' /etc/systemd/logind.conf 2>/dev/null; sed -i 's/^#*IdleAction=.*/IdleAction=ignore/' /etc/systemd/logind.conf 2>/dev/null; sed -i 's/^#*IdleActionSec=.*/IdleActionSec=0/' /etc/systemd/logind.conf 2>/dev/null",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
        KeepAliveItem {
            id: "keepAliveTimer",
            title: "建一个每分钟的保活定时器",
            reason: "同 Windows 的思路：证明机器还在被使用，避免被云平台的空闲检测回收。",
            script: "systemd-run --unit=nm-keepalive --on-active=1min --on-unit-active=1min --timer-property=Persistent=true /bin/sh -c 'date >> /tmp/nm-keepalive.txt'",
            need_admin: true,
            layer: "software",
            need_restart: false,
        },
    ]
}

/// 硬件层（必须去机房或云控制台做）—— 软件管不了这部分。
///
/// 这部分不给命令，只给指引。假装能远程搞定「来电自启」是骗人的。
pub fn hardware_layer_notes() -> Value {
    json!([
        {
            "id": "acPowerRestore",
            "title": "来电 / 恢复供电后自动开机",
            "why": "系统一旦开始关机流程，软件拦不住。要在真的断电后自动回来，只能靠 BIOS/UEFI 或 IPMI/BMC。",
            "where": "BIOS/UEFI 里的「Restore on AC Power Loss」或「AC Back」设为 Power On / 开机。虚拟机则要看宿主机或云平台的设置。",
        },
        {
            "id": "watchdog",
            "title": "硬件看门狗（Watchdog）",
            "why": "系统卡死时看门狗会强制重启机器，相当于「保证它一直在跑」。",
            "where": "IPMI/BMC 的 Watchdog 选项，或虚拟化平台的看门狗设置。",
        },
        {
            "id": "cloudAutoRestart",
            "title": "云主机：停止实例后自动重启",
            "why": "云平台判定实例空闲后会自动停机（这跟 Windows 自己的电源设置无关）。",
            "where": "云厂商控制台里的「实例空闲自动停止 / 停止后自动重启」类开关。腾讯云、阿里云、AWS 都有这一项。",
        },
        {
            "id": "powerSupply",
            "title": "电源与散热",
            "why": "机器会因为过热或电源波动自己断电，这跟系统设置无关。",
            "where": "机房侧：检查散热、电源冗余（UPS）。",
        }
    ])
}

/// 检查保活措施当前的状态。
///
/// 返回每一条的「是否已达标」，界面上直接标绿/标橙。
pub async fn check_windows_keep_alive(
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
  try { return [string](Get-ItemProperty -Path $path -Name $name -ErrorAction Stop).$name } catch { return 'NA' }
}
[Console]::Out.Write('NMKA:sleep=' + (Read-PowerValue 'SUB_SLEEP' 'STANDBYIDLE'))
[Console]::Out.Write('NMKA:video=' + (Read-PowerValue 'SUB_VIDEO' 'VIDEOIDLE'))
[Console]::Out.Write('NMKA:disk=' + (Read-PowerValue 'SUB_DISK' 'DISKIDLE'))
[Console]::Out.Write('NMKA:hibFile=' + (Read-RegValue 'HKLM:\SYSTEM\CurrentControlSet\Control\Power' 'HibernateEnabled'))
[Console]::Out.Write('NMKA:fastStartup=' + (Read-RegValue 'HKLM:\SYSTEM\CurrentControlSet\Control\Session Manager\Power' 'HiberbootEnabled'))
[Console]::Out.Write('NMKA:maxDiscTime=' + (Read-RegValue 'HKLM:\SOFTWARE\Policies\Microsoft\Windows NT\Terminal Services' 'MaxDisconnectionTime'))
[Console]::Out.Write('NMKA:singleSession=' + (Read-RegValue 'HKLM:\SYSTEM\CurrentControlSet\Control\Terminal Server' 'fSingleSessionPerUser'))
[Console]::Out.Write('NMKA:noSuspend=' + (Read-RegValue 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\Explorer' 'NoSuspend'))
$task = schtasks /query /tn "NetworkManager-KeepAlive" 2>$null | Out-String
if ($task -match 'NetworkManager-KeepAlive') { [Console]::Out.Write('NMKA:keepAliveTask=1') } else { [Console]::Out.Write('NMKA:keepAliveTask=0') }
"#;
    let out = crate::winsrv::run_powershell(profile, credential, script).await?;
    let mut map: Vec<(String, String)> = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if let Some(body) = line.strip_prefix("NMKA:") {
            if let Some((key, value)) = body.split_once('=') {
                map.push((key.trim().to_string(), value.trim().to_string()));
            }
        }
    }
    let get = |key: &str| -> String {
        map.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let num = |key: &str| -> i64 { get(key).parse::<i64>().unwrap_or(-1) };

    // 各项的达标判定
    let sleep_ok = num("sleep") == 0;
    let video_ok = num("video") == 0;
    let disk_ok = num("disk") == 0;
    let hib_ok = get("hibFile") == "0";
    let fast_ok = get("fastStartup") == "0";
    // MaxDisconnectionTime 为 0 或未设置（默认就是保留会话）都算达标
    let disc_time = get("maxDiscTime");
    let keep_session_ok = disc_time.is_empty() || disc_time == "0" || disc_time == "NA";
    let single_ok = get("singleSession") == "0";
    let no_suspend_ok = get("noSuspend") == "1";
    let task_ok = get("keepAliveTask") == "1";

    Ok(json!({
        "items": [
            { "id": "sleep", "ok": sleep_ok, "value": power_text(num("sleep")) },
            { "id": "video", "ok": video_ok, "value": power_text(num("video")) },
            { "id": "hdd", "ok": disk_ok, "value": power_text(num("disk")) },
            { "id": "hibernateFile", "ok": hib_ok, "value": if hib_ok { "已关闭" } else { "已启用" } },
            { "id": "fastStartup", "ok": fast_ok, "value": if fast_ok { "已关闭" } else { "已启用" } },
            { "id": "keepSession", "ok": keep_session_ok, "value": if keep_session_ok { "断开后保留会话".to_string() } else { format!("{} 秒后注销", disc_time) } },
            { "id": "singleSession", "ok": single_ok, "value": if single_ok { "允许多会话" } else { "每用户单会话" } },
            { "id": "disableSleepLock", "ok": no_suspend_ok, "value": if no_suspend_ok { "已禁用休眠按钮" } else { "未禁用" } },
            { "id": "rdpKeepAliveTask", "ok": task_ok, "value": if task_ok { "已建立" } else { "未建立" } },
        ],
    }))
}

/// Linux 侧的保活状态检查。
pub async fn check_linux_keep_alive(
    profile: &crate::models::SshServerProfile,
    credential: &str,
) -> Result<Value, String> {
    let script = r#"
if systemctl is-enabled sleep.target 2>/dev/null | grep -q masked; then echo "NMKA:sleep=masked"; else echo "NMKA:sleep=notmasked"; fi
if systemctl is-enabled suspend.target 2>/dev/null | grep -q masked; then echo "NMKA:suspend=masked"; else echo "NMKA:suspend=notmasked"; fi
if grep -qE '^IdleAction=ignore' /etc/systemd/logind.conf 2>/dev/null; then echo "NMKA:idle=ignore"; else echo "NMKA:idle=unknown"; fi
if systemctl list-timers 2>/dev/null | grep -q nm-keepalive; then echo "NMKA:keepAlive=1"; else echo "NMKA:keepAlive=0"; fi
if [ -n "$DISPLAY" ]; then echo "NMKA:display=$DISPLAY"; else echo "NMKA:display="; fi
"#;
    let out = crate::winsrv::run_shell(profile, credential, script).await?;
    let mut map: Vec<(String, String)> = Vec::new();
    for line in out.lines() {
        let line = line.trim();
        if let Some(body) = line.strip_prefix("NMKA:") {
            if let Some((key, value)) = body.split_once('=') {
                map.push((key.trim().to_string(), value.trim().to_string()));
            }
        }
    }
    let get = |key: &str| -> String {
        map.iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let sleep_masked = get("sleep") == "masked";
    let suspend_masked = get("suspend") == "masked";
    let idle_ignore = get("idle") == "ignore";
    let task_ok = get("keepAlive") == "1";
    Ok(json!({
        "items": [
            { "id": "noSleep", "ok": sleep_masked && suspend_masked, "value": format!("睡眠目标 {} / 挂起目标 {}", if sleep_masked { "已屏蔽" } else { "未屏蔽" }, if suspend_masked { "已屏蔽" } else { "未屏蔽" }) },
            { "id": "maskSleepConf", "ok": idle_ignore, "value": if idle_ignore { "IdleAction=ignore" } else { "未设置" } },
            { "id": "keepAliveTimer", "ok": task_ok, "value": if task_ok { "已建立" } else { "未建立" } },
        ],
    }))
}

fn power_text(seconds: i64) -> String {
    if seconds < 0 {
        "读取失败".to_string()
    } else if seconds == 0 {
        "从不".to_string()
    } else {
        format!("{} 分钟", seconds / 60)
    }
}

/// 保活清单转成前端要的 JSON。
pub fn keep_alive_to_json(items: &[KeepAliveItem]) -> Value {
    json!(items
        .iter()
        .map(|item| {
            json!({
                "id": item.id,
                "title": item.title,
                "reason": item.reason,
                "script": item.script,
                "needAdmin": item.need_admin,
                "layer": item.layer,
                "needRestart": item.need_restart,
            })
        })
        .collect::<Vec<Value>>())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 电源值转人话() {
        assert_eq!(power_text(0), "从不");
        assert_eq!(power_text(1800), "30 分钟");
        assert_eq!(power_text(-1), "读取失败");
    }

    #[test]
    fn windows保活清单覆盖了所有层() {
        let items = windows_keep_alive();
        let ids: Vec<&str> = items.iter().map(|i| i.id).collect();
        // 软件层必须有的
        assert!(ids.contains(&"sleep"), "缺睡眠设置");
        assert!(ids.contains(&"video"), "缺息屏设置");
        assert!(ids.contains(&"hibernateFile"), "缺休眠文件");
        assert!(ids.contains(&"fastStartup"), "缺快速启动");
        assert!(ids.contains(&"keepSession"), "缺会话保留");
        assert!(ids.contains(&"rdpKeepAliveTask"), "缺保活计划任务");
        // 全部是软件层，硬件层单独给
        assert!(items.iter().all(|i| i.layer == "software"));
    }

    #[test]
    fn linux保活清单合理() {
        let items = linux_keep_alive();
        let ids: Vec<&str> = items.iter().map(|i| i.id).collect();
        assert!(ids.contains(&"noSleep"));
        assert!(ids.contains(&"keepAliveTimer"));
        assert!(items.iter().all(|i| i.layer == "software"));
    }

    #[test]
    fn 硬件层必须说明是软件搞不定的() {
        let notes = hardware_layer_notes();
        let list = notes.as_array().expect("应是数组");
        assert!(!list.is_empty());
        // 每条都要有「去哪儿配」的说明 —— 硬件层给不出命令，给指引就够
        for item in list {
            assert!(item.get("where").and_then(|v| v.as_str()).is_some());
            assert!(item.get("why").and_then(|v| v.as_str()).is_some());
        }
    }

    #[test]
    fn 保持开机清单的每条都有原因说明() {
        for item in windows_keep_alive().into_iter().chain(linux_keep_alive()) {
            assert!(!item.reason.is_empty(), "{} 缺原因说明", item.id);
            assert!(!item.title.is_empty(), "{} 缺标题", item.id);
            assert!(!item.script.is_empty(), "{} 缺命令", item.id);
        }
    }
}
