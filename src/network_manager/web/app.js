"use strict";

const SESSION_TOKEN = document.querySelector('meta[name="network-session-token"]')?.content || "";
const CUSTOM_FRAME = new URLSearchParams(window.location.search).get("customFrame") === "1";
document.body.classList.toggle("custom-frame", CUSTOM_FRAME);
let appState = null;
let currentPage = "overview";
let currentNodeTab = "local";
let sourceSignature = "";
let settingsInitialized = false;
let refreshing = false;
let logsRefreshing = false;
let lastLogsRefreshAt = 0;
let lastLogsContent = null;
let coreActionPending = false;
let coreActionStartedAt = 0;
// 接管动作的硬兜底：后端再慢、请求再怎么挂起，也不能让按钮永远停在「处理中」。
const CORE_ACTION_MAX_WAIT_MS = 25000;
let requestedPage = currentPage;
let pageSwitchFrame = 0;
let initialFailureCount = 0;
let statePollTimer = 0;
// 服务器部署页的多选集合（表格视图下用于批量操作）
const sshSelection = new Set();
// 正在等后端回话的服务器（profileId）。部署/检查是同步长请求，后端状态要等
// 下一轮轮询才可见，这里先本地记一笔，点击后界面立刻显示「检查中」。
const pendingServerTasks = new Set();
const renderSignatures = new Map();
const COLLAPSED_NODE_GROUPS_KEY = "network-manager.collapsed-node-groups";
const DEFAULT_SERVER_PROXY_PORT = 24443;
const MIN_RANDOM_SERVER_PROXY_PORT = 10000;
const collapsedNodeGroups = loadCollapsedNodeGroups();

function serverProxyPortError(port, sshPort) {
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    return "远端代理端口必须在 1 到 65535 之间";
  }
  if (port === sshPort) return `远端代理端口不能与 SSH 端口 ${sshPort} 相同`;
  return "";
}

function loadCollapsedNodeGroups() {
  try {
    const stored = JSON.parse(sessionStorage.getItem(COLLAPSED_NODE_GROUPS_KEY) || "[]");
    return new Set(Array.isArray(stored) ? stored.filter((name) => typeof name === "string") : []);
  } catch (_error) {
    return new Set();
  }
}

function saveCollapsedNodeGroups() {
  try {
    sessionStorage.setItem(COLLAPSED_NODE_GROUPS_KEY, JSON.stringify([...collapsedNodeGroups]));
  } catch (_error) {
    // Session storage is optional in embedded browser profiles.
  }
}

function toggleNodeGroup(groupName) {
  if (collapsedNodeGroups.has(groupName)) collapsedNodeGroups.delete(groupName);
  else collapsedNodeGroups.add(groupName);
  saveCollapsedNodeGroups();
  renderNodes();
}

function setBootstrapStatus(message, isError = false) {
  const status = byId("bootstrap-status");
  if (!status) return;
  status.classList.toggle("is-error", isError);
  const label = status.querySelector("strong");
  if (label) label.textContent = message;
}

const PAGE_TITLES = {
  overview: "运行概览",
  rules: "分流规则",
  nodes: "代理与节点",
  servers: "服务器管理",
  settings: "设置",
  env: "环境管理",
  logs: "运行日志",
};

const MODE_META = [
  ["RULE", "规则分流", "按分流规则选择出口"],
  ["GLOBAL_CLASH", "全局 Clash", "所有流量经 Clash 转发"],
  ["GLOBAL_V2RAY", "全局 v2ray", "所有流量经 v2ray 转发"],
  ["GLOBAL_BUILTIN", "全局节点", "所有流量经当前内置节点转发"],
  ["SMART", "智能节点", "自动选择低延迟节点，变慢或失效时切换"],
  ["DIRECT", "全局直连", "所有流量不经过代理"],
];

const RULE_TYPES = [
  ["PROCESS-NAME", "程序名称"],
  ["DOMAIN", "完整域名"],
  ["DOMAIN-SUFFIX", "域名后缀"],
  ["DOMAIN-KEYWORD", "域名关键字"],
  ["IP-CIDR", "IPv4 网段"],
];

const TARGETS = [
  ["CLASH", "Clash"],
  ["V2RAY", "v2ray"],
  ["BUILTIN", "内置节点"],
  ["DIRECT", "直连"],
];

const byId = (id) => document.getElementById(id);

function setText(target, value) {
  const element = typeof target === "string" ? byId(target) : target;
  const text = String(value ?? "");
  if (element.textContent !== text) element.textContent = text;
}

function shouldRender(key, value) {
  const signature = JSON.stringify(value);
  if (renderSignatures.get(key) === signature) return false;
  renderSignatures.set(key, signature);
  return true;
}

function escapeHtml(value) {
  return String(value ?? "")
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function icon(name) {
  return `<span class="icon" style="--icon:url('./icons/${name}.svg')"></span>`;
}

async function apiRequest(path, options = {}) {
  const { timeoutMs, ...init } = options;
  const headers = { "X-Network-Session": SESSION_TOKEN, ...(init.headers || {}) };
  // 只有明确给了 timeoutMs 的请求才上 AbortController：环境安装、原生文件对话框
  // 这类动作本来就可以跑几分钟，不能一刀切超时。
  const controller = timeoutMs ? new AbortController() : null;
  const timer = controller
    ? window.setTimeout(() => controller.abort(), timeoutMs)
    : 0;
  try {
    const response = await fetch(path, {
      ...init,
      headers,
      cache: "no-store",
      ...(controller ? { signal: controller.signal } : {}),
    });
    if (!response.ok) {
      let message = `请求失败 (${response.status})`;
      try { message = (await response.json()).error || message; } catch (_error) { /* ignore */ }
      throw new Error(message);
    }
    return response;
  } catch (error) {
    if (controller && controller.signal.aborted) {
      throw new Error(`请求超时（${Math.round(timeoutMs / 1000)} 秒），请稍后查看状态`);
    }
    throw error;
  } finally {
    if (timer) window.clearTimeout(timer);
  }
}

async function invoke(method, ...args) {
  try {
    const response = await apiRequest("/api/call", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ method, args }),
    });
    const payload = await response.json();
    if (!payload.ok) throw new Error(payload.error || "操作失败");
    await refreshState();
    scheduleStateRefresh(statePollDelay());
    return payload.result;
  } catch (error) {
    showToast("error", error.message || "操作失败");
    return null;
  }
}

/**
 * 长动作专用：invoke 把错误吞成 null，安装失败时调用方照样会走「成功」分支，
 * 界面显示「安装完成」但卡片还是缺失。环境安装必须拿到真实错误。
 * 这里只保留 invoke 的状态刷新逻辑，错误一律往上抛。
 */
async function invokeStrict(method, ...args) {
  const response = await apiRequest("/api/call", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ method, args }),
  });
  const payload = await response.json();
  if (!payload.ok) throw new Error(payload.error || "操作失败");
  await refreshState();
  scheduleStateRefresh(statePollDelay());
  return payload.result;
}

async function performWindowAction(action) {
  return invoke("windowAction", action);
}

function renderActivePage() {
  if (!appState) return;
  if (currentPage === "overview") renderOverview();
  else if (currentPage === "rules") renderRules();
  else if (currentPage === "nodes") {
    renderSources();
    renderNodes();
    renderSubscriptions();
  } else if (currentPage === "servers") {
    // 同一个页面里三个标签：Linux（SSH 部署）、Windows（RDP 会话治理）、文件快传。
    // 只渲染当前标签那半边，另一半不做无谓的子进程调用。
    if (serverTab === "windows") {
      renderWinServerList();
      loadWinServerFixes();
    } else if (serverTab === "transfer") {
      renderXferServers();
    } else {
      renderSshServers();
    }
  }
  else if (currentPage === "settings") renderSettings();
  // 环境页绝不能挂在每轮状态轮询上：体检要起十几个子进程问版本号，
  // 停在这个页面就会每 2.5 秒全量重扫一次，无限循环地弹控制台黑框。
  // 改成只在切进页面时拉一次，见 commitPageSwitch()。
  else if (currentPage === "logs") refreshLogs();
}

function commitPageSwitch() {
  pageSwitchFrame = 0;
  const page = requestedPage;
  if (page === currentPage) return;
  currentPage = page;
  document.querySelectorAll(".nav-item").forEach((item) => {
    item.classList.toggle("is-active", item.dataset.page === page);
  });
  document.querySelectorAll(".page").forEach((item) => {
    item.classList.toggle("is-active", item.id === `page-${page}`);
  });
  byId("page-title").textContent = PAGE_TITLES[page];
  renderActivePage();
  // 体检只在真正切进环境页时跑一次，不跟随状态轮询。
  if (page === "env") refreshEnv();
  scheduleStateRefresh(0);
}

function setPage(page) {
  if (!PAGE_TITLES[page]) return;
  requestedPage = page;
  if (!pageSwitchFrame) pageSwitchFrame = window.requestAnimationFrame(commitPageSwitch);
}

function setNodeTab(tab) {
  currentNodeTab = tab;
  document.querySelectorAll(".tab").forEach((item) => {
    item.classList.toggle("is-active", item.dataset.nodeTab === tab);
  });
  document.querySelectorAll(".node-tab").forEach((item) => {
    item.classList.toggle("is-active", item.id === `node-tab-${tab}`);
  });
}

async function refreshState() {
  if (refreshing) return false;
  refreshing = true;
  try {
    const response = await apiRequest("/api/state");
    appState = await response.json();
    initialFailureCount = 0;
    renderState();
    document.body.classList.add("app-ready");
    window.networkManagerReady = true;
    (appState.toasts || []).forEach((item) => showToast(item.kind, item.message));
    return true;
  } catch (_error) {
    if (appState) {
      setText("overview-status-detail", "后台连接中断，正在重试");
      renderSignatures.delete("overview-core");
    } else {
      initialFailureCount += 1;
      setBootstrapStatus(
        initialFailureCount < 4 ? "后台正在启动，正在重试…" : "暂时无法连接后台，继续重试…",
        initialFailureCount >= 4,
      );
    }
    return false;
  } finally {
    refreshing = false;
  }
}

function statePollDelay() {
  if (!appState) return Math.min(2000, 150 * (2 ** Math.min(initialFailureCount, 4)));
  if (document.hidden) return 10000;
  const nodeTesting = (appState.nodes || []).some((node) => node.latencyStatus === "testing");
  const serverDeploying = (appState.sshServers || []).some(
    (server) => server.deployment?.status === "deploying",
  );
  if (appState.core?.busy || coreActionPending || nodeTesting || serverDeploying || pendingServerTasks.size) return 500;
  if (currentPage === "overview" && appState.core?.running) return 1000;
  if (currentPage === "logs") return 1500;
  return 2500;
}

function scheduleStateRefresh(delay = 0) {
  if (statePollTimer) window.clearTimeout(statePollTimer);
  statePollTimer = window.setTimeout(pollState, Math.max(0, delay));
}

async function pollState() {
  statePollTimer = 0;
  await refreshState();
  scheduleStateRefresh(statePollDelay());
}

function renderState() {
  if (!appState) return;
  applyCapabilities();
  renderChrome();
  renderActivePage();
  // 环境任务的进度跟着已有的状态轮询走，不另开常驻请求。
  renderEnvTask(appState.envTask);
}

function applyCapabilities() {
  const capabilities = appState.capabilities || {};
  const sshDeployment = capabilities.sshDeployment !== false;
  if (!shouldRender("capabilities", { capabilities, version: appState.version })) return;
  byId("server-deployment-nav").classList.toggle("is-hidden", !sshDeployment);
  byId("page-servers").classList.toggle("is-hidden", !sshDeployment);
  byId("overview-ssh-row").classList.toggle("is-hidden", !sshDeployment);
  document.querySelectorAll(".desktop-only-setting").forEach((row) => {
    row.classList.toggle("is-hidden", Boolean(capabilities.headless));
  });
  document.querySelectorAll(".ssh-deployment-setting").forEach((section) => {
    section.classList.toggle("is-hidden", !sshDeployment);
  });
  byId("open-logs").classList.toggle("is-hidden", Boolean(capabilities.headless));
  byId("sidebar-version").textContent = `v${appState.version || ""} · ${capabilities.headless ? "Linux WebGUI" : "WebGUI"}`;
  if (!sshDeployment && currentPage === "servers") setPage("overview");
}

function renderChrome() {
  const { core, traffic } = appState;
  const badge = byId("header-status");
  setText(badge, core.status);
  const badgeClass = `status-badge${core.running ? " is-running" : ""}${core.busy ? " is-busy" : ""}`;
  if (badge.className !== badgeClass) badge.className = badgeClass;
  renderSidebarRate("upload", traffic.uploadRate);
  renderSidebarRate("download", traffic.downloadRate);
  setText("sidebar-memory", traffic.memoryMb);
  renderSidebarTrafficChart(traffic.downloadSamples, traffic.uploadSamples);
}

function openBrowserTextFile(accept, handler) {
  const input = document.createElement("input");
  input.type = "file";
  input.accept = accept;
  input.className = "is-hidden";
  input.addEventListener("change", async () => {
    const file = input.files?.[0];
    input.remove();
    if (!file) return;
    try {
      await handler(file, await file.text());
    } catch (error) {
      showToast("error", error.message || "读取文件失败");
    }
  }, { once: true });
  document.body.appendChild(input);
  input.click();
}

function downloadTextFile(name, content, contentType = "application/json") {
  const blob = new Blob([content], { type: `${contentType};charset=utf-8` });
  const url = URL.createObjectURL(blob);
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  document.body.appendChild(link);
  link.click();
  link.remove();
  window.setTimeout(() => URL.revokeObjectURL(url), 1000);
}

// 写入剪贴板并返回是否成功，调用方据此提示用户，失败时不再静默。
async function copyText(content) {
  try {
    if (navigator.clipboard?.writeText) {
      await navigator.clipboard.writeText(content);
      return true;
    }
  } catch (_error) { /* 回落到下面的临时文本框 */ }
  const area = document.createElement("textarea");
  area.value = content;
  area.style.position = "fixed";
  area.style.opacity = "0";
  document.body.appendChild(area);
  area.select();
  let copied = false;
  try {
    copied = document.execCommand("copy");
  } catch (_error) { copied = false; /* 失败交给调用方提示 */ }
  area.remove();
  return copied;
}

// 把后端返回的节点配置拼成 IP:端口:账号:密码 的普通格式。
// 服务器部署生成的是 Shadowsocks 节点，没有用户名，账号位即加密方式
// （与 ss:// 链接里 base64(加密方式:密码) 的前半段一致），兼容带 username 的节点。
function nodePlainText(node) {
  const host = typeof node.server === "string" ? node.server : "";
  const account = node.username || node.cipher || "";
  const password = typeof node.password === "string" ? node.password : "";
  return `${host}:${node.port ?? ""}:${account}:${password}`;
}

function renderSidebarRate(direction, rate) {
  const parts = String(rate || "0 B/s").trim().split(/\s+/);
  const rawAmount = parts[0] || "0";
  setText(`sidebar-${direction}`, Number(rawAmount) === 0 ? "0.00" : rawAmount);
  setText(`sidebar-${direction}-unit`, parts.slice(1).join(" ") || "B/s");
}

function renderSidebarTrafficChart(downloadValues, uploadValues) {
  const downloads = normalizedSamples(downloadValues);
  const uploads = normalizedSamples(uploadValues);
  if (!shouldRender("sidebar-traffic-chart", { downloads, uploads })) return;
  const peak = Math.max(1024, ...downloads, ...uploads);
  const width = 200;
  const height = 76;
  const padding = 3;
  const chartPath = (values) => values.map((value, index) => {
    const x = (index / Math.max(1, values.length - 1)) * width;
    const y = padding + (height - padding * 2) * (1 - value / peak);
    return `${index === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`;
  }).join(" ");
  byId("sidebar-traffic-chart").innerHTML = `
    <svg viewBox="0 0 ${width} ${height}" preserveAspectRatio="none" role="img" aria-label="实时上传和下载流量曲线">
      <line class="sidebar-chart-grid" x1="0" y1="3" x2="${width}" y2="3"></line>
      <line class="sidebar-chart-grid" x1="0" y1="${height / 2}" x2="${width}" y2="${height / 2}"></line>
      <line class="sidebar-chart-grid" x1="0" y1="${height - 3}" x2="${width}" y2="${height - 3}"></line>
      <path class="sidebar-chart-line upload" d="${chartPath(uploads)}"></path>
      <path class="sidebar-chart-line download" d="${chartPath(downloads)}"></path>
    </svg>`;
}

function renderOverview() {
  const { core, summary, sources, traffic } = appState;
  // 旧逻辑要求「先观察到后端 busy=true，再看到它变 false」才解锁按钮。可是
  // toggleCore 过去在 state 锁里同步执行，期间 /api/state 全部排队，前端根本
  // 没机会看到 busy=true —— 按钮就这样永久停在「处理中」。现在改由请求本身
  // 收尾（见 toggleCoreFromUi），这里只留一道时间兜底，防止请求彻底失联。
  if (coreActionPending && Date.now() - coreActionStartedAt > CORE_ACTION_MAX_WAIT_MS) {
    releaseCoreAction();
  }
  if (shouldRender("overview-core", {
    admin: core.admin,
    running: core.running,
    busy: core.busy,
    mode: core.mode,
    modeLabel: core.modeLabel,
    mixedPort: core.mixedPort,
    pending: coreActionPending,
    hasNodes: appState.nodes.length > 0,
  })) {
    byId("admin-banner").classList.toggle("is-hidden", core.admin);
    setText("overview-status-title", core.running ? "全流量接管运行中" : "全流量接管已停止");
    setText("overview-status-detail", core.running
      ? `${core.modeLabel} · 本地入口 127.0.0.1:${core.mixedPort}`
      : "当前不会修改系统路由");

    const toggle = byId("core-toggle");
    toggle.disabled = core.busy || coreActionPending;
    toggle.className = `button large ${core.running ? "secondary" : "primary"}`;
    toggle.innerHTML = (core.busy || coreActionPending)
      ? `${icon("refresh-cw")}<span>处理中</span>`
      : core.running
        ? `${icon("square")}<span>停止接管</span>`
        : `${icon("play")}<span>启动接管</span>`;

    byId("mode-switch").innerHTML = MODE_META.map(([mode, label]) => {
      const disabled = ["GLOBAL_BUILTIN", "SMART"].includes(mode) && appState.nodes.length === 0;
      return `<button class="segment${core.mode === mode ? " is-active" : ""}" data-mode="${mode}"${disabled ? " disabled" : ""}>${label}</button>`;
    }).join("");
    const modeMeta = MODE_META.find(([mode]) => mode === core.mode);
    setText("mode-description", modeMeta ? modeMeta[2] : core.modeLabel);
  }

  if (shouldRender("overview-summary", { summary, sources })) {
    renderSourceStatus("clash", sources.clash);
    renderSourceStatus("v2ray", sources.v2ray);
    renderSourceStatus("ssh", sources.ssh);
    setText("summary-process", summary.processRules);
    setText("summary-network", summary.networkRules);
    setText("summary-nodes", summary.nodes);
    setText("summary-default", summary.defaultTarget);
  }

  setText("traffic-status", traffic.status);
  byId("traffic-status").classList.toggle("is-running", core.running);
  setText("traffic-download", traffic.downloadRate);
  setText("traffic-upload", traffic.uploadRate);
  setText("traffic-connections", traffic.connections);
  setText("traffic-download-total", traffic.downloadTotal);
  setText("traffic-upload-total", traffic.uploadTotal);
  renderTrafficChart(traffic.downloadSamples, traffic.uploadSamples);
  // 「尚未检测」「内核未运行」是灰的占位态，检测失败要能一眼看出是红的。
  // 归属地查不到就留空，不硬凑。
  const renderExit = (ipId, locId, ip, location) => {
    const text = ip || "尚未检测";
    const el = byId(ipId);
    setText(ipId, text);
    el.classList.toggle("pending", text === "尚未检测" || text === "内核未运行");
    el.classList.toggle("failed", text === "检测失败");
    setText(locId, text === "尚未检测" || text === "检测失败" ? "" : location || "");
    return text;
  };
  const localIp = renderExit("exit-ip-local", "exit-loc-local", appState.localIp, appState.localIpLocation);
  const proxyIp = renderExit("exit-ip-proxy", "exit-loc-proxy", appState.exitIp, appState.exitIpLocation);
  setText("exit-ip", localIp === "尚未检测" && proxyIp === "尚未检测" ? "尚未检测" : "出口已检测");
}

function releaseCoreAction() {
  coreActionPending = false;
  coreActionStartedAt = 0;
  // 按钮内容在 shouldRender 里按签名缓存，状态没变就不会重画，必须主动失效。
  renderSignatures.delete("overview-core");
}

async function toggleCoreFromUi() {
  if (coreActionPending || appState?.core?.busy) return;
  coreActionPending = true;
  coreActionStartedAt = Date.now();
  const toggle = byId("core-toggle");
  toggle.disabled = true;
  toggle.innerHTML = `${icon("refresh-cw")}<span>处理中</span>`;
  try {
    // toggleCore 是同步 API：请求返回时后端动作已经结束（成功或失败都一样）。
    // 自己收尾，不再依赖「轮询碰巧撞见 busy 由 true 变 false」。
    const response = await apiRequest("/api/call", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ method: "toggleCore", args: [] }),
      timeoutMs: CORE_ACTION_MAX_WAIT_MS,
    });
    const payload = await response.json();
    if (!payload.ok) throw new Error(payload.error || "接管操作失败");
  } catch (error) {
    showToast("error", error.message || "接管操作失败");
  } finally {
    releaseCoreAction();
    // 无论成败都拿一次最新状态：成功切按钮，失败也把真实的 running/busy 画回来。
    await refreshState();
    renderOverview();
    scheduleStateRefresh(statePollDelay());
  }
}

function renderSourceStatus(key, source) {
  byId(`overview-${key}-endpoint`).textContent = source.endpoint;
  const status = byId(`overview-${key}-status`);
  status.textContent = source.status;
  status.className = "source-status";
  if (source.available) status.classList.add("is-ok");
  if (source.enabled && !source.available && !source.status.includes("检测")) status.classList.add("is-error");
}

function renderTrafficChart(downloadValues, uploadValues) {
  const downloads = normalizedSamples(downloadValues);
  const uploads = normalizedSamples(uploadValues);
  if (!shouldRender("traffic-chart", { downloads, uploads })) return;
  const peak = Math.max(1024, ...downloads, ...uploads);
  const width = 1000;
  const height = 185;
  const top = 18;
  const right = 12;
  const bottom = 24;
  const left = 54;
  const plotWidth = width - left - right;
  const plotHeight = height - top - bottom;
  const path = (values) => values.map((value, index) => {
    const x = left + (index / Math.max(1, values.length - 1)) * plotWidth;
    const y = top + plotHeight - (value / peak) * plotHeight;
    return `${index === 0 ? "M" : "L"}${x.toFixed(1)},${y.toFixed(1)}`;
  }).join(" ");
  const grid = [0, .5, 1].map((ratio) => {
    const y = top + plotHeight * ratio;
    const label = formatBytes(peak * (1 - ratio), true);
    return `<line class="chart-grid" x1="${left}" y1="${y}" x2="${width - right}" y2="${y}"></line><text class="chart-axis" x="2" y="${y + 4}">${label}</text>`;
  }).join("");
  byId("traffic-chart").innerHTML = `<svg viewBox="0 0 ${width} ${height}" preserveAspectRatio="none" role="img" aria-label="实时上传和下载流量曲线">${grid}<path class="chart-line download" d="${path(downloads)}"></path><path class="chart-line upload" d="${path(uploads)}"></path><text class="chart-axis" x="${left}" y="${height - 5}">60 秒前</text><text class="chart-axis" x="${width - 32}" y="${height - 5}">现在</text></svg>`;
}

function normalizedSamples(values) {
  const samples = (Array.isArray(values) ? values : []).map((value) => Math.max(0, Number(value) || 0));
  while (samples.length < 60) samples.unshift(0);
  return samples.slice(-60);
}

