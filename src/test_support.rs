use std::{
    io,
    path::{Path, PathBuf},
};

pub(crate) struct TestDirectory(PathBuf);

impl TestDirectory {
    pub(crate) fn new() -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!("bak-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("cannot remove test directory {}: {error}", self.0.display());
        }
    }
}

// Build the complete HTTP shape: omitted filter fields are not defaults in the API.
pub(crate) fn unfiltered_config() -> serde_json::Value {
    serde_json::json!({
        "relative_path": {"include": [], "exclude": []},
        "uid": {"include": [], "exclude": []},
        "gid": {"include": [], "exclude": []},
        "mode": {"include": [], "exclude": []},
        "atime": {"from": null, "before": null},
        "mtime": {"from": null, "before": null}
    })
}

pub(crate) fn unfiltered() -> Result<crate::jobs::Filter, serde_json::Error> {
    serde_json::from_value(unfiltered_config())
}

// Rewrite only the public metadata, retaining payload bytes and a valid container digest.
// Tests use this to distinguish structural/AEAD validation from accidental corruption.
pub(crate) fn rewrite_backup_metadata(
    path: &Path,
    update: impl FnOnce(&mut serde_json::Value),
) -> Result<(), Box<dyn std::error::Error>> {
    use sha2::{Digest, Sha256};
    let mut bytes = std::fs::read(path)?;
    let trailer = bytes.len().checked_sub(48).ok_or("short backup")?;
    let length = u64::from_le_bytes(bytes[trailer..trailer + 8].try_into()?) as usize;
    let start = trailer
        .checked_sub(length)
        .ok_or("invalid metadata length")?;
    let mut metadata = serde_json::from_slice(&bytes[start..trailer])?;
    update(&mut metadata);
    let metadata = serde_json::to_vec(&metadata)?;
    bytes.truncate(start);
    bytes.extend_from_slice(&metadata);
    bytes.extend_from_slice(&(metadata.len() as u64).to_le_bytes());
    bytes.extend_from_slice(b"BAKEND\0\x01");
    let digest = Sha256::digest(&bytes);
    bytes.extend_from_slice(&digest);
    std::fs::write(path, bytes)?;
    Ok(())
}
