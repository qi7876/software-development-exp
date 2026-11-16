# C2 Containers

`bak server start -d PATH` launches the Rust server as a background child in a separate process group, with null stdin and stdout/stderr appended to the working directory's `server.log`. The launcher waits for its child's control socket to report readiness. A user-scoped file lock permits one managed background instance; a separate working-directory lock protects job storage for foreground and background processes. `server status` and `server stop` communicate over a Unix socket under `$XDG_STATE_HOME/bak` (default `$HOME/.local/state/bak`), independent of the HTTP listener and API authentication. Stop requests trigger the same supervised graceful shutdown as signals; the controller reports Stopping until shutdown completes. Stale sockets are removed only while holding the instance lock. `--foreground` runs under the caller's supervision without the user-scoped controller. Background management currently requires Unix.

`bak` runs as a single Rust binary. axum serves HTTP on Tokio; a supervised background worker executes one backup or restore at a time. SQLx provides async SQLite access. Blocking operation code runs through Tokio's blocking task pool, allowing the HTTP service to answer status and job queries during execution.

When `web_ui.enabled` is true, axum reads HTML, CSS, JavaScript, and the favicon at request time from `web_ui.path`. These files are deployed separately from the Rust binary, with no frontend build or runtime asset download. Relative paths resolve against the service working directory; startup validates the required files. Public console assets call a selected service's authenticated API directly, which may be on another host/port. Connection profiles (including plain-text keys) and the selected address live in browser-origin `localStorage`; reopening verifies the selected service before enabling operations. Switching verifies the candidate before replacing the active session, then aborts old requests and clears service-specific paths and data. The console loads task history once after connecting or submitting a task; subsequent updates use the manual Refresh button, without background polling. The history API pages by SQLite insertion sequence, returns request paths and recorded timestamps, and uses string cursors to preserve integer precision in JavaScript. State changes are read afresh on each page request. Browser content is rendered as text. CSP restricts scripts/styles to the console origin while permitting HTTP/HTTPS API connections.

Tower HTTP CORS middleware wraps authentication so preflights succeed without a key and cross-origin errors remain readable. Any browser origin may send explicit Bearer-authenticated GET/HEAD/POST requests; cookie credentials are not enabled. CORS is available even when the UI is disabled. No API proxy or remote-job coordination is introduced: each selected service owns its own configuration, queue, repositories, and filesystem paths.

The service reads configuration from the working directory and owns `jobs.sqlite3` there. HTTP handlers commit jobs to SQLite before acknowledging them with HTTP 202 and a Location header. SQLite is the authoritative FIFO queue and history; a Tokio notification wakes the worker, without owning any job data. The worker atomically claims the oldest queued job, commits its running state, executes without holding a database transaction, and commits the outcome. A database constraint allows only one running job.

```text
Users / automation scripts
          |
          | HTTP + Bearer authentication
          v
     axum HTTP service <------> jobs.sqlite3
          |                         ^
          | wake                    | claim / complete
          v                         |
     Tokio job worker --------------+
          |
          v
     backup / restore
          |
          v
     Local repository: one self-contained .bak per backup
```

Full backup execution stores each backup as one self-contained `<backup_id>.bak`, with a bounded encoded payload and plaintext manifest/footer. The manifest records root/entry metadata, statistics, complete compression/encryption configuration, and creation/source information. A final SHA-256 covers all preceding container bytes. There is no repository database or sidecar. See the [archive contract](archive.md).

The synchronous backup operation runs in the worker's blocking task. It writes a unique hidden temporary file, finishes archive/codec/AEAD framing, appends the manifest and digest, and synchronizes the file. A hard link publishes its complete inode under an exclusive final name; directory sync makes publication durable before temporary-link cleanup. Duplicate IDs never overwrite existing backups. Ordinary failures clean their temporary output; crash leftovers remain ignored. Listing scans final files and validates their metadata/footer without reading payload contents; it no longer benefits from a SQL index.

Restore reads metadata from the same open archive descriptor, validates all paths/parent relationships before creating output, selects the recorded codec and external key, and streams the bounded payload. It checks entry headers/sizes, final authentication, and the container digest, then restores directory/root attributes and synchronizes output. Backup/restore currently require Unix, with hard-link and directory-sync support for repository publication.

Only `jobs.sqlite3` remains: WAL, `synchronous=FULL`, foreign keys, five-second busy timeout, and at most four pooled connections. Its schema installation/version commit together. Backup-file publication and the job outcome cannot share a transaction: a complete file can exist after an interrupted outcome commit. Failed restores can leave partial output. Source files must remain unchanged; the service detects size changes during copying but does not provide a filesystem snapshot.

Restart preserves terminal outcomes and queued work. Before serving requests, startup reconciles leftover running backups against their durable job IDs: matching published output is fully verified, and a single complete temporary can be published before the result is committed. Unverifiable backups and interrupted restores fail without replaying or removing their data. Terminal history is unchanged. See [startup recovery](recovery.md). The worker then continues queued jobs in FIFO order. Ctrl-C and Unix SIGTERM stop admission and further claims, finish the active job, and retain the queue for restart. The server supervises the HTTP service and worker: unexpected termination or failure to persist an outcome stops the service with a failure exit status.

Use one service instance per working directory and local filesystems for SQLite databases. Multi-process job coordination, cancellation, automatic retries, progress reporting, and general filesystem repair beyond startup backup reconciliation are outside the current implementation. JSONL logs are no longer written; SQLite history is always enabled. Existing manifests and logs are preserved on disk but not consumed.

The source modules divide responsibilities into configuration and arguments, server lifecycle supervision, and the job manager facade. The server owns private API and scheduling modules: the API handles HTTP routing, authentication, validation, and response mapping; scheduling compiles Cron plans and admits jobs under the service lifecycle. The job manager facade owns private modules for archive framing, content processing, startup reconciliation, backup execution, restore execution, repository metadata and listing, shared filesystem operations, and SQLite initialization. Backup execution owns its private `filter` module and exports the filter configuration through the job manager facade. Repository metadata owns persisted backup statistics, so it does not depend on backup execution. Content processing owns private `crypto` and `lz77` submodules and composes their streams with Zstd and archive hashing. Backup, restore, and repository code use types exposed by `content`; startup and scheduling obtain key management through the job manager facade. HTTP handlers use the facade instead of accessing repository internals. Cross-component HTTP and lifecycle tests in `tests/` run the compiled application without exposing private modules for testing. [README](../../README.md) documents the module layout, HTTP contract, configuration, current status, and acceptance criteria.


At startup, the service loads optional named encryption keys from separate files. The job manager shares this immutable in-memory keyring with the worker. API admission persists only the selected key identifier and fingerprint. The content pipeline composes optional Zstd or project LZ77 compression with optional XChaCha20-Poly1305 or AES-256-GCM STREAM encryption; restore reverses this pipeline. The browser receives key identifiers and fingerprints for selection and restore eligibility, never key material. See [content encryption](content-encryption.md).

The in-process scheduler reads configured Cron plans, records per-plan UTC cursors, and admits requests into the same durable FIFO queue. Its immutable catalog is shared with authenticated HTTP handlers for listing configured plans and immediate execution; manual plan runs use the same active-plan guard without changing Cron cursors. It shares service supervision and shutdown with HTTP and the worker. See [scheduling](scheduling.md).
