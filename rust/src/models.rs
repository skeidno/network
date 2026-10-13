use rand::Rng;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::IpAddr;

pub const CONFIG_VERSION: i32 = 13;
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
    /// 本地 DNS 映射（自定义域名 → 固定 IP）。设备本地配置，不进跨设备导入导出。
    pub local_dns: Vec<LocalDnsEntry>,
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
            local_dns: Vec::new(),
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

// ---------------------------------------------------------------------------
// 本地 DNS 映射
// ---------------------------------------------------------------------------

/// 映射条数上限：够盖住一个局域网，也不至于把内核规则撑到没法看。
pub const LOCAL_DNS_MAX_ENTRIES: usize = 200;

/// 本地 DNS 映射的一条：把自定义域名固定指向某个 IP。
///
/// 用途和平台无关：在局域网里用域名访问 NAS、路由器、自建站点，不用每台设备各
/// 记一套 IP；域名和服务端证书里的名字对上了，HTTPS 才有机会通过校验。
///
/// 域名支持两种写法：
///
/// - `nas.lan`：**精确**匹配这一个名字；
/// - `*.baidu.com`：**后缀**匹配，`baidu.com` 本身和它的所有子域都算（`www.baidu.com`、
///   `tieba.baidu.com`……）。适合「一个站点有几十个子域，全都要指到同一台机器」。
///
/// 通配是用 `*.` 而不是裸后缀，是因为裸后缀太容易误伤（写个 `com` 就等于接管整个 .com）。
/// 两端内核给通配留了不同的口子，所以 `*.x` 的含义在这里定死，由各自的生成器翻译：
/// mihomo 的 `hosts` 用 `+.baidu.com`；sing-box 没有通配查表，改用 `predefined` 动作的
/// 「记录名以 `*` 开头时替换成实际查询名」这条规则（见 SingBoxConfigBuilder）。
///
/// 字段带 `#[serde(default)]`：settings.json 是允许手改的，缺一个字段不该让整份
/// 配置反序列化失败（那会把用户所有配置一起隔离掉）。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LocalDnsEntry {
    pub domain: String,
    pub address: String,
}

/// 通配前缀。写成 `*.baidu.com` 表示「baidu.com 及其所有子域」。
pub const LOCAL_DNS_WILDCARD_PREFIX: &str = "*.";

impl LocalDnsEntry {
    /// 通配条目的后缀部分：`*.baidu.com` → `Some("baidu.com")`；精确条目 → `None`。
    pub fn wildcard_suffix(&self) -> Option<&str> {
        self.domain
            .strip_prefix(LOCAL_DNS_WILDCARD_PREFIX)
            .filter(|suffix| !suffix.is_empty())
    }

    pub fn is_wildcard(&self) -> bool {
        self.wildcard_suffix().is_some()
    }

    /// 与内核规则一致的命中判断：精确条目比较全名，通配条目按后缀（含自身）比较。
    pub fn matches_domain(&self, host: &str) -> bool {
        let host = host.trim().trim_end_matches('.').to_lowercase();
        match self.wildcard_suffix() {
            Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
            None => host == self.domain,
        }
    }
}

/// 只认**字面量** IP。
///
/// 绝不用 `ToSocketAddrs` / `getaddrinfo` 去碰主机名 —— 它收到 `example.com` 会真的
/// 发起 DNS 查询，让一次纯字符串判断变成一次网络往返。`IpAddr::from_str` 只解析字面量，
/// 且拒绝前导零这类歧义写法。
pub fn parse_ip_literal(text: &str) -> Option<IpAddr> {
    let value = text
        .trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .split('%')
        .next()
        .unwrap_or("")
        .trim();
    if value.is_empty() {
        return None;
    }
    value.parse::<IpAddr>().ok()
}

pub fn is_ip_literal(text: &str) -> bool {
    parse_ip_literal(text).is_some()
}

/// 域名归一：能贴 `https://nas.lan/admin` 也能贴 `nas.lan`，末尾的点去掉，统一小写。
/// 通配前缀 `*.` 保留在最前面 —— 它是要落到内核规则里的语义，不能被当成普通字符丢掉。
pub fn normalize_local_dns_domain(raw: &str) -> String {
    let text = raw.trim();
    let (wildcard, rest) = match text.strip_prefix(LOCAL_DNS_WILDCARD_PREFIX) {
        Some(rest) => (true, rest.trim()),
        None => (false, text),
    };
    let host = match url::Url::parse(rest) {
        Ok(parsed) if parsed.host_str().is_some() => parsed.host_str().unwrap_or("").to_string(),
        _ => rest.to_string(),
    };
    let domain = host
        .split('/')
        .next()
        .unwrap_or("")
        .trim()
        .to_lowercase()
        .trim_end_matches('.')
        .to_string();
    if wildcard {
        format!("{LOCAL_DNS_WILDCARD_PREFIX}{domain}")
    } else {
        domain
    }
}

