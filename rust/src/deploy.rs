//! Long-running SSH deployment flow.
//!
//! Runs outside the state lock so the WebGUI keeps polling while a server is
//! being provisioned; progress is surfaced through the toast queue.

use serde_json::{json, Value};

use crate::core::DeployTask;
use crate::models::{ImportedNode, NODE_DIALER_POLICY_KEY, NODE_DIALER_PROXY_KEY};
use crate::server::Shared;
use crate::ssh;

/// 记下这次部署/检查的状态，界面据此显示「检查中／部署中」。
async fn set_task(shared: &Shared, profile_id: &str, status: &str, stage: &str) {
    let mut state = shared.lock().await;
    state.deployments.insert(
        profile_id.to_string(),
        DeployTask {
            status: status.to_string(),
            stage: stage.to_string(),
            error: String::new(),
        },
    );
}

fn text(args: &[Value], index: usize) -> String {
    args.get(index)
        .and_then(|value| value.as_str())
        .unwrap_or("")
        .to_string()
}

fn flag(args: &[Value], index: usize) -> bool {
    args.get(index)
        .and_then(|value| value.as_bool())
        .unwrap_or(false)
}

fn random_hex(len: usize) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdef";
    use rand::Rng;
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

struct Outcome {
    node_config: Value,
    version: String,
    deployed_at: String,
    #[allow(dead_code)]
    reused: bool,
    public_reachable: Option<bool>,
    public_error: String,
    credential: String,
    message: String,
}

