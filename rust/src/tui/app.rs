use crate::core::events::{EventKind, LeanCtxEvent};
use crate::core::gain::gain_score::GainScore;
use crate::core::gain::model_pricing::ModelPricing;
use crate::core::gain::task_classifier::{TaskCategory, TaskClassifier};
use crate::tui::event_reader::EventTail;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::ExecutableCommand;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Row, Table};
use ratatui::Terminal;
use std::io::stdout;
use std::time::{Duration, Instant};

const GREEN: Color = Color::Rgb(52, 211, 153);
const PURPLE: Color = Color::Rgb(129, 140, 248);
const BLUE: Color = Color::Rgb(56, 189, 248);
const YELLOW: Color = Color::Rgb(251, 191, 36);
const MUTED: Color = Color::Rgb(107, 107, 136);
const SURFACE: Color = Color::Rgb(10, 10, 18);
const BG: Color = Color::Rgb(6, 6, 10);

struct AppState {
    events: Vec<LeanCtxEvent>,
    total_saved: u64,
    total_original: u64,
    cache_hits: u64,
    total_calls: u64,
    files: std::collections::HashMap<String, FileHeat>,
    gain_score: Option<GainScore>,
    last_gain_refresh: Instant,
    quit: bool,
    focus: usize,
}

struct FileHeat {
    access_count: u32,
    tokens_saved: u64,
}

impl AppState {
    fn new() -> Self {
        Self {
            events: Vec::new(),
            total_saved: 0,
            total_original: 0,
            cache_hits: 0,
            total_calls: 0,
            files: std::collections::HashMap::new(),
            gain_score: None,
            last_gain_refresh: Instant::now(),
            quit: false,
            focus: 0,
        }
    }

    fn ingest(&mut self, new_events: Vec<LeanCtxEvent>) {
        for ev in &new_events {
            match &ev.kind {
                EventKind::ToolCall {
                    tool: _,
                    tokens_original,
                    tokens_saved,
                    path,
                    ..
                } => {
                    self.total_saved += tokens_saved;
                    self.total_original += tokens_original;
                    self.total_calls += 1;
                    if let Some(p) = path {
                        let entry = self.files.entry(p.clone()).or_insert(FileHeat {
                            access_count: 0,
                            tokens_saved: 0,
                        });
                        entry.access_count += 1;
                        entry.tokens_saved += tokens_saved;
                    }
                }
                EventKind::CacheHit { path, saved_tokens } => {
                    self.cache_hits += 1;
                    self.total_saved += saved_tokens;
                    let entry = self.files.entry(path.clone()).or_insert(FileHeat {
                        access_count: 0,
                        tokens_saved: 0,
                    });
                    entry.access_count += 1;
                    entry.tokens_saved += saved_tokens;
                }
                _ => {}
            }
        }
        self.events.extend(new_events);
        if self.events.len() > 200 {
            let drain = self.events.len() - 200;
            self.events.drain(..drain);
        }
    }

    fn savings_pct(&self) -> f64 {
        if self.total_original == 0 {
            return 0.0;
        }
        self.total_saved as f64 / self.total_original as f64 * 100.0
    }

    fn cache_rate(&self) -> f64 {
        if self.total_calls == 0 {
            return 0.0;
        }
        self.cache_hits as f64 / self.total_calls as f64 * 100.0
    }

    fn refresh_gain_score(&mut self) {
        if self.last_gain_refresh.elapsed() < Duration::from_secs(2) {
            return;
        }
        let engine = crate::core::gain::GainEngine::load();
        self.gain_score = Some(engine.gain_score(None));
        self.last_gain_refresh = Instant::now();
    }
}

