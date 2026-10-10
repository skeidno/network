use serde_json::{json, Value};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;

use crate::core::{AppState, NodeDelay};
use crate::importers::{parse_import_content, unique_node_name};
use crate::models::{
    AppConfig, ImportedNode, RoutingRule, SshServerProfile, SubscriptionSource,
};
use crate::paths::core_log_path;
use crate::server::Shared;

fn text(args: &[Value], index: usize) -> String {
    args.get(index)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

/// 把设置界面的 camelCase 键名翻成 AppConfig 的 snake_case 字段名。
///
/// 两边命名不一致，而 `serde_json::from_value` 会把不匹配 Snake 约定的键当成未知
/// 字段忽略掉 —— 于是出现「接口返回 200、什么都没保存」。这里做一层显式转换，而不是
/// 去给 AppConfig 加 `rename_all`：那样会连带改变 settings.json 的写法，已经落盘的
/// 用户配置就读不回来了。
fn setting_key(key: &str) -> String {
    const MAPPED: &[(&str, &str)] = &[
        ("mixedPort", "mixed_port"),
        ("controllerPort", "controller_port"),
        ("dnsPort", "dns_port"),
        ("serverProxyPort", "server_proxy_port"),
        ("strictRoute", "strict_route"),
        ("startOnLaunch", "start_on_launch"),
        ("closeToTray", "close_to_tray"),
        ("startWithWindows", "start_with_windows"),
    ];
    MAPPED
        .iter()
        .find(|(camel, _)| *camel == key)
        .map(|(_, snake)| (*snake).to_string())
        .unwrap_or_else(|| key.to_string())
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

/// 一次检测的结果：出口 IP，以及能查到的归属地（查不到就为空，不编造）。
struct ExitIdentity {
    ip: String,
    location: String,
}

/// 用于提示语：「1.2.3.4（美国 芝加哥）」，没有归属地就只显示 IP。
fn describe(identity: &ExitIdentity) -> String {
    if identity.location.is_empty() {
        identity.ip.clone()
    } else {
        format!("{}（{}）", identity.ip, identity.location)
    }
}

/// 出口检测端点及其响应格式。全部并发发起。
///
/// 前三个国内**不走代理也能访问**：应用自身进程被内核规则
/// `PROCESS-NAME,NetworkManager.exe,DIRECT` 强制直连，只用 ipify /
/// Cloudflare 这类境外端点的话，国内网络下会被直接重置
/// （`tls handshake eof`），直连出口就永远测不出来。
/// ipip.net 会连归属地一起给（中文），但偶尔返 521，所以 http/https 两个都放上。
const IP_CHECKS: &[(&str, &str)] = &[
    ("http://myip.ipip.net", "ipip"),
    ("https://myip.ipip.net", "ipip"),
    ("http://ip.3322.net", "plain"),
    ("http://ip-api.com/json/", "ip-api"),
    ("https://ipapi.co/json/", "ipapi"),
    ("https://1.1.1.1/cdn-cgi/trace", "trace"),
    ("https://api.ipify.org?format=json", "json"),
    ("http://api.ipify.org?format=json", "json"),
];

/// 直连检测本机出口 IP（明确禁用环境变量代理，避免被系统代理干扰）。
async fn exit_ip_direct() -> Result<ExitIdentity, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .no_proxy()
        .build()
        .map_err(|err| format!("创建 HTTP 客户端失败：{err}"))?;
    fetch_exit_identity(&client).await
}

/// 通过本地混合代理端口检测出口 IP —— 测的是「经过代理后的出口」。
async fn exit_ip_through_proxy(proxy_url: &str) -> Result<ExitIdentity, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .proxy(reqwest::Proxy::all(proxy_url).map_err(|err| format!("代理地址无效：{err}"))?)
        .build()
        .map_err(|err| format!("创建 HTTP 客户端失败：{err}"))?;
    fetch_exit_identity(&client).await
}

