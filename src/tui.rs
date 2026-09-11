use crate::{
    Args,
    prune::{Entry, Kind, Repo, bytes, human},
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Cell, Clear, Padding, Paragraph, Row, Table, TableState, Wrap},
};
use std::{collections::BTreeSet, path::PathBuf};

pub struct Selection {
    pub paths: Vec<PathBuf>,
    pub force: bool,
}

pub fn select(repo: &Repo, entries: &[Entry], args: &Args) -> Result<Option<Selection>> {
    eprintln!("Reading worktree status and target sizes…");
    let target_sizes: Vec<_> = entries.iter().map(|e| bytes(&e.target)).collect();
    let cells: Vec<_> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            [
                e.name().escape_default().to_string(),
                e.branch.as_deref().unwrap_or("detached").to_owned(),
                format!(
                    "{}{}",
                    repo.state(e).unwrap_or_else(|_| "unknown".into()),
                    if e.locked { " / locked" } else { "" }
                ),
                match repo.dirty(e) {
                    Ok(true) => "dirty",
                    Ok(false) => "clean",
                    Err(_) => "unknown",
                }
                .to_owned(),
                human(target_sizes[i]),
            ]
        })
        .collect();
    let mut terminal = ratatui::init();
    let result = (|| -> Result<_> {
        let mut cursor =
            TableState::default().with_selected(if entries.is_empty() { None } else { Some(0) });
        let mut selected = BTreeSet::new();
        for (i, e) in entries.iter().enumerate() {
            if e.kind != Kind::Main
                && !e.locked
                && ((args.stale && e.kind == Kind::Stale)
                    || (args.orphans && e.kind == Kind::Orphan)
                    || args.worktrees.iter().any(|p| {
                        p == &e.path || p.as_os_str() == e.path.file_name().unwrap_or_default()
                    }))
            {
                selected.insert(i);
            }
        }
        let mut preview: Option<(String, bool)> = None;
        let mut modal: Option<(String, bool)> = None;
        let mut force = false;
        let mut scroll = 0u16;
        loop {
            terminal.draw(|f| {
                let areas = Layout::vertical([Constraint::Length(3), Constraint::Min(3), Constraint::Length(5), Constraint::Length(3)]).split(f.area());
                f.render_widget(Paragraph::new(format!("worktree-prune  |  {}  |  {} selected{}", if args.yes { "APPLY" } else { "DRY RUN" }, selected.len(), if force { "  |  FORCE: local files may be lost" } else { "" })).block(Block::bordered()), areas[0]);
                if let Some((text, _)) = &preview {
                    f.render_widget(Paragraph::new(text.as_str()).wrap(Wrap { trim: false }).scroll((scroll, 0)).block(Block::bordered().title("Removal plan")), areas[1]);
                } else {
                    let rows = entries.iter().enumerate().map(|(i, e)| {
                        let marker = if e.kind == Kind::Main || e.locked { "[-]" } else if selected.contains(&i) { "[x]" } else { "[ ]" };
                        let data = &cells[i];
                        Row::new(vec![
                            Cell::from(marker),
                            Cell::from(data[0].as_str()),
                            Cell::from(data[1].as_str()),
                            Cell::from(data[2].as_str()).style(Style::default().fg(state_color(&data[2]))),
                            Cell::from(data[3].as_str()).style(Style::default().fg(match data[3].as_str() {
                                "clean" => Color::Green, "dirty" => Color::Yellow, _ => Color::Red,
                            })),
                            Cell::from(Line::from(data[4].as_str()).right_aligned())
                                .style(Style::default().fg(target_color(target_sizes[i]))),
                        ]).style(if e.kind == Kind::Main || e.locked {
                            Style::default().fg(Color::DarkGray)
                        } else { Style::default() })
                    });
                    let table = Table::new(rows, [
                        Constraint::Length(3),
                        Constraint::Fill(2),
                        Constraint::Fill(4),
                        Constraint::Length(19),
                        Constraint::Length(7),
                        Constraint::Length(11),
                    ])
                    .header(Row::new(vec![
                        Cell::from(""), Cell::from("WORKTREE"), Cell::from("BRANCH"),
                        Cell::from("STATE"), Cell::from("CHANGES"),
                        Cell::from(Line::from("TARGET").right_aligned()),
                    ]).style(Style::default().fg(Color::Cyan).add_modifier(Modifier::BOLD)).bottom_margin(1))
                    .column_spacing(2)
                    .block(Block::bordered().title("Worktrees"))
                    .row_highlight_style(Style::default().bg(Color::DarkGray).add_modifier(Modifier::BOLD))
                    .highlight_symbol("› ");
                    f.render_stateful_widget(table, areas[1], &mut cursor);
                }
                let detail = cursor.selected().and_then(|i| entries.get(i)).map(|e| format!("Path: {}\nTarget: {}\n{}", e.path.display(), e.target.display(), if args.keep_target { "Cargo target will be kept." } else { "Shared and protected targets will be kept." })).unwrap_or_else(|| "No worktrees found.".into());
                f.render_widget(Paragraph::new(detail).wrap(Wrap { trim: false }).block(Block::bordered()), areas[2]);
                let help = match &preview {
                    Some((_, true)) => "y: confirm deletion  ·  Esc: back  ·  ↑↓: scroll  ·  q: cancel",
                    Some(_) => "Esc: back  ·  ↑↓: scroll  ·  q: close (no deletion)",
                    None => "↑↓ / j k: move  ·  Space: select  ·  a: toggle all  ·  Enter: preview  ·  q: quit",
                };
                f.render_widget(Paragraph::new(help).wrap(Wrap { trim: false }).block(Block::bordered()), areas[3]);
                if let Some((message, can_force)) = &modal {
                    render_force_modal(f, message, *can_force, selected.len(), scroll);
                }
            })?;
            let Event::Key(key) = event::read()? else {
                continue;
            };
            if key.kind != KeyEventKind::Press {
                continue;
            }
            if key.code == KeyCode::Char('q')
                || (key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
            {
                return Ok(None);
            }
            if let Some((_, can_force)) = &modal {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => {
                        modal = None;
                        scroll = 0;
                    }
                    KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    KeyCode::Char('y') if *can_force => {
                        let mut check = args.clone();
                        check.worktrees =
                            selected.iter().map(|i| entries[*i].path.clone()).collect();
                        check.stale = false;
                        check.orphans = false;
                        check.force = true;
                        let plan = repo.plan(entries, &check)?;
                        force = true;
                        preview = Some((
                            plan.describe()
                                + "\n\nFORCE: local changes and untracked files will be discarded.\nPress y to permanently remove these items.",
                            plan.errors.is_empty(),
                        ));
                        modal = None;
                        scroll = 0;
                    }
                    _ => (),
                }
                continue;
            }
            if let Some((_, valid)) = &preview {
                match key.code {
                    KeyCode::Esc => {
                        preview = None;
                        force = false;
                        scroll = 0;
                    }
                    KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    KeyCode::Char('y') if *valid => {
                        return Ok(Some(Selection {
                            paths: selected.iter().map(|i| entries[*i].path.clone()).collect(),
                            force,
                        }));
                    }
                    _ => (),
                }
                continue;
            }
            match key.code {
                KeyCode::Esc => return Ok(None),
                KeyCode::Down | KeyCode::Char('j') if !entries.is_empty() => {
                    cursor.select(Some((cursor.selected().unwrap_or(0) + 1) % entries.len()))
                }
                KeyCode::Up | KeyCode::Char('k') if !entries.is_empty() => cursor.select(Some(
                    (cursor.selected().unwrap_or(0) + entries.len() - 1) % entries.len(),
                )),
                KeyCode::Char(' ') => {
                    if let Some(i) = cursor.selected()
                        && entries[i].kind != Kind::Main
                        && !entries[i].locked
                        && !selected.remove(&i)
                    {
                        selected.insert(i);
                    }
                }
                KeyCode::Char('a') => {
                    let available: BTreeSet<_> = entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| e.kind != Kind::Main && !e.locked)
                        .map(|(i, _)| i)
                        .collect();
                    selected = if selected == available {
                        BTreeSet::new()
                    } else {
                        available
                    };
                }
                KeyCode::Enter if !selected.is_empty() => {
                    let mut check = args.clone();
                    check.worktrees = selected.iter().map(|i| entries[*i].path.clone()).collect();
                    check.stale = false;
                    check.orphans = false;
                    let plan = repo.plan(entries, &check)?;
                    let suffix = if !plan.errors.is_empty() {
                        "\n\nBlocked: nothing will be removed."
                    } else {
                        "\n\nPress y to permanently remove these items."
                    };
                    preview = Some((plan.describe() + suffix, plan.errors.is_empty()));
                    if !plan.errors.is_empty() {
                        check.force = true;
                        let forced = repo.plan(entries, &check)?;
                        let can_force = forced.errors.is_empty();
                        let message = if can_force {
                            plan.errors.join("\n\n")
                        } else {
                            forced.errors.join("\n\n")
                        };
                        modal = Some((message, can_force));
                        scroll = 0;
                    }
                }
                _ => (),
            }
        }
    })();
    ratatui::restore();
    result
}

