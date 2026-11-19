# Content encryption and encoding contract

Backup encodes entries, applies the selected compression, then encrypts the
archive stream. The manifest records both settings for restore. The plaintext
manifest and encoded stream are covered by the container SHA-256. See the
[archive contract](archive.md) for framing and [LZ77](lz77.md) for codec details.

## Key lifecycle

The server loads named 32-byte symmetric keys at startup. Key files contain 64
hexadecimal digits with optional surrounding whitespace. `bak keygen --output
PATH` creates a new file with mode `0600` using system randomness. Configuration
stores file paths, and the service holds loaded keys in zeroizing containers.

The fingerprint is lowercase hexadecimal SHA-256 of `b"bak-key-v1\0" || key`.
Admission stores the key ID and fingerprint. Backup execution and restore each
resolve and validate that pair. A missing or mismatched key fails the job before
writing entries. Keep the original key files available for queued jobs and
encrypted archives.

## XChaCha20-Poly1305 encrypted payload version 2

The `xchacha20poly1305-stream-be32-v2` payload uses RustCrypto XChaCha20-Poly1305 and STREAM BE32. The counter and final flag supply the last five bytes of the 24-byte nonce.

The header is 27 bytes.

| Bytes | Meaning |
| --- | --- |
| 0–7 | `b"BAKENC\0\x02"` magic and payload version |
| 8–26 | Fresh random 19-byte STREAM nonce prefix, generated independently for each archive |

Write each 131072-byte plaintext block with `encrypt_next` and the final 0–131071 bytes with `encrypt_last`. A 16-byte tag makes each full ciphertext block 131088 bytes. The shorter final block occupies the remainder at EOF. Readers assemble each block across short reads.

For every block, AAD is the fixed header followed by compact serde JSON for the tuple `(backup_id, relative_path, compression_configuration, key_fingerprint)`. Tuple order, JSON encoding, path representation, algorithm tags, parameters, and null encoding are part of this format. The path is `<backup_id>.bak` in v9 and v10 archives. The key ID selects a key and the fingerprint identifies its bytes.

The reader requires a valid header, authenticated blocks in order, and EOF after the authenticated final block. Exceeding the STREAM counter limit returns an error. Memory use is bounded by block and codec buffers.

## AES-256-GCM encrypted payload version 3

The `aes256gcm-stream-be32-v3` payload uses RustCrypto AES-256-GCM and STREAM BE32 with the same block framing as v2.

The 47-byte header contains `b"BAKENC\0\x03"` (bytes 0–7), a fresh OS-random 32-byte salt (8–39), and an independently random 7-byte STREAM nonce prefix (40–46). STREAM adds its four-byte counter and final flag to form the 12-byte AES-GCM nonce. Derive each archive's 32-byte key with HKDF-SHA256: IKM is the configured key, salt is the header salt, and info is `b"bak-aes-256-gcm-stream-be32-v3"`. The salt provides key separation between archives. Temporary archive keys use zeroizing containers.

AAD is the complete header followed by compact serde JSON for `(backup_id, relative_path, compression_configuration, key_fingerprint, algorithm)`, where algorithm is `"aes-256-gcm"`. The manifest selects the decoder and the reader checks the corresponding header. A configured key can serve both algorithms.

The API takes `{algorithm, key_id}` for encryption. Task and listing records add the resolved fingerprint. Status lists the available key IDs and fingerprints.
