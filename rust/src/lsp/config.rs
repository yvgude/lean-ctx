use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServerConfig {
    pub command: String,
    pub args: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LspServerInfo {
    pub language: &'static str,
    pub binary: &'static str,
    pub install_hint: &'static str,
}

pub const KNOWN_SERVERS: &[LspServerInfo] = &[
    LspServerInfo {
        language: "rust",
        binary: "rust-analyzer",
        install_hint: "rustup component add rust-analyzer",
    },
    LspServerInfo {
        language: "typescript",
        binary: "typescript-language-server",
        // TypeScript ≥ 7 is its own language server; older versions need
        // typescript-language-server as well.
        install_hint: "npm install -g typescript (≤ 6: plus typescript-language-server)",
    },
    LspServerInfo {
        language: "python",
        binary: "pylsp",
        install_hint: "pip install python-lsp-server",
    },
    LspServerInfo {
        language: "go",
        binary: "gopls",
        install_hint: "go install golang.org/x/tools/gopls@latest",
    },
];

pub fn default_servers() -> HashMap<&'static str, LspServerConfig> {
    let mut m = HashMap::new();
    m.insert(
        "rust",
        LspServerConfig {
            command: "rust-analyzer".into(),
            args: vec![],
        },
    );
    m.insert(
        "typescript",
        LspServerConfig {
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
        },
    );
    m.insert(
        "javascript",
        LspServerConfig {
            command: "typescript-language-server".into(),
            args: vec!["--stdio".into()],
        },
    );
    m.insert(
        "python",
        LspServerConfig {
            command: "pylsp".into(),
            args: vec![],
        },
    );
    m.insert(
        "go",
        LspServerConfig {
            command: "gopls".into(),
            args: vec!["serve".into()],
        },
    );
    m
}

pub fn language_for_extension(ext: &str) -> Option<&'static str> {
    match ext {
        "rs" => Some("rust"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "py" | "pyi" => Some("python"),
        "go" => Some("go"),
        "java" => Some("java"),
        "kt" | "kts" => Some("kotlin"),
        "rb" => Some("ruby"),
        "c" | "h" => Some("c"),
        "cpp" | "cxx" | "cc" | "hpp" => Some("cpp"),
        "cs" => Some("csharp"),
        _ => None,
    }
}

pub fn find_binary_in_path(binary: &str) -> Option<PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    std::env::split_paths(&path_var).find_map(|dir| executable_in(&dir, binary))
}

/// `binary` in `dir`. On Windows, npm installs `.cmd` shims (next to an
/// extensionless shell script that Windows cannot run), so `.exe` and `.cmd`
/// come first; `std::process::Command` runs `.cmd` files through `cmd.exe`.
fn executable_in(dir: &Path, binary: &str) -> Option<PathBuf> {
    let names: &[String] = if cfg!(windows) {
        &[
            format!("{binary}.exe"),
            format!("{binary}.cmd"),
            binary.to_string(),
        ]
    } else {
        &[binary.to_string()]
    };
    names.iter().map(|n| dir.join(n)).find(|p| p.is_file())
}

/// Like [`find_binary_in_path`], but only returns servers that can actually
/// run. A rustup proxy (`~/.cargo/bin/rust-analyzer` → `rustup`) exists even
/// when the component is not installed and then fails on start; such a proxy
/// counts only if `--version` succeeds. Plain binaries are not executed.
pub fn find_runnable_server(binary: &str) -> Option<PathBuf> {
    find_binary_in_path(binary).and_then(runnable)
}

fn runnable(path: PathBuf) -> Option<PathBuf> {
    let is_rustup_proxy = std::fs::read_link(&path).is_ok_and(|target| {
        target
            .file_stem()
            .is_some_and(|stem| stem.eq_ignore_ascii_case("rustup"))
    });
    if !is_rustup_proxy {
        return Some(path);
    }
    // Never let the probe install a toolchain (a `rust-toolchain.toml` in the
    // working directory could otherwise trigger a download).
    std::process::Command::new(&path)
        .arg("--version")
        .env("RUSTUP_AUTO_INSTALL", "0")
        .current_dir(std::env::temp_dir())
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
        .then_some(path)
}

/// A language server lean-ctx can start, as resolved for one project.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedServer {
    /// The installed server binary (or npm shim) — what users know it by.
    pub binary: PathBuf,
    /// How to start it (may differ from `binary`: an npm shim on Windows runs through Node).
    pub config: LspServerConfig,
    /// Sent as `initializationOptions`.
    pub init_options: Option<serde_json::Value>,
}

