# bak

`bak` is a file backup manager for individual users, designed to provide reliable, verifiable local backups that can be restored. A single binary runs an axum HTTP service on Tokio and includes a browser console. Users access backups, restores, and durable task history through the console or curl using a Bearer secret key. The current MVP supports backup and restore execution on Unix.

## Development and usage

Install the Rust toolchain specified in `rust-toolchain.toml`. Install uv to run the complete local checks.

```shell
mkdir -p working-directory-example/source working-directory-example/repository working-directory-example/restored
cargo run -- -d working-directory-example
curl -H 'Authorization: Bearer example-local-test-key' http://127.0.0.1:8080/api/status
uv run scripts/check.py
```

Specify an existing working directory containing `config.json`. Supported command-line options are `--help` / `-h`, `--version` / `-v`, and `--working-directory` / `-d`.

Configuration contains `listen`, a nonempty `secret_key`, and optional `web_ui` settings; see [the example](working-directory-example/config.json). Set `web_ui.enabled` to true to serve the embedded console; omission defaults to false. The example enables it. No asset downloads, frontend build, or separate web server are needed. The obsolete `web_ui.archive_url` and `web_ui.directory` fields are accepted but ignored. Relative request paths resolve against the working directory. The former `logging` section must be removed: supplying it prevents startup with an explanation. SQLite job history replaces optional JSONL logs.

## Browser console

With the example service running, open <http://127.0.0.1:8080/> and click Manage connections at the bottom of the sidebar. Add an API address such as `127.0.0.1:8080` and its key (`example-local-test-key` for the example). An optional name identifies the connection. Verify and save checks the key against `/api/status`, saves the connection, and activates it. Saved connections can be switched, edited, or deleted on this page; the three main sidebar entries remain backup, repository, and task views.

Addresses accept HTTP/HTTPS origins with an optional port; omitting the scheme uses HTTP. Paths, query strings, URL credentials, and fragments are not accepted. The console may use its own API or another reachable server, including servers with `web_ui.enabled=false`. The target must run a version that supports cross-origin API requests. Remote servers must listen on a reachable interface (for example, `0.0.0.0:8080` rather than loopback only). For an HTTPS console, use HTTPS remote APIs to avoid browser mixed-content restrictions; bak has no built-in TLS, so HTTPS needs a reverse proxy.

Connection names, normalized API addresses, keys, and the last selected connection are stored in `localStorage` under `bak.connections.v1`. Keys are stored in plain text in the browser profile, not encrypted. The cache belongs to the console's browser origin (scheme, host, and port); opening the console at a different origin uses a separate cache. Reloading or reopening automatically verifies the last selected connection before enabling operations. Disconnect retains saved connections but disables automatic reconnection until one is selected again; Delete removes that connection and key from the cache. Storage failures are displayed rather than silently discarding changes. The console is public, while all operation APIs still require a Bearer key; browser requests do not use cookies.

Switching verifies the target before replacing the active connection. A failed check preserves the current service. A successful switch aborts old page requests and clears paths, loaded backups, and task records so data from different servers is not mixed. Already submitted server jobs continue independently.

1. Put files in `working-directory-example/source`. In the console, create a backup with source `source` and repository `repository`. Compression defaults to enabled; encryption defaults to disabled.
2. Follow View task to the task records and wait for success. View backup opens the repository directly; an existing repository can also be loaded by path from the repository view.
3. Choose a backup, click restore, and enter `restored`, an existing empty directory. Submission opens the task records. Check the result and restored contents.

The backup form offers a collapsed **过滤条件** section for relative path, UID, GID, octal permissions, modification time, and access time. Regex inputs accept one pattern per line; blank lines are ignored. Fields combine with AND, include patterns with OR, and exclusions take precedence. Conditions apply to files, directories, and symlinks; a rejected directory skips its entire subtree, so include patterns must also permit ancestors. Local date/time inputs use the displayed browser timezone, with second precision, inclusive starts, and exclusive ends. Collapsing retains active conditions; Clear removes them. Inputs remain while navigating within one service and are cleared on disconnect or a successful service switch. Regex validation happens during job execution; invalid patterns appear as failed jobs.

