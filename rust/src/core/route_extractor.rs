use std::path::Path;

use serde::{Deserialize, Serialize};

macro_rules! static_regex {
    ($pattern:expr) => {{
        static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
        RE.get_or_init(|| {
            regex::Regex::new($pattern).expect(concat!("BUG: invalid static regex: ", $pattern))
        })
    }};
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RouteEntry {
    pub method: String,
    pub path: String,
    pub handler: String,
    pub file: String,
    pub line: usize,
}

pub fn extract_routes_from_file(file_path: &str, content: &str) -> Vec<RouteEntry> {
    let ext = Path::new(file_path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let mut routes = Vec::new();

    routes.extend(extract_express(file_path, content, ext));
    routes.extend(extract_flask(file_path, content, ext));
    routes.extend(extract_actix(file_path, content, ext));
    routes.extend(extract_spring(file_path, content, ext));
    routes.extend(extract_rails(file_path, content, ext));
    routes.extend(extract_fastapi(file_path, content, ext));
    routes.extend(extract_nextjs(file_path, content, ext));

    routes
}

pub fn extract_routes_from_project(
    project_root: &str,
    files: &std::collections::HashMap<String, super::graph_index::FileEntry>,
) -> Vec<RouteEntry> {
    let mut all_routes = Vec::new();

    for rel_path in files.keys() {
        if !is_route_candidate(rel_path) {
            continue;
        }
        let abs_path = Path::new(project_root).join(rel_path);
        let Ok(content) = std::fs::read_to_string(&abs_path) else {
            continue;
        };
        all_routes.extend(extract_routes_from_file(rel_path, &content));
    }

    all_routes.sort_by(|a, b| a.path.cmp(&b.path).then_with(|| a.method.cmp(&b.method)));
    all_routes
}

fn is_route_candidate(path: &str) -> bool {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");
    matches!(
        ext,
        "js" | "ts" | "jsx" | "tsx" | "py" | "rs" | "java" | "rb" | "go" | "kt"
    )
}

fn extract_express(file: &str, content: &str, ext: &str) -> Vec<RouteEntry> {
    if !matches!(ext, "js" | "ts" | "jsx" | "tsx") {
        return Vec::new();
    }

    let re = static_regex!(
        r#"(?:app|router|server)\s*\.\s*(get|post|put|patch|delete|all|use|options|head)\s*\(\s*['"`]([^'"`]+)['"`]"#
    );

    content
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            re.captures(line).map(|caps| {
                let method = caps[1].to_uppercase();
                let path = caps[2].to_string();
                let handler = extract_handler_name(line);
                RouteEntry {
                    method,
                    path,
                    handler,
                    file: file.to_string(),
                    line: i + 1,
                }
            })
        })
        .collect()
}

fn extract_flask(file: &str, content: &str, ext: &str) -> Vec<RouteEntry> {
    if ext != "py" {
        return Vec::new();
    }

    let route_re = static_regex!(
        r#"@(?:app|blueprint|bp)\s*\.\s*route\s*\(\s*['"]([^'"]+)['"](?:.*methods\s*=\s*\[([^\]]+)\])?"#
    );

    let method_re = static_regex!(
        r#"@(?:app|blueprint|bp)\s*\.\s*(get|post|put|patch|delete)\s*\(\s*['"]([^'"]+)['"]"#
    );

    let mut routes = Vec::new();

    for (i, line) in content.lines().enumerate() {
        if let Some(caps) = route_re.captures(line) {
            let path = caps[1].to_string();
            let methods = caps.get(2).map_or_else(
                || vec!["GET".to_string()],
                |m| {
                    m.as_str()
                        .replace(['\'', '"'], "")
                        .split(',')
                        .map(|s| s.trim().to_uppercase())
                        .collect::<Vec<_>>()
                },
            );

            let handler = find_next_def(content, i);
            for method in methods {
                routes.push(RouteEntry {
                    method,
                    path: path.clone(),
                    handler: handler.clone(),
                    file: file.to_string(),
                    line: i + 1,
                });
            }
        }

        if let Some(caps) = method_re.captures(line) {
            let method = caps[1].to_uppercase();
            let path = caps[2].to_string();
            let handler = find_next_def(content, i);
            routes.push(RouteEntry {
                method,
                path,
                handler,
                file: file.to_string(),
                line: i + 1,
            });
        }
    }

    routes
}

