use std::collections::{HashMap, VecDeque};
use std::io;

use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use crossterm::ExecutableCommand;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Borders, List, ListItem, Paragraph, Tabs};
use ratatui::{Frame, Terminal};
use ratatui::backend::CrosstermBackend;
use time;
use tokio::sync::mpsc::UnboundedReceiver;

use llmux_server::app::TuiEvent;

const MAX_LOG_ENTRIES: usize = 500;

// ---------------------------------------------------------------------------
// TUI application state
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct LogEntry {
    timestamp: String,
    method: String,
    path: String,
    status: u16,
    latency_ms: i64,
    model: String,
}

#[derive(Clone)]
struct DispatchEntry {
    timestamp: String,
    line: String,
}

#[derive(Clone, Default)]
pub struct DashboardInfo {
    pub lan_ip: String,
    pub port: u16,
    pub db_path: String,
    pub db_ok: bool,
    pub master_key_ok: bool,
    pub active_accounts: usize,
    pub total_accounts: usize,
    pub api_keys: usize,
    pub aliases: usize,
    pub account_aliases: Vec<String>,
}

#[derive(Clone, Default)]
struct AccountState {
    last_model: String,
    last_url: String,
    last_error: Option<String>,
    request_count: u64,
    healthy: bool,
}

struct UiState {
    request_logs: VecDeque<LogEntry>,
    dispatch_logs: VecDeque<DispatchEntry>,
    dashboard_info: DashboardInfo,
    account_states: HashMap<String, AccountState>,
    active_tab: usize,
    request_scroll: usize,
    dispatch_scroll: usize,
    pinned_to_bottom: bool,
    traffic_focus: usize, // 0 = Requests top, 1 = Dispatch bottom
}

// ---------------------------------------------------------------------------
// TUI event loop
// ---------------------------------------------------------------------------

pub async fn run_tui(
    mut rx: UnboundedReceiver<TuiEvent>,
    dashboard_info: DashboardInfo,
) -> io::Result<()> {
    let mut backend = CrosstermBackend::new(io::stdout());
    backend.execute(EnterAlternateScreen)?;
    terminal::enable_raw_mode()?;

    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let mut account_states: HashMap<String, AccountState> = HashMap::new();
    for alias in &dashboard_info.account_aliases {
        account_states.insert(alias.clone(), AccountState {
            healthy: true,
            ..Default::default()
        });
    }

    let mut ui = UiState {
        request_logs: VecDeque::new(),
        dispatch_logs: VecDeque::new(),
        dashboard_info,
        account_states,
        active_tab: 0,
        request_scroll: 0,
        dispatch_scroll: 0,
        pinned_to_bottom: true,
        traffic_focus: 0,
    };

    loop {
        // Process incoming events (non-blocking)
        while let Ok(event) = rx.try_recv() {
            handle_event(&mut ui, event);
        }

        // Handle keyboard input
        if event::poll(std::time::Duration::from_millis(50))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == KeyEventKind::Release {
                    continue;
                }
                match key.code {
                    KeyCode::Esc | KeyCode::Char('q') => break,
                    KeyCode::Char('1') => { ui.active_tab = 0; ui.pinned_to_bottom = true; }
                    KeyCode::Char('2') => { ui.active_tab = 1; ui.pinned_to_bottom = true; }
                    KeyCode::Tab => { ui.active_tab = (ui.active_tab + 1) % 2; ui.pinned_to_bottom = true; }
                    KeyCode::Char(' ') if ui.active_tab == 1 => { ui.traffic_focus ^= 1; }
                    KeyCode::Up | KeyCode::Char('k') => { ui.pinned_to_bottom = false; scroll_up(&mut ui); }
                    KeyCode::Down | KeyCode::Char('j') => { scroll_down(&mut ui); }
                    KeyCode::PageUp => { ui.pinned_to_bottom = false; page_up(&mut ui); }
                    KeyCode::PageDown => { page_down(&mut ui); }
                    KeyCode::Home => { ui.pinned_to_bottom = false; scroll_home(&mut ui); }
                    KeyCode::End => { scroll_end(&mut ui); ui.pinned_to_bottom = true; }
                    KeyCode::Char('g') => { ui.pinned_to_bottom = false; scroll_home(&mut ui); }
                    KeyCode::Char('G') => { scroll_end(&mut ui); ui.pinned_to_bottom = true; }
                    _ => {}
                }
            }
        }

        terminal.draw(|f| render(f, &ui))?;
    }

    terminal.backend_mut().execute(LeaveAlternateScreen)?;
    terminal::disable_raw_mode()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Event handling
