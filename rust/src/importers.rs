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

/// 主机名或 IP 字面量的合法性检查：IP 直接放行，域名按 DNS 标签规则校验。
fn valid_proxy_host(host: &str) -> bool {
    if host.parse::<std::net::IpAddr>().is_ok() {
        return true;
    }
    if host.is_empty() || host.len() > 253 {
        return false;
    }
    let trimmed = host.trim_end_matches('.');
    !trimmed.is_empty()
        && trimmed.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
                && label.starts_with(|c: char| c.is_ascii_alphanumeric())
                && label.ends_with(|c: char| c.is_ascii_alphanumeric())
        })
}

/// 解析 `主机:端口:用户名:密码`，生成一条带认证的 HTTP 代理节点。
///
/// 机场/代理商家常按「一行一个」发这种纯文本条目。界面上的导入提示一直写着支持，
/// 但 Python 版的 `parse_authenticated_proxy_line` 在迁到 Rust 时漏掉了，只剩提示文案。
///
/// 密码允许包含冒号（取第三个冒号之后的全部内容），但不允许空白字符。
pub fn parse_authenticated_proxy_line(line: &str) -> Result<Value, String> {
    let text = line.trim();
    let invalid = || "代理 IP 格式应为 主机:端口:用户名:密码".to_string();
    if text.is_empty() {
        return Err(invalid());
    }
    // 主机可能是方括号包起来的 IPv6（`[fd00::1]:8080:user:pass`），方括号内允许冒号。
    let (host, rest) = if let Some(inner) = text.strip_prefix('[') {
        let end = inner.find(']').ok_or_else(invalid)?;
        let rest = inner[end + 1..].strip_prefix(':').ok_or_else(invalid)?;
        (&inner[..end], rest)
    } else {
        text.split_once(':').ok_or_else(invalid)?
    };
    let (port_text, rest) = rest.split_once(':').ok_or_else(invalid)?;
    let (username, password) = rest.split_once(':').ok_or_else(invalid)?;

    if host.is_empty() || host.chars().any(char::is_whitespace) || !valid_proxy_host(host) {
        return Err("代理 IP 的主机名或 IP 地址无效".into());
    }
    let port: i32 = port_text
        .parse()
        .map_err(|_| "代理 IP 的端口必须是数字")?;
    if !(1..=65535).contains(&port) {
        return Err("代理 IP 的端口必须在 1 到 65535 之间".into());
    }
    if username.is_empty() || username.chars().any(char::is_whitespace) {
        return Err("代理 IP 的用户名不能为空或包含空白字符".into());
    }
    if password.is_empty() || password.chars().any(char::is_whitespace) {
        return Err("代理 IP 的密码不能为空或包含空白字符".into());
    }
    Ok(json!({
        "name": format!("HTTP Proxy {host}:{port}"),
        "type": "http",
        "server": host,
        "port": port,
        "username": username,
        "password": password,
    }))
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
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.contains("://") {
            match parse_share_link(line) {
                Ok(node) => nodes.push(node),
                Err(error) => errors.push(format!("第 {} 个链接：{error}", index + 1)),
            }
            continue;
        }
        // 只有「像一条代理 IP」的行才当代理解析：至少三个冒号（主机/端口/用户名/密码）。
        // 不设这道闸，整段 Base64 订阅会被逐行报格式错误，下面的 Base64 兜底就永远走不到。
        if line.matches(':').count() < 3 {
            continue;
        }
        match parse_authenticated_proxy_line(line) {
            Ok(node) => nodes.push(node),
            Err(error) => errors.push(format!("第 {} 行代理 IP：{error}", index + 1)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_authenticated_proxy_lines() {
        // 真实样例：代理商按「一行一个」发的纯文本，界面上一直写着支持这个格式。
        let text = "163.128.72.103:5774:24A3LUWA16312872103A5774:VXeOGZwy106b\n\
                    163.128.72.193:7690:24A3LUXA16312872193A7690:X25LE7kCcVDQ";
        let (nodes, errors) = parse_import_content(text).expect("应该能导入");
        assert!(errors.is_empty(), "不该有错误：{errors:?}");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["type"], "http");
        assert_eq!(nodes[0]["server"], "163.128.72.103");
        assert_eq!(nodes[0]["port"], 5774);
        assert_eq!(nodes[0]["username"], "24A3LUWA16312872103A5774");
        assert_eq!(nodes[0]["password"], "VXeOGZwy106b");
        assert_eq!(nodes[0]["name"], "HTTP Proxy 163.128.72.103:5774");
        assert_eq!(nodes[1]["port"], 7690);
    }

    #[test]
    fn proxy_line_accepts_ipv6_and_colons_in_password() {
        let node = parse_authenticated_proxy_line("[fd00::1]:8080:user:p:a:ss").unwrap();
        assert_eq!(node["server"], "fd00::1");
        assert_eq!(node["port"], 8080);
        assert_eq!(node["password"], "p:a:ss");

        let node = parse_authenticated_proxy_line("proxy.example.com:3128:user:pass").unwrap();
        assert_eq!(node["server"], "proxy.example.com");
    }

    #[test]
    fn proxy_line_rejects_bad_input() {
        // 端口越界 / 非数字
        assert!(parse_authenticated_proxy_line("1.2.3.4:0:u:p").is_err());
        assert!(parse_authenticated_proxy_line("1.2.3.4:70000:u:p").is_err());
        assert!(parse_authenticated_proxy_line("1.2.3.4:abc:u:p").is_err());
        // 密码为空或含空白
        assert!(parse_authenticated_proxy_line("1.2.3.4:80:u:").is_err());
        assert!(parse_authenticated_proxy_line("1.2.3.4:80:u:p w").is_err());
        // 少字段
        assert!(parse_authenticated_proxy_line("1.2.3.4:80:u").is_err());
        // 主机名不合法
        assert!(parse_authenticated_proxy_line("bad_host!:80:u:p").is_err());
    }

    #[test]
    fn base64_subscription_is_not_eaten_by_proxy_line_parser() {
        // 整段 Base64 的订阅没有冒号，不能被当成「代理 IP 格式错误」而挡住 Base64 兜底。
        let encoded = "c3M6Ly9ZV1Z6TFRJMU5pMW5ZMjA2YzJWamNtVjBAMS4yLjMuNDo4Mzg4I2pw";
        let (nodes, errors) = parse_import_content(encoded).expect("Base64 订阅应该能解");
        assert!(errors.is_empty(), "不该有错误：{errors:?}");
        assert_eq!(nodes.len(), 1);
        assert_eq!(nodes[0]["type"], "ss");
        assert_eq!(nodes[0]["server"], "1.2.3.4");
    }

    #[test]
    fn mixed_content_keeps_both_links_and_proxy_lines() {
        let text = "ss://YWVzLTI1Ni1nY206c2VjcmV0@1.2.3.4:8388#jp\n\
                    163.128.72.103:5774:24A3LUWA16312872103A5774:VXeOGZwy106b";
        let (nodes, errors) = parse_import_content(text).expect("应该能导入");
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0]["type"], "ss");
        assert_eq!(nodes[1]["type"], "http");
    }
}