function formatBytes(value, rate = false) {
  const units = ["B", "KB", "MB", "GB"];
  let amount = Math.max(0, Number(value) || 0);
  let index = 0;
  while (amount >= 1024 && index < units.length - 1) {
    amount /= 1024;
    index += 1;
  }
  const precision = amount >= 100 || index === 0 ? 0 : amount >= 10 ? 1 : 2;
  return `${amount.toFixed(precision)} ${units[index]}${rate ? "/s" : ""}`;
}

function formatTime(value) {
  if (!value || value === "未更新") return "未更新";
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return value;
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
    hour12: false,
  }).format(parsed);
}

function renderRules() {
  const rules = appState.rules;
  if (!shouldRender("rules", { rules, fallbackRule: appState.fallbackRule })) return;
  byId("rules-empty").classList.add("is-hidden");
  const regularRows = rules.map((rule) => {
    const statusClass = rule.partiallyEnabled ? " is-partial" : rule.enabled ? " is-enabled" : "";
    const statusText = rule.partiallyEnabled ? "部分启用" : rule.enabled ? "启用" : "停用";
    if (rule.kind === "group") {
      return `<tr class="rule-group-row">
        <td><span class="rule-status${statusClass}">${statusText}</span></td>
        <td><span class="group-label">${escapeHtml(rule.ruleTypeLabel)}</span></td>
        <td title="${escapeHtml(rule.detail)}"><strong>${escapeHtml(rule.note)}</strong><small>${rule.count} 条匹配</small></td>
        <td><span class="chip">${escapeHtml(rule.targetLabel)}</span></td>
        <td>整组共用当前出口 · ${escapeHtml(rule.note)}</td>
        <td class="actions">
          <button class="mini-button" data-rule-action="view" data-rule-group="${rule.groupId}" title="查看全部域名">${icon("file-text")}</button>
          <button class="mini-button" data-rule-action="toggle" data-rule-group="${rule.groupId}" title="${rule.enabled ? "停用整组" : "启用整组"}">${icon(rule.enabled ? "square" : "check")}</button>
          <button class="mini-button" data-rule-action="edit" data-rule-group="${rule.groupId}" title="切换整组出口">${icon("square-pen")}</button>
          <button class="mini-button danger" data-rule-action="delete" data-rule-group="${rule.groupId}" title="删除整组">${icon("trash-2")}</button>
        </td>
      </tr>`;
    }
    return `<tr>
      <td><span class="rule-status${statusClass}">${statusText}</span></td>
      <td title="${escapeHtml(rule.ruleType)}">${escapeHtml(rule.ruleTypeLabel)}</td>
      <td title="${escapeHtml(rule.value)}"><button type="button" class="rule-value-edit" data-rule-action="edit" data-index="${rule.index}" aria-label="编辑匹配内容"><span>${escapeHtml(rule.value)}</span>${icon("square-pen")}</button></td>
      <td><span class="chip">${escapeHtml(rule.targetLabel)}</span></td>
      <td title="${escapeHtml(rule.note)}">${escapeHtml(rule.note || "-")}</td>
      <td class="actions">
        <button class="mini-button" data-rule-action="toggle" data-index="${rule.index}" title="${rule.enabled ? "停用" : "启用"}">${icon(rule.enabled ? "square" : "check")}</button>
        <button class="mini-button" data-rule-action="edit" data-index="${rule.index}" title="编辑">${icon("square-pen")}</button>
        <button class="mini-button" data-rule-action="up" data-index="${rule.index}" title="上移">${icon("arrow-up")}</button>
        <button class="mini-button" data-rule-action="down" data-index="${rule.index}" title="下移">${icon("arrow-down")}</button>
        <button class="mini-button danger" data-rule-action="delete" data-index="${rule.index}" title="删除">${icon("trash-2")}</button>
      </td>
    </tr>`;
  }).join("");
  const lanBypassRow = `<tr class="system-rule-row">
    <td><span class="rule-status is-enabled">强制启用</span></td>
    <td><span class="fallback-label">系统直连</span></td>
    <td><strong>内网与局域网</strong></td>
    <td><span class="chip">直连</span></td>
    <td>优先匹配，不受保底出口影响</td>
    <td class="actions"></td>
  </tr>`;
  const fallback = appState.fallbackRule || { target: "DIRECT", targetLabel: "直连" };
  const fallbackRow = `<tr class="fallback-rule-row">
    <td><span class="rule-status is-enabled">强制启用</span></td>
    <td><span class="fallback-label">强制保底</span></td>
    <td><strong>其他未匹配流量</strong></td>
    <td><span class="chip">${escapeHtml(fallback.targetLabel)}</span></td>
    <td>始终位于规则末尾</td>
    <td class="actions"><button class="mini-button" data-fallback-action="edit" title="修改保底出口">${icon("square-pen")}</button></td>
  </tr>`;
  byId("rules-body").innerHTML = lanBypassRow + regularRows + fallbackRow;
}

function renderSources() {
  const sources = appState.sources;
  const signature = JSON.stringify(sources);
  if (signature === sourceSignature) return;
  if (byId("source-editor").contains(document.activeElement)) return;
  sourceSignature = signature;
  const root = byId("source-editor");
  root.innerHTML = ["clash", "v2ray"].map((key) => {
    const source = sources[key];
    const label = key === "clash" ? "Clash" : "v2ray";
    return `<section class="source-card">
      <div class="source-card-head">
        <div>${icon("plug-zap")}<h3>${label}</h3></div>
        <label class="switch-row"><input id="source-${key}-enabled" type="checkbox"${source.enabled ? " checked" : ""}><i></i></label>
      </div>
      <div class="form-grid">
        <label><span>监听地址</span><input id="source-${key}-host" value="${escapeHtml(source.host)}"></label>
        <label><span>端口</span><input id="source-${key}-port" type="number" min="1" max="65535" value="${source.port}"></label>
        <label><span>协议</span><select id="source-${key}-protocol"><option value="socks5"${source.protocol === "socks5" ? " selected" : ""}>SOCKS5</option><option value="http"${source.protocol === "http" ? " selected" : ""}>HTTP</option></select></label>
      </div>
      <div class="subscription-meta"><span class="${source.available ? "download-color" : ""}">${escapeHtml(source.status)}</span><button class="mini-button" data-source-test="${key}" title="检测端口">${icon("refresh-cw")}</button></div>
    </section>`;
  }).join("");
  enhanceSelects(root);
}

function renderNodes() {
  const sourceNodes = appState.nodes;
  if (!shouldRender("nodes", {
    nodes: sourceNodes,
    nodeGroups: appState.nodeGroups,
    coreRunning: appState.core.running,
    collapsed: [...collapsedNodeGroups].sort(),
  })) return;
  const testing = sourceNodes.some((node) => node.latencyStatus === "testing");
  const nodes = testing ? sourceNodes : [...sourceNodes].sort(compareNodeLatency);
  const testButton = byId("test-all-nodes");
  // 测速是直接 TCP 握手，不需要内核在跑，别因为这个把按钮灰掉。
  testButton.disabled = nodes.length === 0 || testing;
  testButton.querySelector("span:last-child").textContent = testing ? "测速中" : "批量测速";
  const errorCount = sourceNodes.filter((node) => node.latencyStatus === "error").length;
  const deleteErrorButton = byId("delete-error-nodes");
  deleteErrorButton.disabled = errorCount === 0 || testing;
  deleteErrorButton.querySelector("span:last-child").textContent = errorCount
    ? `删除 Error (${errorCount})`
    : "删除 Error";
  byId("nodes-empty").classList.toggle(
    "is-hidden", nodes.length > 0 || (appState.nodeGroups || []).length > 0,
  );
  const grouped = new Map();
  nodes.forEach((node) => {
    const groupName = node.group || node.source || "手动导入";
    if (!grouped.has(groupName)) grouped.set(groupName, []);
    grouped.get(groupName).push(node);
  });
  const groupOrder = [...new Set([...(appState.nodeGroups || []), ...grouped.keys()])];
  byId("node-grid").innerHTML = groupOrder.map((groupName) => {
    const groupNodes = grouped.get(groupName) || [];
    const groupErrors = groupNodes.filter((node) => node.latencyStatus === "error").length;
    const custom = (appState.nodeGroups || []).includes(groupName);
    const collapsed = collapsedNodeGroups.has(groupName);
    const cards = groupNodes.map((node) => {
      const latencyText = node.latencyStatus === "testing"
        ? "测试中"
        : node.latencyStatus === "error"
          ? "Error"
          : node.latencyStatus === "ok"
            ? `${node.latency} ms`
            : "--";
      const latencyLevel = node.latencyStatus !== "ok"
        ? node.latencyStatus
        : node.latency < 300
          ? "fast"
          : node.latency < 900
            ? "medium"
            : "slow";
      return `<article class="node-card${node.selected ? " is-selected" : ""}" data-node-select="${escapeHtml(node.name)}" tabindex="0" role="button" aria-label="选择节点 ${escapeHtml(node.name)}">
        <div class="node-card-head">
          <strong title="${escapeHtml(node.name)}">${escapeHtml(node.name)}</strong>
          <div class="node-card-actions">
            <span class="node-latency ${latencyLevel}" title="${escapeHtml(node.latencyMessage || latencyText)}">${latencyText}</span>
            <button class="mini-button" data-node-test="${node.index}" title="测试该节点" aria-label="测试该节点"${node.latencyStatus === "testing" ? " disabled" : ""}>${icon("wifi")}</button>
            <button class="mini-button" data-node-group="${escapeHtml(node.name)}" title="移动到分组" aria-label="移动到分组">${icon("folder-open")}</button>
            <button class="mini-button danger" data-node-delete="${node.index}" title="删除节点" aria-label="删除节点">${icon("trash-2")}</button>
          </div>
        </div>
        <div class="node-card-meta"><span class="node-badge">${escapeHtml(node.protocol)}</span>${node.region ? `<span class="node-badge region">${escapeHtml(node.region)}</span>` : ""}${node.isRelay ? `<span class="node-badge chain" title="当前作为统一中转使用">${icon("link")}中转</span>` : ""}${node.viaRelay ? `<span class="node-badge chain is-via" title="该节点的连接与测速都会先经过统一中转">${icon("link")}经中转</span>` : ""}</div>
        <div class="node-card-foot">
          <span title="${escapeHtml(node.source)}">${escapeHtml(node.source || "手动导入")}</span>
          <span title="${escapeHtml(node.server)}">${escapeHtml(node.server)}</span>
        </div>
      </article>`;
    }).join("");
    return `<section class="node-group-section${collapsed ? " is-collapsed" : ""}">
      <button type="button" class="node-group-head" data-node-group-toggle="${escapeHtml(groupName)}" aria-expanded="${collapsed ? "false" : "true"}" title="${collapsed ? "展开" : "收起"}${escapeHtml(groupName)}">
        ${icon("folder-open")}
        <strong title="${escapeHtml(groupName)}">${escapeHtml(groupName)}</strong>
        <span>${custom ? "自定义" : "按来源"} · ${groupNodes.length} 个节点</span>
        ${groupErrors ? `<span class="node-group-error">${groupErrors} 个异常</span>` : ""}
        <span class="node-group-chevron" aria-hidden="true">${icon(collapsed ? "chevron-right" : "chevron-down")}</span>
      </button>
      <div class="node-group-grid"${collapsed ? " hidden" : ""}>${cards}</div>
    </section>`;
  }).join("");
}

function compareNodeLatency(left, right) {
  const rank = (node) => {
    if (node.latencyStatus === "ok" && Number.isFinite(Number(node.latency))) {
      return [0, Number(node.latency)];
    }
    if (node.latencyStatus === "idle") return [1, Number.POSITIVE_INFINITY];
    return [2, Number.POSITIVE_INFINITY];
  };
  const [leftGroup, leftDelay] = rank(left);
  const [rightGroup, rightDelay] = rank(right);
  return leftGroup - rightGroup || leftDelay - rightDelay || left.index - right.index;
}

function saveSourcesFromForm() {
  const source = (key) => ({
    enabled: byId(`source-${key}-enabled`).checked,
    host: byId(`source-${key}-host`).value,
    port: Number(byId(`source-${key}-port`).value),
    protocol: byId(`source-${key}-protocol`).value,
  });
  invoke("saveSources", JSON.stringify({ clash: source("clash"), v2ray: source("v2ray") }));
}

function renderSubscriptions() {
  const sources = appState.subscriptions;
  if (!shouldRender("subscriptions", { sources, nodeGroups: appState.nodeGroups })) return;
  syncSubscriptionGroupSelect();
  byId("subscriptions-empty").classList.toggle("is-hidden", sources.length > 0);
  byId("subscription-grid").innerHTML = sources.map((source) => `
    <article class="subscription-card">
      <div class="subscription-card-head">
        <div><h3 title="${escapeHtml(source.name)}">${escapeHtml(source.name)}</h3><p title="${escapeHtml(source.host)}">${escapeHtml(source.host)}</p></div>
        <div class="subscription-actions">
          <button class="mini-button" data-subscription-action="refresh" data-index="${source.index}" title="刷新">${icon("refresh-cw")}</button>
          <button class="mini-button danger" data-subscription-action="delete" data-index="${source.index}" title="删除">${icon("trash-2")}</button>
        </div>
      </div>
      <div class="subscription-meta"><span>${source.nodeCount} 个节点${source.group ? ` · ${escapeHtml(source.group)}` : ""}</span><span>${escapeHtml(formatTime(source.lastUpdated))}</span></div>
    </article>`).join("");
}

function syncSubscriptionGroupSelect() {
  const select = byId("subscription-group");
  const groups = appState.nodeGroups || [];
  const signature = JSON.stringify(groups);
  if (select.dataset.groupSignature === signature) return;
  const selected = groups.includes(select.value) ? select.value : "";
  select.innerHTML = [
    '<option value="">按订阅来源归类</option>',
    ...groups.map((group) => `<option value="${escapeHtml(group)}">${escapeHtml(group)}</option>`),
  ].join("");
  select.value = selected;
  select.dataset.groupSignature = signature;
  rebuildSelectEnhancement(select);
}

function renderSshServers() {
  // 只列 Linux 侧的。Windows 服务器走 SSH 但管的是 RDP 会话，
  // 跟「往机器上装代理」完全是两回事，混在一张表里会让人点错。
  const servers = linuxServers();
  // 选中项只保留仍然存在的服务器，避免删除后残留
  const alive = new Set(servers.map((server) => server.profileId));
  sshSelection.forEach((id) => { if (!alive.has(id)) sshSelection.delete(id); });
  if (!shouldRender("ssh-servers", { servers, pending: [...pendingServerTasks].sort() })) {
    updateSshSelectionUi();
    return;
  }
  byId("ssh-servers-empty").classList.toggle("is-hidden", servers.length > 0);
  const busyOf = (server) => server.deployment?.status === "deploying" || pendingServerTasks.has(server.profileId);
  const deploying = servers.find(busyOf);
  const deployedCount = servers.filter((server) => server.deployed).length;
  byId("ssh-status-title").textContent = deploying
    ? deploying.deployed ? "正在检查远端代理服务" : "正在部署服务器代理"
    : "远端代理部署";
  byId("ssh-status-detail").textContent = deploying
    ? deploying.deployment?.stage || "正在执行远端配置"
    : `${deployedCount} 个服务器节点已就绪；SSH 无需保持连接`;
  const authLabels = { password: "密码", key: "私钥", agent: "SSH Agent" };
  byId("ssh-server-body").innerHTML = servers.map((server) => {
    const task = server.deployment || { status: "idle", stage: "", error: "" };
    const isDeploying = task.status === "deploying" || pendingServerTasks.has(server.profileId);
    const hasError = task.status === "error";
    const portWarning = server.proxyPortWarning || "";
    const hasWarning = task.status === "warning" || Boolean(portWarning);
    // 已部署的那次动作是「检查服务」，状态就该写「检查中」，别一律叫部署中。
    const statusText = isDeploying ? (server.deployed ? "检查中" : "部署中") : hasError ? "部署失败" : portWarning ? "端口需调整" : hasWarning ? "外端口未开放" : server.deployed ? "已部署" : "未部署";
    const statusClass = hasError ? " is-error" : hasWarning ? " is-warning" : isDeploying ? " is-busy" : server.deployed ? " is-running" : "";
    const detail = portWarning || (isDeploying ? task.stage || (server.deployed ? "正在检查远端代理服务" : "正在执行远端配置") : hasError || hasWarning ? task.error || task.stage : server.deployedVersion || "等待部署");
    const selected = sshSelection.has(server.profileId) ? " checked" : "";
    return `<tr data-profile-id="${server.profileId}"${sshSelection.has(server.profileId) ? ' class="is-selected"' : ""}>
      <td class="cell-select"><input type="checkbox" data-ssh-select="${server.profileId}" aria-label="选择 ${escapeHtml(server.name)}"${selected}></td>
      <td title="${escapeHtml(server.name)}"><strong>${escapeHtml(server.name)}</strong>${server.region ? `<small>${escapeHtml(server.region)}</small>` : ""}</td>
      <td title="${escapeHtml(server.username)}@${escapeHtml(server.host)}:${server.port}">${escapeHtml(server.username)}@${escapeHtml(server.host)}:${server.port}</td>
      <td>${authLabels[server.authMethod] || server.authMethod}</td>
      <td title="${escapeHtml(server.host)}:${server.proxyPort}">${escapeHtml(server.host)}:${server.proxyPort}</td>
      <td><span class="small-status${statusClass}" title="${escapeHtml(detail)}">${statusText}</span></td>
      <td class="actions">
        <button class="button primary compact-button" data-ssh-action="deploy" data-profile-id="${server.profileId}"${isDeploying || Boolean(deploying) ? " disabled" : ""}>${icon(isDeploying || server.deployed ? "refresh-cw" : "hard-drive-download")}<span>${isDeploying ? (server.deployed ? "检查中" : "部署中") : server.deployed ? "检查服务" : "部署代理"}</span></button>
        <button class="icon-button" data-ssh-action="copy" data-profile-id="${server.profileId}"${server.shareLink ? "" : " disabled"} title="复制节点（IP:端口:账号:密码）" aria-label="复制节点">${icon("link")}</button>
        <button class="icon-button" data-ssh-action="edit" data-profile-id="${server.profileId}" title="编辑" aria-label="编辑">${icon("square-pen")}</button>
        ${server.hasCredential ? `<button class="icon-button" data-ssh-action="forget" data-profile-id="${server.profileId}" title="清除已保存的密码" aria-label="清除已保存的密码">${icon("key-round")}</button>` : ""}
        <button class="icon-button danger" data-ssh-action="delete" data-profile-id="${server.profileId}" title="删除" aria-label="删除">${icon("trash-2")}</button>
      </td>
    </tr>`;
  }).join("");
  updateSshSelectionUi();
}

function selectedSshServers() {
  return linuxServers().filter((server) => sshSelection.has(server.profileId));
}

function updateSshSelectionUi() {
  const servers = linuxServers();
  const picked = selectedSshServers();
  const count = picked.length;
  const label = byId("ssh-selection-count");
  if (label) label.textContent = count ? `已选择 ${count} / ${servers.length} 台服务器` : "未选择服务器";
  byId("batch-deploy-ssh").disabled = count === 0;
  byId("batch-copy-ssh").disabled = count === 0 || !picked.some((server) => server.shareLink);
  byId("batch-delete-ssh").disabled = count === 0;
  const all = byId("ssh-select-all");
  if (all) {
    all.checked = servers.length > 0 && count === servers.length;
    all.indeterminate = count > 0 && count < servers.length;
    all.disabled = servers.length === 0;
  }
  byId("ssh-server-body")
    .querySelectorAll("tr[data-profile-id]")
    .forEach((row) => row.classList.toggle("is-selected", sshSelection.has(row.dataset.profileId)));
}

function renderSshSelectionOnly() {
  updateSshSelectionUi();
}

function renderSettings() {
  // 证书卡片不受 settingsInitialized 影响：信任状态、覆盖的名字都可能在别处变化
  // （保存映射、装了证书），要跟着每轮状态刷新。
  renderLocalCert();
  renderRelay();
  if (settingsInitialized || byId("settings-form").contains(document.activeElement)) return;
  const settings = appState.settings;
  byId("setting-mixed-port").value = settings.mixedPort;
  byId("setting-controller-port").value = settings.controllerPort;
  byId("setting-dns-port").value = settings.dnsPort;
  byId("setting-server-proxy-port").value = settings.serverProxyPort || DEFAULT_SERVER_PROXY_PORT;
  byId("setting-strict-route").checked = settings.strictRoute;
  // 兼容模式是「非默认值才勾选」：老配置里没有 dnsMode 字段时按 fake-ip 处理。
  byId("setting-dns-compat").checked = settings.dnsMode === "redir-host";
  byId("setting-start-on-launch").checked = settings.startOnLaunch;
  byId("setting-close-to-tray").checked = settings.closeToTray;
  byId("setting-start-with-windows").checked = settings.startWithWindows;
  byId("setting-local-dns").value =
    settings.localDnsText || localDnsTextFromEntries(settings.localDns || []);
  updateLocalDnsSummary();
  settingsInitialized = true;
}

/** 统一中转：整份配置只有一个中转节点，改完立刻生效，不用等「保存设置」。 */
function renderRelay() {
  const relay = appState.relay || { enabled: false, node: "", active: false };
  const select = byId("setting-relay-node");
  const enabled = byId("setting-relay-enabled");
  if (!select || !enabled) return;
  // 用户正开着这个下拉框时别重建选项，否则选到一半会被打断。
  if (select === document.activeElement) return;
  const nodes = appState.nodes || [];
  const options = [
    `<option value=""${relay.node ? "" : " selected"}>未选择</option>`,
    ...(!relay.node || nodes.some((node) => node.name === relay.node)
      ? []
      : [`<option value="${escapeHtml(relay.node)}" selected>${escapeHtml(relay.node)}（已不在节点列表）</option>`]),
    ...nodes.map((node) => `<option value="${escapeHtml(node.name)}"${node.name === relay.node ? " selected" : ""}>${escapeHtml(node.name)} · ${escapeHtml(node.protocol)}</option>`),
  ].join("");
  select.innerHTML = options;
  // 下拉始终可选：开关和选择是两回事，锁住任何一个都会把另一个卡死
  // （先开开关要求已选节点、不选节点又开不了开关）。
  enabled.checked = relay.enabled;
  const hint = byId("setting-relay-hint");
  if (hint) {
    hint.textContent = !relay.enabled
      ? "未启用，节点全部直接连接。"
      : !relay.node
        ? "已启用，但还没选中转节点，暂时不生效。"
        : relay.active
          ? "已生效：所有出站（含节点测速）都先经过它；本机端口类节点除外。"
          : "已启用，但所选节点不在节点列表里，请重新选择。";
    hint.classList.toggle("is-warning", relay.enabled && !relay.active);
  }
}

/** 已经落盘的映射用 Rust 回写的文本表示；界面只做兜底拼接。 */
function localDnsTextFromEntries(entries) {
  return entries.map((entry) => `${entry.domain} ${entry.address}`).join("\n");
}

/** 摘要只数「去掉注释后还有内容的行」，和真正保存时的解析口径一致。 */
function localDnsLineCount(text) {
  return text
    .split("\n")
    .map((line) => line.split("#")[0].trim())
    .filter(Boolean).length;
}

function updateLocalDnsSummary(text) {
  const summary = byId("setting-local-dns-summary");
  if (!summary) return;
  const dirty = text !== undefined;
  const value = dirty ? text : byId("setting-local-dns").value;
  const count = localDnsLineCount(value);
  summary.textContent = count ? `共 ${count} 条映射${dirty ? "（未保存）" : ""}` : "当前没有映射";
  summary.classList.toggle("has-warning", dirty && count > 0);
}

/** 指纹太长，卡片里只露前 6 组字节，够用来和证书管理器里看到的那串对上。 */
function shortFingerprint(value) {
  if (!value) return "";
  return `${value.split(":").slice(0, 6).join(":")}…`;
}

function setLocalCertLine(element, text, tone) {
  if (!element) return;
  element.textContent = text;
  element.classList.toggle("is-ok", tone === "ok");
  element.classList.toggle("is-warn", tone === "warn");
}

