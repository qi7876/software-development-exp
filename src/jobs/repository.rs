use super::{
    JobError, archive,
    content::{BackupOptions, Checksum, CompressionOptions, Content, Encryption, Keyring},
    filesystem::{get_absolute_directory_path, sync_directory},
};
use http::StatusCode;
use std::{
    collections::BTreeMap,
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
};

const MAGIC: &[u8; 8] = b"BAKFILE\x01";
const FOOTER: &[u8; 8] = b"BAKEND\0\x01";
const TRAILER_SIZE: u64 = 48;
const MAX_METADATA_SIZE: u64 = 64 * 1024 * 1024;
pub(super) const FORMAT_VERSION: u32 = 10;
const LEGACY_MANIFEST_VERSION: u32 = 9;

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct BackupStats {
    pub(super) entries_created: i64,
    pub(super) skipped_unsupported: i64,
    pub(super) skipped_filtered: i64,
}

#[derive(serde::Serialize)]
pub(crate) struct RepoBackupInfo {
    pub(super) backup_id: String,
    pub(super) created_at: String,
    pub(super) compression: Option<CompressionOptions>,
    pub(super) encryption: Option<Encryption>,
}

// All metadata needed to list and restore this archive lives in its footer.
#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct Manifest {
    pub(super) format_version: u32,
    pub(super) backup_id: String,
    pub(super) created_at: String,
    pub(super) source: PathBuf,
    pub(super) compression: Option<CompressionOptions>,
    pub(super) encryption: Option<Encryption>,
    pub(super) checksum_algorithm: String,
    #[serde(skip)]
    pub(super) archive_checksum: String,
    pub(super) root_metadata: UnixMetadata,
    pub(super) backup_stats: BackupStats,
    pub(super) entries: Vec<ManifestEntry>,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ManifestEntry {
    pub(super) relative_path: PathBuf,
    pub(super) metadata: UnixMetadata,
    pub(super) kind: ManifestEntryKind,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct UnixMetadata {
    pub(super) uid: u32,
    pub(super) gid: u32,
    pub(super) mode: u32,
    pub(super) atime: FileTimestamp,
    pub(super) mtime: FileTimestamp,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(super) struct FileTimestamp {
    pub(super) seconds: i64,
    pub(super) nanoseconds: u32,
}

#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(super) enum ManifestEntryKind {
    File { original_size: u64 },
    Directory,
    Symlink { target: PathBuf },
    Fifo,
    CharacterDevice { major: u32, minor: u32 },
    BlockDevice { major: u32, minor: u32 },
    HardLink { target: PathBuf },
}

impl UnixMetadata {
    #[cfg(unix)]
    pub(super) fn read(path: &Path) -> Result<Self, JobError> {
        Self::inspect(path).map(|(_, metadata)| metadata)
    }

    #[cfg(unix)]
    pub(super) fn inspect(path: &Path) -> Result<(std::fs::Metadata, Self), JobError> {
        use std::os::unix::fs::MetadataExt;
        let metadata = std::fs::symlink_metadata(path).map_err(|error| {
            JobError::internal(format!(
                "cannot read metadata for {}: {error}",
                path.display()
            ))
        })?;

        let nanoseconds = |value: i64| -> Result<u32, JobError> {
            u32::try_from(value)
                .ok()
                .filter(|value| *value < 1_000_000_000)
                .ok_or_else(|| {
                    JobError::internal(format!(
                        "invalid timestamp nanoseconds for {}: {value}",
                        path.display()
                    ))
                })
        };

        let unix = Self {
            uid: metadata.uid(),
            gid: metadata.gid(),
            mode: metadata.mode() & 0o7777,
            atime: FileTimestamp {
                seconds: metadata.atime(),
                nanoseconds: nanoseconds(metadata.atime_nsec())?,
            },
            mtime: FileTimestamp {
                seconds: metadata.mtime(),
                nanoseconds: nanoseconds(metadata.mtime_nsec())?,
            },
        };
        Ok((metadata, unix))
    }
}

fn validate_id(id: &str) -> Result<(), JobError> {
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\']) {
        return Err(JobError::new(StatusCode::BAD_REQUEST, "invalid backup ID"));
    }
    Ok(())
}

