//! Adversarial compression tests based on TheDecipherist/rtk-test methodology.
//! Each test verifies that safety-critical information survives compression.

use lean_ctx::core::patterns::compress_output;

#[test]
fn adversarial_git_diff_preserves_code_content() {
    let diff = "diff --git a/src/auth.rs b/src/auth.rs\n\
                index abc123..def456 100644\n\
                --- a/src/auth.rs\n\
                +++ b/src/auth.rs\n\
                @@ -10,6 +10,8 @@ fn verify_token(token: &str) -> bool {\n\
                     let decoded = decode(token);\n\
                     if decoded.is_err() {\n\
                         return false;\n\
                +    }\n\
                +    if decoded.unwrap().exp < now() {\n\
                +        return false; // expired token\n\
                     }\n\
                     true\n\
                 }";

    let compressed = compress_output("git diff", diff).unwrap();
    assert!(
        compressed.contains("expired token"),
        "diff must preserve code content: {compressed}"
    );
    assert!(
        compressed.contains('+'),
        "diff must preserve +/- markers: {compressed}"
    );
}

#[test]
fn adversarial_git_diff_preserves_security_bug() {
    let diff = "diff --git a/src/api.rs b/src/api.rs\n\
                --- a/src/api.rs\n\
                +++ b/src/api.rs\n\
                @@ -5,3 +5,5 @@\n\
                -    verify_csrf_token(&request);\n\
                +    // TODO: re-enable CSRF check\n\
                +    // verify_csrf_token(&request);\n\
                     process_request(&request);\n";

    let compressed = compress_output("git diff", diff).unwrap();
    assert!(
        compressed.contains("CSRF") || compressed.contains("csrf"),
        "diff must preserve security-relevant changes: {compressed}"
    );
    assert!(
        compressed.contains("verify_csrf_token"),
        "diff must preserve removed function calls: {compressed}"
    );
}

#[test]
fn adversarial_docker_ps_preserves_unhealthy() {
    let ps_output = "CONTAINER ID   IMAGE          COMMAND       CREATED       STATUS                     PORTS     NAMES\n\
                     abc123def456   nginx:latest   \"nginx -g…\"   2 hours ago   Up 2 hours (unhealthy)     80/tcp    web-prod\n\
                     789ghi012jkl   redis:7        \"redis-se…\"   3 hours ago   Up 3 hours (healthy)       6379/tcp  cache-prod\n\
                     345mno678pqr   postgres:16    \"docker-e…\"   5 hours ago   Exited (1) 30 minutes ago            db-prod";

    let compressed = compress_output("docker ps", ps_output).unwrap();
    assert!(
        compressed.contains("unhealthy"),
        "docker ps must preserve unhealthy status: {compressed}"
    );
    assert!(
        compressed.contains("Exited"),
        "docker ps must preserve Exited status: {compressed}"
    );
    assert!(
        compressed.contains("web-prod"),
        "docker ps must preserve container names: {compressed}"
    );
}

#[test]
fn adversarial_df_preserves_root_filesystem() {
    let mut lines = vec!["Filesystem     1K-blocks    Used Available Use% Mounted on".to_string()];
    lines.push("/dev/sda1  100000000  95000000  5000000  95% /".to_string());
    for i in 0..15 {
        lines.push(format!(
            "tmpfs             1000      100      900   10% /snap/core/{i}"
        ));
    }
    let df_output = lines.join("\n");

    let compressed = compress_output("df -h", &df_output).unwrap();
    assert!(
        compressed.contains("/dev/sda1") || compressed.contains("95%"),
        "df must preserve root filesystem info: {compressed}"
    );
    assert!(
        compressed.contains("/ ") || compressed.contains("Mounted on"),
        "df must preserve mount points: {compressed}"
    );
}

