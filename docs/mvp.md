# Local MVP and follow-up priorities

The MVP is a single-user service for a trusted local device: connect from the embedded browser console, back up an existing source directory to an existing independent repository, browse completed backups, restore into an existing empty directory, and inspect persisted task results. Backup and restore execution currently require Unix. The console supports desktop and mobile widths; paths always refer to the service device. There is no frontend deployment or runtime asset download.

The current format remains version 1 with individually compressed files and repository metadata in SQLite. Hybrid small-file packs remain future work. Incremental backup is not planned.

## Reliability before broader use

| Priority | Gap and consequence | Next deliverable |
| --- | --- | --- |
| 1 | Source files can change while being read; success does not establish a consistent snapshot. | Detect common file changes and fail visibly; document that metadata checks alone do not provide a multi-file snapshot. |
| 1 | Failed or interrupted backups can leave unpublished content; failed restores can leave partial output. | Define ownership of temporary output, reconcile interrupted work, and safely clean only operation-owned contents. |
| 1 | Repository SQLite metadata is required to locate and verify backups; payloads alone are not self-describing. | Choose redundant metadata or per-backup manifests, and prove recovery from a missing/corrupt index. |
| 2 | Integrity is checked during restore only. | Add a read-only repository verification operation that identifies damaged/missing files without a restore destination. |

## Usability after the core loop

Directory entry remains manual because browser file pickers refer to the browser device, not necessarily the server. A server-side directory browser and explicit directory creation could reduce path mistakes; they need a clear filesystem-access boundary. Saved repository shortcuts can follow without storing the access key.

Long-running jobs expose states but not byte/file progress or cancellation. Add observed progress first, then define cancellation semantics and output cleanup. Retrying an uncertain POST can create duplicate jobs; idempotent submission would make reconnect/retry behavior more predictable.

Task history is paginated, while repository backup listing currently returns all summaries. Add repository pagination when real repository sizes justify it. Retention/deletion needs its own safety policy; disk-space preflight and actionable capacity failures are useful before automatic retention.

## Storage and deployment

Implement the documented hybrid pack design only after measuring small-file workloads. Preserve per-file integrity checks, define a new format version, and test truncated/damaged packs as well as ordinary round trips. Individual files within a pack share its damage exposure.

Permissions, modification times, symbolic links, and directory synchronization on non-Unix platforms remain unsupported. Define required preservation semantics before claiming broader platform support. Scheduling, filtering, encryption, and remote storage are later extensions.

The service currently uses one shared Bearer secret and plain HTTP. Non-local deployment requires a deliberate transport/authentication setup; separate users and permission isolation are outside this MVP. Packaging and automated CI should follow the actual supported operating systems and run the unified check command, including browser checks where dependencies are installed.

## Verification

`uv run scripts/check.py` runs the existing Rust and Python checks. After `npm --prefix web ci` and installing Playwright Chromium, `uv run scripts/check.py --web-ui` also runs JavaScript syntax and real-browser checks against a disposable service. An installed Chrome can be selected with `BAK_BROWSER_CHANNEL=chrome`. Node.js and Playwright are development-only dependencies; the shipped executable needs neither.