impl Manifest {
    #[cfg(test)]
    pub(super) fn read(repository: &Path, backup_id: &str) -> Result<Self, JobError> {
        validate_id(backup_id)?;
        Ok(ArchiveFile::read(repository, backup_id)?.manifest)
    }

    fn validate(&self) -> Result<(), JobError> {
        validate_id(&self.backup_id)?;
        if !matches!(
            self.format_version,
            LEGACY_MANIFEST_VERSION | FORMAT_VERSION
        ) || self.checksum_algorithm != "sha256"
        {
            return Err(JobError::internal(
                "unsupported self-contained backup format or checksum algorithm",
            ));
        }
        chrono::DateTime::parse_from_rfc3339(&self.created_at)
            .map_err(|e| JobError::internal(format!("invalid backup timestamp: {e}")))?;
        if let Some(compression) = &self.compression {
            compression.validate()?;
        }
        if let Some(encryption) = &self.encryption
            && (encryption.key_id.trim().is_empty()
                || encryption.fingerprint.len() != 64
                || hex::decode(&encryption.fingerprint).is_err())
        {
            return Err(JobError::internal("invalid encryption metadata"));
        }
        if self.backup_stats.entries_created
            != i64::try_from(self.entries.len()).map_err(|e| JobError::internal(e.to_string()))?
            || self.backup_stats.skipped_filtered < 0
            || self.backup_stats.skipped_unsupported < 0
        {
            return Err(JobError::internal("invalid backup statistics"));
        }
        let valid_metadata = |metadata: &UnixMetadata| -> Result<(), JobError> {
            if metadata.mode > 0o7777
                || metadata.atime.nanoseconds >= 1_000_000_000
                || metadata.mtime.nanoseconds >= 1_000_000_000
            {
                return Err(JobError::internal("invalid Unix metadata"));
            }
            Ok(())
        };
        valid_metadata(&self.root_metadata)?;
        let mut seen: BTreeMap<&Path, &ManifestEntry> = BTreeMap::new();
        for entry in &self.entries {
            let path = &entry.relative_path;
            if path.as_os_str().is_empty()
                || !path.components().all(|c| matches!(c, Component::Normal(_)))
            {
                return Err(JobError::internal(format!(
                    "invalid backup entry path: {}",
                    path.display()
                )));
            }
            if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty())
                && !seen
                    .get(parent)
                    .is_some_and(|parent| matches!(parent.kind, ManifestEntryKind::Directory))
            {
                return Err(JobError::internal(format!(
                    "invalid backup entry parent: {}",
                    path.display()
                )));
            }
            if self.format_version == LEGACY_MANIFEST_VERSION
                && !matches!(
                    entry.kind,
                    ManifestEntryKind::File { .. }
                        | ManifestEntryKind::Directory
                        | ManifestEntryKind::Symlink { .. }
                )
            {
                return Err(JobError::internal(
                    "invalid entry kind for backup format v9",
                ));
            }
            if let ManifestEntryKind::HardLink { target } = &entry.kind
                && !seen.get(target.as_path()).is_some_and(|original| {
                    !matches!(
                        original.kind,
                        ManifestEntryKind::Directory | ManifestEntryKind::HardLink { .. }
                    ) && original.metadata == entry.metadata
                })
            {
                return Err(JobError::internal(format!(
                    "invalid hard link target {} for backup entry {}",
                    target.display(),
                    path.display()
                )));
            }
            if seen.insert(path, entry).is_some() {
                return Err(JobError::internal("duplicate backup entry path"));
            }
            valid_metadata(&entry.metadata)?;
        }
        Ok(())
    }
}

