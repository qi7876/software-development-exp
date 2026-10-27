use super::JobError;
use aead::{
    Payload,
    stream::{DecryptorBE32, EncryptorBE32},
};
use chacha20poly1305::{KeyInit, XChaCha20Poly1305};
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::OpenOptions,
    io::{self, Read, Write},
    path::{Path, PathBuf},
};
use zeroize::{Zeroize, Zeroizing};

const MAGIC: &[u8; 8] = b"BAKENC\0\x01";
const PREFIX: usize = 19; // STREAM BE32 reserves four counter bytes and one final-block byte.
pub(super) const CHUNK: usize = 128 * 1024;
const TAG: usize = 16;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Encryption {
    pub(crate) key_id: String,
    pub(crate) fingerprint: String,
}

#[derive(Default)]
pub(crate) struct Keyring(BTreeMap<String, Zeroizing<[u8; 32]>>);

impl Keyring {
    pub(crate) fn load(
        directory: &Path,
        files: &BTreeMap<String, PathBuf>,
    ) -> Result<Self, JobError> {
        let mut keys = BTreeMap::new();
        for (id, path) in files {
            if id.trim().is_empty() {
                return Err(JobError::internal(
                    "encryption key identifier must not be empty",
                ));
            }
            let text = Zeroizing::new(
                std::fs::read_to_string(directory.join(path))
                    .map_err(|_| JobError::internal(format!("cannot read encryption key {id}")))?,
            );
            let mut key = Zeroizing::new([0; 32]);
            hex::decode_to_slice(text.trim(), key.as_mut()).map_err(|_| {
                JobError::internal(format!(
                    "encryption key {id} must contain 64 hexadecimal digits"
                ))
            })?;
            keys.insert(id.clone(), key);
        }
        Ok(Self(keys))
    }

    pub(crate) fn available(&self) -> Vec<Encryption> {
        self.0
            .iter()
            .map(|(id, key)| Encryption {
                key_id: id.clone(),
                fingerprint: fingerprint(key),
            })
            .collect()
    }

    pub(crate) fn select(&self, id: &str) -> Result<Encryption, JobError> {
        self.available()
            .into_iter()
            .find(|key| key.key_id == id)
            .ok_or_else(|| {
                JobError::new(
                    http::StatusCode::BAD_REQUEST,
                    format!("encryption key {id} is not configured"),
                )
            })
    }

    pub(super) fn resolve(&self, encryption: &Encryption) -> Result<&[u8; 32], JobError> {
        let key = self.0.get(&encryption.key_id).ok_or_else(|| {
            JobError::new(
                http::StatusCode::BAD_REQUEST,
                format!("encryption key {} is not configured", encryption.key_id),
            )
        })?;
        if fingerprint(key) != encryption.fingerprint {
            return Err(JobError::new(
                http::StatusCode::BAD_REQUEST,
                format!(
                    "encryption key {} fingerprint does not match",
                    encryption.key_id
                ),
            ));
        }
        Ok(key)
    }
}

fn fingerprint(key: &[u8; 32]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"bak-key-v1\0");
    hash.update(key);
    hex::encode(hash.finalize())
}

pub(crate) fn generate(path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let mut key = Zeroizing::new([0; 32]);
    OsRng
        .try_fill_bytes(key.as_mut())
        .map_err(|_| "cannot generate encryption key")?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    let encoded = Zeroizing::new(hex::encode(key.as_ref()));
    writeln!(file, "{}", encoded.as_str())?;
    file.sync_all()?;
    Ok(())
}

fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "encrypted payload authentication or framing failed",
    )
}

fn associated(header: &[u8], context: &[u8]) -> Vec<u8> {
    let mut aad = header.to_vec();
    aad.extend_from_slice(context);
    aad
}

pub(super) struct EncryptWriter<W> {
    inner: W,
    stream: EncryptorBE32<XChaCha20Poly1305>,
    buffer: Zeroizing<Vec<u8>>,
    aad: Vec<u8>,
}

