#!/usr/bin/env python3
"""Test embedding model download and hybrid search end-to-end."""

import json
import os
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
    
    def recv(self, timeout=60):
        import select as sel
        fd = self.proc.stdout.fileno()
        deadline = time.time() + timeout
        buf = b""
        while time.time() < deadline:
            remaining = max(0.1, deadline - time.time())
            ready, _, _ = sel.select([fd], [], [], min(remaining, 1.0))
            if ready:
                chunk = os.read(fd, 65536)
                if not chunk:
                    return None
                buf += chunk
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
                if b"\n" in buf:
                    line, rest = buf.split(b"\n", 1)
                    if line.strip():
                        try:
                            return json.loads(line)
                        except json.JSONDecodeError:
                            buf = rest
                            continue
        return None
    
    def request(self, method, params, req_id, timeout=60):
        self.send({"jsonrpc": "2.0", "id": req_id, "method": method, "params": params})
        return self.recv(timeout=timeout)
    
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
        return self.proc.stderr.read()

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
                print(f"    Response: {json.dumps(response, ensure_ascii=False)[:500]}")
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
    
    model_dir = os.path.expanduser("~/.lean-ctx/models")
    model_exists = (
        os.path.exists(os.path.join(model_dir, "model.onnx")) and
        os.path.exists(os.path.join(model_dir, "vocab.txt"))
    )
    
    print("\n" + "=" * 60)
    print("  Embedding Model + Hybrid Search E2E Test")
    print("=" * 60)
    
    if model_exists:
        model_size = os.path.getsize(os.path.join(model_dir, "model.onnx"))
        vocab_size = os.path.getsize(os.path.join(model_dir, "vocab.txt"))
        print(f"\n  Model: {model_size / 1024 / 1024:.1f}MB")
        print(f"  Vocab: {vocab_size / 1024:.0f}KB")
    else:
        print("\n  Model not yet downloaded.")
        print("  Starting server to trigger auto-download...")
        print("  (This may take 30-60 seconds on first run)")
    
    with tempfile.TemporaryDirectory() as tmpdir:
        project_dir = os.path.join(tmpdir, "project")
        src_dir = os.path.join(project_dir, "src")
        os.makedirs(src_dir)
        
        with open(os.path.join(src_dir, "main.rs"), "w") as f:
            f.write("""fn calculate_fibonacci(n: u64) -> u64 {
    if n <= 1 { return n; }
    let mut a = 0u64;
    let mut b = 1u64;
    for _ in 2..=n { let c = a + b; a = b; b = c; }
    b
}
fn main() {
    println!("fib(10) = {}", calculate_fibonacci(10));
}
""")
        
        with open(os.path.join(src_dir, "auth.rs"), "w") as f:
            f.write("""pub struct AuthToken { pub user_id: String, pub permissions: Vec<String> }
pub fn validate_jwt_token(token: &str) -> Result<AuthToken, String> {
    if token.is_empty() { return Err("Empty token".into()); }
    Ok(AuthToken { user_id: "u1".into(), permissions: vec!["read".into()] })
}
pub fn check_permission(token: &AuthToken, required: &str) -> bool {
    token.permissions.iter().any(|p| p == required)
}
""")
        
        with open(os.path.join(src_dir, "utils.rs"), "w") as f:
            f.write("""pub fn format_duration(seconds: u64) -> String {
    format!("{:02}:{:02}:{:02}", seconds / 3600, (seconds % 3600) / 60, seconds % 60)
}
pub fn parse_csv_line(line: &str) -> Vec<String> {
    line.split(',').map(|s| s.trim().to_string()).collect()
}
""")
        
        client = McpClient(BINARY, project_dir)
        
        # Initialize
        print("\n--- Initializing server ---")
        resp = client.request("initialize", {
            "protocolVersion": "2024-11-05",
            "capabilities": {},
            "clientInfo": {"name": "embedding-test", "version": "1.0.0"}
        }, 1)
        check("Server initializes", resp, lambda r: r is not None and "result" in r)
        client.notify("notifications/initialized")
        
        if not model_exists:
            print("\n--- Waiting for model download ---")
            wait_start = time.time()
            max_wait = 120
            while time.time() - wait_start < max_wait:
                if os.path.exists(os.path.join(model_dir, "model.onnx")) and \
                   os.path.exists(os.path.join(model_dir, "vocab.txt")):
                    elapsed = time.time() - wait_start
                    print(f"  Model downloaded in {elapsed:.1f}s")
                    break
                
                tmp_file = os.path.join(model_dir, "model.onnx.tmp")
                if os.path.exists(tmp_file):
                    size = os.path.getsize(tmp_file)
                    print(f"  Downloading: {size / 1024 / 1024:.1f}MB...", end="\r")
                
                time.sleep(2)
            else:
                print(f"\n  WARNING: Model download did not complete in {max_wait}s")
        
        model_ready = (
            os.path.exists(os.path.join(model_dir, "model.onnx")) and
            os.path.exists(os.path.join(model_dir, "vocab.txt"))
        )
        check("Embedding model available", model_ready, lambda r: r)
        
        if model_ready:
            model_size = os.path.getsize(os.path.join(model_dir, "model.onnx"))
            vocab_lines = len(open(os.path.join(model_dir, "vocab.txt")).readlines())
            print(f"  Model: {model_size / 1024 / 1024:.1f}MB, Vocab: {vocab_lines} tokens")
            check("Model size > 20MB", model_size, lambda s: s > 20_000_000)
            check("Vocab has > 25K tokens", vocab_lines, lambda v: v > 25_000)
        
        # Reindex with embeddings
        print("\n--- Reindex with embedding generation ---")
        resp = client.request("tools/call", {
            "name": "ctx_index",
            "arguments": {"project_root": project_dir, "action": "build-full"}
        }, 2, timeout=60)
        text = get_text(resp)
        print(f"  Output: {text}")
        
        check("Reindex completes", resp, lambda r: r is not None)
        if model_ready:
            check("Embeddings generated during reindex", resp,
                  lambda r: "embedding" in text.lower())
        
        # Search — should be hybrid mode now
        print("\n--- Hybrid search test ---")
        resp = client.request("tools/call", {
            "name": "ctx_search",
            "arguments": {"action": "semantic", "query": "fibonacci number calculation", "path": project_dir, "top_k": 5}
        }, 3, timeout=30)
        text = get_text(resp)
        print(f"  Output: {text[:300]}")
        
        check("Search returns results", resp, lambda r: r is not None)
        if model_ready:
            check("Search uses HYBRID mode", resp,
                  lambda r: "hybrid" in text.lower())
        check("Finds fibonacci", resp,
              lambda r: "fibonacci" in text.lower() or "main.rs" in text.lower())
        
        # Cross-domain search  
        print("\n--- Cross-domain semantic search ---")
        queries = [
            ("authentication JWT verify", "auth"),
            ("parse data comma separated", "csv"),
            ("time format hours minutes", "duration"),
        ]
        for query, expected in queries:
            resp = client.request("tools/call", {
                "name": "ctx_search",
                "arguments": {"action": "semantic", "query": query, "path": project_dir, "top_k": 3}
            }, 40 + queries.index((query, expected)), timeout=15)
            text = get_text(resp)
            check(f"'{query}' → matches '{expected}'", resp,
                  lambda r, e=expected: e.lower() in get_text(r).lower())
        
        # Final metrics
        print("\n--- Embedding telemetry ---")
        resp = client.request("tools/call", {
            "name": "ctx_metrics",
            "arguments": {}
        }, 5)
        text = get_text(resp)
        
        check("Telemetry shows search data", resp,
              lambda r: "search queries" in text.lower() or "Search queries" in text)
        if model_ready:
            check("Telemetry shows embedding data", resp,
                  lambda r: "embedding" in text.lower())
        
        stderr = client.close()
        
        print(f"\n{'=' * 60}")
        total = PASS + FAIL
        if FAIL == 0:
            print(f"\033[32m  ALL {total} TESTS PASSED\033[0m")
        else:
            print(f"\033[31m  {PASS}/{total} passed, {FAIL} FAILED\033[0m")
            if stderr:
                print(f"\nServer stderr:")
                print(stderr.decode(errors="replace")[-800:])
        print(f"{'=' * 60}\n")
        sys.exit(1 if FAIL > 0 else 0)

if __name__ == "__main__":
    main()
