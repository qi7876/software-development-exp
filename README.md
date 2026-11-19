# bak

`bak` backs up local directories and restores them on Linux and macOS. A Rust service exposes a browser console and an authenticated HTTP API. Backup and restore tasks run in a persistent queue.

## Quick start

Use the Rust toolchain in `rust-toolchain.toml`. Run these commands from the repository root.

```sh
mkdir -p working-directory-example/source working-directory-example/repository working-directory-example/restored
cargo run --locked -- keygen --output working-directory-example/backup.key
cargo run --locked -- server start -d working-directory-example
```

Open <http://127.0.0.1:8080/>. In **Manage connections**, enter `127.0.0.1:8080` and the example key `example-local-test-key`, then select **Verify and save**.

1. Put files in `working-directory-example/source`. In **Create backup**, enter `source` and `repository`, choose storage settings, and submit.
2. In **Task records**, select **Refresh** to check the result. **View backup** opens the repository. To browse a repository by path, use **Backup repository** → **Load backups**.
3. Select **Restore** on a backup, enter `restored`, and submit. Check the task and the restored files.

Paths refer to the selected server. Relative paths resolve from its working directory. Use existing source and repository directories with disjoint paths, and restore into an existing empty directory outside the repository. Keep the source tree stable during backup. Failed restores may leave partial output. Use a fresh empty destination for a retry.

Backups preserve regular files, directories, symbolic links, FIFOs, character devices, block devices, and hard links. Each hard-linked regular-file inode contributes one payload, and restore recreates the shared inode. Unix sockets are skipped and counted. Device restoration requires permission to create nodes on the destination filesystem.

### Shell completions

With `bak` on your `PATH`, load the script for your current shell.

#### Bash

```bash
bak completions bash > "$HOME/.bak-completion.bash"
source "$HOME/.bak-completion.bash"
```

For future sessions, add the `source` line to your Bash startup file.

#### Zsh

Run `source <(bak completions zsh)` after `compinit`. If your Zsh setup does not already initialize completions, run `autoload -Uz compinit` followed by `compinit`. For future sessions, add the `source` line after `compinit` in `.zshrc`.

#### Fish

Run `bak completions fish | source` to load completions in the current session. Fish automatically loads `bak.fish` from your user completions directory in future sessions.

```fish
set -l fish_config ~/.config/fish
if set -q XDG_CONFIG_HOME[1]; and test -n "$XDG_CONFIG_HOME"
    set fish_config "$XDG_CONFIG_HOME/fish"
end
mkdir -p "$fish_config/completions"
bak completions fish > "$fish_config/completions/bak.fish"
```

## Configuration and service

The directory passed to `server start -d` contains `config.json`, `jobs.sqlite3`, and the background log `server.log`. Run one service per working directory on a local filesystem. Restart the service after editing configuration.

| Field | Purpose |
| --- | --- |
| `listen` | HTTP address and port, such as `127.0.0.1:8080` |
| `secret_key` | Bearer key for API clients |
| `web_ui.enabled` | Serve the browser console. Defaults to false |
| `web_ui.path` | Required when the console is enabled. Directory containing `index.html`, `app.js`, `styles.css`, and `favicon.svg` |
| `encryption.keys` | Optional map of key IDs to key files |
| `scheduled_backups` | Optional Cron plans and timezone |

Deploy the `web/` directory separately and point `web_ui.path` to it. The service reads the assets on each request. Browser connection profiles and Bearer keys are stored in plaintext in origin-scoped `localStorage`. The console accepts a reachable HTTP or HTTPS origin and defaults to HTTP when the scheme is omitted. Use an HTTPS reverse proxy for remote TLS and configure the remote API with an HTTPS origin when serving the console over HTTPS.

```sh
cargo build --release --locked
target/release/bak help server start
target/release/bak server status
target/release/bak server stop
```

`server start` waits for the managed background process to become ready. Check startup errors in `server.log`. Use `server start --foreground -d PATH` under a process supervisor. Managed `status` and `stop` use the current user's control socket under `$XDG_STATE_HOME/bak` or `$HOME/.local/state/bak`. A graceful stop lets the active job finish and keeps queued jobs for the next start.

## Backup settings

### Compression and encryption

The console defaults to Zstd level 3 with encryption set to None. When both options are enabled, the archive is compressed before encryption. Restore uses the saved settings.

| Option | Parameters |
| --- | --- |
| Zstd | Level 1–22, default 3 |
| LZ77 | Window 4–1024 KiB, default 64. Search depth 1–256, default 32 |
| XChaCha20-Poly1305 or AES-256-GCM | Configured encryption key ID |

Generate a key with `bak keygen --output /secure/backup-current.key` and add its path under `encryption.keys`, for example `{"current":"/secure/backup-current.key"}`. Key files contain 64 hexadecimal digits and are created with mode `0600`. Keep independent copies of these files. Assign a new ID when rotating keys and retain the original keys for existing backups and queued jobs. Restore requires the recorded key ID and fingerprint. The manifest remains plaintext, including paths, Unix metadata, link targets, and device numbers.

### Filters

Under **File types**, select any combination of regular files, directories, symbolic links, FIFOs, character devices, and block devices. All six are selected by default. When directories are deselected, the backup still includes the parents needed to restore selected entries. Hard links follow their underlying file type.

The **Filters** form matches relative path, UID, GID, mode, modification time, and access time. Enter one regular expression per line. Fields combine with AND, includes within a field combine with OR, and exclusions take precedence. Paths are relative to the source, UID/GID are decimal, and mode uses four-digit octal such as `0644`.

