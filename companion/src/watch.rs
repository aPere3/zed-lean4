//! Terminal UI: connects to the proxy socket and renders the infoview live.
//!
//! Keys: q quit · j/k scroll · Tab/Shift-Tab focus trace nodes · Enter fold.

use crate::state::{Diag, Goal, InfoviewState, MsgSeg, TextSpan, socket_dir, socket_path_for_root};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Result, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, TryRecvError, channel};
use std::time::Duration;

pub fn run() -> Result<()> {
    let mut terminal = ratatui::init();
    let result = run_inner(&mut terminal);
    ratatui::restore();
    result
}

#[derive(Clone, Copy)]
struct Fold {
    id: u32,
    /// Effectively collapsed in the last render.
    collapsed: bool,
    /// Children not fetched yet: expanding must ask the proxy for them.
    needs_fetch: bool,
}

#[derive(Default)]
struct App {
    state: InfoviewState,
    scroll: u16,
    /// Fold state the user changed, by trace node id.
    fold_overrides: HashMap<u32, bool>,
    /// Focused trace node id (target of Enter).
    focus: Option<u32>,
    /// Trace nodes visible in the last render.
    folds: Vec<Fold>,
}

fn run_inner(terminal: &mut DefaultTerminal) -> Result<()> {
    loop {
        // Rendezvous: wait for the proxy socket to appear.
        let socket = loop {
            match find_socket() {
                Some(p) => break p,
                None => {
                    terminal.draw(draw_waiting)?;
                    if quit_requested(Duration::from_millis(500))? {
                        return Ok(());
                    }
                }
            }
        };
        let Ok(stream) = UnixStream::connect(&socket) else {
            // Stale socket file: remove it and retry.
            let _ = std::fs::remove_file(&socket);
            continue;
        };
        let Ok(mut writer) = stream.try_clone() else {
            continue;
        };

        let rx = spawn_reader(stream);
        let mut app = App::default();
        'connected: loop {
            while event::poll(Duration::ZERO)? {
                if let Event::Key(k) = event::read()?
                    && k.kind == KeyEventKind::Press
                {
                    match k.code {
                        KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                        KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                            return Ok(());
                        }
                        KeyCode::Up | KeyCode::Char('k') => {
                            app.scroll = app.scroll.saturating_sub(1)
                        }
                        KeyCode::Down | KeyCode::Char('j') => {
                            app.scroll = app.scroll.saturating_add(1)
                        }
                        KeyCode::PageUp => app.scroll = app.scroll.saturating_sub(10),
                        KeyCode::PageDown => app.scroll = app.scroll.saturating_add(10),
                        KeyCode::Home => app.scroll = 0,
                        KeyCode::Tab => move_focus(&mut app, 1),
                        KeyCode::BackTab => move_focus(&mut app, -1),
                        KeyCode::Enter | KeyCode::Char(' ') => {
                            if let Some(node_id) = toggle_focused(&mut app) {
                                // Ask the proxy to fetch the lazy children.
                                let _ = writeln!(
                                    writer,
                                    "{}",
                                    serde_json::json!({ "expand": node_id })
                                );
                            }
                        }
                        _ => {}
                    }
                }
            }
            loop {
                match rx.try_recv() {
                    Ok(s) => app.state = s,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'connected, // reconnect
                }
            }
            let rendered = build(&app);
            app.folds = rendered.folds.clone();
            terminal.draw(|f| draw_state(f, &rendered.lines, app.scroll))?;
            if quit_requested(Duration::from_millis(50))? {
                return Ok(());
            }
        }
    }
}

fn move_focus(app: &mut App, dir: i32) {
    if app.folds.is_empty() {
        app.focus = None;
        return;
    }
    let cur = app
        .focus
        .and_then(|id| app.folds.iter().position(|f| f.id == id));
    let next = match (cur, dir) {
        (None, _) => 0,
        (Some(i), 1) => (i + 1) % app.folds.len(),
        (Some(i), _) => (i + app.folds.len() - 1) % app.folds.len(),
    };
    app.focus = Some(app.folds[next].id);
}

