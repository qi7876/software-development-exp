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
pub(crate) struct BackupOptions {
    pub(crate) compression: bool,
    pub(crate) encryption: Option<Encryption>,
}

impl Default for BackupOptions {
    fn default() -> Self {
        Self {
            compression: true,
            encryption: None,
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
            self.options.compression,
            self.options.encryption.as_ref().map(|e| &e.fingerprint),
        ))
        .map_err(io::Error::other)
    }

    pub(super) fn writer(&self, file: File, path: &Path) -> io::Result<ContentWriter> {
        let writer = match self.key {
            Some(key) => Sink::Encrypted(EncryptWriter::new(file, key, &self.context(path)?)?),
            None => Sink::Plain(file),
        };
        if self.options.compression {
            Ok(ContentWriter::Compressed(
                zstd::stream::write::Encoder::new(writer, 0)?,
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
        if self.options.compression {
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
