//! Manual check against a real Mathlib project. Run with:
//! `cargo test --test realproj -- --ignored --nocapture`

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const ROOT: &str = "/Users/apere/Repositories/lean-test";

fn send(w: &mut impl Write, msg: serde_json::Value) {
    let body = msg.to_string();
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    w.flush().unwrap();
}

#[test]
#[ignore]
fn goals_in_real_project() {
    let file_uri = format!("file://{ROOT}/Scratch.lean");
    let text = "import Mathlib\n\n\
        example : \u{2200} m n : Nat, Even n \u{2192} Even (m * n) := by\n  \
        rintro m n hn\n  \
        exact hn.mul_left m\n";

    let mut proxy = Command::new(env!("CARGO_BIN_EXE_zed-lean4-companion"))
        .args(["proxy", "--", "lake", "serve", "--"])
        .current_dir(ROOT)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn proxy");
    let mut stdin = proxy.stdin.take().unwrap();

    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "processId": null, "rootUri": format!("file://{ROOT}"), "capabilities": {} },
        }),
    );
    send(
        &mut stdin,
        serde_json::json!({ "jsonrpc": "2.0", "method": "initialized", "params": {} }),
    );
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "method": "textDocument/didOpen",
            "params": { "textDocument": {
                "uri": file_uri, "languageId": "lean4", "version": 1, "text": text,
            }},
        }),
    );
    // Cursor at start of `rintro`, inside the `by` block.
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "textDocument/documentHighlight",
            "params": {
                "textDocument": { "uri": file_uri },
                "position": { "line": 3, "character": 2 },
            },
        }),
    );

    let socket = {
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in ROOT.bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        std::env::temp_dir()
            .join("zed-lean4-companion")
            .join(format!("{hash:016x}.sock"))
    };
    let deadline = Instant::now() + Duration::from_secs(300);
    let stream = loop {
        match UnixStream::connect(&socket) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(200)),
            Err(e) => panic!("no socket at {}: {e}", socket.display()),
        }
    };
    stream.set_read_timeout(Some(Duration::from_secs(300))).unwrap();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0, "socket closed");
        let state: serde_json::Value = serde_json::from_str(&line).unwrap();
        eprintln!(
            "STATE goals={} term={} processing={}",
            state["goals"], state["term_goal"], state["processing"]
        );
        if state["goals"].as_array().is_some_and(|g| !g.is_empty()) {
            break;
        }
        assert!(Instant::now() < deadline, "no goals before timeout");
    }
    let _ = proxy.kill();
}
