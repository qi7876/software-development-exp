use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, FromRequest, Request, State, rejection::JsonRejection},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::json;
use std::{error::Error, future::IntoFuture, path::PathBuf};
use tokio::sync::watch;

use crate::{
    config::Config,
    jobs::{JobError, JobManager, JobRequest},
};

#[derive(Clone)]
struct AppState {
    jobs: JobManager,
    secret_key: String,
}

pub(crate) fn router(jobs: JobManager, secret_key: String) -> Router {
    let state = AppState { jobs, secret_key };
    Router::new()
        .route("/", get(|| async { "bak is running" }))
        .route(
            "/api/status",
            get(|| async { Json(json!({"version": env!("CARGO_PKG_VERSION")})) }),
        )
        .route("/api/backups", get(list_backups).post(start_backup))
        .route("/api/restores", post(start_restore))
        .route("/api/jobs/{job_id}", get(get_job))
        .fallback(|| async { JobError::new(StatusCode::NOT_FOUND, "route not found") })
        .method_not_allowed_fallback(|| async {
            JobError::new(StatusCode::METHOD_NOT_ALLOWED, "method not allowed")
        })
        .layer(DefaultBodyLimit::max(2 * 1024 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), authorize))
        .with_state(state)
}

pub(crate) async fn serve(config: Config, jobs: JobManager) -> Result<(), Box<dyn Error>> {
    let listener = tokio::net::TcpListener::bind(config.listen)
        .await
        .map_err(|error| format!("cannot listen on {}: {error}", config.listen))?;
    println!("Listening on {}", listener.local_addr()?);
    let (stop, mut server_stop) = watch::channel(false);
    let worker_jobs = jobs.clone();
    let worker_stop = stop.subscribe();
    let mut worker = tokio::spawn(async move { worker_jobs.run_worker(worker_stop).await });
    let server = axum::serve(listener, router(jobs.clone(), config.secret_key))
        .with_graceful_shutdown(async move {
            // Sender closure also means the owner has stopped the service.
            while !*server_stop.borrow() {
                if server_stop.changed().await.is_err() {
                    break;
                }
            }
        })
        .into_future();
    tokio::pin!(server);
    let mut worker_finished = false;
    let mut server_finished = false;
    let outcome: Result<(), Box<dyn Error>> = tokio::select! {
        result = shutdown_signal() => result.map_err(Into::into),
        result = &mut server => {
            server_finished = true;
            match result {
                Ok(()) => Err("HTTP server stopped unexpectedly".into()),
                Err(error) => Err(format!("HTTP server failed: {error}").into()),
            }
        }
        result = &mut worker => {
            worker_finished = true;
            match result {
                Ok(Ok(())) => Err("job worker stopped unexpectedly".into()),
                Ok(Err(error)) => Err(error.into()),
                Err(error) => Err(format!("job worker task failed: {error}").into()),
            }
        }
    };
    stop.send_replace(true);
    let (worker_outcome, server_outcome) = tokio::join!(
        async {
            if worker_finished {
                return Ok(());
            }
            match worker.await {
                Ok(result) => result.map_err(|error| -> Box<dyn Error> { error.into() }),
                Err(error) => {
                    Err(format!("job worker task failed during shutdown: {error}").into())
                }
            }
        },
        async {
            if server_finished {
                return Ok(());
            }
            server.await.map_err(|error| -> Box<dyn Error> {
                format!("HTTP server failed during shutdown: {error}").into()
            })
        }
    );
    jobs.close().await;
    // Report secondary shutdown failures as well as the original cause.
    if outcome.is_err() {
        if let Err(error) = &worker_outcome {
            eprintln!("{error}");
        }
        if let Err(error) = &server_outcome {
            eprintln!("{error}");
        }
    }
    outcome?;
    worker_outcome?;
    server_outcome
}

async fn shutdown_signal() -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        tokio::select! {
            result = tokio::signal::ctrl_c() => result,
            _ = terminate.recv() => Ok(()),
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await
}

async fn authorize(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let path = request.uri().path();
    if path == "/api" || (path.starts_with("/api/") && path != "/api/") {
        let authorized = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.split_once(' '))
            .is_some_and(|(scheme, key)| {
                scheme.eq_ignore_ascii_case("Bearer") && key == state.secret_key
            });
        if !authorized {
            let mut response =
                JobError::new(StatusCode::UNAUTHORIZED, "missing or invalid API key")
                    .into_response();
            response.headers_mut().insert(
                header::WWW_AUTHENTICATE,
                axum::http::HeaderValue::from_static("Bearer"),
            );
            return response;
        }
    }
    next.run(request).await
}

impl IntoResponse for JobError {
    fn into_response(self) -> Response {
        (
            self.code,
            Json(json!({"error": {"code": self.code.as_u16(), "message": self.message}})),
        )
            .into_response()
    }
}