/// Toggles the focused fold. Returns the node id to fetch from the proxy
/// when it is being expanded but its children are not loaded yet.
fn toggle_focused(app: &mut App) -> Option<u32> {
    let id = app.focus?;
    let fold = *app.folds.iter().find(|f| f.id == id)?;
    app.fold_overrides.insert(id, !fold.collapsed);
    (fold.collapsed && fold.needs_fetch).then_some(id)
}

fn quit_requested(timeout: Duration) -> Result<bool> {
    if event::poll(timeout)?
        && let Event::Key(k) = event::read()?
        && k.kind == KeyEventKind::Press
    {
        let ctrl_c = k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
        return Ok(matches!(k.code, KeyCode::Char('q') | KeyCode::Esc) || ctrl_c);
    }
    Ok(false)
}

fn spawn_reader(stream: UnixStream) -> Receiver<InfoviewState> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines() {
            let Ok(line) = line else { break };
            if let Ok(state) = serde_json::from_str::<InfoviewState>(&line)
                && tx.send(state).is_err()
            {
                break;
            }
        }
    });
    rx
}

/// The proxy names the socket after the workspace root; the watcher runs in
/// the same worktree, so the cwd usually matches. Fall back to the most
/// recently modified socket otherwise.
fn find_socket() -> Option<PathBuf> {
    if let Ok(cwd) = std::env::current_dir() {
        let cwd = cwd.canonicalize().unwrap_or(cwd);
        if let Some(s) = cwd.to_str() {
            let exact = socket_path_for_root(s);
            if exact.exists() {
                return Some(exact);
            }
        }
    }
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in std::fs::read_dir(socket_dir()).ok()?.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "sock")
            && let Ok(meta) = entry.metadata()
            && let Ok(modified) = meta.modified()
            && best.as_ref().is_none_or(|(t, _)| modified > *t)
        {
            best = Some((modified, path));
        }
    }
    best.map(|(_, p)| p)
}

fn draw_waiting(f: &mut Frame) {
    let text = vec![
        Line::from(Span::styled(
            "Lean 4 Infoview",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from("Waiting for the language server proxy…"),
        Line::from(Span::styled(
            "Open a .lean file in Zed to start it. Press q to quit.",
            Style::default().fg(Color::DarkGray),
        )),
    ];
    f.render_widget(Paragraph::new(text).wrap(Wrap { trim: false }), f.area());
}

fn draw_state(f: &mut Frame, lines: &[Line<'static>], scroll: u16) {
    let [main, footer] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)])
        .areas(f.area());
    f.render_widget(
        Paragraph::new(lines.to_vec())
            .scroll((scroll, 0))
            .wrap(Wrap { trim: false }),
        main,
    );
    f.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " q quit · j/k scroll · tab traces · ⏎ fold",
            Style::default().fg(Color::DarkGray),
        ))),
        footer,
    );
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

struct Rendered {
    lines: Vec<Line<'static>>,
    folds: Vec<Fold>,
}

struct Ctx<'a> {
    lines: Vec<Line<'static>>,
    folds: Vec<Fold>,
    overrides: &'a HashMap<u32, bool>,
    focus: Option<u32>,
}

