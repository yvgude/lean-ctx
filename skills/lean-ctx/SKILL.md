---
name: lean-ctx
description: Token optimizer — compress shell outputs by 50-99% before they reach the LLM. Reduces context window usage and costs.
metadata: {"openclaw": {"requires": {"bins": ["lean-ctx"]}, "install": [{"id": "brew", "kind": "brew", "formula": "lean-ctx", "bins": ["lean-ctx"], "label": "Install LeanCTX (brew tap yvgude/lean-ctx)"}]}}
---

# LeanCTX — Token Compression for Shell Commands

LeanCTX is a Rust binary that compresses CLI output by 50-99% using 50+ command-specific patterns. It reduces the tokens you consume per shell command, keeping your context window lean and your costs low.

## When to use lean-ctx

Always prefer `lean-ctx -c <command>` over running commands directly when:
- The command produces verbose output (build logs, git diffs, dependency trees, test results)
- You are reading files and only need the structure or API surface
- You want to check token savings for the current session

## Shell commands (use instead of raw exec)

Instead of running commands directly, prefix with `lean-ctx -c`:

```bash
lean-ctx -c git status          # Compressed git output
lean-ctx -c git diff            # Only meaningful diff lines
lean-ctx -c git log --oneline -10
lean-ctx -c npm install         # Strips progress bars, noise
lean-ctx -c npm test
lean-ctx -c cargo build
lean-ctx -c cargo test
lean-ctx -c docker ps
lean-ctx -c kubectl get pods
lean-ctx -c curl -s <url>       # JSON schema extraction
lean-ctx -c ls -la <dir>        # Grouped directory listing
lean-ctx -c find . -name "*.ts" # Compact find results
```

If the user's shell already has lean-ctx aliases configured (`lean-ctx init --global`), commands like `git status` are automatically compressed. You can verify by checking if the output contains `[lean-ctx: ... tok, ...]` at the end.

## File reading (compressed modes)

```bash
lean-ctx read <file>                    # Full content with structured header
lean-ctx read <file> -m map             # Dependency graph + exports + API (~5-15% tokens)
lean-ctx read <file> -m signatures      # Function/class signatures only (~10-20% tokens)
lean-ctx read <file> -m aggressive      # Syntax-stripped (~30-50% tokens)
lean-ctx read <file> -m entropy         # Shannon entropy filtered (~20-40% tokens)
lean-ctx read <file> -m diff            # Only changed lines since last read
```

Use `map` mode when you need to understand what a file does without reading every line.
Use `signatures` mode when you need the API surface of a module.
Use `full` mode only when you will edit the file.

## Analytics

```bash
lean-ctx gain                   # Show cumulative token savings
lean-ctx dashboard              # Open web dashboard at localhost:3333
lean-ctx session                # Show adoption statistics
lean-ctx discover               # Find uncompressed commands in shell history
```

## Tips

- The output suffix `[lean-ctx: 5029→197 tok, -96%]` shows original vs compressed token count
- For large outputs, lean-ctx automatically truncates while preserving relevant context
- JSON responses from curl/wget are reduced to schema outlines
- Build errors are grouped by type with counts
- Test results show only failures with summary counts