pub fn run() -> anyhow::Result<()> {
    enable_raw_mode()?;
    stdout().execute(EnterAlternateScreen)?;
    let backend = ratatui::backend::CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;

    let mut state = AppState::new();
    let mut tail = EventTail::new();
    let tick_rate = Duration::from_millis(200);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| draw(f, &state))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => state.quit = true,
                        KeyCode::Tab => state.focus = (state.focus + 1) % 5,
                        KeyCode::Char('1') => state.focus = 0,
                        KeyCode::Char('2') => state.focus = 1,
                        KeyCode::Char('3') => state.focus = 2,
                        KeyCode::Char('4') => state.focus = 3,
                        KeyCode::Char('5') => state.focus = 4,
                        _ => {}
                    }
                }
            }
        }

        if last_tick.elapsed() >= tick_rate {
            let new = tail.poll();
            if !new.is_empty() {
                state.ingest(new);
            }
            state.refresh_gain_score();
            last_tick = Instant::now();
        }

        if state.quit {
            break;
        }
    }

    disable_raw_mode()?;
    stdout().execute(LeaveAlternateScreen)?;
    Ok(())
}

fn draw(f: &mut ratatui::Frame, state: &AppState) {
    let size = f.area();

    let header_body = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(size);

    draw_header(f, header_body[0], state);

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
        .split(header_body[1]);

    let left = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(columns[0]);

    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(38),
            Constraint::Percentage(37),
            Constraint::Percentage(25),
        ])
        .split(columns[1]);

    draw_live_feed(f, left[0], state);
    draw_heatmap(f, left[1], state);
    draw_savings(f, right[0], state);
    draw_session(f, right[1], state);
    draw_task_activity(f, right[2], state);
}

fn draw_header(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let saved = format_tokens(state.total_saved);
    let pct = format!("{:.0}%", state.savings_pct());
    let env_model = std::env::var("LEAN_CTX_MODEL")
        .or_else(|_| std::env::var("LCTX_MODEL"))
        .ok();
    let pricing = ModelPricing::load();
    let quote = pricing.quote(env_model.as_deref());
    let cost = format!(
        "${:.3}",
        state.total_saved as f64 * quote.cost.input_per_m / 1_000_000.0
    );
    let gain_score = state.gain_score.as_ref().map(|s| s.total).unwrap_or(0);
    let trend_icon = state
        .gain_score
        .as_ref()
        .map(|s| match s.trend {
            crate::core::gain::gain_score::Trend::Rising => "▲",
            crate::core::gain::gain_score::Trend::Stable => "─",
            crate::core::gain::gain_score::Trend::Declining => "▼",
        })
        .unwrap_or("─");
    let trend_color = state
        .gain_score
        .as_ref()
        .map(|s| match s.trend {
            crate::core::gain::gain_score::Trend::Rising => GREEN,
            crate::core::gain::gain_score::Trend::Stable => MUTED,
            crate::core::gain::gain_score::Trend::Declining => YELLOW,
        })
        .unwrap_or(MUTED);

    let spans = vec![
        Span::styled(
            " LeanCTX ",
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ),
        Span::styled("Observatory ", Style::default().fg(MUTED)),
        Span::raw("   "),
        Span::styled(format!("{saved} saved"), Style::default().fg(GREEN)),
        Span::raw("  "),
        Span::styled(format!("{pct} compression"), Style::default().fg(PURPLE)),
        Span::raw("  "),
        Span::styled(format!("{cost} avoided"), Style::default().fg(BLUE)),
        Span::raw("  "),
        Span::styled(format!("{gain_score}/100 gain"), Style::default().fg(GREEN)),
        Span::styled(format!(" {trend_icon}"), Style::default().fg(trend_color)),
        Span::raw("  "),
        Span::styled(
            format!("{} events", state.events.len()),
            Style::default().fg(MUTED),
        ),
    ];

    let header = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::BOTTOM)
            .border_style(Style::default().fg(Color::Rgb(30, 30, 50))),
    );
    f.render_widget(header, area);
}

