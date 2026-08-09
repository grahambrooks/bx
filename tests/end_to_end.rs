//! End-to-end integration test for bx.
//!
//! Spins up a local HTTP server that pretends to be GitHub Releases. Points
//! bx at it via `BX_GITHUB_API_BASE`. Serves a synthetic archive containing
//! a real executable as the "binary". Verifies bx fetches, extracts, finds
//! the binary, and execs it with passed-through args.
//!
//! The fixture binary is `tests/support/fixture_tool.rs`, compiled on demand
//! — see [`fixture_tool`]. It replaced the `#!/bin/sh` scripts these tests
//! used to embed, which was the only reason the suite could not run on
//! Windows.
//!
//! Both archive formats are covered, on every platform: `extract_or_place`
//! in `src/fetch.rs` dispatches on the *asset's* extension rather than the
//! host OS, so a `.zip` is reachable everywhere — and it is the format the
//! asset scorer prefers on Windows.
//!
//! Run with: `cargo test --test end_to_end -- --nocapture`

use assert_cmd::Command;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::thread;

/// Compile `tests/support/fixture_tool.rs` and return the path to the binary.
///
/// Built with `rustc` directly rather than declared as a Cargo target: a
/// `[[bin]]` would ship with `cargo install`, and an `[[example]]` would not
/// be built by the bare `cargo test --test end_to_end` that CLAUDE.md
/// documents. The fixture has no dependencies, so a one-shot `rustc` is all
/// it needs (~0.3s, and only when the source is newer than the output).
fn fixture_tool() -> &'static Path {
    static TOOL: OnceLock<PathBuf> = OnceLock::new();
    TOOL.get_or_init(|| {
        let src = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("support")
            .join("fixture_tool.rs");

        // `cargo_bin` lands in `<target>/<profile>/`, which respects
        // CARGO_TARGET_DIR — put the fixture beside it rather than guessing.
        let out_dir = assert_cmd::cargo::cargo_bin("bx")
            .parent()
            .expect("cargo_bin has a parent dir")
            .to_path_buf();
        let out = out_dir.join(format!("bx-fixture-tool{}", std::env::consts::EXE_SUFFIX));

        if !is_fresh(&out, &src) {
            let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
            let status = std::process::Command::new(rustc)
                .arg(&src)
                .arg("-O")
                .arg("-o")
                .arg(&out)
                .status()
                .expect("failed to run rustc to build the fixture tool");
            assert!(status.success(), "rustc failed to build {}", src.display());
        }
        out
    })
}

/// True when `out` exists and is no older than `src`.
fn is_fresh(out: &Path, src: &Path) -> bool {
    let (Ok(out_meta), Ok(src_meta)) = (std::fs::metadata(out), std::fs::metadata(src)) else {
        return false;
    };
    match (out_meta.modified(), src_meta.modified()) {
        (Ok(o), Ok(s)) => o >= s,
        // No mtime support: rebuild rather than risk a stale fixture.
        _ => false,
    }
}

fn fixture_bytes() -> Vec<u8> {
    std::fs::read(fixture_tool()).expect("read compiled fixture tool")
}

/// The filename the archive entry must use for `tool` on this platform.
/// `cache::find_binary` looks for `<name>.exe` on Windows, and
/// `is_executable` there is extension-based, so the suffix is load-bearing.
fn exe_name(tool: &str) -> String {
    format!("{tool}{}", std::env::consts::EXE_SUFFIX)
}

/// Build an in-memory tar.gz containing the fixture tool at `tool`.
fn make_tarball(tool: &str) -> Vec<u8> {
    let body = fixture_bytes();

    let mut header = tar::Header::new_gnu();
    header.set_path(exe_name(tool)).unwrap();
    header.set_size(body.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();

    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = tar::Builder::new(encoder);
    tar.append(&header, &body[..]).unwrap();
    tar.into_inner().unwrap().finish().unwrap()
}

/// Build an in-memory zip containing the fixture tool at `tool`.
///
/// The zip path through `fetch::extract_or_place` had no runtime coverage at
/// all before this existed — it is the format the scorer prefers on Windows,
/// so it was the default Windows path that went untested.
fn make_zip(tool: &str) -> Vec<u8> {
    use zip::write::SimpleFileOptions;

    let body = fixture_bytes();
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated)
        .unix_permissions(0o755);
    writer.start_file(exe_name(tool), options).unwrap();
    writer.write_all(&body).unwrap();
    writer.finish().unwrap().into_inner()
}

