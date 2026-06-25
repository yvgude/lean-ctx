use crate::core::events::{EventKind, LeanCtxEvent};
use crate::core::gain::gain_score::GainScore;
use crate::core::gain::model_pricing::ModelPricing;
use crate::core::gain::task_classifier::{TaskCategory, TaskClassifier};
use crate::tui::event_reader::EventTail;
use crossterm::ExecutableCommand;
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Gauge, List, ListItem, Paragraph, Row, Table};
use std::io::stdout;
use std::time::{Duration, Instant};

fn tui_colors() -> TuiTheme {
    let t = crate::core::theme::load_theme(&crate::core::config::Config::load().theme);
    let to_ratatui = |c: &crate::core::theme::Color| {
        let (r, g, b) = c.rgb();
        Color::Rgb(r, g, b)
    };
    TuiTheme {
        green: to_ratatui(&t.success),
        muted: to_ratatui(&t.muted),
        surface: to_ratatui(&t.surface),
        bg: to_ratatui(&t.background),
    }
}

struct TuiTheme {
    green: Color,
    muted: Color,
    surface: Color,
    bg: Color,
}

const GREEN: Color = Color::Rgb(52, 211, 153);
const PURPLE: Color = Color::Rgb(129, 140, 248);
const BLUE: Color = Color::Rgb(56, 189, 248);
const YELLOW: Color = Color::Rgb(251, 191, 36);
const RED: Color = Color::Rgb(248, 113, 113);
const MUTED: Color = Color::Rgb(107, 107, 136);
const SURFACE: Color = Color::Rgb(10, 10, 18);
const BG: Color = Color::Rgb(6, 6, 10);

struct AppState {
    events: Vec<LeanCtxEvent>,
    total_saved: u64,
    total_original: u64,
    cache_hits: u64,
    cache_reads: u64,
    total_calls: u64,
    files: std::collections::HashMap<String, FileHeat>,
    gain_score: Option<GainScore>,
    last_gain_refresh: Instant,
    quit: bool,
    focus: usize,
    filter: EventFilter,
    search_query: String,
    search_active: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum EventFilter {
    All,
    Reads,
    Shell,
    Cache,
    Errors,
}

impl EventFilter {
    fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Reads => "reads",
            Self::Shell => "shell",
            Self::Cache => "cache",
            Self::Errors => "errors",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::All => Self::Reads,
            Self::Reads => Self::Shell,
            Self::Shell => Self::Cache,
            Self::Cache => Self::Errors,
            Self::Errors => Self::All,
        }
    }

    fn matches(self, ev: &EventKind) -> bool {
        match self {
            Self::All => true,
            Self::Reads => matches!(ev, EventKind::ToolCall { tool, .. } if tool.contains("read")),
            Self::Shell => matches!(ev, EventKind::ToolCall { tool, .. } if tool.contains("shell")),
            Self::Cache => matches!(ev, EventKind::CacheHit { .. }),
            Self::Errors => matches!(
                ev,
                EventKind::BudgetExhausted { .. }
                    | EventKind::PolicyViolation { .. }
                    | EventKind::SloViolation { .. }
                    | EventKind::BudgetWarning { .. }
                    | EventKind::VerificationWarning { .. }
            ),
        }
    }
}

struct FileHeat {
    access_count: u32,
    tokens_saved: u64,
}