/**
 * 本地证书卡片。状态全部来自 Rust 侧（含带缓存的信任查询），这里只负责措辞 ——
 * 值的判断逻辑放在 Rust 里，桌面端和 Android 端才不会各说各话。
 */
function renderLocalCert() {
  const cert = appState && appState.settings ? appState.settings.localCert : null;
  const ca = byId("local-cert-ca");
  const leaf = byId("local-cert-leaf");
  const trust = byId("local-cert-trust");
  const dir = byId("local-cert-dir");
  if (!ca || !leaf || !trust || !dir) return;

  if (!cert) {
    setLocalCertLine(ca, "无法读取证书状态", "warn");
    setLocalCertLine(leaf, "—");
    setLocalCertLine(trust, "—");
    dir.textContent = "";
    return;
  }

  setLocalCertLine(
    ca,
    cert.caExists
      ? `已生成 · 指纹 ${shortFingerprint(cert.caFingerprint)} · 有效期 ${cert.caValidityDays} 天`
      : "尚未生成",
    cert.caExists ? null : "warn",
  );

  if (!cert.leafExists) {
    setLocalCertLine(leaf, "尚未生成", "warn");
  } else if (cert.namesInSync) {
    setLocalCertLine(
      leaf,
      `覆盖 ${cert.names.length} 个名字 · 有效期至 ${cert.leafNotAfter}（剩 ${cert.leafDaysLeft} 天）`,
    );
  } else {
    setLocalCertLine(
      leaf,
      `证书里是 ${cert.certifiedNames.length} 个名字，当前映射需要 ${cert.names.length} 个 —— 保存映射或点「生成或更新证书」`,
      "warn",
    );
  }

  if (!cert.canInstallTrust) {
    setLocalCertLine(trust, "当前平台请手动把 ca.crt 导入系统根证书库", "warn");
  } else if (cert.caTrusted === true) {
    setLocalCertLine(trust, "已装入当前用户的受信任根证书颁发机构", "ok");
  } else if (cert.caTrusted === false) {
    setLocalCertLine(trust, "未信任 —— 浏览器仍会报证书警告", "warn");
  } else {
    setLocalCertLine(trust, "状态未知", "warn");
  }

  dir.textContent = cert.dir || "";

  byId("install-local-cert").disabled = !cert.caExists || !cert.canInstallTrust;
  byId("uninstall-local-cert").disabled = !cert.canInstallTrust;
  byId("install-local-cert").hidden = !cert.canInstallTrust;
  byId("uninstall-local-cert").hidden = !cert.canInstallTrust;
}

async function refreshLogs(force = false) {
  const now = Date.now();
  // 用户主动点「清空显示」时不能撞上 900ms 节流，否则界面还留着旧日志，
  // 看起来就是「点了没反应 / 卡住」。
  if (currentPage !== "logs" || logsRefreshing || (!force && now - lastLogsRefreshAt < 900)) return;
  logsRefreshing = true;
  lastLogsRefreshAt = now;
  try {
    const response = await apiRequest("/api/logs");
    const logs = await response.text();
    if (currentPage !== "logs") return;
    const output = byId("log-output");
    const shouldFollow = output.scrollTop + output.clientHeight >= output.scrollHeight - 30;
    const content = logs || "暂无运行日志";
    if (content === lastLogsContent) return;
    lastLogsContent = content;
    output.textContent = content;
    if (shouldFollow) output.scrollTop = output.scrollHeight;
  } catch (_error) { /* state polling reports connectivity */ }
  finally { logsRefreshing = false; }
}

/* ------------------------------------------------------------------
   环境管理页：开发环境体检 + 一键部署
   ------------------------------------------------------------------ */
let envScanning = false;
let envBusy = false;
// 本页正在跑批量循环（区别于从 /api/state 里看到的后端任务）。
// 批量循环自己负责收尾，进度组件不能中途把 busy 提前放开。
let envLocalBatch = false;
// 任务结束只处理一次：状态轮询每 2.5 秒来一次，不能每轮都重扫体检。
let envTaskHandled = "";
// busy 是不是「只因为后端任务还在跑」而置上的（页面刷新后没有本地动作可收尾）。
let envTaskAutoBusy = false;
// nvm 状态单独缓存一份：体检结果里只有「装了哪些」，没有「能装哪些」。
// 可装版本要调 nvm（要联网拉 available 列表），单独按需拉一次就好。
let nvmState = null;
// 「可安装版本」要联网拉 nvm list available，只在用户点开时查一次，别跟着体检跑。
let nvmAvailableChecked = false;
// winget 按大版本分包的项目：下拉给大版本系列，可并存。示例版本按语言分别给，
// 别把 Python 的 3.10 / 3.11 / 3.12 印到 PHP 卡片上。
const SERIES_ENV_IDS = ["python", "php", "ruby", "dotnet"];
const SERIES_EXAMPLES = {
  python: "3.10 / 3.11 / 3.12",
  php: "8.3 / 8.4 / 8.5",
  ruby: "3.3 / 3.4 / 4.0",
  dotnet: "8 / 9 / 10",
};
let envVersionTarget = { id: "", versions: [], versionKind: "patch" };
// 最近一次体检结果。查完最新版本要用它重画卡片，不重发 scanEnv。
let envData = null;
// 最新稳定版查询结果：只有用户点「检查最新版本」才会有值，切页绝不自动联网。
let envLatest = null;
let envLatestBusy = false;

const ENV_GROUPS = [
  { id: "core", title: "运行环境与工具", hint: "本程序与构建链路真正依赖的组件" },
  { id: "language", title: "开发语言工具链", hint: "只做体检与单项更新，不进批量部署" },
];

/** 全新系统没有 winget：安装/更新/版本查询全部走官方直连通道。 */
function envIsDirect() {
  return Boolean(envData && envData.installMode === "direct");
}

/**
 * 逐段比数字的版本比较。返回 1 / -1 / 0；格式怪到比不了就返回 0
 * （宁可少提示一个「有新版」，也不要让用户去更新一个看不懂的版本号）。
 */
function compareEnvVersions(left, right) {
  const parts = (value) => String(value ?? "")
    .trim()
    .replace(/^[vV]/, "")
    .split(/[.\-+]/)
    .map((part) => (/^\d+$/.test(part) ? Number(part) : null))
    .filter((part) => part !== null);
  const a = parts(left);
  const b = parts(right);
  if (!a.length || !b.length) return 0;
  for (let i = 0; i < Math.max(a.length, b.length); i += 1) {
    const diff = (a[i] || 0) - (b[i] || 0);
    if (diff > 0) return 1;
    if (diff < 0) return -1;
  }
  return 0;
}

function envLatestInfo(item) {
  if (!envLatest || !envLatest.items) return null;
  return envLatest.items[item.id] || null;
}

/** 这一项是否「已装 + 有官方新版 + 本机能更新」。 */
function envUpdateAvailable(item) {
  const info = envLatestInfo(item);
  if (!item.ready || !item.canUpdate || !info || !info.latest) return false;
  return compareEnvVersions(info.latest, item.version) > 0;
}

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (char) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[char]));
}

function renderEnv(data) {
  envData = data;
  const grid = byId("env-grid");
  const summary = byId("env-summary");
  const items = Array.isArray(data.items) ? data.items : [];

  if (!items.length) {
    summary.textContent = "没有可体检的项目";
    grid.innerHTML = "";
    return;
  }

  const outdated = items.filter((item) => item.outdated);
  const missing = items.filter((item) => !item.ready);
  // 摘要把三种情况分开讲。混成一句「N/M 就绪」的话，
  // 用户看到 5/7 会不知道剩下两项是没装还是版本旧，而这两者要做的事完全不同。
  const parts = [`<strong>${data.ready}/${data.total}</strong> 已就绪`];
  if (outdated.length) parts.push(`${outdated.length} 项版本过旧（${escapeHtml(outdated.map((i) => i.name).join("、"))}）`);
  if (missing.length) parts.push(`${missing.length} 项缺失（${escapeHtml(missing.map((i) => i.name).join("、"))}）`);
  // 「一键部署缺失项」是灰的时候把原因写在摘要里，别让人以为功能没打进这个版本。
  const coreMissing = missing.filter((item) => (item.group || "core") === "core");
  // 批量按钮只管 core 组。语言组有缺失时把「去哪儿点」直接写在摘要里，
  // 别让人对着灰掉的批量按钮以为一键部署功能丢了。
  const langMissing = missing.filter((item) => item.group === "language" && item.canInstall);
  if (!coreMissing.length) {
    parts.push(langMissing.length
      ? `<span class="env-summary-idle">运行环境已齐；${escapeHtml(langMissing.map((i) => i.name).join("、"))} 请点各自卡片里的「一键部署」</span>`
      : `<span class="env-summary-idle">运行环境已齐，无需部署</span>`);
  }
  // 全新系统没有 App Installer（winget 是随商店下发的）。以前这里写「自动安装不可用」，
  // 整列按钮灰掉，用户只能去官网。现在安装自动走官方直连，摘要要把当前通道说清，
  // 并把「修复 winget」作为可选入口摆出来，而不是冒充已经修好。
  const repairButton = byId("env-repair-winget");
  if (repairButton) {
    repairButton.hidden = Boolean(data.winget);
    repairButton.disabled = envBusy;
  }
  if (!data.winget) {
    parts.push(`<span class="env-summary-warn">未找到 winget，已自动切换官方直连安装；点「修复 winget」可恢复完整能力</span>`);
  }
  summary.innerHTML = parts.join(" · ");

  // 分两栏：core 是程序依赖，language 是可选工具链。分组还有个实际作用 ——
  // 「一键部署缺失项」只扫 core 那个 grid，不会把几百 MB 的语言包一起装上。
  grid.innerHTML = ENV_GROUPS.map((group) => {
    const list = items.filter((item) => (item.group || "core") === group.id);
    if (!list.length) return "";
    const ready = list.filter((item) => item.ready && !item.outdated).length;
    return `
      <section class="env-group" data-env-section="${group.id}">
        <div class="env-group-head">
          <h3>${escapeHtml(group.title)}</h3>
          <span class="env-group-hint">${escapeHtml(group.hint)}</span>
          <span class="env-group-count">${ready}/${list.length} 就绪</span>
        </div>
        <div class="env-grid" data-env-group="${group.id}">
          ${list.map(renderEnvCard).join("")}
        </div>
      </section>`;
  }).join("");

  // 一键部署只在「确有缺失且本机能自动装」时才可点，且只管 core 组。
  // 过期项不算在内 —— 它需要的是重装，混进批量安装会让 winget 去做破坏性更大的事。
  const installAll = byId("env-install-all");
  const installable = missing.filter(
    (item) => (item.group || "core") === "core" && item.canInstall
  );
  installAll.disabled = !installable.length || envBusy;
  const hints = ["只部署「运行环境与工具」，语言工具链请单项安装"];
  if (!data.winget) hints.push("本机没有 winget，将走官方安装包直连，不依赖 winget");
  if (installable.length) hints.push(`将依次安装：${installable.map((item) => item.name).join("、")}`);
  else hints.push("没有缺失项");
  if (outdated.length) hints.push(`过期项请单独点「重装升级」`);
  installAll.title = hints.join("；");

  const updatable = items.filter(envUpdateAvailable);
  const updateAll = byId("env-update-all");
  updateAll.disabled = !updatable.length || envBusy || envLatestBusy;
  updateAll.title = updatable.length
    ? `将依次更新：${updatable.map((item) => item.name).join("、")}`
    : "先点「检查最新版本」，查出新版后才能批量更新";

  const uninstallable = items.filter((item) => item.ready && item.canUninstall);
  const uninstallAll = byId("env-uninstall-all");
  if (uninstallAll) {
    uninstallAll.disabled = !uninstallable.length || envBusy;
    uninstallAll.title = uninstallable.length
      ? `打开清单勾选后批量卸载（默认只勾语言工具链）：${uninstallable.map((item) => item.name).join("、")}`
      : "没有可卸载的已装组件";
  }
}

function renderEnvCard(item) {
  // winget 不可用时不给「一键部署」，只留「去官网」，并在卡片上说明原因
  const canInstall = Boolean(item.canInstall);
  const canReinstall = Boolean(item.canReinstall) && item.outdated;
  // 全新系统走官方直连，按钮悬浮说明不能还写着「winget 包」。
  const updateHint = envIsDirect()
    ? "自动更新到官方最新稳定版：从官方发布源直接下载安装包，不依赖 winget；Rust 走 rustup，Node 走官方 MSI"
    : "自动更新到官方最新稳定版：跨大版本会装最新版对应的 winget 包（旧版本保留）；Rust 走 rustup，Node 走 nvm";
  // 三态决定徽章与卡片配色：就绪 / 缺失 / 版本过旧
  const state = item.outdated ? "outdated" : (item.ready ? "ready" : "missing");
  const badgeText = item.outdated ? "版本过旧" : (item.ready ? "已就绪" : "缺失");
  const versionText = item.ready ? (item.version || "已安装") : "未检测到";
  const pathHint = item.path
    ? escapeHtml(item.path)
    : (item.ready ? "位置未记录" : "");

  // 版本过旧时把「需要哪个版本」摆出来。后端的 outdatedNote 里也带当前版本，
  // 但那是给 toast 用的单行摘要；这里已经有版本号单独一行了，再拼一遍会变成
  // 「需要 20.0.0 或更高；当前 18.19.0，建议 20.0.0 以上」—— 同一句话说三遍。
  // outdatedNote 不丢，挪到 title 上做悬浮提示，信息还在但不占版面。
  const hint = item.outdated
    ? `<div class="env-card-hint"${item.outdatedNote ? ` title="${escapeHtml(item.outdatedNote)}"` : ""}>需要 <code>${escapeHtml(item.minVersion)}</code> 或更高版本，重装可升级到最新版</div>`
    : "";

  // 最新稳定版只在查过之后才显示。没查过时卡片保持原样，不摆一行空的占位。
  const info = envLatestInfo(item);
  const updatable = envUpdateAvailable(item);
  let latestLine = "";
  if (info && info.error) {
    latestLine = `<div class="env-card-latest is-error" title="${escapeHtml(info.error)}">最新版本：查询失败</div>`;
  } else if (info && info.latest) {
    // 有新版但不给更新按钮时（Node 走 nvm、winget 会跟它打架），
    // 不能只写「有新版」让人以为按钮丢了 —— 把该走的路径直接写在标记里。
    const hasNew = item.ready && compareEnvVersions(info.latest, item.version) > 0;
    const flag = !item.ready
      ? "官方最新"
      : (hasNew ? (item.canUpdate ? "有新版" : "新版请用版本管理") : "已是最新");
    latestLine = `<div class="env-card-latest${hasNew ? " is-new" : ""}" title="数据来源：${escapeHtml(info.source || "")}">
      <span>最新 <strong>${escapeHtml(info.latest)}</strong></span>
      <span class="env-latest-flag">${flag}</span>
    </div>`;
  }

  // 卸载入口对所有在白名单里的项都给：缺失时也能用来清「装了一半」的残留目录。
  const uninstallAction = item.canUninstall
    ? `<button type="button" class="text-button env-uninstall" data-env-uninstall="${escapeHtml(item.id)}" title="卸载并清理残留目录、PATH 与环境变量，删除前会先列出清单">${item.ready ? "卸载并清理" : "清理残留"}</button>`
    : "";

  let actions;
  if (item.outdated) {
    // 过期项给「重装」，不给「一键部署」—— 重装走 upgrade，破坏性小得多
    actions = `${canReinstall
        ? `<button type="button" class="button primary" data-env-reinstall="${escapeHtml(item.id)}">${icon("refresh-cw")}<span>重装升级</span></button>`
        : ""}
      <button type="button" class="text-button" data-env-open="${escapeHtml(item.id)}">查看官网版本</button>`;
  } else if (item.ready) {
    actions = `${updatable
        ? `<button type="button" class="button primary" data-env-update="${escapeHtml(item.id)}" title="${escapeHtml(updateHint)}">${icon("arrow-up")}<span>更新</span></button>`
        : ""}
      <button type="button" class="text-button" data-env-open="${escapeHtml(item.id)}">打开官网</button>`;
  } else {
    actions = `${canInstall
        ? `<button type="button" class="button primary" data-env-install="${escapeHtml(item.id)}">${icon("download")}<span>一键部署</span></button>`
        : ""}
      <button type="button" class="text-button" data-env-open="${escapeHtml(item.id)}" title="${escapeHtml(envIsDirect() && !canInstall ? "这一项没有稳定的官方直连安装包；可点工具栏「修复 winget」恢复完整安装能力，或前往官网手动下载" : "打开该项目官方下载页")}">去官网下载</button>`;
  }

  return `
    <div class="env-card is-${state}${updatable ? " is-updatable" : ""}" data-env-card="${escapeHtml(item.id)}">
      <div class="env-card-head">
        <span class="env-card-name">${escapeHtml(item.name)}</span>
        <span class="env-badge is-${state}">${badgeText}</span>
      </div>
      <div class="env-card-version ${item.ready && !item.version ? "is-unknown" : ""}">${escapeHtml(versionText)}</div>
      ${latestLine}
      ${hint}
      ${pathHint ? `<div class="env-card-path">${pathHint}</div>` : ""}
      ${item.note ? `<div class="env-card-note">${escapeHtml(item.note)}</div>` : ""}
      ${renderVersionManager(item)}
      <div class="env-card-actions">${actions}${uninstallAction}</div>
    </div>`;
}

/**
 * 版本管理区。只画在支持指定版本的项上（目前只有 Node 走 nvm）。
 *
 * 分两层：
 * - 已装版本做成可点的徽章，点一下就切（切版本不破坏任何东西，手感要快）
 * - 没装的版本放下拉框，选中后点「安装并切换」才动手（要下载，慢）
 */
function renderVersionManager(item) {
  if (!item.supportsVersions) return "";
  // Node 才有 nvm 数据。别的项（Python/Git/JDK）走 winget 源，
  // 版本列表要现查，装之前先让用户知道有哪些可选 —— 展开时再拉，不做无用请求。
  if (item.id !== "node") return renderWingetVersionManager(item);

  const state = nvmState;
  if (!state || !state.installed) {
    return `
      <div class="env-versions">
        <div class="env-versions-title">${icon("download")}<span>版本管理</span></div>
        <div class="env-versions-hint">${escapeHtml(
          state ? state.hint || "未检测到 nvm" : "正在检测 nvm…"
        )}</div>
        <div class="env-versions-actions" style="margin-top:8px">
          <button type="button" class="button secondary" data-env-install="nvm">${icon("download")}<span>安装 nvm</span></button>
        </div>
      </div>`;
  }

  const installed = Array.isArray(state.installedVersions) ? state.installedVersions : [];
  const available = Array.isArray(state.availableVersions) ? state.availableVersions : [];
  const current = state.current || "";
  // 已经装了的就不该再出现在「可安装」下拉里
  const candidates = available.filter((entry) => !installed.includes(entry.version));

  const chips = installed.length
    ? `<div class="env-versions-row">
        <div class="env-version-chips">
          ${installed.map((version) => `
            <button type="button"
              class="env-version-chip ${version === current ? "is-current" : ""}"
              data-nvm-use="${escapeHtml(version)}"
              title="${version === current ? "当前正在使用" : `切换到 ${version}`}"
              ${envBusy ? "disabled" : ""}>${escapeHtml(version)}${version === current ? " ✓" : ""}</button>
          `).join("")}
        </div>
      </div>`
    : "";

  // 可安装版本要联网（nvm list available 实测 2.66 秒），不能挂在进页面这条路径上。
  // 已装版本的徽章是本地数据，先画出来让用户能立刻切换；可装列表点按钮才查。
  let picker;
  if (candidates.length) {
    picker = `<div class="env-versions-row">
        <select class="env-versions-select" id="env-nvm-pick">
          ${candidates.map((entry) => `<option value="${escapeHtml(entry.version)}">${escapeHtml(entry.version)}${entry.channel === "LTS" ? "（LTS）" : ""}</option>`).join("")}
        </select>
        <div class="env-versions-actions">
          <button type="button" class="button primary" data-nvm-install>${icon("download")}<span>安装并切换</span></button>
        </div>
      </div>`;
  } else if (nvmAvailableChecked) {
    picker = `<div class="env-versions-hint">没查到更多可安装版本（nvm 只列 CURRENT / LTS 两列）。要装指定版本可用 <code>nvm install &lt;版本号&gt;</code>，或到 <a href="https://nodejs.org/dist/" target="_blank" rel="noreferrer">nodejs.org/dist</a> 查。</div>`;
  } else {
    picker = `<div class="env-versions-row">
        <button type="button" class="button secondary" data-nvm-query-versions>${icon("download")}<span>查看可安装版本（联网）</span></button>
        <div class="env-versions-hint" style="margin:0">已装版本点上面的徽章就能切换；这一步只查还能装哪些，不会跟着体检自动跑。</div>
      </div>`;
  }

  return `
    <div class="env-versions" data-nvm-for="node">
      <div class="env-versions-title">${icon("cpu")}<span>版本管理</span>
        <span class="env-versions-tag">nvm ${escapeHtml(state.version || "")}</span>
      </div>
      ${chips}
      ${picker}
      <div class="env-versions-hint">已装 ${installed.length} 个版本，可并存，切版本不影响其它项目。切换后新开的终端才会生效（nvm 靠改软链接实现，已开着的窗口里还是旧版本）。</div>
    </div>`;
}

/**
 * 走 winget 的版本选择（Python / Git / JDK）。
 *
 * 与 Node 的关键差别：winget 换版本要先卸载旧的，所以文案必须说清这是破坏性操作，
 * 不能让用户以为点一下就多出一个并存版本。
 */
function renderWingetVersionManager(item) {
  // python / php / ruby / dotnet 在 winget 里按大版本拆成不同的包 id，
  // 下拉给的是「3.10 / 3.11 / 3.12」这类系列；装不同系列可以并存。
  // git / jdk / go 只有一个包，下拉给补丁号，换版本会覆盖现有安装。
  const seriesItem = SERIES_ENV_IDS.includes(item.id);
  // 没展开就不画这个区 —— 版本列表要调 winget 源，不该一进页面就拉
  if (!envVersionTarget || envVersionTarget.id !== item.id) {
    return `
      <div class="env-versions">
        <div class="env-versions-title">${icon("download")}<span>版本管理</span></div>
        <div class="env-versions-actions">
          <button type="button" class="text-button" data-env-query-versions="${escapeHtml(item.id)}">${seriesItem ? "选择要安装的大版本" : "查看可安装版本"}</button>
        </div>
        <div class="env-versions-hint">${envIsDirect()
          ? "本机没有 winget，版本列表来自官方发布源；安装会直接下载官方安装包。"
          : (seriesItem
            ? `winget 里各系列是独立包，装 ${SERIES_EXAMPLES[item.id] || "不同大版本"} 可与现有版本并存；命令行默认用哪个由 PATH / 版本选择器决定。`
            : "<strong>这一项不能多版本并存</strong>：装指定版本会先卸载现有版本。要并存请用 Node 的 nvm。")}</div>
      </div>`;
  }
  const versions = Array.isArray(envVersionTarget.versions) ? envVersionTarget.versions : [];
  if (!versions.length) {
    return `
      <div class="env-versions">
        <div class="env-versions-title">${icon("download")}<span>版本管理</span></div>
        <div class="env-versions-hint">${escapeHtml(envVersionTarget.note || "没有查到可安装的版本，请检查 winget 源或网络连接。")}</div>
      </div>`;
  }
  const isSeries = envVersionTarget.versionKind === "series";
  return `
    <div class="env-versions">
      <div class="env-versions-title">${icon("download")}<span>版本管理</span>
        ${isSeries ? `<span class="env-versions-tag">大版本系列</span>` : ""}
      </div>
      <div class="env-versions-row">
        <select class="env-versions-select" data-env-version-pick="${escapeHtml(item.id)}">
          <option value="">保持 ${escapeHtml(item.version || "当前版本")}</option>
          ${versions.map((version) => `<option value="${escapeHtml(version)}">${isSeries ? `${escapeHtml(item.name)} ${escapeHtml(version)}` : escapeHtml(version)}</option>`).join("")}
        </select>
        <div class="env-versions-actions">
          <button type="button" class="button primary" data-env-install-version="${escapeHtml(item.id)}">${icon("download")}<span>安装该版本</span></button>
        </div>
      </div>
      <div class="env-versions-hint">${envIsDirect()
        ? (isSeries
          ? "<strong>官方直连</strong>：将从官方发布源下载该系列最新补丁的安装包，不卸载现有版本。"
          : "<strong>官方直连</strong>：将从官方发布源下载安装包，安装行为以官方安装器为准。")
        : (isSeries
          ? "<strong>各系列可并存</strong>：winget 里每个大版本是独立包，安装不会卸载现有版本；命令行默认指向哪个由 PATH / 版本选择器决定。"
          : "<strong>不能多版本并存</strong>：装指定版本会先卸载现在的版本（用 winget --version --force）。")}</div>
    </div>`;
}

