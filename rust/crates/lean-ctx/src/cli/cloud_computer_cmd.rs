// SPDX-License-Identifier: Apache-2.0
//! `lean-ctx cloud computer …`: the LeanCTX computers of the person and
//! their teams (cloud computer spec, engine side).
//!
//! `list`, `create`, `start` and `stop` ask LeanCTX Cloud
//! ([`cloud`], `/v1/computers`); the cloud decides role, hours, balance and
//! trial. `add`, `show`, `remove`, `ssh-config`, `ssh` and `deploy-key`
//! work on a local registration: the computer's address and its pinned host
//! keys. Code only ever moves over SSH between this machine and the
//! computer, never through LeanCTX Cloud.

use serde::Serialize;

use lean_ctx_cloud::CloudError;

#[path = "cloud_computer_ssh.rs"]
pub(crate) mod ssh;

#[path = "cloud_computer_cloud.rs"]
pub(crate) mod cloud;

use ssh::{Computer, HostKey, Paths, Registry, Upsert};

const HELP: &str = "\
lean-ctx cloud computer — your LeanCTX cloud computer as an SSH remote host

USAGE:
  lean-ctx cloud computer list [--json]
  lean-ctx cloud computer create [--name <name>] [--role weave_host|general]
        [--team <team id> | --personal] [--request-id <id>] [--yes] [--json]
  lean-ctx cloud computer start <name|id> [--request-id <id>] [--yes] [--json]
  lean-ctx cloud computer stop <name|id> [--request-id <id>] [--json]
  lean-ctx cloud computer add <name> --host <host> [--user <lc-… user>] [--port 22]
        [--identity <private key file>] [--replace] [--json]
        (--host-key \"<type> <base64>\" … | --host-key-file <file> | --fingerprint SHA256:… …)
  lean-ctx cloud computer show <name> [--json]
  lean-ctx cloud computer remove <name>
  lean-ctx cloud computer ssh-config <name> [--write] [--yes]
  lean-ctx cloud computer ssh <name> [-- <command>…]
  lean-ctx cloud computer deploy-key <name>

list        The computers LeanCTX Cloud runs for you and your teams, with their
            state, role, price and what is left of the top-up; --json prints
            the Local Read document cloud.computers. The text list also shows
            the computers registered on this machine. Until LeanCTX Cloud
            offers computers, it says \"LeanCTX computers are not available yet.\"
create      Ask LeanCTX Cloud for a computer. On a machine connected to a
            team: name weave-host and role weave_host by default. On a machine
            connected to your Personal account (--personal): role general,
            and a name is needed; a Weave host is for teams only. LeanCTX
            Cloud picks the size and the region. A computer runs, and is
            billed, until you stop it (lean-ctx cloud computer stop <name>).
start, stop Start or stop a computer. create and start can cost money: they
            ask for you at a terminal (or LeanCTX Code) and are sent only with
            --yes. Run from a script or an agent without a terminal, they are
            refused before anything is sent. This guards against an agent
            starting a computer by accident; it is not a security boundary.
            Without --yes nothing is sent: the command shows who pays (billed
            to the top-up, or free hours where LeanCTX Cloud reports them, and
            the price, as LeanCTX Cloud last listed them) and exits 1
            (confirmation_required). When computers are not available yet, it
            says so before any cost is shown.
            stop always runs. --json prints cloud.computer.ack. LeanCTX Cloud
            decides who may do this, the role, the hours and the top-up.
add         Register a computer and pin its host keys. Give the public host keys
            (--host-key, repeatable, or --host-key-file with one key per line), or
            the fingerprints LeanCTX Cloud shows for it (--fingerprint, repeatable):
            the keys the computer offers are then fetched with ssh-keyscan and only
            keys with exactly those fingerprints are pinned. A computer already
            pinned to other keys, host or port needs --replace. Without --user,
            your own user on the computer (lc-…) is taken from LeanCTX Cloud.
show        A registered computer; --json prints the Local Read document
            cloud.computer (a local registration, live: false).
remove      Forget a computer and its pinned keys.
ssh-config  Print the SSH entry (Host leanctx-<name>, StrictHostKeyChecking yes,
            its own known_hosts, ForwardAgent no). --write installs the entries of
            all registered computers in ~/.ssh/leanctx/config and adds
            `Include ~/.ssh/leanctx/config` at the top of ~/.ssh/config, after a
            confirmation (--yes confirms). VS Code, Cursor and LeanCTX Code then
            reach the computer as host leanctx-<name>.
ssh         Open a shell on the computer, or run <command>, with the pinned host
            keys enforced even when ~/.ssh/config was not changed.
deploy-key  Create an ed25519 deploy key on the computer (once) and print its public
            half, to add as a deploy key on your forge. The private key never
            leaves the computer.

Code reaches the computer only over SSH or through your own forge; it never passes
through LeanCTX Cloud. Agent forwarding stays off.
";

/// Flags that take a value.
const VALUE_FLAGS: &[&str] = &[
    "--host",
    "--user",
    "--port",
    "--identity",
    "--host-key",
    "--host-key-file",
    "--fingerprint",
    "--name",
    "--role",
    "--team",
    "--request-id",
];
/// Flags without a value.
const BOOL_FLAGS: &[&str] = &[
    "--json",
    "--personal",
    "--replace",
    "--write",
    "--yes",
    "-y",
];

/// The parsed arguments of one `cloud computer` command.
#[derive(Debug, Default, PartialEq, Eq)]
struct Args {
    positionals: Vec<String>,
    values: Vec<(String, String)>,
    switches: Vec<String>,
    /// Everything after `--` (the remote command of `ssh`).
    command: Vec<String>,
}

impl Args {
    fn parse(raw: &[String]) -> Result<Self, String> {
        let mut out = Self::default();
        let (head, command) = match raw.iter().position(|a| a == "--") {
            Some(i) => (&raw[..i], raw.get(i + 1..).unwrap_or(&[])),
            None => (raw, &[][..]),
        };
        out.command = command.to_vec();
        let mut iter = head.iter();
        while let Some(arg) = iter.next() {
            if let Some((name, value)) = arg.split_once('=').filter(|(n, _)| n.starts_with("--")) {
                if !VALUE_FLAGS.contains(&name) {
                    return Err(format!("unknown option '{name}'"));
                }
                out.values.push((name.to_owned(), value.to_owned()));
            } else if VALUE_FLAGS.contains(&arg.as_str()) {
                let value = iter
                    .next()
                    .filter(|v| !v.starts_with("--"))
                    .ok_or_else(|| format!("{arg} needs a value"))?;
                out.values.push((arg.clone(), value.clone()));
            } else if BOOL_FLAGS.contains(&arg.as_str()) {
                out.switches.push(arg.clone());
            } else if arg.starts_with('-') {
                return Err(format!("unknown option '{arg}'"));
            } else {
                out.positionals.push(arg.clone());
            }
        }
        Ok(out)
    }

    fn value(&self, name: &str) -> Option<&str> {
        self.values
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    fn all(&self, name: &str) -> Vec<&str> {
        self.values
            .iter()
            .filter(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
            .collect()
    }

    fn has(&self, name: &str) -> bool {
        self.switches.iter().any(|s| s == name)
    }

    fn json(&self) -> bool {
        self.has("--json")
    }

    fn yes(&self) -> bool {
        self.has("--yes") || self.has("-y")
    }

    /// The computer name, the positional after the verb.
    fn name(&self) -> Result<&str, String> {
        let name = self
            .positionals
            .get(1)
            .ok_or_else(|| "name the computer".to_owned())?;
        ssh::check_name(name)?;
        Ok(name)
    }
}

/// A command's failure: the message and whether it is a bad argument
/// (exit 2) or a missing computer (`not_found` in JSON).
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    Invalid(String),
    NotFound(String),
    Failed(String),
}

impl From<String> for Failure {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl Failure {
    fn into_message(self) -> String {
        match self {
            Self::Invalid(m) | Self::NotFound(m) | Self::Failed(m) => m,
        }
    }

    fn local_read(self) -> super::super::local_read::Failure {
        use super::super::local_read::{Code, Failure as F};
        match self {
            Self::Invalid(m) => F::invalid(m),
            Self::NotFound(m) => F::new(Code::NotFound, m),
            Self::Failed(m) => F::new(Code::Internal, m),
        }
    }
}

/// `lean-ctx cloud computer …`.
pub(super) fn run(rest: &[String]) -> Result<(), CloudError> {
    let verb = rest.first().map_or("help", String::as_str);
    let head_end = rest.iter().position(|a| a == "--").unwrap_or(rest.len());
    if matches!(verb, "help" | "--help" | "-h")
        || rest[..head_end].iter().any(|a| a == "--help" || a == "-h")
    {
        print!("{HELP}");
        return Ok(());
    }
    let args = match Args::parse(rest) {
        Ok(args) => args,
        Err(message) => {
            return finish(
                rest.iter().any(|a| a == "--json"),
                Failure::Invalid(message),
            );
        }
    };
    if matches!(verb, "list" | "create" | "start" | "stop") {
        cloud::run(verb, &args);
        return Ok(());
    }
    let json = args.json();
    let result = paths().and_then(|paths| match verb {
        "add" => add(&paths, &args),
        "show" => show(&paths, &args),
        "remove" => remove(&paths, &args),
        "ssh-config" => ssh_config(&paths, &args),
        "ssh" => ssh_session(&paths, &args),
        "deploy-key" => deploy_key(&paths, &args),
        other => Err(Failure::Invalid(format!(
            "unknown subcommand '{other}' (list, create, start, stop, add, show, remove, ssh-config, ssh, deploy-key)"
        ))),
    });
    match result {
        Ok(()) => Ok(()),
        Err(failure) => finish(json, failure),
    }
}

/// `cloud computer list|create|start|stop` for the Local Service socket
/// (LeanCTX Code): `raw` as the route built it (`<verb> [<id>] --json …`).
pub(super) fn answer(
    raw: &[String],
    via: super::super::local_read::Via,
) -> super::super::local_read::Answer {
    match Args::parse(raw) {
        Ok(args) => cloud::answer(&args, via),
        Err(message) => {
            super::super::local_read::Answer::failure(&Failure::Invalid(message).local_read())
        }
    }
}

/// Report `failure` as an error document (`--json`) or on stderr, and exit.
fn finish(json: bool, failure: Failure) -> Result<(), CloudError> {
    if json {
        super::super::local_read::fail(&failure.local_read());
    }
    match failure {
        Failure::Invalid(message) => {
            eprintln!("cloud computer: {message}");
            std::process::exit(2);
        }
        Failure::NotFound(message) | Failure::Failed(message) => Err(CloudError::Local(message)),
    }
}

/// The SSH paths of this machine, for `lean-ctx run --cloud`.
pub(crate) fn ssh_paths() -> Result<Paths, String> {
    paths().map_err(Failure::into_message)
}

/// The host keys `hostname` offers (`ssh-keyscan`), for `lean-ctx run --cloud`.
pub(crate) fn keyscan_text(hostname: &str, port: u16) -> Result<String, String> {
    keyscan(hostname, port).map_err(Failure::into_message)
}

fn paths() -> Result<Paths, Failure> {
    let data_dir = lean_ctx::core::data_dir::lean_ctx_data_dir().map_err(Failure::Failed)?;
    let home = dirs::home_dir().ok_or_else(|| "no home directory".to_owned())?;
    Ok(Paths::new(&data_dir, &home))
}

fn registered<'a>(registry: &'a Registry, name: &str) -> Result<&'a Computer, Failure> {
    registry.get(name).ok_or_else(|| {
        Failure::NotFound(format!(
            "no computer '{name}' is registered (lean-ctx cloud computer add {name} …)"
        ))
    })
}

/// The SSH facts of one computer, as Local Read rows carry them.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct SshRow {
    host_alias: String,
    hostname: String,
    user: String,
    port: u16,
    host_key_fingerprints: Vec<String>,
    config_written: bool,
}