/// 存下来的地址统一成不带方括号的字面量。
///
/// 内核的 hosts 表要的是 `fd00::5`，而用户从 URL 或别处粘过来的多半是 `[fd00::5]`。
/// 校验仍然在带括号的原串上做（两种写法都认），落盘只留一种，免得同一个地址
/// 在配置里出现两种写法。
fn normalize_address_literal(raw: &str) -> String {
    raw.trim()
        .trim_start_matches('[')
        .trim_end_matches(']')
        .trim()
        .to_string()
}

/// 返回 None 表示合法，否则是直接给用户看的失败原因。
pub fn local_dns_domain_error(raw: &str) -> Option<String> {
    let domain = normalize_local_dns_domain(raw);
    let checked = domain
        .strip_prefix(LOCAL_DNS_WILDCARD_PREFIX)
        .unwrap_or(&domain);
    if checked.is_empty() {
        return Some("域名不能为空".into());
    }
    if checked.contains('*') {
        return Some(format!(
            "通配只能写在最前面，例如 {LOCAL_DNS_WILDCARD_PREFIX}baidu.com"
        ));
    }
    if checked.chars().any(char::is_whitespace) {
        return Some("域名里不能有空格".into());
    }
    if checked.contains(':') {
        return Some("这里只填域名，不要带端口 —— 端口由访问方决定".into());
    }
    if !checked.contains('.') {
        return Some("请输入完整域名，例如 nas.lan".into());
    }
    if !is_valid_domain(checked) {
        return Some("域名里有非法字符".into());
    }
    None
}

fn is_valid_domain(domain: &str) -> bool {
    let labels: Vec<&str> = domain.split('.').collect();
    if labels.len() < 2 {
        return false;
    }
    labels.iter().all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && label.chars().next().map(|c| c.is_ascii_alphanumeric()) == Some(true)
            && label.chars().last().map(|c| c.is_ascii_alphanumeric()) == Some(true)
    })
}

pub fn local_dns_address_error(raw: &str) -> Option<String> {
    let address = raw.trim();
    if address.is_empty() {
        return Some("IP 不能为空".into());
    }
    if !is_ip_literal(address) {
        return Some(format!(
            "「{address}」不是合法的 IP（只支持 IPv4 / IPv6 字面量）"
        ));
    }
    None
}

/// 解析「域名 IP」多行文本。做成纯函数，界面与单测共用同一套规则。
///
/// - 空行、`#` 注释行忽略；行尾 `#` 之后也算注释（所以能把 hosts 文件直接粘进来）
/// - 一行的两个字段顺序随意：`nas.lan 192.168.1.10` 和 `192.168.1.10 nas.lan` 等价
/// - 同一域名重复出现时后写的覆盖先写的
/// - 任何一行有问题都直接返回错误（带行号），避免存半截配置
pub fn local_dns_entries_from_text(text: &str) -> Result<Vec<LocalDnsEntry>, String> {
    let mut entries: Vec<LocalDnsEntry> = Vec::new();
    for (index, raw_line) in text.lines().enumerate() {
        let line = raw_line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let parts: Vec<&str> = line
            .split(|c: char| c.is_whitespace() || c == ',')
            .filter(|part| !part.is_empty())
            .collect();
        if parts.len() < 2 {
            return Err(format!(
                "第 {} 行：一行要写「域名 IP」两段内容，例如 nas.lan 192.168.1.10",
                index + 1
            ));
        }
        // 字段顺序随意：hosts 文件是「IP 域名」，命令行习惯是「域名 IP」，两种都收。
        let (domain_part, address_part) = match (is_ip_literal(parts[0]), is_ip_literal(parts[1])) {
            (false, true) => (parts[0], parts[1]),
            (true, false) => (parts[1], parts[0]),
            (true, true) => return Err(format!("第 {} 行：两段都是 IP，没有域名", index + 1)),
            (false, false) => (parts[0], parts[1]),
        };
        if let Some(reason) = local_dns_domain_error(domain_part) {
            return Err(format!("第 {} 行：{reason}", index + 1));
        }
        if let Some(reason) = local_dns_address_error(address_part) {
            return Err(format!("第 {} 行：{reason}", index + 1));
        }
        let domain = normalize_local_dns_domain(domain_part);
        if let Some(position) = entries.iter().position(|entry| entry.domain == domain) {
            entries.remove(position);
        }
        entries.push(LocalDnsEntry {
            domain,
            address: normalize_address_literal(address_part),
        });
    }
    if entries.len() > LOCAL_DNS_MAX_ENTRIES {
        return Err(format!(
            "最多 {LOCAL_DNS_MAX_ENTRIES} 条映射，当前 {} 条",
            entries.len()
        ));
    }
    Ok(entries)
}