async function queryEnvVersions(id) {
  if (envBusy) return;
  setEnvBusy(true);
  try {
    const data = await invokeStrict("envAvailableVersions", id);
    envVersionTarget = {
      id,
      versions: Array.isArray(data?.versions) ? data.versions : [],
      versionKind: data?.versionKind || "patch",
      source: data?.source || "winget",
      note: data?.note || "",
    };
    // 固定「最新稳定版」的项（WebView2 / VC++ / ADB）本来就没有版本列表，
    // 后端给了说明文案，这种情况不是失败，别弹错误 toast。
    if (!envVersionTarget.versions.length && !envVersionTarget.note) {
      showToast("error", "没查到可安装的版本");
    }
  } catch (error) {
    showToast("error", error.message || "查询版本失败");
    return;
  } finally {
    setEnvBusy(false);
  }
  // 用缓存的体检结果重画，不再多发一次 scanEnv（版本区是卡片的一部分）。
  if (envData) renderEnv(envData);
}

async function queryNvmAvailableVersions() {
  if (envBusy || nvmAvailableChecked) return;
  setEnvBusy(true);
  try {
    const data = await invokeStrict("nvmAvailableVersions");
    nvmState = {
      ...(nvmState || {}),
      availableVersions: Array.isArray(data?.availableVersions) ? data.availableVersions : [],
      hint: data?.hint || "",
    };
    nvmAvailableChecked = true;
    if (envData) renderEnv(envData);
  } catch (error) {
    showToast("error", error.message || "查询 Node 可安装版本失败");
  } finally {
    setEnvBusy(false);
  }
}

async function refreshNvm() {
  try {
    nvmState = await invoke("nvmStatus");
  } catch (error) {
    nvmState = { installed: false, hint: `检测 nvm 失败：${error.message || error}`, installedVersions: [], availableVersions: [], version: "", current: "" };
  }
}

async function runNvmAction(method, version, verb) {
  if (envBusy) return;
  setEnvBusy(true);
  // 后端在 nvmInstallVersion / nvmUseVersion 的返回里已经带了重扫过的 env，
  // 直接用它，别再发一次 scanEnv —— 两边来自同一份快照才不会一个说切了、
  // 一个说没切。
  let freshEnv = null;
  try {
    const result = await invokeStrict(method, version);
    if (result && result.nvm) {
      // 后端回的是本地状态（不含联网的可安装版本），把已经查过的列表保住，
      // 否则装完一个版本下拉就空了，用户得再点一次联网查询。
      nvmState = {
        ...(nvmState || {}),
        ...result.nvm,
        availableVersions: nvmAvailableChecked
          ? (nvmState?.availableVersions || [])
          : [],
      };
    }
    freshEnv = result?.env || null;
    showEnvDetail(`${verb}输出`, result?.detail || `${verb}完成`, "ok");
  } catch (error) {
    const message = error.message || String(error);
    showEnvDetail(`${verb}失败`, message, "fail");
    showToast("error", `${verb}失败：${message.split("\n")[0]}`);
  } finally {
    setEnvBusy(false);
  }
  // refreshEnv 现在只看 envScanning，不再被 busy 拦；仍放在 setEnvBusy(false)
  // 之后，是为了让按钮先恢复可用再重画卡片。
  // 失败时后端不返回 env，退回强制重扫（比如卸载的正好是当前版本）。
  if (freshEnv) renderEnv(freshEnv);
  else await refreshEnv(true);
}

async function refreshEnv(force = false) {
  // 体检是只读的，不能因为「有任务在跑」就整个跳过：任务期间切进环境页时，
  // 页面还没渲染过任何卡片，这一跳会让整个列表空白，只剩一张任务卡。
  // busy 只管按钮禁用（setEnvBusy），不挡状态读取；envScanning 防的是并发重扫。
  if (envScanning) return;
  envScanning = true;
  const summary = byId("env-summary");
  summary.textContent = "正在体检…";
  try {
    // 两个查询在后端都走 spawn_blocking（不持状态锁），并行发起。
    // 以前串行 await：nvmStatus 联网 2.66s + scanEnv 1.5s，加起来 4 秒多，
    // 期间 /api/state 全在排队，左下角流量曲线直接停住。
    const [data] = await Promise.all([
      invokeStrict("scanEnv", force),
      nvmState ? Promise.resolve() : refreshNvm(),
    ]);
    // 后端对体检结果有 15 秒缓存，防止意外的高频轮询把整机拖起来；
    // 用户手动点「重新体检」要绕过缓存，否则点了像没反应。
    renderEnv(data);
  } catch (error) {
    summary.textContent = `体检失败：${error.message || error}`;
  } finally {
    envScanning = false;
  }
}

function showEnvDetail(title, body, tone = "") {
  byId("env-detail-title").textContent = title;
  byId("env-detail-body").textContent = body || "（无输出）";
  const panel = byId("env-detail");
  // tone: ok / check / fail。只有日志没有结论时，用户分不清 winget「已成功安装」
  // 到底算不算装上了（PHP 8.5 装完仍显示缺失那次就是这个坑）。
  panel.classList.remove("is-ok", "is-check", "is-fail");
  if (tone) panel.classList.add(`is-${tone}`);
  const subtitle = byId("env-detail-subtitle");
  if (subtitle) {
    subtitle.textContent = tone === "check"
      ? "命令已执行，但体检结果与输出不一致，请看开头的结论与处理建议"
      : tone === "fail"
        ? "命令执行失败，下面是完整输出"
        : tone === "ok"
          ? "结论在第一行，下面是命令原始输出"
          : "下面是命令的原始输出";
  }
  panel.hidden = false;
  panel.scrollIntoView({ block: "nearest", behavior: "smooth" });
}

function setEnvBusy(busy) {
  envBusy = busy;
  // 安装期间把所有按钮禁掉：winget 独占安装锁，连点两次只会得到一段乱序输出
  document.querySelectorAll(
    "[data-env-install], [data-env-reinstall], [data-env-update], [data-env-uninstall], [data-env-install-version], [data-env-query-versions]," +
    "[data-nvm-use], [data-nvm-install], [data-nvm-query-versions], #env-install-all, #env-uninstall-all, #env-refresh, #env-check-latest, #env-update-all, #env-repair-winget"
  )
    .forEach((button) => { button.disabled = busy; });
  // 摘要交给 renderEnv 画真实体检数据；任务提示由下方的进度卡负责。
  // 之前在这里改写摘要，会和体检结果互相覆盖，任务结束后还容易留下一句过期提示。
}

/* ------------------------------------------------------------------------
   环境任务进度：数据来自 /api/state 的 envTask，由后端在每个动作真正
   开始 / 结束时推进。winget 不提供逐百分比进度，所以这里显示的是
   「第 N/M 项 + 当前阶段 + 每项真实结果」，不是估算出来的动画。
   ------------------------------------------------------------------------ */
function renderEnvTask(task) {
  const root = byId("env-task");
  if (!root) return;
  if (!task || typeof task !== "object") {
    root.hidden = true;
    return;
  }
  root.hidden = false;
  const running = Boolean(task.active);
  const total = Number(task.total) || 0;
  // 单项任务后端只能报 1/1 = 100%，可 winget 还要跑几分钟。
  // 这种时候画一根满格进度条是骗人，改成不确定态动画 + 阶段文字。
  const singleRunning = running && total <= 1;
  root.classList.toggle("is-done", task.status === "done");
  root.classList.toggle("is-failed", task.status === "failed");
  root.classList.toggle("is-indeterminate", singleRunning);
  byId("env-task-label").textContent = task.label || "环境任务";
  byId("env-task-status").textContent = running
    ? "进行中"
    : (task.status === "failed" ? "有失败项" : "已完成");
  byId("env-task-count").textContent = `${Number(task.current) || 0}/${total}`;
  byId("env-task-fill").style.width = singleRunning
    ? "35%"
    : `${Math.min(100, Number(task.percent) || 0)}%`;
  byId("env-task-phase").textContent = task.phase || "";
  const seconds = Math.max(0, Math.round((Number(task.elapsedMs) || 0) / 1000));
  byId("env-task-elapsed").textContent = `已用 ${Math.floor(seconds / 60)} 分 ${String(seconds % 60).padStart(2, "0")} 秒`;
  const logs = Array.isArray(task.logs) ? task.logs.slice(-6) : [];
  byId("env-task-logs").innerHTML = logs.map((entry) => `
    <li class="env-task-log ${entry.ok ? "is-ok" : "is-fail"}">
      <span class="env-task-log-mark">${entry.ok ? "✓" : "✕"}</span>
      <strong>${escapeHtml(entry.id)}</strong>
      <span>${escapeHtml(entry.detail || "")}</span>
    </li>`).join("");

  // 刷新页面后任务可能还在跑：按钮必须跟着禁用，不能只看本地 envBusy。
  if (running && !envBusy) {
    setEnvBusy(true);
    envTaskAutoBusy = true;
  }
  // 双保险：任务卡都显示了、卡片区却还是空的（典型场景就是在任务进行中
  // 才切进环境页，切页那次体检可能还没回来），主动补一次只读体检。
  // refreshEnv 已不再被 envBusy 拦住，这里能拿到真实数据把卡片画上。
  const envGrid = byId("env-grid");
  if (running && envGrid && !envGrid.children.length && !envScanning) refreshEnv();
  const key = `${task.startedAt}:${task.status}`;
  if (!running && key !== envTaskHandled) {
    envTaskHandled = key;
    // 自己置的 busy 自己清；批量循环 / 单项动作由它们的 finally 收尾。
    if (envTaskAutoBusy) {
      setEnvBusy(false);
      envTaskAutoBusy = false;
    }
    if (!envLocalBatch && !envBusy) refreshEnv(true);
  }
}

async function beginEnvTask(label, ids) {
  envTaskHandled = "";
  try {
    await invokeStrict("envTaskBegin", label, ids);
  } catch (_error) {
    // 进度通道不可用时也要让动作照常跑，别拿展示问题挡住功能。
  }
}

async function finishEnvTask() {
  try { await invoke("envTaskFinish"); } catch (_error) { /* 同上 */ }
}

function formatEnvBytes(bytes) {
  const value = Number(bytes) || 0;
  return value >= 1048576
    ? `${(value / 1048576).toFixed(1)} MB`
    : `${Math.round(value / 1024)} KB`;
}

/** 单项卸载：先拿只读清单，列全路径让用户确认，再真正动手。 */
async function uninstallEnvById(id) {
  if (envBusy) return;
  let preview;
  try {
    preview = await invokeStrict("envUninstallPreview", id);
  } catch (error) {
    showToast("error", error.message || "读取卸载清单失败");
    return;
  }
  if (!preview.allowed) {
    showToast("error", preview.reason || "该项不允许卸载");
    return;
  }
  const kindLabels = { install: "程序目录", cache: "缓存/配置", userdata: "用户数据" };
  const paths = (Array.isArray(preview.paths) ? preview.paths : []).filter((entry) => entry.exists);
  const rows = paths.map((entry) => `
    <li class="cleanup-item ${entry.kind === "userdata" ? "is-userdata" : ""}">
      <span class="cleanup-kind">${escapeHtml(kindLabels[entry.kind] || entry.kind)}</span>
      <code>${escapeHtml(entry.path)}</code>
      <span class="cleanup-size">约 ${Number(entry.files) || 0} 个文件 · ${formatEnvBytes(entry.bytes)}</span>
      <span class="cleanup-note">${escapeHtml(entry.note || "")}</span>
    </li>`).join("");
  const pathEntries = Array.isArray(preview.pathEntries) ? preview.pathEntries : [];
  const envVars = (Array.isArray(preview.envVars) ? preview.envVars : []).filter((entry) => entry.value);
  openModal(`卸载并清理 ${escapeHtml(preview.name)}`, `
    <p class="modal-warning">⚠️ 此操作非常危险，可能导致不可逆的数据丢失！</p>
    <p>将调用 <code>winget uninstall --purge</code>（${escapeHtml(preview.package || "对应包")}），
       并删除以下残留目录。清单之外的路径一律不动。</p>
    <ul class="cleanup-list">${rows || '<li class="cleanup-empty">没有检测到残留目录</li>'}</ul>
    ${pathEntries.length
      ? `<p class="cleanup-sub">PATH 中将被清理的条目：</p><ul class="cleanup-list">${pathEntries.map((entry) => `<li><code>${escapeHtml(entry)}</code></li>`).join("")}</ul>`
      : ""}
    ${envVars.length
      ? `<p class="cleanup-sub">将被清除的环境变量：</p><ul class="cleanup-list">${envVars.map((entry) => `<li><code>${escapeHtml(entry.name)}</code> = ${escapeHtml(entry.value)}</li>`).join("")}</ul>`
      : ""}
    <label class="env-uninstall-deep"><input type="checkbox" id="env-deep-clean"> 连同用户数据目录一起删除（默认保留，可能含你自己写的项目）</label>
    <p class="modal-hint">已打开的终端需要重开才会读到新的 PATH。</p>
  `, [
    { label: "取消", kind: "secondary", action: closeModal },
    {
      label: "确认卸载并清理",
      kind: "danger",
      action: () => {
        const deep = Boolean(byId("env-deep-clean") && byId("env-deep-clean").checked);
        closeModal();
        runEnvUninstall(id, deep);
      },
    },
  ]);
}

async function runEnvUninstall(id, deep) {
  if (envBusy) return;
  setEnvBusy(true);
  try {
    const result = await invokeStrict("uninstallEnv", id, deep);
    if (result && result.env) renderEnv(result.env);
    showEnvDetail("卸载输出", result?.detail || "卸载完成", "ok");
    showToast("success", `${id} 已卸载并清理`);
  } catch (error) {
    const message = error.message || String(error);
    showEnvDetail("卸载失败", message, "fail");
    showToast("error", `卸载失败：${message.split("\n")[0]}`);
  } finally {
    setEnvBusy(false);
    refreshEnv(true);
  }
}

/** 批量卸载：先勾选，再逐项跑。core 组默认不勾，免得一下卸掉程序自己的依赖。 */
async function uninstallAllEnv() {
  if (envBusy) return;
  const items = (envData && Array.isArray(envData.items) ? envData.items : [])
    .filter((item) => item.ready && item.canUninstall);
  if (!items.length) {
    showToast("info", "没有可卸载的已装组件");
    return;
  }
  const rows = items.map((item) => `
    <label class="env-uninstall-row" data-group="${escapeHtml(item.group || "core")}">
      <input type="checkbox" value="${escapeHtml(item.id)}" ${item.group === "language" ? "checked" : ""}>
      <span><strong>${escapeHtml(item.name)}</strong> ${escapeHtml(item.version || "")}</span>
      <span class="env-uninstall-warn">${item.group === "core" ? "核心组件，卸载可能影响本程序" : "语言工具链"}</span>
    </label>`).join("");
  openModal("一键卸载", `
    <p class="modal-warning">⚠️ 此操作非常危险，可能导致不可逆的数据丢失！</p>
    <p>勾选要卸载的组件。默认只勾语言工具链；核心组件是本程序的依赖，请自己确认后再勾。</p>
    <div class="env-uninstall-list">${rows}</div>
    <label class="env-uninstall-deep"><input type="checkbox" id="env-batch-deep"> 连同用户数据目录一起删除（GOPATH / npm 全局包等，默认保留）</label>
    <p class="modal-hint">逐项执行：官方卸载 → 清残留目录 → 清 PATH 与环境变量 → 复核。进度见页面任务卡。</p>
  `, [
    { label: "取消", kind: "secondary", action: closeModal },
    {
      label: "确认卸载",
      kind: "danger",
      action: () => {
        const ids = [...document.querySelectorAll(".env-uninstall-row input:checked")]
          .map((input) => input.value);
        const deep = Boolean(byId("env-batch-deep") && byId("env-batch-deep").checked);
        closeModal();
        if (!ids.length) {
          showToast("info", "没有勾选任何组件");
          return;
        }
        runEnvBatchUninstall(ids, deep);
      },
    },
  ]);
}

async function runEnvBatchUninstall(ids, deep) {
  if (envBusy) return;
  setEnvBusy(true);
  envLocalBatch = true;
  await beginEnvTask("一键卸载", ids);
  const chunks = [];
  let failed = 0;
  try {
    for (const id of ids) {
      try {
        const result = await invokeStrict("uninstallEnv", id, deep);
        if (result && result.detail) chunks.push(`### ${id}\n${result.detail}`);
        if (result && result.env) renderEnv(result.env);
      } catch (error) {
        failed += 1;
        chunks.push(`### ${id} 失败\n${error.message || error}`);
      }
    }
    showEnvDetail(failed ? `批量卸载输出（${failed} 项失败）` : "批量卸载输出", chunks.join("\n\n"), failed ? "fail" : "ok");
    showToast(failed ? "error" : "success", failed
      ? `${ids.length} 项中 ${failed} 项失败，详见输出`
      : `已卸载并清理 ${ids.length} 项`);
  } finally {
    await finishEnvTask();
    envLocalBatch = false;
    setEnvBusy(false);
    await refreshEnv(true);
  }
}

async function runEnvAction(id, method, verb, version = "") {
  if (envBusy) return;
  setEnvBusy(true);
  try {
    // 版本为空 = 装最新版；后端对 Node 会转给 nvm，其它走 winget 的 --version
    const result = await invokeStrict(method, id, version);
    if (result && result.env) renderEnv(result.env);
    // 后端会用重新体检的结果给出 outcome：命令成功 ≠ 真的装上了。
    // 以前成功时连个 toast 都没有，用户只能对着一屏 winget 日志猜。
    const needsCheck = result?.outcome === "check";
    showEnvDetail(
      needsCheck ? `${verb}完成，但体检未确认` : `${verb}输出`,
      result?.detail || `${verb}完成`,
      needsCheck ? "check" : "ok"
    );
    showToast(needsCheck ? "warn" : "success", needsCheck
      ? `${verb}命令已结束，但体检仍未检测到，详见输出`
      : `${verb}完成`);
  } catch (error) {
    // 失败时把后端带来的详细输出也摊开，别只留一句「安装失败」
    const message = error.message || String(error);
    showEnvDetail(`${verb}失败`, message, "fail");
    showToast("error", `${verb}失败：${message.split("\n")[0]}`);
  } finally {
    setEnvBusy(false);
    // 不管成败都强制重扫：winget 可能装完了但前端没拿到 env 快照，
    // 或者装到一半退出了，卡片必须跟磁盘上的真实状态一致。
    refreshEnv(true);
  }
}

async function installEnv(id) {
  return runEnvAction(id, "installEnv", "安装");
}

/**
 * 全新系统缺 App Installer：可选地把 winget 装回来。
 *
 * 这条动作要下约 315 MB 并注册系统组件，必须由用户明确点一次；安装、更新、
 * 版本查询本来就已自动走官方直连，所以修不成也不阻塞主流程。后端会先尝试
 * 零下载重注册，只有包确实不在时才下载官方 msixbundle（逐个校验 SHA256）。
 */
async function repairWinget() {
  if (envBusy) return;
  const ok = window.confirm(
    "本机没有 winget。将从 winget-cli 官方 release 下载并注册 App Installer（约 315 MB，逐个校验 SHA256）。\n\n" +
    "连不上 GitHub 时会自动尝试国内镜像；若已导入节点配置并开启接管，还会自动走本机代理重试。\n" +
    "这会向当前用户注册一个系统组件，可能需要几分钟。\n" +
    "不修复也不影响使用：安装与版本管理已自动走官方直连通道。\n\n继续吗？"
  );
  if (!ok) return;
  setEnvBusy(true);
  await beginEnvTask("修复 winget", ["winget"]);
  try {
    const result = await invokeStrict("repairWinget");
    if (result && result.env) renderEnv(result.env);
    showEnvDetail("winget 修复输出", result?.detail || "修复完成", "ok");
    showToast("success", "winget 已可用，环境页恢复完整安装能力");
  } catch (error) {
    const message = error.message || String(error);
    showEnvDetail("winget 修复失败（不影响官方直连安装）", message, "fail");
    // 第一行往往是「诊断：state=...」，下载失败时真正有用的是后面那句指引。
    const head = message.includes("所有下载源均失败")
      ? "下载源不可达，可先导入配置并开启接管后重试"
      : message.split("\n")[0];
    showToast("error", `修复未完成：${head}`);
  } finally {
    await finishEnvTask();
    setEnvBusy(false);
    refreshEnv(true);
  }
}

async function reinstallEnv(id) {
  return runEnvAction(id, "reinstallEnv", "重装");
}

async function installAllEnv() {
  if (envBusy) return;
  // 先自己算一遍要装哪些，避免为了「拿 id 列表」多调一次 scanEnv。
  // 只取 core 组的 is-missing：语言工具链是可选的，不该被这个按钮成批装上；
  // 过期项是 is-outdated，天然被排除 —— 批量走 install，重装会先卸载，
  // 两者破坏性不同，不该由一个「一键」按钮替用户决定。
  const cards = [...document.querySelectorAll('[data-env-group="core"] .env-card.is-missing')]
    .map((card) => card.dataset.envCard)
    .filter((id) => {
      const button = document.querySelector(`[data-env-install="${id}"]`);
      return Boolean(button);
    });
  if (!cards.length) {
    // 最常见的一头雾水：core 全齐但 Go 等语言组缺着，批量按钮按设计是灰的。
    // 把该点哪里直接写出来，别让人以为功能没生效。
    const langMissing = [...document.querySelectorAll('[data-env-group="language"] .env-card.is-missing')]
      .filter((card) => card.querySelector("[data-env-install]"))
      .map((card) => card.dataset.envCard);
    showEnvDetail("批量部署说明", langMissing.length
      ? `「一键部署缺失项」只负责「运行环境与工具」。语言工具链体积大且可选，请在各自卡片里点「一键部署」。\n\n待单项部署：${langMissing.join("、")}`
      : "没有可自动安装的缺失项。");
    showToast("info", langMissing.length
      ? "语言工具链请在卡片里单项部署"
      : "没有可自动安装的缺失项");
    return;
  }
  setEnvBusy(true);
  envLocalBatch = true;
  await beginEnvTask("一键部署", cards);
  const chunks = [];
  let failed = 0;
  let needsCheck = 0;
  try {
    for (const id of cards) {
      // 逐个装而不是并发：winget 有全局安装锁，官方安装器也会抢 MSI 安装事务，
      // 并发只会互相踩。全新系统走直连时同样串行，进度卡才对得上真实项数。
      try {
        const result = await invokeStrict("installEnv", id);
        if (result && result.detail) chunks.push(`### ${id}\n${result.detail}`);
        if (result && result.env) renderEnv(result.env);
        if (result?.outcome === "check") needsCheck += 1;
      } catch (error) {
        // 单项失败不中断整批，但要如实计数、把错误留在输出里
        failed += 1;
        chunks.push(`### ${id} 失败\n${error.message || error}`);
      }
    }
    showEnvDetail(
      failed ? `批量部署输出（${failed} 项失败）`
        : needsCheck ? `批量部署输出（${needsCheck} 项待确认）` : "批量部署输出",
      chunks.join("\n\n"),
      failed ? "fail" : needsCheck ? "check" : "ok"
    );
    showToast(failed ? "error" : needsCheck ? "warn" : "success", failed
      ? `${cards.length} 项中 ${failed} 项失败，详见输出`
      : needsCheck
        ? `${cards.length} 项中 ${needsCheck} 项命令完成但体检未确认，详见输出`
        : `已依次部署 ${cards.length} 项`);
  } finally {
    await finishEnvTask();
    envLocalBatch = false;
    setEnvBusy(false);
    refreshEnv(true);
  }
}

