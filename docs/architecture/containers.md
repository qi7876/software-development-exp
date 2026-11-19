# C2 Containers

```text
Browser console / scripts
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
     Source, .bak repository and restore destination
```

## Service and console

The Rust binary runs axum on Tokio. HTTP handlers commit jobs to SQLite before
returning HTTP 202 and a Location header. The supervised worker claims queued
jobs in insertion order and executes one blocking backup or restore at a time.
A database constraint enforces the single running job. Tokio notifications wake
the worker, and SQLite stores requests, states, and outcomes.

The service reads console assets from `web_ui.path` on each request. The browser
calls the selected API directly and stores connection profiles and plaintext
Bearer keys in origin-scoped `localStorage`. Tower HTTP handles CORS preflights.
The history API uses string cursors to preserve SQLite sequence precision in
JavaScript. The console loads history after connecting or submitting a job, then
updates it on explicit refresh.

## Storage

`jobs.sqlite3` lives in the working directory. It uses WAL, `synchronous=FULL`,
foreign keys, a five-second busy timeout, and at most four connections. The
worker claims and completes jobs in SQLite transactions.

The repository contains one self-contained `.bak` file per backup. The backup
worker streams entries through the selected codecs, synchronizes the temporary
file, and publishes it with an exclusive hard link. Restore validates the
manifest and streams the payload from the same open archive descriptor. The
archive and job outcome have separate durability boundaries. See the [archive
contract](archive.md) and [startup recovery](recovery.md).

The service loads named key files into an in-memory keyring shared with the
worker. Tasks and archives record key IDs and fingerprints. The scheduler stores
per-plan cursors in SQLite and queues work through the same job manager. See
[content encryption](content-encryption.md) and [scheduling](scheduling.md).

## Lifecycle

The managed launcher waits for the child to report readiness on a local Unix
socket. A per-user lock guards the background instance and a working-directory
lock guards the task database. The child appends output to `server.log`.

Startup reconciles interrupted jobs before serving requests. Ctrl-C, SIGTERM,
and `server stop` stop new admission and claims, finish the active job, and
preserve the queue for restart. The supervisor exits with failure if a service
task terminates unexpectedly or the worker fails to persist an outcome.
