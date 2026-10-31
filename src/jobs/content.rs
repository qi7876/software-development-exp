use super::{
    JobError,
    crypto::{DecryptReader, EncryptWriter, Encryption, Keyring},
};
use std::{
    fs::File,
    io::{self, BufReader, Read, Write},
    path::Path,
};

use super::lz77;
pub(crate) use super::lz77::Lz77Options;

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct BackupOptions {
    pub(crate) compression: Option<CompressionOptions>,
    pub(crate) encryption: Option<Encryption>,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "algorithm", rename_all = "lowercase")]
pub(crate) enum CompressionOptions {
    Zstd(ZstdOptions),
    Lz77(Lz77Options),
}
impl CompressionOptions {
    pub(crate) fn validate(&self) -> Result<(), JobError> {
        match self {
            Self::Zstd(options) if !(1..=22).contains(&options.level) => Err(JobError::new(
                http::StatusCode::BAD_REQUEST,
                "Zstd level must be 1..=22",
            )),
            Self::Zstd(_) => Ok(()),
            Self::Lz77(options) => options
                .validate()
                .map_err(|error| JobError::new(http::StatusCode::BAD_REQUEST, error.to_string())),
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct ZstdOptions {
    pub(crate) level: i32,
}
impl Default for ZstdOptions {
    fn default() -> Self {
        Self { level: 3 }
    }
}
impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            compression: Some(CompressionOptions::Zstd(ZstdOptions::default())),
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
        // Tuple order and serde's path representation are part of encrypted format v2.
        serde_json::to_vec(&(
            self.backup_id,
            path,
            &self.options.compression,
            self.options.encryption.as_ref().map(|e| &e.fingerprint),
        ))
        .map_err(io::Error::other)
    }

    pub(super) fn writer(&self, file: File, path: &Path) -> io::Result<ContentWriter> {
        let writer = match self.key {
            Some(key) => Sink::Encrypted(EncryptWriter::new(file, key, &self.context(path)?)?),
            None => Sink::Plain(file),
        };
        match &self.options.compression {
            None => Ok(ContentWriter::Direct(writer)),
            Some(CompressionOptions::Zstd(options)) => Ok(ContentWriter::Zstd(
                zstd::stream::write::Encoder::new(writer, options.level)?,
            )),
            Some(CompressionOptions::Lz77(options)) => {
                Ok(ContentWriter::Lz77(lz77::Encoder::new(writer, options)?))
            }
        }
    }

    pub(super) fn reader(&self, file: File, path: &Path) -> io::Result<ContentReader> {
        let reader: Box<dyn Read> = match self.key {
            Some(key) => Box::new(DecryptReader::new(file, key, &self.context(path)?)?),
            None => Box::new(file),
        };
        match &self.options.compression {
            None => Ok(ContentReader::Direct(reader)),
            Some(CompressionOptions::Zstd(_)) => Ok(ContentReader::Zstd(
                zstd::stream::read::Decoder::new(reader)?,
            )),
            Some(CompressionOptions::Lz77(_)) => {
                Ok(ContentReader::Lz77(lz77::Decoder::new(reader)?))
            }
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
    Zstd(zstd::stream::write::Encoder<'static, Sink>),
    Lz77(lz77::Encoder<Sink>),
}
impl ContentWriter {
    pub(super) fn finish(self) -> io::Result<File> {
        let sink = match self {
            Self::Direct(sink) => sink,
            Self::Zstd(encoder) => encoder.finish()?,
            Self::Lz77(encoder) => encoder.finish()?,
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
            Self::Zstd(w) => w.write(buf),
            Self::Lz77(w) => w.write(buf),
        }
    }
    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Direct(w) => w.flush(),
            Self::Zstd(w) => w.flush(),
            Self::Lz77(w) => w.flush(),
        }
    }
}

pub(super) enum ContentReader {
    Direct(Box<dyn Read>),
    Zstd(zstd::stream::read::Decoder<'static, BufReader<Box<dyn Read>>>),
    Lz77(lz77::Decoder<Box<dyn Read>>),
}
impl Read for ContentReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Direct(r) => r.read(buf),
            Self::Zstd(r) => r.read(buf),
            Self::Lz77(r) => r.read(buf),
        }
    }
}
impl ContentReader {
    pub(super) fn finish(self) -> io::Result<()> {
        let mut inner: Box<dyn Read> = match self {
            Self::Direct(r) => r,
            Self::Zstd(decoder) => Box::new(decoder.finish()),
            Self::Lz77(decoder) => decoder.finish()?,
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
                compression: Some(CompressionOptions::Zstd(ZstdOptions { level })),
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
                compression: Some(CompressionOptions::Zstd(ZstdOptions { level })),
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
    fn compression_configurations_round_trip() -> TestResult {
        for compression in [
            None,
            Some(CompressionOptions::Zstd(ZstdOptions::default())),
            Some(CompressionOptions::Lz77(Lz77Options::default())),
        ] {
            let options = BackupOptions {
                compression: compression.clone(),
                encryption: None,
            };
            let restored: BackupOptions = serde_json::from_value(serde_json::to_value(&options)?)?;
            assert_eq!(restored.compression, compression);
        }
        Ok(())
    }
}
