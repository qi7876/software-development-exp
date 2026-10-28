# Content encryption and encoding contract

Compression and encryption are runtime choices per backup, not Cargo feature switches. The default is Zstd compression without encryption. SHA-256 and original sizes always describe plaintext. Combining features means source → Zstd → encryption → disk; restore applies the inverse. Each writer finishes compression, then encryption, then synchronizes its output before metadata publication. Readers must consume the encoded stream completely, including the authenticated final block, before success.

## Key lifecycle

The server loads a mapping of stable identifiers to 32-byte symmetric key files at startup. Key files contain exactly 64 hexadecimal digits, with surrounding whitespace permitted. `bak keygen --output PATH` generates keys using the operating system random source, creates with `0600` on Unix, and refuses to overwrite. Keys are held in zeroizing containers and never serialized. Configuration contains paths, not secrets.

The fingerprint is lowercase hexadecimal SHA-256 of `b"bak-key-v1\0" || key`. Admission resolves the named key and persists its identifier and fingerprint; workers validate both again when executing queued work. Restore resolves the same pair from repository metadata. Restart uses the newly loaded keyring, so changing or removing a key cannot silently change queued encryption or decrypt with a different key. Old named keys must remain configured for old backups. The service does not scan all keys looking for a substitute.

No encryption configuration is required for unencrypted backups/restores. Configured keys that cannot be read or parsed fail startup without including their contents in errors. A missing or mismatched key fails the job before creating payloads or restored entries. Retain independent copies of keys outside the source/repository; no key recovery is implemented.

## Encrypted payload version 1

Algorithm identifier: `xchacha20poly1305-stream-be32-v1`. Use RustCrypto's XChaCha20-Poly1305 with its STREAM BE32 implementation; its counter/final flag form the remaining five bytes of the 24-byte AEAD nonce.

The fixed 27-byte header consists of:

| Bytes | Meaning |
| --- | --- |
| 0–7 | `b"BAKENC\0\x01"` magic and payload version |
| 8–26 | Fresh random 19-byte STREAM nonce prefix, generated independently for each file |

The encryption input is split into 131072-byte full blocks, each written with `encrypt_next`, followed by a shorter final block written with `encrypt_last`. Each ciphertext block adds a 16-byte tag. A final block is mandatory even when its plaintext is empty, including empty files and exact multiples of the block size. There are no length prefixes: full ciphertext blocks have length 131088, and the shorter final block occupies the remainder at EOF. Reads fill a block across short input reads; input read boundaries carry no framing meaning.

For every block, AAD is the fixed header followed by compact serde JSON for the tuple `(backup_id, relative_path, compression_enabled, key_fingerprint)`. Tuple order, JSON encoding, path representation, and boolean encoding are part of this format. This binds payloads to their file, backup, mode, key, and header. Key identifier names are routing metadata; key fingerprints identify material.

A reader accepts EOF only after successful final-block authentication. Missing/partial headers, invalid magic, incomplete blocks, missing final tags, changed/reordered/duplicated blocks, extra trailing bytes, wrong keys, and changed associated data fail decoding. The STREAM counter limit fails explicitly rather than wrapping. Memory is bounded by a few blocks plus Zstd buffers; neither backup nor restore buffers a complete file.

## Limits and compatibility

Encryption protects regular-file contents only. Filenames, directory layout, source paths, Unix metadata, original sizes, SHA-256 values, and symbolic link targets remain visible. Metadata is still trusted; this is not full-repository authentication or protection from a malicious API server. The service sees plaintext and keys. Failed or interrupted restores can leave partial output, as with existing checksum failures.

Repository format/schema v3 stores compression, encryption format, key identifier and fingerprint in the same publication transaction as entries and statistics. Job schema v2 prevents older request JSON from being treated as new-format work. Old databases are rejected without migration or deletion. Use the prior binary with prior directories to operate old data.
