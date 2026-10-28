"use strict";

const $ = (id) => document.getElementById(id);
const state = {
  encryptionKeys: [],
  connection: null,
  connections: [],
  selectedUrl: null,
  editingUrl: null,
  connectionAttempt: null,
  session: new AbortController(),
  connected: false,
  timer: null,
  cursors: [null],
  page: 0,
  displayedPage: 0,
  next: null,
  jobs: [],
  repository: "",
  backups: [],
  selection: null,
  jobsRequest: 0,
  repositoryRequest: 0,
};
const statuses = {
  queued: "排队中",
  running: "执行中",
  succeeded: "已完成",
  failed: "失败",
};
const views = {
  backup: "创建备份",
  repository: "备份仓库",
  jobs: "任务记录",
  connections: "连接管理",
};
const storageKey = "bak.connections.v1";

function showView(focus = false) {
  const requested = location.hash.slice(1);
  const view = Object.hasOwn(views, requested) ? requested : "backup";
  if (requested !== view) history.replaceState(null, "", `#${view}`);
  for (const name of Object.keys(views)) $(name).hidden = name !== view;
  document.querySelectorAll("nav a").forEach((link) => {
    if (link.hash === `#${view}`) link.setAttribute("aria-current", "page");
    else link.removeAttribute("aria-current");
  });
  document.title = `bak · ${views[view]}`;
  if (focus) {
    $(`${view}-title`).focus({ preventScroll: true });
    scrollTo(0, 0);
  }
}

function navigate(view) {
  if (location.hash === `#${view}`) showView(true);
  else location.hash = view;
}

window.addEventListener("hashchange", () => showView(true));
document.querySelector(".skip-link").addEventListener("click", (event) => {
  event.preventDefault();
  $("main").focus();
});
$("api-url").value = location.origin;
showView();

function openConnection() {
  navigate("connections");
}

$("open-connection").addEventListener("click", openConnection);

function message(id, text = "", error = false) {
  const element = $(id);
  element.textContent = text;
  element.className = `message ${error ? "error" : "success"}`;
}

function node(tag, className = "", text = "") {
  const element = document.createElement(tag);
  element.className = className;
  element.textContent = text;
  return element;
}

function empty(title, description = "") {
  const element = node("div", "empty-state");
  element.append(node("p", "", title));
  if (description) element.append(node("p", "", description));
  return element;
}

function date(value) {
  return new Intl.DateTimeFormat("zh-CN", {
    dateStyle: "medium",
    timeStyle: "medium",
  }).format(new Date(value));
}

async function requestApi(connection, path, options, signal) {
  let response;
  let body;
  try {
    response = await fetch(new URL(path, connection.url), {
      ...options,
      headers: {
        Authorization: `Bearer ${connection.key}`,
        ...(options.body ? { "Content-Type": "application/json" } : {}),
      },
      signal: AbortSignal.any([signal, AbortSignal.timeout(15000)]),
      redirect: "error",
      cache: "no-store",
      credentials: "omit",
    });
    body = await response.json();
  } catch (error) {
    if (signal.aborted) throw new DOMException("连接已取消", "AbortError");
    throw new Error(
      options.method === "POST"
        ? "未收到完整的服务响应，任务可能已提交。请先刷新任务记录，确认后再重试。"
        : "无法连接 API，请检查地址、服务状态和跨域配置。HTTPS 页面应连接 HTTPS API。",
    );
  }
  if (signal.aborted) throw new DOMException("连接已取消", "AbortError");
  if (!response.ok) {
    const error = new Error(
      response.status === 401
        ? "访问密钥无效，请检查后重新连接。"
        : body?.error?.message || `请求失败（${response.status}）`,
    );
    error.status = response.status;
    throw error;
  }
  return body;
}

async function api(path, options = {}) {
  const session = state.session;
  try {
    return await requestApi(state.connection, path, options, session.signal);
  } catch (error) {
    if (session.signal.aborted)
      throw new DOMException("连接已取消", "AbortError");
    if (error.status === 401) {
      const connection = state.connection;
      resetSession();
      if (connection) editConnection(connection);
      openConnection();
      message("connection-message", error.message, true);
    }
    throw error;
  }
}

