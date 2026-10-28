use super::{
    JobError,
    crypto::{DecryptReader, EncryptWriter, Encryption, Keyring},
};
use std::{
    fs::File,
    io::{self, BufReader, Read, Write},
    path::Path,
};

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(try_from = "BackupOptionsWire", into = "BackupOptionsWire")]
pub(crate) struct BackupOptions {
    pub(crate) compression: Option<ZstdOptions>,
    pub(crate) encryption: Option<Encryption>,
}

#[derive(Clone, Debug)]
pub(crate) struct ZstdOptions {
    pub(crate) level: i32,
}

impl Default for ZstdOptions {
    fn default() -> Self {
        Self {
            level: Self::default_level(),
        }
    }
}

impl ZstdOptions {
    pub(crate) fn default_level() -> i32 {
        3
    }

    pub(crate) fn validate(&self) -> Result<(), JobError> {
        if !(1..=22).contains(&self.level) {
            return Err(JobError::new(
                http::StatusCode::BAD_REQUEST,
                "compression_level must be an integer from 1 to 22",
            ));
        }
        Ok(())
    }
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            compression: Some(ZstdOptions::default()),
            encryption: None,
        }
    }
}

impl BackupOptions {
    pub(crate) fn validate(&self) -> Result<(), JobError> {
        if let Some(compression) = &self.compression {
            compression.validate()?;
        }
        Ok(())
    }
}

// Preserve the existing task JSON while keeping disabled compression parameter-free internally.
#[derive(serde::Serialize, serde::Deserialize)]
struct BackupOptionsWire {
    compression: bool,
    #[serde(default = "ZstdOptions::default_level")]
    compression_level: i32,
    encryption: Option<Encryption>,
}

impl TryFrom<BackupOptionsWire> for BackupOptions {
    type Error = JobError;
    fn try_from(wire: BackupOptionsWire) -> Result<Self, Self::Error> {
        let compression = ZstdOptions {
            level: wire.compression_level,
        };
        compression.validate()?;
        Ok(Self {
            compression: wire.compression.then_some(compression),
            encryption: wire.encryption,
        })
    }
}

impl From<BackupOptions> for BackupOptionsWire {
    fn from(options: BackupOptions) -> Self {
        Self {
            compression: options.compression.is_some(),
            compression_level: options
                .compression
                .map_or_else(ZstdOptions::default_level, |compression| compression.level),
            encryption: options.encryption,
        }
    }
}

pub(super) struct Content<'a> {
    options: &'a BackupOptions,
    key: Option<&'a [u8; 32]>,
    backup_id: &'a str,
}

impl<'a> Content<'a> {
    pub(super) fn new(
        options: &'a BackupOptions,
        keys: &'a Keyring,
        backup_id: &'a str,
    ) -> Result<Self, JobError> {
        options.validate()?;
        let key = options
            .encryption
            .as_ref()
            .map(|encryption| keys.resolve(encryption))
            .transpose()?;
        Ok(Self {
            options,
            key,
            backup_id,
        })
    }

    fn context(&self, path: &Path) -> io::Result<Vec<u8>> {
        // Tuple order and serde's path representation are part of encrypted format v1.
        serde_json::to_vec(&(
            self.backup_id,
            path,
            self.options.compression.is_some(),
            self.options.encryption.as_ref().map(|e| &e.fingerprint),
        ))
        .map_err(io::Error::other)
    }

    pub(super) fn writer(&self, file: File, path: &Path) -> io::Result<ContentWriter> {
        let writer = match self.key {
            Some(key) => Sink::Encrypted(EncryptWriter::new(file, key, &self.context(path)?)?),
            None => Sink::Plain(file),
        };
        if let Some(compression) = &self.options.compression {
            Ok(ContentWriter::Compressed(
                zstd::stream::write::Encoder::new(writer, compression.level)?,
            ))
        } else {
            Ok(ContentWriter::Direct(writer))
        }
    }

    pub(super) fn reader(&self, file: File, path: &Path) -> io::Result<ContentReader> {
        let reader: Box<dyn Read> = match self.key {
            Some(key) => Box::new(DecryptReader::new(file, key, &self.context(path)?)?),
            None => Box::new(file),
        };
        if self.options.compression.is_some() {
            Ok(ContentReader::Compressed(zstd::stream::read::Decoder::new(
                reader,
            )?))
        } else {
            Ok(ContentReader::Direct(reader))
        }
    }
}

pub(super) enum Sink {
    Plain(File),
    Encrypted(EncryptWriter<File>),
}
impl Write for Sink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Plain(w) => w.write(buf),
            Self::Encrypted(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Plain(w) => w.flush(),
            Self::Encrypted(w) => w.flush(),
        }
    }
}

