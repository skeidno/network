use serde_json::Value;
use std::path::Path;

use crate::models::{
    AppConfig, RoutingRule, COMMON_CHINA_GROUP, COMMON_OVERSEAS_GROUP, DEFAULT_DIRECT_DOMAINS,
    DEFAULT_PROXY_DOMAINS, PROXY_DOMAINS_V11,
};
use crate::paths::settings_path;

pub fn load() -> AppConfig {
    let path = settings_path();
    if !path.exists() {
        let config = AppConfig::default();
        save(&config);
        return config;
    }
    match std::fs::read_to_string(&path).and_then(|raw| {
        serde_json::from_str::<Value>(&raw)
            .map_err(|err| std::io::Error::new(std::io::ErrorKind::InvalidData, err))
    }) {
        Ok(value) => {
            let stored_version = value.get("version").and_then(|v| v.as_i64()).unwrap_or(0);
            match serde_json::from_value::<AppConfig>(value) {
                Ok(mut config) => {
                    config.version = crate::models::CONFIG_VERSION;
                    config.mode = config.mode.to_uppercase();
                    config.default_target = config.default_target.to_uppercase();
                    // 映射列表是外部文本存下来的（settings.json 允许手改），读回来时
                    // 不该信任：非法条目、重复域名、超量的一次性清掉。
                    let raw_entries = std::mem::take(&mut config.local_dns);
                    let entries = crate::models::sanitize_local_dns_entries(raw_entries.clone());
                    let entries_changed = entries != raw_entries;
                    config.local_dns = entries;
                    // 半成品规则（缺匹配内容）渲染出来是 `DOMAIN-SUFFIX,,X`，mihomo 会
                    // 直接 fatal 退出，整个接管起不来。这里一次性清掉；历史上「添加规则」
                    // 写坏的空规则也靠这一步自愈，不用用户自己去列表里翻。
                    let raw_rules = std::mem::take(&mut config.rules);
                    let rules_before = raw_rules.len();
                    let rules: Vec<RoutingRule> = raw_rules
                        .into_iter()
                        .filter(RoutingRule::is_complete)
                        .collect();
                    let rules_changed = rules.len() != rules_before;
                    config.rules = rules;
                    // 迁移只在内存里改的话，每次启动都要重算一遍，配置文件里的
                    // 旧值也永远在那里；改了就落盘，让用户看到真实生效的值。
                    if migrate(&mut config, stored_version) || entries_changed || rules_changed {
                        save(&config);
                    }
                    config
                }
                Err(_) => {
                    quarantine(&path);
                    let config = AppConfig::default();
                    save(&config);
                    config
                }
            }
        }
        Err(_) => {
            quarantine(&path);
            let config = AppConfig::default();
            save(&config);
            config
        }
    }
}

/// 老配置文件里 `close_to_tray` 的一次性纠正。
///
/// v10 时把它统一改成了 false（关闭即退出），理由是「托盘图标被 Windows 11 收进
/// 溢出区、点不到」。那个结论是错的：托盘收不到消息的真实原因是通知走 SendMessage
/// 不进消息队列、且通知码在 lParam 低位字 —— 那两个 bug 已经修好了（见
/// [`crate::tray`]）。托盘现在能点能唤回，默认值也就回到托盘类程序该有的行为：
/// 关闭只收起界面，程序继续在后台跑。存量配置一并纠正，否则升级后行为没变化。
/// 返回是否真的改动了（决定是否要重新落盘）。
fn migrate(config: &mut AppConfig, stored_version: i64) -> bool {
    let mut changed = false;
    if stored_version < 11 {
        let before = config.close_to_tray;
        config.close_to_tray = AppConfig::default().close_to_tray;
        changed = config.close_to_tray != before;
    }
    if stored_version < 12 {
        changed = append_new_domains(config) || changed;
    }
    if stored_version < 14 {
        changed = append_china_group(config) || changed;
    }
    changed
}