// ---------------------------------------------------------------------------

fn handle_event(ui: &mut UiState, event: TuiEvent) {
    match event {
        TuiEvent::Request { timestamp, method, path, status, latency_ms, model } => {
            ui.request_logs.push_back(LogEntry {
                timestamp, method, path, status, latency_ms, model,
            });
            if ui.request_logs.len() > MAX_LOG_ENTRIES {
                ui.request_logs.pop_front();
            }
            if ui.pinned_to_bottom {
                ui.request_scroll = ui.request_logs.len().saturating_sub(1);
            }
        }
        TuiEvent::Dispatch { timestamp, account, model, url, tag } => {
            let tag_str = tag.map(|t| format!(" [{}]", t)).unwrap_or_default();
            let line = format!("⚡{tag_str} {} → {} → {}", account, model, url);
            ui.dispatch_logs.push_back(DispatchEntry { timestamp, line });
            if ui.dispatch_logs.len() > MAX_LOG_ENTRIES {
                ui.dispatch_logs.pop_front();
            }
            let s = ui.account_states.entry(account.clone()).or_default();
            s.last_model = model;
            s.last_url = url;
            s.request_count += 1;
            s.healthy = true;
            s.last_error = None;
            if ui.pinned_to_bottom { ui.dispatch_scroll = ui.dispatch_logs.len().saturating_sub(1); }
        }
        TuiEvent::Retry { account, status, message } => {
            let ts = time::OffsetDateTime::now_local()
                .unwrap_or_else(|_| time::OffsetDateTime::now_utc())
                .format(&time::format_description::parse("[hour]:[minute]:[second]").unwrap())
                .unwrap_or_default();
            let line = format!("🔀 Account {} ({}): {}", account, status, message);
            ui.dispatch_logs.push_back(DispatchEntry { timestamp: ts, line });
            if ui.dispatch_logs.len() > MAX_LOG_ENTRIES {
                ui.dispatch_logs.pop_front();
            }
            if ui.pinned_to_bottom { ui.dispatch_scroll = ui.dispatch_logs.len().saturating_sub(1); }
            let s = ui.account_states.entry(account.clone()).or_default();
            s.healthy = false;
            s.last_error = Some(format!("{} {}", status, message));
        }
    }
}

// ---------------------------------------------------------------------------
// Scroll helpers
// ---------------------------------------------------------------------------

fn scroll_up(ui: &mut UiState) {
    if ui.active_tab == 1 {
        if ui.traffic_focus == 0 {
            ui.request_scroll = ui.request_scroll.saturating_sub(1);
        } else {
            ui.dispatch_scroll = ui.dispatch_scroll.saturating_sub(1);
        }
    }
}

fn scroll_down(ui: &mut UiState) {
    if ui.active_tab == 1 {
        if ui.traffic_focus == 0 {
            let max_r = ui.request_logs.len().saturating_sub(1);
            ui.request_scroll = (ui.request_scroll + 1).min(max_r);
            if ui.request_scroll >= max_r { ui.pinned_to_bottom = true; }
        } else {
            let max_d = ui.dispatch_logs.len().saturating_sub(1);
            ui.dispatch_scroll = (ui.dispatch_scroll + 1).min(max_d);
            if ui.dispatch_scroll >= max_d { ui.pinned_to_bottom = true; }
        }
    }
}

fn page_up(ui: &mut UiState) {
    if ui.active_tab == 1 {
        if ui.traffic_focus == 0 {
            ui.request_scroll = ui.request_scroll.saturating_sub(10);
        } else {
            ui.dispatch_scroll = ui.dispatch_scroll.saturating_sub(10);
        }
    }
}

fn page_down(ui: &mut UiState) {
    if ui.active_tab == 1 {
        if ui.traffic_focus == 0 {
            let max_r = ui.request_logs.len().saturating_sub(1);
            ui.request_scroll = (ui.request_scroll + 10).min(max_r);
            if ui.request_scroll >= max_r { ui.pinned_to_bottom = true; }
        } else {
            let max_d = ui.dispatch_logs.len().saturating_sub(1);
            ui.dispatch_scroll = (ui.dispatch_scroll + 10).min(max_d);
            if ui.dispatch_scroll >= max_d { ui.pinned_to_bottom = true; }
        }
    }
}

