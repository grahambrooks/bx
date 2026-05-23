# bx (Binary Execute)

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
- Per-platform cache at `$XDG_CACHE_HOME/bx/<owner>/<repo>/<tag>/`
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
| **1** | `.bx.toml` manifest, checksum verification, `bx prune` |
| **2** | `bx mcp add/list/update/inspect` — writes/reads MCP client configs |
| **3** | Skill frontmatter integration: `bx ensure --skill <dir>` |
| **4** | Sigstore verification, trust-on-first-use, `--offline` mode |

## Configuration

| Env var | Effect |
|---|---|
| `GITHUB_TOKEN` | Authenticated API requests (higher rate limits, private repos) |
| `BX_GITHUB_API_BASE` | Override the GitHub API base URL (testing, GHES) |
| `BX_LOG` | Tracing filter, e.g. `BX_LOG=debug` or `BX_LOG=bx::fetch=trace` |
| `XDG_CACHE_HOME` | Cache root override on Linux (standard XDG behaviour) |

## Architecture

```
src/
├── main.rs       # CLI entry, clap setup, error rendering
├── lib.rs        # Pipeline orchestration: spec → resolve → fetch → exec
├── spec.rs       # owner/repo[@ref][#bin] parser
├── platform.rs   # OS/arch detection + keyword vocabularies
├── github.rs     # Minimal Releases API client
├── asset.rs      # Asset-name scoring heuristic
├── cache.rs      # Cache layout + binary discovery
├── fetch.rs      # Download + tar.gz/zip extraction
├── exec.rs       # Stdio-inheriting child process exec
└── error.rs      # Typed errors with rich Display
```

## Build and test

```sh
make            # list available targets
make build      # cargo build --release
make test       # 24 unit + 3 integration
```

## Releasing

Releases are calver-tagged (`vYYYY.M.D`) and built by
[`.github/workflows/release.yml`](.github/workflows/release.yml).

```sh
make release                    # triggers today's date
make release VERSION=2026.5.23  # explicit version
```

`make release` requires the [`gh`](https://cli.github.com) CLI and triggers
the workflow, which builds darwin/linux/windows artifacts, publishes a GitHub
release, and pushes a Homebrew formula bump in [`Formula/bx.rb`](Formula/bx.rb).

## License

MIT.