impl ResolvedServer {
    /// Display name of the server binary (`gopls`, `tsc`, …), without a
    /// Windows extension.
    pub fn binary_name(&self) -> String {
        self.binary.file_stem().map_or_else(
            || self.binary.to_string_lossy().into_owned(),
            |n| n.to_string_lossy().into_owned(),
        )
    }

    /// Where the binary lives.
    pub fn binary_path(&self) -> &Path {
        &self.binary
    }

    fn new(binary: PathBuf, config: LspServerConfig) -> Self {
        Self {
            binary,
            config,
            init_options: None,
        }
    }
}

const TS_LANGUAGE_SERVER: &str = "typescript-language-server";

/// The server lean-ctx would start for `language` in `project_root`, or
/// `None` when none can run. `project_root = None` asks what this machine
/// provides (doctor). The command is the resolved path, so starting it does
/// not depend on `PATH` lookup rules.
pub fn resolve_server(language: &str, project_root: Option<&Path>) -> Option<ResolvedServer> {
    if matches!(language, "typescript" | "javascript") {
        return resolve_typescript(project_root, &find_binary_in_path);
    }
    let mut config = default_servers().remove(language)?;
    let binary = find_runnable_server(&config.command)?;
    config.command = binary.to_string_lossy().into_owned();
    Some(ResolvedServer::new(binary, config))
}

/// A server binary the user configured explicitly (`[lsp] <language> =
/// "<path>"`), with the arguments and options its kind needs.
pub fn configured_server(language: &str, command: &str, project_root: &Path) -> ResolvedServer {
    let binary = PathBuf::from(command);
    let stem = binary
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase());
    let plain = |args: &[&str]| LspServerConfig {
        command: command.to_string(),
        args: args.iter().map(|a| (*a).to_string()).collect(),
    };
    let (config, init_options) = match (language, stem.as_deref()) {
        ("typescript" | "javascript", Some("tsc")) => (
            npm_launch(
                &find_binary_in_path,
                &binary,
                "typescript",
                "tsc",
                &["--lsp", "--stdio"],
            )
            .unwrap_or_else(|| plain(&["--lsp", "--stdio"])),
            None,
        ),
        ("typescript" | "javascript", Some(TS_LANGUAGE_SERVER)) => (
            npm_launch(
                &find_binary_in_path,
                &binary,
                TS_LANGUAGE_SERVER,
                TS_LANGUAGE_SERVER,
                &["--stdio"],
            )
            .unwrap_or_else(|| plain(&["--stdio"])),
            project_typescript(project_root)
                .is_none()
                .then(|| tsserver_options(&binary))
                .flatten(),
        ),
        ("typescript" | "javascript", _) => (plain(&["--stdio"]), None),
        ("go", _) => (plain(&["serve"]), None),
        _ => (plain(&[]), None),
    };
    ResolvedServer {
        binary,
        config,
        init_options,
    }
}

/// TypeScript ≤ 6 ships `tsserver.js`, which `typescript-language-server`
/// drives. TypeScript ≥ 7 (the native port) has no `tsserver.js`; its `tsc`
/// is a language server itself (`tsc --lsp --stdio`). The project's own
/// TypeScript decides; without one, what is installed machine-wide. Versions
/// are read from `package.json` — nothing is executed to find out.
fn resolve_typescript(
    project_root: Option<&Path>,
    which: &dyn Fn(&str) -> Option<PathBuf>,
) -> Option<ResolvedServer> {
    let tls = |init_options| {
        let binary = which(TS_LANGUAGE_SERVER)?;
        Some(ResolvedServer {
            config: npm_launch(
                which,
                &binary,
                TS_LANGUAGE_SERVER,
                TS_LANGUAGE_SERVER,
                &["--stdio"],
            )?,
            binary,
            init_options,
        })
    };
    if let Some(node_modules) = project_root.and_then(project_typescript) {
        return if typescript_major(&node_modules.join("typescript"))? >= 7 {
            native_tsc(which, executable_in(&node_modules.join(".bin"), "tsc")?)
        } else {
            // It finds the workspace's TypeScript on its own.
            tls(None)
        };
    }
    // `npm install -g typescript-language-server typescript` (TypeScript ≤ 6):
    // the server does not look beside itself, so its `tsserver.js` is passed.
    if let Some(options) = which(TS_LANGUAGE_SERVER).and_then(|p| tsserver_options(&p)) {
        return tls(Some(options));
    }
    // `npm install -g typescript` (TypeScript ≥ 7).
    let tsc = which("tsc")?;
    if typescript_major(&npm_package_of(&tsc, "typescript")?)? >= 7 {
        native_tsc(which, tsc)
    } else {
        None
    }
}

