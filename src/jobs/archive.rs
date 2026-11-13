//! Sequential archive framing, independent of compression and encryption.
use super::repository::{ManifestEntry, ManifestEntryKind, UnixMetadata};
use sha2::{Digest, Sha256};
use std::{
    io::{self, BufReader, BufWriter, Read, Write},
    path::PathBuf,
};

pub(super) const FILE_NAME: &str = "archive.bak";
const MAGIC: &[u8; 8] = b"BAKARC\0\x01";
const MAX_HEADER_SIZE: usize = 1024 * 1024;

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Header {
    relative_path: PathBuf,
    metadata: UnixMetadata,
    kind: EntryKind,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
enum EntryKind {
    File { original_size: u64 },
    Directory,
    Symlink { target: PathBuf },
}

impl Header {
    fn from_entry(entry: &ManifestEntry) -> Self {
        Self {
            relative_path: entry.relative_path.clone(),
            metadata: entry.metadata.clone(),
            kind: match &entry.kind {
                ManifestEntryKind::File { original_size, .. } => EntryKind::File {
                    original_size: *original_size,
                },
                ManifestEntryKind::Directory => EntryKind::Directory,
                ManifestEntryKind::Symlink { target } => EntryKind::Symlink {
                    target: target.clone(),
                },
            },
        }
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// Copy exactly one file, leaving the next record unread. Memory is bounded.
fn copy_file(reader: &mut impl Read, writer: &mut impl Write, size: u64) -> io::Result<[u8; 32]> {
    let mut remaining = size;
    let mut buffer = [0; 128 * 1024];
    let mut hasher = Sha256::new();
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
        hasher.update(&buffer[..count]);
        remaining -= count as u64;
    }
    Ok(hasher.finalize().into())
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

    /// File checksums are written after the data, so packing needs only one pass.
    pub(super) fn append(
        &mut self,
        entry: &mut ManifestEntry,
        source: &mut impl Read,
    ) -> io::Result<()> {
        let header = serde_json::to_vec(&Header::from_entry(entry))?;
        if header.len() > MAX_HEADER_SIZE {
            return Err(invalid("archive entry header exceeds 1 MiB"));
        }
        self.inner.write_all(&(header.len() as u32).to_le_bytes())?;
        self.inner.write_all(&header)?;
        if let ManifestEntryKind::File {
            original_size,
            checksum,
        } = &mut entry.kind
        {
            let digest = copy_file(source, &mut self.inner, *original_size)?;
            if source.read(&mut [0])? != 0 {
                return Err(invalid("source file grew while packing archive"));
            }
            self.inner.write_all(&digest)?;
            *checksum = hex::encode(digest);
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

#[cfg(test)]
mod tests {
    use super::super::repository::FileTimestamp;
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
        let mut file = entry(
            "目录/file.bin",
            ManifestEntryKind::File {
                original_size: data.len() as u64,
                checksum: String::new(),
            },
        );
        let mut writer = Writer::new(Vec::new())?;
        writer.append(&mut file, &mut &data[..])?;
        Ok((writer.finish()?, file))
    }

    fn unpack(bytes: &[u8], file: &ManifestEntry) -> io::Result<Vec<u8>> {
        let mut reader = Reader::new(bytes)?;
        reader.expect_entry(file)?;
        let ManifestEntryKind::File {
            original_size,
            checksum,
        } = &file.kind
        else {
            return Err(invalid("expected file"));
        };
        let mut output = Vec::new();
        reader.read_file(&mut output, *original_size, checksum)?;
        reader.finish()?;
        Ok(output)
    }

    #[test]
    fn golden_empty_archive_and_mixed_entries() -> TestResult {
        assert_eq!(Writer::new(Vec::new())?.finish()?, b"BAKARC\0\x01\0\0\0\0");
        Reader::new(&b"BAKARC\0\x01\0\0\0\0"[..])?.finish()?;
        let mut entries = [
            entry("empty", ManifestEntryKind::Directory),
            entry(
                "link",
                ManifestEntryKind::Symlink {
                    target: "../missing".into(),
                },
            ),
            entry(
                "zero",
                ManifestEntryKind::File {
                    original_size: 0,
                    checksum: String::new(),
                },
            ),
            entry(
                "binary",
                ManifestEntryKind::File {
                    original_size: 300_001,
                    checksum: String::new(),
                },
            ),
        ];
        let binary: Vec<u8> = (0..=255).cycle().take(300_001).collect();
        let mut writer = Writer::new(Vec::new())?;
        for file in &mut entries {
            writer.append(
                file,
                &mut if file.relative_path == PathBuf::from("binary") {
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
            if let ManifestEntryKind::File {
                original_size,
                checksum,
            } = &file.kind
            {
                let mut output = Vec::new();
                reader.read_file(&mut output, *original_size, checksum)?;
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
        for position in [0, 8, 12, bytes.len() - 4 - 32 - 1, bytes.len() - 5] {
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
            let mut file = entry(
                "file",
                ManifestEntryKind::File {
                    original_size: size,
                    checksum: String::new(),
                },
            );
            let mut writer = Writer::new(Vec::new())?;
            assert!(writer.append(&mut file, &mut &b"payload"[..]).is_err());
        }
        let mut huge = entry(&"x".repeat(MAX_HEADER_SIZE), ManifestEntryKind::Directory);
        assert!(
            Writer::new(Vec::new())?
                .append(&mut huge, &mut io::empty())
                .is_err()
        );
        Ok(())
    }

    proptest! {
        #![proptest_config(Config { cases: 32, rng_seed: RngSeed::Fixed(0xBA_A2_C001), ..Config::default() })]
        #[test]
        fn arbitrary_file_bytes_round_trip(data in proptest::collection::vec(any::<u8>(), 0..16385)) {
            let (bytes, file) = pack(&data)?;
            prop_assert_eq!(unpack(&bytes, &file)?, data);
        }
    }
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
        let header: Header = serde_json::from_slice(&bytes)?;
        if header != Header::from_entry(entry) {
            return Err(invalid("archive entry differs from repository metadata"));
        }
        Ok(())
    }

    pub(super) fn read_file(
        &mut self,
        destination: &mut impl Write,
        size: u64,
        checksum: &str,
    ) -> io::Result<()> {
        let digest = copy_file(&mut self.inner, destination, size)?;
        let mut stored = [0; 32];
        self.inner.read_exact(&mut stored)?;
        if stored != digest || hex::encode(digest) != checksum {
            return Err(invalid("archive file checksum mismatch"));
        }
        Ok(())
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