function report(id, error) {
  if (error.name !== "AbortError") message(id, error.message, true);
}

function resetSession() {
  state.session.abort();
  state.session = new AbortController();
  clearTimeout(state.timer);
  state.connection = null;
  state.connected = false;
  state.jobsRequest++;
  state.repositoryRequest++;
  state.jobs = [];
  state.backups = [];
  state.repository = "";
  state.selection = null;
  state.cursors = [null];
  state.page = 0;
  state.displayedPage = 0;
  state.next = null;
  $("disconnect-button").hidden = true;
  $("connection-state").textContent = "未连接";
  $("connection-dot").classList.add("muted");
  $("connection-dot").classList.remove("error");
  $("version").textContent = "";
  $("connection-name").textContent = "";
  $("connection-host").textContent = "";
  ["source", "backup-repository", "repository-path", "destination"].forEach(
    (id) => {
      $(id).value = "";
    },
  );
  clearFilter();
  state.encryptionKeys = [];
  $("compression").checked = true;
  $("encryption").checked = false;
  $("encryption-key").replaceChildren(new Option("选择服务端密钥", ""));
  updateEncodingOptions();
  $("backup-filter").open = false;
  document.querySelectorAll("[data-connected]").forEach((element) => {
    element.disabled = true;
  });
  $("refresh-jobs").disabled = true;
  $("previous-jobs").disabled = true;
  $("next-jobs").disabled = true;
  ["backup-button", "repository-button", "restore-button"].forEach((id) => {
    $(id).disabled = false;
  });
  $("jobs-page").textContent = "第 1 页";
  $("backup-job-link").hidden = true;
  $("restore-dialog").close();
  ["connection", "backup", "repository", "jobs", "restore"].forEach((name) =>
    message(`${name}-message`),
  );
  $("repository-label").textContent = "尚未载入仓库";
  $("jobs-updated").textContent = "连接后自动更新任务状态。";
  $("job-list").replaceChildren(empty("尚未连接服务。"));
  delete $("job-list").dataset.snapshot;
  $("backup-list").replaceChildren(empty("尚未载入仓库。"));
  renderConnections();
}

function normalizeUrl(value) {
  const input = value.trim();
  const url = new URL(input.includes("://") ? input : `http://${input}`);
  if (
    !["http:", "https:"].includes(url.protocol) ||
    url.username ||
    url.password ||
    url.pathname !== "/" ||
    url.search ||
    url.hash
  ) {
    throw new Error(
      "请输入 HTTP / HTTPS 地址和端口，不包含用户名、路径或查询参数。",
    );
  }
  return url.origin;
}

function saveConnections() {
  try {
    localStorage.setItem(
      storageKey,
      JSON.stringify({
        connections: state.connections,
        selectedUrl: state.selectedUrl,
      }),
    );
    message("storage-message");
  } catch {
    message(
      "storage-message",
      "浏览器无法保存连接，修改仅在当前页面有效。请检查浏览器存储设置。",
      true,
    );
  }
}

function cancelConnectionAttempt() {
  state.connectionAttempt?.abort();
  state.connectionAttempt = null;
  $("connect-button").disabled = false;
}

function resetEditor() {
  state.editingUrl = null;
  $("profile-name").value = "";
  $("api-url").value = location.origin;
  $("secret").value = "";
  $("connection-form-title").textContent = "添加连接";
  $("cancel-edit-connection").hidden = true;
}

function editConnection(connection) {
  cancelConnectionAttempt();
  state.editingUrl = connection.url;
  $("profile-name").value = connection.name;
  $("api-url").value = connection.url;
  $("secret").value = connection.key;
  $("connection-form-title").textContent = "编辑连接";
  $("cancel-edit-connection").hidden = false;
  message("connection-message");
}