Backup storage has two independent runtime options: **压缩（Zstd）** and **加密（XChaCha20-Poly1305）**. The four combinations store raw bytes, Zstd bytes, encrypted bytes, or encrypted Zstd bytes. Selecting both always compresses before encrypting. Encryption requires choosing a configured server key. The repository and restore dialog show each backup's storage mode, and restore automatically uses its saved mode and key. The console disables restore when the service lacks the matching key fingerprint.

Encryption protects regular-file contents only. Filenames, directory structure, Unix metadata, original sizes, SHA-256 checksums, and symbolic link targets remain visible. The API service reads plaintext and holds configured keys in memory; this is repository encryption, not end-to-end browser encryption. Failed restores can leave partial output.

All paths refer to the device running bak, including when the browser is on another device. Source, repository, and restore destination directories must exist. No OS directory picker or automatic directory creation is provided. Source and repository must not overlap; restore destination must be empty and outside the repository. Symbolic links are preserved; special files are skipped. Keep source files unchanged during a backup. Failed or interrupted restores can leave partial output.

The console supports wide desktop windows (at least 1200 CSS pixels). The three sidebar entries open separate views, preserve form inputs when switching views within one service, and support browser back/forward navigation. Task records update every three seconds, show server-side errors, and paginate through persisted history. Task IDs and execution timestamps expand under Details. A connection failure is also shown in the sidebar while the console keeps retrying; an invalid key opens connection management for correction. The UI shows task states rather than byte progress. Requests that lose their response are not automatically resubmitted: inspect history before retrying. The service uses one shared key rather than separate user accounts.

Browser assets live in `web/` and are embedded at Rust compile time; rebuild after editing them. Node.js 20 or later is needed only for the optional browser checks:

```shell
npm --prefix web ci
# Install the matching browser once, or use BAK_BROWSER_CHANNEL=chrome with installed Chrome:
cd web
npx playwright install chromium
cd ..
uv run scripts/check.py --web-ui
```

The browser checks launch disposable real services and verify authentication, backup/restore content and empty directories, preservation of an occupied destination, durable history, pagination, text-safe rendering of paths/errors, network recovery, and desktop view navigation. Two services with different keys exercise cross-origin backup submission, failed-switch preservation, cache restoration after closing a page, editing/deleting profiles, and clearing old-service data on successful switches. Test files use `.js` with the web package's ES module setting.

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

This version is a breaking format change: repository databases require schema/format v3 and task databases require schema v2. Use a new service working directory and a new repository. Previous databases are rejected, not migrated or deleted; use the previous application version and original directories to access older backups.

## HTTP API

API routes require `Authorization: Bearer <secret_key>`. `GET /` returns the console when enabled, otherwise a public service message; `GET /api/status` returns the version and `encryption_keys`, a list of `{key_id, fingerprint}` records; key material is never returned. GET routes also support HEAD.

Browser CORS preflights are handled before authentication. API requests allow any browser origin with explicit `Authorization` and `Content-Type` headers and GET/HEAD/POST methods; `Location` is exposed to clients. Actual requests still require the key, including cross-origin requests, and cookie credentials are not enabled. Console CSP permits HTTP/HTTPS API connections while scripts and styles remain restricted to the console origin.

| Request | Behavior |
| --- | --- |
| `POST /api/backups` | Accept JSON with `source`, `repository`, `filter`, required `compression` (boolean), and optional `encryption_key_id` (string or null); return HTTP 202 after durably queuing a job. |
| `POST /api/restores` | Accept JSON with `repository`, `backup_id`, and `destination`; return HTTP 202 after durably queuing a job. |
| `GET /api/jobs/{job_id}` | Return HTTP 200 with the stored job, or HTTP 404 if it does not exist. |
| `GET /api/jobs?limit=20&before=CURSOR` | Return newest jobs first in `{"jobs":[...],"next_cursor":null}`; omit `before` for the first page. |
| `GET /api/backups?repository=PATH` | Return HTTP 200 with published backup summaries in `{"backups":[...]}`. |