fn release_json(asset_url: &str, asset_name: &str, asset_size: u64) -> String {
    // Purely cosmetic — `asset.rs` scores on the name, and `github::Asset`
    // parses `content_type` without consulting it — but keep the fixture
    // internally consistent so it does not read as authoritative.
    let content_type = if asset_name.ends_with(".zip") {
        "application/zip"
    } else {
        "application/gzip"
    };

    serde_json::json!({
        "tag_name": "v1.0.0",
        "name": "v1.0.0",
        "assets": [
            {
                "name": asset_name,
                "browser_download_url": asset_url,
                "size": asset_size,
                "content_type": content_type
            }
        ]
    })
    .to_string()
}

/// Route table shared between the test thread and per-connection handlers.
/// Each entry is `(path, body, content_type)`.
type Routes = Arc<Mutex<Vec<(String, Vec<u8>, &'static str)>>>;

/// Build a `<os>-<arch>` slug for the *current* host that the asset scorer in
/// `src/asset.rs` will accept. Hardcoding `linux-x64` made these tests fail on
/// any other host because the scorer correctly rejects wrong-platform assets.
fn host_platform_slug() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "darwin-arm64",
        ("macos", "x86_64") => "darwin-x64",
        ("linux", "aarch64") => "linux-arm64",
        ("linux", "x86_64") => "linux-x64",
        ("windows", "x86_64") => "windows-x64",
        ("windows", "aarch64") => "windows-arm64",
        (os, arch) => panic!("unsupported test host: {os}-{arch}"),
    }
}

fn handle_request(mut stream: TcpStream, routes: Routes) {
    let mut buf = [0u8; 4096];
    let n = match stream.read(&mut buf) {
        Ok(n) => n,
        Err(_) => return,
    };
    let request = String::from_utf8_lossy(&buf[..n]);
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/")
        .to_string();

    let routes = routes.lock().unwrap();
    if let Some((_, body, ct)) = routes.iter().find(|(p, _, _)| p == &path) {
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = stream.write_all(body);
    } else {
        let _ = write!(
            stream,
            "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
    }
}

#[test]
fn fetches_extracts_and_execs_a_binary() {
    let tarball = make_tarball("fake-tool");
    let tarball_size = tarball.len() as u64;

    let asset_name = format!("fake-tool-v1.0.0-{}.tar.gz", host_platform_slug());
    let asset_name = asset_name.as_str();

    let (listener, base) = bind_loopback();
    let asset_url = format!("{base}/download/{asset_name}");
    let json = release_json(&asset_url, asset_name, tarball_size);

    let routes = vec![
        (
            "/repos/test-owner/fake-tool/releases/tags/v1.0.0".to_string(),
            json.into_bytes(),
            "application/json",
        ),
        (
            format!("/download/{asset_name}"),
            tarball,
            "application/gzip",
        ),
    ];
    serve(listener, routes);

    let cache_root = tempfile::tempdir().unwrap();

    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("BX_CACHE_DIR", cache_root.path())
        .env("BX_LOG", "info")
        .arg("test-owner/fake-tool@v1.0.0")
        .arg("--")
        .arg("hello")
        .arg("world");

    let output = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(
        stdout.contains("fake-tool args: hello world"),
        "expected fake-tool stdout to be passed through, got: {stdout}"
    );

    // Second invocation with the same pinned ref must use the cache and not
    // touch the network at all — proved by pointing at an unreachable host.
    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env(
        "BX_GITHUB_API_BASE",
        "http://invalid-host-that-does-not-exist:1",
    )
    .env("BX_CACHE_DIR", cache_root.path())
    .arg("test-owner/fake-tool@v1.0.0")
    .arg("--")
    .arg("cached")
    .arg("invocation");

    let output = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&output.get_output().stdout);
    assert!(
        stdout.contains("fake-tool args: cached invocation"),
        "expected cached invocation to work without hitting the network, got: {stdout}"
    );
}