function renderConnections() {
  const list = $("connection-list");
  list.replaceChildren();
  if (!state.connections.length) {
    list.append(empty("没有已保存的连接。请在下方添加。"));
    return;
  }
  const table = node("table", "data-table connections-table");
  table.append(node("caption", "sr-only", "已保存的 API 连接"));
  const head = node("thead");
  const header = node("tr");
  for (const label of ["名称", "API 地址", "状态", "操作"]) {
    const cell = node("th", "", label);
    cell.scope = "col";
    header.append(cell);
  }
  head.append(header);
  const body = node("tbody");
  for (const connection of state.connections) {
    const row = node("tr", "connection-row");
    const active = state.connection?.url === connection.url;
    row.append(
      node("td", "", connection.name),
      node("td", "mono", connection.url),
      node("td", "", active ? "当前连接" : "未连接"),
    );
    const actions = node("td", "connection-actions");
    const connect = node("button", "secondary", "连接");
    connect.type = "button";
    connect.disabled = active;
    connect.addEventListener("click", () => {
      void connectProfile(connection);
    });
    const edit = node("button", "secondary", "编辑");
    edit.type = "button";
    edit.addEventListener("click", () => {
      editConnection(connection);
      $("profile-name").focus();
    });
    const remove = node("button", "secondary", "删除");
    remove.type = "button";
    remove.addEventListener("click", () => {
      cancelConnectionAttempt();
      if (active) resetSession();
      state.connections = state.connections.filter(
        (item) => item.url !== connection.url,
      );
      if (state.selectedUrl === connection.url) state.selectedUrl = null;
      if (state.editingUrl === connection.url) resetEditor();
      saveConnections();
      message("connection-message", "连接已删除。");
      renderConnections();
    });
    actions.append(connect, edit, remove);
    row.append(actions);
    body.append(row);
  }
  table.append(head, body);
  list.append(table);
}

async function connectProfile(connection, save = false) {
  cancelConnectionAttempt();
  const attempt = new AbortController();
  state.connectionAttempt = attempt;
  const editingUrl = state.editingUrl;
  $("connect-button").disabled = true;
  message("connection-message", `正在验证 ${connection.url}…`);
  try {
    const status = await requestApi(
      connection,
      "/api/status",
      {},
      attempt.signal,
    );
    if (!status || typeof status.version !== "string")
      throw new Error("该地址未返回有效的 bak API 状态。");
    if (attempt !== state.connectionAttempt) return;
    if (save) {
      state.connections = state.connections.filter(
        (item) => item.url !== editingUrl && item.url !== connection.url,
      );
      state.connections.push(connection);
      resetEditor();
    }
    resetSession();
    state.connection = connection;
    state.connected = true;
    state.selectedUrl = connection.url;
    saveConnections();
    $("disconnect-button").hidden = false;
    $("connection-state").textContent = "已连接";
    $("connection-dot").classList.remove("muted");
    $("connection-name").textContent = connection.name;
    $("connection-host").textContent = connection.url;
    $("version").textContent = `v${status.version}`;
    document.querySelectorAll("[data-connected]").forEach((element) => {
      element.disabled = false;
    });
    state.encryptionKeys = status.encryption_keys || [];
    for (const key of state.encryptionKeys)
      $("encryption-key").append(new Option(key.key_id, key.key_id));
    updateEncodingOptions();
    $("refresh-jobs").disabled = false;
    renderConnections();
    message("connection-message", `已连接 ${connection.name}。`);
    await refreshJobs();
  } catch (error) {
    if (attempt === state.connectionAttempt)
      report("connection-message", error);
  } finally {
    if (attempt === state.connectionAttempt) {
      state.connectionAttempt = null;
      $("connect-button").disabled = false;
    }
  }
}