pub(super) enum ContentWriter {
    Direct(Sink),
    Compressed(zstd::stream::write::Encoder<'static, Sink>),
}
impl ContentWriter {
    pub(super) fn finish(self) -> io::Result<File> {
        let sink = match self {
            Self::Direct(sink) => sink,
            Self::Compressed(encoder) => encoder.finish()?,
        };
        match sink {
            Sink::Plain(file) => Ok(file),
            Sink::Encrypted(writer) => writer.finish(),
        }
    }
}
impl Write for ContentWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Direct(w) => w.write(buf),
            Self::Compressed(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Direct(w) => w.flush(),
            Self::Compressed(w) => w.flush(),
        }
    }
}

pub(super) enum ContentReader {
    Direct(Box<dyn Read>),
    Compressed(zstd::stream::read::Decoder<'static, BufReader<Box<dyn Read>>>),
}
impl Read for ContentReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Direct(r) => r.read(buf),
            Self::Compressed(r) => r.read(buf),
        }
    }
}
impl ContentReader {
    pub(super) fn finish(self) -> io::Result<()> {
        let mut inner: Box<dyn Read> = match self {
            Self::Direct(r) => r,
            Self::Compressed(decoder) => Box::new(decoder.finish()),
        };
        let mut byte = [0];
        if inner.read(&mut byte)? != 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "trailing payload data",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn enabled_compression_levels_are_validated() -> TestResult {
        let keys = Keyring::default();
        for level in [1, 3, 22, 0, 23, -1] {
            let options = BackupOptions {
                compression: Some(ZstdOptions { level }),
                encryption: None,
            };
            let result = Content::new(&options, &keys, "test");
            assert_eq!(result.is_ok(), (1..=22).contains(&level));
            if let Err(error) = result {
                assert_eq!(error.code, http::StatusCode::BAD_REQUEST);
            }
        }
        Ok(())
    }

    #[test]
    fn selected_levels_match_zstd_streams_and_restore_with_default_settings() -> TestResult {
        let directory = crate::test_support::TestDirectory::new()?;
        let keys = Keyring::default();
        let input: Vec<u8> = (0..65000u64).map(|n| ((n * n / 17) % 251) as u8).collect();
        let mut encodings = Vec::new();
        for level in [1, 3, 9] {
            let options = BackupOptions {
                compression: Some(ZstdOptions { level }),
                ..BackupOptions::default()
            };
            let content = Content::new(&options, &keys, "test")?;
            let path = directory.path().join(format!("level-{level}"));
            let mut writer = content.writer(File::create(&path)?, Path::new("file"))?;
            writer.write_all(&input)?;
            writer.finish()?;
            let encoded = std::fs::read(&path)?;
            let mut reference = zstd::stream::write::Encoder::new(Vec::new(), level)?;
            reference.write_all(&input)?;
            assert_eq!(encoded, reference.finish()?);
            encodings.push(encoded);
            let defaults = BackupOptions::default();
            let restore = Content::new(&defaults, &keys, "test")?;
            let mut reader = restore.reader(File::open(&path)?, Path::new("file"))?;
            let mut decoded = Vec::new();
            reader.read_to_end(&mut decoded)?;
            reader.finish()?;
            assert_eq!(decoded, input);
        }
        assert!(encodings.windows(2).any(|pair| pair[0] != pair[1]));
        Ok(())
    }

    #[test]
    fn task_json_preserves_enabled_and_disabled_compression() -> TestResult {
        for level in [None, Some(9)] {
            let options = BackupOptions {
                compression: level.map(|level| ZstdOptions { level }),
                encryption: None,
            };
            let value = serde_json::to_value(&options)?;
            assert_eq!(
                value,
                serde_json::json!({"compression": level.is_some(), "compression_level": level.unwrap_or(3), "encryption": null})
            );
            let restored: BackupOptions = serde_json::from_value(value)?;
            assert_eq!(restored.compression.map(|options| options.level), level);
        }
        assert!(
            serde_json::from_value::<BackupOptions>(
                serde_json::json!({"compression":false, "compression_level":23, "encryption":null})
            )
            .is_err()
        );
        let keys = Keyring::default();
        let disabled = BackupOptions {
            compression: None,
            encryption: None,
        };
        assert!(Content::new(&disabled, &keys, "test").is_ok());
        Ok(())
    }

    #[test]
    fn old_queued_requests_keep_default_level() -> TestResult {
        let value = serde_json::json!({ "kind":"Backup", "source":"source", "repository":"repo", "filter": crate::test_support::unfiltered_config(), "compression":true, "encryption":null });
        let request: crate::jobs::JobRequest = serde_json::from_value(value)?;
        let crate::jobs::JobRequest::Backup { options, .. } = request else {
            return Err("unexpected request kind".into());
        };
        assert!(matches!(
            options.compression,
            Some(ZstdOptions { level: 3 })
        ));
        Ok(())
    }
}