#[test]
fn adversarial_pytest_preserves_xfail_xpass() {
    let output = "============================= test session starts ==============================\n\
                  collected 20 items\n\
                  \n\
                  tests/test_auth.py ....x.X...                                          [100%]\n\
                  \n\
                  ================== 15 passed, 2 xfailed, 1 xpassed, 2 warnings in 3.5s ==================";

    let compressed = compress_output("pytest", output).unwrap();
    assert!(
        compressed.contains("xfailed") || compressed.contains("xfail"),
        "pytest must preserve xfailed counter: {compressed}"
    );
    assert!(
        compressed.contains("xpassed") || compressed.contains("xpass"),
        "pytest must preserve xpassed counter: {compressed}"
    );
    assert!(
        compressed.contains("warning"),
        "pytest must preserve warnings counter: {compressed}"
    );
}

#[test]
fn adversarial_git_log_preserves_full_history() {
    let mut log_lines: Vec<String> = Vec::new();
    for i in 0..60 {
        log_lines.push(format!("abc{i:04x} fix: commit message number {i}"));
    }
    let output = log_lines.join("\n");

    let compressed_unlimited = compress_output("git log -n 60 --oneline", &output).unwrap();
    assert!(
        compressed_unlimited.contains("commit message number 59"),
        "git log with explicit -n must preserve all entries: {compressed_unlimited}"
    );

    let compressed_default = compress_output("git log --oneline", &output).unwrap();
    assert!(
        compressed_default.contains("commit message number 59"),
        "git log default should show all 60 entries (under 100 cap): {compressed_default}"
    );
}

#[test]
fn adversarial_grep_preserves_context() {
    let mut grep_lines: Vec<String> = Vec::new();
    for i in 0..80 {
        grep_lines.push(format!("src/auth.rs:{i}:    let user = get_user(id);"));
    }
    let output = grep_lines.join("\n");

    let compressed = compress_output("grep -rn 'get_user'", &output).unwrap();
    assert!(
        compressed.contains("get_user"),
        "grep output <=100 lines must pass through verbatim: {compressed}"
    );
    assert_eq!(
        compressed
            .lines()
            .filter(|l| l.contains("get_user"))
            .count(),
        80,
        "all 80 grep matches must be preserved: {compressed}"
    );
}

#[test]
fn adversarial_log_preserves_critical_severity() {
    let mut log = Vec::new();
    for i in 0..40 {
        log.push(format!("2024-01-01 10:00:{i:02} INFO  request processed"));
    }
    log.insert(
        20,
        "2024-01-01 10:00:20 CRITICAL database connection lost".to_string(),
    );
    log.insert(
        25,
        "2024-01-01 10:00:25 ERROR OOMKilled: container exceeded memory".to_string(),
    );
    let output = log.join("\n");

    let compressed = compress_output("cat /var/log/app.log", &output);
    let text = compressed.unwrap_or_else(|| output.clone());
    assert!(
        text.contains("CRITICAL") || text.contains("database connection lost"),
        "log output must preserve CRITICAL lines: {text}"
    );
}

#[test]
fn adversarial_npm_audit_preserves_cve_ids() {
    let audit = "# npm audit report\n\
                 \n\
                 lodash  <=4.17.20\n\
                 Severity: critical\n\
                 Prototype Pollution - https://github.com/advisories/GHSA-xxxx\n\
                 fix available via `npm audit fix --force`\n\
                 depends on vulnerable versions of lodash\n\
                 node_modules/lodash\n\
                 \n\
                 express  <4.17.3\n\
                 Severity: high\n\
                 CVE-2024-12345 - Open redirect vulnerability\n\
                 fix available via `npm audit fix`\n\
                 node_modules/express\n\
                 \n\
                 2 vulnerabilities (1 high, 1 critical)\n";

    let compressed = compress_output("npm audit", audit).unwrap();
    assert!(
        compressed.contains("CVE-2024-12345"),
        "npm audit must preserve CVE IDs: {compressed}"
    );
    assert!(
        compressed.contains("critical"),
        "npm audit must preserve severity levels: {compressed}"
    );
}

