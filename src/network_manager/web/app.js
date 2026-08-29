"use strict";

const SESSION_TOKEN = document.querySelector('meta[name="network-session-token"]')?.content || "";
let appState = null;
let currentPage = "overview";
let currentNodeTab = "local";
let sourceSignature = "";
let settingsInitialized = false;
let refreshing = false;

const PAGE_TITLES = {
  overview: "运行概览",
  rules: "分流规则",
  nodes: "代理与节点",
  servers: "SSH 服务器",
  settings: "设置",
  logs: "运行日志",
};

const MODE_META = [
  ["RULE", "规则分流", "按分流规则选择出口"],
  ["GLOBAL_CLASH", "全局 Clash", "所有流量经 Clash 转发"],
  ["GLOBAL_V2RAY", "全局 v2ray", "所有流量经 v2ray 转发"],
  ["GLOBAL_SSH", "全局 SSH", "所有流量经当前 SSH 服务器转发"],
  ["GLOBAL_BUILTIN", "全局节点", "所有流量经当前内置节点转发"],
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
  ["SSH", "SSH 服务器"],
  ["BUILTIN", "内置节点"],
  ["DIRECT", "直连"],
];

const byId = (id) => document.getElementById(id);

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
    window.setTimeout(refreshState, 80);
    return payload.result;
  } catch (error) {
    showToast("error", error.message || "操作失败");
    return null;
  }
}

function setPage(page) {
  if (!PAGE_TITLES[page]) return;
  currentPage = page;
  document.querySelectorAll(".nav-item").forEach((item) => {
    item.classList.toggle("is-active", item.dataset.page === page);
  });
  document.querySelectorAll(".page").forEach((item) => {
    item.classList.toggle("is-active", item.id === `page-${page}`);
  });
  byId("page-title").textContent = PAGE_TITLES[page];
  if (page === "logs") refreshLogs();
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
  if (refreshing) return;
  refreshing = true;
  try {
    const response = await apiRequest("/api/state");
    appState = await response.json();
    renderState();
    (appState.toasts || []).forEach((item) => showToast(item.kind, item.message));
  } catch (_error) {
    byId("overview-status-detail").textContent = "后台连接中断，正在重试";
  } finally {
    refreshing = false;
  }
}

function renderState() {
  if (!appState) return;
  renderChrome();
  renderOverview();
  renderRules();
  renderSources();
  renderNodes();
  renderSubscriptions();
  renderSshServers();
  renderSettings();
  if (currentPage === "logs") refreshLogs();
}

function renderChrome() {
  const { core, traffic } = appState;
  const badge = byId("header-status");
  badge.textContent = core.status;
  badge.className = `status-badge${core.running ? " is-running" : ""}${core.busy ? " is-busy" : ""}`;
  renderSidebarRate("upload", traffic.uploadRate);
  renderSidebarRate("download", traffic.downloadRate);
  byId("sidebar-memory").textContent = traffic.memoryMb;
  renderSidebarTrafficChart(traffic.downloadSamples, traffic.uploadSamples);
}

function renderSidebarRate(direction, rate) {
  const parts = String(rate || "0 B/s").trim().split(/\s+/);
  const rawAmount = parts[0] || "0";
  byId(`sidebar-${direction}`).textContent = Number(rawAmount) === 0 ? "0.00" : rawAmount;
  byId(`sidebar-${direction}-unit`).textContent = parts.slice(1).join(" ") || "B/s";
}

