#[cfg(test)]
mod passthrough_tests {
    use super::super::is_excluded_command;

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

#[cfg(test)]
mod verbatim_output_tests {
    use super::super::classification::is_git_data_command;
    use super::super::classification::is_verbatim_output;
    use super::super::engine::compress_if_beneficial;

    #[test]
    fn http_clients_are_verbatim() {
        assert!(is_verbatim_output("curl https://api.example.com"));
        assert!(is_verbatim_output(
            "curl -s -H 'Accept: application/json' https://api.example.com/data"
        ));
        assert!(is_verbatim_output(
            "curl -X POST -d '{\"key\":\"val\"}' https://api.example.com"
        ));
        assert!(is_verbatim_output("/usr/bin/curl https://example.com"));
        assert!(is_verbatim_output("wget -qO- https://example.com"));
        assert!(is_verbatim_output("wget https://example.com/file.json"));
        assert!(is_verbatim_output("http GET https://api.example.com"));
        assert!(is_verbatim_output("https PUT https://api.example.com/data"));
        assert!(is_verbatim_output("xh https://api.example.com"));
        assert!(is_verbatim_output("curlie https://api.example.com"));
        assert!(is_verbatim_output(
            "grpcurl -plaintext localhost:50051 list"
        ));
    }

    #[test]
    fn file_viewers_are_verbatim() {
        assert!(is_verbatim_output("cat package.json"));
        assert!(is_verbatim_output("cat /etc/hosts"));
        assert!(is_verbatim_output("/bin/cat file.txt"));
        assert!(is_verbatim_output("bat src/main.rs"));
        assert!(is_verbatim_output("batcat README.md"));
        assert!(is_verbatim_output("head -20 log.txt"));
        assert!(is_verbatim_output("head -n 50 file.rs"));
        assert!(is_verbatim_output("tail -100 server.log"));
        assert!(is_verbatim_output("tail -n 20 file.txt"));
    }

    #[test]
    fn tail_follow_not_verbatim() {
        assert!(!is_verbatim_output("tail -f /var/log/syslog"));
        assert!(!is_verbatim_output("tail --follow server.log"));
    }

    #[test]
    fn data_format_tools_are_verbatim() {
        assert!(is_verbatim_output("jq '.items' data.json"));
        assert!(is_verbatim_output("jq -r '.name' package.json"));
        assert!(is_verbatim_output("yq '.spec' deployment.yaml"));
        assert!(is_verbatim_output("xq '.rss.channel.title' feed.xml"));
        assert!(is_verbatim_output("fx data.json"));
        assert!(is_verbatim_output("gron data.json"));
        assert!(is_verbatim_output("mlr --csv head -n 5 data.csv"));
        assert!(is_verbatim_output("miller --json head data.json"));
        assert!(is_verbatim_output("dasel -f config.toml '.database.host'"));
        assert!(is_verbatim_output("csvlook data.csv"));
        assert!(is_verbatim_output("csvcut -c 1,3 data.csv"));
        assert!(is_verbatim_output("csvjson data.csv"));
    }

    #[test]
    fn binary_viewers_are_verbatim() {
        assert!(is_verbatim_output("xxd binary.dat"));
        assert!(is_verbatim_output("hexdump -C binary.dat"));
        assert!(is_verbatim_output("od -A x -t x1z binary.dat"));
        assert!(is_verbatim_output("strings /usr/bin/curl"));
        assert!(is_verbatim_output("file unknown.bin"));
    }

    #[test]
    fn infra_inspection_is_verbatim() {
        assert!(is_verbatim_output("terraform output"));
        assert!(is_verbatim_output("terraform show"));
        assert!(is_verbatim_output("terraform state show aws_instance.web"));
        assert!(is_verbatim_output("terraform state list"));
        assert!(is_verbatim_output("terraform state pull"));
        assert!(is_verbatim_output("tofu output"));
        assert!(is_verbatim_output("tofu show"));
        assert!(is_verbatim_output("pulumi stack output"));
        assert!(is_verbatim_output("pulumi stack export"));
        assert!(is_verbatim_output("docker inspect my-container"));
        assert!(is_verbatim_output("podman inspect my-pod"));
        assert!(is_verbatim_output("kubectl get pods -o yaml"));
        assert!(is_verbatim_output("kubectl get deploy -ojson"));
        assert!(is_verbatim_output("kubectl get svc --output yaml"));
        assert!(is_verbatim_output("kubectl get pods --output=json"));
        assert!(is_verbatim_output("k get pods -o yaml"));
        assert!(is_verbatim_output("kubectl describe pod my-pod"));
        assert!(is_verbatim_output("k describe deployment web"));
        assert!(is_verbatim_output("helm get values my-release"));
        assert!(is_verbatim_output("helm template my-chart"));
    }

