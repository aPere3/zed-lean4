//! LSP proxy: sits between Zed and the real Lean server (`lake serve --`).
//!
//! All traffic is passed through unchanged. On the side, the proxy:
//! - tracks the cursor from `textDocument/documentHighlight` requests
//!   (Zed sends one on every selection change);
//! - sends its own `$/lean/plainGoal` / `$/lean/plainTermGoal` requests
//!   (string ids prefixed `infoview:`, so they never collide with Zed's
//!   numeric ids, and their responses are filtered out of the stream);
//! - collects `textDocument/publishDiagnostics` and `$/lean/fileProgress`;
//! - broadcasts an `InfoviewState` JSON line to every watcher connected
//!   on the unix socket, on every change.

use crate::rpc::{read_message, write_message};
use crate::state::{Diag, InfoviewState, socket_dir, socket_path_for_root};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufReader, Result, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone)]
struct Cursor {
    uri: String,
    line: u64,
    character: u64,
}

struct Shared {
    state: Mutex<InfoviewState>,
    clients: Mutex<Vec<UnixStream>>,
    child_stdin: Mutex<ChildStdin>,
    cursor: Mutex<Option<Cursor>>,
    diags: Mutex<HashMap<String, Vec<Diag>>>,
    /// Generation of the last goal-request pair; stale responses are dropped.
    generation: AtomicU64,
    socket_path: Mutex<Option<std::path::PathBuf>>,
}

