//! LSP proxy: sits between Zed and the real Lean server (`lake serve --`).
//!
//! All traffic is passed through unchanged. On the side, the proxy:
//! - tracks the cursor from `textDocument/documentHighlight` requests
//!   (Zed sends one on every selection change) and from `didChange`;
//! - keeps one `$/lean/rpc/connect` session per file (with keep-alives) and
//!   queries `Lean.Widget.getInteractiveGoals` / `getInteractiveTermGoal` /
//!   `getInteractiveDiagnostics` at the cursor. Its own requests use string
//!   ids prefixed `infoview:`, so they never collide with Zed's numeric ids,
//!   and their responses are filtered out of the stream;
//! - releases the RpcPtr references it received once they are flattened,
//!   keeping only lazy-trace-children pointers for on-demand expansion;
//! - broadcasts an `InfoviewState` JSON line to every watcher connected on
//!   the unix socket, on every change, and accepts `{"expand": <trace id>}`
//!   commands from watchers to fetch lazy trace children.

use crate::convert;
use crate::rpc::{read_message, write_message};
use crate::state::{Diag, InfoviewState, MsgSeg, TextSpan, socket_dir, socket_path_for_root};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::io::{BufRead, BufReader, Result, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::process::{ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Lean server error code: the RPC session has expired.
const RPC_NEEDS_RECONNECT: i64 = -32900;

#[derive(Clone)]
struct Cursor {
    uri: String,
    line: u64,
    character: u64,
}

struct ExpandReq {
    generation: u64,
    node_id: u32,
    uri: String,
    /// The LazyTraceChildren pointer, released once the call has answered.
    ptr: Value,
}

struct Shared {
    state: Mutex<InfoviewState>,
    clients: Mutex<Vec<UnixStream>>,
    child_stdin: Mutex<ChildStdin>,
    cursor: Mutex<Option<Cursor>>,
    /// Plain diagnostics per uri, shown until interactive ones arrive.
    plain_diags: Mutex<HashMap<String, Vec<Diag>>>,
    /// uri -> RPC sessionId (opaque JSON value).
    sessions: Mutex<HashMap<String, Value>>,
    /// Pending `$/lean/rpc/connect` request id -> uri.
    pending_connect: Mutex<HashMap<String, String>>,
    /// Pending goal/term/diag request id -> uri (needed to release refs).
    pending_goal: Mutex<HashMap<String, String>>,
    /// Pending trace-expansion request id -> request info.
    pending_expand: Mutex<HashMap<String, ExpandReq>>,
    /// Generation of the last goal-request wave; stale responses are dropped.
    generation: AtomicU64,
    /// Generation the displayed interactive diagnostics belong to.
    diag_generation: AtomicU64,
    /// Uri the displayed interactive diagnostics belong to.
    diag_uri: Mutex<Option<String>>,
    next_req: AtomicU64,
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
        plain_diags: Mutex::new(HashMap::new()),
        sessions: Mutex::new(HashMap::new()),
        pending_connect: Mutex::new(HashMap::new()),
        pending_goal: Mutex::new(HashMap::new()),
        pending_expand: Mutex::new(HashMap::new()),
        generation: AtomicU64::new(0),
        diag_generation: AtomicU64::new(0),
        diag_uri: Mutex::new(None),
        next_req: AtomicU64::new(0),
        socket_path: Mutex::new(None),
    });

    // RPC sessions expire without keep-alives (VSCode sends one every 10 s).
    {
        let shared = shared.clone();
        std::thread::spawn(move || {
            loop {
                std::thread::sleep(Duration::from_secs(10));
                let sessions: Vec<(String, Value)> = shared
                    .sessions
                    .lock()
                    .unwrap()
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect();
                for (uri, session_id) in sessions {
                    send_to_server(
                        &shared,
                        &json!({
                            "jsonrpc": "2.0",
                            "method": "$/lean/rpc/keepAlive",
                            "params": { "uri": uri, "sessionId": session_id },
                        }),
                    );
                }
            }
        });
    }

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

fn send_to_server(shared: &Shared, msg: &Value) {
    let mut child_in = shared.child_stdin.lock().unwrap();
    let _ = write_message(&mut *child_in, msg.to_string().as_bytes());
}

fn inspect_client_msg(shared: &Arc<Shared>, v: &Value) {
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
                            message: vec![MsgSeg::Text(TextSpan {
                                text: d["message"].as_str().unwrap_or("").to_string(),
                                diff: None,
                            })],
                        })
                        .collect()
                })
                .unwrap_or_default();
            shared
                .plain_diags
                .lock()
                .unwrap()
                .insert(uri.to_string(), diags.clone());
            let at_cursor = shared
                .cursor
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|c| c.uri == uri);
            // Show plain diagnostics only until interactive ones exist;
            // afterwards the next wave (fileProgress done, cursor move)
            // refreshes the interactive ones without flicker.
            if at_cursor && shared.diag_generation.load(Ordering::SeqCst) == 0 {
                shared.state.lock().unwrap().diagnostics = diags;
                broadcast(shared);
            }
        }
        Some("$/lean/fileProgress") => {
            let p = &v["params"];
            let Some(uri) = p["textDocument"]["uri"].as_str() else {
                return;
            };
            let processing = p["processing"].as_array().is_some_and(|a| !a.is_empty());
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
                // Elaboration finished: refresh everything at the cursor.
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

fn ensure_session(shared: &Arc<Shared>, uri: &str) {
    {
        let pending = shared.pending_connect.lock().unwrap();
        if pending.values().any(|u| u == uri) {
            return;
        }
    }
    let n = shared.next_req.fetch_add(1, Ordering::SeqCst);
    let id = format!("infoview:c:{n}");
    shared
        .pending_connect
        .lock()
        .unwrap()
        .insert(id.clone(), uri.to_string());
    log(format_args!("-> rpc/connect {uri}"));
    send_to_server(
        shared,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "$/lean/rpc/connect",
            "params": { "uri": uri },
        }),
    );
}