#[test]
fn adversarial_docker_logs_preserves_critical() {
    let mut log = Vec::new();
    for i in 0..50 {
        log.push(format!(
            "2024-01-01T10:00:{i:02}Z INFO  healthy check passed"
        ));
    }
    log.insert(
        15,
        "2024-01-01T10:00:15Z FATAL  out of memory, container killed".to_string(),
    );
    log.insert(
        30,
        "2024-01-01T10:00:30Z ERROR  panic: runtime error".to_string(),
    );
    let output = log.join("\n");

    let compressed = compress_output("docker logs mycontainer", &output).unwrap();
    assert!(
        compressed.contains("FATAL") || compressed.contains("out of memory"),
        "docker logs must preserve FATAL lines: {compressed}"
    );
}

#[test]
fn adversarial_pip_uninstall_preserves_package_names() {
    let output = "Found existing installation: requests 2.28.0\n\
                  Uninstalling requests-2.28.0:\n\
                    Successfully uninstalled requests-2.28.0\n\
                  Found existing installation: flask 2.3.0\n\
                  Uninstalling flask-2.3.0:\n\
                    Successfully uninstalled flask-2.3.0\n\
                  Found existing installation: numpy 1.24.0\n\
                  Uninstalling numpy-1.24.0:\n\
                    Successfully uninstalled numpy-1.24.0\n";

    let compressed = compress_output("pip uninstall requests flask numpy -y", output).unwrap();
    assert!(
        compressed.contains("requests"),
        "pip uninstall must list package names: {compressed}"
    );
    assert!(
        compressed.contains("flask"),
        "pip uninstall must list package names: {compressed}"
    );
    assert!(
        compressed.contains("numpy"),
        "pip uninstall must list package names: {compressed}"
    );
}

#[test]
fn adversarial_middle_truncation_preserves_errors() {
    let mut lines: Vec<String> = Vec::new();
    for i in 0..60 {
        lines.push(format!("line {i}: normal output"));
    }
    lines[30] = "ERROR: critical failure in module X".to_string();
    lines[35] = "WARNING: disk space low".to_string();
    let output = lines.join("\n");

    let compressed = lean_ctx::shell::compress_if_beneficial_pub("unknown-command", &output);
    if compressed.contains('[') && compressed.contains("omitted") {
        assert!(
            compressed.contains("ERROR") || compressed.contains("critical failure"),
            "truncation must preserve error lines: {compressed}"
        );
    }
}

// ===== Regression tests: Scenarios that were SAFE in TheDecipherist/rtk-test v3.2.5 =====
// These must stay SAFE after the adversarial hardening changes.

#[test]
fn regression_git_status_detached_head() {
    let output = "HEAD detached at 48a7098\nnothing to commit, working tree clean";
    let compressed = compress_output("git status", output).unwrap();
    assert!(
        compressed.contains("detached") || compressed.contains("HEAD detached"),
        "git status must preserve DETACHED HEAD warning: {compressed}"
    );
}

#[test]
fn regression_log_critical_severity() {
    let output = "[INFO] health check ok\n\
                  [INFO] health check ok\n\
                  [CRITICAL] database connection lost\n\
                  [INFO] health check ok\n\
                  [ERROR] retry failed";
    let compressed = compress_output("cat /var/log/app.log", output);
    let text = compressed.unwrap_or_else(|| output.to_string());
    assert!(
        text.contains("CRITICAL"),
        "cat log must preserve CRITICAL lines: {text}"
    );
    assert!(
        text.contains("ERROR"),
        "cat log must preserve ERROR lines: {text}"
    );
}

#[test]
fn regression_ls_shows_dotenv() {
    let output = ".env\n.gitignore\nREADME.md\nsrc\npackage.json";
    let compressed = compress_output("ls -a", output);
    let text = compressed.unwrap_or_else(|| output.to_string());
    assert!(text.contains(".env"), "ls must show .env file: {text}");
}