fn extract_fastapi(file: &str, content: &str, ext: &str) -> Vec<RouteEntry> {
    if ext != "py" {
        return Vec::new();
    }

    let re = static_regex!(
        r#"@(?:app|router)\s*\.\s*(get|post|put|patch|delete)\s*\(\s*['"]([^'"]+)['"]"#
    );

    content
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            re.captures(line).map(|caps| {
                let method = caps[1].to_uppercase();
                let path = caps[2].to_string();
                let handler = find_next_def(content, i);
                RouteEntry {
                    method,
                    path,
                    handler,
                    file: file.to_string(),
                    line: i + 1,
                }
            })
        })
        .collect()
}

fn extract_actix(file: &str, content: &str, ext: &str) -> Vec<RouteEntry> {
    if ext != "rs" {
        return Vec::new();
    }

    let attr_re = static_regex!(r#"#\[(get|post|put|patch|delete)\s*\(\s*"([^"]+)""#);

    let resource_re = static_regex!(
        r#"web::resource\s*\(\s*"([^"]+)"\s*\)\s*\.route\s*\(.*Method::(GET|POST|PUT|PATCH|DELETE)"#
    );

    let mut routes = Vec::new();

    for (i, line) in content.lines().enumerate() {
        if let Some(caps) = attr_re.captures(line) {
            let method = caps[1].to_uppercase();
            let path = caps[2].to_string();
            let handler = find_next_fn_rust(content, i);
            routes.push(RouteEntry {
                method,
                path,
                handler,
                file: file.to_string(),
                line: i + 1,
            });
        }

        if let Some(caps) = resource_re.captures(line) {
            let path = caps[1].to_string();
            let method = caps[2].to_uppercase();
            routes.push(RouteEntry {
                method,
                path,
                handler: extract_handler_name(line),
                file: file.to_string(),
                line: i + 1,
            });
        }
    }

    routes
}

fn extract_spring(file: &str, content: &str, ext: &str) -> Vec<RouteEntry> {
    if !matches!(ext, "java" | "kt") {
        return Vec::new();
    }

    let re = static_regex!(
        r#"@(GetMapping|PostMapping|PutMapping|PatchMapping|DeleteMapping|RequestMapping)\s*\(\s*(?:value\s*=\s*)?["']([^"']+)["']"#
    );

    content
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            re.captures(line).map(|caps| {
                let annotation = &caps[1];
                let method = match annotation {
                    "GetMapping" => "GET",
                    "PostMapping" => "POST",
                    "PutMapping" => "PUT",
                    "PatchMapping" => "PATCH",
                    "DeleteMapping" => "DELETE",
                    _ => "*",
                }
                .to_string();
                let path = caps[2].to_string();
                let handler = find_next_method_java(content, i);
                RouteEntry {
                    method,
                    path,
                    handler,
                    file: file.to_string(),
                    line: i + 1,
                }
            })
        })
        .collect()
}

fn extract_rails(file: &str, content: &str, ext: &str) -> Vec<RouteEntry> {
    if ext != "rb" {
        return Vec::new();
    }

    let re = static_regex!(
        r#"(get|post|put|patch|delete)\s+['"]([^'"]+)['"](?:\s*,\s*to:\s*['"]([^'"]+)['"])?"#
    );

    content
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            re.captures(line).map(|caps| {
                let method = caps[1].to_uppercase();
                let path = caps[2].to_string();
                let handler = caps
                    .get(3)
                    .map(|m| m.as_str().to_string())
                    .unwrap_or_default();
                RouteEntry {
                    method,
                    path,
                    handler,
                    file: file.to_string(),
                    line: i + 1,
                }
            })
        })
        .collect()
}

fn extract_nextjs(file: &str, content: &str, ext: &str) -> Vec<RouteEntry> {
    if !matches!(ext, "ts" | "js") {
        return Vec::new();
    }

    if !file.contains("api/") && !file.contains("app/") {
        return Vec::new();
    }

    let re = static_regex!(
        r"export\s+(?:async\s+)?function\s+(GET|POST|PUT|PATCH|DELETE|HEAD|OPTIONS)\s*\("
    );

    content
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            re.captures(line).map(|caps| {
                let method = caps[1].to_string();
                let route_path = file_to_nextjs_route(file);
                RouteEntry {
                    method,
                    path: route_path,
                    handler: caps[1].to_string(),
                    file: file.to_string(),
                    line: i + 1,
                }
            })
        })
        .collect()
}