fn scroll_home(ui: &mut UiState) {
    if ui.active_tab == 1 {
        if ui.traffic_focus == 0 {
            ui.request_scroll = 0;
        } else {
            ui.dispatch_scroll = 0;
        }
    }
}

fn scroll_end(ui: &mut UiState) {
    if ui.active_tab == 1 {
        if ui.traffic_focus == 0 {
            ui.request_scroll = ui.request_logs.len().saturating_sub(1);
        } else {
            ui.dispatch_scroll = ui.dispatch_logs.len().saturating_sub(1);
        }
    }
}

// ---------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------

fn render(f: &mut Frame, ui: &UiState) {
    let area = f.area();

    let ver = env!("CARGO_PKG_VERSION");
    let banner = Paragraph::new(Line::from(Span::styled(
        format!("  Local AI Gateway & Multiplexer  ·  v{ver}"),
        Style::default().fg(Color::DarkGray),
    )))
    .alignment(Alignment::Center)
    .block(Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Thick)
        .border_style(Style::default().fg(Color::Cyan))
        .title(Line::from(Span::styled(" ⚡ LLMux ", Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD))))
        .title_alignment(Alignment::Center));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),  // banner
            Constraint::Length(2),  // tabs
            Constraint::Min(0),     // content — shrinks before header/tabs
            Constraint::Length(1),  // status bar
        ])
        .split(area);

    f.render_widget(banner, chunks[0]);
    render_tabs(f, chunks[1], ui.active_tab);
    match ui.active_tab {
        0 => render_dashboard(f, chunks[2], ui),
        1 => render_traffic(f, chunks[2], ui),
        _ => {}
    }
    render_status_bar(f, chunks[3], ui);
}

fn render_tabs(f: &mut Frame, area: Rect, active: usize) {
    let titles = vec![" Dashboard ", " Traffic "];
    let tab_widgets: Vec<Line> = titles
        .iter()
        .enumerate()
        .map(|(i, t)| {
            if i == active {
                Line::from(Span::styled(*t, Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD)))
            } else {
                Line::from(Span::styled(*t, Style::default().fg(Color::Gray)))
            }
        })
        .collect();

    let tabs = Tabs::new(tab_widgets)
        .block(Block::default())
        .highlight_style(Style::default().fg(Color::Yellow))
        .select(active)
        .divider(Span::raw("│"));

    f.render_widget(tabs, area);
}

fn render_dashboard(f: &mut Frame, area: Rect, ui: &UiState) {
    let info = &ui.dashboard_info;
    let ok = if info.db_ok && info.master_key_ok { "[OK]" } else { "[FAIL]" };

    let splits = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(10), // system info
            Constraint::Min(3),     // account status
        ])
        .split(area);

    // ── System info ──
    let srv = format!("http://{}:{}", info.lan_ip, info.port);
    let db  = format!("{}  {}", info.db_path, ok);
    let acc = format!("{} active / {} total", info.active_accounts, info.total_accounts);
    let keys = info.api_keys.to_string();
    let aliases = info.aliases.to_string();
    let port = info.port.to_string();

    let items = vec![
        dash("Server", &srv, Color::Green),
        dash("Database", &db, Color::Cyan),
        dash("Port", &port, Color::Cyan),
        ListItem::new(""),
        dash("Accounts", &acc, Color::White),
        dash("API Keys", &keys, Color::White),
        dash("Aliases", &aliases, Color::White),
    ];

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" System "));
    f.render_widget(list, splits[0]);

    // ── Account status ──
    let mut acc_items: Vec<ListItem> = Vec::new();
    for (alias, state) in &ui.account_states {
        let (icon, status) = if let Some(ref err) = state.last_error {
            ("🔴", (format!("ERR: {}", err), Color::Red))
        } else if state.last_model.is_empty() {
            ("⚪", ("idle".to_string(), Color::DarkGray))
        } else {
            ("🟢", (state.last_model.clone(), Color::Green))
        };
        let count_str = if state.request_count > 0 {
            format!("#{}", state.request_count)
        } else {
            "    ".to_string()
        };
        acc_items.push(ListItem::new(Line::from(vec![
            Span::styled(format!("  {} {:14}", icon, alias), Style::default().fg(Color::White)),
            Span::styled(format!("{:>5}  ", count_str), Style::default().fg(Color::DarkGray)),
            Span::styled(status.0, Style::default().fg(status.1)),
        ])));
    }

    let acc_list = List::new(acc_items)
        .block(Block::default().borders(Borders::ALL).title(" Accounts "));
    f.render_widget(acc_list, splits[1]);
}

