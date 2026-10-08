# bak

`bak` is a cross-platform file backup manager for individual users, designed to provide reliable, verifiable local backups that can be restored.
A single binary runs an HTTP service. Users interact with it through curl using a Bearer secret key; a web console is planned for later.

## Development and usage

```shell
cargo run -- -d working-directory-example
curl -H 'Authorization: Bearer example-local-test-key' http://127.0.0.1:8080/api/status
uv run scripts/check.py
```

Specify a working directory containing `config.json`.
Supported command-line options are `--help` / `-h`, `--version` / `-v`, and `--working-directory` / `-d`.

## Project structure

```text
src/
  main.rs     Startup sequence and error reporting on exit
  args.rs     Command-line arguments
  config.rs   Configuration loading and validation
  api.rs      HTTP server, routing, authentication, and requests / responses
  jobs.rs     Synchronous operations, in-memory job results, and job logs
  backup.rs   Backup execution and listing
  restore.rs  Restore execution
```

## Current status and acceptance criteria

Command-line parsing, configuration, HTTP routing, authentication, JSON error responses, completed job queries, and optional JSON Lines job logs are implemented. Backup, restore, and listing file operations are not yet implemented and return HTTP 501. Backup and restore responses include the completed job with `state: "failed"` and `error.code: 501`. The manifest model and its read/write operations are implemented but have not yet been integrated with file operations. There is currently no web console or progress reporting.

The HTTP server uses one request worker. Backup and restore run synchronously within their requests: a response contains the final job result, with HTTP 200 on success or the operation's error status on failure, plus a `Location` header for querying the completed job. Other requests, including status and job queries, wait until the current request finishes. There are no background job threads, concurrent operations, or cancellation endpoint. `GET /api/status` returns the version only.

`logging.enabled` controls job logging and defaults to `false`. `logging.file` specifies the log file and defaults to `jobs.jsonl`; relative paths are resolved against the working directory. Logs append a `running` record before each operation and a `succeeded` or `failed` record afterward, including the request, result, and error. A start-log failure prevents execution; a completion-log failure returns HTTP 500 while retaining the completed result in memory. Restarting clears in-memory jobs, so queries for previous `job_id` values return HTTP 404. When logging is enabled, logs remain available for troubleshooting. Backup metadata is stored separately in manifests.

The current milestone is a minimal end-to-end workflow for local full backups and restores, with the following acceptance criteria:

- Preserve regular file contents, directory structure, and empty directories; compress each file with Zstd, save a manifest, and verify content integrity.
- Execute backup and restore requests synchronously, return the completed result and job ID, and support later result queries.
- Run one operation at a time. Store the information needed for restoration in manifests, keep completed job records available for queries within the process, and optionally write start and completion records to logs.
- Restore a backup into a separate empty directory and compare relative paths, entry types, and contents. Report clear reasons for failures or interruptions while preserving source files and existing successful backups.

File operations, path validation, and consistency checks after abnormal exits are not yet implemented. Following the pair-programming agreement, the next steps are to implement backup and restore file operations incrementally and integrate manifest writing. Path validation must still reject overlapping source, repository, and restore destination paths within an operation. Each step should define its inputs, outputs, and failure behavior and be verified before proceeding.

## Documentation

- [C1 System Context](docs/architecture/system-context.md)
- [C2 Containers](docs/architecture/containers.md)

## TODOs

- Service restart: design this later; no corresponding endpoint or control flow is currently retained.
- Web console: implement after the core backup and restore workflow is complete, controlled by `web_ui.enabled`.
- Permissions, modification times, symbolic links, and other advanced restore capabilities.
- Scheduled jobs, incremental backups, retention policies, file filtering, and repository encryption. Consider WebDAV / S3 after the local workflow is stable. Mobile clients, real-time synchronization, and multi-user collaboration are outside the current scope.

## License

[LICENSE](LICENSE).
