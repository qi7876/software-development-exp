# Repository schema v2

This is a breaking, SQL-first change. The schema is installed, but the Rust
manifest model and its read/write bindings still use format v1. Backup publication
and the existing backup/restore tests will fail until that implementation is
updated. The SQL tests can run independently:

```sh
cargo test jobs::storage::tests
cargo test v2_schema_enforces_metadata_and_entry_types
```

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

## Remaining Rust work

- Extend `Manifest` with required root metadata and statistics, and extend entries
  with metadata and the symlink target variant.
- Update `Manifest::read` and `Manifest::write` to read/bind the new columns; existing
  inserts intentionally fail rather than silently inventing source metadata.
- Switch manifest validation, backup generation, and restore validation to format 2.
- Add metadata/link collection and restoration, statistics, filtering, and API fields.
- Update existing fixtures and round-trip tests to supply the required v2 fields.

The schema rejects old databases before querying their tables. It does not delete
or upgrade any existing database or backup contents.
