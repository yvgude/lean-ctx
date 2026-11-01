//! Shell-output compression patterns.
//!
//! The patterns themselves live in the `lean-ctx-patterns` crate and are
//! re-exported here unchanged. This facade adds what needs the engine: the
//! protected-command policy gate, the leading-`cd` unwrap (shell segmenting),
//! the user filter rules and the configured curl passthrough URLs.

pub use lean_ctx_patterns::*;

/// Compress `output` of `command` with its pattern, or `None` when no pattern
/// applies or none makes it shorter. Protected commands are never compressed.
pub fn compress_output(command: &str, output: &str) -> Option<String> {
    // Policy gate: protected commands (Passthrough + Verbatim) must
    // never be pattern-compressed. The caller (compress_if_beneficial
    // or ctx_shell::handle) should have already checked, but we
    // defend in depth here.
    let policy = crate::shell::output_policy::classify(command, &[]);
    if policy.is_protected() {
        return None;
    }
    let command = strip_leading_cd(command);

    let stripped;
    let clean_output = {
        let s = crate::core::compressor::strip_ansi(output);
        if s.len() < output.len() {
            stripped = s;
            &stripped
        } else {
            output
        }
    };

    if let Some(engine) = crate::core::filters::FilterEngine::load()
        && let Some(filtered) = engine.apply(command, clean_output)
    {
        return shorter_only(filtered, output);
    }

    compress_clean(&EngineEnv, command, clean_output, output)
}

/// Route `cmd` to its dedicated compressor, with the engine's configuration.
pub fn try_specific_pattern(cmd: &str, output: &str) -> Option<String> {
    try_specific_pattern_with(&EngineEnv, cmd, output)
}

/// `cd <dir> && <cmd>` produces exactly `<cmd>`'s output, so it must hit the
/// same pattern as `<cmd>` (and as `git -C <dir> …`) instead of falling through
/// to the generic truncation (#1893). Only that exact two-segment shape is
/// unwrapped; anything longer mixes outputs and is left alone.
pub(crate) fn strip_leading_cd(command: &str) -> &str {
    use crate::core::shell_allowlist::Separator;
    let segments = crate::core::shell_allowlist::segments_with_separators(command);
    let [(first, Some(Separator::And)), (rest, None)] = segments.as_slice() else {
        return command;
    };
    let is_plain_cd = first
        .strip_prefix("cd ")
        .is_some_and(|dir| !dir.trim().is_empty() && !dir.contains(['$', '`', '(']));
    if !is_plain_cd {
        return command;
    }
    command
        .rfind(rest.as_str())
        .map_or(command, |at| &command[at..])
}

/// The engine's [`PatternEnv`]: configuration is loaded only when a pattern
/// asks, so commands without such a question pay nothing.
struct EngineEnv;

