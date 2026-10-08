use http::StatusCode;
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

use crate::jobs::JobError;

#[derive(serde::Serialize)]
pub(crate) struct BackupSummary {
    pub(crate) backup_id: String,
    pub(crate) created_at: String,
}

pub(crate) fn list(_repository: &Path) -> Result<Vec<BackupSummary>, JobError> {
    Err(JobError::new(
        StatusCode::NOT_IMPLEMENTED,
        "backup listing is not implemented yet",
    ))
}

pub(crate) fn run(_source: &Path, _repository: &Path, _backup_id: &str) -> Result<(), JobError> {
    Err(JobError::new(
        StatusCode::NOT_IMPLEMENTED,
        "backup execution is not implemented yet",
    ))
}

// A manifest is written once inside the backup's private staging directory.
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Manifest writing will be wired into backup::run")
)]
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Manifest {
    pub(crate) format_version: u32,
    pub(crate) backup_id: String,
    pub(crate) created_at: String,
    pub(crate) source: PathBuf,
    pub(crate) compression: String,
    pub(crate) checksum_algorithm: String,
    pub(crate) entries: Vec<ManifestEntry>,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "Manifest entries will be collected by backup::run"
    )
)]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
pub(crate) enum ManifestEntry {
    File {
        path: PathBuf,
        original_size: u64,
        checksum: String,
    },
    Directory {
        path: PathBuf,
    },
}

#[cfg_attr(
    not(test),
    expect(dead_code, reason = "Manifest IO will be called by backup and restore")
)]
impl Manifest {
    pub(crate) fn read(directory: &Path) -> Result<Self, JobError> {
        let path = directory.join("manifest.json");
        let data = fs::read(&path).map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot read manifest {}: {error}", path.display()),
            )
        })?;
        let manifest: Self = serde_json::from_slice(&data).map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot parse manifest {}: {error}", path.display()),
            )
        })?;
        manifest.validate_format().map_err(|error| {
            JobError::new(
                error.code,
                format!("invalid manifest {}: {}", path.display(), error.message),
            )
        })?;
        Ok(manifest)
    }

    pub(crate) fn write(&self, directory: &Path) -> Result<(), JobError> {
        self.validate_format()?;
        let path = directory.join("manifest.json");
        let data = serde_json::to_vec_pretty(self).map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("cannot serialize manifest {}: {error}", path.display()),
            )
        })?;
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|error| {
                JobError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("cannot create manifest {}: {error}", path.display()),
                )
            })?;
        file.write_all(&data)
            .and_then(|()| file.sync_all())
            .map_err(|error| {
                JobError::new(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("cannot write manifest {}: {error}", path.display()),
                )
            })
    }

    fn validate_format(&self) -> Result<(), JobError> {
        if self.format_version != 1
            || self.compression != "zstd"
            || self.checksum_algorithm != "sha256"
        {
            return Err(JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "unsupported manifest format, compression or checksum algorithm",
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::error::Error;

    #[test]
    fn manifest_round_trip_and_invalid_files() -> Result<(), Box<dyn Error>> {
        let directory = std::env::temp_dir().join(format!("bak-manifest-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&directory)?;
        let manifest = Manifest {
            format_version: 1,
            backup_id: "backup-001".to_owned(),
            created_at: "2026-10-09T00:00:00Z".to_owned(),
            source: "/source".into(),
            compression: "zstd".to_owned(),
            checksum_algorithm: "sha256".to_owned(),
            entries: vec![
                ManifestEntry::Directory {
                    path: "empty".into(),
                },
                ManifestEntry::File {
                    path: "file.txt".into(),
                    original_size: 0,
                    checksum: "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
                        .to_owned(),
                },
            ],
        };
        assert!(Manifest::read(&directory).is_err());
        manifest.write(&directory).map_err(|error| error.message)?;
        let loaded = Manifest::read(&directory).map_err(|error| error.message)?;
        assert_eq!(
            serde_json::to_value(&manifest)?,
            serde_json::to_value(loaded)?
        );
        assert!(manifest.write(&directory).is_err());
        let path = directory.join("manifest.json");
        fs::write(&path, b"{")?;
        let error = Manifest::read(&directory)
            .err()
            .ok_or("accepted broken manifest")?;
        assert!(error.message.contains("cannot parse manifest"));
        assert!(error.message.contains("manifest.json"));
        let mut unsupported = manifest;
        unsupported.format_version = 2;
        fs::write(&path, serde_json::to_vec(&unsupported)?)?;
        assert!(Manifest::read(&directory).is_err());
        fs::remove_dir_all(directory)?;
        Ok(())
    }
}