function renderSidebarTrafficChart(downloadValues, uploadValues) {
  const downloads = normalizedSamples(downloadValues);
  const uploads = normalizedSamples(uploadValues);
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
  byId("admin-banner").classList.toggle("is-hidden", core.admin);
  byId("overview-status-title").textContent = core.running ? "全流量接管运行中" : "全流量接管已停止";
  byId("overview-status-detail").textContent = core.running
    ? `${core.modeLabel} · 本地入口 127.0.0.1:${core.mixedPort}`
    : "当前不会修改系统路由";

  const toggle = byId("core-toggle");
  toggle.disabled = core.busy;
  toggle.className = `button large ${core.running ? "secondary" : "primary"}`;
  toggle.innerHTML = core.busy
    ? `${icon("refresh-cw")}<span>处理中</span>`
    : core.running
      ? `${icon("square")}<span>停止接管</span>`
      : `${icon("play")}<span>启动接管</span>`;

  byId("mode-switch").innerHTML = MODE_META.map(([mode, label]) => {
    const disabled = (mode === "GLOBAL_BUILTIN" && appState.nodes.length === 0)
      || (mode === "GLOBAL_SSH" && appState.sshServers.length === 0);
    return `<button class="segment${core.mode === mode ? " is-active" : ""}" data-mode="${mode}"${disabled ? " disabled" : ""}>${label}</button>`;
  }).join("");
  const modeMeta = MODE_META.find(([mode]) => mode === core.mode);
  byId("mode-description").textContent = modeMeta ? modeMeta[2] : core.modeLabel;

  renderSourceStatus("clash", sources.clash);
  renderSourceStatus("v2ray", sources.v2ray);
  renderSourceStatus("ssh", sources.ssh);
  byId("summary-process").textContent = summary.processRules;
  byId("summary-network").textContent = summary.networkRules;
  byId("summary-nodes").textContent = summary.nodes;
  byId("summary-default").textContent = summary.defaultTarget;

  byId("traffic-status").textContent = traffic.status;
  byId("traffic-status").classList.toggle("is-running", core.running);
  byId("traffic-download").textContent = traffic.downloadRate;
  byId("traffic-upload").textContent = traffic.uploadRate;
  byId("traffic-connections").textContent = traffic.connections;
  byId("traffic-download-total").textContent = traffic.downloadTotal;
  byId("traffic-upload-total").textContent = traffic.uploadTotal;
  renderTrafficChart(traffic.downloadSamples, traffic.uploadSamples);
  byId("exit-ip").textContent = appState.exitIp;
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
  byId("rules-empty").classList.toggle("is-hidden", rules.length > 0);
  byId("rules-body").innerHTML = rules.map((rule) => {
    const statusClass = rule.partiallyEnabled ? " is-partial" : rule.enabled ? " is-enabled" : "";
    const statusText = rule.partiallyEnabled ? "部分启用" : rule.enabled ? "启用" : "停用";
    if (rule.kind === "group") {
      return `<tr class="rule-group-row">
        <td><span class="rule-status${statusClass}">${statusText}</span></td>
        <td><span class="group-label">${escapeHtml(rule.ruleTypeLabel)}</span></td>
        <td title="${escapeHtml(rule.detail)}"><strong>${escapeHtml(rule.note)}</strong><small>${rule.count} 条匹配</small></td>
        <td><span class="chip">${escapeHtml(rule.targetLabel)}</span></td>
        <td>整组共用一个出口</td>
        <td class="actions">
          <button class="mini-button" data-rule-action="toggle" data-rule-group="${rule.groupId}" title="${rule.enabled ? "停用整组" : "启用整组"}">${icon(rule.enabled ? "square" : "check")}</button>
          <button class="mini-button" data-rule-action="edit" data-rule-group="${rule.groupId}" title="切换整组出口">${icon("square-pen")}</button>
          <button class="mini-button danger" data-rule-action="delete" data-rule-group="${rule.groupId}" title="删除整组">${icon("trash-2")}</button>
        </td>
      </tr>`;
    }
    return `<tr>
      <td><span class="rule-status${statusClass}">${statusText}</span></td>
      <td title="${escapeHtml(rule.ruleType)}">${escapeHtml(rule.ruleTypeLabel)}</td>
      <td title="${escapeHtml(rule.value)}">${escapeHtml(rule.value)}</td>
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
}

function renderSources() {
  const sources = appState.sources;
  const signature = JSON.stringify(sources);
  if (signature === sourceSignature) return;
  if (byId("source-editor").contains(document.activeElement)) return;
  sourceSignature = signature;
  byId("source-editor").innerHTML = ["clash", "v2ray"].map((key) => {
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
}

function renderNodes() {
  const sourceNodes = appState.nodes;
  const testing = sourceNodes.some((node) => node.latencyStatus === "testing");
  const nodes = testing ? sourceNodes : [...sourceNodes].sort(compareNodeLatency);
  const testButton = byId("test-all-nodes");
  testButton.disabled = !appState.core.running || nodes.length === 0 || testing;
  testButton.querySelector("span:last-child").textContent = testing ? "测速中" : "批量测速";
  byId("nodes-empty").classList.toggle("is-hidden", nodes.length > 0);
  byId("node-grid").innerHTML = nodes.map((node) => {
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
          <div class="node-card-actions"><span class="node-latency ${latencyLevel}">${latencyText}</span><button class="mini-button danger" data-node-delete="${node.index}" title="删除节点" aria-label="删除节点">${icon("trash-2")}</button></div>
        </div>
        <div class="node-card-meta"><span class="node-badge">${escapeHtml(node.protocol)}</span></div>
        <div class="node-card-foot">
          <span title="${escapeHtml(node.source)}">${escapeHtml(node.source || "手动导入")}</span>
          <span title="${escapeHtml(node.server)}">${escapeHtml(node.server)}</span>
        </div>
      </article>`;
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
      <div class="subscription-meta"><span>${source.nodeCount} 个节点</span><span>${escapeHtml(formatTime(source.lastUpdated))}</span></div>
    </article>`).join("");
}

function renderSshServers() {
  const servers = appState.sshServers || [];
  const tunnel = appState.sshTunnel || { status: "stopped", running: false };
  byId("ssh-servers-empty").classList.toggle("is-hidden", servers.length > 0);
  const labels = {
    stopped: ["SSH 隧道未连接", "选择服务器后建立本地 SOCKS5 出口"],
    connecting: ["正在连接 SSH 服务器", "正在验证服务器与账号，请稍候"],
    connected: ["SSH 隧道已连接", `本地 SOCKS5：127.0.0.1:${tunnel.localPort}`],
    error: ["SSH 隧道连接失败", tunnel.error || "请检查服务器和认证信息"],
  };
  const [title, detail] = labels[tunnel.status] || labels.stopped;
  byId("ssh-status-title").textContent = title;
  byId("ssh-status-detail").textContent = detail;
  byId("stop-ssh").disabled = tunnel.status === "stopped";
  byId("test-ssh-exit").disabled = !tunnel.running;
  const authLabels = { password: "密码", key: "私钥", agent: "SSH Agent" };
  byId("ssh-server-grid").innerHTML = servers.map((server) => {
    const active = tunnel.profileId === server.profileId && tunnel.running;
    const connecting = tunnel.profileId === server.profileId && tunnel.status === "connecting";
    return `<article class="ssh-server-card${active ? " is-active" : ""}">
      <div class="ssh-server-card-head">
        <div><h3 title="${escapeHtml(server.name)}">${escapeHtml(server.name)}</h3><p>${escapeHtml(server.username)}@${escapeHtml(server.host)}:${server.port}</p></div>
        <span class="small-status${active ? " is-running" : ""}">${active ? "已连接" : connecting ? "连接中" : "未连接"}</span>
      </div>
      <div class="ssh-server-meta">
        <span><small>认证</small><strong>${authLabels[server.authMethod] || server.authMethod}</strong></span>
        <span><small>本地 SOCKS5</small><strong>127.0.0.1:${server.localPort}</strong></span>
      </div>
      <div class="ssh-server-actions">
        <button class="button primary" data-ssh-action="connect" data-profile-id="${server.profileId}"${tunnel.status === "connecting" || active ? " disabled" : ""}>${icon("play")}<span>连接</span></button>
        <button class="icon-button" data-ssh-action="edit" data-profile-id="${server.profileId}" title="编辑" aria-label="编辑">${icon("square-pen")}</button>
        <button class="icon-button danger" data-ssh-action="delete" data-profile-id="${server.profileId}" title="删除" aria-label="删除">${icon("trash-2")}</button>
      </div>
    </article>`;
  }).join("");
}

function renderSettings() {
  if (settingsInitialized || byId("settings-form").contains(document.activeElement)) return;
  const settings = appState.settings;
  byId("setting-mixed-port").value = settings.mixedPort;
  byId("setting-controller-port").value = settings.controllerPort;
  byId("setting-dns-port").value = settings.dnsPort;
  byId("setting-strict-route").checked = settings.strictRoute;
  byId("setting-start-on-launch").checked = settings.startOnLaunch;
  byId("setting-close-to-tray").checked = settings.closeToTray;
  byId("setting-start-with-windows").checked = settings.startWithWindows;
  settingsInitialized = true;
}

async function refreshLogs() {
  try {
    const response = await apiRequest("/api/logs");
    const logs = await response.text();
    const output = byId("log-output");
    const shouldFollow = output.scrollTop + output.clientHeight >= output.scrollHeight - 30;
    output.textContent = logs || "暂无运行日志";
    if (shouldFollow) output.scrollTop = output.scrollHeight;
  } catch (_error) { /* state polling reports connectivity */ }
}

function openRuleDialog(rule = null) {
  const typeOptions = RULE_TYPES.map(([value, label]) => `<option value="${value}"${rule?.ruleType === value ? " selected" : ""}>${label}</option>`).join("");
  const targetOptions = TARGETS.map(([value, label]) => `<option value="${value}"${rule?.target === value ? " selected" : ""}>${label}</option>`).join("");
  openModal(rule ? "编辑规则" : "添加规则", `
    <div class="form-grid">
      <label><span>匹配类型</span><select id="modal-rule-type">${typeOptions}</select></label>
      <label><span>匹配内容</span><input id="modal-rule-value" value="${escapeHtml(rule?.value || "")}" placeholder="例如 example.com"></label>
      <label><span>流量去向</span><select id="modal-rule-target">${targetOptions}</select></label>
      <label><span>备注</span><input id="modal-rule-note" value="${escapeHtml(rule?.note || "")}" placeholder="可选"></label>
      <label class="switch-row"><span><strong>启用规则</strong><small>保存后立即应用</small></span><input id="modal-rule-enabled" type="checkbox"${rule?.enabled === false ? "" : " checked"}><i></i></label>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "保存", kind: "primary", action: () => {
      const payload = {
        index: rule?.index ?? -1,
        ruleType: byId("modal-rule-type").value,
        value: byId("modal-rule-value").value,
        target: byId("modal-rule-target").value,
        note: byId("modal-rule-note").value,
        enabled: byId("modal-rule-enabled").checked,
      };
      invoke("saveRule", JSON.stringify(payload));
      closeModal();
      window.setTimeout(refreshState, 250);
    } },
  ]);
}

