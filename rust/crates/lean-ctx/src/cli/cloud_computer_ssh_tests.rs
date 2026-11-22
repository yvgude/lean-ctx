// SPDX-License-Identifier: Apache-2.0
use super::*;

/// Generated with `ssh-keygen -t ed25519`; `ssh-keygen -l` prints the
/// fingerprint below.
const KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMSnzHDyEYoG7MTo4f10p0byopnf2iL0tUKUh9vd2zYW";
const FINGERPRINT: &str = "SHA256:0ffjc0LKN6E6K2TAoU7JnGFm66vaaBps0il/sCsWtOs";

fn computer(name: &str) -> Computer {
    Computer {
        name: name.into(),
        hostname: "203.0.113.7".into(),
        user: "lc-6f0b7c3e".into(),
        port: 22,
        identity_file: None,
        host_keys: vec![KEY.into()],
    }
}

#[test]
fn fingerprints_match_ssh_keygen() {
    let key = HostKey::parse(&format!("{KEY} comment here")).expect("valid key");
    assert_eq!(key.fingerprint(), FINGERPRINT);
    assert_eq!(key.openssh(), KEY);
}

#[test]
fn host_keys_are_checked() {
    assert!(HostKey::parse("ssh-ed25519").is_err());
    assert!(HostKey::parse("ssh-dss AAAAB3NzaC1kc3M=").is_err());
    assert!(HostKey::parse("ssh-ed25519 !!!notbase64").is_err());
    // An ed25519 blob claimed to be RSA.
    let blob = KEY.split_whitespace().nth(1).expect("blob");
    assert!(HostKey::parse(&format!("ssh-rsa {blob}")).is_err());
    assert!(HostKey::parse("ssh-ed25519 AAAA").is_err());
}

#[test]
fn fingerprint_syntax_is_checked() {
    assert!(check_fingerprint(FINGERPRINT).is_ok());
    assert!(check_fingerprint("MD5:aa:bb").is_err());
    assert!(check_fingerprint("SHA256:short").is_err());
}

#[test]
fn keyscan_pins_only_the_announced_fingerprints() {
    let other = "ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBFakeKeyThatIsNotRealButHasAValidPrefixAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
    let scan =
        format!("# 203.0.113.7:22 SSH-2.0-OpenSSH_9.6\n203.0.113.7 {KEY}\n203.0.113.7 {other}\n");
    let keys = keys_matching(&scan, &[FINGERPRINT.to_owned()]).expect("matches");
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].openssh(), KEY);
}

#[test]
fn keyscan_refuses_when_an_announced_key_is_missing() {
    let scan = format!("203.0.113.7 {KEY}\n");
    let missing = "SHA256:AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA".to_owned();
    let error = keys_matching(&scan, &[FINGERPRINT.to_owned(), missing.clone()])
        .expect_err("one fingerprint is not offered");
    assert!(error.contains(&missing));
    assert!(keys_matching("", &[FINGERPRINT.to_owned()]).is_err());
}

#[test]
fn names_hosts_users_and_paths_are_checked() {
    assert!(check_name("dev-1").is_ok());
    assert!(check_name(&"x".repeat(31)).is_ok(), "the cloud allows 31");
    for bad in ["", "-dev", "Dev", "dev box", "a/b", "x".repeat(32).as_str()] {
        assert!(check_name(bad).is_err(), "{bad}");
    }
    for good in ["203.0.113.7", "dev.example.net", "2001:db8::1"] {
        assert!(check_hostname(good).is_ok(), "{good}");
    }
    for bad in ["-oProxyCommand=x", "host name", "h*", "user@host", ""] {
        assert!(check_hostname(bad).is_err(), "{bad}");
    }
    assert!(check_user("lc-6f0b7c3e").is_ok());
    assert!(check_user("Root").is_err());
    assert!(check_user("-x").is_err());
    assert!(check_config_path("/home/p/.ssh/id ed25519").is_ok());
    assert!(check_config_path("/a\"b").is_err());
    assert!(check_config_path("/a\nHost *").is_err());
}

#[test]
fn a_changed_pin_needs_replace() {
    let mut registry = Registry::default();
    assert_eq!(registry.upsert(computer("dev"), false), Ok(Upsert::Added));
    assert_eq!(
        registry.upsert(computer("dev"), false),
        Ok(Upsert::Unchanged)
    );
    let mut moved = computer("dev");
    moved.hostname = "198.51.100.9".into();
    assert!(registry.upsert(moved.clone(), false).is_err());
    assert_eq!(
        registry.get("dev").map(|c| c.hostname.as_str()),
        Some("203.0.113.7")
    );
    assert_eq!(registry.upsert(moved, true), Ok(Upsert::Updated));
    let mut identity = computer("dev");
    identity.hostname = "198.51.100.9".into();
    identity.identity_file = Some("/k".into());
    assert_eq!(registry.upsert(identity, false), Ok(Upsert::Updated));
}