impl AppState {
    fn new() -> Self {
        let store = crate::core::stats::load();
        let heatmap = crate::core::heatmap::HeatMap::load();
        let files = heatmap
            .entries
            .values()
            .map(|e| {
                (
                    e.path.clone(),
                    FileHeat {
                        access_count: e.access_count,
                        tokens_saved: e.total_tokens_saved,
                    },
                )
            })
            .collect();
        Self {
            events: Vec::new(),
            total_saved: store
                .total_input_tokens
                .saturating_sub(store.total_output_tokens),
            total_original: store.total_input_tokens,
            cache_hits: store.cep.total_cache_hits,
            cache_reads: store.cep.total_cache_reads,
            total_calls: store.total_commands,
            files,
            gain_score: None,
            last_gain_refresh: Instant::now(),
            quit: false,
            focus: 0,
            filter: EventFilter::All,
            search_query: String::new(),
            search_active: false,
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
                EventKind::Compression { path, .. } => {
                    let entry = self.files.entry(path.clone()).or_insert(FileHeat {
                        access_count: 0,
                        tokens_saved: 0,
                    });
                    entry.access_count += 1;
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
        if self.cache_reads == 0 {
            return 0.0;
        }
        self.cache_hits as f64 / self.cache_reads as f64 * 100.0
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
    // Seed the view with recent history so `watch` isn't a blank screen when
    // launched while idle — the log is already populated (#560).
    let backfill = tail.backfill(20);
    if !backfill.is_empty() {
        state.ingest(backfill);
    }
    let tick_rate = Duration::from_millis(200);
    let mut last_tick = Instant::now();

    loop {
        terminal.draw(|f| draw(f, &state))?;

        let timeout = tick_rate.saturating_sub(last_tick.elapsed());
        if event::poll(timeout)?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            if state.search_active {
                match key.code {
                    KeyCode::Esc | KeyCode::Enter => state.search_active = false,
                    KeyCode::Backspace => {
                        state.search_query.pop();
                    }
                    KeyCode::Char(c) => state.search_query.push(c),
                    _ => {}
                }
            } else {
                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => state.quit = true,
                    KeyCode::Tab => state.focus = (state.focus + 1) % 5,
                    KeyCode::Char('1') => state.focus = 0,
                    KeyCode::Char('2') => state.focus = 1,
                    KeyCode::Char('3') => state.focus = 2,
                    KeyCode::Char('4') => state.focus = 3,
                    KeyCode::Char('5') => state.focus = 4,
                    KeyCode::Char('f') => state.filter = state.filter.next(),
                    KeyCode::Char('/') => {
                        state.search_active = true;
                        state.search_query.clear();
                    }
                    _ => {}
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
    let tc = tui_colors();
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
            Constraint::Length(5),
            Constraint::Percentage(35),
            Constraint::Percentage(35),
            Constraint::Min(0),
        ])
        .split(columns[1]);

    draw_live_feed(f, left[0], state);
    draw_heatmap(f, left[1], state);
    draw_gain_score_widget(f, right[0], state, &tc);
    draw_savings(f, right[1], state);
    draw_session(f, right[2], state);
    draw_task_activity(f, right[3], state);
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
        "${:.2}",
        state.total_saved as f64 * quote.cost.input_per_m / 1_000_000.0
    );
    let gain_score = state.gain_score.as_ref().map_or(0, |s| s.total);
    let trend_icon = state.gain_score.as_ref().map_or("─", |s| match s.trend {
        crate::core::gain::gain_score::Trend::Rising => "▲",
        crate::core::gain::gain_score::Trend::Stable => "─",
        crate::core::gain::gain_score::Trend::Declining => "▼",
    });
    let trend_color = state.gain_score.as_ref().map_or(MUTED, |s| match s.trend {
        crate::core::gain::gain_score::Trend::Rising => GREEN,
        crate::core::gain::gain_score::Trend::Stable => MUTED,
        crate::core::gain::gain_score::Trend::Declining => YELLOW,
    });

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

fn draw_gain_score_widget(f: &mut ratatui::Frame, area: Rect, state: &AppState, tc: &TuiTheme) {
    let gain_score = state.gain_score.as_ref().map_or(0, |s| s.total);
    let default_lvl = crate::core::gain::gain_score::GainLevel {
        level: 0,
        title: "Novice",
        min_score: 0,
    };
    let lvl = state
        .gain_score
        .as_ref()
        .map_or(default_lvl, crate::core::gain::gain_score::GainScore::level);

    let block = Block::default()
        .title(Span::styled(
            " Gain Score ",
            Style::default().fg(tc.green).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(30, 30, 50)))
        .style(Style::default().bg(tc.surface));

    let inner = block.inner(area);
    f.render_widget(block, area);

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(1), Constraint::Length(2)])
        .split(inner);

    let score_line = Line::from(vec![
        Span::styled(
            format!(" {gain_score}/100 "),
            Style::default().fg(tc.green).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("Lv{} {}", lvl.level, lvl.title),
            Style::default().fg(tc.muted),
        ),
    ]);
    f.render_widget(Paragraph::new(score_line), chunks[0]);

    let ratio = (f64::from(gain_score) / 100.0).min(1.0);
    f.render_widget(
        Gauge::default()
            .ratio(ratio)
            .gauge_style(Style::default().fg(tc.green).bg(tc.bg))
            .label(format!("{gain_score}%")),
        chunks[1],
    );
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
                    Span::styled(format!("{n:>4}"), Style::default().fg(MUTED)),
                ]))
            })
            .collect()
    };

    let list = List::new(items).block(block);
    f.render_widget(list, area);
}

