mod aggregated;
mod core;
mod diagnostics;
mod overlay;

pub(super) fn handle(
    path: &str,
    _query_str: &str,
    method: &str,
    body: &str,
) -> Option<(&'static str, &'static str, String)> {
    if method.eq_ignore_ascii_case("POST")
        && let result @ Some(_) = overlay::post_route(path, body)
    {
        return result;
    }

    aggregated::get_route(path)
        .or_else(|| core::get_route(path))
        .or_else(|| overlay::get_route(path))
        .or_else(|| diagnostics::get_route(path))
}
