# Sequential archive format v1

Repository format/schema v6 stores one `backups/<backup_id>/archive.bak` per
backup. This project-defined format replaces the mirrored source tree; it does
not implement tar compatibility or invoke an external packer. SQLite remains the
publication authority and catalog. Restoration requires both the catalog and
archive; root metadata and backup settings are stored only in SQLite.

## Pipeline and tradeoffs

Backup collects filtered entries and Unix metadata before reading file data,
preserving the existing traversal and directory pruning behavior. It then writes:

```text
entry headers + file data + checksums + end marker
    → optional Zstd or project LZ77
    → optional XChaCha20-Poly1305 or AES-256-GCM STREAM
    → archive.bak
```

Packing reads each file once and uses a 128 KiB copy buffer plus bounded header,
I/O, codec and encryption buffers. Collected entry metadata still occupies O(n)
memory. Directories and symbolic links are header-only records: packing never
opens link targets or creates their mirrored paths in the repository. Compression
history spans entries; only the final archive needs file synchronization.

Whole-stream encoding makes full restoration sequential. There is no random
access index or selective extraction API. A damaged stream can prevent restoration
of later records. Source trees must remain unchanged: the packer detects a file
shrinking or growing during its copy, but does not provide snapshot consistency
or detect every concurrent edit.

## Wire layout before encoding

All integer fields below are unsigned little-endian. There is no block padding.

| Field | Representation |
| --- | --- |
| Magic/version | 8 bytes `b"BAKARC\0\x01"` |
| Entry header length | u32, 1 through 1,048,576 |
| Entry header | Exactly that many bytes of UTF-8 JSON |
| File data | Exactly `original_size` bytes; files only |
| File digest | 32 raw SHA-256 bytes; files only, including empty files |
| Further entries | Repeat header length, header, optional data and digest |
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

The header deliberately omits the checksum: placing the digest after file data
allows packing and hashing in one pass without seeking or buffering a whole file.
SQLite stores the same final digest as lowercase hexadecimal. Header lengths are
checked before allocation or writing, and unknown header fields/types are errors.

## Publication and restore

The packer reserves a new backup directory and creates the archive exclusively.
It finishes archive framing, compression and encryption in that order, synchronizes
the archive and its ancestor directories, and only then commits the SQLite header,
ordered entries and statistics in one transaction. Duplicate IDs do not overwrite
existing archives. Failed publication can leave an unpublished archive; listing
continues to read only committed SQLite records. Automatic cleanup is not provided.

Restore validates all catalog paths before creating entries and validates each
archive header against the corresponding catalog path, metadata and kind before
creating that entry. It copies exactly the declared file size and compares the
computed SHA-256 with both the raw archive trailer and SQLite checksum. It requires
the explicit end marker, rejects extra decoded bytes or records, and finishes the
outer codec/authentication reader before success. Saved directory attributes are
applied after children and root attributes last. Partial output can remain after
a failed restore. Repositories remain trusted; this is not a sandbox for extracting
adversarially constructed archives or modified catalogs.

The encryption AAD uses `archive.bak` as the path, together with the existing
backup ID, compression configuration and key fingerprint bindings. An encrypted
archive covers its headers as well as file data; the separate SQLite catalog
still exposes paths, metadata, sizes, checksums and link targets. See
[content encryption](content-encryption.md).

Repository schemas through v5 are rejected without migration or deletion. Use
a new repository for v6 and the prior binary to operate old repositories. The
HTTP request contract and job schema v5 are unchanged.