async fn read_json<T: DeserializeOwned>(request: Request) -> Result<T, JobError> {
    Json::<T>::from_request(request, &())
        .await
        .map(|Json(value)| value)
        .map_err(|error: JsonRejection| {
            // axum distinguishes JSON shape failures as 422; bak's contract uses 400.
            let status = if error.status() == StatusCode::UNPROCESSABLE_ENTITY {
                StatusCode::BAD_REQUEST
            } else {
                error.status()
            };
            JobError::new(status, error.body_text())
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

async fn accepted_job(jobs: &JobManager, request: JobRequest) -> Result<Response, JobError> {
    let job = jobs.submit(request).await?;
    let location = format!("/api/jobs/{}", job.job_id);
    Ok((
        StatusCode::ACCEPTED,
        [(header::LOCATION, location)],
        Json(job.to_json()?),
    )
        .into_response())
}

async fn start_backup(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, JobError> {
    let request: BackupRequest = read_json(request).await?;
    validate_path(&request.source, "source")?;
    validate_path(&request.repository, "repository")?;
    accepted_job(
        &state.jobs,
        JobRequest::Backup {
            source: request.source,
            repository: request.repository,
        },
    )
    .await
}

async fn start_restore(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, JobError> {
    let request: RestoreRequest = read_json(request).await?;
    validate_path(&request.repository, "repository")?;
    validate_path(&request.destination, "destination")?;
    validate_backup_id(&request.backup_id)?;
    accepted_job(
        &state.jobs,
        JobRequest::Restore {
            repository: request.repository,
            backup_id: request.backup_id,
            destination: request.destination,
        },
    )
    .await
}

async fn list_backups(
    State(state): State<AppState>,
    request: Request,
) -> Result<Response, JobError> {
    let mut repository = None;
    for (key, value) in url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
    {
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
    Ok(Json(json!({"backups": state.jobs.list_backups(&repository).await?})).into_response())
}

async fn get_job(State(state): State<AppState>, request: Request) -> Result<Response, JobError> {
    let encoded = request
        .uri()
        .path()
        .strip_prefix("/api/jobs/")
        .ok_or_else(|| JobError::internal("job route has no job_id"))?;
    let job_id = percent_encoding::percent_decode_str(encoded)
        .decode_utf8()
        .map_err(|error| {
            JobError::new(StatusCode::BAD_REQUEST, format!("invalid job_id: {error}"))
        })?;
    let job = state
        .jobs
        .get_job(&job_id)
        .await?
        .ok_or_else(|| JobError::new(StatusCode::NOT_FOUND, "job not found"))?;
    Ok(Json(job.to_json()?).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        jobs::{JobInfo, JobResult, JobState},
        test_support::TestDirectory,
    };
    use axum::body::{Body, to_bytes};
    use serde_json::Value;
    use std::time::Duration;
    use tower::ServiceExt;

    fn request(
        method: &str,
        path: &str,
        auth: &str,
        content_type: &str,
        body: Vec<u8>,
    ) -> Result<Request, axum::http::Error> {
        Request::builder()
            .method(method)
            .uri(path)
            .header(header::AUTHORIZATION, auth)
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(body))
    }

    async fn json_body(response: Response) -> Result<Value, Box<dyn Error>> {
        Ok(serde_json::from_slice(
            &to_bytes(response.into_body(), usize::MAX).await?,
        )?)
    }

    #[tokio::test]
    async fn routing_auth_and_input_errors_follow_the_public_contract() -> Result<(), Box<dyn Error>>
    {
        let directory = TestDirectory::new()?;
        let jobs = JobManager::open(directory.path().to_owned()).await?;
        let app = router(jobs.clone(), "test-key".into());
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
            let response = app
                .clone()
                .oneshot(request(
                    method,
                    path,
                    auth,
                    content_type,
                    body.as_bytes().to_vec(),
                )?)
                .await?;
            assert_eq!(response.status().as_u16(), expected, "{method} {path}");
            if expected == 401 {
                assert_eq!(
                    response
                        .headers()
                        .get(header::WWW_AUTHENTICATE)
                        .and_then(|v| v.to_str().ok()),
                    Some("Bearer")
                );
            }
            let body = json_body(response).await?;
            assert_eq!(body["error"]["code"], expected, "{method} {path}");
            assert!(body["error"]["message"].is_string());
        }
        let response = app
            .oneshot(request(
                "GET",
                "/api/status",
                "bEaReR test-key",
                "",
                vec![],
            )?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            json_body(response).await?,
            json!({"version": env!("CARGO_PKG_VERSION")})
        );
        jobs.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn oversized_json_and_head_requests() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let jobs = JobManager::open(directory.path().to_owned()).await?;
        let app = router(jobs.clone(), "test-key".into());
        let response = app
            .clone()
            .oneshot(request(
                "POST",
                "/api/backups",
                "Bearer test-key",
                "application/json",
                vec![b' '; 2 * 1024 * 1024 + 1],
            )?)
            .await?;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(json_body(response).await?["error"]["code"], 413);
        let response = app
            .oneshot(request(
                "HEAD",
                "/api/status",
                "Bearer test-key",
                "",
                vec![],
            )?)
            .await?;
        assert_eq!(response.status(), StatusCode::OK);
        assert!(to_bytes(response.into_body(), usize::MAX).await?.is_empty());
        jobs.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn request_errors_return_status_and_json() -> Result<(), Box<dyn Error>> {
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
            let response = JobError::new(status, "request failed").into_response();
            assert_eq!(response.status(), status);
            assert_eq!(
                json_body(response).await?,
                json!({"error": {"code": status.as_u16(), "message": "request failed"}})
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn operation_requests_return_accepted_queryable_jobs() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let jobs = JobManager::open(directory.path().to_owned()).await?;
        let app = router(jobs.clone(), "test-key".into());
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
            let response = app
                .clone()
                .oneshot(request(
                    "POST",
                    path,
                    "Bearer test-key",
                    "application/json",
                    serde_json::to_vec(&payload)?,
                )?)
                .await?;
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            let location = response
                .headers()
                .get(header::LOCATION)
                .ok_or("missing location")?
                .to_str()?
                .to_owned();
            let accepted = json_body(response).await?;
            assert_eq!(accepted["kind"], kind);
            assert_eq!(accepted["state"], "queued");
            assert!(accepted["result"].is_null());
            assert!(accepted["error"].is_null());
            let response = app
                .clone()
                .oneshot(request("GET", &location, "Bearer test-key", "", vec![])?)
                .await?;
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(json_body(response).await?, accepted);
            let (stop, receiver) = watch::channel(false);
            let worker_jobs = jobs.clone();
            let worker = tokio::spawn(async move { worker_jobs.run_worker(receiver).await });
            tokio::time::timeout(Duration::from_secs(5), async {
                loop {
                    let response = app
                        .clone()
                        .oneshot(request("GET", &location, "Bearer test-key", "", vec![])?)
                        .await?;
                    assert_eq!(response.status(), StatusCode::OK);
                    let body = json_body(response).await?;
                    if body["state"] == "failed" {
                        assert_eq!(body["error"]["code"], 501);
                        assert_eq!(body["job_id"], accepted["job_id"]);
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                Ok::<_, Box<dyn Error>>(())
            })
            .await??;
            stop.send(true)?;
            worker.await??;
            let response = app
                .clone()
                .oneshot(request(
                    "POST",
                    &format!("{location}/cancel"),
                    "Bearer test-key",
                    "",
                    vec![],
                )?)
                .await?;
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        }
        jobs.close().await;
        Ok(())
    }

    #[tokio::test]
    async fn database_admission_failure_returns_json_500() -> Result<(), Box<dyn Error>> {
        let directory = TestDirectory::new()?;
        let jobs = JobManager::open(directory.path().to_owned()).await?;
        jobs.close().await;
        let app = router(jobs, "test-key".into());
        let response = app
            .oneshot(request(
                "POST",
                "/api/backups",
                "Bearer test-key",
                "application/json",
                br#"{"source":"source","repository":"repo"}"#.to_vec(),
            )?)
            .await?;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(json_body(response).await?["error"]["code"], 500);
        Ok(())
    }

    #[test]
    fn job_results_and_errors_follow_the_public_contract() -> Result<(), Box<dyn Error>> {
        for (state, expected_state, result, error) in [
            (JobState::Queued, "queued", Value::Null, Value::Null),
            (JobState::Running, "running", Value::Null, Value::Null),
            (
                JobState::Succeeded(JobResult::Backup {
                    backup_id: "backup-001".into(),
                }),
                "succeeded",
                json!({"backup_id": "backup-001"}),
                Value::Null,
            ),
            (
                JobState::Failed(JobError::internal("cannot read source")),
                "failed",
                Value::Null,
                json!({"code": 500, "message": "cannot read source"}),
            ),
        ] {
            let job = JobInfo {
                job_id: "job-001".into(),
                request: JobRequest::Backup {
                    source: "source".into(),
                    repository: "repository".into(),
                },
                state,
            };
            assert_eq!(
                job.to_json()?,
                json!({"job_id": "job-001", "kind": "backup", "state": expected_state, "result": result, "error": error})
            );
        }
        let job = JobInfo {
            job_id: "job-002".into(),
            request: JobRequest::Restore {
                repository: "repository".into(),
                backup_id: "backup-001".into(),
                destination: "restored".into(),
            },
            state: JobState::Succeeded(JobResult::Restore {
                destination: "restored".into(),
            }),
        };
        assert_eq!(job.to_json()?["result"], json!({"destination": "restored"}));
        assert_eq!(job.to_json()?["kind"], "restore");
        Ok(())
    }
}
