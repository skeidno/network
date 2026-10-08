//! 跨设备配置导出 / 导入。
//!
//! 与 Python 版 `portable_config.py` 保持字段级一致：导出时把内置的
//! "常用海外" 分组折叠成 `routing.commonOverseas`，导入时再展开回来，
//! 这样同一份文件可以在不同机器（本地端口不同）之间搬运。

use serde_json::{json, Value};

use crate::models::{
    default_routing_rules, normalize_group, AppConfig, ImportedNode, RoutingRule,
    SubscriptionSource, COMMON_OVERSEAS_GROUP, DEFAULT_PROXY_DOMAINS,
};

pub const PORTABLE_FORMAT: &str = "network-manager-config";
pub const PORTABLE_VERSION: i64 = 1;

const RULE_TYPES: [&str; 4] = ["DOMAIN", "DOMAIN-SUFFIX", "DOMAIN-KEYWORD", "IP-CIDR"];
const MODES: [&str; 5] = [
    "RULE",
    "GLOBAL_CLASH",
    "GLOBAL_V2RAY",
    "GLOBAL_SSH",
    "GLOBAL_BUILTIN",
];
const TARGETS: [&str; 5] = ["CLASH", "V2RAY", "SSH", "BUILTIN", "DIRECT"];
const MIN_RANDOM_SERVER_PROXY_PORT: i32 = 10000;

fn portable_rule_types() -> [(&'static str, &'static str); 4] {
    [
        ("domain", "DOMAIN"),
        ("domain_suffix", "DOMAIN-SUFFIX"),
        ("domain_keyword", "DOMAIN-KEYWORD"),
        ("ip_cidr", "IP-CIDR"),
    ]
}

pub fn is_common_overseas_rule(rule: &RoutingRule) -> bool {
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

fn portable_target(target: &str) -> &'static str {
    if target.eq_ignore_ascii_case("DIRECT") {
        "direct"
    } else {
        "proxy"
    }
}

fn windows_target(target: Option<&Value>, has_nodes: bool) -> String {
    let raw = target
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_lowercase();
    if raw == "proxy" && has_nodes {
        "BUILTIN".into()
    } else {
        "DIRECT".into()
    }
}

fn str_field(item: &Value, key: &str) -> String {
    match item.get(key) {
        Some(value) => as_text(value),
        None => String::new(),
    }
}

/// 把一个标量 JSON 值读成字符串（数组项本身就是标量时使用）。
fn as_text(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        _ => String::new(),
    }
}

fn bool_field(item: &Value, key: &str, default: bool) -> bool {
    match item.get(key) {
        Some(Value::Bool(flag)) => *flag,
        Some(Value::Number(number)) => number.as_f64().map(|v| v != 0.0).unwrap_or(default),
        Some(Value::String(text)) => matches!(text.as_str(), "1" | "true" | "yes"),
        _ => default,
    }
}