#[test]
fn regression_pip_list_all_packages() {
    let mut lines = vec![
        "Package    Version".to_string(),
        "---------- -------".to_string(),
    ];
    for i in 0..50 {
        lines.push(format!("package-{i}  1.0.{i}"));
    }
    let output = lines.join("\n");
    let compressed = compress_output("pip list", &output);
    let text = compressed.unwrap_or_else(|| output.clone());
    assert!(
        text.contains("package-0") && text.contains("package-49"),
        "pip list must show all packages: first and last must be present"
    );
}

#[test]
fn regression_git_stash_verbatim() {
    let output = "No local changes to save";
    let compressed = compress_output("git stash", output);
    let text = compressed.unwrap_or_else(|| output.to_string());
    assert!(
        text.contains("No local changes"),
        "git stash must pass through verbatim: {text}"
    );

    let output2 = "Saved working directory and index state WIP on main: abc1234 fix: typo";
    let compressed2 = compress_output("git stash", output2);
    let text2 = compressed2.unwrap_or_else(|| output2.to_string());
    assert!(
        text2.contains("Saved") || text2.contains("WIP on main"),
        "git stash save must pass through: {text2}"
    );
}

#[test]
fn regression_ruff_preserves_file_line_col() {
    let output = "src/api.py:42:10: E501 Line too long (120 > 79)\n\
                  src/api.py:88:1: F401 'os' imported but unused\n\
                  Found 2 errors.";
    let compressed = compress_output("ruff check", output);
    let text = compressed.unwrap_or_else(|| output.to_string());
    assert!(
        text.contains("src/api.py:42:10"),
        "ruff must preserve file:line:col references: {text}"
    );
    assert!(
        text.contains("src/api.py:88:1"),
        "ruff must preserve all references: {text}"
    );
}

#[test]
fn regression_find_preserves_full_paths() {
    let output = "/home/user/project/src/api/file.ts\n\
                  /home/user/project/src/utils/helper.ts\n\
                  /home/user/project/tests/test_api.ts";
    let compressed = compress_output("find . -name '*.ts'", output);
    let text = compressed.unwrap_or_else(|| output.to_string());
    assert!(
        text.contains("/home/user/project/src/api/file.ts"),
        "find must preserve full absolute paths: {text}"
    );
}

#[test]
fn regression_ls_recursive_preserves_tree() {
    let output = "./src:\napi.ts\nutils.ts\n\n./src/components:\nButton.tsx\nHeader.tsx\n\n./tests:\ntest_api.ts";
    let compressed = compress_output("ls -R", output);
    let text = compressed.unwrap_or_else(|| output.to_string());
    assert!(
        text.contains("./src:") && text.contains("./tests:"),
        "ls -R must preserve directory headers: {text}"
    );
}

#[test]
fn regression_wc_pipe_correct() {
    let output = "42";
    let compressed = compress_output("wc -l", output);
    let text = compressed.unwrap_or_else(|| output.to_string());
    assert!(text.contains("42"), "wc output must be preserved: {text}");
}

// ===== New adversarial tests: comprehensive coverage across ecosystems =====

#[test]
fn adversarial_npm_install_preserves_packages() {
    let output = "\
npm warn deprecated glob@7.2.3: Glob versions prior to v9 are no longer supported
npm warn deprecated inflight@1.0.6: This module is not supported

added 547 packages, and audited 548 packages in 12s

75 packages are looking for funding
  run `npm fund` for details

found 0 vulnerabilities";

    let compressed = compress_output("npm install", output).unwrap();
    assert!(
        compressed.contains("547"),
        "npm install must preserve package count: {compressed}"
    );
}

#[test]
fn adversarial_npm_install_with_explicit_packages() {
    let output = "\
+ express@4.18.2
+ lodash@4.17.21
+ axios@1.6.2

added 58 packages, and audited 59 packages in 3s

found 0 vulnerabilities";

    let compressed = compress_output("npm install express lodash axios", output).unwrap();
    assert!(
        compressed.contains("express"),
        "npm install must preserve installed package names: {compressed}"
    );
    assert!(
        compressed.contains("lodash"),
        "npm install must preserve installed package names: {compressed}"
    );
    assert!(
        compressed.contains("axios"),
        "npm install must preserve installed package names: {compressed}"
    );
}

