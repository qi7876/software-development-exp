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