/// `cloud.computer` for a local registration. State, price, size and role
/// are the Cloud's answer (`cloud.computers`) and are not invented here.
#[derive(Debug, Serialize, PartialEq, Eq)]
struct Row {
    name: String,
    source: &'static str,
    ssh: SshRow,
}

fn row(computer: &Computer, config_written: bool) -> Row {
    Row {
        name: computer.name.clone(),
        source: "local",
        ssh: SshRow {
            host_alias: computer.alias(),
            hostname: computer.hostname.clone(),
            user: computer.user.clone(),
            port: computer.port,
            host_key_fingerprints: computer.fingerprints(),
            config_written,
        },
    }
}

#[derive(Debug, Serialize)]
struct ComputerDoc {
    available: bool,
    live: bool,
    as_of: Option<String>,
    computer: Row,
}

fn computer_doc(paths: &Paths, computer: &Computer) -> ComputerDoc {
    ComputerDoc {
        available: true,
        live: false,
        as_of: None,
        computer: row(computer, ssh::config_written(paths, &computer.name)),
    }
}

/// The computer `add` describes: its address and the host keys to pin.
/// Without `--user`, `cloud_user` gives the person's own user on the
/// computer (LeanCTX Cloud creates one `lc-…` user per person).
fn computer_from_args(
    args: &Args,
    name: &str,
    cloud_user: impl FnOnce() -> Result<String, String>,
) -> Result<Computer, Failure> {
    let hostname = args
        .value("--host")
        .ok_or_else(|| Failure::Invalid("--host is required".into()))?;
    ssh::check_hostname(hostname).map_err(Failure::Invalid)?;
    let user = match args.value("--user") {
        Some(user) => user.to_owned(),
        None => cloud_user().map_err(Failure::Invalid)?,
    };
    ssh::check_user(&user).map_err(Failure::Invalid)?;
    let port = match args.value("--port") {
        None => 22,
        Some(raw) => raw
            .parse::<u16>()
            .ok()
            .filter(|p| *p > 0)
            .ok_or_else(|| Failure::Invalid(format!("--port '{raw}' is not 1-65535")))?,
    };
    let identity_file = args.value("--identity").map(str::to_owned);
    if let Some(identity) = &identity_file {
        ssh::check_config_path(identity).map_err(Failure::Invalid)?;
    }
    let keys = pinned_keys(args, hostname, port)?;
    Ok(Computer {
        name: name.to_owned(),
        hostname: hostname.to_owned(),
        user,
        port,
        identity_file,
        host_keys: keys.iter().map(HostKey::openssh).collect(),
    })
}

