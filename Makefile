.DEFAULT_GOAL := help

# Optional version override consumed by `make release`, e.g. `make release VERSION=2026.5.23`.
# When empty, `make release` uses today's UTC date as YYYY.M.D.
VERSION ?=

.PHONY: help build test release clippy fmt clean

help: ## Show this help.
	@awk 'BEGIN {FS = ":.*##"; printf "Usage: make \033[36m<target>\033[0m\n\nTargets:\n"} \
	  /^[a-zA-Z_-]+:.*?##/ {printf "  \033[36m%-10s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

build: ## Build the release binary.
	cargo build --release

test: ## Run unit and integration tests.
	cargo test

clippy: ## Lint with clippy, treating warnings as errors.
	cargo clippy --all-targets -- -D warnings

fmt: ## Format the source tree with rustfmt.
	cargo fmt

clean: ## Remove cargo build artifacts.
	cargo clean

release: ## Tag origin/main as vVERSION and push the tag (runs the release workflow). Optional: VERSION=YYYY.M.D
	@set -e; \
	V="$(VERSION)"; [ -n "$$V" ] || V="$$(date -u +%Y.%-m.%-d)"; \
	git fetch --quiet origin main --tags; \
	if git rev-parse -q --verify "refs/tags/v$$V" >/dev/null; then \
	  echo "error: tag v$$V already exists" >&2; exit 1; fi; \
	echo "Tagging origin/main as v$$V..."; \
	git tag "v$$V" origin/main; \
	git push origin "v$$V"
