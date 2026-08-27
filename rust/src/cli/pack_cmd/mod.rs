mod checkpoint;
mod management;
mod package;
mod pr;
mod transfer;

#[allow(unreachable_pub, unused_imports)]
pub use management::*;
#[allow(unreachable_pub, unused_imports)]
pub use package::*;
#[allow(unreachable_pub, unused_imports)]
pub use pr::*;
#[allow(unreachable_pub, unused_imports)]
pub use transfer::*;

pub(crate) fn cmd_pack(args: &[String]) {
    let project_root = super::common::detect_project_root(args);

    let subcommand = args
        .iter()
        .find(|a| !a.starts_with("--"))
        .map_or("pr", String::as_str);

    match subcommand {
        "pr" => cmd_pack_pr(args, &project_root),
        "create" => cmd_pack_create(args, &project_root),
        "install" => cmd_pack_install(args, &project_root),
        "update" => super::pack_remote::cmd_pack_update(args, &project_root),
        "list" | "ls" => cmd_pack_list(),
        "info" => cmd_pack_info(args),
        "remove" | "rm" => cmd_pack_remove(args),
        "export" => cmd_pack_export(args),
        "import" => cmd_pack_import(args, &project_root),
        "verify" => cmd_pack_verify(args),
        "checkpoint-seal" => checkpoint::cmd_pack_checkpoint_seal(args),
        "checkpoint-inspect" => checkpoint::cmd_pack_checkpoint_inspect(args),
        "snapshot-v1-inspect" => checkpoint::cmd_pack_snapshot_v1_inspect(args),
        "auto-load" => cmd_pack_auto_load(args),
        "publish" => cmd_pack_publish(args),
        "send" => cmd_pack_send(args, &project_root),
        "receive" => cmd_pack_receive(args, &project_root),
        "help" | "--help" | "-h" => print_usage(),
        other => {
            eprintln!("Unknown pack subcommand: {other}");
            print_usage();
        }
    }
}

fn print_usage() {
    let ext = crate::core::contracts::PACKAGE_EXTENSION;
    eprintln!(
        "lean-ctx pack — Context Package Manager\n\n\
         SUBCOMMANDS:\n\
         \n\
         Create & Manage:\n\
         \x20 create   --name <name> [--version <v>] [--level 1|2|3] [--scope @ns] [--description <d>] [--author <a>] [--tags <t>] [--layers <l>]\n\
         \x20 create   --kind skills --name @ns/<name> --from <dir> --description <d>  Build a signed skills pack from a directory\n\
         \x20 list     List all installed packages\n\
         \x20 info     <name>[@version]  Show package details\n\
         \x20 remove   <name>[@version]  Remove a package\n\
         \n\
         Share & Distribute:\n\
         \x20 export   <name>[@version] [--output=<path>] [--sign] [--private] [--allow-secrets]  Export to .{ext} file (--sign: ed25519, required for publish; --private: hidden on the hosted registry; secret scan blocks credential-shaped content unless --allow-secrets)\n\
         \x20 import   <file.{ext}> [--apply]            Import from file\n\
         \x20 verify   <file.{ext}> [...]                Verify integrity + signature, no install (spec \u{a7}8/\u{a7}9; exit 1 on failure)\n\
         \x20 checkpoint-seal --checkpoint=<payload.json> --output=<file.{ext}> --name=<name> [--version=<v>] [--unsigned]\n\
         \x20 checkpoint-inspect <file.{ext}>             Verify and emit the open checkpoint envelope as bounded JSON\n\
         \x20 snapshot-v1-inspect <file.json>              Verify bounded signed SnapshotV1 migration input\n\
         \x20 install  <name>[@version] [--file=<path>]    Apply package to current project\n\
         \x20 install  <ns>/<name>[@version]              Install from the hosted registry\n\
         \x20                                             (ctxpkg.com; verifies sha256 + signature, pins in ctxpkg.lock,\n\
         \x20                                             resolves declared dependencies depth-1)\n\
         \x20 update   <ns>/<name>                        Refresh a hosted pack + its dependencies to the newest versions\n\
         \x20 publish  <file.{ext}> [--registry <url>] [--token <ctxp_…>]  Publish (signed, scoped @ns/name)\n\
         \n\
         A2A Transport:\n\
         \x20 send     <file.{ext}> [--target <url>] [--to <agent>] [--secret <key>]\n\
         \x20 receive  <envelope.json> [--secret <key>] [--apply]\n\
         \n\
         Automation:\n\
         \x20 auto-load [<name>[@version]] [--off]          Manage auto-load packages\n\
         \n\
         PR Pack:\n\
         \x20 pr       [--base <ref>] [--format json|markdown] [--depth <n>]  PR context pack\n\
         \n\
         CONFORMANCE LEVELS:\n\
         \x20 1 (Basic)     Flat nodes, no edges (any tool can implement)\n\
         \x20 2 (Graph)     Typed nodes + edges, dependency resolution, graph-merge\n\
         \x20 3 (Cognitive)  Activation energy, Hebbian weights, temporal decay\n\
         \n\
         EXAMPLES:\n\
         \x20 lean-ctx pack create --name rust-patterns --description \"Rust best practices\"\n\
         \x20 lean-ctx pack create --name auth-service --level 2 --scope @company\n\
         \x20 lean-ctx pack export rust-patterns --output=rust-patterns.{ext}\n\
         \x20 lean-ctx pack send rust-patterns.{ext} --target http://remote:3344\n\
         \x20 lean-ctx pack receive envelope.json --secret mykey --apply\n\
         \x20 lean-ctx pack list\n"
    );
}
