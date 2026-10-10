# bak

`bak` is a cross-platform file backup manager for individual users, designed to provide reliable, verifiable local backups that can be restored. A single binary runs an axum HTTP service on Tokio. Users interact with it through curl using a Bearer secret key; a web console is planned for later.

## Development and usage

Install the Rust toolchain specified in `rust-toolchain.toml`. Install uv to run the complete local checks.

```shell
cargo run -- -d working-directory-example
curl -H 'Authorization: Bearer example-local-test-key' http://127.0.0.1:8080/api/status
uv run scripts/check.py
```

Specify an existing working directory containing `config.json`. Supported command-line options are `--help` / `-h`, `--version` / `-v`, and `--working-directory` / `-d`.

Configuration contains `listen`, a nonempty `secret_key`, and `web_ui` settings; see [the example](working-directory-example/config.json). Keep `web_ui.enabled` false while the console is unimplemented. Relative request paths and the web UI directory resolve against the working directory. The former `logging` section must be removed: supplying it prevents startup with an explanation. SQLite job history replaces optional JSONL logs.

## HTTP API

API routes require `Authorization: Bearer <secret_key>`. `GET /` returns a public service message; `GET /api/status` returns the version. GET routes also support HEAD.

| Request | Behavior |
| --- | --- |
| `POST /api/backups` | Accept JSON with `source` and `repository`; return HTTP 202 after durably queuing a job. |
| `POST /api/restores` | Accept JSON with `repository`, `backup_id`, and `destination`; return HTTP 202 after durably queuing a job. |
| `GET /api/jobs/{job_id}` | Return HTTP 200 with the stored job, or HTTP 404 if it does not exist. |
| `GET /api/backups?repository=PATH` | Backup listing remains unimplemented and returns HTTP 501. |

```shell
curl -i -H 'Authorization: Bearer example-local-test-key' \
  -H 'Content-Type: application/json' \
  -d '{"source":"source","repository":"repository"}' \
  http://127.0.0.1:8080/api/backups
# Poll the Location header returned above:
curl -H 'Authorization: Bearer example-local-test-key' \
  http://127.0.0.1:8080/api/jobs/JOB_ID
```

Accepted responses include `Location: /api/jobs/{job_id}` and a job with fields `job_id`, `kind`, `state`, `result`, and `error`. States are `queued`, `running`, `succeeded`, and `failed`. Pending jobs have null result/error; completed jobs contain either a result or an error. POST no longer waits for execution. A failed job query still returns HTTP 200: its `error.code` describes the operation failure. Backup and restore currently finish as failed jobs with error code 501.

Request errors use `{"error":{"code":400,"message":"..."}}` with the corresponding HTTP status. JSON bodies are limited to 2 MiB; `application/json` and `application/*+json` are accepted. Invalid or missing fields return 400, oversized bodies 413, unsupported media types 415, and missing or invalid authentication 401 with `WWW-Authenticate: Bearer`. There is no cancellation endpoint.

## Persistence and operation lifecycle

The service creates `jobs.sqlite3` in the working directory. One worker executes committed jobs in FIFO insertion order while HTTP status and job queries remain responsive. Completed results and queued jobs survive restart. Client disconnection does not cancel a committed job; retrying a POST creates another job.

At startup, jobs left `running` by an earlier service exit become failed with code 500 and an interruption message. They are never automatically retried because execution may have affected external files. Previously queued jobs then continue in order. On Ctrl-C or Unix SIGTERM, the service stops accepting requests and claiming jobs, waits for its active operation to finish and persist its outcome, and leaves queued jobs for the next start. A worker or outcome-persistence failure stops the service with a failure exit status rather than continuing to accept jobs with a broken worker.

Run only one service instance per working directory. Both the working directory database and repository databases require local filesystems. SQLite uses WAL, `synchronous=FULL`, foreign keys, and a five-second busy timeout; pools allow at most four connections. Schema installation and versioning are transactional; unsupported versions prevent use.

Repository metadata read/write support stores backup headers and ordered file/directory entries in `repository.sqlite3` at each repository root. A complete metadata record is committed atomically and an existing backup ID cannot be overwritten. The model retains format version 1, Zstd compression, and SHA-256 checksums. It is not yet connected to backup or restore execution. JSON `manifest.json` files and old JSONL logs are left untouched, and are not read or migrated.

ACID guarantees cover individual database transactions. Job history and repository metadata use separate databases; they do not share a transaction. File contents remain outside SQLite, so future file operations must coordinate durable file writes with metadata publication. WAL databases may have `-wal` and `-shm` sidecars; do not copy a live database file alone as a database backup.

## Project structure

```text
src/
  main.rs          Startup and exit reporting
  args.rs          Command-line arguments
  config.rs        Configuration loading and validation
  api.rs           axum routing, authentication, JSON responses, and shutdown supervision
  jobs.rs          Durable FIFO job lifecycle and the background worker
  storage.rs       SQLite connection settings and schema initialization
  backup.rs        Backup/listing stubs and transactional repository metadata
  restore.rs       Restore execution stub
  test_support.rs  Temporary directories for tests
```

## Current status and acceptance criteria

Command-line parsing, configuration, authenticated HTTP routing, JSON error responses, durable background jobs, restart recovery, and repository metadata storage are implemented. Backup, restore, and listing file operations remain unimplemented. There is no web console, progress reporting, or cancellation.

The current milestone is a minimal end-to-end workflow for local full backups and restores, with these acceptance criteria:

- Preserve regular file contents, directory structure, and empty directories; compress each file with Zstd and verify its SHA-256 integrity.
- Return a durable job ID immediately, execute one operation at a time, and retain queryable outcomes across restart.
- Publish complete repository metadata transactionally after the referenced file contents are durable.
- Restore into a separate empty directory and compare relative paths, entry types, and contents. Report clear failures and interruptions while preserving source files and existing successful backups.

The next step is incremental implementation of file operations, including rejection of overlapping source, repository, and restore destination paths. File consistency and cleanup after abnormal exits remain to be implemented. Verify each step before continuing.

## Documentation

- [C1 System Context](docs/architecture/system-context.md)
- [C2 Containers](docs/architecture/containers.md)

## TODOs

- Web console, controlled by `web_ui.enabled`, after the core local workflow is complete.
- Permissions, modification times, symbolic links, and other advanced restore capabilities.
- Scheduled jobs, incremental backups, retention policies, file filtering, and repository encryption. Consider WebDAV / S3 after the local workflow is stable. Mobile clients, real-time synchronization, and multi-user collaboration are outside the current scope.

## License

[LICENSE](LICENSE).