/// 所有端点同时发起。先拿到「带归属地」的结果就直接返回；
/// 只拿到裸 IP 时，再宽限 3 秒等一个带归属地的响应，实在没有就用裸 IP。
async fn fetch_exit_identity(client: &reqwest::Client) -> Result<ExitIdentity, String> {
    let (tx, mut rx) = tokio::sync::mpsc::channel(IP_CHECKS.len());
    for (endpoint, kind) in IP_CHECKS {
        let tx = tx.clone();
        let client = client.clone();
        tokio::spawn(async move {
            let outcome = probe_exit_endpoint(&client, endpoint, kind).await;
            let _ = tx.send((endpoint, outcome)).await;
        });
    }
    drop(tx);

    let mut fallback: Option<ExitIdentity> = None;
    let mut failures: Vec<String> = Vec::new();
    let mut deadline: Option<tokio::time::Instant> = None;
    loop {
        let next = match deadline {
            Some(until) => tokio::time::timeout_at(until, rx.recv()).await.ok().flatten(),
            None => rx.recv().await,
        };
        match next {
            Some((_, Ok(identity))) => {
                if identity.location.is_empty() {
                    // 先兜底记着，同时开始计时等更好的结果
                    if fallback.is_none() {
                        fallback = Some(identity);
                        deadline = Some(tokio::time::Instant::now() + Duration::from_secs(3));
                    }
                } else {
                    return Ok(identity);
                }
            }
            Some((endpoint, Err(err))) => failures.push(format!("{endpoint}: {err}")),
            None => break,
        }
    }
    fallback.ok_or_else(|| {
        let detail = failures.last().cloned().unwrap_or_else(|| "检测端点没有返回 IP".into());
        format!("所有出口检测端点均失败：{detail}")
    })
}

async fn probe_exit_endpoint(
    client: &reqwest::Client,
    endpoint: &str,
    kind: &str,
) -> Result<ExitIdentity, String> {
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
    parse_exit_identity(&text, kind).ok_or_else(|| "响应中没有 IP".to_string())
}

/// 按端点各自的响应格式解析出 IP 与归属地。
fn parse_exit_identity(text: &str, kind: &str) -> Option<ExitIdentity> {
    let ip = match kind {
        "ip-api" => json_str(text, &["query"]),
        "ipapi" => json_str(text, &["ip"]),
        "json" | "ipip" | "plain" => first_ipv4(text),
        _ => trace_value(text, "ip").or_else(|| first_ipv4(text)),
    }?;
    if !is_public_ip(&ip) {
        return None;
    }
    let location = match kind {
        // 「当前 IP：1.2.3.4  来自于：美国 伊利诺伊州 芝加哥  colocrossing.com」
        "ipip" => ipip_location(text),
        "ip-api" => geo_location(
            country_name(&json_str(text, &["countryCode"]).unwrap_or_default()),
            &[json_str(text, &["city"]), json_str(text, &["regionName"])],
        ),
        "ipapi" => geo_location(
            country_name(&json_str(text, &["country_code"]).unwrap_or_default()),
            &[json_str(text, &["city"]), json_str(text, &["region"])],
        ),
        // Cloudflare trace 只给国家码 + 机房三字码，机房码要翻成城市才看得懂
        "trace" => geo_location(
            country_name(&trace_value(text, "loc").unwrap_or_default()),
            &[trace_value(text, "colo").map(|colo| colo_name(&colo))],
        ),
        _ => String::new(),
    };
    Some(ExitIdentity { ip, location })
}

/// ipip.net 的归属地直接是中文，形如「中国 浙江 杭州 电信」。
/// 末尾的运营商域名（纯 ASCII，如 colocrossing.com）丢掉，只留中文地名。
fn ipip_location(text: &str) -> String {
    let tail = match text.split_once("来自于：") {
        Some((_, tail)) => tail,
        None => return String::new(),
    };
    tail.split_whitespace()
        .take_while(|token| token.chars().any(|ch| !ch.is_ascii()))
        .collect::<Vec<_>>()
        .join(" ")
}

/// 国家用中文名，城市/机房保留原始写法，拼成「美国 芝加哥」这样的短串。
fn geo_location(country: String, parts: &[Option<String>]) -> String {
    let mut location = country.clone();
    for part in parts.iter().flatten() {
        let part = part.trim();
        if part.is_empty() || part == country {
            continue;
        }
        location.push(' ');
        location.push_str(part);
        break;
    }
    location
}