#[test]
fn adversarial_cargo_build_preserves_errors() {
    let output = "\
   Compiling myapp v0.1.0 (/home/user/myapp)
error[E0308]: mismatched types
 --> src/main.rs:42:10
  |
42|     foo(x)
  |         ^ expected `&str`, found `String`
  |

error[E0599]: no method named `bar` found for struct `Config`
  --> src/config.rs:15:10
   |
15 |     cfg.bar()
   |         ^^^ method not found in `Config`

error: could not compile `myapp` (bin \"myapp\") due to 2 previous errors";

    let compressed = compress_output("cargo build", output).unwrap();
    assert!(
        compressed.contains("E0308"),
        "cargo build must preserve error codes: {compressed}"
    );
    assert!(
        compressed.contains("E0599"),
        "cargo build must preserve all error codes: {compressed}"
    );
    assert!(
        compressed.contains("mismatched types") || compressed.contains("expected"),
        "cargo build must preserve error messages: {compressed}"
    );
}

#[test]
fn adversarial_eslint_preserves_file_line_and_rules() {
    let output = "\
/home/user/project/src/App.tsx
   5:10  error    'useState' is defined but never used  no-unused-vars
  12:15  warning  Unexpected any                        @typescript-eslint/no-explicit-any
  33:1   error    Missing return type                   @typescript-eslint/explicit-function-return-type

/home/user/project/src/utils.ts
   8:5   error    Unexpected var                        no-var

4 problems (3 errors, 1 warning)";

    let compressed = compress_output("eslint .", output).unwrap();
    assert!(
        compressed.contains("no-unused-vars") || compressed.contains("unused"),
        "eslint must preserve rule names: {compressed}"
    );
    assert!(
        compressed.contains("3 error") || compressed.contains("error"),
        "eslint must preserve error counts: {compressed}"
    );
}

#[test]
fn adversarial_go_build_preserves_errors() {
    let output = "\
./main.go:15:2: undefined: Config
./main.go:23:10: cannot use x (variable of type string) as int value in argument to process
./handlers/auth.go:42:5: too many arguments in call to validateToken
./handlers/auth.go:55:12: impossible type assertion: *http.Request does not implement CustomRequest";

    let compressed = compress_output("go build ./...", output).unwrap();
    assert!(
        compressed.contains("main.go") || compressed.contains("undefined"),
        "go build must preserve file references: {compressed}"
    );
    assert!(
        compressed.contains("auth.go") || compressed.contains("validateToken"),
        "go build must preserve all error locations: {compressed}"
    );
}

#[test]
fn adversarial_docker_build_preserves_step_errors() {
    let output = "\
#1 [internal] load build definition from Dockerfile
#1 DONE 0.0s

#5 [2/5] RUN apt-get update && apt-get install -y curl
#5 DONE 15.2s

#6 [3/5] COPY requirements.txt .
#6 DONE 0.1s

#7 [4/5] RUN pip install -r requirements.txt
#7 4.521 ERROR: Could not find a version that satisfies the requirement nonexistent-package==99.0
#7 4.521 ERROR: No matching distribution found for nonexistent-package==99.0
#7 ERROR: process \"/bin/sh -c pip install -r requirements.txt\" did not complete successfully: exit code: 1";

    let compressed = compress_output("docker build .", output).unwrap();
    assert!(
        compressed.contains("ERROR") || compressed.contains("error"),
        "docker build must preserve error lines: {compressed}"
    );
    assert!(
        compressed.contains("nonexistent-package") || compressed.contains("not complete"),
        "docker build must preserve error details: {compressed}"
    );
}

#[test]
fn adversarial_tsc_preserves_type_errors() {
    let output = "\
src/api/routes.ts(15,10): error TS2304: Cannot find name 'Request'.
src/api/routes.ts(22,5): error TS2339: Property 'userId' does not exist on type 'Session'.
src/utils/auth.ts(8,3): error TS2345: Argument of type 'string' is not assignable to parameter of type 'number'.
src/utils/auth.ts(42,20): error TS7006: Parameter 'req' implicitly has an 'any' type.

Found 4 errors in 2 files.";

    let compressed = compress_output("tsc --noEmit", output).unwrap();
    assert!(
        compressed.contains("TS2304"),
        "tsc must preserve error code TS2304: {compressed}"
    );
    assert!(
        compressed.contains("TS2339"),
        "tsc must preserve error code TS2339: {compressed}"
    );
    assert!(
        compressed.contains("routes.ts") || compressed.contains("auth.ts"),
        "tsc must preserve file references: {compressed}"
    );
    assert!(
        compressed.contains("4 error"),
        "tsc must preserve error count: {compressed}"
    );
}

#[test]
fn adversarial_dotnet_build_preserves_errors() {
    let output = "\
Microsoft (R) Build Engine version 17.8.3+195e7f5a3 for .NET
Copyright (C) Microsoft Corporation. All rights reserved.

  Determining projects to restore...
  All projects are up-to-date for restore.
Controllers/UserController.cs(15,25): error CS0246: The type or namespace name 'UserService' could not be found
Models/User.cs(8,12): error CS0246: The type or namespace name 'JsonProperty' could not be found

Build FAILED.

Controllers/UserController.cs(15,25): error CS0246: The type or namespace name 'UserService' could not be found
Models/User.cs(8,12): error CS0246: The type or namespace name 'JsonProperty' could not be found
    2 Error(s)
    0 Warning(s)

Time Elapsed 00:00:01.82";

    let compressed = compress_output("dotnet build", output).unwrap();
    assert!(
        compressed.contains("CS0246") || compressed.contains("error"),
        "dotnet build must preserve error codes: {compressed}"
    );
    assert!(
        compressed.contains("FAILED") || compressed.contains('2'),
        "dotnet build must preserve build result: {compressed}"
    );
}

#[test]
fn adversarial_composer_install_preserves_packages() {
    let output = "\
Loading composer repositories with package information
Updating dependencies
Lock file operations: 5 installs, 0 updates, 0 removals
  - Installing psr/log (3.0.0): Extracting archive
  - Installing monolog/monolog (3.5.0): Extracting archive
  - Installing symfony/console (7.0.3): Extracting archive
  - Installing laravel/framework (11.0.0): Extracting archive
  - Installing phpunit/phpunit (10.5.0): Extracting archive
Writing lock file
Generating optimized autoload files
Package operations: 5 installs, 0 updates, 0 removals";

    let compressed = compress_output("composer install", output).unwrap();
    assert!(
        compressed.contains('5') || compressed.contains("install"),
        "composer install must preserve package count: {compressed}"
    );
}

#[test]
fn adversarial_cargo_test_preserves_failures() {
    let output = "\
   Compiling myapp v0.1.0 (/home/user/myapp)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.34s
     Running unittests src/lib.rs (target/debug/deps/myapp-abc123)

running 25 tests
test auth::tests::login_works ... ok
test auth::tests::logout_works ... ok
test auth::tests::token_expired ... FAILED
test db::tests::connection_pool ... ok

failures:

---- auth::tests::token_expired stdout ----
thread 'auth::tests::token_expired' panicked at 'assertion failed: `(left == right)`
  left: `true`,
 right: `false`', src/auth.rs:142:9

failures:
    auth::tests::token_expired

test result: FAILED. 24 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.52s";

    let compressed = compress_output("cargo test", output).unwrap();
    assert!(
        compressed.contains("1 failed") || compressed.contains("FAILED"),
        "cargo test must preserve failure count: {compressed}"
    );
    assert!(
        compressed.contains("24 passed") || compressed.contains("24"),
        "cargo test must preserve passed count: {compressed}"
    );
}

#[test]
fn adversarial_kubectl_get_preserves_pod_status() {
    let output = "\
NAME                          READY   STATUS             RESTARTS      AGE
api-deploy-abc123-xyz         1/1     Running            0             5d
web-deploy-def456-uvw         0/1     CrashLoopBackOff   15 (2m ago)   1h
worker-deploy-ghi789-rst      1/1     Running            0             3d
db-migrate-job-abc            0/1     Error              0             30m";

    let compressed = compress_output("kubectl get pods", output).unwrap();
    assert!(
        compressed.contains("CrashLoopBackOff"),
        "kubectl get pods must preserve CrashLoopBackOff: {compressed}"
    );
    assert!(
        compressed.contains("Error"),
        "kubectl get pods must preserve Error status: {compressed}"
    );
}

#[test]
fn adversarial_terraform_plan_preserves_changes() {
    let output = "\
Terraform will perform the following actions:

  # aws_instance.web will be destroyed
  - resource \"aws_instance\" \"web\" {
      - ami           = \"ami-abc123\" -> null
      - instance_type = \"t3.large\" -> null
    }

  # aws_security_group.allow_all will be created
  + resource \"aws_security_group\" \"allow_all\" {
      + name        = \"allow_all\"
      + ingress {
          + from_port   = 0
          + to_port     = 65535
          + protocol    = \"-1\"
          + cidr_blocks = [\"0.0.0.0/0\"]
        }
    }

Plan: 1 to add, 0 to change, 1 to destroy.";

    let compressed = compress_output("terraform plan", output).unwrap();
    assert!(
        compressed.contains("destroy") || compressed.contains("1 to destroy"),
        "terraform plan must preserve destructive actions: {compressed}"
    );
    assert!(
        compressed.contains("add") || compressed.contains("1 to add"),
        "terraform plan must preserve additions: {compressed}"
    );
}

