# bx (Binary Execute)

[![ci](https://github.com/grahambrooks/bx/actions/workflows/ci.yml/badge.svg)](https://github.com/grahambrooks/bx/actions/workflows/ci.yml)
[![latest release](https://img.shields.io/github/v/release/grahambrooks/bx?sort=semver)](https://github.com/grahambrooks/bx/releases/latest)
[![platforms](https://img.shields.io/badge/platforms-macOS%20%7C%20Linux%20%7C%20Windows-lightgrey)](#install)
[![rust](https://img.shields.io/badge/rust-1.97.1%2B-orange)](https://www.rust-lang.org)
[![license](https://img.shields.io/badge/license-MIT-blue)](LICENSE)

`bx` is a missing primitive for running local binary STDIO MCP servers —
similar to `npx`/`uvx`/`pipx`, but without dragging in a Node or Python
runtime. It fetches the right binary for your platform from a GitHub release,
caches it, and execs it with full stdio passthrough.

```sh
bx grahambrooks/symgraph -- --version          # latest release
bx grahambrooks/symgraph@v2026.4.13 serve      # pinned tag
bx grahambrooks/symgraph#cli -- foo            # named binary
bx --refresh grahambrooks/symgraph serve       # ignore cache
```

## Install

### Homebrew (macOS, Linux)

```sh
brew tap grahambrooks/bx https://github.com/grahambrooks/bx
brew install bx
```

### From release

Grab the right archive for your platform from the
[latest release](https://github.com/grahambrooks/bx/releases/latest) and put
`bx` on your `PATH`.

### From source

```sh
cargo install --git https://github.com/grahambrooks/bx
```

## Why

MCP server configs across the ecosystem default to `npx`, which forces a Node
runtime even for compiled tools. `bx` is the equivalent for native binaries:
one command fetches the right asset from a GitHub release for your platform,
caches it, and execs it with stdio passthrough.

The eventual goal is for skills in a marketplace to declare their MCP server
dependencies in frontmatter, and `bx ensure --skill` resolves them
transparently — see the milestones below.

## Status

Milestone 0 ships the foundation:

- `owner/repo[@ref][#binary]` spec parsing
- GitHub Releases resolution (latest + pinned tag)
- Asset selection via a scoring heuristic (handles `darwin-arm64`,
  `x86_64-unknown-linux-gnu`, etc.)
- Tarball and zip extraction
- Per-platform cache (`~/.cache/bx` on Linux, `~/Library/Caches/dev.bx.bx` on
  macOS, `%LOCALAPPDATA%\bx\cache` on Windows), laid out as
  `<owner>/<repo>/<tag>/`
- Fast-path: pinned refs hit cache before the network
- Exit-code and stdio passthrough (important for MCP stdio transport)
- Clean error chain reporting

Milestone 1:

- `bx prune` — garbage-collect cache (`--keep N`, `--all`, `--dry-run`)
- `.bx.toml` manifest with `bx add <spec>` and `bx ensure [--record]`
- Archive checksum verification (SHA-256) enforced on fetch

```sh
bx add grahambrooks/symgraph@v2026.4.13   # pin + record sha
bx ensure                                  # verify everything in .bx.toml
bx ensure --record                         # backfill checksums for this platform
```

Verification is enforced on download. Cache hits trust the prior install
(the lockfile-style "verify once on install" contract). Run `bx --refresh`
to force re-fetch + re-verify.

## Roadmap

| Milestone | Adds |
|---|---|
| **0** ✅ | Fetch + cache + exec end-to-end |
| **1** ✅ | `.bx.toml` manifest, checksum verification, `bx prune` |
| **2** | `bx mcp add/list/update/inspect` — writes/reads MCP client configs |
| **3** | Skill frontmatter integration: `bx ensure --skill <dir>` |
| **4** | Sigstore verification, trust-on-first-use, `--offline` mode |

## Configuration

| Env var | Effect |
|---|---|
| `GITHUB_TOKEN` | Authenticated API requests (higher rate limits, private repos) |
| `BX_GITHUB_API_BASE` | Override the GitHub API base URL (testing, GHES) |
| `BX_LOG` | Tracing filter, e.g. `BX_LOG=debug` or `BX_LOG=bx::fetch=trace` |
| `BX_CACHE_DIR` | Cache root override, all platforms |
| `XDG_CACHE_HOME` | Cache root override on Linux only (standard XDG behaviour) |

## Architecture

```
src/
├── main.rs       # CLI entry, clap subcommand dispatch, error rendering
├── lib.rs        # Pipeline: spec → resolve → fetch → verify → exec
│                 # Also hosts `ensure` + `add` operations
├── spec.rs       # owner/repo[@ref][#bin] parser
├── platform.rs   # OS/arch detection + keyword vocabularies
├── github.rs     # Minimal Releases API client
├── tls.rs        # Installs the rustls `ring` provider for the HTTP clients
├── asset.rs      # Asset-name scoring heuristic
├── cache.rs      # Cache layout + binary discovery
├── fetch.rs      # Download, extract, and (optional) sha256 verification
├── exec.rs       # Stdio-inheriting child process exec
├── checksum.rs   # SHA-256 of the downloaded archive
├── manifest.rs   # `.bx.toml` schema, walk-up lookup, load/save
├── prune.rs      # `bx prune` — GC the cache
└── error.rs      # Typed errors with rich Display
```

## Sandboxing

`bx` runs binaries with the same privileges as the calling user, with **no
sandbox by default**. Adoption friction stays at zero: install bx, run a
tool, it works. There is no hidden "secure by default" mode — if no policy
is declared, no sandbox is applied.

For enterprise or untrusted-source use, sandboxing is opt-in via any of:

- `bx --sandbox <profile> <spec>` — ad-hoc, per invocation
- `[tool.sandbox]` table in `.bx.toml` — per pinned tool
- `BX_SANDBOX_DEFAULT=<profile>` env var — org-wide default (e.g. shipped via MDM)

Built-in profiles:

| Profile | Read | Write | Network |
|---|---|---|---|
| `strict` | system paths, the binary's cache dir, cwd | nothing | deny |
| `project` | `strict` + `~/.config` (read-only) | cwd subtree | deny |
| `permissive` | `$HOME` | cwd subtree | allow |

Network is all-or-nothing: per-host allow-listing is **not** offered because
macOS Seatbelt cannot filter by hostname, so exposing it would be a promise
only one platform could keep. `strict` is the recommended profile for the
"untrusted public binary" threat model.

`.bx.toml` schema:

```toml
[[tool]]
spec = "owner/server@v1.0"

[tool.sandbox]
profile = "strict"
# allow_network   = true              # override the profile's network default
# readonly_paths  = ["/opt/data"]     # additive overrides, layered on the profile
# readwrite_paths = ["./.server-data"]
# denied_paths    = ["./.env"]        # masked off even if a broader allow covers it
```

**Status: macOS, Linux, and Windows shipped.** Each platform uses its native
isolation primitive:

- **macOS** applies an Apple Seatbelt profile in-process via `sandbox_init()`.
- **Linux** wraps the binary in
  [`bubblewrap`](https://github.com/containers/bubblewrap) (which must be
  installed — `bwrap` on `PATH`).
- **Windows** launches the binary in an
  [AppContainer](https://learn.microsoft.com/windows/win32/secauthz/appcontainer-isolation):
  bx derives the package SID, grants the policy's read-only/read-write paths to
  it via inheritable ACEs (reverted when the child exits), and calls
  `CreateProcessW` with a `SECURITY_CAPABILITIES` attribute. Network maps to the
  `internetClient`/`privateNetworkClientServer` capabilities (omitted ⇒ blocked).
  The `strict` profile additionally runs as a Less-Privileged AppContainer
  (LPAC); a binary whose dependencies are not readable by `ALL RESTRICTED
  APPLICATION PACKAGES` may need the `project`/`permissive` profile instead.

On a platform without sandbox support — or when Linux `bwrap` is missing — `bx`
logs a warning and runs unwrapped unless `BX_SANDBOX_FALLBACK=error` is set.
Windows fails closed: a sandbox you opted into that cannot be applied is an
error, never a silent downgrade. Stdio is always inherited raw, sandboxed or
not (Windows inherits the std handles directly — no console/PTY), so MCP stdio
servers behave identically everywhere.

Enforcement is covered by integration tests on macOS and Linux, not just unit
tests on the generated policy: under `strict`, a child's attempt to write
outside the policy is verified to actually fail (with an unsandboxed control run
proving the denial comes from the sandbox). The Linux case runs under `bwrap`
when a usable user namespace is available and skips cleanly otherwise. The
Windows AppContainer path is compiled and unit-tested in CI on a Windows runner;
its plan generation (`sandbox::appcontainer`) is unit-tested on every platform.

The macOS/Linux profile generators and the Windows AppContainer model are
adapted from Microsoft's [MXC](https://github.com/microsoft/mxc) (MIT). bx
vendors only the security-sensitive *plan generation* logic; it applies the
result to its own process so the child's stdin/stdout passthrough is never
routed through a PTY/console (which would corrupt MCP's newline-framed
JSON-RPC).

## Build and test

```sh
make            # list available targets
make build      # cargo build --release
make test       # 75 unit + 6 integration
```

## Releasing

Releases are calver-tagged (`vYYYY.M.D`) and built by
[`.github/workflows/release.yml`](.github/workflows/release.yml).

```sh
make release                    # triggers today's date
make release VERSION=2026.5.23  # explicit version
```

`make release` requires the [`gh`](https://cli.github.com) CLI and triggers
the workflow, which builds `darwin-arm64`, `linux-x64`, `linux-arm64`, and
`windows-x64` artifacts, publishes a GitHub release, and pushes a Homebrew
formula bump in [`Formula/bx.rb`](Formula/bx.rb). Intel Macs are not a
supported build target.

## License

MIT — see [LICENSE](LICENSE).

The sandbox plan generators are adapted from
[MXC](https://github.com/microsoft/mxc) (MIT). Because they are vendored by
hand rather than pulled in as a dependency, their notice is carried in
[THIRD-PARTY-NOTICES.md](THIRD-PARTY-NOTICES.md) instead of appearing in
`Cargo.toml`.
