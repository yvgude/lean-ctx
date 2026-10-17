.PHONY: setup-hooks install install-release build check dev test preflight preflight-fast enterprise-ci-check help

ENTERPRISE_DIR ?= ../lean-ctx-enterprise

# ── Setup ─────────────────────────────────────────────────

setup-hooks: ## Configure git to use .githooks/ for hooks
	git config core.hooksPath .githooks
	@echo "Git hooks configured: .githooks/"

# ── Build & Install ──────────────────────────────────────

install: dev ## Fast source build + atomic install + restart

install-release: ## Production release build + atomic install + restart
	@mkdir -p "$$HOME/.local/bin"
	bash rust/dev-install.sh --release

check: ## Type-check changes without linking a binary
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

preflight: ## Full local CI-parity gate (fmt, clippy, doc, gen_docs, win-check, lib tests)
	scripts/preflight.sh full

preflight-fast: ## Static CI-parity gate (no full test run) — what pre-push runs
	scripts/preflight.sh fast

# ── Enterprise CI ────────────────────────────────────────────

enterprise-ci-check: ## Validate the separate enterprise checkout for feature-flag CI
	@test -d "$(ENTERPRISE_DIR)" || { echo "Enterprise checkout not found: $(ENTERPRISE_DIR)"; exit 1; }
	@test -f "$(ENTERPRISE_DIR)/Cargo.toml" || { echo "Missing Cargo.toml in $(ENTERPRISE_DIR)"; exit 1; }
	@cd "$(ENTERPRISE_DIR)" && cargo metadata --no-deps --format-version 1 >/dev/null
	@echo "Enterprise checkout ready: $(ENTERPRISE_DIR)"

# ── Help ──────────────────────────────────────────────────

help: ## Show this help
	@grep -E '^[a-zA-Z_-]+:.*?## .*$$' $(MAKEFILE_LIST) | \
		awk 'BEGIN {FS = ":.*?## "}; {printf "  \033[36m%-18s\033[0m %s\n", $$1, $$2}'

.DEFAULT_GOAL := help