$("connect-form").addEventListener("submit", (event) => {
  event.preventDefault();
  try {
    const url = normalizeUrl($("api-url").value);
    const connection = {
      name: $("profile-name").value.trim() || new URL(url).host,
      url,
      key: $("secret").value,
    };
    if (!connection.key) throw new Error("请输入访问密钥。");
    if (
      state.connections.some(
        (item) => item.url === url && item.url !== state.editingUrl,
      )
    ) {
      throw new Error("该地址已保存，请编辑已有连接。");
    }
    void connectProfile(connection, true);
  } catch (error) {
    report("connection-message", error);
  }
});
$("cancel-edit-connection").addEventListener("click", () => {
  cancelConnectionAttempt();
  resetEditor();
  message("connection-message");
});
$("disconnect-button").addEventListener("click", () => {
  cancelConnectionAttempt();
  resetSession();
  state.selectedUrl = null;
  saveConnections();
  renderConnections();
});

function restoreConnections() {
  try {
    const saved = JSON.parse(localStorage.getItem(storageKey) || "null");
    if (saved !== null) {
      if (!Array.isArray(saved.connections))
        throw new Error("invalid saved connections");
      const urls = new Set();
      for (const item of saved.connections) {
        if (
          typeof item.name !== "string" ||
          typeof item.url !== "string" ||
          typeof item.key !== "string" ||
          !item.key ||
          normalizeUrl(item.url) !== item.url ||
          urls.has(item.url)
        ) {
          throw new Error("invalid saved connection");
        }
        urls.add(item.url);
      }
      state.connections = saved.connections;
      state.selectedUrl = saved.selectedUrl;
    }
  } catch {
    message(
      "storage-message",
      "无法读取浏览器中的连接配置，请重新添加连接。",
      true,
    );
  }
  renderConnections();
  const selected = state.connections.find(
    (item) => item.url === state.selectedUrl,
  );
  if (selected) void connectProfile(selected);
}

async function loadBackups(repository) {
  const request = ++state.repositoryRequest;
  message("repository-message", "正在读取备份…");
  $("repository-button").disabled = true;
  // Previous results must not appear to belong to a newly requested repository.
  state.repository = "";
  state.backups = [];
  $("repository-label").textContent = "正在载入";
  $("backup-list").replaceChildren(empty("正在载入备份", repository));
  try {
    const result = await api(
      `/api/backups?${new URLSearchParams({ repository })}`,
    );
    if (request !== state.repositoryRequest) return;
    state.repository = repository;
    state.backups = result.backups;
    $("repository-path").value = repository;
    if (!$("backup-repository").value.trim())
      $("backup-repository").value = repository;
    $("repository-label").textContent = `${result.backups.length} 份备份`;
    message("repository-message");
    renderBackups();
  } catch (error) {
    if (request !== state.repositoryRequest) return;
    $("repository-label").textContent = "载入失败";
    $("backup-list").replaceChildren(
      empty("无法载入仓库", "检查目录是否存在及服务是否可访问，然后重试。"),
    );
    report("repository-message", error);
  } finally {
    if (request === state.repositoryRequest)
      $("repository-button").disabled = false;
  }
}

function renderBackups() {
  const list = $("backup-list");
  list.replaceChildren();
  if (!state.backups.length) {
    list.append(empty("仓库中没有已完成的备份。"));
    return;
  }
  const table = node("table", "data-table");
  table.append(node("caption", "sr-only", "当前仓库的已完成备份"));
  const head = node("thead");
  const header = node("tr");
  for (const label of ["创建时间", "备份编号", "存储模式", "操作"]) {
    const cell = node("th", "", label);
    cell.scope = "col";
    header.append(cell);
  }
  head.append(header);
  const body = node("tbody");
  for (const backup of state.backups) {
    const row = node("tr", "backup-row");
    const created = node("td");
    const time = node("time", "", date(backup.created_at));
    time.dateTime = backup.created_at;
    created.append(time);
    const info = node("td", "mono", backup.backup_id);
    const action = node("td");
    const button = node("button", "secondary", "恢复");
    button.type = "button";
    button.addEventListener("click", () => {
      state.selection = {
        backup_id: backup.backup_id,
        repository: state.repository,
      };
      $("restore-id").textContent = backup.backup_id;
      $("restore-repository").textContent = state.repository;
      $("restore-mode").textContent = storageMode(backup);
      $("restore-key-label").hidden = !backup.encryption;
      $("restore-key").hidden = !backup.encryption;
      $("restore-key").textContent = backup.encryption?.key_id || "";
      $("destination").value = "";
      const available =
        !backup.encryption ||
        state.encryptionKeys.some(
          (key) =>
            key.key_id === backup.encryption.key_id &&
            key.fingerprint === backup.encryption.fingerprint,
        );
      $("restore-button").disabled = !available;
      message(
        "restore-message",
        available ? "" : "服务端缺少匹配的加密密钥，请配置后重新连接。",
        !available,
      );
      $("restore-dialog").showModal();
      $("destination").focus();
    });
    action.append(button);
    row.append(created, info, node("td", "", storageMode(backup)), action);
    body.append(row);
  }
  table.append(head, body);
  list.append(table);
}