    #[test]
    fn terraform_plan_not_verbatim() {
        assert!(!is_verbatim_output("terraform plan"));
        assert!(!is_verbatim_output("terraform apply"));
        assert!(!is_verbatim_output("terraform init"));
    }

    #[test]
    fn kubectl_get_is_now_verbatim() {
        assert!(is_verbatim_output("kubectl get pods"));
        assert!(is_verbatim_output("kubectl get deployments"));
    }

    #[test]
    fn crypto_commands_are_verbatim() {
        assert!(is_verbatim_output("openssl x509 -in cert.pem -text"));
        assert!(is_verbatim_output(
            "openssl s_client -connect example.com:443"
        ));
        assert!(is_verbatim_output("openssl req -new -x509 -key key.pem"));
        assert!(is_verbatim_output("gpg --list-keys"));
        assert!(is_verbatim_output("ssh-keygen -l -f key.pub"));
    }

    #[test]
    fn database_queries_are_verbatim() {
        assert!(is_verbatim_output(r#"psql -c "SELECT * FROM users" mydb"#));
        assert!(is_verbatim_output("psql --command 'SELECT 1' mydb"));
        assert!(is_verbatim_output(r#"mysql -e "SELECT * FROM users" mydb"#));
        assert!(is_verbatim_output("mysql --execute 'SHOW TABLES' mydb"));
        assert!(is_verbatim_output(
            r#"mariadb -e "SELECT * FROM users" mydb"#
        ));
        assert!(is_verbatim_output(
            r#"sqlite3 data.db "SELECT * FROM users""#
        ));
        assert!(is_verbatim_output("mongosh --eval 'db.users.find()' mydb"));
    }

    #[test]
    fn interactive_db_not_verbatim() {
        assert!(!is_verbatim_output("psql mydb"));
        assert!(!is_verbatim_output("mysql -u root mydb"));
    }

    #[test]
    fn dns_network_inspection_is_verbatim() {
        assert!(is_verbatim_output("dig example.com"));
        assert!(is_verbatim_output("dig +short example.com A"));
        assert!(is_verbatim_output("nslookup example.com"));
        assert!(is_verbatim_output("host example.com"));
        assert!(is_verbatim_output("whois example.com"));
        assert!(is_verbatim_output("drill example.com"));
    }

    #[test]
    fn language_one_liners_are_verbatim() {
        assert!(is_verbatim_output(
            "python -c 'import json; print(json.dumps({\"key\": \"value\"}))'"
        ));
        assert!(is_verbatim_output("python3 -c 'print(42)'"));
        assert!(is_verbatim_output(
            "node -e 'console.log(JSON.stringify({a:1}))'"
        ));
        assert!(is_verbatim_output("node --eval 'console.log(1)'"));
        assert!(is_verbatim_output("ruby -e 'puts 42'"));
        assert!(is_verbatim_output("perl -e 'print 42'"));
        assert!(is_verbatim_output("php -r 'echo json_encode([1,2,3]);'"));
    }

    #[test]
    fn language_scripts_not_verbatim() {
        assert!(!is_verbatim_output("python script.py"));
        assert!(!is_verbatim_output("node server.js"));
        assert!(!is_verbatim_output("ruby app.rb"));
    }

    #[test]
    fn container_listings_are_verbatim() {
        assert!(is_verbatim_output("docker ps"));
        assert!(is_verbatim_output("docker ps -a"));
        assert!(is_verbatim_output("docker images"));
        assert!(is_verbatim_output("docker images -a"));
        assert!(is_verbatim_output("podman ps"));
        assert!(is_verbatim_output("podman images"));
        assert!(is_verbatim_output("kubectl get pods"));
        assert!(is_verbatim_output("kubectl get deployments -A"));
        assert!(is_verbatim_output("kubectl get svc --all-namespaces"));
        assert!(is_verbatim_output("k get pods"));
        assert!(is_verbatim_output("helm list"));
        assert!(is_verbatim_output("helm ls --all-namespaces"));
        assert!(is_verbatim_output("docker compose ps"));
        assert!(is_verbatim_output("docker-compose ps"));
    }

    #[test]
    fn file_listings_are_verbatim() {
        assert!(is_verbatim_output("find . -name '*.rs'"));
        assert!(is_verbatim_output("find /var/log -type f"));
        assert!(is_verbatim_output("fd --extension rs"));
        assert!(is_verbatim_output("fdfind .rs src/"));
        assert!(is_verbatim_output("ls -la"));
        assert!(is_verbatim_output("ls -lah /tmp"));
        assert!(is_verbatim_output("exa -la"));
        assert!(is_verbatim_output("eza --long"));
    }

    #[test]
    fn system_queries_are_verbatim() {
        assert!(is_verbatim_output("stat file.txt"));
        assert!(is_verbatim_output("wc -l file.txt"));
        assert!(is_verbatim_output("du -sh /var"));
        assert!(is_verbatim_output("df -h"));
        assert!(is_verbatim_output("free -m"));
        assert!(is_verbatim_output("uname -a"));
        assert!(is_verbatim_output("id"));
        assert!(is_verbatim_output("whoami"));
        assert!(is_verbatim_output("hostname"));
        assert!(is_verbatim_output("which python3"));
        assert!(is_verbatim_output("readlink -f ./link"));
        assert!(is_verbatim_output("sha256sum file.tar.gz"));
        assert!(is_verbatim_output("base64 file.bin"));
        assert!(is_verbatim_output("ip addr show"));
        assert!(is_verbatim_output("ss -tlnp"));
    }

    #[test]
    fn pipe_tail_detection() {
        assert!(
            is_verbatim_output("kubectl get pods -o json | jq '.items[].metadata.name'"),
            "piped to jq must be verbatim"
        );
        assert!(
            is_verbatim_output("aws s3api list-objects --bucket x | jq '.Contents'"),
            "piped to jq must be verbatim"
        );
        assert!(
            is_verbatim_output("docker inspect web | head -50"),
            "piped to head must be verbatim"
        );
        assert!(
            is_verbatim_output("terraform state pull | jq '.resources'"),
            "piped to jq must be verbatim"
        );
        assert!(
            is_verbatim_output("echo hello | wc -l"),
            "piped to wc (system query) should be verbatim"
        );
    }

    #[test]
    fn build_commands_not_verbatim() {
        assert!(!is_verbatim_output("cargo build"));
        assert!(!is_verbatim_output("npm run build"));
        assert!(!is_verbatim_output("make"));
        assert!(!is_verbatim_output("docker build ."));
        assert!(!is_verbatim_output("go build ./..."));
        assert!(!is_verbatim_output("cargo test"));
        assert!(!is_verbatim_output("pytest"));
        assert!(!is_verbatim_output("npm install"));
        assert!(!is_verbatim_output("pip install requests"));
        assert!(!is_verbatim_output("terraform plan"));
        assert!(!is_verbatim_output("terraform apply"));
    }

    #[test]
    fn cloud_cli_queries_are_verbatim() {
        assert!(is_verbatim_output("aws sts get-caller-identity"));
        assert!(is_verbatim_output("aws ec2 describe-instances"));
        assert!(is_verbatim_output(
            "aws s3api list-objects --bucket my-bucket"
        ));
        assert!(is_verbatim_output("aws iam list-users"));
        assert!(is_verbatim_output("aws ecs describe-tasks --cluster x"));
        assert!(is_verbatim_output("aws rds describe-db-instances"));
        assert!(is_verbatim_output("gcloud compute instances list"));
        assert!(is_verbatim_output("gcloud projects describe my-project"));
        assert!(is_verbatim_output("gcloud iam roles list"));
        assert!(is_verbatim_output("gcloud container clusters list"));
        assert!(is_verbatim_output("az vm list"));
        assert!(is_verbatim_output("az account show"));
        assert!(is_verbatim_output("az network nsg list"));
        assert!(is_verbatim_output("az aks show --name mycluster"));
    }

    #[test]
    fn cloud_cli_mutations_not_verbatim() {
        assert!(!is_verbatim_output("aws configure"));
        assert!(!is_verbatim_output("gcloud auth login"));
        assert!(!is_verbatim_output("az login"));
        assert!(!is_verbatim_output("gcloud app deploy"));
    }

    #[test]
    fn package_manager_info_is_verbatim() {
        assert!(is_verbatim_output("npm list"));
        assert!(is_verbatim_output("npm ls --all"));
        assert!(is_verbatim_output("npm info react"));
        assert!(is_verbatim_output("npm view react versions"));
        assert!(is_verbatim_output("npm outdated"));
        assert!(is_verbatim_output("npm audit"));
        assert!(is_verbatim_output("yarn list"));
        assert!(is_verbatim_output("yarn info react"));
        assert!(is_verbatim_output("yarn why react"));
        assert!(is_verbatim_output("yarn audit"));
        assert!(is_verbatim_output("pnpm list"));
        assert!(is_verbatim_output("pnpm why react"));
        assert!(is_verbatim_output("pnpm outdated"));
        assert!(is_verbatim_output("pip list"));
        assert!(is_verbatim_output("pip show requests"));
        assert!(is_verbatim_output("pip freeze"));
        assert!(is_verbatim_output("pip3 list"));
        assert!(is_verbatim_output("gem list"));
        assert!(is_verbatim_output("gem info rails"));
        assert!(is_verbatim_output("cargo metadata"));
        assert!(is_verbatim_output("cargo tree"));
        assert!(is_verbatim_output("go list ./..."));
        assert!(is_verbatim_output("go version"));
        assert!(is_verbatim_output("composer show"));
        assert!(is_verbatim_output("composer outdated"));
        assert!(is_verbatim_output("brew list"));
        assert!(is_verbatim_output("brew info node"));
        assert!(is_verbatim_output("brew deps node"));
        assert!(is_verbatim_output("apt list --installed"));
        assert!(is_verbatim_output("apt show nginx"));
        assert!(is_verbatim_output("dpkg -l"));
        assert!(is_verbatim_output("dpkg -s nginx"));
    }

    #[test]
    fn package_manager_install_not_verbatim() {
        assert!(!is_verbatim_output("npm install"));
        assert!(!is_verbatim_output("yarn add react"));
        assert!(!is_verbatim_output("pip install requests"));
        assert!(!is_verbatim_output("cargo build"));
        assert!(!is_verbatim_output("go build"));
        assert!(!is_verbatim_output("brew install node"));
        assert!(!is_verbatim_output("apt install nginx"));
    }

    #[test]
    fn version_and_help_are_verbatim() {
        assert!(is_verbatim_output("node --version"));
        assert!(is_verbatim_output("python3 --version"));
        assert!(is_verbatim_output("rustc -V"));
        assert!(is_verbatim_output("docker version"));
        assert!(is_verbatim_output("git --version"));
        assert!(is_verbatim_output("cargo --help"));
        assert!(is_verbatim_output("docker help"));
        assert!(is_verbatim_output("git -h"));
        assert!(is_verbatim_output("npm help install"));
    }

    #[test]
    fn version_flag_needs_binary_context() {
        assert!(!is_verbatim_output("--version"));
        assert!(
            !is_verbatim_output("some command with --version and other args too"),
            "commands with 4+ tokens should not match version check"
        );
    }

    #[test]
    fn config_viewers_are_verbatim() {
        assert!(is_verbatim_output("git config --list"));
        assert!(is_verbatim_output("git config --global --list"));
        assert!(is_verbatim_output("git config user.email"));
        assert!(is_verbatim_output("npm config list"));
        assert!(is_verbatim_output("npm config get registry"));
        assert!(is_verbatim_output("yarn config list"));
        assert!(is_verbatim_output("pip config list"));
        assert!(is_verbatim_output("rustup show"));
        assert!(is_verbatim_output("rustup target list"));
        assert!(is_verbatim_output("docker context ls"));
        assert!(is_verbatim_output("kubectl config view"));
        assert!(is_verbatim_output("kubectl config get-contexts"));
        assert!(is_verbatim_output("kubectl config current-context"));
    }

    #[test]
    fn config_setters_not_verbatim() {
        assert!(!is_verbatim_output("git config --set user.name foo"));
        assert!(!is_verbatim_output("git config --unset user.name"));
    }

    #[test]
    fn log_viewers_are_verbatim() {
        assert!(is_verbatim_output("journalctl -u nginx"));
        assert!(is_verbatim_output("journalctl --since '1 hour ago'"));
        assert!(is_verbatim_output("dmesg"));
        assert!(is_verbatim_output("dmesg --level=err"));
        assert!(is_verbatim_output("docker logs mycontainer"));
        assert!(is_verbatim_output("docker logs --tail 100 web"));
        assert!(is_verbatim_output("kubectl logs pod/web"));
        assert!(is_verbatim_output("docker compose logs web"));
    }

    #[test]
    fn follow_logs_not_verbatim() {
        assert!(!is_verbatim_output("journalctl -f"));
        assert!(!is_verbatim_output("journalctl --follow -u nginx"));
        assert!(!is_verbatim_output("dmesg -w"));
        assert!(!is_verbatim_output("dmesg --follow"));
        assert!(!is_verbatim_output("docker logs -f web"));
        assert!(!is_verbatim_output("kubectl logs -f pod/web"));
        assert!(!is_verbatim_output("docker compose logs -f"));
    }

    #[test]
    fn archive_listings_are_verbatim() {
        assert!(is_verbatim_output("tar -tf archive.tar.gz"));
        assert!(is_verbatim_output("tar tf archive.tar"));
        assert!(is_verbatim_output("unzip -l archive.zip"));
        assert!(is_verbatim_output("zipinfo archive.zip"));
        assert!(is_verbatim_output("lsar archive.7z"));
    }

    #[test]
    fn clipboard_tools_are_verbatim() {
        assert!(is_verbatim_output("pbpaste"));
        assert!(is_verbatim_output("wl-paste"));
        assert!(is_verbatim_output("xclip -o"));
        assert!(is_verbatim_output("xclip -selection clipboard -o"));
        assert!(is_verbatim_output("xsel -o"));
        assert!(is_verbatim_output("xsel --output"));
    }

    #[test]
    fn git_data_commands_are_verbatim() {
        assert!(is_verbatim_output("git remote -v"));
        assert!(is_verbatim_output("git remote show origin"));
        assert!(is_verbatim_output("git config --list"));
        assert!(is_verbatim_output("git rev-parse HEAD"));
        assert!(is_verbatim_output("git rev-parse --show-toplevel"));
        assert!(is_verbatim_output("git ls-files"));
        assert!(is_verbatim_output("git ls-tree HEAD"));
        assert!(is_verbatim_output("git ls-remote origin"));
        assert!(is_verbatim_output("git shortlog -sn"));
        assert!(is_verbatim_output("git for-each-ref --format='%(refname)'"));
        assert!(is_verbatim_output("git cat-file -p HEAD"));
        assert!(is_verbatim_output("git describe --tags"));
        assert!(is_verbatim_output("git merge-base main feature"));
    }

    #[test]
    fn git_mutations_not_verbatim_via_git_data() {
        assert!(!is_git_data_command("git commit -m 'fix'"));
        assert!(!is_git_data_command("git push"));
        assert!(!is_git_data_command("git pull"));
        assert!(!is_git_data_command("git fetch"));
        assert!(!is_git_data_command("git add ."));
        assert!(!is_git_data_command("git rebase main"));
        assert!(!is_git_data_command("git cherry-pick abc123"));
    }

    #[test]
    fn task_dry_run_is_verbatim() {
        assert!(is_verbatim_output("make -n build"));
        assert!(is_verbatim_output("make --dry-run"));
        assert!(is_verbatim_output("ansible-playbook --check site.yml"));
        assert!(is_verbatim_output(
            "ansible-playbook --diff --check site.yml"
        ));
    }

    #[test]
    fn task_execution_not_verbatim() {
        assert!(!is_verbatim_output("make build"));
        assert!(!is_verbatim_output("make"));
        assert!(!is_verbatim_output("ansible-playbook site.yml"));
    }

    #[test]
    fn env_dump_is_verbatim() {
        assert!(is_verbatim_output("env"));
        assert!(is_verbatim_output("printenv"));
        assert!(is_verbatim_output("printenv PATH"));
        assert!(is_verbatim_output("locale"));
    }

    #[test]
    fn curl_json_output_preserved() {
        let json = r#"{"users":[{"id":1,"name":"Alice","email":"alice@example.com"},{"id":2,"name":"Bob","email":"bob@example.com"}],"total":2,"page":1}"#;
        let result = compress_if_beneficial("curl https://api.example.com/users", json);
        assert!(
            result.contains("alice@example.com"),
            "curl JSON data must be preserved verbatim, got: {result}"
        );
        assert!(
            result.contains(r#""name":"Bob""#),
            "curl JSON data must be preserved verbatim, got: {result}"
        );
    }

    #[test]
    fn curl_html_output_preserved() {
        let html = "<!DOCTYPE html><html><head><title>Test Page</title></head><body><h1>Hello World</h1><p>Some important content here that should not be summarized.</p></body></html>";
        let result = compress_if_beneficial("curl https://example.com", html);
        assert!(
            result.contains("Hello World"),
            "curl HTML content must be preserved, got: {result}"
        );
        assert!(
            result.contains("important content"),
            "curl HTML content must be preserved, got: {result}"
        );
    }

    #[test]
    fn curl_headers_preserved() {
        let headers = "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nX-Request-Id: abc-123\r\nX-RateLimit-Remaining: 59\r\nContent-Length: 1234\r\nServer: nginx\r\nDate: Mon, 01 Jan 2024 00:00:00 GMT\r\n\r\n";
        let result = compress_if_beneficial("curl -I https://api.example.com", headers);
        assert!(
            result.contains("X-Request-Id: abc-123"),
            "curl headers must be preserved, got: {result}"
        );
        assert!(
            result.contains("X-RateLimit-Remaining"),
            "curl headers must be preserved, got: {result}"
        );
    }

    #[test]
    fn cat_output_preserved() {
        let content = r#"{
  "name": "lean-ctx",
  "version": "3.5.16",
  "description": "Context Runtime for AI Agents",
  "main": "index.js",
  "scripts": {
    "build": "cargo build --release",
    "test": "cargo test"
  }
}"#;
        let result = compress_if_beneficial("cat package.json", content);
        assert!(
            result.contains(r#""version": "3.5.16""#),
            "cat output must be preserved, got: {result}"
        );
    }

    #[test]
    fn jq_output_preserved() {
        let json = r#"[
  {"id": 1, "status": "active", "name": "Alice"},
  {"id": 2, "status": "inactive", "name": "Bob"},
  {"id": 3, "status": "active", "name": "Charlie"}
]"#;
        let result =
            compress_if_beneficial("jq '.[] | select(.status==\"active\")' data.json", json);
        assert!(
            result.contains("Charlie"),
            "jq output must be preserved, got: {result}"
        );
    }

    #[test]
    fn wget_output_preserved() {
        let content = r#"{"key": "value", "data": [1, 2, 3]}"#;
        let result = compress_if_beneficial("wget -qO- https://api.example.com/data", content);
        assert!(
            result.contains(r#""data": [1, 2, 3]"#),
            "wget data output must be preserved, got: {result}"
        );
    }

    #[test]
    fn large_curl_output_gets_truncated_not_destroyed() {
        let mut json = String::from("[");
        for i in 0..500 {
            if i > 0 {
                json.push(',');
            }
            json.push_str(&format!(
                r#"{{"id":{i},"name":"user_{i}","email":"user{i}@example.com","role":"admin"}}"#
            ));
        }
        json.push(']');
        let result = compress_if_beneficial("curl https://api.example.com/all-users", &json);
        assert!(
            result.contains("user_0"),
            "first items must be preserved in truncated output, got len: {}",
            result.len()
        );
        if result.contains("lines omitted") {
            assert!(
                result.contains("verbatim truncated"),
                "must mark as verbatim truncated, got: {result}"
            );
        }
    }
}

#[cfg(test)]
mod cli_api_data_tests {
    use super::super::classification::is_verbatim_output;

    #[test]
    fn gh_api_is_verbatim() {
        assert!(is_verbatim_output("gh api repos/owner/repo/issues/198"));
        assert!(is_verbatim_output("gh api repos/owner/repo/pulls/42"));
        assert!(is_verbatim_output(
            "gh api repos/owner/repo/issues/198 --jq '.body'"
        ));
    }

    #[test]
    fn gh_json_and_jq_flags_are_verbatim() {
        assert!(is_verbatim_output("gh pr list --json number,title"));
        assert!(is_verbatim_output("gh issue list --jq '.[]'"));
        assert!(is_verbatim_output("gh pr view 42 --json body --jq '.body'"));
        assert!(is_verbatim_output("gh pr view 5 --template '{{.body}}'"));
    }

    #[test]
    fn gh_search_and_release_verbatim() {
        assert!(is_verbatim_output("gh search repos lean-ctx"));
        assert!(is_verbatim_output("gh release view v3.5.18"));
        assert!(is_verbatim_output("gh gist view abc123"));
        assert!(is_verbatim_output("gh gist list"));
    }

    #[test]
    fn gh_run_log_verbatim() {
        assert!(is_verbatim_output("gh run view 12345 --log"));
        assert!(is_verbatim_output("gh run view 12345 --log-failed"));
    }

    #[test]
    fn glab_api_is_verbatim() {
        assert!(is_verbatim_output("glab api projects/123/issues"));
    }

    #[test]
    fn jira_linear_verbatim() {
        assert!(is_verbatim_output("jira issue view PROJ-42"));
        assert!(is_verbatim_output("jira issue list"));
        assert!(is_verbatim_output("linear issue list"));
    }

    #[test]
    fn saas_cli_data_commands_verbatim() {
        assert!(is_verbatim_output("stripe charges list"));
        assert!(is_verbatim_output("vercel logs my-deploy"));
        assert!(is_verbatim_output("fly status"));
        assert!(is_verbatim_output("railway logs"));
        assert!(is_verbatim_output("heroku logs --tail"));
        assert!(is_verbatim_output("heroku config"));
    }

    #[test]
    fn gh_pr_create_not_verbatim() {
        assert!(!is_verbatim_output("gh pr create --title 'Fix bug'"));
        assert!(!is_verbatim_output("gh issue create --body 'desc'"));
    }

    #[test]
    fn gh_api_pipe_is_verbatim() {
        assert!(is_verbatim_output(
            "gh api repos/owner/repo/pulls/42 | jq '.body'"
        ));
    }
}

#[cfg(test)]
mod structural_output_tests {
    use super::super::classification::has_structural_output;
    use super::super::engine::compress_if_beneficial;

    #[test]
    fn git_diff_is_structural() {
        assert!(has_structural_output("git diff"));
        assert!(has_structural_output("git diff --cached"));
        assert!(has_structural_output("git diff --staged"));
        assert!(has_structural_output("git diff HEAD~1"));
        assert!(has_structural_output("git diff main..feature"));
        assert!(has_structural_output("git diff -- src/main.rs"));
    }

    #[test]
    fn git_show_is_structural() {
        assert!(has_structural_output("git show"));
        assert!(has_structural_output("git show HEAD"));
        assert!(has_structural_output("git show abc1234"));
        assert!(has_structural_output("git show stash@{0}"));
    }

    #[test]
    fn git_blame_is_structural() {
        assert!(has_structural_output("git blame src/main.rs"));
        assert!(has_structural_output("git blame -L 10,20 file.rs"));
    }

    #[test]
    fn git_with_flags_is_structural() {
        assert!(has_structural_output("git -C /tmp diff"));
        assert!(has_structural_output("git --git-dir /path diff HEAD"));
        assert!(has_structural_output("git -c core.pager=cat show abc"));
    }

    #[test]
    fn case_insensitive() {
        assert!(has_structural_output("Git Diff"));
        assert!(has_structural_output("GIT DIFF --cached"));
        assert!(has_structural_output("git SHOW HEAD"));
    }

    #[test]
    fn full_path_git_binary() {
        assert!(has_structural_output("/usr/bin/git diff"));
        assert!(has_structural_output("/usr/local/bin/git show HEAD"));
    }

    #[test]
    fn standalone_diff_is_structural() {
        assert!(has_structural_output("diff file1.txt file2.txt"));
        assert!(has_structural_output("diff -u old.py new.py"));
        assert!(has_structural_output("diff -r dir1 dir2"));
        assert!(has_structural_output("/usr/bin/diff a b"));
        assert!(has_structural_output("colordiff file1 file2"));
        assert!(has_structural_output("icdiff old.rs new.rs"));
        assert!(has_structural_output("delta"));
    }

    #[test]
    fn git_log_with_patch_is_structural() {
        assert!(has_structural_output("git log -p"));
        assert!(has_structural_output("git log --patch"));
        assert!(has_structural_output("git log -p HEAD~5"));
        assert!(has_structural_output("git log -p --stat"));
        assert!(has_structural_output("git log --patch --follow file.rs"));
    }

    #[test]
    fn git_log_without_patch_not_structural() {
        assert!(!has_structural_output("git log"));
        assert!(!has_structural_output("git log --oneline"));
        assert!(!has_structural_output("git log -n 5"));
    }

    #[test]
    fn git_log_with_stat_is_structural() {
        assert!(has_structural_output("git log --stat"));
        assert!(has_structural_output("git log --stat -n 5"));
    }

    #[test]
    fn git_stash_show_is_structural() {
        assert!(has_structural_output("git stash show"));
        assert!(has_structural_output("git stash show -p"));
        assert!(has_structural_output("git stash show --patch"));
        assert!(has_structural_output("git stash show stash@{0}"));
    }

    #[test]
    fn git_stash_without_show_not_structural() {
        assert!(!has_structural_output("git stash"));
        assert!(!has_structural_output("git stash list"));
        assert!(!has_structural_output("git stash pop"));
        assert!(!has_structural_output("git stash drop"));
    }

    #[test]
    fn non_structural_git_commands() {
        assert!(!has_structural_output("git status"));
        assert!(!has_structural_output("git commit -m 'fix'"));
        assert!(!has_structural_output("git push"));
        assert!(!has_structural_output("git pull"));
        assert!(!has_structural_output("git branch"));
        assert!(!has_structural_output("git fetch"));
        assert!(!has_structural_output("git add ."));
    }

    #[test]
    fn non_git_commands() {
        assert!(!has_structural_output("cargo build"));
        assert!(!has_structural_output("npm run build"));
    }

    #[test]
    fn verbatim_commands_are_also_structural() {
        assert!(has_structural_output("ls -la"));
        assert!(has_structural_output("docker ps"));
        assert!(has_structural_output("curl https://api.example.com"));
        assert!(has_structural_output("cat file.txt"));
        assert!(has_structural_output("aws ec2 describe-instances"));
        assert!(has_structural_output("npm list"));
        assert!(has_structural_output("node --version"));
        assert!(has_structural_output("journalctl -u nginx"));
        assert!(has_structural_output("git remote -v"));
        assert!(has_structural_output("pbpaste"));
        assert!(has_structural_output("env"));
    }

    #[test]
    fn git_diff_output_preserves_hunks() {
        let diff = "diff --git a/src/main.rs b/src/main.rs\n\
            index abc1234..def5678 100644\n\
            --- a/src/main.rs\n\
            +++ b/src/main.rs\n\
            @@ -1,5 +1,6 @@\n\
             fn main() {\n\
            +    println!(\"hello\");\n\
                 let x = 1;\n\
                 let y = 2;\n\
            -    let z = 3;\n\
            +    let z = x + y;\n\
             }";
        let result = compress_if_beneficial("git diff", diff);
        assert!(
            result.contains("+    println!"),
            "must preserve added lines, got: {result}"
        );
        assert!(
            result.contains("-    let z = 3;"),
            "must preserve removed lines, got: {result}"
        );
        assert!(
            result.contains("@@ -1,5 +1,6 @@"),
            "must preserve hunk headers, got: {result}"
        );
    }

    #[test]
    fn git_diff_large_preserves_content() {
        let mut diff = String::new();
        diff.push_str("diff --git a/file.rs b/file.rs\n");
        diff.push_str("--- a/file.rs\n+++ b/file.rs\n");
        diff.push_str("@@ -1,100 +1,100 @@\n");
        for i in 0..80 {
            diff.push_str(&format!("+added line {i}: some actual code content\n"));
            diff.push_str(&format!("-removed line {i}: old code content\n"));
        }
        let result = compress_if_beneficial("git diff", &diff);
        assert!(
            result.contains("+added line 0"),
            "must preserve first added line, got len: {}",
            result.len()
        );
        assert!(
            result.contains("-removed line 0"),
            "must preserve first removed line, got len: {}",
            result.len()
        );
    }
}
