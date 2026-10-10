# C1 System Context

`bak` is a cross-platform file backup manager for individual users, intended to create reliable local backups and restore them into a separate directory.

Individual users and automation scripts access its HTTP API through clients such as curl, authenticated by a Bearer secret key. Users submit backup and restore requests, receive a durable job ID immediately, and query outcomes while background execution proceeds. A web console is planned.

The service reads local configuration and persists its job queue and history in a SQLite database in the working directory. Repository metadata support stores the information needed for identification, verification, and restoration in a SQLite database at each local repository root. File contents remain in the filesystem. Backup, listing, and restore file operations are not yet implemented.

```text
Users / automation scripts <-- HTTP --> bak <---> local configuration
                                         |
                                         +----> SQLite job queue and history
                                         |
                                         +----> local repositories and restore destinations
                                                (file operations planned;
                                                 SQLite metadata support implemented)
```

SQLite transactions preserve job admission and metadata records; they do not make external filesystem changes transactional. Deployment currently assumes one service instance per working directory and local filesystems for its SQLite databases.
