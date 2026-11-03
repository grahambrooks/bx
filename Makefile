.DEFAULT_GOAL := help

# release, release-dry, release-version (release-kit; tag-only, date-based CalVer)
include release.mk

.PHONY: help check fmt fmt-check lint clippy test tls-smoke hooks build clean

help: ## Show this help.
	@awk 'BEGIN {FS = ":.*##"; printf "Usage: make \033[36m<target>\033[0m\n\nTargets:\n"} \
	  /^[a-zA-Z_-]+:.*?##/ {printf "  \033[36m%-16s\033[0m %s\n", $$1, $$2}' $(MAKEFILE_LIST)

check: fmt-check lint test tls-smoke ## The pre-push gate: the same commands as CI's test job.

fmt: ## Format the source tree with rustfmt.
	cargo fmt --all

fmt-check: ## Verify formatting.
	cargo fmt --all --check

lint: ## Lint with clippy, warnings as errors (as CI).
	cargo clippy --workspace --all-targets --all-features --locked -- -D warnings

clippy: lint ## Alias for lint.

test: ## Run unit and integration tests (as CI).
	cargo test --workspace --all-targets --all-features --locked

tls-smoke: ## Real-network TLS check (the #[ignore]d test CI runs).
	cargo test --locked --test tls_smoke -- --ignored

hooks: ## Install the git hooks (pre-commit and pre-push) from prek.toml.
	prek install

build: ## Build the release binary.
	cargo build --release

clean: ## Remove cargo build artifacts.
	cargo clean
