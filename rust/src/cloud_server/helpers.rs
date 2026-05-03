use axum::http::StatusCode;

pub fn internal_error(e: impl std::fmt::Display) -> (StatusCode, String) {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
