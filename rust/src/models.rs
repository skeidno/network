use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const CONFIG_VERSION: i32 = 12;
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
            // 关闭窗口默认只收起界面、程序退到托盘继续跑（这是托盘类程序的常规行为）。
            // 之前默认关闭即退出，是因为托盘点击唤不回界面 —— 那是托盘消息链路的 bug
            // （explorer 用 SendMessage 投递、通知码在 lParam 低位字），已经修好了，
            // 见 rust/src/tray.rs 的记录。要完整退出用设置页的「退出并停止接管」
            // 或托盘菜单的退出；想让 X 直接退出就把这个开关取消勾选。
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
    ("discord.gg", "Discord"),
    ("discordapp.net", "Discord"),
    ("discordcdn.com", "Discord"),
    ("discord.media", "Discord"),
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
    ("whatsapp.net", "WhatsApp"),
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
    // Telegram 官方域名体系不止 telegram.org / t.me：下载服务器、图片 CDN、
    // Instant View、短链都挂在别的后缀上，只放行主域名会出现「能登录但图片
    // 全是灰块」的情况。
    ("telegram.me", "Telegram"),
    ("cdn-telegram.org", "Telegram"),
    ("telesco.pe", "Telegram"),
    ("telegra.ph", "Telegram"),
    ("telegram.dog", "Telegram"),
    ("telega.one", "Telegram"),
    // 社交媒体的静态资源 CDN 与自有后缀不在主域名下。
    ("fbcdn.net", "Facebook"),
    ("cdninstagram.com", "Instagram"),
    ("threads.net", "Threads"),
    ("scdn.co", "Spotify"),
    ("nflximg.net", "Netflix"),
    ("nflxvideo.net", "Netflix"),
    ("twitchcdn.net", "Twitch"),
    ("redd.it", "Reddit"),
    ("redditstatic.com", "Reddit"),
    ("line.me", "LINE"),
    ("line-apps.com", "LINE"),
    ("pinterest.com", "Pinterest"),
    ("tumblr.com", "Tumblr"),
    ("quora.com", "Quora"),
    ("medium.com", "Medium"),
    ("substack.com", "Substack"),
    ("mastodon.social", "Mastodon"),
    ("wikimedia.org", "Wikipedia"),
    // 加密货币：交易所主站 + 常见的区域备用域名。
    ("binance.com", "币安 Binance"),
    ("binance.me", "币安 Binance"),
    ("binance.us", "币安 Binance US"),
    ("okx.com", "OKX 欧易"),
    ("okex.com", "OKX 欧易"),
    ("coinbase.com", "Coinbase"),
    ("bybit.com", "Bybit"),
    ("kucoin.com", "KuCoin"),
    ("gate.io", "Gate.io"),
    ("bitget.com", "Bitget"),
    ("kraken.com", "Kraken"),
    ("bitfinex.com", "Bitfinex"),
    ("crypto.com", "Crypto.com"),
    ("coingecko.com", "CoinGecko"),
    ("coinmarketcap.com", "CoinMarketCap"),
    ("tradingview.com", "TradingView"),
    ("dexscreener.com", "DEX Screener"),
    ("gmgn.ai", "GMGN"),
    ("metamask.io", "MetaMask"),
    ("etherscan.io", "Etherscan"),
    // AI 工具。
    ("x.ai", "Grok"),
    ("poe.com", "Poe"),
    ("midjourney.com", "Midjourney"),
    ("stability.ai", "Stability AI"),
    ("runwayml.com", "Runway"),
    ("suno.com", "Suno"),
    ("cursor.com", "Cursor"),
    ("cursor.sh", "Cursor"),
    ("deepseek.com", "DeepSeek"),
    // 开发者常用： registry / CDN 往往不是主域名后缀的子域。
    ("stackexchange.com", "Stack Exchange"),
    ("registry.npmjs.org", "npm"),
    ("pythonhosted.org", "PyPI"),
    ("files.pythonhosted.org", "PyPI"),
    ("crates.io", "crates.io"),
    ("rust-lang.org", "Rust"),
    ("jetbrains.com", "JetBrains"),
    ("vercel.com", "Vercel"),
    ("netlify.com", "Netlify"),
    ("cloudflare.com", "Cloudflare"),
    ("digitalocean.com", "DigitalOcean"),
    ("vultr.com", "Vultr"),
    ("linode.com", "Linode"),
    ("herokuapp.com", "Heroku"),
    ("render.com", "Render"),
    // 流媒体。
    ("disneyplus.com", "Disney+"),
    ("hulu.com", "Hulu"),
    ("max.com", "HBO Max"),
    ("paramountplus.com", "Paramount+"),
    ("primevideo.com", "Prime Video"),
    ("soundcloud.com", "SoundCloud"),
    ("vimeo.com", "Vimeo"),
    ("dailymotion.com", "Dailymotion"),
    // 生产力与工具类。
    ("deepl.com", "DeepL"),
    ("grammarly.com", "Grammarly"),
    ("canva.com", "Canva"),
    ("figma.com", "Figma"),
    ("miro.com", "Miro"),
    ("notion.site", "Notion"),
    ("dropbox.com", "Dropbox"),
    ("bitwarden.com", "Bitwarden"),
    ("1password.com", "1Password"),
    ("proton.me", "Proton"),
    ("protonmail.com", "Proton Mail"),
    ("zoom.us", "Zoom"),
    ("outlook.com", "Outlook"),
    ("live.com", "Microsoft 账号"),
    ("office.com", "Microsoft 365"),
    // 财经媒体。
    ("bloomberg.com", "Bloomberg"),
    ("reuters.com", "Reuters"),
    ("wsj.com", "华尔街日报"),
    ("ft.com", "金融时报"),
    ("economist.com", "经济学人"),
    ("nytimes.com", "纽约时报"),
    ("bbc.com", "BBC"),
    ("bbc.co.uk", "BBC"),
    // 购物与游戏。
    ("amazon.com", "Amazon"),
    ("ebay.com", "eBay"),
    ("shopify.com", "Shopify"),
    ("bestbuy.com", "Best Buy"),
    ("iherb.com", "iHerb"),
    ("epicgames.com", "Epic Games"),
    ("roblox.com", "Roblox"),
    ("minecraft.net", "Minecraft"),
    ("playstation.com", "PlayStation"),
    ("nintendo.com", "Nintendo"),
    ("blizzard.com", "Blizzard"),
    ("battle.net", "Battle.net"),
    ("riotgames.com", "Riot Games"),
    ("archive.org", "Internet Archive"),
];

