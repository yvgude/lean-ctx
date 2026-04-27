use crate::core::patterns;
use crate::core::tokens::count_tokens;

const BUILTIN_PASSTHROUGH: &[&str] = &[
    // JS/TS dev servers & watchers
    "turbo",
    "nx serve",
    "nx dev",
    "next dev",
    "vite dev",
    "vite preview",
    "vitest",
    "nuxt dev",
    "astro dev",
    "webpack serve",
    "webpack-dev-server",
    "nodemon",
    "concurrently",
    "pm2",
    "pm2 logs",
    "gatsby develop",
    "expo start",
    "react-scripts start",
    "ng serve",
    "remix dev",
    "wrangler dev",
    "hugo server",
    "hugo serve",
    "jekyll serve",
    "bun dev",
    "ember serve",
    // Package manager script runners (wrap dev servers via package.json)
    "npm run dev",
    "npm run start",
    "npm run serve",
    "npm run watch",
    "npm run preview",
    "npm run storybook",
    "npm run test:watch",
    "npm start",
    "npx ",
    "pnpm run dev",
    "pnpm run start",
    "pnpm run serve",
    "pnpm run watch",
    "pnpm run preview",
    "pnpm run storybook",
    "pnpm dev",
    "pnpm start",
    "pnpm preview",
    "yarn dev",
    "yarn start",
    "yarn serve",
    "yarn watch",
    "yarn preview",
    "yarn storybook",
    "bun run dev",
    "bun run start",
    "bun run serve",
    "bun run watch",
    "bun run preview",
    "bun start",
    "deno task dev",
    "deno task start",
    "deno task serve",
    "deno run --watch",
    // Docker
    "docker compose up",
    "docker-compose up",
    "docker compose logs",
    "docker-compose logs",
    "docker compose exec",
    "docker-compose exec",
    "docker compose run",
    "docker-compose run",
    "docker compose watch",
    "docker-compose watch",
    "docker logs",
    "docker attach",
    "docker exec -it",
    "docker exec -ti",
    "docker run -it",
    "docker run -ti",
    "docker stats",
    "docker events",
    // Kubernetes
    "kubectl logs",
    "kubectl exec -it",
    "kubectl exec -ti",
    "kubectl attach",
    "kubectl port-forward",
    "kubectl proxy",
    // System monitors & streaming
    "top",
    "htop",
    "btop",
    "watch ",
    "tail -f",
    "tail -f ",
    "journalctl -f",
    "journalctl --follow",
    "dmesg -w",
    "dmesg --follow",
    "strace",
    "tcpdump",
    "ping ",
    "ping6 ",
    "traceroute",
    "mtr ",
    "nmap ",
    "iperf ",
    "iperf3 ",
    "ss -l",
    "netstat -l",
    "lsof -i",
    "socat ",
    // Editors & pagers
    "less",
    "more",
    "vim",
    "nvim",
    "vi ",
    "nano",
    "micro ",
    "helix ",
    "hx ",
    "emacs",
    // Terminal multiplexers
    "tmux",
    "screen",
    // Interactive shells & REPLs
    "ssh ",
    "telnet ",
    "nc ",
    "ncat ",
    "psql",
    "mysql",
    "sqlite3",
    "redis-cli",
    "mongosh",
    "mongo ",
    "python3 -i",
    "python -i",
    "irb",
    "rails console",
    "rails c ",
    "iex",
    // Python servers, workers, watchers
    "flask run",
    "uvicorn ",
    "gunicorn ",
    "hypercorn ",
    "daphne ",
    "django-admin runserver",
    "manage.py runserver",
    "python manage.py runserver",
    "python -m http.server",
    "python3 -m http.server",
    "streamlit run",
    "gradio ",
    "celery worker",
    "celery -a",
    "celery -b",
    "dramatiq ",
    "rq worker",
    "watchmedo ",
    "ptw ",
    "pytest-watch",
    // Ruby / Rails
    "rails server",
    "rails s",
    "puma ",
    "unicorn ",
    "thin start",
    "foreman start",
    "overmind start",
    "guard ",
    "sidekiq",
    "resque ",
    // PHP / Laravel
    "php artisan serve",
    "php -s ",
    "php artisan queue:work",
    "php artisan queue:listen",
    "php artisan horizon",
    "php artisan tinker",
    "sail up",
    // Java / JVM
    "./gradlew bootrun",
    "gradlew bootrun",
    "gradle bootrun",
    "./gradlew run",
    "mvn spring-boot:run",
    "./mvnw spring-boot:run",
    "mvnw spring-boot:run",
    "mvn quarkus:dev",
    "./mvnw quarkus:dev",
    "sbt run",
    "sbt ~compile",
    "lein run",
    "lein repl",
    // Go
    "go run ",
    "air ",
    "gin ",
    "realize start",
    "reflex ",
    "gowatch ",
    // .NET / C#
    "dotnet run",
    "dotnet watch",
    "dotnet ef",
    // Elixir / Erlang
    "mix phx.server",
    "iex -s mix",
    // Swift
    "swift run",
    "swift package ",
    "vapor serve",
    // Zig
    "zig build run",
    // Rust
    "cargo watch",
    "cargo run",
    "cargo leptos watch",
    "bacon ",
    // General watchers & task runners
    "make dev",
    "make serve",
    "make watch",
    "make run",
    "make start",
    "just dev",
    "just serve",
    "just watch",
    "just start",
    "just run",
    "task dev",
    "task serve",
    "task watch",
    "nix develop",
    "devenv up",
    // CI/CD & infrastructure (long-running)
    "act ",
    "skaffold dev",
    "tilt up",
    "garden dev",
    "telepresence ",
    // Load testing & benchmarking
    "ab ",
    "wrk ",
    "hey ",
    "vegeta ",
    "k6 run",
    "artillery run",
    // Authentication flows (device code, OAuth, SSO)
    "az login",
    "az account",
    "gh",
    "gcloud auth",
    "gcloud init",
    "aws sso",
    "aws configure sso",
    "firebase login",
    "netlify login",
    "vercel login",
    "heroku login",
    "flyctl auth",
    "fly auth",
    "railway login",
    "supabase login",
    "wrangler login",
    "doppler login",
    "vault login",
    "oc login",
    "kubelogin",
    "--use-device-code",
];

