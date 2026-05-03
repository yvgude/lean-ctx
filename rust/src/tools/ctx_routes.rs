use crate::core::graph_index;
use crate::core::route_extractor::{self, RouteEntry};

pub fn handle(method: Option<&str>, path_prefix: Option<&str>, project_root: &str) -> String {
    let index = graph_index::load_or_build(project_root);
    let routes = route_extractor::extract_routes_from_project(project_root, &index.files);

    if routes.is_empty() {
        return format!(
            "No HTTP routes found in project ({} files scanned)",
            index.file_count()
        );
    }

    let filtered = filter_routes(&routes, method, path_prefix);

    if filtered.is_empty() {
        let filter_desc = match (method, path_prefix) {
            (Some(m), Some(p)) => format!("{m} {p}"),
            (Some(m), None) => m.to_string(),
            (None, Some(p)) => p.to_string(),
            _ => String::new(),
        };
        return format!(
            "No routes matching '{}' ({} total routes found)",
            filter_desc,
            routes.len()
        );
    }

    let mut out = format!("{} route(s):\n", filtered.len());
    for route in &filtered {
        let handler = if route.handler.is_empty() {
            String::new()
        } else {
            format!(" → {}", route.handler)
        };
        out.push_str(&format!(
            "  {:>6} {}{} ({}:L{})\n",
            route.method, route.path, handler, route.file, route.line
        ));
    }
    out
}

fn filter_routes<'a>(
    routes: &'a [RouteEntry],
    method: Option<&str>,
    path_prefix: Option<&str>,
) -> Vec<&'a RouteEntry> {
    routes
        .iter()
        .filter(|r| {
            let method_match = method.is_none_or(|m| r.method.eq_ignore_ascii_case(m));
            let path_match = path_prefix.is_none_or(|p| r.path.starts_with(p));
            method_match && path_match
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::route_extractor::RouteEntry;

    fn sample_routes() -> Vec<RouteEntry> {
        vec![
            RouteEntry {
                method: "GET".to_string(),
                path: "/api/users".to_string(),
                handler: "getUsers".to_string(),
                file: "routes.ts".to_string(),
                line: 5,
            },
            RouteEntry {
                method: "POST".to_string(),
                path: "/api/users".to_string(),
                handler: "createUser".to_string(),
                file: "routes.ts".to_string(),
                line: 10,
            },
            RouteEntry {
                method: "GET".to_string(),
                path: "/api/items".to_string(),
                handler: "getItems".to_string(),
                file: "items.ts".to_string(),
                line: 3,
            },
        ]
    }

    #[test]
    fn filter_by_method() {
        let routes = sample_routes();
        let filtered = filter_routes(&routes, Some("GET"), None);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_by_path() {
        let routes = sample_routes();
        let filtered = filter_routes(&routes, None, Some("/api/users"));
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn filter_by_both() {
        let routes = sample_routes();
        let filtered = filter_routes(&routes, Some("POST"), Some("/api/users"));
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].handler, "createUser");
    }

    #[test]
    fn no_filter_returns_all() {
        let routes = sample_routes();
        let filtered = filter_routes(&routes, None, None);
        assert_eq!(filtered.len(), 3);
    }
}
