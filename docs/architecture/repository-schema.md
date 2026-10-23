# Repository schema v2

The v2 model and SQL read/write bindings persist root and entry metadata,
symbolic link targets, and statistics in one transaction. Backup collects these
attributes before reading contents or descending into directories. Symbolic links
have no payload in the repository: their original target is stored only in SQLite.
Unsupported entry types are skipped and counted. Filtering is not implemented yet.

Ordinary file/directory content restore uses the v2 model. Applying saved metadata
and recreating symbolic links remain unfinished; the current symlink restore branch
still attempts to open a nonexistent payload. Passing content round-trip tests does
not establish metadata or symlink restore support.

Repository databases require `PRAGMA user_version = 2`; no migration is provided.
Use a new repository for v2. Job queue tables remain at schema version 1 because
their structure has not changed. Job request/result JSON changes are separate work.

## Storage contract

`backups` stores the header, source root metadata, and final statistics.
`entries` stores regular files, directories, and symbolic links. All metadata and
statistics columns are required, with no fabricated defaults.

| Columns | Representation |
| --- | --- |
| `format_version` | Must be 2 |
| `root_uid`, `root_gid`; entry `uid`, `gid` | Numeric IDs, 0 through 4294967295 |
| `root_mode`; entry `mode` | Permission and special bits only, 0 through 4095 (octal 07777), without file type bits |
| `root_atime_seconds`, `root_mtime_seconds`; entry `atime_seconds`, `mtime_seconds` | Signed Unix seconds |
| Corresponding `*_nanoseconds` | 0 through 999999999 |
| `entries_created` | Final entry count, excluding the source root |
| `skipped_unsupported`, `skipped_filtered` | Nonnegative, disjoint skip counts |
| `path` | JSON-serialized relative `PathBuf`, as in the existing storage convention |
| `link_target` | JSON-serialized original target `PathBuf`; do not resolve or normalize |

Each entry belongs to an existing backup; ordinals and paths are unique within a
backup. File entries require nonnegative `original_size` and a `checksum`, with
no `link_target`. Directory entries have none of these three fields. Symlink
entries require only `link_target` and have no compressed payload file.

The application must still validate safe relative paths, unique normalized paths,
parent directory types, absence of descendants beneath symbolic links, algorithm
names, and checksums. SQL text uniqueness does not establish path equivalence.
`entries_created` must equal the number of inserted entries; validate this in the
application before committing the existing single publication transaction. Root
metadata, entries, and counts must all be bound in that transaction.

## Verification and remaining work

Run `cargo test jobs::backup::tests` for traversal, content integrity, root and entry
metadata, metadata-only links (including dangling and cyclic links), unsupported
type statistics, empty sources, and atomic metadata publication. The full test suite
also exercises ordinary file/directory restore and HTTP lifecycle behavior.

- Implement metadata and symlink restoration, including no-follow operations and
  rejection of descendants beneath symlink entries before writing output.
- Add filtering and expose backup statistics through job results and the UI as needed.
- Add restore tests for metadata, links, ownership failures, and restrictive directory
  permissions when those behaviors are implemented.

The schema rejects old databases before querying their tables. It does not delete
or upgrade any existing database or backup contents.
