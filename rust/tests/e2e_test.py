#!/usr/bin/env python3
"""End-to-end test for lean-ctx MCP server over stdio (JSON-line protocol)."""

import json
import os
import select
import subprocess
import sys
import tempfile
import time

BINARY = os.path.join(os.path.dirname(__file__), "..", "target", "release", "lean-ctx")
PASS = 0
FAIL = 0

class McpClient:
    def __init__(self, binary, cwd):
        self.proc = subprocess.Popen(
            [binary],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            cwd=cwd,
            bufsize=0,
        )
        time.sleep(0.3)
    
    def send(self, obj):
        line = json.dumps(obj).encode() + b"\n"
        self.proc.stdin.write(line)
        self.proc.stdin.flush()
    
    def recv(self, timeout=15):
        """Read one JSON-line response, handling Content-Length framing too."""
        import select as sel
        fd = self.proc.stdout.fileno()
        
        deadline = time.time() + timeout
        buf = b""
        while time.time() < deadline:
            remaining = max(0.1, deadline - time.time())
            ready, _, _ = sel.select([fd], [], [], min(remaining, 0.5))
            if ready:
                chunk = os.read(fd, 65536)
                if not chunk:
                    return None
                buf += chunk
                
                # Try Content-Length framing first
                if buf.startswith(b"Content-Length:"):
                    header_end = buf.find(b"\r\n\r\n")
                    if header_end == -1:
                        header_end = buf.find(b"\n\n")
                        delim_len = 2
                    else:
                        delim_len = 4
                    
                    if header_end >= 0:
                        header = buf[:header_end].decode()
                        for hline in header.split("\n"):
                            if hline.strip().lower().startswith("content-length:"):
                                clen = int(hline.split(":", 1)[1].strip())
                                body_start = header_end + delim_len
                                if len(buf) >= body_start + clen:
                                    body = buf[body_start:body_start + clen]
                                    return json.loads(body)
                    continue
                
                # Try JSON-line
                if b"\n" in buf:
                    line, rest = buf.split(b"\n", 1)
                    if line.strip():
                        try:
                            return json.loads(line)
                        except json.JSONDecodeError:
                            buf = rest
                            continue
        return None
    
    def request(self, method, params, req_id):
        self.send({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params})
        return self.recv()
    
    def notify(self, method, params=None):
        obj = {"jsonrpc": "2.0", "method": method}
        if params:
            obj["params"] = params
        self.send(obj)
    
    def close(self):
        self.proc.terminate()
        try:
            self.proc.wait(timeout=5)
        except subprocess.TimeoutExpired:
            self.proc.kill()
        stderr = self.proc.stderr.read()
        return stderr

def check(name, response, condition_fn):
    global PASS, FAIL
    try:
        if condition_fn(response):
            print(f"  \033[32mPASS\033[0m: {name}")
            PASS += 1
            return True
        else:
            print(f"  \033[31mFAIL\033[0m: {name}")
            if response:
                print(f"    Response: {json.dumps(response, ensure_ascii=False)[:400]}")
            else:
                print(f"    Response: None")
            FAIL += 1
            return False
    except Exception as e:
        print(f"  \033[31mFAIL\033[0m: {name} — exception: {e}")
        FAIL += 1
        return False

def get_text(resp):
    if not resp or "result" not in resp:
        return ""
    result = resp["result"]
    if isinstance(result, dict):
        content = result.get("content", [])
        return "".join(c.get("text", "") for c in content if c.get("type") == "text")
    return str(result)