fn draw_task_activity(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .title(Span::styled(
            " Task Activity ",
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if state.focus == 4 {
            GREEN
        } else {
            Color::Rgb(30, 30, 50)
        }))
        .style(Style::default().bg(SURFACE));

    let mut counts: std::collections::HashMap<TaskCategory, u64> = std::collections::HashMap::new();
    for ev in state.events.iter().rev().take(120) {
        if let EventKind::ToolCall { tool, .. } = &ev.kind {
            let cat = TaskClassifier::classify_tool(tool);
            *counts.entry(cat).or_insert(0) += 1;
        }
    }

    let mut rows: Vec<(TaskCategory, u64)> = counts.into_iter().collect();
    rows.sort_by_key(|x| std::cmp::Reverse(x.1));

    let max_items = area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = if rows.is_empty() {
        vec![ListItem::new(Line::from(vec![Span::styled(
            "No tool calls yet.",
            Style::default().fg(MUTED),
        )]))]
    } else {
        rows.into_iter()
            .take(max_items)
            .map(|(cat, n)| {
                ListItem::new(Line::from(vec![
                    Span::styled(
                        format!("{:<14}", cat.label()),
                        Style::default().fg(Color::Rgb(220, 220, 240)),
                    ),
                    Span::styled(format!("{:>4}", n), Style::default().fg(MUTED)),
                ]))
            })
            .collect()
    };

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_live_feed(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .title(Span::styled(
            " Live Feed ",
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if state.focus == 0 {
            GREEN
        } else {
            Color::Rgb(30, 30, 50)
        }))
        .style(Style::default().bg(SURFACE));

    let visible = area.height.saturating_sub(2) as usize;
    let start = state.events.len().saturating_sub(visible);
    let items: Vec<ListItem> = state.events[start..]
        .iter()
        .rev()
        .map(|ev| {
            let (icon, tool, detail, color) = match &ev.kind {
                EventKind::ToolCall {
                    tool,
                    tokens_original,
                    tokens_saved,
                    mode,
                    ..
                } => {
                    let pct = if *tokens_original > 0 {
                        format!("-{}%", tokens_saved * 100 / tokens_original)
                    } else {
                        String::new()
                    };
                    let m = mode.as_deref().unwrap_or("");
                    (
                        ">>",
                        tool.as_str(),
                        format!(
                            "{} {}t->{}t {}",
                            m,
                            tokens_original,
                            tokens_original - tokens_saved,
                            pct
                        ),
                        GREEN,
                    )
                }
                EventKind::CacheHit { path, saved_tokens } => {
                    let short = path.rsplit('/').next().unwrap_or(path);
                    (
                        "**",
                        "cache",
                        format!("{short} {saved_tokens}t saved"),
                        PURPLE,
                    )
                }
                EventKind::Compression {
                    path,
                    strategy,
                    before_lines,
                    after_lines,
                    ..
                } => {
                    let short = path.rsplit('/').next().unwrap_or(path);
                    (
                        "~~",
                        "compress",
                        format!("{short} {strategy} {before_lines}L->{after_lines}L"),
                        BLUE,
                    )
                }
                EventKind::AgentAction {
                    agent_id, action, ..
                } => ("@@", "agent", format!("{agent_id} {action}"), YELLOW),
                EventKind::KnowledgeUpdate {
                    category,
                    key,
                    action,
                } => (
                    "!!",
                    "knowledge",
                    format!("{action} {category}/{key}"),
                    PURPLE,
                ),
                EventKind::ThresholdShift {
                    language,
                    new_entropy,
                    new_jaccard,
                    ..
                } => (
                    "~~",
                    "threshold",
                    format!("{language} e={new_entropy:.2} j={new_jaccard:.2}"),
                    MUTED,
                ),
            };
            let ts = &ev.timestamp[11..19.min(ev.timestamp.len())];
            ListItem::new(Line::from(vec![
                Span::styled(format!("{ts} "), Style::default().fg(MUTED)),
                Span::styled(format!("{icon} "), Style::default().fg(color)),
                Span::styled(
                    format!("{tool:14}"),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(detail, Style::default().fg(MUTED)),
            ]))
        })
        .collect();

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_heatmap(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .title(Span::styled(
            " File Heatmap ",
            Style::default().fg(YELLOW).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if state.focus == 2 {
            GREEN
        } else {
            Color::Rgb(30, 30, 50)
        }))
        .style(Style::default().bg(SURFACE));

    let mut files: Vec<_> = state.files.iter().collect();
    files.sort_by_key(|x| std::cmp::Reverse(x.1.access_count));
    let max_access = files.first().map(|f| f.1.access_count).unwrap_or(1).max(1);

    let visible = (area.height.saturating_sub(2)) as usize;
    let rows: Vec<Row> = files
        .iter()
        .take(visible)
        .map(|(path, heat)| {
            let short = path.rsplit('/').next().unwrap_or(path);
            let bar_len = (heat.access_count as f64 / max_access as f64 * 12.0) as usize;
            let bar: String = "█".repeat(bar_len) + &"░".repeat(12 - bar_len);
            Row::new(vec![
                ratatui::widgets::Cell::from(Span::styled(
                    format!("{short:20}"),
                    Style::default().fg(Color::White),
                )),
                ratatui::widgets::Cell::from(Span::styled(bar, Style::default().fg(YELLOW))),
                ratatui::widgets::Cell::from(Span::styled(
                    format!("{}x", heat.access_count),
                    Style::default().fg(MUTED),
                )),
                ratatui::widgets::Cell::from(Span::styled(
                    format!("{}t", format_tokens(heat.tokens_saved)),
                    Style::default().fg(GREEN),
                )),
            ])
        })
        .collect();

    let table = Table::new(
        rows,
        [
            Constraint::Length(22),
            Constraint::Length(14),
            Constraint::Length(6),
            Constraint::Length(10),
        ],
    )
    .block(block);
    f.render_widget(table, area);
}

fn draw_savings(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .title(Span::styled(
            " Token Savings ",
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if state.focus == 1 {
            GREEN
        } else {
            Color::Rgb(30, 30, 50)
        }))
        .style(Style::default().bg(SURFACE));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Length(1),
            Constraint::Length(2),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(inner);

    let pct = state.savings_pct();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                format!(" {} saved ", format_tokens(state.total_saved)),
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(format!("({:.0}%)", pct), Style::default().fg(MUTED)),
        ])),
        chunks[0],
    );

    let ratio = (pct / 100.0).min(1.0);
    f.render_widget(
        Gauge::default()
            .ratio(ratio)
            .gauge_style(Style::default().fg(GREEN).bg(BG))
            .label(format!("{:.0}%", pct)),
        chunks[1],
    );

    f.render_widget(Paragraph::new(""), chunks[2]);

    let cache_pct = state.cache_rate();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Cache Hit Rate ", Style::default().fg(PURPLE)),
            Span::styled(format!("{:.0}%", cache_pct), Style::default().fg(MUTED)),
        ])),
        chunks[3],
    );

    let cache_ratio = (cache_pct / 100.0).min(1.0);
    f.render_widget(
        Gauge::default()
            .ratio(cache_ratio)
            .gauge_style(Style::default().fg(PURPLE).bg(BG))
            .label(format!("{:.0}%", cache_pct)),
        chunks[4],
    );
}