$("repository-form").addEventListener("submit", (event) => {
  event.preventDefault();
  const repository = $("repository-path").value.trim();
  if (repository) void loadBackups(repository);
});

async function submitted(job, id) {
  const session = state.session;
  state.page = 0;
  state.cursors = [null];
  await refreshJobs();
  if (session.signal.aborted) return;
  message(id, `${job.kind === "backup" ? "备份" : "恢复"}任务已提交。`);
  if (id === "backup-message") $("backup-job-link").hidden = false;
}

function storageMode(backup) {
  if (backup.encryption) return backup.compression ? "压缩后加密" : "仅加密";
  return backup.compression ? "仅压缩" : "不压缩、不加密";
}

function updateEncodingOptions() {
  $("encryption").disabled = !state.encryptionKeys.length;
  const encrypted = $("encryption").checked;
  $("encryption-key-field").hidden = !encrypted;
  $("encryption-key").required = encrypted;
  $("encryption-key").disabled = !encrypted;
  $("encryption-help").hidden = state.encryptionKeys.length > 0;
  $("encryption-scope").hidden = !encrypted;
  $("encoding-order").hidden = !encrypted || !$("compression").checked;
}
$("compression").addEventListener("change", updateEncodingOptions);
$("encryption").addEventListener("change", updateEncodingOptions);

const regexFilterFields = ["relative_path", "uid", "gid", "mode"];
const timeFilterFields = ["mtime", "atime"];
const filterInputs = [
  ...$("backup-filter").querySelectorAll("input, textarea"),
];

function patterns(value) {
  return value.split(/\r?\n/).filter((line) => line.trim() !== "");
}

function updateFilterSummary() {
  const count = [...regexFilterFields, ...timeFilterFields].filter((field) =>
    filterInputs.some(
      (input) =>
        input.id.startsWith(`filter-${field}-`) && input.value.trim() !== "",
    ),
  ).length;
  $("filter-summary").textContent = count
    ? `过滤条件 · ${count} 项`
    : "过滤条件";
}

function clearFilter() {
  filterInputs.forEach((input) => {
    input.value = "";
  });
  updateFilterSummary();
}

function backupFilter() {
  const filter = {};
  for (const field of regexFilterFields) {
    filter[field] = {};
    for (const list of ["include", "exclude"])
      filter[field][list] = patterns($(`filter-${field}-${list}`).value);
  }
  for (const field of timeFilterFields) {
    const range = {};
    for (const bound of ["from", "before"]) {
      const input = $(`filter-${field}-${bound}`);
      if (!input.value) continue;
      const time = new Date(input.value);
      // Reject local times normalized across a daylight-saving gap.
      const parts = input.value.split(/[-T:]/).map(Number);
      const actual = [
        time.getFullYear(),
        time.getMonth() + 1,
        time.getDate(),
        time.getHours(),
        time.getMinutes(),
        time.getSeconds(),
      ];
      if (
        !Number.isFinite(time.getTime()) ||
        parts.some((part, i) => part !== actual[i])
      ) {
        $("backup-filter").open = true;
        input.focus();
        throw new Error("请输入有效的本地日期时间。");
      }
      range[bound] = time.toISOString();
    }
    if (
      range.from &&
      range.before &&
      new Date(range.from) >= new Date(range.before)
    ) {
      $("backup-filter").open = true;
      $(`filter-${field}-before`).focus();
      throw new Error(
        `${field === "mtime" ? "修改" : "访问"}时间的起始须早于截止。`,
      );
    }
    filter[field] = range;
  }
  return filter;
}