const SCRIPT_RUNNER_PREFIXES: &[&str] = &[
    "npm run ",
    "npm start",
    "npx ",
    "pnpm run ",
    "pnpm dev",
    "pnpm start",
    "pnpm preview",
    "yarn ",
    "bun run ",
    "bun start",
    "deno task ",
];

const DEV_SCRIPT_KEYWORDS: &[&str] = &[
    "dev",
    "start",
    "serve",
    "watch",
    "preview",
    "storybook",
    "hot",
    "live",
    "hmr",
];

fn is_dev_script_runner(cmd: &str) -> bool {
    for prefix in SCRIPT_RUNNER_PREFIXES {
        if let Some(rest) = cmd.strip_prefix(prefix) {
            let script_name = rest.split_whitespace().next().unwrap_or("");
            for kw in DEV_SCRIPT_KEYWORDS {
                if script_name.contains(kw) {
                    return true;
                }
            }
        }
    }
    false
}

pub(super) fn is_excluded_command(command: &str, excluded: &[String]) -> bool {
    let cmd = command.trim().to_lowercase();
    for pattern in BUILTIN_PASSTHROUGH {
        if pattern.starts_with("--") {
            if cmd.contains(pattern) {
                return true;
            }
        } else if pattern.ends_with(' ') || pattern.ends_with('\t') {
            if cmd == pattern.trim() || cmd.starts_with(pattern) {
                return true;
            }
        } else if cmd == *pattern
            || cmd.starts_with(&format!("{pattern} "))
            || cmd.starts_with(&format!("{pattern}\t"))
            || cmd.contains(&format!(" {pattern} "))
            || cmd.contains(&format!(" {pattern}\t"))
            || cmd.contains(&format!("|{pattern} "))
            || cmd.contains(&format!("|{pattern}\t"))
            || cmd.ends_with(&format!(" {pattern}"))
            || cmd.ends_with(&format!("|{pattern}"))
        {
            return true;
        }
    }

    if is_dev_script_runner(&cmd) {
        return true;
    }

    if excluded.is_empty() {
        return false;
    }
    excluded.iter().any(|excl| {
        let excl_lower = excl.trim().to_lowercase();
        cmd == excl_lower || cmd.starts_with(&format!("{excl_lower} "))
    })
}