fn rpc_call(
    shared: &Shared,
    c: &Cursor,
    session_id: &Value,
    id: String,
    method: &str,
    params: Value,
) {
    send_to_server(
        shared,
        &json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "$/lean/rpc/call",
            "params": {
                "method": method,
                "params": params,
                "sessionId": session_id,
                "textDocument": { "uri": c.uri },
                "position": { "line": c.line, "character": c.character },
            },
        }),
    );
}

/// Frees RpcPtr references we no longer need, so the server can drop them.
fn release_refs(shared: &Shared, uri: &str, refs: Vec<Value>) {
    if refs.is_empty() {
        return;
    }
    let Some(session_id) = shared.sessions.lock().unwrap().get(uri).cloned() else {
        return; // session gone: refs died with it
    };
    send_to_server(
        shared,
        &json!({
            "jsonrpc": "2.0",
            "method": "$/lean/rpc/release",
            "params": { "uri": uri, "sessionId": session_id, "refs": refs },
        }),
    );
}

/// Lazy-children pointers still stored in `segs` (they must stay alive).
fn kept_ptr_keys(segs: &[MsgSeg]) -> HashSet<String> {
    let mut kept = HashSet::new();
    convert::visit_traces(segs, &mut |node| {
        if let Some(ptr) = &node.lazy_ptr {
            kept.insert(ptr.to_string());
        }
    });
    kept
}

fn send_goal_requests(shared: &Arc<Shared>) {
    let Some(c) = shared.cursor.lock().unwrap().clone() else {
        return;
    };
    {
        let mut st = shared.state.lock().unwrap();
        st.line = c.line as u32;
        st.column = c.character as u32;
        if st.uri.as_deref() != Some(&c.uri) {
            st.uri = Some(c.uri.clone());
            // Until interactive diagnostics for this file arrive.
            st.diagnostics = shared
                .plain_diags
                .lock()
                .unwrap()
                .get(&c.uri)
                .cloned()
                .unwrap_or_default();
        }
    }
    let session = shared.sessions.lock().unwrap().get(&c.uri).cloned();
    let Some(session_id) = session else {
        ensure_session(shared, &c.uri);
        broadcast(shared);
        return;
    };
    let generation = shared.generation.fetch_add(1, Ordering::SeqCst) + 1;
    log(format_args!(
        "-> goals? gen={generation} {}:{}:{}",
        c.uri, c.line, c.character
    ));
    let tdpp = json!({
        "textDocument": { "uri": c.uri },
        "position": { "line": c.line, "character": c.character },
    });
    for (kind, method, params) in [
        ("g", "Lean.Widget.getInteractiveGoals", tdpp.clone()),
        ("t", "Lean.Widget.getInteractiveTermGoal", tdpp),
        ("d", "Lean.Widget.getInteractiveDiagnostics", json!({})),
    ] {
        let id = format!("infoview:{kind}:{generation}");
        shared
            .pending_goal
            .lock()
            .unwrap()
            .insert(id.clone(), c.uri.clone());
        rpc_call(shared, &c, &session_id, id, method, params);
    }
    broadcast(shared);
}