impl<W: Write> EncryptWriter<W> {
    pub(super) fn new(mut inner: W, key: &[u8; 32], context: &[u8]) -> io::Result<Self> {
        let mut prefix = [0u8; PREFIX];
        OsRng
            .try_fill_bytes(&mut prefix)
            .map_err(|_| io::Error::other("cannot generate encryption nonce"))?;
        let mut header = MAGIC.to_vec();
        header.extend_from_slice(&prefix);
        inner.write_all(&header)?;
        Ok(Self {
            inner,
            stream: EncryptorBE32::from_aead(XChaCha20Poly1305::new(key.into()), (&prefix).into()),
            buffer: Zeroizing::new(Vec::with_capacity(CHUNK)),
            aad: associated(&header, context),
        })
    }

    pub(super) fn finish(mut self) -> io::Result<W> {
        let encrypted = self
            .stream
            .encrypt_last(Payload {
                msg: &self.buffer,
                aad: &self.aad,
            })
            .map_err(|_| invalid())?;
        self.inner.write_all(&encrypted)?;
        self.inner.flush()?;
        Ok(self.inner)
    }
}

impl<W: Write> Write for EncryptWriter<W> {
    fn write(&mut self, input: &[u8]) -> io::Result<usize> {
        let n = input.len().min(CHUNK - self.buffer.len());
        self.buffer.extend_from_slice(&input[..n]);
        if self.buffer.len() == CHUNK {
            let encrypted = self
                .stream
                .encrypt_next(Payload {
                    msg: &self.buffer,
                    aad: &self.aad,
                })
                .map_err(|_| invalid())?;
            self.inner.write_all(&encrypted)?;
            self.buffer.zeroize();
        }
        Ok(n)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

pub(super) struct DecryptReader<R> {
    inner: R,
    stream: Option<DecryptorBE32<XChaCha20Poly1305>>,
    buffer: Zeroizing<Vec<u8>>,
    offset: usize,
    aad: Vec<u8>,
    failed: bool,
}

impl<R: Read> DecryptReader<R> {
    pub(super) fn new(mut inner: R, key: &[u8; 32], context: &[u8]) -> io::Result<Self> {
        let mut header = [0u8; 8 + PREFIX];
        inner.read_exact(&mut header)?;
        if &header[..8] != MAGIC {
            return Err(invalid());
        }
        let prefix = aead::generic_array::GenericArray::from_slice(&header[8..]);
        Ok(Self {
            inner,
            stream: Some(DecryptorBE32::from_aead(
                XChaCha20Poly1305::new(key.into()),
                prefix,
            )),
            buffer: Zeroizing::new(Vec::new()),
            offset: 0,
            aad: associated(&header, context),
            failed: false,
        })
    }

    fn next(&mut self) -> io::Result<()> {
        let mut encrypted = vec![0; CHUNK + TAG];
        let mut n = 0;
        while n < encrypted.len() {
            match self.inner.read(&mut encrypted[n..]) {
                Ok(0) => break,
                Ok(count) => n += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        let payload = Payload {
            msg: &encrypted[..n],
            aad: &self.aad,
        };
        let plaintext = if n == encrypted.len() {
            self.stream
                .as_mut()
                .ok_or_else(invalid)?
                .decrypt_next(payload)
        } else {
            self.stream
                .take()
                .ok_or_else(invalid)?
                .decrypt_last(payload)
        }
        .map_err(|_| invalid())?;
        self.buffer = Zeroizing::new(plaintext);
        self.offset = 0;
        Ok(())
    }
}

impl<R: Read> Read for DecryptReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if self.failed {
            return Err(invalid());
        }
        if self.offset == self.buffer.len() {
            if self.stream.is_none() {
                return Ok(0);
            }
            if let Err(error) = self.next() {
                self.failed = true;
                return Err(error);
            }
        }
        let n = output.len().min(self.buffer.len() - self.offset);
        output[..n].copy_from_slice(&self.buffer[self.offset..self.offset + n]);
        self.offset += n;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn encrypt(data: &[u8], key: &[u8; 32], context: &[u8]) -> io::Result<Vec<u8>> {
        let mut writer = EncryptWriter::new(Vec::new(), key, context)?;
        // Small writes exercise buffering independently of input read boundaries.
        for part in data.chunks(7919) {
            writer.write_all(part)?;
        }
        writer.finish()
    }
    fn decrypt(data: &[u8], key: &[u8; 32], context: &[u8]) -> io::Result<Vec<u8>> {
        let mut reader = DecryptReader::new(Cursor::new(data), key, context)?;
        let mut out = Vec::new();
        reader.read_to_end(&mut out)?;
        Ok(out)
    }

    #[test]
    fn streaming_boundaries_and_unique_nonces() -> TestResult {
        for size in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK, 3 * CHUNK + 17] {
            let input: Vec<u8> = (0..size).map(|n| (n % 251) as u8).collect();
            let a = encrypt(&input, &[7; 32], b"context")?;
            let b = encrypt(&input, &[7; 32], b"context")?;
            assert_ne!(a, b);
            assert_eq!(decrypt(&a, &[7; 32], b"context")?, input);
            assert!(decrypt(&a, &[8; 32], b"context").is_err());
            assert!(decrypt(&a, &[7; 32], b"other-file").is_err());
        }
        Ok(())
    }

    #[test]
    fn tampering_truncation_reordering_and_trailing_data_are_rejected() -> TestResult {
        let data: Vec<u8> = (0..CHUNK * 2).map(|n| (n % 251) as u8).collect();
        let original = encrypt(&data, &[7; 32], b"context")?;
        for offset in [0, 7, 8, 26, 27, CHUNK, original.len() - 1] {
            let mut changed = original.clone();
            changed[offset] ^= 1;
            assert!(
                decrypt(&changed, &[7; 32], b"context").is_err(),
                "tampered byte {offset}"
            );
        }
        for length in [
            0,
            8,
            26,
            27,
            28,
            27 + CHUNK + TAG,
            original.len() - TAG,
            original.len() - 1,
        ] {
            assert!(
                decrypt(&original[..length], &[7; 32], b"context").is_err(),
                "truncated at {length}"
            );
        }
        let start = MAGIC.len() + PREFIX;
        let block = CHUNK + TAG;
        let mut reordered = original.clone();
        reordered[start..start + block]
            .copy_from_slice(&original[start + block..start + 2 * block]);
        reordered[start + block..start + 2 * block]
            .copy_from_slice(&original[start..start + block]);
        assert!(decrypt(&reordered, &[7; 32], b"context").is_err());
        let mut duplicated = original.clone();
        duplicated.splice(start..start, original[start..start + block].iter().copied());
        assert!(decrypt(&duplicated, &[7; 32], b"context").is_err());
        for trailing in [vec![0], vec![0; block]] {
            let mut changed = original.clone();
            changed.extend(trailing);
            assert!(decrypt(&changed, &[7; 32], b"context").is_err());
        }
        Ok(())
    }

    #[test]
    fn key_generation_loading_and_identity_checks() -> TestResult {
        let directory = crate::test_support::TestDirectory::new()?;
        let path = directory.path().join("key");
        generate(&path)?;
        let original = std::fs::read(&path)?;
        assert!(generate(&path).is_err());
        assert_eq!(std::fs::read(&path)?, original);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)?.permissions().mode() & 0o777,
                0o600
            );
        }
        let files = BTreeMap::from([("main".to_string(), PathBuf::from("key"))]);
        let keys = Keyring::load(directory.path(), &files)?;
        let selected = keys.select("main")?;
        assert_eq!(keys.resolve(&selected)?.len(), 32);
        assert!(Keyring::default().resolve(&selected).is_err());
        std::fs::write(&path, hex::encode([42; 32]))?;
        let changed = Keyring::load(directory.path(), &files)?;
        assert!(changed.resolve(&selected).is_err());
        std::fs::write(&path, "not-a-key")?;
        assert!(Keyring::load(directory.path(), &files).is_err());
        Ok(())
    }
}
