.DEFAULT_GOAL := help

# Optional version override consumed by `make release`, e.g. `make release VERSION=2026.5.23`.
# When empty, the release workflow falls back to today's date as YYYY.M.D.
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

release: ## Trigger the release workflow. Optional: VERSION=YYYY.M.D
	@command -v gh >/dev/null 2>&1 || { \
	  echo "error: gh CLI not found (https://cli.github.com)" >&2; exit 1; }
	@if [ -n "$(VERSION)" ]; then \
	  echo "Triggering release v$(VERSION)..."; \
	  gh workflow run release.yml -f version="$(VERSION)"; \
	else \
	  echo "Triggering release with today's calver..."; \
	  gh workflow run release.yml; \
	fi