/// Consumes responses to our own requests. Returns true if the message was ours.
fn handle_our_response(shared: &Arc<Shared>, v: &Value) -> bool {
    let Some(id) = v["id"].as_str() else {
        return false;
    };
    if !id.starts_with("infoview:") {
        return false;
    }
    // A request *from* the server could in principle carry any id; only treat
    // id-only messages (responses) as ours.
    if v["method"].as_str().is_some() {
        return false;
    }
    let Some((kind, tail)) = id
        .strip_prefix("infoview:")
        .and_then(|rest| rest.split_once(':'))
    else {
        return true;
    };

    if !v["error"].is_null() {
        let code = v["error"]["code"].as_i64().unwrap_or(0);
        log(format_args!("<- error for {kind}:{tail}: {}", v["error"]));
        shared.pending_connect.lock().unwrap().remove(id);
        shared.pending_goal.lock().unwrap().remove(id);
        if let Some(req) = shared.pending_expand.lock().unwrap().remove(id) {
            release_refs(shared, &req.uri, vec![req.ptr]);
        }
        if code == RPC_NEEDS_RECONNECT
            && let Some(c) = shared.cursor.lock().unwrap().clone()
        {
            shared.sessions.lock().unwrap().remove(&c.uri);
            ensure_session(shared, &c.uri);
        }
        return true;
    }

    match kind {
        "c" => {
            let Some(uri) = shared.pending_connect.lock().unwrap().remove(id) else {
                return true;
            };
            let session_id = v["result"]["sessionId"].clone();
            if session_id.is_null() {
                log(format_args!(
                    "<- rpc/connect: no sessionId in {}",
                    v["result"]
                ));
                return true;
            }
            log(format_args!("<- rpc/connect ok for {uri}"));
            shared
                .sessions
                .lock()
                .unwrap()
                .insert(uri.clone(), session_id);
            let at_cursor = shared
                .cursor
                .lock()
                .unwrap()
                .as_ref()
                .is_some_and(|c| c.uri == uri);
            if at_cursor {
                send_goal_requests(shared);
            }
        }
        "g" | "t" | "d" => {
            let Some(uri) = shared.pending_goal.lock().unwrap().remove(id) else {
                return true;
            };
            let generation = tail.parse::<u64>().unwrap_or(0);
            let stale = generation != shared.generation.load(Ordering::SeqCst);
            let result = &v["result"];
            let mut refs = Vec::new();
            convert::collect_ptrs(result, &mut refs);
            if stale {
                release_refs(shared, &uri, refs);
                return true;
            }
            log(format_args!(
                "<- result for {kind} gen={generation}: {}",
                if result.is_null() { "null" } else { "ok" }
            ));
            match kind {
                "g" => {
                    let goals = (!result.is_null()).then(|| {
                        result["goals"]
                            .as_array()
                            .map(|gs| gs.iter().map(convert::conv_goal).collect())
                            .unwrap_or_default()
                    });
                    shared.state.lock().unwrap().goals = goals;
                    release_refs(shared, &uri, refs);
                }
                "t" => {
                    let term = (!result.is_null()).then(|| convert::conv_term_goal(result));
                    shared.state.lock().unwrap().term_goal = term;
                    release_refs(shared, &uri, refs);
                }
                "d" => {
                    let diags: Vec<Diag> = result
                        .as_array()
                        .map(|ds| {
                            ds.iter()
                                .enumerate()
                                .map(|(i, d)| convert::conv_interactive_diag(d, i))
                                .collect()
                        })
                        .unwrap_or_default();
                    // Keep the new lazy-children pointers alive; free the rest.
                    let kept: HashSet<String> =
                        diags.iter().flat_map(|d| kept_ptr_keys(&d.message)).collect();
                    refs.retain(|r| !kept.contains(&r.to_string()));
                    release_refs(shared, &uri, refs);
                    // Free the previous diagnostics' unfetched pointers.
                    let old_uri = shared.diag_uri.lock().unwrap().clone();
                    let mut old_ptrs = Vec::new();
                    {
                        let mut st = shared.state.lock().unwrap();
                        for d in &mut st.diagnostics {
                            convert::visit_traces_mut(&mut d.message, &mut |node| {
                                if let Some(ptr) = node.lazy_ptr.take() {
                                    old_ptrs.push(ptr);
                                }
                            });
                        }
                        st.diagnostics = diags;
                    }
                    if let Some(old_uri) = old_uri {
                        release_refs(shared, &old_uri, old_ptrs);
                    }
                    *shared.diag_uri.lock().unwrap() = Some(uri);
                    shared.diag_generation.store(generation, Ordering::SeqCst);
                }
                _ => unreachable!(),
            }
            broadcast(shared);
        }
        "x" => {
            let Some(req) = shared.pending_expand.lock().unwrap().remove(id) else {
                return true;
            };
            let result = &v["result"];
            let mut refs = vec![req.ptr];
            convert::collect_ptrs(result, &mut refs);
            let stale = req.generation != shared.diag_generation.load(Ordering::SeqCst);
            if stale {
                release_refs(shared, &req.uri, refs);
                return true;
            }
            let children = result
                .as_array()
                .map(|list| convert::conv_children(list, req.node_id as u64))
                .unwrap_or_default();
            let kept: HashSet<String> = children.iter().flat_map(|c| kept_ptr_keys(c)).collect();
            refs.retain(|r| !kept.contains(&r.to_string()));
            release_refs(shared, &req.uri, refs);
            {
                let mut st = shared.state.lock().unwrap();
                for d in &mut st.diagnostics {
                    if let Some(node) = convert::find_trace_mut(&mut d.message, req.node_id) {
                        node.children = children;
                        node.truncated = false;
                        break;
                    }
                }
            }
            broadcast(shared);
        }
        _ => {}
    }
    true
}

