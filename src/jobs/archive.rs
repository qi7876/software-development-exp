//! Sequential archive framing, independent of compression and encryption.
use super::repository::{ManifestEntry, ManifestEntryKind};
use std::{
    io::{self, BufReader, BufWriter, Read, Write},
    path::{Path, PathBuf},
};

pub(super) fn file_name(backup_id: &str) -> String {
    format!("{backup_id}.bak")
}

pub(super) fn path(repository: &Path, backup_id: &str) -> PathBuf {
    repository.join(file_name(backup_id))
}
const MAGIC: &[u8; 8] = b"BAKARC\0\x02";
const MAX_HEADER_SIZE: usize = 1024 * 1024;

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Copy exactly one file, leaving the next record unread. Memory is bounded.
fn copy_file(reader: &mut impl Read, writer: &mut impl Write, size: u64) -> io::Result<()> {
    let mut remaining = size;
    let mut buffer = [0; 128 * 1024];
    while remaining > 0 {
        let length = remaining.min(buffer.len() as u64) as usize;
        let count = match reader.read(&mut buffer[..length]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "truncated archive file data",
            ));
        }
        writer.write_all(&buffer[..count])?;
        remaining -= count as u64;
    }
    Ok(())
}

pub(super) struct Writer<W: Write> {
    inner: BufWriter<W>,
}

impl<W: Write> Writer<W> {
    pub(super) fn new(inner: W) -> io::Result<Self> {
        let mut inner = BufWriter::new(inner);
        inner.write_all(MAGIC)?;
        Ok(Self { inner })
    }

    /// Copy a bounded file payload without seeking or buffering the complete file.
    pub(super) fn append(
        &mut self,
        entry: &ManifestEntry,
        source: &mut impl Read,
    ) -> io::Result<()> {
        let header = serde_json::to_vec(entry)?;
        if header.len() > MAX_HEADER_SIZE {
            return Err(invalid("archive entry header exceeds 1 MiB"));
        }
        self.inner.write_all(&(header.len() as u32).to_le_bytes())?;
        self.inner.write_all(&header)?;
        if let ManifestEntryKind::File { original_size } = &entry.kind {
            copy_file(source, &mut self.inner, *original_size)?;
            if source.read(&mut [0])? != 0 {
                return Err(invalid("source file grew while packing archive"));
            }
        }
        Ok(())
    }

    pub(super) fn finish(mut self) -> io::Result<W> {
        self.inner.write_all(&0u32.to_le_bytes())?;
        self.inner.into_inner().map_err(|error| error.into_error())
    }
}

pub(super) struct Reader<R> {
    inner: BufReader<R>,
}

impl<R: Read> Reader<R> {
    pub(super) fn new(inner: R) -> io::Result<Self> {
        let mut inner = BufReader::new(inner);
        let mut magic = [0; 8];
        inner.read_exact(&mut magic)?;
        if &magic != MAGIC {
            return Err(invalid("unsupported archive format"));
        }
        Ok(Self { inner })
    }

    fn header_length(&mut self) -> io::Result<usize> {
        let mut length = [0; 4];
        self.inner.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_HEADER_SIZE {
            return Err(invalid("archive entry header exceeds 1 MiB"));
        }
        Ok(length)
    }

    /// Validate the header before creating an output entry.
    pub(super) fn expect_entry(&mut self, entry: &ManifestEntry) -> io::Result<()> {
        let length = self.header_length()?;
        if length == 0 {
            return Err(invalid("archive ended before expected entry"));
        }
        let mut bytes = vec![0; length];
        self.inner.read_exact(&mut bytes)?;
        let header: ManifestEntry = serde_json::from_slice(&bytes)?;
        if &header != entry {
            return Err(invalid("archive entry differs from repository metadata"));
        }
        Ok(())
    }

    pub(super) fn read_file(&mut self, destination: &mut impl Write, size: u64) -> io::Result<()> {
        copy_file(&mut self.inner, destination, size)
    }

