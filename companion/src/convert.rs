//! Conversion from the Lean server's interactive (RPC) response JSON to the
//! socket-friendly `state` types.
//!
//! `TaggedText` trees are flattened to spans, keeping only the goal-diff
//! tags. `RpcPtr` handles are dropped (no subexpression inspection yet) and
//! released by the proxy, except lazy trace children, whose pointers the
//! proxy keeps for on-demand expansion.

use crate::state::{Diag, Goal, Hyp, MsgSeg, TextSpan, TraceNode, fnv1a};
use serde_json::Value;

fn mix(seed: u64, v: u64) -> u64 {
    (seed ^ v).wrapping_mul(0x100000001b3)
}

/// Flattens a `TaggedText<SubexprInfo>`; the innermost `diffStatus` wins.
pub fn flatten_code(tt: &Value, diff: Option<&str>, out: &mut Vec<TextSpan>) {
    if let Some(s) = tt["text"].as_str() {
        out.push(TextSpan {
            text: s.to_string(),
            diff: diff.map(String::from),
        });
    } else if let Some(parts) = tt["append"].as_array() {
        for p in parts {
            flatten_code(p, diff, out);
        }
    } else if let Some(pair) = tt["tag"].as_array()
        && pair.len() == 2
    {
        let d = pair[0]["diffStatus"].as_str().or(diff);
        flatten_code(&pair[1], d, out);
    }
}

fn code(tt: &Value) -> Vec<TextSpan> {
    let mut out = Vec::new();
    flatten_code(tt, None, &mut out);
    out
}

/// Converts an `InteractiveGoal`.
pub fn conv_goal(v: &Value) -> Goal {
    Goal {
        name: v["userName"].as_str().map(String::from),
        prefix: v["goalPrefix"].as_str().unwrap_or("⊢ ").to_string(),
        hyps: v["hyps"]
            .as_array()
            .map(|hs| hs.iter().map(conv_hyp).collect())
            .unwrap_or_default(),
        target: code(&v["type"]),
        is_inserted: v["isInserted"].as_bool().unwrap_or(false),
        is_removed: v["isRemoved"].as_bool().unwrap_or(false),
    }
}

/// Converts an `InteractiveTermGoal` (same core, no name/prefix).
pub fn conv_term_goal(v: &Value) -> Goal {
    let mut g = conv_goal(v);
    g.name = None;
    g
}

