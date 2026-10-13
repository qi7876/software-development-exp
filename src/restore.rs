use crate::backup::{
    Manifest, ManifestEntry, get_absolute_directory_path, paths_overlap, sync_directory,
};
use crate::jobs::JobError;
use http::StatusCode;
use sha2::{Digest, Sha256};
use std::{
    io::{self, Read, Write},
    path::{Component, Path},
};

struct FileBackupResult {
    original_size: u64,
    checksum: String,
}

fn validate_entry_path(path: &Path) -> Result<(), JobError> {
    if path.as_os_str().is_empty()
        || !path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(JobError::internal(format!(
            "invalid backup entry path: {}",
            path.display()
        )));
    }

    Ok(())
}

/// restore one file
fn restore_one_file(
    absolute_backup_file_path: &Path,
    absolute_destination_file_path: &Path,
) -> Result<FileBackupResult, JobError> {
    let backup_file = std::fs::OpenOptions::new()
        .read(true)
        .open(absolute_backup_file_path)
        .map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to open backup file {}: {error}",
                    absolute_backup_file_path.display(),
                ),
            )
        })?;
    let mut destination_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(absolute_destination_file_path)
        .map_err(|error| match error.kind() {
            io::ErrorKind::AlreadyExists => JobError::new(
                StatusCode::CONFLICT,
                format!(
                    "destination file {} already exists: {error}",
                    absolute_destination_file_path.display(),
                ),
            ),
            _ => JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to create destination file {}: {error}",
                    absolute_destination_file_path.display(),
                ),
            ),
        })?;

    let mut buffer = [0u8; 128 * 1024];
    let mut backup_file_decoder =
        zstd::stream::read::Decoder::new(backup_file).map_err(|error| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!(
                    "failed to create zstd decoder on backup file {}: {error}",
                    absolute_backup_file_path.display(),
                ),
            )
        })?;
    let mut hasher = Sha256::new();
    let mut original_size: u64 = 0;

    loop {
        let n = backup_file_decoder.read(&mut buffer).map_err(|error| {
            JobError::internal(format!(
                "failed to decompress and forward data form backup file {} to buffer: {error}",
                absolute_backup_file_path.display(),
            ))
        })?;

        if n == 0 {
            break;
        }

        let chunk = &buffer[..n];
        destination_file.write_all(chunk).map_err(|error| {
            JobError::internal(format!(
                "failed to restore data to destination file {}: {error}",
                absolute_destination_file_path.display(),
            ))
        })?;
        hasher.update(chunk);
        original_size += n as u64;
    }
    destination_file.sync_all().map_err(|error| {
        JobError::internal(format!(
            "failed to sync destination file {}: {error}",
            absolute_destination_file_path.display(),
        ))
    })?;

    Ok(FileBackupResult {
        original_size,
        checksum: hex::encode(hasher.finalize()),
    })
}

/// restore all
fn restore_all_files(
    manifest_entries: &Vec<ManifestEntry>,
    absolute_backup_path: &Path,
    absolute_destination_path: &Path,
) -> Result<(), JobError> {
    for manifest_entry in manifest_entries {
        validate_entry_path(match manifest_entry {
            ManifestEntry::File { path, .. } => path,
            ManifestEntry::Directory { path } => path,
        })?;
    }

    for manifest_entry in manifest_entries {
        match manifest_entry {
            ManifestEntry::File {
                path,
                original_size,
                checksum,
            } => {
                let absolute_backup_file_path = absolute_backup_path.join(path);
                let absolute_destination_file_path = absolute_destination_path.join(path);
                let file_restore_result =
                    restore_one_file(&absolute_backup_file_path, &absolute_destination_file_path)?;
                if &file_restore_result.checksum != checksum
                    || &file_restore_result.original_size != original_size
                {
                    return Err(JobError::internal(format!(
                        "the content of backup file {} is changed after the backup",
                        absolute_backup_file_path.display()
                    )));
                }
            }
            ManifestEntry::Directory { path } => {
                let absolute_destination_directory_path = absolute_destination_path.join(path);
                std::fs::create_dir(&absolute_destination_directory_path).map_err(|error| {
                    JobError::internal(format!(
                        "failed to create new destination directory {}: {error}",
                        absolute_destination_directory_path.display(),
                    ))
                })?;
            }
        }
    }

    for manifest_entry in manifest_entries.iter().rev() {
        if let ManifestEntry::Directory { path } = manifest_entry {
            sync_directory(&absolute_destination_path.join(path))?;
        }
    }

    Ok(())
}

pub(crate) fn run(repository: &Path, backup_id: &str, destination: &Path) -> Result<(), JobError> {
    let absolute_repository_path = get_absolute_directory_path(repository)?;
    let absolute_destination_path = get_absolute_directory_path(destination)?;
    if paths_overlap(&absolute_repository_path, &absolute_destination_path) {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "repository path {} and destination path {} overlap",
                absolute_repository_path.display(),
                absolute_destination_path.display()
            ),
        ));
    }

    #[cfg(not(unix))]
    sync_directory(&absolute_repository_path)?;

    std::fs::create_dir_all(&absolute_destination_path).map_err(|error| {
        JobError::internal(format!(
            "failed to create destination directory {}: {error}",
            absolute_destination_path.display(),
        ))
    })?;

    // determine whether the destination path is empty or not
    if std::fs::read_dir(&absolute_destination_path)
        .map_err(|error| {
            JobError::internal(format!(
                "failed to read destination path {}: {error}",
                absolute_destination_path.display()
            ))
        })?
        .next()
        .transpose()
        .map_err(|error| {
            JobError::internal(format!(
                "failed to retrieve content from the destination path {}: {error}",
                absolute_destination_path.display()
            ))
        })?
        .is_some()
    {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            format!(
                "destination path {} is not empty",
                absolute_destination_path.display()
            ),
        ));
    }

    let handle = tokio::runtime::Handle::try_current()
        .map_err(|error| JobError::internal(format!("cannot access Tokio runtime: {error}")))?;

    let manifest = handle.block_on(Manifest::read(&absolute_repository_path, backup_id))?;

    if manifest.format_version != 1 {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            "unsupported format verions",
        ));
    }
    if manifest.compression_algorithm != "zstd" {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            "unsupported compression_algorithm",
        ));
    }
    if manifest.checksum_algorithm != "sha256" {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            "unsupported compression_algorithm",
        ));
    }

    let absolute_backup_path = absolute_repository_path.join(format!("backups/{}", backup_id));

    restore_all_files(
        &manifest.entries,
        &absolute_backup_path,
        &absolute_destination_path,
    )?;

    sync_directory(&absolute_destination_path)?;

    Ok(())
}

#[cfg(test)]
mod tests;
