const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;
let appState = null;
let draftPaths = [];
let toastTimer = null;
let sending = false;
let appVersion = "";
let activityFilter = "all";
let activitySearch = "";
const $ = (selector) => document.querySelector(selector);
const fileName = (path) => path.split(/[\\/]/).pop() || path;
const escapeHtml = (value) => String(value ?? "").replace(/[&<>'"]/g, (char) => ({"&":"&amp;","<":"&lt;",">":"&gt;","'":"&#39;",'"':"&quot;"})[char]);

function toast(message, error = false) {
  const element = $("#toast");
  element.textContent = message;
  element.className = `toast show${error ? " error" : ""}`;
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => element.className = "toast", 3200);
}
async function loadAppVersion() {
  try {
    appVersion = await invoke("get_app_version");
    $("#app-version").textContent = `v${appVersion}`;
    $("#about-version").textContent = `版本 v${appVersion}`;
    document.title = `OTrans v${appVersion}`;
  } catch (error) {
    console.warn("Unable to read app version", error);
    $("#app-version").textContent = "版本未知";
    $("#about-version").textContent = "版本未知";
  }
}
async function call(command, args = {}) {
  try { return await invoke(command, args); }
  catch (error) { toast(String(error), true); throw error; }
}
function routeText(state) {
  const c = state.connection;
  if (!state.paired) return "尚未配对，请在设置中连接飞牛";
  const route = c.activeRoute === "lan" ? "内网" : c.activeRoute === "wan" ? "外网" : "离线";
  const realtime = c.realtimeStatus === "connected" ? "实时已连接" : c.realtimeStatus === "connecting" ? "实时连接中" : "实时未连接";
  let peer = "局域网直传未启用";
  if (state.settings.lanPeerEnabled) {
    const count = Number(state.lanPeerSourceCount) || 0;
    peer = c.lanPeerStatus === "ready" ? `直传服务已监听:${c.lanPeerPort}（可提供 ${count} 个文件，非传输结果）` : (c.lanPeerDetail || "局域网直传启动中");
  }
  return `${route} · ${realtime} · ${peer}`;
}
function renderActivity() {
  const activity = appState?.activity || [];
  const query = activitySearch.trim().toLocaleLowerCase();
  const visible = activity.filter((item) => {
    const matchesType = activityFilter === "all" || item.kind === activityFilter;
    const matchesQuery = !query || `${item.message} ${item.route || ""}`.toLocaleLowerCase().includes(query);
    return matchesType && matchesQuery;
  });
  const routeNames = { lan:"局域网直传", nas:"NAS", mixed:"直传 + NAS", text:"纯文本" };
  $("#activity-count").textContent = visible.length === activity.length ? String(activity.length) : `${visible.length}/${activity.length}`;
  $("#activity-tab-count").textContent = String(activity.length);
  $("#clear-activity-button").disabled = activity.length === 0;
  $("#activity-list").innerHTML = visible.length ? visible.map((item) => {
    const route = routeNames[item.route] ? `<span class="route-badge route-${escapeHtml(item.route)}">${routeNames[item.route]}</span>` : "";
    const open = item.path ? `<button type="button" class="activity-open" data-open-activity="${escapeHtml(item.id)}">打开</button>` : "";
    return `<div class="activity ${escapeHtml(item.kind)}"><i class="activity-dot"></i><p>${route}${escapeHtml(item.message)}</p><div class="activity-tail"><time>${escapeHtml(formatTime(item.at))}</time>${open}</div></div>`;
  }).join("") : `<div class="empty-state"><strong>${activity.length ? "没有符合条件的记录" : "暂无传送记录"}</strong><span>${activity.length ? "尝试调整筛选条件或搜索词" : "发送或收到内容后会显示在这里"}</span></div>`;
}
function switchTab(name) {
  document.querySelectorAll("[data-tab]").forEach((button) => button.classList.toggle("active", button.dataset.tab === name));
  document.querySelectorAll("[data-tab-panel]").forEach((panel) => {
    const active = panel.dataset.tabPanel === name;
    panel.hidden = !active;
    panel.classList.toggle("active", active);
  });
}
function render(state) {
  appState = state;
  const pill = $("#connection-pill");
  const route = state.connection.activeRoute;
  const connecting = state.paired && state.connection.realtimeStatus === "connecting";
  pill.className = `connection-pill ${connecting ? "connecting" : route || "offline"}`;
  pill.querySelector("b").textContent = connecting ? "连接中" : state.paired ? (route === "lan" ? "内网已连接" : route === "wan" ? "外网已连接" : "已断开") : "未连接";
  pill.title = routeText(state);
  $("#sync-button").disabled = state.syncing || !state.paired;
  $("#sync-button").textContent = state.syncing ? "同步中…" : "立即同步";
  $("#clipboard-button").disabled = !state.paired;
  $("#choose-files-button").disabled = !state.paired;
  $("#pair-button").disabled = state.paired;
  $("#disconnect-button").disabled = !state.paired;
  renderDraft();
  renderActivity();
}
function formatTime(value) {
  const date = new Date(value);
  return Number.isNaN(date.valueOf()) ? "" : date.toLocaleString("zh-CN", { month:"2-digit", day:"2-digit", hour:"2-digit", minute:"2-digit" });
}
function addPaths(paths) {
  const known = new Set(draftPaths.map((path) => path.toLocaleLowerCase()));
  for (const path of paths || []) {
    if (!path || known.has(path.toLocaleLowerCase())) continue;
    draftPaths.push(path); known.add(path.toLocaleLowerCase());
  }
  renderDraft();
}
function renderDraft() {
  const text = $("#draft-text").value.trim();
  $("#draft-count").textContent = draftPaths.length ? `${draftPaths.length} 个附件${text ? " + 文字" : ""}` : (text ? "仅文字" : "暂无附件");
  $("#send-button").disabled = sending || appState?.paired === false || (!text && draftPaths.length === 0);
  $("#send-button").textContent = sending ? "正在上传…" : "发送到中转站";
  $("#draft-files").innerHTML = draftPaths.map((path, index) => `<div class="file-chip"><span class="file-icon">${escapeHtml((fileName(path).split('.').pop() || 'FILE').slice(0,4).toUpperCase())}</span><div><strong title="${escapeHtml(path)}">${escapeHtml(fileName(path))}</strong><small>${escapeHtml(path)}</small></div><button data-remove="${index}" title="移除">×</button></div>`).join("");
}
function fillSettings() {
  const s = appState.settings;
  $("#settings-lan-url").value = s.lanServerUrl;
  $("#settings-wan-url").value = s.wanServerUrl;
  $("#settings-device-name").value = s.deviceName;
  $("#settings-inbox").value = s.inboxPath;
  $("#settings-outbox").value = s.outboxPath;
  $("#settings-lan-peer").checked = s.lanPeerEnabled;
  $("#settings-autostart").checked = s.startAtLogin;
  $("#settings-peer-detail").textContent = appState.connection.lanPeerDetail || "动态端口仅供同一局域网设备访问，不需要映射到公网。";
  $("#settings-pair-code").value = "";
}
function settingsInput() {
  return {
    lanServerUrl: $("#settings-lan-url").value,
    wanServerUrl: $("#settings-wan-url").value,
    deviceName: $("#settings-device-name").value,
    inboxPath: $("#settings-inbox").value,
    outboxPath: $("#settings-outbox").value,
    startAtLogin: $("#settings-autostart").checked,
    lanPeerEnabled: $("#settings-lan-peer").checked
  };
}
async function initializeDragDrop() {
  const zone = $("#drop-zone");
  for (const name of ["dragenter", "dragover"]) document.addEventListener(name, (event) => { event.preventDefault(); zone.classList.add("dragging"); });
  for (const name of ["dragleave", "drop"]) document.addEventListener(name, (event) => { event.preventDefault(); zone.classList.remove("dragging"); });
  document.addEventListener("drop", (event) => {
    const text = event.dataTransfer?.getData("text/plain")?.trim();
    if (text && !(event.dataTransfer?.files?.length)) $("#draft-text").value = [$("#draft-text").value.trim(), text].filter(Boolean).join("\n");
    renderDraft();
  });
  try {
    await window.__TAURI__.webview.getCurrentWebview().onDragDropEvent((event) => {
      const payload = event.payload;
      zone.classList.toggle("dragging", payload.type === "over");
      if (payload.type === "drop") addPaths(payload.paths || []);
      if (payload.type === "cancel") zone.classList.remove("dragging");
    });
  } catch (error) { console.warn("Native drag and drop unavailable", error); }
}

window.addEventListener("DOMContentLoaded", async () => {
  await loadAppVersion();
  render(await call("get_state"));
  renderDraft();
  await initializeDragDrop();
  await listen("state-changed", ({ payload }) => render(payload));
  await listen("incoming-transfer", ({ payload }) => toast(`${payload.route} · 收到：${payload.title}`));
  document.querySelectorAll("[data-tab]").forEach((button) => button.addEventListener("click", () => switchTab(button.dataset.tab)));
  $("#draft-text").addEventListener("input", renderDraft);
  $("#draft-text").addEventListener("keydown", (event) => {
    if ((event.ctrlKey || event.metaKey) && event.key === "Enter" && !$("#send-button").disabled) {
      event.preventDefault();
      $("#send-button").click();
    }
  });
  $("#draft-files").addEventListener("click", (event) => { const index = event.target.dataset.remove; if (index !== undefined) { draftPaths.splice(Number(index), 1); renderDraft(); } });
  $("#clear-draft").addEventListener("click", () => { draftPaths = []; $("#draft-text").value = ""; renderDraft(); });
  $("#choose-files-button").addEventListener("click", async () => addPaths(await call("choose_files")));
  $("#clipboard-button").addEventListener("click", async () => {
    const content = await call("read_clipboard");
    if (content.text) $("#draft-text").value = [$("#draft-text").value.trim(), content.text].filter(Boolean).join("\n");
    addPaths(content.paths);
    toast(content.kind === "empty" ? "剪切板里没有可发送的内容" : "已加入剪切板内容");
  });
  $("#send-button").addEventListener("click", async () => {
    sending = true; renderDraft();
    try { render(await call("send_draft", { input: { text: $("#draft-text").value, paths: draftPaths } })); draftPaths=[]; $("#draft-text").value=""; toast("已发送到中转站"); }
    finally { sending=false; renderDraft(); }
  });
  $("#pair-button").addEventListener("click", async () => {
    const settings = settingsInput();
    await call("pair", { input: { lanServerUrl: settings.lanServerUrl, wanServerUrl: settings.wanServerUrl, deviceName: settings.deviceName, pairCode: $("#settings-pair-code").value } });
    render(await call("save_settings", { input: settings }));
    $("#settings-pair-code").value=""; $("#settings-dialog").close(); toast("配对成功");
  });
  $("#sync-button").addEventListener("click", async () => { render(await call("sync_now")); toast("同步完成"); });
  document.querySelectorAll("[data-activity-filter]").forEach((button) => button.addEventListener("click", () => {
    activityFilter = button.dataset.activityFilter;
    document.querySelectorAll("[data-activity-filter]").forEach((candidate) => candidate.classList.toggle("active", candidate === button));
    renderActivity();
  }));
  $("#activity-list").addEventListener("click", async (event) => {
    const button = event.target.closest("[data-open-activity]");
    if (!button) return;
    const item = (appState.activity || []).find((candidate) => candidate.id === button.dataset.openActivity);
    if (item?.path) await call("open_path", { path: item.path });
  });
  $("#activity-search").addEventListener("input", (event) => { activitySearch = event.target.value; renderActivity(); });
  $("#clear-activity-button").addEventListener("click", async () => {
    if (!confirm("清空这台电脑上的传送动态？这不会删除中转站内容或已接收文件。")) return;
    render(await call("clear_activity"));
    toast("本地传送动态已清空");
  });
  $("#test-notification-button").addEventListener("click", async () => toast(await call("test_notification")));
  $("#about-button").addEventListener("click", () => {
    $("#settings-dialog").close();
    $("#about-dialog").showModal();
  });
  $("#close-about").addEventListener("click", () => $("#about-dialog").close());
  $("#open-inbox-button").addEventListener("click", () => call("open_path", { path: appState.settings.inboxPath }));
  $("#open-outbox-button").addEventListener("click", () => call("open_path", { path: appState.settings.outboxPath }));
  $("#settings-button").addEventListener("click", () => { fillSettings(); $("#settings-dialog").showModal(); });
  $("#close-settings").addEventListener("click", () => $("#settings-dialog").close());
  $("#cancel-settings").addEventListener("click", () => $("#settings-dialog").close());
  document.querySelectorAll("[data-pick]").forEach((button) => button.addEventListener("click", async () => {
    const outbox = button.dataset.pick === "outbox";
    const path = await call("choose_directory", { title: outbox ? "选择自动发送文件夹" : "选择接收文件夹" });
    if (path) $(outbox ? "#settings-outbox" : "#settings-inbox").value = path;
  }));
  $("#settings-form").addEventListener("submit", async (event) => {
    event.preventDefault();
    render(await call("save_settings", { input: settingsInput() }));
    $("#settings-dialog").close(); toast("设置已保存");
  });
  $("#disconnect-button").addEventListener("click", async () => {
    if (!confirm("确定断开这台电脑的配对？本地已接收文件不会删除。")) return;
    render(await call("disconnect")); $("#settings-dialog").close(); toast("已断开配对");
  });
});