Each hard-link path is evaluated separately. A rejected directory prunes its subtree, so include patterns must allow ancestors. To select `.txt` files directly inside `docs`, include both `^docs$` and `^docs/[^/]+\.txt$`. The console interprets date/time fields in the browser timezone. Start times are inclusive and end times are exclusive. Invalid filters appear as failed tasks.

## Scheduled backups

Add this member to `config.json` to run daily at 02:00 in Shanghai.

```json
{
  "scheduled_backups": {
    "timezone": "Asia/Shanghai",
    "plans": [{
      "id": "daily-source",
      "cron": "0 2 * * *",
      "source": "source",
      "repository": "repository"
    }]
  }
}
```

Use stable, unique plan IDs, an IANA timezone, and five-field numeric Cron expressions. Sunday is 0 or 7. Restricted day-of-month and weekday fields match with OR. Plans default to enabled, Zstd level 3, all paths, and encryption set to None. Optional `filter`, `compression`, and `encryption` use the API shapes below.

The scheduler evaluates the current minute while the service runs and skips plans with queued or running work. In **Scheduled backups**, select **Back up now** to queue the saved settings, including for a plan with `enabled: false`. Configure the required encryption key before running a plan. See the [scheduling design](docs/architecture/scheduling.md).

## HTTP API

Send `Authorization: Bearer <secret_key>` to the API. Use `application/json` for request bodies, up to 2 MiB.

| Method and path | Purpose |
| --- | --- |
| `GET /api/status` | Service version and encryption key IDs and fingerprints |
| `POST /api/backups` | Queue a backup with `source`, `repository`, `filter`, and optional storage settings |
| `GET /api/backups?repository=PATH` | List published backups in `{"backups":[...]}` |
| `POST /api/restores` | Queue a restore with `repository`, `backup_id`, and `destination` |
| `GET /api/jobs/{job_id}` | Read a job and its result |
| `GET /api/jobs?limit=20&before=CURSOR` | List jobs newest first. Limits range from 1 to 100 |
| `GET /api/schedules` | List the configured timezone and plans |
| `POST /api/schedules/{id}/run` | Queue a configured plan by its URL-encoded ID |

```sh
curl -i -H 'Authorization: Bearer example-local-test-key' \
  -H 'Content-Type: application/json' \
  -d '{"source":"source","repository":"repository","compression":{"algorithm":"zstd","level":3},"filter":{"relative_path":{"include":[],"exclude":[]},"uid":{"include":[],"exclude":[]},"gid":{"include":[],"exclude":[]},"mode":{"include":[],"exclude":[]},"atime":{},"mtime":{}}}' \
  http://127.0.0.1:8080/api/backups
```

Accepted submissions return HTTP 202 and a `Location` header. Poll that URL with the same Bearer key to follow `queued`, `running`, `succeeded`, or `failed`. Successful jobs return `result.backup_id` or `result.destination`. Job failures return `error.code` and `error.message` in the job record. Check task history before retrying a submission whose response was lost. To paginate, pass `next_cursor` as `before` until the cursor is null. History includes the original request and execution times in UTC Unix milliseconds.

Request errors return `{"error":{"code":400,"message":"..."}}` with the matching HTTP status. Common statuses are 400 for invalid input, 401 for authentication errors, 413 for oversized bodies, and 415 for unsupported media types.

- Set `compression` to `{"algorithm":"zstd","level":3}` or `{"algorithm":"lz77","window_kib":64,"search_depth":32}`. Omit the field or use null for uncompressed data.
- Set `encryption` to `{"algorithm":"xchacha20-poly1305","key_id":"current"}` or use `aes-256-gcm` as the algorithm. Omit the field or use null to store plaintext data.
- Supply the six filter fields shown in the example. Regex fields contain `include` and `exclude` arrays. Empty regex arrays leave the field unrestricted. Time fields accept optional RFC 3339 `from` and `before` bounds. The worker validates regexes and time bounds during execution.
- Use the optional `filter.file_types` array with `regular_file`, `directory`, `symbolic_link`, `fifo`, `character_device`, and `block_device`. Omit it or use `null` to select all types. An empty array produces a backup with root metadata alone.
- Scheduled jobs include `schedule_id` and a `scheduled_for` timestamp in UTC Unix milliseconds. A busy plan returns HTTP 409, an unknown plan returns 404, and an unavailable key returns 400.

## Repositories and recovery

Each `<backup_id>.bak` holds its archive data, manifest, and SHA-256 digest. Copy the file with its original name to another repository to restore it there. Keep any key required by the backup. New archives use v10, and v9 archives can share the same repository. Formats through v8 require their original application version and repository database.

The repository filesystem must support hard links and directory synchronization. Startup verifies interrupted backup output before resolving its task. Interrupted restores are recorded as failed and their output is available for inspection. See the [archive contract](docs/architecture/archive.md) and [startup recovery](docs/architecture/recovery.md).

## Development

```sh
uv run scripts/check.py
```

Run browser checks with Node.js 20+ and Playwright Chromium.

```sh
npm --prefix web ci
(cd web && npx playwright install chromium)
uv run scripts/check.py --web-ui
```

Installed Chrome can be selected with `BAK_BROWSER_CHANNEL=chrome`. Run report and diagram checks with `uv run scripts/check.py --report`. The [course report guide](docs/report/README.md) covers report generation and performance measurements.

## Architecture

- [C1 system context](docs/architecture/system-context.md) and [C2 containers](docs/architecture/containers.md)
- [Repository manifest](docs/architecture/repository-schema.md), [encryption](docs/architecture/content-encryption.md), and [LZ77](docs/architecture/lz77.md)
- [Local workflow and follow-up priorities](docs/mvp.md)

## License

[LICENSE](LICENSE)