fn dash(label: &str, value: &str, color: Color) -> ListItem<'static> {
    let line = Line::from(vec![
        Span::styled(format!("  {:12}  ", label), Style::default().fg(Color::Gray)),
        Span::styled(value.to_owned(), Style::default().fg(color)),
    ]);
    ListItem::new(line)
}

fn render_traffic(f: &mut Frame, area: Rect, ui: &UiState) {
    let splits = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Ratio(1, 2),
            Constraint::Ratio(1, 2),
        ])
        .split(area);

    render_request_logs(f, splits[0], ui, ui.traffic_focus == 0);
    render_dispatch_logs(f, splits[1], ui, ui.traffic_focus == 1);
}

fn render_request_logs(f: &mut Frame, area: Rect, ui: &UiState, focused: bool) {
    let logs: Vec<&LogEntry> = ui.request_logs.iter().collect();
    let vis_height = area.height.saturating_sub(2) as usize;
    let end = (ui.request_scroll + vis_height).min(logs.len());
    let start = ui.request_scroll.min(logs.len().saturating_sub(1));
    let window = &logs[start..end];

    let items: Vec<ListItem> = window
        .iter()
        .map(|entry| {
            let (status_icon, status_color) = if entry.status < 300 {
                ("✅", Color::Green)
            } else if entry.status < 500 {
                ("⚠️", Color::Yellow)
            } else {
                ("❌", Color::Red)
            };
            let latency = if entry.latency_ms > 0 {
                format!("{}ms", entry.latency_ms)
            } else {
                "--".to_string()
            };

            let mut spans = vec![
                Span::styled(entry.timestamp.clone(), Style::default().fg(Color::DarkGray)),
                Span::raw("  "),
                Span::styled(status_icon, Style::default().fg(status_color)),
                Span::raw(" "),
                Span::styled(format!("{}", entry.status), Style::default().fg(status_color)),
                Span::raw("  "),
                Span::styled(format!("{:>4}", latency), Style::default().fg(Color::Cyan)),
                Span::raw("  "),
            ];
            if !entry.model.is_empty() {
                spans.push(Span::styled(format!("{} ", entry.model), Style::default().fg(Color::Magenta)));
            }
            spans.push(Span::styled(format!("{} {}", entry.method, entry.path), Style::default().fg(Color::White)));
            ListItem::new(Line::from(spans))
        })
        .collect();

    let title_style = if focused { Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD) } else { Style::default() };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Requests ").title_style(title_style));

    f.render_widget(list, area);
}

fn render_dispatch_logs(f: &mut Frame, area: Rect, ui: &UiState, focused: bool) {
    let logs: Vec<&DispatchEntry> = ui.dispatch_logs.iter().collect();
    let vis_height = area.height.saturating_sub(2) as usize;
    let end = (ui.dispatch_scroll + vis_height).min(logs.len());
    let start = ui.dispatch_scroll.min(logs.len().saturating_sub(1));
    let window = &logs[start..end];

    let items: Vec<ListItem> = window
        .iter()
        .map(|entry| {
            let color = if entry.line.contains("⚡") {
                Color::Cyan
            } else if entry.line.contains("🔀") {
                Color::Yellow
            } else {
                Color::White
            };
            ListItem::new(Line::from(vec![
                Span::styled(entry.timestamp.clone(), Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(entry.line.clone(), Style::default().fg(color)),
            ]))
        })
        .collect();

    let title_style = if focused { Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD) } else { Style::default() };
    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Dispatch ").title_style(title_style));

    f.render_widget(list, area);
}

fn render_status_bar(f: &mut Frame, area: Rect, ui: &UiState) {
    let hint = match ui.active_tab {
        0 => "Tab/1-2:switch  q:quit".to_string(),
        1 => {
            let focus = if ui.traffic_focus == 0 { "[▼Requests]" } else { "[▼Dispatch]" };
            format!("{focus} Space:switch  ↑↓/jk:scroll  gg/G:jump  End:latest  |  Reqs:{}  Disp:{}", ui.request_logs.len(), ui.dispatch_logs.len())
        }
        _ => String::new(),
    };
    let status = Line::from(Span::styled(hint, Style::default().fg(Color::DarkGray)));
    f.render_widget(Paragraph::new(status), area);
}
