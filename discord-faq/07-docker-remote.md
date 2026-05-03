# **FAQ — Docker & Remote**

---

**Q: How do I use lean-ctx in Docker?**
```dockerfile
# Download pre-built binary
RUN curl -fsSL https://leanctx.com/install.sh | sh

# For Claude Code: set env file
ENV CLAUDE_ENV_FILE=/root/.lean-ctx/env
RUN lean-ctx setup
```
Important: Use `CLAUDE_ENV_FILE` (not just `BASH_ENV`) for Claude Code in Docker.
Full guide: <https://leanctx.com/docs/remote-setup/>

**Q: How do I use lean-ctx over SSH / remote?**
lean-ctx supports remote setups via SSH port-forwarding or running the MCP server directly on the remote machine. See: <https://leanctx.com/docs/remote-setup/>