fn conv_hyp(v: &Value) -> Hyp {
    Hyp {
        names: v["names"]
            .as_array()
            .map(|ns| {
                ns.iter()
                    .filter_map(|n| n.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
        ty: code(&v["type"]),
        val: (!v["val"].is_null()).then(|| code(&v["val"])),
        is_instance: v["isInstance"].as_bool().unwrap_or(false),
        is_type: v["isType"].as_bool().unwrap_or(false),
        is_inserted: v["isInserted"].as_bool().unwrap_or(false),
        is_removed: v["isRemoved"].as_bool().unwrap_or(false),
    }
}

/// Converts a `TaggedText<MsgEmbed>` (diagnostic message / trace body).
pub fn conv_msg(tt: &Value, diff: Option<&str>, seed: u64, out: &mut Vec<MsgSeg>) {
    if let Some(s) = tt["text"].as_str() {
        out.push(MsgSeg::Text(TextSpan {
            text: s.to_string(),
            diff: diff.map(String::from),
        }));
    } else if let Some(parts) = tt["append"].as_array() {
        for (i, p) in parts.iter().enumerate() {
            conv_msg(p, diff, mix(seed, i as u64), out);
        }
    } else if let Some(pair) = tt["tag"].as_array()
        && pair.len() == 2
    {
        let embed = &pair[0];
        let alt = &pair[1];
        if !embed["expr"].is_null() {
            let mut spans = Vec::new();
            flatten_code(&embed["expr"], diff, &mut spans);
            out.extend(spans.into_iter().map(MsgSeg::Text));
        } else if !embed["goal"].is_null() {
            out.push(MsgSeg::Goal(conv_goal(&embed["goal"])));
        } else if !embed["trace"].is_null() {
            out.push(MsgSeg::Trace(conv_trace(&embed["trace"], seed)));
        } else if !embed["widget"].is_null() {
            // User widgets cannot render in a TUI: use the alt text.
            let w_alt = &embed["widget"]["alt"];
            let src = if w_alt.is_null() { alt } else { w_alt };
            conv_msg(src, diff, mix(seed, 1), out);
        } else {
            conv_msg(alt, diff, mix(seed, 1), out);
        }
    }
}

fn conv_trace(tr: &Value, seed: u64) -> TraceNode {
    let cls = tr["cls"].as_str().unwrap_or("trace").to_string();
    let id = mix(seed, fnv1a(cls.as_bytes())) as u32;
    let mut header = Vec::new();
    conv_msg(&tr["msg"], None, mix(id as u64, 1), &mut header);
    let mut node = TraceNode {
        id,
        cls,
        header,
        children: Vec::new(),
        collapsed: tr["collapsed"].as_bool().unwrap_or(true),
        truncated: false,
        lazy_ptr: None,
    };
    if let Some(strict) = tr["children"]["strict"].as_array() {
        node.children = conv_children(strict, id as u64);
    } else if !tr["children"]["lazy"].is_null() {
        node.lazy_ptr = Some(tr["children"]["lazy"].clone());
        node.truncated = true;
    }
    node
}

/// Converts a list of `TaggedText<MsgEmbed>` children of a trace node.
pub fn conv_children(list: &[Value], seed: u64) -> Vec<Vec<MsgSeg>> {
    list.iter()
        .enumerate()
        .map(|(i, c)| {
            let mut out = Vec::new();
            conv_msg(c, None, mix(seed, i as u64 + 2), &mut out);
            out
        })
        .collect()
}

/// Converts an `InteractiveDiagnostic`.
pub fn conv_interactive_diag(v: &Value, index: usize) -> Diag {
    let line = v["range"]["start"]["line"].as_u64().unwrap_or(0);
    let column = v["range"]["start"]["character"].as_u64().unwrap_or(0);
    // Seed from position, not list index, so trace ids survive list shifts.
    let seed = mix(mix(fnv1a(b"diag"), line), index as u64);
    let mut message = Vec::new();
    conv_msg(&v["message"], None, seed, &mut message);
    Diag {
        line: line as u32,
        column: column as u32,
        severity: v["severity"].as_u64().unwrap_or(1) as u8,
        message,
    }
}

/// Visits every trace node (pre-order), including nested ones.
pub fn visit_traces(segs: &[MsgSeg], f: &mut impl FnMut(&TraceNode)) {
    for seg in segs {
        if let MsgSeg::Trace(node) = seg {
            f(node);
            visit_traces(&node.header, f);
            for child in &node.children {
                visit_traces(child, f);
            }
        }
    }
}

/// Visits every trace node (pre-order), including nested ones, mutably.
pub fn visit_traces_mut(segs: &mut [MsgSeg], f: &mut impl FnMut(&mut TraceNode)) {
    for seg in segs {
        if let MsgSeg::Trace(node) = seg {
            f(node);
            visit_traces_mut(&mut node.header, f);
            for child in &mut node.children {
                visit_traces_mut(child, f);
            }
        }
    }
}

/// Collects every RpcPtr (single-key `{"p": …}` object) in a raw response.
pub fn collect_ptrs(v: &Value, out: &mut Vec<Value>) {
    match v {
        Value::Object(map) => {
            if map.len() == 1 && map.contains_key("p") {
                out.push(v.clone());
            } else {
                for x in map.values() {
                    collect_ptrs(x, out);
                }
            }
        }
        Value::Array(items) => {
            for x in items {
                collect_ptrs(x, out);
            }
        }
        _ => {}
    }
}

pub fn find_trace_mut(segs: &mut [MsgSeg], id: u32) -> Option<&mut TraceNode> {
    for seg in segs {
        if let MsgSeg::Trace(node) = seg {
            if node.id == id {
                return Some(node);
            }
            if let Some(found) = find_trace_mut(&mut node.header, id) {
                return Some(found);
            }
            for child in &mut node.children {
                if let Some(found) = find_trace_mut(child, id) {
                    return Some(found);
                }
            }
        }
    }
    None
}
