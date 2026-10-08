use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

use crate::core::AppState;
use crate::importers::{parse_import_content, unique_node_name};
use crate::models::{
    AppConfig, ImportedNode, RoutingRule, SshServerProfile, SubscriptionSource,
};
use crate::paths::core_log_path;

fn text(args: &[Value], index: usize) -> String {
    args.get(index)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

fn number(args: &[Value], index: usize) -> i64 {
    args.get(index).and_then(|value| value.as_i64()).unwrap_or(0)
}

#[allow(dead_code)]
fn flag(args: &[Value], index: usize) -> bool {
    args.get(index)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn payload(args: &[Value], index: usize) -> Result<Value, String> {
    let value = args.get(index).cloned().unwrap_or(Value::Null);
    match value {
        Value::String(raw) => serde_json::from_str(&raw).map_err(|_| "参数不是合法 JSON".into()),
        other => Ok(other),
    }
}

fn node_endpoint(node: &ImportedNode) -> (String, u16) {
    let host = node
        .config
        .get("server")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim_matches(|c| c == '[' || c == ']')
        .to_string();
    let port = node
        .config
        .get("port")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    (host, port.clamp(0, 65535) as u16)
}

/// 出口 IP 检测端点。全部并发发起，谁先返回就用谁。
///
/// 前三个是国内**不走代理也能访问**的站点：应用自身进程被内核规则
/// `PROCESS-NAME,NetworkManager.exe,DIRECT` 强制直连，如果只用 ipify /
/// Cloudflare 这类境外端点，国内网络下会被直接重置（`tls handshake eof`），
/// 直连出口就永远测不出来。后两个保留给境外环境和代理路径使用。
const IP_CHECKS: &[&str] = &[
    "https://myip.ipip.net",
    "http://ip.3322.net",
    "https://1.1.1.1/cdn-cgi/trace",
    "https://api.ipify.org?format=json",
    "http://api.ipify.org?format=json",
];

/// 直连检测本机出口 IP（明确禁用环境变量代理，避免被系统代理干扰）。
async fn exit_ip_direct() -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .no_proxy()
        .build()
        .map_err(|err| format!("创建 HTTP 客户端失败：{err}"))?;
    fetch_exit_ip(&client).await
}

/// 通过本地混合代理端口检测出口 IP —— 测的是「经过代理后的出口」。
async fn exit_ip_through_proxy(proxy_url: &str) -> Result<String, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .proxy(reqwest::Proxy::all(proxy_url).map_err(|err| format!("代理地址无效：{err}"))?)
        .build()
        .map_err(|err| format!("创建 HTTP 客户端失败：{err}"))?;
    fetch_exit_ip(&client).await
}

/// 所有端点同时发起，谁先拿到 IP 就用谁。
/// 串行轮询时每个端点都要等满超时，最坏要 60 秒；并发后最坏 12 秒。
async fn fetch_exit_ip(client: &reqwest::Client) -> Result<String, String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(IP_CHECKS.len());
    for endpoint in IP_CHECKS {
        let tx = tx.clone();
        let client = client.clone();
        tokio::spawn(async move {
            let outcome = probe_exit_endpoint(&client, endpoint).await;
            let _ = tx.send((endpoint, outcome)).await;
        });
    }
    drop(tx);

    let mut failures: Vec<String> = Vec::new();
    while let Some((endpoint, outcome)) = rx.recv().await {
        match outcome {
            Ok(address) => return Ok(address),
            Err(err) => failures.push(format!("{endpoint}: {err}")),
        }
    }
    let detail = failures.last().cloned().unwrap_or_else(|| "检测端点没有返回 IP".into());
    Err(format!("所有出口检测端点均失败：{detail}"))
}

async fn probe_exit_endpoint(client: &reqwest::Client, endpoint: &str) -> Result<String, String> {
    let response = client
        .get(endpoint)
        .send()
        .await
        .map_err(|err| {
            // reqwest 的错误外壳不带细节，展开整个 source 链才能看到
            // 是 DNS、连接超时还是 TLS 问题。
            let mut chain = vec![err.to_string()];
            let mut source = std::error::Error::source(&err);
            while let Some(cause) = source {
                chain.push(cause.to_string());
                source = cause.source();
            }
            chain.join(" ← ")
        })?
        .error_for_status()
        .map_err(|err| err.to_string())?;
    let text = response.text().await.map_err(|err| err.to_string())?;
    extract_ip(&text).ok_or_else(|| "响应中没有 IP".to_string())
}

