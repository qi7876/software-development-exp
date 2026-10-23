"use strict";

const $ = (id) => document.getElementById(id);
const state = {
  key: "",
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
const views = { backup: "创建备份", repository: "备份仓库", jobs: "任务记录" };

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
$("connection-host").textContent = location.host;
showView();

function openConnection() {
  if (!$("connection-dialog").open) $("connection-dialog").showModal();
  $("secret").focus();
}

$("open-connection").addEventListener("click", openConnection);
$("close-connection").addEventListener("click", () =>
  $("connection-dialog").close(),
);
$("connection-dialog").addEventListener("close", () => {
  $("secret").value = "";
  if (!state.connected && state.key) disconnect();
});

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

async function api(path, options = {}) {
  const session = state.session;
  let response;
  let body;
  try {
    response = await fetch(path, {
      ...options,
      headers: {
        Authorization: `Bearer ${state.key}`,
        ...(options.body ? { "Content-Type": "application/json" } : {}),
      },
      signal: AbortSignal.any([session.signal, AbortSignal.timeout(15000)]),
      cache: "no-store",
      credentials: "omit",
    });
    body = await response.json();
  } catch (error) {
    if (session.signal.aborted)
      throw new DOMException("连接已取消", "AbortError");
    throw new Error(
      options.method === "POST"
        ? "未收到完整的服务响应，任务可能已提交。请先刷新任务记录，确认后再重试。"
        : "暂时无法连接服务，请检查服务是否运行。页面会继续尝试更新任务。",
    );
  }
  if (session.signal.aborted)
    throw new DOMException("连接已取消", "AbortError");
  if (!response.ok) {
    if (response.status === 401) {
      disconnect();
      openConnection();
      message("connection-message", "访问密钥无效或已更改，请重新连接。", true);
    }
    throw new Error(body.error?.message || `请求失败（${response.status}）`);
  }
  return body;
}

function report(id, error) {
  if (error.name !== "AbortError") message(id, error.message, true);
}

function disconnect() {
  state.session.abort();
  state.session = new AbortController();
  clearTimeout(state.timer);
  state.key = "";
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
  $("secret").value = "";
  $("open-connection").hidden = false;
  $("disconnect-button").hidden = true;
  $("connection-state").textContent = "未连接";
  $("connection-dot").classList.add("muted");
  $("connection-dot").classList.remove("error");
  $("version").textContent = "";
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
}

$("connect-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const key = $("secret").value;
  disconnect();
  state.key = key;
  const session = state.session;
  $("connect-button").disabled = true;
  message("connection-message", "正在连接…");
  try {
    const status = await api("/api/status");
    state.connected = true;
    $("secret").value = "";
    $("open-connection").hidden = true;
    $("disconnect-button").hidden = false;
    $("connection-state").textContent = "已连接";
    $("connection-dot").classList.remove("muted");
    $("version").textContent = `v${status.version}`;
    document.querySelectorAll("[data-connected]").forEach((element) => {
      element.disabled = false;
    });
    $("refresh-jobs").disabled = false;
    message("connection-message");
    $("connection-dialog").close();
    await refreshJobs();
  } catch (error) {
    if (session === state.session) {
      state.key = "";
      report("connection-message", error);
    }
  } finally {
    if (session === state.session || !state.key)
      $("connect-button").disabled = false;
  }
});
$("disconnect-button").addEventListener("click", () => {
  disconnect();
  $("open-connection").focus();
});

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
  for (const label of ["创建时间", "备份编号", "操作"]) {
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
      $("destination").value = "";
      message("restore-message");
      $("restore-dialog").showModal();
      $("destination").focus();
    });
    action.append(button);
    row.append(created, info, action);
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
    const job = await api("/api/backups", {
      method: "POST",
      body: JSON.stringify({ source, repository }),
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