pub(super) struct ArchiveFile {
    pub(super) file: File,
    pub(super) manifest: Manifest,
    pub(super) payload_len: u64,
    pub(super) metadata_len: u64,
}

impl ArchiveFile {
    pub(super) fn read(repository: &Path, backup_id: &str) -> Result<Self, JobError> {
        validate_id(backup_id)?;
        check_existing_format(repository)?;
        Self::open(&archive::path(repository, backup_id))
    }

    pub(super) fn open(path: &Path) -> Result<Self, JobError> {
        let id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| JobError::internal("invalid backup filename"))?;
        Self::open_expected(path, id)
    }

    pub(super) fn open_expected(path: &Path, backup_id: &str) -> Result<Self, JobError> {
        validate_id(backup_id)?;
        if !path.try_exists()? {
            return Err(JobError::new(StatusCode::NOT_FOUND, "backup not found"));
        }
        let read = || -> Result<Self, JobError> {
            if !std::fs::symlink_metadata(path)?.is_file() {
                return Err(JobError::internal("backup must be a regular file"));
            }
            let mut file = File::open(path)?;
            let length = file.metadata()?.len();
            if length < 8 + TRAILER_SIZE {
                return Err(JobError::internal("truncated self-contained backup"));
            }
            let mut magic = [0; 8];
            file.read_exact(&mut magic)?;
            if &magic != MAGIC {
                return Err(JobError::internal(
                    "unsupported backup format; legacy archives require the prior binary and repository database",
                ));
            }
            file.seek(SeekFrom::End(-(TRAILER_SIZE as i64)))?;
            let mut bytes = [0; 8];
            file.read_exact(&mut bytes)?;
            let metadata_len = u64::from_le_bytes(bytes);
            file.read_exact(&mut bytes)?;
            if &bytes != FOOTER {
                return Err(JobError::internal("invalid or incomplete backup footer"));
            }
            let mut digest = [0; 32];
            file.read_exact(&mut digest)?;
            if metadata_len == 0
                || metadata_len > MAX_METADATA_SIZE
                || metadata_len > length - 8 - TRAILER_SIZE
            {
                return Err(JobError::internal("invalid backup metadata length"));
            }
            let payload_len = length - 8 - TRAILER_SIZE - metadata_len;
            file.seek(SeekFrom::Start(8 + payload_len))?;
            let mut metadata = vec![0; metadata_len as usize];
            file.read_exact(&mut metadata)?;
            let mut manifest: Manifest = serde_json::from_slice(&metadata)?;
            manifest.archive_checksum = hex::encode(digest);
            manifest.validate()?;
            if manifest.backup_id != backup_id {
                return Err(JobError::internal(
                    "backup filename does not match its stored ID",
                ));
            }
            file.rewind()?;
            Ok(Self {
                file,
                manifest,
                payload_len,
                metadata_len,
            })
        };
        read().map_err(|error| {
            JobError::internal(format!(
                "cannot read backup {}: {}",
                path.display(),
                error.message
            ))
        })
    }
    pub(super) fn finish_checksum(&self, mut input: Checksum<File>) -> Result<(), JobError> {
        let length = self.metadata_len + 16;
        if io::copy(&mut (&mut input).take(length), &mut io::sink())? != length {
            return Err(JobError::internal("truncated backup metadata"));
        }
        if input.finish().1 != self.manifest.archive_checksum {
            return Err(JobError::internal("archive payload checksum mismatch"));
        }
        Ok(())
    }

    // Recovery validates the same framing, entry sizes, AEAD, and digest as restore,
    // but consumes file data into a sink without creating destination files.
    pub(super) fn verify(&self, keys: &Keyring) -> Result<(), JobError> {
        let options = BackupOptions {
            compression: self.manifest.compression.clone(),
            encryption: self.manifest.encryption.clone(),
        };
        let content = Content::new(&options, keys, &self.manifest.backup_id)?;
        let mut file = self.file.try_clone()?;
        file.rewind()?;
        let mut input = Checksum::new(file);
        let mut magic = [0; 8];
        input.read_exact(&mut magic)?;
        let name = archive::file_name(&self.manifest.backup_id);
        let decoder = content.reader((&mut input).take(self.payload_len), Path::new(&name))?;
        let mut reader = archive::Reader::new(decoder)?;
        for entry in &self.manifest.entries {
            reader.expect_entry(entry)?;
            if let ManifestEntryKind::File { original_size } = entry.kind {
                reader.read_file(&mut io::sink(), original_size)?;
            }
        }
        reader.finish()?.finish()?;
        self.finish_checksum(input)
    }
}

