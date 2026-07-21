//! Snapshot regression tests for all compression pattern modules.
//!
//! Tests `try_specific_pattern` (pattern dispatch) and `compress_output` (full pipeline)
//! to ensure every pattern module produces stable, expected output.
//!
//! Run `cargo insta review` after intentional pattern changes to accept new snapshots.

use lean_ctx::core::patterns::{compress_output, try_specific_pattern};

fn assert_pattern(name: &str, cmd: &str, output: &str) {
    let result = try_specific_pattern(cmd, output);
    let compressed = result
        .unwrap_or_else(|| panic!("[{name}] try_specific_pattern({cmd:?}, ...) returned None"));
    insta::assert_snapshot!(name, compressed);
}

fn assert_pipeline(name: &str, cmd: &str, output: &str) {
    let result = compress_output(cmd, output);
    let compressed =
        result.unwrap_or_else(|| panic!("[{name}] compress_output({cmd:?}, ...) returned None"));
    insta::assert_snapshot!(name, compressed);
}

// ═══════════════════════════════════════════════════════════════════════
// git patterns
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_git_status() {
    assert_pattern(
        "git_status",
        "git status",
        "On branch main\n\
         Your branch is up to date with 'origin/main'.\n\n\
         Changes not staged for commit:\n\
         (use \"git add <file>...\" to update what will be committed)\n\
         (use \"git restore <file>...\" to discard changes in working directory)\n\
         \tmodified:   src/main.rs\n\
         \tmodified:   src/lib.rs\n\n\
         Untracked files:\n\
         (use \"git add <file>...\" to include in what will be committed)\n\
         \tnew_file.rs\n\n\
         no changes added to commit (use \"git add\" and/or \"git commit -a\")\n",
    );
}

#[test]
fn pattern_git_diff() {
    assert_pattern(
        "git_diff",
        "git diff",
        "diff --git a/src/main.rs b/src/main.rs\n\
         index abc1234..def5678 100644\n\
         --- a/src/main.rs\n\
         +++ b/src/main.rs\n\
         @@ -1,5 +1,7 @@\n\
         +use std::io;\n\
          fn main() {\n\
         -    println!(\"hello\");\n\
         +    println!(\"hello world\");\n\
         +    io::stdout().flush().unwrap();\n\
          }\n\
         diff --git a/src/lib.rs b/src/lib.rs\n\
         index 1111111..2222222 100644\n\
         --- a/src/lib.rs\n\
         +++ b/src/lib.rs\n\
         @@ -10,3 +10,5 @@\n\
          pub fn greet() -> &'static str {\n\
         -    \"hi\"\n\
         +    \"hello\"\n\
          }\n\
         +\n\
         +pub fn farewell() -> &'static str { \"bye\" }\n",
    );
}

