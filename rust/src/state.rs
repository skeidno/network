use serde_json::{json, Value};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::core::{memory_mb, AppState, NodeDelay};
use crate::models::{
    server_proxy_port_error, AppConfig, RoutingRule, COMMON_CHINA_GROUP,
    COMMON_OVERSEAS_GROUP,
};

pub const MODE_LABELS: &[(&str, &str)] = &[
    ("RULE", "规则分流"),
    ("GLOBAL_CLASH", "全局 Clash"),
    ("GLOBAL_V2RAY", "全局 v2ray"),
    ("GLOBAL_SSH", "全局自建服务器"),
    ("GLOBAL_BUILTIN", "全局内置节点"),
    ("SMART", "自动选优"),
    ("DIRECT", "直连"),
];

const TARGET_LABELS: &[(&str, &str)] = &[
    ("CLASH", "Clash 本地端口"),
    ("V2RAY", "v2ray 本地端口"),
    ("SSH", "SSH 服务器出口"),
    ("BUILTIN", "内置节点组"),
    ("DIRECT", "直连"),
];

const RULE_LABELS: &[(&str, &str)] = &[
    ("PROCESS-NAME", "程序"),
    ("DOMAIN", "完整域名"),
    ("DOMAIN-SUFFIX", "域名后缀"),
    ("DOMAIN-KEYWORD", "域名关键词"),
    ("IP-CIDR", "IP / CIDR"),
    ("GROUP", "预置规则组"),
    ("PROXY-ENDPOINT", "代理域名前置"),
];

/// 字节数转成人看的单位。界面上的「↑ 0.00 / ↓ 0.00」要的是数值和单位分开的文本
/// （`12.3 KB/s`），所以这里只负责拼成「数值 + 空格 + 单位」。
fn format_bytes(value: f64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut amount = value.max(0.0);
    let mut index = 0;
    while amount >= 1024.0 && index < UNITS.len() - 1 {
        amount /= 1024.0;
        index += 1;
    }
    let precision = if index == 0 || amount >= 100.0 {
        0
    } else if amount >= 10.0 {
        1
    } else {
        2
    };
    format!("{amount:.precision$} {}", UNITS[index])
}

fn format_rate(bytes_per_second: f64) -> String {
    format!("{}/s", format_bytes(bytes_per_second))
}

fn label(table: &[(&str, &str)], key: &str) -> String {
    table
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, text)| (*text).to_string())
        .unwrap_or_else(|| key.to_string())
}

fn port_is_open(host: &str, port: i32) -> bool {
    let Ok(mut addresses) = format!("{host}:{port}").to_socket_addrs() else {
        return false;
    };
    addresses.any(|address| {
        TcpStream::connect_timeout(&address, Duration::from_millis(250)).is_ok()
    })
}

fn source_state(name: &str, protocol: &str, host: &str, port: i32, enabled: bool) -> Value {
    let available = enabled && port_is_open(host, port);
    json!({
        "enabled": enabled,
        "protocol": protocol,
        "host": host,
        "port": port,
        "endpoint": format!("{host}:{port}"),
        "status": if available { "正在监听" } else if !enabled { "已禁用" } else { "未监听" },
        "available": available,
        "name": name,
    })
}

/// 内置规则组的定义：判定函数 + 展示名 + 默认域名清单。
struct BuiltinGroup {
    id: &'static str,
    name: &'static str,
    members: fn(&RoutingRule) -> bool,
}

const BUILTIN_GROUPS: [BuiltinGroup; 2] = [
    BuiltinGroup {
        id: COMMON_CHINA_GROUP,
        name: "常用国内站点",
        members: crate::portable::is_common_china_rule,
    },
    BuiltinGroup {
        id: COMMON_OVERSEAS_GROUP,
        name: "常用海外站点",
        members: crate::portable::is_common_overseas_rule,
    },
];

