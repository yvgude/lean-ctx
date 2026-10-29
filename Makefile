.PHONY: setup-hooks install install-release build check dev test precommit-check preflight preflight-fast help

# ── Setup ─────────────────────────────────────────────────

setup-hooks: ## Configure git to use .githooks/ for hooks
	git config core.hooksPath .githooks
	@echo "Git hooks configured: .githooks/"

# ── Build & Install ──────────────────────────────────────

install: dev ## Fast source build + atomic install + restart

install-release: ## Production release build + atomic install + restart
	@mkdir -p "$$HOME/.local/bin"
	bash rust/dev-install.sh --release

check: ## Type-check changes without linking a binary + local-free invariant
	python3 scripts/check-local-free.py
	cd rust && cargo check --locked

build: ## Optimized incremental development build
	cd rust && cargo fast

dev: ## Fast source build + atomic install + restart
	@mkdir -p "$$HOME/.local/bin"
	bash rust/dev-install.sh

test: ## Run all Rust tests + clippy
	umask 077; cd rust && cargo test --locked && cargo clippy --locked

# ── CI-parity gate ───────────────────────────────────────
# Mirrors .github/workflows/ci.yml so green-here => green-in-CI for the
# deterministic jobs (fmt/clippy/doc/gen_docs/cross-platform compile).

precommit-check: ## Run the pre-commit checks on the staged tree once; the commit's hook then skips them
	umask 077; .githooks/pre-commit

preflight: ## Full local CI-parity gate (fmt, clippy, doc, gen_docs, win-check, lib tests)
	scripts/preflight.sh full

preflight-fast: ## Static CI-parity gate (no full test run) — what pre-push runs
	scripts/preflight.sh fast

# ── Help ──────────────────────────────────────────────────

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help