```shell
curl -i -H 'Authorization: Bearer example-local-test-key' \
  -H 'Content-Type: application/json' \
  -d '{"source":"source","repository":"repository","compression":true,"encryption_key_id":null,"filter":{"relative_path":{"include":[],"exclude":[]},"uid":{"include":[],"exclude":[]},"gid":{"include":[],"exclude":[]},"mode":{"include":[],"exclude":[]},"atime":{},"mtime":{}}}' \
  http://127.0.0.1:8080/api/backups
# Poll the Location header returned above:
curl -H 'Authorization: Bearer example-local-test-key' \
  http://127.0.0.1:8080/api/jobs/JOB_ID
```

Accepted responses include `Location: /api/jobs/{job_id}` and a job with fields `job_id`, `kind`, `state`, `result`, and `error`. States are `queued`, `running`, `succeeded`, and `failed`. Pending jobs have null result/error; completed jobs contain either a result or an error. POST no longer waits for execution. A failed job query still returns HTTP 200: its `error.code` describes the operation failure. Successful backup jobs contain `{"backup_id":"..."}` in `result`; successful restore jobs contain `{"destination":"..."}`. Both have a null error. Backup and restore execution support Unix platforms only.

Request errors use `{"error":{"code":400,"message":"..."}}` with the corresponding HTTP status. JSON bodies are limited to 2 MiB; `application/json` and `application/*+json` are accepted. Invalid or missing fields return 400, oversized bodies 413, unsupported media types 415, and missing or invalid authentication 401 with `WWW-Authenticate: Bearer`. There is no cancellation endpoint.

An omitted or null `encryption_key_id` disables encryption. An unknown key returns HTTP 400 before admission. Accepted encrypted jobs persist the selected identifier and fingerprint, allowing restart without persisting the secret. Restore resolves its key from backup metadata.

Backup filter objects require `relative_path`, `uid`, `gid`, `mode`, `atime`, and `mtime`. Regex fields require `include` and `exclude` arrays; empty arrays impose no restriction. Include patterns are alternatives, and any exclude match rejects the entry. Fields combine with AND. Relative paths have no leading slash; UID/GID use decimal text and permissions use four-digit octal text such as `0644`. Time fields accept optional RFC 3339 `from` (inclusive) and `before` (exclusive) bounds, with nanosecond precision and timezone offsets. Filter configuration persists with the queued job and survives restart. Invalid regexes or reversed/equal time bounds fail the job with error code 400 before repository contents are created; they are currently checked during execution, after HTTP 202 admission.

Filtering uses strict directory pruning: a rejected directory and its entire subtree are omitted, so include expressions must also permit the ancestors of selected files. For example, `^docs(/|$)` permits `docs` and its descendants, while `\.txt$` alone rejects a directory named `docs` before inspecting its files. Statistics count only encountered entries: unsupported types are counted before filtering, rejected supported entries are counted once, and descendants of rejected directories are not inspected or counted. Counts are stored in repository metadata; job results currently return only the backup ID. The browser console submits a complete filter object, including when all conditions are blank.

Job listing defaults to 20 entries and accepts limits from 1 to 100. Pass the returned string `next_cursor` as `before`; null means no older entries. Cursors use insertion order, so new jobs do not shift older pages; task states can still change between requests. Each entry includes the usual job fields plus `request` (original paths and request kind), `queued_at`, `started_at`, and `finished_at`. Times are Unix milliseconds; start/finish times are null until recorded. Unknown, duplicate, or invalid listing parameters return 400. Existing submission and individual-job responses are unchanged.

## Persistence and operation lifecycle

The service creates schema-v2 `jobs.sqlite3` in the working directory. One worker executes committed jobs in FIFO insertion order while HTTP status and job queries remain responsive. Completed results and queued jobs survive restart. Client disconnection does not cancel a committed job; retrying a POST creates another job.

At startup, jobs left `running` by an earlier service exit become failed with code 500 and an interruption message. They are never automatically retried because execution may have affected external files. Previously queued jobs then continue in order. On Ctrl-C or Unix SIGTERM, the service stops accepting requests and claiming jobs, waits for its active operation to finish and persist its outcome, and leaves queued jobs for the next start. A worker or outcome-persistence failure stops the service with a failure exit status rather than continuing to accept jobs with a broken worker.

Run only one service instance per working directory. Both the working directory database and repository databases require local filesystems. SQLite uses WAL, `synchronous=FULL`, foreign keys, and a five-second busy timeout; pools allow at most four connections. Schema installation and versioning are transactional; unsupported versions prevent use.

