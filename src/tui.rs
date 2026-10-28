use crate::{
    Args,
    prune::{Entry, Kind, Plan, Workspace, bytes, human},
};
use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::{
    layout::{Constraint, Layout, Margin, Rect},
    style::{Color, Modifier, Style},
    symbols,
    text::{Line, Span},
    widgets::{
        Block, BorderType, Cell, Clear, HighlightSpacing, LineGauge, List, ListItem, Padding,
        Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState, Table, TableState, Wrap,
    },
};
use rayon::prelude::*;
use std::{collections::BTreeSet, path::PathBuf};

pub struct Selection {
    pub paths: Vec<PathBuf>,
    pub targets: Vec<PathBuf>,
    pub clean_only: bool,
    pub force: bool,
    pub sudo: bool,
}

enum ModalKind {
    ConfirmPlan,
    ConfirmForce { needs_force: bool, needs_sudo: bool },
    HardBlocked,
}

struct Modal {
    clean_only: bool,
    kind: ModalKind,
    reasons: Vec<String>,
    plan_lines: Vec<Line<'static>>,
    freed_bytes: u64,
}

pub fn select(repo: &Workspace, entries: &[Entry], args: &Args) -> Result<Option<Selection>> {
    let mut mode_args = args.clone();
    let args = &mut mode_args;
    eprintln!("Reading worktree status and target sizes…");
    let target_sizes: Vec<_> = entries.par_iter().map(|e| bytes(&e.target)).collect();
    let total_target_bytes: u64 = target_sizes.iter().sum();
    let cells: Vec<_> = entries
        .par_iter()
        .enumerate()
        .map(|(i, e)| {
            [
                repo.label(e).escape_default().to_string(),
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
        let mut modal: Option<Modal> = None;
        let mut scroll = 0u16;
        loop {
            let selectable_count = entries
                .iter()
                .filter(|e| selectable(e, args.clean_only))
                .count();
            let selected_bytes: u64 = selected.iter().map(|&i| target_sizes[i]).sum();
            terminal.draw(|f| {
                let areas = Layout::vertical([
                    Constraint::Length(4),
                    Constraint::Min(5),
                    Constraint::Length(6),
                    Constraint::Length(3),
                ])
                .split(f.area());

                render_header(
                    f,
                    areas[0],
                    args,
                    selected.len(),
                    selectable_count,
                    selected_bytes,
                    total_target_bytes,
                );

                render_worktree_table(
                    f,
                    areas[1],
                    entries,
                    &cells,
                    &target_sizes,
                    &selected,
                    &mut cursor,
                    args.clean_only,
                );

                render_inspector(
                    f,
                    areas[2],
                    entries,
                    &target_sizes,
                    &selected,
                    selected_bytes,
                    cursor.selected(),
                    args,
                );

                render_footer(f, areas[3]);

                if let Some(m) = &modal {
                    render_modal(f, m, selected.len(), scroll);
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
            if let Some(m) = &modal {
                match key.code {
                    KeyCode::Esc | KeyCode::Char('n') => {
                        modal = None;
                        scroll = 0;
                    }
                    KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    KeyCode::Char('y') => match m.kind {
                        ModalKind::ConfirmPlan => {
                            return Ok(Some(Selection {
                                paths: selected.iter().map(|i| entries[*i].path.clone()).collect(),
                                targets: selected
                                    .iter()
                                    .map(|i| entries[*i].target.clone())
                                    .collect(),
                                clean_only: m.clean_only,
                                force: false,
                                sudo: false,
                            }));
                        }
                        ModalKind::ConfirmForce {
                            needs_force,
                            needs_sudo,
                        } => {
                            return Ok(Some(Selection {
                                paths: selected.iter().map(|i| entries[*i].path.clone()).collect(),
                                targets: selected
                                    .iter()
                                    .map(|i| entries[*i].target.clone())
                                    .collect(),
                                clean_only: m.clean_only,
                                force: needs_force,
                                sudo: needs_sudo,
                            }));
                        }
                        ModalKind::HardBlocked => (),
                    },
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
                KeyCode::Char('c') => {
                    args.clean_only = !args.clean_only;
                    selected.clear();
                }
                KeyCode::Char(' ') => {
                    if let Some(i) = cursor.selected()
                        && selectable(&entries[i], args.clean_only)
                        && !selected.remove(&i)
                    {
                        selected.insert(i);
                    }
                }
                KeyCode::Char('a') => {
                    let available: BTreeSet<_> = entries
                        .iter()
                        .enumerate()
                        .filter(|(_, e)| selectable(e, args.clean_only))
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
                    check.force = false;
                    check.sudo = false;
                    let base_plan = repo.plan(entries, &check)?;
                    let freed_bytes = selected
                        .iter()
                        .filter(|&&i| {
                            entries[i].kind == Kind::Orphan
                                || (!args.keep_target && entries[i].target.is_dir())
                        })
                        .map(|&i| target_sizes[i])
                        .sum();

                    if base_plan.errors.is_empty() {
                        modal = Some(Modal {
                            clean_only: args.clean_only,
                            kind: ModalKind::ConfirmPlan,
                            reasons: Vec::new(),
                            plan_lines: format_plan_lines(&base_plan),
                            freed_bytes,
                        });
                    } else {
                        let mut force_args = check.clone();
                        force_args.force = true;
                        let force_plan = repo.plan(entries, &force_args)?;

                        let mut sudo_args = check.clone();
                        sudo_args.sudo = true;
                        let sudo_plan = repo.plan(entries, &sudo_args)?;

                        let mut both_args = check;
                        both_args.force = true;
                        both_args.sudo = true;
                        let both_plan = repo.plan(entries, &both_args)?;

                        if both_plan.errors.is_empty() {
                            let needs_sudo = !force_plan.errors.is_empty();
                            let needs_force =
                                !args.clean_only && (!sudo_plan.errors.is_empty() || needs_sudo);
                            let mut reasons = base_plan.errors;
                            for err in force_plan.errors {
                                if !reasons.contains(&err) {
                                    reasons.push(err);
                                }
                            }
                            modal = Some(Modal {
                                clean_only: args.clean_only,
                                kind: ModalKind::ConfirmForce {
                                    needs_force,
                                    needs_sudo,
                                },
                                reasons,
                                plan_lines: format_plan_lines(&both_plan),
                                freed_bytes,
                            });
                        } else {
                            modal = Some(Modal {
                                clean_only: args.clean_only,
                                kind: ModalKind::HardBlocked,
                                reasons: both_plan.errors.clone(),
                                plan_lines: format_plan_lines(&both_plan),
                                freed_bytes: 0,
                            });
                        }
                    }
                    scroll = 0;
                }
                _ => (),
            }
        }
    })();
    ratatui::restore();
    result
}

fn selectable(entry: &Entry, clean_only: bool) -> bool {
    if clean_only {
        matches!(entry.kind, Kind::Main | Kind::Worktree)
    } else {
        entry.kind != Kind::Main && !entry.locked
    }
}

fn render_header(
    frame: &mut ratatui::Frame,
    area: Rect,
    args: &Args,
    selected_count: usize,
    selectable_count: usize,
    selected_bytes: u64,
    total_bytes: u64,
) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(75, 95, 130)))
        .padding(Padding::horizontal(1));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(inner);

    let mut badges = vec![
        Span::styled(
            if args.clean_only {
                " CLEAN TARGETS — keep worktrees "
            } else {
                " worktree-prune "
            },
            Style::default()
                .fg(Color::Rgb(16, 20, 28))
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            if args.yes { " APPLY " } else { " DRY RUN " },
            Style::default()
                .fg(Color::Rgb(16, 20, 28))
                .bg(if args.yes {
                    Color::Rgb(105, 210, 145)
                } else {
                    Color::Yellow
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(
            format!("{selected_count}/{selectable_count} selected"),
            Style::default()
                .fg(if selected_count > 0 {
                    Color::White
                } else {
                    Color::DarkGray
                })
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled("  │  ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(
                "Reclaim: {} / {}",
                human(selected_bytes),
                human(total_bytes)
            ),
            Style::default().fg(if selected_bytes > 0 {
                Color::Cyan
            } else {
                Color::Gray
            }),
        ),
    ];
    if args.keep_target {
        badges.push(Span::styled("  │  ", Style::default().fg(Color::DarkGray)));
        badges.push(Span::styled(
            "KEEP TARGET",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if args.delete_branch {
        badges.push(Span::styled("  │  ", Style::default().fg(Color::DarkGray)));
        badges.push(Span::styled(
            "DELETE BRANCH",
            Style::default()
                .fg(Color::Magenta)
                .add_modifier(Modifier::BOLD),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(badges)), rows[0]);

    if rows[1].height > 0 {
        let ratio = if total_bytes > 0 {
            (selected_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0)
        } else if selectable_count > 0 {
            (selected_count as f64 / selectable_count as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let gauge = LineGauge::default()
            .ratio(ratio)
            .label(Span::styled(
                format!("{:>3.0}% ", ratio * 100.0),
                Style::default().fg(Color::Gray),
            ))
            .filled_style(
                Style::default()
                    .fg(if selected_count > 0 {
                        Color::Cyan
                    } else {
                        Color::DarkGray
                    })
                    .add_modifier(Modifier::BOLD),
            )
            .unfilled_style(Style::default().fg(Color::Rgb(50, 58, 75)))
            .filled_symbol(symbols::line::THICK_HORIZONTAL)
            .unfilled_symbol(symbols::line::HORIZONTAL);
        frame.render_widget(gauge, rows[1]);
    }
}

#[allow(clippy::too_many_arguments)]
fn render_worktree_table(
    frame: &mut ratatui::Frame,
    area: Rect,
    entries: &[Entry],
    cells: &[[String; 5]],
    target_sizes: &[u64],
    selected: &BTreeSet<usize>,
    cursor: &mut TableState,
    clean_only: bool,
) {
    let rows = entries.iter().enumerate().map(|(i, e)| {
        let protected = !selectable(e, clean_only);
        let is_selected = selected.contains(&i);
        let marker = if protected {
            Cell::from(Span::styled("[-]", Style::default().fg(Color::DarkGray)))
        } else if is_selected {
            Cell::from(Span::styled(
                "[x]",
                Style::default()
                    .fg(Color::Rgb(105, 210, 145))
                    .add_modifier(Modifier::BOLD),
            ))
        } else {
            Cell::from(Span::styled("[ ]", Style::default().fg(Color::Gray)))
        };
        let data = &cells[i];
        let name_style = if protected {
            Style::default().fg(Color::DarkGray)
        } else if is_selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        Row::new(vec![
            marker,
            Cell::from(data[0].as_str()).style(name_style),
            Cell::from(data[1].as_str()).style(if data[1] == "detached" {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default()
            }),
            Cell::from(data[2].as_str()).style(
                Style::default()
                    .fg(state_color(&data[2]))
                    .add_modifier(Modifier::BOLD),
            ),
            Cell::from(data[3].as_str()).style(Style::default().fg(match data[3].as_str() {
                "clean" => Color::Green,
                "dirty" => Color::Yellow,
                _ => Color::Red,
            })),
            Cell::from(Line::from(data[4].as_str()).right_aligned())
                .style(Style::default().fg(target_color(target_sizes[i]))),
        ])
        .style(if protected {
            Style::default().fg(Color::DarkGray)
        } else {
            Style::default()
        })
    });

    let table = Table::new(
        rows,
        [
            Constraint::Length(4),
            Constraint::Fill(2),
            Constraint::Fill(3),
            Constraint::Length(18),
            Constraint::Length(9),
            Constraint::Length(12),
        ],
    )
    .header(
        Row::new(vec![
            Cell::from(""),
            Cell::from("WORKTREE"),
            Cell::from("BRANCH"),
            Cell::from("STATE"),
            Cell::from("CHANGES"),
            Cell::from(Line::from("TARGET").right_aligned()),
        ])
        .style(
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1),
    )
    .column_spacing(2)
    .block(
        Block::bordered()
            .border_type(BorderType::Rounded)
            .border_style(Style::default().fg(Color::Rgb(75, 95, 130)))
            .title(Span::styled(
                format!(" Worktrees ({}) ", entries.len()),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ))
            .padding(Padding::horizontal(1)),
    )
    .row_highlight_style(
        Style::default()
            .bg(Color::Rgb(42, 50, 68))
            .add_modifier(Modifier::BOLD),
    )
    .highlight_symbol("› ")
    .highlight_spacing(HighlightSpacing::Always);

    frame.render_stateful_widget(table, area, cursor);

    if entries.len() > 1 {
        let mut scrollbar_state =
            ScrollbarState::new(entries.len()).position(cursor.selected().unwrap_or(0));
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(Some("│"))
            .thumb_symbol("┃")
            .style(Style::default().fg(Color::Rgb(90, 110, 145)));
        frame.render_stateful_widget(
            scrollbar,
            area.inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn render_inspector(
    frame: &mut ratatui::Frame,
    area: Rect,
    entries: &[Entry],
    target_sizes: &[u64],
    selected: &BTreeSet<usize>,
    selected_bytes: u64,
    current_idx: Option<usize>,
    args: &Args,
) {
    let cols =
        Layout::horizontal([Constraint::Percentage(64), Constraint::Percentage(36)]).split(area);

    let detail_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(65, 80, 110)))
        .title(Span::styled(
            " Inspector ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1));

    if let Some(i) = current_idx
        && let Some(e) = entries.get(i)
    {
        let status_note = if args.clean_only {
            Span::styled(
                "Clean Cargo target only; keep worktree and branch",
                Style::default().fg(Color::Cyan),
            )
        } else if e.kind == Kind::Main {
            Span::styled(
                "Main checkout (protected)",
                Style::default().fg(Color::Cyan),
            )
        } else if e.locked {
            Span::styled(
                "Locked worktree (unlock explicitly to remove)",
                Style::default().fg(Color::Yellow),
            )
        } else if args.keep_target {
            Span::styled(
                "Cargo target will be kept (--keep-target)",
                Style::default().fg(Color::Yellow),
            )
        } else {
            Span::styled(
                format!(
                    "Kind: {:?}  •  Target size: {} (unshared targets removed)",
                    e.kind,
                    human(target_sizes[i])
                ),
                Style::default().fg(Color::Gray),
            )
        };
        let detail_table = Table::new(
            vec![
                Row::new(vec![
                    Cell::from(Span::styled(
                        "Path",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Cell::from(e.path.display().to_string()),
                ]),
                Row::new(vec![
                    Cell::from(Span::styled(
                        "Target",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Cell::from(e.target.display().to_string()),
                ]),
                Row::new(vec![
                    Cell::from(Span::styled(
                        "Policy",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )),
                    Cell::from(Line::from(status_note)),
                ]),
            ],
            [Constraint::Length(8), Constraint::Fill(1)],
        )
        .column_spacing(1)
        .block(detail_block);
        frame.render_widget(detail_table, cols[0]);
    } else {
        frame.render_widget(
            Paragraph::new("No worktrees found.").block(detail_block),
            cols[0],
        );
    }

    let summary_block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(65, 80, 110)))
        .title(Span::styled(
            " Batch Summary ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ))
        .padding(Padding::horizontal(1));

    let items = vec![
        ListItem::new(Line::from(vec![
            Span::styled("Selected: ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                format!("{} item(s)", selected.len()),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled("To free:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                human(selected_bytes),
                Style::default()
                    .fg(target_color(selected_bytes))
                    .add_modifier(Modifier::BOLD),
            ),
        ])),
        ListItem::new(Line::from(vec![
            Span::styled("Confirm:  ", Style::default().fg(Color::DarkGray)),
            Span::styled(
                "Single modal confirmation",
                Style::default().fg(Color::Gray),
            ),
        ])),
    ];
    frame.render_widget(List::new(items).block(summary_block), cols[1]);
}

fn render_footer(frame: &mut ratatui::Frame, area: Rect) {
    let key_style = Style::default()
        .fg(Color::Rgb(16, 20, 28))
        .bg(Color::Rgb(130, 150, 185))
        .add_modifier(Modifier::BOLD);
    let desc_style = Style::default().fg(Color::Gray);
    let sep = Span::styled("   ", Style::default());

    let line = Line::from(vec![
        Span::styled(" ↑↓ / j k ", key_style),
        Span::styled(" move", desc_style),
        sep.clone(),
        Span::styled(" Space ", key_style),
        Span::styled(" select", desc_style),
        sep.clone(),
        Span::styled(" a ", key_style),
        Span::styled(" toggle all", desc_style),
        sep.clone(),
        Span::styled(" c ", key_style),
        Span::styled(" clean/remove mode", desc_style),
        sep.clone(),
        Span::styled(" Enter ", key_style),
        Span::styled(" preview & confirm", desc_style),
        sep,
        Span::styled(" q ", key_style),
        Span::styled(" quit", desc_style),
    ]);

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(Color::Rgb(65, 80, 110)))
        .padding(Padding::horizontal(1));
    frame.render_widget(Paragraph::new(line).block(block), area);
}

fn format_plan_lines(plan: &Plan) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for note in &plan.notes {
        lines.push(Line::from(vec![
            Span::styled(
                "NOTE ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(note.clone()),
        ]));
    }
    for item in &plan.items {
        if item.clean_only {
            lines.push(Line::from(format!(
                "CLEAN cargo target: {}",
                item.entry.target.display()
            )));
            lines.push(Line::from(format!(
                "KEEP worktree and branch: {}",
                item.entry.path.display()
            )));
            continue;
        }
        lines.push(Line::from(vec![
            Span::styled(
                "REMOVE ",
                Style::default()
                    .fg(Color::Rgb(245, 119, 128))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{:?}: ", item.entry.kind),
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(item.entry.path.display().to_string()),
        ]));
        if item.target {
            lines.push(Line::from(vec![
                Span::styled("  └─ cargo target: ", Style::default().fg(Color::DarkGray)),
                Span::styled(
                    item.entry.target.display().to_string(),
                    Style::default().fg(Color::Gray),
                ),
            ]));
        }
    }
    for err in &plan.errors {
        lines.push(Line::from(vec![
            Span::styled(
                "BLOCKED: ",
                Style::default()
                    .fg(Color::Rgb(245, 119, 128))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(err.clone()),
        ]));
    }
    if lines.is_empty() {
        lines.push(Line::from("Nothing to do."));
    }
    lines
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

fn render_modal(frame: &mut ratatui::Frame, modal: &Modal, selected: usize, scroll: u16) {
    let screen = frame.area();
    let background = Color::Rgb(24, 28, 39);
    let panel = Color::Rgb(32, 38, 51);
    let foreground = Color::Rgb(230, 234, 242);
    let muted = Color::Rgb(154, 165, 184);

    let (accent, mut title, subtitle) = match modal.kind {
        ModalKind::ConfirmPlan => (
            Color::Rgb(105, 210, 145),
            "Removal plan — confirm deletion",
            format!(
                "{selected} selected  •  {} target data to free",
                human(modal.freed_bytes)
            ),
        ),
        ModalKind::ConfirmForce {
            needs_force: _,
            needs_sudo: true,
        } => (
            Color::Rgb(245, 155, 75),
            "Blocked — confirm FORCE + SUDO",
            format!(
                "{selected} selected  •  {} to free  •  sudo required for locked folders",
                human(modal.freed_bytes)
            ),
        ),
        ModalKind::ConfirmForce {
            needs_force: _,
            needs_sudo: false,
        } => (
            Color::Rgb(245, 190, 85),
            "Blocked — confirm FORCE",
            format!(
                "{selected} selected  •  {} to free  •  Nothing has been removed yet",
                human(modal.freed_bytes)
            ),
        ),
        ModalKind::HardBlocked => (
            Color::Rgb(245, 119, 128),
            "Blocked — cannot force",
            format!("{selected} selected  •  Nothing has been removed"),
        ),
    };

    if modal.clean_only {
        title = match modal.kind {
            ModalKind::ConfirmPlan => "Clean targets — keep worktrees",
            ModalKind::ConfirmForce { .. } => "Clean targets — confirm SUDO",
            ModalKind::HardBlocked => "Clean targets — blocked",
        };
    }
    frame.buffer_mut().set_style(
        screen,
        Style::default()
            .fg(Color::Rgb(83, 91, 108))
            .bg(Color::Rgb(15, 18, 26)),
    );
    let width = screen.width.saturating_sub(4).min(100);
    let height = screen.height.saturating_sub(2).min(30);
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
        .title(if modal.clean_only {
            " Clean targets "
        } else {
            " Removal plan "
        })
        .border_style(Style::default().fg(accent))
        .style(Style::default().bg(background).fg(foreground))
        .padding(Padding::horizontal(if width >= 60 { 2 } else { 1 }));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let compact = inner.height < 20;
    let parts = Layout::vertical([
        Constraint::Length(if compact { 2 } else { 3 }),
        Constraint::Min(4),
        Constraint::Length(if compact { 3 } else { 4 }),
        Constraint::Length(3),
        Constraint::Length(1),
    ])
    .split(inner);

    let mut header_spans = vec![Span::styled(
        format!("!  {title}"),
        Style::default().fg(accent).add_modifier(Modifier::BOLD),
    )];
    if let ModalKind::ConfirmForce {
        needs_force,
        needs_sudo,
    } = modal.kind
    {
        if needs_force {
            header_spans.push(Span::raw("  "));
            header_spans.push(Span::styled(
                " FORCE ",
                Style::default()
                    .fg(background)
                    .bg(Color::Rgb(245, 190, 85))
                    .add_modifier(Modifier::BOLD),
            ));
        }
        if needs_sudo {
            header_spans.push(Span::raw(" "));
            header_spans.push(Span::styled(
                " SUDO ",
                Style::default()
                    .fg(background)
                    .bg(Color::Rgb(245, 119, 128))
                    .add_modifier(Modifier::BOLD),
            ));
        }
    }

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(header_spans),
            Line::from(Span::styled(subtitle, Style::default().fg(muted))),
        ]),
        parts[0],
    );

    let mut body_lines: Vec<Line<'static>> = Vec::new();
    if !modal.reasons.is_empty() {
        body_lines.push(Line::from(Span::styled(
            "Reasons for blocking:",
            Style::default().fg(accent).add_modifier(Modifier::BOLD),
        )));
        for r in &modal.reasons {
            body_lines.push(Line::from(vec![
                Span::styled("  • ", Style::default().fg(accent)),
                Span::raw(r.clone()),
            ]));
        }
        body_lines.push(Line::from(""));
    }
    if !modal.plan_lines.is_empty() {
        body_lines.push(Line::from(Span::styled(
            "Plan details:",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        body_lines.extend(modal.plan_lines.iter().cloned());
    }

    let total_lines = body_lines.len();
    frame.render_widget(
        Paragraph::new(body_lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll, 0))
            .block(
                Block::bordered()
                    .border_type(BorderType::Rounded)
                    .title(" Checks & items ")
                    .border_style(Style::default().fg(Color::Rgb(67, 78, 98)))
                    .style(Style::default().bg(panel).fg(foreground))
                    .padding(Padding::horizontal(1)),
            ),
        parts[1],
    );

    if total_lines > parts[1].height.saturating_sub(2) as usize {
        let mut sb_state = ScrollbarState::new(total_lines).position(scroll as usize);
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .style(Style::default().fg(accent));
        frame.render_stateful_widget(
            sb,
            parts[1].inner(Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut sb_state,
        );
    }

    let (callout_title, warning) = if modal.clean_only {
        (
            "Keep worktrees and branches",
            "Only the listed Cargo build targets will be deleted. Source files, local changes and Git registrations are preserved.",
        )
    } else {
        match modal.kind {
            ModalKind::ConfirmPlan => (
                "Ready to remove",
                "Selected worktrees and their unshared Cargo targets will be permanently deleted.",
            ),
            ModalKind::ConfirmForce {
                needs_force: _,
                needs_sudo: true,
            } => (
                "Before you continue (FORCE + SUDO)",
                "Locked/unwritable or foreign-owned folders will be removed with sudo (password may be requested). Local changes and untracked files will be lost.",
            ),
            ModalKind::ConfirmForce {
                needs_force: _,
                needs_sudo: false,
            } => (
                "Before you continue (FORCE)",
                "Local changes and untracked files will be lost. Local-only commits remain only in the local branch.",
            ),
            ModalKind::HardBlocked => (
                "This selection is protected",
                "Force and sudo cannot bypass these protections. Resolve the reasons above, then try again.",
            ),
        }
    };

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                callout_title,
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
    match modal.kind {
        ModalKind::ConfirmPlan => {
            buttons.push(Span::raw("   "));
            buttons.push(Span::styled(
                if modal.clean_only {
                    "  y  Confirm clean  "
                } else {
                    "  y  Confirm removal  "
                },
                Style::default()
                    .fg(background)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        ModalKind::ConfirmForce {
            needs_force: _,
            needs_sudo: true,
        } => {
            buttons.push(Span::raw("   "));
            buttons.push(Span::styled(
                if modal.clean_only {
                    "  y  Clean with sudo  "
                } else {
                    "  y  Confirm force + sudo  "
                },
                Style::default()
                    .fg(background)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        ModalKind::ConfirmForce {
            needs_force: _,
            needs_sudo: false,
        } => {
            buttons.push(Span::raw("   "));
            buttons.push(Span::styled(
                "  y  Confirm force removal  ",
                Style::default()
                    .fg(background)
                    .bg(accent)
                    .add_modifier(Modifier::BOLD),
            ));
        }
        ModalKind::HardBlocked => (),
    }
    frame.render_widget(
        Paragraph::new(vec![Line::from(""), Line::from(buttons).right_aligned()]),
        parts[3],
    );
    frame.render_widget(
        Paragraph::new("↑/↓ Scroll  •  y Confirm  •  n/Esc Back  •  q Quit")
            .style(Style::default().fg(muted)),
        parts[4],
    );
}
