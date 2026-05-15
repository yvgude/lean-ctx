use super::passthrough::{BUILTIN_PASSTHROUGH, DEV_SCRIPT_KEYWORDS, SCRIPT_RUNNER_PREFIXES};

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

pub(in crate::shell) fn is_excluded_command(command: &str, excluded: &[String]) -> bool {
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

pub(super) fn is_search_output(command: &str) -> bool {
    let c = command.trim_start();
    c.starts_with("grep ")
        || c.starts_with("rg ")
        || c.starts_with("find ")
        || c.starts_with("fd ")
        || c.starts_with("ag ")
        || c.starts_with("ack ")
}

/// Returns true for commands whose output structure is critical for developer
/// readability. Pattern compression (light cleanup like removing `index` lines
/// or limiting context) still applies, but the terse pipeline and generic
/// compressors are skipped so diff hunks, blame annotations, etc. remain
/// fully readable.
pub fn has_structural_output(command: &str) -> bool {
    if is_verbatim_output(command) {
        return true;
    }
    if is_standalone_diff_command(command) {
        return true;
    }
    is_structural_git_command(command)
}

/// Returns true for commands where the output IS the purpose of the command.
/// These must never have their content transformed — only size-limited if huge.
/// Checks both the full command AND the last pipe segment for comprehensive coverage.
pub fn is_verbatim_output(command: &str) -> bool {
    is_verbatim_single(command) || is_verbatim_pipe_tail(command)
}

fn is_verbatim_single(command: &str) -> bool {
    is_http_client(command)
        || is_file_viewer(command)
        || is_data_format_tool(command)
        || is_binary_viewer(command)
        || is_infra_inspection(command)
        || is_crypto_command(command)
        || is_database_query(command)
        || is_dns_network_inspection(command)
        || is_language_one_liner(command)
        || is_container_listing(command)
        || is_file_listing(command)
        || is_system_query(command)
        || is_cloud_cli_query(command)
        || is_cli_api_data_command(command)
        || is_package_manager_info(command)
        || is_version_or_help(command)
        || is_config_viewer(command)
        || is_log_viewer(command)
        || is_archive_listing(command)
        || is_clipboard_tool(command)
        || is_git_data_command(command)
        || is_task_dry_run(command)
        || is_env_dump(command)
}

/// CLI tools that fetch or output raw API/structured data.
/// These MUST never be compressed -- compression destroys the payload.
fn is_cli_api_data_command(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();

    // gh (GitHub CLI) -- api, run view --log, search, release view, gist view
    if cl.starts_with("gh ")
        && (cl.starts_with("gh api ")
            || cl.starts_with("gh api\t")
            || cl.contains(" --json")
            || cl.contains(" --jq ")
            || cl.contains(" --template ")
            || (cl.contains("run view") && (cl.contains("--log") || cl.contains("log-failed")))
            || cl.starts_with("gh search ")
            || cl.starts_with("gh release view")
            || cl.starts_with("gh gist view")
            || cl.starts_with("gh gist list"))
    {
        return true;
    }

    // GitLab CLI (glab)
    if cl.starts_with("glab ") && cl.starts_with("glab api ") {
        return true;
    }

    // Jira CLI
    if cl.starts_with("jira ") && (cl.contains(" view") || cl.contains(" list")) {
        return true;
    }

    // Linear CLI
    if cl.starts_with("linear ") {
        return true;
    }

    // Stripe, Twilio, Vercel, Netlify, Fly, Railway, Supabase CLIs
    let first = first_binary(command);
    if matches!(
        first,
        "stripe" | "twilio" | "vercel" | "netlify" | "flyctl" | "fly" | "railway" | "supabase"
    ) && (cl.contains(" list")
        || cl.contains(" get")
        || cl.contains(" show")
        || cl.contains(" status")
        || cl.contains(" info")
        || cl.contains(" logs")
        || cl.contains(" inspect")
        || cl.contains(" export")
        || cl.contains(" describe"))
    {
        return true;
    }

    // Cloudflare (wrangler)
    if cl.starts_with("wrangler ")
        && !cl.starts_with("wrangler dev")
        && (cl.contains(" tail") || cl.contains(" secret list") || cl.contains(" kv "))
    {
        return true;
    }

    // Heroku
    if cl.starts_with("heroku ")
        && (cl.contains(" config")
            || cl.contains(" logs")
            || cl.contains(" ps")
            || cl.contains(" info"))
    {
        return true;
    }

    false
}

/// For piped commands like `kubectl get pods -o json | jq '.items[]'`,
/// check if the LAST command in the pipe is a verbatim tool.
fn is_verbatim_pipe_tail(command: &str) -> bool {
    if !command.contains('|') {
        return false;
    }
    let last_segment = command.rsplit('|').next().unwrap_or("").trim();
    if last_segment.is_empty() {
        return false;
    }
    is_verbatim_single(last_segment)
}

fn is_http_client(command: &str) -> bool {
    let first = first_binary(command);
    matches!(
        first,
        "curl" | "wget" | "http" | "https" | "xh" | "curlie" | "grpcurl" | "grpc_cli"
    )
}

fn is_file_viewer(command: &str) -> bool {
    let first = first_binary(command);
    match first {
        "cat" | "bat" | "batcat" | "pygmentize" | "highlight" => true,
        "head" | "tail" => !command.contains("-f") && !command.contains("--follow"),
        _ => false,
    }
}

fn is_data_format_tool(command: &str) -> bool {
    let first = first_binary(command);
    matches!(
        first,
        "jq" | "yq"
            | "xq"
            | "fx"
            | "gron"
            | "mlr"
            | "miller"
            | "dasel"
            | "csvlook"
            | "csvcut"
            | "csvgrep"
            | "csvjson"
            | "in2csv"
            | "sql2csv"
    )
}

fn is_binary_viewer(command: &str) -> bool {
    let first = first_binary(command);
    matches!(first, "xxd" | "hexdump" | "od" | "strings" | "file")
}

fn is_infra_inspection(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();
    if cl.starts_with("terraform output")
        || cl.starts_with("terraform show")
        || cl.starts_with("terraform state show")
        || cl.starts_with("terraform state list")
        || cl.starts_with("terraform state pull")
        || cl.starts_with("tofu output")
        || cl.starts_with("tofu show")
        || cl.starts_with("tofu state show")
        || cl.starts_with("tofu state list")
        || cl.starts_with("tofu state pull")
        || cl.starts_with("pulumi stack output")
        || cl.starts_with("pulumi stack export")
    {
        return true;
    }
    if cl.starts_with("docker inspect") || cl.starts_with("podman inspect") {
        return true;
    }
    if (cl.starts_with("kubectl get") || cl.starts_with("k get"))
        && (cl.contains("-o yaml")
            || cl.contains("-o json")
            || cl.contains("-oyaml")
            || cl.contains("-ojson")
            || cl.contains("--output yaml")
            || cl.contains("--output json")
            || cl.contains("--output=yaml")
            || cl.contains("--output=json"))
    {
        return true;
    }
    if cl.starts_with("kubectl describe") || cl.starts_with("k describe") {
        return true;
    }
    if cl.starts_with("helm get") || cl.starts_with("helm template") {
        return true;
    }
    false
}

fn is_crypto_command(command: &str) -> bool {
    let first = first_binary(command);
    if first == "openssl" {
        return true;
    }
    matches!(first, "gpg" | "age" | "ssh-keygen" | "certutil")
}

fn is_database_query(command: &str) -> bool {
    let cl = command.to_ascii_lowercase();
    if cl.starts_with("psql ") && (cl.contains(" -c ") || cl.contains("--command")) {
        return true;
    }
    if cl.starts_with("mysql ") && (cl.contains(" -e ") || cl.contains("--execute")) {
        return true;
    }
    if cl.starts_with("mariadb ") && (cl.contains(" -e ") || cl.contains("--execute")) {
        return true;
    }
    if cl.starts_with("sqlite3 ") && cl.contains('"') {
        return true;
    }
    if cl.starts_with("mongosh ") && cl.contains("--eval") {
        return true;
    }
    false
}

fn is_dns_network_inspection(command: &str) -> bool {
    let first = first_binary(command);
    matches!(
        first,
        "dig" | "nslookup" | "host" | "whois" | "drill" | "resolvectl"
    )
}

fn is_language_one_liner(command: &str) -> bool {
    let cl = command.to_ascii_lowercase();
    (cl.starts_with("python ") || cl.starts_with("python3 "))
        && (cl.contains(" -c ") || cl.contains(" -c\"") || cl.contains(" -c'"))
        || (cl.starts_with("node ") && (cl.contains(" -e ") || cl.contains(" --eval")))
        || (cl.starts_with("ruby ") && cl.contains(" -e "))
        || (cl.starts_with("perl ") && cl.contains(" -e "))
        || (cl.starts_with("php ") && cl.contains(" -r "))
}

fn is_container_listing(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();
    if cl.starts_with("docker ps") || cl.starts_with("docker images") {
        return true;
    }
    if cl.starts_with("podman ps") || cl.starts_with("podman images") {
        return true;
    }
    if cl.starts_with("kubectl get") || cl.starts_with("k get") {
        return true;
    }
    if cl.starts_with("helm list") || cl.starts_with("helm ls") {
        return true;
    }
    if cl.starts_with("docker compose ps") || cl.starts_with("docker-compose ps") {
        return true;
    }
    false
}

fn is_file_listing(command: &str) -> bool {
    let first = first_binary(command);
    matches!(
        first,
        "find" | "fd" | "fdfind" | "ls" | "exa" | "eza" | "lsd"
    )
}

fn is_system_query(command: &str) -> bool {
    let first = first_binary(command);
    matches!(
        first,
        "stat"
            | "wc"
            | "du"
            | "df"
            | "free"
            | "uname"
            | "id"
            | "whoami"
            | "hostname"
            | "uptime"
            | "lscpu"
            | "lsblk"
            | "ip"
            | "ifconfig"
            | "route"
            | "ss"
            | "netstat"
            | "base64"
            | "sha256sum"
            | "sha1sum"
            | "md5sum"
            | "cksum"
            | "readlink"
            | "realpath"
            | "which"
            | "type"
            | "command"
    )
}

fn is_cloud_cli_query(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();
    let cloud_query_verbs = [
        "describe",
        "get",
        "list",
        "show",
        "export",
        "inspect",
        "info",
        "status",
        "whoami",
        "caller-identity",
        "account",
    ];

    let is_aws = cl.starts_with("aws ") && !cl.starts_with("aws configure");
    let is_gcloud =
        cl.starts_with("gcloud ") && !cl.starts_with("gcloud auth") && !cl.contains(" deploy");
    let is_az = cl.starts_with("az ") && !cl.starts_with("az login");

    if !(is_aws || is_gcloud || is_az) {
        return false;
    }

    cloud_query_verbs
        .iter()
        .any(|verb| cl.contains(&format!(" {verb}")))
}

fn is_package_manager_info(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();

    if cl.starts_with("npm ") {
        return cl.starts_with("npm list")
            || cl.starts_with("npm ls")
            || cl.starts_with("npm info")
            || cl.starts_with("npm view")
            || cl.starts_with("npm show")
            || cl.starts_with("npm outdated")
            || cl.starts_with("npm audit");
    }
    if cl.starts_with("yarn ") {
        return cl.starts_with("yarn list")
            || cl.starts_with("yarn info")
            || cl.starts_with("yarn why")
            || cl.starts_with("yarn outdated")
            || cl.starts_with("yarn audit");
    }
    if cl.starts_with("pnpm ") {
        return cl.starts_with("pnpm list")
            || cl.starts_with("pnpm ls")
            || cl.starts_with("pnpm why")
            || cl.starts_with("pnpm outdated")
            || cl.starts_with("pnpm audit");
    }
    if cl.starts_with("pip ") || cl.starts_with("pip3 ") {
        return cl.contains(" list") || cl.contains(" show") || cl.contains(" freeze");
    }
    if cl.starts_with("gem ") {
        return cl.starts_with("gem list")
            || cl.starts_with("gem info")
            || cl.starts_with("gem specification");
    }
    if cl.starts_with("cargo ") {
        return cl.starts_with("cargo metadata")
            || cl.starts_with("cargo tree")
            || cl.starts_with("cargo pkgid");
    }
    if cl.starts_with("go ") {
        return cl.starts_with("go list") || cl.starts_with("go version");
    }
    if cl.starts_with("composer ") {
        return cl.starts_with("composer show")
            || cl.starts_with("composer info")
            || cl.starts_with("composer outdated");
    }
    if cl.starts_with("brew ") {
        return cl.starts_with("brew list")
            || cl.starts_with("brew info")
            || cl.starts_with("brew deps")
            || cl.starts_with("brew outdated");
    }
    if cl.starts_with("apt ") || cl.starts_with("dpkg ") {
        return cl.starts_with("apt list")
            || cl.starts_with("apt show")
            || cl.starts_with("dpkg -l")
            || cl.starts_with("dpkg --list")
            || cl.starts_with("dpkg -s");
    }
    false
}

fn is_version_or_help(command: &str) -> bool {
    let parts: Vec<&str> = command.split_whitespace().collect();
    if parts.len() < 2 || parts.len() > 3 {
        return false;
    }
    parts.iter().any(|p| {
        *p == "--version"
            || *p == "-V"
            || p.eq_ignore_ascii_case("version")
            || *p == "--help"
            || *p == "-h"
            || p.eq_ignore_ascii_case("help")
    })
}

fn is_config_viewer(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();
    if cl.starts_with("git config") && !cl.contains("--set") && !cl.contains("--unset") {
        return true;
    }
    if cl.starts_with("npm config list") || cl.starts_with("npm config get") {
        return true;
    }
    if cl.starts_with("yarn config") && !cl.contains(" set") {
        return true;
    }
    if cl.starts_with("pip config list") || cl.starts_with("pip3 config list") {
        return true;
    }
    if cl.starts_with("rustup show") || cl.starts_with("rustup target list") {
        return true;
    }
    if cl.starts_with("docker context ls") || cl.starts_with("docker context list") {
        return true;
    }
    if cl.starts_with("kubectl config")
        && (cl.contains("view") || cl.contains("get-contexts") || cl.contains("current-context"))
    {
        return true;
    }
    false
}

fn is_log_viewer(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();
    if cl.starts_with("journalctl") && !cl.contains("-f") && !cl.contains("--follow") {
        return true;
    }
    if cl.starts_with("dmesg") && !cl.contains("-w") && !cl.contains("--follow") {
        return true;
    }
    if cl.starts_with("docker logs") && !cl.contains("-f") && !cl.contains("--follow") {
        return true;
    }
    if cl.starts_with("kubectl logs") && !cl.contains("-f") && !cl.contains("--follow") {
        return true;
    }
    if cl.starts_with("docker compose logs") && !cl.contains("-f") && !cl.contains("--follow") {
        return true;
    }
    false
}

fn is_archive_listing(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();
    if cl.starts_with("tar ") && (cl.contains(" -tf") || cl.contains(" -t") || cl.contains(" tf")) {
        return true;
    }
    if cl.starts_with("unzip -l") || cl.starts_with("unzip -Z") {
        return true;
    }
    let first = first_binary(command);
    matches!(first, "zipinfo" | "lsar" | "7z" if cl.contains(" l ") || cl.contains(" l\t"))
        || first == "zipinfo"
        || first == "lsar"
}

fn is_clipboard_tool(command: &str) -> bool {
    let first = first_binary(command);
    if matches!(first, "pbpaste" | "wl-paste") {
        return true;
    }
    let cl = command.trim().to_ascii_lowercase();
    if cl.starts_with("xclip") && cl.contains("-o") {
        return true;
    }
    if cl.starts_with("xsel") && (cl.contains("-o") || cl.contains("--output")) {
        return true;
    }
    false
}

pub(super) fn is_git_data_command(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();
    if !cl.contains("git") {
        return false;
    }
    let exact_data_subs = [
        "remote",
        "rev-parse",
        "rev-list",
        "ls-files",
        "ls-tree",
        "ls-remote",
        "shortlog",
        "for-each-ref",
        "cat-file",
        "name-rev",
        "describe",
        "merge-base",
    ];

    let mut tokens = cl.split_whitespace();
    while let Some(tok) = tokens.next() {
        let base = tok.rsplit('/').next().unwrap_or(tok);
        if base != "git" {
            continue;
        }
        let mut skip_next = false;
        for arg in tokens.by_ref() {
            if skip_next {
                skip_next = false;
                continue;
            }
            if arg == "-c" || arg == "-C" || arg == "--git-dir" || arg == "--work-tree" {
                skip_next = true;
                continue;
            }
            if arg.starts_with('-') {
                continue;
            }
            return exact_data_subs.contains(&arg);
        }
        return false;
    }
    false
}

fn is_task_dry_run(command: &str) -> bool {
    let cl = command.trim().to_ascii_lowercase();
    if cl.starts_with("make ") && (cl.contains(" -n") || cl.contains(" --dry-run")) {
        return true;
    }
    if cl.starts_with("ansible") && (cl.contains("--check") || cl.contains("--diff")) {
        return true;
    }
    false
}

fn is_env_dump(command: &str) -> bool {
    let first = first_binary(command);
    matches!(first, "env" | "printenv" | "set" | "export" | "locale")
}

/// Extracts the binary name (basename, no path) from the first token of a command.
fn first_binary(command: &str) -> &str {
    let first = command.split_whitespace().next().unwrap_or("");
    first.rsplit('/').next().unwrap_or(first)
}

/// Non-git diff tools: `diff`, `colordiff`, `icdiff`, `delta`.
fn is_standalone_diff_command(command: &str) -> bool {
    let first = command.split_whitespace().next().unwrap_or("");
    let base = first.rsplit('/').next().unwrap_or(first);
    base.eq_ignore_ascii_case("diff")
        || base.eq_ignore_ascii_case("colordiff")
        || base.eq_ignore_ascii_case("icdiff")
        || base.eq_ignore_ascii_case("delta")
}

/// Git subcommands that produce structural output the developer must read verbatim.
fn is_structural_git_command(command: &str) -> bool {
    let mut tokens = command.split_whitespace();
    while let Some(tok) = tokens.next() {
        let base = tok.rsplit('/').next().unwrap_or(tok);
        if !base.eq_ignore_ascii_case("git") {
            continue;
        }
        let mut skip_next = false;
        let remaining: Vec<&str> = tokens.collect();
        for arg in &remaining {
            if skip_next {
                skip_next = false;
                continue;
            }
            if *arg == "-C" || *arg == "-c" || *arg == "--git-dir" || *arg == "--work-tree" {
                skip_next = true;
                continue;
            }
            if arg.starts_with('-') {
                continue;
            }
            let sub = arg.to_ascii_lowercase();
            return match sub.as_str() {
                "diff" | "show" | "blame" => true,
                "log" => has_patch_flag(&remaining) || has_stat_flag(&remaining),
                "stash" => remaining.iter().any(|a| a.eq_ignore_ascii_case("show")),
                _ => false,
            };
        }
        return false;
    }
    false
}

/// Returns true if the argument list contains `-p` or `--patch`.
fn has_patch_flag(args: &[&str]) -> bool {
    args.iter()
        .any(|a| *a == "-p" || *a == "--patch" || a.starts_with("-p"))
}

/// Returns true if the argument list contains `--stat`.
fn has_stat_flag(args: &[&str]) -> bool {
    args.iter()
        .any(|a| *a == "--stat" || a.starts_with("--stat="))
}
