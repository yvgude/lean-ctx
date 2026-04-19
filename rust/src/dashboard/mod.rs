use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const DEFAULT_PORT: u16 = 3333;
const DEFAULT_HOST: &str = "127.0.0.1";
const DASHBOARD_HTML: &str = include_str!("dashboard.html");

pub async fn start(port: Option<u16>, host: Option<String>) {
    let port = port.unwrap_or_else(|| {
        std::env::var("LEAN_CTX_PORT")
            .ok()
            .and_then(|p| p.parse().ok())
            .unwrap_or(DEFAULT_PORT)
    });

    let host = host.unwrap_or_else(|| {
        std::env::var("LEAN_CTX_HOST")
            .ok()
            .unwrap_or_else(|| DEFAULT_HOST.to_string())
    });

    let addr = format!("{host}:{port}");
    let is_local = host == "127.0.0.1" || host == "localhost" || host == "::1";

    // Avoid accidental multiple dashboard instances (common source of "it hangs").
    // Only safe to auto-detect for local dashboards without auth.
    if is_local && dashboard_responding(&host, port) {
        println!("\n  lean-ctx dashboard already running → http://{host}:{port}");
        println!("  Tip: use Ctrl+C in the existing terminal to stop it.\n");
        open_browser(&format!("http://localhost:{port}"));
        return;
    }

    let token = if !is_local {
        let t = generate_token();
        save_token(&t);
        Some(Arc::new(t))
    } else {
        None
    };

    if !is_local {
        let t = token.as_ref().unwrap();
        eprintln!(
            "  \x1b[33m⚠\x1b[0m Binding to {host} — authentication enabled.\n  \
             Bearer token: \x1b[1;32m{t}\x1b[0m\n  \
             Browser URL:  http://<your-ip>:{port}/?token={t}"
        );
    }

    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind to {addr}: {e}");
            std::process::exit(1);
        }
    };

    let stats_path = crate::core::data_dir::lean_ctx_data_dir()
        .map(|d| d.join("stats.json").display().to_string())
        .unwrap_or_else(|_| "~/.lean-ctx/stats.json".to_string());

    if host == "0.0.0.0" {
        println!("\n  lean-ctx dashboard → http://0.0.0.0:{port} (all interfaces)");
        println!("  Local access:  http://localhost:{port}");
    } else {
        println!("\n  lean-ctx dashboard → http://{host}:{port}");
    }
    println!("  Stats file: {stats_path}");
    println!("  Press Ctrl+C to stop\n");

    if is_local {
        open_browser(&format!("http://localhost:{port}"));
    }
    if crate::shell::is_container() && is_local {
        println!("  Tip (Docker): bind 0.0.0.0 + publish port:");
        println!("    lean-ctx dashboard --host=0.0.0.0 --port={port}");
        println!("    docker run ... -p {port}:{port} ...");
        println!();
    }

    loop {
        if let Ok((stream, _)) = listener.accept().await {
            let token_ref = token.clone();
            tokio::spawn(handle_request(stream, token_ref));
        }
    }
}

fn generate_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let seed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("lctx_{:016x}", seed ^ 0xdeadbeef_cafebabe)
}

fn save_token(token: &str) {
    if let Ok(dir) = crate::core::data_dir::lean_ctx_data_dir() {
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("dashboard.token"), token);
    }
}

fn open_browser(url: &str) {
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }

    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open")
            .arg(url)
            .stderr(std::process::Stdio::null())
            .spawn();
    }

    #[cfg(target_os = "windows")]
    {
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn();
    }
}

fn dashboard_responding(host: &str, port: u16) -> bool {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::Duration;

    let addr = format!("{host}:{port}");
    let Ok(mut s) = TcpStream::connect_timeout(
        &addr
            .parse()
            .unwrap_or_else(|_| std::net::SocketAddr::from(([127, 0, 0, 1], port))),
        Duration::from_millis(150),
    ) else {
        return false;
    };
    let _ = s.set_read_timeout(Some(Duration::from_millis(150)));
    let _ = s.set_write_timeout(Some(Duration::from_millis(150)));

    let req = "GET /api/version HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    if s.write_all(req.as_bytes()).is_err() {
        return false;
    }
    let mut buf = [0u8; 256];
    let Ok(n) = s.read(&mut buf) else {
        return false;
    };
    let head = String::from_utf8_lossy(&buf[..n]);
    head.starts_with("HTTP/1.1 200") || head.starts_with("HTTP/1.0 200")
}

