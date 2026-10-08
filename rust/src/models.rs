use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CONFIG_VERSION: i32 = 9;
pub const DEFAULT_SERVER_PROXY_PORT: i32 = 24443;
pub const COMMON_OVERSEAS_GROUP: &str = "common-overseas";
pub const NODE_DIALER_PROXY_KEY: &str = "_network-manager-dialer-proxy";
pub const NODE_DIALER_POLICY_KEY: &str = "_network-manager-dialer-policy";

pub fn random_hex(len: usize) -> String {
    const ALPHABET: &[u8] = b"0123456789abcdef";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

fn random_token_urlsafe(len: usize) -> String {
    const ALPHABET: &[u8] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut rng = rand::thread_rng();
    (0..len)
        .map(|_| ALPHABET[rng.gen_range(0..ALPHABET.len())] as char)
        .collect()
}

pub fn random_server_proxy_port() -> i32 {
    let mut rng = rand::thread_rng();
    10000 + rng.gen_range(0..(65536 - 10000))
}

pub fn normalize_group(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn default_controller_secret() -> String {
    random_token_urlsafe(32)
}

fn default_clash() -> Upstream {
    Upstream {
        name: "Clash 7897".into(),
        host: "127.0.0.1".into(),
        port: 7897,
        protocol: "socks5".into(),
        enabled: true,
    }
}

fn default_v2ray() -> Upstream {
    Upstream {
        name: "v2ray 10808".into(),
        host: "127.0.0.1".into(),
        port: 10808,
        protocol: "socks5".into(),
        enabled: true,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Upstream {
    pub name: String,
    pub host: String,
    pub port: i32,
    pub protocol: String,
    pub enabled: bool,
}

impl Default for Upstream {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: "127.0.0.1".into(),
            port: 1080,
            protocol: "socks5".into(),
            enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RoutingRule {
    pub rule_type: String,
    pub value: String,
    pub target: String,
    pub enabled: bool,
    pub note: String,
    pub group: String,
}

impl Default for RoutingRule {
    fn default() -> Self {
        Self {
            rule_type: "DOMAIN-SUFFIX".into(),
            value: String::new(),
            target: "DIRECT".into(),
            enabled: true,
            note: String::new(),
            group: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ImportedNode {
    pub node_id: String,
    pub source: String,
    pub config: Value,
    pub source_id: String,
    pub group: String,
}

impl Default for ImportedNode {
    fn default() -> Self {
        Self {
            node_id: random_hex(16),
            source: "手动导入".into(),
            config: Value::Object(Default::default()),
            source_id: String::new(),
            group: String::new(),
        }
    }
}

impl ImportedNode {
    pub fn name(&self) -> String {
        self.config
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("未命名节点")
            .to_string()
    }

    pub fn protocol(&self) -> String {
        self.config
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string()
    }

    pub fn dialer_proxy(&self) -> String {
        self.config
            .get(NODE_DIALER_PROXY_KEY)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    }

    pub fn dialer_policy(&self) -> String {
        self.config
            .get(NODE_DIALER_POLICY_KEY)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SubscriptionSource {
    pub source_id: String,
    pub name: String,
    pub url: String,
    pub last_updated: String,
    pub group: String,
}

impl Default for SubscriptionSource {
    fn default() -> Self {
        Self {
            source_id: random_hex(16),
            name: "订阅".into(),
            url: String::new(),
            last_updated: String::new(),
            group: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SshServerProfile {
    pub profile_id: String,
    pub name: String,
    pub host: String,
    pub port: i32,
    pub username: String,
    pub local_port: i32,
    pub auth_method: String,
    pub key_path: String,
    pub remember_password: bool,
    pub auto_connect: bool,
    pub proxy_port: i32,
    pub deployed_node_id: String,
    pub deployed_at: String,
    pub deployed_version: String,
    pub region: String,
    pub proxy_reachable: Option<bool>,
    pub proxy_reachability_error: String,
}

impl Default for SshServerProfile {
    fn default() -> Self {
        Self {
            profile_id: random_hex(16),
            name: "SSH 服务器".into(),
            host: String::new(),
            port: 22,
            username: "root".into(),
            local_port: 10888,
            auth_method: "password".into(),
            key_path: String::new(),
            remember_password: false,
            auto_connect: false,
            proxy_port: DEFAULT_SERVER_PROXY_PORT,
            deployed_node_id: String::new(),
            deployed_at: String::new(),
            deployed_version: String::new(),
            region: String::new(),
            proxy_reachable: None,
            proxy_reachability_error: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub version: i32,
    pub mode: String,
    pub default_target: String,
    pub mixed_port: i32,
    pub controller_port: i32,
    pub dns_port: i32,
    pub server_proxy_port: i32,
    pub controller_secret: String,
    pub strict_route: bool,
    pub start_on_launch: bool,
    pub close_to_tray: bool,
    pub start_with_windows: bool,
    pub clash: Upstream,
    pub v2ray: Upstream,
    pub selected_node: String,
    pub imported_nodes: Vec<ImportedNode>,
    pub node_groups: Vec<String>,
    pub subscriptions: Vec<SubscriptionSource>,
    pub ssh_servers: Vec<SshServerProfile>,
    pub selected_ssh_server: String,
    pub rules: Vec<RoutingRule>,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            version: CONFIG_VERSION,
            mode: "RULE".into(),
            default_target: "DIRECT".into(),
            mixed_port: 17897,
            controller_port: 19090,
            dns_port: 11053,
            server_proxy_port: random_server_proxy_port(),
            controller_secret: default_controller_secret(),
            strict_route: true,
            start_on_launch: false,
            close_to_tray: true,
            start_with_windows: false,
            clash: default_clash(),
            v2ray: default_v2ray(),
            selected_node: String::new(),
            imported_nodes: Vec::new(),
            node_groups: Vec::new(),
            subscriptions: Vec::new(),
            ssh_servers: Vec::new(),
            selected_ssh_server: String::new(),
            rules: default_routing_rules(),
        }
    }
}

pub fn default_routing_rules() -> Vec<RoutingRule> {
    let mut rules = vec![RoutingRule {
        rule_type: "PROCESS-NAME".into(),
        value: "Discord.exe".into(),
        target: "CLASH".into(),
        note: "Discord 全部流量".into(),
        ..Default::default()
    }];
    for (domain, label) in DEFAULT_PROXY_DOMAINS {
        rules.push(RoutingRule {
            rule_type: "DOMAIN-SUFFIX".into(),
            value: (*domain).into(),
            target: "CLASH".into(),
            note: (*label).into(),
            group: COMMON_OVERSEAS_GROUP.into(),
            ..Default::default()
        });
    }
    rules
}

pub const DEFAULT_PROXY_DOMAINS: &[(&str, &str)] = &[
    ("discord.com", "Discord"),
    ("discordapp.com", "Discord"),
    ("google.com", "Google"),
    ("googleapis.com", "Google"),
    ("gstatic.com", "Google"),
    ("googleusercontent.com", "Google"),
    ("youtube.com", "YouTube"),
    ("youtu.be", "YouTube"),
    ("ytimg.com", "YouTube"),
    ("googlevideo.com", "YouTube"),
    ("openai.com", "ChatGPT / OpenAI"),
    ("chatgpt.com", "ChatGPT / OpenAI"),
    ("oaistatic.com", "ChatGPT / OpenAI"),
    ("oaiusercontent.com", "ChatGPT / OpenAI"),
    ("claude.ai", "Claude / Anthropic"),
    ("anthropic.com", "Claude / Anthropic"),
    ("github.com", "GitHub"),
    ("githubassets.com", "GitHub"),
    ("githubusercontent.com", "GitHub"),
    ("gitlab.com", "GitLab"),
    ("stackoverflow.com", "Stack Overflow"),
    ("docker.com", "Docker"),
    ("docker.io", "Docker"),
    ("npmjs.com", "npm"),
    ("pypi.org", "PyPI"),
    ("huggingface.co", "Hugging Face"),
    ("perplexity.ai", "Perplexity"),
    ("x.com", "X / Twitter"),
    ("twitter.com", "X / Twitter"),
    ("twimg.com", "X / Twitter"),
    ("facebook.com", "Facebook"),
    ("instagram.com", "Instagram"),
    ("reddit.com", "Reddit"),
    ("telegram.org", "Telegram"),
    ("t.me", "Telegram"),
    ("whatsapp.com", "WhatsApp"),
    ("linkedin.com", "LinkedIn"),
    ("netflix.com", "Netflix"),
    ("spotify.com", "Spotify"),
    ("twitch.tv", "Twitch"),
    ("wikipedia.org", "Wikipedia"),
    ("notion.so", "Notion"),
    ("slack.com", "Slack"),
    ("duckduckgo.com", "DuckDuckGo"),
    ("steamcommunity.com", "Steam"),
    ("steampowered.com", "Steam"),
];

pub fn server_proxy_port_error(port: i32, ssh_port: i32) -> String {
    if !(1..=65535).contains(&port) {
        return "远端代理端口必须在 1 到 65535 之间".into();
    }
    if port == ssh_port {
        return "远端代理端口不能与 SSH 端口相同".into();
    }
    if port < 1024 {
        return "远端代理端口建议使用 1024 以上的端口".into();
    }
    String::new()
}
