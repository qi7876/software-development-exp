//! Embedded console and authenticated HTTP API in the same process.

use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::get,
};
use serde::Serialize;

use crate::config::Config;

pub(crate) fn router(config: Config) -> Router {
    let api = Router::new()
        .route("/status", get(status))
        .fallback(api_not_found)
        .layer(middleware::from_fn_with_state(
            Arc::new(format!("Bearer {}", config.secret_key)),
            authenticate,
        ));
    Router::new()
        .route("/", get(console))
        .nest("/api", api)
        .layer(middleware::from_fn(no_store))
}

async fn console() -> Html<&'static str> {
    Html(include_str!("../web/index.html"))
}

#[derive(Serialize)]
struct Status {
    status: &'static str,
    package_version: &'static str,
    backup_available: bool,
}

async fn status() -> Json<Status> {
    Json(Status {
        status: "ok",
        package_version: env!("CARGO_PKG_VERSION"),
        backup_available: false,
    })
}

async fn authenticate(
    State(expected): State<Arc<String>>,
    request: Request,
    next: Next,
) -> Response {
    let mut headers = request.headers().get_all(header::AUTHORIZATION).iter();
    let authorized = headers
        .next()
        .is_some_and(|value| value.as_bytes() == expected.as_bytes())
        && headers.next().is_none();
    if !authorized {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            Json(serde_json::json!({"error": "unauthorized"})),
        )
            .into_response();
    }
    next.run(request).await
}

async fn api_not_found() -> impl IntoResponse {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({"error": "API endpoint not found"})),
    )
}

async fn no_store(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    use super::*;

    fn app() -> Router {
        router(Config {
            listen: ([127, 0, 0, 1], 8080).into(),
            secret_key: "test-secret".to_owned(),
        })
    }

    #[tokio::test]
    async fn api_requires_a_single_correct_bearer_key() {
        for path in ["/api/status", "/api/unknown"] {
            for authorization in [
                None,
                Some("Bearer wrong"),
                Some("Basic test-secret"),
                Some("Bearer "),
            ] {
                let mut request = Request::builder().uri(path);
                if let Some(value) = authorization {
                    request = request.header(header::AUTHORIZATION, value);
                }
                let response = app()
                    .oneshot(request.body(Body::empty()).expect("valid request"))
                    .await
                    .expect("router must respond");
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
                assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            }
        }
        let request = Request::builder()
            .uri("/api/status")
            .header(header::AUTHORIZATION, "Bearer test-secret")
            .header(header::AUTHORIZATION, "Bearer wrong")
            .body(Body::empty())
            .expect("valid request");
        let response = app().oneshot(request).await.expect("router must respond");
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn status_is_json_and_reports_actual_capabilities() {
        let request = Request::builder()
            .uri("/api/status")
            .header(header::AUTHORIZATION, "Bearer test-secret")
            .body(Body::empty())
            .expect("valid request");
        let response = app().oneshot(request).await.expect("router must respond");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), 1024)
            .await
            .expect("small status body");
        let status: serde_json::Value = serde_json::from_slice(&body).expect("JSON status");
        assert_eq!(status["status"], "ok");
        assert_eq!(status["package_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(status["backup_available"], false);
        assert!(!String::from_utf8_lossy(&body).contains("test-secret"));
    }

    #[tokio::test]
    async fn console_is_public_but_does_not_embed_the_secret() {
        let request = Request::builder()
            .uri("/")
            .body(Body::empty())
            .expect("valid request");
        let response = app().oneshot(request).await.expect("router must respond");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/html; charset=utf-8"
        );
        let body = to_bytes(response.into_body(), 16384)
            .await
            .expect("embedded console body");
        let html = String::from_utf8_lossy(&body);
        assert!(html.contains("Data Backup"));
        assert!(!html.contains("test-secret"));
    }

    #[tokio::test]
    async fn unknown_api_returns_an_authenticated_json_error() {
        let request = Request::builder()
            .uri("/api/unknown")
            .header(header::AUTHORIZATION, "Bearer test-secret")
            .body(Body::empty())
            .expect("valid request");
        let response = app().oneshot(request).await.expect("router must respond");
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
    }
}