$("filter-time-help").textContent =
  `时间按浏览器时区 ${Intl.DateTimeFormat().resolvedOptions().timeZone} 输入，精确到秒；留空的一端不限。`;
$("backup-filter").addEventListener("input", updateFilterSummary);
$("backup-filter").addEventListener(
  "invalid",
  () => {
    $("backup-filter").open = true;
  },
  true,
);
$("clear-filter").addEventListener("click", clearFilter);

$("backup-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const source = $("source").value.trim();
  const repository = $("backup-repository").value.trim();
  if (!source || !repository)
    return message("backup-message", "请输入源目录和仓库路径。", true);
  $("backup-button").disabled = true;
  $("backup-job-link").hidden = true;
  message("backup-message", "正在提交备份任务…");
  try {
    const filter = backupFilter();
    const job = await api("/api/backups", {
      method: "POST",
      body: JSON.stringify({
        source,
        repository,
        filter,
        compression: $("compression").checked,
        encryption_key_id: $("encryption").checked
          ? $("encryption-key").value
          : null,
      }),
    });
    // Keep this repository visible; successful completion will refresh its contents.
    $("repository-path").value = repository;
    void loadBackups(repository);
    await submitted(job, "backup-message");
  } catch (error) {
    report("backup-message", error);
  } finally {
    $("backup-button").disabled = false;
  }
});

function closeRestore() {
  $("restore-dialog").close();
}
$("close-restore").addEventListener("click", closeRestore);
$("cancel-restore").addEventListener("click", closeRestore);
$("restore-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const destination = $("destination").value.trim();
  if (!state.selection || !destination)
    return message("restore-message", "请输入恢复目标目录。", true);
  $("restore-button").disabled = true;
  message("restore-message", "正在提交恢复任务…");
  try {
    const job = await api("/api/restores", {
      method: "POST",
      body: JSON.stringify({ ...state.selection, destination }),
    });
    closeRestore();
    navigate("jobs");
    await submitted(job, "jobs-message");
  } catch (error) {
    report("restore-message", error);
  } finally {
    $("restore-button").disabled = false;
  }
});

async function refreshJobs() {
  if (!state.connected) return;
  clearTimeout(state.timer);
  const request = ++state.jobsRequest;
  $("refresh-jobs").disabled = true;
  $("previous-jobs").disabled = true;
  $("next-jobs").disabled = true;
  try {
    const params = new URLSearchParams({ limit: "20" });
    const cursor = state.cursors[state.page];
    if (cursor) params.set("before", cursor);
    const result = await api(`/api/jobs?${params}`);
    if (request !== state.jobsRequest) return;
    state.jobs = result.jobs;
    state.next = result.next_cursor;
    state.displayedPage = state.page;
    renderJobs();
    message("jobs-message");
    $("connection-state").textContent = "已连接";
    $("connection-dot").classList.remove("error");
    $("jobs-updated").textContent =
      `每 3 秒自动更新 · 最近更新 ${date(Date.now())}`;
    if (
      state.repository &&
      result.jobs.some(
        (job) =>
          job.kind === "backup" &&
          job.state === "succeeded" &&
          !state.backups.some(
            (backup) => backup.backup_id === job.result.backup_id,
          ) &&
          job.request.repository === state.repository,
      )
    ) {
      void loadBackups(state.repository);
    }
  } catch (error) {
    if (request !== state.jobsRequest) return;
    state.page = state.displayedPage;
    report("jobs-message", error);
    $("connection-state").textContent = "连接异常";
    $("connection-dot").classList.add("error");
    $("jobs-updated").textContent = "暂未更新，当前显示上次读取的结果。";
  } finally {
    if (request === state.jobsRequest && state.connected) {
      $("refresh-jobs").disabled = false;
      $("previous-jobs").disabled = state.page === 0;
      $("next-jobs").disabled = !state.next;
      state.timer = setTimeout(refreshJobs, 3000);
    }
  }
}

