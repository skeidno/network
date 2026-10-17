use serde_json::{json, Value};
use std::net::IpAddr;

use crate::models::{
    AppConfig, LocalDnsEntry, RoutingRule, NODE_DIALER_POLICY_KEY, NODE_DIALER_PROXY_KEY,
};

pub const TARGET_NAMES: &[(&str, &str)] = &[
    ("CLASH", "UPSTREAM-CLASH"),
    ("V2RAY", "UPSTREAM-V2RAY"),
    ("SSH", "UPSTREAM-SSH"),
    ("BUILTIN", "IMPORTED-NODES"),
    ("DIRECT", "DIRECT"),
];

/// 出口检测专用的入站监听名。
///
/// 为什么必须单开一个入站：规则第一条是 `PROCESS-NAME,NetworkManager.exe,DIRECT`，
/// 它保证「直连出口」能测到本机真实 IP（否则本程序自己的请求也会被 TUN 送进节点）。
/// 但同一条规则会把「代理出口」的检测请求也变成直连 —— 两个出口永远相同。
/// 所以代理检测改走这个专用入站，并在规则最前面用 `IN-PORT` 把它强制指向内置节点组，
/// 优先级高于进程名规则。
pub const PROBE_LISTENER_NAME: &str = "egress-probe";

/// 检测入站端口：由混合端口**确定性派生**（紧邻的一个不冲突值）。
///
/// 不能用「找一个当前空闲的端口」：内核正常运行时该端口已被内核占用，
/// 再探测会被判成"被占用"而换一个，于是配置里写的端口和内核实际监听的对不上。
pub fn probe_port(config: &AppConfig) -> i32 {
    let mut port = (config.mixed_port + 1).clamp(1, 65535);
    for _ in 0..8 {
        if port != config.controller_port && port != config.dns_port && port < 65535 {
            return port;
        }
        port += 1;
    }
    port
}

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

/// fake-ip 黑名单：名单里的域名永远回真实 IP，不发 198.18.x.x 假地址。
///
/// 收三类域名：
/// - 本机/局域网：本就不该出网，假地址毫无意义；
/// - 系统级服务：NTP 校时（UDP/123，不过 HTTPS）、Windows/Apple/Google 的连通性探测
///   （探测结果决定托盘图标和 captive portal 弹窗），拿到假地址会直接坏；
/// - 会校验对端地址的公共服务（验证码类），假地址段会被拒。
///
/// 国内直连站点**不进**这张表：那不是黑名单能穷尽的，一律回真实 IP 的诉求
/// 由设置里的「DNS 兼容模式」（redir-host）兜底，见 AppConfig::dns_mode。
const FAKE_IP_FILTER: &[&str] = &[
    // 本机与局域网
    "localhost",
    "+.lan",
    "+.local",
    "+.home.arpa",
    "+.internal",
    "+.localhost",
    // NTP 校时
    "time.windows.com",
    "time.nist.gov",
    "time.apple.com",
    "+.pool.ntp.org",
    "+.ntp.org.cn",
    "ntp.aliyun.com",
    "ntp.tencent.com",
    "time1.google.com",
    "time2.google.com",
    "time3.google.com",
    "time4.google.com",
    // 系统连通性探测与推送
    "+.msftconnecttest.com",
    "+.msftncsi.com",
    "+.msftdns.net",
    "captive.apple.com",
    "+.push.apple.com",
    "connectivitycheck.gstatic.com",
    "connectivitycheck.platform.hicloud.com",
    // 验证码（会校验对端地址段）
    "challenges.cloudflare.com",
    "+.hcaptcha.com",
    "+.recaptcha.net",
    // QQ 快速登录组件
    "localhost.ptlogin2.qq.com",
];

/// 判断地址是否落在某个 CIDR 里。只用来做「这个映射是不是指向内网」这一个判断，
/// 所以不引第三方库，前缀位运算就够了。
fn cidr_contains(cidr: &str, address: IpAddr) -> bool {
    let Some((network, prefix)) = cidr.split_once('/') else {
        return false;
    };
    let (Ok(network), Ok(prefix)) = (network.parse::<IpAddr>(), prefix.parse::<u8>()) else {
        return false;
    };
    match (network, address) {
        (IpAddr::V4(net), IpAddr::V4(addr)) if prefix <= 32 => {
            let mask = if prefix == 0 { 0 } else { u32::MAX << (32 - prefix) };
            u32::from(net) & mask == u32::from(addr) & mask
        }
        (IpAddr::V6(net), IpAddr::V6(addr)) if prefix <= 128 => {
            let mask = if prefix == 0 {
                0
            } else {
                u128::MAX << (128 - prefix)
            };
            u128::from(net) & mask == u128::from(addr) & mask
        }
        _ => false,
    }
}

