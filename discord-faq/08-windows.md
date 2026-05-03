# **FAQ — Windows**

---

**Q: Is Windows supported?**
Yes! lean-ctx supports Windows with PowerShell and Git Bash. Some tips:
- Use the latest version — many Windows path-handling fixes were added in v3.2.2+
- The updater infinite-loop bug (GNU timeout conflict) was fixed in v3.2.0
- `ctx_graph` path normalization issues were fixed in v3.2.2

**Q: Bash hook strips slashes from paths on Windows!**
This was a path-handling bug in Claude Code's hook execution on Windows with Git Bash. Fixed in v3.2.4. Update: `lean-ctx update`.