fn build(app: &App) -> Rendered {
    let s = &app.state;
    let mut ctx = Ctx {
        lines: Vec::new(),
        folds: Vec::new(),
        overrides: &app.fold_overrides,
        focus: app.focus,
    };

    // Header: file:line:col + status.
    let file = s
        .uri
        .as_deref()
        .map(short_path)
        .unwrap_or_else(|| "—".to_string());
    let mut header = vec![
        Span::styled(
            format!("{file}:{}:{}", s.line + 1, s.column + 1),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
    ];
    if !s.server_alive {
        header.push(Span::styled(
            "server stopped",
            Style::default().fg(Color::Red),
        ));
    } else if s.processing {
        header.push(Span::styled(
            "⟳ elaborating…",
            Style::default().fg(Color::Yellow),
        ));
    } else {
        header.push(Span::styled("✓", Style::default().fg(Color::Green)));
    }
    ctx.lines.push(Line::from(header));
    ctx.lines.push(Line::from(""));

    // Tactic goals.
    match &s.goals {
        None => {}
        Some(goals) if goals.is_empty() => {
            ctx.lines.push(section("Tactic state"));
            ctx.lines.push(Line::from(Span::styled(
                "Goals accomplished 🎉",
                Style::default().fg(Color::Green),
            )));
            ctx.lines.push(Line::from(""));
        }
        Some(goals) => {
            ctx.lines.push(section(&format!(
                "Tactic state ({} goal{})",
                goals.len(),
                if goals.len() == 1 { "" } else { "s" }
            )));
            for (i, goal) in goals.iter().enumerate() {
                if i > 0 {
                    ctx.lines.push(Line::from(Span::styled(
                        "─".repeat(40),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                push_goal(&mut ctx.lines, 0, goal);
            }
            ctx.lines.push(Line::from(""));
        }
    }

    // Term goal.
    if let Some(term) = &s.term_goal {
        ctx.lines.push(section("Expected type"));
        push_goal(&mut ctx.lines, 0, term);
        ctx.lines.push(Line::from(""));
    }

    // Diagnostics.
    if !s.diagnostics.is_empty() {
        ctx.lines
            .push(section(&format!("Messages ({})", s.diagnostics.len())));
        for d in &s.diagnostics {
            push_diag_header(&mut ctx.lines, d);
            push_segs(&mut ctx, &d.message, 2);
        }
    }

    if s.goals.is_none() && s.term_goal.is_none() && s.diagnostics.is_empty() {
        ctx.lines.push(Line::from(Span::styled(
            "No info at cursor.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    Rendered {
        lines: ctx.lines,
        folds: ctx.folds,
    }
}

fn section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    ))
}

/// Styles goal-diff tags like the VSCode infoview does.
fn diff_style(diff: &Option<String>, base: Style) -> Style {
    match diff.as_deref() {
        Some("wasChanged" | "willChange") => base.fg(Color::Yellow),
        Some("wasInserted" | "willInsert" | "willInserted") => base.fg(Color::Green),
        Some("wasDeleted" | "willDelete" | "willDeleted") => {
            base.fg(Color::Red).add_modifier(Modifier::CROSSED_OUT)
        }
        _ => base,
    }
}

fn rich_parts(spans: &[TextSpan], base: Style) -> Vec<(String, Style)> {
    spans
        .iter()
        .map(|s| (s.text.clone(), diff_style(&s.diff, base)))
        .collect()
}

/// Appends styled parts as one or more lines, splitting on '\n' and applying
/// the indent to every produced line.
fn push_rich(lines: &mut Vec<Line<'static>>, indent: usize, parts: &[(String, Style)]) {
    let pad = " ".repeat(indent);
    let mut cur: Vec<Span<'static>> = vec![Span::raw(pad.clone())];
    for (text, style) in parts {
        let mut first = true;
        for piece in text.split('\n') {
            if !first {
                lines.push(Line::from(std::mem::take(&mut cur)));
                cur.push(Span::raw(pad.clone()));
            }
            if !piece.is_empty() {
                cur.push(Span::styled(piece.to_string(), *style));
            }
            first = false;
        }
    }
    lines.push(Line::from(cur));
}

fn push_goal(lines: &mut Vec<Line<'static>>, indent: usize, g: &Goal) {
    let base = if g.is_removed {
        Style::default()
            .fg(Color::Red)
            .add_modifier(Modifier::CROSSED_OUT)
    } else if g.is_inserted {
        Style::default().fg(Color::Green)
    } else {
        Style::default()
    };
    if let Some(name) = &g.name {
        push_rich(
            lines,
            indent,
            &[(format!("case {name}"), Style::default().fg(Color::Magenta))],
        );
    }
    for h in &g.hyps {
        let mut hbase = if h.is_removed {
            Style::default()
                .fg(Color::Red)
                .add_modifier(Modifier::CROSSED_OUT)
        } else if h.is_inserted {
            Style::default().fg(Color::Green)
        } else {
            base
        };
        if h.is_instance {
            hbase = hbase.add_modifier(Modifier::DIM);
        }
        let mut parts = vec![
            (h.names.join(" "), hbase),
            (" : ".to_string(), hbase.fg(Color::DarkGray)),
        ];
        parts.extend(rich_parts(&h.ty, hbase));
        if let Some(val) = &h.val {
            parts.push((" := ".to_string(), hbase.fg(Color::DarkGray)));
            parts.extend(rich_parts(val, hbase));
        }
        push_rich(lines, indent, &parts);
    }
    let mut parts = vec![(g.prefix.clone(), base.add_modifier(Modifier::BOLD))];
    parts.extend(rich_parts(&g.target, base));
    push_rich(lines, indent, &parts);
}

fn push_diag_header(lines: &mut Vec<Line<'static>>, d: &Diag) {
    let (label, color) = match d.severity {
        1 => ("error", Color::Red),
        2 => ("warning", Color::Yellow),
        4 => ("hint", Color::DarkGray),
        _ => ("info", Color::Blue),
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!("▸ {}:{} ", d.line + 1, d.column + 1),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            label,
            Style::default().fg(color).add_modifier(Modifier::BOLD),
        ),
    ]));
}

fn push_segs(ctx: &mut Ctx, segs: &[MsgSeg], indent: usize) {
    // Consecutive text segments render as one rich block; goals and traces
    // flush it and render as blocks of their own.
    let mut parts: Vec<(String, Style)> = Vec::new();
    let flush = |ctx: &mut Ctx, parts: &mut Vec<(String, Style)>| {
        if !parts.is_empty() {
            push_rich(&mut ctx.lines, indent, parts);
            parts.clear();
        }
    };
    for seg in segs {
        match seg {
            MsgSeg::Text(span) => {
                parts.push((span.text.clone(), diff_style(&span.diff, Style::default())));
            }
            MsgSeg::Goal(goal) => {
                flush(ctx, &mut parts);
                push_goal(&mut ctx.lines, indent, goal);
            }
            MsgSeg::Trace(node) => {
                flush(ctx, &mut parts);
                let collapsed = ctx
                    .overrides
                    .get(&node.id)
                    .copied()
                    .unwrap_or(node.collapsed);
                ctx.folds.push(Fold {
                    id: node.id,
                    collapsed,
                    needs_fetch: node.truncated,
                });
                let focused = ctx.focus == Some(node.id);
                let mut marker_style = Style::default().fg(Color::Cyan);
                if focused {
                    marker_style = marker_style.add_modifier(Modifier::REVERSED);
                }
                let mut header_parts = vec![
                    (
                        format!("{} ", if collapsed { "▶" } else { "▼" }),
                        marker_style,
                    ),
                    (
                        format!("[{}] ", node.cls),
                        if focused {
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::REVERSED)
                        } else {
                            Style::default().fg(Color::DarkGray)
                        },
                    ),
                ];
                for seg in &node.header {
                    match seg {
                        MsgSeg::Text(span) => header_parts
                            .push((span.text.clone(), diff_style(&span.diff, Style::default()))),
                        _ => header_parts.push(("…".to_string(), Style::default())),
                    }
                }
                push_rich(&mut ctx.lines, indent, &header_parts);
                if !collapsed {
                    for child in &node.children {
                        push_segs(ctx, child, indent + 2);
                    }
                    if node.truncated {
                        push_rich(
                            &mut ctx.lines,
                            indent + 2,
                            &[(
                                "… (loading)".to_string(),
                                Style::default().fg(Color::DarkGray),
                            )],
                        );
                    }
                }
            }
        }
    }
    flush(ctx, &mut parts);
}

/// `file:///a/b/c/d.lean` -> `c/d.lean`.
fn short_path(uri: &str) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    let parts: Vec<&str> = path.rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}