/// 给老配置补一组「常用国内站点」直连规则。
///
/// 只在版本升级时跑一次，所以用户之后把整组删掉不会被塞回来。
/// 已经存在的同名后缀一律跳过：用户自己配过的规则优先，不制造互相打架的重复项。
fn append_china_group(config: &mut AppConfig) -> bool {
    if config
        .rules
        .iter()
        .any(|rule| rule.group == COMMON_CHINA_GROUP)
    {
        return false;
    }
    let present: Vec<String> = config
        .rules
        .iter()
        .map(|rule| crate::mihomo::normalize_rule_value(&rule.rule_type, &rule.value))
        .collect();
    let added: Vec<RoutingRule> = DEFAULT_DIRECT_DOMAINS
        .iter()
        .filter(|(domain, _)| !present.iter().any(|value| value == *domain))
        .map(|(domain, label)| RoutingRule {
            rule_type: "DOMAIN-SUFFIX".into(),
            value: (*domain).into(),
            target: "DIRECT".into(),
            enabled: true,
            note: (*label).into(),
            group: COMMON_CHINA_GROUP.into(),
        })
        .collect();
    if added.is_empty() {
        return false;
    }
    // 插到海外组之前，让规则列表里「国内直连 → 海外代理」的次序一眼看得出来。
    let position = config
        .rules
        .iter()
        .position(|rule| crate::portable::is_common_overseas_rule(rule))
        .unwrap_or(config.rules.len());
    let tail = config.rules.split_off(position);
    config.rules.extend(added);
    config.rules.extend(tail);
    true
}

/// 把新版本追加进去的内置域名补到已有配置里。
///
/// 只补用户「还没见过」的：差集用 [`PROXY_DOMAINS_V11`] 算，用户自己删掉过的
/// 旧域名不会被塞回来。整组都被删掉时同样不重建 —— 那多半是主动的选择。
/// 新条目沿用组内现有成员的去向和开关状态，不会出现「补齐后整组变成半启用」。
fn append_new_domains(config: &mut AppConfig) -> bool {
    let mut insert_after: Option<usize> = None;
    let mut sample: Option<(String, bool)> = None;
    let mut present: Vec<String> = Vec::new();
    for (index, rule) in config.rules.iter().enumerate() {
        present.push(crate::mihomo::normalize_rule_value(
            &rule.rule_type,
            &rule.value,
        ));
        let in_group = rule.group == COMMON_OVERSEAS_GROUP
            || (rule.rule_type == "DOMAIN-SUFFIX"
                && PROXY_DOMAINS_V11
                    .iter()
                    .any(|domain| *domain == rule.value));
        if in_group {
            if sample.is_none() {
                sample = Some((rule.target.clone(), rule.enabled));
            }
            insert_after = Some(index + 1);
        }
    }
    let Some(position) = insert_after else {
        return false;
    };
    let (target, enabled) = sample.unwrap_or_else(|| ("CLASH".to_string(), true));
    let added: Vec<RoutingRule> = DEFAULT_PROXY_DOMAINS
        .iter()
        .filter(|(domain, _)| {
            !PROXY_DOMAINS_V11.contains(domain) && !present.iter().any(|value| value == *domain)
        })
        .map(|(domain, label)| RoutingRule {
            rule_type: "DOMAIN-SUFFIX".into(),
            value: (*domain).into(),
            target: target.clone(),
            enabled,
            note: (*label).into(),
            group: COMMON_OVERSEAS_GROUP.into(),
        })
        .collect();
    if added.is_empty() {
        return false;
    }
    // 插到组内最后一条之后，让这一组在规则列表里仍然挨在一起。
    let tail = config.rules.split_off(position);
    config.rules.extend(added);
    config.rules.extend(tail);
    true
}

