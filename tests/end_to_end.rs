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
use std::net::{TcpListener, TcpStream};
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