fn native_tsc(which: &dyn Fn(&str) -> Option<PathBuf>, tsc: PathBuf) -> Option<ResolvedServer> {
    let config = npm_launch(which, &tsc, "typescript", "tsc", &["--lsp", "--stdio"])?;
    Some(ResolvedServer::new(tsc, config))
}

/// How to start the npm-installed binary `bin` (`bin_name` of `package`).
/// A Unix bin is executable itself. A Windows `.cmd`/`.bat` shim is not an
/// executable image, and `std` running batch files through `cmd.exe` is
/// documented as not to be relied upon — so, like the shim itself, Node
/// runs the package's own `bin` entry point.
fn npm_launch(
    which: &dyn Fn(&str) -> Option<PathBuf>,
    bin: &Path,
    package: &str,
    bin_name: &str,
    args: &[&str],
) -> Option<LspServerConfig> {
    let args = args.iter().map(|a| (*a).to_string());
    let is_shim = bin
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("cmd") || ext.eq_ignore_ascii_case("bat"));
    if !is_shim {
        return bin.is_file().then(|| LspServerConfig {
            command: bin.to_string_lossy().into_owned(),
            args: args.collect(),
        });
    }
    let entry = npm_bin_entry(&npm_package_of(bin, package)?, bin_name)?;
    let node = which("node")?;
    Some(LspServerConfig {
        command: node.to_string_lossy().into_owned(),
        args: std::iter::once(entry.to_string_lossy().into_owned())
            .chain(args)
            .collect(),
    })
}

/// The script `package.json`'s `bin` maps `bin_name` to (a string `bin` is
/// the package's only binary).
fn npm_bin_entry(package: &Path, bin_name: &str) -> Option<PathBuf> {
    let manifest = std::fs::read_to_string(package.join("package.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest).ok()?;
    let relative = match manifest.get("bin")? {
        serde_json::Value::String(only) => only.as_str(),
        bins => bins.get(bin_name)?.as_str()?,
    };
    let entry = package.join(relative);
    // Stays inside the package (a manifest is data, not a path authority).
    (entry.is_file()
        && !Path::new(relative).components().any(|c| {
            matches!(
                c,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        }))
    .then_some(entry)
}

/// The `node_modules` holding the project's own `typescript`, nearest first.
fn project_typescript(project_root: &Path) -> Option<PathBuf> {
    project_root
        .ancestors()
        .map(|dir| dir.join("node_modules"))
        .find(|nm| typescript_major(&nm.join("typescript")).is_some())
}

/// Major version of the `typescript` package in `package_dir`.
fn typescript_major(package_dir: &Path) -> Option<u32> {
    let manifest = std::fs::read_to_string(package_dir.join("package.json")).ok()?;
    let manifest: serde_json::Value = serde_json::from_str(&manifest).ok()?;
    if manifest.get("name")?.as_str()? != "typescript" {
        return None;
    }
    manifest
        .get("version")?
        .as_str()?
        .split('.')
        .next()?
        .parse()
        .ok()
}

/// The npm package directory `name` that the installed binary `bin` belongs
/// to. Unix: the bin is a symlink into `…/node_modules/<name>/`. Windows: a
/// `.cmd` shim (not a link) — globally `<prefix>/<bin>.cmd` beside
/// `<prefix>/node_modules/<name>`, locally `node_modules/.bin/<bin>.cmd`.
fn npm_package_of(bin: &Path, name: &str) -> Option<PathBuf> {
    let is_package = |dir: &Path| {
        dir.file_name().is_some_and(|n| n == name)
            && dir
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "node_modules")
    };
    if let Some(dir) = std::fs::canonicalize(bin).ok().and_then(|real| {
        real.ancestors()
            .find(|d| is_package(d))
            .map(Path::to_path_buf)
    }) {
        return Some(dir);
    }
    let dir = bin.parent()?;
    let shim_target = if dir.file_name().is_some_and(|n| n == ".bin") {
        dir.parent()?.join(name)
    } else {
        dir.join("node_modules").join(name)
    };
    shim_target
        .join("package.json")
        .is_file()
        .then_some(shim_target)
}

/// `initializationOptions` pointing `typescript-language-server` (installed
/// as `server`) at the `tsserver.js` installed with it: a nested dependency
/// or a sibling package.
fn tsserver_options(server: &Path) -> Option<serde_json::Value> {
    let package = npm_package_of(server, TS_LANGUAGE_SERVER)?;
    let tsserver = [
        package.join("node_modules"),
        package.parent()?.to_path_buf(),
    ]
    .into_iter()
    .map(|nm| nm.join("typescript/lib/tsserver.js"))
    .find(|p| p.is_file())?;
    Some(serde_json::json!({ "tsserver": { "path": tsserver } }))
}

pub fn install_hint_for_language(language: &str) -> &'static str {
    for info in KNOWN_SERVERS {
        if info.language == language {
            return info.install_hint;
        }
    }
    "No install instructions available for this language server."
}

pub fn binary_for_language(language: &str) -> Option<&'static str> {
    for info in KNOWN_SERVERS {
        if info.language == language {
            return Some(info.binary);
        }
    }
    None
}

