use serde_json::{json, Value};
use std::net::IpAddr;

use crate::models::{AppConfig, RoutingRule, NODE_DIALER_POLICY_KEY, NODE_DIALER_PROXY_KEY};

const TARGET_NAMES: &[(&str, &str)] = &[
    ("CLASH", "UPSTREAM-CLASH"),
    ("V2RAY", "UPSTREAM-V2RAY"),
    ("SSH", "UPSTREAM-SSH"),
    ("BUILTIN", "IMPORTED-NODES"),
    ("DIRECT", "DIRECT"),
];

const UPSTREAM_PROCESSES: &[&str] = &[
    "verge-mihomo.exe",
    "clash.exe",
    "clash-win64.exe",
    "v2rayN.exe",
    "xray.exe",
    "v2ray.exe",
    "NetworkManager.exe",
    "network-manager.exe",
    "network-manager-headless",
    "mihomo",
    "sshd",
    "ssh",
];

const LAN_IPV4_CIDRS: &[&str] = &[
    "0.0.0.0/8",
    "10.0.0.0/8",
    "100.64.0.0/10",
    "127.0.0.0/8",
    "169.254.0.0/16",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "224.0.0.0/4",
    "240.0.0.0/4",
];

const LAN_IPV6_CIDRS: &[&str] = &["::/128", "::1/128", "fc00::/7", "fe80::/10", "ff00::/8"];

fn target_name(target: &str) -> String {
    TARGET_NAMES
        .iter()
        .find(|(key, _)| *key == target)
        .map(|(_, value)| (*value).to_string())
        .unwrap_or_else(|| "DIRECT".to_string())
}

pub fn normalize_rule_value(rule_type: &str, value: &str) -> String {
    let text = value.trim();
    match rule_type {
        "DOMAIN" | "DOMAIN-SUFFIX" | "DOMAIN-KEYWORD" => text.trim_end_matches('.').to_lowercase(),
        _ => text.to_string(),
    }
}

fn rule_line(rule: &RoutingRule) -> String {
    let value = normalize_rule_value(&rule.rule_type, &rule.value);
    let suffix = if rule.rule_type == "IP-CIDR" {
        ",no-resolve"
    } else {
        ""
    };
    format!(
        "{},{},{}{}",
        rule.rule_type,
        value,
        target_name(&rule.target),
        suffix
    )
}

fn route_exclusions(config: &AppConfig) -> Vec<String> {
    let mut hosts = vec![config.clash.host.clone(), config.v2ray.host.clone()];
    for node in &config.imported_nodes {
        if node.dialer_proxy().is_empty() {
            hosts.push(
                node.config
                    .get("server")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string(),
            );
        }
    }
    let mut exclusions: Vec<String> = Vec::new();
    for raw in hosts {
        let host = raw.trim().trim_matches(|c| c == '[' || c == ']');
        if let Ok(address) = host.parse::<IpAddr>() {
            let cidr = match address {
                IpAddr::V4(_) => format!("{address}/32"),
                IpAddr::V6(_) => format!("{address}/128"),
            };
            if !exclusions.contains(&cidr) {
                exclusions.push(cidr);
            }
        }
    }
    exclusions
}

fn dialer_route_rules(config: &AppConfig) -> Vec<String> {
    let names: Vec<String> = config.imported_nodes.iter().map(|n| n.name()).collect();
    let mut rules: Vec<String> = Vec::new();
    let mut seen: Vec<(String, String)> = Vec::new();
    for node in &config.imported_nodes {
        let relay = node.dialer_proxy();
        let host = node
            .config
            .get("server")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .trim_matches(|c| c == '[' || c == ']')
            .trim_end_matches('.')
            .to_string();
        let key = (host.to_lowercase(), relay.clone());
        if host.is_empty()
            || !names.contains(&relay)
            || relay == node.name()
            || seen.contains(&key)
        {
            continue;
        }
        seen.push(key);
        match host.parse::<IpAddr>() {
            Err(_) => rules.push(format!("DOMAIN,{},{relay}", host.to_lowercase())),
            Ok(IpAddr::V6(address)) => {
                rules.push(format!("IP-CIDR6,{address}/128,{relay},no-resolve"))
            }
            Ok(IpAddr::V4(address)) => {
                rules.push(format!("IP-CIDR,{address}/32,{relay},no-resolve"))
            }
        }
    }
    rules
}

