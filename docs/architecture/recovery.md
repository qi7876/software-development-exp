# Startup recovery

File publication and SQLite outcome commits are separate durability boundaries.
Startup reconciles `running` jobs while holding the working-directory lock,
before HTTP, worker, and scheduler startup. A backup's job UUID identifies its
expected `<job_id>.bak` in the recorded repository.

| Interrupted state | Startup action |
| --- | --- |
| Expected final file exists and verifies | Synchronize file and directory, then commit `succeeded` |
| Final file is absent and exactly one matching regular `.partial` verifies | Synchronize the file, publish it with an exclusive hard link, synchronize the directory, then commit `succeeded` |
| Backup output is missing, corrupt, mismatched, inaccessible, or ambiguous | Commit `failed` with code 500 and keep the files |
| Interrupted restore | Commit `failed` with code 500 and keep the destination |
| Running job cannot be decoded or its outcome cannot be committed | Fail startup and leave the job for the next attempt |

## Verification and cleanup

A candidate must match the job ID and recorded compression and encryption
settings. Recovery reads v9 and v10 archives, checks entry headers and sizes,
consumes the codec and AEAD end markers, and verifies the container digest.
Encrypted backups require the original key ID and fingerprint. Verification
reads the backup file independently of the source directory.

Temporary candidates are regular files named `.<job_id>.<UUID>.partial`.
After durable publication, recovery removes matching temporary links that share
the verified file's device and inode. Cleanup errors are logged. A crash after
publication leaves the final archive available for verification on the next
start.

`finished_at` records the reconciliation time. Results appear in the startup
log and job history. The managed launcher reports the starting PID and log path
while waiting for reconciliation and readiness. Queued tasks then resume in
FIFO order.