/**
 * 查各语言官方最新稳定版。
 *
 * 只在用户点按钮时调用 —— 不自动触发、不跟随任何轮询；后端另有 10 分钟缓存。
 * 这是环境页唯一会联网的动作。之前把联网探测塞进每轮体检，停在环境页就
 * 每 2.5 秒拉一次全量版本表，别再把它接回自动刷新链上。
 */
async function checkEnvLatest(force = false) {
  if (envLatestBusy || envBusy) return;
  envLatestBusy = true;
  const bar = byId("env-latest-bar");
  const button = byId("env-check-latest");
  button.disabled = true;
  bar.classList.remove("is-error");
  bar.classList.add("is-busy");
  bar.textContent = "正在查询各语言官方最新稳定版（联网，通常 1~3 秒）…";
  try {
    const data = await invoke("envLatestVersions", force);
    envLatest = data;
    const bits = [`已查 ${Number(data.ok || 0)} 项`];
    if (data.failed) bits.push(`${data.failed} 项查询失败（对应卡片上有提示）`);
    if (data.checkedAt) bits.push(`检查于 ${data.checkedAt}`);
    bits.push("点「更新」会自动跨大版本装最新版（旧版本保留）");
    bar.textContent = bits.join(" · ");
    showToast("success", "最新版本已更新");
  } catch (error) {
    bar.classList.add("is-error");
    bar.textContent = `查询失败：${error.message || error}`;
    showToast("error", error.message || "查询最新版本失败");
  } finally {
    bar.classList.remove("is-busy");
    envLatestBusy = false;
    button.disabled = false;
    // 用缓存的体检结果重画卡片，不重发 scanEnv，也不重新联网
    if (envData) renderEnv(envData);
  }
}

function updateEnvById(id) {
  // 把查到的官方最新稳定版一起传下去：后端据此判断是否跨大版本、该装哪个 winget 包。
  // 没查过最新版本时传空串，后端会保守地走同系列升级。
  const info = envLatest && envLatest.items ? envLatest.items[id] : null;
  const latest = info && info.latest ? info.latest : "";
  return runEnvAction(id, "updateEnv", "更新", latest);
}

/** 依次更新所有「查到新版且本机能更新」的项。逐条跑，winget 有全局安装锁。 */
async function updateAllEnv() {
  if (envBusy || envLatestBusy) return;
  const ids = [...document.querySelectorAll(".env-card")]
    .filter((card) => card.querySelector("[data-env-update]"))
    .map((card) => card.dataset.envCard);
  if (!ids.length) {
    showToast("success", "没有需要更新的项");
    return;
  }
  setEnvBusy(true);
  envLocalBatch = true;
  await beginEnvTask("一键更新", ids);
  const chunks = [];
  let failed = 0;
  let needsCheck = 0;
  try {
    for (const id of ids) {
      const info = envLatest && envLatest.items ? envLatest.items[id] : null;
      const latest = info && info.latest ? info.latest : "";
      try {
        const result = await invokeStrict("updateEnv", id, latest);
        if (result && result.detail) chunks.push(`### ${id}\n${result.detail}`);
        if (result && result.env) {
          envData = result.env;
          renderEnv(result.env);
        }
        if (result?.outcome === "check") needsCheck += 1;
      } catch (error) {
        failed += 1;
        chunks.push(`### ${id} 失败\n${error.message || error}`);
      }
    }
    showEnvDetail(
      failed ? `批量更新输出（${failed} 项失败）`
        : needsCheck ? `批量更新输出（${needsCheck} 项待确认）` : "批量更新输出",
      chunks.join("\n\n"),
      failed ? "fail" : needsCheck ? "check" : "ok"
    );
    showToast(failed ? "error" : needsCheck ? "warn" : "success", failed
      ? `${ids.length} 项中 ${failed} 项失败，详见输出`
      : needsCheck
        ? `${ids.length} 项中 ${needsCheck} 项命令完成但体检未确认，详见输出`
        : `已依次更新 ${ids.length} 项`);
  } finally {
    await finishEnvTask();
    envLocalBatch = false;
    setEnvBusy(false);
    await refreshEnv(true);
  }
}

/* ------------------------------------------------------------------
   服务器管理页的标签切换
   同一页里放 Linux（SSH 部署）与 Windows（RDP 会话治理）两套完全不同的东西：
   前者是「往机器上装代理」，后者是「让机器别睡着」。硬凑成一张表反而乱，
   所以用标签分开，但放在同一个「服务器管理」入口下 —— 它们本来就是一件事：
   管手头的服务器。
   ------------------------------------------------------------------ */
let serverTab = "linux";

function setServerTab(tab) {
  if (tab !== "linux" && tab !== "windows" && tab !== "transfer") return;
  if (serverTab === tab) return;
  serverTab = tab;
  document.querySelectorAll("[data-server-tab]").forEach((button) => {
    button.classList.toggle("is-active", button.dataset.serverTab === tab);
  });
  byId("server-tab-linux").classList.toggle("is-active", tab === "linux");
  byId("server-tab-windows").classList.toggle("is-active", tab === "windows");
  byId("server-tab-transfer").classList.toggle("is-active", tab === "transfer");
  // 切过去才渲染那半边。Windows 那半要起 powercfg / reg 子进程，
  // 不该在用户没看它的时候白跑一趟。
  if (tab === "windows") {
    renderWinServerList();
    loadWinServerFixes();
  } else if (tab === "transfer") {
    // 服务器清单与另外两个标签共用同一份 appState，切过来重画一次即可
    renderXferServers();
  } else {
    renderSshServers();
  }
}

/* ------------------------------------------------------------------
   服务器管理页 · Windows 标签
   Linux 那边是「往机器上装代理」，Windows 那边是「让机器别睡着」——
   两件事都用 SSH 连过去，Windows Server 2019+ 内置 OpenSSH，
   所以客户端与凭据完全共用。

   注意：字节格式化复用页面已有的 formatBytes，这里不重复定义。
   ------------------------------------------------------------------ */
let winSrvSelected = "";
let winSrvMetrics = null;
let winSrvPower = null;
let winSrvFixes = [];
let winSrvUsers = null;
let winSrvLoading = false;

function winServers() {
  return (appState.sshServers || []).filter((server) => server.kind === "windows");
}

/** Linux 侧的服务器（走代理部署）。老配置没有 kind 字段，一律当 Linux。 */
function linuxServers() {
  return (appState.sshServers || []).filter((server) => server.kind !== "windows");
}

function renderWinServerList() {
  const box = byId("winserver-list");
  if (!box) return;
  const servers = winServers();
  if (!servers.length) {
    box.innerHTML = `<div class="empty-state has-icon" style="--icon:url('./icons/server.svg')">
      还没有 Windows 服务器。点「添加 Windows 服务器」填上 IP 与 SSH 账号即可<br>
      <small>需要目标机开启 OpenSSH 服务器功能（Windows Server 2019+ 内置）</small>
    </div>`;
    return;
  }
  box.innerHTML = servers.map((server) => `
    <button type="button" class="winserver-srv ${server.profileId === winSrvSelected ? "is-active" : ""}"
      data-winsrv-pick="${escapeHtml(server.profileId)}">
      <span class="winserver-srv-name">${escapeHtml(server.name)}</span>
      <span class="winserver-srv-host">${escapeHtml(server.host)}:${server.port}</span>
      <span class="winserver-srv-user">${escapeHtml(server.username)}</span>
    </button>
  `).join("");
}

async function selectWinServer(profileId) {
  winSrvSelected = profileId;
  winSrvMetrics = null;
  winSrvPower = null;
  winSrvUsers = null;
  renderWinServerList();
  updateWinSrvApplyButton();
  await refreshWinServerMonitor();
  // 保活清单按服务器类型（linux/windows）不同，要跟着换
  await loadWinServerKeepAlive();
}

/// 拉取选中服务器的性能与电源设置。
async function refreshWinServerMonitor() {
  if (!winSrvSelected || winSrvLoading) return;
  winSrvLoading = true;
  const box = byId("winserver-monitor");
  if (box) box.innerHTML = `<div class="winserver-item-reason">正在连接服务器采集状态…</div>`;
  try {
    // 两项并行拉，各自走一次 SSH。串行会等两倍时间。
    const [metrics, power] = await Promise.all([
      invoke("winSrvMetrics", winSrvSelected),
      invoke("winSrvPowerSettings", winSrvSelected),
    ]);
    winSrvMetrics = metrics;
    winSrvPower = power;
    renderWinServerMonitor();
  } catch (error) {
    if (box) {
      box.innerHTML = `<div class="winserver-item-reason">采集失败：${escapeHtml(error.message || error)}</div>`;
    }
  } finally {
    winSrvLoading = false;
  }
}

function renderWinServerMonitor() {
  const box = byId("winserver-monitor");
  if (!box || !winSrvMetrics) return;
  const m = winSrvMetrics;
  const p = winSrvPower || {};

  const gauge = (label, percent, detail) => `
    <div class="winserver-gauge">
      <div class="winserver-gauge-head"><span>${label}</span><strong>${percent}%</strong></div>
      <div class="winserver-gauge-track"><span style="width:${Math.max(0, Math.min(100, percent))}%"></span></div>
      <div class="winserver-gauge-foot">${detail}</div>
    </div>`;

  box.innerHTML = `
    <div class="winserver-monitor-head">
      <div>
        <div class="winserver-monitor-host">${escapeHtml(m.hostName || "未知主机")}</div>
        <div class="winserver-monitor-os">${escapeHtml(m.os || "")} ${escapeHtml(m.osVersion || "")} · 已运行 ${escapeHtml(m.uptimeText || "—")} · ${m.cpuCores || 0} 核</div>
      </div>
      <button id="winserver-refresh-monitor" type="button" class="button secondary"><span class="icon" style="--icon:url('./icons/refresh-cw.svg')"></span><span>刷新</span></button>
    </div>
    <div class="winserver-gauges">
      ${gauge("CPU", m.cpuPercent || 0, `${m.cpuPercent || 0}% 平均负载`)}
      ${gauge("内存", m.memoryPercent || 0, `${formatBytes(m.memoryUsedBytes)} / ${formatBytes(m.memoryTotalBytes)}`)}
      ${gauge("系统盘", m.diskPercent || 0, `${formatBytes(m.diskUsedBytes)} / ${formatBytes(m.diskTotalBytes)}`)}
    </div>
    ${renderWinPowerItems(p)}
  `;
}

/// 远端电源/RDP 配置的达标情况。
function renderWinPowerItems(power) {
  if (!power || (power.sleepSeconds === undefined && power.sleepSeconds !== 0)) return "";
  const items = [
    {
      title: "在此时间后睡眠",
      ok: power.sleepSeconds === 0,
      value: power.sleepSeconds === 0 ? "从不" : `${Math.round(power.sleepSeconds / 60)} 分钟`,
      reason: "RDP 断开后没人动鼠标，超时就进睡眠，远程直接断。",
    },
    {
      title: "在此时间后关闭显示",
      ok: power.videoSeconds === 0,
      value: power.videoSeconds === 0 ? "从不" : `${Math.round(power.videoSeconds / 60)} 分钟`,
      reason: "这就是「息屏」的来源，跟睡眠是两套设置。",
    },
    {
      title: "休眠文件",
      ok: !power.hibernateFilePresent,
      value: power.hibernateFilePresent ? "已启用" : "已禁用",
      reason: "开着时「关机」其实是休眠，重启远程也救不回来。",
    },
    {
      title: "每用户单会话",
      ok: !power.singleSessionPerUser,
      value: power.singleSessionPerUser ? "开启" : "关闭",
      reason: "开启时同名用户重连会踢掉上一个会话 —— 这就是「必须用子用户」的原因。",
    },
    {
      title: "屏幕保护程序",
      ok: !power.screenSaverActive,
      value: power.screenSaverActive ? "已启用" : "已禁用",
      reason: "屏保会遮住画面，让人误以为远程掉线了。",
    },
  ];
  return `
    <div class="winserver-power-list">
      <div class="env-versions-title"><span class="icon" style="--icon:url('./icons/power.svg')"></span><span>断连后会怎样</span>
        ${power.schemeName ? `<span class="env-versions-tag">电源计划 ${escapeHtml(power.schemeName)}</span>` : ""}
      </div>
      ${items.map((item) => `
        <div class="winserver-item ${item.ok ? "is-ok" : "is-bad"}">
          <span class="winserver-item-dot"></span>
          <div class="winserver-item-main">
            <div class="winserver-item-title">
              ${escapeHtml(item.title)}
              <span class="winserver-item-value">${escapeHtml(item.value)}</span>
            </div>
            <div class="winserver-item-reason">${escapeHtml(item.reason)}</div>
          </div>
        </div>
      `).join("")}
    </div>`;
}

function renderWinServerFixes() {
  const box = byId("winserver-fixes");
  if (!box) return;
  if (!winSrvFixes.length) {
    box.innerHTML = `<div class="winserver-item-reason">正在读取修复项…</div>`;
    return;
  }
  // 用 div + 显式 label for/id 关联，**不用 <label> 包住整个条目**。
  // <label> 作为 flex 容器时，Chromium 会给它内部的 div 算出 0 宽度
  // （内容竖排、每字一行，item 高得离谱）—— 实测踩过，很难一眼看出来。
  // 拆成 sibling 的 input + label 更稳。
  box.innerHTML = winSrvFixes.map((fix) => `
    <div class="winserver-fix has-checkbox">
      <input type="checkbox" id="winsrv-fix-${escapeHtml(fix.id)}" value="${escapeHtml(fix.id)}"${fix.needAdmin ? ' title="需要管理员权限"' : ""}>
      <div class="winserver-fix-main">
        <label class="winserver-fix-title" for="winsrv-fix-${escapeHtml(fix.id)}">${escapeHtml(fix.title)}${fix.needAdmin ? ' <span class="winserver-item-tag">需管理员</span>' : ""}</label>
        <div class="winserver-fix-reason">${escapeHtml(fix.reason)}</div>
        <div class="winserver-fix-command">${escapeHtml(fix.script)}</div>
      </div>
    </div>
  `).join("");
  updateWinSrvApplyButton();
}

function renderWinServerUsers() {
  const box = byId("winserver-user-list");
  if (!box) return;
  if (!winSrvUsers) {
    box.innerHTML = `<div class="winserver-item-reason">点「刷新用户列表」读取远端用户</div>`;
    return;
  }
  const users = Array.isArray(winSrvUsers.users) ? winSrvUsers.users : [];
  if (!users.length) {
    box.innerHTML = `<div class="winserver-item-reason">远端没有读到用户</div>`;
    return;
  }
  box.innerHTML = users.map((user) => `
    <span class="winserver-user-chip ${user.rdpMember ? "is-rdp" : ""}"
      title="${user.rdpMember ? "在「远程桌面用户」组里，可以远程登录" : "不在该组里，远程桌面登录会被拒绝"}">${escapeHtml(user.name)}${user.rdpMember ? " ·可远程" : ""}</span>
  `).join("");
}

async function loadWinServerFixes() {
  if (winSrvFixes.length) {
    renderWinServerFixes();
    return;
  }
  try {
    winSrvFixes = (await invoke("winSrvFixes")) || [];
  } catch (_error) {
    winSrvFixes = [];
  }
  renderWinServerFixes();
}

async function refreshWinServerUsers() {
  if (!winSrvSelected) {
    showToast("error", "先选一台服务器");
    return;
  }
  const box = byId("winserver-user-list");
  if (box) box.innerHTML = `<div class="winserver-item-reason">正在读取远端用户…</div>`;
  try {
    winSrvUsers = await invoke("winSrvUsers", winSrvSelected);
    renderWinServerUsers();
  } catch (error) {
    if (box) {
      box.innerHTML = `<div class="winserver-item-reason">读取失败：${escapeHtml(error.message || error)}</div>`;
    }
  }
}

function updateWinSrvApplyButton() {
  const button = byId("winserver-apply-selected");
  if (!button) return;
  const count = document.querySelectorAll("#winserver-fixes input[type=checkbox]:checked").length;
  button.disabled = !count || !winSrvSelected;
  button.textContent = count ? `在远端执行 ${count} 项` : "在远端执行";
}

async function applyWinServerFixes() {
  if (!winSrvSelected) {
    showToast("error", "先选一台服务器");
    return;
  }
  const ids = [...document.querySelectorAll("#winserver-fixes input[type=checkbox]:checked")]
    .map((input) => input.value);
  if (!ids.length) {
    showToast("error", "先勾选要执行的修复项");
    return;
  }
  const chunks = [];
  for (const id of ids) {
    try {
      const result = await invoke("winSrvRunFix", winSrvSelected, id);
      chunks.push(`### ${id}\n${result?.detail || "已执行"}`);
    } catch (error) {
      chunks.push(`### ${id} —— 失败\n${error.message || error}`);
    }
  }
  const body = byId("winserver-detail-body");
  if (body) body.textContent = chunks.join("\n\n");
  const panel = byId("winserver-detail");
  if (panel) {
    panel.hidden = false;
    panel.scrollIntoView({ block: "nearest", behavior: "smooth" });
  }
  // 执行完重采一次，让达标状态立刻更新
  await refreshWinServerMonitor();
}

async function createWinServerUser() {
  if (!winSrvSelected) {
    showToast("error", "先选一台服务器");
    return;
  }
  const name = byId("winserver-username").value.trim();
  const password = byId("winserver-password").value;
  const admin = byId("winserver-admin").checked;
  if (!name) {
    showToast("error", "先填用户名");
    return;
  }
  try {
    const result = await invoke("winSrvCreateUser", winSrvSelected, name, password, admin);
    const body = byId("winserver-command-body");
    if (body) body.textContent = result.detail;
    const panel = byId("winserver-command");
    if (panel) panel.hidden = false;
    byId("winserver-password").value = "";
    await refreshWinServerUsers();
  } catch (error) {
    showToast("error", error.message || "创建失败");
  }
}

async function copyWinServerCommand() {
  const text = byId("winserver-command-body").textContent;
  try {
    if (navigator.clipboard && navigator.clipboard.writeText) {
      await navigator.clipboard.writeText(text);
    } else {
      // WebView2 在非安全上下文里可能不给 clipboard API，退回老办法
      const area = document.createElement("textarea");
      area.value = text;
      document.body.append(area);
      area.select();
      document.execCommand("copy");
      area.remove();
    }
    showToast("success", "已复制");
  } catch (_error) {
    showToast("error", "复制失败，请手动选中复制");
  }
}

async function openWinServerRdpForSelected() {
  if (!winSrvSelected) {
    showToast("error", "先选一台服务器");
    return;
  }
  const server = winServers().find((item) => item.profileId === winSrvSelected);
  if (!server) return;
  invoke("winServerOpenRdp", server.host);
}

