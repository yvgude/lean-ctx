// SPDX-License-Identifier: Apache-2.0
//! Subcommand-scoped entries (`winapp ui list-windows *`) through the whole
//! gate, as `ctx_shell` runs it on Windows PowerShell.

use super::ps_statements::HostGuard;

const LIST: &str = "git,echo,winapp ui list-windows *,winapp ui inspect *";

fn gate(cmd: &str, powershell: bool, granted: &[String]) -> Result<(), String> {
    let _lock = lean_ctx::core::data_dir::test_env_lock();
    let _allowlist = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_ALLOWLIST_OVERRIDE", LIST);
    let _mode = lean_ctx::test_env::VarGuard::set("LEAN_CTX_SHELL_SECURITY", "enforce");
    let _host = HostGuard::powershell(powershell);
    super::check_shell_allowlist_granted(cmd, granted).map_err(|err| err.to_string())
}

#[test]
fn scoped_entries_admit_only_their_subcommands() {
    for powershell in [true, false] {
        for granted in [Vec::new(), vec!["git".to_string()]] {
            for cmd in [
                "winapp ui status -a __LEANCTX_NONEXISTENT_APP_TEST__",
                "winapp ui get-value -a notepad",
                "winapp ui set-value -a notepad x",
                "winapp.exe ui status -a x",
                "WinApp ui status -a x",
                "winapp ui list-windows; winapp ui status",
                "& winapp ui status -a x",
                "& 'winapp' ui status -a x",
            ] {
                assert!(
                    gate(cmd, powershell, &granted).is_err(),
                    "must be blocked (powershell {powershell}, grant {granted:?}): {cmd:?}"
                );
            }
            for cmd in ["winapp ui list-windows", "winapp ui inspect -a notepad"] {
                if let Err(err) = gate(cmd, powershell, &granted) {
                    panic!("should pass (powershell {powershell}): {cmd:?}\n{err}");
                }
            }
        }
    }
}