/// The same pipeline over a `.zip` instead of a `.tar.gz`.
///
/// `fetch::extract_or_place` picks the extractor from the asset's extension,
/// not the host OS, so this is reachable on every platform — and it is the
/// extension the scorer *prefers* on Windows, which made it the default
/// Windows path with zero runtime coverage until this test existed.
#[test]
fn fetches_extracts_and_execs_from_a_zip() {
    let archive = make_zip("zip-tool");
    let size = archive.len() as u64;
    let asset_name = format!("zip-tool-v1.0.0-{}.zip", host_platform_slug());
    let asset_name = asset_name.as_str();

    let (listener, base) = bind_loopback();
    let asset_url = format!("{base}/dl/{asset_name}");
    let json = release_json(&asset_url, asset_name, size);
    serve(
        listener,
        vec![
            (
                "/repos/o/zip-tool/releases/tags/v1.0.0".to_string(),
                json.into_bytes(),
                "application/json",
            ),
            (format!("/dl/{asset_name}"), archive, "application/zip"),
        ],
    );

    let cache_root = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("BX_CACHE_DIR", cache_root.path())
        .arg("o/zip-tool@v1.0.0")
        .arg("--")
        .arg("from")
        .arg("zip");

    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("zip-tool args: from zip"),
        "expected the zip-extracted binary to run with args passed through, got: {stdout}"
    );
}

#[test]
fn passes_through_nonzero_exit_codes() {
    let tarball = make_tarball("exit-test");
    let tarball_size = tarball.len() as u64;
    let asset_name = format!("exit-test-v1-{}.tar.gz", host_platform_slug());
    let asset_name = asset_name.as_str();

    let (listener, base) = bind_loopback();
    let asset_url = format!("{base}/dl/{asset_name}");
    let json = release_json(&asset_url, asset_name, tarball_size);

    let routes = vec![
        (
            "/repos/o/exit-test/releases/latest".to_string(),
            json.into_bytes(),
            "application/json",
        ),
        (format!("/dl/{asset_name}"), tarball, "application/gzip"),
    ];
    serve(listener, routes);

    let cache_root = tempfile::tempdir().unwrap();

    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("BX_CACHE_DIR", cache_root.path())
        .arg("o/exit-test")
        .arg("--")
        .arg("--exit")
        .arg("42");

    cmd.assert().code(42);
}

#[test]
fn reports_release_not_found_clearly() {
    let (listener, base) = bind_loopback();
    serve(listener, vec![]); // any request -> 404

    let cache_root = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("BX_CACHE_DIR", cache_root.path())
        .arg("nobody/nothing@v9.9.9");

    let assert = cmd.assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    assert!(
        stderr.contains("not found"),
        "expected clear 'not found' message, got: {stderr}"
    );
}