pub(super) fn compress_and_measure(command: &str, stdout: &str, stderr: &str) -> (String, usize) {
    let compressed_stdout = compress_if_beneficial(command, stdout);
    let compressed_stderr = compress_if_beneficial(command, stderr);

    let mut result = String::new();
    if !compressed_stdout.is_empty() {
        result.push_str(&compressed_stdout);
    }
    if !compressed_stderr.is_empty() {
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str(&compressed_stderr);
    }

    let content_for_counting = if let Some(pos) = result.rfind("\n[lean-ctx: ") {
        &result[..pos]
    } else {
        &result
    };
    let output_tokens = count_tokens(content_for_counting);
    (result, output_tokens)
}

fn compress_if_beneficial(command: &str, output: &str) -> String {
    if output.trim().is_empty() {
        return String::new();
    }

    if crate::tools::ctx_shell::contains_auth_flow(output) {
        return output.to_string();
    }

    let original_tokens = count_tokens(output);

    if original_tokens < 50 {
        return output.to_string();
    }

    let min_output_tokens = 5;

    if let Some(compressed) = patterns::compress_output(command, output) {
        if !compressed.trim().is_empty() {
            let compressed_tokens = count_tokens(&compressed);
            if compressed_tokens >= min_output_tokens && compressed_tokens < original_tokens {
                let ratio = compressed_tokens as f64 / original_tokens as f64;
                if ratio < 0.05 && original_tokens > 100 {
                    tracing::warn!("compression removed >95% of content, returning original");
                    return output.to_string();
                }
                let saved = original_tokens - compressed_tokens;
                let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
                if pct >= 5 {
                    return format!(
                        "{compressed}\n[lean-ctx: {original_tokens}→{compressed_tokens} tok, -{pct}%]"
                    );
                }
                return compressed;
            }
            if compressed_tokens < min_output_tokens {
                return output.to_string();
            }
        }
    }

    let cleaned = crate::core::compressor::lightweight_cleanup(output);
    let cleaned_tokens = count_tokens(&cleaned);
    if cleaned_tokens < original_tokens {
        let lines: Vec<&str> = cleaned.lines().collect();
        if lines.len() > 30 {
            let compressed = truncate_with_safety_scan(&lines, original_tokens);
            if let Some(c) = compressed {
                return c;
            }
        }
        if cleaned_tokens < original_tokens {
            let saved = original_tokens - cleaned_tokens;
            let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
            if pct >= 5 {
                return format!(
                    "{cleaned}\n[lean-ctx: {original_tokens}→{cleaned_tokens} tok, -{pct}%]"
                );
            }
            return cleaned;
        }
    }

    let lines: Vec<&str> = output.lines().collect();
    if lines.len() > 30 {
        if let Some(c) = truncate_with_safety_scan(&lines, original_tokens) {
            return c;
        }
    }

    output.to_string()
}

fn truncate_with_safety_scan(lines: &[&str], original_tokens: usize) -> Option<String> {
    use crate::core::safety_needles;

    let first = &lines[..5];
    let last = &lines[lines.len() - 5..];
    let middle = &lines[5..lines.len() - 5];

    let safety_lines = safety_needles::extract_safety_lines(middle, 20);
    let safety_count = safety_lines.len();
    let omitted = middle.len() - safety_count;

    let mut parts = Vec::new();
    parts.push(first.join("\n"));
    if safety_count > 0 {
        parts.push(format!(
            "[{omitted} lines omitted, {safety_count} safety-relevant lines preserved]"
        ));
        parts.push(safety_lines.join("\n"));
    } else {
        parts.push(format!("[{omitted} lines omitted]"));
    }
    parts.push(last.join("\n"));

    let compressed = parts.join("\n");
    let ct = count_tokens(&compressed);
    if ct >= original_tokens {
        return None;
    }
    let saved = original_tokens - ct;
    let pct = (saved as f64 / original_tokens as f64 * 100.0).round() as usize;
    if pct >= 5 {
        Some(format!(
            "{compressed}\n[lean-ctx: {original_tokens}→{ct} tok, -{pct}%]"
        ))
    } else {
        Some(compressed)
    }
}

/// Public wrapper for integration tests to exercise the compression pipeline.
pub fn compress_if_beneficial_pub(command: &str, output: &str) -> String {
    compress_if_beneficial(command, output)
}

#[cfg(test)]
mod passthrough_tests {
    use super::is_excluded_command;

