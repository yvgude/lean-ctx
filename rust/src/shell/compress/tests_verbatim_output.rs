use super::*;
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

/// GH #688 (severe): a `sed`/`awk` file dump must never enter the generic
/// terse pipeline — its dictionary layer word-substitutes code identifiers
/// that happen to match English words (`function`→`fn`, `return`→`ret`)
/// with no code-awareness, corrupting source read via a range-print
/// instead of `cat`.
#[test]
fn sed_awk_file_dumps_are_verbatim() {
    assert!(is_verbatim_output("sed -n '1,50p' build_windows.ps1"));
    assert!(is_verbatim_output("sed -n '/start/,/end/p' file.rs"));
    // #1084: semicolon-separated multi-range selectors must stay verbatim
    assert!(is_verbatim_output(
        "sed -n '236,270p;780,800p' custom_components/solvis_control/const.py"
    ));
    assert!(is_verbatim_output("sed 's/foo/bar/' file.txt"));
    assert!(is_verbatim_output("awk '{print}' file.txt"));
    assert!(is_verbatim_output("awk -F, '{print $1}' data.csv"));
    assert!(is_verbatim_output("gawk '{print $0}' file.log"));
}

#[test]
fn sed_awk_in_place_not_verbatim() {
    assert!(!is_verbatim_output("sed -i 's/foo/bar/' file.txt"));
    assert!(!is_verbatim_output("sed -i.bak 's/foo/bar/' file.txt"));
    assert!(!is_verbatim_output("sed --in-place 's/foo/bar/' file.txt"));
    assert!(!is_verbatim_output("sed --in-place=.bak 's/x/y/' f.txt"));
    assert!(!is_verbatim_output("sed -ni 's/foo/bar/p' file.txt"));
    assert!(!is_verbatim_output("gawk -i inplace '{print}' file.txt"));
}

