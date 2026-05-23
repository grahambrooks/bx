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

`bx` is a thin pipeline: **parse spec → resolve release → select asset → fetch & extract → exec**. Each step is one module under `src/` and is called from `lib.rs::run`. `main.rs` is only CLI parsing and error rendering.

### Pipeline invariants worth knowing

- **Pinned refs (`@vX.Y.Z`) take a fast path** in `lib.rs::run`: if the binary is already in the cache, `bx` execs it without any network call. This is what makes MCP servers snappy when launched from a config. Unpinned (`Ref::Latest`) refs always hit the GitHub API because "latest" can change. The cache hit lookup must stay synchronous and cheap — don't add network or async work to it.
- **Stdio is inherited, not piped.** `exec.rs` uses `Command::status()` (not `exec(3)`) so we keep a chance to add cleanup later, but stdio passthrough is non-negotiable: MCP clients talk to bx's stdin/stdout, which is really the child's. Don't introduce buffering or output capture in the exec path.
- **Exit codes pass through.** The child's exit code is clamped to `u8` and returned. Signal-killed children become `130` (SIGINT convention). Integration tests assert this — see `passes_through_nonzero_exit_codes`.

### Asset selection (`asset.rs`)

The scorer is a small heuristic, not a manifest. It rewards matches on this platform's OS/arch keyword vocabularies (defined in `platform.rs`), strongly penalises matches on *other* platforms' keywords (so `darwin-x64` doesn't tie with `linux-x64` on a linux box), rewards the preferred archive extension (zip on Windows, tar.gz elsewhere), and tie-breaks by file size (larger wins — usually the fully-bundled artifact). Noise filters drop checksums, signatures, source tarballs, and `.mcpb` bundles before scoring. When asset selection misbehaves for a new release format, prefer extending the keyword vocabularies in `platform.rs` over adding special cases in `asset.rs`.

### Cache layout (`cache.rs`)

`<cache_root>/<owner>/<repo>/<tag>/` — one dir per resolved tag. `find_binary` looks at the dir root, then `bin/`, then walks the tree (handles archives that expand into a versioned subdir). Tag names are sanitised so a branch like `feature/foo` doesn't escape the cache root. There is no automatic GC; `bx prune` is a future milestone.

### Errors (`error.rs`)

All errors are `BxError` variants with `thiserror`. `main.rs` walks the `source()` chain and prints `caused by:` lines — keep the chain intact when adding new error variants (use `#[from]` for upstream errors so the source link is preserved).

## Testing patterns

`tests/end_to_end.rs` spins up an in-process HTTP server, points `bx` at it via `BX_GITHUB_API_BASE`, and serves a synthetic tar.gz containing a shell script as the "binary". Both `assert_cmd` and a `tempfile` cache root are used so tests don't touch the real cache. When adding integration coverage, extend this file rather than introducing a new test harness — the FakeServer + tempdir + env-overrides pattern is the convention.

## Environment variables that matter for development

| Var | Purpose |
|---|---|
| `BX_GITHUB_API_BASE` | Redirect API calls (used by integration tests and for GHES) |
| `BX_LOG` | `tracing-subscriber` `EnvFilter` string, e.g. `debug` or `bx::fetch=trace` |
| `XDG_CACHE_HOME` | Cache root override (integration tests use this to isolate) |
| `GITHUB_TOKEN` | Auth for higher rate limits / private repos |

## Roadmap context

The project is at **Milestone 0** (fetch+cache+exec). Upcoming milestones add a `.bx.toml` manifest with checksum verification (M1), MCP client config management `bx mcp add/list/...` (M2), skill frontmatter resolution `bx ensure --skill` (M3), and Sigstore + TOFU + `--offline` (M4). Several design choices are intentionally simple to make these additions cheap: the asset scorer can be replaced by a manifest lookup without touching callers, and `Ref` is an enum so semver ranges can be added without breaking the spec parser's CLI surface.
