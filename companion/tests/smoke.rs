//! End-to-end smoke test: drives the proxy like Zed would, against the real
//! `lean --server`, and checks that goal state comes out of the unix socket.
//!
//! Skipped (passes trivially) when `lean` is not on PATH.

use std::io::{BufRead, BufReader, Error, ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn read_message(r: &mut impl BufRead) -> std::io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    let mut line = String::new();
    loop {
        line.clear();
        if r.read_line(&mut line)? == 0 {
            return Ok(None);
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(v) = line.strip_prefix("Content-Length:") {
            content_length = v.trim().parse().ok();
        }
    }
    let len = content_length.ok_or_else(|| Error::new(ErrorKind::InvalidData, "no length"))?;
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf)?;
    Ok(Some(buf))
}

fn send(w: &mut impl Write, msg: serde_json::Value) {
    let body = msg.to_string();
    write!(w, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
    w.flush().unwrap();
}

#[test]
fn proxy_end_to_end() {
    if Command::new("lean").arg("--version").output().is_err() {
        eprintln!("lean not found; skipping smoke test");
        return;
    }

    let root = std::env::temp_dir().join(format!("zed-lean4-companion-smoke-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let root = root.canonicalize().unwrap();
    let file = root.join("Smoke.lean");
    let text = "example : 1 + 1 = 2 := by\n  rfl\n";
    std::fs::write(&file, text).unwrap();
    let root_uri = format!("file://{}", root.display());
    let file_uri = format!("file://{}", file.display());

    let mut proxy = Command::new(env!("CARGO_BIN_EXE_zed-lean4-companion"))
        .args(["proxy", "--", "lean", "--server"])
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn proxy");
    let mut stdin = proxy.stdin.take().unwrap();
    let mut stdout = BufReader::new(proxy.stdout.take().unwrap());

    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": { "processId": null, "rootUri": root_uri, "capabilities": {} },
        }),
    );
    // The initialize response must pass through untouched.
    let resp = loop {
        let msg = read_message(&mut stdout).unwrap().expect("server closed");
        let v: serde_json::Value = serde_json::from_slice(&msg).unwrap();
        if v["id"] == 1 {
            break v;
        }
    };
    assert!(
        resp["result"]["capabilities"].is_object(),
        "bad initialize response: {resp}"
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
    // Cursor on line 1, before `rfl` — like Zed's documentHighlight on click.
    send(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "textDocument/documentHighlight",
            "params": {
                "textDocument": { "uri": file_uri },
                "position": { "line": 1, "character": 2 },
            },
        }),
    );

    // Connect to the socket the proxy announced for our root.
    let socket = {
        let mut hash: u64 = 0xcbf29ce484222325;
        for b in root.to_str().unwrap().bytes() {
            hash ^= b as u64;
            hash = hash.wrapping_mul(0x100000001b3);
        }
        std::env::temp_dir()
            .join("zed-lean4-companion")
            .join(format!("{hash:016x}.sock"))
    };
    let deadline = Instant::now() + Duration::from_secs(60);
    let stream = loop {
        match UnixStream::connect(&socket) {
            Ok(s) => break s,
            Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Err(e) => panic!("socket never appeared at {}: {e}", socket.display()),
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(60)))
        .unwrap();

    // Read state lines until the goal shows up.
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    let goals = loop {
        line.clear();
        assert!(reader.read_line(&mut line).unwrap() > 0, "socket closed");
        let state: serde_json::Value = serde_json::from_str(&line).unwrap();
        if let Some(goals) = state["goals"].as_array()
            && !goals.is_empty()
        {
            break goals.clone();
        }
        assert!(Instant::now() < deadline, "no goals before timeout");
    };
    let target: String = goals[0]["target"]
        .as_array()
        .expect("structured goal target")
        .iter()
        .map(|s| s["text"].as_str().unwrap_or(""))
        .collect();
    assert!(target.contains("1 + 1 = 2"), "unexpected goal: {target}");

    // The documentHighlight response (or error) must still reach the client.
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let msg = read_message(&mut stdout).unwrap().expect("server closed");
        let v: serde_json::Value = serde_json::from_slice(&msg).unwrap();
        assert!(
            !v["id"].as_str().is_some_and(|s| s.starts_with("infoview:")),
            "proxy leaked its own response to the client: {v}"
        );
        if v["id"] == 2 {
            break;
        }
        assert!(Instant::now() < deadline, "no documentHighlight response");
    }

    send(
        &mut stdin,
        serde_json::json!({ "jsonrpc": "2.0", "id": 3, "method": "shutdown", "params": null }),
    );
    send(
        &mut stdin,
        serde_json::json!({ "jsonrpc": "2.0", "method": "exit", "params": null }),
    );
    drop(stdin);
    let status = proxy.wait().unwrap();
    assert!(status.success(), "proxy exited with {status}");
    assert!(!socket.exists(), "socket file not cleaned up");
    let _ = std::fs::remove_dir_all(&root);
}