/// Cloudflare trace 里的机房三字码转城市名（常见的那些），查不到就原样返回。
fn colo_name(code: &str) -> String {
    match code.to_ascii_uppercase().as_str() {
        "LAX" => "洛杉矶",
        "SJC" => "圣何塞",
        "SFO" => "旧金山",
        "SEA" => "西雅图",
        "ORD" => "芝加哥",
        "DFW" => "达拉斯",
        "IAD" => "华盛顿",
        "EWR" => "纽瓦克",
        "ATL" => "亚特兰大",
        "MIA" => "迈阿密",
        "DEN" => "丹佛",
        "PHX" => "凤凰城",
        "YYZ" => "多伦多",
        "YVR" => "温哥华",
        "LHR" => "伦敦",
        "CDG" => "巴黎",
        "AMS" => "阿姆斯特丹",
        "FRA" => "法兰克福",
        "MAD" => "马德里",
        "FCO" => "罗马",
        "ZRH" => "苏黎世",
        "ARN" => "斯德哥尔摩",
        "DUB" => "都柏林",
        "IST" => "伊斯坦布尔",
        "MOW" => "莫斯科",
        "DXB" => "迪拜",
        "DOH" => "多哈",
        "TLV" => "特拉维夫",
        "NRT" => "东京",
        "KIX" => "大阪",
        "ICN" => "首尔",
        "HKG" => "中国香港",
        "TPE" => "中国台湾",
        "SIN" => "新加坡",
        "KUL" => "吉隆坡",
        "BKK" => "曼谷",
        "CGK" => "雅加达",
        "MNL" => "马尼拉",
        "SGN" => "胡志明市",
        "DEL" => "德里",
        "BOM" => "孟买",
        "SYD" => "悉尼",
        "MEL" => "墨尔本",
        "AKL" => "奥克兰",
        "JNB" => "约翰内斯堡",
        "LOS" => "拉各斯",
        "GRU" => "圣保罗",
        "SCL" => "圣地亚哥",
        other => return other.to_string(),
    }
    .to_string()
}

/// Cloudflare trace 是一行行的 `key=value`。
fn trace_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    text.lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .map(|value| value.trim().to_string())
}

fn json_str(text: &str, keys: &[&str]) -> Option<String> {
    let value: Value = serde_json::from_str(text).ok()?;
    keys.iter()
        .find_map(|key| value.get(*key)?.as_str())
        .map(|value| value.trim().to_string())
}