pub fn build(config: &AppConfig) -> Value {
    let mut proxies: Vec<Value> = Vec::new();
    if config.clash.enabled {
        proxies.push(json!({
            "name": target_name("CLASH"),
            "type": config.clash.protocol,
            "server": config.clash.host,
            "port": config.clash.port,
            "udp": config.clash.protocol == "socks5",
        }));
    }
    if config.v2ray.enabled {
        proxies.push(json!({
            "name": target_name("V2RAY"),
            "type": config.v2ray.protocol,
            "server": config.v2ray.host,
            "port": config.v2ray.port,
            "udp": config.v2ray.protocol == "socks5",
        }));
    }
    let ssh_target_used = config.mode == "GLOBAL_SSH"
        || (config.mode == "RULE" && config.default_target == "SSH")
        || config
            .rules
            .iter()
            .any(|rule| rule.enabled && rule.target == "SSH");
    let ssh_profile = config
        .ssh_servers
        .iter()
        .find(|profile| ssh_target_used && profile.profile_id == config.selected_ssh_server);
    if let Some(profile) = ssh_profile {
        proxies.push(json!({
            "name": target_name("SSH"),
            "type": "socks5",
            "server": "127.0.0.1",
            "port": profile.local_port,
            "udp": false,
        }));
    }
    let imported_names: Vec<String> = config.imported_nodes.iter().map(|n| n.name()).collect();
    for node in &config.imported_nodes {
        if let Value::Object(map) = &node.config {
            let mut proxy = serde_json::Map::new();
            let mut dialer = String::new();
            for (key, value) in map {
                if key == NODE_DIALER_PROXY_KEY {
                    dialer = value.as_str().unwrap_or("").trim().to_string();
                    continue;
                }
                if key == NODE_DIALER_POLICY_KEY {
                    continue;
                }
                proxy.insert(key.clone(), value.clone());
            }
            if imported_names.contains(&dialer) && dialer != node.name() {
                proxy.insert("dialer-proxy".into(), Value::String(dialer));
            }
            proxies.push(Value::Object(proxy));
        }
    }

    let mut rules: Vec<String> = UPSTREAM_PROCESSES
        .iter()
        .map(|name| format!("PROCESS-NAME,{name},DIRECT"))
        .collect();
    rules.push("DST-PORT,22,DIRECT".into());
    rules.extend(dialer_route_rules(config));
    rules.extend(
        ["lan", "local", "home.arpa"]
            .iter()
            .map(|suffix| format!("DOMAIN-SUFFIX,{suffix},DIRECT")),
    );
    rules.extend(
        LAN_IPV4_CIDRS
            .iter()
            .map(|cidr| format!("IP-CIDR,{cidr},DIRECT,no-resolve")),
    );
    rules.extend(
        LAN_IPV6_CIDRS
            .iter()
            .map(|cidr| format!("IP-CIDR6,{cidr},DIRECT,no-resolve")),
    );
    let final_target = match config.mode.as_str() {
        "RULE" => {
            rules.extend(
                config
                    .rules
                    .iter()
                    .filter(|rule| rule.enabled)
                    .map(rule_line),
            );
            target_name(&config.default_target)
        }
        "GLOBAL_CLASH" => target_name("CLASH"),
        "GLOBAL_V2RAY" => target_name("V2RAY"),
        "GLOBAL_SSH" => target_name("SSH"),
        "GLOBAL_BUILTIN" => target_name("BUILTIN"),
        "SMART" => "SMART-NODES".to_string(),
        _ => "DIRECT".to_string(),
    };
    rules.push(format!("MATCH,{final_target}"));

    let mut result = json!({
        "mixed-port": config.mixed_port,
        "allow-lan": false,
        "bind-address": "127.0.0.1",
        "mode": "rule",
        "log-level": "info",
        "ipv6": false,
        "unified-delay": true,
        "tcp-concurrent": true,
        "keep-alive-interval": 15,
        "keep-alive-idle": 15,
        "disable-keep-alive": false,
        "find-process-mode": "strict",
        "external-controller": format!("127.0.0.1:{}", config.controller_port),
        "secret": config.controller_secret,
        "profile": {"store-selected": false, "store-fake-ip": true},
        "tun": {
            "enable": true,
            "stack": "mixed",
            "device": "NetWorkManger",
            "auto-route": true,
            "auto-detect-interface": true,
            "strict-route": config.strict_route,
            "dns-hijack": ["any:53", "tcp://any:53"],
            // 必须是扁平的 CIDR 字符串列表：mihomo 用 netip.Prefix 解析，
            // 嵌套数组会让它启动失败并报 "cannot unmarshal !!seq into netip.Prefix"。
            "route-exclude-address": LAN_IPV4_CIDRS
                .iter()
                .chain(LAN_IPV6_CIDRS.iter())
                .map(|cidr| (*cidr).to_string())
                .chain(route_exclusions(config))
                .collect::<Vec<String>>(),
        },
        "sniffer": {
            "enable": true,
            "force-dns-mapping": true,
            "parse-pure-ip": true,
            "override-destination": true,
            // 端口必须是整数：mihomo 按 []int 解析，写成字符串会让嗅探规则失效。
            "sniff": {
                "HTTP": {"ports": [80, "8080-8880"]},
                "TLS": {"ports": [443, 8443]},
                "QUIC": {"ports": [443, 8443]},
            },
            "skip-domain": ["Mijia Cloud", "+.push.apple.com"],
        },
        "dns": {
            "enable": true,
            "listen": format!("127.0.0.1:{}", config.dns_port),
            "ipv6": false,
            "enhanced-mode": "fake-ip",
            "fake-ip-range": "198.18.0.1/16",
            "fake-ip-filter": [
                "+.lan", "+.local", "localhost.ptlogin2.qq.com",
                "time.windows.com", "time.nist.gov"
            ],
            "default-nameserver": ["223.5.5.5", "1.1.1.1"],
            "nameserver": [
                "https://dns.alidns.com/dns-query",
                "https://1.1.1.1/dns-query"
            ],
        },
        "proxies": proxies,
        "rules": rules,
    });

    if !config.imported_nodes.is_empty() {
        let mut node_names: Vec<String> = config.imported_nodes.iter().map(|n| n.name()).collect();
        if node_names.contains(&config.selected_node) {
            node_names.retain(|name| name != &config.selected_node);
            node_names.insert(0, config.selected_node.clone());
        }
        result["proxy-groups"] = json!([
            {"name": target_name("BUILTIN"), "type": "select", "proxies": node_names},
            {
                "name": "SMART-NODES",
                "type": "url-test",
                "proxies": node_names,
                "url": "https://www.gstatic.com/generate_204",
                "interval": 60,
                "tolerance": 120,
                "lazy": config.mode != "SMART",
                "timeout": 6000,
                "max-failed-times": 2,
                "expected-status": 204,
            }
        ]);
    }
    result
}

pub fn render_yaml(config: &AppConfig) -> Result<String, String> {
    let value = build(config);
    serde_yaml::to_string(&value).map_err(|err| format!("生成配置失败：{err}"))
}
