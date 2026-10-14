use super::JobError;
use http::StatusCode;
use std::path::{Path, PathBuf};

pub fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

pub fn get_absolute_directory_path(path: &Path) -> Result<PathBuf, JobError> {
    let path = std::fs::canonicalize(path).map_err(|error| {
        JobError::internal(format!(
            "failed to resolve path {}: {error}",
            path.display()
        ))
    })?;
    let path_metadata = path.metadata().map_err(|error| {
        JobError::internal(format!(
            "failed to load path {} metadata : {error}",
            path.display(),
        ))
    })?;
    if !path_metadata.is_dir() {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            format!("path {} is not a directory", path.display()),
        ));
    };
    Ok(path)
}

#[cfg(unix)]
pub fn sync_directory(path: &Path) -> Result<(), JobError> {
    std::fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            JobError::internal(format!(
                "failed to sync directory {}: {error}",
                path.display()
            ))
        })
}

#[cfg(not(unix))]
pub fn sync_directory(path: &Path) -> Result<(), JobError> {
    Err(JobError::new(
        StatusCode::NOT_IMPLEMENTED,
        format!(
            "directory synchronization is not implemented on this platform: {}",
            path.display()
        ),
    ))
}