/// 解析结果回写成编辑框文本，保证「打开编辑器不改动」时是等价的一行一条。
pub fn local_dns_entries_to_text(entries: &[LocalDnsEntry]) -> String {
    entries
        .iter()
        .map(|entry| format!("{} {}", entry.domain, entry.address))
        .collect::<Vec<_>>()
        .join("\n")
}

/// 只保留仍然合法的那几条：配置是外部文本存下来的，读回来时不该信任。
pub fn sanitize_local_dns_entries(entries: Vec<LocalDnsEntry>) -> Vec<LocalDnsEntry> {
    let mut result: Vec<LocalDnsEntry> = Vec::new();
    for entry in entries {
        let domain = normalize_local_dns_domain(&entry.domain);
        let address = normalize_address_literal(&entry.address);
        if local_dns_domain_error(&domain).is_some() || local_dns_address_error(&address).is_some() {
            continue;
        }
        if let Some(position) = result.iter().position(|item| item.domain == domain) {
            result.remove(position);
        }
        result.push(LocalDnsEntry { domain, address });
    }
    result.truncate(LOCAL_DNS_MAX_ENTRIES);
    result
}

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

#[cfg(test)]
mod local_dns_tests {
    use super::*;

    fn parsed(text: &str) -> Vec<LocalDnsEntry> {
        local_dns_entries_from_text(text).expect("应当解析成功")
    }

    #[test]
    fn accepts_both_field_orders_and_urls() {
        let entries = parsed(
            "nas.lan 192.168.1.10\n192.168.1.11 router.lan\nhttps://git.lan/admin 10.0.0.7\n",
        );
        assert_eq!(
            entries,
            vec![
                LocalDnsEntry {
                    domain: "nas.lan".into(),
                    address: "192.168.1.10".into()
                },
                LocalDnsEntry {
                    domain: "router.lan".into(),
                    address: "192.168.1.11".into()
                },
                LocalDnsEntry {
                    domain: "git.lan".into(),
                    address: "10.0.0.7".into()
                },
            ]
        );
    }

    #[test]
    fn ignores_blank_lines_and_comments() {
        // 直接把 /etc/hosts 粘进来：整行注释、行尾注释、空行都要吃掉。
        let text = "# 家里的服务\n\n\nnas.lan 192.168.1.10   # NAS\n\n";
        assert_eq!(parsed(text).len(), 1);
    }