/// The in-place check is token-based: filenames or pattern text containing
/// "-i" as a substring are NOT in-place flags — a substring match would
/// silently drop those dumps back into the terse pipeline (the exact
/// corruption GH #688 fixes).
#[test]
fn sed_awk_filenames_containing_dash_i_stay_verbatim() {
    assert!(is_verbatim_output("sed -n '1,20p' my-input.txt"));
    assert!(is_verbatim_output("awk '{print}' data-import.csv"));
    assert!(is_verbatim_output("sed -n '5p' check-install.sh"));
    assert!(is_verbatim_output("awk -F: '{print $1 - i}' totals.txt"));
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
fn kubectl_get_uses_pattern_not_verbatim() {
    assert!(!is_verbatim_output("kubectl get pods"));
    assert!(!is_verbatim_output("kubectl get deployments"));
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
    // kubectl get uses pattern compressor now (not verbatim)
    assert!(!is_verbatim_output("kubectl get pods"));
    assert!(!is_verbatim_output("kubectl get deployments -A"));
    assert!(!is_verbatim_output("kubectl get svc --all-namespaces"));
    assert!(!is_verbatim_output("k get pods"));
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
    assert!(is_verbatim_output("exa -la"));
    assert!(is_verbatim_output("eza --long"));
}

#[test]
fn ls_is_compressible_via_its_dedicated_listing_pattern() {
    assert!(!is_verbatim_output("ls -la"));
    assert!(!is_verbatim_output("ls -lah /tmp"));
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

/// GH #688 (severe) regression: a `sed -n` range-print of PowerShell
/// source must come back byte-exact. Before the classification fix, this
/// fell through to the generic terse pipeline, which word-substituted
/// `function`->`fn`, `return`->`ret` and dropped bare `else` lines as
/// low-information.
#[test]
fn sed_dump_of_powershell_code_is_byte_exact() {
    let content = "function Sign-File([string]$path) {\n    param(\n        [string]$certPath,\n        [string]$password\n    )\n    $env = [Environment]::GetEnvironmentVariable(\"CERT_PATH\")\n    if ($path -eq $env) {\n        Write-Host \"Signing $path with certificate\"\n        return $true\n    } else {\n        Write-Host \"Skipping unsigned file\"\n        return $false\n    }\n}\n";
    let result = compress_if_beneficial("sed -n '1,13p' build_windows.ps1", content);
    assert_eq!(
        result, content,
        "sed range-print of source must be byte-exact, got:\n{result}"
    );
}

#[test]
fn jq_output_preserved() {
    let json = r#"[
  {"id": 1, "status": "active", "name": "Alice"},
  {"id": 2, "status": "inactive", "name": "Bob"},
  {"id": 3, "status": "active", "name": "Charlie"}
]"#;
    let result = compress_if_beneficial("jq '.[] | select(.status==\"active\")' data.json", json);
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
fn verbatim_json_crush_gate_is_opt_in_and_lossless() {
    use super::super::engine::verbatim_json_crush;

    // Redundant array: constant role/status/region, only id/name vary — a
    // verbatim `gh api`-style payload the lossless crusher can halve.
    let mut json = String::from("[");
    for i in 0..24 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
                r#"{{"role":"member","status":"active","region":"eu-central-1","id":{i},"name":"user_{i}"}}"#
            ));
    }
    json.push(']');
    let original_tokens = 1_000; // any value above the floor

    // Off by default → never touched (output stays verbatim downstream).
    assert!(verbatim_json_crush(&json, original_tokens, 5, false).is_none());

    // Opt-in → reshaped into the crushed form (lossless body proven in the
    // `json_schema`/`json_crush` roundtrip tests) and clearly smaller.
    let crushed =
        verbatim_json_crush(&json, original_tokens, 5, true).expect("redundant json is crushed");
    assert!(crushed.contains("_lc_crush"), "expected crushed form");
    assert!(crushed.len() < json.len(), "crush must shrink the payload");

    // Low-redundancy JSON: nothing to factor → None even when enabled.
    let hetero = r#"[{"id":1,"k":"aaa"},{"id":2,"k":"bbb"},{"id":3,"k":"ccc"}]"#;
    assert!(verbatim_json_crush(hetero, original_tokens, 5, true).is_none());
}

#[test]
fn verbatim_json_crush_lossy_drops_noise_behind_recoverable_handle() {
    use super::super::engine::verbatim_json_crush_lossy;
    let _lock = crate::core::data_dir::test_env_lock();
    let _community = crate::core::policy::runtime::TestPolicyOverride::set(None);

    // A near-unique high-entropy column (`ts`) the lossless stage cannot
    // factor. The opt-in lossy stage drops it — but only behind a CCR handle
    // that still recovers the verbatim original (incl. the dropped column).
    let mut json = String::from("[");
    for i in 0..60 {
        if i > 0 {
            json.push(',');
        }
        json.push_str(&format!(
            r#"{{"status":"ok","ts":"2026-06-22T10:{i:02}:{i:02}.{i:09}Z","id":{i}}}"#
        ));
    }
    json.push(']');
    let original_tokens = 1_000;

    // Off by default → no lossy drop.
    assert!(verbatim_json_crush_lossy(&json, original_tokens, 5, false).is_none());

    let out = verbatim_json_crush_lossy(&json, original_tokens, 5, true)
        .expect("high-entropy json escalates to lossy");
    assert!(out.contains("_lc_crush"), "expected crushed form: {out}");
    assert!(
        out.contains("ctx_expand(id="),
        "lossy output must advertise a recovery handle: {out}"
    );

    // The handle resolves to the verbatim original, dropped column included —
    // the safety contract: lossy is never irrecoverable.
    let handle = crate::proxy::ccr::persist_json(&json).expect("same content -> same handle");
    let recovered = std::fs::read_to_string(&handle).expect("tee readable");
    assert!(
        recovered.contains("2026-06-22T10:42:42.000000042Z"),
        "dropped column must survive in the recoverable original"
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