function openRuleGroupDialog(group) {
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
  openModal("常用海外站点", `
    <div class="form-grid">
      <div><span class="field-label">整组流量去向</span><div class="group-target-options" role="radiogroup">${targetOptions}</div></div>
      <div class="group-dialog-summary"><strong>${group.count} 条匹配</strong><span>Google、ChatGPT、Claude、YouTube、GitHub 等常用海外站点</span></div>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "应用到整组", kind: "primary", action: () => {
      invoke("saveRuleGroup", JSON.stringify({
        groupId: group.groupId,
        target: document.querySelector(".group-target-option.is-active")?.dataset.groupTarget,
      }));
      closeModal();
      window.setTimeout(refreshState, 250);
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

function openPasteDialog() {
  openModal("粘贴导入", `
    <div class="form-grid">
      <label><span>来源名称</span><input id="modal-import-name" value="手动导入"></label>
      <label><span>订阅或节点内容</span><textarea id="modal-import-content" placeholder="支持 Clash YAML、Base64 订阅及常见节点链接"></textarea></label>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "开始导入", kind: "primary", action: () => {
      invoke("importPaste", byId("modal-import-name").value, byId("modal-import-content").value);
      closeModal();
    } },
  ]);
}

function openSshServerDialog(server = null) {
  const authMethod = server?.authMethod || "password";
  openModal(server ? "编辑 SSH 服务器" : "添加 SSH 服务器", `
    <div class="form-grid ssh-form-grid">
      <label class="wide-field"><span>名称</span><input id="modal-ssh-name" value="${escapeHtml(server?.name || "我的服务器")}" required></label>
      <label><span>服务器 IP / 域名</span><input id="modal-ssh-host" value="${escapeHtml(server?.host || "")}" placeholder="例如 203.0.113.10" required></label>
      <label><span>SSH 端口</span><input id="modal-ssh-port" type="number" min="1" max="65535" value="${server?.port || 22}" required></label>
      <label><span>用户名</span><input id="modal-ssh-username" value="${escapeHtml(server?.username || "root")}" required></label>
      <label><span>本地 SOCKS5 端口</span><input id="modal-ssh-local-port" type="number" min="1024" max="65535" value="${server?.localPort || 10888}" required></label>
      <label><span>认证方式</span><select id="modal-ssh-auth"><option value="password"${authMethod === "password" ? " selected" : ""}>账号密码</option><option value="key"${authMethod === "key" ? " selected" : ""}>私钥文件</option><option value="agent"${authMethod === "agent" ? " selected" : ""}>SSH Agent</option></select></label>
      <label id="modal-ssh-secret-field"><span>密码 / 私钥口令</span><input id="modal-ssh-password" type="password" autocomplete="new-password" placeholder="${server?.hasCredential ? "已安全保存，留空保持不变" : "连接凭据"}"></label>
      <label id="modal-ssh-key-field" class="wide-field"><span>私钥路径</span><div class="input-action"><input id="modal-ssh-key-path" value="${escapeHtml(server?.keyPath || "")}" placeholder="选择 OpenSSH 私钥"><button id="modal-pick-ssh-key" type="button" class="icon-button" title="选择私钥" aria-label="选择私钥">${icon("folder-open")}</button></div></label>
      <label class="switch-row wide-field"><span><strong>记住凭据</strong><small>Windows 下使用当前用户 DPAPI 加密</small></span><input id="modal-ssh-remember" type="checkbox"${server?.rememberPassword ? " checked" : ""}><i></i></label>
      <label class="switch-row wide-field"><span><strong>启动后自动连接</strong><small>仅在凭据可用或使用密钥时生效</small></span><input id="modal-ssh-auto" type="checkbox"${server?.autoConnect ? " checked" : ""}><i></i></label>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "保存", kind: "primary", action: () => {
      const payload = {
        profileId: server?.profileId || "",
        name: byId("modal-ssh-name").value,
        host: byId("modal-ssh-host").value,
        port: Number(byId("modal-ssh-port").value),
        username: byId("modal-ssh-username").value,
        localPort: Number(byId("modal-ssh-local-port").value),
        authMethod: byId("modal-ssh-auth").value,
        keyPath: byId("modal-ssh-key-path").value,
        rememberPassword: byId("modal-ssh-remember").checked,
        autoConnect: byId("modal-ssh-auto").checked,
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
  byId("modal-pick-ssh-key").addEventListener("click", async () => {
    const path = await invoke("pickSshKey");
    if (path) byId("modal-ssh-key-path").value = path;
  });
  updateAuthFields();
}

function connectSshServer(server) {
  if (server.authMethod === "agent" || server.hasCredential) {
    invoke("startSshServer", server.profileId, "");
    return;
  }
  const label = server.authMethod === "key" ? "私钥口令（没有可留空）" : "SSH 密码";
  openModal(`连接 ${server.name}`, `
    <div class="form-grid">
      <div class="group-dialog-summary"><strong>${escapeHtml(server.username)}@${escapeHtml(server.host)}:${server.port}</strong><span>本地 SOCKS5：127.0.0.1:${server.localPort}</span></div>
      <label><span>${label}</span><input id="modal-connect-password" type="password" autocomplete="current-password" autofocus></label>
    </div>`, [
    { label: "取消", kind: "secondary", action: closeModal },
    { label: "连接", kind: "primary", action: () => {
      invoke("startSshServer", server.profileId, byId("modal-connect-password").value);
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
  dialog.showModal();
}

function closeModal() {
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

function bindEvents() {
  document.querySelectorAll(".nav-item").forEach((item) => item.addEventListener("click", () => setPage(item.dataset.page)));
  document.querySelectorAll(".tab").forEach((item) => item.addEventListener("click", () => setNodeTab(item.dataset.nodeTab)));
  byId("header-refresh").addEventListener("click", refreshState);
  byId("core-toggle").addEventListener("click", () => invoke("toggleCore"));
  byId("mode-switch").addEventListener("click", (event) => {
    const button = event.target.closest("[data-mode]");
    if (button && !button.disabled) invoke("setMode", button.dataset.mode);
  });
  byId("test-exit").addEventListener("click", () => invoke("testExit"));
  byId("add-rule").addEventListener("click", () => openRuleDialog());
  byId("rules-body").addEventListener("click", (event) => {
    const button = event.target.closest("[data-rule-action]");
    if (!button) return;
    const action = button.dataset.ruleAction;
    const groupId = button.dataset.ruleGroup;
    if (groupId) {
      const group = appState.rules.find((rule) => rule.groupId === groupId);
      if (action === "edit") openRuleGroupDialog(group);
      else if (action === "delete") confirmAction("删除规则组", "确定删除常用海外站点规则组？", () => invoke("ruleGroupAction", groupId, "delete"));
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
  byId("import-file").addEventListener("click", () => invoke("importFile"));
  byId("test-all-nodes").addEventListener("click", () => invoke("testAllNodes"));
  byId("node-grid").addEventListener("click", (event) => {
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
    invoke("addSubscription", byId("subscription-name").value, byId("subscription-url").value);
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
  byId("stop-ssh").addEventListener("click", () => invoke("stopSshServer"));
  byId("test-ssh-exit").addEventListener("click", () => invoke("testSshExit"));
  byId("ssh-server-grid").addEventListener("click", (event) => {
    const button = event.target.closest("[data-ssh-action]");
    if (!button) return;
    const server = appState.sshServers.find((item) => item.profileId === button.dataset.profileId);
    if (!server) return;
    if (button.dataset.sshAction === "connect") connectSshServer(server);
    else if (button.dataset.sshAction === "edit") openSshServerDialog(server);
    else confirmAction("删除 SSH 服务器", `确定删除“${server.name}”？`, () => invoke("deleteSshServer", server.profileId));
  });
  byId("settings-form").addEventListener("submit", (event) => {
    event.preventDefault();
    invoke("saveSettings", JSON.stringify({
      mixedPort: Number(byId("setting-mixed-port").value),
      controllerPort: Number(byId("setting-controller-port").value),
      dnsPort: Number(byId("setting-dns-port").value),
      strictRoute: byId("setting-strict-route").checked,
      startOnLaunch: byId("setting-start-on-launch").checked,
      closeToTray: byId("setting-close-to-tray").checked,
      startWithWindows: byId("setting-start-with-windows").checked,
    }));
    settingsInitialized = false;
  });
  byId("clear-logs").addEventListener("click", () => { invoke("clearLogs"); refreshLogs(); });
  byId("open-logs").addEventListener("click", () => invoke("openLogs"));
  byId("modal-close").addEventListener("click", closeModal);
  byId("app-modal").addEventListener("click", (event) => {
    if (event.target === byId("app-modal")) closeModal();
  });
}

function initialize() {
  bindEvents();
  refreshState();
  window.setInterval(refreshState, 1000);
}

initialize();
