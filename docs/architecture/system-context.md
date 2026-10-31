# C1 System Context

`bak` is a cross-platform file backup manager for individual users, intended to create reliable local backups and restore them into a separate directory.

Individual users access an optional browser console embedded in the binary; automation scripts use clients such as curl. Both access the same HTTP API, authenticated by a Bearer secret key. Users submit backup and restore requests, receive a durable job ID immediately, and query outcomes while background execution proceeds. The console supports repository listing, restore selection, paginated task history, and switching between local or remote API services. Browser-origin storage persists connection addresses, names, and plain-text keys; the selected service is verified when reopening the page. Each service keeps its own jobs and repositories. Paths always belong to the selected server device.

The service reads local configuration and persists its job queue and history in a SQLite database in the working directory. Full backups store the information needed for identification, verification, and restoration in a SQLite database at each local repository root. Contents, optionally compressed with Zstd or project LZ77 and/or encrypted with XChaCha20-Poly1305, mirror the source tree in a separate directory per backup. Users can list published backup summaries from repository metadata. Backup and restore execution currently support Unix platforms.

```text
Users / automation scripts <-- HTTP --> bak <---> local configuration
                                         |
                                         +----> SQLite job queue and history
                                         |
                                         +----> local repositories and restore destinations
                                                (full backups and restores
                                                 implemented on Unix)
```

SQLite transactions preserve job admission and metadata records; they do not make external filesystem changes transactional. Deployment currently assumes one service instance per working directory and local filesystems for its SQLite databases.
