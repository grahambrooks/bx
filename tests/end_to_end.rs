//! End-to-end integration test for bx.
//!
//! Spins up a local HTTP server that pretends to be GitHub Releases. Points
//! bx at it via `BX_GITHUB_API_BASE`. Serves a synthetic `.tar.gz` containing
//! a shell script as the "binary". Verifies bx fetches, extracts, finds the
//! binary, and execs it with passed-through args.
//!
//! Run with: `cargo test --test end_to_end -- --nocapture`

use assert_cmd::Command;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::thread;

/// Build a tar.gz in-memory containing a single executable file
/// `fake-tool/fake-tool` that prints its args.
fn make_tarball(binary_name: &str) -> Vec<u8> {
    let script_body = b"#!/bin/sh\necho \"fake-tool args: $*\"\nexit 0\n";

    let mut header = tar::Header::new_gnu();
    header.set_path(binary_name).unwrap();
    header.set_size(script_body.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();

    let buf: Vec<u8> = Vec::new();
    let encoder = GzEncoder::new(buf, Compression::default());
    let mut tar = tar::Builder::new(encoder);
    tar.append(&header, &script_body[..]).unwrap();
    let encoder = tar.into_inner().unwrap();
    encoder.finish().unwrap()
}

fn release_json(asset_url: &str, asset_name: &str, asset_size: u64) -> String {
    serde_json::json!({
        "tag_name": "v1.0.0",
        "name": "v1.0.0",
        "assets": [
            {
                "name": asset_name,
                "browser_download_url": asset_url,
                "size": asset_size,
                "content_type": "application/gzip"
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

    // We'll fill in the server addr after start to construct the asset URL.
    // To break the cycle: start the server with a placeholder, then mutate the
    // routes. Simpler: just construct routes with a known addr by binding twice.
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    drop(listener); // free the port

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

    // Bind the actual server on the same port we previously released.
    // There's a small race here but for a local test it's fine.
    let listener = TcpListener::bind(addr).unwrap();
    let routes_arc = Arc::new(Mutex::new(routes));
    let _server = thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let routes = routes_arc.clone();
            thread::spawn(move || handle_request(stream, routes));
        }
    });

    let cache_root = tempfile::tempdir().unwrap();

    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("XDG_CACHE_HOME", cache_root.path())
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
    .env("XDG_CACHE_HOME", cache_root.path())
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

#[test]
fn passes_through_nonzero_exit_codes() {
    // Build a tarball whose script exits with a specific code.
    let script_body = b"#!/bin/sh\nexit 42\n";
    let mut header = tar::Header::new_gnu();
    header.set_path("exit-test").unwrap();
    header.set_size(script_body.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();

    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = tar::Builder::new(encoder);
    tar.append(&header, &script_body[..]).unwrap();
    let tarball = tar.into_inner().unwrap().finish().unwrap();
    let tarball_size = tarball.len() as u64;
    let asset_name = format!("exit-test-v1-{}.tar.gz", host_platform_slug());
    let asset_name = asset_name.as_str();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    drop(listener);

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

    let listener = TcpListener::bind(addr).unwrap();
    let routes_arc = Arc::new(Mutex::new(routes));
    let _server = thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let routes = routes_arc.clone();
            thread::spawn(move || handle_request(stream, routes));
        }
    });

    let cache_root = tempfile::tempdir().unwrap();

    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("XDG_CACHE_HOME", cache_root.path())
        .arg("o/exit-test");

    cmd.assert().code(42);
}

#[test]
fn reports_release_not_found_clearly() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    drop(listener);

    let listener = TcpListener::bind(addr).unwrap();
    let routes_arc: Routes = Arc::new(Mutex::new(vec![])); // any request -> 404
    let _server = thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let routes = routes_arc.clone();
            thread::spawn(move || handle_request(stream, routes));
        }
    });

    let cache_root = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("XDG_CACHE_HOME", cache_root.path())
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
#[test]
#[cfg(unix)]
fn stdio_passes_through_to_child() {
    let script_body = b"#!/bin/sh\nIFS= read -r line\nprintf 'reply:%s\\n' \"$line\"\n";
    let mut header = tar::Header::new_gnu();
    header.set_path("stdio-echo").unwrap();
    header.set_size(script_body.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = tar::Builder::new(encoder);
    tar.append(&header, &script_body[..]).unwrap();
    let tarball = tar.into_inner().unwrap().finish().unwrap();
    let tarball_size = tarball.len() as u64;
    let asset_name = format!("stdio-echo-v1-{}.tar.gz", host_platform_slug());
    let asset_name = asset_name.as_str();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    drop(listener);

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

    let listener = TcpListener::bind(addr).unwrap();
    let routes_arc: Routes = Arc::new(Mutex::new(routes));
    let _server = thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let routes = routes_arc.clone();
            thread::spawn(move || handle_request(stream, routes));
        }
    });

    let cache_root = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("XDG_CACHE_HOME", cache_root.path())
        .arg("o/stdio-echo")
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
    let script_body = b"#!/bin/sh\nIFS= read -r line\nprintf 'sandboxed:%s\\n' \"$line\"\n";
    let mut header = tar::Header::new_gnu();
    header.set_path("sb-echo").unwrap();
    header.set_size(script_body.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut tar = tar::Builder::new(encoder);
    tar.append(&header, &script_body[..]).unwrap();
    let tarball = tar.into_inner().unwrap().finish().unwrap();
    let tarball_size = tarball.len() as u64;
    let asset_name = format!("sb-echo-v1.0.0-{}.tar.gz", host_platform_slug());
    let asset_name = asset_name.as_str();

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    drop(listener);

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

    let listener = TcpListener::bind(addr).unwrap();
    let routes_arc: Routes = Arc::new(Mutex::new(routes));
    let _server = thread::spawn(move || {
        for stream in listener.incoming() {
            let stream = match stream {
                Ok(s) => s,
                Err(_) => continue,
            };
            let routes = routes_arc.clone();
            thread::spawn(move || handle_request(stream, routes));
        }
    });

    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .arg("--sandbox")
        .arg("strict")
        .arg("o/sb-echo@v1.0.0")
        .write_stdin("ping\n");

    let assert = cmd.assert().success();
    let stdout = String::from_utf8_lossy(&assert.get_output().stdout);
    assert!(
        stdout.contains("sandboxed:ping"),
        "expected sandboxed child to run and pass stdio through, got: {stdout}"
    );
}