pub async fn run(shared: Shared, args: Vec<Value>) -> Result<Value, String> {
    let profile_id = text(&args, 0);
    let mut password = text(&args, 1);
    let remember = flag(&args, 2);
    let force = flag(&args, 3);

    // Snapshot what we need, then release the lock.
    let (profile, existing_node, default_port) = {
        let mut state = shared.lock().await;
        let profile = state
            .config
            .ssh_servers
            .iter()
            .find(|item| item.profile_id == profile_id)
            .cloned()
            .ok_or_else(|| "SSH 服务器不存在".to_string())?;
        if profile.auth_method != "agent" && password.is_empty() {
            password = crate::credentials::get(&profile_id)
                .map_err(|_| "未找到 SSH 密码，请编辑服务器后重新保存凭据".to_string())?;
        }
        let source_id = ssh::deployment_source_id(&profile_id);
        let existing = state
            .config
            .imported_nodes
            .iter()
            .find(|node| node.source_id == source_id)
            .cloned();
        state.config.selected_ssh_server = profile_id.clone();
        let default_port = state.config.server_proxy_port;
        let _ = state.apply_config();
        (profile, existing, default_port)
    };

    let mut profile = profile;
    let mut repair_from_port = 0;
    if !profile.deployed_node_id.is_empty() && profile.proxy_port != default_port {
        repair_from_port = profile.proxy_port;
        profile.proxy_port = default_port;
    }
    let port_error = crate::models::server_proxy_port_error(profile.proxy_port, profile.port);
    if !port_error.is_empty() {
        return Err(port_error);
    }
    if profile.proxy_port == profile.port {
        return Err(format!(
            "默认部署端口 {} 与 SSH 端口冲突，请先在设置中更换",
            profile.proxy_port
        ));
    }

    // 从这里开始才有远端动作，先把状态挂出去，让界面立刻切到「检查中／部署中」。
    // 放在端口校验之后：那些早退路径不该留下一个永远转圈的状态。
    set_task(
        &shared,
        &profile_id,
        "deploying",
        if force {
            "正在重新部署并轮换节点密码"
        } else if existing_node.is_some() {
            "正在检查远端代理服务"
        } else {
            "正在查找远端现有代理服务"
        },
    )
    .await;

    let mut progress: Vec<String> = Vec::new();
    let mut reporter = |stage: String| {
        progress.push(stage);
    };

    let existing_config = existing_node.as_ref().map(|node| node.config.clone());

    let result = if force {
        reporter("正在重新部署并轮换节点密码".into());
        ssh::deploy(&profile, &password, &mut reporter).await
    } else {
        if existing_config.is_some() {
            reporter("正在检查远端代理服务".into());
        } else {
            reporter("正在查找远端现有代理服务".into());
        }
        match ssh::inspect(&profile, &password).await {
            Ok(inspection) => {
                let status = inspection
                    .get("status")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if status == "active" {
                    let mut node = inspection.get("nodeConfig").cloned();
                    if node.is_none() && node_matches(&existing_config, &profile) {
                        node = existing_config.clone();
                    }
                    if let Some(node) = node {
                        Ok(reused_result(&profile, &inspection, node))
                    } else {
                        reporter("远端服务配置不匹配，正在修复部署".into());
                        ssh::deploy(&profile, &password, &mut reporter).await
                    }
                } else {
                    reporter("远端服务未运行，正在修复部署".into());
                    ssh::deploy(&profile, &password, &mut reporter).await
                }
            }
            Err(err) => Err(err),
        }
    };

    let outcome = match result {
        Ok(mut deployed) => {
            reporter("正在验证公网代理端口".into());
            let (reachable, error) = ssh::probe_public(&profile).await;
            deployed.public_reachable = Some(reachable);
            deployed.public_error = error;
            let mut message = if repair_from_port > 0 {
                format!(
                    "旧端口 {} 已自动调整为 {}",
                    repair_from_port,
                    deployed
                        .node_config
                        .get("port")
                        .and_then(|v| v.as_i64())
                        .unwrap_or(0)
                )
            } else if deployed.reused {
                "远端代理已存在并正常运行，无需重复部署".to_string()
            } else {
                "服务器代理部署完成".to_string()
            };
            if force && !deployed.reused {
                message = format!("{message}，节点密码已轮换");
            }
            Outcome {
                node_config: deployed.node_config,
                version: deployed.version,
                deployed_at: deployed.deployed_at,
                reused: deployed.reused,
                public_reachable: deployed.public_reachable,
                public_error: deployed.public_error,
                credential: password.clone(),
                message,
            }
        }
        Err(err) => {
            let mut guard = shared.lock().await;
            let mut message = err.clone();
            if password.is_empty() && message.contains("认证失败") {
                message = format!(
                    "{message}；本次使用的是已保存的旧密码，若服务器密码已变更，请在弹窗中输入新密码并勾选覆盖"
                );
            }
            guard.deployments.insert(
                profile_id.clone(),
                DeployTask {
                    status: "error".into(),
                    stage: String::new(),
                    error: message.clone(),
                },
            );
            guard.notify("error", message.clone());
            return Err(message);
        }
    };

    let mut state = shared.lock().await;
    let Some(snapshot) = state
        .config
        .ssh_servers
        .iter()
        .find(|item| item.profile_id == profile_id)
        .cloned()
    else {
        state.deployments.remove(&profile_id);
        return Err("SSH 服务器不存在".into());
    };

    let deployed_port = outcome
        .node_config
        .get("port")
        .and_then(|v| v.as_i64())
        .unwrap_or(0) as i32;
    let ssh_port = snapshot.port;
    let target_name = snapshot.name.clone();
    let port_error = crate::models::server_proxy_port_error(deployed_port, ssh_port);
    if !port_error.is_empty() {
        state.deployments.insert(
            profile_id.clone(),
            DeployTask {
                status: "error".into(),
                stage: String::new(),
                error: port_error.clone(),
            },
        );
        state.notify("error", port_error.clone());
        return Err(port_error);
    }

    let source_id = ssh::deployment_source_id(&profile_id);
    let current = state
        .config
        .imported_nodes
        .iter()
        .find(|node| node.source_id == source_id)
        .cloned();
    let other_names: std::collections::HashSet<String> = state
        .config
        .imported_nodes
        .iter()
        .filter(|node| node.source_id != source_id)
        .map(|node| node.name())
        .collect();

    let mut node_config = outcome.node_config.clone();
    let base_name = node_config
        .get("name")
        .and_then(|v| v.as_str())
        .map(|value| value.to_string())
        .unwrap_or_else(|| target_name.clone());
    let mut node_name = base_name.clone();
    let mut suffix = 2;
    while other_names.contains(&node_name) {
        node_name = format!("{base_name} ({suffix})");
        suffix += 1;
    }
    node_config["name"] = json!(node_name);
    if let Some(current) = current.as_ref() {
        let dialer = current.dialer_proxy();
        if !dialer.is_empty() {
            node_config[NODE_DIALER_PROXY_KEY] = json!(dialer);
        }
        let policy = current.dialer_policy();
        if !policy.is_empty() {
            node_config[NODE_DIALER_POLICY_KEY] = json!(policy);
        }
    }

    let node = ImportedNode {
        node_id: current
            .as_ref()
            .map(|item| item.node_id.clone())
            .unwrap_or_else(|| random_hex(16)),
        source: format!("服务器部署 · {target_name}"),
        config: node_config,
        source_id: source_id.clone(),
        group: current.as_ref().map(|item| item.group.clone()).unwrap_or_default(),
    };
    let node_id = node.node_id.clone();
    state
        .config
        .imported_nodes
        .retain(|item| item.source_id != source_id);
    state.config.imported_nodes.push(node);
    state.config.selected_node = node_name.clone();
    state.config.selected_ssh_server = profile_id.clone();

    let mut message = outcome.message;
    let used_stored = text(&args, 1).is_empty();

    // Finalise the server record in its own scope to end the mutable borrow.
    {
        let Some(target) = state
            .config
            .ssh_servers
            .iter_mut()
            .find(|item| item.profile_id == profile_id)
        else {
            return Err("SSH 服务器不存在".into());
        };
        target.deployed_node_id = node_id;
        target.proxy_port = deployed_port;
        target.deployed_at = outcome.deployed_at.clone();
        target.deployed_version = outcome.version.clone();
        target.proxy_reachable = outcome.public_reachable;
        target.proxy_reachability_error = outcome.public_error.clone();

        if !used_stored || remember {
            let existing_credential = crate::credentials::get(&profile_id).unwrap_or_default();
            if outcome.credential != existing_credential {
                let replaced = target.remember_password;
                match crate::credentials::set(&profile_id, &outcome.credential) {
                    Ok(()) => {
                        target.remember_password = true;
                        message += if replaced {
                            "，SSH 凭据已覆盖更新"
                        } else {
                            "，SSH 凭据已安全保存"
                        };
                    }
                    Err(err) => message = format!("{message}；凭据保存失败：{err}"),
                }
            } else if !target.remember_password {
                target.remember_password = true;
            }
        }
    }

    // 干完了：清掉进度状态，界面回到「已部署」。
    state.deployments.remove(&profile_id);
    apply_automatic_node_dialers(&mut state.config.imported_nodes);
    let _ = state.apply_config();
    let running = state.core.is_running();
    if running {
        let config = state.config.clone();
        if let Err(err) = state.core.write_config(&config) {
            state.notify("error", err);
        }
    }
    state.notify("success", message);
    Ok(json!(true))
}

