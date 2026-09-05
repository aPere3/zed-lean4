//! Terminal UI: connects to the proxy socket and renders the infoview live.

use crate::state::{InfoviewState, socket_dir, socket_path_for_root};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use std::io::{BufRead, BufReader, Result};
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

        let rx = spawn_reader(stream);
        let mut state = InfoviewState::default();
        let mut scroll: u16 = 0;
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
                        KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                        KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                        KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                        KeyCode::PageDown => scroll = scroll.saturating_add(10),
                        KeyCode::Home => scroll = 0,
                        _ => {}
                    }
                }
            }
            loop {
                match rx.try_recv() {
                    Ok(s) => state = s,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => break 'connected, // reconnect
                }
            }
            terminal.draw(|f| draw_state(f, &state, scroll))?;
            if quit_requested(Duration::from_millis(50))? {
                return Ok(());
            }
        }
    }
}

fn quit_requested(timeout: Duration) -> Result<bool> {
    if event::poll(timeout)?
        && let Event::Key(k) = event::read()?
        && k.kind == KeyEventKind::Press
    {
        let ctrl_c =
            k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL);
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

fn draw_state(f: &mut Frame, s: &InfoviewState, scroll: u16) {
    let mut lines: Vec<Line> = Vec::new();

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
    lines.push(Line::from(header));
    lines.push(Line::from(""));

    // Tactic goals.
    match &s.goals {
        None => {}
        Some(goals) if goals.is_empty() => {
            lines.push(section("Tactic state"));
            lines.push(Line::from(Span::styled(
                "Goals accomplished 🎉",
                Style::default().fg(Color::Green),
            )));
            lines.push(Line::from(""));
        }
        Some(goals) => {
            lines.push(section(&format!(
                "Tactic state ({} goal{})",
                goals.len(),
                if goals.len() == 1 { "" } else { "s" }
            )));
            for (i, goal) in goals.iter().enumerate() {
                if i > 0 {
                    lines.push(Line::from(Span::styled(
                        "─".repeat(40),
                        Style::default().fg(Color::DarkGray),
                    )));
                }
                for l in goal.lines() {
                    lines.push(goal_line(l));
                }
            }
            lines.push(Line::from(""));
        }
    }

    // Term goal.
    if let Some(term) = &s.term_goal {
        lines.push(section("Expected type"));
        for l in term.lines() {
            lines.push(goal_line(l));
        }
        lines.push(Line::from(""));
    }

    // Diagnostics.
    if !s.diagnostics.is_empty() {
        lines.push(section(&format!("Messages ({})", s.diagnostics.len())));
        for d in &s.diagnostics {
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
                Span::styled(label, Style::default().fg(color).add_modifier(Modifier::BOLD)),
            ]));
            for l in d.message.lines() {
                lines.push(Line::from(format!("  {l}")));
            }
        }
    }

    if s.goals.is_none() && s.term_goal.is_none() && s.diagnostics.is_empty() {
        lines.push(Line::from(Span::styled(
            "No info at cursor.",
            Style::default().fg(Color::DarkGray),
        )));
    }

    f.render_widget(
        Paragraph::new(lines).scroll((scroll, 0)).wrap(Wrap { trim: false }),
        f.area(),
    );
}

fn section(title: &str) -> Line<'static> {
    Line::from(Span::styled(
        title.to_string(),
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD | Modifier::UNDERLINED),
    ))
}

fn goal_line(l: &str) -> Line<'static> {
    let trimmed = l.trim_start();
    let style = if trimmed.starts_with('⊢') {
        Style::default().add_modifier(Modifier::BOLD)
    } else if trimmed.starts_with("case ") {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default()
    };
    Line::from(Span::styled(l.to_string(), style))
}

/// `file:///a/b/c/d.lean` -> `c/d.lean`.
fn short_path(uri: &str) -> String {
    let path = uri.strip_prefix("file://").unwrap_or(uri);
    let parts: Vec<&str> = path.rsplit('/').take(2).collect();
    parts.into_iter().rev().collect::<Vec<_>>().join("/")
}
