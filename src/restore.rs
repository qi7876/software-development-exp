use http::StatusCode;
use std::path::Path;

use crate::jobs::JobError;

pub(crate) fn run(
    _repository: &Path,
    _backup_id: &str,
    _destination: &Path,
) -> Result<(), JobError> {
    Err(JobError::new(
        StatusCode::NOT_IMPLEMENTED,
        "restore execution is not implemented yet",
    ))
}