/// 上一版内置的域名快照（v12 之前）。
///
/// 「常用海外站点」不是每次启动算出来的派生数据，而是实实在在写进配置文件的
/// 一组规则，所以往 [`DEFAULT_PROXY_DOMAINS`] 里加了域名，老配置文件里也不会
/// 自动出现。升级时要把新域名补进去，但用户自己删掉过的条目不能被塞回来 ——
/// 只能拿新旧清单做差集，这时就需要上一版的快照。
///
/// 下次再扩充上面的清单时，把当时的旧列表挪到这里即可。
pub const PROXY_DOMAINS_V11: &[&str] = &[
    "discord.com",
    "discordapp.com",
    "google.com",
    "googleapis.com",
    "gstatic.com",
    "googleusercontent.com",
    "youtube.com",
    "youtu.be",
    "ytimg.com",
    "googlevideo.com",
    "openai.com",
    "chatgpt.com",
    "oaistatic.com",
    "oaiusercontent.com",
    "claude.ai",
    "anthropic.com",
    "github.com",
    "githubassets.com",
    "githubusercontent.com",
    "gitlab.com",
    "stackoverflow.com",
    "docker.com",
    "docker.io",
    "npmjs.com",
    "pypi.org",
    "huggingface.co",
    "perplexity.ai",
    "x.com",
    "twitter.com",
    "twimg.com",
    "facebook.com",
    "instagram.com",
    "reddit.com",
    "telegram.org",
    "t.me",
    "whatsapp.com",
    "linkedin.com",
    "netflix.com",
    "spotify.com",
    "twitch.tv",
    "wikipedia.org",
    "notion.so",
    "slack.com",
    "duckduckgo.com",
    "steamcommunity.com",
    "steampowered.com",
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