    #[test]
    fn turbo_is_passthrough() {
        assert!(is_excluded_command("turbo run dev", &[]));
        assert!(is_excluded_command("turbo run build", &[]));
        assert!(is_excluded_command("pnpm turbo run dev", &[]));
        assert!(is_excluded_command("npx turbo run dev", &[]));
    }

    #[test]
    fn dev_servers_are_passthrough() {
        assert!(is_excluded_command("next dev", &[]));
        assert!(is_excluded_command("vite dev", &[]));
        assert!(is_excluded_command("nuxt dev", &[]));
        assert!(is_excluded_command("astro dev", &[]));
        assert!(is_excluded_command("nodemon server.js", &[]));
    }

    #[test]
    fn interactive_tools_are_passthrough() {
        assert!(is_excluded_command("vim file.rs", &[]));
        assert!(is_excluded_command("nvim", &[]));
        assert!(is_excluded_command("htop", &[]));
        assert!(is_excluded_command("ssh user@host", &[]));
        assert!(is_excluded_command("tail -f /var/log/syslog", &[]));
    }

    #[test]
    fn docker_streaming_is_passthrough() {
        assert!(is_excluded_command("docker logs my-container", &[]));
        assert!(is_excluded_command("docker logs -f webapp", &[]));
        assert!(is_excluded_command("docker attach my-container", &[]));
        assert!(is_excluded_command("docker exec -it web bash", &[]));
        assert!(is_excluded_command("docker exec -ti web bash", &[]));
        assert!(is_excluded_command("docker run -it ubuntu bash", &[]));
        assert!(is_excluded_command("docker compose exec web bash", &[]));
        assert!(is_excluded_command("docker stats", &[]));
        assert!(is_excluded_command("docker events", &[]));
    }

    #[test]
    fn kubectl_is_passthrough() {
        assert!(is_excluded_command("kubectl logs my-pod", &[]));
        assert!(is_excluded_command("kubectl logs -f deploy/web", &[]));
        assert!(is_excluded_command("kubectl exec -it pod -- bash", &[]));
        assert!(is_excluded_command(
            "kubectl port-forward svc/web 8080:80",
            &[]
        ));
        assert!(is_excluded_command("kubectl attach my-pod", &[]));
        assert!(is_excluded_command("kubectl proxy", &[]));
    }

    #[test]
    fn database_repls_are_passthrough() {
        assert!(is_excluded_command("psql -U user mydb", &[]));
        assert!(is_excluded_command("mysql -u root -p", &[]));
        assert!(is_excluded_command("sqlite3 data.db", &[]));
        assert!(is_excluded_command("redis-cli", &[]));
        assert!(is_excluded_command("mongosh", &[]));
    }

    #[test]
    fn streaming_tools_are_passthrough() {
        assert!(is_excluded_command("journalctl -f", &[]));
        assert!(is_excluded_command("ping 8.8.8.8", &[]));
        assert!(is_excluded_command("strace -p 1234", &[]));
        assert!(is_excluded_command("tcpdump -i eth0", &[]));
        assert!(is_excluded_command("tail -F /var/log/app.log", &[]));
        assert!(is_excluded_command("tmux new -s work", &[]));
        assert!(is_excluded_command("screen -S dev", &[]));
    }

    #[test]
    fn additional_dev_servers_are_passthrough() {
        assert!(is_excluded_command("gatsby develop", &[]));
        assert!(is_excluded_command("ng serve --port 4200", &[]));
        assert!(is_excluded_command("remix dev", &[]));
        assert!(is_excluded_command("wrangler dev", &[]));
        assert!(is_excluded_command("hugo server", &[]));
        assert!(is_excluded_command("bun dev", &[]));
        assert!(is_excluded_command("cargo watch -x test", &[]));
    }

    #[test]
    fn normal_commands_not_excluded() {
        assert!(!is_excluded_command("git status", &[]));
        assert!(!is_excluded_command("cargo test", &[]));
        assert!(!is_excluded_command("npm run build", &[]));
        assert!(!is_excluded_command("ls -la", &[]));
    }

    #[test]
    fn user_exclusions_work() {
        let excl = vec!["myapp".to_string()];
        assert!(is_excluded_command("myapp serve", &excl));
        assert!(!is_excluded_command("git status", &excl));
    }

