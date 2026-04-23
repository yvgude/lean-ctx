# **FAQ — Shell Hook Issues**

---

**Q: My commands are broken after installing!**
Run `lean-ctx-off` to fix your current session immediately. Then run `lean-ctx setup` again to refresh hooks. If the problem persists, run `lean-ctx uninstall` and reinstall.

**Q: The shell hook compresses too much — signal is lost!**
This was addressed in recent versions. If a command's output is too aggressively compressed:
1. Update to latest: `lean-ctx update`
2. Exclude specific commands in config:
```toml
# ~/.lean-ctx/config.toml
excluded_commands = ["git stash", "your-command"]
```
3. Or disable for a single run: `LEAN_CTX_DISABLED=1 your-command`

**Q: Auth flows (az login, gh auth, etc.) are broken — the device code is hidden!**
Fixed since v2.21.10. lean-ctx now auto-detects 21+ auth commands and preserves their output uncompressed. Update to latest: `lean-ctx update`.

Workaround for older versions:
```toml
# ~/.lean-ctx/config.toml
excluded_commands = ["az login", "gh auth"]
```

**Q: The `[lean-ctx: NNN→NNN tok, -XX%]` stats line wastes tokens!**
Fixed in v3.2.6. The stats line is no longer appended to stdout by default. Update: `lean-ctx update`.

**Q: lean-ctx blocks image viewing in Claude Code!**
Fixed in recent versions. Binary/image files are now passed through without compression. Update: `lean-ctx update`.

**Q: `git commit -m "$(cat <<'EOF' ...)"` fails with syntax error!**
Fixed comprehensively in v3.3.4+. Two fixes:
1. The PreToolUse hook (Claude Code/Codex/Copilot) no longer wraps heredoc commands in `lean-ctx -c '...'` — they pass through to the shell directly.
2. `ctx_shell` now uses smart heredoc detection: only heredoc + file redirect (`cat <<EOF > file.txt`) is blocked. Legitimate heredoc uses (input piping, SQL, commit messages) are allowed.
Update: `lean-ctx update`.

**Q: `gh --comment "closing — see #407"` or `find` with many excludes gives wrong results!**
Fixed in v3.3.4+. The shell hook now uses direct argv execution, preserving em-dashes, `#` signs, nested quotes, and all special characters exactly as typed. Update: `lean-ctx update`.

If you still hit issues with a specific command, exclude it:
```toml
# ~/.lean-ctx/config.toml
excluded_commands = ["your-command"]
```