fn draw_session(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let block = Block::default()
        .title(Span::styled(
            " Session ",
            Style::default().fg(BLUE).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if state.focus == 3 {
            GREEN
        } else {
            Color::Rgb(30, 30, 50)
        }))
        .style(Style::default().bg(SURFACE));

    let cost = state.total_saved as f64 * 2.5 / 1_000_000.0;

    let lines = vec![
        Line::from(vec![
            Span::styled("  Calls     ", Style::default().fg(MUTED)),
            Span::styled(
                format!("{}", state.total_calls),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Files     ", Style::default().fg(MUTED)),
            Span::styled(
                format!("{}", state.files.len()),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Original  ", Style::default().fg(MUTED)),
            Span::styled(
                format_tokens(state.total_original),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Sent      ", Style::default().fg(MUTED)),
            Span::styled(
                format_tokens(state.total_original.saturating_sub(state.total_saved)),
                Style::default().fg(Color::White),
            ),
        ]),
        Line::from(vec![
            Span::styled("  Saved     ", Style::default().fg(MUTED)),
            Span::styled(format!("${cost:.3}"), Style::default().fg(GREEN)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  q=quit Tab=focus 1-4=panel",
            Style::default().fg(Color::Rgb(50, 50, 70)),
        )),
    ];

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}
