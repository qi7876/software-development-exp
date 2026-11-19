# Scheduled backup lifecycle

The scheduler loads `scheduled_backups` from configuration. It validates plan
structure, Cron, filters, and compression at startup, and resolves encryption
keys for enabled plans. The plan catalog can be queried through the HTTP API.
Restart the service to apply edits.

The scheduler checks UTC wall time once per second and evaluates the current
minute in the configured IANA timezone. Startup initializes cursors at least to
the current minute. Forward jumps skip elapsed minutes and backward jumps
respect the persisted high-water mark. Repeated local times during a daylight
saving transition can trigger twice, subject to the one-active-job limit.

## Admission and persistence

Job schema v5 stores per-plan cursors as UTC Unix minutes and records
`schedule_id` and `scheduled_for` on scheduled jobs. An automatic trigger advances
the cursor and conditionally queues the job in one SQLite transaction, even when
the plan is busy. A partial unique index enforces one queued or running job per
plan. Notification follows commit.

`POST /api/schedules/{id}/run` queues the saved request, including for disabled
automatic plans. It uses the request time for `scheduled_for`, shares the
active-job constraint, and leaves the Cron cursor in place. Responses are
202 with Location for admission, 409 for busy plans, 404 for unknown IDs, and
400 for an unavailable encryption key. The API represents `scheduled_for` in
UTC Unix milliseconds.

Queued requests retain a snapshot of the settings and key fingerprint. Plan
edits affect future submissions. Committed jobs survive shutdown in the shared
FIFO queue. The service supervisor reports scheduler errors and stops the
process.