    #[test]
    fn later_duplicate_wins_and_keeps_one_entry() {
        let entries = parsed("nas.lan 192.168.1.10\nNAS.lan 192.168.1.99");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].domain, "nas.lan");
        assert_eq!(entries[0].address, "192.168.1.99");
    }

    #[test]
    fn accepts_ipv6_literals_and_drops_the_brackets() {
        // 粘 URL 写法要认，但落盘只留一种：hosts 表里需要的是 `fd00::5`。
        let entries = parsed("nas.lan [fd00::5]");
        assert_eq!(entries[0].address, "fd00::5");
    }

    #[test]
    fn rejects_a_line_with_only_one_field() {
        let error = local_dns_entries_from_text("nas.lan").expect_err("应当报错");
        assert!(error.starts_with("第 1 行："), "{error}");
        assert!(error.contains("nas.lan 192.168.1.10"), "{error}");
    }

    #[test]
    fn rejects_two_ip_addresses_without_a_domain() {
        let error = local_dns_entries_from_text("nas.lan 192.168.1.10\n192.168.1.10 192.168.1.11")
            .expect_err("应当报错");
        assert!(error.starts_with("第 2 行："), "{error}");
        assert!(error.contains("没有域名"), "{error}");
    }

    #[test]
    fn accepts_wildcard_entries() {
        let entries = parsed("*.baidu.com 127.0.0.1");
        assert_eq!(entries[0].domain, "*.baidu.com");
        assert!(entries[0].is_wildcard());
        assert_eq!(entries[0].wildcard_suffix(), Some("baidu.com"));
    }

    #[test]
    fn wildcard_normalization_keeps_the_prefix() {
        // 大小写、末尾的点、整条 URL 都要能贴进来，前缀不能被吃掉。
        assert_eq!(normalize_local_dns_domain("*.BAIDU.com."), "*.baidu.com");
        assert_eq!(normalize_local_dns_domain(" *.baidu.com "), "*.baidu.com");
    }

    #[test]
    fn wildcard_matches_the_apex_and_every_subdomain() {
        let entry = LocalDnsEntry {
            domain: "*.baidu.com".into(),
            address: "127.0.0.1".into(),
        };
        assert!(entry.matches_domain("baidu.com"));
        assert!(entry.matches_domain("www.baidu.com"));
        assert!(entry.matches_domain("tieba.baidu.com"));
        assert!(entry.matches_domain("WWW.BAIDU.COM."));
        // 后缀匹配要按标签来，不能被同尾字符串蹭到。
        assert!(!entry.matches_domain("notbaidu.com"));
        assert!(!entry.matches_domain("baidu.com.cn"));
    }

    #[test]
    fn exact_entries_stay_exact() {
        let entry = LocalDnsEntry {
            domain: "nas.lan".into(),
            address: "192.168.1.10".into(),
        };
        assert!(entry.matches_domain("nas.lan"));
        assert!(!entry.matches_domain("www.nas.lan"));
        assert!(!entry.matches_domain("other.lan"));
    }

    #[test]
    fn rejects_bad_wildcards_and_missing_dots() {
        // 通配只认最前面的 `*.`，且后面必须是一个完整域名（`*.com` 这类会接管整个顶级域）。
        assert!(local_dns_entries_from_text("nas.*.lan 192.168.1.10").is_err());
        assert!(local_dns_entries_from_text("*.com 192.168.1.10").is_err());
        assert!(local_dns_entries_from_text("*.lan 192.168.1.10").is_err());
        assert!(local_dns_entries_from_text("nas 192.168.1.10").is_err());
        assert!(local_dns_entries_from_text("nas.lan:8080 192.168.1.10").is_err());
    }

    #[test]
    fn rejects_hostnames_as_addresses() {
        // 地址只认字面量：写域名的话，判断本身会变成一次真实 DNS 查询。
        let error = local_dns_entries_from_text("nas.lan router.lan").expect_err("应当报错");
        assert!(error.contains("不是合法的 IP"), "{error}");
        assert!(local_dns_entries_from_text("nas.lan 192.168.1.256").is_err());
        assert!(local_dns_entries_from_text("nas.lan 10.0.0").is_err());
    }

    #[test]
    fn text_round_trip_is_stable() {
        let text = "nas.lan 192.168.1.10\nrouter.lan 192.168.1.1";
        assert_eq!(local_dns_entries_to_text(&parsed(text)), text);
    }

    #[test]
    fn sanitize_drops_invalid_and_duplicate_entries() {
        let entries = vec![
            LocalDnsEntry {
                domain: "NAS.lan".into(),
                address: " 192.168.1.10 ".into(),
            },
            LocalDnsEntry {
                domain: "nas.lan".into(),
                address: "192.168.1.99".into(),
            },
            LocalDnsEntry {
                domain: "*.com".into(),
                address: "192.168.1.1".into(),
            },
            LocalDnsEntry {
                domain: "ok.lan".into(),
                address: "router.lan".into(),
            },
            LocalDnsEntry {
                domain: String::new(),
                address: "192.168.1.1".into(),
            },
        ];
        assert_eq!(
            sanitize_local_dns_entries(entries),
            vec![LocalDnsEntry {
                domain: "nas.lan".into(),
                address: "192.168.1.99".into()
            }]
        );
    }

    #[test]
    fn rejects_more_entries_than_the_limit() {
        let text = (0..LOCAL_DNS_MAX_ENTRIES + 1)
            .map(|index| format!("host{index}.lan 192.168.1.1"))
            .collect::<Vec<_>>()
            .join("\n");
        let error = local_dns_entries_from_text(&text).expect_err("应当报错");
        assert!(error.contains("最多"), "{error}");
    }

    #[test]
    fn config_without_local_dns_field_still_loads() {
        // 老配置文件里没有 local_dns：不能因此整份配置被隔离掉。
        let json = serde_json::json!({"mode": "RULE", "mixed_port": 17897});
        let config: AppConfig = serde_json::from_value(json).expect("应当能读回");
        assert!(config.local_dns.is_empty());
    }

    #[test]
    fn entry_with_missing_address_does_not_break_the_config() {
        // 手改 settings.json 时少写一个字段，只应当丢掉这一条。
        let json = serde_json::json!({
            "mode": "RULE",
            "local_dns": [{"domain": "nas.lan"}, {"domain": "router.lan", "address": "192.168.1.1"}],
        });
        let config: AppConfig = serde_json::from_value(json).expect("应当能读回");
        let sanitized = sanitize_local_dns_entries(config.local_dns);
        assert_eq!(sanitized.len(), 1);
        assert_eq!(sanitized[0].domain, "router.lan");
    }
}