#[test]
fn pattern_git_log() {
    assert_pattern(
        "git_log",
        "git log",
        "commit abc1234567890abcdef1234567890abcdef123456\n\
         Author: Alice <alice@example.com>\n\
         Date:   Mon Jan 15 10:23:45 2024 +0100\n\n\
             feat: add user authentication\n\n\
         commit def5678901234567890abcdef1234567890abcdef\n\
         Author: Bob <bob@example.com>\n\
         Date:   Sun Jan 14 08:00:00 2024 +0100\n\n\
             fix: handle null pointer in config parser\n\n\
         commit 111aaaa234567890abcdef1234567890abcdef1234\n\
         Author: Charlie <charlie@example.com>\n\
         Date:   Sat Jan 13 12:30:00 2024 +0100\n\n\
             refactor: extract config module from main\n\n\
         commit 222bbbb234567890abcdef1234567890abcdef1234\n\
         Author: Alice <alice@example.com>\n\
         Date:   Fri Jan 12 09:15:00 2024 +0100\n\n\
             docs: update README with API examples\n\n\
         commit 333cccc234567890abcdef1234567890abcdef1234\n\
         Author: Bob <bob@example.com>\n\
         Date:   Thu Jan 11 14:00:00 2024 +0100\n\n\
             test: add integration tests for auth flow\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// gh / glab
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_gh_pr_list() {
    assert_pattern(
        "gh_pr_list",
        "gh pr list",
        "Showing 5 of 12 open pull requests in org/repo\n\n\
         #42  feat: add oauth    feature/oauth   OPEN\n\
         #41  fix: login crash   fix/login       OPEN\n\
         #40  docs: api guide    docs/api        OPEN\n\
         #39  refactor: db       refactor/db     OPEN\n\
         #38  chore: deps        chore/deps      OPEN\n",
    );
}

#[test]
fn pattern_glab_mr_list() {
    assert_pattern(
        "glab_mr_list",
        "glab mr list",
        "Showing 4 open merge requests on mygroup/myproject (Page 1)\n\n\
         !142 feat: add oauth support          feature/oauth    alice (alice@dev)  2024-01-15T10:23:45Z\n\
         !141 fix: login crash on empty token   fix/login        bob (bob@dev)      2024-01-14T08:00:00Z\n\
         !140 docs: add API reference guide     docs/api-ref     charlie (cdev)     2024-01-13T12:30:00Z\n\
         !139 refactor: database layer          refactor/db      diana (ddev)       2024-01-12T09:15:00Z\n\
         !138 chore: bump dependencies          chore/deps       eve (edev)         2024-01-11T14:00:00Z\n\
         !137 test: add E2E smoke tests         test/e2e         frank (fdev)       2024-01-10T16:45:00Z\n\
         !136 ci: add ARM64 runner              ci/arm-runner    grace (gdev)       2024-01-09T11:30:00Z\n\
         !135 perf: optimize query planner      perf/queries     hank (hdev)        2024-01-08T13:20:00Z\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// cargo
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_cargo_test() {
    assert_pattern(
        "cargo_test",
        "cargo test",
        "   Compiling myapp v0.1.0 (/home/user/myapp)\n\
            Finished `test` profile [unoptimized + debuginfo] target(s) in 2.34s\n\
              Running unittests src/lib.rs (target/debug/deps/myapp-abc123)\n\n\
         running 15 tests\n\
         test auth::tests::login_ok ... ok\n\
         test auth::tests::login_fail ... ok\n\
         test auth::tests::refresh_token ... ok\n\
         test db::tests::connect ... ok\n\
         test db::tests::migrate ... ok\n\
         test db::tests::query_users ... ok\n\
         test api::tests::health ... ok\n\
         test api::tests::create_user ... ok\n\
         test api::tests::delete_user ... ok\n\
         test api::tests::list_users ... ok\n\
         test cache::tests::set_get ... ok\n\
         test cache::tests::expire ... ok\n\
         test cache::tests::evict ... ok\n\
         test config::tests::load ... ok\n\
         test config::tests::validate ... ok\n\n\
         test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.45s\n",
    );
}

#[test]
fn pattern_cargo_build() {
    assert_pattern(
        "cargo_build",
        "cargo build",
        "   Compiling proc-macro2 v1.0.86\n\
            Compiling unicode-ident v1.0.12\n\
            Compiling quote v1.0.36\n\
            Compiling syn v2.0.68\n\
            Compiling serde_derive v1.0.203\n\
            Compiling serde v1.0.203\n\
            Compiling serde_json v1.0.117\n\
            Compiling tokio-macros v2.3.0\n\
            Compiling tokio v1.38.0\n\
            Compiling axum v0.7.5\n\
            Compiling myapp v0.1.0 (/home/user/myapp)\n\
             Finished `dev` profile [unoptimized + debuginfo] target(s) in 45.12s\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// docker
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_docker_build() {
    assert_pattern(
        "docker_build",
        "docker build -t myapp .",
        "#1 [internal] load build definition from Dockerfile\n\
         #1 transferring dockerfile: 520B done\n\
         #1 DONE 0.0s\n\n\
         #2 [internal] load metadata for docker.io/library/rust:1.79\n\
         #2 DONE 1.2s\n\n\
         #3 [internal] load .dockerignore\n\
         #3 transferring context: 2B done\n\
         #3 DONE 0.0s\n\n\
         #4 [1/5] FROM docker.io/library/rust:1.79@sha256:abc123\n\
         #4 CACHED\n\n\
         #5 [2/5] WORKDIR /app\n\
         #5 CACHED\n\n\
         #6 [3/5] COPY Cargo.toml Cargo.lock ./\n\
         #6 DONE 0.1s\n\n\
         #7 [4/5] RUN cargo build --release\n\
         #7 3.456    Compiling myapp v0.1.0 (/app)\n\
         #7 15.23     Finished release [optimized] target(s) in 14.99s\n\
         #7 DONE 15.3s\n\n\
         #8 [5/5] COPY . .\n\
         #8 DONE 0.2s\n\n\
         #9 exporting to image\n\
         #9 exporting layers 0.3s done\n\
         #9 writing image sha256:def456 done\n\
         #9 naming to docker.io/library/myapp done\n\
         #9 DONE 0.4s\n",
    );
}

#[test]
fn pattern_docker_ps() {
    assert_pattern(
        "docker_ps",
        "docker ps",
        "CONTAINER ID   IMAGE          COMMAND                  CREATED        STATUS        PORTS                    NAMES\n\
         abc1234def56   nginx:latest   \"/docker-entrypoint.…\"   3 days ago     Up 3 days     0.0.0.0:80->80/tcp       web\n\
         789ghi012jkl   redis:7        \"docker-entrypoint.s…\"   7 days ago     Up 7 days     0.0.0.0:6379->6379/tcp   cache\n\
         mno345pqr678   postgres:16    \"docker-entrypoint.s…\"   14 days ago    Up 14 days    0.0.0.0:5432->5432/tcp   db\n\
         stu901vwx234   rabbitmq:3     \"docker-entrypoint.s…\"   30 days ago    Up 30 days    0.0.0.0:5672->5672/tcp   queue\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// npm / pnpm / bun / deno
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_npm_install() {
    assert_pattern(
        "npm_install",
        "npm install",
        "\nadded 847 packages, and audited 848 packages in 12s\n\n\
         142 packages are looking for funding\n\
           run `npm fund` for details\n\n\
         3 vulnerabilities (1 moderate, 2 high)\n\n\
         To address all issues, run:\n\
           npm audit fix\n\n\
         Run `npm audit` for details.\n",
    );
}

#[test]
fn pattern_pnpm_install() {
    assert_pattern(
        "pnpm_install",
        "pnpm install",
        "Lockfile is up to date, resolution step is skipped\n\
         Already up to date\n\
         Done in 1.2s\n\
         Packages: +0\n\
         Progress: resolved 847, reused 847, downloaded 0, added 0, done\n",
    );
}

#[test]
fn pattern_bun_test() {
    assert_pattern(
        "bun_test",
        "bun test",
        "bun test v1.0.0\n\n\
         tests/auth.test.ts:\n\
         \u{2713} login succeeds [2.34ms]\n\
         \u{2713} logout clears session [1.23ms]\n\
         \u{2713} register creates user [3.45ms]\n\n\
         tests/api.test.ts:\n\
         \u{2713} GET /health returns 200 [0.56ms]\n\
         \u{2713} POST /users creates user [4.56ms]\n\
         \u{2717} DELETE /users/:id returns 204 [1.23ms]\n\n\
          5 pass\n\
          1 fail\n\
          6 expect() calls\n\
         Ran 6 tests across 2 files [52.00ms]\n",
    );
}

#[test]
fn pattern_deno_test() {
    assert_pattern(
        "deno_test",
        "deno test",
        "running 8 tests from ./tests/auth_test.ts\n\
         test login ... ok (15ms)\n\
         test logout ... ok (8ms)\n\
         test register ... ok (23ms)\n\
         running 5 tests from ./tests/api_test.ts\n\
         test health ... ok (5ms)\n\
         test create ... ok (12ms)\n\
         test delete ... FAILED (10ms)\n\
         test update ... ok (8ms)\n\
         test list ... ok (6ms)\n\n\
          ERRORS \n\n\
         delete => ./tests/api_test.ts:25:6\n\
         error: AssertionError: Expected 204 but got 403\n\n\
          FAILURES \n\n\
         delete => ./tests/api_test.ts:25:6\n\n\
         ok | 7 passed | 1 failed (120ms)\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// pip / poetry / mypy / ruff / pytest
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_pip_install() {
    assert_pattern(
        "pip_install",
        "pip install -r requirements.txt",
        "Collecting flask==3.0.0\n\
           Downloading flask-3.0.0-py3-none-any.whl (101 kB)\n\
         Collecting werkzeug>=3.0.0\n\
           Using cached werkzeug-3.0.1-py3-none-any.whl (226 kB)\n\
         Collecting jinja2>=3.1.2\n\
           Using cached Jinja2-3.1.3-py3-none-any.whl (133 kB)\n\
         Collecting click>=8.1.3\n\
           Using cached click-8.1.7-py3-none-any.whl (97 kB)\n\
         Collecting itsdangerous>=2.1.2\n\
           Using cached itsdangerous-2.1.2-py3-none-any.whl (15 kB)\n\
         Collecting markupsafe>=2.0\n\
           Using cached MarkupSafe-2.1.5-cp312-cp312-manylinux_2_17_x86_64.whl (23 kB)\n\
         Collecting blinker>=1.6.2\n\
           Using cached blinker-1.7.0-py3-none-any.whl (13 kB)\n\
         Installing collected packages: markupsafe, itsdangerous, click, blinker, werkzeug, jinja2, flask\n\
         Successfully installed blinker-1.7.0 click-8.1.7 flask-3.0.0 itsdangerous-2.1.2 jinja2-3.1.3 markupsafe-2.1.5 werkzeug-3.0.1\n",
    );
}

#[test]
fn pattern_poetry_install() {
    assert_pattern(
        "poetry_install",
        "poetry install",
        "Installing dependencies from lock file\n\n\
         Package operations: 15 installs, 2 updates, 0 removals\n\n\
           - Installing markupsafe (2.1.5)\n\
           - Installing jinja2 (3.1.3)\n\
           - Installing click (8.1.7)\n\
           - Installing werkzeug (3.0.1)\n\
           - Installing flask (3.0.0)\n\
           - Installing requests (2.31.0)\n\
           - Updating certifi (2023.11.17 -> 2024.2.2)\n\
           - Updating urllib3 (2.1.0 -> 2.2.0)\n\
           - Installing pytest (8.0.0)\n\
           - Installing coverage (7.4.0)\n\n\
         Installing the current project: myapp (0.1.0)\n",
    );
}

#[test]
fn pattern_mypy() {
    assert_pattern(
        "mypy_check",
        "mypy src/",
        "src/auth.py:12: error: Argument 1 to \"verify\" has incompatible type \"Optional[str]\"; expected \"str\"  [arg-type]\n\
         src/auth.py:25: error: Missing return statement  [return]\n\
         src/auth.py:40: error: Incompatible return value type (got \"None\", expected \"User\")  [return-value]\n\
         src/db.py:8: error: Incompatible types in assignment (expression has type \"None\", variable has type \"Connection\")  [assignment]\n\
         src/db.py:15: error: Name \"cursor\" is not defined  [name-defined]\n\
         src/db.py:33: error: \"Connection\" has no attribute \"execute_many\"  [attr-defined]\n\
         src/api.py:30: error: \"Dict[str, Any]\" has no attribute \"items\"  [attr-defined]\n\
         src/api.py:45: error: Argument 1 to \"loads\" has incompatible type \"bytes\"; expected \"str\"  [arg-type]\n\
         src/utils.py:5: error: Missing type parameters for generic type \"Dict\"  [type-arg]\n\
         src/utils.py:12: error: Need type annotation for \"cache\" (hint: \"cache: Dict[str, Any] = ...\")  [var-annotated]\n\
         Found 10 errors in 4 files (checked 12 source files)\n",
    );
}

#[test]
fn pattern_ruff() {
    assert_pattern(
        "ruff_check",
        "ruff check src/",
        "src/auth.py:3:1: F401 [*] `os` imported but unused\n\
         src/auth.py:15:5: E712 Comparison to `True` should be `if cond:` or `if cond is True:`\n\
         src/auth.py:22:1: F841 Local variable `result` is assigned to but never used\n\
         src/db.py:8:1: E501 Line too long (120 > 88)\n\
         src/db.py:22:5: W291 Trailing whitespace\n\
         src/db.py:35:1: E302 Expected 2 blank lines after class or function definition, found 1\n\
         src/api.py:1:1: I001 [*] Import block is un-sorted or un-formatted\n\
         src/api.py:15:5: B006 Do not use mutable data structures for argument defaults\n\
         src/api.py:28:1: E303 Too many blank lines (3)\n\
         src/utils.py:10:5: B006 Do not use mutable data structures for argument defaults\n\
         src/utils.py:18:9: SIM108 [*] Use ternary operator\n\
         src/config.py:5:1: F811 Redefinition of unused `load` from line 3\n\
         Found 12 errors.\n\
         [*] 3 fixable with the `--fix` option.\n",
    );
}

#[test]
fn pattern_pytest() {
    assert_pattern(
        "pytest",
        "pytest tests/ -v",
        "============================= test session starts ==============================\n\
         platform linux -- Python 3.12.0, pytest-8.0.0, pluggy-1.4.0\n\
         rootdir: /home/user/project\n\
         collected 20 items\n\n\
         tests/test_auth.py::test_login PASSED                                   [  5%]\n\
         tests/test_auth.py::test_logout PASSED                                  [ 10%]\n\
         tests/test_auth.py::test_register PASSED                                [ 15%]\n\
         tests/test_db.py::test_connect PASSED                                   [ 20%]\n\
         tests/test_db.py::test_query PASSED                                     [ 25%]\n\
         tests/test_db.py::test_migrate PASSED                                   [ 30%]\n\
         tests/test_api.py::test_health PASSED                                   [ 35%]\n\
         tests/test_api.py::test_create PASSED                                   [ 40%]\n\
         tests/test_api.py::test_delete FAILED                                   [ 45%]\n\
         tests/test_api.py::test_update PASSED                                   [ 50%]\n\
         tests/test_cache.py::test_set PASSED                                    [ 55%]\n\
         tests/test_cache.py::test_get PASSED                                    [ 60%]\n\
         tests/test_cache.py::test_expire PASSED                                 [ 65%]\n\
         tests/test_cache.py::test_evict PASSED                                  [ 70%]\n\
         tests/test_config.py::test_load PASSED                                  [ 75%]\n\
         tests/test_config.py::test_validate PASSED                              [ 80%]\n\
         tests/test_config.py::test_defaults PASSED                              [ 85%]\n\
         tests/test_utils.py::test_parse PASSED                                  [ 90%]\n\
         tests/test_utils.py::test_format PASSED                                 [ 95%]\n\
         tests/test_utils.py::test_sanitize PASSED                               [100%]\n\n\
         =================================== FAILURES ===================================\n\
         ________________________________ test_delete ___________________________________\n\n\
             def test_delete():\n\
         >       assert response.status_code == 204\n\
         E       AssertionError: assert 403 == 204\n\n\
         tests/test_api.py:45: AssertionError\n\
         =========================== short test summary info ============================\n\
         FAILED tests/test_api.py::test_delete - AssertionError: assert 403 == 204\n\
         ========================= 19 passed, 1 failed in 2.34s =========================\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// terraform / kubectl / helm
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_terraform_plan() {
    assert_pattern(
        "terraform_plan",
        "terraform plan",
        "Terraform used the selected providers to generate the following execution plan.\n\
         Resource actions are indicated with the following symbols:\n\
           + create\n\
           ~ update in-place\n\n\
         Terraform will perform the following actions:\n\n\
           # aws_instance.web will be created\n\
           + resource \"aws_instance\" \"web\" {\n\
               + ami           = \"ami-0c55b159cbfafe1f0\"\n\
               + instance_type = \"t2.micro\"\n\
               + tags          = {\n\
                   + \"Name\" = \"web-server\"\n\
                 }\n\
             }\n\n\
           # aws_s3_bucket.data will be updated in-place\n\
           ~ resource \"aws_s3_bucket\" \"data\" {\n\
               ~ tags = {\n\
                   + \"Environment\" = \"production\"\n\
                 }\n\
             }\n\n\
         Plan: 1 to add, 1 to change, 0 to destroy.\n",
    );
}

#[test]
fn pattern_kubectl_get_pods() {
    assert_pattern(
        "kubectl_get_pods",
        "kubectl get pods -n default",
        "NAME                          READY   STATUS    RESTARTS   AGE\n\
         web-app-6b8c4d5f9-abc12      1/1     Running   0          3d\n\
         web-app-6b8c4d5f9-def34      1/1     Running   0          3d\n\
         web-app-6b8c4d5f9-ghi56      1/1     Running   1          3d\n\
         redis-master-0               1/1     Running   0          7d\n\
         redis-slave-6fc55b5b-jkl78   1/1     Running   0          7d\n\
         redis-slave-6fc55b5b-mno90   1/1     Running   0          7d\n\
         postgres-0                   1/1     Running   0          14d\n\
         celery-worker-pqr12          1/1     Running   2          3d\n\
         celery-beat-stu34            1/1     Running   0          3d\n\
         nginx-ingress-vwx56          1/1     Running   0          30d\n",
    );
}

#[test]
fn pattern_helm_list() {
    assert_pattern(
        "helm_list",
        "helm list -A",
        "NAME          \tNAMESPACE  \tREVISION\tUPDATED                                \tSTATUS  \tCHART              \tAPP VERSION\n\
         cert-manager  \tcert-mgr   \t3       \t2024-01-15 10:23:45.123456 +0000 UTC   \tdeployed\tcert-manager-1.13.3\tv1.13.3    \n\
         ingress-nginx \tingress    \t5       \t2024-02-20 14:30:00.000000 +0000 UTC   \tdeployed\tingress-nginx-4.9.0\t1.9.5      \n\
         prometheus    \tmonitoring \t2       \t2024-03-01 09:00:00.000000 +0000 UTC   \tdeployed\tprometheus-25.8.0  \tv2.48.0    \n\
         grafana       \tmonitoring \t4       \t2024-03-10 11:15:00.000000 +0000 UTC   \tdeployed\tgrafana-7.0.19     \t10.2.3     \n\
         redis         \tdefault    \t1       \t2024-03-15 08:45:00.000000 +0000 UTC   \tdeployed\tredis-18.6.1       \t7.2.4      \n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// golang
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_go_test() {
    assert_pattern(
        "go_test",
        "go test ./...",
        "ok  \tmyapp/auth\t0.234s\n\
         ok  \tmyapp/db  \t1.456s\n\
         ok  \tmyapp/api \t0.789s\n\
         ok  \tmyapp/cache\t0.123s\n\
         --- FAIL: TestUserCreate (0.01s)\n\
             user_test.go:45: expected status 201, got 400\n\
         FAIL\tmyapp/handlers\t0.567s\n\
         ok  \tmyapp/middleware\t0.234s\n\
         ok  \tmyapp/config\t0.100s\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// eslint / typescript / prettier
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_eslint() {
    assert_pattern(
        "eslint",
        "eslint src/",
        "\n/home/user/project/src/App.tsx\n\
           3:1   warning  Unexpected console statement       no-console\n\
           7:10  error    'unused' is defined but never used  no-unused-vars\n\
          15:5   warning  Unexpected console statement       no-console\n\n\
         /home/user/project/src/utils.ts\n\
           1:1   error    Missing return type on function    @typescript-eslint/explicit-function-return-type\n\
          12:3   warning  Unexpected any                     @typescript-eslint/no-explicit-any\n\
          18:1   error    Prefer const over let              prefer-const\n\n\
         /home/user/project/src/api.ts\n\
           5:10  error    'axios' is not defined             no-undef\n\n\
         \u{2716} 7 problems (4 errors, 3 warnings)\n\
           1 error and 1 warning potentially fixable with the `--fix` option.\n",
    );
}

#[test]
fn pattern_typescript() {
    assert_pattern(
        "typescript",
        "tsc --noEmit",
        "src/App.tsx(12,5): error TS2322: Type 'string' is not assignable to type 'number'.\n\
         src/App.tsx(25,10): error TS2345: Argument of type 'null' is not assignable to parameter of type 'User'.\n\
         src/utils.ts(3,1): error TS7006: Parameter 'x' implicitly has an 'any' type.\n\
         src/api.ts(15,3): error TS2304: Cannot find name 'Response'.\n\
         src/types.ts(8,5): error TS2739: Type '{}' is missing the following properties from type 'Config': host, port\n\n\
         Found 5 errors in 4 files.\n\n\
         Errors  Files\n\
              2  src/App.tsx\n\
              1  src/utils.ts\n\
              1  src/api.ts\n\
              1  src/types.ts\n",
    );
}

#[test]
fn pattern_prettier() {
    assert_pattern(
        "prettier",
        "prettier --check src/",
        "Checking formatting...\n\
         [warn] src/App.tsx\n\
         [warn] src/utils.ts\n\
         [warn] src/api.ts\n\
         [warn] src/config.ts\n\
         [warn] src/types.ts\n\
         [warn] Code style issues found in 5 files. Run Prettier to fix.\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// maven / make / just
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_maven_build() {
    assert_pattern(
        "maven_build",
        "mvn clean install",
        "[INFO] Scanning for projects...\n\
         [INFO] \n\
         [INFO] ----------------------< com.example:myapp >-----------------------\n\
         [INFO] Building myapp 1.0-SNAPSHOT\n\
         [INFO] --------------------------------[ jar ]---------------------------------\n\
         [INFO] \n\
         [INFO] --- maven-clean-plugin:3.2.0:clean (default-clean) @ myapp ---\n\
         [INFO] Deleting /home/user/myapp/target\n\
         [INFO] \n\
         [INFO] --- maven-resources-plugin:3.3.1:resources (default-resources) @ myapp ---\n\
         [INFO] Copying 3 resources\n\
         [INFO] \n\
         [INFO] --- maven-compiler-plugin:3.11.0:compile (default-compile) @ myapp ---\n\
         [INFO] Changes detected - recompiling the module!\n\
         [INFO] Compiling 42 source files to /home/user/myapp/target/classes\n\
         [INFO] \n\
         [INFO] --- maven-surefire-plugin:3.2.2:test (default-test) @ myapp ---\n\
         [INFO] Tests run: 28, Failures: 0, Errors: 0, Skipped: 2\n\
         [INFO] \n\
         [INFO] --- maven-jar-plugin:3.3.0:jar (default-jar) @ myapp ---\n\
         [INFO] Building jar: /home/user/myapp/target/myapp-1.0-SNAPSHOT.jar\n\
         [INFO] \n\
         [INFO] --- maven-install-plugin:3.1.1:install (default-install) @ myapp ---\n\
         [INFO] Installing /home/user/myapp/target/myapp-1.0-SNAPSHOT.jar\n\
         [INFO] ------------------------------------------------------------------------\n\
         [INFO] BUILD SUCCESS\n\
         [INFO] ------------------------------------------------------------------------\n\
         [INFO] Total time:  12.345 s\n\
         [INFO] Finished at: 2024-06-15T10:30:00+02:00\n\
         [INFO] ------------------------------------------------------------------------\n",
    );
}

#[test]
fn pattern_make() {
    assert_pattern(
        "make_build",
        "make -j4",
        "cc -Wall -O2 -c src/main.c -o build/main.o\n\
         cc -Wall -O2 -c src/parser.c -o build/parser.o\n\
         cc -Wall -O2 -c src/lexer.c -o build/lexer.o\n\
         cc -Wall -O2 -c src/codegen.c -o build/codegen.o\n\
         cc -Wall -O2 -c src/optimizer.c -o build/optimizer.o\n\
         cc -Wall -O2 -c src/utils.c -o build/utils.o\n\
         cc -Wall -O2 -c src/debug.c -o build/debug.o\n\
         cc -Wall -O2 -c src/error.c -o build/error.o\n\
         cc build/main.o build/parser.o build/lexer.o build/codegen.o build/optimizer.o build/utils.o build/debug.o build/error.o -o bin/compiler\n",
    );
}

#[test]
fn pattern_just() {
    assert_pattern(
        "just_list",
        "just --list",
        "Available recipes:\n\
             build         # Build the project\n\
             clean         # Remove build artifacts\n\
             dev           # Start development server\n\
             fmt           # Format source code\n\
             lint          # Run linters\n\
             release       # Create a release build\n\
             test          # Run all tests\n\
             test-e2e      # Run end-to-end tests\n\
             test-unit     # Run unit tests\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// playwright / next/vite build
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_playwright() {
    assert_pattern(
        "playwright",
        "npx playwright test",
        "\nRunning 12 tests using 4 workers\n\n\
           \u{2713}  1 [chromium] > tests/login.spec.ts:5:3 > should login (2.3s)\n\
           \u{2713}  2 [chromium] > tests/login.spec.ts:15:3 > should show error (1.2s)\n\
           \u{2713}  3 [firefox] > tests/login.spec.ts:5:3 > should login (3.1s)\n\
           \u{2713}  4 [firefox] > tests/login.spec.ts:15:3 > should show error (1.8s)\n\
           \u{2713}  5 [webkit] > tests/login.spec.ts:5:3 > should login (2.8s)\n\
           \u{2713}  6 [webkit] > tests/login.spec.ts:15:3 > should show error (1.5s)\n\
           \u{2713}  7 [chromium] > tests/dashboard.spec.ts:5:3 > should load (1.1s)\n\
           \u{2713}  8 [chromium] > tests/dashboard.spec.ts:12:3 > should filter (0.9s)\n\
           \u{2713}  9 [firefox] > tests/dashboard.spec.ts:5:3 > should load (1.4s)\n\
           \u{2713} 10 [firefox] > tests/dashboard.spec.ts:12:3 > should filter (1.2s)\n\
           \u{2713} 11 [webkit] > tests/dashboard.spec.ts:5:3 > should load (1.3s)\n\
           \u{2713} 12 [webkit] > tests/dashboard.spec.ts:12:3 > should filter (1.0s)\n\n\
           12 passed (15.6s)\n",
    );
}

#[test]
fn pattern_next_build() {
    assert_pattern(
        "next_build",
        "next build",
        "   Creating an optimized production build...\n\
            Compiled successfully.\n\n\
         Route (app)                              Size     First Load JS\n\
         \u{250c} \u{25cb} /                                    5.23 kB        89.2 kB\n\
         \u{251c} \u{25cb} /about                               2.14 kB        86.1 kB\n\
         \u{251c} \u{25cb} /api/health                          0 B            83.9 kB\n\
         \u{251c} \u{25cf} /dashboard                           12.4 kB        96.4 kB\n\
         \u{2514} \u{25cb} /login                               3.56 kB        87.5 kB\n\
         + First Load JS shared by all            83.9 kB\n\
           \u{251c} chunks/main-abc123.js                52.3 kB\n\
           \u{251c} chunks/pages/_app-def456.js          28.1 kB\n\
           \u{2514} other shared chunks (total)           3.50 kB\n\n\
         \u{25cb}  (Static)   prerendered as static content\n\
         \u{25cf}  (Dynamic)  server-rendered on demand\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// ruby / swift / zig / cmake / ninja / bazel
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_ruby_rubocop() {
    assert_pattern(
        "ruby_rubocop",
        "rubocop app/",
        "Inspecting 25 files\n\
         ..C..W..C.C..W....C..W..C\n\n\
         Offenses:\n\n\
         app/models/user.rb:3:5: C: Style/StringLiterals: Prefer single-quoted strings.\n\
         app/models/user.rb:15:3: W: Lint/UselessAssignment: Useless assignment to variable.\n\
         app/controllers/users_controller.rb:8:1: C: Metrics/MethodLength: Method has too many lines.\n\
         app/controllers/users_controller.rb:22:5: C: Style/GuardClause: Use a guard clause.\n\
         app/controllers/posts_controller.rb:12:3: W: Lint/UnusedMethodArgument: Unused method argument.\n\
         app/services/auth_service.rb:5:1: C: Layout/LineLength: Line is too long. [125/120]\n\
         app/services/auth_service.rb:18:3: C: Style/RedundantReturn: Redundant return.\n\
         app/helpers/application_helper.rb:3:5: W: Lint/SuppressedException: Do not suppress exceptions.\n\
         app/mailers/user_mailer.rb:10:1: C: Style/FrozenStringLiteralComment: Missing frozen string literal comment.\n\n\
         25 files inspected, 9 offenses detected\n",
    );
}

#[test]
fn pattern_swift_build() {
    assert_pattern(
        "swift_build",
        "swift build",
        "Building for debugging...\n\
         [1/8] Compiling MyApp main.swift\n\
         [2/8] Compiling MyApp Config.swift\n\
         [3/8] Compiling MyApp Router.swift\n\
         [4/8] Compiling MyApp Models.swift\n\
         [5/8] Compiling MyApp Database.swift\n\
         [6/8] Compiling MyApp Auth.swift\n\
         [7/8] Compiling MyApp Utils.swift\n\
         [8/8] Linking MyApp\n\
         Build complete! (5.23s)\n",
    );
}

#[test]
fn pattern_zig_build() {
    assert_pattern(
        "zig_build",
        "zig build",
        "info: Build succeeded.\n\
         info: Semantic Analysis: 0.234s\n\
         info: Code Generation: 1.456s\n\
         info: Linking: 0.789s\n\
         info: Total: 2.479s\n\
         info: Memory: 156 MiB\n",
    );
}

#[test]
fn pattern_cmake() {
    assert_pattern(
        "cmake_build",
        "cmake --build build",
        "[  8%] Building CXX object src/CMakeFiles/mylib.dir/parser.cpp.o\n\
         [ 16%] Building CXX object src/CMakeFiles/mylib.dir/lexer.cpp.o\n\
         [ 25%] Building CXX object src/CMakeFiles/mylib.dir/codegen.cpp.o\n\
         [ 33%] Building CXX object src/CMakeFiles/mylib.dir/optimizer.cpp.o\n\
         [ 41%] Linking CXX static library libmylib.a\n\
         [ 41%] Built target mylib\n\
         [ 50%] Building CXX object app/CMakeFiles/myapp.dir/main.cpp.o\n\
         [ 58%] Linking CXX executable myapp\n\
         [ 58%] Built target myapp\n\
         [ 66%] Building CXX object tests/CMakeFiles/tests.dir/test_parser.cpp.o\n\
         [ 75%] Building CXX object tests/CMakeFiles/tests.dir/test_lexer.cpp.o\n\
         [ 83%] Linking CXX executable tests\n\
         [ 83%] Built target tests\n\
         [100%] Built target all\n",
    );
}

#[test]
fn pattern_ninja() {
    assert_pattern(
        "ninja_build",
        "ninja -C build",
        "[1/10] CXX obj/src/main.o\n\
         [2/10] CXX obj/src/parser.o\n\
         [3/10] CXX obj/src/lexer.o\n\
         [4/10] CXX obj/src/codegen.o\n\
         [5/10] CXX obj/src/optimizer.o\n\
         [6/10] CXX obj/src/utils.o\n\
         [7/10] LINK mylib.a\n\
         [8/10] CXX obj/app/main.o\n\
         [9/10] LINK myapp\n\
         [10/10] STAMP build.stamp\n",
    );
}

#[test]
fn pattern_bazel_build() {
    assert_pattern(
        "bazel_build",
        "bazel build //...",
        "INFO: Analyzed 15 targets (3 packages loaded, 42 targets configured).\n\
         INFO: Found 15 targets...\n\
         [0 / 5] Compiling src/main.cc\n\
         [1 / 5] Compiling src/parser.cc\n\
         [2 / 5] Compiling src/lexer.cc\n\
         [3 / 5] Linking //src:myapp\n\
         [4 / 5] Compiling tests/test_parser.cc\n\
         [5 / 5] Linking //tests:all_tests\n\
         INFO: Elapsed time: 12.345s, Critical Path: 8.234s\n\
         INFO: 5 processes: 2 internal, 3 linux-sandbox.\n\
         INFO: Build completed successfully, 5 total actions\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// dotnet / flutter / ansible
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_dotnet_build() {
    assert_pattern(
        "dotnet_build",
        "dotnet build",
        "MSBuild version 17.8.3+195e7f5a3 for .NET\n\
           Determining projects to restore...\n\
           All projects are up-to-date for restore.\n\
           MyApp -> /home/user/MyApp/bin/Debug/net8.0/MyApp.dll\n\n\
         Build succeeded.\n\
             0 Warning(s)\n\
             0 Error(s)\n\n\
         Time Elapsed 00:00:03.45\n",
    );
}

#[test]
fn pattern_flutter_analyze() {
    assert_pattern(
        "flutter_analyze",
        "flutter analyze",
        "Analyzing myapp...\n\n\
           info - Unused import - lib/old_widget.dart:1:8 - unused_import\n\
           info - Unused import - lib/utils.dart:3:8 - unused_import\n\
           warning - The parameter 'context' is not used - lib/home.dart:15:30 - unused_element\n\
           error - The method 'build' isn't defined for the type 'Widget' - lib/broken.dart:22:5 - undefined_method\n\n\
         4 issues found. (1 error, 1 warning, and 2 infos)\n",
    );
}

#[test]
fn pattern_ansible_playbook() {
    assert_pattern(
        "ansible_playbook",
        "ansible-playbook deploy.yml",
        "\nPLAY [Deploy application] ****************************************************\n\n\
         TASK [Gathering Facts] ********************************************************\n\
         ok: [web1.example.com]\n\
         ok: [web2.example.com]\n\
         ok: [web3.example.com]\n\n\
         TASK [Install dependencies] ***************************************************\n\
         changed: [web1.example.com]\n\
         changed: [web2.example.com]\n\
         changed: [web3.example.com]\n\n\
         TASK [Deploy code] ************************************************************\n\
         changed: [web1.example.com]\n\
         changed: [web2.example.com]\n\
         changed: [web3.example.com]\n\n\
         TASK [Restart service] ********************************************************\n\
         changed: [web1.example.com]\n\
         changed: [web2.example.com]\n\
         changed: [web3.example.com]\n\n\
         PLAY RECAP *********************************************************************\n\
         web1.example.com           : ok=4    changed=3    unreachable=0    failed=0    skipped=0\n\
         web2.example.com           : ok=4    changed=3    unreachable=0    failed=0    skipped=0\n\
         web3.example.com           : ok=4    changed=3    unreachable=0    failed=0    skipped=0\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// aws / psql / mysql / prisma
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_aws_ec2() {
    assert_pattern(
        "aws_ec2",
        "aws ec2 describe-instances",
        r#"{"Reservations":[{"Instances":[{"InstanceId":"i-abc123","InstanceType":"t3.micro","State":{"Name":"running"},"PublicIpAddress":"54.1.2.3","PrivateIpAddress":"10.0.1.5","Tags":[{"Key":"Name","Value":"web-1"}]},{"InstanceId":"i-def456","InstanceType":"t3.small","State":{"Name":"running"},"PublicIpAddress":"54.4.5.6","PrivateIpAddress":"10.0.1.6","Tags":[{"Key":"Name","Value":"web-2"}]},{"InstanceId":"i-ghi789","InstanceType":"m5.large","State":{"Name":"stopped"},"PrivateIpAddress":"10.0.2.10","Tags":[{"Key":"Name","Value":"worker-1"}]}]}]}"#,
    );
}

#[test]
fn pattern_psql() {
    assert_pattern(
        "psql_query",
        "psql -c 'SELECT * FROM users'",
        " id |   name    |        email         | role      | active | created_at\n\
         ----+-----------+----------------------+-----------+--------+------------------------\n\
           1 | Alice     | alice@example.com    | admin     | t      | 2024-01-15 10:23:45+00\n\
           2 | Bob       | bob@example.com      | user      | t      | 2024-01-16 08:00:00+00\n\
           3 | Charlie   | charlie@example.com  | user      | f      | 2024-01-17 12:30:00+00\n\
           4 | Diana     | diana@example.com    | moderator | t      | 2024-01-18 09:15:00+00\n\
           5 | Eve       | eve@example.com      | user      | t      | 2024-01-19 14:00:00+00\n\
           6 | Frank     | frank@example.com    | user      | f      | 2024-01-20 06:00:00+00\n\
           7 | Grace     | grace@example.com    | admin     | t      | 2024-01-21 11:45:00+00\n\
           8 | Hank      | hank@example.com     | user      | t      | 2024-01-22 15:30:00+00\n\
           9 | Irene     | irene@example.com    | user      | t      | 2024-01-23 09:00:00+00\n\
          10 | Jack      | jack@example.com     | moderator | t      | 2024-01-24 13:15:00+00\n\
         (10 rows)\n",
    );
}

#[test]
fn pattern_mysql() {
    assert_pattern(
        "mysql_query",
        "mysql -e 'SHOW TABLES'",
        "+-------------------+\n\
         | Tables_in_mydb    |\n\
         +-------------------+\n\
         | users             |\n\
         | posts             |\n\
         | comments          |\n\
         | categories        |\n\
         | tags              |\n\
         | post_tags         |\n\
         | sessions          |\n\
         | migrations        |\n\
         | audit_logs        |\n\
         | permissions       |\n\
         | roles             |\n\
         | role_permissions  |\n\
         | user_roles        |\n\
         | api_tokens        |\n\
         | rate_limits       |\n\
         +-------------------+\n\
         15 rows in set (0.00 sec)\n",
    );
}

#[test]
fn pattern_prisma_migrate() {
    assert_pattern(
        "prisma_migrate",
        "npx prisma migrate dev",
        "Environment variables loaded from .env\n\
         Prisma schema loaded from prisma/schema.prisma\n\
         Datasource \"db\": PostgreSQL database \"mydb\", schema \"public\" at \"localhost:5432\"\n\n\
         Applying migration `20240115_add_users`\n\
         Applying migration `20240116_add_posts`\n\
         Applying migration `20240117_add_comments`\n\n\
         The following migration(s) have been applied:\n\n\
         migrations/\n\
           \u{2514}\u{2500} 20240115_add_users/\n\
             \u{2514}\u{2500} migration.sql\n\
           \u{2514}\u{2500} 20240116_add_posts/\n\
             \u{2514}\u{2500} migration.sql\n\
           \u{2514}\u{2500} 20240117_add_comments/\n\
             \u{2514}\u{2500} migration.sql\n\n\
         Your database is now in sync with your schema.\n\n\
         \u{2714} Generated Prisma Client (v5.8.0) to ./node_modules/@prisma/client in 234ms\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// composer / artisan / mix
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_composer_install() {
    assert_pattern(
        "composer_install",
        "composer install",
        "Installing dependencies from lock file (including require-dev)\n\
         Verifying lock file contents can be installed on current platform.\n\
         Package operations: 42 installs, 0 updates, 0 removals\n\
           - Downloading laravel/framework (v11.0.0)\n\
           - Downloading nesbot/carbon (3.0.0)\n\
           - Downloading doctrine/dbal (4.0.0)\n\
           - Installing laravel/framework (v11.0.0): Extracting archive\n\
           - Installing nesbot/carbon (3.0.0): Extracting archive\n\
           - Installing doctrine/dbal (4.0.0): Extracting archive\n\
         Generating optimized autoload files\n\
         > Illuminate\\Foundation\\ComposerScripts::postAutoloadDump\n\
         > @php artisan package:discover --ansi\n\n\
            INFO  Discovering packages.\n\n\
           laravel/sail ............................................................. DONE\n\
           laravel/sanctum .......................................................... DONE\n\
           laravel/tinker ........................................................... DONE\n\n\
         80 packages you are using are looking for funding.\n\
         Use the `composer fund` command to find out more!\n",
    );
}

#[test]
fn pattern_artisan() {
    assert_pattern(
        "artisan_route_list",
        "php artisan route:list",
        "+--------+-----------+-------------------+------------------+-------------------------------------------------+\n\
         | Domain | Method    | URI               | Name             | Action                                          |\n\
         +--------+-----------+-------------------+------------------+-------------------------------------------------+\n\
         |        | GET|HEAD  | /                 | home             | App\\Http\\Controllers\\HomeController@index        |\n\
         |        | GET|HEAD  | api/users         | users.index      | App\\Http\\Controllers\\Api\\UserController@index    |\n\
         |        | POST      | api/users         | users.store      | App\\Http\\Controllers\\Api\\UserController@store    |\n\
         |        | GET|HEAD  | api/users/{user}  | users.show       | App\\Http\\Controllers\\Api\\UserController@show     |\n\
         |        | PUT|PATCH | api/users/{user}  | users.update     | App\\Http\\Controllers\\Api\\UserController@update   |\n\
         |        | DELETE    | api/users/{user}  | users.destroy    | App\\Http\\Controllers\\Api\\UserController@destroy  |\n\
         |        | GET|HEAD  | api/posts         | posts.index      | App\\Http\\Controllers\\Api\\PostController@index    |\n\
         |        | POST      | api/posts         | posts.store      | App\\Http\\Controllers\\Api\\PostController@store    |\n\
         +--------+-----------+-------------------+------------------+-------------------------------------------------+\n",
    );
}

#[test]
fn pattern_mix_test() {
    assert_pattern(
        "mix_test",
        "mix test",
        "Compiling 3 files (.ex)\n\
         Generated myapp app\n\
         ....\n\n\
         Finished in 0.8 seconds (0.3s async, 0.5s sync)\n\
         4 tests, 0 failures\n\n\
         Randomized with seed 12345\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// grep / find / fd / ls / curl / wget / env
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_grep() {
    let mut lines = String::new();
    let files = &[
        "src/main.rs",
        "src/lib.rs",
        "src/utils.rs",
        "src/config.rs",
        "src/api.rs",
        "src/db.rs",
        "src/cache.rs",
        "src/auth.rs",
        "src/middleware.rs",
        "src/router.rs",
    ];
    for (fi, file) in files.iter().enumerate() {
        for j in 0..4 {
            let line_num = fi * 20 + j * 5 + 3;
            lines.push_str(&format!(
                "{file}:{line_num}: // TODO: task {}\n",
                fi * 4 + j + 1
            ));
        }
    }
    assert_pattern("grep_search", "grep -rn TODO src/", &lines);
}

#[test]
fn pattern_find() {
    assert_pattern(
        "find",
        "find src/ -name '*.rs'",
        "src/main.rs\n\
         src/lib.rs\n\
         src/config.rs\n\
         src/utils.rs\n\
         src/api/mod.rs\n\
         src/api/handlers.rs\n\
         src/api/middleware.rs\n\
         src/api/routes.rs\n\
         src/api/auth.rs\n\
         src/db/mod.rs\n\
         src/db/models.rs\n\
         src/db/migrations.rs\n\
         src/db/pool.rs\n\
         src/db/schema.rs\n\
         src/auth/mod.rs\n\
         src/auth/jwt.rs\n\
         src/auth/oauth.rs\n\
         src/auth/session.rs\n\
         src/cache/mod.rs\n\
         src/cache/redis.rs\n\
         src/cache/memory.rs\n\
         src/cache/policy.rs\n\
         src/core/mod.rs\n\
         src/core/engine.rs\n\
         src/core/parser.rs\n",
    );
}

#[test]
fn pattern_fd() {
    assert_pattern(
        "fd_search",
        "fd '.rs$' src/",
        "src/main.rs\n\
         src/lib.rs\n\
         src/config.rs\n\
         src/utils.rs\n\
         src/api/mod.rs\n\
         src/api/handlers.rs\n\
         src/api/middleware.rs\n\
         src/api/routes.rs\n\
         src/db/mod.rs\n\
         src/db/models.rs\n\
         src/db/migrations.rs\n\
         src/db/pool.rs\n\
         src/auth/mod.rs\n\
         src/auth/jwt.rs\n\
         src/auth/oauth.rs\n\
         src/cache/mod.rs\n\
         src/cache/redis.rs\n\
         src/cache/memory.rs\n",
    );
}

#[test]
fn pattern_ls() {
    assert_pattern(
        "ls",
        "ls -la",
        "total 128\n\
         drwxr-xr-x  12 user user  384 Jan 15 10:23 .\n\
         drwxr-xr-x   5 user user  160 Jan 10 08:00 ..\n\
         drwxr-xr-x   8 user user  256 Jan 15 10:23 .git\n\
         -rw-r--r--   1 user user   45 Jan 15 10:23 .gitignore\n\
         -rw-r--r--   1 user user  234 Jan 14 09:00 Cargo.toml\n\
         -rw-r--r--   1 user user 1234 Jan 14 09:00 Cargo.lock\n\
         -rw-r--r--   1 user user  567 Jan 13 12:30 README.md\n\
         drwxr-xr-x   4 user user  128 Jan 15 10:23 src\n\
         drwxr-xr-x   3 user user   96 Jan 12 14:00 tests\n\
         drwxr-xr-x   2 user user   64 Jan 11 11:00 benches\n\
         -rw-r--r--   1 user user  890 Jan 10 08:00 LICENSE\n\
         drwxr-xr-x   2 user user   64 Jan 15 10:23 target\n",
    );
}

#[test]
fn pattern_curl() {
    assert_pattern(
        "curl",
        "curl https://api.example.com/users",
        r#"{"users":[{"id":1,"name":"Alice","email":"alice@example.com","role":"admin"},{"id":2,"name":"Bob","email":"bob@example.com","role":"user"},{"id":3,"name":"Charlie","email":"charlie@example.com","role":"user"},{"id":4,"name":"Diana","email":"diana@example.com","role":"mod"},{"id":5,"name":"Eve","email":"eve@example.com","role":"user"}],"total":5,"page":1}"#,
    );
}

#[test]
fn pattern_wget() {
    assert_pattern(
        "wget",
        "wget https://example.com/data.tar.gz",
        "--2024-01-15 10:23:45--  https://example.com/data.tar.gz\n\
         Resolving example.com (example.com)... 93.184.216.34\n\
         Connecting to example.com (example.com)|93.184.216.34|:443... connected.\n\
         HTTP request sent, awaiting response... 200 OK\n\
         Length: 10485760 (10M) [application/gzip]\n\
         Saving to: 'data.tar.gz'\n\n\
         data.tar.gz         100%[===================>]  10.00M  5.23MB/s    in 1.9s\n\n\
         2024-01-15 10:23:47 (5.23 MB/s) - 'data.tar.gz' saved [10485760/10485760]\n",
    );
}

#[test]
fn pattern_env() {
    assert_pattern(
        "env_filter",
        "env",
        "HOME=/home/user\n\
         USER=user\n\
         SHELL=/bin/bash\n\
         PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/snap/bin\n\
         LANG=en_US.UTF-8\n\
         TERM=xterm-256color\n\
         EDITOR=vim\n\
         DISPLAY=:0\n\
         XDG_SESSION_TYPE=x11\n\
         XDG_RUNTIME_DIR=/run/user/1000\n\
         DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus\n\
         SSH_AUTH_SOCK=/tmp/ssh-abc123/agent.1234\n\
         GPG_AGENT_INFO=/tmp/gpg-def456/S.gpg-agent:1:1\n\
         DOCKER_HOST=unix:///var/run/docker.sock\n\
         GOPATH=/home/user/go\n\
         CARGO_HOME=/home/user/.cargo\n\
         RUSTUP_HOME=/home/user/.rustup\n\
         NVM_DIR=/home/user/.nvm\n\
         NODE_VERSION=20.11.0\n\
         PYTHON_VERSION=3.12.0\n\
         VIRTUAL_ENV=/home/user/project/.venv\n\
         CONDA_DEFAULT_ENV=base\n\
         AWS_PROFILE=production\n\
         KUBECONFIG=/home/user/.kube/config\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// systemd / composer / sysinfo
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_systemctl_status() {
    assert_pattern(
        "systemctl_status",
        "systemctl status nginx",
        "\u{25cf} nginx.service - A high performance web server and a reverse proxy server\n\
              Loaded: loaded (/lib/systemd/system/nginx.service; enabled; vendor preset: enabled)\n\
              Active: active (running) since Mon 2024-01-15 10:23:45 UTC; 3 days ago\n\
                Docs: man:nginx(8)\n\
             Process: 1234 ExecStartPre=/usr/sbin/nginx -t -q -g daemon on; (code=exited, status=0/SUCCESS)\n\
             Process: 1235 ExecStart=/usr/sbin/nginx -g daemon on; (code=exited, status=0/SUCCESS)\n\
            Main PID: 1236 (nginx)\n\
               Tasks: 5 (limit: 4915)\n\
              Memory: 12.3M\n\
                 CPU: 1.234s\n\
              CGroup: /system.slice/nginx.service\n\
                      \u{251c}\u{2500}1236 \"nginx: master process /usr/sbin/nginx -g daemon on;\"\n\
                      \u{251c}\u{2500}1237 \"nginx: worker process\"\n\
                      \u{251c}\u{2500}1238 \"nginx: worker process\"\n\
                      \u{251c}\u{2500}1239 \"nginx: worker process\"\n\
                      \u{2514}\u{2500}1240 \"nginx: worker process\"\n\n\
         Jan 15 10:23:44 server1 systemd[1]: Starting A high performance web server...\n\
         Jan 15 10:23:45 server1 systemd[1]: Started A high performance web server.\n",
    );
}

// ═══════════════════════════════════════════════════════════════════════
// Fallback patterns (via compress_output pipeline)
// ═══════════════════════════════════════════════════════════════════════

#[test]
fn pattern_json_schema_fallback() {
    let large_json = r#"{"users":[{"id":1,"name":"Alice","email":"alice@example.com","role":"admin","active":true,"department":"engineering","location":"NYC","hire_date":"2020-01-15"},{"id":2,"name":"Bob","email":"bob@example.com","role":"user","active":true,"department":"marketing","location":"SF","hire_date":"2021-03-20"},{"id":3,"name":"Charlie","email":"charlie@example.com","role":"user","active":false,"department":"engineering","location":"NYC","hire_date":"2019-06-01"},{"id":4,"name":"Diana","email":"diana@example.com","role":"moderator","active":true,"department":"support","location":"London","hire_date":"2022-09-15"},{"id":5,"name":"Eve","email":"eve@example.com","role":"user","active":true,"department":"engineering","location":"Berlin","hire_date":"2023-01-10"},{"id":6,"name":"Frank","email":"frank@example.com","role":"user","active":false,"department":"marketing","location":"NYC","hire_date":"2020-07-22"},{"id":7,"name":"Grace","email":"grace@example.com","role":"admin","active":true,"department":"engineering","location":"SF","hire_date":"2018-11-30"},{"id":8,"name":"Hank","email":"hank@example.com","role":"user","active":true,"department":"support","location":"London","hire_date":"2023-05-15"},{"id":9,"name":"Irene","email":"irene@example.com","role":"user","active":true,"department":"engineering","location":"Berlin","hire_date":"2021-08-01"},{"id":10,"name":"Jack","email":"jack@example.com","role":"moderator","active":true,"department":"marketing","location":"NYC","hire_date":"2022-02-28"}],"total":10,"page":1,"per_page":20}"#;
    assert_pipeline("json_schema", "my-api-tool list-users", large_json);
}

#[test]
fn pattern_log_dedup_fallback() {
    let mut logs = String::new();
    for i in 0..30 {
        logs.push_str(&format!(
            "2024-01-15 10:23:{i:02} INFO  Processing request for /api/users\n"
        ));
    }
    logs.push_str("2024-01-15 10:24:00 WARN  High latency detected: 250ms\n");
    for i in 0..10 {
        logs.push_str(&format!(
            "2024-01-15 10:24:{:02} INFO  Processing request for /api/users\n",
            i + 1
        ));
    }
    assert_pipeline("log_dedup", "my-server run --log-level info", &logs);
}

#[test]
fn pattern_generic_test_output() {
    assert_pipeline(
        "generic_test",
        "my-test-runner run",
        " PASS  src/auth.test.ts\n\
         PASS  src/db.test.ts\n\
         PASS  src/cache.test.ts\n\
         FAIL  src/api.test.ts\n\
           \u{25cf} DELETE /users/:id returns 204\n\n\
             expect(received).toBe(expected)\n\n\
             Expected: 204\n\
             Received: 403\n\n\
               42 |     const response = await request(app).delete('/users/1');\n\
               43 |     expect(response.status).toBe(204);\n\
                  |                              ^\n\n\
         Test Suites: 1 failed, 3 passed, 4 total\n\
         Tests:       1 failed, 19 passed, 20 total\n\
         Snapshots:   0 total\n\
         Time:        3.456 s\n",
    );
}