    #[test]
    fn auth_commands_excluded() {
        assert!(is_excluded_command("az login --use-device-code", &[]));
        assert!(is_excluded_command("gh auth login", &[]));
        assert!(is_excluded_command("gh pr close --comment 'done'", &[]));
        assert!(is_excluded_command("gh issue list", &[]));
        assert!(is_excluded_command("gcloud auth login", &[]));
        assert!(is_excluded_command("aws sso login", &[]));
        assert!(is_excluded_command("firebase login", &[]));
        assert!(is_excluded_command("vercel login", &[]));
        assert!(is_excluded_command("heroku login", &[]));
        assert!(is_excluded_command("az login", &[]));
        assert!(is_excluded_command("kubelogin convert-kubeconfig", &[]));
        assert!(is_excluded_command("vault login -method=oidc", &[]));
        assert!(is_excluded_command("flyctl auth login", &[]));
    }

    #[test]
    fn auth_exclusion_does_not_affect_normal_commands() {
        assert!(!is_excluded_command("git log", &[]));
        assert!(!is_excluded_command("npm run build", &[]));
        assert!(!is_excluded_command("cargo test", &[]));
        assert!(!is_excluded_command("aws s3 ls", &[]));
        assert!(!is_excluded_command("gcloud compute instances list", &[]));
        assert!(!is_excluded_command("az vm list", &[]));
    }

    #[test]
    fn npm_script_runners_are_passthrough() {
        assert!(is_excluded_command("npm run dev", &[]));
        assert!(is_excluded_command("npm run start", &[]));
        assert!(is_excluded_command("npm run serve", &[]));
        assert!(is_excluded_command("npm run watch", &[]));
        assert!(is_excluded_command("npm run preview", &[]));
        assert!(is_excluded_command("npm run storybook", &[]));
        assert!(is_excluded_command("npm run test:watch", &[]));
        assert!(is_excluded_command("npm start", &[]));
        assert!(is_excluded_command("npx vite", &[]));
        assert!(is_excluded_command("npx next dev", &[]));
    }

    #[test]
    fn pnpm_script_runners_are_passthrough() {
        assert!(is_excluded_command("pnpm run dev", &[]));
        assert!(is_excluded_command("pnpm run start", &[]));
        assert!(is_excluded_command("pnpm run serve", &[]));
        assert!(is_excluded_command("pnpm run watch", &[]));
        assert!(is_excluded_command("pnpm run preview", &[]));
        assert!(is_excluded_command("pnpm dev", &[]));
        assert!(is_excluded_command("pnpm start", &[]));
        assert!(is_excluded_command("pnpm preview", &[]));
    }

    #[test]
    fn yarn_script_runners_are_passthrough() {
        assert!(is_excluded_command("yarn dev", &[]));
        assert!(is_excluded_command("yarn start", &[]));
        assert!(is_excluded_command("yarn serve", &[]));
        assert!(is_excluded_command("yarn watch", &[]));
        assert!(is_excluded_command("yarn preview", &[]));
        assert!(is_excluded_command("yarn storybook", &[]));
    }

    #[test]
    fn bun_deno_script_runners_are_passthrough() {
        assert!(is_excluded_command("bun run dev", &[]));
        assert!(is_excluded_command("bun run start", &[]));
        assert!(is_excluded_command("bun run serve", &[]));
        assert!(is_excluded_command("bun run watch", &[]));
        assert!(is_excluded_command("bun run preview", &[]));
        assert!(is_excluded_command("bun start", &[]));
        assert!(is_excluded_command("deno task dev", &[]));
        assert!(is_excluded_command("deno task start", &[]));
        assert!(is_excluded_command("deno task serve", &[]));
        assert!(is_excluded_command("deno run --watch main.ts", &[]));
    }

    #[test]
    fn python_servers_are_passthrough() {
        assert!(is_excluded_command("flask run --port 5000", &[]));
        assert!(is_excluded_command("uvicorn app:app --reload", &[]));
        assert!(is_excluded_command("gunicorn app:app -w 4", &[]));
        assert!(is_excluded_command("hypercorn app:app", &[]));
        assert!(is_excluded_command("daphne app.asgi:application", &[]));
        assert!(is_excluded_command(
            "django-admin runserver 0.0.0.0:8000",
            &[]
        ));
        assert!(is_excluded_command("python manage.py runserver", &[]));
        assert!(is_excluded_command("python -m http.server 8080", &[]));
        assert!(is_excluded_command("python3 -m http.server", &[]));
        assert!(is_excluded_command("streamlit run app.py", &[]));
        assert!(is_excluded_command("gradio app.py", &[]));
        assert!(is_excluded_command("celery worker -A app", &[]));
        assert!(is_excluded_command("celery -A app worker", &[]));
        assert!(is_excluded_command("celery -B", &[]));
        assert!(is_excluded_command("dramatiq tasks", &[]));
        assert!(is_excluded_command("rq worker", &[]));
        assert!(is_excluded_command("ptw tests/", &[]));
        assert!(is_excluded_command("pytest-watch", &[]));
    }