/// 映射目标是不是内网地址。
///
/// 和 Android 端用的是同一份局域网网段清单（那里叫 `lanCidrs`），判定口径要一致，
/// 否则同一个映射在手机和电脑上会走向不同的出口。
fn is_lan_address(address: &str) -> bool {
    let Some(parsed) = crate::models::parse_ip_literal(address) else {
        return false;
    };
    LAN_IPV4_CIDRS
        .iter()
        .chain(LAN_IPV6_CIDRS.iter())
        .any(|cidr| cidr_contains(cidr, parsed))
}

/// 本地 DNS 映射里指向内网地址的那几条，生成显式直连规则。
///
/// 局域网里的服务交给节点毫无意义，而兜底的 `IP-CIDR` 规则在这个场景下不一定命中：
/// TUN 开着嗅探、`override-destination` 又是 true 时，规则匹配看到的是域名而不是 IP。
/// 这里补一条域名规则，让「映射到内网」= 「直连」这个结论和 Android 端完全一致。
fn local_dns_direct_rules(config: &AppConfig) -> Vec<String> {
    config
        .local_dns
        .iter()
        .filter(|entry| !entry.domain.is_empty() && is_lan_address(&entry.address))
        .map(|entry| match entry.wildcard_suffix() {
            Some(suffix) => format!("DOMAIN-SUFFIX,{},DIRECT", suffix.to_lowercase()),
            None => format!("DOMAIN,{},DIRECT", entry.domain.to_lowercase()),
        })
        .collect()
}

/// hosts 表的键。通配条目翻译成 `+.domain`，不是 `*.domain`。
///
/// 实测（mihomo v1.19.32）：`*.demo.test` 只命中 `a.demo.test`，查询 `demo.test` 本身会漏；
/// `+.demo.test` 才是「自身 + 所有子域」，正好对上 `*.baidu.com` 在两端定下的语义。
fn local_dns_hosts_key(entry: &LocalDnsEntry) -> String {
    match entry.wildcard_suffix() {
        Some(suffix) => format!("+.{suffix}"),
        None => entry.domain.clone(),
    }
}

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

/// 统一中转是否真的生效：开了开关、且中转节点确实还在节点列表里。
pub fn relay_is_active(config: &AppConfig) -> bool {
    !global_relay(config).is_empty()
}

/// 当前生效的统一中转节点名；没开或名字不存在则返回空。
fn global_relay(config: &AppConfig) -> String {
    if !config.relay_enabled {
        return String::new();
    }
    let name = config.relay_node.trim();
    if name.is_empty() || !config.imported_nodes.iter().any(|n| n.name() == name) {
        return String::new();
    }
    name.to_string()
}