impl PatternEnv for EngineEnv {
    fn is_curl_passthrough(&self, command: &str) -> bool {
        let cfg = crate::core::config::Config::load();
        curl::is_passthrough(command, &cfg.passthrough_urls)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::tokens::count_tokens;

    #[test]
    fn routes_git_commands() {
        let output = "On branch main\nnothing to commit";
        assert!(compress_output("git status", output).is_some());
    }

    #[test]
    fn vcs_path_never_inflates_tokens() {
        // Regression for the `compress_output_never_inflates_tokens` property
        // (Windows CI): the VCS branch returned the git compressor's reshaped
        // output without the `shorter_only` guard the other paths use, so this
        // tiny adversarial `git status` body grew 10 -> 11 tokens. The result
        // must now never tokenize larger than its input (None == use original).
        let output = " Abu a\nAa aa_A00A\n\n-";
        if let Some(compressed) = compress_output("git status", output) {
            assert!(
                count_tokens(&compressed) <= count_tokens(output),
                "VCS compress inflated: {} > {}",
                count_tokens(&compressed),
                count_tokens(output),
            );
        }
    }

    #[test]
    fn routes_cargo_commands() {
        let output = "   Compiling lean-ctx v2.1.1\n    Finished `release` profile [optimized] target(s) in 30.5s";
        assert!(compress_output("cargo build --release", output).is_some());
    }

    #[test]
    fn routes_npm_commands() {
        let output = "added 150 packages, and audited 151 packages in 5s\n\n25 packages are looking for funding\n  run `npm fund` for details\n\nfound 0 vulnerabilities";
        assert!(compress_output("npm install", output).is_some());
    }

    #[test]
    fn routes_docker_commands() {
        let output = "CONTAINER ID   IMAGE     COMMAND   CREATED   STATUS    PORTS     NAMES";
        // docker ps is Verbatim (via is_container_listing), so compress_output
        // correctly returns None (policy gate). docker build should still compress.
        assert!(compress_output("docker ps", output).is_none());
        let build_output =
            "Step 1/5 : FROM node:18\n ---> abc123\nStep 2/5 : COPY . .\nSuccessfully built def456";
        assert!(compress_output("docker build .", build_output).is_some());
    }

    #[test]
    fn routes_mypy_commands() {
        let output = "src/main.py:10: error: Missing return  [return]\nFound 1 error in 1 file (checked 3 source files)";
        assert!(compress_output("mypy .", output).is_some());
        assert!(compress_output("python -m mypy src/", output).is_some());
    }

    #[test]
    fn routes_pytest_commands() {
        let output = "===== test session starts =====\ncollected 5 items\ntest_main.py ..... [100%]\n===== 5 passed in 0.5s =====";
        assert!(compress_output("pytest", output).is_some());
        assert!(compress_output("python -m pytest tests/", output).is_some());
    }

    #[test]
    fn routes_data_domain() {
        let dbt =
            "20:14:02  Found 12 models\n20:14:20  Done. PASS=11 WARN=0 ERROR=1 SKIP=0 TOTAL=12";
        assert!(
            compress_output("dbt run", dbt).is_some(),
            "dbt routed+compressible"
        );
        let alembic = "INFO  [alembic.runtime.migration] Context impl PostgresqlImpl.\nINFO  [alembic.runtime.migration] Running upgrade  -> a1b2c3, create users table\nINFO  [alembic.runtime.migration] Running upgrade a1b2c3 -> d4e5f6, add email index";
        assert!(compress_output("alembic upgrade head", alembic).is_some());
        let flyway = "Flyway Community Edition 9.22.0 by Redgate\nDatabase: jdbc:postgresql://localhost/db\nMigrating schema \"public\" to version \"5 - add orders\"\nSuccessfully applied 1 migration to schema \"public\", now at version v5";
        assert!(compress_output("flyway migrate", flyway).is_some());
        let spark = "23/01/01 12:00:00 INFO SparkContext: Running Spark version 3.4.0\n23/01/01 12:00:01 INFO ResourceUtils: none configured\n23/01/01 12:00:10 INFO DAGScheduler: Job 0 finished: collect, took 5.1 s\n23/01/01 12:00:15 ERROR Executor: Exception in task 0.0";
        assert!(compress_output("spark-submit app.py", spark).is_some());
    }

    #[test]
    fn routes_ai_domain() {
        let ollama = "NAME              ID              SIZE      MODIFIED\nllama3.2:latest   a80c4f17acd5    2.0 GB    3 days ago\nqwen2.5-coder:7b  2b0496514337    4.7 GB    2 weeks ago";
        assert!(compress_output("ollama list", ollama).is_some());
        let mlflow = "2024/01/01 12:00:01 INFO mlflow.projects.backend.local: === Running command 'python train.py' ===\nCollecting numpy==1.26.0\nDownloading numpy-1.26.0.whl (18.2 MB)\n2024/01/01 12:00:30 INFO mlflow.projects: === Run (ID 'abc123def456') succeeded ===";
        assert!(compress_output("mlflow run .", mlflow).is_some());
    }

    #[test]
    fn routes_security_domain() {
        let trivy = "2024-01-01T12:00:00.000Z\tINFO\tscanning\nnginx:latest (debian 12.1)\n=====\nTotal: 45 (LOW: 20, HIGH: 8, CRITICAL: 2)";
        assert!(compress_output("trivy image nginx", trivy).is_some());
        let grype = "NAME       INSTALLED  FIXED-IN  TYPE  VULNERABILITY   SEVERITY\nlibssl1.1  1.1.1n     1.1.1w    deb   CVE-2023-1234   Critical\nzlib1g     1.2.11     1.2.13    deb   CVE-2022-5678   High";
        assert!(compress_output("grype nginx", grype).is_some());
        let syft = "NAME       VERSION    TYPE\nadduser    3.118      deb\napt        2.6.1      deb\nlodash     4.17.21    npm";
        assert!(compress_output("syft nginx", syft).is_some());
        let semgrep = "Scanning 120 files.\n  src/app.py\n     python.security.dangerous-subprocess-use\n        Detected subprocess.\n         42┆ subprocess.call(x)\nRan 450 rules on 120 files: 1 findings.";
        assert!(compress_output("semgrep scan", semgrep).is_some());
        let swiftlint = "Linting Swift files in current working directory\nLinting 'A.swift' (1/3)\nLinting 'B.swift' (2/3)\nLinting 'C.swift' (3/3)\n/path/A.swift:10:5: warning: Line Length Violation: Line should be 120 chars or less (line_length)\n/path/A.swift:22:1: warning: Trailing Whitespace Violation: no trailing whitespace (trailing_whitespace)\n/path/B.swift:5:1: error: Force Cast Violation: avoid force casts (force_cast)\n/path/C.swift:8:3: warning: Todo Violation: resolve TODOs (todo)\nDone linting! Found 4 violations, 1 serious in 3 files.";
        assert!(compress_output("swiftlint", swiftlint).is_some());
    }

    #[test]
    fn routes_vcs_toolchain_domain() {
        let jj = "@  qpvuntsm user@host.com 2024-01-01 12:00:00 1234abcd\n│  add feature x\n○  zzzzmmmm user@host.com 2024-01-01 11:00:00 main 5678efab\n│  initial commit\n~";
        assert!(compress_output("jj log", jj).is_some());
        let mise = "node    20.10.0  ~/.config/mise/config.toml\npython  3.12.0   ~/.tool-versions\nrust    1.75.0   ~/.config/mise/config.toml";
        assert!(compress_output("mise ls", mise).is_some());
        let buf_lines: Vec<String> = (0..30)
            .map(|i| format!("proto/f{i}.proto:{i}:1:Field name should be lower_snake_case here."))
            .collect();
        let buf = buf_lines.join("\n");
        assert!(compress_output("buf lint", &buf).is_some());
        let gem = "Fetching rails-7.1.0.gem\nSuccessfully installed activesupport-7.1.0\nSuccessfully installed rails-7.1.0\nParsing documentation for rails-7.1.0\nInstalling ri documentation for rails-7.1.0\nDone installing documentation for rails after 3 seconds\n2 gems installed";
        assert!(compress_output("gem install rails", gem).is_some());
        let uv = "Resolved 42 packages in 120ms\nDownloading numpy (18.2MiB)\n 100%|████████| 18.2M/18.2M [00:01<00:00, 15.3MiB/s]\nDownloading pandas (12.1MiB)\n 100%|████████| 12.1M/12.1M [00:00<00:00, 14.1MiB/s]\nPrepared 5 packages in 1.2s\nInstalled 5 packages in 30ms\n + numpy==1.26.0\n + pandas==2.1.0";
        assert!(compress_output("uv add pandas", uv).is_some());
    }

    #[test]
    fn routes_edge_infra_domain() {
        let pulumi = "Updating (dev):\n     Type   Name   Status\n +   pulumi:pulumi:Stack proj created\n +   aws:s3:Bucket b1 created\n +   aws:s3:Bucket b2 created\n +   aws:s3:Bucket b3 created\n +   aws:lambda:Function fn created\n\nOutputs:\n    url: \"https://x.example.com\"\n\nResources:\n    + 5 created\n    10 unchanged\n\nDuration: 35s";
        assert!(compress_output("pulumi up", pulumi).is_some());
        let linkerd = "kubernetes-api\n--------------\n√ can initialize the client\n√ can query the Kubernetes API\n√ is running the minimum kubectl version\n\nlinkerd-existence\n-----------------\n√ 'linkerd-config' config map exists\n× control plane pods are ready\n    some pods are not ready\n\nStatus check results are ×";
        assert!(compress_output("linkerd check", linkerd).is_some());
        let argocd = "Name:               argocd/myapp\nProject:            default\nSync Status:        Synced\nHealth Status:      Healthy\n\nGROUP  KIND  NAMESPACE  NAME  STATUS  HEALTH  HOOK  MESSAGE\n  Service ns s1 Synced Healthy\n  Service ns s2 Synced Healthy\n  Service ns s3 Synced Healthy\napps Deployment ns d1 OutOfSync Progressing";
        assert!(compress_output("argocd app get myapp", argocd).is_some());
        let vercel = "Vercel CLI 33.0.0\nInstalling dependencies...\nadded 420 packages in 12s\nBuilding...\nCompiling pages\nCollecting page data\nGenerating static pages\nProduction: https://my-app.vercel.app [45s]";
        assert!(compress_output("vercel deploy --prod", vercel).is_some());
        let wrangler = "wrangler 3.0.0\n-------------------\nyour worker has access to the following bindings:\n- KV Namespaces:\n  - CACHE: abc123\nTotal Upload: 1.2 MiB / gzip: 0.4 MiB\nUploaded my-worker (3.5 sec)\nPublished my-worker (1.2 sec)\n  https://my-worker.example.workers.dev";
        assert!(compress_output("wrangler deploy", wrangler).is_some());
    }

    #[test]
    fn unknown_command_returns_none() {
        assert!(compress_output("some-unknown-tool --version", "v1.0").is_none());
    }

    #[test]
    fn case_insensitive_routing() {
        let output = "On branch main\nnothing to commit";
        assert!(compress_output("Git Status", output).is_some());
        assert!(compress_output("GIT STATUS", output).is_some());
    }

    #[test]
    fn routes_vp_and_vite_plus() {
        let output = "  VITE v5.0.0  ready in 200 ms\n\n  -> Local:   http://localhost:5173/\n  -> Network: http://192.168.1.2:5173/";
        assert!(compress_output("vp build", output).is_some());
        assert!(compress_output("vite-plus build", output).is_some());
    }

    #[test]
    fn routes_bunx_commands() {
        let output = "1 pass tests\n0 fail tests\n3 skip tests\nDone 12ms\nsome extra line\nmore output here";
        let result = compress_output("bunx test", output);
        assert!(
            result.is_some(),
            "bunx should compress when output is large enough"
        );
        assert!(result.unwrap().contains("bun test: 1 passed"));
    }

    #[test]
    fn routes_deno_task() {
        let output = "Task dev deno run --allow-net server.ts\nListening on http://localhost:8000";
        assert!(try_specific_pattern("deno task dev", output).is_some());
    }

    #[test]
    fn routes_jest_commands() {
        let output = "PASS  tests/main.test.js\nTest Suites: 1 passed, 1 total\nTests:       5 passed, 5 total\nTime:        2.5 s";
        assert!(try_specific_pattern("jest", output).is_some());
        assert!(try_specific_pattern("npx jest --coverage", output).is_some());
    }

    #[test]
    fn routes_mocha_commands() {
        let output = "  3 passing (50ms)\n  1 failing\n\n  1) Array #indexOf():\n     Error: expected -1 to equal 0";
        assert!(try_specific_pattern("mocha", output).is_some());
        assert!(try_specific_pattern("npx mocha tests/", output).is_some());
    }

    #[test]
    fn routes_tofu_commands() {
        let output = "Initializing the backend...\nInitializing provider plugins...\nTerraform has been successfully initialized!";
        assert!(try_specific_pattern("tofu init", output).is_some());
    }

    #[test]
    fn routes_ps_commands() {
        let mut lines = vec!["USER PID %CPU %MEM VSZ RSS TTY STAT START TIME COMMAND".to_string()];
        for i in 0..20 {
            lines.push(format!("user {i} 0.0 0.1 1234 123 ? S 10:00 0:00 proc_{i}"));
        }
        let output = lines.join("\n");
        assert!(try_specific_pattern("ps aux", &output).is_some());
    }

    #[test]
    fn routes_ping_commands() {
        let output = "PING google.com (1.2.3.4): 56 data bytes\n64 bytes from 1.2.3.4: icmp_seq=0 ttl=116 time=12ms\n3 packets transmitted, 3 packets received, 0.0% packet loss\nrtt min/avg/max/stddev = 11/12/13/1 ms";
        assert!(try_specific_pattern("ping -c 3 google.com", output).is_some());
    }

    #[test]
    fn routes_jq_to_json_schema() {
        let output = "{\"name\": \"test\", \"version\": \"1.0\", \"items\": [{\"id\": 1}, {\"id\": 2}, {\"id\": 3}, {\"id\": 4}, {\"id\": 5}, {\"id\": 6}, {\"id\": 7}, {\"id\": 8}, {\"id\": 9}, {\"id\": 10}]}";
        assert!(try_specific_pattern("jq '.items' data.json", output).is_some());
    }

    #[test]
    fn routes_linting_tools() {
        let lint_output = "src/main.py:10: error: Missing return\nsrc/main.py:20: error: Unused var\nFound 2 errors";
        assert!(try_specific_pattern("hadolint Dockerfile", lint_output).is_some());
        assert!(try_specific_pattern("oxlint src/", lint_output).is_some());
        assert!(try_specific_pattern("pyright src/", lint_output).is_some());
        assert!(try_specific_pattern("basedpyright src/", lint_output).is_some());
    }

    #[test]
    fn routes_fd_commands() {
        let output = "src/main.rs\nsrc/lib.rs\nsrc/util/helpers.rs\nsrc/util/math.rs\ntests/integration.rs\n";
        assert!(try_specific_pattern("fd --extension rs", output).is_some());
        assert!(try_specific_pattern("fdfind .rs", output).is_some());
    }

    #[test]
    fn routes_just_commands() {
        let output = "Available recipes:\n    build\n    test\n    lint\n";
        assert!(try_specific_pattern("just --list", output).is_some());
        assert!(try_specific_pattern("just build", output).is_some());
    }

    #[test]
    fn routes_ninja_commands() {
        let output = "[1/10] Compiling foo.c\n[10/10] Linking app\n";
        assert!(try_specific_pattern("ninja", output).is_some());
        assert!(try_specific_pattern("ninja -j4", output).is_some());
    }

    #[test]
    fn routes_clang_commands() {
        let output =
            "src/main.c:10:5: error: use of undeclared identifier 'foo'\n1 error generated.\n";
        assert!(try_specific_pattern("clang src/main.c", output).is_some());
        assert!(try_specific_pattern("clang++ -std=c++17 main.cpp", output).is_some());
    }

    #[test]
    fn routes_cargo_run() {
        let output = "   Compiling foo v0.1.0\n    Finished `dev` profile\nHello, world!";
        assert!(try_specific_pattern("cargo run", output).is_some());
    }

    #[test]
    fn routes_cargo_bench() {
        let output = "   Compiling foo v0.1.0\ntest bench_parse ... bench: 1234 ns/iter";
        assert!(try_specific_pattern("cargo bench", output).is_some());
    }

    #[test]
    fn routes_build_tools() {
        let build_output = "   Compiling foo v0.1.0\n    Finished release [optimized]";
        assert!(try_specific_pattern("gcc -o main main.c", build_output).is_some());
        assert!(try_specific_pattern("g++ -o main main.cpp", build_output).is_some());
    }

    #[test]
    fn routes_monorepo_tools() {
        let output = "npm warn deprecated inflight@1.0.6\nnpm warn deprecated rimraf@3.0.2\nadded 150 packages, and audited 151 packages in 5s\n\n25 packages are looking for funding\n  run `npm fund` for details\n\nfound 0 vulnerabilities";
        assert!(try_specific_pattern("turbo install", output).is_some());
        assert!(try_specific_pattern("nx install", output).is_some());
    }

    #[test]
    fn gh_api_passthrough_never_compresses() {
        let huge = "line\n".repeat(5000);
        assert!(
            compress_output("gh api repos/owner/repo/actions/jobs/123/logs", &huge).is_none(),
            "gh api must never be compressed, even for large output"
        );
        assert!(compress_output("gh api repos/owner/repo/actions/runs/123/logs", &huge).is_none());
    }

    #[test]
    fn gh_log_flags_passthrough() {
        let huge = "line\n".repeat(5000);
        assert!(compress_output("gh run view 123 --log-failed", &huge).is_none());
        assert!(compress_output("gh run view 123 --log", &huge).is_none());
    }

    #[test]
    fn strip_leading_cd_unwraps_only_plain_two_segment_cd() {
        assert_eq!(strip_leading_cd("cd /x && git log -5"), "git log -5");
        assert_eq!(strip_leading_cd("cd 'a b' && git status"), "git status");
        assert_eq!(strip_leading_cd("git log -5"), "git log -5");
        for kept in [
            "cd /x && git log && git status",
            "cd /x; git log",
            "cd /x || git log",
            "cd $HOME && git log",
            "cd $(pwd) && git log",
            "pushd /x && git log",
        ] {
            assert_eq!(strip_leading_cd(kept), kept, "{kept}");
        }
    }

    #[test]
    fn cd_prefixed_git_log_uses_git_pattern() {
        let mut out = String::new();
        for i in 0..30 {
            out.push_str(&format!(
                "commit {i:040x}\nAuthor: A <a@b.c>\nDate:   Mon Jan 1 00:00:00 2026 +0000\n\n    subject {i}\n\n    - bullet body line\n\n src/f{i}.rs | 4 ++--\n 1 file changed, 2 insertions(+), 2 deletions(-)\n\n"
            ));
        }
        let plain = compress_output("git -C /x log --stat -30", &out);
        let cd = compress_output("cd /x && git log --stat -30", &out);
        assert!(plain.is_some());
        assert_eq!(cd, plain);
        assert!(cd.unwrap().contains("+60/-60 total"));
    }

    #[test]
    fn gh_structured_commands_still_compress() {
        let output = "On branch main\nnothing to commit";
        assert!(try_specific_pattern("gh pr list", output).is_some());
        assert!(try_specific_pattern("gh run list", output).is_some());
    }
}