/// Handles an `{"expand": id}` command from a watcher: fetches the lazy
/// children of that trace node.
fn expand_trace(shared: &Arc<Shared>, node_id: u32) {
    let Some(uri) = shared.diag_uri.lock().unwrap().clone() else {
        return;
    };
    let Some(session_id) = shared.sessions.lock().unwrap().get(&uri).cloned() else {
        return;
    };
    let generation = shared.diag_generation.load(Ordering::SeqCst);
    let ptr = {
        let mut st = shared.state.lock().unwrap();
        st.diagnostics
            .iter_mut()
            .find_map(|d| convert::find_trace_mut(&mut d.message, node_id))
            .and_then(|node| node.lazy_ptr.take())
    };
    let Some(ptr) = ptr else {
        return; // unknown node, or fetch already in flight
    };
    // The rpc/call envelope needs a position in the session file; the exact
    // one does not matter for lazyTraceChildrenToInteractive.
    let cursor = shared.cursor.lock().unwrap().clone();
    let c = match cursor {
        Some(c) if c.uri == uri => c,
        _ => Cursor {
            uri: uri.clone(),
            line: 0,
            character: 0,
        },
    };
    let n = shared.next_req.fetch_add(1, Ordering::SeqCst);
    let id = format!("infoview:x:{n}");
    log(format_args!("-> expand trace node {node_id}"));
    shared.pending_expand.lock().unwrap().insert(
        id.clone(),
        ExpandReq {
            generation,
            node_id,
            uri,
            ptr: ptr.clone(),
        },
    );
    rpc_call(
        shared,
        &c,
        &session_id,
        id,
        "Lean.Widget.lazyTraceChildrenToInteractive",
        ptr,
    );
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
            let mut write_half = stream;
            let Ok(read_half) = write_half.try_clone() else {
                continue;
            };
            // Greet the new watcher with the current state.
            let line = current_state_line(&shared);
            if write_half.write_all(line.as_bytes()).is_err() {
                continue;
            }
            shared.clients.lock().unwrap().push(write_half);
            // Commands from this watcher.
            let shared = shared.clone();
            std::thread::spawn(move || {
                let reader = BufReader::new(read_half);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if let Ok(v) = serde_json::from_str::<Value>(&line)
                        && let Some(node_id) = v["expand"].as_u64()
                    {
                        expand_trace(&shared, node_id as u32);
                    }
                }
            });
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