// ===== Issue #149: TheDecipherist security review — additional adversarial tests =====

#[test]
fn adversarial_git_show_preserves_diff_content() {
    let output = "\
commit abc1234def5678901234567890abcdef12345678
Author: Dev <dev@example.com>
Date:   Mon Jan 1 10:00:00 2024 +0000

    fix: remove fee from charge calculation

diff --git a/src/billing.rs b/src/billing.rs
--- a/src/billing.rs
+++ b/src/billing.rs
@@ -10,3 +10,3 @@
-    return charge(amount + fee);
+    return charge(amount);  // BUG: fee not applied
     log_transaction(amount);";

    let compressed = compress_output("git show abc1234", output).unwrap();
    assert!(
        compressed.contains("charge(amount)") || compressed.contains("fee not applied"),
        "git show must preserve diff code content: {compressed}"
    );
    assert!(
        compressed.contains('+') || compressed.contains('-'),
        "git show must preserve diff +/- markers: {compressed}"
    );
}

#[test]
fn adversarial_git_show_preserves_security_change() {
    let output = "\
commit deadbeef12345678901234567890abcdef12345678
Author: Dev <dev@example.com>
Date:   Mon Jan 1 10:00:00 2024 +0000

    chore: disable auth temporarily

diff --git a/src/auth.rs b/src/auth.rs
--- a/src/auth.rs
+++ b/src/auth.rs
@@ -5,3 +5,5 @@
-    verify_csrf_token(&request);
+    // HACK: skip CSRF for now
+    // verify_csrf_token(&request);
     process_request(&request);";

    let compressed = compress_output("git show deadbeef", output).unwrap();
    assert!(
        compressed.contains("CSRF") || compressed.contains("csrf"),
        "git show must preserve security-relevant changes: {compressed}"
    );
    assert!(
        compressed.contains("verify_csrf_token"),
        "git show must preserve removed function calls: {compressed}"
    );
}

