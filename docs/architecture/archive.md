# Self-contained backup format v10

A published backup is one `<backup_id>.bak` file. It contains the codec settings,
key identifier/fingerprint, root and entry metadata, file data, creation time,
source path, statistics, and a SHA-256 digest. No repository database or sidecar is
required. Copy the file, keeping its name, into an empty repository to list and
restore it. Encrypted backups still require their external configured key.

## Container layout

All lengths are unsigned little-endian. There is no padding.

| Field | Representation |
| --- | --- |
| Container magic/version | 8 bytes `b"BAKFILE\x01"` |
| Encoded payload | Sequential archive v2, optionally compressed and encrypted |
| Metadata | Compact UTF-8 JSON `Manifest`, `format_version: 10` for new backups |
| Metadata length | u64, 1 through 67,108,864 bytes (64 MiB) |
| Footer marker | 8 bytes `b"BAKEND\0\x01"` |
| Digest | 32 raw SHA-256 bytes covering every preceding byte |

The last 48 bytes locate the metadata and bound the encoded payload without
scanning or decoding it. Validate lengths before allocation. The manifest holds
`backup_id`, `created_at`, `source`, `compression`, `encryption`,
`checksum_algorithm`, `root_metadata`, `backup_stats`, and ordered `entries`.
`archive_checksum` is derived from the final digest when reading; it is not
serialized inside its own checksum input. The filename must match the stored ID.
Unknown container versions, missing markers, impossible lengths, malformed
metadata, invalid codec settings, and invalid metadata invariants fail explicitly.

Compression is null, `{"algorithm":"zstd","level":3}`, or
`{"algorithm":"lz77","window_kib":64,"search_depth":32}`. Encryption is null or
`{"algorithm":"aes-256-gcm","key_id":"current","fingerprint":"..."}` (also
supports `xchacha20-poly1305`). The encrypted stream contains its own nonce and,
for AES, salt. Secret key bytes are never embedded. See [content encryption](content-encryption.md).

## Streaming pipeline

```text
entry headers + file data + end marker
    → optional Zstd or project LZ77
    → optional XChaCha20-Poly1305 or AES-256-GCM STREAM
    → container payload
container magic + payload + metadata + length + footer marker
    → SHA-256 accumulated during writing
    → final digest
```

Metadata is placed after the payload because file sizes and the final entry list
are known during packing. This avoids reserving a variable-size header or
rereading the file to calculate its digest. File contents use a 128 KiB copy buffer
and bounded codec/AEAD buffers. Collected entry metadata still occupies O(n)
memory; the metadata section is limited to 64 MiB.

The decoded payload retains sequential archive v2: `b"BAKARC\0\x02"`, followed by
u32 entry-header length (1 through 1,048,576), a JSON `ManifestEntry`, and exactly
`original_size` data bytes for each `File` entry. Directories, symlinks, FIFOs,
character devices, block devices, and hard-link aliases are header-only. The
explicit end marker is u32 zero followed by decoded EOF. Entry kinds and
hard-link references follow the [manifest contract](repository-schema.md).

## Publication and recovery

Write exclusively to a unique hidden `.partial` file in the destination
repository. Finish archive framing, compression, and encryption; append the
metadata, footer, and digest; then synchronize the file. Publish it by creating a
hard link at `<backup_id>.bak`, which cannot overwrite an existing name.
Synchronize the repository directory, remove the temporary link, and synchronize
it again. The repository filesystem must support hard links and directory sync.

Ordinary failures remove only the writer's own temporary file. A process crash
can leave `.partial` files, which listing ignores. [Startup reconciliation](recovery.md)
can publish one verified complete temporary for its interrupted task; unrelated or
unverifiable files are retained. Final `.bak` names refer to complete inodes. A directory-sync failure
after linking can leave a visible final backup despite a failed job. File
publication and the subsequent job-history commit are separate durability
boundaries; a complete backup may exist even if task completion was interrupted.
New job executions use the persisted job UUID as the backup ID, enabling verified
startup repair of that gap without changing this container format.

## Listing and restoration

Listing scans `.bak` files and reads their metadata/footer, without reading file
payloads or requiring encryption keys. Results are newest first, with backup IDs
ascending for equal timestamps. This replaces an indexed SQL query: listing cost
now grows with the number and metadata size of backups. Invalid final `.bak`
files are reported as errors; unrelated files and `.partial` output are ignored.

Restore opens the container, validates all relative paths, path uniqueness,
parent-before-child directory relationships, hard-link targets, timestamps, modes,
statistics, and codec settings before creating output entries. It resolves the
configured key, selects the recorded decoder, and compares each decoded entry
header against the saved manifest. It reads only the bounded encoded payload
into the codec, requires the archive end marker and codec/AEAD completion, then hashes the metadata and
footer bytes and verifies the final digest. Saved directory attributes are applied
after children. Root attributes are applied last. Regular files and directories
are synchronized before success. FIFO and device metadata is restored through
filesystem operations on the node paths. Creating a device node requires OS and
destination filesystem support as well as sufficient process privileges. Failed
restores can leave partial output.

The outer metadata is plaintext, as the old SQLite catalog was: file names,
source paths, Unix metadata, sizes, statistics, link targets, and device numbers
remain visible.
SHA-256 detects accidental corruption, not deliberate replacement of both content
and digest. Encryption authenticates encoded entry headers/file data and binds
the backup ID, canonical archive filename, compression settings, and key
fingerprint through the existing AAD. The outer metadata is not fully AEAD
protected; repositories remain trusted. Keep source trees unchanged during backup.

## Compatibility

The service uses job schema v5. New backups always use manifest v10. Existing v9
archives use the same container and stream framing and can be listed, verified,
and restored alongside v10 archives. Formats through v8 require their original
database and prior binary.
