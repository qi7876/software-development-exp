use axum::{Router, routing::get};

pub(super) fn router<S: Clone + Send + Sync + 'static>(enabled: bool) -> Router<S> {
    if !enabled {
        return Router::new().route("/", get(|| async { "bak is running" }));
    }
    Router::new()
        .route(
            "/ui/favicon.svg",
            get(|| async { asset("image/svg+xml", include_str!("../../web/favicon.svg")) }),
        )
        .route(
            "/",
            get(|| async {
                asset(
                    "text/html; charset=utf-8",
                    include_str!("../../web/index.html"),
                )
            }),
        )
        .route(
            "/ui/app.js",
            get(|| async {
                asset(
                    "text/javascript; charset=utf-8",
                    include_str!("../../web/app.js"),
                )
            }),
        )
        .route(
            "/ui/styles.css",
            get(|| async {
                asset(
                    "text/css; charset=utf-8",
                    include_str!("../../web/styles.css"),
                )
            }),
        )
}

fn asset(content_type: &'static str, body: &'static str) -> impl axum::response::IntoResponse {
    (
        [
            ("content-type", content_type),
            ("cache-control", "no-store"),
            ("x-content-type-options", "nosniff"),
            ("referrer-policy", "no-referrer"),
            (
                "content-security-policy",
                "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'",
            ),
        ],
        body,
    )
}