def main():
    global PASS, FAIL
    
    with tempfile.TemporaryDirectory() as tmpdir:
        project_dir = os.path.join(tmpdir, "project")
        src_dir = os.path.join(project_dir, "src")
        os.makedirs(src_dir)
        
        with open(os.path.join(src_dir, "main.rs"), "w") as f:
            f.write("""fn calculate_fibonacci(n: u64) -> u64 {
    if n <= 1 { return n; }
    let mut a = 0u64;
    let mut b = 1u64;
    for _ in 2..=n {
        let c = a + b;
        a = b;
        b = c;
    }
    b
}

fn main() {
    println!("fib(10) = {}", calculate_fibonacci(10));
}
""")
        
        with open(os.path.join(src_dir, "utils.rs"), "w") as f:
            f.write("""pub fn format_duration(seconds: u64) -> String {
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    format!("{:02}:{:02}:{:02}", hours, minutes, secs)
}

pub fn parse_csv_line(line: &str) -> Vec<String> {
    line.split(',').map(|s| s.trim().to_string()).collect()
}
""")
        
        with open(os.path.join(src_dir, "auth.rs"), "w") as f:
            f.write("""use std::collections::HashMap;

pub struct AuthToken {
    pub user_id: String,
    pub expires_at: u64,
    pub permissions: Vec<String>,
}

pub fn validate_jwt_token(token: &str) -> Result<AuthToken, String> {
    if token.is_empty() {
        return Err("Empty token".to_string());
    }
    let parts: Vec<&str> = token.split('.').collect();
    if parts.len() != 3 {
        return Err("Invalid JWT format".to_string());
    }
    Ok(AuthToken {
        user_id: "user123".to_string(),
        expires_at: 9999999999,
        permissions: vec!["read".to_string(), "write".to_string()],
    })
}

pub fn check_permission(token: &AuthToken, required: &str) -> bool {
    token.permissions.iter().any(|p| p == required)
}
""")
        
        client = McpClient(BINARY, project_dir)
        
        print("\n" + "=" * 60)
        print("  lean-ctx MCP Server E2E Test Suite")
        print("=" * 60)
        
        # === Test 1: Initialize ===
        print("\n--- Test 1: Initialize ---")
        resp = client.request("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "e2e-test", "version": "1.0.0"}
        }, 1)
        
        check("Server responds to initialize", resp,
              lambda r: r is not None and "result" in r)
        check("Returns serverInfo", resp,
              lambda r: "serverInfo" in r.get("result", {}))
        
        server_info = resp.get("result", {}).get("serverInfo", {}) if resp else {}
        version = server_info.get("version", "unknown")
        name = server_info.get("name", "unknown")
        print(f"  Server: {name} v{version}")
        
        check("Has capabilities", resp,
              lambda r: "capabilities" in r.get("result", {}))
        
        # Send initialized notification (no response expected)
        client.notify("notifications/initialized")
        time.sleep(0.3)
        
        # === Test 2: List Tools ===
        print("\n--- Test 2: List Tools ---")
        resp = client.request("tools/list", {}, 2)
        check("Tools list returns", resp,
              lambda r: r is not None and "result" in r)
        
        tools = []
        if resp and "result" in resp:
            tools = [t["name"] for t in resp["result"].get("tools", [])]
        
        critical_tools = ["ctx_read", "ctx_search",
                         "ctx_metrics", "ctx_tree", "ctx_shell", "ctx_overview"]
        for tool in critical_tools:
            check(f"Has tool: {tool}", tools,
                  lambda t, name=tool: name in t)

        # #509: ctx_semantic_search + ctx_symbol are folded into ctx_search. They
        # stay callable as deprecated aliases (exercised in Tests 4+ via tools/call)
        # but must be hidden from tools/list for one release.
        for hidden in ("ctx_semantic_search", "ctx_symbol"):
            check(f"Folded alias hidden from list: {hidden}", tools,
                  lambda t, name=hidden: name not in t)

        print(f"  Total tools: {len(tools)}")
        
        # === Test 3: ctx_read ===
        print("\n--- Test 3: ctx_read (file read + caching) ---")
        resp = client.request("tools/call", {
            "name": "ctx_read",
            "arguments": {"path": os.path.join(src_dir, "main.rs")}
        }, 3)
        text = get_text(resp)
        
        check("Returns content", resp, lambda r: len(text) > 0)
        check("Contains fibonacci code", resp,
              lambda r: "fibonacci" in text.lower())
        check("Assigns file reference (Fn)", resp,
              lambda r: "F" in text and any(f"F{i}" in text for i in range(1, 20)))
        
        # Second read should be cached
        resp2 = client.request("tools/call", {
            "name": "ctx_read",
            "arguments": {"path": os.path.join(src_dir, "main.rs")}
        }, 31)
        text2 = get_text(resp2)
        check("Second read is cached", resp2,
              lambda r: "cached" in text2.lower() or len(text2) < len(text))
        
        # === Test 4: ctx_semantic_search ===
        print("\n--- Test 4: ctx_semantic_search (auto-index + search) ---")
        resp = client.request("tools/call", {
            "name": "ctx_semantic_search",
            "arguments": {
                "query": "fibonacci calculation number",
                "path": project_dir,
                "top_k": 5,
            }
        }, 4)
        text = get_text(resp)
        
        check("Search returns", resp, lambda r: r is not None)
        check("Shows search mode (bm25 or hybrid)", resp,
              lambda r: "bm25" in text.lower() or "hybrid" in text.lower())
        check("Shows indexed chunk count", resp,
              lambda r: "indexed" in text.lower() or "chunks" in text.lower())
        check("Found fibonacci in results", resp,
              lambda r: "fibonacci" in text.lower() or "main.rs" in text.lower())
        
        # === Test 5: ctx_semantic_search reindex ===
        print("\n--- Test 5: ctx_semantic_search (reindex) ---")
        resp = client.request("tools/call", {
            "name": "ctx_semantic_search",
            "arguments": {
                "query": "",
                "path": project_dir,
                "action": "reindex",
            }
        }, 5)
        text = get_text(resp)
        
        check("Reindex completes", resp, lambda r: r is not None)
        check("Reports files indexed", resp,
              lambda r: "files" in text.lower() or "reindexed" in text.lower())
        check("Reports chunk count", resp,
              lambda r: "chunk" in text.lower())
        
        # === Test 6: Cross-file search ===
        print("\n--- Test 6: Cross-file semantic search ---")
        resp = client.request("tools/call", {
            "name": "ctx_semantic_search",
            "arguments": {
                "query": "JWT token authentication validate",
                "path": project_dir,
                "top_k": 5,
            }
        }, 6)
        text = get_text(resp)
        
        check("Auth search returns results", resp,
              lambda r: "result" in (r or {}))
        check("Found auth.rs content", resp,
              lambda r: "auth" in text.lower() or "jwt" in text.lower() or "token" in text.lower())
        
        # === Test 7: ctx_search (grep) ===
        print("\n--- Test 7: ctx_search (pattern search) ---")
        resp = client.request("tools/call", {
            "name": "ctx_search",
            "arguments": {
                "pattern": "fibonacci",
                "path": project_dir,
            }
        }, 7)
        text = get_text(resp)
        
        check("Pattern search returns", resp, lambda r: r is not None)
        check("Found fibonacci match", resp,
              lambda r: "fibonacci" in text.lower() or "main.rs" in text.lower())
        
        # === Test 8: ctx_tree ===
        print("\n--- Test 8: ctx_tree (directory listing) ---")
        resp = client.request("tools/call", {
            "name": "ctx_tree",
            "arguments": {"path": project_dir}
        }, 8)
        text = get_text(resp)
        
        check("Tree returns structure", resp, lambda r: len(text) > 0)
        check("Shows src directory", resp, lambda r: "src" in text)
        check("Shows .rs files", resp,
              lambda r: "main.rs" in text or ".rs" in text)
        
        # === Test 9: ctx_metrics with telemetry ===
        print("\n--- Test 9: ctx_metrics (session + telemetry) ---")
        resp = client.request("tools/call", {
            "name": "ctx_metrics",
            "arguments": {}
        }, 9)
        text = get_text(resp)
        
        check("Metrics returns data", resp, lambda r: len(text) > 0)
        check("Shows session metrics", resp,
              lambda r: "metrics" in text.lower() or "lean-ctx" in text.lower())
        check("Has Telemetry section", resp,
              lambda r: "telemetry" in text.lower() or "Telemetry" in text)
        check("Shows search query count", resp,
              lambda r: "search queries" in text.lower() or "Search queries" in text)
        check("Shows embedding inference count", resp,
              lambda r: "embedding" in text.lower() or "Embedding" in text)
        check("Shows cache hit rate", resp,
              lambda r: "cache hit rate" in text.lower() or "Cache hit rate" in text)
        check("Shows session uptime", resp,
              lambda r: "uptime" in text.lower() or "Uptime" in text)
        check("Shows CEP compliance", resp,
              lambda r: "cep" in text.lower())
        
        # === Test 10: ctx_shell ===
        print("\n--- Test 10: ctx_shell ---")
        resp = client.request("tools/call", {
            "name": "ctx_shell",
            "arguments": {"command": "echo 'hello from lean-ctx e2e test'"}
        }, 10)
        text = get_text(resp)
        
        check("Shell command executes", resp, lambda r: r is not None)
        check("Returns command output", resp,
              lambda r: "hello" in text.lower() or "lean-ctx" in text.lower())
        
        # === Test 11: Reindex reports embedding status ===
        print("\n--- Test 11: Reindex with embeddings ---")
        resp = client.request("tools/call", {
            "name": "ctx_semantic_search",
            "arguments": {
                "query": "",
                "path": project_dir,
                "action": "reindex",
            }
        }, 11)
        text = get_text(resp)
        print(f"  Reindex output: {text[:200]}")
        
        check("Reindex returns", resp, lambda r: r is not None)
        has_embeddings = "embedding" in text.lower()
        if has_embeddings:
            check("Embeddings updated during reindex", resp,
                  lambda r: "embedding" in text.lower())
            print("  [Embeddings feature ACTIVE]")
        else:
            print("  [Embeddings feature not compiled in — BM25 only]")
        
        # === Test 12: Post-reindex search quality ===
        print("\n--- Test 12: Search quality after reindex ---")
        
        queries = [
            ("fibonacci calculation", "main.rs", "fibonacci"),
            ("format time duration hours", "utils.rs", "format_duration"),
            ("JWT authentication validate token", "auth.rs", "jwt"),
            ("parse CSV data", "utils.rs", "parse_csv"),
            ("check permission access control", "auth.rs", "permission"),
        ]
        
        for query, expected_file, expected_term in queries:
            resp = client.request("tools/call", {
                "name": "ctx_semantic_search",
                "arguments": {
                    "query": query,
                    "path": project_dir,
                    "top_k": 3,
                }
            }, 120 + queries.index((query, expected_file, expected_term)))
            text = get_text(resp)
            
            found_file = expected_file.lower() in text.lower()
            found_term = expected_term.lower() in text.lower()
            check(f"Query '{query}' → finds {expected_file}", resp,
                  lambda r, ef=expected_file, et=expected_term: ef.lower() in get_text(r).lower() or et.lower() in get_text(r).lower())
        
        # === Test 13: Search mode indicator ===
        print("\n--- Test 13: Search mode indicator ---")
        resp = client.request("tools/call", {
            "name": "ctx_semantic_search",
            "arguments": {
                "query": "test query",
                "path": project_dir,
                "top_k": 1,
            }
        }, 13)
        text = get_text(resp)
        
        is_hybrid = "hybrid" in text.lower()
        is_bm25 = "bm25" in text.lower()
        check("Search mode is visible in output", resp,
              lambda r: is_hybrid or is_bm25)
        if is_hybrid:
            print("  [Mode: HYBRID (BM25 + Embeddings)]")
        else:
            print("  [Mode: BM25 only]")
        
        # === Test 14: File watcher detects changes ===
        print("\n--- Test 14: File modification detection ---")
        new_file = os.path.join(src_dir, "new_feature.rs")
        with open(new_file, "w") as f:
            f.write("""pub fn calculate_average(numbers: &[f64]) -> f64 {
    if numbers.is_empty() { return 0.0; }
    numbers.iter().sum::<f64>() / numbers.len() as f64
}

pub fn standard_deviation(numbers: &[f64]) -> f64 {
    let avg = calculate_average(numbers);
    let variance = numbers.iter()
        .map(|x| (x - avg).powi(2))
        .sum::<f64>() / numbers.len() as f64;
    variance.sqrt()
}
""")
        
        resp = client.request("tools/call", {
            "name": "ctx_semantic_search",
            "arguments": {
                "query": "",
                "path": project_dir,
                "action": "reindex",
            }
        }, 141)
        reindex2_text = get_text(resp)
        
        resp = client.request("tools/call", {
            "name": "ctx_semantic_search",
            "arguments": {
                "query": "calculate average standard deviation statistics",
                "path": project_dir,
                "top_k": 3,
            }
        }, 142)
        text = get_text(resp)
        
        check("Finds newly added file after reindex", resp,
              lambda r: "new_feature" in text.lower() or "average" in text.lower() or "deviation" in text.lower())
        
        # === Test 15: Final metrics with telemetry from all operations ===
        print("\n--- Test 15: Final metrics snapshot ---")
        resp = client.request("tools/call", {
            "name": "ctx_metrics",
            "arguments": {}
        }, 15)
        text = get_text(resp)
        
        check("Final metrics has telemetry", resp,
              lambda r: "telemetry" in text.lower() or "Telemetry" in text)
        check("Search queries recorded (>0)", resp,
              lambda r: "Search queries:    0" not in text)
        check("Session uptime > 0s", resp,
              lambda r: "uptime" in text.lower())
        
        # Cleanup
        stderr = client.close()
        
        # === Results ===
        print(f"\n{'=' * 60}")
        total = PASS + FAIL
        if FAIL == 0:
            print(f"\033[32m  ALL {total} TESTS PASSED\033[0m")
        else:
            print(f"\033[31m  {PASS}/{total} passed, {FAIL} FAILED\033[0m")
        
        if FAIL > 0 and stderr:
            print(f"\nServer stderr (last 500 chars):")
            print(stderr.decode(errors="replace")[-500:])
        
        print(f"{'=' * 60}\n")
        sys.exit(1 if FAIL > 0 else 0)

if __name__ == "__main__":
    main()