/// 常见的 ISO 国家码转中文名。只在端点只给国家码时用，查不到就原样返回。
fn country_name(code: &str) -> String {
    let upper = code.to_ascii_uppercase();
    match upper.as_str() {
        "CN" => "中国",
        "HK" => "中国香港",
        "MO" => "中国澳门",
        "TW" => "中国台湾",
        "US" => "美国",
        "JP" => "日本",
        "KR" => "韩国",
        "SG" => "新加坡",
        "MY" => "马来西亚",
        "TH" => "泰国",
        "VN" => "越南",
        "IN" => "印度",
        "ID" => "印度尼西亚",
        "PH" => "菲律宾",
        "AU" => "澳大利亚",
        "NZ" => "新西兰",
        "GB" | "UK" => "英国",
        "DE" => "德国",
        "FR" => "法国",
        "NL" => "荷兰",
        "CH" => "瑞士",
        "SE" => "瑞典",
        "NO" => "挪威",
        "FI" => "芬兰",
        "DK" => "丹麦",
        "PL" => "波兰",
        "ES" => "西班牙",
        "IT" => "意大利",
        "RU" => "俄罗斯",
        "TR" => "土耳其",
        "CA" => "加拿大",
        "MX" => "墨西哥",
        "BR" => "巴西",
        "AR" => "阿根廷",
        "CL" => "智利",
        "ZA" => "南非",
        "EG" => "埃及",
        "AE" => "阿联酋",
        "SA" => "沙特阿拉伯",
        "IL" => "以色列",
        "UA" => "乌克兰",
        "CZ" => "捷克",
        "AT" => "奥地利",
        "BE" => "比利时",
        "IE" => "爱尔兰",
        "PT" => "葡萄牙",
        "GR" => "希腊",
        "RO" => "罗马尼亚",
        "HU" => "匈牙利",
        other => return other.to_string(),
    }
    .to_string()
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

/// 节点的「可用性/延迟」用一次 TCP 握手衡量：够轻，且不依赖内核是否在跑。
async fn measure_delay(host: &str, port: u16) -> Result<i64, String> {
    if host.is_empty() || port == 0 {
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

/// 解析这次要测速的节点下标。
///
/// 前端传的是节点**名字**，而这里原来直接按数字下标解析 —— 名字解析不出来就退化成
/// 0，于是「点某个节点测速」永远在测第一个节点，被点的那张卡片自然毫无动静。
/// 现在两种写法都认：字符串按名字找，其余按下标。
fn test_indices(state: &AppState, method: &str, args: &[Value]) -> Vec<usize> {
    if method == "testAllNodes" {
        return (0..state.config.imported_nodes.len()).collect();
    }
    if method == "testSource" {
        let index = number(args, 0) as usize;
        let source_id = state
            .config
            .subscriptions
            .get(index)
            .map(|source| source.source_id.clone())
            .unwrap_or_default();
        return state
            .config
            .imported_nodes
            .iter()
            .enumerate()
            .filter(|(_, node)| node.source_id == source_id)
            .map(|(index, _)| index)
            .collect();
    }
    match args.first() {
        Some(Value::String(name)) => state
            .config
            .imported_nodes
            .iter()
            .position(|node| &node.name() == name)
            .into_iter()
            .collect(),
        _ => vec![number(args, 0) as usize],
    }
}

/// 把选中的节点标成「测试中」，界面据此显示进度并禁用按钮。
fn mark_nodes_testing(state: &mut AppState, indices: &[usize]) {
    let names: Vec<String> = indices
        .iter()
        .filter_map(|index| state.config.imported_nodes.get(*index).map(|node| node.name()))
        .collect();
    for name in names {
        state.node_delays.insert(
            name,
            NodeDelay {
                status: "testing".into(),
                delay: None,
                message: String::new(),
            },
        );
    }
}

/// 后台逐个测速并回写结果。
///
/// 每次等待网络前都重新取锁、测完立刻放锁：期间界面照常轮询，能看到「测试中」
/// 一步步变成具体延迟。
pub async fn run_node_tests(shared: Shared, indices: Vec<usize>) {
    for index in indices {
        let target = {
            let state = shared.lock().await;
            state.config.imported_nodes.get(index).map(|node| {
                let (host, port) = node_endpoint(node);
                (node.name(), host, port)
            })
        };
        let Some((name, host, port)) = target else {
            continue;
        };
        let delay = match measure_delay(&host, port).await {
            Ok(delay) => NodeDelay {
                status: "ok".into(),
                delay: Some(delay),
                message: String::new(),
            },
            Err(error) => NodeDelay {
                status: "error".into(),
                delay: None,
                message: error,
            },
        };
        let mut state = shared.lock().await;
        state.node_delays.insert(name, delay);
    }
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

pub async fn dispatch(
    shared: &Shared,
    state: &mut AppState,
    method: &str,
    args: Vec<Value>,
) -> Result<Value, String> {
    match method {
        "getState" => Ok(crate::state::build(state)),
        "getLogs" => {
            // 只读尾部 512 KB，再做宽松 UTF-8 解码：既避免每次读取几 MB 文件，
            // 也保证日志里出现个别坏字节时不会整段变成空白/乱码。
            let path = core_log_path();
            Ok(Value::String(crate::core::read_log_tail(
                &path,
                512 * 1024,
                300,
            )))
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
                    // 设置界面提交 camelCase，AppConfig 的字段名是 snake_case。
                    // 不转换的话这些键会被当成未知字段忽略：接口照常返回 200，
                    // 什么都没保存 —— 用户改了开关刷新后又变回去。
                    let normalized = setting_key(&key);
                    target.insert(normalized, value.clone());
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
            // 先把要测的节点标成 testing 并立即返回，真正的连接放到后台任务里做。
            // 以前是在这里同步 await 测完才返回：单节点还好，批量测速（几十个节点
            // × 最多 3 秒超时）不但让这个请求挂很久，还因为整个 dispatch 持有
            // state 锁，让界面的状态轮询一起卡住 —— 表现就是「点了没反应」。
            let indices = test_indices(state, method, &args);
            mark_nodes_testing(state, &indices);
            let worker = shared.clone();
            tokio::spawn(async move {
                run_node_tests(worker, indices).await;
            });
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
            match crate::platform::restart_as_admin() {
                Ok(true) => {
                    crate::gui::send(crate::gui::AppEvent::Quit);
                    Ok(json!(true))
                }
                // 用户在 UAC 上点了「否」：不该把当前实例退掉，否则界面直接没了。
                Ok(false) => Err("提权重启已取消".to_string()),
                Err(err) => Err(err),
            }
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
                Ok(identity) => (identity, None),
                Err(err) => {
                    // 直连失败也要可见，不能静默吞掉，否则卡片一直停在「尚未检测」。
                    state.notify("warning", format!("直连出口检测失败：{err}"));
                    (
                        ExitIdentity { ip: "检测失败".to_string(), location: String::new() },
                        Some(err),
                    )
                }
            };
            state.local_ip = local_ip.ip.clone();
            state.local_ip_location = local_ip.location.clone();
            match proxy_result {
                Ok(identity) => {
                    state.exit_ip = identity.ip.clone();
                    state.exit_ip_location = identity.location.clone();
                    if local_error.is_some() {
                        state.notify("warning", format!("代理出口：{}", describe(&identity)));
                    } else {
                        state.notify(
                            "success",
                            format!("直连 {} · 代理出口 {}", describe(&local_ip), describe(&identity)),
                        );
                        if local_ip.ip == identity.ip {
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
                    state.exit_ip_location.clear();
                    let detail = if core_running { err } else { "请先启动接管核心".to_string() };
                    state.notify("error", format!("代理出口检测失败：{detail}"));
                }
            }
            apply(state)
        }
        other => Err(format!("不支持的操作：{other}")),
    }
}
