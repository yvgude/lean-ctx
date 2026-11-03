// SPDX-License-Identifier: Apache-2.0
//! #2063: on Windows, `lean-ctx call ctx_search` with `action:"semantic"` and
//! `lean-ctx call ctx_compose` printed nothing and never returned on a
//! ~170-file project, while `semantic-search` answered in seconds. Each call
//! here runs the real binary on a project of that size, after the reporter's
//! `index build-full`, and must answer well inside the budget.
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const BUDGET: Duration = Duration::from_mins(1);

fn fixture(root: &Path) {
    for module in 0..170 {
        let dir = root.join("src").join(format!("area{}", module % 12));
        std::fs::create_dir_all(&dir).unwrap();
        let mut body = format!("//! Module {module}: slew target handling.\n\n");
        for function in 0..6 {
            body.push_str(&format!(
                "/// Checks the slew limit for axis {function} in module {module}.\n\
                 pub fn check_slew_target_{module}_{function}(ra: f64, dec: f64) -> bool {{\n    \
                 let limit = {function}.0 * 15.0 + {module}.0;\n    \
                 ra.abs() + dec.abs() < limit\n}}\n\n"
            ));
        }
        std::fs::write(dir.join(format!("module_{module}.rs")), body).unwrap();
    }
}

fn call(root: &Path, home: &Path, tool: &str, json: &str) -> (String, String, Duration) {
    // Files, not pipes: a helper process that inherits a pipe would keep a
    // reader blocked after the call itself was killed.
    let out_path = home.join(format!("{tool}.stdout"));
    let err_path = home.join(format!("{tool}.stderr"));
    let started = Instant::now();
    let mut child = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(std::env::var_os("SystemRoot").map(|v| ("SystemRoot", v)))
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LEAN_CTX_DATA_DIR", home.join("data"))
        .env("LEAN_CTX_TELEMETRY", "0")
        .env("LEAN_CTX_PHASE_TIMING", "1")
        .current_dir(root)
        .args(["call", tool, "--project-root"])
        .arg(root)
        .args(["--json", json])
        .stdin(Stdio::null())
        .stdout(std::fs::File::create(&out_path).unwrap())
        .stderr(std::fs::File::create(&err_path).unwrap())
        .spawn()
        .unwrap();
    loop {
        if child.try_wait().unwrap().is_some() {
            break;
        }
        if started.elapsed() > BUDGET {
            let _ = child.kill();
            let _ = child.wait();
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    let elapsed = started.elapsed();
    let read = |path: &Path| String::from_utf8_lossy(&std::fs::read(path).unwrap()).into_owned();
    (read(&out_path), read(&err_path), elapsed)
}

#[test]
fn oneshot_semantic_search_and_compose_answer_on_a_real_sized_project() {
    let fixture_dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(fixture_dir.path()).unwrap();
    let root = base.join("project");
    let home = base.join("home");
    std::fs::create_dir_all(&home).unwrap();
    fixture(&root);
    // The reporter's flow: build every index with the CLI first.
    let build_log = home.join("build-full.stderr");
    let built = Command::new(env!("CARGO_BIN_EXE_lean-ctx"))
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .envs(std::env::var_os("SystemRoot").map(|v| ("SystemRoot", v)))
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .env("LEAN_CTX_DATA_DIR", home.join("data"))
        .env("LEAN_CTX_TELEMETRY", "0")
        .current_dir(&root)
        .args(["index", "build-full"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(std::fs::File::create(&build_log).unwrap())
        .status()
        .unwrap();
    let mut report = format!(
        "build-full: {built}\n{}\n",
        String::from_utf8_lossy(&std::fs::read(&build_log).unwrap())
            .chars()
            .take(800)
            .collect::<String>()
    );

    let cases = [
        (
            "ctx_search",
            r#"{"action":"semantic","query":"slew target limit check","top_k":3,"mode":"bm25"}"#,
        ),
        (
            "ctx_search",
            r#"{"action":"semantic","query":"slew target limit check","top_k":3}"#,
        ),
        (
            "ctx_search",
            r#"{"action":"semantic","query":"slew target limit check","top_k":3,"mode":"hybrid"}"#,
        ),
        (
            "ctx_compose",
            r#"{"task":"check_slew_target slew limit check"}"#,
        ),
        // #2060: a repeated one-shot compose returns content, never a cache stub.
        (
            "ctx_compose",
            r#"{"task":"check_slew_target slew limit check"}"#,
        ),
    ];
    let mut failed = false;
    for (tool, json) in cases {
        let (out, err, elapsed) = call(&root, &home, tool, json);
        // Results, or an explicit error (a runner without ONNX Runtime
        // cannot embed) — never silence. Roots print without `\\?\`.
        let answered = out.contains("check_slew_target") || out.starts_with("ERR:");
        failed |= !answered
            || elapsed > Duration::from_secs(30)
            || out.contains(r"\\?\")
            || out.contains("cross-agent cache ·");
        report.push_str(&format!(
            "\n=== {tool} {json}\nelapsed {elapsed:?}, answered {answered}\n--- stdout (head)\n{}\n--- stderr\n{err}\n",
            out.chars().take(600).collect::<String>()
        ));
    }
    assert!(!failed, "{report}");
}