/// 把属于内置组的若干条规则合成界面上的一行「整组」。
///
/// 组是写进配置文件的真实规则，不是每次启动算出来的派生数据，所以这里只是
/// 把它们聚在一起展示：整组开关、整组换出口、整组编辑域名都作用在这些成员上。
fn group_state(group: &BuiltinGroup, members: &[(usize, &RoutingRule)]) -> Value {
    let targets: std::collections::HashSet<&str> =
        members.iter().map(|(_, item)| item.target.as_str()).collect();
    let target = if targets.len() == 1 {
        members
            .first()
            .map(|(_, item)| item.target.clone())
            .unwrap_or_default()
    } else {
        "MIXED".to_string()
    };
    let enabled_count = members.iter().filter(|(_, item)| item.enabled).count();
    let entries: Vec<Value> = members
        .iter()
        .map(|(_, item)| {
            json!({
                "domain": item.value,
                "label": item.note,
                "enabled": item.enabled,
            })
        })
        .collect();
    let detail = members
        .iter()
        .map(|(_, item)| item.value.clone())
        .collect::<Vec<_>>()
        .join("、");
    let default_entries: Vec<Value> = crate::portable::builtin_group_defaults(group.id)
        .into_iter()
        .map(|domain| json!(domain))
        .collect();
    json!({
        "kind": "group",
        "groupId": group.id,
        "enabled": enabled_count == members.len(),
        "partiallyEnabled": 0 < enabled_count && enabled_count < members.len(),
        "ruleType": "GROUP",
        "ruleTypeLabel": label(RULE_LABELS, "GROUP"),
        "value": format!("{} 条匹配", members.len()),
        "detail": detail,
        "entries": entries,
        "defaultEntries": default_entries,
        "target": target,
        "targetLabel": if targets.len() == 1 { label(TARGET_LABELS, &target) } else { "多个去向".to_string() },
        "note": group.name,
        "count": members.len(),
    })
}

fn rule_states(config: &AppConfig) -> Vec<Value> {
    // 每个内置组各自的成员下标；一行组内规则在界面上只占一行。
    let grouped: Vec<(&BuiltinGroup, Vec<(usize, &RoutingRule)>)> = BUILTIN_GROUPS
        .iter()
        .map(|group| {
            let members = config
                .rules
                .iter()
                .enumerate()
                .filter(|(_, rule)| (group.members)(rule))
                .collect();
            (group, members)
        })
        .collect();
    let mut group_added: Vec<&str> = Vec::new();

    let mut states: Vec<Value> = Vec::new();
    for (index, rule) in config.rules.iter().enumerate() {
        let mut handled = false;
        for (group, members) in &grouped {
            if !members.iter().any(|(member_index, _)| *member_index == index) {
                continue;
            }
            handled = true;
            if members.is_empty() || group_added.contains(&group.id) {
                continue;
            }
            group_added.push(group.id);
            states.push(group_state(group, members));
        }
        if handled {
            continue;
        }
        states.push(json!({
            "kind": "rule",
            "index": index,
            "enabled": rule.enabled,
            "partiallyEnabled": false,
            "ruleType": rule.rule_type,
            "ruleTypeLabel": label(RULE_LABELS, &rule.rule_type),
            "value": rule.value,
            "target": rule.target,
            "targetLabel": label(TARGET_LABELS, &rule.target),
            "note": rule.note,
            "group": rule.group,
        }));
    }

    states
}

fn running_process_names() -> Vec<String> {
    use sysinfo::{ProcessRefreshKind, RefreshKind, System};
    let mut system = System::new_with_specifics(
        RefreshKind::new().with_processes(ProcessRefreshKind::new()),
    );
    system.refresh_processes(sysinfo::ProcessesToUpdate::All);
    let mut names: std::collections::BTreeMap<String, String> = std::collections::BTreeMap::new();
    for process in system.processes().values() {
        let name = process.name().to_string_lossy().trim().to_string();
        if !name.is_empty() {
            names.insert(name.to_lowercase(), name);
        }
    }
    names.into_values().take(600).collect()
}