    /// Require an explicit end marker and fully consume the outer codec/AEAD.
    pub(super) fn finish(mut self) -> io::Result<R> {
        if self.header_length()? != 0 {
            return Err(invalid("unexpected extra archive entry"));
        }
        if self.inner.read(&mut [0])? != 0 {
            return Err(invalid("trailing archive data"));
        }
        Ok(self.inner.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::super::repository::{FileTimestamp, UnixMetadata};
    use super::*;
    use proptest::{
        prelude::*,
        test_runner::{Config, RngSeed},
    };

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn entry(path: &str, kind: ManifestEntryKind) -> ManifestEntry {
        ManifestEntry {
            relative_path: path.into(),
            metadata: UnixMetadata {
                uid: 1000,
                gid: 1000,
                mode: 0o750,
                atime: FileTimestamp {
                    seconds: -1,
                    nanoseconds: 123,
                },
                mtime: FileTimestamp {
                    seconds: 1,
                    nanoseconds: 999_999_999,
                },
            },
            kind,
        }
    }

    fn pack(data: &[u8]) -> io::Result<(Vec<u8>, ManifestEntry)> {
        let file = entry(
            "目录/file.bin",
            ManifestEntryKind::File {
                original_size: data.len() as u64,
            },
        );
        let mut writer = Writer::new(Vec::new())?;
        writer.append(&file, &mut &data[..])?;
        Ok((writer.finish()?, file))
    }

    fn unpack(bytes: &[u8], file: &ManifestEntry) -> io::Result<Vec<u8>> {
        let mut reader = Reader::new(bytes)?;
        reader.expect_entry(file)?;
        let ManifestEntryKind::File { original_size } = &file.kind else {
            return Err(invalid("expected file"));
        };
        let mut output = Vec::new();
        reader.read_file(&mut output, *original_size)?;
        reader.finish()?;
        Ok(output)
    }

    #[test]
    fn golden_empty_archive_and_mixed_entries() -> TestResult {
        assert_eq!(Writer::new(Vec::new())?.finish()?, b"BAKARC\0\x02\0\0\0\0");
        Reader::new(&b"BAKARC\0\x02\0\0\0\0"[..])?.finish()?;
        let entries = [
            entry("empty", ManifestEntryKind::Directory),
            entry(
                "link",
                ManifestEntryKind::Symlink {
                    target: "../missing".into(),
                },
            ),
            entry("zero", ManifestEntryKind::File { original_size: 0 }),
            entry(
                "binary",
                ManifestEntryKind::File {
                    original_size: 300_001,
                },
            ),
        ];
        let binary: Vec<u8> = (0..=255).cycle().take(300_001).collect();
        let mut writer = Writer::new(Vec::new())?;
        for file in &entries {
            writer.append(
                file,
                &mut if file.relative_path == std::path::Path::new("binary") {
                    binary.as_slice()
                } else {
                    &[]
                },
            )?;
        }
        let bytes = writer.finish()?;
        let mut reader = Reader::new(bytes.as_slice())?;
        for file in &entries {
            reader.expect_entry(file)?;
            if let ManifestEntryKind::File { original_size } = &file.kind {
                let mut output = Vec::new();
                reader.read_file(&mut output, *original_size)?;
                assert_eq!(
                    output.as_slice(),
                    if *original_size == 0 {
                        &[]
                    } else {
                        binary.as_slice()
                    }
                );
            }
        }
        reader.finish()?;
        Ok(())
    }

    #[test]
    fn truncation_corruption_and_trailing_data_are_rejected() -> TestResult {
        let (bytes, file) = pack(b"payload")?;
        for end in 0..bytes.len() {
            assert!(
                unpack(&bytes[..end], &file).is_err(),
                "accepted truncation at {end}"
            );
        }
        for position in [0, 8, 12, bytes.len() - 1] {
            let mut damaged = bytes.clone();
            damaged[position] ^= 1;
            assert!(
                unpack(&damaged, &file).is_err(),
                "accepted corruption at {position}"
            );
        }
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(unpack(&trailing, &file).is_err());
        let mut oversized = bytes.clone();
        oversized[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(unpack(&oversized, &file).is_err());
        let mut extra_entry = bytes.clone();
        let end = extra_entry.len() - 4;
        extra_entry[end..].copy_from_slice(&1u32.to_le_bytes());
        assert!(unpack(&extra_entry, &file).is_err());
        let empty = Writer::new(Vec::new())?.finish()?;
        assert!(unpack(&empty, &file).is_err());
        Ok(())
    }

    #[test]
    fn metadata_mismatches_and_source_size_changes_are_rejected() -> TestResult {
        let (bytes, mut file) = pack(b"payload")?;
        file.metadata.mode ^= 1;
        assert!(unpack(&bytes, &file).is_err());
        for size in [6, 8] {
            let file = entry(
                "file",
                ManifestEntryKind::File {
                    original_size: size,
                },
            );
            let mut writer = Writer::new(Vec::new())?;
            assert!(writer.append(&file, &mut &b"payload"[..]).is_err());
        }
        let huge = entry(&"x".repeat(MAX_HEADER_SIZE), ManifestEntryKind::Directory);
        assert!(
            Writer::new(Vec::new())?
                .append(&huge, &mut io::empty())
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn short_io_and_write_failures_are_observable() -> TestResult {
        struct ShortIo<T>(T);
        impl<T: Read> Read for ShortIo<T> {
            fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
                let length = buffer.len().min(3);
                self.0.read(&mut buffer[..length])
            }
        }
        impl<T: Write> Write for ShortIo<T> {
            fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
                self.0.write(&buffer[..buffer.len().min(3)])
            }
            fn flush(&mut self) -> io::Result<()> {
                self.0.flush()
            }
        }
        let data = vec![42; 131_073];
        let file = entry(
            "file",
            ManifestEntryKind::File {
                original_size: data.len() as u64,
            },
        );
        let mut writer = Writer::new(ShortIo(Vec::new()))?;
        writer.append(&file, &mut ShortIo(data.as_slice()))?;
        let bytes = writer.finish()?.0;
        let mut reader = Reader::new(ShortIo(bytes.as_slice()))?;
        reader.expect_entry(&file)?;
        let ManifestEntryKind::File { original_size } = &file.kind else {
            return Err("expected file".into());
        };
        let mut output = ShortIo(Vec::new());
        reader.read_file(&mut output, *original_size)?;
        reader.finish()?;
        assert_eq!(output.0, data);

        // A zero-capacity output reports WriteZero, including errors delayed by buffering.
        let mut writer = Writer::new(&mut [][..])?;
        writer.append(
            &entry("directory", ManifestEntryKind::Directory),
            &mut io::empty(),
        )?;
        assert_eq!(
            writer
                .finish()
                .err()
                .ok_or("lost buffered write error")?
                .kind(),
            io::ErrorKind::WriteZero
        );
        let mut reader = Reader::new(bytes.as_slice())?;
        reader.expect_entry(&file)?;
        assert_eq!(
            reader
                .read_file(&mut &mut [][..], *original_size)
                .err()
                .ok_or("lost restore write error")?
                .kind(),
            io::ErrorKind::WriteZero
        );
        Ok(())
    }

    proptest! {
        #![proptest_config(Config { cases: 32, rng_seed: RngSeed::Fixed(0xBAA2_C001), ..Config::default() })]
        #[test]
        fn arbitrary_file_bytes_round_trip(data in proptest::collection::vec(any::<u8>(), 0..16385)) {
            let (bytes, file) = pack(&data)?;
            prop_assert_eq!(unpack(&bytes, &file)?, data);
        }
    }
}
