use http::StatusCode;
use rouille::{Request, Response, Server};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{error::Error, io::Read, path::PathBuf, sync::Mutex};

use crate::{
    config::Config,
    jobs::{JobError, JobInfo, JobManager, JobState},
};

pub(crate) fn serve(config: Config, jobs: JobManager) -> Result<(), Box<dyn Error>> {
    // Rouille requires a Send + Sync handler even with one request worker.
    let jobs = Mutex::new(jobs);
    let server = Server::new(config.listen, move |request| match jobs.lock() {
        Ok(mut jobs) => handle(request, &mut jobs, &config.secret_key),
        Err(error) => Response::from(JobError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot access job manager: {error}"),
        )),
    })
    .map_err(|error| format!("cannot listen on {}: {error}", config.listen))?
    .pool_size(1);
    println!("Listening on {}", server.server_addr());
    server.run();
    Err("HTTP server stopped unexpectedly".into())
}

fn handle(request: &Request, jobs: &mut JobManager, secret_key: &str) -> Response {
    let path = request.raw_url().split('?').next().unwrap_or("");
    if path == "/api" || (path.starts_with("/api/") && path != "/api/") {
        let authorized = request
            .header("Authorization")
            .and_then(|value| value.split_once(' '))
            .is_some_and(|(scheme, key)| {
                scheme.eq_ignore_ascii_case("Bearer") && key == secret_key
            });
        if !authorized {
            return Response::from(JobError::new(
                StatusCode::UNAUTHORIZED,
                "missing or invalid API key",
            ))
            .with_additional_header("WWW-Authenticate", "Bearer");
        }
    }
    route(request, path, jobs).unwrap_or_else(Response::from)
}

fn route(request: &Request, path: &str, jobs: &mut JobManager) -> Result<Response, JobError> {
    let segments: Vec<_> = path.split('/').collect();
    match (request.method(), segments.as_slice()) {
        ("GET" | "HEAD", ["", ""]) => Ok(Response::text("bak is running")),
        ("GET" | "HEAD", ["", "api", "status"]) => Ok(status()),
        ("GET" | "HEAD", ["", "api", "backups"]) => list_backups(request, jobs),
        ("POST", ["", "api", "backups"]) => start_backup(request, jobs),
        ("POST", ["", "api", "restores"]) => start_restore(request, jobs),
        ("GET" | "HEAD", ["", "api", "jobs", job_id]) if !job_id.is_empty() => {
            get_job(&decode_job_id(job_id)?, jobs)
        }
        (_, ["", ""] | ["", "api", "status" | "backups" | "restores"]) => Err(JobError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
        )),
        (_, ["", "api", "jobs", job_id]) if !job_id.is_empty() => Err(JobError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed",
        )),
        _ => Err(JobError::new(StatusCode::NOT_FOUND, "route not found")),
    }
}

impl From<JobError> for Response {
    fn from(error: JobError) -> Self {
        Response::json(&json!({"error": error_json(&error)})).with_status_code(error.code.as_u16())
    }
}

fn decode_job_id(encoded: &str) -> Result<String, JobError> {
    percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .map(|value| value.into_owned())
        .map_err(|error| JobError::new(StatusCode::BAD_REQUEST, format!("invalid job_id: {error}")))
}

