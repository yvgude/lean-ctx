# Development Guide

## Repository Structure

This project uses **two remotes** with different visibility:

| Remote | URL | Content |
|--------|-----|---------|
| `github` | github.com/yvgude/lean-ctx | Open source only (MIT) |
| `origin` | gitlab.pounce.ch/root/lean-ctx | Everything including proprietary code |

### What lives where

| Path | GitHub | GitLab | Description |
|------|--------|--------|-------------|
| `rust/` | Yes | Yes | Open-source lean-ctx engine |
| `packages/` | Yes | Yes | npm packages (lean-ctx-bin, pi-lean-ctx) |
| `CONTRIBUTING.md` | Yes | Yes | Contribution guide |
| `.github/` | Yes | Yes | CI workflows, issue templates |
| `cloud/` | **No** | Yes (deploy branch) | Proprietary cloud API backend |
| `docker-compose.yml` | **No** | Yes (deploy branch) | Deployment configuration |
| `website/` | **No** | **No** (separate deploy) | Astro website |

### Protection layers

Three layers prevent proprietary code from leaking to GitHub:

1. **`.gitignore`** — `cloud/` and `docker-compose.yml` are ignored
2. **Pre-push hook** (`.githooks/pre-push`) — blocks pushes containing private paths
3. **CI guardrail** (`.github/workflows/security-check.yml`) — fails if private code detected

## Push Workflow

```bash
# Open-source push to GitHub:
git push github main
# or:
make push-github

# Everything to GitLab (creates deploy branch with cloud/):
make push-gitlab

# Both:
make push-all
```

### How `make push-gitlab` works

1. Pushes `main` branch to GitLab (clean, no cloud/)
2. Creates a `deploy` branch from `main`
3. Force-adds `cloud/` and `docker-compose.yml` from the previous deploy
4. Commits and force-pushes `deploy` to GitLab
5. Switches back to `main` and restores local cloud/ files

## Setup (new machine)

```bash
git clone https://gitlab.pounce.ch/root/lean-ctx.git
cd lean-ctx

# Configure hooks
make setup-hooks

# Add GitHub remote
git remote add github git@github.com:yvgude/lean-ctx.git

# Restore cloud/ files locally
git checkout origin/deploy -- cloud/ docker-compose.yml
git reset HEAD cloud/ docker-compose.yml
```

## Build

```bash
# Engine
cd rust && cargo test && cargo clippy

# Cloud backend
cd cloud && cargo build

# Website
cd website && npm install && npm run build
```

## Adding new proprietary paths

1. Add the path to `.gitignore`
2. Add the path to `.github-ignore`
3. Run `git rm --cached <path>` if already tracked
4. Update the `push-gitlab` Makefile target if needed