/// 各端点返回格式五花八门，统一按三种形态解析：
/// JSON（`{"ip":"1.2.3.4"}` / 搜狐的 `{"cip":"1.2.3.4"}`）、
/// Cloudflare trace（`ip=1.2.3.4`）、
/// 纯文本（ipip.net 的「当前 IP：1.2.3.4 来自于…」、淘宝的 `ipCallback({ip:"1.2.3.4"})`）。
fn extract_ip(text: &str) -> Option<String> {
    if let Ok(value) = serde_json::from_str::<Value>(text) {
        for key in ["ip", "cip"] {
            if let Some(ip) = value.get(key).and_then(|v| v.as_str()) {
                if is_public_ip(ip.trim()) {
                    return Some(ip.trim().to_string());
                }
            }
        }
    }
    for line in text.lines() {
        if let Some(ip) = line.strip_prefix("ip=") {
            if is_public_ip(ip.trim()) {
                return Some(ip.trim().to_string());
            }
        }
    }
    first_ipv4(text)
}

/// 在文本里找第一个公网 IPv4 地址。
fn first_ipv4(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    for start in 0..bytes.len() {
        if let Some((ip, _)) = parse_ipv4_at(bytes, start) {
            if is_public_ip(&ip) {
                return Some(ip);
            }
        }
    }
    None
}

/// 排除不可能代表出口身份的地址：
/// 私有网段、回环、链路本地、CGNAT，以及文档保留段。
/// 198.18/15 是 mihomo 的 fake-ip 段，尤其不能当成出口 IP 显示出来。
fn is_public_ip(ip: &str) -> bool {
    let octets: Vec<u8> = ip
        .split('.')
        .map(|part| part.parse::<u8>().ok())
        .collect::<Option<Vec<u8>>>()
        .unwrap_or_default();
    if octets.len() != 4 {
        return false;
    }
    let [a, b, _, _] = [octets[0], octets[1], octets[2], octets[3]];
    !matches!(
        (a, b),
        (0, _)
            | (10, _)
            | (127, _)
            | (169, 254)
            | (172, 16..=31)
            | (192, 168)
            | (100, 64..=127)
            | (198, 18..=19)
            | (192, 0)
            | (198, 51)
            | (203, 0)
            | (224..=255, _)
    )
}

/// 从 `start` 起尝试解析一个 IPv4；不符合就返回 None。
/// 前后若还是数字或点则不认，避免把更长数字串的片段当成地址。
fn parse_ipv4_at(bytes: &[u8], start: usize) -> Option<(String, usize)> {
    if start > 0 && (bytes[start - 1].is_ascii_digit() || bytes[start - 1] == b'.') {
        return None;
    }
    let mut pos = start;
    let mut octets = [0u16; 4];
    for index in 0..4 {
        let digit_start = pos;
        let mut value: u16 = 0;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() && value <= 255 {
            value = value * 10 + (bytes[pos] - b'0') as u16;
            pos += 1;
        }
        let digits = pos - digit_start;
        if digits == 0 || digits > 3 || value > 255 {
            return None;
        }
        octets[index] = value;
        if index < 3 {
            if pos >= bytes.len() || bytes[pos] != b'.' {
                return None;
            }
            pos += 1;
        }
    }
    if pos < bytes.len() && (bytes[pos].is_ascii_digit() || bytes[pos] == b'.') {
        return None;
    }
    Some((
        format!("{}.{}.{}.{}", octets[0], octets[1], octets[2], octets[3]),
        pos,
    ))
}

async fn measure_delay(host: &str, port: u16) -> Result<i64, String> {    if host.is_empty() || port == 0 {
        return Err("节点缺少地址或端口".into());
    }
    let address = format!("{host}:{port}");
    let start = Instant::now();
    let stream = tokio::time::timeout(Duration::from_millis(3000), TcpStream::connect(&address))
        .await
        .map_err(|_| "连接超时".to_string())?
        .map_err(|err| format!("连接失败：{err}"))?;
    drop(stream);
    Ok(start.elapsed().as_millis() as i64)
}

/// 常用海外站点规则组在 `rules` 中的下标。
fn common_rule_indexes(state: &AppState) -> Vec<usize> {
    state
        .config
        .rules
        .iter()
        .enumerate()
        .filter(|(_, rule)| crate::portable::is_common_overseas_rule(rule))
        .map(|(index, _)| index)
        .collect()
}

/// 清掉指向已删除节点的中转引用。
pub fn clear_node_dialer_references(nodes: &mut [ImportedNode], removed: &[String]) {
    for node in nodes.iter_mut() {
        let dialer = node.dialer_proxy();
        if !dialer.is_empty() && removed.iter().any(|name| name == &dialer) {
            if let Value::Object(map) = &mut node.config {
                map.remove(crate::models::NODE_DIALER_PROXY_KEY);
                map.remove(crate::models::NODE_DIALER_POLICY_KEY);
            }
        }
    }
}

