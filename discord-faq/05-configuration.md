# **FAQ — Configuration**

---

**Q: Where is the config file?**
`~/.lean-ctx/config.toml` — created on demand. If it doesn't exist, defaults are used.

**Q: What is `rules_scope` and how do I use it?**
`rules_scope` controls where lean-ctx places agent rule files during `lean-ctx init`:
```toml
# ~/.lean-ctx/config.toml
rules_scope = "local"    # rules in project dir (default)
rules_scope = "global"   # rules in home dir
```
This affects where CLAUDE.md, AGENTS.md, .cursorrules etc. are written.

**Q: How do I disable specific tools?**
```toml
# ~/.lean-ctx/config.toml
disabled_tools = ["ctx_execute", "ctx_edit"]
```

**Q: How do I exclude commands from compression?**
```toml
# ~/.lean-ctx/config.toml
excluded_commands = ["az login", "my-custom-tool"]
```