/// Prove the property MCP STDIO transport relies on: a line written to bx's
/// stdin reaches the child binary, and the child's reply lands on bx's stdout
/// unmodified. Stdio inheritance is the *documented* behaviour of
/// `Command::status` in `src/exec.rs`, but until this test landed nothing
/// actually verified it end-to-end.
///
/// Runs on Windows too, where stdio passthrough goes through `CreateProcess`
/// handle inheritance rather than fork/exec — a genuinely different mechanism
/// carrying the same contract.
#[test]
fn stdio_passes_through_to_child() {
    let tarball = make_tarball("stdio-echo");
    let tarball_size = tarball.len() as u64;
    let asset_name = format!("stdio-echo-v1-{}.tar.gz", host_platform_slug());
    let asset_name = asset_name.as_str();

    let (listener, base) = bind_loopback();
    let asset_url = format!("{base}/dl/{asset_name}");
    let json = release_json(&asset_url, asset_name, tarball_size);
    let routes = vec![
        (
            "/repos/o/stdio-echo/releases/latest".to_string(),
            json.into_bytes(),
            "application/json",
        ),
        (format!("/dl/{asset_name}"), tarball, "application/gzip"),
    ];
    serve(listener, routes);

    let cache_root = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("BX_CACHE_DIR", cache_root.path())
        .arg("o/stdio-echo")
        .arg("--")
        .arg("--echo-stdin")
        .arg("reply")
        .write_stdin("hello-mcp\n");

    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("reply:hello-mcp"),
        "expected stdin to pass through and child's reply to surface on stdout, got: {stdout}"
    );
}

/// macOS only: prove the Seatbelt path actually applies. `sandbox_init` must
/// accept the generated profile, the child must still run, and — critically —
/// stdin/stdout must pass through under the sandbox exactly as they do without
/// it (the MCP-stdio contract). Uses `--sandbox strict`, whose policy adds the
/// real cache root to the readable set so the fetched binary can be exec'd.
#[cfg(target_os = "macos")]
#[test]
fn macos_sandbox_strict_applies_and_passes_stdio() {
    let tarball = make_tarball("sb-echo");
    let tarball_size = tarball.len() as u64;
    let asset_name = format!("sb-echo-v1.0.0-{}.tar.gz", host_platform_slug());
    let asset_name = asset_name.as_str();

    let (listener, base) = bind_loopback();
    let asset_url = format!("{base}/dl/{asset_name}");
    let json = release_json(&asset_url, asset_name, tarball_size);
    let routes = vec![
        (
            "/repos/o/sb-echo/releases/tags/v1.0.0".to_string(),
            json.into_bytes(),
            "application/json",
        ),
        (format!("/dl/{asset_name}"), tarball, "application/gzip"),
    ];
    serve(listener, routes);

    let cache_root = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("BX_CACHE_DIR", cache_root.path())
        .arg("--sandbox")
        .arg("strict")
        .arg("o/sb-echo@v1.0.0")
        .arg("--")
        .arg("--echo-stdin")
        .arg("sandboxed")
        .write_stdin("ping\n");

    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("sandboxed:ping"),
        "expected sandboxed child to run and pass stdio through, got: {stdout}"
    );
}

/// Bind a loopback server socket and return it together with its base URL.
///
/// Keep the returned listener alive and hand it straight to [`serve`]. We
/// deliberately do *not* bind→drop→rebind to learn the port first: that pattern
/// raced a parallel test for the freed port and produced flaky `AddrInUse`
/// failures under `cargo test` (seen on the CI macOS runner). Binding once and
/// moving the live listener into the server thread closes the window entirely.
fn bind_loopback() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    (listener, base)
}

/// Stand up a fake repo `o/<tool>` whose only release (`v1.0.0`) ships the
/// fixture tool as a tar.gz, and return the API base URL to point bx at.
fn serve_fixture_repo(tool: &str) -> String {
    let tarball = make_tarball(tool);
    let size = tarball.len() as u64;
    let asset = format!("{tool}-v1.0.0-{}.tar.gz", host_platform_slug());

    let (listener, base) = bind_loopback();
    let asset_url = format!("{base}/dl/{asset}");
    serve(
        listener,
        vec![
            (
                format!("/repos/o/{tool}/releases/tags/v1.0.0"),
                release_json(&asset_url, &asset, size).into_bytes(),
                "application/json",
            ),
            (format!("/dl/{asset}"), tarball, "application/gzip"),
        ],
    );
    base
}

/// Serve `routes` from an already-bound `listener` on a detached thread.
fn serve(listener: TcpListener, routes: Vec<(String, Vec<u8>, &'static str)>) {
    let routes_arc: Routes = Arc::new(Mutex::new(routes));
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let routes = routes_arc.clone();
            thread::spawn(move || handle_request(stream, routes));
        }
    });
}

