# Startup recovery and reconciliation

File publication and SQLite task completion remain separate commit boundaries.
Startup closes the normal crash gap with verification and idempotent state repair;
it does not create a transaction spanning SQLite and the filesystem.

## Durable identity and scope

New backup executions use the already persisted job UUID as their backup ID.
This establishes the expected `<job_id>.bak` before any file work, without adding
a database table or changing job schema v5 or backup format v9. API callers should
continue treating IDs as opaque. Previously completed jobs keep their saved result
IDs. Queued jobs admitted by older versions acquire the new identity when executed.

The working-directory process lock is held before configuration/key loading and
`JobManager::open`. Before binding HTTP, starting workers, or initializing Cron
cursors, reconciliation reads only jobs left `running`. It does not scan completed
history, audit every repository, infer ownership by timestamps, import unknown
backups into job history, or replay interrupted operations. Legacy running backups
whose independently generated IDs cannot be correlated are failed conservatively;
their files remain untouched.

## Decisions

| Interrupted state | Startup action |
| --- | --- |
| Expected final file exists and verifies | Synchronize file/directory, then commit `succeeded` with the backup ID |
| No final file; exactly one matching regular `.partial` file verifies | Synchronize it, publish with an exclusive hard link, synchronize the directory, then commit `succeeded` |
| Missing, corrupt, mismatched, unverifiable, or ambiguous backup | Commit `failed`/500 with an interruption/reconciliation diagnostic; preserve the files |
| Interrupted restore | Commit `failed`/500; preserve the destination and do not replay |
| Cannot decode the running job or commit its outcome | Fail startup; retain the durable running state for a later attempt |

A candidate must carry the expected ID and match the persisted compression and
encryption settings. Verification consumes the entire bounded payload without
creating output: archive headers and file sizes, codec completion, AEAD final
blocks, and the SHA-256 container digest must all pass. Encrypted recovery requires
the saved key ID/fingerprint to remain available. Missing/wrong keys or inaccessible
files cannot be claimed successful; the task fails with a diagnostic and its files
remain available for later manual inspection/restoration. The source directory
is not needed.

Temporary candidates must match `.<job_id>.<UUID>.partial` and be regular files;
symlinks and unrelated names are not followed. Multiple candidates are not guessed
between. An invalid final file is never overwritten using a temporary candidate.
After durable publication, only temporary paths confirmed to refer to the same
inode as the verified file are removed. Independent copies and unconfirmed files
are retained. Cleanup errors are logged without invalidating a durable backup.

## Ordering and retries

Verification → file sync → optional exclusive link → directory sync → best-effort
temporary-link cleanup → SQLite terminal-state commit.

If the process exits or the database commit fails after publication, the next
startup finds the same final file and verifies it again. Existing final names are
never overwritten. A committed terminal result is not rewritten on subsequent
starts. Original request paths, start time, schedule identity, and queued ordering
remain unchanged; `finished_at` records the reconciliation time. Recovery successes
and failures are visible in startup output/server logs and normal job history.

The remaining queued jobs start only after reconciliation finishes. Full decoding
can take substantial time for a large interrupted backup, so background `start`
prints a starting PID/log path and waits for readiness without a fixed 30-second
kill deadline. The child exiting with an initialization error still fails the
launcher. Foreground startup performs the same reconciliation.

## Verification

Tests cover all nine storage modes, publication from complete temporaries, a missing
source, repeated startup, incomplete/corrupt/ambiguous/unrelated files, missing
keys, same-inode-only cleanup, database-commit failure and retry, and preservation
of interrupted restore output. A real HTTP process test holds SQLite completion
with a writer lock, waits for final-file publication, kills the process, and checks
startup reconciliation plus continued FIFO execution.
