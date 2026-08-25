//! Application state, event loop, and data loading.

use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

use rustemd::control::{Control, SocketClient, TimerInfo, UnitFileInfo, UnitStatus, UnitSummary};

use crate::render;

pub(crate) type Terminal = ratatui::Terminal<ratatui::backend::CrosstermBackend<std::io::Stdout>>;

/// Detect and connect to a running daemon, or fail with a clear message.
pub fn connect(user: bool) -> Result<SocketClient, String> {
    let ctl = SocketClient::for_mode(user).map_err(|e| e.to_string())?;
    // Cheapest read as a liveness probe: if the socket is absent/unreachable
    // this errors, which we surface as "no daemon running".
    ctl.get_default().map_err(|e| {
        let mode = if user { "user" } else { "system" };
        let flag = if user { " --user" } else { "" };
        format!("no {mode} manager reachable — start one with `rustemd daemon{flag}` ({e})")
    })?;
    Ok(ctl)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tab {
    Units,
    Services,
    Timers,
    Files,
}

impl Tab {
    pub(crate) const ALL: [Tab; 4] = [Tab::Units, Tab::Services, Tab::Timers, Tab::Files];

    pub(crate) fn label(self) -> &'static str {
        match self {
            Tab::Units => "Units",
            Tab::Services => "Services",
            Tab::Timers => "Timers",
            Tab::Files => "Unit files",
        }
    }

    pub(crate) fn index(self) -> usize {
        match self {
            Tab::Units => 0,
            Tab::Services => 1,
            Tab::Timers => 2,
            Tab::Files => 3,
        }
    }
}

enum Action {
    Start,
    Stop,
    Restart,
    Reload,
    Enable,
    Disable,
    DaemonReload,
}

pub(crate) struct App {
    ctl: SocketClient,
    user: bool,
    tab: Tab,
    selected: usize,
    filter: String,
    searching: bool,

    units: Vec<UnitSummary>,
    services: Vec<UnitSummary>,
    timers: Vec<TimerInfo>,
    files: Vec<UnitFileInfo>,

    detail: Vec<UnitStatus>,
    default_target: String,

    running: bool,
    last_refresh: Instant,
    status: Option<(String, bool)>,
}

impl App {
    pub(crate) fn new(ctl: SocketClient, user: bool) -> Self {
        App {
            ctl,
            user,
            tab: Tab::Units,
            selected: 0,
            filter: String::new(),
            searching: false,
            units: Vec::new(),
            services: Vec::new(),
            timers: Vec::new(),
            files: Vec::new(),
            detail: Vec::new(),
            default_target: String::new(),
            running: true,
            // Start stale so the first frame triggers a refresh immediately.
            last_refresh: Instant::now() - Duration::from_secs(10),
            status: None,
        }
    }

    pub(crate) fn run(&mut self, terminal: &mut Terminal) -> Result<(), String> {
        self.refresh()?;
        while self.running {
            terminal
                .draw(|f| render::draw(self, f))
                .map_err(|e| e.to_string())?;
            if event::poll(Duration::from_millis(250)).map_err(|e| e.to_string())?
                && let Event::Key(key) = event::read().map_err(|e| e.to_string())?
                && key.kind == KeyEventKind::Press
            {
                self.handle_key(key)?;
            }
            if self.last_refresh.elapsed() >= Duration::from_secs(2) {
                self.refresh()?;
            }
        }
        Ok(())
    }

    // ---- data ---------------------------------------------------------------

    fn refresh(&mut self) -> Result<(), String> {
        self.units = self.ctl.list_units(&[], None).map_err(|e| e.to_string())?;
        self.services = self
            .ctl
            .list_units(&["service"], None)
            .map_err(|e| e.to_string())?;
        self.timers = self.ctl.list_timers().map_err(|e| e.to_string())?;
        self.files = self.ctl.list_unit_files().map_err(|e| e.to_string())?;
        self.default_target = self.ctl.get_default().map_err(|e| e.to_string())?;

        let len = self.visible_indices().len();
        if len > 0 && self.selected >= len {
            self.selected = len - 1;
        }
        self.refresh_detail();
        self.last_refresh = Instant::now();
        Ok(())
    }

