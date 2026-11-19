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

#[cfg(target_os = "linux")]
pub(super) fn device_numbers(raw: u64) -> std::io::Result<(u32, u32)> {
    Ok((nix::libc::major(raw), nix::libc::minor(raw)))
}

#[cfg(target_os = "macos")]
pub(super) fn device_numbers(raw: u64) -> std::io::Result<(u32, u32)> {
    let raw = raw as nix::libc::dev_t;
    Ok((nix::libc::major(raw) as u32, nix::libc::minor(raw) as u32))
}

#[cfg(target_os = "linux")]
pub(super) fn encode_device(major: u32, minor: u32) -> std::io::Result<nix::libc::dev_t> {
    // mknod takes the kernel's 32-bit device encoding.
    if major >= 1 << 12 || minor >= 1 << 20 {
        return Err(invalid_device_number(major, minor));
    }
    Ok(nix::libc::makedev(major, minor))
}

#[cfg(target_os = "macos")]
pub(super) fn encode_device(major: u32, minor: u32) -> std::io::Result<nix::libc::dev_t> {
    if major >= 1 << 8 || minor >= 1 << 24 {
        return Err(invalid_device_number(major, minor));
    }
    Ok(nix::libc::makedev(major as i32, minor as i32))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn invalid_device_number(major: u32, minor: u32) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        format!("device number {major}:{minor} is out of range for this platform"),
    )
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(super) fn device_numbers(_: u64) -> std::io::Result<(u32, u32)> {
    Err(unsupported_device_number())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
pub(super) fn encode_device(_: u32, _: u32) -> std::io::Result<nix::libc::dev_t> {
    Err(unsupported_device_number())
}

#[cfg(all(unix, not(any(target_os = "linux", target_os = "macos"))))]
fn unsupported_device_number() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "device number encoding is unsupported on this platform",
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn device_encoding_checks_platform_limits() -> std::io::Result<()> {
        #[cfg(target_os = "linux")]
        let (major_limit, minor_limit) = (1 << 12, 1 << 20);
        #[cfg(target_os = "macos")]
        let (major_limit, minor_limit) = (1 << 8, 1 << 24);
        let (major, minor) = (major_limit - 1, minor_limit - 1);
        let raw = encode_device(major, minor)?;
        #[cfg(target_os = "macos")]
        let raw = raw as u64;
        assert_eq!(device_numbers(raw)?, (major, minor));
        assert!(encode_device(major_limit, 0).is_err());
        assert!(encode_device(0, minor_limit).is_err());
        Ok(())
    }

    #[test]
    fn overlap_paths() {
        let source_1 = Path::new("/data/source");
        let backup_1 = Path::new("/data/backup");
        assert!(!paths_overlap(source_1, backup_1));
        assert!(paths_overlap(source_1, source_1));
        let backup_2 = Path::new("/data/source/backup");
        assert!(paths_overlap(source_1, backup_2));
        let source_2 = Path::new("/data/backup/source");
        assert!(paths_overlap(source_2, backup_1));
        let backup_3 = Path::new("/data/source-copy");
        assert!(!paths_overlap(source_1, backup_3));
    }
}
