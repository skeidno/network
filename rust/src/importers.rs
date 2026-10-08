use base64::Engine;
use serde_json::{json, Value};

fn engine() -> impl Engine {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
}

/// 订阅内容里的 base64 可能带 `=` 补位、也可能是标准字母表。
///
/// `URL_SAFE_NO_PAD` 引擎使用 `DecodePaddingMode::RequireNone`，一旦输入里
/// 带了 `=` 就会直接报错，所以这里统一先去掉补位再用无补位引擎解码，
/// 失败后再退回到标准字母表尝试。
fn decode_base64(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let url_safe = engine();
    let stripped = trimmed.trim_end_matches('=').replace('+', "-").replace('/', "_");
    if let Ok(bytes) = url_safe.decode(&stripped) {
        if let Ok(text) = String::from_utf8(bytes) {
            return Some(text);
        }
    }
    let standard = trimmed.trim_end_matches('=').replace('-', "+").replace('_', "/");
    for candidate in [
        standard.clone(),
        format!("{standard}=="),
        format!("{standard}="),
    ] {
        if let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(candidate) {
            if let Ok(text) = String::from_utf8(bytes) {
                return Some(text);
            }
        }
    }
    None
}

fn split_name(link: &str) -> (&str, String) {
    match link.split_once('#') {
        Some((head, fragment)) => (
            head,
            urlencoding_decode(fragment).trim().to_string(),
        ),
        None => (link, String::new()),
    }
}