fn node_matches(node_config: &Option<Value>, profile: &crate::models::SshServerProfile) -> bool {
    let Some(config) = node_config else {
        return false;
    };
    let port = config.get("port").and_then(|v| v.as_i64()).unwrap_or(0);
    config
        .get("server")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        == profile.host
        && port == profile.proxy_port as i64
}

fn reused_result(
    profile: &crate::models::SshServerProfile,
    inspection: &Value,
    node: Value,
) -> ssh::DeployResult {
    ssh::DeployResult {
        share_link: ssh::shadowsocks_share_link(&node),
        node_config: node,
        version: inspection
            .get("version")
            .and_then(|v| v.as_str())
            .filter(|value| !value.trim().is_empty())
            .map(|value| value.to_string())
            .unwrap_or_else(|| profile.deployed_version.clone()),
        deployed_at: profile.deployed_at.clone(),
        firewall: "unchanged".into(),
        reused: true,
        public_reachable: None,
        public_error: String::new(),
    }
}

/// Bind authenticated HTTP proxies to the preferred deployed relay.
pub fn apply_automatic_node_dialers(nodes: &mut [ImportedNode]) -> bool {
    let relay_name = nodes
        .iter()
        .enumerate()
        .filter(|(_, node)| node.source_id.starts_with("server-deployment:"))
        .min_by_key(|(index, node)| relay_preference(node, *index))
        .map(|(_, node)| node.name());
    let relay_name = relay_name.unwrap_or_default();
    let mut changed = false;
    for node in nodes.iter_mut() {
        let eligible = node.protocol().to_lowercase() == "http"
            && !node
                .config
                .get("username")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .is_empty()
            && !node
                .config
                .get("password")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .is_empty()
            && !node.source_id.starts_with("server-deployment:");
        if !eligible || matches!(node.dialer_policy().as_str(), "manual" | "direct") {
            continue;
        }
        if !node.dialer_proxy().is_empty() && node.dialer_policy() != "auto" {
            continue;
        }
        if !relay_name.is_empty() {
            if node.dialer_proxy() != relay_name || node.dialer_policy() != "auto" {
                node.config[NODE_DIALER_PROXY_KEY] = json!(relay_name);
                node.config[NODE_DIALER_POLICY_KEY] = json!("auto");
                changed = true;
            }
        } else if node.dialer_policy() == "auto" {
            node.config
                .as_object_mut()
                .map(|map| map.remove(NODE_DIALER_PROXY_KEY));
            node.config
                .as_object_mut()
                .map(|map| map.remove(NODE_DIALER_POLICY_KEY));
            changed = true;
        }
    }
    changed
}

fn relay_preference(node: &ImportedNode, index: usize) -> (usize, usize) {
    let label = format!("{} {} {}", node.name(), node.source, node.group).to_lowercase();
    let priorities: &[&[&str]] = &[
        &["香港", "hong kong", "hongkong", " hk "],
        &["海外", "overseas"],
        &["新加坡", "singapore", "日本", "japan", "东京", "tokyo"],
        &["美国", "usa", "united states"],
    ];
    for (priority, keywords) in priorities.iter().enumerate() {
        if keywords
            .iter()
            .any(|keyword| format!(" {label} ").contains(keyword))
        {
            return (priority, index);
        }
    }
    (priorities.len(), index)
}