/// 校验单条规则，返回错误列表（空表示通过）。
pub fn validate_rule(rule: &RoutingRule) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    if !RULE_TYPES.contains(&rule.rule_type.as_str()) && rule.rule_type != "PROCESS-NAME" {
        errors.push("不支持的规则类型".into());
    }
    if !TARGETS.contains(&rule.target.as_str()) {
        errors.push("不支持的代理目标".into());
    }
    let value = crate::mihomo::normalize_rule_value(&rule.rule_type, &rule.value);
    if value.is_empty() {
        errors.push("匹配内容不能为空".into());
        return errors;
    }
    if value.contains(',') || value.contains('\n') || value.contains('\r') {
        errors.push("匹配内容不能包含逗号或换行".into());
    }
    if matches!(rule.rule_type.as_str(), "DOMAIN" | "DOMAIN-SUFFIX")
        && (value.contains(' ') || !value.contains('.'))
    {
        errors.push("请输入有效域名，例如 example.com".into());
    }
    if rule.rule_type == "PROCESS-NAME"
        && value
            .chars()
            .any(|ch| matches!(ch, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'))
    {
        errors.push("请输入程序文件名，例如 Discord.exe".into());
    }
    if rule.rule_type == "IP-CIDR" && parse_cidr(&value).is_none() {
        errors.push("请输入有效 IP 或 CIDR，例如 1.2.3.0/24".into());
    }
    errors
}

fn parse_cidr(value: &str) -> Option<()> {
    let (addr, prefix) = match value.split_once('/') {
        Some((addr, prefix)) => (addr, Some(prefix)),
        None => (value, None),
    };
    let octets: Vec<&str> = addr.split('.').collect();
    if octets.len() != 4 {
        return None;
    }
    for octet in &octets {
        if octet.is_empty() || octet.len() > 3 {
            return None;
        }
        let parsed: u32 = octet.parse().ok()?;
        if parsed > 255 {
            return None;
        }
    }
    if let Some(prefix) = prefix {
        let bits: u32 = prefix.parse().ok()?;
        if bits > 32 {
            return None;
        }
    }
    Some(())
}

/// 整体配置校验，与 Python `validate_config` 对齐。
pub fn validate_config(config: &AppConfig) -> Vec<String> {
    let mut errors: Vec<String> = Vec::new();
    if !MODES.contains(&config.mode.as_str()) && config.mode != "SMART" && config.mode != "DIRECT" {
        errors.push("运行模式无效".into());
    }
    if !TARGETS.contains(&config.default_target.as_str()) {
        errors.push("默认目标无效".into());
    }
    let ports = (config.mixed_port, config.controller_port, config.dns_port);
    if ports.0 < 1 || ports.0 > 65535 || ports.1 < 1 || ports.1 > 65535 || ports.2 < 1 || ports.2 > 65535 {
        errors.push("本地端口必须在 1 到 65535 之间".into());
    }
    if ports.0 == ports.1 || ports.1 == ports.2 || ports.0 == ports.2 {
        errors.push("入口、控制器和 DNS 端口不能相同".into());
    }
    if config.server_proxy_port < MIN_RANDOM_SERVER_PROXY_PORT || config.server_proxy_port > 65535 {
        errors.push(format!(
            "默认服务器部署端口必须在 {MIN_RANDOM_SERVER_PROXY_PORT} 到 65535 之间"
        ));
    }
    for upstream in [&config.clash, &config.v2ray] {
        if upstream.host.trim().is_empty() {
            errors.push(format!("{} 地址不能为空", upstream.name));
        }
        if upstream.port < 1 || upstream.port > 65535 {
            errors.push(format!("{} 端口无效", upstream.name));
        }
        if upstream.protocol != "socks5" && upstream.protocol != "http" {
            errors.push(format!("{} 仅支持 SOCKS5 或 HTTP", upstream.name));
        }
    }
    let mut active_targets: Vec<String> = vec![config.default_target.clone()];
    match config.mode.as_str() {
        "GLOBAL_CLASH" => active_targets.push("CLASH".into()),
        "GLOBAL_V2RAY" => active_targets.push("V2RAY".into()),
        "GLOBAL_SSH" => active_targets.push("SSH".into()),
        "GLOBAL_BUILTIN" | "SMART" => active_targets.push("BUILTIN".into()),
        _ => {}
    }
    for rule in &config.rules {
        if rule.enabled {
            active_targets.push(rule.target.clone());
        }
    }
    active_targets.sort();
    active_targets.dedup();
    for target in active_targets {
        let enabled = match target.as_str() {
            "CLASH" => config.clash.enabled,
            "V2RAY" => config.v2ray.enabled,
            "SSH" => config
                .ssh_servers
                .iter()
                .any(|profile| profile.profile_id == config.selected_ssh_server),
            "BUILTIN" => !config.imported_nodes.is_empty(),
            _ => true,
        };
        if !enabled {
            errors.push(format!("规则使用了已禁用的 {target} 代理源"));
        }
    }
    for (index, rule) in config.rules.iter().enumerate() {
        for message in validate_rule(rule) {
            errors.push(format!("第 {} 条规则：{message}", index + 1));
        }
    }
    errors
}

pub fn common_overseas_rules(
    values: &Value,
    target: &str,
    enabled: bool,
) -> Result<Vec<RoutingRule>, String> {
    let Some(list) = values.as_array() else {
        return Err("commonOverseas.domains 必须是数组".into());
    };
    if list.is_empty() || list.len() > 500 {
        return Err("每次必须填写 1 到 500 条匹配内容".into());
    }
    let mut rules: Vec<RoutingRule> = Vec::new();
    let mut seen: Vec<String> = Vec::new();
    for (index, raw) in list.iter().enumerate() {
        let value = crate::mihomo::normalize_rule_value("DOMAIN-SUFFIX", &as_text(raw));
        let key = value.to_lowercase();
        if seen.contains(&key) {
            continue;
        }
        let rule = RoutingRule {
            rule_type: "DOMAIN-SUFFIX".into(),
            value: value.clone(),
            target: target.to_string(),
            enabled,
            note: DEFAULT_PROXY_DOMAINS
                .iter()
                .find(|(domain, _)| *domain == value)
                .map(|(_, label)| (*label).to_string())
                .unwrap_or_else(|| "自定义".into()),
            group: COMMON_OVERSEAS_GROUP.into(),
        };
        let errors = validate_rule(&rule);
        if !errors.is_empty() {
            return Err(format!("第 {} 行：{}", index + 1, errors.join("；")));
        }
        seen.push(key);
        rules.push(rule);
    }
    if rules.is_empty() {
        return Err("匹配内容不能为空".into());
    }
    Ok(rules)
}

pub fn export_portable_config(config: &AppConfig) -> Value {
    let common_rules: Vec<&RoutingRule> = config
        .rules
        .iter()
        .filter(|rule| is_common_overseas_rule(rule))
        .collect();
    let mut custom_rules: Vec<Value> = Vec::new();
    for rule in &config.rules {
        if rule.rule_type == "PROCESS-NAME" || is_common_overseas_rule(rule) {
            continue;
        }
        let Some(portable_type) = portable_rule_types()
            .iter()
            .find(|(_, internal)| *internal == rule.rule_type)
            .map(|(portable, _)| *portable)
        else {
            continue;
        };
        custom_rules.push(json!({
            "type": portable_type,
            "value": crate::mihomo::normalize_rule_value(&rule.rule_type, &rule.value),
            "target": portable_target(&rule.target),
            "enabled": rule.enabled,
            "note": rule.note,
        }));
    }
    let mode = match config.mode.as_str() {
        "GLOBAL_BUILTIN" => "global",
        "SMART" => "smart",
        "DIRECT" => "direct",
        _ => "rule",
    };
    let selected_id = config
        .imported_nodes
        .iter()
        .find(|node| node.name() == config.selected_node)
        .map(|node| node.node_id.clone())
        .unwrap_or_default();
    json!({
        "format": PORTABLE_FORMAT,
        "version": PORTABLE_VERSION,
        "exportedAt": chrono::Utc::now().to_rfc3339(),
        "routing": {
            "mode": mode,
            "fallback": portable_target(&config.default_target),
            "commonOverseas": {
                "enabled": !common_rules.is_empty() && common_rules.iter().all(|rule| rule.enabled),
                "target": portable_target(common_rules.first().map(|rule| rule.target.as_str()).unwrap_or("BUILTIN")),
                "domains": common_rules
                    .iter()
                    .map(|rule| crate::mihomo::normalize_rule_value(&rule.rule_type, &rule.value))
                    .collect::<Vec<String>>(),
            },
        },
        "selectedNodeId": selected_id,
        "nodeGroups": config.node_groups,
        "nodes": config.imported_nodes.iter().map(|node| json!({
            "id": node.node_id,
            "sourceId": node.source_id,
            "sourceName": node.source,
            "group": node.group,
            "config": node.config,
        })).collect::<Vec<Value>>(),
        "subscriptions": config.subscriptions.iter().map(|source| json!({
            "id": source.source_id,
            "name": source.name,
            "url": source.url,
            "updatedAt": source.last_updated,
            "group": source.group,
        })).collect::<Vec<Value>>(),
        "rules": custom_rules,
    })
}

fn object_list(payload: &Value, name: &str, limit: usize) -> Result<Vec<Value>, String> {
    let Some(list) = payload.get(name) else {
        return Ok(Vec::new());
    };
    let Some(items) = list.as_array() else {
        return Err(format!("{name} 必须是对象数组"));
    };
    if !items.iter().all(|item| item.is_object()) {
        return Err(format!("{name} 必须是对象数组"));
    }
    if items.len() > limit {
        return Err(format!("{name} 数量超过限制"));
    }
    Ok(items.clone())
}

fn validate_root(payload: &Value) -> Result<(), String> {
    if !payload.is_object() {
        return Err("配置文件根节点必须是对象".into());
    }
    if payload.get("format").and_then(|v| v.as_str()) != Some(PORTABLE_FORMAT) {
        return Err("不是 Network Manager 跨设备配置".into());
    }
    if payload.get("version").and_then(|v| v.as_i64()) != Some(PORTABLE_VERSION) {
        return Err("暂不支持这个配置版本".into());
    }
    Ok(())
}

pub fn import_portable_config(current: &AppConfig, payload: &Value) -> Result<AppConfig, String> {
    validate_root(payload)?;
    let mut imported = current.clone();
    let nodes_data = object_list(payload, "nodes", 5_000)?;
    let subscriptions_data = object_list(payload, "subscriptions", 500)?;
    let rules_data = object_list(payload, "rules", 5_000)?;

    let mut node_groups: Vec<String> = Vec::new();
    if let Some(raw_groups) = payload.get("nodeGroups") {
        let Some(list) = raw_groups.as_array() else {
            return Err("nodeGroups 必须是数组".into());
        };
        for value in list {
            let group = normalize_group(&as_text(value));
            if !group.is_empty() && !node_groups.contains(&group) {
                node_groups.push(group);
            }
        }
    }

    let mut nodes: Vec<ImportedNode> = Vec::new();
    let mut used_names: Vec<String> = Vec::new();
    for (index, item) in nodes_data.iter().enumerate() {
        let Some(raw_config) = item.get("config") else {
            return Err(format!("第 {} 个节点缺少 config", index + 1));
        };
        if !raw_config.is_object() {
            return Err(format!("第 {} 个节点缺少 config", index + 1));
        }
        let mut config = raw_config.clone();
        let base_name = config
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let base_name = if base_name.is_empty() {
            format!("导入节点 {}", index + 1)
        } else {
            base_name
        };
        let mut name = base_name.clone();
        let mut suffix = 2;
        while used_names.contains(&name) {
            name = format!("{base_name} ({suffix})");
            suffix += 1;
        }
        used_names.push(name.clone());
        if let Value::Object(map) = &mut config {
            map.insert("name".into(), Value::String(name));
        }
        let group = normalize_group(&str_field(item, "group"));
        if !group.is_empty() && !node_groups.contains(&group) {
            node_groups.push(group.clone());
        }
        nodes.push(ImportedNode {
            node_id: nonempty(str_field(item, "id"), crate::models::random_hex(8)),
            source: nonempty(str_field(item, "sourceName"), "跨设备导入".into()),
            source_id: str_field(item, "sourceId"),
            config,
            group,
        });
    }

    imported.imported_nodes = nodes;
    imported.node_groups = node_groups;
    imported.subscriptions = subscriptions_data
        .iter()
        .filter(|item| !str_field(item, "url").trim().is_empty())
        .map(|item| SubscriptionSource {
            source_id: nonempty(str_field(item, "id"), crate::models::random_hex(8)),
            name: nonempty(str_field(item, "name"), "订阅".into()),
            url: str_field(item, "url").trim().to_string(),
            last_updated: str_field(item, "updatedAt"),
            group: normalize_group(&str_field(item, "group")),
        })
        .collect();
    for source in &imported.subscriptions {
        if !source.group.is_empty() && !imported.node_groups.contains(&source.group) {
            imported.node_groups.push(source.group.clone());
        }
    }

    let selected_id = str_field(payload, "selectedNodeId");
    imported.selected_node = imported
        .imported_nodes
        .iter()
        .find(|node| node.node_id == selected_id)
        .map(|node| node.name())
        .unwrap_or_else(|| {
            imported
                .imported_nodes
                .first()
                .map(|node| node.name())
                .unwrap_or_default()
        });

    let routing = payload.get("routing").cloned().unwrap_or_else(|| json!({}));
    if !routing.is_object() {
        return Err("routing 必须是对象".into());
    }
    let has_nodes = !imported.imported_nodes.is_empty();
    let mode = str_field(&routing, "mode").to_lowercase();
    imported.mode = match mode.as_str() {
        "global" => {
            if has_nodes {
                "GLOBAL_BUILTIN".into()
            } else {
                "RULE".into()
            }
        }
        "smart" => {
            if has_nodes {
                "SMART".into()
            } else {
                "RULE".into()
            }
        }
        "direct" => "DIRECT".into(),
        _ => "RULE".into(),
    };
    imported.default_target = windows_target(routing.get("fallback"), has_nodes);

    let process_rules: Vec<RoutingRule> = imported
        .rules
        .iter()
        .filter(|rule| rule.rule_type == "PROCESS-NAME")
        .cloned()
        .collect();
    let common_settings = routing.get("commonOverseas").cloned().unwrap_or_else(|| json!({}));
    if !common_settings.is_object() {
        return Err("commonOverseas 必须是对象".into());
    }
    let common_enabled = bool_field(&common_settings, "enabled", true);
    let common_target = windows_target(common_settings.get("target"), has_nodes);
    let common_domains = match common_settings.get("domains") {
        Some(value) => value.clone(),
        None => json!(default_routing_rules()
            .iter()
            .filter(|rule| rule.rule_type == "DOMAIN-SUFFIX")
            .map(|rule| rule.value.clone())
            .collect::<Vec<String>>()),
    };
    let common_rules = common_overseas_rules(&common_domains, &common_target, common_enabled)?;

    let mut portable_rules: Vec<RoutingRule> = Vec::new();
    for item in &rules_data {
        let raw_type = str_field(item, "type").to_lowercase();
        let Some(rule_type) = portable_rule_types()
            .iter()
            .find(|(portable, _)| *portable == raw_type)
            .map(|(_, internal)| *internal)
        else {
            continue;
        };
        let rule = RoutingRule {
            rule_type: rule_type.to_string(),
            value: crate::mihomo::normalize_rule_value(rule_type, &str_field(item, "value")),
            target: windows_target(item.get("target"), has_nodes),
            enabled: bool_field(item, "enabled", true),
            note: str_field(item, "note").trim().to_string(),
            group: String::new(),
        };
        let errors = validate_rule(&rule);
        if !errors.is_empty() {
            return Err(errors.join("；"));
        }
        portable_rules.push(rule);
    }
    imported.rules = [process_rules, common_rules, portable_rules].concat();

    crate::deploy::apply_automatic_node_dialers(&mut imported.imported_nodes);
    let errors = validate_config(&imported);
    if let Some(first) = errors.first() {
        return Err(first.clone());
    }
    Ok(imported)
}

fn nonempty(value: String, fallback: String) -> String {
    if value.trim().is_empty() {
        fallback
    } else {
        value.trim().to_string()
    }
}