> Development status: repository format v3 backs up Unix metadata and symbolic link targets, with per-backup entry and skip counts. Restore applies Unix metadata and recreates symbolic links; backup filtering is implemented with strict directory pruning. See [the schema contract and remaining work](docs/architecture/repository-schema.md).

Full backups store headers and ordered file/directory entries in `repository.sqlite3` at each repository root. A complete metadata record is committed atomically and an existing backup ID cannot be overwritten. Format version 3 stores a mirrored tree under `backups/<backup_id>/`: each regular file retains its relative name and contains raw, Zstd-compressed, encrypted, or compressed-then-encrypted bytes according to its backup header. Directory entries preserve empty directories. Metadata records each file's original byte count and SHA-256 checksum of its uncompressed contents, plus a UTC RFC 3339 creation timestamp. Entry ordinals preserve metadata ordering; file locations are determined by relative paths.

Backup execution finishes and synchronizes each compressed file, synchronizes the mirrored directories and their repository ancestors, then commits the metadata transaction before reporting success. SQLite metadata defines which backups are published; a directory alone does not establish a completed backup. JSON `manifest.json` files and old JSONL logs are left untouched, and are not read or migrated.

Restore requires an existing empty destination directory whose canonical path does not overlap the repository. It validates every metadata entry path before writing, decodes mirrored files according to the saved mode without overwriting existing paths, authenticates encrypted streams, verifies uncompressed sizes and SHA-256 checksums, then synchronizes files and directories before success. Missing backups return error code 404; invalid metadata, damaged contents, and integrity mismatches fail the job. A failed or interrupted restore may leave partial output; automatic cleanup is not implemented.

Listing reads committed metadata headers and returns each backup's `backup_id` and `created_at`, newest first with backup IDs ascending for equal timestamps. It does not inspect or verify payload contents. An existing repository directory without a metadata database returns an empty list without creating the database; directories from unpublished backups are ignored. Invalid paths, corrupt databases, and unsupported schemas return errors rather than an empty list.

ACID guarantees cover individual database transactions. Job history and repository metadata use separate databases; they do not share a transaction. File contents remain outside SQLite. Failed or interrupted operations may leave backup directories without metadata; automatic cleanup is not implemented. Keep source files unchanged while backing up: concurrent source modifications are not detected. WAL databases may have `-wal` and `-shm` sidecars; do not copy a live database file alone as a database backup.

## Project structure

```text
src/
  main.rs          Startup and exit reporting
  args.rs          Command-line arguments
  config.rs        Configuration loading and validation
  server.rs        HTTP listener, worker supervision, and graceful shutdown
  server/
    api.rs         Private HTTP routing, authentication, validation, and JSON responses
    ui.rs          Optional embedded console assets and browser security headers
  jobs.rs          Job manager facade, durable FIFO lifecycle, and the background worker
  jobs/
    backup.rs      Full backup traversal and synchronization
    content.rs     Optional compression/encryption pipelines
    crypto.rs      Named keys and authenticated streaming encryption
    restore.rs     Restore path validation, decoding, and integrity verification
    repository.rs  Repository metadata types, transactional persistence, and listing
    filesystem.rs  Shared path resolution, overlap checks, and directory synchronization
    storage.rs     SQLite connection settings and schema initialization
  test_support.rs  Temporary directories for tests
tests/
  server.rs        Application integration tests through the compiled binary and HTTP
web/               Embedded HTML, CSS, and JavaScript; optional Playwright browser checks
```

## Current status and acceptance criteria

Command-line parsing, configuration, authenticated HTTP routing, JSON error responses, durable background jobs,
restart recovery, repository metadata storage and listing, Unix full backup and restore execution, and an embedded browser console with paginated task history are implemented.

Backup jobs require existing source and repository directories. Equal or nested paths are rejected after resolving symlinks.
Recursive source discovery is implemented, including empty directories. Symbolic links are preserved; unsupported entry types are skipped and counted.
Backup checks cover stored metadata, decompressed contents, empty directories, source preservation, duplicate IDs, and failed metadata publication. Restore component checks cover round trips, buffer boundaries, invalid paths, damaged payloads, missing backups, and overlapping paths. Listing checks cover stable ordering, empty repositories, failed publication, invalid metadata, and HTTP responses. Component tests stay inline with their implementation modules. Run `cargo test jobs::backup::tests` or `cargo test jobs::restore::tests` for the operation suites, `cargo test jobs::repository::tests` for metadata and listing, and `cargo test jobs::filesystem::tests` for shared path logic.