fn urlencoding_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(hi), Some(lo)) = (hex(bytes[index + 1]), hex(bytes[index + 2])) {
                out.push(hi * 16 + lo);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_query(text: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in text.split('&') {
        if let Some((key, value)) = pair.split_once('=') {
            map.insert(key.to_string(), urlencoding_decode(value));
        }
    }
    map
}

use std::collections::HashMap;

pub fn parse_ss_link(link: &str) -> Result<Value, String> {
    let (head, name) = split_name(link);
    let body = head.strip_prefix("ss://").unwrap_or(head);
    let (userinfo, host_part) = match body.rfind('@') {
        Some(index) => (body[..index].to_string(), body[index + 1..].to_string()),
        None => {
            let decoded = decode_base64(body).ok_or("无法解析 ss 链接")?;
            let (info, host) = decoded.rsplit_once('@').ok_or("ss 链接缺少服务器地址")?;
            (info.to_string(), host.to_string())
        }
    };
    let decoded_userinfo = decode_base64(&userinfo).unwrap_or_else(|| userinfo.clone());
    let (method, password) = decoded_userinfo
        .split_once(':')
        .ok_or("ss 链接缺少加密方式与密码")?;
    let (host, port) = host_part
        .rsplit_once(':')
        .ok_or("ss 链接缺少端口")?;
    let port: i32 = port
        .split('/')
        .next()
        .unwrap_or("0")
        .parse()
        .map_err(|_| "ss 端口无效")?;
    let node = json!({
        "name": if name.is_empty() { format!("{host}:{port}") } else { name },
        "type": "ss",
        "server": host.trim_matches(|c| c == '[' || c == ']'),
        "port": port,
        "cipher": method,
        "password": password,
        "udp": true,
    });
    Ok(node)
}

pub fn parse_vmess_link(link: &str) -> Result<Value, String> {
    let (head, name) = split_name(link);
    let body = head.strip_prefix("vmess://").unwrap_or(head);
    let decoded = decode_base64(body).ok_or("无法解析 vmess 链接")?;
    let payload: Value =
        serde_json::from_str(&decoded).map_err(|_| "vmess 内容不是合法 JSON")?;
    let get = |key: &str| {
        payload
            .get(key)
            .and_then(|value| match value {
                Value::String(text) => Some(text.clone()),
                Value::Number(number) => Some(number.to_string()),
                _ => None,
            })
            .unwrap_or_default()
    };
    let port: i32 = get("port").parse().unwrap_or(0);
    let mut node = json!({
        "name": if name.is_empty() { get("ps") } else { name },
        "type": "vmess",
        "server": get("add"),
        "port": port,
        "uuid": get("id"),
        "alterId": get("aid").parse::<i32>().unwrap_or(0),
        "cipher": if get("scy").is_empty() { "auto".to_string() } else { get("scy") },
        "udp": true,
    });
    match get("net").as_str() {
        "ws" => {
            node["network"] = json!("ws");
            node["ws-opts"] = json!({"path": get("path"), "headers": {"Host": get("host")}});
        }
        "grpc" => {
            node["network"] = json!("grpc");
            node["grpc-opts"] = json!({"grpc-service-name": get("path")});
        }
        "h2" => {
            node["network"] = json!("h2");
            node["h2-opts"] = json!({"path": get("path"), "host": [get("host")]});
        }
        _ => {
            if get("net") == "tcp" {
                node["network"] = json!("tcp");
            }
        }
    }
    if get("tls") == "tls" {
        node["tls"] = json!(true);
        if !get("sni").is_empty() {
            node["servername"] = json!(get("sni"));
        }
    }
    Ok(node)
}

pub fn parse_vless_link(link: &str) -> Result<Value, String> {
    let (head, name) = split_name(link);
    let body = head.strip_prefix("vless://").unwrap_or(head);
    let (userinfo, host_part) = body.rsplit_once('@').ok_or("vless 链接缺少服务器地址")?;
    let (host_part, query_text) = host_part.split_once('?').unwrap_or((host_part, ""));
    let query = parse_query(query_text);
    let (host, port) = host_part.rsplit_once(':').ok_or("vless 链接缺少端口")?;
    let port: i32 = port.parse().unwrap_or(0);
    let mut node = json!({
        "name": if name.is_empty() { format!("{host}:{port}") } else { name },
        "type": "vless",
        "server": host.trim_matches(|c| c == '[' || c == ']'),
        "port": port,
        "uuid": userinfo,
        "udp": true,
    });
    let security = query.get("security").cloned().unwrap_or_default();
    if security == "tls" || security == "reality" {
        node["tls"] = json!(true);
        if let Some(sni) = query.get("sni") {
            node["servername"] = json!(sni);
        }
    }
    match query.get("type").cloned().unwrap_or_default().as_str() {
        "ws" => {
            node["network"] = json!("ws");
            node["ws-opts"] = json!({"path": query.get("path").cloned().unwrap_or_default()});
        }
        "grpc" => {
            node["network"] = json!("grpc");
            node["grpc-opts"] = json!({"grpc-service-name": query.get("serviceName").cloned().unwrap_or_default()});
        }
        _ => {}
    }
    Ok(node)
}

pub fn parse_trojan_link(link: &str) -> Result<Value, String> {
    let (head, name) = split_name(link);
    let body = head.strip_prefix("trojan://").unwrap_or(head);
    let (userinfo, host_part) = body.rsplit_once('@').ok_or("trojan 链接缺少服务器地址")?;
    let (host_part, query_text) = host_part.split_once('?').unwrap_or((host_part, ""));
    let query = parse_query(query_text);
    let (host, port) = host_part.rsplit_once(':').ok_or("trojan 链接缺少端口")?;
    let port: i32 = port.parse().unwrap_or(0);
    let mut node = json!({
        "name": if name.is_empty() { format!("{host}:{port}") } else { name },
        "type": "trojan",
        "server": host.trim_matches(|c| c == '[' || c == ']'),
        "port": port,
        "password": userinfo,
        "udp": true,
    });
    if let Some(sni) = query.get("sni") {
        node["sni"] = json!(sni);
    }
    Ok(node)
}

pub fn parse_hysteria2_link(link: &str) -> Result<Value, String> {
    let (head, name) = split_name(link);
    let body = head.strip_prefix("hysteria2://").unwrap_or(head);
    let body = body.strip_prefix("hy2://").unwrap_or(body);
    let (userinfo, host_part) = body.rsplit_once('@').ok_or("hysteria2 链接缺少服务器地址")?;
    let (host_part, query_text) = host_part.split_once('?').unwrap_or((host_part, ""));
    let query = parse_query(query_text);
    let (host, port) = host_part.rsplit_once(':').ok_or("hysteria2 链接缺少端口")?;
    let port: i32 = port.parse().unwrap_or(0);
    let mut node = json!({
        "name": if name.is_empty() { format!("{host}:{port}") } else { name },
        "type": "hysteria2",
        "server": host.trim_matches(|c| c == '[' || c == ']'),
        "port": port,
        "password": userinfo,
        "udp": true,
    });
    if let Some(sni) = query.get("sni") {
        node["sni"] = json!(sni);
    }
    if let Some(obfs) = query.get("obfs") {
        node["obfs"] = json!(obfs);
    }
    Ok(node)
}

pub fn parse_share_link(link: &str) -> Result<Value, String> {
    let trimmed = link.trim();
    if trimmed.is_empty() {
        return Err("空链接".into());
    }
    if let Some(rest) = trimmed.strip_prefix("ss://") {
        return parse_ss_link(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("vmess://") {
        return parse_vmess_link(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("vless://") {
        return parse_vless_link(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("trojan://") {
        return parse_trojan_link(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("hysteria2://") {
        return parse_hysteria2_link(rest);
    }
    if let Some(rest) = trimmed.strip_prefix("hy2://") {
        return parse_hysteria2_link(rest);
    }
    Err(format!("暂不支持的链接类型：{}", &trimmed[..trimmed.len().min(12)]))
}

fn yaml_proxies(text: &str) -> Vec<Value> {
    let value: Value = match serde_yaml::from_str(text) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    value
        .get("proxies")
        .and_then(|proxies| proxies.as_array())
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|item| item.get("name").is_some() && item.get("type").is_some())
        .collect()
}

/// 单次导入内容的体积上限（10 MB），与 Python 版本保持一致。
const MAX_IMPORT_BYTES: usize = 10 * 1024 * 1024;

fn unstructured_nodes(text: &str) -> (Vec<Value>, Vec<String>) {
    let mut nodes: Vec<Value> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match parse_share_link(line) {
            Ok(node) => nodes.push(node),
            Err(error) => errors.push(format!("第 {} 个链接：{error}", index + 1)),
        }
    }
    (nodes, errors)
}

pub fn parse_import_content(text: &str) -> Result<(Vec<Value>, Vec<String>), String> {
    if text.len() > MAX_IMPORT_BYTES {
        return Err("导入内容超过 10 MB 限制".into());
    }
    let normalized = text.trim().trim_start_matches('\u{feff}');
    if normalized.is_empty() {
        return Err("没有可导入的内容".into());
    }
    let yaml_nodes = yaml_proxies(normalized);
    if !yaml_nodes.is_empty() {
        return Ok((yaml_nodes, Vec::new()));
    }
    let (mut nodes, mut errors) = unstructured_nodes(normalized);
    if nodes.is_empty() && errors.is_empty() && !normalized.contains("://") {
        if let Some(decoded) = decode_base64(normalized) {
            let decoded = decoded.trim_start_matches('\u{feff}');
            let (retry_nodes, retry_errors) = unstructured_nodes(&decoded);
            nodes = retry_nodes;
            errors = retry_errors;
            if nodes.is_empty() && errors.is_empty() {
                let yaml_nodes = yaml_proxies(&decoded);
                if !yaml_nodes.is_empty() {
                    return Ok((yaml_nodes, Vec::new()));
                }
            }
        }
    }
    if nodes.is_empty() {
        return Err(errors
            .first()
            .cloned()
            .unwrap_or_else(|| "未找到 Clash proxies、支持的分享链接或代理 IP".into()));
    }
    Ok((nodes, errors))
}

pub fn unique_node_name(base: &str, existing: &[String]) -> String {
    if !existing.contains(&base.to_string()) {
        return base.to_string();
    }
    let mut suffix = 2;
    loop {
        let candidate = format!("{base} ({suffix})");
        if !existing.contains(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}