fn route_exclusions(config: &AppConfig) -> Vec<String> {
    let mut hosts = vec![config.clash.host.clone(), config.v2ray.host.clone()];
    let relay = global_relay(config);
    for node in &config.imported_nodes {
        // 开了统一中转，本机唯一需要直连的就是中转自己 —— 其余节点的连接都
        // 是中转替我们发出去的，本机根本不会碰它们，没必要也不可能在这里路由排除。
        if relay.is_empty() || node.name() == relay {
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

/// DNS 段。
///
/// 应答模式由 `dns_mode` 决定：
/// - `fake-ip`（默认）：不在 [`FAKE_IP_FILTER`] 里的域名一律回 198.18.x.x 假地址，
///   靠 TUN 还原域名、再按规则匹配（最快）；
/// - `redir-host`：所有域名都回真实 IP，域名匹配交给连接时的嗅探器。这是给
///   「黑名单永远堵不全」准备的兼容模式 —— 某些应用会把假地址缓存、上报或拿去做
///   不走 TUN 的连接，开启接管后就会异常，切到这个模式能救回来。
///
/// 两种模式共用的解析器策略：
/// - `default-nameserver` 只负责解析下面那几个 DoH 的域名本身，必须是无域名依赖的纯 IP；
/// - `nameserver` 是主解析器，国内 DoH 优先、境外兜底；
/// - `respect-rules: true` 让解析也按规则表分流（配合 `direct-nameserver`）：
///   命中直连的域名用国内 DNS 拿真实 IP，更快也不容易被污染；
/// - `proxy-server-nameserver` 固定节点服务器域名的解析器，
///   避免「要先连上节点才能解析节点域名」的死循环。
fn dns_section(config: &AppConfig) -> Value {
    let compat = config.dns_mode.eq_ignore_ascii_case("redir-host");
    let mut dns = json!({
        "enable": true,
        "listen": format!("127.0.0.1:{}", config.dns_port),
        "ipv6": false,
        "enhanced-mode": if compat { "redir-host" } else { "fake-ip" },
        "default-nameserver": ["223.5.5.5", "119.29.29.29"],
        "nameserver": [
            "https://223.5.5.5/dns-query",
            "https://doh.pub/dns-query",
            "https://1.1.1.1/dns-query"
        ],
        "respect-rules": true,
        "direct-nameserver": [
            "https://223.5.5.5/dns-query",
            "https://119.29.29.29/dns-query"
        ],
        "proxy-server-nameserver": [
            "https://223.5.5.5/dns-query",
            "https://119.29.29.29/dns-query"
        ],
    });
    // redir-host 下不分配假地址，这两个字段没有意义，留着反而会让内核读到不一致的状态。
    if !compat {
        dns["fake-ip-range"] = json!("198.18.0.1/16");
        dns["fake-ip-filter"] = json!(FAKE_IP_FILTER);
    }
    dns
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
    let relay = global_relay(config);
    for node in &config.imported_nodes {
        if let Value::Object(map) = &node.config {
            let mut proxy = serde_json::Map::new();
            for (key, value) in map {
                // 老版本存在节点上的前置设置一律作废：中转现在是全局一个，
                // 混着用只会出现「有的节点有前置、有的没有」这种最难排查的情况。
                if key == NODE_DIALER_PROXY_KEY || key == NODE_DIALER_POLICY_KEY {
                    continue;
                }
                proxy.insert(key.clone(), value.clone());
            }
            // 中转自己不能挂自己，否则握手时会无限找拨号代理。
            if !relay.is_empty() && relay != node.name() {
                proxy.insert("dialer-proxy".into(), Value::String(relay.clone()));
            }
            proxies.push(Value::Object(proxy));
        }
    }

    // 检测入站必须排在最前面：它要压过下面的 `PROCESS-NAME,NetworkManager.exe,DIRECT`，
    // 否则代理出口检测又会退化成直连（见 PROBE_LISTENER_NAME 的说明）。
    let probe_port = probe_port(config);
    let mut rules: Vec<String> = Vec::new();
    if !config.imported_nodes.is_empty() {
        rules.push(format!("IN-PORT,{probe_port},{}", target_name("BUILTIN")));
    }
    rules.extend(
        UPSTREAM_PROCESSES
            .iter()
            .map(|name| format!("PROCESS-NAME,{name},DIRECT")),
    );
    rules.push("DST-PORT,22,DIRECT".into());
    // 本地 DNS 映射里指向内网的域名排在局域网后缀规则之前，和用户自建规则相比也是
    // 先命中先算 —— 映射是用户明确写下的意图，优先级应当高于后面的通用规则。
    rules.extend(local_dns_direct_rules(config));
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
                    // 空值规则必须排除：`DOMAIN-SUFFIX,,X` 会让 mihomo 直接 fatal 退出，
                    // 整个接管都起不来（见 RoutingRule::is_complete 的说明）。
                    .filter(|rule| rule.enabled && rule.is_complete())
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
        "dns": dns_section(config),
        "proxies": proxies,
        "rules": rules,
        // 只给出口检测用的入站：绑定回环，外部访问不到，也不参与 TUN。
        "listeners": [
            {
                "name": PROBE_LISTENER_NAME,
                "type": "mixed",
                "port": probe_port,
                "listen": "127.0.0.1",
            }
        ],
    });

    // 本地 DNS 映射（自定义域名 → 固定 IP）。`hosts` 是内核查表，命中就返回写死的
    // 地址、不往外发查询，所以局域网里的自建服务不会被上游 DNS 的缓存/劫持带偏。
    // 通配条目在这里翻译成 `+.domain`（见 local_dns_hosts_key）。
    // 只在真的有映射时才写这个键：没有映射的用户拿到的配置和以前逐字节一致。
    if !config.local_dns.is_empty() {
        result["hosts"] = Value::Object(
            config
                .local_dns
                .iter()
                .map(|entry| (local_dns_hosts_key(entry), Value::String(entry.address.clone())))
                .collect(),
        );
    }

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::LocalDnsEntry;

    fn config_with(entries: &[(&str, &str)]) -> AppConfig {
        let mut config = AppConfig::default();
        config.local_dns = entries
            .iter()
            .map(|(domain, address)| LocalDnsEntry {
                domain: (*domain).into(),
                address: (*address).into(),
            })
            .collect();
        config
    }

    fn rules_of(value: &Value) -> Vec<String> {
        value["rules"]
            .as_array()
            .expect("rules 应当是数组")
            .iter()
            .map(|rule| rule.as_str().unwrap_or_default().to_string())
            .collect()
    }

    fn filter_of(value: &Value) -> Vec<String> {
        value["dns"]["fake-ip-filter"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .map(|item| item.as_str().unwrap_or_default().to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    #[test]
    fn a_config_without_mappings_looks_exactly_as_before() {
        // 没用这个功能的用户，生成的配置不该多出 hosts 键。
        let value = build(&AppConfig::default());
        assert!(value.get("hosts").is_none());
        // 默认必须是 fake-ip 模式，且带上 fake-ip 段。
        assert_eq!(value["dns"]["enhanced-mode"], "fake-ip");
        assert_eq!(value["dns"]["fake-ip-range"], "198.18.0.1/16");
        assert!(!filter_of(&value).is_empty());
    }

    #[test]
    fn fake_ip_filter_keeps_the_system_critical_classes() {
        // 黑名单要覆盖三类「拿到假地址就会坏」的域名：本机/局域网、NTP 校时、连通性探测。
        let filter = filter_of(&build(&AppConfig::default()));
        for expected in [
            "+.lan",
            "+.local",
            "time.windows.com",
            "time.apple.com",
            "+.msftconnecttest.com",
            "captive.apple.com",
            "connectivitycheck.gstatic.com",
        ] {
            assert!(filter.contains(&expected.to_string()), "缺少 {expected}：{filter:?}");
        }
    }

    #[test]
    fn dns_resolution_follows_the_rules() {
        // respect-rules 与两个按角色区分的解析器：直连用国内 DNS，节点域名也用国内 DNS
        // （否则「要连上节点才能解析节点域名」就成了死循环）。
        let value = build(&AppConfig::default());
        assert_eq!(value["dns"]["respect-rules"], true);
        assert!(value["dns"]["direct-nameserver"].is_array());
        assert!(value["dns"]["proxy-server-nameserver"].is_array());
        // default-nameserver 只能是无域名依赖的纯 IP，否则自举解析会打转。
        for server in value["dns"]["default-nameserver"].as_array().unwrap() {
            assert!(
                server.as_str().unwrap().parse::<IpAddr>().is_ok(),
                "default-nameserver 必须是纯 IP：{server}"
            );
        }
    }

    #[test]
    fn redir_host_mode_drops_the_fake_ip_bits() {
        // 兼容模式：所有域名都回真实 IP，fake-ip 的两个字段都不该出现。
        let mut config = AppConfig::default();
        config.dns_mode = "redir-host".into();
        let value = build(&config);
        assert_eq!(value["dns"]["enhanced-mode"], "redir-host");
        assert!(value["dns"].get("fake-ip-range").is_none());
        assert!(value["dns"].get("fake-ip-filter").is_none());
    }

    #[test]
    fn unknown_dns_mode_falls_back_to_fake_ip() {
        let mut config = AppConfig::default();
        config.dns_mode = "nonsense".into();
        assert_eq!(build(&config)["dns"]["enhanced-mode"], "fake-ip");
    }

    #[test]
    fn mappings_become_a_hosts_table() {
        let value = build(&config_with(&[("nas.lan", "192.168.1.10")]));
        assert_eq!(value["hosts"]["nas.lan"], "192.168.1.10");
    }

    #[test]
    fn wildcard_mappings_are_translated_to_the_plus_syntax() {
        // `*.baidu.com` 要翻成 `+.baidu.com`：实测 mihomo 的 `*.x` 不含 x 本身。
        let value = build(&config_with(&[("*.baidu.com", "127.0.0.1")]));
        assert_eq!(value["hosts"]["+.baidu.com"], "127.0.0.1");
        assert!(value["hosts"].get("*.baidu.com").is_none());
    }

    #[test]
    fn wildcard_lan_mappings_become_domain_suffix_rules() {
        let value = build(&config_with(&[("*.baidu.com", "10.0.2.2")]));
        assert!(
            rules_of(&value).contains(&"DOMAIN-SUFFIX,baidu.com,DIRECT".to_string()),
            "{:?}",
            rules_of(&value)
        );
    }

    #[test]
    fn wildcard_public_mappings_keep_normal_routing() {
        let value = build(&config_with(&[("*.cdn.example.com", "1.1.1.1")]));
        assert!(!rules_of(&value).contains(&"DOMAIN-SUFFIX,cdn.example.com,DIRECT".to_string()));
    }

    #[test]
    fn fake_ip_filter_is_left_alone() {
        // `hosts` 命中就返回固定 IP，fake-ip 阶段根本轮不到它 —— 不往 filter 里塞域名，
        // 免得哪天 hosts 那条路没走上时，反而把一个只存在于内网的域名推给公网 DNS。
        let value = build(&config_with(&[("nas.lan", "192.168.1.10")]));
        let filter = filter_of(&value);
        assert!(!filter.contains(&"nas.lan".to_string()), "{filter:?}");
    }

    #[test]
    fn lan_mappings_are_forced_direct() {
        // 各网段各挑一个，确保内网判定用的是和 Android 端同一份清单。
        for address in ["192.168.1.10", "10.0.0.5", "172.16.3.4", "127.0.0.1", "fd00::5"] {
            let value = build(&config_with(&[("nas.lan", address)]));
            assert!(
                rules_of(&value).contains(&"DOMAIN,nas.lan,DIRECT".to_string()),
                "{address} 应当被判成内网地址"
            );
        }
    }

    #[test]
    fn public_mappings_keep_normal_routing() {
        // 映射到公网地址只是钉住解析结果，走哪个出口仍然交给分流规则决定。
        let value = build(&config_with(&[("cdn.example.com", "1.1.1.1")]));
        assert!(!rules_of(&value).contains(&"DOMAIN,cdn.example.com,DIRECT".to_string()));
    }

    #[test]
    fn lan_mappings_win_over_user_rules() {
        // 用户给同一个域名写了「走节点」的规则：映射是更明确的一条，先命中先算，
        // 所以映射的直连规则必须排在用户规则前面。
        let mut config = config_with(&[("nas.lan", "192.168.1.10")]);
        config.rules = vec![crate::models::RoutingRule {
            rule_type: "DOMAIN-SUFFIX".into(),
            value: "nas.lan".into(),
            target: "CLASH".into(),
            ..Default::default()
        }];
        let rules = rules_of(&build(&config));
        let mapping = rules
            .iter()
            .position(|rule| rule == "DOMAIN,nas.lan,DIRECT")
            .expect("应当有映射直连规则");
        let user_rule = rules
            .iter()
            .position(|rule| rule == "DOMAIN-SUFFIX,nas.lan,UPSTREAM-CLASH")
            .expect("应当有用户规则");
        assert!(mapping < user_rule, "映射规则要排在用户规则之前：{rules:?}");
    }

    fn relay_config(names: &[&str]) -> AppConfig {
        let mut config = AppConfig::default();
        config.imported_nodes = names
            .iter()
            .map(|name| {
                let mut map = serde_json::Map::new();
                map.insert("name".into(), Value::String((*name).into()));
                map.insert("type".into(), Value::String("http".into()));
                map.insert("server".into(), Value::String("1.2.3.4".into()));
                map.insert("port".into(), json!(8080));
                crate::models::ImportedNode {
                    config: Value::Object(map),
                    ..Default::default()
                }
            })
            .collect();
        config
    }

    #[test]
    fn global_relay_chains_every_other_outbound() {
        let mut config = relay_config(&["中转", "节点 A", "节点 B"]);
        config.relay_enabled = true;
        config.relay_node = "中转".into();
        let proxies = build(&config)["proxies"].as_array().expect("proxies").clone();
        let dialer = |name: &str| {
            proxies
                .iter()
                .find(|proxy| proxy["name"] == Value::String(name.into()))
                .and_then(|proxy| proxy.get("dialer-proxy"))
                .and_then(|value| value.as_str())
                .map(|value| value.to_string())
        };
        assert_eq!(dialer("节点 A").as_deref(), Some("中转"));
        assert_eq!(dialer("节点 B").as_deref(), Some("中转"));
        // 中转自己不能挂自己，否则握手时会无限找拨号代理。
        assert_eq!(dialer("中转"), None);
    }

    #[test]
    fn relay_off_or_missing_node_changes_nothing() {
        let mut config = relay_config(&["节点 A", "节点 B"]);
        // 没开开关。
        let proxies = build(&config)["proxies"].as_array().expect("proxies").clone();
        assert!(!proxies.iter().any(|proxy| proxy.get("dialer-proxy").is_some()));
        // 开了但名字对不上节点列表：算没生效，不能把 dialer-proxy 指向一个不存在的出站。
        config.relay_enabled = true;
        config.relay_node = "查无此节点".into();
        assert!(!crate::mihomo::relay_is_active(&config));
        let proxies = build(&config)["proxies"].as_array().expect("proxies").clone();
        assert!(!proxies.iter().any(|proxy| proxy.get("dialer-proxy").is_some()));
    }

    #[test]
    fn stale_per_node_relay_settings_are_dropped() {
        let mut config = relay_config(&["节点 A"]);
        if let Value::Object(map) = &mut config.imported_nodes[0].config {
            map.insert(
                super::NODE_DIALER_PROXY_KEY.into(),
                Value::String("某个旧前置".into()),
            );
        }
        let proxies = build(&config)["proxies"].as_array().expect("proxies").clone();
        assert!(
            !proxies.iter().any(|proxy| proxy.get("dialer-proxy").is_some()),
            "节点上残留的旧前置设置必须作废：{proxies:?}"
        );
    }

    #[test]
    fn incomplete_rules_never_reach_the_kernel() {
        // 空值规则渲染成 `DOMAIN-SUFFIX,,IMPORTED-NODES` 会让 mihomo 直接 fatal
        // 退出（missing subsequent parameters），整个接管起不来 —— 所以必须过滤掉。
        let mut config = config_with(&[]);
        config.rules = vec![
            crate::models::RoutingRule {
                rule_type: "DOMAIN-SUFFIX".into(),
                value: "example.com".into(),
                target: "CLASH".into(),
                ..Default::default()
            },
            // 就是这条：value 为空。
            crate::models::RoutingRule {
                rule_type: "DOMAIN-SUFFIX".into(),
                value: String::new(),
                target: "BUILTIN".into(),
                ..Default::default()
            },
            crate::models::RoutingRule {
                rule_type: String::new(),
                value: "whatever.com".into(),
                target: "BUILTIN".into(),
                ..Default::default()
            },
        ];
        let rules = rules_of(&build(&config));
        assert!(rules.contains(&"DOMAIN-SUFFIX,example.com,UPSTREAM-CLASH".to_string()));
        assert!(
            !rules.iter().any(|rule| rule.contains(",,")),
            "不该出现空值规则：{rules:?}"
        );
        assert_eq!(rules.iter().filter(|rule| rule.contains("example.com")).count(), 1);
    }

    #[test]
    fn cidr_matching_agrees_with_the_lan_list() {
        let address = |text: &str| text.parse::<IpAddr>().expect("测试用的都是字面量");
        assert!(is_lan_address("192.168.1.10"));
        assert!(is_lan_address("100.64.0.1"));
        assert!(is_lan_address("fd00::5"));
        assert!(!is_lan_address("8.8.8.8"));
        assert!(!is_lan_address("2001:4860:4860::8888"));
        // 不能把域名字符串当成 IP（那会触发真实解析），这里必须是 false。
        assert!(!is_lan_address("router.lan"));
        assert!(cidr_contains("192.168.0.0/16", address("192.168.255.254")));
        assert!(!cidr_contains("192.168.0.0/16", address("192.169.0.1")));
        assert!(cidr_contains("0.0.0.0/0", address("8.8.8.8")));
    }
}
