# C2 Containers

`bak` runs as a single binary with an HTTP service and uses the local file system for configuration, backup repositories, restore destinations, and optional job logs.

The HTTP service uses Rouille with one request worker. It processes requests sequentially, and backup and restore execute directly within the request. Status and job queries wait while an operation runs. Rouille manages its networking threads internally; there are no application job threads or concurrent operations.

The binary contains these modules:

- `args`: parse command-line arguments.
- `config`: load and validate configuration.
- `api`: handle routing, authentication, input validation, and responses.
- `jobs`: execute operations synchronously, retain completed results, and append optional logs.
- `backup`: execute and list backups; currently file-operation stubs plus manifest support.
- `restore`: restore backups; currently a file-operation stub.

`main` loads configuration, opens the optional log, and passes ownership of the job manager to the HTTP service. The job manager owns an ordinary map and log file. A single mutex at the HTTP handler boundary satisfies Rouille's `Send + Sync` handler requirement; job execution and logging need no shared ownership or locks.

```text
Users / automation scripts
          |
          | HTTP + Bearer authentication
          v
         api --> jobs --> backup / restore
                   |             |
                   v             v
               Job logs     Local file system
```

Backup and restore responses contain the final result and a job ID. Successful operations return HTTP 200; failed operations return their error status. Completed results remain queryable until process exit. Cancellation and active-job tracking are removed. Start and completion logs retain evidence of attempts interrupted by process exit.
