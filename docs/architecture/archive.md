# Self-contained backup format v10

A published backup is one `<backup_id>.bak` file. It contains the codec settings,
key identifier/fingerprint, root and entry metadata, file data, creation time,
source path, statistics, and a SHA-256 digest. Preserve the filename when moving
a backup. Encrypted backups require their configured key.

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

The last 48 bytes locate the metadata and bound the encoded payload. Readers
validate lengths before allocation. The manifest holds
`backup_id`, `created_at`, `source`, `compression`, `encryption`,
`checksum_algorithm`, `root_metadata`, `backup_stats`, and ordered `entries`.
The reader derives `archive_checksum` from the final digest. The filename must match the stored ID.
Unknown container versions, missing markers, impossible lengths, malformed
metadata, invalid codec settings, and invalid metadata invariants fail explicitly.

Compression is null, `{"algorithm":"zstd","level":3}`, or
`{"algorithm":"lz77","window_kib":64,"search_depth":32}`. Encryption is null or
`{"algorithm":"aes-256-gcm","key_id":"current","fingerprint":"..."}` (also
supports `xchacha20-poly1305`). The encrypted stream contains its own nonce and,
for AES, salt. See [content encryption](content-encryption.md).

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
are known during packing. This supports a single streaming write of the container and digest. File data
uses a 128 KiB copy buffer plus codec and AEAD buffers. Metadata memory scales
with entry count, and the serialized metadata limit is 64 MiB.

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
exclusive hard link at `<backup_id>.bak`.
Synchronize the repository directory, remove the temporary link, and synchronize
it again. The repository filesystem must support hard links and directory sync.

Ordinary write failures remove the temporary file. A crash can leave
`.partial` files. The final name can exist after a directory sync or job-commit
failure. [Startup recovery](recovery.md) uses the job UUID to reconcile these
files with task history.

## Listing and restoration

Listing scans the metadata and footers of final `.bak` files. Results are newest
first, with ascending backup IDs for timestamp ties. Cost grows with archive
count and metadata size. Invalid final archives return errors.

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

The plaintext manifest exposes paths, sizes, Unix attributes, link targets, and
device numbers. The container SHA-256 detects corruption. Encryption
authenticates the encoded stream and binds the backup ID, filename, compression
settings, and key fingerprint. Trust in the manifest depends on repository access.

## Compatibility

The service uses job schema v5. New backups always use manifest v10. Existing v9
archives use the same container and stream framing and can be listed, verified,
and restored alongside v10 archives. Formats through v8 require their original
database and prior binary.