fn state_color(state: &str) -> Color {
    if state.ends_with(" / locked") {
        return Color::Yellow;
    }
    match state {
        "merged" | "pushed" => Color::Green,
        "main" => Color::Cyan,
        "stale" | "orphan" => Color::Yellow,
        _ => Color::Red,
    }
}

fn target_color(bytes: u64) -> Color {
    // Match the GiB units used by human(), without rounding at the thresholds.
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes > 100 * GIB {
        Color::Red
    } else if bytes >= 10 * GIB {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn render_force_modal(
    frame: &mut ratatui::Frame,
    message: &str,
    can_force: bool,
    selected: usize,
    scroll: u16,
) {
    let screen = frame.area();
    let background = Color::Rgb(24, 28, 39);
    let panel = Color::Rgb(32, 38, 51);
    let foreground = Color::Rgb(230, 234, 242);
    let muted = Color::Rgb(154, 165, 184);
    let accent = if can_force {
        Color::Rgb(245, 190, 85)
    } else {
        Color::Rgb(245, 119, 128)
    };
    frame.buffer_mut().set_style(
        screen,
        Style::default()
            .fg(Color::Rgb(83, 91, 108))
            .bg(Color::Rgb(15, 18, 26)),
    );
    let width = screen.width.saturating_sub(4).min(96);
    let height = screen.height.saturating_sub(2).min(28);
    let area = Rect::new(
        screen.x + (screen.width - width) / 2,
        screen.y + (screen.height - height) / 2,
        width,
        height,
    );
    let shadow = Rect::new(area.x + 1, area.y + 1, width, height).intersection(screen);
    frame.render_widget(Clear, shadow);
    frame.render_widget(
        Block::default().style(Style::default().bg(Color::Black)),
        shadow,
    );
    frame.render_widget(Clear, area);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(background).fg(foreground))
        .padding(Padding::horizontal(if width >= 60 { 2 } else { 1 }));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let compact = inner.height < 20;
    let parts = Layout::vertical([
        Constraint::Length(if compact { 2 } else { 3 }),
        Constraint::Min(2),
        Constraint::Length(if compact { 4 } else { 5 }),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(inner);
    let title = if can_force {
        "Blocked — confirm FORCE"
    } else {
        "Blocked — cannot force"
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                format!("!  {title}"),
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                format!("{selected} selected  /  Nothing has been removed"),
                Style::default().fg(muted),
            )),
        ]),
        parts[0],
    );
    frame.render_widget(
        Paragraph::new(message)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(" Reasons for blocking ")
                    .border_style(Style::default().fg(Color::Rgb(67, 78, 98)))
                    .style(Style::default().bg(panel).fg(foreground))
                    .padding(Padding::horizontal(1)),
            ),
        parts[1],
    );
    let warning = if can_force {
        "Local changes and untracked files will be lost. Local-only commits may remain only in the local branch. You will review the forced plan before deletion."
    } else {
        "Force cannot bypass these protections. Resolve the reasons above, then try again."
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                if can_force {
                    "Before you continue"
                } else {
                    "This selection is protected"
                },
                Style::default().fg(accent).add_modifier(Modifier::BOLD),
            )),
            Line::from(warning),
        ])
        .wrap(Wrap { trim: false }),
        parts[2],
    );
    let mut buttons = vec![Span::styled(
        "  n / Esc  Cancel  ",
        Style::default()
            .fg(foreground)
            .bg(Color::Rgb(57, 66, 83))
            .add_modifier(Modifier::BOLD),
    )];
    if can_force {
        buttons.push(Span::raw("   "));
        buttons.push(Span::styled(
            "  y  Enable force  ",
            Style::default()
                .fg(background)
                .bg(accent)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(
        Paragraph::new(vec![Line::from(""), Line::from(buttons).right_aligned()]),
        parts[3],
    );
    frame.render_widget(
        Paragraph::new("↑ / ↓  Scroll reasons").style(Style::default().fg(muted)),
        parts[4],
    );
}
