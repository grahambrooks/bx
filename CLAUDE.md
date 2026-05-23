# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Commands

```sh
cargo build --release
cargo test                                       # all unit + integration tests
cargo test --test end_to_end                     # integration tests only
cargo test --test end_to_end -- --nocapture      # see binary stdout/stderr
cargo test spec::tests::with_tag                 # single unit test by path
cargo clippy --all-targets -- -D warnings
cargo fmt
```

Run the binary locally without installing:
```sh
cargo run -- grahambrooks/symgraph@v2026.4.13 -- --help
BX_LOG=debug cargo run -- grahambrooks/symgraph -- --help
```

## Architecture

`bx` is a thin pipeline: **parse spec → resolve release → select asset → fetch → verify → extract → exec**. Each step is one module under `src/` and is called from `lib.rs::run`. `main.rs` is only CLI parsing (clap subcommands: `<spec>` default, `prune`, `add`, `ensure`) and error rendering.

### Pipeline invariants worth knowing

- **Pinned refs (`@vX.Y.Z`) take a fast path** in `lib.rs::run`: if the binary is already in the cache, `bx` execs it without any network call. This is what makes MCP servers snappy when launched from a config. Unpinned (`Ref::Latest`) refs always hit the GitHub API because "latest" can change. The cache hit lookup must stay synchronous and cheap — don't add network or async work to it.
- **Stdio is inherited, not piped.** `exec.rs` uses `Command::status()` (not `exec(3)`) so we keep a chance to add cleanup later, but stdio passthrough is non-negotiable: MCP clients talk to bx's stdin/stdout, which is really the child's. Don't introduce buffering or output capture in the exec path.
- **Exit codes pass through.** The child's exit code is clamped to `u8` and returned. Signal-killed children become `130` (SIGINT convention). Integration tests assert this — see `passes_through_nonzero_exit_codes`.
- **Checksum verification fires on fetch, not on cache hit.** `fetch::ensure` accepts an `Option<&str>` expected sha and returns `Fetched { binary, archive_sha256 }`. `lib::run` walks up for a `.bx.toml`, looks up the spec, and passes the per-platform checksum (if any) into the fetch. Cache hits trust the prior install — the lockfile-style "verify once on install" contract. `bx --refresh` is the escape hatch when you want to re-verify; future M4 work (sigstore + binary-hash sidecars) will close the gap if it becomes load-bearing.

### Asset selection (`asset.rs`)

The scorer is a small heuristic, not a manifest. It rewards matches on this platform's OS/arch keyword vocabularies (defined in `platform.rs`), strongly penalises matches on *other* platforms' keywords (so `darwin-x64` doesn't tie with `linux-x64` on a linux box), rewards the preferred archive extension (zip on Windows, tar.gz elsewhere), and tie-breaks by file size (larger wins — usually the fully-bundled artifact). Noise filters drop checksums, signatures, source tarballs, and `.mcpb` bundles before scoring. When asset selection misbehaves for a new release format, prefer extending the keyword vocabularies in `platform.rs` over adding special cases in `asset.rs`.

### Cache layout (`cache.rs`) and pruning (`prune.rs`)

`<cache_root>/<owner>/<repo>/<tag>/` — one dir per resolved tag. `find_binary` looks at the dir root, then `bin/`, then walks the tree (handles archives that expand into a versioned subdir). Tag names are sanitised so a branch like `feature/foo` doesn't escape the cache root. GC is on-demand via `bx prune` (default: keep newest tag per repo; `--keep N`, `--all`, `--dry-run`). `prune::prune_at(root, opts)` is the testable inner function; pass a tempdir to isolate tests from the real cache.

### Manifest (`manifest.rs`) + checksum (`checksum.rs`)

`.bx.toml` schema is `[[tool]]` array-of-tables with `spec` (exact-match string) and a `[tool.checksums]` per-platform map keyed by platform slug (`darwin-arm64`, `linux-x64`, …). `manifest::find(start)` walks ancestors looking for `.bx.toml`; `Manifest::load/save` round-trip via the `toml` crate (comments are NOT preserved — this is auto-managed, hand-edits survive only when no `bx add`/`bx ensure --record` writes the file). `record_checksum` is idempotent. `checksum::sha256_hex` hashes the *archive*, not the extracted binary, for reasons documented at the top of the file (archive is what travels the wire; extraction is non-deterministic; matches ecosystem conventions).

### Errors (`error.rs`)

All errors are `BxError` variants with `thiserror`. `main.rs` walks the `source()` chain and prints `caused by:` lines — keep the chain intact when adding new error variants (use `#[from]` for upstream errors so the source link is preserved). Notable M1 additions: `Manifest(String)` for parse/serialize, `ChecksumMismatch { expected, actual, asset }` (exit code 1 with a clear message).

## Testing patterns

`tests/end_to_end.rs` spins up an in-process HTTP server (an inline `TcpListener` thread + a `Routes` type alias for the route table), points `bx` at it via `BX_GITHUB_API_BASE`, and serves a synthetic tar.gz containing a shell script as the "binary". Both `assert_cmd` and a `tempfile` cache root are used so tests don't touch the real cache. When adding integration coverage, extend this file rather than introducing a new test harness — the inline-server + tempdir + env-overrides pattern is the convention. Tests that build a fixture asset must use `host_platform_slug()` (not a hardcoded slug like `linux-x64`) so the asset scorer accepts them on whichever runner is executing the tests.

## Environment variables that matter for development

| Var | Purpose |
|---|---|
| `BX_GITHUB_API_BASE` | Redirect API calls (used by integration tests and for GHES) |
| `BX_LOG` | `tracing-subscriber` `EnvFilter` string, e.g. `debug` or `bx::fetch=trace` |
| `XDG_CACHE_HOME` | Cache root override (integration tests use this to isolate) |
| `GITHUB_TOKEN` | Auth for higher rate limits / private repos |

## Roadmap context

**Milestone 0** (fetch+cache+exec) and **Milestone 1** (`.bx.toml` + checksum verification + `bx prune`) are shipped. Upcoming milestones: MCP client config management `bx mcp add/list/...` (M2), skill frontmatter resolution `bx ensure --skill` (M3), and Sigstore + TOFU + `--offline` (M4). Several design choices are intentionally simple to make these additions cheap: the asset scorer can be replaced by a manifest lookup without touching callers, `Ref` is an enum so semver ranges can be added without breaking the spec parser's CLI surface, and `fetch::ensure` already returns the archive sha so sigstore attestation can hook in without re-hashing.

## Releasing + Homebrew

Releases are calver-tagged (`vYYYY.M.D`) by `.github/workflows/release.yml`, triggered via `make release` (optionally `VERSION=YYYY.M.D`). The workflow builds `darwin-arm64`, `linux-x64`, `linux-arm64`, and `windows-x64` (no `darwin-x64` — intentional), publishes a GitHub release, and pushes a Homebrew formula bump to `main` via `.github/scripts/update_formula.py`. The updater uses `# sha256:<platform>` sentinel comments in `Formula/bx.rb` to find each line — don't remove those.