/// The host keys of exactly one source: `--host-key`, `--host-key-file`
/// or `--fingerprint` (checked against `ssh-keyscan`).
fn pinned_keys(args: &Args, hostname: &str, port: u16) -> Result<Vec<HostKey>, Failure> {
    let given = args.all("--host-key");
    let file = args.value("--host-key-file");
    let fingerprints = args.all("--fingerprint");
    let sources = usize::from(!given.is_empty())
        + usize::from(file.is_some())
        + usize::from(!fingerprints.is_empty());
    if sources != 1 {
        return Err(Failure::Invalid(
            "pin the host keys with exactly one of --host-key, --host-key-file or --fingerprint"
                .into(),
        ));
    }
    let keys = if let Some(file) = file {
        let text = std::fs::read_to_string(file)
            .map_err(|e| Failure::Invalid(format!("cannot read {file}: {e}")))?;
        parse_key_lines(text.lines())?
    } else if given.is_empty() {
        let wanted: Vec<String> = fingerprints.iter().map(|f| (*f).to_owned()).collect();
        for fingerprint in &wanted {
            ssh::check_fingerprint(fingerprint).map_err(Failure::Invalid)?;
        }
        ssh::keys_matching(&keyscan(hostname, port)?, &wanted)?
    } else {
        parse_key_lines(given.into_iter())?
    };
    let mut unique: Vec<HostKey> = Vec::new();
    for key in keys {
        if !unique.contains(&key) {
            unique.push(key);
        }
    }
    if unique.is_empty() {
        return Err(Failure::Invalid("no host key to pin".into()));
    }
    Ok(unique)
}