/** 添加 / 编辑 Windows 服务器。字段比 Linux 少 —— 不需要代理端口。 */
function openWinServerDialog(server = null) {
  const authMethod = server?.authMethod || "password";
  const hasSavedCredential = Boolean(server?.hasCredential);
  openModal(server ? "编辑 Windows 服务器" : "添加 Windows 服务器", `
    <div class="form-grid ssh-form-grid">
      <label class="wide-field"><span>名称</span><input id="modal-winsrv-name" value="${escapeHtml(server?.name || "我的 Windows 服务器")}" required></label>
      <label><span>服务器 IP / 域名</span><input id="modal-winsrv-host" value="${escapeHtml(server?.host || "")}" placeholder="例如 192.168.1.10" required></label>
      <label><span>SSH 端口</span><input id="modal-winsrv-port" type="number" min="1" max="65535" value="${server?.port || 22}" required></label>
      <label><span>用户名</span><input id="modal-winsrv-username" value="${escapeHtml(server?.username || "Administrator")}" required></label>
      <label><span>认证方式</span><select id="modal-winsrv-auth"><option value="password"${authMethod === "password" ? " selected" : ""}>账号密码</option><option value="key"${authMethod === "key" ? " selected" : ""}>私钥文件</option><option value="agent"${authMethod === "agent" ? " selected" : ""}>SSH Agent</option></select></label>
      <label id="modal-winsrv-secret-field"><span>密码 / 私钥口令</span><input id="modal-winsrv-password" type="password" autocomplete="new-password" placeholder="${server?.hasCredential ? "已安全保存，留空保持不变" : "连接凭据"}"></label>
      <label id="modal-winsrv-key-field" class="wide-field"><span>私钥路径</span><div class="input-action"><input id="modal-winsrv-key-path" value="${escapeHtml(server?.keyPath || "")}" placeholder="选择 OpenSSH 私钥"><button id="modal-pick-winsrv-key" type="button" class="icon-button" title="选择私钥" aria-label="选择私钥">${icon("folder-open")}</button></div></label>
      <label class="switch-row wide-field"><span><strong>记住凭据</strong><small>${hasSavedCredential ? "已保存密码；留空即保持不变" : "Windows 下使用当前用户 DPAPI 加密"}</small></span><input id="modal-winsrv-remember" type="checkbox"${hasSavedCredential ? " checked" : ""}><i></i></label>
      <div class="group-dialog-summary wide-field"><strong>怎么连</strong><span>通过 SSH（OpenSSH）连到目标机执行命令。<strong>Windows Server 2019 及以后内置 OpenSSH 服务器</strong>，在「设置 → 应用 → 可选功能」里装上即可；工作组机器也能用，不需要配证书。</span></div>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "保存", kind: "primary", action: () => {
      const payload = {
        profileId: server?.profileId || "",
        name: byId("modal-winsrv-name").value,
        region: "",
        host: byId("modal-winsrv-host").value,
        port: Number(byId("modal-winsrv-port").value),
        username: byId("modal-winsrv-username").value,
        proxyPort: 0,
        authMethod: byId("modal-winsrv-auth").value,
        keyPath: byId("modal-winsrv-key-path").value,
        rememberPassword: byId("modal-winsrv-remember").checked,
        kind: "windows",
      };
      invoke("saveSshServer", JSON.stringify(payload), byId("modal-winsrv-password").value);
      closeModal();
    } },
  ]);
  const updateAuthFields = () => {
    const method = byId("modal-winsrv-auth").value;
    byId("modal-winsrv-key-field").classList.toggle("is-hidden", method !== "key");
    byId("modal-winsrv-secret-field").classList.toggle("is-hidden", method === "agent");
    byId("modal-winsrv-remember").disabled = method === "agent";
  };
  byId("modal-winsrv-auth").addEventListener("change", updateAuthFields);
  byId("modal-winsrv-password").addEventListener("input", (event) => {
    if (event.target.value) byId("modal-winsrv-remember").checked = true;
  });
  byId("modal-pick-winsrv-key").addEventListener("click", async () => {
    const path = await invoke("pickSshKey");
    if (path) byId("modal-winsrv-key-path").value = path;
  });
  updateAuthFields();
}

/* ------------------------------------------------------------------
   远程截图 + 断开后保持运行
   ------------------------------------------------------------------ */
let winSrvShot = null;
let winSrvKeepAlive = [];
let winSrvKeepAliveStatus = null;
let winSrvKeepAliveHardware = [];
let winSrvShotBusy = false;

async function captureWinServerScreen() {
  if (!winSrvSelected) {
    showToast("error", "先选一台服务器");
    return;
  }
  if (winSrvShotBusy) return;
  winSrvShotBusy = true;
  const box = byId("winserver-shot-box");
  const info = byId("winserver-shot-info");
  const button = byId("winserver-shot");
  button.disabled = true;
  box.innerHTML = `<div class="winserver-item-reason">正在抓取屏幕…`;
  info.textContent = "抓取中…";
  try {
    const result = await invoke("serverScreenshot", winSrvSelected);
    winSrvShot = result;
    renderWinServerShot(result);
  } catch (error) {
    box.innerHTML = `<div class="winserver-item-reason">抓取失败：${escapeHtml(error.message || error)}</div>`;
    info.textContent = "抓取失败";
  } finally {
    winSrvShotBusy = false;
    button.disabled = false;
  }
}

function renderWinServerShot(result) {
  const box = byId("winserver-shot-box");
  const info = byId("winserver-shot-info");
  if (!result || !result.data) {
    box.innerHTML = `<div class="winserver-item-reason">没有拿到图片</div>`;
    return;
  }
  info.textContent = `${result.width}×${result.height} · ${result.method} · ${Math.round((result.sizeBytes || 0) / 1024)} KB`;
  // data URI 直接进 img 的 src。图片已经在后端压到 1280 宽以内，
  // base64 后也就百来 KB，一次带过来比再设计一套图片下载接口简单。
  box.innerHTML = `
    <div class="winserver-shot-frame">
      <img src="${result.data}" alt="服务器屏幕画面" class="winserver-shot-img">
    </div>
    <div class="winserver-shot-foot">
      <span>${escapeHtml(result.method || "")}</span>
      <button type="button" class="text-button" data-shot-download>下载原图</button>
    </div>`;
}

function downloadWinServerShot() {
  if (!winSrvShot || !winSrvShot.data) return;
  // data URI → blob → 触发下载
  const link = document.createElement("a");
  link.href = winSrvShot.data;
  link.download = `server-screen-${Date.now()}.png`;
  document.body.append(link);
  link.click();
  link.remove();
}

/** 加载保活清单与当前状态。 */
async function loadWinServerKeepAlive() {
  if (!winSrvSelected) return;
  const kind = currentWinSrvKind();
  try {
    const list = await invoke("keepAliveList", kind);
    winSrvKeepAlive = Array.isArray(list?.items) ? list.items : [];
    winSrvKeepAliveHardware = Array.isArray(list?.hardware) ? list.hardware : [];
    renderWinServerKeepAliveList(winSrvKeepAliveHardware);
    await refreshWinServerKeepAliveStatus();
  } catch (error) {
    byId("winserver-keepalive-list").innerHTML =
      `<div class="winserver-item-reason">读取失败：${escapeHtml(error.message || error)}</div>`;
  }
}

/** 当前选中服务器的类型（linux / windows）。 */
function currentWinSrvKind() {
  const server = winServers().find((item) => item.profileId === winSrvSelected);
  return server && server.kind === "windows" ? "windows" : "linux";
}

async function refreshWinServerKeepAliveStatus() {
  if (!winSrvSelected) return;
  const box = byId("winserver-keepalive-items");
  try {
    const status = await invoke("keepAliveStatus", winSrvSelected);
    winSrvKeepAliveStatus = status;
    // 状态到位后重画清单，条目上的「已达标/未达标」徽章才会出现
    // （清单是先渲染的，那时还没有状态）
    renderWinServerKeepAliveList(winSrvKeepAliveHardware || []);
    const items = Array.isArray(status?.items) ? status.items : [];
    if (!items.length) {
      box.innerHTML = `<div class="winserver-item-reason">没有可检查的项</div>`;
      return;
    }
    box.innerHTML = items.map((item) => `
      <div class="winserver-item ${item.ok ? "is-ok" : "is-bad"}">
        <span class="winserver-item-dot"></span>
        <div class="winserver-item-main">
          <div class="winserver-item-title">
            ${escapeHtml(titleOfKeepAlive(item.id))}
            <span class="winserver-item-value">${escapeHtml(item.value || "")}</span>
          </div>
        </div>
      </div>
    `).join("");
  } catch (error) {
    box.innerHTML = `<div class="winserver-item-reason">检查失败：${escapeHtml(error.message || error)}</div>`;
  }
}

/** 从保活清单里取标题（状态检查只返回 id 与值）。 */
function titleOfKeepAlive(id) {
  const found = winSrvKeepAlive.find((item) => item.id === id);
  return found ? found.title : id;
}

function renderWinServerKeepAliveList(hardware) {
  const box = byId("winserver-keepalive-list");
  if (!winSrvKeepAlive.length) {
    box.innerHTML = `<div class="winserver-item-reason">正在读取…</div>`;
  } else {
    box.innerHTML = winSrvKeepAlive.map((item) => {
      const status = (winSrvKeepAliveStatus?.items || []).find((s) => s.id === item.id);
      const done = status ? status.ok : null;
      return `
        <div class="winserver-fix has-action">
          <div class="winserver-fix-main">
            <div class="winserver-fix-title">
              ${escapeHtml(item.title)}
              ${item.needAdmin ? ' <span class="winserver-item-tag">需管理员</span>' : ""}
              ${done === true ? ' <span class="winserver-keepalive-ok">已达标</span>'
                : done === false ? ' <span class="winserver-keepalive-bad">未达标</span>' : ""}
            </div>
            <div class="winserver-fix-reason">${escapeHtml(item.reason)}</div>
            <div class="winserver-fix-command">${escapeHtml(item.script)}</div>
          </div>
          <button type="button" class="button secondary" data-keepalive-apply="${escapeHtml(item.id)}">${icon("play")}<span>执行</span></button>
        </div>`;
    }).join("");
  }
  const hw = byId("winserver-hardware");
  const list = Array.isArray(hardware) ? hardware : [];
  hw.innerHTML = list.length
    ? list.map((item) => `
        <div class="winserver-hardware-item">
          <div class="winserver-hardware-title">${escapeHtml(item.title)}</div>
          <div class="winserver-hardware-why">${escapeHtml(item.why)}</div>
          <div class="winserver-hardware-where"><strong>去哪儿配：</strong>${escapeHtml(item.where)}</div>
        </div>`).join("")
    : `<div class="winserver-item-reason">没有硬件层条目</div>`;
}

async function applyWinServerKeepAlive(id) {
  if (!winSrvSelected) {
    showToast("error", "先选一台服务器");
    return;
  }
  try {
    const result = await invoke("keepAliveApply", winSrvSelected, id);
    const body = byId("winserver-detail-body");
    if (body) body.textContent = result?.detail || "已执行";
    const panel = byId("winserver-detail");
    if (panel) {
      panel.hidden = false;
      panel.scrollIntoView({ block: "nearest", behavior: "smooth" });
    }
    // 状态立刻重查，达标标记当场更新
    await refreshWinServerKeepAliveStatus();
  } catch (error) {
    showToast("error", error.message || "执行失败");
  }
}

/* ------------------------------------------------------------------
   服务器管理页 · 文件快传

   复用的是「本机 ⇄ 远端」那条 SSH 连接。后端实现是逐文件 + 分块 base64
   （手写的 SSH 客户端只有 exec 通道，没有 SFTP），所以：
   - 断在中途不会有"半个文件"—— 每个文件重传时第一块是 `>` 截断；
   - 增量与镜像是**按文件**比对（大小 + 修改时间），不是整包哈希；
   - 目录会递归，空目录不保留（没有文件的目录没有传输意义）。

   镜像会删东西，所以按钮做成了「点两次」的两段式确认 ——
   不依赖 window.confirm（wry 里对 script dialog 的支持不保证）。
   ------------------------------------------------------------------ */
let xferMode = "upload";
/** 勾选的服务器，**保持点击顺序** —— 服务器互传要靠顺序区分源和目标。 */
let xferPicked = [];
let xferBusy = false;
let xferBrowsePath = "";
let xferMirrorArmed = false;

function xferServers() {
  return (appState?.sshServers || []).map((server) => ({
    profileId: server.profileId,
    name: server.name || server.host,
    host: server.host,
    port: server.port,
    kind: server.kind === "windows" ? "windows" : "linux",
  }));
}

function xferServerById(profileId) {
  return xferServers().find((server) => server.profileId === profileId) || null;
}

function renderXferServers() {
  const box = byId("xfer-server-list");
  if (!box) return;
  const servers = xferServers();
  if (!servers.length) {
    box.innerHTML = `<div class="winserver-item-reason">还没有服务器。先去「Linux 服务器」或「Windows 服务器」加一台。</div>`;
    updateXferHint();
    return;
  }
  box.innerHTML = servers.map((server, index) => {
    const order = xferPicked.indexOf(server.profileId);
    const picked = order >= 0;
    return `
      <button type="button" class="xfer-server ${picked ? "is-picked" : ""}" data-xfer-pick="${escapeHtml(server.profileId)}">
        <span class="xfer-server-box">${picked ? order + 1 : ""}</span>
        <span class="xfer-server-main">
          <span class="xfer-server-name">${escapeHtml(server.name)}</span>
          <span class="xfer-server-host">${escapeHtml(server.host)}:${server.port}</span>
        </span>
        <span class="xfer-server-kind">${server.kind === "windows" ? "Win" : "Linux"}</span>
      </button>`;
  }).join("");
  updateXferHint();
}

function toggleXferPick(profileId) {
  const index = xferPicked.indexOf(profileId);
  if (index >= 0) xferPicked.splice(index, 1);
  else xferPicked.push(profileId);
  // 换服务器等于换了目标机，之前浏览出来的目录清单不再对得上
  xferBrowsePath = "";
  byId("xfer-browse-box").innerHTML = "";
  renderXferServers();
  armXferMirror(false);
}

function updateXferHint() {
  const hint = byId("xfer-hint");
  const start = byId("xfer-start");
  if (!hint || !start) return;
  const count = xferPicked.length;
  let text = "";
  if (xferMode === "move") {
    text = count === 2
      ? `${xferServerById(xferPicked[0])?.name || "源"} → ${xferServerById(xferPicked[1])?.name || "目标"}`
      : `勾两台（现在是 ${count} 台）：先源后目标`;
  } else if (count === 0) {
    text = "先在左边勾一台服务器";
  } else if (count === 1) {
    text = xferServerById(xferPicked[0])?.name || "";
  } else {
    text = `已勾 ${count} 台，会依次推过去（后端一批 4 台）`;
  }
  hint.textContent = text;
  const label = start.querySelector("span:last-child");
  if (label) {
    label.textContent = xferMirrorArmed
      ? "再点一次：确认删除"
      : xferMode === "move" ? "开始中转" : xferMode === "download" ? "下载到本机" : count > 1 ? `推送到 ${count} 台` : "开始传输";
  }
  start.classList.toggle("is-danger", xferMirrorArmed);
}

function setXferMode(mode) {
  if (!["upload", "download", "move"].includes(mode)) return;
  xferMode = mode;
  document.querySelectorAll("[data-xfer-mode]").forEach((button) => {
    button.classList.toggle("is-active", button.dataset.xferMode === mode);
  });
  const local = byId("xfer-local");
  const remote = byId("xfer-remote");
  if (mode === "upload") {
    local.placeholder = "C:\\Users\\me\\build";
    remote.placeholder = "/var/www/site";
  } else if (mode === "download") {
    local.placeholder = "D:\\backup\\site";
    remote.placeholder = "/var/www/site";
  } else {
    local.placeholder = "（服务器之间不需要本机路径）";
    remote.placeholder = "/var/www/site";
  }
  local.disabled = mode === "move";
  byId("xfer-pick-file").disabled = mode === "move";
  byId("xfer-pick-dir").disabled = mode === "move";
  armXferMirror(false);
  updateXferHint();
}

/** 镜像的两段式确认：勾上之后第一次点只是「上膛」，第二次才真的动手。 */
function armXferMirror(armed) {
  xferMirrorArmed = armed;
  updateXferHint();
}

async function pickXferLocal(selectDirectory) {
  try {
    const picked = await invoke("fileTransferPickLocal", selectDirectory);
    if (picked) {
      byId("xfer-local").value = picked;
      armXferMirror(false);
    }
  } catch (error) {
    showToast("error", error.message || "选择失败");
  }
}

function joinRemotePath(base, name) {
  const trimmed = String(base || "").replace(/[\\/]+$/, "");
  if (!trimmed) return name;
  return `${trimmed}/${name}`;
}

async function browseXferRemote(path) {
  if (!xferPicked.length) {
    showToast("error", "先在左边勾一台服务器");
    return;
  }
  const box = byId("xfer-browse-box");
  const target = xferPicked[0];
  const wanted = path || byId("xfer-remote").value.trim() || (xferServerById(target)?.kind === "windows" ? "C:/" : "/");
  box.innerHTML = `<div class="winserver-item-reason">正在列目录…</div>`;
  try {
    const result = await invoke("fileTransferBrowse", target, wanted);
    xferBrowsePath = result.path || wanted;
    byId("xfer-remote").value = xferBrowsePath;
    armXferMirror(false);
    renderXferBrowse(result);
  } catch (error) {
    box.innerHTML = `<div class="winserver-item-reason">列目录失败：${escapeHtml(error.message || error)}</div>`;
  }
}

function renderXferBrowse(result) {
  const box = byId("xfer-browse-box");
  const entries = Array.isArray(result?.entries) ? result.entries : [];
  const parent = result?.parent || "";
  const rows = [];
  if (parent) {
    rows.push(`<button type="button" class="xfer-entry is-dir" data-xfer-enter="${escapeHtml(parent)}">
      <span class="xfer-entry-icon">${icon("arrow-up")}</span><span class="xfer-entry-name">..</span></button>`);
  }
  for (const entry of entries) {
    const name = entry.name || "";
    const isDir = entry.kind === "dir";
    const meta = isDir ? "目录" : formatBytes(entry.size || 0);
    const attr = isDir
      ? `data-xfer-enter="${escapeHtml(joinRemotePath(xferBrowsePath, name))}"`
      : `data-xfer-file="${escapeHtml(joinRemotePath(xferBrowsePath, name))}"`;
    rows.push(`<button type="button" class="xfer-entry ${isDir ? "is-dir" : ""}" ${attr}>
      <span class="xfer-entry-icon">${icon(isDir ? "folder-open" : "file-text")}</span>
      <span class="xfer-entry-name">${escapeHtml(name)}</span>
      <span class="xfer-entry-meta">${escapeHtml(meta)}</span></button>`);
  }
  box.innerHTML = `
    <div class="xfer-browse-head">
      <span>${escapeHtml(result?.path || xferBrowsePath)}</span>
      <span>${entries.length} 项</span>
    </div>
    <div class="xfer-browse-list">${rows.join("") || `<div class="winserver-item-reason">空目录</div>`}</div>`;
}

async function startXfer() {
  if (xferBusy) return;
  const local = byId("xfer-local").value.trim();
  const remote = byId("xfer-remote").value.trim();
  const sync = byId("xfer-sync").checked;
  const mirror = byId("xfer-mirror").checked;

  if (!xferPicked.length) {
    showToast("error", "先在左边勾一台服务器");
    return;
  }
  if (xferMode === "move" && xferPicked.length !== 2) {
    showToast("error", "服务器之间要正好勾两台：先源后目标");
    return;
  }
  if (xferMode !== "move" && !local) {
    showToast("error", "先填本机路径");
    return;
  }
  if (!remote) {
    showToast("error", "先填远端路径");
    return;
  }
  if (mirror && !xferMirrorArmed) {
    armXferMirror(true);
    showToast("info", "镜像会删掉目标端多出来的文件，再点一次才动手");
    return;
  }

  xferBusy = true;
  const start = byId("xfer-start");
  start.disabled = true;
  byId("xfer-result").innerHTML = `<div class="winserver-item-reason">正在传输…大目录可能要等一会儿，别关窗口</div>`;
  try {
    if (xferMode === "upload" && xferPicked.length > 1) {
      const result = await invoke("fileTransferPushMany", xferPicked, local, remote, sync, mirror);
      renderXferMulti(result);
    } else if (xferMode === "upload") {
      const stats = await invoke("fileTransferUpload", xferPicked[0], local, remote, sync, mirror);
      renderXferStats(`${xferServerById(xferPicked[0])?.name || "服务器"} · 已上传`, stats);
    } else if (xferMode === "download") {
      const stats = await invoke("fileTransferDownload", xferPicked[0], remote, local, sync, mirror);
      renderXferStats(`${xferServerById(xferPicked[0])?.name || "服务器"} · 已下载`, stats);
    } else {
      const stats = await invoke("fileTransferMove", xferPicked[0], xferPicked[1], remote, mirror);
      renderXferStats(`${xferServerById(xferPicked[0])?.name || "源"} → ${xferServerById(xferPicked[1])?.name || "目标"}`, stats);
    }
  } catch (error) {
    byId("xfer-result").innerHTML = `<div class="winserver-item-reason">传输失败：${escapeHtml(error.message || error)}</div>`;
    showToast("error", error.message || "传输失败");
  } finally {
    xferBusy = false;
    start.disabled = false;
    armXferMirror(false);
  }
}

function renderXferStats(title, stats) {
  const notes = Array.isArray(stats?.notes) ? stats.notes : [];
  const bits = [`${stats?.files || 0} 个文件`, formatBytes(stats?.bytes || 0)];
  if (stats?.skipped) bits.push(`跳过 ${stats.skipped}`);
  if (stats?.removed) bits.push(`删除 ${stats.removed}`);
  byId("xfer-result").innerHTML = `
    <div class="xfer-stat-head">
      <strong>${escapeHtml(title)}</strong>
      <span>${escapeHtml(bits.join(" · "))}</span>
    </div>
    <div class="xfer-notes">${notes
      .map((note) => `<div class="xfer-note">${escapeHtml(note)}</div>`)
      .join("") || `<div class="winserver-item-reason">没有可展示的明细</div>`}</div>`;
}

function renderXferMulti(result) {
  const rows = Array.isArray(result?.results) ? result.results : [];
  byId("xfer-result").innerHTML = `
    <div class="xfer-stat-head">
      <strong>批量推送</strong>
      <span>${result?.succeeded || 0} 台成功 · ${result?.failed || 0} 台失败</span>
    </div>
    <div class="xfer-notes">${rows
      .map((row) => `<div class="xfer-note ${row.ok ? "is-ok" : "is-bad"}">
        <span>${row.ok ? "✓" : "✕"}</span>
        <strong>${escapeHtml(row.name || row.profileId || "未知")}</strong>
        <span>${escapeHtml(row.detail || "")}</span>
      </div>`)
      .join("")}</div>`;
}

function closeCustomSelects(except = null) {  document.querySelectorAll(".custom-select.is-open").forEach((root) => {
    if (root === except) return;
    root.classList.remove("is-open");
    root.classList.remove("opens-up");
    root.querySelector(".custom-select-trigger")?.setAttribute("aria-expanded", "false");
  });
}

function enhanceSelect(select) {
  if (select.dataset.enhanced === "true") return;
  select.dataset.enhanced = "true";
  select.classList.add("enhanced-native-select");

  const root = document.createElement("div");
  root.className = "custom-select";
  const trigger = document.createElement("button");
  trigger.type = "button";
  trigger.className = "custom-select-trigger";
  trigger.setAttribute("aria-haspopup", "listbox");
  trigger.setAttribute("aria-expanded", "false");
  const menu = document.createElement("div");
  menu.className = "custom-select-menu";
  menu.setAttribute("role", "listbox");

  const sync = () => {
    const selected = select.selectedOptions[0];
    trigger.innerHTML = `<span>${escapeHtml(selected?.textContent || "请选择")}</span>${icon("chevron-down")}`;
    menu.querySelectorAll(".custom-select-option").forEach((item) => {
      const active = item.dataset.value === select.value;
      item.classList.toggle("is-selected", active);
      item.setAttribute("aria-selected", String(active));
    });
  };

  [...select.options].forEach((option) => {
    const item = document.createElement("button");
    item.type = "button";
    item.className = "custom-select-option";
    item.dataset.value = option.value;
    item.disabled = option.disabled;
    item.setAttribute("role", "option");
    item.innerHTML = `<span>${escapeHtml(option.textContent)}</span>${icon("check")}`;
    item.addEventListener("click", (event) => {
      event.preventDefault();
      event.stopPropagation();
      if (select.value !== option.value) {
        select.value = option.value;
        select.dispatchEvent(new Event("change", { bubbles: true }));
      }
      sync();
      closeCustomSelects();
      trigger.focus();
    });
    menu.append(item);
  });

  trigger.addEventListener("click", (event) => {
    event.preventDefault();
    event.stopPropagation();
    const opening = !root.classList.contains("is-open");
    closeCustomSelects(root);
    root.classList.toggle("is-open", opening);
    trigger.setAttribute("aria-expanded", String(opening));
    if (opening) {
      const boundary = root.closest(".modal-content")?.getBoundingClientRect()
        || { top: 8, bottom: window.innerHeight - 8 };
      const triggerRect = trigger.getBoundingClientRect();
      const menuHeight = Math.min(menu.scrollHeight, 230) + 6;
      const roomBelow = boundary.bottom - triggerRect.bottom;
      const roomAbove = triggerRect.top - boundary.top;
      root.classList.toggle("opens-up", roomBelow < menuHeight && roomAbove > roomBelow);
      menu.querySelector(".is-selected")?.scrollIntoView({ block: "nearest" });
    }
  });
  trigger.addEventListener("keydown", (event) => {
    if (!["ArrowDown", "ArrowUp", "Escape"].includes(event.key)) return;
    event.preventDefault();
    if (event.key === "Escape") {
      closeCustomSelects();
      return;
    }
    const options = [...menu.querySelectorAll(".custom-select-option:not(:disabled)")];
    const index = Math.max(0, options.findIndex((item) => item.dataset.value === select.value));
    const offset = event.key === "ArrowDown" ? 1 : -1;
    options[(index + offset + options.length) % options.length]?.click();
  });

  root.append(trigger, menu);
  select.insertAdjacentElement("afterend", root);
  select.addEventListener("change", sync);
  sync();
}

function enhanceSelects(container) {
  container.querySelectorAll("select").forEach(enhanceSelect);
}

function rebuildSelectEnhancement(select) {
  if (select.dataset.enhanced === "true") {
    const customSelect = select.nextElementSibling;
    if (customSelect?.classList.contains("custom-select")) customSelect.remove();
    delete select.dataset.enhanced;
    select.classList.remove("enhanced-native-select");
  }
  enhanceSelect(select);
}

function splitRuleValues(value) {
  return String(value || "").split(/\r?\n/).map((item) => item.trim()).filter(Boolean);
}

function uniqueRuleValues(value) {
  const values = splitRuleValues(value);
  const seen = new Set();
  return values.filter((item) => {
    const key = item.toLocaleLowerCase();
    if (seen.has(key)) return false;
    seen.add(key);
    return true;
  });
}

function updateRuleValueSummary(textareaId, summaryId) {
  const textarea = byId(textareaId);
  const summary = byId(summaryId);
  if (!textarea || !summary) return;
  const values = splitRuleValues(textarea.value);
  const unique = uniqueRuleValues(textarea.value);
  const duplicateCount = values.length - unique.length;
  summary.textContent = duplicateCount
    ? `${unique.length} 条有效内容 · ${duplicateCount} 条重复`
    : `${unique.length} 条匹配内容`;
  summary.classList.toggle("has-warning", duplicateCount > 0);
}

function normalizeRuleValueEditor(textareaId, summaryId) {
  const textarea = byId(textareaId);
  if (!textarea) return;
  textarea.value = uniqueRuleValues(textarea.value).join("\n");
  updateRuleValueSummary(textareaId, summaryId);
  textarea.focus();
}

function findRuleValue(textarea, search, fromStart = false) {
  const query = search.value.trim();
  if (!query) return;
  const source = textarea.value.toLocaleLowerCase();
  const needle = query.toLocaleLowerCase();
  const start = fromStart ? 0 : Math.max(textarea.selectionEnd, 0);
  let index = source.indexOf(needle, start);
  if (index < 0 && start > 0) index = source.indexOf(needle);
  if (index < 0) {
    showToast("error", `没有找到“${query}”`);
    return;
  }
  textarea.focus();
  textarea.setSelectionRange(index, index + query.length);
}

function updateProcessPickerCount() {
  const count = document.querySelectorAll('#modal-rule-process-list input[type="checkbox"]:checked').length;
  const label = byId("modal-rule-process-count");
  if (label) label.textContent = count ? `已选择 ${count} 个程序` : "选择正在运行的程序";
}

function syncRuleValueEditor() {
  const processRule = byId("modal-rule-type")?.value === "PROCESS-NAME";
  byId("modal-rule-process-field")?.classList.toggle("is-hidden", !processRule);
  byId("modal-rule-lines-field")?.classList.toggle("is-hidden", processRule);
}

function openRuleDialog(rule = null) {
  const typeOptions = RULE_TYPES.map(([value, label]) => `<option value="${value}"${rule?.ruleType === value ? " selected" : ""}>${label}</option>`).join("");
  const targetOptions = TARGETS.map(([value, label]) => `<option value="${value}"${rule?.target === value ? " selected" : ""}>${label}</option>`).join("");
  const selectedProcesses = new Set(rule?.ruleType === "PROCESS-NAME" ? [rule.value] : []);
  const runningProcesses = [...new Set([
    ...(appState.runningProcesses || []),
    ...selectedProcesses,
  ])].sort((left, right) => left.localeCompare(right, undefined, { sensitivity: "base" }));
  const processOptions = runningProcesses.map((name, index) => `
    <label class="process-picker-option" data-process-search="${escapeHtml(name.toLowerCase())}">
      <input id="modal-process-${index}" type="checkbox" value="${escapeHtml(name)}"${selectedProcesses.has(name) ? " checked" : ""}>
      <span title="${escapeHtml(name)}">${escapeHtml(name)}</span>
    </label>`).join("") || '<div class="process-picker-empty">暂未检测到正在运行的程序</div>';
  openModal(rule ? "编辑规则" : "添加规则", `
    <div class="form-grid">
      <label><span>匹配类型</span><select id="modal-rule-type">${typeOptions}</select></label>
      <div id="modal-rule-process-field" class="wide-field">
        <span class="field-label">匹配内容</span>
        <details id="modal-rule-process-picker" class="process-picker">
          <summary><span id="modal-rule-process-count">选择正在运行的程序</span>${icon("chevron-down")}</summary>
          <div class="process-picker-menu">
            <input id="modal-rule-process-search" type="search" placeholder="搜索当前进程">
            <div id="modal-rule-process-list" class="process-picker-list">${processOptions}</div>
          </div>
        </details>
        <label class="process-manual-field"><span>手动补充（可选，一行一个程序）</span><textarea id="modal-rule-process-manual" rows="3" placeholder="例如 MyApp.exe"></textarea></label>
      </div>
      <label id="modal-rule-lines-field" class="wide-field"><span>匹配内容（一行一条）</span><textarea id="modal-rule-values" rows="7" spellcheck="false" placeholder="例如 example.com\nexample.org">${escapeHtml(rule?.ruleType === "PROCESS-NAME" ? "" : rule?.value || "")}</textarea><span class="rule-value-toolbar"><small id="modal-rule-values-summary">0 条匹配内容</small><button id="modal-rule-values-normalize" type="button" class="text-button">整理并去重</button></span></label>
      <label><span>流量去向</span><select id="modal-rule-target">${targetOptions}</select></label>
      <label><span>备注</span><input id="modal-rule-note" value="${escapeHtml(rule?.note || "")}" placeholder="可选"></label>
      <label class="switch-row"><span><strong>启用规则</strong><small>保存后立即应用</small></span><input id="modal-rule-enabled" type="checkbox"${rule?.enabled === false ? "" : " checked"}><i></i></label>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "保存", kind: "primary", action: () => {
      const ruleType = byId("modal-rule-type").value;
      const values = ruleType === "PROCESS-NAME"
        ? [
            ...[...document.querySelectorAll('#modal-rule-process-list input[type="checkbox"]:checked')].map((item) => item.value),
            ...splitRuleValues(byId("modal-rule-process-manual").value),
          ]
        : splitRuleValues(byId("modal-rule-values").value);
      if (!values.length) {
        showToast("error", "请至少选择或填写一条匹配内容");
        return;
      }
      const payload = {
        index: rule?.index ?? -1,
        ruleType,
        values,
        target: byId("modal-rule-target").value,
        note: byId("modal-rule-note").value,
        enabled: byId("modal-rule-enabled").checked,
      };
      invoke("saveRule", JSON.stringify(payload));
      closeModal();
      scheduleStateRefresh(250);
    } },
  ]);
  const ruleType = byId("modal-rule-type");
  ruleType.addEventListener("change", syncRuleValueEditor);
  byId("modal-rule-process-list").addEventListener("change", updateProcessPickerCount);
  byId("modal-rule-process-search").addEventListener("input", (event) => {
    const query = event.target.value.trim().toLowerCase();
    document.querySelectorAll(".process-picker-option").forEach((option) => {
      option.classList.toggle("is-hidden", !option.dataset.processSearch.includes(query));
    });
  });
  byId("modal-rule-values").addEventListener("input", () => updateRuleValueSummary("modal-rule-values", "modal-rule-values-summary"));
  byId("modal-rule-values-normalize").addEventListener("click", () => normalizeRuleValueEditor("modal-rule-values", "modal-rule-values-summary"));
  syncRuleValueEditor();
  updateProcessPickerCount();
  updateRuleValueSummary("modal-rule-values", "modal-rule-values-summary");
}