/// [`resolve_server`] for `project_root`, or an actionable error.
pub fn check_server_available(
    language: &str,
    project_root: &Path,
) -> Result<ResolvedServer, String> {
    let servers = default_servers();
    let config = servers
        .get(language)
        .ok_or_else(|| format!("No LSP server configured for '{language}'"))?;

    resolve_server(language, Some(project_root)).ok_or_else(|| {
        let hint = install_hint_for_language(language);
        format!(
            "Language server '{}' not found in PATH (or not installed behind its rustup proxy).\n\
             \n\
             ctx_refactor requires an external language server for '{}' files.\n\
             Install it with:\n\
             \n\
             \x20   {}\n\
             \n\
             Then retry. This is optional — ctx_search and ctx_graph work without it.",
            config.command, language, hint
        )
    })
}

#[cfg(test)]
mod npm_shim_tests {
    use std::path::Path;

    fn write(path: &Path, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    /// npm's Windows install layout: `.cmd` shims (plain files, not links)
    /// beside `<prefix>/node_modules/<package>`. The package behind a shim is
    /// found, and — since a batch shim is no executable — Node starts the
    /// package's `bin` entry, for both TypeScript server kinds.
    #[test]
    fn windows_style_shims_start_the_package_entry_with_node() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path();
        let nm = prefix.join("node_modules");
        let node = prefix.join("node.exe");
        write(&node, "");
        write(&prefix.join("tsc.cmd"), "@node tsc %*");
        write(
            &nm.join("typescript/package.json"),
            r#"{"name":"typescript","version":"7.0.2","bin":{"tsc":"./bin/tsc"}}"#,
        );
        write(&nm.join("typescript/bin/tsc"), "");
        // Outside the prefix: its `node_modules` is no project TypeScript.
        let project_dir = tempfile::tempdir().unwrap();
        let project = project_dir.path();
        let which = |name: &str| {
            if name == "node" {
                return Some(node.clone());
            }
            let shim = prefix.join(format!("{name}.cmd"));
            shim.is_file().then_some(shim)
        };
        let entry = |pkg: &str, rel: &str| nm.join(pkg).join(rel).to_string_lossy().into_owned();

        let native = super::resolve_typescript(Some(project), &which).unwrap();
        assert_eq!(native.binary, prefix.join("tsc.cmd"));
        assert_eq!(native.binary_name(), "tsc");
        assert_eq!(Path::new(&native.config.command), node);
        assert_eq!(
            native.config.args,
            [
                entry("typescript", "./bin/tsc"),
                "--lsp".into(),
                "--stdio".into()
            ]
        );

        // TypeScript 5 beside typescript-language-server (string `bin`).
        write(
            &nm.join("typescript/package.json"),
            r#"{"name":"typescript","version":"5.9.3"}"#,
        );
        write(&nm.join("typescript/lib/tsserver.js"), "");
        write(&prefix.join("typescript-language-server.cmd"), "");
        write(
            &nm.join("typescript-language-server/package.json"),
            r#"{"bin":"lib/cli.mjs"}"#,
        );
        write(&nm.join("typescript-language-server/lib/cli.mjs"), "");
        let tls = super::resolve_typescript(Some(project), &which).unwrap();
        assert_eq!(tls.binary_name(), "typescript-language-server");
        assert_eq!(
            tls.config.args,
            [
                entry("typescript-language-server", "lib/cli.mjs"),
                "--stdio".into()
            ]
        );
        assert_eq!(
            tls.init_options.unwrap()["tsserver"]["path"],
            nm.join("typescript/lib/tsserver.js").to_str().unwrap()
        );

        // A manifest cannot point the entry outside its package.
        write(
            &nm.join("typescript-language-server/package.json"),
            r#"{"bin":"../typescript/lib/tsserver.js"}"#,
        );
        assert_eq!(super::resolve_typescript(Some(project), &which), None);
    }

