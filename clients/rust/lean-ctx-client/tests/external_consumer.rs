//! Downstream compile/run boundary for the public OCLA client.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use lean_ctx_client::MAX_OCLA_WIRE_BYTES;
use serde_json::Value;

const CRATES_IO_SOURCE: &str = "registry+https://github.com/rust-lang/crates.io-index";

const CONSUMER_PACKAGE: &str = "lean-ctx-ocla-external-consumer";
const PUBLIC_CLIENT_PACKAGE: &str = "lean-ctx-client";
const EXPECTED_OUTPUT: &str =
    "ocla_api=ocla/v1;token_schema_version=1;agent_schema_version=1;gateway_admissible=true\n";

#[test]
fn standalone_ocla_consumer_builds_and_runs_offline() {
    let client_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let fixture_root = client_root.join("tests/external-consumer");
    let project_root = client_root.join("target/external-consumer/project");
    materialize_consumer(&fixture_root, &project_root);
    let consumer_manifest = project_root.join("Cargo.toml");
    let isolated_target = client_root.join("target/external-consumer/build");

    let metadata = cargo(
        client_root,
        &isolated_target,
        [
            "metadata",
            "--locked",
            "--offline",
            "--format-version",
            "1",
            "--manifest-path",
            utf8(&consumer_manifest),
        ],
    );
    assert_success("cargo metadata", &metadata);
    assert_public_dependency_boundary(&metadata.stdout, client_root);

    let output = cargo(
        client_root,
        &isolated_target,
        [
            "run",
            "--locked",
            "--offline",
            "--quiet",
            "--manifest-path",
            utf8(&consumer_manifest),
            "--",
            "tests/fixtures/canonical-token-envelope-v1.json",
            "tests/fixtures/agent-envelope-v1.json",
        ],
    );
    assert_success("external consumer", &output);
    assert_eq!(
        String::from_utf8(output.stdout).expect("UTF-8 stdout"),
        EXPECTED_OUTPUT
    );
    assert!(
        output.stderr.is_empty(),
        "external consumer emitted stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let oversized_wire = isolated_target.join("oversized-wire.json");
    fs::create_dir_all(&isolated_target).expect("create isolated target");
    fs::write(&oversized_wire, vec![b' '; MAX_OCLA_WIRE_BYTES + 1])
        .expect("write oversized wire fixture");
    let rejected = cargo(
        client_root,
        &isolated_target,
        [
            "run",
            "--locked",
            "--offline",
            "--quiet",
            "--manifest-path",
            utf8(&consumer_manifest),
            "--",
            utf8(&oversized_wire),
            "tests/fixtures/agent-envelope-v1.json",
        ],
    );
    assert!(
        !rejected.status.success(),
        "oversized wire must fail closed"
    );
    assert!(rejected.stdout.is_empty(), "rejected input emitted stdout");
    assert!(
        String::from_utf8_lossy(&rejected.stderr)
            .contains("OCLA wire document exceeds the public size bound"),
        "unexpected oversized-wire stderr: {}",
        String::from_utf8_lossy(&rejected.stderr)
    );
}

fn materialize_consumer(fixture_root: &Path, project_root: &Path) {
    if project_root.exists() {
        fs::remove_dir_all(project_root).expect("clear consumer project");
    }
    fs::create_dir_all(project_root.join("src")).expect("create consumer project");
    for (source, destination) in [
        ("Cargo.toml.in", "Cargo.toml"),
        ("Cargo.lock.fixture", "Cargo.lock"),
        ("main.rs.fixture", "src/main.rs"),
    ] {
        let bytes = fs::read(fixture_root.join(source)).expect("read consumer fixture");
        fs::write(project_root.join(destination), bytes).expect("materialize consumer fixture");
    }
}

fn cargo<'a>(
    working_directory: &Path,
    isolated_target: &Path,
    arguments: impl IntoIterator<Item = &'a str>,
) -> Output {
    Command::new(env!("CARGO"))
        .args(arguments)
        .current_dir(working_directory)
        .env("CARGO_TARGET_DIR", isolated_target)
        .output()
        .expect("cargo starts")
}

fn assert_success(label: &str, output: &Output) {
    assert!(
        output.status.success(),
        "{label} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn assert_public_dependency_boundary(metadata: &[u8], client_root: &Path) {
    let metadata: Value = serde_json::from_slice(metadata).expect("metadata JSON");
    let packages = metadata["packages"].as_array().expect("packages array");

    let consumer = packages
        .iter()
        .find(|package| package["name"] == CONSUMER_PACKAGE)
        .expect("external consumer package");
    let dependencies = consumer["dependencies"]
        .as_array()
        .expect("consumer dependencies");
    assert_eq!(
        dependencies.len(),
        1,
        "consumer must have one public dependency"
    );
    assert_eq!(dependencies[0]["name"], PUBLIC_CLIENT_PACKAGE);
    assert_eq!(
        canonical(Path::new(
            dependencies[0]["path"]
                .as_str()
                .expect("client path dependency")
        )),
        canonical(client_root)
    );

    for package in packages {
        let name = package["name"].as_str().expect("package name");
        let source = package["source"].as_str();
        if matches!(name, CONSUMER_PACKAGE | PUBLIC_CLIENT_PACKAGE) {
            assert!(
                source.is_none(),
                "local public package must be source-less: {name}"
            );
        } else {
            assert_eq!(
                source,
                Some(CRATES_IO_SOURCE),
                "non-public or local dependency in external consumer graph: {name}"
            );
        }
    }
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().expect("canonical path")
}

fn utf8(path: &Path) -> &str {
    path.to_str().expect("UTF-8 repository path")
}