function openRuleGroupDialog(group, focusValues = false) {
  const targetIcons = {
    CLASH: "wifi",
    V2RAY: "route",
    BUILTIN: "server",
    DIRECT: "link",
  };
  const targetOptions = TARGETS.map(([value, label]) => `
    <button type="button" class="group-target-option${group.target === value ? " is-active" : ""}" data-group-target="${value}" role="radio" aria-checked="${group.target === value}">
      ${icon(targetIcons[value])}<span>${label}</span>
    </button>`).join("");
  const entries = Array.isArray(group.entries) ? group.entries : [];
  const values = entries.map((entry) => entry.domain).join("\n");
  openModal(escapeHtml(group.note || "规则组"), `
    <div class="form-grid rule-group-editor">
      <div class="wide-field">
        <div class="rule-group-editor-head"><span class="field-label">匹配内容（一行一个域名）</span><label class="rule-value-search"><span class="sr-only">查找域名</span><input id="rule-group-search" type="search" placeholder="查找域名"><button id="rule-group-find" type="button" class="mini-button" title="查找下一个">${icon("search")}</button></label></div>
        <textarea id="rule-group-values" rows="13" spellcheck="false" placeholder="example.com\nexample.org">${escapeHtml(values)}</textarea>
        <div class="rule-value-toolbar"><small id="rule-group-values-summary">${entries.length} 条匹配内容</small><span><button id="rule-group-normalize" type="button" class="text-button">整理并去重</button><button id="rule-group-reset" type="button" class="text-button">恢复默认</button></span></div>
      </div>
      <div><span class="field-label">整组流量去向</span><div class="group-target-options" role="radiogroup">${targetOptions}</div></div>
      <div class="group-dialog-summary"><strong>按从上到下的顺序保存</strong><span>支持直接粘贴多行；空行会忽略，重复域名会自动合并</span></div>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "应用到整组", kind: "primary", action: () => {
      const values = uniqueRuleValues(byId("rule-group-values").value);
      if (!values.length) {
        showToast("error", "请至少保留一条匹配内容");
        byId("rule-group-values").focus();
        return;
      }
      invoke("saveRuleGroup", JSON.stringify({
        groupId: group.groupId,
        target: document.querySelector(".group-target-option.is-active")?.dataset.groupTarget,
        values,
      }));
      closeModal();
      scheduleStateRefresh(250);
    } },
  ]);
  document.querySelectorAll(".group-target-option").forEach((option) => {
    option.addEventListener("click", () => {
      document.querySelectorAll(".group-target-option").forEach((item) => {
        const selected = item === option;
        item.classList.toggle("is-active", selected);
        item.setAttribute("aria-checked", String(selected));
      });
    });
  });
  const textarea = byId("rule-group-values");
  const search = byId("rule-group-search");
  textarea.addEventListener("input", () => updateRuleValueSummary("rule-group-values", "rule-group-values-summary"));
  byId("rule-group-normalize").addEventListener("click", () => normalizeRuleValueEditor("rule-group-values", "rule-group-values-summary"));
  byId("rule-group-reset").addEventListener("click", () => {
    textarea.value = (group.defaultEntries || []).join("\n");
    updateRuleValueSummary("rule-group-values", "rule-group-values-summary");
    textarea.focus();
  });
  byId("rule-group-find").addEventListener("click", () => findRuleValue(textarea, search));
  search.addEventListener("input", () => findRuleValue(textarea, search, true));
  search.addEventListener("keydown", (event) => {
    if (event.key !== "Enter") return;
    event.preventDefault();
    findRuleValue(textarea, search);
  });
  updateRuleValueSummary("rule-group-values", "rule-group-values-summary");
  if (focusValues) window.setTimeout(() => textarea.focus(), 0);
}

function openFallbackRuleDialog() {
  const fallback = appState.fallbackRule || { target: "DIRECT" };
  const targetIcons = {
    CLASH: "wifi",
    V2RAY: "route",
    BUILTIN: "server",
    DIRECT: "link",
  };
  const targetOptions = TARGETS.map(([value, label]) => `
    <button type="button" class="group-target-option${fallback.target === value ? " is-active" : ""}" data-group-target="${value}" role="radio" aria-checked="${fallback.target === value}">
      ${icon(targetIcons[value])}<span>${label}</span>
    </button>`).join("");
  openModal("强制保底规则", `
    <div class="form-grid">
      <div><span class="field-label">未匹配流量去向</span><div class="group-target-options" role="radiogroup">${targetOptions}</div></div>
      <div class="group-dialog-summary"><strong>始终启用 · 固定最后匹配</strong><span>默认直连，也可切换到本地端口或当前内置节点</span></div>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "保存保底出口", kind: "primary", action: () => {
      const target = document.querySelector(".group-target-option.is-active")?.dataset.groupTarget;
      invoke("setDefaultTarget", target);
      closeModal();
      scheduleStateRefresh(250);
    } },
  ]);
  document.querySelectorAll(".group-target-option").forEach((option) => {
    option.addEventListener("click", () => {
      document.querySelectorAll(".group-target-option").forEach((item) => {
        const selected = item === option;
        item.classList.toggle("is-active", selected);
        item.setAttribute("aria-checked", String(selected));
      });
    });
  });
}

function openRuleGroupDomains(group) {
  const entries = Array.isArray(group.entries) ? group.entries : [];
  const rows = entries.map((entry) => `
    <div class="rule-domain-row" data-domain-search="${escapeHtml(`${entry.domain} ${entry.label}`.toLowerCase())}">
      <span class="rule-status${entry.enabled ? " is-enabled" : ""}">${entry.enabled ? "启用" : "停用"}</span>
      <strong title="${escapeHtml(entry.domain)}">${escapeHtml(entry.domain)}</strong>
      <span title="${escapeHtml(entry.label)}">${escapeHtml(entry.label || "其他")}</span>
    </div>`).join("");
  openModal(`${escapeHtml(group.note || "规则组")}域名`, `
    <div class="rule-domain-browser">
      <div class="group-dialog-summary"><strong>${group.count} 条匹配 · ${escapeHtml(group.targetLabel)}</strong><span>整组共用当前出口</span></div>
      <label class="rule-domain-search"><span>搜索域名或分类</span><input id="rule-domain-filter" type="search" placeholder="例如 arcteryx、Google、ChatGPT" autocomplete="off"></label>
      <div class="rule-domain-list-head"><span>状态</span><span>域名</span><span>分类</span></div>
      <div id="rule-domain-list" class="rule-domain-list">${rows}</div>
      <div id="rule-domain-count" class="rule-domain-count">显示 ${entries.length} / ${entries.length} 条</div>
    </div>`, [
    { label: "关闭", kind: "secondary", action: closeModal },
  ]);
  const filter = byId("rule-domain-filter");
  filter.addEventListener("input", () => {
    const query = filter.value.trim().toLowerCase();
    let visible = 0;
    document.querySelectorAll(".rule-domain-row").forEach((row) => {
      const matches = !query || row.dataset.domainSearch.includes(query);
      row.hidden = !matches;
      if (matches) visible += 1;
    });
    byId("rule-domain-count").textContent = `显示 ${visible} / ${entries.length} 条`;
  });
  filter.focus();
}

function openNodeGroupManager() {
  const groups = appState.nodeGroups || [];
  const rows = groups.length
    ? groups.map((group) => {
      const count = appState.nodes.filter((node) => node.customGroup === group).length;
      return `<div class="node-group-row"><span title="${escapeHtml(group)}">${escapeHtml(group)} · ${count} 个节点</span><button class="mini-button danger" data-node-group-delete="${escapeHtml(group)}" title="删除分组" aria-label="删除分组">${icon("trash-2")}</button></div>`;
    }).join("")
    : '<div class="node-group-row"><span>尚未创建自定义分组</span></div>';
  openModal("节点分组管理", `
    <div class="node-group-manager">
      <div class="node-group-create"><input id="new-node-group-name" maxlength="40" placeholder="例如 巴西住宅代理"><button id="create-node-group" class="button primary">新建分组</button></div>
      <div class="group-dialog-summary"><strong>自动归类始终保留</strong><span>未指定自定义分组的节点会按订阅、服务器部署或手动导入来源聚集。</span></div>
      <div class="node-group-rows">${rows}</div>
    </div>`, [
    { label: "完成", kind: "secondary", action: closeModal },
  ]);
  const create = () => {
    const input = byId("new-node-group-name");
    const name = input.value.trim();
    if (!name) return;
    invoke("createNodeGroup", name);
    closeModal();
  };
  byId("create-node-group").addEventListener("click", create);
  byId("new-node-group-name").addEventListener("keydown", (event) => {
    if (event.key === "Enter") create();
  });
  byId("modal-content").onclick = (event) => {
    const button = event.target.closest("[data-node-group-delete]");
    if (!button) return;
    const name = button.dataset.nodeGroupDelete;
    confirmAction(
      "删除节点分组",
      `删除“${name}”后，其中节点会恢复按来源自动归类。`,
      () => invoke("deleteNodeGroup", name),
    );
  };
  byId("new-node-group-name").focus();
}

function openNodeGroupDialog(node) {
  const groups = appState.nodeGroups || [];
  const options = [
    `<option value=""${node.customGroup ? "" : " selected"}>按来源自动归类（${escapeHtml(node.source || "手动导入")}）</option>`,
    ...groups.map((group) => `<option value="${escapeHtml(group)}"${node.customGroup === group ? " selected" : ""}>${escapeHtml(group)}</option>`),
  ].join("");
  openModal("移动节点到分组", `
    <div class="form-grid">
      <div class="group-dialog-summary"><strong>${escapeHtml(node.name)}</strong><span>${escapeHtml(node.server)}</span></div>
      <label><span>目标分组</span><select id="modal-node-group">${options}</select></label>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "应用", kind: "primary", action: () => {
      invoke("assignNodeGroup", node.name, byId("modal-node-group").value);
      closeModal();
    } },
  ]);
}


function openPasteDialog() {
  const groupOptions = [
    '<option value="">按来源自动归类</option>',
    ...(appState.nodeGroups || []).map((group) => `<option value="${escapeHtml(group)}">${escapeHtml(group)}</option>`),
  ].join("");
  openModal("粘贴导入", `
    <div class="form-grid">
      <label><span>导入分组</span><select id="modal-import-group">${groupOptions}</select></label>
      <label><span>订阅或节点内容</span><textarea id="modal-import-content" placeholder="支持 Clash YAML、Base64、分享链接及 主机:端口:用户名:密码"></textarea></label>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "开始导入", kind: "primary", action: () => {
      invoke(
        "importPaste",
        "手动导入",
        byId("modal-import-content").value,
        byId("modal-import-group").value,
      );
      closeModal();
    } },
  ]);
}

function openSshServerDialog(server = null) {
  const authMethod = server?.authMethod || "password";
  const hasSavedCredential = Boolean(server?.hasCredential);
  const rememberCredential = server
    ? server.rememberPassword || hasSavedCredential
    : authMethod !== "agent";
  openModal(server ? "编辑服务器部署" : "添加服务器部署", `
    <div class="form-grid ssh-form-grid">
      <label class="wide-field"><span>名称</span><input id="modal-ssh-name" value="${escapeHtml(server?.name || "我的服务器")}" required></label>
      <label class="wide-field"><span>地区标签</span><input id="modal-ssh-region" value="${escapeHtml(server?.region || "")}" placeholder="例如 美国、日本、新加坡"></label>
      <label><span>服务器 IP / 域名</span><input id="modal-ssh-host" value="${escapeHtml(server?.host || "")}" placeholder="例如 203.0.113.10" required></label>
      <label><span>SSH 端口</span><input id="modal-ssh-port" type="number" min="1" max="65535" value="${server?.port || 22}" required></label>
      <label><span>用户名</span><input id="modal-ssh-username" value="${escapeHtml(server?.username || "root")}" required></label>
      <label><span>远端代理端口</span><input id="modal-ssh-proxy-port" type="number" min="1" max="65535" value="${server?.proxyPort || appState.settings.serverProxyPort || DEFAULT_SERVER_PROXY_PORT}" required><small>${server?.deployed ? `当前服务使用 ${server.proxyPort}；与默认端口不一致时检查服务会迁移` : `默认使用设置中的高位端口 ${appState.settings.serverProxyPort || DEFAULT_SERVER_PROXY_PORT}`}</small></label>
      <label><span>认证方式</span><select id="modal-ssh-auth"><option value="password"${authMethod === "password" ? " selected" : ""}>账号密码</option><option value="key"${authMethod === "key" ? " selected" : ""}>私钥文件</option><option value="agent"${authMethod === "agent" ? " selected" : ""}>SSH Agent</option></select></label>
      <label id="modal-ssh-secret-field"><span>密码 / 私钥口令</span><input id="modal-ssh-password" type="password" autocomplete="new-password" placeholder="${server?.hasCredential ? "已安全保存，留空保持不变" : "连接凭据"}"></label>
      <label id="modal-ssh-key-field" class="wide-field"><span>私钥路径</span><div class="input-action"><input id="modal-ssh-key-path" value="${escapeHtml(server?.keyPath || "")}" placeholder="选择 OpenSSH 私钥"><button id="modal-pick-ssh-key" type="button" class="icon-button" title="选择私钥" aria-label="选择私钥">${icon("folder-open")}</button></div></label>
      <label class="switch-row wide-field"><span><strong>记住凭据</strong><small>${hasSavedCredential ? "已保存密码；留空即保持不变，取消勾选会删除已保存的密码" : "Windows 下使用当前用户 DPAPI 加密"}</small></span><input id="modal-ssh-remember" type="checkbox"${rememberCredential ? " checked" : ""}><i></i></label>
      <div class="group-dialog-summary wide-field"><strong>部署内容</strong><span>在 Linux 服务器安装独立 Shadowsocks 2022 服务，SSH 断开后仍可使用。</span></div>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "保存", kind: "primary", action: () => {
      const sshPort = Number(byId("modal-ssh-port").value);
      const proxyPort = Number(byId("modal-ssh-proxy-port").value);
      const portError = serverProxyPortError(proxyPort, sshPort);
      if (portError) {
        showToast("error", portError);
        byId("modal-ssh-proxy-port").focus();
        return;
      }
      const payload = {
        profileId: server?.profileId || "",
        name: byId("modal-ssh-name").value,
        region: byId("modal-ssh-region").value,
        host: byId("modal-ssh-host").value,
        port: sshPort,
        username: byId("modal-ssh-username").value,
        proxyPort,
        authMethod: byId("modal-ssh-auth").value,
        keyPath: byId("modal-ssh-key-path").value,
        rememberPassword: byId("modal-ssh-remember").checked,
      };
      invoke("saveSshServer", JSON.stringify(payload), byId("modal-ssh-password").value);
      closeModal();
    } },
  ]);
  const updateAuthFields = () => {
    const method = byId("modal-ssh-auth").value;
    byId("modal-ssh-key-field").classList.toggle("is-hidden", method !== "key");
    byId("modal-ssh-secret-field").classList.toggle("is-hidden", method === "agent");
    byId("modal-ssh-remember").disabled = method === "agent";
  };
  byId("modal-ssh-auth").addEventListener("change", updateAuthFields);
  byId("modal-ssh-password").addEventListener("input", (event) => {
    if (event.target.value) byId("modal-ssh-remember").checked = true;
  });
  byId("modal-pick-ssh-key").addEventListener("click", async () => {
    const path = await invoke("pickSshKey");
    if (path) byId("modal-ssh-key-path").value = path;
  });
  updateAuthFields();
}

/// 触发一次部署/检查：先本地标成进行中（界面立刻有反馈），请求结束后再交回给
/// 后端的真实状态。
function runServerTask(profileId, ...args) {
  pendingServerTasks.add(profileId);
  renderSshServers();
  scheduleStateRefresh(0);
  return invoke("deploySshServer", profileId, ...args).finally(() => {
    pendingServerTasks.delete(profileId);
    scheduleStateRefresh(0);
  });
}

function deploySshServer(server) {
  if (server.authMethod === "agent") {
    runServerTask(server.profileId, "");
    return;
  }
  const hasSaved = Boolean(server.hasCredential);
  const label = server.authMethod === "key" ? "私钥口令（没有可留空）" : "SSH 密码";
  openModal(`${server.deployed ? "检查或修复" : "部署"} ${server.name}`, `
    <div class="form-grid">
      <div class="group-dialog-summary"><strong>${escapeHtml(server.username)}@${escapeHtml(server.host)}:${server.port}</strong><span>${server.deployed ? "先检查远端服务；仅在服务缺失或停止时修复" : `将部署代理节点：${escapeHtml(server.host)}:${server.proxyPort}`}</span></div>
      <label><span>${label}</span><input id="modal-connect-password" type="password" autocomplete="current-password"${hasSaved ? "" : " autofocus"} placeholder="${hasSaved ? "已保存；留空继续使用，输入新密码将覆盖" : ""}"></label>
      <label class="switch-row"><span><strong>${hasSaved ? "保存并覆盖旧凭据" : "成功后记住凭据"}</strong><small>${hasSaved ? "新密码验证通过后会替换这台服务器已保存的凭据" : "后续检查或修复无需再次输入"}</small></span><input id="modal-connect-remember" type="checkbox" checked><i></i></label>
      ${server.deployed ? `<label class="switch-row"><span><strong>轮换节点密码</strong><small>强制重新部署并生成新的节点密码，旧的 ss:// 链接会失效</small></span><input id="modal-connect-redeploy" type="checkbox"><i></i></label>` : ""}
    </div>`, [
      { label: "取消", kind: "secondary", action: closeModal },
      { label: server.deployed ? "检查服务" : "开始部署", kind: "primary", action: () => {
      runServerTask(
        server.profileId,
        byId("modal-connect-password").value,
        byId("modal-connect-remember").checked,
        Boolean(byId("modal-connect-redeploy")?.checked),
      );
      closeModal();
    } },
  ]);
}

function confirmAction(title, message, onConfirm) {
  openModal(title, `<p>${escapeHtml(message)}</p>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "删除", kind: "danger", action: () => { closeModal(); onConfirm(); } },
  ]);
}

function openModal(title, content, actions) {
  const dialog = byId("app-modal");
  byId("modal-title").textContent = title;
  byId("modal-content").innerHTML = content;
  const actionRoot = byId("modal-actions");
  actionRoot.innerHTML = "";
  actions.forEach((action) => {
    const button = document.createElement("button");
    button.className = `button ${action.kind}`;
    button.textContent = action.label;
    button.addEventListener("click", action.action);
    actionRoot.append(button);
  });
  enhanceSelects(byId("modal-content"));
  dialog.showModal();
}

function closeModal() {
  closeCustomSelects();
  const dialog = byId("app-modal");
  if (dialog.open) dialog.close();
}

function showToast(kind, message) {
  const toast = document.createElement("div");
  toast.className = `toast ${kind}`;
  toast.textContent = message;
  byId("toast-container").append(toast);
  window.setTimeout(() => toast.remove(), 3600);
}

const THEME_KEY = "network-manager.theme";
const THEME_ORDER = ["system", "light", "dark"];
const THEME_LABELS = { system: "跟随系统", light: "浅色模式", dark: "深色模式" };

function readStoredTheme() {
  try {
    const value = localStorage.getItem(THEME_KEY);
    return THEME_ORDER.includes(value) ? value : "system";
  } catch (_error) {
    // Local storage is optional in embedded browser profiles.
    return "system";
  }
}

function storeTheme(theme) {
  try { localStorage.setItem(THEME_KEY, theme); } catch (_error) { /* optional */ }
}