fn quarantine(path: &Path) {
    let mut backup = path.to_path_buf();
    backup.set_extension("invalid.json");
    let _ = std::fs::rename(path, backup);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{default_routing_rules, DEFAULT_PROXY_DOMAINS};

    fn config_from(domains: &[&str]) -> AppConfig {
        let mut config = AppConfig::default();
        config.rules = domains
            .iter()
            .map(|domain| RoutingRule {
                rule_type: "DOMAIN-SUFFIX".into(),
                value: (*domain).into(),
                target: "CLASH".into(),
                enabled: true,
                note: String::new(),
                group: COMMON_OVERSEAS_GROUP.into(),
            })
            .collect();
        config
    }

    #[test]
    fn appends_domains_added_after_v11() {
        let mut config = config_from(PROXY_DOMAINS_V11);
        assert!(append_new_domains(&mut config));
        assert_eq!(config.rules.len(), DEFAULT_PROXY_DOMAINS.len());
        for domain in ["binance.com", "cdn-telegram.org", "telesco.pe"] {
            assert!(
                config.rules.iter().any(|rule| rule.value == domain),
                "补齐后应包含 {domain}"
            );
        }
    }

    #[test]
    fn new_entries_follow_the_group_target_and_switch() {
        let mut config = config_from(&PROXY_DOMAINS_V11[..2]);
        config.rules[0].target = "V2RAY".to_string();
        config.rules[0].enabled = false;
        assert!(append_new_domains(&mut config));
        let added = config.rules.last().expect("应该有补齐的规则");
        assert_eq!(added.target, "V2RAY");
        assert!(!added.enabled);
    }

    #[test]
    fn appends_after_the_group_keeps_custom_rules_last() {
        let mut config = config_from(PROXY_DOMAINS_V11);
        config.rules.push(RoutingRule {
            value: "example.com".into(),
            ..Default::default()
        });
        assert!(append_new_domains(&mut config));
        assert_eq!(
            config.rules.last().map(|rule| rule.value.as_str()),
            Some("example.com")
        );
    }

    #[test]
    fn does_not_restore_domains_the_user_deleted() {
        let mut config = config_from(PROXY_DOMAINS_V11);
        config.rules.retain(|rule| rule.value != "discord.com");
        assert!(append_new_domains(&mut config));
        assert!(!config.rules.iter().any(|rule| rule.value == "discord.com"));
    }

    #[test]
    fn does_not_rebuild_a_group_the_user_removed() {
        let mut config = AppConfig::default();
        config.rules = vec![RoutingRule {
            value: "example.com".into(),
            ..Default::default()
        }];
        assert!(!append_new_domains(&mut config));
        assert_eq!(config.rules.len(), 1);
    }

    #[test]
    fn china_group_defaults_go_direct() {
        let rules = default_routing_rules();
        let china: Vec<&RoutingRule> = rules
            .iter()
            .filter(|rule| rule.group == COMMON_CHINA_GROUP)
            .collect();
        assert_eq!(china.len(), DEFAULT_DIRECT_DOMAINS.len());
        assert!(
            china.iter().all(|rule| rule.target == "DIRECT"),
            "国内站点那一组必须整组直连"
        );
        for domain in ["weixin.qq.com", "taobao.com", "alipay.com", "feishu.cn"] {
            assert!(
                china.iter().any(|rule| domain.ends_with(&rule.value)),
                "清单里应能匹配 {domain}"
            );
        }
    }

    #[test]
    fn china_group_is_seeded_ahead_of_the_overseas_group() {
        let mut config = config_from(&DEFAULT_PROXY_DOMAINS[..3].iter().map(|(d, _)| *d).collect::<Vec<_>>());
        assert!(append_china_group(&mut config));
        let first_china = config
            .rules
            .iter()
            .position(|rule| rule.group == COMMON_CHINA_GROUP)
            .expect("应补出国内组");
        let first_overseas = config
            .rules
            .iter()
            .position(|rule| rule.group == COMMON_OVERSEAS_GROUP)
            .expect("海外组还在");
        assert!(
            first_china < first_overseas,
            "国内直连要先匹配，不能排在海外组后面"
        );
    }

    #[test]
    fn china_group_skips_domains_the_user_already_set() {
        let mut config = AppConfig::default();
        config.rules = vec![RoutingRule {
            rule_type: "DOMAIN-SUFFIX".into(),
            value: "baidu.com".into(),
            target: "CLASH".into(),
            enabled: true,
            note: "我自己配的".into(),
            group: String::new(),
        }];
        assert!(append_china_group(&mut config));
        let matched: Vec<&RoutingRule> = config
            .rules
            .iter()
            .filter(|rule| rule.value == "baidu.com")
            .collect();
        assert_eq!(
            matched.len(),
            1,
            "用户自己配过的域名不能被补一条互相打架的重复规则"
        );
        assert_eq!(matched[0].target, "CLASH");
    }

    #[test]
    fn china_group_is_not_seeded_twice() {
        let mut config = AppConfig::default();
        config.rules = default_routing_rules();
        assert!(!append_china_group(&mut config));
        assert_eq!(config.rules.len(), default_routing_rules().len());
    }

    #[test]
    fn stays_unchanged_when_everything_is_already_there() {
        let mut config = AppConfig::default();
        config.rules = default_routing_rules();
        assert!(!append_new_domains(&mut config));
        assert_eq!(config.rules.len(), default_routing_rules().len());
    }
}

pub fn save(config: &AppConfig) {
    let path = settings_path();
    let temporary = path.with_extension("tmp");
    let payload = serde_json::to_string_pretty(config).unwrap_or_else(|_| "{}".into());
    let _ = std::fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")));
    if std::fs::write(&temporary, format!("{payload}\n")).is_ok() {
        let _ = std::fs::rename(&temporary, &path);
    }
}