    #[test]
    fn ruby_servers_are_passthrough() {
        assert!(is_excluded_command("rails server -p 3000", &[]));
        assert!(is_excluded_command("rails s", &[]));
        assert!(is_excluded_command("puma -C config.rb", &[]));
        assert!(is_excluded_command("unicorn -c config.rb", &[]));
        assert!(is_excluded_command("thin start", &[]));
        assert!(is_excluded_command("foreman start", &[]));
        assert!(is_excluded_command("overmind start", &[]));
        assert!(is_excluded_command("guard -G Guardfile", &[]));
        assert!(is_excluded_command("sidekiq", &[]));
        assert!(is_excluded_command("resque work", &[]));
    }

    #[test]
    fn php_servers_are_passthrough() {
        assert!(is_excluded_command("php artisan serve", &[]));
        assert!(is_excluded_command("php -S localhost:8000", &[]));
        assert!(is_excluded_command("php artisan queue:work", &[]));
        assert!(is_excluded_command("php artisan queue:listen", &[]));
        assert!(is_excluded_command("php artisan horizon", &[]));
        assert!(is_excluded_command("php artisan tinker", &[]));
        assert!(is_excluded_command("sail up", &[]));
    }

    #[test]
    fn java_servers_are_passthrough() {
        assert!(is_excluded_command("./gradlew bootRun", &[]));
        assert!(is_excluded_command("gradlew bootRun", &[]));
        assert!(is_excluded_command("gradle bootRun", &[]));
        assert!(is_excluded_command("mvn spring-boot:run", &[]));
        assert!(is_excluded_command("./mvnw spring-boot:run", &[]));
        assert!(is_excluded_command("mvn quarkus:dev", &[]));
        assert!(is_excluded_command("./mvnw quarkus:dev", &[]));
        assert!(is_excluded_command("sbt run", &[]));
        assert!(is_excluded_command("sbt ~compile", &[]));
        assert!(is_excluded_command("lein run", &[]));
        assert!(is_excluded_command("lein repl", &[]));
        assert!(is_excluded_command("./gradlew run", &[]));
    }

    #[test]
    fn go_servers_are_passthrough() {
        assert!(is_excluded_command("go run main.go", &[]));
        assert!(is_excluded_command("go run ./cmd/server", &[]));
        assert!(is_excluded_command("air -c .air.toml", &[]));
        assert!(is_excluded_command("gin --port 3000", &[]));
        assert!(is_excluded_command("realize start", &[]));
        assert!(is_excluded_command("reflex -r '.go$' go run .", &[]));
        assert!(is_excluded_command("gowatch run", &[]));
    }

    #[test]
    fn dotnet_servers_are_passthrough() {
        assert!(is_excluded_command("dotnet run", &[]));
        assert!(is_excluded_command("dotnet run --project src/Api", &[]));
        assert!(is_excluded_command("dotnet watch run", &[]));
        assert!(is_excluded_command("dotnet ef database update", &[]));
    }

    #[test]
    fn elixir_servers_are_passthrough() {
        assert!(is_excluded_command("mix phx.server", &[]));
        assert!(is_excluded_command("iex -s mix phx.server", &[]));
        assert!(is_excluded_command("iex -S mix phx.server", &[]));
    }

    #[test]
    fn swift_zig_servers_are_passthrough() {
        assert!(is_excluded_command("swift run MyApp", &[]));
        assert!(is_excluded_command("swift package resolve", &[]));
        assert!(is_excluded_command("vapor serve --port 8080", &[]));
        assert!(is_excluded_command("zig build run", &[]));
    }

    #[test]
    fn rust_watchers_are_passthrough() {
        assert!(is_excluded_command("cargo watch -x test", &[]));
        assert!(is_excluded_command("cargo run --bin server", &[]));
        assert!(is_excluded_command("cargo leptos watch", &[]));
        assert!(is_excluded_command("bacon test", &[]));
    }

