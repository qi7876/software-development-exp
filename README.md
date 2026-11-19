# bak

`bak` is a file backup manager for individual users, designed to provide reliable, verifiable local backups that can be restored. A single binary runs an axum HTTP service on Tokio and includes a browser console. Users access backups, restores, and durable task history through the console or curl using a Bearer secret key. The current MVP supports backup and restore execution on Unix.

## Quick start

You need the Rust toolchain specified in `rust-toolchain.toml`. Backup and restore run on Unix only (Linux and macOS).

```shell
# 1. Prepare a working directory with source, repository and restore folders
mkdir -p working-directory-example/source working-directory-example/repository working-directory-example/restored

# 2. Generate the encryption key that the example config.json refers to
cargo run -- keygen --output working-directory-example/backup.key

# 3. Start the service
cargo run -- server start -d working-directory-example

# 4. Check that it answers
curl -H 'Authorization: Bearer example-local-test-key' http://127.0.0.1:8080/api/status
```

Open <http://127.0.0.1:8080/> for the browser console. The command returns after the background service is ready and prints its PID, listening address, working directory, and log path. Run `cargo run -- server status` to inspect it and `cargo run -- server stop` to stop it. For a release build, run `cargo build --release` and use `target/release/bak` in place of `cargo run --`.

Step 2 is needed because `backup.key` is not stored in git. If you do not want encryption, remove the `encryption` section from `working-directory-example/config.json` instead.

## User guide

### Concepts

- **Working directory**: the folder passed with `-d`. It holds `config.json` and the task database `jobs.sqlite3`. Relative paths in requests and in the configuration resolve against it. Run only one service per working directory.
- **Source**: the directory to back up.
- **Repository**: a directory that stores backups. One repository can hold many backups, each identified by a backup ID. Source and repository must not contain each other.
- **Task**: backups and restores run as queued tasks, one at a time in submission order. Submitting returns at once; you check the outcome in Task records. Tasks and their results survive a restart.

All paths refer to the machine running bak, even if you open the console from another device. Directories must exist before you use them, because bak does not create them.

### Configure the service

`config.json` in the working directory:

| Field | Required | Meaning |
| --- | --- | --- |
| `listen` | yes | Address and port, for example `127.0.0.1:8080`. Use `0.0.0.0:8080` to accept connections from other machines. |
| `secret_key` | yes | Non-empty API key. Clients send it as `Authorization: Bearer <secret_key>`. |
| `web_ui.enabled` | no | Serve the browser console. Defaults to false. |
| `web_ui.path` | when enabled | Directory containing `index.html`, `app.js`, `styles.css`, and `favicon.svg`. Absolute or relative to the working directory (`-d`). |
| `encryption.keys` | no | Map of key IDs to key file paths. See [Encryption keys](#encryption-keys). |
| `scheduled_backups` | no | Cron-based backup plans. See [Scheduled backups](#scheduled-backups). |

The service refuses to start, with a message, if `config.json` is missing or invalid, `secret_key` is empty, a configured key file cannot be read, or an enabled WebUI has no path or missing/unreadable assets. Restart the service after any change to `config.json`. The obsolete `web_ui.archive_url` and `web_ui.directory` fields are accepted but ignored. The former `logging` section must be removed.

Treat `secret_key` as a password. Anyone who has it can read and write any path the service can reach. bak has no built-in TLS, so keep `listen` on loopback or put a reverse proxy with HTTPS in front when exposing it.

### Connect the console

1. Open the console and select **Manage connections**.
2. Enter the API address (`127.0.0.1:8080`) and the key, then select **Verify and save**.

The console remembers connections in the browser and reconnects to the last one on reload. It can also manage other bak servers. See [Browser console](#browser-console) for details on storage, switching and cross-origin use.

### Create a backup

1. Put files in the source directory, for example `working-directory-example/source`.
2. Open **Create backup**. Enter the source (`source`) and repository (`repository`), either absolute or relative to the working directory.
3. Choose compression and encryption (see below) and optionally set filters.
4. Select **Create backup**, then **View tasks**, and select **Refresh** until the task shows **Succeeded**. Task details show the backup ID and the settings used.

The same request with curl:

```shell
curl -i -H 'Authorization: Bearer example-local-test-key' \
  -H 'Content-Type: application/json' \
  -d '{"source":"source","repository":"repository","compression":{"algorithm":"zstd","level":3},"encryption":null,"filter":{"relative_path":{"include":[],"exclude":[]},"uid":{"include":[],"exclude":[]},"gid":{"include":[],"exclude":[]},"mode":{"include":[],"exclude":[]},"atime":{},"mtime":{}}}' \
  http://127.0.0.1:8080/api/backups
```

The response is `202 Accepted` with a `Location` header. Poll that URL until `state` is `succeeded` or `failed`.

Keep source files unchanged while a backup runs, because bak does not detect concurrent modification.

### Choose compression and encryption

| Setting | Options | Notes |
| --- | --- | --- |
| Compression | None, Zstd, LZ77 | Zstd level 1–22 (default 3). LZ77 is the project's own codec with a 4–1024 KiB window and a search depth of 1–256. |
| Encryption | None, XChaCha20-Poly1305, AES-256-GCM | Needs a key configured on the server. |

When both are used, the complete archive is compressed first and then encrypted. The plaintext metadata inside each `.bak` file still exposes file names, directory structure, Unix metadata, sizes and symbolic link targets, plus the archive checksum. The console defaults to Zstd level 3 with no encryption. API clients must choose explicitly, and omitted or null `compression` means uncompressed.

Restore reads the codec and key from the backup, so you never enter these settings again.

### Filter what is backed up

The **Filters** section limits a backup by relative path, UID, GID, permissions (four-digit octal such as `0644`), modified time and accessed time. Rules:

- Enter one regular expression per line. Blank lines are ignored.
- A file must satisfy every field (AND). Within a field, any include pattern may match (OR), and any exclude match rejects the entry.
- A rejected directory skips its whole subtree, so include patterns must also permit the parent directories.

Examples:

| Goal | Relative path include | Relative path exclude |
| --- | --- | --- |
| Only the `docs` folder | `^docs(/\|$)` | |
| Everything except logs and `tmp` | | `\.log$`<br>`^tmp(/\|$)` |

`\.txt$` alone matches only `.txt` files in the top level of the source, because every subdirectory is rejected before its contents are examined. To take `.txt` files from the subdirectory `a`, list both the directory and its files as separate include lines: `^a$` and `^a/.*\.txt$`.

Invalid patterns are not rejected when you submit. The task fails with a 400 error in Task records.

### Browse and restore

1. Open **Backup repository**, enter the repository path and select **Load backups**. After a backup, **View backup** in the task opens its repository directly.
2. Choose a backup and select **Restore**.
3. Enter a destination that is an existing, empty directory outside the repository (for example `restored`), then select **Start restore**.
4. Check the result in Task records and inspect the destination.

Restore verifies each file's size and the complete archive's SHA-256 checksum, and authenticates encrypted data. If a restore fails or is interrupted, partial files can remain in the destination and are not cleaned up. Use a fresh empty directory for the next attempt. Restoring an encrypted backup requires the same key (same ID and contents) in the server configuration. Without it, **Restore** is disabled and the dialog explains why.

### Check task history

**Task records** lists tasks newest first, 20 per page. Nothing refreshes by itself; select **Refresh** to update. Open **Details** on a task for its ID, repository, start and finish times, and compression, or its schedule ID and time for scheduled runs. A backup task left `running` is reconciled against its archive on the next start: verified output can become Succeeded; otherwise it becomes Failed with an explanation. Interrupted restores become Failed. Operations are not replayed.

### Schedule backups, encrypt, and call the API

- Automatic daily or hourly backups: [Scheduled backups](#scheduled-backups).
- Creating and rotating keys: [Encryption keys](#encryption-keys).
- Using curl or scripts instead of the console: [HTTP API](#http-api).

### Troubleshooting

| Symptom | Likely cause and fix |
| --- | --- |
| Service exits at start | Read the printed message. Usual causes are a missing or malformed `config.json`, an empty `secret_key`, an unreadable key file, or a `logging` section. |
| **Invalid access key** | The key differs from `secret_key`. Fix it in **Manage connections**. |
| **Cannot reach the API** | Wrong address, service not running, a server listening on loopback only, or an HTTPS console talking to an HTTP API (mixed content). |
| Encryption is unavailable | No keys configured or the service was not restarted after adding them. See [Encryption keys](#encryption-keys). |
| Backup task failed | Open Details or the error line: the source or repository does not exist, they overlap, or a filter is invalid. |
| Restore is disabled | The server has no key matching the backup's key ID and fingerprint. Add the original key and restart. |
| Restore failed immediately | The destination is missing, not empty, or overlaps the repository. |
| Response lost after submitting | The task may already be queued. Refresh Task records before submitting again, because a retry creates another task. |

## Development

Install the Rust toolchain specified in `rust-toolchain.toml`. Install uv to run the complete local checks.

```shell
uv run scripts/check.py
```

The root command accepts subcommands only, with no root options:

```shell
bak help
bak help server start
bak version
bak keygen --output /secure/backup.key
bak server start -d /path/to/workdir
bak server status
bak server stop
```

`server start` requires an existing working directory containing `config.json`. It starts one managed background server for the current user, independent of the invoking terminal; a second start is rejected until that instance stops. `status` and `stop` use the same user's local control socket, so they work from any directory without reading `config.json` or supplying an API key. Status reports Running, Stopping, or stopped, with PID, working directory, and listening address when available. Stopping an already stopped server succeeds. `stop` waits for the active job and database shutdown to finish; queued jobs remain for the next start.

Background stdout and stderr append to `server.log` in the working directory. Startup errors return a failure status and point to that log. Control files live under `$XDG_STATE_HOME/bak`, or `$HOME/.local/state/bak` when unset; use the same environment for start/status/stop. A file lock prevents two foreground or background services from opening the same working directory. Stale control sockets after an abrupt exit are replaced on the next start.

For terminal debugging or a process supervisor, use `bak server start --foreground -d /path/to/workdir`; stop it with Ctrl-C or SIGTERM. Foreground processes are managed by their caller and do not appear in `server status`. Background management currently requires Unix. Child commands accept `--help` / `-h`; root `--help`, `--version`, and `-d` have been replaced by `help`, `version`, and `server start -d`.

## Browser console

With the example service running, open <http://127.0.0.1:8080/> and click Manage connections below Task records in the sidebar. Add an API address such as `127.0.0.1:8080` and its key (`example-local-test-key` for the example). Verify and save checks the key against `/api/status`, saves the connection, and activates it. Saved connections can be switched, edited, or deleted on this page; the five sidebar entries open backup, repository, task records, and connection management views. Connection status and version remain at the bottom without an API address, with a filled black square when connected, a hollow square when disconnected, and a red square when a refresh fails; there is no separate Disconnect button.

Addresses accept HTTP/HTTPS origins with an optional port; omitting the scheme uses HTTP. Paths, query strings, URL credentials, and fragments are not accepted. The console may use its own API or another reachable server, including servers with `web_ui.enabled=false`. The target must run a version that supports cross-origin API requests. Remote servers must listen on a reachable interface (for example, `0.0.0.0:8080` rather than loopback only). For an HTTPS console, use HTTPS remote APIs to avoid browser mixed-content restrictions; bak has no built-in TLS, so HTTPS needs a reverse proxy.

Normalized API addresses, keys, and the last selected connection are stored in `localStorage` under `bak.connections.v1`. Existing saved entries with a name remain readable; names are ignored and omitted when connections are saved again. Keys are stored in plain text in the browser profile, not encrypted. The cache belongs to the console's browser origin (scheme, host, and port); opening the console at a different origin uses a separate cache. Reloading or reopening automatically verifies the last selected connection before enabling operations. Delete removes that connection and key from the cache; deleting the active connection also ends the current session and clears the automatic-reconnection selection. Storage failures are displayed rather than silently discarding changes. The console is public, while all operation APIs still require a Bearer key; browser requests do not use cookies.

Switching verifies the target before replacing the active connection. A failed check preserves the current service. A successful switch aborts old page requests and clears paths, loaded backups, and task records so data from different servers is not mixed. Already submitted server jobs continue independently.

1. Put files in `working-directory-example/source`. In the console, create a backup with source `source` and repository `repository`. Compression defaults to enabled; encryption defaults to disabled.
2. Follow View tasks to the task records and click Refresh to check for success. View backup opens the repository directly; an existing repository can also be loaded by path from the repository view.
3. Choose a backup, click Restore, and enter `restored`, an existing empty directory. Submission opens the task records. Check the result and restored contents.

The backup form directly displays the **Filters** section for relative path, UID, GID, octal permissions, modification time, and access time. Regex inputs accept one pattern per line; blank lines are ignored. Fields combine with AND, include patterns with OR, and exclusions take precedence. Conditions apply to each supported entry path, including hard-link aliases. A rejected directory skips its entire subtree, so include patterns must also permit ancestors. Local date/time inputs use the browser timezone, with second precision, inclusive starts, and exclusive ends. Inputs remain while navigating within one service and are cleared when deleting the active connection or successfully switching services. Regex validation happens during job execution; invalid patterns appear as failed jobs.

Backup storage displays separate **Compression** and **Encryption** sections without checkboxes. The compression algorithm selector offers None, Zstd, and the project's own streaming LZ77 codec; the encryption algorithm selector offers None, XChaCha20-Poly1305, and AES-256-GCM, giving nine storage modes. Selecting None skips the corresponding process and hides and disables its parameters. Encryption requires a configured server key; without one, its algorithm option is unavailable while None remains selectable. Combining compression with encryption always compresses first. The UI defaults to Zstd level 3 without encryption; restoration automatically selects the saved codec and key.

Zstd levels range from 1 to 22 (default 3), using a synchronized slider and number field. LZ77 instead exposes a history window of 4–1024 KiB (default 64) and a candidate search limit of 1–256 (default 32). These creation settings apply to this backup; restore reads its saved format automatically. Switching algorithms retains each algorithm's inputs and disables hidden controls; turning compression off retains the inputs. A successful service switch or deletion of the active connection resets the settings. Task details, backup summaries and the restore dialog show the actual codec and parameters. See the [LZ77 format and design](docs/architecture/lz77.md).

Encryption protects the complete archive stream. The plaintext metadata inside each `.bak` file still exposes filenames, directory structure, Unix metadata, original sizes, the archive SHA-256 checksum, and symbolic link targets. The API service reads plaintext and holds configured keys in memory; this is repository encryption, not end-to-end browser encryption. Failed restores can leave partial output.

All paths refer to the device running bak, including when the browser is on another device. Source, repository, and restore destination directories must exist. No OS directory picker or automatic directory creation is provided. Source and repository must not overlap; restore destination must be empty and outside the repository. Symbolic links are preserved; special files are skipped. Keep source files unchanged during a backup. Failed or interrupted restores can leave partial output.

The console supports desktop and tablet windows (768 CSS pixels and wider). Page content follows the available width up to 1040 CSS pixels, with flexible margins. Form fields wrap to one column when space is limited; wide tables scroll within their lists. System fonts, a black-white-gray palette, square-cornered bordered boxes, and no icons or shadows provide visual hierarchy; task states are distinguished by border style and fill, and only errors and failed tasks use red. The console text is in English. Form controls retain native interaction, focus indicators, and disabled states with a consistent monospace font and font size for inputs, selects, and textareas. Single-line inputs and selects share a common height; select arrows have inset spacing. The two-column layout places navigation in the left sidebar and page content on the right in bordered boxes. The Create backup button is at the upper right of the backup page. The five sidebar entries open separate views, preserve form inputs when switching views within one service, and support browser back/forward navigation. Task records load once after connecting or submitting a task; further status updates use the Refresh button, with no background polling. They show server-side errors and paginate through persisted history. Task IDs and execution timestamps expand under Details. A connection failure is also shown in the sidebar and can be retried with Refresh; an invalid key opens connection management for correction. The UI shows task states rather than byte progress. Requests that lose their response are not automatically resubmitted: inspect history before retrying. The service uses one shared key rather than separate user accounts.

Browser assets live in `web/` and are distributed separately from the `bak` binary. Copy that directory to the deployment machine and configure its location:

```json
"web_ui": {
  "enabled": true,
  "path": "/opt/bak/web"
}
```

The example configuration uses `"path": "../web"`, relative to `working-directory-example/`. Assets are read from disk on each request; updating them does not require rebuilding or restarting `bak`. Only the four console files above are exposed, at `/` and `/ui/*`. With `enabled=false`, no WebUI files are required. Node.js 20 or later is needed only for the optional browser checks:

```shell
npm --prefix web ci
# Install the matching browser once, or use BAK_BROWSER_CHANNEL=chrome with installed Chrome:
cd web
npx playwright install chromium
cd ..
uv run scripts/check.py --web-ui
```

The browser checks launch disposable real services and verify authentication, backup/restore content and empty directories, preservation of an occupied destination, durable history, pagination, text-safe rendering of paths/errors, network recovery, and desktop view navigation. Scheduled-plan checks cover enabled/disabled plans, safe rendering, encrypted LZ77 backup/restore, missing keys, network recovery, and service switches with delayed old responses. Two services with different keys exercise cross-origin backup submission, failed-switch preservation, cache restoration after closing a page, editing/deleting profiles, and clearing old-service data on successful switches. Test files use `.js` with the web package's ES module setting.

## Scheduled backups

Configure optional `scheduled_backups` in `config.json`:

```json
"scheduled_backups": {
  "timezone": "Asia/Shanghai",
  "plans": [{
    "id": "daily-source",
    "enabled": true,
    "cron": "0 2 * * *",
    "source": "source",
    "repository": "repository",
    "compression": {"algorithm": "zstd", "level": 3},
    "encryption": null
  }]
}
```

This runs daily at 02:00 in the specified IANA timezone. Multiple plans are supported. The example configuration ships with its plan disabled. Omit the section or use empty `plans` to disable scheduling. Restart to apply changes. The console can view plans and run them immediately; editing plans still requires changing the configuration.

Each plan needs a stable unique nonempty `id`, five-field numeric Cron expression (minute, hour, day, month, weekday; Sunday 0 or 7), source and repository. Lists, ranges and steps are supported; seconds, years, names and shortcuts are rejected. Restricted day and weekday fields use OR. `enabled` defaults to true. Relative paths resolve against the service working directory; source and repository must exist when backup executes.

Optional `filter` uses the manual backup API's complete filter object and defaults to all files. Omitted `compression` defaults to Zstd 3; null disables compression. Optional `encryption` uses the existing `{algorithm, key_id}` object and defaults to disabled. Active plans resolve key identity at startup. Invalid settings prevent startup with a diagnostic; filesystem failures are recorded as failed jobs when executed.

The service must be running. Scheduling begins with the next minute; downtime and skipped minutes are not replayed. A plan with queued/running work skips that occurrence; other plans and manual tasks share the FIFO worker. Failed backups wait for the next occurrence without automatic retry. Clock jumps and daylight-saving behavior are specified in [scheduling](docs/architecture/scheduling.md).

Open **Scheduled backups** to see the active service's configured plans, including disabled plans, their Cron expressions, timezone, source, repository, compression, encryption key ID, and filters. The list loads when opening the view and updates with **Refresh**. Select **Back up now** to submit the selected plan's saved settings and open Task records. Disabled automatic plans can also run manually, provided any required encryption key is configured. Changes to the plan file require a service restart.

Immediate runs join the normal FIFO queue; they do not interrupt an active task or change Cron cursors. One queued/running task per plan is enforced atomically across manual and automatic triggers. A busy plan returns HTTP 409; unknown plans return 404 and missing encryption keys return 400. Submissions with a lost response are never automatically retried: check Task records first.

Tasks expose nullable `schedule_id` and `scheduled_for` (UTC Unix milliseconds for the matching minute, or the request time for an immediate plan run); both are null for ordinary backups submitted through Create backup. The console displays these in task details. This feature requires job schema v5 without migration, while new backups use format v10. Old job histories require the prior binary and original working directory.

## Encryption keys

Generate independent keys outside the source and backup repository, then configure them on the service:

```shell
bak keygen --output /secure/backup-current.key
bak keygen --output /secure/backup-previous.key
```

`keygen` does not require `-d`, refuses to overwrite an existing file, and creates files with Unix permissions `0600`. Each file contains a random 256-bit key encoded as 64 hexadecimal digits and a newline. The encryption keys are separate from the API Bearer authentication secret.

Add to `config.json`:

```json
"encryption": {
  "keys": {
    "current": "/secure/backup-current.key",
    "previous": "/secure/backup-previous.key"
  }
}
```

The `encryption` section is optional. Key paths may be absolute or relative to the service working directory. All configured keys load at startup; unreadable or invalid files prevent startup. Changes require a service restart and browser reconnection. Keep old keys under stable identifiers while their backups remain in use, and add a new identifier when rotating keys. Backups and queued jobs pin both the identifier and a SHA-256 fingerprint. Removing a key or replacing its contents makes operations requiring the original key fail; it never silently selects another key. Keep independent copies of the required key files: losing a key makes its encrypted backups unrecoverable. Keys are never stored in task history, repository metadata, API responses, or browser storage.

This version writes self-contained backup format v10 and uses task schema v5. Existing v9 backups can share a repository with v10 backups. Formats through v8 require the prior binary and original repository database.

## HTTP API

API routes require `Authorization: Bearer <secret_key>`. `GET /` returns the console when enabled, otherwise a public service message; `GET /api/status` returns the version and `encryption_keys`, a list of `{key_id, fingerprint}` records; key material is never returned. GET routes also support HEAD.

Browser CORS preflights are handled before authentication. API requests allow any browser origin with explicit `Authorization` and `Content-Type` headers and GET/HEAD/POST methods; `Location` is exposed to clients. Actual requests still require the key, including cross-origin requests, and cookie credentials are not enabled. Console CSP permits HTTP/HTTPS API connections while scripts and styles remain restricted to the console origin.

| Request | Behavior |
| --- | --- |
| `POST /api/backups` | Accept JSON with `source`, `repository`, `filter`, optional `compression` (null or a tagged algorithm configuration), and optional `encryption` (null or an object containing `algorithm` and `key_id`); return HTTP 202 after durably queuing a job. |
| `GET /api/schedules` | Return `{"timezone":"...","plans":[...]}` with all startup-loaded plans, including disabled ones and their complete settings. Encryption settings contain algorithm/key ID, never key material. |
| `POST /api/schedules/{id}/run` | No body required. Queue the saved plan immediately and return HTTP 202 with `Location`; percent-encode the plan ID as a URL path segment. Does not advance the Cron cursor. |
| `POST /api/restores` | Accept JSON with `repository`, `backup_id`, and `destination`; return HTTP 202 after durably queuing a job. |
| `GET /api/jobs/{job_id}` | Return HTTP 200 with the stored job, or HTTP 404 if it does not exist. |
| `GET /api/jobs?limit=20&before=CURSOR` | Return newest jobs first in `{"jobs":[...],"next_cursor":null}`; omit `before` for the first page. |
| `GET /api/backups?repository=PATH` | Return HTTP 200 with published backup summaries in `{"backups":[...]}`. |

```shell
curl -i -H 'Authorization: Bearer example-local-test-key' \
  -H 'Content-Type: application/json' \
  -d '{"source":"source","repository":"repository","compression":{"algorithm":"zstd","level":3},"encryption":null,"filter":{"relative_path":{"include":[],"exclude":[]},"uid":{"include":[],"exclude":[]},"gid":{"include":[],"exclude":[]},"mode":{"include":[],"exclude":[]},"atime":{},"mtime":{}}}' \
  http://127.0.0.1:8080/api/backups
# Poll the Location header returned above:
curl -H 'Authorization: Bearer example-local-test-key' \
  http://127.0.0.1:8080/api/jobs/JOB_ID
```

Accepted responses include `Location: /api/jobs/{job_id}` and a job with fields `job_id`, `kind`, `state`, `result`, and `error`. States are `queued`, `running`, `succeeded`, and `failed`. Pending jobs have null result/error; completed jobs contain either a result or an error. POST no longer waits for execution. A failed job query still returns HTTP 200: its `error.code` describes the operation failure. Successful backup jobs contain `{"backup_id":"..."}` in `result`; successful restore jobs contain `{"destination":"..."}`. Both have a null error. Backup and restore execution support Unix platforms only.

Request errors use `{"error":{"code":400,"message":"..."}}` with the corresponding HTTP status. JSON bodies are limited to 2 MiB; `application/json` and `application/*+json` are accepted. Invalid or missing fields return 400, oversized bodies 413, unsupported media types 415, and missing or invalid authentication 401 with `WWW-Authenticate: Bearer`. There is no cancellation endpoint.

`compression` is null or omitted for uncompressed backups. Select `{"algorithm":"zstd","level":3}` or `{"algorithm":"lz77","window_kib":64,"search_depth":32}` to enable a codec. Parameters omitted inside a selected algorithm use its defaults. Unknown algorithms, unknown fields, invalid types and out-of-range values return HTTP 400 before admission. The old boolean `compression` and separate `compression_level` are no longer accepted. The UI still defaults to Zstd; API clients choose explicitly. Complete settings persist in jobs and backup metadata, and restore takes no codec parameters. New backups use manifest v10 with XChaCha encrypted payload v2 or AES encrypted payload v3. The service also reads manifest v9 and uses job schema v5.

An omitted or null `encryption` disables encryption. Select `{"algorithm":"aes-256-gcm","key_id":"current"}` or `{"algorithm":"xchacha20-poly1305","key_id":"current"}`. Both fields are required. Unknown algorithms, fields, keys, invalid types, and the removed `encryption_key_id` request field return HTTP 400 before admission. Both algorithms use the existing 32-byte key files. Accepted encrypted jobs persist the selected algorithm, identifier and fingerprint, allowing restart without persisting the secret. Restore resolves its key from backup metadata.

Backup filter objects require `relative_path`, `uid`, `gid`, `mode`, `atime`, and `mtime`. Regex fields require `include` and `exclude` arrays; empty arrays impose no restriction. Include patterns are alternatives, and any exclude match rejects the entry. Fields combine with AND. Relative paths have no leading slash; UID/GID use decimal text and permissions use four-digit octal text such as `0644`. Time fields accept optional RFC 3339 `from` (inclusive) and `before` (exclusive) bounds, with nanosecond precision and timezone offsets. Filter configuration persists with the queued job and survives restart. Invalid regexes or reversed/equal time bounds fail the job with error code 400 before repository contents are created; they are currently checked during execution, after HTTP 202 admission.

Filtering uses strict directory pruning: a rejected directory and its entire subtree are omitted, so include expressions must also permit the ancestors of selected files. For example, `^docs(/|$)` permits `docs` and its descendants, while `\.txt$` alone rejects a directory named `docs` before inspecting its files. Statistics count only encountered entries: unsupported types are counted before filtering, rejected supported entries are counted once, and descendants of rejected directories are not inspected or counted. Counts are stored in repository metadata; job results currently return only the backup ID. The browser console submits a complete filter object, including when all conditions are blank.

Job listing defaults to 20 entries and accepts limits from 1 to 100. Pass the returned string `next_cursor` as `before`; null means no older entries. Cursors use insertion order, so new jobs do not shift older pages; task states can still change between requests. Each entry includes the usual job fields plus `request` (original paths and request kind), `queued_at`, `started_at`, and `finished_at`. Times are Unix milliseconds; start/finish times are null until recorded. Unknown, duplicate, or invalid listing parameters return 400. Existing submission and individual-job responses are unchanged.

## Persistence and operation lifecycle

The service creates schema-v5 `jobs.sqlite3` in the working directory. One worker executes committed jobs in FIFO insertion order while HTTP status and job queries remain responsive. Completed results and queued jobs survive restart. Client disconnection does not cancel a committed job; retrying a POST creates another job.

Before accepting HTTP requests, startup reconciles jobs left `running`. New backup executions use the persisted task UUID as their backup ID. A matching final archive is fully verified and synchronized before the task is completed as Succeeded. If only one matching regular `.partial` file exists and is complete, it is verified and published without overwriting any final file. Missing, damaged, mismatched, ambiguous, or unverifiable output fails the task with code 500 and an explanation; files are preserved. Encrypted recovery needs the original configured key. Interrupted restores fail without replaying or deleting destination contents. Existing terminal outcomes and queued jobs are unchanged; queued jobs then continue in order. See [startup recovery](docs/architecture/recovery.md) for scope, cleanup, legacy behavior, and failure boundaries.

Recovery removes only temporary hard links confirmed to refer to the same verified file. Unknown files and independent copies remain. A failed database outcome commit stops startup and leaves the task available for the next attempt; a published file is verified again rather than overwritten. Recovery can take time for a large archive, so background startup reports its PID/log path and waits for readiness without an arbitrary timeout. On Ctrl-C or Unix SIGTERM, the service stops accepting requests and claiming jobs, waits for its active operation to finish and persist its outcome, and leaves queued jobs for the next start. A worker or outcome-persistence failure stops the service with a failure exit status rather than continuing to accept jobs with a broken worker.

Run only one service instance per working directory. The working directory task database requires a local filesystem. SQLite uses WAL, `synchronous=FULL`, foreign keys, and a five-second busy timeout; pools allow at most four connections. Schema installation and versioning are transactional; unsupported versions prevent use.

Backups preserve regular files, directories, symbolic links, FIFOs (named pipes), character devices, block devices, and hard links. FIFO and device records store Unix metadata, including the major and minor numbers for device nodes. Socket entries are skipped and counted. Each selected hard-link path is restored, with one payload per regular-file inode. See the [metadata contract](docs/architecture/repository-schema.md).

Each `<backup_id>.bak` at the repository root is a complete backup. It contains a sequential encoded archive plus a plaintext manifest and SHA-256 footer; there is no `repository.sqlite3`, sidecar, `backups/` directory, or mirrored source tree. Copy that file with its original name into an empty repository to list and restore it. Keep the external encryption key: algorithm, key ID, and fingerprint are stored in the file, but key material is not. See the [archive contract](docs/architecture/archive.md).

The pipeline is source entries → archive → optional Zstd/LZ77 → optional encryption, wrapped by a container carrying metadata and a digest. The SHA-256 covers every container byte except the final 32-byte digest itself and is calculated during streaming IO without an extra full-file read. File contents use bounded buffers; collected metadata uses O(number of entries) memory and is limited to 64 MiB per backup. These metadata values remain readable when the payload is encrypted.

Writes go to a unique hidden `.partial` file, which is completed and synchronized before publication via a hard link to the final name. Existing backup IDs cannot be overwritten. The repository directory is synchronized before and after temporary-link removal. Ordinary failures clean up their own temporary output; crashes may leave ignored `.partial` files. Repository filesystems must support hard links and directory synchronization. File publication and task-history completion are separate: a completed file can remain even if the job outcome could not be saved.

Restore requires an existing empty destination outside the repository. It validates metadata and all entry paths/parents before writing, compares decoded headers with the manifest, checks exact file sizes, requires codec/authentication completion, and verifies the container digest. Unix attributes and links are restored, and files/directories are synchronized before success. Restoring a character or block device node requires permission to create it on the destination filesystem. If the operating system denies creation, the job reports the path and system error. Missing backups return error code 404; malformed metadata, damaged archives, and integrity failures fail the task. Failed restores may leave partial output.

Listing scans final `.bak` files and reads their metadata without scanning payloads or requiring keys. Results remain newest first, with backup IDs ascending for timestamp ties. Listing cost grows with the number and metadata size of backups; there is no database index. Invalid final files are errors, while unrelated files and `.partial` files are ignored. Legacy repository databases are preserved and rejected; access those backups with the prior binary and original database.

## Project structure

```text
src/
  main.rs          Startup and exit reporting
  args.rs          Command-line arguments
  config.rs        Configuration loading and validation
  daemon.rs        Background process startup, local control socket, and instance locks
  server.rs        HTTP listener, worker supervision, and graceful shutdown
  server/
    api.rs         Private HTTP routing, authentication, validation, and JSON responses
    schedule.rs    Cron plan validation and scheduled job admission
    ui.rs          Optional external console assets and browser security headers
  jobs.rs          Job manager facade, durable FIFO lifecycle, and the background worker
  jobs/
    archive.rs     Sequential archive framing and streaming file verification
    backup.rs      Full backup traversal, packing and synchronization
    backup/
      filter.rs    Backup path, Unix metadata, and timestamp filtering
    content.rs     Optional compression/encryption pipelines and archive hashing
    content/
      crypto.rs    Named keys and authenticated streaming encryption
      lz77.rs      Project LZ77 matching, framing, streaming IO and codec tests
    restore.rs     Restore path validation, decoding, and integrity verification
    recovery.rs    Startup reconciliation of interrupted jobs and verified backup output
    repository.rs  Self-contained manifests, atomic archive publication, and listing
    filesystem.rs  Shared path resolution, overlap checks, and directory synchronization
    storage.rs     Task database connection settings and schema initialization
  test_support.rs  Temporary directories for tests
tests/
  cli.rs           CLI and background lifecycle integration tests
  server.rs        Application integration tests through the compiled binary and HTTP
web/               Separately deployed HTML, CSS, and JavaScript; optional Playwright browser checks
```

## Current status and acceptance criteria

Command-line parsing, configuration, authenticated HTTP routing, JSON error responses, durable background jobs,
restart recovery, repository metadata storage and listing, Unix full backup and restore execution, and an independently deployed browser console with paginated task history are implemented.

Backup jobs require existing source and repository directories. Equal or nested paths are rejected after resolving symlinks.
Recursive source discovery includes empty directories.
Archive checks cover framing, truncation, corruption, header limits, source size changes, and streaming round trips. Backup checks cover stored metadata, packed contents, empty directories, source preservation, duplicate IDs, and failed metadata publication. Restore component checks cover round trips, buffer boundaries, invalid paths, damaged payloads, missing backups, and overlapping paths. Listing checks cover stable ordering, empty repositories, failed publication, invalid metadata, and HTTP responses. Component tests stay inline with their implementation modules. Run `cargo test jobs::backup::tests` or `cargo test jobs::restore::tests` for the operation suites, `cargo test jobs::repository::tests` for metadata and listing, and `cargo test jobs::filesystem::tests` for shared path logic.

CLI tests in `tests/cli.rs` cover command help, detached startup, live status, duplicate rejection, startup failures, stale sockets after SIGKILL, and graceful stop with active/queued jobs. Cross-component tests live in `tests/server.rs` and launch the compiled binary on an OS-assigned loopback port. They check authentication, failed job reporting, backup/list/restore round trips, occupied destination preservation, persisted history across restart, HTTP responsiveness during an active backup, and SIGTERM shutdown that finishes the active job while retaining queued work. Abrupt termination with SIGKILL after publication verifies that restart recovers the interrupted backup result, resumes queued work, and preserves completed results. A task-database write lock holds completion pending during the shutdown and interruption tests, with real file encoding active. Fixtures terminate and reap child processes and remove temporary directories. Run `cargo test --test server` for these Unix integration tests; `uv run scripts/check.py` includes them.

Proptest generates directory trees, occupied destinations, integrity mismatches, and listing headers with 32 cases per component property. Process-level HTTP round trips and occupied destination checks each use eight generated cases. Fixed seeds keep checks reproducible; failed property cases shrink and are persisted for replay. Retain generated regression seed files in version control. These tests do not simulate power loss.

For the course report and diagram checks, run `uv run scripts/check.py --report`. On macOS with Instruments, `uv run scripts/analyze_backup.py` measures real backup/restore workflows, runs a ten-minute stability sample, and records CPU, allocation, and leak traces using the optimized `profiling` build. This analysis is separate from routine CI. See [report materials and evidence](docs/report/README.md) for datasets, reproduction commands, and measurement limits.

There is no byte-level progress reporting or cancellation.

The current milestone is a minimal end-to-end workflow for local full backups and restores, with these acceptance criteria:

- Preserve regular file contents, directory structure, and empty directories; optionally compress with Zstd or project LZ77 and/or encrypt with XChaCha20-Poly1305 or AES-256-GCM, and verify the SHA-256 integrity of the complete encoded archive.
- Return a durable job ID immediately, execute one operation at a time, and retain queryable outcomes across restart.
- Publish complete repository metadata transactionally after the referenced file contents are durable.
- Restore into a separate empty directory and compare relative paths, entry types, and contents. Report clear failures and interruptions while preserving source files and existing successful backups.
- Complete backup, repository browsing, restore, and task-history queries from the browser console, with visible failures and browser-persisted API connection profiles.

This closes the local MVP workflow. The next reliability priorities are source-file consistency and cleanup after failed or interrupted operations. See the [MVP follow-up assessment](docs/mvp.md) for priorities and current boundaries.

## Documentation

- [C1 System Context](docs/architecture/system-context.md)
- [C2 Containers](docs/architecture/containers.md)

## TODOs

- Hybrid full-backup storage: group small files into independently compressed `tar.zst` packs and compress large files individually. Close a pack when its accumulated uncompressed file size reaches a target; close the final pack at end of traversal even if it is below target. Keep each file within one pack, allow the last file to exceed the target, and impose a file-count limit for empty or tiny files. Large files do not close a pending pack. Stream pack contents without buffering the whole pack in memory. Record file locations, original sizes, SHA-256 checksums, and empty directories in backup metadata; restore each pack in one pass. Choose size and count thresholds using representative data; 32 MiB is an initial candidate, not a fixed requirement. This is a future format change; current backups remain unchanged.
- Source-change detection and a documented consistency policy; broader orphan inspection and interrupted-restore recovery beyond the implemented startup backup reconciliation.
- Repository verification without restoring, and recovery of repository metadata from a redundant copy or a per-backup manifest.
- UI directory browsing, saved repository shortcuts, byte/file progress, and safe task cancellation, as separate follow-ups.
- Directory synchronization support for non-Unix backup and restore execution.
- Permissions, modification times, symbolic links, and other advanced restore capabilities.
- Retention policies, and encryption of repository metadata. Consider WebDAV / S3 after the local workflow is stable. Incremental backups are not planned. Mobile clients, real-time synchronization, and multi-user collaboration are outside the current scope.

## License

[LICENSE](LICENSE).