#[test]
fn adversarial_docker_ps_unhealthy_narrow_columns() {
    // Simulate narrow STATUS column where (unhealthy) bleeds into PORTS area
    let output = "\
CONTAINER ID   IMAGE          COMMAND    CREATED      STATUS                    PORTS     NAMES
abc123def456   nginx:latest   \"nginx\"    2 hours ago  Up 2 hours (unhealthy)    80/tcp    web
789ghi012jkl   redis:7        \"redis\"    3 hours ago  Up 3 hours                6379/tcp  cache";

    let compressed = compress_output("docker ps", output).unwrap();
    assert!(
        compressed.contains("unhealthy"),
        "docker ps must preserve (unhealthy) even with tight column layout: {compressed}"
    );
    assert!(
        compressed.contains("web"),
        "docker ps must preserve container names: {compressed}"
    );
}

#[test]
fn adversarial_docker_ps_exited_containers() {
    let output = "\
CONTAINER ID   IMAGE          COMMAND    CREATED      STATUS                        PORTS     NAMES
abc123def456   nginx:latest   \"nginx\"    2 hours ago  Exited (1) 30 minutes ago               web-crashed
789ghi012jkl   redis:7        \"redis\"    3 hours ago  Up 3 hours (healthy)          6379/tcp  cache";

    let compressed = compress_output("docker ps -a", output).unwrap();
    assert!(
        compressed.contains("Exited"),
        "docker ps -a must preserve Exited status: {compressed}"
    );
    assert!(
        compressed.contains("healthy"),
        "docker ps -a must preserve (healthy) annotation: {compressed}"
    );
    assert!(
        compressed.contains("web-crashed"),
        "docker ps -a must show crashed containers: {compressed}"
    );
}

