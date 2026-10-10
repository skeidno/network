use serde_json::Value;
use std::path::Path;

use crate::models::{
    AppConfig, RoutingRule, COMMON_OVERSEAS_GROUP, DEFAULT_PROXY_DOMAINS, PROXY_DOMAINS_V11,
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
                    // 迁移只在内存里改的话，每次启动都要重算一遍，配置文件里的
                    // 旧值也永远在那里；改了就落盘，让用户看到真实生效的值。
                    if migrate(&mut config, stored_version) {
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
    changed
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
    use crate::models::default_routing_rules;

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