async fn handle_request(mut stream: tokio::net::TcpStream, token: Option<Arc<String>>) {
    let mut buf = vec![0u8; 4096];
    let n = match stream.read(&mut buf).await {
        Ok(n) if n > 0 => n,
        _ => return,
    };

    let request = String::from_utf8_lossy(&buf[..n]);

    let raw_path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");

    let (path, query_token) = if let Some(idx) = raw_path.find('?') {
        let p = &raw_path[..idx];
        let qs = &raw_path[idx + 1..];
        let tok = qs
            .split('&')
            .find_map(|pair| pair.strip_prefix("token="))
            .map(|t| t.to_string());
        (p.to_string(), tok)
    } else {
        (raw_path.to_string(), None)
    };

    let query_str = raw_path.find('?').map(|i| &raw_path[i + 1..]).unwrap_or("");

    let is_api = path.starts_with("/api/");

    if let Some(ref expected) = token {
        let has_header_auth = check_auth(&request, expected);
        let has_query_auth = query_token
            .as_deref()
            .map(|t| t == expected.as_str())
            .unwrap_or(false);

        if is_api && !has_header_auth && !has_query_auth {
            let body = r#"{"error":"unauthorized"}"#;
            let response = format!(
                "HTTP/1.1 401 Unauthorized\r\n\
                 Content-Type: application/json\r\n\
                 Content-Length: {}\r\n\
                 WWW-Authenticate: Bearer\r\n\
                 Connection: close\r\n\
                 \r\n\
                 {body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
            return;
        }
    }

    let path = path.as_str();

    let compute =
        std::panic::catch_unwind(|| route_response(path, query_str, &query_token, &token));
    let (status, content_type, body) = match compute {
        Ok(v) => v,
        Err(_) => (
            "500 Internal Server Error",
            "application/json",
            r#"{"error":"dashboard route panicked"}"#.to_string(),
        ),
    };

    let cache_header = if content_type.starts_with("application/json") {
        "Cache-Control: no-cache, no-store, must-revalidate\r\nPragma: no-cache\r\n"
    } else {
        ""
    };

    let response = format!(
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         {cache_header}\
         Access-Control-Allow-Origin: *\r\n\
         Connection: close\r\n\
         \r\n\
         {body}",
        body.len()
    );

    let _ = stream.write_all(response.as_bytes()).await;
}

fn route_response(
    path: &str,
    query_str: &str,
    query_token: &Option<String>,
    token: &Option<Arc<String>>,
) -> (&'static str, &'static str, String) {
    match path {
        "/api/stats" => {
            let store = crate::core::stats::load();
            let json = serde_json::to_string(&store).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/gain" => {
            let env_model = std::env::var("LEAN_CTX_MODEL")
                .or_else(|_| std::env::var("LCTX_MODEL"))
                .ok();
            let engine = crate::core::gain::GainEngine::load();
            let payload = serde_json::json!({
                "summary": engine.summary(env_model.as_deref()),
                "tasks": engine.task_breakdown(),
                "heatmap": engine.heatmap_gains(20),
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/mcp" => {
            let mcp_path = crate::core::data_dir::lean_ctx_data_dir()
                .map(|d| d.join("mcp-live.json"))
                .unwrap_or_default();
            let json = std::fs::read_to_string(&mcp_path).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/agents" => {
            let json = build_agents_json();
            ("200 OK", "application/json", json)
        }
        "/api/knowledge" => {
            let project_root = detect_project_root_for_dashboard();
            let _ =
                crate::core::knowledge::ProjectKnowledge::migrate_legacy_empty_root(&project_root);
            let knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(&project_root);
            let json = serde_json::to_string(&knowledge).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/gotchas" => {
            let project_root = detect_project_root_for_dashboard();
            let store = crate::core::gotcha_tracker::GotchaStore::load(&project_root);
            let json = serde_json::to_string(&store).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/buddy" => {
            let buddy = crate::core::buddy::BuddyState::compute();
            let json = serde_json::to_string(&buddy).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/version" => {
            let json = crate::core::version_check::version_info_json();
            ("200 OK", "application/json", json)
        }
        "/api/heatmap" => {
            let project_root = detect_project_root_for_dashboard();
            let index = crate::core::graph_index::load_or_build(&project_root);
            let entries = build_heatmap_json(&index);
            ("200 OK", "application/json", entries)
        }
        "/api/events" => {
            let evs = crate::core::events::load_events_from_file(200);
            let json = serde_json::to_string(&evs).unwrap_or_else(|_| "[]".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/graph" => {
            let root = detect_project_root_for_dashboard();
            let index = crate::core::graph_index::load_or_build(&root);
            let json = serde_json::to_string(&index).unwrap_or_else(|_| {
                "{\"error\":\"failed to serialize project index\"}".to_string()
            });
            ("200 OK", "application/json", json)
        }
        "/api/feedback" => {
            let store = crate::core::feedback::FeedbackStore::load();
            let json = serde_json::to_string(&store).unwrap_or_else(|_| {
                "{\"error\":\"failed to serialize feedback store\"}".to_string()
            });
            ("200 OK", "application/json", json)
        }
        "/api/session" => {
            let session = crate::core::session::SessionState::load_latest().unwrap_or_default();
            let json = serde_json::to_string(&session)
                .unwrap_or_else(|_| "{\"error\":\"failed to serialize session\"}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/search-index" => {
            let root_s = detect_project_root_for_dashboard();
            let root = std::path::Path::new(&root_s);
            let index = crate::core::vector_index::BM25Index::load_or_build(root);
            let summary = bm25_index_summary_json(&index);
            let json = serde_json::to_string(&summary).unwrap_or_else(|_| {
                "{\"error\":\"failed to serialize search index summary\"}".to_string()
            });
            ("200 OK", "application/json", json)
        }
        "/api/compression-demo" => {
            let body = match extract_query_param(query_str, "path") {
                None => r#"{"error":"missing path query parameter"}"#.to_string(),
                Some(rel) => {
                    let task = extract_query_param(query_str, "task");
                    let root = detect_project_root_for_dashboard();
                    let root_pb = std::path::Path::new(&root);
                    let candidate = std::path::Path::new(&rel);
                    let full = if candidate.is_absolute() {
                        candidate.to_path_buf()
                    } else {
                        let direct = root_pb.join(&rel);
                        if direct.exists() {
                            direct
                        } else {
                            let in_rust = root_pb.join("rust").join(&rel);
                            if in_rust.exists() {
                                in_rust
                            } else {
                                direct
                            }
                        }
                    };
                    match std::fs::read_to_string(&full) {
                        Ok(content) => {
                            let ext = full.extension().and_then(|e| e.to_str()).unwrap_or("rs");
                            let path_str = full.to_string_lossy().to_string();
                            let original_lines = content.lines().count();
                            let original_tokens = crate::core::tokens::count_tokens(&content);
                            let modes = compression_demo_modes_json(
                                &content,
                                &path_str,
                                ext,
                                original_tokens,
                                task.as_deref(),
                            );
                            let original_preview: String = content.chars().take(8000).collect();
                            serde_json::json!({
                                "path": path_str,
                                "task": task,
                                "original_lines": original_lines,
                                "original_tokens": original_tokens,
                                "original": original_preview,
                                "modes": modes,
                            })
                            .to_string()
                        }
                        Err(_) => r#"{"error":"failed to read file"}"#.to_string(),
                    }
                }
            };
            ("200 OK", "application/json", body)
        }
        "/" | "/index.html" => {
            let mut html = DASHBOARD_HTML.to_string();
            if let Some(ref tok) = query_token {
                let script = format!(
                    "<script>window.__LEAN_CTX_TOKEN__=\"{}\";</script>",
                    tok.replace('"', "")
                );
                html = html.replacen("<head>", &format!("<head>{script}"), 1);
            } else if let Some(ref t) = token {
                let script = format!(
                    "<script>window.__LEAN_CTX_TOKEN__=\"{}\";</script>",
                    t.as_str()
                );
                html = html.replacen("<head>", &format!("<head>{script}"), 1);
            }
            ("200 OK", "text/html; charset=utf-8", html)
        }
        "/favicon.ico" => ("204 No Content", "text/plain", String::new()),
        _ => ("404 Not Found", "text/plain", "Not Found".to_string()),
    }
}

fn check_auth(request: &str, expected_token: &str) -> bool {
    for line in request.lines() {
        let lower = line.to_lowercase();
        if lower.starts_with("authorization:") {
            let value = line["authorization:".len()..].trim();
            if let Some(token) = value.strip_prefix("Bearer ") {
                return token.trim() == expected_token;
            }
            if let Some(token) = value.strip_prefix("bearer ") {
                return token.trim() == expected_token;
            }
        }
    }
    false
}

fn extract_query_param(qs: &str, key: &str) -> Option<String> {
    for pair in qs.split('&') {
        let (k, v) = match pair.split_once('=') {
            Some(kv) => kv,
            None => continue,
        };
        if k == key {
            return Some(percent_decode_query_component(v));
        }
    }
    None
}

fn percent_decode_query_component(s: &str) -> String {
    let mut out: Vec<u8> = Vec::with_capacity(s.len());
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < b.len() => {
                let h1 = (b[i + 1] as char).to_digit(16);
                let h2 = (b[i + 2] as char).to_digit(16);
                if let (Some(a), Some(d)) = (h1, h2) {
                    out.push(((a << 4) | d) as u8);
                    i += 3;
                } else {
                    out.push(b'%');
                    i += 1;
                }
            }
            _ => {
                out.push(b[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn compression_mode_json(output: &str, original_tokens: usize) -> serde_json::Value {
    let tokens = crate::core::tokens::count_tokens(output);
    let savings_pct = if original_tokens > 0 {
        ((original_tokens.saturating_sub(tokens)) as f64 / original_tokens as f64 * 100.0).round()
            as i64
    } else {
        0
    };
    serde_json::json!({
        "output": output,
        "tokens": tokens,
        "savings_pct": savings_pct
    })
}

fn compression_demo_modes_json(
    content: &str,
    path: &str,
    ext: &str,
    original_tokens: usize,
    task: Option<&str>,
) -> serde_json::Value {
    let map_out = crate::core::signatures::extract_file_map(path, content);
    let sig_out = crate::core::signatures::extract_signatures(content, ext)
        .iter()
        .map(|s| s.to_compact())
        .collect::<Vec<_>>()
        .join("\n");
    let aggressive_out = crate::core::filters::aggressive_filter(content);
    let entropy_out = crate::core::entropy::entropy_compress_adaptive(content, path).output;

    let mut cache = crate::core::cache::SessionCache::new();
    let reference_out =
        crate::tools::ctx_read::handle(&mut cache, path, "reference", crate::tools::CrpMode::Off);
    let task_out = task.filter(|t| !t.trim().is_empty()).map(|t| {
        crate::tools::ctx_read::handle_with_task(
            &mut cache,
            path,
            "task",
            crate::tools::CrpMode::Off,
            Some(t),
        )
    });

    serde_json::json!({
        "map": compression_mode_json(&map_out, original_tokens),
        "signatures": compression_mode_json(&sig_out, original_tokens),
        "reference": compression_mode_json(&reference_out, original_tokens),
        "aggressive": compression_mode_json(&aggressive_out, original_tokens),
        "entropy": compression_mode_json(&entropy_out, original_tokens),
        "task": task_out.as_deref().map(|s| compression_mode_json(s, original_tokens)).unwrap_or(serde_json::Value::Null),
    })
}

fn bm25_index_summary_json(index: &crate::core::vector_index::BM25Index) -> serde_json::Value {
    let mut sorted: Vec<&crate::core::vector_index::CodeChunk> = index.chunks.iter().collect();
    sorted.sort_by_key(|c| std::cmp::Reverse(c.token_count));
    let top: Vec<serde_json::Value> = sorted
        .into_iter()
        .take(20)
        .map(|c| {
            serde_json::json!({
                "file_path": c.file_path,
                "symbol_name": c.symbol_name,
                "token_count": c.token_count,
                "kind": c.kind,
                "start_line": c.start_line,
                "end_line": c.end_line,
            })
        })
        .collect();
    let mut lang: HashMap<String, usize> = HashMap::new();
    for c in &index.chunks {
        let e = std::path::Path::new(&c.file_path)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_string();
        *lang.entry(e).or_default() += 1;
    }
    serde_json::json!({
        "doc_count": index.doc_count,
        "chunk_count": index.chunks.len(),
        "top_chunks_by_token_count": top,
        "language_distribution": lang,
    })
}

fn build_heatmap_json(index: &crate::core::graph_index::ProjectIndex) -> String {
    let mut connection_counts: std::collections::HashMap<String, usize> =
        std::collections::HashMap::new();
    for edge in &index.edges {
        *connection_counts.entry(edge.from.clone()).or_default() += 1;
        *connection_counts.entry(edge.to.clone()).or_default() += 1;
    }

    let max_tokens = index
        .files
        .values()
        .map(|f| f.token_count)
        .max()
        .unwrap_or(1) as f64;
    let max_connections = connection_counts.values().max().copied().unwrap_or(1) as f64;

    let mut entries: Vec<serde_json::Value> = index
        .files
        .values()
        .map(|f| {
            let connections = connection_counts.get(&f.path).copied().unwrap_or(0);
            let token_norm = f.token_count as f64 / max_tokens;
            let conn_norm = connections as f64 / max_connections;
            let heat = token_norm * 0.4 + conn_norm * 0.6;
            serde_json::json!({
                "path": f.path,
                "tokens": f.token_count,
                "connections": connections,
                "language": f.language,
                "heat": (heat * 100.0).round() / 100.0,
            })
        })
        .collect();

    entries.sort_by(|a, b| {
        b["heat"]
            .as_f64()
            .unwrap_or(0.0)
            .partial_cmp(&a["heat"].as_f64().unwrap_or(0.0))
            .unwrap()
    });

    serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string())
}

fn build_agents_json() -> String {
    let registry = crate::core::agents::AgentRegistry::load_or_create();
    let agents: Vec<serde_json::Value> = registry
        .agents
        .iter()
        .filter(|a| a.status != crate::core::agents::AgentStatus::Finished)
        .map(|a| {
            let age_min = (chrono::Utc::now() - a.last_active).num_minutes();
            serde_json::json!({
                "id": a.agent_id,
                "type": a.agent_type,
                "role": a.role,
                "status": format!("{}", a.status),
                "status_message": a.status_message,
                "last_active_minutes_ago": age_min,
                "pid": a.pid
            })
        })
        .collect();

    let pending_msgs = registry.scratchpad.len();

    let shared_dir = crate::core::data_dir::lean_ctx_data_dir()
        .unwrap_or_else(|_| dirs::home_dir().unwrap_or_default().join(".lean-ctx"))
        .join("agents")
        .join("shared");
    let shared_count = if shared_dir.exists() {
        std::fs::read_dir(&shared_dir)
            .map(|rd| rd.count())
            .unwrap_or(0)
    } else {
        0
    };

    serde_json::json!({
        "agents": agents,
        "total_active": agents.len(),
        "pending_messages": pending_msgs,
        "shared_contexts": shared_count
    })
    .to_string()
}

fn detect_project_root_for_dashboard() -> String {
    // Prefer last known project context from the persisted session. This makes the dashboard
    // show the same project data even if it is launched from an arbitrary working directory.
    if let Some(session) = crate::core::session::SessionState::load_latest() {
        if let Some(root) = session.project_root.as_deref() {
            if !root.trim().is_empty() {
                return promote_to_git_root(root);
            }
        }
        if let Some(cwd) = session.shell_cwd.as_deref() {
            if !cwd.trim().is_empty() {
                let r = crate::core::protocol::detect_project_root_or_cwd(cwd);
                return promote_to_git_root(&r);
            }
        }
        if let Some(last) = session.files_touched.last() {
            if !last.path.trim().is_empty() {
                if let Some(parent) = Path::new(&last.path).parent() {
                    let p = parent.to_string_lossy().to_string();
                    let r = crate::core::protocol::detect_project_root_or_cwd(&p);
                    return promote_to_git_root(&r);
                }
            }
        }
    }

    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| ".".to_string());
    let r = crate::core::protocol::detect_project_root_or_cwd(&cwd);
    promote_to_git_root(&r)
}

fn promote_to_git_root(path: &str) -> String {
    git_root_for(path).unwrap_or_else(|| path.to_string())
}

fn git_root_for(path: &str) -> Option<String> {
    let mut p = Path::new(path);
    loop {
        let git = p.join(".git");
        if git.exists() {
            return Some(p.to_string_lossy().to_string());
        }
        p = p.parent()?;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_auth_with_valid_bearer() {
        let req = "GET /api/stats HTTP/1.1\r\nAuthorization: Bearer lctx_abc123\r\n\r\n";
        assert!(check_auth(req, "lctx_abc123"));
    }

    #[test]
    fn check_auth_with_invalid_bearer() {
        let req = "GET /api/stats HTTP/1.1\r\nAuthorization: Bearer wrong_token\r\n\r\n";
        assert!(!check_auth(req, "lctx_abc123"));
    }

    #[test]
    fn check_auth_missing_header() {
        let req = "GET /api/stats HTTP/1.1\r\nHost: localhost\r\n\r\n";
        assert!(!check_auth(req, "lctx_abc123"));
    }

    #[test]
    fn check_auth_lowercase_bearer() {
        let req = "GET /api/stats HTTP/1.1\r\nauthorization: bearer lctx_abc123\r\n\r\n";
        assert!(check_auth(req, "lctx_abc123"));
    }

    #[test]
    fn query_token_parsing() {
        let raw_path = "/index.html?token=lctx_abc123&other=val";
        let idx = raw_path.find('?').unwrap();
        let qs = &raw_path[idx + 1..];
        let tok = qs.split('&').find_map(|pair| pair.strip_prefix("token="));
        assert_eq!(tok, Some("lctx_abc123"));
    }

    #[test]
    fn api_path_detection() {
        assert!("/api/stats".starts_with("/api/"));
        assert!("/api/version".starts_with("/api/"));
        assert!(!"/".starts_with("/api/"));
        assert!(!"/index.html".starts_with("/api/"));
        assert!(!"/favicon.ico".starts_with("/api/"));
    }
}