    fn refresh_detail(&mut self) {
        self.detail = match self.selected_target() {
            Some(name) => self.ctl.status(&[name.as_str()]).unwrap_or_default(),
            None => Vec::new(),
        };
    }

    /// Unit name whose live status fills the detail pane.
    fn selected_target(&self) -> Option<String> {
        let i = *self.visible_indices().get(self.selected)?;
        match self.tab {
            Tab::Units => self.units.get(i).map(|u| u.unit.clone()),
            Tab::Services => self.services.get(i).map(|u| u.unit.clone()),
            Tab::Timers => self.timers.get(i).map(|t| t.activates.clone()),
            Tab::Files => self.files.get(i).map(|f| f.file.clone()),
        }
    }

    /// Unit name that start/stop/restart/enable/disable operate on.
    fn action_name(&self) -> Option<String> {
        let i = *self.visible_indices().get(self.selected)?;
        match self.tab {
            Tab::Units => self.units.get(i).map(|u| u.unit.clone()),
            Tab::Services => self.services.get(i).map(|u| u.unit.clone()),
            Tab::Timers => self.timers.get(i).map(|t| t.unit.clone()),
            Tab::Files => self.files.get(i).map(|f| f.file.clone()),
        }
    }

    pub(crate) fn visible_indices(&self) -> Vec<usize> {
        let q = self.filter.to_lowercase();
        let hit = |s: &str| q.is_empty() || s.to_lowercase().contains(&q);
        match self.tab {
            Tab::Units => filter_idx(&self.units, |u| hit(&u.unit)),
            Tab::Services => filter_idx(&self.services, |u| hit(&u.unit)),
            Tab::Timers => filter_idx(&self.timers, |t| hit(&t.unit)),
            Tab::Files => filter_idx(&self.files, |f| hit(&f.file)),
        }
    }

    // ---- actions ------------------------------------------------------------

    fn run_action(&mut self, action: Action) {
        let name = self.action_name();
        let result = match action {
            Action::DaemonReload => self
                .ctl
                .reload_daemon()
                .map(|_| "daemon reloaded".to_string()),
            _ => {
                let Some(n) = name else {
                    self.status = Some(("nothing selected".to_string(), true));
                    return;
                };
                let units: &[&str] = &[n.as_str()];
                match action {
                    Action::Start => self.ctl.start(units).map(|_| format!("started {n}")),
                    Action::Stop => self.ctl.stop(units).map(|_| format!("stopped {n}")),
                    Action::Restart => self.ctl.restart(units).map(|_| format!("restarted {n}")),
                    Action::Reload => self.ctl.reload(units).map(|_| format!("reloaded {n}")),
                    Action::Enable => self
                        .ctl
                        .enable(units)
                        .map(|m| format!("enabled {n}: {}", m.join(", "))),
                    Action::Disable => self
                        .ctl
                        .disable(units)
                        .map(|m| format!("disabled {n}: {}", m.join(", "))),
                    Action::DaemonReload => unreachable!(),
                }
            }
        };
        match result {
            Ok(msg) => {
                self.status = Some((msg, false));
                let _ = self.refresh();
            }
            Err(e) => self.status = Some((e.to_string(), true)),
        }
    }

    // ---- navigation ---------------------------------------------------------

    fn next_tab(&mut self) {
        self.tab = Tab::ALL[(self.tab.index() + 1) % Tab::ALL.len()];
        self.after_tab_change();
    }

    fn prev_tab(&mut self) {
        self.tab = Tab::ALL[(self.tab.index() + Tab::ALL.len() - 1) % Tab::ALL.len()];
        self.after_tab_change();
    }

    fn after_tab_change(&mut self) {
        self.selected = 0;
        self.filter.clear();
        self.searching = false;
        self.refresh_detail();
    }

    fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
        self.refresh_detail();
    }

    fn move_down(&mut self) {
        let len = self.visible_indices().len();
        if len > 0 {
            self.selected = (self.selected + 1).min(len - 1);
        }
        self.refresh_detail();
    }

    // ---- events -------------------------------------------------------------

    fn handle_key(&mut self, key: KeyEvent) -> Result<(), String> {
        // Ctrl+Q quits from any state (search, etc.).
        if key.code == KeyCode::Char('q') && key.modifiers == KeyModifiers::CONTROL {
            self.running = false;
            return Ok(());
        }
        if self.searching {
            return self.handle_search_key(key);
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.running = false,
            KeyCode::Tab => self.next_tab(),
            KeyCode::BackTab => self.prev_tab(),
            KeyCode::Up | KeyCode::Char('k') => self.move_up(),
            KeyCode::Down | KeyCode::Char('j') => self.move_down(),
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(10);
                self.refresh_detail();
            }
            KeyCode::PageDown => {
                let len = self.visible_indices().len();
                if len > 0 {
                    self.selected = (self.selected + 10).min(len - 1);
                }
                self.refresh_detail();
            }
            KeyCode::Home => {
                self.selected = 0;
                self.refresh_detail();
            }
            KeyCode::End => {
                let len = self.visible_indices().len();
                if len > 0 {
                    self.selected = len - 1;
                }
                self.refresh_detail();
            }
            KeyCode::Char('/') => {
                self.searching = true;
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Char('f') => {
                let _ = self.refresh();
                self.status = Some(("refreshed".to_string(), false));
            }
            KeyCode::Char('1') => self.switch_to(Tab::Units),
            KeyCode::Char('2') => self.switch_to(Tab::Services),
            KeyCode::Char('3') => self.switch_to(Tab::Timers),
            KeyCode::Char('4') => self.switch_to(Tab::Files),
            KeyCode::Char('s') => self.run_action(Action::Start),
            KeyCode::Char('x') => self.run_action(Action::Stop),
            KeyCode::Char('r') => self.run_action(Action::Restart),
            KeyCode::Char('l') => self.run_action(Action::Reload),
            KeyCode::Char('e') => self.run_action(Action::Enable),
            KeyCode::Char('d') => self.run_action(Action::Disable),
            KeyCode::Char('R') => self.run_action(Action::DaemonReload),
            _ => {}
        }
        Ok(())
    }

    fn switch_to(&mut self, tab: Tab) {
        if self.tab != tab {
            self.tab = tab;
            self.after_tab_change();
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) -> Result<(), String> {
        match key.code {
            KeyCode::Esc => {
                self.searching = false;
                self.filter.clear();
            }
            KeyCode::Enter => self.searching = false,
            KeyCode::Char(c) => {
                self.filter.push(c);
                self.selected = 0;
            }
            KeyCode::Backspace => {
                if self.filter.is_empty() {
                    self.searching = false;
                } else {
                    self.filter.pop();
                }
                self.selected = 0;
            }
            _ => {}
        }
        Ok(())
    }

    // ---- accessors used by render ------------------------------------------

    pub(crate) fn user(&self) -> bool {
        self.user
    }
    pub(crate) fn tab(&self) -> Tab {
        self.tab
    }
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }
    pub(crate) fn filter(&self) -> &str {
        &self.filter
    }
    pub(crate) fn searching(&self) -> bool {
        self.searching
    }
    pub(crate) fn units(&self) -> &[UnitSummary] {
        &self.units
    }
    pub(crate) fn services(&self) -> &[UnitSummary] {
        &self.services
    }
    pub(crate) fn timers(&self) -> &[TimerInfo] {
        &self.timers
    }
    pub(crate) fn files(&self) -> &[UnitFileInfo] {
        &self.files
    }
    pub(crate) fn detail(&self) -> &[UnitStatus] {
        &self.detail
    }
    pub(crate) fn default_target(&self) -> &str {
        &self.default_target
    }
    pub(crate) fn status(&self) -> Option<&(String, bool)> {
        self.status.as_ref()
    }
}

fn filter_idx<T>(items: &[T], pred: impl Fn(&T) -> bool) -> Vec<usize> {
    items
        .iter()
        .enumerate()
        .filter(|(_, it)| pred(it))
        .map(|(i, _)| i)
        .collect()
}