/// 当前选中节点被删掉后，回落到第一个可用节点。
pub fn repair_selected_node(state: &mut AppState, removed: &[String]) {
    if removed
        .iter()
        .any(|name| *name == state.config.selected_node)
    {
        state.config.selected_node = state
            .config
            .imported_nodes
            .first()
            .map(|node| node.name())
            .unwrap_or_default();
    }
}

fn subscription_from_url(name: &str, url: &str, group: &str) -> SubscriptionSource {
    let trimmed_name = name.trim();
    SubscriptionSource {
        source_id: crate::models::random_hex(16),
        name: if trimmed_name.is_empty() {
            "订阅".to_string()
        } else {
            trimmed_name.to_string()
        },
        url: url.trim().to_string(),
        last_updated: String::new(),
        group: group.to_string(),
    }
}

/// 下载订阅并把它带来的节点合并进配置。
///
/// 刷新时会保留同名节点原先的分组与中转设置，避免用户手动调整被覆盖。
async fn download_subscription(
    state: &mut AppState,
    source: &mut SubscriptionSource,
    existing: Option<&SubscriptionSource>,
) -> Result<usize, String> {
    if let Some(previous) = existing {
        source.source_id = previous.source_id.clone();
    }
    let response = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .map_err(|err| format!("创建 HTTP 客户端失败：{err}"))?
        .get(&source.url)
        .header("User-Agent", "NetworkManager/0.6.3")
        .send()
        .await
        .map_err(|err| format!("订阅下载失败：{err}"))?
        .text()
        .await
        .map_err(|err| format!("订阅读取失败：{err}"))?;
    let (nodes, _errors) = parse_import_content(&response)?;

    let mut previous_groups: Vec<(String, String)> = Vec::new();
    let mut previous_dialers: Vec<(String, String)> = Vec::new();
    let mut previous_policies: Vec<(String, String)> = Vec::new();
    let mut previous_names: Vec<String> = Vec::new();
    for node in state
        .config
        .imported_nodes
        .iter()
        .filter(|node| node.source_id == source.source_id)
    {
        previous_names.push(node.name());
        if !node.group.is_empty() {
            previous_groups.push((node.name(), node.group.clone()));
        }
        let dialer = node.dialer_proxy();
        if !dialer.is_empty() {
            previous_dialers.push((node.name(), dialer));
        }
        let policy = node.dialer_policy();
        if !policy.is_empty() {
            previous_policies.push((node.name(), policy));
        }
    }
    let remaining: Vec<ImportedNode> = state
        .config
        .imported_nodes
        .iter()
        .filter(|node| node.source_id != source.source_id)
        .cloned()
        .collect();
    state.config.imported_nodes = remaining;
    let count = add_nodes(state, nodes, &source.name, &source.source_id);
    let fresh: Vec<String> = state
        .config
        .imported_nodes
        .iter()
        .rev()
        .take(count)
        .map(|node| node.name())
        .collect();
    for node in state.config.imported_nodes.iter_mut().rev().take(count) {
        let name = node.name();
        if let Some(group) = previous_groups
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, group)| group.clone())
        {
            node.group = group;
        } else {
            node.group = source.group.clone();
        }
        if let Some(dialer) = previous_dialers
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.clone())
        {
            node.config[crate::models::NODE_DIALER_PROXY_KEY] = json!(dialer);
        }
        if let Some(policy) = previous_policies
            .iter()
            .find(|(key, _)| *key == name)
            .map(|(_, value)| value.clone())
        {
            node.config[crate::models::NODE_DIALER_POLICY_KEY] = json!(policy);
        }
    }
    let gone: Vec<String> = previous_names
        .into_iter()
        .filter(|name| !fresh.contains(name))
        .collect();
    clear_node_dialer_references(&mut state.config.imported_nodes, &gone);
    repair_selected_node(state, &gone);
    if state.config.selected_node.is_empty() {
        state.config.selected_node = state
            .config
            .imported_nodes
            .first()
            .map(|node| node.name())
            .unwrap_or_default();
    }
    source.last_updated = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string();
    Ok(count)
}

