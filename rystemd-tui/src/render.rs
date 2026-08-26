//! Frame rendering: title bar, tab bar, list + detail panes, status line.

use ratatui::Frame;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, List, ListItem, ListState, Paragraph, Tabs, Wrap,
};

use rystemd::control::{TimerInfo, UnitFileInfo, UnitSummary};
use rystemd::timespan::{fmt_ago, fmt_left};

use crate::app::{App, Tab};
use crate::theme::{
    self, C_ACCENT, C_ACTIVE, C_BORDER, C_BORDER_ACTIVE, C_DIM, C_ERROR, C_INFO, C_WARN,
};

pub(crate) fn draw(app: &App, f: &mut Frame) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // title
            Constraint::Length(1), // tabs
            Constraint::Min(0),    // main
            Constraint::Length(2), // status
        ])
        .split(f.area());

    draw_title(app, f, chunks[0]);
    draw_tabs(app, f, chunks[1]);
    draw_main(app, f, chunks[2]);
    draw_status(app, f, chunks[3]);
}

fn draw_title(app: &App, f: &mut Frame, area: Rect) {
    let mode = if app.user() { "user" } else { "system" };
    let mut spans = vec![
        Span::styled(
            "rystemd-tui",
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::raw(format!(" · {mode} manager")),
    ];
    if !app.default_target().is_empty() {
        spans.push(Span::raw(format!(" · default {}", app.default_target())));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_tabs(app: &App, f: &mut Frame, area: Rect) {
    let titles: Vec<Line> = Tab::ALL.iter().map(|t| Line::from(t.label())).collect();
    let tabs = Tabs::new(titles)
        .select(app.tab().index())
        .highlight_style(theme::highlight_style())
        .divider(Span::raw(" │ "));
    f.render_widget(tabs, area);
}

fn draw_main(app: &App, f: &mut Frame, area: Rect) {
    let panes = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(area);
    draw_list(app, f, panes[0]);
    draw_detail(app, f, panes[1]);
}

fn draw_list(app: &App, f: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(Span::styled(
            format!(" {} ", app.tab().label()),
            Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(C_BORDER_ACTIVE));

    let items: Vec<ListItem> = app
        .visible_indices()
        .iter()
        .map(|&i| row_for(app, i))
        .collect();

    let list = List::new(items)
        .block(block)
        .highlight_style(theme::highlight_style());

    let mut state = ListState::default();
    state.select(Some(app.selected()));
    f.render_stateful_widget(list, area, &mut state);
}

fn row_for(app: &App, i: usize) -> ListItem<'static> {
    match app.tab() {
        Tab::Units => unit_row(&app.units()[i]),
        Tab::Services => unit_row(&app.services()[i]),
        Tab::Timers => timer_row(&app.timers()[i]),
        Tab::Files => file_row(&app.files()[i]),
    }
}

fn unit_row(u: &UnitSummary) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::raw(" "),
        Span::styled("●", Style::default().fg(theme::state_color(&u.active))),
        Span::raw(" "),
        Span::styled(u.unit.clone(), Style::default().fg(C_ACCENT)),
        Span::raw(" "),
        Span::styled(format!("({})", u.sub), Style::default().fg(C_DIM)),
        Span::raw("  "),
        Span::styled(u.description.clone(), Style::default().fg(C_DIM)),
    ]))
}

fn timer_row(t: &TimerInfo) -> ListItem<'static> {
    let next = t
        .next_left
        .filter(|d| *d >= 0)
        .map(|d| fmt_left(std::time::Duration::from_secs(d as u64)))
        .unwrap_or_else(|| "-".to_string());
    ListItem::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(format!("{next:<12}"), Style::default().fg(C_WARN)),
        Span::styled(t.unit.clone(), Style::default().fg(C_ACCENT)),
        Span::raw("  "),
        Span::styled(format!("→ {}", t.activates), Style::default().fg(C_DIM)),
    ]))
}

fn file_row(f: &UnitFileInfo) -> ListItem<'static> {
    ListItem::new(Line::from(vec![
        Span::raw(" "),
        Span::styled(
            format!("{:<12}", f.state),
            Style::default().fg(theme::enabled_color(&f.state)),
        ),
        Span::styled(f.file.clone(), Style::default().fg(C_ACCENT)),
        Span::raw("  "),
        Span::styled(f.path.clone(), Style::default().fg(C_DIM)),
    ]))
}

fn draw_detail(app: &App, f: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(Span::styled(" Status ", Style::default().fg(C_ACCENT)))
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(Style::default().fg(C_BORDER));

    let mut lines: Vec<Line> = Vec::new();
    if let Some(u) = app.detail().first() {
        lines.push(Line::from(vec![
            Span::styled(
                &u.name,
                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" — "),
            Span::styled(&u.description, Style::default().fg(C_DIM)),
        ]));
        lines.push(Line::from(""));
        lines.push(Line::from(vec![
            Span::raw(" Loaded: "),
            Span::styled(&u.load, Style::default().fg(C_WARN)),
            Span::raw(match &u.path {
                Some(p) => format!(" ({p})"),
                None => String::new(),
            }),
        ]));
        lines.push(Line::from(vec![
            Span::raw(" Active: "),
            Span::styled(
                &u.active,
                Style::default().fg(theme::state_color(&u.active)),
            ),
            Span::raw(format!(" ({})", u.sub)),
            Span::raw(match u.active_enter {
                Some(t) => format!(" since {}", fmt_epoch(t)),
                None => String::new(),
            }),
        ]));
        if let Some(pid) = u.main_pid {
            lines.push(Line::from(vec![
                Span::raw("Main PID: "),
                Span::styled(pid.to_string(), Style::default().fg(C_INFO)),
            ]));
        }
        if !u.enabled.is_empty() {
            lines.push(Line::from(vec![
                Span::raw(" Enabled: "),
                Span::styled(&u.enabled, Style::default().fg(C_WARN)),
            ]));
        }
        if !u.log.is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Logs:",
                Style::default().fg(C_ACCENT).add_modifier(Modifier::BOLD),
            )));
            for l in u.log.iter().take(12) {
                lines.push(Line::from(Span::styled(
                    format!("  {l}"),
                    Style::default().fg(C_DIM),
                )));
            }
        }
    } else {
        lines.push(Line::from(Span::styled(
            "(nothing selected)",
            Style::default().fg(C_DIM),
        )));
    }

    let para = Paragraph::new(lines).block(block).wrap(Wrap { trim: true });
    f.render_widget(para, area);
}

fn fmt_epoch(secs: u64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    fmt_ago(std::time::Duration::from_secs(now.saturating_sub(secs)))
}

fn draw_status(app: &App, f: &mut Frame, area: Rect) {
    let hints = "q/Esc quit · Tab tabs · ↑↓/jk move · / filter · f refresh · s start · x stop · r restart · e enable · d disable · R reload-daemon";
    let mut spans = vec![Span::styled(hints, Style::default().fg(C_DIM))];
    if let Some((msg, is_error)) = app.status() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            msg.clone(),
            Style::default().fg(if *is_error { C_ERROR } else { C_ACTIVE }),
        ));
    } else if app.searching() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!("/{}", app.filter()),
            Style::default().fg(C_WARN),
        ));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}
