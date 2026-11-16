# Local MVP and follow-up priorities

The MVP is a single-user backup service: connect from the browser console, back up an existing source directory to an existing independent repository, browse completed backups, restore into an existing empty directory, and inspect persisted task results. The console can select local or remote HTTP/HTTPS APIs and persists connection profiles and keys in browser storage. Switching verifies the target and clears old service data. Backup and restore execution currently require Unix. The console supports wide desktop windows of at least 1200 CSS pixels; paths always refer to the selected service device. Frontend assets are deployed separately and served from `web_ui.path`; there is no runtime asset download.

The current format is self-contained v9: one encoded archive with its codec settings and metadata, without a repository database. Zstd/LZ77 compression and XChaCha20-Poly1305 content encryption are independent per-backup choices; encrypted backups require named server keys. Hybrid small-file packs remain future work. Incremental backup is not planned.

## Reliability before broader use

| Priority | Gap and consequence | Next deliverable |
| --- | --- | --- |
| 1 | Source files can change while being read; success does not establish a consistent snapshot. | Detect common file changes and fail visibly; document that metadata checks alone do not provide a multi-file snapshot. |
| 1 | Interrupted backups can leave ignored temporary files; failed restores can leave partial output. | Startup reconciliation now verifies known interrupted backups and completes recoverable output. Add an explicit inspection/cleanup workflow for unconfirmed leftovers and interrupted restores. |
| 1 | Listing scans per-file metadata instead of an index, so cost grows with repository size. | Measure representative large repositories before introducing any optional, rebuildable index. |
| 2 | Integrity is checked during restore and interrupted-backup reconciliation, but there is no whole-repository audit. | Add a read-only repository verification operation that identifies damaged/missing files without a restore destination. |

## Usability after the core loop

Directory entry remains manual because browser file pickers refer to the browser device, not necessarily the server. A server-side directory browser and explicit directory creation could reduce path mistakes; they need a clear filesystem-access boundary. Saved repository shortcuts can follow, scoped to each API connection.

Long-running jobs expose states but not byte/file progress or cancellation. Add observed progress first, then define cancellation semantics and output cleanup. Retrying an uncertain POST can create duplicate jobs; idempotent submission would make reconnect/retry behavior more predictable.

Task history is paginated, while repository backup listing currently returns all summaries. Add repository pagination when real repository sizes justify it. Retention/deletion needs its own safety policy; disk-space preflight and actionable capacity failures are useful before automatic retention.

## Storage and deployment

Implement the documented hybrid pack design only after measuring small-file workloads. Preserve per-file integrity checks, define a new format version, and test truncated/damaged packs as well as ordinary round trips. Individual files within a pack share its damage exposure.

Permissions, modification times, symbolic links, and directory synchronization on non-Unix platforms remain unsupported. Define required preservation semantics before claiming broader platform support. Filtering, file-content encryption and configuration-driven scheduled backups are implemented. Encrypted metadata and remote storage remain later extensions.

The service currently uses one shared Bearer secret and plain HTTP. Non-local deployment requires a deliberate transport/authentication setup; separate users and permission isolation are outside this MVP. Packaging and automated CI should follow the actual supported operating systems and run the unified check command, including browser checks where dependencies are installed.

## Verification

`uv run scripts/check.py` runs the existing Rust and Python checks. After `npm --prefix web ci` and installing Playwright Chromium, `uv run scripts/check.py --web-ui` also runs JavaScript syntax and real-browser checks against a disposable service. An installed Chrome can be selected with `BAK_BROWSER_CHANNEL=chrome`. Node.js and Playwright are development-only dependencies; the shipped executable needs neither.
