# Sequential archive format v2

Repository format/schema v8 stores one `<backup_id>.bak` per
backup. This project-defined format replaces the mirrored source tree; it does
not implement tar compatibility or invoke an external packer. SQLite remains the
publication authority and catalog. Restoration requires both the catalog and
archive; root metadata and backup settings are stored only in SQLite.

## Pipeline and tradeoffs

Backup collects filtered entries and Unix metadata before reading file data,
preserving the existing traversal and directory pruning behavior. It then writes:

```text
entry headers + file data + end marker
    → optional Zstd or project LZ77
    → optional XChaCha20-Poly1305 or AES-256-GCM STREAM
    → SHA-256 during writing
    → <backup_id>.bak
```

Packing reads each file once and uses a 128 KiB copy buffer plus bounded header,
I/O, codec and encryption buffers. Collected entry metadata still occupies O(n)
memory. Directories and symbolic links are header-only records: packing never
opens link targets or creates their mirrored paths in the repository. Compression
history spans entries; only the final archive needs file synchronization.

There is one SHA-256 per backup, covering the exact final `.bak` bytes after
compression and encryption, including encoding headers and final authentication
tags. The digest is stored as lowercase hexadecimal in SQLite's backup header.
Backup hashes accepted output bytes while writing, and restore hashes raw input
bytes while reading, so neither operation needs an additional pass over the
archive. A checksum mismatch identifies a damaged archive rather than an individual file. Per-file hashing and checksum trailers are removed. Compression can
reduce the number of bytes hashed; no runtime speedup is assumed without a
representative measurement.

Whole-stream encoding makes full restoration sequential. There is no random
access index or selective extraction API. A damaged stream can prevent restoration
of later records. Source trees must remain unchanged: the packer detects a file
shrinking or growing during its copy, but does not provide snapshot consistency
or detect every concurrent edit.

## Wire layout before encoding

All integer fields below are unsigned little-endian. There is no block padding.

| Field | Representation |
| --- | --- |
| Magic/version | 8 bytes `b"BAKARC\0\x02"` |
| Entry header length | u32, 1 through 1,048,576 |
| Entry header | Exactly that many bytes of UTF-8 JSON |
| File data | Exactly `original_size` bytes; files only |
| Further entries | Repeat header length, header, optional file data |
| End marker | u32 zero, followed by decoded EOF |

A header has `relative_path`, `metadata` and `kind`. Paths use the existing serde
`PathBuf` representation; non-UTF-8 paths are not supported. Metadata contains
`uid`, `gid`, `mode`, `atime` and `mtime` as described in the
[repository schema](repository-schema.md). Kind is one of:

```json
{"type":"File","original_size":123}
{"type":"Directory"}
{"type":"Symlink","target":"../original-target"}
```

There are no per-entry checksums. Header lengths are checked before allocation or
writing, and unknown header fields/types are errors.

## Publication and restore

The packer creates `<backup_id>.bak` exclusively at the repository root. No
archive subdirectory is created.
It finishes archive framing, compression and encryption in that order, synchronizes
the archive and repository root, and only then commits the SQLite header,
archive digest, ordered entries and statistics in one transaction. Duplicate IDs do not overwrite
existing archives. Failed publication can leave an unpublished archive; listing
continues to read only committed SQLite records. Automatic cleanup is not provided.

Restore validates all catalog paths before creating entries and validates each
archive header against the corresponding catalog path, metadata and kind before
creating that entry. It copies exactly the declared file size. It requires
the explicit end marker, rejects extra decoded bytes or records, and finishes the
outer codec/authentication reader, then compares the final raw-input digest with
SQLite's `archive_checksum` before success. Saved directory attributes are
applied after children and root attributes last. Partial output can remain after
a failed restore. Repositories remain trusted; this is not a sandbox for extracting
adversarially constructed archives or modified catalogs.

The encryption AAD uses `<backup_id>.bak` as the path, together with the existing
backup ID, compression configuration and key fingerprint bindings. An encrypted
archive covers its headers as well as file data; the separate SQLite catalog
still exposes paths, metadata, file sizes, the archive checksum and link targets. See
[content encryption](content-encryption.md).

Repository schemas through v7, including the previous `backups/` layout, are rejected without migration or deletion. Use
a new repository for v8 and the prior binary to operate old repositories. The
HTTP request contract and job schema v5 are unchanged.
