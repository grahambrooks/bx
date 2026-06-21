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

### Sandboxing policy (`sandbox/`)

Sandboxing is **shipped for macOS, Linux, and Windows**. **Out of the box, `bx` runs binaries with no sandbox** — a deliberate adoption-friction decision, not an oversight. The contract:

- The default code path (no `--sandbox` flag, no `[tool.sandbox]` in `.bx.toml`, no `BX_SANDBOX_DEFAULT` env) MUST remain unsandboxed. `exec::run` takes an `Option<&sandbox::Policy>`; the `None` arm is byte-for-byte the legacy unsandboxed path — a contract, not a placeholder. `lib::resolve_sandbox` returns `None` unless a policy is explicitly opted into.
- Do NOT introduce a "secure by default" behavior under any flag rename or refactor. Enterprise opt-in is `BX_SANDBOX_DEFAULT=strict` via env / MDM, not a compile-time toggle. Precedence: `--sandbox` > `[tool.sandbox]` > `BX_SANDBOX_DEFAULT`.
- **Stdio passthrough is non-negotiable in every sandboxed path.** macOS applies the Seatbelt profile via `sandbox_init()` in `Command::pre_exec` on bx's *own* command (stdio untouched); Linux execs `bwrap … -- <argv>` with inherited stdio; Windows calls `CreateProcessW` with `STARTF_USESTDHANDLES` + `bInheritHandles=TRUE`, inheriting bx's std handles raw. Never adopt MXC's `ScriptRunner`, which routes the child through a PTY/console and captures output — it would corrupt MCP's newline-framed JSON-RPC. This is why we vendor only the *plan generators* (`sandbox::seatbelt::build_profile`, `sandbox::bwrap::build_args`, `sandbox::appcontainer::build_plan`), not MXC's exec path.
- On platforms without a backend — or when Linux `bwrap` is not on `PATH` — `exec::fallback` logs a `WARN` and runs unsandboxed; only `BX_SANDBOX_FALLBACK=error` refuses. **Windows fails closed instead**: it has a backend, so a sandbox that cannot be *applied* (an AppContainer/ACL Win32 call fails) is a hard `BxError::Sandbox`, never a silent downgrade. `fallback` is therefore `#[cfg(not(any(macos, windows)))]`.
- Three built-in profiles in `sandbox::Profile`: `strict` (deny-all + cache + cwd read, no network), `project` (`strict` + cwd write + `~/.config` read), `permissive` (`$HOME` read + cwd write + network allow). `strict` is recommended for the "untrusted public binaries" threat model. Per-host network filtering is intentionally absent (Seatbelt can't enforce it) — network is all-or-nothing.
- **Windows AppContainer specifics.** `appcontainer::build_plan` is pure (no syscalls), unit-tested on every host like the other two generators; the Win32 *application* lives in `exec.rs`'s `#[cfg(windows)] mod win` and is compile-tested only on the Windows CI runner (it cannot link on macOS/Linux because reqwest→ring needs a Windows C toolchain). AppContainer's filesystem model is ACL-based, not a launch-time allow-list, so `win` mutates DACLs on the policy paths (grant the package SID, **revert via `GrantGuard` on scope exit**) — the only host-state mutation in any backend. The `strict` profile sets `Policy.lpac` (Less-Privileged AppContainer); looser profiles leave it off so binaries with dependencies unreadable by `ALL RESTRICTED APPLICATION PACKAGES` still load. Win32 constants in `win` are defined locally (stable ABI) to minimise dependence on exact `windows-sys` symbol paths.
- The generators and the Windows AppContainer model are adapted from [MXC](https://github.com/microsoft/mxc) (MIT). When upstream changes its Seatbelt rules, bwrap flags, or AppContainer/LPAC handling, re-sync `seatbelt.rs`/`bwrap.rs`/`appcontainer.rs` by hand — they are vendored, not a dependency. bx's `sandbox::Policy` is a trimmed projection of MXC's `ContainerPolicy` (only the fields the generators read); bx runs headless, so MXC's UI/clipboard/GUI knobs are hard-coded to the locked-down variant.

The user-facing schema, profile table, and rationale live in the README's `## Sandboxing` section — keep that as the user-facing source of truth and this section as the contributor-facing contract.

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
