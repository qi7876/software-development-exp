# C2 Containers

`bak` runs as a single Rust binary. axum serves HTTP on Tokio; a supervised background worker executes one backup or restore at a time. SQLx provides async SQLite access. Blocking operation code runs through Tokio's blocking task pool, allowing the HTTP service to answer status and job queries during execution.

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
     Local repository: repository.sqlite3 + external file contents
```

Repository metadata support stores each backup's header and ordered entries in one transaction in `repository.sqlite3` at the repository root. Backups are immutable by ID. This replaces JSON manifest file I/O; no legacy manifest import or read fallback exists. Backup, restore, and listing remain stubs, so this metadata support is not yet connected to the worker's file operations.

SQLite databases use WAL, `synchronous=FULL`, foreign keys, a five-second busy timeout, and pools of at most four connections. Schema initialization and its version commit together; unsupported versions are errors. ACID applies separately to job and repository database transactions. External file contents and these two databases cannot be committed together. Future backup implementation must make referenced file contents durable before publishing metadata and handle incomplete external work after interruptions.

Restart preserves terminal outcomes and queued work. Before serving requests, startup marks leftover running jobs failed with an interruption error; it does not replay them. The worker then continues queued jobs in FIFO order. Ctrl-C and Unix SIGTERM stop admission and further claims, finish the active job, and retain the queue for restart. The HTTP service supervises the worker: unexpected termination or failure to persist an outcome stops the service with a failure exit status.

Use one service instance per working directory and local filesystems for SQLite databases. Multi-process job coordination, cancellation, automatic retries, progress reporting, and filesystem crash recovery are outside the current implementation. JSONL logs are no longer written; SQLite history is always enabled. Existing manifests and logs are preserved on disk but not consumed.

The source modules divide responsibilities into configuration and arguments, HTTP routing and lifecycle supervision, durable jobs and worker execution, SQLite initialization, and backup/restore logic. [README](../../README.md) documents the HTTP contract, configuration, current status, and acceptance criteria.
