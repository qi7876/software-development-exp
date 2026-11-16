# Repository metadata v9

The repository no longer has a SQL schema. Each published `<backup_id>.bak`
contains its own manifest and digest. See the [container contract](archive.md)
for layout, publication, compatibility, checksum scope, and listing tradeoffs.
`jobs.sqlite3` remains the separate durable task queue/history, at schema v5.

## Manifest invariants

| Field | Meaning |
| --- | --- |
| `format_version` | 9 |
| `backup_id` | Non-empty identifier with no path separators; matches the filename |
| `created_at` | UTC RFC 3339 timestamp for generated backups |
| `source` | Original absolute source path |
| `compression` | Null or complete Zstd/LZ77 configuration |
| `encryption` | Null or algorithm, key ID, and 64-hex-character fingerprint |
| `checksum_algorithm` | `sha256`; raw digest stored at the end of the file |
| `root_metadata` | Source root's Unix metadata |
| `backup_stats` | Entry count and nonnegative filtered/unsupported skip counts |
| `entries` | Ordered file, directory, and symbolic-link metadata |

Unix metadata holds uid/gid as u32, mode in 0–4095 (`07777`, without type bits),
and accessed/modified times as signed Unix seconds plus nanoseconds in
0–999999999. Unsupported entry types are skipped and counted. Root/entry metadata
is collected before reading contents or descending into directories.

Paths must be non-empty relative paths composed of normal components. Paths
cannot repeat. Each parent must already appear as a directory, preventing entries
under files or symbolic links. The entry count must match `entries_created`.
A file has nonnegative u64 `original_size`; a symlink retains its original target
without resolving it; a directory has no payload. JSON path strings require UTF-8.

Restore compares the decoded archive headers with this manifest before writing
each entry. Files are created exclusively in an existing empty destination outside
the repository. Directory metadata is restored after children, and source-root
metadata last, using open handles for synchronization even with restrictive modes.

## Verification

Repository tests exercise single-file portability, exclusive publication,
abandoned/failed output, metadata validation, malformed lengths/versions, listing
order, and preservation of legacy databases. Backup/restore tests retain Unix
attributes, links, empty directories, filtering, exact sizes, checksum failures,
and path validation. HTTP tests copy only the `.bak` file into a new repository
and list/restore all nine compression/encryption combinations with external keys.
