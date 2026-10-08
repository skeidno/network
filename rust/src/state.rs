use serde_json::{json, Value};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use crate::core::{memory_mb, AppState, NodeDelay};
use crate::models::{
    server_proxy_port_error, AppConfig, RoutingRule, COMMON_OVERSEAS_GROUP,
    DEFAULT_PROXY_DOMAINS,
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

fn label(table: &[(&str, &str)], key: &str) -> String {
    table
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, text)| (*text).to_string())
        .unwrap_or_else(|| key.to_string())
}

fn is_common_overseas_rule(rule: &RoutingRule) -> bool {
    if rule.group == COMMON_OVERSEAS_GROUP {
        return true;
    }
    if rule.rule_type != "DOMAIN-SUFFIX" {
        return false;
    }
    let value = crate::mihomo::normalize_rule_value(&rule.rule_type, &rule.value);
    DEFAULT_PROXY_DOMAINS
        .iter()
        .any(|(domain, _)| *domain == value)
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

fn rule_states(config: &AppConfig) -> Vec<Value> {
    let common: Vec<(usize, &RoutingRule)> = config
        .rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| is_common_overseas_rule(rule))
        .collect();
    let common_indexes: std::collections::HashSet<usize> =
        common.iter().map(|(index, _)| *index).collect();

    let mut states: Vec<Value> = Vec::new();
    let mut group_added = false;
    for (index, rule) in config.rules.iter().enumerate() {
        if common_indexes.contains(&index) {
            if group_added {
                continue;
            }
            group_added = true;
            let targets: std::collections::HashSet<&str> =
                common.iter().map(|(_, item)| item.target.as_str()).collect();
            let target = if targets.len() == 1 {
                common
                    .first()
                    .map(|(_, item)| item.target.clone())
                    .unwrap_or_default()
            } else {
                "MIXED".to_string()
            };
            let enabled_count = common.iter().filter(|(_, item)| item.enabled).count();
            let entries: Vec<Value> = common
                .iter()
                .map(|(_, item)| {
                    json!({
                        "domain": item.value,
                        "label": item.note,
                        "enabled": item.enabled,
                    })
                })
                .collect();
            let detail = common
                .iter()
                .map(|(_, item)| item.value.clone())
                .collect::<Vec<_>>()
                .join("、");
            let default_entries: Vec<Value> = DEFAULT_PROXY_DOMAINS
                .iter()
                .map(|(domain, _)| json!(domain))
                .collect();
            states.push(json!({
                "kind": "group",
                "groupId": COMMON_OVERSEAS_GROUP,
                "enabled": enabled_count == common.len(),
                "partiallyEnabled": 0 < enabled_count && enabled_count < common.len(),
                "ruleType": "GROUP",
                "ruleTypeLabel": label(RULE_LABELS, "GROUP"),
                "value": format!("{} 条匹配", common.len()),
                "detail": detail,
                "entries": entries,
                "defaultEntries": default_entries,
                "target": target,
                "targetLabel": if targets.len() == 1 { label(TARGET_LABELS, &target) } else { "多个去向".to_string() },
                "note": "常用海外站点",
                "count": common.len(),
            }));
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

    let relay_entries: Vec<Value> = config
        .imported_nodes
        .iter()
        .filter(|node| {
            node.protocol().to_lowercase() == "http"
                && !node.source_id.starts_with("server-deployment:")
                && !node
                    .config
                    .get("server")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .is_empty()
        })
        .map(|node| {
            let relay = node.dialer_proxy();
            let policy = node.dialer_policy();
            json!({
                "node": node.name(),
                "endpoint": node.config.get("server").and_then(|v| v.as_str()).unwrap_or(""),
                "port": node.config.get("port").and_then(|v| v.as_i64()).unwrap_or(0),
                "relay": relay.clone(),
                "policy": if policy.is_empty() { if relay.is_empty() { "direct".to_string() } else { "manual".to_string() } } else { policy },
            })
        })
        .collect();

    if !relay_entries.is_empty() {
        let relays: Vec<String> = relay_entries
            .iter()
            .filter_map(|entry| entry.get("relay").and_then(|v| v.as_str()))
            .filter(|value| !value.is_empty())
            .map(|value| value.to_string())
            .collect::<std::collections::BTreeSet<_>>()
            .into_iter()
            .collect();
        let configured_count = relay_entries
            .iter()
            .filter(|entry| {
                entry
                    .get("relay")
                    .and_then(|v| v.as_str())
                    .map(|value| !value.is_empty())
                    .unwrap_or(false)
            })
            .count();
        let automatic_count = relay_entries
            .iter()
            .filter(|entry| entry.get("policy").and_then(|v| v.as_str()) == Some("auto"))
            .count();
        let detail = relay_entries
            .iter()
            .filter_map(|entry| entry.get("endpoint").and_then(|v| v.as_str()))
            .collect::<Vec<_>>()
            .join("、");
        states.push(json!({
            "kind": "relay",
            "enabled": configured_count == relay_entries.len(),
            "partiallyEnabled": 0 < configured_count && configured_count < relay_entries.len(),
            "automatic": automatic_count == relay_entries.len(),
            "ruleType": "PROXY-ENDPOINT",
            "ruleTypeLabel": label(RULE_LABELS, "PROXY-ENDPOINT"),
            "value": format!("{} 个代理入口", relay_entries.len()),
            "detail": detail,
            "entries": relay_entries,
            "target": if relays.len() == 1 { relays[0].clone() } else if relays.is_empty() { "DIRECT".to_string() } else { "MULTIPLE".to_string() },
            "targetLabel": if relays.len() == 1 { format!("经 {}", relays[0]) } else if relays.is_empty() { "直连".to_string() } else { format!("经 {} 个前置节点", relays.len()) },
            "note": "第 2 阶段；可编辑，代码直接使用代理域名时同样生效",
            "count": relay_entries.len(),
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
                "dialerProxy": node.dialer_proxy(),
                "dialerPolicy": node.dialer_policy(),
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
            json!({
                "profileId": profile.profile_id,
                "name": profile.name,
                "region": profile.region,
                "host": profile.host,
                "port": profile.port,
                "username": profile.username,
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
            })
        })
        .collect();

    let toasts: Vec<Value> = std::mem::take(&mut state.toasts);

    json!({
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": {
            "platform": std::env::consts::OS,
            "headless": state.headless,
            "sshDeployment": cfg!(windows),
            "browserFiles": false,
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
            "downloadRate": "0 B/s",
            "uploadRate": "0 B/s",
            "downloadTotal": "0 B",
            "uploadTotal": "0 B",
            "connections": "0",
            "downloadSamples": vec![0.0f64; 60],
            "uploadSamples": vec![0.0f64; 60],
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
        "runningProcesses": running_process_names(),
        "importing": state.importing,
        "settings": {
            "mixedPort": config.mixed_port,
            "controllerPort": config.controller_port,
            "dnsPort": config.dns_port,
            "serverProxyPort": config.server_proxy_port,
            "strictRoute": config.strict_route,
            "startOnLaunch": config.start_on_launch,
            "closeToTray": config.close_to_tray,
            "startWithWindows": config.start_with_windows,
        },
        "toasts": toasts,
    })
}