fn parse_key_lines<'a>(lines: impl Iterator<Item = &'a str>) -> Result<Vec<HostKey>, Failure> {
    lines
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| HostKey::parse(l).map_err(Failure::Invalid))
        .collect()
}

/// The host keys `hostname` offers, as `ssh-keyscan` prints them.
fn keyscan(hostname: &str, port: u16) -> Result<String, Failure> {
    let output = std::process::Command::new("ssh-keyscan")
        .args(["-T", "10", "-p", &port.to_string(), hostname])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| format!("cannot run ssh-keyscan: {e}"))?;
    if output.stdout.is_empty() {
        return Err(Failure::Failed(format!(
            "{hostname}:{port} offered no host key (is the computer running?)"
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn add(paths: &Paths, args: &Args) -> Result<(), Failure> {
    let name = args.name().map_err(Failure::Invalid)?;
    let computer = computer_from_args(args, name, || cloud::ssh_user(name))?;
    let mut registry = Registry::load(&paths.registry)?;
    let change = registry
        .upsert(computer.clone(), args.has("--replace"))
        .map_err(Failure::Invalid)?;
    if change != Upsert::Unchanged {
        registry.save(&paths.registry)?;
    }
    ssh::refresh(paths, &registry)?;
    if args.json() {
        super::super::local_read::emit("cloud.computer", || Ok(computer_doc(paths, &computer)));
        return Ok(());
    }
    let verb = match change {
        Upsert::Added => "pinned",
        Upsert::Updated => "re-pinned",
        Upsert::Unchanged => "already pinned",
    };
    println!(
        "{verb} computer '{name}' ({} → {}@{}:{})",
        computer.alias(),
        computer.user,
        computer.hostname,
        computer.port
    );
    for fingerprint in computer.fingerprints() {
        println!("  {fingerprint}");
    }
    if !ssh::config_written(paths, name) {
        println!("next: lean-ctx cloud computer ssh-config {name} --write");
    }
    Ok(())
}

/// The computers registered on this machine, for the text list.
fn print_local_registrations() {
    let Ok(paths) = paths() else { return };
    let Ok(registry) = Registry::load(&paths.registry) else {
        return;
    };
    if registry.computers.is_empty() {
        return;
    }
    println!("registered on this machine:");
    for computer in &registry.computers {
        let written = if ssh::config_written(&paths, &computer.name) {
            ""
        } else {
            "  (ssh-config not written)"
        };
        println!(
            "  {:<16} {}@{}:{}  {} host key(s){written}",
            computer.alias(),
            computer.user,
            computer.hostname,
            computer.port,
            computer.host_keys.len()
        );
    }
}

fn show(paths: &Paths, args: &Args) -> Result<(), Failure> {
    let name = args.name().map_err(Failure::Invalid)?;
    let registry = Registry::load(&paths.registry)?;
    let computer = registered(&registry, name)?;
    if args.json() {
        super::super::local_read::emit("cloud.computer", || Ok(computer_doc(paths, computer)));
        return Ok(());
    }
    println!("{}", computer.alias());
    println!(
        "  host      {}@{}:{}",
        computer.user, computer.hostname, computer.port
    );
    if let Some(identity) = &computer.identity_file {
        println!("  identity  {identity}");
    }
    for fingerprint in computer.fingerprints() {
        println!("  host key  {fingerprint}");
    }
    println!(
        "  ssh-config {}",
        if ssh::config_written(paths, name) {
            "written"
        } else {
            "not written"
        }
    );
    Ok(())
}

fn remove(paths: &Paths, args: &Args) -> Result<(), Failure> {
    let name = args.name().map_err(Failure::Invalid)?;
    let mut registry = Registry::load(&paths.registry)?;
    if !registry.remove(name) {
        return Err(Failure::NotFound(format!(
            "no computer '{name}' is registered"
        )));
    }
    registry.save(&paths.registry)?;
    ssh::refresh(paths, &registry)?;
    println!("removed computer '{name}' and its pinned host keys");
    Ok(())
}

fn ssh_config(paths: &Paths, args: &Args) -> Result<(), Failure> {
    let name = args.name().map_err(Failure::Invalid)?;
    let registry = Registry::load(&paths.registry)?;
    let computer = registered(&registry, name)?;
    print!("{}", ssh::render_host_block(computer, &paths.known_hosts));
    if !args.has("--write") {
        return Ok(());
    }
    let prompt = format!(
        "Write {} and {}, and add `{}` at the top of {}?",
        paths.leanctx_config.display(),
        paths.known_hosts.display(),
        ssh::INCLUDE_LINE,
        paths.ssh_config.display()
    );
    if !super::super::prompt::confirm(&prompt, args.yes()) {
        return Err(Failure::Failed("nothing was written".into()));
    }
    ssh::write_ssh_config(paths, &registry)?;
    println!("written: ssh {} reaches the computer", computer.alias());
    Ok(())
}

/// `ssh` with the pin enforced; exits with ssh's status.
fn ssh_session(paths: &Paths, args: &Args) -> Result<(), Failure> {
    let name = args.name().map_err(Failure::Invalid)?;
    let registry = Registry::load(&paths.registry)?;
    let computer = registered(&registry, name)?;
    ssh::write_known_hosts(paths, &registry)?;
    let status = std::process::Command::new("ssh")
        .args(ssh::ssh_args(computer, &paths.known_hosts, &args.command))
        .status()
        .map_err(|e| format!("cannot run ssh: {e}"))?;
    std::process::exit(status.code().unwrap_or(255));
}

fn deploy_key(paths: &Paths, args: &Args) -> Result<(), Failure> {
    let name = args.name().map_err(Failure::Invalid)?;
    let registry = Registry::load(&paths.registry)?;
    let computer = registered(&registry, name)?;
    ssh::write_known_hosts(paths, &registry)?;
    let command = vec![ssh::deploy_key_command(name)];
    let output = std::process::Command::new("ssh")
        .args(ssh::ssh_args(computer, &paths.known_hosts, &command))
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::inherit())
        .output()
        .map_err(|e| format!("cannot run ssh: {e}"))?;
    if !output.status.success() {
        return Err(Failure::Failed(format!(
            "the computer did not create the deploy key (ssh exit {})",
            output.status.code().unwrap_or(255)
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout
        .lines()
        .map(str::trim)
        .rfind(|l| !l.is_empty())
        .ok_or_else(|| "the computer printed no public key".to_owned())?;
    let key =
        HostKey::parse(line).map_err(|e| format!("the computer printed no public key: {e}"))?;
    println!("{} leanctx-deploy@{name}", key.openssh());
    eprintln!(
        "fingerprint {}; add it as a deploy key of the repository on your forge",
        key.fingerprint()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|a| (*a).to_owned()).collect()
    }

    #[test]
    fn args_split_flags_positionals_and_the_remote_command() {
        let args = Args::parse(&strings(&[
            "ssh",
            "dev",
            "--port=2222",
            "--json",
            "--",
            "ls",
            "--help",
        ]))
        .expect("parses");
        assert_eq!(args.positionals, ["ssh", "dev"]);
        assert_eq!(args.value("--port"), Some("2222"));
        assert!(args.json());
        assert_eq!(args.command, ["ls", "--help"]);
    }

    #[test]
    fn args_collect_repeated_values_and_refuse_unknown_options() {
        let args = Args::parse(&strings(&[
            "add",
            "dev",
            "--fingerprint",
            "SHA256:a",
            "--fingerprint",
            "SHA256:b",
        ]))
        .expect("parses");
        assert_eq!(args.all("--fingerprint"), ["SHA256:a", "SHA256:b"]);
        assert!(Args::parse(&strings(&["add", "--bogus"])).is_err());
        assert!(Args::parse(&strings(&["add", "dev", "--host"])).is_err());
        assert!(Args::parse(&strings(&["add", "dev", "--host", "--json"])).is_err());
    }

    #[test]
    fn the_name_is_checked() {
        let args = Args::parse(&strings(&["show", "Dev!"])).expect("parses");
        assert!(args.name().is_err());
        let args = Args::parse(&strings(&["show"])).expect("parses");
        assert!(args.name().is_err());
    }

    #[allow(clippy::unnecessary_wraps)]
    fn lc_user() -> Result<String, String> {
        Ok("lc-6f0b7c3e".into())
    }

    const KEY: &str =
        "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMSnzHDyEYoG7MTo4f10p0byopnf2iL0tUKUh9vd2zYW";

    #[test]
    fn add_pins_exactly_one_key_source() {
        let both = Args::parse(&strings(&[
            "add",
            "dev",
            "--host",
            "203.0.113.7",
            "--host-key",
            KEY,
            "--fingerprint",
            "SHA256:0ffjc0LKN6E6K2TAoU7JnGFm66vaaBps0il/sCsWtOs",
        ]))
        .expect("parses");
        assert!(matches!(
            computer_from_args(&both, "dev", lc_user),
            Err(Failure::Invalid(_))
        ));
        let none = Args::parse(&strings(&["add", "dev", "--host", "203.0.113.7"])).expect("parses");
        assert!(matches!(
            computer_from_args(&none, "dev", lc_user),
            Err(Failure::Invalid(_))
        ));
    }

    #[test]
    fn add_builds_the_computer_from_given_keys() {
        let args = Args::parse(&strings(&[
            "add",
            "dev",
            "--host",
            "dev.example.net",
            "--port",
            "2222",
            "--host-key",
            &format!("{KEY} root@dev"),
            "--host-key",
            KEY,
        ]))
        .expect("parses");
        let computer = computer_from_args(&args, "dev", lc_user).expect("valid");
        assert_eq!(computer.user, "lc-6f0b7c3e", "the cloud's per-person user");
        assert_eq!(computer.port, 2222);
        assert_eq!(computer.host_keys, [KEY]);
        assert_eq!(
            computer.fingerprints(),
            ["SHA256:0ffjc0LKN6E6K2TAoU7JnGFm66vaaBps0il/sCsWtOs"]
        );
        let bad_port = Args::parse(&strings(&[
            "add",
            "dev",
            "--host",
            "h",
            "--port",
            "0",
            "--host-key",
            KEY,
        ]))
        .expect("parses");
        assert!(computer_from_args(&bad_port, "dev", lc_user).is_err());
        let no_user = Args::parse(&strings(&["add", "dev", "--host", "h", "--host-key", KEY]))
            .expect("parses");
        assert!(matches!(
            computer_from_args(&no_user, "dev", || Err("give --user".into())),
            Err(Failure::Invalid(_))
        ));
        let own = Args::parse(&strings(&[
            "add",
            "dev",
            "--host",
            "h",
            "--user",
            "lc-0a1b2c3d",
            "--host-key",
            KEY,
        ]))
        .expect("parses");
        let computer = computer_from_args(&own, "dev", || panic!("not asked")).expect("valid");
        assert_eq!(computer.user, "lc-0a1b2c3d");
    }

    #[test]
    fn rows_carry_only_local_facts() {
        let computer = Computer {
            name: "dev".into(),
            hostname: "203.0.113.7".into(),
            user: "lc-6f0b7c3e".into(),
            port: 22,
            identity_file: Some("/home/p/.ssh/id_ed25519".into()),
            host_keys: vec![KEY.into()],
        };
        let value = serde_json::to_value(row(&computer, true)).expect("serializes");
        assert_eq!(value["source"], "local");
        assert_eq!(value["ssh"]["host_alias"], "leanctx-dev");
        assert_eq!(value["ssh"]["config_written"], true);
        assert_eq!(
            value["ssh"]["host_key_fingerprints"][0],
            "SHA256:0ffjc0LKN6E6K2TAoU7JnGFm66vaaBps0il/sCsWtOs"
        );
        assert!(value.get("state").is_none(), "no invented cloud state");
        assert!(value["ssh"].get("identity_file").is_none());
    }
}