fn read_json<T: DeserializeOwned>(request: &Request) -> Result<T, JobError> {
    // Keep the previous HTTP body's 2 MiB limit.
    const MAX_BODY_BYTES: u64 = 2 * 1024 * 1024;
    let content_type = request
        .header("Content-Type")
        .and_then(|value| value.split(';').next())
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if content_type != "application/json"
        && !(content_type.starts_with("application/") && content_type.ends_with("+json"))
    {
        return Err(JobError::new(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json or application/*+json",
        ));
    }
    let body = request.data().ok_or_else(|| {
        JobError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "request body has already been read",
        )
    })?;
    let mut bytes = Vec::new();
    body.take(MAX_BODY_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| {
            JobError::new(
                StatusCode::BAD_REQUEST,
                format!("cannot read request body: {error}"),
            )
        })?;
    if bytes.len() as u64 > MAX_BODY_BYTES {
        return Err(JobError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "request body exceeds 2 MiB",
        ));
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        JobError::new(
            StatusCode::BAD_REQUEST,
            format!("cannot parse request JSON: {error}"),
        )
    })
}

#[derive(Deserialize)]
struct BackupRequest {
    source: PathBuf,
    repository: PathBuf,
}

#[derive(Deserialize)]
struct RestoreRequest {
    repository: PathBuf,
    backup_id: String,
    destination: PathBuf,
}

fn error_json(error: &JobError) -> Value {
    json!({"code": error.code.as_u16(), "message": error.message})
}

fn validate_path(path: &std::path::Path, field: &str) -> Result<(), JobError> {
    if path.as_os_str().is_empty() {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            format!("{field} must not be empty"),
        ));
    }
    Ok(())
}

fn validate_backup_id(id: &str) -> Result<(), JobError> {
    if id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\']) {
        return Err(JobError::new(
            StatusCode::BAD_REQUEST,
            "backup_id must be a non-empty identifier, not a path",
        ));
    }
    Ok(())
}

fn job_json(job: &JobInfo) -> Result<Value, JobError> {
    job.to_json()
}

fn completed_job(job: JobInfo) -> Result<Response, JobError> {
    let status = match &job.state {
        JobState::Succeeded(_) => StatusCode::OK,
        JobState::Failed(error) => error.code,
        JobState::Running => {
            return Err(JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "synchronous job returned before completion",
            ));
        }
    };
    let mut location = url::Url::parse("http://localhost/api/jobs/").map_err(|error| {
        JobError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("cannot build job location: {error}"),
        )
    })?;
    location
        .path_segments_mut()
        .map_err(|()| {
            JobError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "cannot build job location path",
            )
        })?
        .pop_if_empty()
        .push(&job.job_id);
    Ok(Response::json(&job_json(&job)?)
        .with_status_code(status.as_u16())
        .with_additional_header("Location", location.path().to_owned()))
}

fn status() -> Response {
    Response::json(&json!({"version": env!("CARGO_PKG_VERSION")}))
}

fn start_backup(request: &Request, jobs: &mut JobManager) -> Result<Response, JobError> {
    let request: BackupRequest = read_json(request)?;
    validate_path(&request.source, "source")?;
    validate_path(&request.repository, "repository")?;
    completed_job(jobs.start_backup(request.source, request.repository)?)
}

fn start_restore(request: &Request, jobs: &mut JobManager) -> Result<Response, JobError> {
    let request: RestoreRequest = read_json(request)?;
    validate_path(&request.repository, "repository")?;
    validate_path(&request.destination, "destination")?;
    validate_backup_id(&request.backup_id)?;
    completed_job(jobs.start_restore(request.repository, request.backup_id, request.destination)?)
}

fn list_backups(request: &Request, jobs: &JobManager) -> Result<Response, JobError> {
    let mut repository = None;
    for (key, value) in url::form_urlencoded::parse(request.raw_query_string().as_bytes()) {
        if key == "repository"
            && repository
                .replace(PathBuf::from(value.into_owned()))
                .is_some()
        {
            return Err(JobError::new(
                StatusCode::BAD_REQUEST,
                "duplicate repository parameter",
            ));
        }
    }
    let repository = repository
        .ok_or_else(|| JobError::new(StatusCode::BAD_REQUEST, "missing repository parameter"))?;
    validate_path(&repository, "repository")?;
    Ok(Response::json(
        &json!({"backups": jobs.list_backups(&repository)?}),
    ))
}

fn get_job(job_id: &str, jobs: &JobManager) -> Result<Response, JobError> {
    let job = jobs
        .get_job(job_id)
        .ok_or_else(|| JobError::new(StatusCode::NOT_FOUND, "job not found"))?;
    Ok(Response::json(&job_json(job)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::jobs::{JobRequest, JobResult};
    use std::error::Error;

    #[test]
    fn routing_auth_and_input_errors_follow_the_public_contract() -> Result<(), Box<dyn Error>> {
        let mut jobs = JobManager::new(PathBuf::new(), None);
        let cases = [
            ("GET", "/api/status", "", "", "", 401),
            ("GET", "/api", "", "", "", 401),
            ("GET", "/api/", "", "", "", 404),
            ("GET", "/api/unknown", "", "", "", 401),
            (
                "POST",
                "/api/backups",
                "Bearer wrong",
                "application/json",
                "{",
                401,
            ),
            ("GET", "/api/unknown", "Bearer test-key", "", "", 404),
            ("GET", "/api/jobs/unknown", "Bearer test-key", "", "", 404),
            ("POST", "/api/status", "Bearer test-key", "", "", 405),
            (
                "POST",
                "/api/backups",
                "Bearer test-key",
                "text/plain",
                "{}",
                415,
            ),
            (
                "POST",
                "/api/backups",
                "Bearer test-key",
                "application/json",
                "{",
                400,
            ),
            (
                "POST",
                "/api/backups",
                "Bearer test-key",
                "application/json",
                "{}",
                400,
            ),
            (
                "POST",
                "/api/backups",
                "Bearer test-key",
                "application/vnd.bak+json",
                "{}",
                400,
            ),
            (
                "POST",
                "/api/backups",
                "Bearer test-key",
                "application/json",
                r#"{"source":"","repository":"repo"}"#,
                400,
            ),
            (
                "POST",
                "/api/restores",
                "Bearer test-key",
                "application/json",
                r#"{"repository":"repo","backup_id":"../backup","destination":"restore"}"#,
                400,
            ),
            ("GET", "/api/backups", "Bearer test-key", "", "", 400),
            (
                "GET",
                "/api/backups?repository=",
                "Bearer test-key",
                "",
                "",
                400,
            ),
            (
                "GET",
                "/api/backups?repository=a&repository=b",
                "Bearer test-key",
                "",
                "",
                400,
            ),
            ("GET", "/api/jobs/%FF", "Bearer test-key", "", "", 400),
            (
                "POST",
                "/api/jobs/%FF/cancel",
                "Bearer test-key",
                "",
                "",
                404,
            ),
        ];
        for (method, path, auth, content_type, body, expected) in cases {
            let request = Request::fake_http(
                method,
                path,
                vec![
                    ("Authorization".to_owned(), auth.to_owned()),
                    ("Content-Type".to_owned(), content_type.to_owned()),
                ],
                body.as_bytes().to_vec(),
            );
            let response = handle(&request, &mut jobs, "test-key");
            assert_eq!(response.status_code, expected, "{method} {path}");
            if expected == 401 {
                assert!(response.headers.iter().any(|(name, value)| {
                    name.eq_ignore_ascii_case("WWW-Authenticate") && value == "Bearer"
                }));
            }
            let body: Value = serde_json::from_reader(response.data.into_reader_and_size().0)?;
            assert_eq!(body["error"]["code"], expected);
            assert!(body["error"]["message"].is_string());
        }
        let request = Request::fake_http(
            "GET",
            "/api/status",
            vec![("Authorization".to_owned(), "bEaReR test-key".to_owned())],
            vec![],
        );
        let response = handle(&request, &mut jobs, "test-key");
        assert_eq!(response.status_code, 200);
        let body: Value = serde_json::from_reader(response.data.into_reader_and_size().0)?;
        assert_eq!(body, json!({"version": env!("CARGO_PKG_VERSION")}));
        Ok(())
    }

    #[test]
    fn oversized_json_returns_413() -> Result<(), Box<dyn Error>> {
        let mut jobs = JobManager::new(PathBuf::new(), None);
        let request = Request::fake_http(
            "POST",
            "/api/backups",
            vec![
                ("Authorization".to_owned(), "Bearer test-key".to_owned()),
                ("Content-Type".to_owned(), "application/json".to_owned()),
            ],
            vec![b' '; 2 * 1024 * 1024 + 1],
        );
        let response = handle(&request, &mut jobs, "test-key");
        assert_eq!(response.status_code, 413);
        let body: Value = serde_json::from_reader(response.data.into_reader_and_size().0)?;
        assert_eq!(body["error"]["code"], 413);
        Ok(())
    }

    #[test]
    fn request_errors_return_status_and_json() -> Result<(), Box<dyn Error>> {
        for status in [
            StatusCode::BAD_REQUEST,
            StatusCode::UNAUTHORIZED,
            StatusCode::NOT_FOUND,
            StatusCode::METHOD_NOT_ALLOWED,
            StatusCode::CONFLICT,
            StatusCode::PAYLOAD_TOO_LARGE,
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            StatusCode::INTERNAL_SERVER_ERROR,
            StatusCode::NOT_IMPLEMENTED,
        ] {
            let response = Response::from(JobError::new(status, "request failed"));
            assert_eq!(response.status_code, status.as_u16());
            let mut body = Vec::new();
            response
                .data
                .into_reader_and_size()
                .0
                .read_to_end(&mut body)?;
            assert_eq!(
                serde_json::from_slice::<Value>(&body)?,
                json!({"error": {"code": status.as_u16(), "message": "request failed"}})
            );
        }
        Ok(())
    }

    fn backup_job(state: JobState) -> JobInfo {
        JobInfo {
            job_id: "job-001".to_owned(),
            request: JobRequest::Backup {
                source: PathBuf::from("source"),
                repository: PathBuf::from("repository"),
            },
            state,
        }
    }

    #[test]
    fn completed_jobs_return_outcome_status_and_location() -> Result<(), Box<dyn Error>> {
        for (state, expected_status) in [
            (
                JobState::Succeeded(JobResult::Backup {
                    backup_id: "backup-001".into(),
                }),
                200,
            ),
            (
                JobState::Failed(JobError::new(
                    StatusCode::NOT_IMPLEMENTED,
                    "not implemented",
                )),
                501,
            ),
        ] {
            let job = backup_job(state);
            let expected = job.to_json().map_err(|error| error.message)?;
            let response = completed_job(job).map_err(|error| error.message)?;
            assert_eq!(response.status_code, expected_status);
            assert_eq!(
                response
                    .headers
                    .iter()
                    .find(|(name, _)| name.eq_ignore_ascii_case("Location"))
                    .map(|(_, value)| value.as_ref()),
                Some("/api/jobs/job-001")
            );
            let body: Value = serde_json::from_reader(response.data.into_reader_and_size().0)?;
            assert_eq!(body, expected);
        }
        assert!(completed_job(backup_job(JobState::Running)).is_err());
        Ok(())
    }

    #[test]
    fn operation_requests_return_completed_queryable_jobs() -> Result<(), Box<dyn Error>> {
        let mut jobs = JobManager::new(PathBuf::new(), None);
        for (path, payload, kind) in [
            (
                "/api/backups",
                json!({"source": "source", "repository": "repo"}),
                "backup",
            ),
            (
                "/api/restores",
                json!({"repository": "repo", "backup_id": "backup-001", "destination": "restored"}),
                "restore",
            ),
        ] {
            let request = Request::fake_http(
                "POST",
                path,
                vec![
                    ("Authorization".into(), "Bearer test-key".into()),
                    ("Content-Type".into(), "application/json".into()),
                ],
                serde_json::to_vec(&payload)?,
            );
            let response = handle(&request, &mut jobs, "test-key");
            assert_eq!(response.status_code, 501);
            let location = response
                .headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("Location"))
                .ok_or("missing job location")?
                .1
                .to_string();
            let completed: Value = serde_json::from_reader(response.data.into_reader_and_size().0)?;
            assert_eq!(completed["kind"], kind);
            assert_eq!(completed["state"], "failed");
            assert_eq!(completed["error"]["code"], 501);
            let request = Request::fake_http(
                "GET",
                &location,
                vec![("Authorization".into(), "Bearer test-key".into())],
                vec![],
            );
            let response = handle(&request, &mut jobs, "test-key");
            assert_eq!(response.status_code, 200);
            let stored: Value = serde_json::from_reader(response.data.into_reader_and_size().0)?;
            assert_eq!(stored, completed);
            let request = Request::fake_http(
                "POST",
                format!("{location}/cancel"),
                vec![("Authorization".into(), "Bearer test-key".into())],
                vec![],
            );
            assert_eq!(handle(&request, &mut jobs, "test-key").status_code, 404);
        }
        Ok(())
    }

    #[test]
    fn job_results_and_errors_follow_the_public_contract() -> Result<(), Box<dyn Error>> {
        let succeeded = job_json(&backup_job(JobState::Succeeded(JobResult::Backup {
            backup_id: "backup-001".to_owned(),
        })))
        .map_err(|error| error.message)?;
        assert_eq!(succeeded["state"], "succeeded");
        assert_eq!(
            succeeded["result"],
            serde_json::json!({"backup_id": "backup-001"})
        );
        assert!(succeeded["error"].is_null());

        let failed = job_json(&backup_job(JobState::Failed(JobError {
            code: StatusCode::INTERNAL_SERVER_ERROR,
            message: "cannot read source".to_owned(),
        })))
        .map_err(|error| error.message)?;
        assert!(failed["result"].is_null());
        assert_eq!(failed["error"]["code"], 500);
        assert_eq!(failed["error"]["message"], "cannot read source");

        let mut restored = JobInfo::new(
            "job-002".to_owned(),
            JobRequest::Restore {
                repository: PathBuf::from("repository"),
                backup_id: "backup-001".to_owned(),
                destination: PathBuf::from("restored"),
            },
        );
        restored.state = JobState::Succeeded(JobResult::Restore {
            destination: PathBuf::from("restored"),
        });
        let restored = job_json(&restored).map_err(|error| error.message)?;
        assert_eq!(restored["kind"], "restore");
        assert_eq!(
            restored["result"],
            serde_json::json!({"destination": "restored"})
        );

        for job in [succeeded, failed, restored] {
            assert!(job.get("progress").is_none());
        }
        Ok(())
    }
}
