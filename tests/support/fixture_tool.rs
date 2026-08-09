//! The "binary" that the end-to-end tests pretend to download from GitHub.
//!
//! This used to be a `#!/bin/sh` script embedded in the test fixtures, which
//! is why the whole integration suite was Unix-only and the Windows CI job
//! could run nothing but `cargo test --lib`. A real compiled executable works
//! identically on all three runners, so every e2e test now covers Windows too.
//!
//! It is compiled on demand by `tests/end_to_end.rs` (see `fixture_tool`),
//! with `rustc` directly rather than as a Cargo target: a `[[bin]]` would be
//! picked up by `cargo install`, and an `[[example]]` would not be built by
//! the `cargo test --test end_to_end` invocation that CLAUDE.md documents.
//! It has no dependencies beyond `std` for that reason — keep it that way.
//!
//! Behaviour is selected by the first argument. bx passes everything after
//! `--` straight through, so a test drives this with e.g.
//! `bx owner/repo@v1.0.0 -- --exit 42`.
//!
//!   --exit <code>       exit with <code>
//!   --echo-stdin <tag>  read one line from stdin, print "<tag>:<line>"
//!   --probe-write       try to create ./probe.txt, print write:ALLOWED|DENIED
//!   (anything else)     print "<argv0-stem> args: <args joined by spaces>"
//!
//! The default branch prints its own argv[0] stem so a test can assert that
//! bx found and exec'd the *right* binary, not merely something executable.

use std::io::{BufRead, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();

    match args.first().map(String::as_str) {
        Some("--exit") => {
            let code: i32 = args
                .get(1)
                .expect("--exit needs a code")
                .parse()
                .expect("--exit code must be an integer");
            std::process::exit(code);
        }

        Some("--echo-stdin") => {
            let tag = args.get(1).expect("--echo-stdin needs a tag");
            let mut line = String::new();
            std::io::stdin()
                .lock()
                .read_line(&mut line)
                .expect("read stdin");
            println!("{tag}:{}", line.trim_end_matches(['\r', '\n']));
        }

        Some("--probe-write") => {
            // Deliberately not `?`/unwrap: a denial is the expected outcome
            // under a sandbox, and we must report it rather than panic.
            let denied = std::fs::File::create("probe.txt")
                .and_then(|mut f| f.write_all(b"x"))
                .is_err();
            println!("write:{}", if denied { "DENIED" } else { "ALLOWED" });
        }

        _ => {
            let argv0 = std::env::args().next().unwrap_or_default();
            let stem = std::path::Path::new(&argv0)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("fixture")
                .to_string();
            println!("{stem} args: {}", args.join(" "));
        }
    }

    // MCP clients read line-framed JSON-RPC off this stream; flush explicitly
    // so a test never sees a truncated line if stdout is a pipe.
    std::io::stdout().flush().expect("flush stdout");
}