pub(super) struct PendingArchive {
    output: Option<Checksum<File>>,
    temporary: PathBuf,
    destination: PathBuf,
}

impl PendingArchive {
    pub(super) fn create(repository: &Path, backup_id: &str) -> Result<Self, JobError> {
        validate_id(backup_id)?;
        let destination = archive::path(repository, backup_id);
        if destination.try_exists()? {
            return Err(JobError::new(
                StatusCode::CONFLICT,
                "backup ID already exists",
            ));
        }
        let temporary = repository.join(format!(".{backup_id}.{}.partial", uuid::Uuid::new_v4()));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(&temporary)?;
        let mut pending = Self {
            output: Some(Checksum::new(file)),
            temporary,
            destination,
        };
        pending.output()?.write_all(MAGIC)?;
        Ok(pending)
    }

    pub(super) fn output(&mut self) -> io::Result<&mut Checksum<File>> {
        self.output
            .as_mut()
            .ok_or_else(|| io::Error::other("backup output is already finalized"))
    }

    pub(super) fn publish(mut self, manifest: &Manifest) -> Result<(), JobError> {
        manifest.validate()?;
        if self.destination.file_stem().and_then(|name| name.to_str())
            != Some(manifest.backup_id.as_str())
        {
            return Err(JobError::internal(
                "publication ID does not match backup metadata",
            ));
        }
        let bytes = serde_json::to_vec(manifest)?;
        if bytes.len() as u64 > MAX_METADATA_SIZE {
            return Err(JobError::internal("backup metadata exceeds 64 MiB"));
        }
        let output = self.output()?;
        output.write_all(&bytes)?;
        output.write_all(&(bytes.len() as u64).to_le_bytes())?;
        output.write_all(FOOTER)?;
        let (mut file, checksum) = self
            .output
            .take()
            .ok_or_else(|| JobError::internal("backup output is already finalized"))?
            .finish();
        file.write_all(&hex::decode(checksum).map_err(|e| JobError::internal(e.to_string()))?)?;
        file.sync_all()?;
        // A hard link publishes the complete inode without replacing an existing backup ID.
        std::fs::hard_link(&self.temporary, &self.destination).map_err(|e| {
            JobError::new(
                if e.kind() == io::ErrorKind::AlreadyExists {
                    StatusCode::CONFLICT
                } else {
                    StatusCode::INTERNAL_SERVER_ERROR
                },
                format!("cannot publish backup {}: {e}", self.destination.display()),
            )
        })?;
        let parent = self
            .destination
            .parent()
            .ok_or_else(|| JobError::internal("backup has no repository directory"))?;
        sync_directory(parent)?;
        std::fs::remove_file(&self.temporary)?;
        sync_directory(parent)?;
        Ok(())
    }
}

impl Drop for PendingArchive {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.temporary)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!(
                "cannot remove partial backup {}: {error}",
                self.temporary.display()
            );
        }
    }
}

pub(super) fn check_existing_format(repository: &Path) -> Result<(), JobError> {
    if repository.join("repository.sqlite3").try_exists()? {
        return Err(JobError::internal(
            "legacy repository database detected; use a new repository for self-contained backups and the prior binary for old backups",
        ));
    }
    Ok(())
}

