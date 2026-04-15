# Deployment Guide — lean-ctx

## Website Deployment (leanctx.com)

### Prerequisites
- SSH access to `administrator@185.142.213.170` via `~/.ssh/pounce_server`
- Server sudo password: stored in `server_access.md` (not in repo)
- Node.js >= 22.12.0 (use `/opt/homebrew/bin/node` on macOS)

### Steps

#### 1. Build Website
```bash
cd /Users/yvesgugger/Documents/Privat/Projects/lean-ctx/website
PATH="/opt/homebrew/bin:$PATH" npm run build
```

#### 2. Sync to Server
```bash
cd /Users/yvesgugger/Documents/Privat/Projects/lean-ctx
rsync -az --delete \
  -e "ssh -i ~/.ssh/pounce_server" \
  --exclude ".git" --exclude "node_modules" --exclude "website/node_modules" \
  --exclude "website/dist" --exclude "dist" --exclude ".env" --exclude "deploy.sh" \
  --exclude "rust/target" \
  ./ administrator@185.142.213.170:/home/administrator/lean-ctx/
```

#### 3. Build Docker Image
```bash
ssh -i ~/.ssh/pounce_server administrator@185.142.213.170 \
  "cd /home/administrator/lean-ctx && \
   sudo docker build -t lean-ctx-web -f Dockerfile.web ."
```

#### 4. Restart Container
```bash
ssh -i ~/.ssh/pounce_server administrator@185.142.213.170 \
  "sudo docker stop lean-ctx-web 2>/dev/null; \
   sudo docker rm lean-ctx-web 2>/dev/null; \
   sudo docker run -d \
     --name lean-ctx-web \
     --network coolify \
     --restart unless-stopped \
     --label traefik.enable=true \
     --label 'traefik.http.routers.lean-ctx.rule=Host(\`leanctx.com\`) || Host(\`www.leanctx.com\`)' \
     --label traefik.http.routers.lean-ctx.entrypoints=websecure \
     --label traefik.http.routers.lean-ctx.tls=true \
     --label traefik.http.routers.lean-ctx.tls.certresolver=letsencrypt \
     --label traefik.http.services.lean-ctx.loadbalancer.server.port=80 \
     lean-ctx-web"
```

#### 5. Verify
```bash
command curl -s -o /dev/null -w '%{http_code}' https://leanctx.com/
# Should return 200
```

### Troubleshooting

- **Traefik Host rules empty**: Backtick escaping in SSH commands. Use `\`` not `\\\\\\\``.
- **Node.js too old**: Use `PATH="/opt/homebrew/bin:$PATH"` before build commands.
- **Astro "Unexpected &"**: Wrap PowerShell code in template literals `{` `` ` `` `}` inside `<pre><code>`.

---

## Server Details

| Property | Value |
|----------|-------|
| IP | `185.142.213.170` |
| User | `administrator` |
| SSH Key | `~/.ssh/pounce_server` |
| Docker Network | `coolify` |
| Reverse Proxy | Traefik |
| TLS | Let's Encrypt |
| Container Name | `lean-ctx-web` |
| Image | `lean-ctx-web` (nginx:alpine) |
| Port | 80 (internal), 443 (external via Traefik) |

---

## Git Push

### GitHub
```bash
# Standard push (may fail for workflow files due to OAuth scope)
git push github main --tags

# SSH-based push (bypasses OAuth scope restriction)
GIT_SSH_COMMAND="ssh -i ~/.ssh/id_ed25519 -o IdentitiesOnly=yes" git push github main --tags
```

### GitLab
```bash
git push origin main --tags
```

**Note**: GitLab push may show exit code 1 despite success — this is lean-ctx's shell hook compressing the output. Check the actual message for "main -> main".
