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
