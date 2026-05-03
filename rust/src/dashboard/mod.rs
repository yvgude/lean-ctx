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

    let token = if is_local {
        None
    } else {
        let t = generate_token();
        save_token(&t);
        Some(Arc::new(t))
    };

    if !is_local {
        if let Some(t) = token.as_ref() {
            eprintln!(
                "  \x1b[33m⚠\x1b[0m Binding to {host} — authentication enabled.\n  \
                 Bearer token: \x1b[1;32m{t}\x1b[0m\n  \
                 Browser URL:  http://<your-ip>:{port}/?token={t}"
            );
        }
    }

    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Failed to bind to {addr}: {e}");
            std::process::exit(1);
        }
    };

    let stats_path = crate::core::data_dir::lean_ctx_data_dir().map_or_else(
        |_| "~/.lean-ctx/stats.json".to_string(),
        |d| d.join("stats.json").display().to_string(),
    );

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
            .map(std::string::ToString::to_string);
        (p.to_string(), tok)
    } else {
        (raw_path.to_string(), None)
    };

    let query_str = raw_path.find('?').map_or("", |i| &raw_path[i + 1..]);

    let is_api = path.starts_with("/api/");

    if let Some(ref expected) = token {
        let has_header_auth = check_auth(&request, expected);
        let has_query_auth = query_token
            .as_deref()
            .is_some_and(|t| t == expected.as_str());

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

    let compute = std::panic::catch_unwind(|| {
        route_response(path, query_str, query_token.as_ref(), token.as_ref())
    });
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
    query_token: Option<&String>,
    token: Option<&Arc<String>>,
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
        "/api/profile" => {
            let active_name = crate::core::profiles::active_profile_name();
            let profile = crate::core::profiles::active_profile();
            let all = crate::core::profiles::list_profiles();
            let active_info = all.iter().find(|p| p.name == active_name);
            let available: Vec<serde_json::Value> = all
                .iter()
                .map(|p| {
                    serde_json::json!({
                        "name": p.name,
                        "description": p.description,
                        "source": p.source.to_string(),
                    })
                })
                .collect();
            let payload = serde_json::json!({
                "active_name": active_name,
                "active_source": active_info.map(|i| i.source.to_string()),
                "active_description": active_info.map(|i| i.description.clone()),
                "profile": profile,
                "available": available,
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/knowledge" => {
            let project_root = detect_project_root_for_dashboard();
            let policy = crate::core::config::Config::load()
                .memory_policy_effective()
                .unwrap_or_default();
            let _ = crate::core::knowledge::ProjectKnowledge::migrate_legacy_empty_root(
                &project_root,
                &policy,
            );

            let mut knowledge =
                crate::core::knowledge::ProjectKnowledge::load_or_create(&project_root);
            if knowledge.facts.is_empty() {
                // Keep /api/knowledge fast: avoid forcing a full index build here.
                let idx = crate::core::graph_index::ProjectIndex::load(&project_root);
                if crate::core::knowledge_bootstrap::bootstrap_if_empty(
                    &mut knowledge,
                    &project_root,
                    idx.as_ref(),
                    &policy,
                ) {
                    let _ = knowledge.save();
                }
            }
            let json = serde_json::to_string(&knowledge).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/knowledge-relations" => {
            let project_root = detect_project_root_for_dashboard();
            let policy = crate::core::config::Config::load()
                .memory_policy_effective()
                .unwrap_or_default();

            let knowledge = crate::core::knowledge::ProjectKnowledge::load_or_create(&project_root);
            let graph = crate::core::knowledge_relations::KnowledgeRelationGraph::load_or_create(
                &knowledge.project_hash,
            );

            let current_ids: std::collections::HashSet<String> = knowledge
                .facts
                .iter()
                .filter(|f| f.is_current())
                .map(|f| format!("{}/{}", f.category, f.key))
                .collect();

            let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
            let mut edges: Vec<serde_json::Value> = Vec::new();

            let mut push_edge = |from: String, to: String, kind: String, derived: bool| {
                if from.trim().is_empty() || to.trim().is_empty() || from == to {
                    return;
                }
                if !current_ids.contains(&from) || !current_ids.contains(&to) {
                    return;
                }
                let key = format!("{from}|{kind}|{to}");
                if !seen.insert(key) {
                    return;
                }
                edges.push(serde_json::json!({
                    "from": from,
                    "to": to,
                    "kind": kind,
                    "derived": derived,
                }));
            };

            // Explicit user-managed relations.
            for e in &graph.edges {
                push_edge(e.from.id(), e.to.id(), e.kind.as_str().to_string(), false);
            }

            // Derived: `supersedes` links (stored on facts).
            for f in knowledge.facts.iter().filter(|f| f.is_current()) {
                let Some(to) = f
                    .supersedes
                    .as_deref()
                    .and_then(crate::core::knowledge_relations::parse_node_ref)
                else {
                    continue;
                };
                let from = format!("{}/{}", f.category, f.key);
                push_edge(from, to.id(), "supersedes".to_string(), true);
            }

            // Derived: soft references in values like `category/key` or `category:key`.
            for f in knowledge.facts.iter().filter(|f| f.is_current()) {
                let from = format!("{}/{}", f.category, f.key);
                for raw in f.value.split_whitespace() {
                    let tok = raw.trim_matches(|c: char| {
                        !c.is_ascii_alphanumeric() && c != '/' && c != ':' && c != '_' && c != '-'
                    });
                    let Some(to) = crate::core::knowledge_relations::parse_node_ref(tok) else {
                        continue;
                    };
                    if to.id() == from {
                        continue;
                    }
                    push_edge(from.clone(), to.id(), "related_to".to_string(), true);
                }
            }

            let max_edges = policy.knowledge.max_facts.saturating_mul(8);
            if max_edges > 0 && edges.len() > max_edges {
                edges.truncate(max_edges);
            }

            let payload = serde_json::json!({
                "project_root": project_root,
                "project_hash": knowledge.project_hash,
                "edges": edges,
                "explicit_edges_total": graph.edges.len(),
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
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
        "/api/pulse" => {
            let stats_path = crate::core::data_dir::lean_ctx_data_dir()
                .map(|d| d.join("stats.json"))
                .unwrap_or_default();
            let meta = std::fs::metadata(&stats_path).ok();
            let size = meta.as_ref().map_or(0, std::fs::Metadata::len);
            let mtime = meta
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0, |d| d.as_secs());
            use md5::Digest;
            let hash = format!(
                "{:x}",
                md5::Md5::digest(format!("{size}-{mtime}").as_bytes())
            );
            let json = format!(r#"{{"hash":"{hash}","ts":{mtime}}}"#);
            ("200 OK", "application/json", json)
        }
        "/api/heatmap" => {
            let project_root = detect_project_root_for_dashboard();
            let index = crate::core::graph_index::load_or_build(&project_root);
            let entries = build_heatmap_json(&index);
            ("200 OK", "application/json", entries)
        }
        "/metrics" => {
            let prom = crate::core::telemetry::global_metrics().to_prometheus();
            ("200 OK", "text/plain; version=0.0.4; charset=utf-8", prom)
        }
        "/api/anomaly" => {
            let s = crate::core::anomaly::summary();
            let json = serde_json::to_string(&s).unwrap_or_else(|_| "[]".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/episodes" => {
            let root = detect_project_root_for_dashboard();
            let hash = crate::core::project_hash::hash_project_root(&root);
            let store = crate::core::episodic_memory::EpisodicStore::load_or_create(&hash);
            let stats = store.stats();
            let recent: Vec<_> = store.recent(20).into_iter().cloned().collect();
            let payload = serde_json::json!({
                "project_root": root,
                "project_hash": hash,
                "stats": {
                    "total_episodes": stats.total_episodes,
                    "successes": stats.successes,
                    "failures": stats.failures,
                    "success_rate": stats.success_rate,
                    "total_tokens": stats.total_tokens,
                },
                "recent": recent,
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/procedures" => {
            let root = detect_project_root_for_dashboard();
            let hash = crate::core::project_hash::hash_project_root(&root);
            let store = crate::core::procedural_memory::ProceduralStore::load_or_create(&hash);
            let task = extract_query_param(query_str, "task").or_else(|| {
                crate::core::session::SessionState::load_latest_for_project_root(&root)
                    .and_then(|s| s.task.map(|t| t.description))
            });
            let suggestions: Vec<serde_json::Value> = task.as_deref().map_or(Vec::new(), |t| {
                store
                    .suggest(t)
                    .into_iter()
                    .take(10)
                    .map(|p| {
                        serde_json::json!({
                            "id": p.id,
                            "name": p.name,
                            "description": p.description,
                            "confidence": p.confidence,
                            "times_used": p.times_used,
                            "times_succeeded": p.times_succeeded,
                            "success_rate": p.success_rate(),
                            "steps": p.steps,
                            "activation_keywords": p.activation_keywords,
                            "last_used": p.last_used,
                            "created_at": p.created_at,
                        })
                    })
                    .collect()
            });
            let payload = serde_json::json!({
                "project_root": root,
                "project_hash": hash,
                "total_procedures": store.procedures.len(),
                "task": task,
                "suggestions": suggestions,
                "procedures": store.procedures,
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/verification" => {
            let snap = crate::core::output_verification::stats_snapshot();
            let json = serde_json::to_string(&snap).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/slos" => {
            let snap = crate::core::slo::evaluate_quiet();
            let history = crate::core::slo::violation_history(100);
            let payload = serde_json::json!({
                "snapshot": snap,
                "history": history,
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
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
        "/api/graph/enrich" => {
            let root = detect_project_root_for_dashboard();
            let project_path = std::path::Path::new(&root);
            let result = match crate::core::property_graph::CodeGraph::open(project_path) {
                Ok(graph) => {
                    match crate::core::graph_enricher::enrich_graph(&graph, project_path, 500) {
                        Ok(stats) => {
                            let nc = graph.node_count().unwrap_or(0);
                            let ec = graph.edge_count().unwrap_or(0);
                            serde_json::json!({
                                "commits_indexed": stats.commits_indexed,
                                "tests_indexed": stats.tests_indexed,
                                "knowledge_indexed": stats.knowledge_indexed,
                                "edges_created": stats.edges_created,
                                "total_nodes": nc,
                                "total_edges": ec,
                            })
                        }
                        Err(e) => serde_json::json!({"error": e.to_string()}),
                    }
                }
                Err(e) => serde_json::json!({"error": e.to_string()}),
            };
            let json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/graph/stats" => {
            let root = detect_project_root_for_dashboard();
            let project_path = std::path::Path::new(&root);
            let result = match crate::core::property_graph::CodeGraph::open(project_path) {
                Ok(graph) => {
                    let nc = graph.node_count().unwrap_or(0);
                    let ec = graph.edge_count().unwrap_or(0);
                    serde_json::json!({
                        "node_count": nc,
                        "edge_count": ec,
                        "db_path": graph.db_path().display().to_string(),
                    })
                }
                Err(e) => serde_json::json!({"error": e.to_string()}),
            };
            let json = serde_json::to_string(&result).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/call-graph" => {
            let root = detect_project_root_for_dashboard();
            let index = crate::core::graph_index::load_or_build(&root);
            let call_graph = crate::core::call_graph::CallGraph::load_or_build(&root, &index);
            let _ = call_graph.save();
            let payload = serde_json::json!({
                "project_root": call_graph.project_root,
                "edges": call_graph.edges,
                "file_hashes": call_graph.file_hashes,
                "indexed_file_count": index.files.len(),
                "indexed_symbol_count": index.symbols.len(),
                "analyzed_file_count": call_graph.file_hashes.len(),
            });
            let json = serde_json::to_string(&payload)
                .unwrap_or_else(|_| "{\"error\":\"failed to serialize call graph\"}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/feedback" => {
            let store = crate::core::feedback::FeedbackStore::load();
            let json = serde_json::to_string(&store).unwrap_or_else(|_| {
                "{\"error\":\"failed to serialize feedback store\"}".to_string()
            });
            ("200 OK", "application/json", json)
        }
        "/api/symbols" => {
            let root = detect_project_root_for_dashboard();
            let index = crate::core::graph_index::load_or_build(&root);
            let q = extract_query_param(query_str, "q");
            let kind = extract_query_param(query_str, "kind");
            let json = build_symbols_json(&index, q.as_deref(), kind.as_deref());
            ("200 OK", "application/json", json)
        }
        "/api/routes" => {
            let root = detect_project_root_for_dashboard();
            let index = crate::core::graph_index::load_or_build(&root);
            let routes =
                crate::core::route_extractor::extract_routes_from_project(&root, &index.files);
            let route_candidate_count = index
                .files
                .keys()
                .filter(|p| {
                    std::path::Path::new(p.as_str())
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_some_and(|e| {
                            matches!(e, "js" | "ts" | "py" | "rs" | "java" | "rb" | "go" | "kt")
                        })
                })
                .count();
            let payload = serde_json::json!({
                "routes": routes,
                "indexed_file_count": index.files.len(),
                "route_candidate_count": route_candidate_count,
            });
            let json =
                serde_json::to_string(&payload).unwrap_or_else(|_| "{\"routes\":[]}".to_string());
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
        "/api/search" => {
            let q = extract_query_param(query_str, "q").unwrap_or_default();
            let limit: usize = extract_query_param(query_str, "limit")
                .and_then(|l| l.parse().ok())
                .unwrap_or(20);
            if q.trim().is_empty() {
                (
                    "200 OK",
                    "application/json",
                    r#"{"results":[]}"#.to_string(),
                )
            } else {
                let root_s = detect_project_root_for_dashboard();
                let root = std::path::Path::new(&root_s);
                let index = crate::core::vector_index::BM25Index::load_or_build(root);
                let hits = index.search(&q, limit);
                let results: Vec<serde_json::Value> = hits
                    .iter()
                    .map(|r| {
                        serde_json::json!({
                            "score": (r.score * 100.0).round() / 100.0,
                            "file_path": r.file_path,
                            "symbol_name": r.symbol_name,
                            "kind": r.kind,
                            "start_line": r.start_line,
                            "end_line": r.end_line,
                            "snippet": r.snippet,
                        })
                    })
                    .collect();
                let json = serde_json::json!({ "results": results }).to_string();
                ("200 OK", "application/json", json)
            }
        }
        "/api/compression-demo" => {
            let body = match extract_query_param(query_str, "path") {
                None => r#"{"error":"missing path query parameter"}"#.to_string(),
                Some(rel) => {
                    let task = extract_query_param(query_str, "task");
                    let root = detect_project_root_for_dashboard();
                    let root_pb = std::path::Path::new(&root);
                    let rel = normalize_dashboard_demo_path(&rel);
                    let candidate = std::path::Path::new(&rel);

                    let mut tried_paths: Vec<String> = Vec::new();
                    let mut full: Option<std::path::PathBuf> = None;
                    let mut content: Option<String> = None;

                    let mut attempts: Vec<std::path::PathBuf> = Vec::new();
                    if candidate.is_absolute() {
                        attempts.push(candidate.to_path_buf());
                    } else {
                        attempts.push(root_pb.join(&rel));
                        attempts.push(root_pb.join("rust").join(&rel));
                    }

                    for p in attempts {
                        tried_paths.push(p.to_string_lossy().to_string());
                        let p = if candidate.is_absolute() {
                            p
                        } else {
                            match crate::core::pathjail::jail_path(&p, root_pb) {
                                Ok(j) => j,
                                Err(_) => continue,
                            }
                        };

                        if let Ok(c) = std::fs::read_to_string(&p) {
                            full = Some(p);
                            content = Some(c);
                            break;
                        }
                    }

                    let mut resolved_from: Option<String> = None;
                    let mut candidates: Vec<String> = Vec::new();

                    if content.is_none() && !candidate.is_absolute() && !rel.trim().is_empty() {
                        // Premium path healing: try to map stale paths to current indexed files.
                        let index = crate::core::graph_index::load_or_build(&root);
                        let requested_key = crate::core::graph_index::graph_match_key(&rel);
                        let requested_name = requested_key.rsplit('/').next().unwrap_or("");

                        let mut exact: Vec<String> = Vec::new();
                        let mut suffix: Vec<String> = Vec::new();
                        let mut filename: Vec<String> = Vec::new();
                        let mut seen = std::collections::HashSet::<&str>::new();

                        for p in index.files.keys() {
                            let p_str = p.as_str();
                            if !seen.insert(p_str) {
                                continue;
                            }
                            let p_key = crate::core::graph_index::graph_match_key(p_str);
                            if p_key == requested_key {
                                exact.push(p_str.to_string());
                            } else if !requested_key.is_empty() && p_key.ends_with(&requested_key) {
                                suffix.push(p_str.to_string());
                            } else if !requested_name.is_empty()
                                && p_key
                                    .rsplit('/')
                                    .next()
                                    .is_some_and(|n| n == requested_name)
                            {
                                filename.push(p_str.to_string());
                            }
                        }

                        let mut best = if !exact.is_empty() {
                            exact
                        } else if !suffix.is_empty() {
                            suffix
                        } else {
                            filename
                        };
                        best.sort_by_key(String::len);

                        if best.len() == 1 {
                            let rel2 = best[0].clone();
                            let p2 = root_pb.join(rel2.trim_start_matches(['/', '\\']));
                            tried_paths.push(p2.to_string_lossy().to_string());
                            if let Ok(p2) = crate::core::pathjail::jail_path(&p2, root_pb) {
                                if let Ok(c2) = std::fs::read_to_string(&p2) {
                                    full = Some(p2);
                                    content = Some(c2);
                                    resolved_from = Some(rel2);
                                } else {
                                    candidates = best;
                                }
                            } else {
                                candidates = best;
                            }
                        } else if best.len() > 1 {
                            best.truncate(10);
                            candidates = best;
                        }
                    }

                    match (full, content) {
                        (Some(full), Some(content)) => {
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
                                "resolved_from": resolved_from,
                            })
                            .to_string()
                        }
                        _ => serde_json::json!({
                            "error": "failed to read file",
                            "project_root": root,
                            "requested_path": rel,
                            "candidates": candidates,
                            "tried_paths": tried_paths,
                        })
                        .to_string(),
                    }
                }
            };
            ("200 OK", "application/json", body)
        }
        "/" | "/index.html" => {
            let mut html = DASHBOARD_HTML.to_string();
            if let Some(tok) = query_token {
                let script = format!(
                    "<script>window.__LEAN_CTX_TOKEN__=\"{}\";</script>",
                    tok.replace('"', "")
                );
                html = html.replacen("<head>", &format!("<head>{script}"), 1);
            } else if let Some(t) = token {
                let script = format!(
                    "<script>window.__LEAN_CTX_TOKEN__=\"{}\";</script>",
                    t.as_str()
                );
                html = html.replacen("<head>", &format!("<head>{script}"), 1);
            }
            ("200 OK", "text/html; charset=utf-8", html)
        }
        "/api/pipeline-stats" => {
            let stats = crate::core::pipeline::PipelineStats::load();
            let json = serde_json::to_string(&stats).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/context-ledger" => {
            let ledger = crate::core::context_ledger::ContextLedger::load();
            let pressure = ledger.pressure();
            let payload = serde_json::json!({
                "window_size": ledger.window_size,
                "entries_count": ledger.entries.len(),
                "total_tokens_sent": ledger.total_tokens_sent,
                "total_tokens_saved": ledger.total_tokens_saved,
                "compression_ratio": ledger.compression_ratio(),
                "pressure": {
                    "utilization": pressure.utilization,
                    "remaining_tokens": pressure.remaining_tokens,
                    "recommendation": format!("{:?}", pressure.recommendation),
                },
                "mode_distribution": ledger.mode_distribution(),
                "entries": ledger.entries.iter().take(50).collect::<Vec<_>>(),
            });
            let json = serde_json::to_string(&payload).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
        }
        "/api/intent" => {
            let session_path = crate::core::data_dir::lean_ctx_data_dir()
                .ok()
                .map(|d| d.join("sessions"));
            let mut intent_data = serde_json::json!({"active": false});
            if let Some(dir) = session_path {
                if let Ok(entries) = std::fs::read_dir(&dir) {
                    let mut newest: Option<(std::time::SystemTime, std::path::PathBuf)> = None;
                    for e in entries.flatten() {
                        if e.path().extension().is_some_and(|ext| ext == "json") {
                            if let Ok(meta) = e.metadata() {
                                let mtime = meta.modified().unwrap_or(std::time::UNIX_EPOCH);
                                if newest.as_ref().is_none_or(|(t, _)| mtime > *t) {
                                    newest = Some((mtime, e.path()));
                                }
                            }
                        }
                    }
                    if let Some((_, path)) = newest {
                        if let Ok(content) = std::fs::read_to_string(&path) {
                            if let Ok(session) = serde_json::from_str::<serde_json::Value>(&content)
                            {
                                if let Some(intent) = session.get("active_structured_intent") {
                                    if !intent.is_null() {
                                        intent_data = serde_json::json!({
                                            "active": true,
                                            "intent": intent,
                                            "session_file": path.file_name().unwrap_or_default().to_string_lossy(),
                                        });
                                    }
                                }
                            }
                        }
                    }
                }
            }
            let json = serde_json::to_string(&intent_data).unwrap_or_else(|_| "{}".to_string());
            ("200 OK", "application/json", json)
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
        let Some((k, v)) = pair.split_once('=') else {
            continue;
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

fn normalize_dashboard_demo_path(path: &str) -> String {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return String::new();
    }

    let candidate = Path::new(trimmed);
    if candidate.is_absolute() || is_windows_absolute_path(trimmed) {
        return trimmed.to_string();
    }

    let mut p = trimmed;
    while p.starts_with("./") || p.starts_with(".\\") {
        p = &p[2..];
    }

    p.trim_start_matches(['\\', '/'])
        .replace('\\', std::path::MAIN_SEPARATOR_STR)
}

fn is_windows_absolute_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    if bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/')
    {
        return true;
    }

    path.starts_with("\\\\") || path.starts_with("//")
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
        .map(super::core::signatures::Signature::to_compact)
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
        "task": task_out.as_deref().map_or(serde_json::Value::Null, |s| compression_mode_json(s, original_tokens)),
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

fn build_symbols_json(
    index: &crate::core::graph_index::ProjectIndex,
    query: Option<&str>,
    kind: Option<&str>,
) -> String {
    let query = query
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty());
    let kind = kind
        .map(|k| k.trim().to_lowercase())
        .filter(|k| !k.is_empty());

    let mut symbols: Vec<&crate::core::graph_index::SymbolEntry> = index
        .symbols
        .values()
        .filter(|sym| {
            let kind_match = match kind.as_ref() {
                Some(k) => sym.kind.eq_ignore_ascii_case(k),
                None => true,
            };
            let query_match = match query.as_ref() {
                Some(q) => {
                    let name = sym.name.to_lowercase();
                    let file = sym.file.to_lowercase();
                    let symbol_kind = sym.kind.to_lowercase();
                    name.contains(q) || file.contains(q) || symbol_kind.contains(q)
                }
                None => true,
            };
            kind_match && query_match
        })
        .collect();

    symbols.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.start_line.cmp(&b.start_line))
            .then_with(|| a.name.cmp(&b.name))
    });
    symbols.truncate(500);

    serde_json::to_string(
        &symbols
            .into_iter()
            .map(|sym| {
                serde_json::json!({
                    "name": sym.name,
                    "kind": sym.kind,
                    "file": sym.file,
                    "start_line": sym.start_line,
                    "end_line": sym.end_line,
                    "is_exported": sym.is_exported,
                })
            })
            .collect::<Vec<_>>(),
    )
    .unwrap_or_else(|_| "[]".to_string())
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
            .unwrap_or(std::cmp::Ordering::Equal)
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
        std::fs::read_dir(&shared_dir).map_or(0, std::iter::Iterator::count)
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
    if let Ok(explicit) = std::env::var("LEAN_CTX_DASHBOARD_PROJECT") {
        if !explicit.trim().is_empty() {
            return promote_to_git_root(&explicit);
        }
    }

    if let Some(session) = crate::core::session::SessionState::load_latest() {
        // Try project_root first, but only if it resolves to a real project (has .git or markers).
        // MCP sessions often set project_root to a temp sandbox directory that contains no code.
        if let Some(root) = session.project_root.as_deref() {
            if !root.trim().is_empty() {
                if let Some(git_root) = git_root_for(root) {
                    return git_root;
                }
                if is_real_project(root) {
                    return root.to_string();
                }
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
        .map_or_else(|_| ".".to_string(), |p| p.to_string_lossy().to_string());
    let r = crate::core::protocol::detect_project_root_or_cwd(&cwd);
    promote_to_git_root(&r)
}

fn is_real_project(path: &str) -> bool {
    let p = Path::new(path);
    if !p.is_dir() {
        return false;
    }
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "requirements.txt",
        "pom.xml",
        "build.gradle",
        "CMakeLists.txt",
        ".lean-ctx.toml",
    ];
    MARKERS.iter().any(|m| p.join(m).exists())
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
    use tempfile::tempdir;

    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

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

    #[test]
    fn normalize_dashboard_demo_path_strips_rooted_relative_windows_path() {
        let normalized = normalize_dashboard_demo_path(r"\backend\list_tables.js");
        assert_eq!(
            normalized,
            format!("backend{}list_tables.js", std::path::MAIN_SEPARATOR)
        );
    }

    #[test]
    fn normalize_dashboard_demo_path_preserves_absolute_windows_path() {
        let input = r"C:\repo\backend\list_tables.js";
        assert_eq!(normalize_dashboard_demo_path(input), input);
    }

    #[test]
    fn normalize_dashboard_demo_path_preserves_unc_path() {
        let input = r"\\server\share\backend\list_tables.js";
        assert_eq!(normalize_dashboard_demo_path(input), input);
    }

    #[test]
    fn normalize_dashboard_demo_path_strips_dot_slash_prefix() {
        assert_eq!(
            normalize_dashboard_demo_path("./src/main.rs"),
            "src/main.rs"
        );
        assert_eq!(
            normalize_dashboard_demo_path(r".\src\main.rs"),
            format!("src{}main.rs", std::path::MAIN_SEPARATOR)
        );
    }

    #[test]
    fn api_profile_returns_json() {
        let (_status, _ct, body) = route_response("/api/profile", "", None, None);
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert!(v.get("active_name").is_some(), "missing active_name");
        assert!(
            v.pointer("/profile/profile/name")
                .and_then(|n| n.as_str())
                .is_some(),
            "missing profile.profile.name"
        );
        assert!(v.get("available").and_then(|a| a.as_array()).is_some());
    }

    #[test]
    fn api_episodes_returns_json() {
        let (_status, _ct, body) = route_response("/api/episodes", "", None, None);
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert!(v.get("project_hash").is_some());
        assert!(v.get("stats").is_some());
        assert!(v.get("recent").and_then(|a| a.as_array()).is_some());
    }

    #[test]
    fn api_procedures_returns_json() {
        let (_status, _ct, body) = route_response("/api/procedures", "", None, None);
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert!(v.get("project_hash").is_some());
        assert!(v.get("procedures").and_then(|a| a.as_array()).is_some());
        assert!(v.get("suggestions").and_then(|a| a.as_array()).is_some());
    }

    #[test]
    fn api_compression_demo_heals_moved_file_paths() {
        let _g = ENV_LOCK.lock().expect("env lock");
        let td = tempdir().expect("tempdir");
        let root = td.path();
        std::fs::create_dir_all(root.join("src").join("moved")).expect("mkdir");
        std::fs::write(
            root.join("src").join("moved").join("foo.rs"),
            "pub fn foo() { println!(\"hi\"); }\n",
        )
        .expect("write foo.rs");

        let root_s = root.to_string_lossy().to_string();
        std::env::set_var("LEAN_CTX_DASHBOARD_PROJECT", &root_s);

        let (_status, _ct, body) =
            route_response("/api/compression-demo", "path=src/foo.rs", None, None);
        let v: serde_json::Value = serde_json::from_str(&body).expect("valid JSON");
        assert!(v.get("error").is_none(), "unexpected error: {body}");
        assert_eq!(
            v.get("resolved_from").and_then(|x| x.as_str()),
            Some("src/moved/foo.rs")
        );

        std::env::remove_var("LEAN_CTX_DASHBOARD_PROJECT");
        if let Some(dir) = crate::core::graph_index::ProjectIndex::index_dir(&root_s) {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}