function prefersDarkTheme() {
  return Boolean(window.matchMedia && window.matchMedia("(prefers-color-scheme: dark)").matches);
}

function applyTheme(theme) {
  const resolved = theme === "system" ? (prefersDarkTheme() ? "dark" : "light") : theme;
  document.documentElement.dataset.theme = resolved;
  const button = byId("theme-toggle");
  if (!button) return;
  const glyph = theme === "system" ? "monitor" : theme === "dark" ? "moon" : "sun";
  const label = `主题：${THEME_LABELS[theme]}（点击切换）`;
  button.innerHTML = icon(glyph);
  button.title = label;
  button.setAttribute("aria-label", label);
}

function cycleTheme() {
  const next = THEME_ORDER[(THEME_ORDER.indexOf(readStoredTheme()) + 1) % THEME_ORDER.length];
  storeTheme(next);
  applyTheme(next);
}

function bindScrollShadow() {
  const scroller = byId("page-content");
  const update = () => document.body.classList.toggle("page-scrolled", scroller.scrollTop > 0);
  scroller.addEventListener("scroll", update, { passive: true });
  update();
}

function bindEvents() {
  if (CUSTOM_FRAME) {
    byId("window-drag-region").addEventListener("pointerdown", (event) => {
      if (event.button === 0) performWindowAction("drag");
    });
    byId("window-drag-region").addEventListener("dblclick", () => performWindowAction("maximize"));
    document.querySelectorAll("[data-window-action]").forEach((button) => {
      button.addEventListener("click", () => performWindowAction(button.dataset.windowAction));
    });
  }
  document.querySelectorAll(".nav-item").forEach((item) => item.addEventListener("click", () => setPage(item.dataset.page)));
  byId("theme-toggle").addEventListener("click", cycleTheme);
  bindScrollShadow();
  document.addEventListener("click", (event) => {
    if (!event.target.closest(".custom-select")) closeCustomSelects();
  });
  document.addEventListener("keydown", (event) => {
    if (event.key === "Escape") closeCustomSelects();
  });
  document.querySelectorAll(".tab").forEach((item) => item.addEventListener("click", () => setNodeTab(item.dataset.nodeTab)));
  byId("header-refresh").addEventListener("click", () => scheduleStateRefresh(0));
  byId("core-toggle").addEventListener("click", toggleCoreFromUi);
  byId("restart-admin").addEventListener("click", () => invoke("restartAsAdmin"));
  byId("mode-switch").addEventListener("click", (event) => {
    const button = event.target.closest("[data-mode]");
    if (button && !button.disabled) invoke("setMode", button.dataset.mode);
  });
  byId("test-exit").addEventListener("click", () => invoke("testExit"));
  byId("add-rule").addEventListener("click", () => openRuleDialog());
  byId("rules-body").addEventListener("click", (event) => {
    const fallbackButton = event.target.closest("[data-fallback-action]");
    if (fallbackButton) {
      openFallbackRuleDialog();
      return;
    }
    const button = event.target.closest("[data-rule-action]");
    if (!button) return;
    const action = button.dataset.ruleAction;
    const groupId = button.dataset.ruleGroup;
    if (groupId) {
      const group = appState.rules.find((rule) => rule.groupId === groupId);
      if (action === "view") openRuleGroupDialog(group, true);
      else if (action === "edit") openRuleGroupDialog(group);
      else if (action === "delete") confirmAction("删除规则组", `确定删除${group.note || "该规则组"}？`, () => invoke("ruleGroupAction", groupId, "delete"));
      else invoke("ruleGroupAction", groupId, action);
      return;
    }
    const index = Number(button.dataset.index);
    const rule = appState.rules.find((item) => item.kind === "rule" && item.index === index);
    if (action === "edit") openRuleDialog(rule);
    else if (action === "delete") confirmAction("删除规则", "确定删除这条分流规则？", () => invoke("ruleAction", index, "delete"));
    else invoke("ruleAction", index, action);
  });
  byId("source-editor").addEventListener("click", (event) => {
    const button = event.target.closest("[data-source-test]");
    if (button) invoke("testSource", button.dataset.sourceTest);
  });
  byId("source-editor").addEventListener("change", (event) => {
    if (event.target.matches('[id^="source-"][id$="-enabled"]')) saveSourcesFromForm();
  });
  byId("save-sources").addEventListener("click", saveSourcesFromForm);
  byId("import-paste").addEventListener("click", openPasteDialog);
  byId("import-file").addEventListener("click", () => {
    if (appState?.capabilities?.browserFiles) {
      openBrowserTextFile(".yaml,.yml,.txt,.conf,.json", (file, content) => invoke("importFileText", file.name, content));
    } else invoke("importFile");
  });
  byId("portable-config-import").addEventListener("click", async () => {
    if (appState?.capabilities?.browserFiles) {
      openBrowserTextFile(".json", (_file, content) => invoke("importPortableConfigText", content));
      return;
    }
    // Windows 走原生「打开」对话框。invoke 会把错误吞成 null，这里要如实提示。
    try {
      await invokeStrict("importPortableConfig");
    } catch (error) {
      showToast("error", `导入失败：${error.message || error}`);
    }
  });
  byId("quit-application")?.addEventListener("click", () => performWindowAction("quit"));
  byId("portable-config-export").addEventListener("click", async () => {
    if (appState?.capabilities?.browserFiles) {
      const content = await invoke("exportPortableConfigText");
      if (content) downloadTextFile("network-manager-config.json", content);
      return;
    }
    // WebView2 没有默认的 <a download> 处理，Windows 端由后端弹「另存为」写盘。
    try {
      const result = await invokeStrict("exportPortableConfig");
      if (result?.path) showToast("success", `配置已导出到 ${result.path}`);
    } catch (error) {
      showToast("error", `导出失败：${error.message || error}`);
    }
  });
  byId("test-all-nodes").addEventListener("click", () => invoke("testAllNodes"));
  byId("manage-node-groups").addEventListener("click", openNodeGroupManager);
  byId("delete-error-nodes").addEventListener("click", () => {
    const count = appState.nodes.filter((node) => node.latencyStatus === "error").length;
    if (!count) return;
    confirmAction(
      "删除测速失败节点",
      `确定删除 ${count} 个 Error 节点？订阅记录会保留，之后刷新原订阅即可恢复。`,
      () => invoke("deleteErrorNodes"),
    );
  });
  byId("node-grid").addEventListener("click", (event) => {
    const groupToggle = event.target.closest("[data-node-group-toggle]");
    if (groupToggle) {
      toggleNodeGroup(groupToggle.dataset.nodeGroupToggle);
      return;
    }
    const testButton = event.target.closest("[data-node-test]");
    if (testButton) {
      event.stopPropagation();
      invoke("testNode", Number(testButton.dataset.nodeTest));
      return;
    }
    const groupButton = event.target.closest("[data-node-group]");
    if (groupButton) {
      event.stopPropagation();
      const node = appState.nodes.find((item) => item.name === groupButton.dataset.nodeGroup);
      if (node) openNodeGroupDialog(node);
      return;
    }
    const button = event.target.closest("[data-node-delete]");
    if (button) {
      event.stopPropagation();
      const index = Number(button.dataset.nodeDelete);
      confirmAction("删除节点", `确定删除“${appState.nodes[index]?.name || "该节点"}”？`, () => invoke("deleteNode", index));
      return;
    }
    const card = event.target.closest("[data-node-select]");
    if (card) invoke("selectNode", card.dataset.nodeSelect);
  });
  byId("node-grid").addEventListener("keydown", (event) => {
    if (event.key !== "Enter" && event.key !== " ") return;
    const card = event.target.closest("[data-node-select]");
    if (card) {
      event.preventDefault();
      invoke("selectNode", card.dataset.nodeSelect);
    }
  });
  byId("add-subscription").addEventListener("click", () => {
    invoke(
      "addSubscription",
      byId("subscription-name").value,
      byId("subscription-url").value,
      byId("subscription-group").value,
    );
  });
  byId("refresh-subscriptions").addEventListener("click", () => invoke("refreshAllSubscriptions"));
  byId("subscription-grid").addEventListener("click", (event) => {
    const button = event.target.closest("[data-subscription-action]");
    if (!button) return;
    const index = Number(button.dataset.index);
    if (button.dataset.subscriptionAction === "refresh") invoke("refreshSubscription", index);
    else confirmAction("删除订阅", "订阅及由它导入的节点将一并删除。", () => invoke("deleteSubscription", index));
  });
  byId("add-ssh-server").addEventListener("click", () => openSshServerDialog());
  byId("ssh-select-all").addEventListener("change", (event) => {
    sshSelection.clear();
    if (event.target.checked) linuxServers().forEach((server) => sshSelection.add(server.profileId));
    renderSshSelectionOnly();
  });
  byId("ssh-server-body").addEventListener("change", (event) => {
    const box = event.target.closest("[data-ssh-select]");
    if (!box) return;
    const id = box.dataset.sshSelect;
    if (box.checked) sshSelection.add(id);
    else sshSelection.delete(id);
    renderSshSelectionOnly();
  });
  byId("batch-deploy-ssh").addEventListener("click", () => {
    const picked = selectedSshServers();
    if (!picked.length) return;
    openModal(`批量部署 ${picked.length} 台服务器`, `
      <div class="group-dialog-summary"><strong>将依次连接并部署选中的服务器</strong><span>部署在后台串行执行，可切到运行日志查看进度；未保存凭据的服务器会单独报错</span></div>
      <div class="group-dialog-summary"><strong>本次选中的服务器</strong><span>${escapeHtml(picked.map((server) => server.name).join("、"))}</span></div>`,
      [
        { label: "取消", kind: "secondary", action: () => {} },
        { label: `开始部署 ${picked.length} 台`, kind: "primary", action: () => {
          picked.forEach((server) => runServerTask(server.profileId, "", false));
        } },
      ]);
  });
  byId("batch-copy-ssh").addEventListener("click", async () => {
    const links = selectedSshServers().map((server) => server.shareLink).filter(Boolean);
    if (!links.length) return;
    const copied = await copyText(links.join("\n"));
    showToast(copied ? "success" : "error", copied ? `已复制 ${links.length} 条节点链接` : "复制失败，请手动复制");
  });
  byId("batch-delete-ssh").addEventListener("click", () => {
    const picked = selectedSshServers();
    if (!picked.length) return;
    confirmAction(`删除 ${picked.length} 台服务器`, `将删除所选服务器记录和对应内置节点，已保存的 SSH 密码也会一并清除，但不会卸载远端服务。确定继续？`, () => {
      picked.forEach((server) => invoke("deleteSshServer", server.profileId));
      sshSelection.clear();
    });
  });
  byId("ssh-server-body").addEventListener("click", async (event) => {
    const button = event.target.closest("[data-ssh-action]");
    if (!button) return;
    const server = appState.sshServers.find((item) => item.profileId === button.dataset.profileId);
    if (!server) return;
    if (button.dataset.sshAction === "deploy") deploySshServer(server);
    else if (button.dataset.sshAction === "copy") {
      // 之前只调用了 invoke 却把返回的节点配置丢掉了，什么都没复制。
      // 现在拼成 IP:端口:账号:密码 写入剪贴板，并把结果提示出来。
      const node = await invoke("copyServerNode", server.profileId);
      if (!node) return; // invoke 失败时已经弹过错误提示
      const copied = await copyText(nodePlainText(node));
      showToast(copied ? "success" : "error", copied ? `已复制“${server.name}”的节点（IP:端口:账号:密码）` : "复制失败，请手动复制节点信息");
    }
    else if (button.dataset.sshAction === "edit") openSshServerDialog(server);
    else if (button.dataset.sshAction === "forget") confirmAction("清除已保存的密码", `将删除“${server.name}”已保存的 SSH 密码，服务器记录和代理节点都会保留。确定继续？`, () => invoke("forgetSshCredential", server.profileId));
    else confirmAction("删除服务器记录", `将删除“${server.name}”和对应内置节点，已保存的 SSH 密码也会一并清除，但不会卸载远端服务。确定继续？`, () => invoke("deleteSshServer", server.profileId));
  });
  byId("settings-form").addEventListener("submit", (event) => {
    event.preventDefault();
    invoke("saveSettings", JSON.stringify({
      mixedPort: Number(byId("setting-mixed-port").value),
      controllerPort: Number(byId("setting-controller-port").value),
      dnsPort: Number(byId("setting-dns-port").value),
      serverProxyPort: Number(byId("setting-server-proxy-port").value),
      strictRoute: byId("setting-strict-route").checked,
      dnsMode: byId("setting-dns-compat").checked ? "redir-host" : "fake-ip",
      startOnLaunch: byId("setting-start-on-launch").checked,
      closeToTray: byId("setting-close-to-tray").checked,
      startWithWindows: byId("setting-start-with-windows").checked,
    }));
    settingsInitialized = false;
  });
  // 统一中转改完立刻生效：中转会影响每一条出去的连接，攒到「保存设置」一起提交
  // 会让用户在改完到点保存之间拿到的还是老链路。
  byId("setting-relay-enabled").addEventListener("change", async (event) => {
    const node = byId("setting-relay-node").value;
    const result = await invoke("setRelay", JSON.stringify({ enabled: event.target.checked, node }));
    if (!result) renderRelay(); // 被拒绝时把界面拉回实际状态
  });
  byId("setting-relay-node").addEventListener("change", async (event) => {
    const result = await invoke("setRelay", JSON.stringify({ enabled: true, node: event.target.value }));
    if (!result) renderRelay();
  });
  // 本地 DNS 映射：独立保存，不跟「保存设置」一起提交 —— 它是多行文本，
  // 解析失败要能单独报错并把编辑框留着，混进表单里会让整组设置一起白改。
  byId("setting-local-dns").addEventListener("input", (event) => {
    updateLocalDnsSummary(event.target.value);
  });
  byId("save-local-dns").addEventListener("click", async () => {
    const result = await invoke("saveLocalDns", byId("setting-local-dns").value);
    if (!result) return; // 失败时已经提示过，输入内容保持原样让用户改
    byId("setting-local-dns").value = result.text || "";
    updateLocalDnsSummary();
    settingsInitialized = false;
  });
  // 证书按钮：动作本身在 Rust 里做（生成、certutil 装/卸、打开目录），
  // 这里只负责在各方法之间做「先有证书才能装」这一层顺序约束。
  byId("generate-local-cert").addEventListener("click", () => invoke("generateLocalCerts"));
  byId("install-local-cert").addEventListener("click", () => invoke("installLocalCa"));
  byId("uninstall-local-cert").addEventListener("click", () =>
    // 不用 confirmAction：那个封装把确认按钮写死成「删除」，放这儿措辞不对。
    openModal(
      "取消信任本地 CA",
      "<p>浏览器和系统随即不再信任这套证书签出来的站点，需要时再点「信任 CA」即可恢复。确定继续？</p>",
      [
        { label: "取消", kind: "secondary", action: closeModal },
        {
          label: "取消信任",
          kind: "danger",
          action: () => {
            closeModal();
            invoke("uninstallLocalCa");
          },
        },
      ],
    ),
  );
  byId("open-local-cert-dir").addEventListener("click", () => invoke("openLocalCertDir"));
  byId("setting-random-server-proxy-port").addEventListener("click", () => {
    const values = new Uint32Array(1);
    crypto.getRandomValues(values);
    byId("setting-server-proxy-port").value =
      MIN_RANDOM_SERVER_PROXY_PORT + (values[0] % (65536 - MIN_RANDOM_SERVER_PROXY_PORT));
  });
  // 原来 invoke 没 await 就立刻 refreshLogs，两个请求抢同一把状态锁，可能先读到
  // 旧日志再清空，看起来像「点了没反应 / 卡住」。等清空落地再重读。
  byId("clear-logs").addEventListener("click", async () => {
    await invoke("clearLogs");
    refreshLogs(true);
  });
  byId("open-logs").addEventListener("click", () => invoke("openLogs"));

  // 环境管理页
  byId("env-refresh").addEventListener("click", () => refreshEnv(true));
  // 用户主动点才联网；传 true 绕过后端 10 分钟缓存，点了一定真查
  byId("env-check-latest").addEventListener("click", () => checkEnvLatest(true));
  byId("env-update-all").addEventListener("click", () => updateAllEnv());
  byId("env-install-all").addEventListener("click", () => installAllEnv());
  byId("env-repair-winget").addEventListener("click", () => repairWinget());
  byId("env-uninstall-all").addEventListener("click", () => uninstallAllEnv());
  byId("env-task-close").addEventListener("click", () => {
    byId("env-task").hidden = true;
    envTaskHandled = "";
    invoke("envTaskClear");
  });
  byId("env-detail-close").addEventListener("click", () => { byId("env-detail").hidden = true; });
  // 卡片里的按钮是渲染出来的，用事件委托绑一次就够，不用每张卡重绑
  byId("env-grid").addEventListener("click", (event) => {
    // 顺序有讲究：版本相关的先判，因为它们也带 data-env-* 前缀
    const nvmUse = event.target.closest("[data-nvm-use]");
    if (nvmUse) {
      runNvmAction("nvmUseVersion", nvmUse.dataset.nvmUse, "切换版本");
      return;
    }
    const nvmInstall = event.target.closest("[data-nvm-install]");
    if (nvmInstall) {
      const picker = byId("env-nvm-pick");
      if (!picker || !picker.value) {
        showToast("error", "先选一个要安装的版本");
        return;
      }
      runNvmAction("nvmInstallVersion", picker.value, "安装版本");
      return;
    }
    const nvmQuery = event.target.closest("[data-nvm-query-versions]");
    if (nvmQuery) {
      queryNvmAvailableVersions();
      return;
    }
    const installVersion = event.target.closest("[data-env-install-version]");
    if (installVersion) {
      const id = installVersion.dataset.envInstallVersion;
      const picker = document.querySelector(`[data-env-version-pick="${id}"]`);
      const version = picker ? picker.value : "";
      if (!version) {
        showToast("error", "先选一个要安装的版本");
        return;
      }
      // 大版本系列是独立包、可并存；补丁版本才要覆盖现有安装，确认文案要分开讲。
      const card = document.querySelector(`.env-card[data-env-card="${id}"] .env-card-version`);
      const current = card ? card.textContent.trim() : "当前版本";
      const warning = envIsDirect()
        ? `${id} 目前是 ${current}。\n本机没有 winget，将从官方发布源下载 ${version} 对应的官方安装包并安装（不依赖 winget）。\n\n继续吗？`
        : (SERIES_ENV_IDS.includes(id)
          ? `${id} 目前是 ${current}。\n将安装 ${version} 系列的最新补丁（winget 独立包，与现有版本并存，不会卸载）。\n\n继续吗？`
          : `${id} 目前是 ${current}。\n安装 ${version} 会先卸载现有版本（不能多版本并存）。\n\n继续吗？`);
      if (!window.confirm(warning)) return;
      runEnvAction(id, "installEnv", "安装", version);
      return;
    }
    const queryVersions = event.target.closest("[data-env-query-versions]");
    if (queryVersions) {
      queryEnvVersions(queryVersions.dataset.envQueryVersions);
      return;
    }
    const uninstall = event.target.closest("[data-env-uninstall]");
    if (uninstall) {
      uninstallEnvById(uninstall.dataset.envUninstall);
      return;
    }
    const update = event.target.closest("[data-env-update]");
    if (update) {
      updateEnvById(update.dataset.envUpdate);
      return;
    }
    const reinstall = event.target.closest("[data-env-reinstall]");
    if (reinstall) {
      reinstallEnv(reinstall.dataset.envReinstall);
      return;
    }
    const install = event.target.closest("[data-env-install]");
    if (install) {
      installEnv(install.dataset.envInstall);
      return;
    }
    const open = event.target.closest("[data-env-open]");
    if (open) invoke("openEnvDownload", open.dataset.envOpen);
  });

  // 服务器管理页的标签切换
  document.querySelectorAll("[data-server-tab]").forEach((button) => {
    button.addEventListener("click", () => setServerTab(button.dataset.serverTab));
  });

  // Windows 服务器管理（远程，SSH）
  byId("winserver-add").addEventListener("click", () => openWinServerDialog());
  byId("winserver-connect").addEventListener("click", () => openWinServerRdpForSelected());
  byId("winserver-refresh-all").addEventListener("click", () => refreshWinServerMonitor());
  byId("winserver-list").addEventListener("click", (event) => {
    const pick = event.target.closest("[data-winsrv-pick]");
    if (pick) selectWinServer(pick.dataset.winsrvPick);
  });
  byId("winserver-fixes").addEventListener("change", updateWinSrvApplyButton);
  byId("winserver-apply-selected").addEventListener("click", () => applyWinServerFixes());
  byId("winserver-detail-close").addEventListener("click", () => { byId("winserver-detail").hidden = true; });
  byId("winserver-create-user").addEventListener("click", () => createWinServerUser());
  byId("winserver-copy-command").addEventListener("click", () => copyWinServerCommand());
  byId("winserver-refresh-users").addEventListener("click", () => refreshWinServerUsers());
  byId("winserver-monitor").addEventListener("click", (event) => {
    if (event.target.closest("#winserver-refresh-monitor")) refreshWinServerMonitor();
  });
  // 远程截图与「断开后保持运行」
  byId("winserver-shot").addEventListener("click", () => captureWinServerScreen());
  byId("winserver-keepalive-refresh").addEventListener("click", () => refreshWinServerKeepAliveStatus());
  byId("winserver-shot-box").addEventListener("click", (event) => {
    if (event.target.closest("[data-shot-download]")) downloadWinServerShot();
  });
  byId("winserver-keepalive-list").addEventListener("click", (event) => {
    const apply = event.target.closest("[data-keepalive-apply]");
    if (apply) applyWinServerKeepAlive(apply.dataset.keepaliveApply);
  });

  // 文件快传
  byId("xfer-server-list").addEventListener("click", (event) => {
    const pick = event.target.closest("[data-xfer-pick]");
    if (pick) toggleXferPick(pick.dataset.xferPick);
  });
  document.querySelectorAll("[data-xfer-mode]").forEach((button) => {
    button.addEventListener("click", () => setXferMode(button.dataset.xferMode));
  });
  byId("xfer-pick-file").addEventListener("click", () => pickXferLocal(false));
  byId("xfer-pick-dir").addEventListener("click", () => pickXferLocal(true));
  byId("xfer-browse").addEventListener("click", () => browseXferRemote());
  byId("xfer-browse-up").addEventListener("click", () => {
    const current = byId("xfer-remote").value.trim() || xferBrowsePath;
    const parent = current.replace(/[\\/]+$/, "").replace(/[\\/][^\\/]*$/, "");
    browseXferRemote(parent || (xferServerById(xferPicked[0])?.kind === "windows" ? "C:/" : "/"));
  });
  byId("xfer-browse-box").addEventListener("click", (event) => {
    const enter = event.target.closest("[data-xfer-enter]");
    if (enter) {
      browseXferRemote(enter.dataset.xferEnter);
      return;
    }
    const file = event.target.closest("[data-xfer-file]");
    if (file) {
      byId("xfer-remote").value = file.dataset.xferFile;
      armXferMirror(false);
    }
  });
  byId("xfer-start").addEventListener("click", () => startXfer());
  // 改路径 / 改镜像勾选都要把「已上膛」的状态收回去，否则下一次点击会直接删东西
  byId("xfer-local").addEventListener("input", () => armXferMirror(false));
  byId("xfer-remote").addEventListener("input", () => armXferMirror(false));
  byId("xfer-mirror").addEventListener("change", () => armXferMirror(false));
  byId("modal-close").addEventListener("click", closeModal);
  byId("app-modal").addEventListener("click", (event) => {
    if (event.target === byId("app-modal")) closeModal();
  });
}

function initialize() {
  window.networkManagerReady = false;
  applyTheme(readStoredTheme());
  bindEvents();
  document.addEventListener("visibilitychange", () => scheduleStateRefresh(document.hidden ? 10000 : 0));
  if (window.matchMedia) {
    const query = window.matchMedia("(prefers-color-scheme: dark)");
    query.addEventListener?.("change", () => {
      if (readStoredTheme() === "system") applyTheme("system");
    });
  }
  pollState();
}

initialize();