pub fn run(server_cmd: Vec<String>) -> Result<()> {
    let mut child = Command::new(&server_cmd[0])
        .args(&server_cmd[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()?;

    let child_stdin = child.stdin.take().expect("child stdin is piped");
    let child_stdout = child.stdout.take().expect("child stdout is piped");

    let shared = Arc::new(Shared {
        state: Mutex::new(InfoviewState {
            server_alive: true,
            ..Default::default()
        }),
        clients: Mutex::new(Vec::new()),
        child_stdin: Mutex::new(child_stdin),
        cursor: Mutex::new(None),
        diags: Mutex::new(HashMap::new()),
        generation: AtomicU64::new(0),
        socket_path: Mutex::new(None),
    });

    // Server -> Zed.
    let s2c = {
        let shared = shared.clone();
        std::thread::spawn(move || {
            let mut child_out = BufReader::new(child_stdout);
            let mut out = std::io::stdout();
            while let Ok(Some(msg)) = read_message(&mut child_out) {
                if let Ok(v) = serde_json::from_slice::<Value>(&msg) {
                    if handle_our_response(&shared, &v) {
                        continue;
                    }
                    inspect_server_msg(&shared, &v);
                }
                if write_message(&mut out, &msg).is_err() {
                    break;
                }
            }
            // Server is gone: tell watchers, then clean up.
            shared.state.lock().unwrap().server_alive = false;
            broadcast(&shared);
            if let Some(p) = shared.socket_path.lock().unwrap().take() {
                let _ = std::fs::remove_file(p);
            }
        })
    };

    // Zed -> server (main thread). Forward first, inspect after: goal
    // requests triggered by a didChange must reach the server after the
    // edit itself, or they are answered against the old document.
    let mut stdin = BufReader::new(std::io::stdin());
    while let Ok(Some(msg)) = read_message(&mut stdin) {
        {
            let mut child_in = shared.child_stdin.lock().unwrap();
            if write_message(&mut *child_in, &msg).is_err() {
                break;
            }
        }
        if let Ok(v) = serde_json::from_slice::<Value>(&msg) {
            inspect_client_msg(&shared, &v);
        }
    }

    // Zed closed our stdin: close the server's stdin so it exits too.
    drop(shared.child_stdin.lock().unwrap().flush());
    let _ = s2c.join();
    let _ = child.wait();
    if let Some(p) = shared.socket_path.lock().unwrap().take() {
        let _ = std::fs::remove_file(p);
    }
    Ok(())
}

/// Goes to stderr, which Zed shows in the language server logs panel.
fn log(msg: std::fmt::Arguments) {
    eprintln!("[companion] {msg}");
}

fn inspect_client_msg(shared: &Arc<Shared>, v: &Value) {
    if let Some(method) = v["method"].as_str() {
        log(format_args!("<- client: {method}"));
    }
    match v["method"].as_str() {
        Some("initialize") => {
            let root = v["params"]["rootUri"]
                .as_str()
                .or_else(|| v["params"]["rootPath"].as_str())
                .unwrap_or("unknown");
            let root = root.strip_prefix("file://").unwrap_or(root).to_string();
            start_socket(shared, &root);
        }
        Some("textDocument/documentHighlight") => {
            let p = &v["params"];
            if let (Some(uri), Some(line), Some(character)) = (
                p["textDocument"]["uri"].as_str(),
                p["position"]["line"].as_u64(),
                p["position"]["character"].as_u64(),
            ) {
                *shared.cursor.lock().unwrap() = Some(Cursor {
                    uri: uri.to_string(),
                    line,
                    character,
                });
                send_goal_requests(shared);
            }
        }
        Some("textDocument/didChange") => {
            // Track typing: move the cursor to the end of the last change.
            let p = &v["params"];
            if let Some(uri) = p["textDocument"]["uri"].as_str()
                && let Some(changes) = p["contentChanges"].as_array()
                && let Some(last) = changes.last()
                && let (Some(line), Some(character)) = (
                    last["range"]["end"]["line"].as_u64(),
                    last["range"]["end"]["character"].as_u64(),
                )
            {
                let inserted = last["text"].as_str().unwrap_or("");
                let (line, character) = advance(line, character, inserted);
                *shared.cursor.lock().unwrap() = Some(Cursor {
                    uri: uri.to_string(),
                    line,
                    character,
                });
                send_goal_requests(shared);
            }
        }
        _ => {}
    }
}

/// Position after inserting `text` at (line, character).
fn advance(line: u64, character: u64, text: &str) -> (u64, u64) {
    let newlines = text.matches('\n').count() as u64;
    if newlines == 0 {
        (line, character + text.chars().count() as u64)
    } else {
        let last = text.rsplit('\n').next().unwrap_or("");
        (line + newlines, last.chars().count() as u64)
    }
}

fn inspect_server_msg(shared: &Arc<Shared>, v: &Value) {
    match v["method"].as_str() {
        Some("textDocument/publishDiagnostics") => {
            let p = &v["params"];
            let Some(uri) = p["uri"].as_str() else { return };
            let diags: Vec<Diag> = p["diagnostics"]
                .as_array()
                .map(|ds| {
                    ds.iter()
                        .map(|d| Diag {
                            line: d["range"]["start"]["line"].as_u64().unwrap_or(0) as u32,
                            column: d["range"]["start"]["character"].as_u64().unwrap_or(0) as u32,
                            severity: d["severity"].as_u64().unwrap_or(1) as u8,
                            message: d["message"].as_str().unwrap_or("").to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default();
            shared
                .diags
                .lock()
                .unwrap()
                .insert(uri.to_string(), diags.clone());
            let at_cursor = shared
                .cursor
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|c| c.uri == uri);
            if at_cursor {
                shared.state.lock().unwrap().diagnostics = diags;
                broadcast(shared);
            }
        }
        Some("$/lean/fileProgress") => {
            let p = &v["params"];
            let Some(uri) = p["textDocument"]["uri"].as_str() else {
                return;
            };
            let processing = p["processing"]
                .as_array()
                .is_some_and(|a| !a.is_empty());
            let at_cursor = shared
                .cursor
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|c| c.uri == uri);
            if at_cursor {
                let changed = {
                    let mut st = shared.state.lock().unwrap();
                    let changed = st.processing != processing;
                    st.processing = processing;
                    changed
                };
                // Elaboration finished: refresh the goals at the cursor.
                if changed && !processing {
                    send_goal_requests(shared);
                } else if changed {
                    broadcast(shared);
                }
            }
        }
        _ => {}
    }
}

fn send_goal_requests(shared: &Arc<Shared>) {
    let Some(c) = shared.cursor.lock().unwrap().clone() else {
        return;
    };
    let generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
    log(format_args!(
        "-> goals? gen={generation} {}:{}:{}",
        c.uri, c.line, c.character
    ));
    let params = json!({
        "textDocument": { "uri": c.uri },
        "position": { "line": c.line, "character": c.character },
    });
    let reqs = [
        json!({
            "jsonrpc": "2.0",
            "id": format!("infoview:g:{generation}"),
            "method": "$/lean/plainGoal",
            "params": params,
        }),
        json!({
            "jsonrpc": "2.0",
            "id": format!("infoview:t:{generation}"),
            "method": "$/lean/plainTermGoal",
            "params": params,
        }),
    ];
    {
        let mut child_in = shared.child_stdin.lock().unwrap();
        for r in &reqs {
            let _ = write_message(&mut *child_in, r.to_string().as_bytes());
        }
    }
    {
        let mut st = shared.state.lock().unwrap();
        st.line = c.line as u32;
        st.column = c.character as u32;
        if st.uri.as_deref() != Some(&c.uri) {
            st.uri = Some(c.uri.clone());
            st.diagnostics = shared
                .diags
                .lock()
                .unwrap()
                .get(&c.uri)
                .cloned()
                .unwrap_or_default();
        }
    }
    broadcast(shared);
}

/// Consumes responses to our own requests. Returns true if the message was ours.
fn handle_our_response(shared: &Arc<Shared>, v: &Value) -> bool {
    let Some(id) = v["id"].as_str() else {
        return false;
    };
    let Some(rest) = id.strip_prefix("infoview:") else {
        return false;
    };
    // A request *from* the server could in principle carry any id; only treat
    // id-only messages (responses) as ours.
    if v["method"].as_str().is_some() {
        return false;
    }
    let (kind, generation) = match rest.split_once(':') {
        Some((k, g)) => (k, g.parse::<u64>().unwrap_or(0)),
        None => return true,
    };
    if generation != shared.generation.load(Ordering::SeqCst) {
        log(format_args!("<- stale response {kind} gen={generation}"));
        return true;
    }
    if !v["error"].is_null() {
        log(format_args!(
            "<- error for {kind} gen={generation}: {}",
            v["error"]
        ));
        return true; // keep the previous state
    }
    let result = &v["result"];
    log(format_args!(
        "<- result for {kind} gen={generation}: {}",
        if result.is_null() { "null" } else { "ok" }
    ));
    {
        let mut st = shared.state.lock().unwrap();
        match kind {
            "g" => {
                st.goals = if result.is_null() {
                    None
                } else if let Some(goals) = result["goals"].as_array() {
                    Some(
                        goals
                            .iter()
                            .filter_map(|g| g.as_str().map(String::from))
                            .collect(),
                    )
                } else {
                    // Old servers: only `rendered` (markdown). Strip the fences.
                    let rendered = result["rendered"].as_str().unwrap_or("");
                    let text: String = rendered
                        .lines()
                        .filter(|l| !l.trim_start().starts_with("```"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let text = text.trim();
                    if text.is_empty() {
                        Some(vec![])
                    } else {
                        Some(vec![text.to_string()])
                    }
                };
            }
            "t" => {
                st.term_goal = result["goal"].as_str().map(String::from);
            }
            _ => {}
        }
    }
    broadcast(shared);
    true
}

fn start_socket(shared: &Arc<Shared>, root: &str) {
    let dir = socket_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = socket_path_for_root(root);
    let _ = std::fs::remove_file(&path); // stale socket from a previous run
    let listener = match UnixListener::bind(&path) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("zed-lean4-companion: cannot bind {}: {e}", path.display());
            return;
        }
    };
    eprintln!("zed-lean4-companion: socket at {}", path.display());
    *shared.socket_path.lock().unwrap() = Some(path);

    let shared = shared.clone();
    std::thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let mut stream = stream;
            // Greet the new watcher with the current state.
            let line = current_state_line(&shared);
            if stream.write_all(line.as_bytes()).is_ok() {
                shared.clients.lock().unwrap().push(stream);
            }
        }
    });
}

fn current_state_line(shared: &Arc<Shared>) -> String {
    let st = shared.state.lock().unwrap();
    let mut line = serde_json::to_string(&*st).unwrap_or_else(|_| "{}".into());
    line.push('\n');
    line
}

fn broadcast(shared: &Arc<Shared>) {
    let line = current_state_line(shared);
    let mut clients = shared.clients.lock().unwrap();
    clients.retain_mut(|c| c.write_all(line.as_bytes()).is_ok());
}
