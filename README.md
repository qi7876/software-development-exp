# bak

`bak` creates full backups of local directories and restores them on Linux and macOS. The browser console and Bearer-authenticated HTTP API share a persistent task queue. Each backup is a portable `.bak` file.

## Install and run

Download the Linux x86_64 or macOS ARM64 package and `SHA256SUMS` from
[GitHub Releases](https://github.com/qi7876/software-development-exp/releases).
For example, verify and extract the Linux package for v1.0.0.

```sh
shasum -a 256 --ignore-missing -c SHA256SUMS &&
  tar -xzf bak-v1.0.0-linux-x86_64.tar.gz &&
  cd bak-v1.0.0-linux-x86_64 &&
  export PATH="$PWD:$PATH"
```

For a source install, use the Rust toolchain pinned in [rust-toolchain.toml](rust-toolchain.toml),
run `cargo install --locked --path .` from the repository root, and add Cargo's bin directory to `PATH`.

Run these commands from the extracted package or repository root.

```sh
mkdir -p working-directory-example/source working-directory-example/repository working-directory-example/restored
bak keygen --output working-directory-example/backup.key
bak server start -d working-directory-example
```

The [example configuration](working-directory-example/config.json) loads `backup.key` at startup. Generate it once and reuse it on subsequent starts.

1. Open <http://127.0.0.1:8080/>. In **Manage connections**, enter `127.0.0.1:8080` and the example key `example-local-test-key`, then select **Verify and save**.
2. Put files in `working-directory-example/source`. In **Create backup**, enter `source` and `repository`, then submit.
3. In **Task records**, select **Refresh** until the backup completes, then select **View backup**. To browse by path, use **Backup repository** → **Load backups**.
4. Select **Restore**, enter `restored`, and submit. Check the result in **Task records** and the files in `working-directory-example/restored`.

Relative paths resolve from the server's working directory. Use existing, disjoint source and repository directories. Keep the source stable during backup. Restore into an existing empty directory outside the repository. Use a fresh empty destination after a failed restore, which can leave partial output.

## Service and configuration

Use `bak server start --help` for command options, `bak server status` to check the managed instance, and `bak server stop` to stop it. Stop waits for the active job and preserves the queue for restart. For a supervisor, use `bak server start --foreground -d PATH`.

The working directory holds `config.json`, `jobs.sqlite3`, and the managed server log `server.log`. Run one service per working directory on a local filesystem. Restart the service to apply configuration edits. Use the top-level fields listed below.

| Field | Purpose |
| --- | --- |
| `listen` | HTTP listening address, such as `127.0.0.1:8080` |
| `secret_key` | Bearer key for API requests |
| `web_ui.enabled` | Serve the browser console. Defaults to false |
| `web_ui.path` | Console asset directory, required when enabled. Use the repository's `web/` directory or a deployed copy |
| `encryption.keys` | Map of key IDs to key files |
| `scheduled_backups` | IANA timezone and Cron plans |

The service reads console assets on each request. The browser stores connection profiles and Bearer keys in plaintext in origin-scoped `localStorage`. Connection addresses default to HTTP. For remote HTTPS, put the API behind a TLS reverse proxy and enter its HTTPS origin in the console.

### Shell completions

| Shell | Load completions |
| --- | --- |
| Bash | `source <(bak completions bash)` |
| Zsh | `source <(bak completions zsh)` after `compinit` |
| Fish | `bak completions fish \| source` |

For Bash and Zsh, add the command to your shell startup file. For Fish, save the generated script as `bak.fish` in your Fish configuration directory's `completions/` subdirectory.

## Backup settings

Backups preserve regular files, directories, symbolic links, FIFOs, character devices, and block devices. Hard-linked paths share an inode after restore. Unix sockets are skipped and counted. Restoring device nodes requires filesystem support and process privileges.

### Storage

The console defaults to Zstd level 3 and no encryption. Compression runs before encryption. Restore reads the recorded settings.

| Option | Parameters |
| --- | --- |
| Zstd | Level 1–22, default 3 |
| LZ77 | Window 4–1024 KiB, default 64. Search depth 1–256, default 32 |
| XChaCha20-Poly1305 or AES-256-GCM | Configured encryption key ID |

Generate key files with `bak keygen --output PATH` and register them in `encryption.keys`, for example `{"current":"/secure/backup-current.key"}`. Key files contain 64 hexadecimal digits and use mode `0600`. Keep copies of the keys required by existing archives and queued jobs. Assign a new ID when rotating keys. The manifest stores paths and Unix metadata in plaintext.

### Filters

**File types** selects among the six supported types. All are selected by default. When directories are deselected, the backup keeps the parent directories required by selected entries. Each hard-link path is evaluated separately using its filesystem type.

**Filters** combines relative path, UID, GID, mode, and access and modification times. Enter one regular expression per line. All fields must match. Within a field any include can match, and exclusions take precedence. UID and GID use decimal. Mode uses four-digit octal, such as `0644`.

A directory rejected by a path or metadata rule prunes its subtree. To select `.txt` files directly inside `docs`, include both `^docs$` and `^docs/[^/]+\.txt$`. The console interprets times in the browser's timezone. Start bounds are inclusive and end bounds are exclusive.

## Scheduled backups

Set `scheduled_backups` in `config.json`. This example runs daily at 02:00 in Shanghai.

```json
{
  "timezone": "Asia/Shanghai",
  "plans": [
    {"id": "daily-source", "cron": "0 2 * * *", "source": "source", "repository": "repository"}
  ]
}
```

Use unique plan IDs and five-field numeric Cron expressions. Sunday is 0 or 7. Restricted day-of-month and weekday fields match with OR. Plans default to enabled, Zstd level 3, all paths, and no encryption. Optional `filter`, `compression`, and `encryption` use the API shapes below.

Automatic runs skip missed minutes and plans with queued or running work. **Scheduled backups** → **Back up now** submits a plan with its saved settings, including when `enabled` is false. See the [scheduling lifecycle](docs/architecture/scheduling.md).

## HTTP API

Send `Authorization: Bearer <secret_key>`. JSON request bodies use `application/json` and may be up to 2 MiB.

| Endpoint | Purpose |
| --- | --- |
| `GET /api/status` | Service version and available encryption key IDs and fingerprints |
| `POST /api/backups` | Queue a backup with `source`, `repository`, `filter`, and optional storage settings |
| `GET /api/backups?repository=PATH` | List published backups in `{"backups":[...]}` |
| `POST /api/restores` | Queue a restore with `repository`, `backup_id`, and `destination` |
| `GET /api/jobs/{job_id}` | Read a job and its result |
| `GET /api/jobs?limit=20&before=CURSOR` | List jobs newest first, with limits from 1 to 100 |
| `GET /api/schedules` | List the configured timezone and plans |
| `POST /api/schedules/{id}/run` | Queue a plan by its URL-encoded ID |

```sh
curl -i http://127.0.0.1:8080/api/backups \
  -H 'Authorization: Bearer example-local-test-key' \
  -H 'Content-Type: application/json' --data-binary @- <<'JSON'
{
  "source": "source",
  "repository": "repository",
  "compression": {"algorithm": "zstd", "level": 3},
  "filter": {
    "relative_path": {"include": [], "exclude": []},
    "uid": {"include": [], "exclude": []},
    "gid": {"include": [], "exclude": []},
    "mode": {"include": [], "exclude": []},
    "atime": {},
    "mtime": {}
  }
}
JSON
```

- For LZ77 use `"compression":{"algorithm":"lz77","window_kib":64,"search_depth":32}`. Omission or null stores uncompressed data.
- For encryption use `"encryption":{"algorithm":"xchacha20-poly1305","key_id":"current"}` or `aes-256-gcm`. Omission or null stores plaintext data.
- Include all six filter fields shown above. Empty regex arrays leave the field unrestricted. Time fields accept optional RFC 3339 `from` and `before` bounds. The worker validates filter rules during execution.
- Optionally set `filter.file_types` to an array of `regular_file`, `directory`, `symbolic_link`, `fifo`, `character_device`, and `block_device`. Omission or null selects all types. An empty array backs up only the source root's metadata.

A submission returns HTTP 202 and a `Location` header. Poll that URL for `queued`, `running`, `succeeded`, or `failed`. Success returns `result.backup_id` or `result.destination`. Failed jobs include `error.code` and `error.message`. For history pagination, pass `next_cursor` as `before` until it is null. Times use UTC Unix milliseconds. Scheduled jobs include `schedule_id` and `scheduled_for`.

Request errors return the matching HTTP status and `{"error":{"code":400,"message":"..."}}`. Running a plan returns 409 when it is busy, 404 for an unknown ID, or 400 for an unavailable key.

## Repositories and recovery

Copy a `<backup_id>.bak` file to another repository under its original name. For an encrypted backup, configure its original key ID and key material on the destination service. `bak` 1.0 reads and writes v10 archives. To recover data from an earlier archive format, use the release that created it.

The repository filesystem must support hard links and directory synchronization. At startup, the service verifies interrupted backup output and records interrupted restores as failed, preserving their output. See the [archive format](docs/architecture/archive.md) and [startup recovery](docs/architecture/recovery.md).

## Development

Run `uv run scripts/check.py` for Rust and Python checks. Install Node.js 20+ to run the browser checks.

```sh
npm --prefix web ci
(cd web && npx playwright install chromium)
uv run scripts/check.py --web-ui
```

Set `BAK_BROWSER_CHANNEL=chrome` to use installed Chrome. For report checks, run `uv run scripts/check.py --report`. See the [course report guide](docs/report/README.md).

### CI and releases

Pull requests to `main` run the project checks and build both release packages.
In GitHub Actions, select **Release** → **Run workflow** on `main` to build and
download the packages and checksums from the `release-files` artifact. Enable
**publish** to create the GitHub Release and tag the tested commit with the
version in `Cargo.toml`.

## Architecture

- [C1 system context](docs/architecture/system-context.md) and [C2 containers](docs/architecture/containers.md)
- [Repository manifest](docs/architecture/repository-schema.md), [encryption](docs/architecture/content-encryption.md), and [LZ77](docs/architecture/lz77.md)
- [Follow-up priorities](docs/mvp.md)

[MIT license](LICENSE)