fn file_to_nextjs_route(file: &str) -> String {
    let parts: Vec<&str> = file.split('/').collect();
    if let Some(api_pos) = parts.iter().position(|p| *p == "api") {
        let route_parts = &parts[api_pos..];
        let mut route = format!("/{}", route_parts.join("/"));
        if route.ends_with("/route.ts") || route.ends_with("/route.js") {
            route = route.replace("/route.ts", "").replace("/route.js", "");
        }
        route = route.replace('[', ":").replace(']', "");
        return route;
    }
    format!("/{file}")
}

fn extract_handler_name(line: &str) -> String {
    let parts: Vec<&str> = line.split([',', ')']).collect();
    if parts.len() > 1 {
        let handler = parts
            .last()
            .unwrap_or(&"")
            .trim()
            .trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if !handler.is_empty() {
            return handler.to_string();
        }
    }
    String::new()
}

fn find_next_def(content: &str, after_line: usize) -> String {
    let def_re = static_regex!(r"def\s+(\w+)");
    for line in content.lines().skip(after_line + 1).take(5) {
        if let Some(caps) = def_re.captures(line) {
            return caps[1].to_string();
        }
    }
    String::new()
}

fn find_next_fn_rust(content: &str, after_line: usize) -> String {
    let fn_re = static_regex!(r"(?:pub\s+)?(?:async\s+)?fn\s+(\w+)");
    for line in content.lines().skip(after_line + 1).take(5) {
        if let Some(caps) = fn_re.captures(line) {
            return caps[1].to_string();
        }
    }
    String::new()
}

fn find_next_method_java(content: &str, after_line: usize) -> String {
    let method_re = static_regex!(r"(?:public|private|protected)\s+\S+\s+(\w+)\s*\(");
    for line in content.lines().skip(after_line + 1).take(5) {
        if let Some(caps) = method_re.captures(line) {
            return caps[1].to_string();
        }
    }
    String::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn express_get_route() {
        let code = r"app.get('/api/users', getUsers);";
        let routes = extract_express("routes.js", code, "js");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/api/users");
    }

    #[test]
    fn express_post_route() {
        let code = r#"router.post("/api/items", createItem);"#;
        let routes = extract_express("routes.ts", code, "ts");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "POST");
        assert_eq!(routes[0].path, "/api/items");
    }

    #[test]
    fn flask_route_decorator() {
        let code = "@app.route('/hello')\ndef hello():\n    return 'hi'";
        let routes = extract_flask("app.py", code, "py");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/hello");
        assert_eq!(routes[0].handler, "hello");
    }

    #[test]
    fn flask_route_with_methods() {
        let code = "@app.route('/data', methods=['GET', 'POST'])\ndef handle_data():\n    pass";
        let routes = extract_flask("app.py", code, "py");
        assert_eq!(routes.len(), 2);
    }

    #[test]
    fn fastapi_route() {
        let code = "@app.get('/items')\nasync def list_items():\n    pass";
        let routes = extract_fastapi("main.py", code, "py");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].handler, "list_items");
    }

    #[test]
    fn actix_attribute_route() {
        let code = "#[get(\"/health\")]\nasync fn health_check() -> impl Responder {\n    HttpResponse::Ok()\n}";
        let routes = extract_actix("main.rs", code, "rs");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/health");
        assert_eq!(routes[0].handler, "health_check");
    }

    #[test]
    fn spring_get_mapping() {
        let code = "@GetMapping(\"/api/users\")\npublic List<User> getUsers() {";
        let routes = extract_spring("UserController.java", code, "java");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/api/users");
        assert_eq!(routes[0].handler, "getUsers");
    }

    #[test]
    fn rails_route() {
        let code = "get '/users', to: 'users#index'";
        let routes = extract_rails("routes.rb", code, "rb");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert_eq!(routes[0].path, "/users");
        assert_eq!(routes[0].handler, "users#index");
    }

    #[test]
    fn nextjs_route_handler() {
        let code = "export async function GET(request: Request) {\n  return Response.json({});\n}";
        let routes = extract_nextjs("src/app/api/users/route.ts", code, "ts");
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].method, "GET");
        assert!(routes[0].path.contains("/api/users"));
    }

    #[test]
    fn ignores_non_route_files() {
        assert!(!is_route_candidate("README.md"));
        assert!(!is_route_candidate("image.png"));
        assert!(is_route_candidate("server.ts"));
        assert!(is_route_candidate("routes.rb"));
    }
}