function renderJobs() {
  $("jobs-page").textContent = `第 ${state.page + 1} 页`;
  const list = $("job-list");
  const snapshot = JSON.stringify(state.jobs);
  if (list.dataset.snapshot === snapshot) return;
  list.dataset.snapshot = snapshot;
  // Preserve expanded details and keyboard focus during background refreshes.
  const expanded = new Set(
    [...list.querySelectorAll("details[open]")].map(
      (element) => element.dataset.job,
    ),
  );
  const focused = document.activeElement?.closest("details")?.dataset.job;
  const focusedAction = document.activeElement?.matches("button[data-job]")
    ? document.activeElement.dataset.job
    : null;
  list.replaceChildren();
  if (!state.jobs.length) {
    list.append(empty("没有任务记录。"));
    return;
  }
  for (const job of state.jobs) {
    const row = node("article", "job");
    const heading = node("div", "job-heading");
    const time = node("time", "", date(job.queued_at));
    time.dateTime = new Date(job.queued_at).toISOString();
    heading.append(
      node(
        "span",
        "job-title",
        job.kind === "backup" ? "创建备份" : "恢复文件",
      ),
      node("span", `badge ${job.state}`, statuses[job.state] || job.state),
      time,
    );
    const path =
      job.kind === "backup"
        ? `${job.request.source} → ${job.request.repository}`
        : `${job.request.repository} → ${job.request.destination}`;
    row.append(heading, node("p", "job-path", path));
    if (job.result)
      row.append(
        node(
          "p",
          "job-result",
          job.kind === "backup"
            ? `备份编号：${job.result.backup_id}`
            : `已恢复并校验：${job.result.destination}`,
        ),
      );
    if (job.error)
      row.append(
        node("p", "job-error", `${job.error.code} · ${job.error.message}`),
      );
    const details = node("details");
    details.dataset.job = job.job_id;
    details.open = expanded.has(job.job_id);
    details.append(node("summary", "", "任务详情"));
    const fields = node("dl");
    const values = [
      ["任务编号", job.job_id],
      ["仓库", job.request.repository],
      ["开始时间", job.started_at === null ? "尚未开始" : date(job.started_at)],
      [
        "结束时间",
        job.finished_at === null ? "尚未结束" : date(job.finished_at),
      ],
    ];
    if (job.kind === "restore")
      values.unshift(["备份编号", job.request.backup_id]);
    for (const [label, value] of values)
      fields.append(
        node("dt", "", label),
        node("dd", label === "任务编号" ? "mono job-id" : "", value),
      );
    details.append(fields);
    const actions = node("div", "job-actions");
    actions.append(details);
    if (job.kind === "backup" && job.state === "succeeded") {
      const browse = node("button", "secondary", "查看备份");
      browse.type = "button";
      browse.dataset.job = job.job_id;
      browse.addEventListener("click", () => {
        navigate("repository");
        void loadBackups(job.request.repository);
      });
      actions.append(browse);
    }
    row.append(actions);
    list.append(row);
    if (focusedAction === job.job_id)
      row.querySelector("button[data-job]")?.focus({ preventScroll: true });
    if (focused === job.job_id)
      details.querySelector("summary").focus({ preventScroll: true });
  }
}

$("refresh-jobs").addEventListener("click", refreshJobs);
$("previous-jobs").addEventListener("click", () => {
  if (state.page > 0) {
    state.page--;
    void refreshJobs();
  }
});
$("next-jobs").addEventListener("click", () => {
  if (state.next) {
    state.cursors = state.cursors.slice(0, state.page + 1);
    state.cursors.push(state.next);
    state.page++;
    void refreshJobs();
  }
});

restoreConnections();