Cross-component tests live in `tests/server.rs` and launch the compiled binary on an OS-assigned loopback port. They check authentication, failed job reporting, backup/list/restore round trips, occupied destination preservation, persisted history across restart, HTTP responsiveness during an active backup, and SIGTERM shutdown that finishes the active job while retaining queued work. Abrupt termination with SIGKILL verifies that restart fails the interrupted job, resumes queued work, and preserves completed results. A real repository SQLite write lock holds metadata publication pending during the shutdown and interruption tests. Fixtures terminate and reap child processes and remove temporary directories. Run `cargo test --test server` for these Unix integration tests; `uv run scripts/check.py` includes them.

Proptest generates directory trees, occupied destinations, integrity mismatches, and listing headers with 32 cases per component property. Process-level HTTP round trips and occupied destination checks each use eight generated cases. Fixed seeds keep checks reproducible; failed property cases shrink and are persisted for replay. Retain generated regression seed files in version control. These tests do not simulate power loss.

For the course report and diagram checks, run `uv run scripts/check.py --report`. On macOS with Instruments, `uv run scripts/analyze_backup.py` measures real backup/restore workflows, runs a ten-minute stability sample, and records CPU, allocation, and leak traces using the optimized `profiling` build. This analysis is separate from routine CI. See [report materials and evidence](docs/report/README.md) for datasets, reproduction commands, and measurement limits.

There is no byte-level progress reporting or cancellation.

The current milestone is a minimal end-to-end workflow for local full backups and restores, with these acceptance criteria:

- Preserve regular file contents, directory structure, and empty directories; optionally compress with Zstd and/or encrypt with XChaCha20-Poly1305, and verify plaintext SHA-256 integrity.
- Return a durable job ID immediately, execute one operation at a time, and retain queryable outcomes across restart.
- Publish complete repository metadata transactionally after the referenced file contents are durable.
- Restore into a separate empty directory and compare relative paths, entry types, and contents. Report clear failures and interruptions while preserving source files and existing successful backups.
- Complete backup, repository browsing, restore, and task-history queries from the embedded console, with visible failures and browser-persisted API connection profiles.

This closes the local MVP workflow. The next reliability priorities are source-file consistency and cleanup after failed or interrupted operations. See the [MVP follow-up assessment](docs/mvp.md) for priorities and current boundaries.

## Documentation

- [C1 System Context](docs/architecture/system-context.md)
- [C2 Containers](docs/architecture/containers.md)

## TODOs

- Hybrid full-backup storage: group small files into independently compressed `tar.zst` packs and compress large files individually. Close a pack when its accumulated uncompressed file size reaches a target; close the final pack at end of traversal even if it is below target. Keep each file within one pack, allow the last file to exceed the target, and impose a file-count limit for empty or tiny files. Large files do not close a pending pack. Stream pack contents without buffering the whole pack in memory. Record file locations, original sizes, SHA-256 checksums, and empty directories in backup metadata; restore each pack in one pass. Choose size and count thresholds using representative data; 32 MiB is an initial candidate, not a fixed requirement. This is a future format change; current backups remain unchanged.
- Source-change detection and a documented consistency policy; failed/interrupted-operation cleanup and orphan reconciliation.
- Repository verification without restoring, and recovery of repository metadata from a redundant copy or a per-backup manifest.
- UI directory browsing, saved repository shortcuts, byte/file progress, and safe task cancellation, as separate follow-ups.
- Directory synchronization support for non-Unix backup and restore execution.
- Permissions, modification times, symbolic links, and other advanced restore capabilities.
- Scheduled jobs, retention policies, and encryption of repository metadata. Consider WebDAV / S3 after the local workflow is stable. Incremental backups are not planned. Mobile clients, real-time synchronization, and multi-user collaboration are outside the current scope.

## License

[LICENSE](LICENSE).