/// Linux only: verify `bwrap` is not just present but actually *usable* (an
/// unprivileged user namespace can be created). A developer machine without
/// user-namespace support should skip the enforcement test rather than fail
/// it — this mirrors `exec::fallback`, which runs unsandboxed when no backend
/// is available.
///
/// **CI is different.** `ci.yml` installs bubblewrap precisely so this test
/// exercises the real backend, so on CI an unusable bwrap is a failure, not a
/// skip. Without this the suite would go green having silently tested nothing
/// — the failure mode is invisible exactly when it matters.
#[cfg(target_os = "linux")]
fn require_bwrap_or_skip(test: &str) -> bool {
    let probe = std::process::Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--unshare-user", "true"])
        .output();

    let why = match &probe {
        Ok(o) if o.status.success() => return true,
        // Carry bwrap's own diagnosis into the failure. The usual cause is a
        // host that forbids unprivileged user namespaces — Ubuntu restricts
        // them by default via `kernel.apparmor_restrict_unprivileged_userns`,
        // which ci.yml relaxes for the Linux job.
        Ok(o) => format!(
            "bwrap exited {}: {}",
            o.status,
            String::from_utf8_lossy(&o.stderr).trim()
        ),
        Err(e) => format!("could not run bwrap: {e}"),
    };

    assert!(
        std::env::var_os("CI").is_none(),
        "{test}: bwrap is unusable but CI is set — ci.yml installs bubblewrap \
         and relaxes the userns restriction so this test covers the real \
         backend. Refusing to skip silently. {why}"
    );
    eprintln!("skipping {test}: {why}");
    false
}

/// Enforcement: prove `strict` actually *denies* a write the policy never
/// granted — not merely that the sandboxed child runs (which
/// `*_sandbox_strict_applies_and_passes_stdio` already covers). The fake tool
/// tries to create `probe.txt` in its cwd; under `strict` the cwd is read-only,
/// so the write must fail. A control run of the *same* fixture without
/// `--sandbox` must succeed, proving the denial came from the sandbox and not
/// some unrelated error — without that control a profile that silently broke
/// every exec would still pass.
///
/// Runs on all three backends. Windows enforces through AppContainer DACLs
/// rather than a launch-time allow-list, so this is the first test to execute
/// that code at all — `sandbox::appcontainer` unit tests only cover the pure
/// plan generator.
#[test]
fn sandbox_strict_denies_out_of_policy_write() {
    #[cfg(target_os = "linux")]
    if !require_bwrap_or_skip("sandbox_strict_denies_out_of_policy_write") {
        return;
    }

    let base = serve_fixture_repo("probe-tool");

    // Shared cache across both runs so the second is a pure cache hit (no
    // network). `strict` adds this cache root to the readable set, so the
    // sandboxed child can still exec the fetched binary out of it.
    let cache = tempfile::tempdir().unwrap();

    // Sandboxed run — also performs the one-time fetch. The write must be
    // DENIED and no file may appear in the cwd.
    let sb_dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("BX_CACHE_DIR", cache.path())
        .current_dir(sb_dir.path())
        .arg("--sandbox")
        .arg("strict")
        .arg("o/probe-tool@v1.0.0")
        .arg("--")
        .arg("--probe-write");
    let assert = cmd.assert().success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        out.contains("write:DENIED"),
        "strict must deny the out-of-policy write, got: {out}"
    );
    assert!(
        !sb_dir.path().join("probe.txt").exists(),
        "probe.txt must not be created under the sandbox"
    );

    // Control run — identical fixture, no sandbox, cache hit (pointed at an
    // unreachable host to prove no network). The write must SUCCEED.
    let ctl_dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env(
        "BX_GITHUB_API_BASE",
        "http://invalid-host-that-does-not-exist:1",
    )
    .env("BX_CACHE_DIR", cache.path())
    .current_dir(ctl_dir.path())
    .arg("o/probe-tool@v1.0.0")
    .arg("--")
    .arg("--probe-write");
    let assert = cmd.assert().success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        out.contains("write:ALLOWED"),
        "unsandboxed control must allow the write, got: {out}"
    );
    assert!(
        ctl_dir.path().join("probe.txt").exists(),
        "control run should have created probe.txt"
    );
}