pub fn build(state: &mut AppState) -> Value {
    // 拉状态时顺手采一次流量：界面就是靠这一路把速度、累计和曲线刷出来的。
    state.sample_traffic();
    let config = &state.config;
    let running = state.core.is_running();
    let busy = state.busy;
    let core_status = if busy {
        "处理中"
    } else if running {
        "接管中"
    } else {
        "已停止"
    };
    let selected_ssh = config
        .ssh_servers
        .iter()
        .find(|profile| profile.profile_id == config.selected_ssh_server);

    let server_regions: Vec<(String, String)> = config
        .ssh_servers
        .iter()
        .map(|profile| {
            (
                format!("server-deployment:{}", profile.profile_id),
                profile.region.clone(),
            )
        })
        .collect();

    let relay_active = crate::mihomo::relay_is_active(config);

    let nodes: Vec<Value> = config
        .imported_nodes
        .iter()
        .enumerate()
        .map(|(index, node)| {
            let server = node
                .config
                .get("server")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let port = node.config.get("port").and_then(|v| v.as_i64());
            let delay = state
                .node_delays
                .get(&node.name())
                .cloned()
                .unwrap_or(NodeDelay {
                    status: "idle".into(),
                    delay: None,
                    message: String::new(),
                });
            let is_server_deployment = node.source_id.starts_with("server-deployment:");
            // 这个节点的连接（包括测速）是不是要绕一遍中转：
            // 中转自己不用绕，server 落在本机的节点也不绕（远端中转连不到我们本机）。
            let via_relay = relay_active
                && node.name() != config.relay_node
                && !crate::mihomo::is_loopback_server(&server);
            let region = server_regions
                .iter()
                .find(|(source_id, _)| *source_id == node.source_id)
                .map(|(_, region)| region.clone())
                .unwrap_or_default();
            json!({
                "index": index,
                "name": node.name(),
                "protocol": node.protocol(),
                "server": match port { Some(port) => format!("{server}:{port}"), None => server },
                "source": node.source,
                "group": if !node.group.is_empty() { node.group.clone() }
                    else if is_server_deployment { "服务器部署".to_string() }
                    else if !node.source.is_empty() { node.source.clone() }
                    else { "手动导入".to_string() },
                "customGroup": node.group,
                "region": region,
                "selected": node.name() == config.selected_node,
                "latencyStatus": delay.status,
                "latency": delay.delay,
                "latencyMessage": delay.message,
                // 中转现在整份配置只有一个，挂在节点上的是哪个已经没有意义，
                // 但先留着这两个键，免得前端读 undefined 崩。
                "isRelay": node.name() == config.relay_node && config.relay_enabled,
                "viaRelay": via_relay,
            })
        })
        .collect();

    let subscriptions: Vec<Value> = config
        .subscriptions
        .iter()
        .enumerate()
        .map(|(index, source)| {
            let count = config
                .imported_nodes
                .iter()
                .filter(|node| node.source_id == source.source_id)
                .count();
            let host = url::Url::parse(&source.url)
                .ok()
                .map(|url| {
                    let netloc = match url.port() {
                        Some(port) => match url.host_str() {
                            Some(host) => format!("{host}:{port}"),
                            None => "订阅地址".to_string(),
                        },
                        None => url
                            .host_str()
                            .map(|host| host.to_string())
                            .unwrap_or_else(|| "订阅地址".to_string()),
                    };
                    netloc
                })
                .unwrap_or_else(|| "订阅地址".to_string());
            json!({
                "index": index,
                "name": source.name,
                "host": host,
                "nodeCount": count,
                "group": source.group,
                "lastUpdated": if source.last_updated.is_empty() { "未更新".to_string() } else { source.last_updated.clone() },
            })
        })
        .collect();

    let ssh_servers: Vec<Value> = config
        .ssh_servers
        .iter()
        .map(|profile| {
            let warning = if profile.deployed_node_id.is_empty()
                || profile.proxy_port == config.server_proxy_port
            {
                server_proxy_port_error(profile.proxy_port, profile.port)
            } else {
                format!(
                    "当前端口 {} 与默认部署端口不一致；点击检查服务将迁移到 {}",
                    profile.proxy_port, config.server_proxy_port
                )
            };
            // 前端靠这个字段显示「检查中／部署中」，以前后端没有它，
            // 状态列永远停在「已部署」，点了按钮看起来毫无反应。
            let task = state.deployments.get(&profile.profile_id);
            // 前端复制按钮的可点状态依赖 shareLink，但状态里从来没有这个字段，
            // 已部署服务器的复制按钮因此一直是灰的。按节点配置现算一份。
            let share_link = config
                .imported_nodes
                .iter()
                .find(|node| node.source_id == format!("server-deployment:{}", profile.profile_id))
                .map(|node| crate::ssh::shadowsocks_share_link(&node.config))
                .unwrap_or_default();
            json!({
                "profileId": profile.profile_id,
                "name": profile.name,
                "region": profile.region,
                "host": profile.host,
                "port": profile.port,
                "username": profile.username,
                "deployment": {
                    "status": task.map(|item| item.status.clone()).unwrap_or_else(|| "idle".into()),
                    "stage": task.map(|item| item.stage.clone()).unwrap_or_default(),
                    "error": task.map(|item| item.error.clone()).unwrap_or_default(),
                },
                "proxyPort": profile.proxy_port,
                "proxyPortWarning": warning,
                "proxyReachable": profile.proxy_reachable,
                "authMethod": profile.auth_method,
                "keyPath": profile.key_path,
                "rememberPassword": profile.remember_password,
                "hasCredential": crate::credentials::has(&profile.profile_id),
                "deployed": !profile.deployed_node_id.is_empty(),
                "deployedVersion": profile.deployed_version,
                "deployedAt": profile.deployed_at,
                "proxyReachabilityError": profile.proxy_reachability_error,
                "shareLink": share_link,
            })
        })
        .collect();

    let toasts: Vec<Value> = std::mem::take(&mut state.toasts);

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": {
            "platform": std::env::consts::OS,
            // Linux 上没有原生窗口：即便没带 --headless 启动，界面也应当按
            // 「Linux WebGUI」渲染（隐藏开机自启、关闭到托盘这类只有桌面端
            // 才有意义的设置）。
            "headless": state.headless || !cfg!(windows),
            "sshDeployment": cfg!(windows),
            // Rust 侧只有 importFileText / *PortableConfigText 这类「浏览器读文件、
            // 把内容发回来」的接口，没有原生文件对话框。非 Windows 必须让前端走
            // 浏览器选文件，否则它会去调不存在的 importFile。
            "browserFiles": !cfg!(windows),
        },
        "core": {
            "running": running,
            "busy": busy,
            "status": core_status,
            "admin": crate::platform::is_admin(),
            "mode": config.mode,
            "modeLabel": label(MODE_LABELS, &config.mode),
            "mixedPort": config.mixed_port,
        },
        "summary": {
            "processRules": config.rules.iter().filter(|r| r.enabled && r.rule_type == "PROCESS-NAME").count(),
            "networkRules": config.rules.iter().filter(|r| r.enabled && r.rule_type != "PROCESS-NAME").count(),
            "nodes": config.imported_nodes.len(),
            "defaultTarget": label(TARGET_LABELS, &config.default_target),
        },
        "sources": {
            "clash": source_state("clash", &config.clash.protocol, &config.clash.host, config.clash.port, config.clash.enabled),
            "v2ray": source_state("v2ray", &config.v2ray.protocol, &config.v2ray.host, config.v2ray.port, config.v2ray.enabled),
            "ssh": {
                "enabled": selected_ssh.map(|p| !p.deployed_node_id.is_empty()).unwrap_or(false),
                "endpoint": selected_ssh
                    .filter(|p| !p.deployed_node_id.is_empty())
                    .map(|p| format!("{}:{}", p.host, p.proxy_port))
                    .unwrap_or_else(|| "尚未配置".to_string()),
                "status": if selected_ssh.map(|p| !p.deployed_node_id.is_empty()).unwrap_or(false) { "已部署" } else { "未部署" },
                "available": selected_ssh.map(|p| !p.deployed_node_id.is_empty()).unwrap_or(false),
            },
        },
        "traffic": {
            "status": if running { "实时更新" } else { "接管停止" },
            "downloadRate": format_rate(state.traffic_download_rate),
            "uploadRate": format_rate(state.traffic_upload_rate),
            "downloadTotal": format_bytes(state.traffic_download_total as f64),
            "uploadTotal": format_bytes(state.traffic_upload_total as f64),
            "connections": state.traffic_connections.to_string(),
            "downloadSamples": state.traffic_download_samples,
            "uploadSamples": state.traffic_upload_samples,
            "memoryMb": memory_mb().round() as i64,
        },
        "exitIp": state.exit_ip,
        "exitIpLocation": state.exit_ip_location,
        "localIp": state.local_ip,
        "localIpLocation": state.local_ip_location,
        "rules": rule_states(config),
        "fallbackRule": {
            "target": config.default_target,
            "targetLabel": label(TARGET_LABELS, &config.default_target),
        },
        "nodes": nodes,
        "nodeGroups": config.node_groups,
        "subscriptions": subscriptions,
        "sshServers": ssh_servers,
        "selectedNode": config.selected_node,
        "relay": {
            "enabled": config.relay_enabled,
            "node": config.relay_node,
            // 开着但名字对不上节点列表时算「没生效」，界面要能看出这个区别。
            "active": crate::mihomo::relay_is_active(config),
        },
        "runningProcesses": running_process_names(),
        "importing": state.importing,
        "settings": {
            "mixedPort": config.mixed_port,
            "controllerPort": config.controller_port,
            "dnsPort": config.dns_port,
            "serverProxyPort": config.server_proxy_port,
            "strictRoute": config.strict_route,
            "dnsMode": config.dns_mode,
            "startOnLaunch": config.start_on_launch,
            "closeToTray": config.close_to_tray,
            "startWithWindows": config.start_with_windows,
            // 本地 DNS 映射：数组给界面显示条数和摘要，文本给编辑框做初值 ——
            // 回写文本这一步放在 Rust 里，两端（桌面端 / Android 端）的落盘格式才一致。
            "localDns": config.local_dns.iter().map(|entry| json!({
                "domain": entry.domain,
                "address": entry.address,
            })).collect::<Vec<Value>>(),
            "localDnsText": crate::models::local_dns_entries_to_text(&config.local_dns),
            // 本地证书状态：给设置页的证书卡片用。只读几个小文件，信任查询带缓存，
            // 放在每轮状态里也不会拖慢轮询。
            "localCert": crate::localca::status(config),
        },
        "toasts": toasts,
    })
}
