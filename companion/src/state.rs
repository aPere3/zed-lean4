//! Infoview state shared between the proxy and the watcher, and the
//! unix-socket rendezvous logic.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct InfoviewState {
    /// File the cursor is in (LSP uri).
    pub uri: Option<String>,
    /// Cursor position, 0-based (LSP convention).
    pub line: u32,
    pub column: u32,
    /// `None`: cursor is not in a tactic proof. `Some([])`: goals accomplished.
    pub goals: Option<Vec<Goal>>,
    /// Expected type at the cursor, if any.
    pub term_goal: Option<Goal>,
    /// Diagnostics for the cursor file.
    pub diagnostics: Vec<Diag>,
    /// The server is still elaborating the cursor file.
    pub processing: bool,
    /// The real language server is running.
    pub server_alive: bool,
}

/// One span of pretty-printed code. `diff` is a Lean goal-diff tag:
/// `wasChanged`, `willChange`, `wasInserted`, `willInsert`, `wasDeleted`,
/// `willDelete`.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TextSpan {
    pub text: String,
    pub diff: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Goal {
    /// `case` name, if any.
    pub name: Option<String>,
    /// Usually `"⊢ "`.
    pub prefix: String,
    pub hyps: Vec<Hyp>,
    pub target: Vec<TextSpan>,
    pub is_inserted: bool,
    pub is_removed: bool,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Hyp {
    pub names: Vec<String>,
    pub ty: Vec<TextSpan>,
    /// `let`-value, if any.
    pub val: Option<Vec<TextSpan>>,
    pub is_instance: bool,
    pub is_type: bool,
    pub is_inserted: bool,
    pub is_removed: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diag {
    pub line: u32,
    pub column: u32,
    /// LSP severity: 1 error, 2 warning, 3 info, 4 hint.
    pub severity: u8,
    pub message: Vec<MsgSeg>,
}

/// A segment of an interactive diagnostic message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MsgSeg {
    Text(TextSpan),
    /// A goal embedded in the message (e.g. "unsolved goals").
    Goal(Goal),
    /// A collapsible trace node.
    Trace(TraceNode),
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct TraceNode {
    /// Stable-ish id, used by the watcher to keep fold state across updates.
    pub id: u32,
    /// Trace class, e.g. `Meta.synthInstance`.
    pub cls: String,
    pub header: Vec<MsgSeg>,
    pub children: Vec<Vec<MsgSeg>>,
    /// Collapsed by default (server hint).
    pub collapsed: bool,
    /// Children exist on the server but are not fetched yet. The watcher
    /// requests them by sending `{"expand": id}` back on the socket.
    pub truncated: bool,
    /// Proxy-internal: RpcPtr to fetch the children. Never serialized.
    #[serde(skip)]
    pub lazy_ptr: Option<serde_json::Value>,
}

pub fn socket_dir() -> PathBuf {
    std::env::temp_dir().join("zed-lean4-companion")
}

/// Socket path derived from the workspace root path, so a watcher started in
/// the same worktree finds the proxy without configuration.
pub fn socket_path_for_root(root: &str) -> PathBuf {
    socket_dir().join(format!("{:016x}.sock", fnv1a(root.as_bytes())))
}

pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}
