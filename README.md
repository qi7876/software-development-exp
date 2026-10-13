# bak

`bak` is a cross-platform file backup manager for individual users, designed to provide reliable, verifiable local backups that can be restored. A single binary runs an axum HTTP service on Tokio. Users interact with it through curl using a Bearer secret key; a web console is planned for later.

## Development and usage

Install the Rust toolchain specified in `rust-toolchain.toml`. Install uv to run the complete local checks.

```shell
mkdir -p working-directory-example/source working-directory-example/repository
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
| `GET /api/backups?repository=PATH` | Return HTTP 200 with published backup summaries in `{"backups":[...]}`. |

```shell
curl -i -H 'Authorization: Bearer example-local-test-key' \
  -H 'Content-Type: application/json' \
  -d '{"source":"source","repository":"repository"}' \
  http://127.0.0.1:8080/api/backups
# Poll the Location header returned above:
curl -H 'Authorization: Bearer example-local-test-key' \
  http://127.0.0.1:8080/api/jobs/JOB_ID
```

Accepted responses include `Location: /api/jobs/{job_id}` and a job with fields `job_id`, `kind`, `state`, `result`, and `error`. States are `queued`, `running`, `succeeded`, and `failed`. Pending jobs have null result/error; completed jobs contain either a result or an error. POST no longer waits for execution. A failed job query still returns HTTP 200: its `error.code` describes the operation failure. Successful backup jobs contain `{"backup_id":"..."}` in `result`; successful restore jobs contain `{"destination":"..."}`. Both have a null error. Backup and restore execution currently support Unix platforms; on other platforms they fail with code 501 before creating output contents.

Request errors use `{"error":{"code":400,"message":"..."}}` with the corresponding HTTP status. JSON bodies are limited to 2 MiB; `application/json` and `application/*+json` are accepted. Invalid or missing fields return 400, oversized bodies 413, unsupported media types 415, and missing or invalid authentication 401 with `WWW-Authenticate: Bearer`. There is no cancellation endpoint.

## Persistence and operation lifecycle

The service creates `jobs.sqlite3` in the working directory. One worker executes committed jobs in FIFO insertion order while HTTP status and job queries remain responsive. Completed results and queued jobs survive restart. Client disconnection does not cancel a committed job; retrying a POST creates another job.

At startup, jobs left `running` by an earlier service exit become failed with code 500 and an interruption message. They are never automatically retried because execution may have affected external files. Previously queued jobs then continue in order. On Ctrl-C or Unix SIGTERM, the service stops accepting requests and claiming jobs, waits for its active operation to finish and persist its outcome, and leaves queued jobs for the next start. A worker or outcome-persistence failure stops the service with a failure exit status rather than continuing to accept jobs with a broken worker.

Run only one service instance per working directory. Both the working directory database and repository databases require local filesystems. SQLite uses WAL, `synchronous=FULL`, foreign keys, and a five-second busy timeout; pools allow at most four connections. Schema installation and versioning are transactional; unsupported versions prevent use.

Full backups store headers and ordered file/directory entries in `repository.sqlite3` at each repository root. A complete metadata record is committed atomically and an existing backup ID cannot be overwritten. Format version 1 stores a mirrored tree under `backups/<backup_id>/`: each regular file retains its relative name and contains Zstd-compressed bytes. Directory entries preserve empty directories. Metadata records each file's original byte count and SHA-256 checksum of its uncompressed contents, plus a UTC RFC 3339 creation timestamp. Entry ordinals preserve metadata ordering; file locations are determined by relative paths.

Backup execution finishes and synchronizes each compressed file, synchronizes the mirrored directories and their repository ancestors, then commits the metadata transaction before reporting success. SQLite metadata defines which backups are published; a directory alone does not establish a completed backup. JSON `manifest.json` files and old JSONL logs are left untouched, and are not read or migrated.

Restore requires an existing empty destination directory whose canonical path does not overlap the repository. It validates every metadata entry path before writing, decompresses mirrored files without overwriting existing paths, verifies uncompressed sizes and SHA-256 checksums, then synchronizes files and directories before success. Missing backups return error code 404; invalid metadata, damaged contents, and integrity mismatches fail the job. A failed or interrupted restore may leave partial output; automatic cleanup is not implemented.

Listing reads committed metadata headers and returns each backup's `backup_id` and `created_at`, newest first with backup IDs ascending for equal timestamps. It does not inspect or verify payload contents. An existing repository directory without a metadata database returns an empty list without creating the database; directories from unpublished backups are ignored. Invalid paths, corrupt databases, and unsupported schemas return errors rather than an empty list.

ACID guarantees cover individual database transactions. Job history and repository metadata use separate databases; they do not share a transaction. File contents remain outside SQLite. Failed or interrupted operations may leave backup directories without metadata; automatic cleanup is not implemented. Keep source files unchanged while backing up: concurrent source modifications are not detected. WAL databases may have `-wal` and `-shm` sidecars; do not copy a live database file alone as a database backup.

## Project structure

```text
src/
  main.rs          Startup and exit reporting
  args.rs          Command-line arguments
  config.rs        Configuration loading and validation
  api.rs           axum routing, authentication, JSON responses, and shutdown supervision
  jobs.rs          Durable FIFO job lifecycle and the background worker
  storage.rs       SQLite connection settings and schema initialization
  backup.rs        Full backups, mirrored file storage, transactional metadata, and listing
  restore.rs       Restore path validation, decompression, and integrity verification
  test_support.rs  Temporary directories for tests
```

## Current status and acceptance criteria

Command-line parsing, configuration, authenticated HTTP routing, JSON error responses, durable background jobs,
restart recovery, repository metadata storage and listing, and Unix full backup and restore execution are implemented.

Backup jobs require existing source and repository directories. Equal or nested paths are rejected after resolving symlinks.
Recursive source discovery is implemented, including empty directories. Symlinks and other unsupported types inside source are rejected.
Backup checks cover stored metadata, decompressed contents, empty directories, source preservation, duplicate IDs, and failed metadata publication. Restore integration checks cover round trips, buffer boundaries, invalid paths, damaged payloads, missing backups, and overlapping paths. Listing checks cover stable ordering, empty repositories, failed publication, invalid metadata, and HTTP responses. Proptest generates directory trees, occupied destinations, integrity mismatches, and listing headers with 32 cases per property and fixed seeds for reproducible checks. Failed property cases shrink and are persisted for replay; retain generated regression seed files in version control. Run `cargo test restore::tests` or `cargo test backup::list_tests` for the respective suites. These tests do not simulate power loss.
There is no web console, progress reporting, or cancellation.

The current milestone is a minimal end-to-end workflow for local full backups and restores, with these acceptance criteria:

- Preserve regular file contents, directory structure, and empty directories; compress each file with Zstd and verify its SHA-256 integrity.
- Return a durable job ID immediately, execute one operation at a time, and retain queryable outcomes across restart.
- Publish complete repository metadata transactionally after the referenced file contents are durable.
- Restore into a separate empty directory and compare relative paths, entry types, and contents. Report clear failures and interruptions while preserving source files and existing successful backups.

The next work is file consistency and cleanup after failed or interrupted operations. Verify each step before continuing.

## Documentation

- [C1 System Context](docs/architecture/system-context.md)
- [C2 Containers](docs/architecture/containers.md)

## TODOs

- Web console, controlled by `web_ui.enabled`, after the core local workflow is complete.
- Directory synchronization support for non-Unix backup and restore execution.
- Permissions, modification times, symbolic links, and other advanced restore capabilities.
- Scheduled jobs, incremental backups, retention policies, file filtering, and repository encryption. Consider WebDAV / S3 after the local workflow is stable. Mobile clients, real-time synchronization, and multi-user collaboration are outside the current scope.

## License

[LICENSE](LICENSE).