/// Build a `tar.gz` containing a single executable script at `name`.
fn script_tarball(name: &str, body: &[u8]) -> Vec<u8> {
    let mut header = tar::Header::new_gnu();
    header.set_path(name).unwrap();
    header.set_size(body.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    let mut tar = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    tar.append(&header, body).unwrap();
    tar.into_inner().unwrap().finish().unwrap()
}

/// Reserve a loopback port and return its `(addr, base_url)`. The caller builds
/// the route table (which must embed `base`) and then hands `addr` to [`listen`].
/// Mirrors the bind→drop→rebind dance used inline by the other tests so the
/// asset URL can carry the port before the server starts listening.
fn reserve() -> (SocketAddr, String) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    let base = format!("http://{addr}");
    (addr, base)
}

/// Start the fake-GitHub server on `addr` serving `routes` (detached thread).
fn listen(addr: SocketAddr, routes: Vec<(String, Vec<u8>, &'static str)>) {
    let listener = TcpListener::bind(addr).unwrap();
    let routes_arc: Routes = Arc::new(Mutex::new(routes));
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let routes = routes_arc.clone();
            thread::spawn(move || handle_request(stream, routes));
        }
    });
}

/// Linux only: verify `bwrap` is not just present but actually *usable* (an
/// unprivileged user namespace can be created). CI without user-namespace
/// support should skip the enforcement test rather than fail it — this mirrors
/// `exec::fallback`, which runs unsandboxed when no backend is available.
#[cfg(target_os = "linux")]
fn bwrap_usable() -> bool {
    std::process::Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--unshare-user", "true"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Enforcement: prove `strict` actually *denies* a write the policy never
/// granted — not merely that the sandboxed child runs (which
/// `*_sandbox_strict_applies_and_passes_stdio` already covers). The fake tool
/// tries to create `probe.txt` in its cwd; under `strict` the cwd is read-only,
/// so the write must fail. A control run of the *same* fixture without
/// `--sandbox` must succeed, proving the denial came from the sandbox and not
/// some unrelated error — without that control a profile that silently broke
/// every exec would still pass.
#[cfg(any(target_os = "macos", target_os = "linux"))]
#[test]
fn sandbox_strict_denies_out_of_policy_write() {
    #[cfg(target_os = "linux")]
    if !bwrap_usable() {
        eprintln!(
            "skipping sandbox_strict_denies_out_of_policy_write: bwrap unusable on this host"
        );
        return;
    }

    let script =
        b"#!/bin/sh\nif printf 'x' > probe.txt 2>/dev/null; then printf 'write:ALLOWED\\n'; else printf 'write:DENIED\\n'; fi\n";
    let tarball = script_tarball("probe-tool", script);
    let size = tarball.len() as u64;
    let asset = format!("probe-tool-v1.0.0-{}.tar.gz", host_platform_slug());

    let (addr, base) = reserve();
    let asset_url = format!("{base}/dl/{asset}");
    listen(
        addr,
        vec![
            (
                "/repos/o/probe-tool/releases/tags/v1.0.0".to_string(),
                release_json(&asset_url, &asset, size).into_bytes(),
                "application/json",
            ),
            (format!("/dl/{asset}"), tarball, "application/gzip"),
        ],
    );

    // Shared cache across both runs so the second is a pure cache hit (no
    // network). `strict` adds this cache root to the readable set, so the
    // sandboxed child can still exec the fetched binary out of it.
    let cache = tempfile::tempdir().unwrap();

    // Sandboxed run — also performs the one-time fetch. The write must be
    // DENIED and no file may appear in the cwd.
    let sb_dir = tempfile::tempdir().unwrap();
    let mut cmd = Command::cargo_bin("bx").unwrap();
    cmd.env("BX_GITHUB_API_BASE", &base)
        .env("XDG_CACHE_HOME", cache.path())
        .current_dir(sb_dir.path())
        .arg("--sandbox")
        .arg("strict")
        .arg("o/probe-tool@v1.0.0");
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
    .env("XDG_CACHE_HOME", cache.path())
    .current_dir(ctl_dir.path())
    .arg("o/probe-tool@v1.0.0");
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