#[test]
fn the_registry_round_trips_and_stays_sorted() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cloud").join("computers.json");
    assert_eq!(Registry::load(&path), Ok(Registry::default()));
    let mut registry = Registry::default();
    registry.upsert(computer("web"), false).expect("added");
    registry.upsert(computer("dev"), false).expect("added");
    registry.save(&path).expect("saved");
    let loaded = Registry::load(&path).expect("loads");
    assert_eq!(loaded, registry);
    assert_eq!(loaded.computers[0].name, "dev");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    assert!(registry.remove("web"));
    assert!(!registry.remove("web"));
    std::fs::write(&path, "{\"schema\":\"other\",\"computers\":[]}").expect("write");
    assert!(Registry::load(&path).is_err());
}

#[test]
fn the_host_block_pins_the_key_and_disables_forwarding() {
    let mut c = computer("dev");
    c.identity_file = Some("/home/p/.ssh/id_ed25519".into());
    let block = render_host_block(&c, Path::new("/home/p/.ssh/leanctx/known_hosts"));
    assert_eq!(
        block,
        "Host leanctx-dev\n  HostName 203.0.113.7\n  User lc-6f0b7c3e\n  Port 22\n  \
         IdentityFile \"/home/p/.ssh/id_ed25519\"\n  IdentitiesOnly yes\n  \
         StrictHostKeyChecking yes\n  UserKnownHostsFile \"/home/p/.ssh/leanctx/known_hosts\"\n  \
         HostKeyAlias leanctx-dev\n  ForwardAgent no\n"
    );
    let without = render_host_block(&computer("dev"), Path::new("/kh"));
    assert!(!without.contains("IdentityFile"));
    assert!(!without.contains("IdentitiesOnly"));
}

#[test]
fn known_hosts_are_keyed_by_the_alias() {
    let mut registry = Registry::default();
    registry.upsert(computer("dev"), false).expect("added");
    let text = render_known_hosts(&registry);
    assert!(text.lines().any(|l| l == format!("leanctx-dev {KEY}")));
    assert!(!text.contains("203.0.113.7"));
}

#[test]
fn the_include_goes_first_and_only_once() {
    assert_eq!(with_include(""), Some(format!("{INCLUDE_LINE}\n")));
    let existing = "Host github.com\n  User git\n";
    let updated = with_include(existing).expect("added");
    assert!(updated.starts_with(INCLUDE_LINE));
    assert!(updated.ends_with(existing));
    assert_eq!(with_include(&updated), None);
    // An Include below a Host line is scoped to that host: not ours.
    let scoped = format!("Host x\n  {INCLUDE_LINE}\n");
    assert!(!has_include(&scoped));
}

#[test]
fn writing_the_config_installs_every_piece() {
    let home = tempfile::tempdir().expect("home");
    let data = tempfile::tempdir().expect("data");
    let paths = Paths::new(data.path(), home.path());
    std::fs::create_dir_all(home.path().join(".ssh")).expect("ssh dir");
    std::fs::write(&paths.ssh_config, "Host github.com\n  User git\n").expect("config");
    let mut registry = Registry::default();
    registry.upsert(computer("dev"), false).expect("added");

    refresh(&paths, &registry).expect("refresh");
    assert!(paths.known_hosts.exists());
    assert!(
        !paths.leanctx_config.exists(),
        "refresh never installs the config"
    );
    assert!(!config_written(&paths, "dev"));

    write_ssh_config(&paths, &registry).expect("written");
    assert!(config_written(&paths, "dev"));
    let main = std::fs::read_to_string(&paths.ssh_config).expect("main");
    assert!(main.starts_with(INCLUDE_LINE));
    assert!(main.contains("Host github.com"));
    write_ssh_config(&paths, &registry).expect("again");
    let again = std::fs::read_to_string(&paths.ssh_config).expect("main");
    assert_eq!(again.matches(INCLUDE_LINE).count(), 1);

    registry.remove("dev");
    refresh(&paths, &registry).expect("refresh");
    assert!(!config_written(&paths, "dev"));
    let known = std::fs::read_to_string(&paths.known_hosts).expect("known");
    assert!(!known.contains("leanctx-dev"));
}

