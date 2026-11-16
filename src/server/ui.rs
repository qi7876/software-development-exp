use crate::{config::WebUIConfig, jobs::JobError};
use axum::{Router, http::StatusCode, response::IntoResponse, routing::get};

pub(super) fn router<S: Clone + Send + Sync + 'static>(config: WebUIConfig) -> Router<S> {
    if !config.enabled {
        return Router::new().route("/", get(|| async { "bak is running" }));
    }
    let Some(path) = config.path else {
        // Config::load requires a path when the console is enabled.
        return Router::new();
    };
    let mut router = Router::new();
    for (route, name, content_type) in [
        ("/", "index.html", "text/html; charset=utf-8"),
        ("/ui/app.js", "app.js", "text/javascript; charset=utf-8"),
        ("/ui/styles.css", "styles.css", "text/css; charset=utf-8"),
        ("/ui/favicon.svg", "favicon.svg", "image/svg+xml"),
    ] {
        let file = path.join(name);
        router = router.route(
            route,
            get(move || {
                let file = file.clone();
                async move {
                    match tokio::fs::read(&file).await {
                        Ok(body) => asset(content_type, body).into_response(),
                        Err(error) => {
                            eprintln!("cannot read web UI asset {}: {error}", file.display());
                            JobError::new(
                                StatusCode::INTERNAL_SERVER_ERROR,
                                "cannot read web UI asset",
                            )
                            .into_response()
                        }
                    }
                }
            }),
        );
    }
    router
}

fn asset(content_type: &'static str, body: Vec<u8>) -> impl axum::response::IntoResponse {
    (
        [
            ("content-type", content_type),
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            (
                "content-security-policy",
                "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self' http: https:; img-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
            ),
        ],
        body,
    )
}