    #[test]
    fn general_task_runners_are_passthrough() {
        assert!(is_excluded_command("make dev", &[]));
        assert!(is_excluded_command("make serve", &[]));
        assert!(is_excluded_command("make watch", &[]));
        assert!(is_excluded_command("make run", &[]));
        assert!(is_excluded_command("make start", &[]));
        assert!(is_excluded_command("just dev", &[]));
        assert!(is_excluded_command("just serve", &[]));
        assert!(is_excluded_command("just watch", &[]));
        assert!(is_excluded_command("just start", &[]));
        assert!(is_excluded_command("just run", &[]));
        assert!(is_excluded_command("task dev", &[]));
        assert!(is_excluded_command("task serve", &[]));
        assert!(is_excluded_command("task watch", &[]));
        assert!(is_excluded_command("nix develop", &[]));
        assert!(is_excluded_command("devenv up", &[]));
    }

    #[test]
    fn cicd_infra_are_passthrough() {
        assert!(is_excluded_command("act push", &[]));
        assert!(is_excluded_command("docker compose watch", &[]));
        assert!(is_excluded_command("docker-compose watch", &[]));
        assert!(is_excluded_command("skaffold dev", &[]));
        assert!(is_excluded_command("tilt up", &[]));
        assert!(is_excluded_command("garden dev", &[]));
        assert!(is_excluded_command("telepresence connect", &[]));
    }

    #[test]
    fn networking_monitoring_are_passthrough() {
        assert!(is_excluded_command("mtr 8.8.8.8", &[]));
        assert!(is_excluded_command("nmap -sV host", &[]));
        assert!(is_excluded_command("iperf -s", &[]));
        assert!(is_excluded_command("iperf3 -c host", &[]));
        assert!(is_excluded_command("socat TCP-LISTEN:8080,fork -", &[]));
    }

    #[test]
    fn load_testing_is_passthrough() {
        assert!(is_excluded_command("ab -n 1000 http://localhost/", &[]));
        assert!(is_excluded_command("wrk -t12 -c400 http://localhost/", &[]));
        assert!(is_excluded_command("hey -n 10000 http://localhost/", &[]));
        assert!(is_excluded_command("vegeta attack", &[]));
        assert!(is_excluded_command("k6 run script.js", &[]));
        assert!(is_excluded_command("artillery run test.yml", &[]));
    }

    #[test]
    fn smart_script_detection_works() {
        assert!(is_excluded_command("npm run dev:ssr", &[]));
        assert!(is_excluded_command("npm run dev:local", &[]));
        assert!(is_excluded_command("yarn start:production", &[]));
        assert!(is_excluded_command("pnpm run serve:local", &[]));
        assert!(is_excluded_command("bun run watch:css", &[]));
        assert!(is_excluded_command("deno task dev:api", &[]));
        assert!(is_excluded_command("npm run storybook:ci", &[]));
        assert!(is_excluded_command("yarn preview:staging", &[]));
        assert!(is_excluded_command("pnpm run hot-reload", &[]));
        assert!(is_excluded_command("npm run hmr-server", &[]));
        assert!(is_excluded_command("bun run live-server", &[]));
    }

    #[test]
    fn smart_detection_does_not_false_positive() {
        assert!(!is_excluded_command("npm run build", &[]));
        assert!(!is_excluded_command("npm run lint", &[]));
        assert!(!is_excluded_command("npm run test", &[]));
        assert!(!is_excluded_command("npm run format", &[]));
        assert!(!is_excluded_command("yarn build", &[]));
        assert!(!is_excluded_command("yarn test", &[]));
        assert!(!is_excluded_command("pnpm run lint", &[]));
        assert!(!is_excluded_command("bun run build", &[]));
    }

    #[test]
    fn gh_fully_excluded() {
        assert!(is_excluded_command("gh", &[]));
        assert!(is_excluded_command(
            "gh pr close --comment 'closing — see #407'",
            &[]
        ));
        assert!(is_excluded_command(
            "gh issue create --title \"bug\" --body \"desc\"",
            &[]
        ));
        assert!(is_excluded_command("gh api repos/owner/repo/pulls", &[]));
        assert!(is_excluded_command("gh run list --limit 5", &[]));
    }
}