#[test]
fn config_written_matches_host_and_user_not_only_the_name() {
    let home = tempfile::tempdir().expect("home");
    let data = tempfile::tempdir().expect("data");
    let paths = Paths::new(data.path(), home.path());
    let mut registry = Registry::default();
    registry.upsert(computer("dev"), false).expect("added");
    registry.save(&paths.registry).expect("saved");
    write_ssh_config(&paths, &registry).expect("written");
    assert!(config_written_to(
        &paths,
        "dev",
        "203.0.113.7",
        "lc-6f0b7c3e",
        Some(22)
    ));
    assert!(config_written_to(
        &paths,
        "dev",
        "203.0.113.7",
        "lc-6f0b7c3e",
        None
    ));
    assert!(!config_written_to(
        &paths,
        "dev",
        "198.51.100.9",
        "lc-6f0b7c3e",
        None
    ));
    assert!(!config_written_to(
        &paths,
        "dev",
        "203.0.113.7",
        "lc-0a1b2c3d",
        None
    ));
    assert!(!config_written_to(
        &paths,
        "dev",
        "203.0.113.7",
        "lc-6f0b7c3e",
        Some(2222)
    ));
    assert!(!config_written_to(
        &paths,
        "web",
        "203.0.113.7",
        "lc-6f0b7c3e",
        None
    ));
}

#[cfg(unix)]
#[test]
fn a_symlinked_ssh_config_keeps_its_link_and_mode() {
    use std::os::unix::fs::PermissionsExt as _;
    let home = tempfile::tempdir().expect("home");
    let data = tempfile::tempdir().expect("data");
    let dotfiles = tempfile::tempdir().expect("dotfiles");
    let paths = Paths::new(data.path(), home.path());
    std::fs::create_dir_all(home.path().join(".ssh")).expect("ssh dir");
    let real = dotfiles.path().join("ssh_config");
    std::fs::write(&real, "Host github.com\n  User git\n").expect("config");
    std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).expect("mode");
    std::os::unix::fs::symlink(&real, &paths.ssh_config).expect("link");
    let mut registry = Registry::default();
    registry.upsert(computer("dev"), false).expect("added");

    write_ssh_config(&paths, &registry).expect("written");
    let link = std::fs::symlink_metadata(&paths.ssh_config).expect("meta");
    assert!(link.file_type().is_symlink(), "the link is kept");
    let text = std::fs::read_to_string(&real).expect("target");
    assert!(text.starts_with(INCLUDE_LINE), "the target is written");
    assert!(text.contains("Host github.com"));
    let mode = std::fs::metadata(&real).expect("meta").permissions().mode();
    assert_eq!(mode & 0o777, 0o640, "the mode is kept");
    let strays: Vec<_> = std::fs::read_dir(dotfiles.path())
        .expect("dir")
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
        .collect();
    assert!(strays.is_empty(), "no temporary file is left");

    // A link that leads nowhere is refused, with the line to add by hand.
    std::fs::remove_file(&real).expect("rm");
    let error = write_ssh_config(&paths, &registry).expect_err("dangling");
    assert!(error.contains(INCLUDE_LINE), "{error}");
}

#[test]
fn a_concurrent_edit_of_the_ssh_config_is_not_overwritten() {
    let home = tempfile::tempdir().expect("home");
    let path = home.path().join("config");
    std::fs::write(&path, "Host a\n").expect("config");
    let error = write_user_file(&path, "Host old\n", b"new").expect_err("changed");
    assert!(error.contains("changed"), "{error}");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "Host a\n");
    write_user_file(&path, "Host a\n", b"new").expect("unchanged since read");
    assert_eq!(std::fs::read_to_string(&path).expect("read"), "new");
}

#[test]
fn ssh_arguments_enforce_the_pin() {
    let mut c = computer("dev");
    c.port = 2222;
    c.identity_file = Some("/k/id".into());
    let args = ssh_args(&c, Path::new("/kh dir/known_hosts"), &["uptime".to_owned()]);
    let joined = args.join(" ");
    assert!(joined.contains("-o StrictHostKeyChecking=yes"));
    assert!(joined.contains("-o UserKnownHostsFile=\"/kh dir/known_hosts\""));
    assert!(joined.contains("-o HostKeyAlias=leanctx-dev"));
    assert!(joined.contains("-o ForwardAgent=no"));
    assert!(joined.contains("-p 2222"));
    assert!(joined.contains("-i /k/id -o IdentitiesOnly=yes"));
    assert!(joined.ends_with("lc-6f0b7c3e@203.0.113.7 -- uptime"));
    let shell = ssh_args(&computer("dev"), Path::new("/kh"), &[]);
    assert_eq!(
        shell.last().map(String::as_str),
        Some("lc-6f0b7c3e@203.0.113.7")
    );
}

#[test]
fn the_deploy_key_command_never_overwrites_a_key() {
    let command = deploy_key_command("dev");
    assert!(command.contains("[ -f \"$k\" ] || ssh-keygen"));
    assert!(command.contains("leanctx-deploy@dev"));
    assert!(command.ends_with("cat \"$k.pub\""));
    assert!(
        !command.contains("cat \"$k\";"),
        "never prints the private key"
    );
}
