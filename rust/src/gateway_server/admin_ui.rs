//! Embedded admin dashboard (enterprise#45) — the org monitoring console
//! served from the gateway's admin port.
//!
//! Everything is compiled into the binary (`include_str!`/`include_bytes!`,
//! same rule as the Context Cockpit): no CDN, no build step, renders offline
//! and inside airgapped clusters. Fonts and the vendored Chart.js are shared
//! with the cockpit sources so both surfaces stay visually identical.
//!
//! Auth split: this router serves only the *static shell* (login screen) and
//! is mounted **outside** the Bearer middleware — every number the shell
//! renders comes from the `/api/admin/*` endpoints, which stay guarded. The
//! token never appears in a URL; the shell keeps it in `sessionStorage`.

use axum::http::header;
use axum::response::IntoResponse;

const ADMIN_INDEX_HTML: &str = include_str!("static/index.html");
const ADMIN_CSS: &str = include_str!("static/admin.css");
const ADMIN_JS: &str = include_str!("static/admin.js");

// Shared with the cockpit: identical typography and chart engine.
const FONTS_CSS: &str = include_str!("../dashboard/static/fonts/fonts.css");
const FONT_INTER_WOFF2: &[u8] = include_bytes!("../dashboard/static/fonts/inter-variable.woff2");
const FONT_JETBRAINS_WOFF2: &[u8] =
    include_bytes!("../dashboard/static/fonts/jetbrains-mono-variable.woff2");
const FONT_SPACE_GROTESK_WOFF2: &[u8] =
    include_bytes!("../dashboard/static/fonts/space-grotesk-variable.woff2");
const VENDOR_CHART_JS: &str = include_str!("../dashboard/static/vendor/chart.umd.min.js");

/// Static-shell router. Mounted unguarded (see module docs).
pub fn router() -> axum::Router {
    axum::Router::new()
        .route("/", axum::routing::get(index))
        .route("/static/admin.css", axum::routing::get(css))
        .route("/static/admin.js", axum::routing::get(js))
        .route("/static/fonts/fonts.css", axum::routing::get(fonts_css))
        .route(
            "/static/vendor/chart.umd.min.js",
            axum::routing::get(chart_js),
        )
        .route("/static/fonts/{file}", axum::routing::get(font_file))
}

async fn index() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        ADMIN_INDEX_HTML,
    )
}

async fn css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        ADMIN_CSS,
    )
}

async fn js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        ADMIN_JS,
    )
}

async fn fonts_css() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        FONTS_CSS,
    )
}

async fn chart_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        VENDOR_CHART_JS,
    )
}

async fn font_file(
    axum::extract::Path(file): axum::extract::Path<String>,
) -> axum::response::Response {
    let bytes: &'static [u8] = match file.as_str() {
        "inter-variable.woff2" => FONT_INTER_WOFF2,
        "jetbrains-mono-variable.woff2" => FONT_JETBRAINS_WOFF2,
        "space-grotesk-variable.woff2" => FONT_SPACE_GROTESK_WOFF2,
        _ => return axum::http::StatusCode::NOT_FOUND.into_response(),
    };
    ([(header::CONTENT_TYPE, "font/woff2")], bytes).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_assets_are_nonempty_and_wired() {
        assert!(ADMIN_INDEX_HTML.contains("<!doctype html"));
        assert!(
            ADMIN_INDEX_HTML.contains("/static/admin.js"),
            "shell must load the app script"
        );
        assert!(ADMIN_CSS.contains(":root"), "design tokens present");
        assert!(
            ADMIN_JS.contains("/api/admin/usage"),
            "app must talk to the guarded API"
        );
        assert!(!VENDOR_CHART_JS.is_empty());
        assert!(!FONT_INTER_WOFF2.is_empty());
    }

    #[test]
    fn shell_never_embeds_credentials() {
        // The shell is served unguarded — it must not contain tokens or
        // secret-looking material (the Bearer token arrives via user input).
        for needle in ["Bearer ", "LEAN_CTX_GATEWAY_ADMIN_TOKEN="] {
            assert!(
                !ADMIN_INDEX_HTML.contains(needle),
                "index.html must not embed {needle}"
            );
        }
        assert!(
            !ADMIN_JS.contains("localStorage.setItem('leanctx-admin-token'"),
            "token must live in sessionStorage, not persist in localStorage"
        );
    }
}