/// Dump the DACL of `path` as `icacls` renders it.
///
/// Shelling out beats re-deriving the AppContainer SID in the test: the point
/// is that host ACL state is *observably unchanged*, and `icacls` is the
/// tool an operator would reach for to check that by hand.
#[cfg(windows)]
fn icacls(path: &Path) -> String {
    let out = std::process::Command::new("icacls")
        .arg(path)
        .output()
        .expect("run icacls");
    assert!(
        out.status.success(),
        "icacls {} failed: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// Windows only: the AppContainer backend is the only one that mutates host
/// state — it adds ACEs for the package SID to every policy path and relies on
/// `GrantGuard`'s `Drop` to put them back. Nothing verified that revert, so a
/// leak would accumulate silently on real users' directories.
///
/// Asserting the denial in the same test matters: without it a build where
/// sandboxing silently no-op'd would grant nothing, revert nothing, and pass.
#[cfg(windows)]
#[test]
fn windows_sandbox_reverts_dacls_on_exit() {
    let base = serve_fixture_repo("probe-tool");
    let cache = tempfile::tempdir().unwrap();
    let sb_dir = tempfile::tempdir().unwrap();

    let before = icacls(sb_dir.path());

    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("BX_CACHE_DIR", cache.path())
        .current_dir(sb_dir.path())
        .arg("--sandbox")
        .arg("strict")
        .arg("o/probe-tool@v1.0.0")
        .arg("--")
        .arg("--probe-write");
    let assert = cmd.assert().success();
    let out = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        out.contains("write:DENIED"),
        "sandbox must have been applied for the revert check to mean anything, got: {out}"
    );

    let after = icacls(sb_dir.path());
    assert_eq!(
        before, after,
        "GrantGuard must revert every ACE it added; cwd DACL differs after a sandboxed run"
    );
}

/// Windows only: a sandbox that cannot be *applied* is a hard error, never a
/// silent downgrade to unsandboxed. macOS/Linux warn and fall back when no
/// backend exists (`exec::fallback`); Windows has a backend, so a failed Win32
/// call must surface. `exec::launch` aborts on the first `grant_path` error.
///
/// The trigger is a policy path that does not exist — `build_plan` passes
/// paths through verbatim and `GetNamedSecurityInfoW` then fails on it. On
/// macOS/Linux the same manifest is harmless, which is why this is Windows-only.
#[cfg(windows)]
#[test]
fn windows_sandbox_fails_closed_when_a_policy_path_cannot_be_applied() {
    let base = serve_fixture_repo("probe-tool");
    let cache = tempfile::tempdir().unwrap();
    let dir = tempfile::tempdir().unwrap();

    std::fs::write(
        dir.path().join(".bx.toml"),
        r#"[[tool]]
spec = "o/probe-tool@v1.0.0"

[tool.sandbox]
profile = "strict"
readonly_paths = ["C:\\bx-nonexistent-path-fail-closed-probe"]
"#,
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("BX_CACHE_DIR", cache.path())
        .current_dir(dir.path())
        .arg("o/probe-tool@v1.0.0")
        .arg("--")
        .arg("--probe-write");

    let assert = cmd.assert().failure();
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr);
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);

    assert!(
        stderr.to_lowercase().contains("sandbox"),
        "expected a sandbox error explaining the failure, got stderr: {stderr}"
    );
    // The contract is that the binary does not run at all — not that it runs
    // and is denied. A downgrade would show the child's output here.
    assert!(
        !stdout.contains("write:"),
        "binary must not run when the sandbox cannot be applied, got stdout: {stdout}"
    );
    assert!(
        !dir.path().join("probe.txt").exists(),
        "binary must not have run, but it created probe.txt"
    );
}
