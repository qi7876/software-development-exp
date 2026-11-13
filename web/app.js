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
  queued: "Queued",
  running: "Running",
  succeeded: "Succeeded",
  failed: "Failed",
};
const views = {
  backup: "Create backup",
  repository: "Backup repository",
  jobs: "Task records",
  connections: "Connections",
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
  return new Intl.DateTimeFormat("en-US", {
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
    if (signal.aborted) throw new DOMException("Connection cancelled", "AbortError");
    throw new Error(
      options.method === "POST"
        ? "No complete response from the service. The task may have been submitted. Refresh task records and check before retrying."
        : "Cannot reach the API. Check the address, service status and CORS settings. An HTTPS page must connect to an HTTPS API.",
    );
  }
  if (signal.aborted) throw new DOMException("Connection cancelled", "AbortError");
  if (!response.ok) {
    const error = new Error(
      response.status === 401
        ? "Invalid access key. Check it and reconnect."
        : body?.error?.message || `Request failed (${response.status})`,
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
      throw new DOMException("Connection cancelled", "AbortError");
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
  $("connection-state").textContent = "Disconnected";
  $("connection-status").dataset.state = "disconnected";
  $("version").textContent = "";
  ["source", "backup-repository", "repository-path", "destination"].forEach(
    (id) => {
      $(id).value = "";
    },
  );
  clearFilter();
  state.encryptionKeys = [];
  $("compression-algorithm").value = "zstd";
  $("lz77-window").value = "64";
  $("lz77-search-depth").value = "32";
  $("compression-level").value = "3";
  $("compression-level-slider").value = "3";
  $("encryption-algorithm").value = "none";
  $("encryption-key").replaceChildren(new Option("Select a server key", ""));
  updateEncodingOptions();
  document.querySelectorAll("[data-connected]").forEach((element) => {
    element.disabled = true;
  });
  $("refresh-jobs").disabled = true;
  $("previous-jobs").disabled = true;
  $("next-jobs").disabled = true;
  ["repository-button", "restore-button"].forEach((id) => {
    $(id).disabled = false;
  });
  $("jobs-page").textContent = "Page 1";
  $("backup-job-link").hidden = true;
  $("restore-dialog").close();
  ["connection", "backup", "repository", "jobs", "restore"].forEach((name) =>
    message(`${name}-message`),
  );
  $("repository-label").textContent = "No repository loaded";
  $("jobs-updated").textContent = "Task records load after connecting. Click Refresh to update.";
  $("job-list").replaceChildren(empty("Not connected."));
  delete $("job-list").dataset.snapshot;
  $("backup-list").replaceChildren(empty("No repository loaded."));
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
      "Enter an HTTP or HTTPS address and port, without username, path or query.",
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
      "The browser cannot save connections. Changes apply to this page only. Check browser storage settings.",
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
  $("api-url").value = location.origin;
  $("secret").value = "";
  $("connection-form-title").textContent = "Add connection";
  $("cancel-edit-connection").hidden = true;
}

function editConnection(connection) {
  cancelConnectionAttempt();
  state.editingUrl = connection.url;
  $("api-url").value = connection.url;
  $("secret").value = connection.key;
  $("connection-form-title").textContent = "Edit connection";
  $("cancel-edit-connection").hidden = false;
  message("connection-message");
}

function renderConnections() {
  const list = $("connection-list");
  list.replaceChildren();
  if (!state.connections.length) {
    list.append(empty("No saved connections. Add one below."));
    return;
  }
  const table = node("table", "data-table connections-table");
  table.append(node("caption", "sr-only", "Saved API connections"));
  const head = node("thead");
  const header = node("tr");
  for (const label of ["API address", "Status", "Actions"]) {
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
      node("td", "mono", connection.url),
      node("td", "", active ? "Current" : "Disconnected"),
    );
    const actions = node("td", "connection-actions");
    const connect = node("button", "", "Connect");
    connect.type = "button";
    connect.disabled = active;
    connect.addEventListener("click", () => {
      void connectProfile(connection);
    });
    const edit = node("button", "", "Edit");
    edit.type = "button";
    edit.addEventListener("click", () => {
      editConnection(connection);
      $("api-url").focus();
    });
    const remove = node("button", "danger", "Delete");
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
      message("connection-message", "Connection deleted.");
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
  message("connection-message", `Verifying ${connection.url}…`);
  try {
    const status = await requestApi(
      connection,
      "/api/status",
      {},
      attempt.signal,
    );
    if (!status || typeof status.version !== "string")
      throw new Error("This address did not return a valid bak API status.");
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
    $("connection-state").textContent = "Connected";
    $("connection-status").dataset.state = "connected";
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
    message("connection-message", "Connected.");
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
      url,
      key: $("secret").value,
    };
    if (!connection.key) throw new Error("Enter the access key.");
    if (
      state.connections.some(
        (item) => item.url === url && item.url !== state.editingUrl,
      )
    ) {
      throw new Error("This address is already saved. Edit the existing connection.");
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

function restoreConnections() {
  try {
    const saved = JSON.parse(localStorage.getItem(storageKey) || "null");
    if (saved !== null) {
      if (!Array.isArray(saved.connections))
        throw new Error("invalid saved connections");
      const urls = new Set();
      for (const item of saved.connections) {
        if (
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
      state.connections = saved.connections.map(({ url, key }) => ({ url, key }));
      state.selectedUrl = saved.selectedUrl;
    }
  } catch {
    message(
      "storage-message",
      "Cannot read saved connections from the browser. Add the connection again.",
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
  message("repository-message", "Reading backups…");
  $("repository-button").disabled = true;
  // Previous results must not appear to belong to a newly requested repository.
  state.repository = "";
  state.backups = [];
  $("repository-label").textContent = "Loading";
  $("backup-list").replaceChildren(empty("Loading backups", repository));
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
    $("repository-label").textContent = `${result.backups.length} ${result.backups.length === 1 ? "backup" : "backups"}`;
    message("repository-message");
    renderBackups();
  } catch (error) {
    if (request !== state.repositoryRequest) return;
    $("repository-label").textContent = "Load failed";
    $("backup-list").replaceChildren(
      empty("Cannot load repository", "Check that the directory exists and the service can access it, then retry."),
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
    list.append(empty("No completed backups in the repository."));
    return;
  }
  const table = node("table", "data-table");
  table.append(node("caption", "sr-only", "Completed backups in the current repository"));
  const head = node("thead");
  const header = node("tr");
  for (const label of ["Created", "Backup ID", "Storage mode", "Actions"]) {
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
    const button = node("button", "", "Restore");
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
        available ? "" : "The server lacks the matching encryption key. Configure it and reconnect.",
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
  message(id, `${job.kind === "backup" ? "Backup" : "Restore"} task submitted.`);
  if (id === "backup-message") $("backup-job-link").hidden = false;
}

function compressionDescription(compression) {
  if (!compression) return "No compression";
  if (compression.algorithm === "zstd")
    return `Zstd · level ${compression.level}`;
  return `LZ77 · ${compression.window_kib} KiB · ${compression.search_depth} candidates`;
}
function storageMode(backup) {
  return `${compressionDescription(backup.compression)} · ${backup.encryption ? (backup.encryption.algorithm === "aes-256-gcm" ? "AES-256-GCM" : "XChaCha20-Poly1305") : "No encryption"}`;
}
function compressionOptions() {
  if ($("compression-algorithm").value === "none") return null;
  if ($("compression-algorithm").value === "zstd")
    return { algorithm: "zstd", level: Number($("compression-level").value) };
  return {
    algorithm: "lz77",
    window_kib: Number($("lz77-window").value),
    search_depth: Number($("lz77-search-depth").value),
  };
}

$("compression-level-slider").addEventListener("input", () => {
  $("compression-level").value = $("compression-level-slider").value;
});
$("compression-level").addEventListener("input", () => {
  if ($("compression-level").validity.valid)
    $("compression-level-slider").value = $("compression-level").value;
});

function updateEncodingOptions() {
  const algorithm = $("compression-algorithm").value;
  const zstd = algorithm === "zstd";
  const lz77 = algorithm === "lz77";
  $("compression-level-fields").hidden = !zstd;
  $("compression-level-fields").disabled = !zstd;
  $("lz77-fields").hidden = !lz77;
  $("lz77-fields").disabled = !lz77;
  const hasKeys = state.encryptionKeys.length > 0;
  $("encryption-xchacha").disabled = !hasKeys;
  $("encryption-aes").disabled = !hasKeys;
  const encrypted = $("encryption-algorithm").value !== "none";
  $("encryption-key-field").hidden = !encrypted;
  $("encryption-key").required = encrypted;
  $("encryption-key").disabled = !encrypted;
  $("encryption-help").hidden = hasKeys;
}
$("compression-algorithm").addEventListener("change", updateEncodingOptions);
$("encryption-algorithm").addEventListener("change", updateEncodingOptions);

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
    ? `Filters · ${count}`
    : "Filters";
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
        input.focus();
        throw new Error("Enter a valid local date and time.");
      }
      range[bound] = time.toISOString();
    }
    if (
      range.from &&
      range.before &&
      new Date(range.from) >= new Date(range.before)
    ) {
      $(`filter-${field}-before`).focus();
      throw new Error(
        `The ${field === "mtime" ? "modified" : "accessed"} time start must be earlier than the end.`,
      );
    }
    filter[field] = range;
  }
  return filter;
}

$("backup-filter").addEventListener("input", updateFilterSummary);

$("backup-form").addEventListener("submit", async (event) => {
  event.preventDefault();
  const source = $("source").value.trim();
  const repository = $("backup-repository").value.trim();
  if (!source || !repository)
    return message("backup-message", "Enter the source directory and repository path.", true);
  $("backup-button").disabled = true;
  $("backup-job-link").hidden = true;
  message("backup-message", "Submitting backup task…");
  try {
    const filter = backupFilter();
    const job = await api("/api/backups", {
      method: "POST",
      body: JSON.stringify({
        source,
        repository,
        filter,
        compression: compressionOptions(),
        encryption: $("encryption-algorithm").value !== "none"
          ? { algorithm: $("encryption-algorithm").value, key_id: $("encryption-key").value }
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
    $("backup-button").disabled = !state.connected;
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
    return message("restore-message", "Enter the restore destination directory.", true);
  $("restore-button").disabled = true;
  message("restore-message", "Submitting restore task…");
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
    $("connection-state").textContent = "Connected";
    $("connection-status").dataset.state = "connected";
    $("jobs-updated").textContent = `Last updated ${date(Date.now())}`;
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
    $("connection-state").textContent = "Connection error";
    $("connection-status").dataset.state = "error";
    $("jobs-updated").textContent = "Update failed. Showing the last result. Click Refresh to retry.";
  } finally {
    if (request === state.jobsRequest && state.connected) {
      $("refresh-jobs").disabled = false;
      $("previous-jobs").disabled = state.page === 0;
      $("next-jobs").disabled = !state.next;
    }
  }
}

function renderJobs() {
  $("jobs-page").textContent = `Page ${state.page + 1}`;
  const list = $("job-list");
  const snapshot = JSON.stringify(state.jobs);
  if (list.dataset.snapshot === snapshot) return;
  list.dataset.snapshot = snapshot;
  // Preserve expanded details and keyboard focus when refreshing.
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
    list.append(empty("No task records."));
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
        job.kind === "backup" ? "Create backup" : "Restore files",
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
            ? `Backup ID: ${job.result.backup_id}`
            : `Restored and verified: ${job.result.destination}`,
        ),
      );
    if (job.error)
      row.append(
        node("p", "job-error", `${job.error.code} · ${job.error.message}`),
      );
    const details = node("details");
    details.dataset.job = job.job_id;
    details.open = expanded.has(job.job_id);
    details.append(node("summary", "", "Details"));
    const fields = node("dl");
    const values = [
      ["Task ID", job.job_id],
      ["Repository", job.request.repository],
      ["Started", job.started_at === null ? "Not started" : date(job.started_at)],
      [
        "Finished",
        job.finished_at === null ? "Not finished" : date(job.finished_at),
      ],
    ];
    if (job.schedule_id)
      values.unshift(["Schedule ID", job.schedule_id], ["Scheduled for", date(job.scheduled_for)]);
    if (job.kind === "restore")
      values.unshift(["Backup ID", job.request.backup_id]);
    if (job.kind === "backup" && job.request.compression)
      values.push([
        "Compression",
        compressionDescription(job.request.compression),
      ]);
    for (const [label, value] of values)
      fields.append(
        node("dt", "", label),
        node("dd", label === "Task ID" ? "mono job-id" : "", value),
      );
    details.append(fields);
    const actions = node("div", "job-actions");
    actions.append(details);
    if (job.kind === "backup" && job.state === "succeeded") {
      const browse = node("button", "", "View backup");
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
