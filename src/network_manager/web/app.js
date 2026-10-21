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
let coreActionObservedBusy = false;
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
  servers: "服务器部署",
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
  const headers = { "X-Network-Session": SESSION_TOKEN, ...(options.headers || {}) };
  const response = await fetch(path, { ...options, headers, cache: "no-store" });
  if (!response.ok) {
    let message = `请求失败 (${response.status})`;
    try { message = (await response.json()).error || message; } catch (_error) { /* ignore */ }
    throw new Error(message);
  }
  return response;
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
  } else if (currentPage === "servers") renderSshServers();
  else if (currentPage === "settings") renderSettings();
  // 环境页不在每轮状态里渲染：体检要起子进程问版本号，塞进状态轮询会拖慢整页刷新。
  // 改成切进页面时按需拉一次。
  else if (currentPage === "env") refreshEnv();
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
  if (coreActionPending && core.busy) coreActionObservedBusy = true;
  if (coreActionPending && coreActionObservedBusy && !core.busy) {
    coreActionPending = false;
    coreActionObservedBusy = false;
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

async function toggleCoreFromUi() {
  if (coreActionPending || appState?.core?.busy) return;
  coreActionPending = true;
  coreActionObservedBusy = false;
  const toggle = byId("core-toggle");
  toggle.disabled = true;
  toggle.innerHTML = `${icon("refresh-cw")}<span>处理中</span>`;
  const accepted = await invoke("toggleCore");
  if (!accepted) {
    coreActionPending = false;
    await refreshState();
    scheduleStateRefresh(statePollDelay());
    return;
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
  const servers = appState.sshServers || [];
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
  const servers = appState.sshServers || [];
  return servers.filter((server) => sshSelection.has(server.profileId));
}

function updateSshSelectionUi() {
  const servers = appState.sshServers || [];
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

async function refreshLogs() {
  const now = Date.now();
  if (currentPage !== "logs" || logsRefreshing || now - lastLogsRefreshAt < 900) return;
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

function escapeHtml(value) {
  return String(value ?? "").replace(/[&<>"']/g, (char) => ({
    "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;",
  }[char]));
}

function renderEnv(data) {
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
  summary.innerHTML = parts.join(" · ");

  grid.innerHTML = items.map((item) => {
    // winget 不可用时不给「一键部署」，只留「去官网」，并在卡片上说明原因
    const canInstall = Boolean(item.canInstall);
    const canReinstall = Boolean(item.canReinstall) && item.outdated;
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

    let actions;
    if (item.outdated) {
      // 过期项给「重装」，不给「一键部署」—— 重装走 upgrade，破坏性小得多
      actions = `${canReinstall
          ? `<button type="button" class="button primary" data-env-reinstall="${escapeHtml(item.id)}">${icon("refresh-cw")}<span>重装升级</span></button>`
          : ""}
        <button type="button" class="text-button" data-env-open="${escapeHtml(item.id)}">查看官网版本</button>`;
    } else if (item.ready) {
      actions = `<button type="button" class="text-button" data-env-open="${escapeHtml(item.id)}">打开官网</button>`;
    } else {
      actions = `${canInstall
          ? `<button type="button" class="button primary" data-env-install="${escapeHtml(item.id)}">${icon("download")}<span>一键部署</span></button>`
          : ""}
        <button type="button" class="text-button" data-env-open="${escapeHtml(item.id)}">去官网下载</button>`;
    }

    return `
      <div class="env-card is-${state}" data-env-card="${escapeHtml(item.id)}">
        <div class="env-card-head">
          <span class="env-card-name">${escapeHtml(item.name)}</span>
          <span class="env-badge is-${state}">${badgeText}</span>
        </div>
        <div class="env-card-version ${item.ready && !item.version ? "is-unknown" : ""}">${escapeHtml(versionText)}</div>
        ${hint}
        ${pathHint ? `<div class="env-card-path">${pathHint}</div>` : ""}
        ${item.note ? `<div class="env-card-note">${escapeHtml(item.note)}</div>` : ""}
        <div class="env-card-actions">${actions}</div>
      </div>`;
  }).join("");

  // 一键部署只在「确有缺失且本机能自动装」时才可点。
  // 过期项不算在内 —— 它需要的是重装，混进批量安装会让 winget 去做破坏性更大的事。
  const installAll = byId("env-install-all");
  const installable = missing.filter((item) => item.canInstall);
  installAll.disabled = !installable.length || envBusy;
  const hints = [];
  if (!data.winget) hints.push("本机没有 winget，无法自动安装");
  else if (installable.length) hints.push(`将依次安装：${installable.map((item) => item.name).join("、")}`);
  else hints.push("没有缺失项");
  if (outdated.length) hints.push(`过期项请单独点「重装升级」`);
  installAll.title = hints.join("；");
}

async function refreshEnv() {
  if (envScanning || envBusy) return;
  envScanning = true;
  const summary = byId("env-summary");
  summary.textContent = "正在体检…";
  try {
    const data = await invoke("scanEnv");
    renderEnv(data);
  } catch (error) {
    summary.textContent = `体检失败：${error.message || error}`;
  } finally {
    envScanning = false;
  }
}

function showEnvDetail(title, body) {
  byId("env-detail-title").textContent = title;
  byId("env-detail-body").textContent = body || "（无输出）";
  const panel = byId("env-detail");
  panel.hidden = false;
  panel.scrollIntoView({ block: "nearest", behavior: "smooth" });
}

function setEnvBusy(busy) {
  envBusy = busy;
  // 安装期间把所有按钮禁掉：winget 独占安装锁，连点两次只会得到一段乱序输出
  document.querySelectorAll("[data-env-install], [data-env-reinstall], #env-install-all, #env-refresh")
    .forEach((button) => { button.disabled = busy; });
  const summary = byId("env-summary");
  if (busy) summary.textContent = "正在执行安装，这一步可能要几分钟…";
}

async function runEnvAction(id, method, verb) {
  if (envBusy) return;
  setEnvBusy(true);
  try {
    const result = await invoke(method, id);
    if (result && result.env) renderEnv(result.env);
    showEnvDetail(`${verb}输出`, result?.detail || `${verb}完成`);
  } catch (error) {
    // 失败时把后端带来的详细输出也摊开，别只留一句「安装失败」
    showEnvDetail(`${verb}失败`, error.message || String(error));
  } finally {
    setEnvBusy(false);
    if (byId("env-detail").hidden) refreshEnv();
  }
}

async function installEnv(id) {
  return runEnvAction(id, "installEnv", "安装");
}

async function reinstallEnv(id) {
  return runEnvAction(id, "reinstallEnv", "重装");
}

async function installAllEnv() {
  if (envBusy) return;
  // 先自己算一遍要装哪些，避免为了「拿 id 列表」多调一次 scanEnv。
  // 只取 is-missing —— 过期项是 is-outdated，天然被排除在批量安装之外。
  // 这是故意的：批量走的是 install，而重装会先卸载，两者破坏性不同，
  // 不该由一个「一键」按钮替用户决定。
  const cards = [...document.querySelectorAll(".env-card.is-missing")]
    .map((card) => card.dataset.envCard)
    .filter((id) => {
      const button = document.querySelector(`[data-env-install="${id}"]`);
      return Boolean(button);
    });
  if (!cards.length) {
    showToast("success", "没有可自动安装的缺失项");
    return;
  }
  setEnvBusy(true);
  const chunks = [];
  try {
    for (const id of cards) {
      // 逐个装而不是并发：winget 有全局安装锁，并发只会互相抢
      const result = await invoke("installEnv", id);
      if (result && result.detail) chunks.push(`### ${id}\n${result.detail}`);
      if (result && result.env) renderEnv(result.env);
    }
    showEnvDetail("批量部署输出", chunks.join("\n\n"));
    showToast("success", `已依次尝试部署 ${cards.length} 项`);
  } catch (error) {
    chunks.push(`### 失败\n${error.message || error}`);
    showEnvDetail("批量部署中断", chunks.join("\n\n"));
  } finally {
    setEnvBusy(false);
    refreshEnv();
  }
}

function closeCustomSelects(except = null) {
  document.querySelectorAll(".custom-select.is-open").forEach((root) => {
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
  byId("portable-config-import").addEventListener("click", () => {
    if (appState?.capabilities?.browserFiles) {
      openBrowserTextFile(".json", (_file, content) => invoke("importPortableConfigText", content));
    } else invoke("importPortableConfig");
  });
  byId("quit-application")?.addEventListener("click", () => performWindowAction("quit"));
  byId("portable-config-export").addEventListener("click", async () => {
    if (appState?.capabilities?.browserFiles) {
      const content = await invoke("exportPortableConfigText");
      if (content) downloadTextFile("network-manager-config.json", content);
    } else invoke("exportPortableConfig");
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
    if (event.target.checked) (appState.sshServers || []).forEach((server) => sshSelection.add(server.profileId));
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
  byId("clear-logs").addEventListener("click", () => { invoke("clearLogs"); refreshLogs(); });
  byId("open-logs").addEventListener("click", () => invoke("openLogs"));

  // 环境管理页
  byId("env-refresh").addEventListener("click", () => refreshEnv());
  byId("env-install-all").addEventListener("click", () => installAllEnv());
  byId("env-detail-close").addEventListener("click", () => { byId("env-detail").hidden = true; });
  // 卡片里的按钮是渲染出来的，用事件委托绑一次就够，不用每张卡重绑
  byId("env-grid").addEventListener("click", (event) => {
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
