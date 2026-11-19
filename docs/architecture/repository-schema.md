# Repository metadata v10

Each published `<backup_id>.bak` contains its own manifest and digest. See the
[container contract](archive.md) for framing and publication.

## Manifest invariants

| Field | Meaning |
| --- | --- |
| `format_version` | 10 for new archives; v9 is also readable |
| `backup_id` | Non-empty identifier with no path separators; matches the filename |
| `created_at` | UTC RFC 3339 timestamp for generated backups |
| `source` | Original absolute source path |
| `compression` | Null or complete Zstd/LZ77 configuration |
| `encryption` | Null or algorithm, key ID, and 64-hex-character fingerprint |
| `checksum_algorithm` | `sha256`; raw digest stored at the end of the file |
| `root_metadata` | Source root's Unix metadata |
| `backup_stats` | Entry count and nonnegative filtered/unsupported skip counts |
| `entries` | Ordered metadata for all archived paths |

Unix metadata holds uid/gid as u32, permission mode in 0–4095 (`07777`),
and accessed/modified times as signed Unix seconds plus nanoseconds in
0–999999999. Socket entries are skipped and counted. Root/entry metadata
is collected before reading contents or descending into directories.

Paths are unique, non-empty, and relative, with normal components. Each parent
must already appear as a directory. The entry count
must match `entries_created`. JSON path strings require UTF-8.

| Entry kind | Additional fields |
| --- | --- |
| `File` | `original_size` as a u64, followed by that many bytes in the archive stream |
| `Directory` | None |
| `Symlink` | Unresolved `target` path |
| `Fifo` | None |
| `CharacterDevice`, `BlockDevice` | `major` and `minor` device numbers as u32 |
| `HardLink` | `target` is the relative path of a preceding original non-directory entry |

The first selected path for a given `(st_dev, st_ino)` of a non-directory entry
stores its original kind and attributes. Each alias stores the same attributes and
references that path directly. The restored aliases share the original's inode.
Version 9 accepts `File`, `Directory`, and `Symlink` entries.

Restore compares the decoded archive headers with this manifest before writing
each entry. Files are created exclusively in an existing empty destination outside
the repository. Directory metadata is restored after children, and source-root
metadata last, using open handles for synchronization.