#[test]
fn adversarial_git_log_100_plus_commits() {
    let mut log_lines: Vec<String> = Vec::new();
    for i in 0..120 {
        log_lines.push(format!("abc{i:04x} fix: commit message number {i}"));
    }
    let output = log_lines.join("\n");

    let compressed = compress_output("git log --oneline", &output).unwrap();
    assert!(
        compressed.contains("commit message number 99"),
        "git log default should show at least 100 entries: {compressed}"
    );
    assert!(
        compressed.contains("20 more commits"),
        "git log should indicate truncated count: {compressed}"
    );
}

#[test]
fn adversarial_git_log_explicit_limit_unlimited() {
    let mut log_lines: Vec<String> = Vec::new();
    for i in 0..120 {
        log_lines.push(format!("abc{i:04x} fix: commit message number {i}"));
    }
    let output = log_lines.join("\n");

    let compressed = compress_output("git log -n 120 --oneline", &output).unwrap();
    assert!(
        compressed.contains("commit message number 119"),
        "git log with explicit -n must preserve all entries: {compressed}"
    );
    assert!(
        !compressed.contains("more commits"),
        "git log with explicit -n must not truncate: {compressed}"
    );
}

#[test]
fn adversarial_safeguard_ratio_prevents_over_compression() {
    use lean_ctx::core::compressor::safeguard_ratio;

    let original = "a]b ".repeat(200);
    let over_compressed = "x";
    let result = safeguard_ratio(&original, over_compressed);
    assert_eq!(
        result, original,
        "safeguard_ratio must return original when compression ratio < 0.15"
    );

    let mild_compressed = "a]b ".repeat(80);
    let result2 = safeguard_ratio(&original, &mild_compressed);
    assert_eq!(
        result2, mild_compressed,
        "safeguard_ratio must allow mild compression"
    );
}

#[test]
fn adversarial_shell_hook_preserves_errors_in_truncation() {
    let mut lines: Vec<String> = Vec::new();
    for i in 0..100 {
        lines.push(format!("normal output line {i}"));
    }
    lines[50] = "CRITICAL: database corruption detected in row 4821".to_string();
    lines[75] = "ERROR: payment processing service unreachable".to_string();
    let output = lines.join("\n");

    let compressed = lean_ctx::shell::compress_if_beneficial_pub("cat /var/log/app.log", &output);
    assert!(
        compressed.contains("CRITICAL") || compressed.contains("database corruption"),
        "shell hook must preserve CRITICAL lines during truncation: {compressed}"
    );
    assert!(
        compressed.contains("ERROR") || compressed.contains("payment processing"),
        "shell hook must preserve ERROR lines during truncation: {compressed}"
    );
}
