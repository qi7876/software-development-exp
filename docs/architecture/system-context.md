# C1 System Context

`bak` provides local backups and restores on Linux and macOS. Individual users
work in the browser console, and scripts use the same Bearer-authenticated HTTP
API. Clients submit tasks and read their results by job ID.

```text
Users and scripts <-- HTTP --> bak
                                |
                                +--> Configuration and key files
                                +--> SQLite task queue and history
                                +--> Source, backup and restore directories
```

Paths refer to the selected server. Each service owns its configuration, queue,
and filesystem paths. Deploy one instance per working directory and place the
task database on a local filesystem. Each backup is a portable `.bak` file.
Encrypted backups also require their configured key.

See [C2 Containers](containers.md) for the runtime and storage boundaries.