fn add_nodes(state: &mut AppState, nodes: Vec<Value>, source: &str, source_id: &str) -> usize {
    let mut existing: Vec<String> = state
        .config
        .imported_nodes
        .iter()
        .map(|node| node.name())
        .collect();
    let mut added = 0;
    for mut config in nodes {
        let base = config
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("未命名节点")
            .to_string();
        let name = unique_node_name(&base, &existing);
        existing.push(name.clone());
        if let Value::Object(map) = &mut config {
            map.insert("name".into(), Value::String(name));
        }
        state.config.imported_nodes.push(ImportedNode {
            node_id: crate::models::random_hex(16),
            source: source.to_string(),
            config,
            source_id: source_id.to_string(),
            group: String::new(),
        });
        added += 1;
    }
    added
}

fn apply(state: &mut AppState) -> Result<Value, String> {
    state.apply_config()?;
    Ok(json!(true))
}

pub async fn dispatch(state: &mut AppState, method: &str, args: Vec<Value>) -> Result<Value, String> {
    match method {
        "getState" => Ok(crate::state::build(state)),
        "getLogs" => {
            let path = core_log_path();
            let raw = std::fs::read_to_string(path).unwrap_or_default();
            let lines: Vec<&str> = raw.lines().collect();
            let tail = lines
                .iter()
                .rev()
                .take(300)
                .cloned()
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect::<Vec<_>>();
            Ok(Value::String(tail.join("\n")))
        }
        "clearLogs" => {
            let _ = std::fs::write(core_log_path(), "");
            Ok(json!(true))
        }
        "openLogs" => {
            let path = core_log_path();
            #[cfg(windows)]
            {
                let _ = std::process::Command::new("explorer")
                    .arg(path.parent().unwrap_or(std::path::Path::new(".")))
                    .spawn();
            }
            Ok(json!(true))
        }
        "toggleCore" => {
            if state.core.is_running() {
                state.core.stop()?;
                Ok(json!(false))
            } else {
                state.busy = true;
                let result = state.core.start(&state.config);
                state.busy = false;
                result?;
                Ok(json!(true))
            }
        }
        "setMode" => {
            let mode = text(&args, 0).to_uppercase();
            if !crate::state::MODE_LABELS
                .iter()
                .any(|(key, _)| *key == mode.as_str())
            {
                state.notify("error", "运行模式无效");
                return Ok(json!(null));
            }
            if matches!(mode.as_str(), "GLOBAL_BUILTIN" | "SMART")
                && state.config.imported_nodes.is_empty()
            {
                state.notify("error", "请先导入至少一个节点");
                return Ok(json!(null));
            }
            state.config.mode = mode;
            apply(state)
        }
        "setDefaultTarget" => {
            let target = text(&args, 0).to_uppercase();
            if !["CLASH", "V2RAY", "SSH", "BUILTIN", "DIRECT"].contains(&target.as_str()) {
                state.notify("error", "默认目标无效");
                return Ok(json!(null));
            }
            state.config.default_target = target;
            apply(state)
        }
        "selectNode" => {
            let name = text(&args, 0);
            if !state
                .config
                .imported_nodes
                .iter()
                .any(|node| node.name() == name)
            {
                return Ok(json!(null));
            }
            state.config.selected_node = name;
            apply(state)
        }
        "saveSettings" => {
            let patch = payload(&args, 0)?;
            let mut merged = serde_json::to_value(&state.config).unwrap_or(json!({}));
            if let (Value::Object(target), Value::Object(source)) = (&mut merged, &patch) {
                for (key, value) in source {
                    target.insert(key.clone(), value.clone());
                }
            }
            if let Ok(config) = serde_json::from_value::<AppConfig>(merged) {
                let startup = config.start_with_windows;
                let changed = startup != state.config.start_with_windows;
                state.config = config;
                if changed {
                    if let Err(err) = crate::startup::set_enabled(startup) {
                        state.notify("error", format!("开机自启设置失败：{err}"));
                    }
                }
            }
            apply(state)
        }
        "saveSources" => {
            let patch = payload(&args, 0)?;
            for (key, slot) in [("clash", "Clash 7897"), ("v2ray", "v2ray 10808")] {
                let Some(item) = patch.get(key) else {
                    return Err("代理源设置无效".into());
                };
                let port = item
                    .get("port")
                    .and_then(|v| v.as_i64())
                    .ok_or("代理源设置无效")? as i32;
                let protocol = item
                    .get("protocol")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if (protocol != "socks5" && protocol != "http") || !(1..=65535).contains(&port) {
                    state.notify("error", "代理源协议或端口无效");
                    return Ok(json!(null));
                }
                let upstream = crate::models::Upstream {
                    name: slot.to_string(),
                    host: item
                        .get("host")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .trim()
                        .to_string(),
                    port,
                    protocol,
                    enabled: item.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true),
                };
                if key == "clash" {
                    state.config.clash = upstream;
                } else {
                    state.config.v2ray = upstream;
                }
            }
            state.notify("success", "本地代理源已保存");
            apply(state)
        }
        "saveRule" => {
            let patch = payload(&args, 0)?;
            let rule: RoutingRule =
                serde_json::from_value(patch).map_err(|_| "规则内容无效".to_string())?;
            let index = number(&args, 1) as usize;
            if index < state.config.rules.len() {
                state.config.rules[index] = rule;
            } else {
                state.config.rules.push(rule);
            }
            apply(state)
        }
        "ruleAction" => {
            let action = text(&args, 0);
            let index = number(&args, 1) as usize;
            let rule = state
                .config
                .rules
                .get_mut(index)
                .ok_or("规则不存在".to_string())?;
            match action.as_str() {
                "toggle" => rule.enabled = !rule.enabled,
                "delete" => {
                    state.config.rules.remove(index);
                }
                _ => return Err(format!("不支持的规则操作：{action}")),
            }
            apply(state)
        }
        "saveRuleGroup" => {
            let payload_value = payload(&args, 0)?;
            let group_id = payload_value
                .get("groupId")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if group_id != crate::models::COMMON_OVERSEAS_GROUP {
                state.notify("error", "规则组无效");
                return Ok(json!(null));
            }
            let target = payload_value
                .get("target")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if !["CLASH", "V2RAY", "SSH", "BUILTIN", "DIRECT"].contains(&target.as_str()) {
                state.notify("error", "规则组去向无效");
                return Ok(json!(null));
            }
            let members: Vec<usize> = common_rule_indexes(state);
            if members.is_empty() {
                state.notify("error", "规则组已经不存在");
                return Ok(json!(null));
            }
            let values = payload_value.get("values").cloned();
            match values {
                None => {
                    for index in members {
                        state.config.rules[index].target = target.clone();
                    }
                    state.notify("success", "常用海外站点去向已更新");
                }
                Some(Value::Null) => {
                    for index in members {
                        state.config.rules[index].target = target.clone();
                    }
                    state.notify("success", "常用海外站点去向已更新");
                }
                Some(values) => {
                    let list = values.as_array().cloned().unwrap_or_default();
                    let rebuilt = match crate::portable::common_overseas_rules(
                        &json!(list),
                        &target,
                        true,
                    ) {
                        Ok(rules) => rules,
                        Err(err) => {
                            state.notify("error", err);
                            return Ok(json!(null));
                        }
                    };
                    let keep: Vec<RoutingRule> = state
                        .config
                        .rules
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| !members.contains(index))
                        .map(|(_, rule)| rule.clone())
                        .collect();
                    state.config.rules = [keep, rebuilt].concat();
                    state.notify("success", "常用海外站点已更新");
                }
            }
            apply(state)
        }
        "ruleGroupAction" => {
            let group_id = text(&args, 0);
            let action = text(&args, 1);
            if group_id != crate::models::COMMON_OVERSEAS_GROUP {
                return Ok(json!(null));
            }
            let members = common_rule_indexes(state);
            if members.is_empty() {
                return Ok(json!(null));
            }
            match action.as_str() {
                "toggle" => {
                    let enabled = !members
                        .iter()
                        .all(|index| state.config.rules[*index].enabled);
                    for index in members {
                        state.config.rules[index].enabled = enabled;
                    }
                    state.notify("success", "常用海外站点规则组状态已更新");
                }
                "delete" => {
                    state.config.rules = state
                        .config
                        .rules
                        .iter()
                        .enumerate()
                        .filter(|(index, _)| !members.contains(index))
                        .map(|(_, rule)| rule.clone())
                        .collect();
                    state.notify("success", "常用海外站点规则组已删除");
                }
                _ => return Ok(json!(null)),
            }
            apply(state)
        }
        "createNodeGroup" => {
            let name = text(&args, 0);
            if !name.is_empty() && !state.config.node_groups.contains(&name) {
                state.config.node_groups.push(name);
            }
            apply(state)
        }
        "deleteNodeGroup" => {
            let name = text(&args, 0);
            state.config.node_groups.retain(|group| group != &name);
            apply(state)
        }
        "assignNodeGroup" => {
            let index = number(&args, 0) as usize;
            let group = text(&args, 1);
            if let Some(node) = state.config.imported_nodes.get_mut(index) {
                node.group = group;
            }
            apply(state)
        }
        "setNodeDialerProxy" => {
            let index = number(&args, 0) as usize;
            let proxy = text(&args, 1);
            if let Some(node) = state.config.imported_nodes.get_mut(index) {
                if let Value::Object(map) = &mut node.config {
                    if proxy.is_empty() {
                        map.remove(crate::models::NODE_DIALER_PROXY_KEY);
                    } else {
                        map.insert(
                            crate::models::NODE_DIALER_PROXY_KEY.into(),
                            Value::String(proxy),
                        );
                    }
                }
            }
            apply(state)
        }
        "saveProxyRelayRules" => {
            let patch = payload(&args, 0)?;
            if let Some(entries) = patch.get("relays").and_then(|v| v.as_array()) {
                for entry in entries {
                    let index = entry.get("index").and_then(|v| v.as_i64()).unwrap_or(-1);
                    let relay = entry
                        .get("relay")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    if let Some(node) = state.config.imported_nodes.get_mut(index as usize) {
                        if let Value::Object(map) = &mut node.config {
                            if relay.is_empty() {
                                map.remove(crate::models::NODE_DIALER_PROXY_KEY);
                            } else {
                                map.insert(
                                    crate::models::NODE_DIALER_PROXY_KEY.into(),
                                    Value::String(relay),
                                );
                            }
                        }
                    }
                }
            }
            apply(state)
        }
        // importPaste/importFileText 的签名是 (name, content, group_name)。
        "importPaste" | "importFileText" => {
            let name = text(&args, 0);
            let content = text(&args, 1);
            let group_name = text(&args, 2);
            let (nodes, errors) = match parse_import_content(&content) {
                Ok(parsed) => parsed,
                Err(err) => {
                    state.notify("error", format!("配置导入失败：{err}"));
                    return Ok(json!(null));
                }
            };
            let group = crate::models::normalize_group(&group_name);
            if !group.is_empty() && !state.config.node_groups.contains(&group) {
                state.notify("error", "导入分组不存在");
                return Ok(json!(null));
            }
            let source = if name.trim().is_empty() {
                "文件导入".to_string()
            } else {
                name.trim().to_string()
            };
            let mut count = add_nodes(state, nodes, &source, "");
            if !group.is_empty() {
                for node in state.config.imported_nodes.iter_mut().rev().take(count) {
                    node.group = group.clone();
                }
            }
            if count > 0 && state.config.selected_node.is_empty() {
                state.config.selected_node = state
                    .config
                    .imported_nodes
                    .last()
                    .map(|node| node.name())
                    .unwrap_or_default();
            }
            apply(state)?;
            let kind = if count > 0 { "success" } else { "info" };
            state.notify(kind, format!("已导入 {count} 个节点"));
            if !errors.is_empty() {
                state.notify("info", format!("另有 {} 个链接未识别", errors.len()));
            }
            count = count.max(0);
            Ok(json!(count))
        }
        "exportPortableConfigText" => {
            let payload = crate::portable::export_portable_config(&state.config);
            Ok(json!(serde_json::to_string_pretty(&payload).unwrap_or_default()))
        }
        "importPortableConfigText" => {
            let content = text(&args, 0);
            if state.core.is_running() {
                state.notify("error", "请先停止接管，再导入跨设备配置");
                return Ok(json!(null));
            }
            let result = (|| -> Result<AppConfig, String> {
                if content.len() > 10 * 1024 * 1024 {
                    return Err("配置文件不能超过 10 MB".into());
                }
                let payload: Value = serde_json::from_str(&content)
                    .map_err(|err| format!("配置导入失败：{err}"))?;
                crate::portable::import_portable_config(&state.config, &payload)
            })();
            match result {
                Ok(config) => {
                    state.config = config;
                    crate::store::save(&state.config);
                    match state.core.write_config(&state.config) {
                        Ok(()) => state.notify("success", "跨设备配置已导入，本机端口保持不变"),
                        Err(err) => {
                            state.notify("error", format!("跨设备配置导入后写盘失败：{err}"))
                        }
                    }
                }
                Err(err) => state.notify("error", format!("配置导入失败：{err}")),
            }
            Ok(json!(null))
        }
        "deleteNode" => {
            let index = number(&args, 0) as usize;
            if index < state.config.imported_nodes.len() {
                state.config.imported_nodes.remove(index);
            }
            apply(state)
        }
        "deleteErrorNodes" => {
            let failed: Vec<String> = state
                .node_delays
                .iter()
                .filter(|(_, delay)| delay.status == "error")
                .map(|(name, _)| name.clone())
                .collect();
            state
                .config
                .imported_nodes
                .retain(|node| !failed.contains(&node.name()));
            apply(state)
        }
        "testNode" | "testAllNodes" | "testSource" => {
            let indices: Vec<usize> = if method == "testAllNodes" {
                (0..state.config.imported_nodes.len()).collect()
            } else if method == "testSource" {
                let index = number(&args, 0) as usize;
                let source_id = state
                    .config
                    .subscriptions
                    .get(index)
                    .map(|source| source.source_id.clone())
                    .unwrap_or_default();
                state
                    .config
                    .imported_nodes
                    .iter()
                    .enumerate()
                    .filter(|(_, node)| node.source_id == source_id)
                    .map(|(index, _)| index)
                    .collect()
            } else {
                vec![number(&args, 0) as usize]
            };
            for index in indices {
                let Some(node) = state.config.imported_nodes.get(index) else {
                    continue;
                };
                let name = node.name();
                let (host, port) = node_endpoint(node);
                match measure_delay(&host, port).await {
                    Ok(delay) => {
                        state.node_delays.insert(
                            name,
                            crate::core::NodeDelay {
                                status: "ok".into(),
                                delay: Some(delay),
                                message: String::new(),
                            },
                        );
                    }
                    Err(error) => {
                        state.node_delays.insert(
                            name,
                            crate::core::NodeDelay {
                                status: "error".into(),
                                delay: None,
                                message: error,
                            },
                        );
                    }
                }
            }
            Ok(json!(true))
        }
        "addSubscription" => {
            let name = text(&args, 0);
            let url = text(&args, 1);
            let group_name = text(&args, 2);
            let lower = url.trim().to_lowercase();
            if !lower.starts_with("http://") && !lower.starts_with("https://") {
                state.notify("error", "订阅地址必须以 http:// 或 https:// 开头");
                return Ok(json!(null));
            }
            let group = crate::models::normalize_group(&group_name);
            if !group.is_empty() && !state.config.node_groups.contains(&group) {
                state.notify("error", "订阅分组不存在");
                return Ok(json!(null));
            }
            let mut source = subscription_from_url(&name, &url, &group);
            match download_subscription(state, &mut source, None).await {
                Ok(count) => {
                    state.config.subscriptions.push(source.clone());
                    apply(state)?;
                    state.notify("success", format!("订阅已添加，导入 {count} 个节点"));
                    Ok(json!(true))
                }
                Err(err) => {
                    state.notify("error", format!("订阅添加失败：{err}"));
                    Ok(json!(null))
                }
            }
        }
        "refreshSubscription" | "refreshAllSubscriptions" => {
            let targets: Vec<usize> = if method == "refreshAllSubscriptions" {
                if state.config.subscriptions.is_empty() {
                    state.notify("info", "没有可刷新的订阅");
                    return Ok(json!(0));
                }
                (0..state.config.subscriptions.len()).collect()
            } else {
                vec![number(&args, 0) as usize]
            };
            let mut total = 0usize;
            let mut failures = 0usize;
            for index in targets {
                let Some(existing) = state.config.subscriptions.get(index).cloned() else {
                    continue;
                };
                if existing.url.is_empty() {
                    continue;
                }
                let mut source = existing.clone();
                source.group = existing.group.clone();
                match download_subscription(state, &mut source, Some(&existing)).await {
                    Ok(count) => {
                        if let Some(entry) = state.config.subscriptions.get_mut(index) {
                            *entry = source;
                        }
                        total += count;
                    }
                    Err(err) => {
                        failures += 1;
                        state.notify("error", format!("刷新 {} 失败：{err}", existing.name));
                    }
                }
            }
            apply(state)?;
            if failures > 0 {
                state.notify("error", format!("订阅刷新完成，{failures} 个失败"));
            } else {
                state.notify("success", "全部订阅已刷新");
            }
            Ok(json!(total))
        }
        "deleteSubscription" => {
            let index = number(&args, 0) as usize;
            if let Some(source) = state.config.subscriptions.get(index).cloned() {
                let removed: Vec<String> = state
                    .config
                    .imported_nodes
                    .iter()
                    .filter(|node| node.source_id == source.source_id)
                    .map(|node| node.name())
                    .collect();
                state
                    .config
                    .imported_nodes
                    .retain(|node| node.source_id != source.source_id);
                clear_node_dialer_references(&mut state.config.imported_nodes, &removed);
                state.config.subscriptions.remove(index);
                repair_selected_node(state, &removed);
            }
            apply(state)
        }
        "saveSshServer" => {
            let patch = payload(&args, 0)?;
            let password = text(&args, 1);
            let mut profile: SshServerProfile = serde_json::from_value(patch)
                .map_err(|_| "服务器配置无效".to_string())?;
            if profile.profile_id.is_empty() {
                profile.profile_id = format!("{:x}", rand::random::<u64>());
            }
            let remember = profile.remember_password;
            let profile_id = profile.profile_id.clone();
            if let Some(existing) = state
                .config
                .ssh_servers
                .iter_mut()
                .find(|item| item.profile_id == profile_id)
            {
                *existing = profile;
            } else {
                state.config.ssh_servers.push(profile);
            }
            if remember && !password.is_empty() {
                crate::credentials::set(&profile_id, &password)?;
            } else if !remember {
                crate::credentials::delete(&profile_id);
            }
            apply(state)
        }
        "deleteSshServer" => {
            let profile_id = text(&args, 0);
            crate::credentials::delete(&profile_id);
            state
                .config
                .ssh_servers
                .retain(|profile| profile.profile_id != profile_id);
            apply(state)
        }
        "forgetSshCredential" => {
            let profile_id = text(&args, 0);
            crate::credentials::delete(&profile_id);
            apply(state)
        }
        "pickSshKey" => {
            let picked = tokio::task::spawn_blocking(crate::gui::pick_ssh_key_blocking)
                .await
                .unwrap_or(None);
            Ok(picked.map(Value::String).unwrap_or(Value::Null))
        }
        "restartAsAdmin" => {
            // TUN 接管需要管理员令牌，前端提示条里的「以管理员身份重启」走这里。
            if crate::platform::is_admin() {
                return Err("已经是管理员权限".to_string());
            }
            crate::platform::restart_as_admin()?;
            crate::gui::send(crate::gui::AppEvent::Quit);
            Ok(json!(true))
        }
        "windowAction" => {
            match text(&args, 0).as_str() {
                "minimize" => crate::gui::send(crate::gui::AppEvent::Minimize),
                "maximize" => crate::gui::send(crate::gui::AppEvent::Maximize),
                "hide" => crate::gui::send(crate::gui::AppEvent::Hide),
                "close" => crate::gui::send(crate::gui::AppEvent::Close),
                "quit" => crate::gui::send(crate::gui::AppEvent::Quit),
                "open" | "show" => crate::gui::send(crate::gui::AppEvent::Show),
                _ => {}
            }
            Ok(json!(true))
        }
        "deploySshServer" => Err("部署任务已转入后台执行".into()),
        "copyServerNode" => {
            let profile_id = text(&args, 0);
            let node = state
                .config
                .imported_nodes
                .iter()
                .find(|node| node.source_id == format!("server-deployment:{profile_id}"))
                .map(|node| node.config.clone())
                .ok_or("该服务器还没有可复制的节点".to_string())?;
            Ok(node)
        }
        "testExit" => {
            // 一次测两个出口：直连（本机真实 IP）与代理（经过内置节点的出口）。
            // 代理检测沿用 Python 语义：必须先启动内核，通过本地混合端口出站。
            let core_running = state.core.is_running();
            let (local, proxy_result) = tokio::join!(
                exit_ip_direct(),
                async {
                    if !core_running {
                        return Err("内核未运行".to_string());
                    }
                    let proxy = format!("http://127.0.0.1:{}", state.config.mixed_port);
                    exit_ip_through_proxy(&proxy).await
                }
            );
            let (local_ip, local_error) = match local {
                Ok(address) => (address, None),
                Err(err) => {
                    // 直连失败也要可见，不能静默吞掉，否则卡片一直停在「尚未检测」。
                    state.notify("warning", format!("直连出口检测失败：{err}"));
                    ("检测失败".to_string(), Some(err))
                }
            };
            state.local_ip = local_ip.clone();
            match proxy_result {
                Ok(address) => {
                    state.exit_ip = address.clone();
                    if local_error.is_some() {
                        state.notify("warning", format!("代理出口：{address}（直连出口检测失败）"));
                    } else {
                        state.notify("success", format!("直连 {local_ip} · 代理出口 {address}"));
                        if local_ip == address {
                            // TUN 接管会把「直连」流量也送进内核，两个 IP 相同是
                            // 预期结果，说明接管生效；不是检测出错。
                            state.notify(
                                "info",
                                "直连与代理出口相同：当前处于接管状态，直连流量也走了内核".to_string(),
                            );
                        }
                    }
                }
                Err(err) => {
                    // 内核没跑就别留着上一次的代理出口，避免误以为代理还生效。
                    state.exit_ip = if core_running {
                        "检测失败".to_string()
                    } else {
                        "内核未运行".to_string()
                    };
                    let detail = if core_running { err } else { "请先启动接管核心".to_string() };
                    state.notify("error", format!("代理出口检测失败：{detail}"));
                }
            }
            apply(state)
        }
        other => Err(format!("不支持的操作：{other}")),
    }
}