fn draw_live_feed(f: &mut ratatui::Frame, area: Rect, state: &AppState) {
    let filter_label = if state.filter == EventFilter::All {
        " Live Feed ".to_string()
    } else {
        format!(" Live Feed [{}] ", state.filter.label())
    };
    let title_spans = if state.search_active {
        vec![
            Span::styled(
                filter_label,
                Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!(" /{}", state.search_query),
                Style::default().fg(YELLOW),
            ),
        ]
    } else {
        vec![Span::styled(
            filter_label,
            Style::default().fg(GREEN).add_modifier(Modifier::BOLD),
        )]
    };
    let block = Block::default()
        .title(Line::from(title_spans))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(if state.focus == 0 {
            GREEN
        } else {
            Color::Rgb(30, 30, 50)
        }))
        .style(Style::default().bg(SURFACE));

    if state.events.is_empty() {
        let msg = Paragraph::new(vec![
            Line::from(""),
            Line::from(Span::styled(
                "  Waiting for events...",
                Style::default().fg(MUTED),
            )),
            Line::from(""),
            Line::from(Span::styled(
                "  Use lean-ctx in your editor or run:",
                Style::default().fg(MUTED),
            )),
            Line::from(Span::styled(
                "  lean-ctx -c \"git status\"",
                Style::default().fg(BLUE),
            )),
        ])
        .block(block);
        f.render_widget(msg, area);
        return;
    }

    let visible = area.height.saturating_sub(2) as usize;
    let filtered_events: Vec<&LeanCtxEvent> = state
        .events
        .iter()
        .filter(|ev| state.filter.matches(&ev.kind))
        .filter(|ev| {
            if state.search_query.is_empty() {
                return true;
            }
            let q = &state.search_query;
            match &ev.kind {
                EventKind::ToolCall { tool, path, .. } => {
                    tool.contains(q.as_str())
                        || path.as_ref().is_some_and(|p| p.contains(q.as_str()))
                }
                EventKind::CacheHit { path, .. } | EventKind::Compression { path, .. } => {
                    path.contains(q.as_str())
                }
                _ => false,
            }
        })
        .collect();
    let start = filtered_events.len().saturating_sub(visible);
    let items: Vec<ListItem> = filtered_events[start..]
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
                EventKind::BudgetWarning {
                    role,
                    dimension,
                    percent,
                    ..
                } => (
                    "$$",
                    "budget",
                    format!("{role} {dimension} {percent}% WARNING"),
                    YELLOW,
                ),
                EventKind::BudgetExhausted {
                    role, dimension, ..
                } => ("!!", "budget", format!("{role} {dimension} EXHAUSTED"), RED),
                EventKind::PolicyViolation { role, tool, reason } => (
                    "XX",
                    "policy",
                    format!("{role} blocked {tool}: {reason}"),
                    RED,
                ),
                EventKind::RoleChanged { from, to } => {
                    ("->", "role", format!("{from} -> {to}"), BLUE)
                }
                EventKind::ProfileChanged { from, to } => {
                    ("->", "profile", format!("{from} -> {to}"), BLUE)
                }
                EventKind::SloViolation {
                    slo_name, action, ..
                } => ("!!", "slo", format!("{slo_name} violated → {action}"), RED),
                EventKind::Anomaly {
                    metric,
                    deviation_factor,
                    ..
                } => (
                    "??",
                    "anomaly",
                    format!("{metric} {deviation_factor:.1}x StdDev"),
                    YELLOW,
                ),
                EventKind::VerificationWarning {
                    warning_kind,
                    detail,
                    ..
                } => (
                    "!?",
                    "verify",
                    format!(
                        "{warning_kind}: {}",
                        detail.chars().take(40).collect::<String>()
                    ),
                    YELLOW,
                ),
                EventKind::ThresholdAdapted { language, arm, .. } => (
                    "~>",
                    "adapt",
                    format!("{language}/{arm} threshold adapted"),
                    BLUE,
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
    if files.is_empty() {
        let msg = Paragraph::new("Waiting for file activity...")
            .style(Style::default().fg(MUTED))
            .block(block);
        f.render_widget(msg, area);
        return;
    }
    let max_access = files.first().map_or(1, |f| f.1.access_count).max(1);

    let visible = (area.height.saturating_sub(2)) as usize;
    let rows: Vec<Row> = files
        .iter()
        .take(visible)
        .map(|(path, heat)| {
            let short = path.rsplit('/').next().unwrap_or(path);
            let bar_len = (f64::from(heat.access_count) / f64::from(max_access) * 12.0) as usize;
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
            Span::styled(format!("({pct:.0}%)"), Style::default().fg(MUTED)),
        ])),
        chunks[0],
    );

    let ratio = (pct / 100.0).min(1.0);
    f.render_widget(
        Gauge::default()
            .ratio(ratio)
            .gauge_style(Style::default().fg(GREEN).bg(BG))
            .label(format!("{pct:.0}%")),
        chunks[1],
    );

    f.render_widget(Paragraph::new(""), chunks[2]);

    let cache_pct = state.cache_rate();
    f.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" Cache Hit Rate ", Style::default().fg(PURPLE)),
            Span::styled(format!("{cache_pct:.0}%"), Style::default().fg(MUTED)),
            Span::styled(
                format!(" ({}/{})", state.cache_hits, state.cache_reads),
                Style::default().fg(MUTED),
            ),
        ])),
        chunks[3],
    );

    let cache_ratio = (cache_pct / 100.0).min(1.0);
    f.render_widget(
        Gauge::default()
            .ratio(cache_ratio)
            .gauge_style(Style::default().fg(PURPLE).bg(BG))
            .label(format!("{cache_pct:.0}%")),
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
            "  q=quit Tab=focus 1-5=panel f=filter /=search",
            Style::default().fg(Color::Rgb(50, 50, 70)),
        )),
    ];

    let paragraph = Paragraph::new(lines).block(block);
    f.render_widget(paragraph, area);
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000_000_000 {
        format!("{:.2}T", n as f64 / 1_000_000_000_000.0)
    } else if n >= 1_000_000_000 {
        // 2 decimals at B-scale: a heavy user crosses 1B and the figure must
        // keep growing visibly instead of sticking at "1000.0M" / "1.0B".
        format!("{:.2}B", n as f64 / 1_000_000_000.0)
    } else if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        format!("{n}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_state() -> AppState {
        AppState {
            events: Vec::new(),
            total_saved: 0,
            total_original: 0,
            cache_hits: 0,
            cache_reads: 0,
            total_calls: 0,
            files: std::collections::HashMap::new(),
            gain_score: None,
            last_gain_refresh: Instant::now(),
            quit: false,
            focus: 0,
            filter: EventFilter::All,
            search_query: String::new(),
            search_active: false,
        }
    }

    #[test]
    fn format_tokens_scales_through_billions() {
        assert_eq!(format_tokens(512), "512");
        assert_eq!(format_tokens(1_500), "1.5K");
        assert_eq!(format_tokens(2_500_000), "2.5M");
        // Heavy users cross 1B — must read as B, not "1310.0M" or a frozen cap.
        assert_eq!(format_tokens(1_310_000_000), "1.31B");
        assert_eq!(format_tokens(2_000_000_000_000), "2.00T");
    }

    #[test]
    fn ingest_toolcall_with_path_populates_heatmap() {
        let mut s = mk_state();
        s.ingest(vec![LeanCtxEvent {
            id: 1,
            timestamp: "t".to_string(),
            kind: EventKind::ToolCall {
                tool: "ctx_read".to_string(),
                tokens_original: 100,
                tokens_saved: 80,
                mode: Some("full".to_string()),
                duration_ms: 1,
                path: Some("src/main.rs".to_string()),
            },
        }]);

        let entry = s.files.get("src/main.rs").expect("file entry missing");
        assert_eq!(entry.access_count, 1);
        assert_eq!(entry.tokens_saved, 80);
    }

    #[test]
    fn ingest_compression_counts_access_without_fake_tokens() {
        let mut s = mk_state();
        s.ingest(vec![LeanCtxEvent {
            id: 1,
            timestamp: "t".to_string(),
            kind: EventKind::Compression {
                path: "src/lib.rs".to_string(),
                before_lines: 100,
                after_lines: 10,
                strategy: "entropy".to_string(),
                kept_line_count: 10,
                removed_line_count: 90,
            },
        }]);

        let entry = s.files.get("src/lib.rs").expect("file entry missing");
        assert_eq!(entry.access_count, 1);
        assert_eq!(entry.tokens_saved, 0);
    }

    /// Renders the full observatory layout off-screen and verifies every panel
    /// is laid out without panicking. Run with `--nocapture` to eyeball the grid.
    #[test]
    fn dashboard_snapshot_renders_all_panels() {
        use ratatui::Terminal;
        use ratatui::backend::TestBackend;

        let mut state = mk_state();
        state.total_saved = 515_300_000;
        state.total_original = 752_000_000;
        state.total_calls = 22_599;
        state.ingest(vec![
            LeanCtxEvent {
                id: 1,
                timestamp: "2026-06-03T20:00".to_string(),
                kind: EventKind::ToolCall {
                    tool: "ctx_read".to_string(),
                    tokens_original: 4200,
                    tokens_saved: 3360,
                    mode: Some("map".to_string()),
                    duration_ms: 5,
                    path: Some("src/core/stats/format.rs".to_string()),
                },
            },
            LeanCtxEvent {
                id: 2,
                timestamp: "2026-06-03T20:01".to_string(),
                kind: EventKind::CacheHit {
                    path: "src/core/theme.rs".to_string(),
                    saved_tokens: 1200,
                },
            },
        ]);

        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|f| draw(f, &state))
            .expect("draw must not panic");

        let backend = terminal.backend();
        println!("{backend:?}");

        let text: String = backend
            .buffer()
            .content
            .iter()
            .map(ratatui::buffer::Cell::symbol)
            .collect();
        assert!(text.contains("LeanCTX"), "header brand missing from render");
        assert!(text.contains("Gain Score"), "gain score panel missing");
        assert!(text.contains("Heatmap"), "heatmap panel missing");
    }
}