pub(super) fn list(repository: &Path) -> Result<Vec<RepoBackupInfo>, JobError> {
    let directory = get_absolute_directory_path(repository)?;
    check_existing_format(&directory)?;
    let mut summaries = Vec::new();
    for entry in std::fs::read_dir(&directory)? {
        let path = entry?.path();
        if path.extension() != Some(std::ffi::OsStr::new("bak")) {
            continue;
        }
        let manifest = ArchiveFile::open(&path)?.manifest;
        summaries.push(RepoBackupInfo {
            backup_id: manifest.backup_id,
            created_at: manifest.created_at,
            compression: manifest.compression,
            encryption: manifest.encryption,
        });
    }
    summaries.sort_by(|a, b| {
        b.created_at
            .cmp(&a.created_at)
            .then_with(|| a.backup_id.cmp(&b.backup_id))
    });
    Ok(summaries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{TestDirectory, rewrite_backup_metadata};
    use proptest::{
        prelude::*,
        test_runner::{Config, RngSeed},
    };
    use sha2::{Digest, Sha256};
    type TestResult = Result<(), Box<dyn std::error::Error>>;

    fn metadata(id: &str, time: &str) -> Manifest {
        Manifest {
            format_version: FORMAT_VERSION,
            backup_id: id.into(),
            created_at: time.into(),
            source: "/source".into(),
            compression: None,
            encryption: None,
            checksum_algorithm: "sha256".into(),
            archive_checksum: String::new(),
            root_metadata: UnixMetadata {
                uid: 0,
                gid: 0,
                mode: 0o755,
                atime: FileTimestamp {
                    seconds: 0,
                    nanoseconds: 0,
                },
                mtime: FileTimestamp {
                    seconds: 0,
                    nanoseconds: 0,
                },
            },
            backup_stats: BackupStats {
                entries_created: 0,
                skipped_filtered: 0,
                skipped_unsupported: 0,
            },
            entries: Vec::new(),
        }
    }

    fn publish(repository: &Path, manifest: &Manifest) -> Result<(), JobError> {
        let mut pending = PendingArchive::create(repository, &manifest.backup_id)?;
        pending.output()?.write_all(b"encoded-payload")?;
        pending.publish(manifest)
    }

    #[test]
    fn device_entries_use_headers_without_payload() -> TestResult {
        let directory = TestDirectory::new()?;
        let mut manifest = metadata("devices", "2026-11-16T00:00:00Z");
        manifest.entries = [
            (
                "character",
                ManifestEntryKind::CharacterDevice { major: 1, minor: 3 },
            ),
            (
                "block",
                ManifestEntryKind::BlockDevice {
                    major: 259,
                    minor: 65_537,
                },
            ),
        ]
        .into_iter()
        .map(|(path, kind)| ManifestEntry {
            relative_path: path.into(),
            metadata: manifest.root_metadata.clone(),
            kind,
        })
        .collect();
        manifest.backup_stats.entries_created = 2;
        let mut pending = PendingArchive::create(directory.path(), &manifest.backup_id)?;
        let mut writer = archive::Writer::new(pending.output()?)?;
        for entry in &manifest.entries {
            writer.append(entry, &mut io::empty())?;
        }
        writer.finish()?;
        pending.publish(&manifest)?;
        let backup = ArchiveFile::read(directory.path(), &manifest.backup_id)?;
        assert_eq!(backup.manifest.entries, manifest.entries);
        backup.verify(&Keyring::default())?;
        Ok(())
    }

    #[test]
    fn one_file_carries_metadata_and_integrity_into_an_empty_repository() -> TestResult {
        let original = TestDirectory::new()?;
        let destination = TestDirectory::new()?;
        let manifest = metadata("copy-me", "2026-11-16T00:00:00Z");
        publish(original.path(), &manifest)?;
        let name = archive::file_name(&manifest.backup_id);
        std::fs::copy(original.path().join(&name), destination.path().join(&name))?;
        let read = Manifest::read(destination.path(), "copy-me")?;
        assert_eq!(
            serde_json::to_value(&manifest)?,
            serde_json::to_value(&read)?
        );
        let bytes = std::fs::read(destination.path().join(name))?;
        assert_eq!(&bytes[..8], MAGIC);
        assert_eq!(&bytes[bytes.len() - 40..bytes.len() - 32], FOOTER);
        assert_eq!(
            read.archive_checksum,
            hex::encode(Sha256::digest(&bytes[..bytes.len() - 32]))
        );
        assert_eq!(list(destination.path())?[0].backup_id, "copy-me");
        assert_eq!(std::fs::read_dir(destination.path())?.count(), 1);
        assert!(!destination.path().join("repository.sqlite3").exists());
        Ok(())
    }

    #[test]
    fn publication_is_exclusive_even_when_two_writers_start_together() -> TestResult {
        let directory = TestDirectory::new()?;
        let manifest = metadata("same-id", "2026-11-16T00:00:00Z");
        let mut first = PendingArchive::create(directory.path(), "same-id")?;
        let mut second = PendingArchive::create(directory.path(), "same-id")?;
        assert!(list(directory.path())?.is_empty());
        first.output()?.write_all(b"first")?;
        second.output()?.write_all(b"second")?;
        first.publish(&manifest)?;
        let before = std::fs::read(directory.path().join("same-id.bak"))?;
        assert_eq!(
            second
                .publish(&manifest)
                .err()
                .ok_or("overwrote published backup")?
                .code,
            StatusCode::CONFLICT
        );
        assert_eq!(std::fs::read(directory.path().join("same-id.bak"))?, before);
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 1);
        Ok(())
    }

    #[test]
    fn failed_or_abandoned_writers_do_not_publish_or_remove_unowned_partials() -> TestResult {
        let directory = TestDirectory::new()?;
        let orphan = directory.path().join(".old.partial");
        std::fs::write(&orphan, b"preserve")?;
        {
            let mut pending = PendingArchive::create(directory.path(), "unfinished")?;
            pending.output()?.write_all(b"partial")?;
        }
        let mut invalid = metadata("invalid", "2026-11-16T00:00:00Z");
        invalid.backup_stats.entries_created = 1;
        assert!(publish(directory.path(), &invalid).is_err());
        let mismatch = PendingArchive::create(directory.path(), "wrong-id")?;
        assert!(
            mismatch
                .publish(&metadata("other-id", "2026-11-16T00:00:00Z"))
                .is_err()
        );
        assert!(list(directory.path())?.is_empty());
        assert_eq!(std::fs::read_dir(directory.path())?.count(), 1);
        assert_eq!(std::fs::read(orphan)?, b"preserve");
        Ok(())
    }

    #[test]
    fn listings_preserve_order_and_require_no_key_material() -> TestResult {
        use super::super::content::{CompressionOptions, EncryptionAlgorithm, ZstdOptions};
        let directory = TestDirectory::new()?;
        for (id, time) in [
            ("b", "2026-11-16T00:00:00Z"),
            ("a", "2026-11-16T00:00:00Z"),
            ("new", "2026-11-17T00:00:00Z"),
        ] {
            let mut manifest = metadata(id, time);
            manifest.compression = Some(CompressionOptions::Zstd(ZstdOptions { level: 7 }));
            manifest.encryption = Some(Encryption {
                algorithm: EncryptionAlgorithm::Aes256Gcm,
                key_id: "external-key".into(),
                fingerprint: "ab".repeat(32),
            });
            publish(directory.path(), &manifest)?;
        }
        let items = list(directory.path())?;
        assert_eq!(
            items
                .iter()
                .map(|m| m.backup_id.as_str())
                .collect::<Vec<_>>(),
            ["new", "a", "b"]
        );
        assert_eq!(
            items[0]
                .encryption
                .as_ref()
                .ok_or("missing encryption")?
                .key_id,
            "external-key"
        );
        assert_eq!(serde_json::to_value(&items[0].compression)?["level"], 7);
        Ok(())
    }

    #[test]
    fn invalid_footer_lengths_versions_and_trailing_bytes_are_rejected() -> TestResult {
        let directory = TestDirectory::new()?;
        let path = directory.path().join("test.bak");
        publish(directory.path(), &metadata("test", "2026-11-16T00:00:00Z"))?;
        let valid = std::fs::read(&path)?;
        let mut cases = vec![
            vec![],
            valid[..7].to_vec(),
            valid[..valid.len() - 1].to_vec(),
            [valid.as_slice(), &[0]].concat(),
        ];
        let mut version = valid.clone();
        version[7] = 99;
        cases.push(version);
        for length in [0, MAX_METADATA_SIZE + 1, u64::MAX] {
            let mut bytes = valid.clone();
            let start = bytes.len() - TRAILER_SIZE as usize;
            bytes[start..start + 8].copy_from_slice(&length.to_le_bytes());
            cases.push(bytes);
        }
        for bytes in cases {
            std::fs::write(&path, &bytes)?;
            assert!(ArchiveFile::open(&path).is_err());
            assert!(list(directory.path()).is_err());
            assert_eq!(std::fs::read(&path)?, bytes);
        }
        Ok(())
    }

    #[test]
    fn invalid_metadata_is_rejected_before_any_restoration() -> TestResult {
        let directory = TestDirectory::new()?;
        let path = directory.path().join("test.bak");
        publish(directory.path(), &metadata("test", "2026-11-16T00:00:00Z"))?;
        let original = std::fs::read(&path)?;
        for (field, value) in [
            ("format_version", serde_json::json!(8)),
            ("created_at", serde_json::json!("invalid")),
            ("compression", serde_json::json!({"algorithm":"unknown"})),
            (
                "encryption",
                serde_json::json!({"algorithm":"aes-256-gcm","key_id":"key","fingerprint":"invalid"}),
            ),
            (
                "backup_stats",
                serde_json::json!({"entries_created":1,"skipped_filtered":0,"skipped_unsupported":0}),
            ),
            ("backup_id", serde_json::json!("../escape")),
        ] {
            std::fs::write(&path, &original)?;
            rewrite_backup_metadata(&path, |metadata| {
                metadata[field] = value;
            })?;
            assert!(
                ArchiveFile::open(&path).is_err(),
                "accepted invalid {field}"
            );
        }
        Ok(())
    }

    #[test]
    fn legacy_databases_and_invalid_ids_are_preserved_without_writes() -> TestResult {
        let directory = TestDirectory::new()?;
        for id in ["", ".", "..", "../escape", "a/b", "a\\b"] {
            assert!(PendingArchive::create(directory.path(), id).is_err());
        }
        assert!(list(directory.path())?.is_empty());
        let database = directory.path().join("repository.sqlite3");
        std::fs::write(&database, b"legacy database")?;
        assert!(check_existing_format(directory.path()).is_err());
        assert!(list(directory.path()).is_err());
        assert_eq!(std::fs::read(database)?, b"legacy database");
        Ok(())
    }

    proptest! {
        #![proptest_config(Config { cases: 32, failure_persistence: None, rng_seed: RngSeed::Fixed(0xA11F11E), ..Config::default() })]
        #[test]
        fn arbitrary_bytes_are_rejected_without_panicking(bytes in proptest::collection::vec(any::<u8>(), 0..8193)) {
            let directory = TestDirectory::new()?;
            let path = directory.path().join("random.bak");
            std::fs::write(&path, &bytes)?;
            prop_assert!(ArchiveFile::open(&path).is_err());
        }
    }
}