    /// An explicitly configured server gets the arguments its kind needs.
    #[test]
    fn configured_servers_get_their_kind_specific_arguments() {
        let root = Path::new("/nonexistent-project");
        let args = |lang: &str, cmd: &str| super::configured_server(lang, cmd, root).config.args;
        assert_eq!(args("typescript", "/opt/ts/bin/tsc"), ["--lsp", "--stdio"]);
        assert_eq!(
            args("typescript", "/opt/typescript-language-server"),
            ["--stdio"]
        );
        assert_eq!(args("go", "/opt/gopls"), ["serve"]);
        assert!(args("rust", "/opt/rust-analyzer").is_empty());
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::runnable;
    use std::os::unix::fs::{PermissionsExt, symlink};

    /// A rustup proxy without the installed component must not count as an
    /// available server (doctor showed ✓ and ctx_refactor failed on start).
    #[test]
    fn rustup_proxy_counts_only_when_it_runs() {
        let dir = tempfile::tempdir().unwrap();
        let rustup = dir.path().join("rustup");
        let proxy = dir.path().join("fake-analyzer");
        symlink(&rustup, &proxy).unwrap();

        let mut verdicts = Vec::new();
        for exit_code in [1, 0] {
            std::fs::write(&rustup, format!("#!/bin/sh\nexit {exit_code}\n")).unwrap();
            std::fs::set_permissions(&rustup, std::fs::Permissions::from_mode(0o755)).unwrap();
            verdicts.push(runnable(proxy.clone()).is_some());
        }
        assert_eq!(verdicts, vec![false, true]);
    }

    /// The project's TypeScript picks the server; without one, the machine's:
    /// typescript-language-server with the `tsserver.js` beside it (≤ 6), or
    /// TypeScript 7's own `tsc --lsp`.
    #[test]
    fn typescript_server_follows_the_typescript_version() {
        use std::path::{Path, PathBuf};
        fn write(path: &Path, content: &str) {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, content).unwrap();
        }
        fn typescript(pkg: &Path, version: &str) {
            write(
                &pkg.join("package.json"),
                &format!(r#"{{"name":"typescript","version":"{version}"}}"#),
            );
        }
        let dir = tempfile::tempdir().unwrap();
        let (global, bin) = (dir.path().join("lib/node_modules"), dir.path().join("bin"));
        // `npm install -g typescript-language-server typescript@5`.
        write(&global.join("typescript-language-server/lib/cli.mjs"), "");
        typescript(&global.join("typescript"), "5.9.3");
        write(&global.join("typescript/lib/tsserver.js"), "");
        write(&global.join("typescript/bin/tsc"), "");
        std::fs::create_dir_all(&bin).unwrap();
        symlink(
            global.join("typescript-language-server/lib/cli.mjs"),
            bin.join("typescript-language-server"),
        )
        .unwrap();
        symlink(global.join("typescript/bin/tsc"), bin.join("tsc")).unwrap();
        let which = |name: &str| Some(bin.join(name)).filter(|p| p.exists());
        let resolve = |root: &Path| super::resolve_typescript(Some(root), &which);
        let command = |r: Option<super::ResolvedServer>| r.map(|r| r.binary_name());

        let bare = dir.path().join("bare");
        std::fs::create_dir_all(&bare).unwrap();
        let tls = resolve(&bare).unwrap();
        assert_eq!(tls.binary_name(), "typescript-language-server");
        assert_eq!(
            tls.init_options.unwrap()["tsserver"]["path"],
            std::fs::canonicalize(global.join("typescript/lib/tsserver.js"))
                .unwrap()
                .to_str()
                .unwrap()
        );

        // A TypeScript 7 project uses its own `tsc --lsp`, a TypeScript 5
        // project typescript-language-server (which finds that TypeScript).
        let ts7 = dir.path().join("ts7");
        typescript(&ts7.join("node_modules/typescript"), "7.0.2");
        write(&ts7.join("node_modules/.bin/tsc"), "");
        let native = resolve(&ts7.join("packages/app")).unwrap();
        assert_eq!(native.config.args, ["--lsp", "--stdio"]);
        assert!(Path::new(&native.config.command).starts_with(&ts7));
        let ts5 = dir.path().join("ts5");
        typescript(&ts5.join("node_modules/typescript"), "5.4.0");
        assert_eq!(resolve(&ts5).unwrap().init_options, None);

        // `npm install -g typescript@7` alone: the global `tsc` serves.
        std::fs::remove_file(bin.join("typescript-language-server")).unwrap();
        assert_eq!(command(resolve(&bare)), None, "tsc 5 is no server");
        typescript(&global.join("typescript"), "7.0.2");
        let global_native = resolve(&bare).unwrap();
        assert_eq!(
            PathBuf::from(&global_native.config.command),
            bin.join("tsc")
        );
    }
}
