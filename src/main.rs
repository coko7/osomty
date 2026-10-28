mod canvas;
mod map;
mod mvt;
mod search;
mod tiles;

use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

use anyhow::Result;
use ratatui::crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::crossterm::execute;
use ratatui::layout::{Alignment, Constraint, Layout, Position, Rect};
use ratatui::style::{Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::canvas::Canvas;
use crate::map::{Category, MAX_VIEW_ZOOM, MIN_ZOOM, Marker, Poi, View, to_world};
use crate::search::Place;
use crate::tiles::TileManager;

const ATTRIBUTION: &str = "© OpenStreetMap contributors · OpenFreeMap ";
const PANEL_WIDTH: u16 = 44;
/// Zoom change per wheel event (terminals often send several per notch).
const WHEEL_ZOOM_STEP: f64 = 0.1;
const KEY_ZOOM_STEP: f64 = 0.5;
/// How far the zoom target may run ahead of the current zoom.
const MAX_PENDING_ZOOM: f64 = 1.5;
/// Exponential easing rate (1/s) of animated zoom.
const ZOOM_EASE: f64 = 12.0;
const FRAME: Duration = Duration::from_millis(16);

/// Animated zoom towards `target`, keeping the pixel at (`px`,`py`) from the centre fixed.
#[derive(Clone, Copy)]
struct ZoomAnim {
    target: f64,
    px: f64,
    py: f64,
}

#[derive(PartialEq, Eq, Clone, Copy)]
enum Focus {
    Map,
    Search,
    Panel,
}

#[derive(Default)]
struct SearchState {
    input: String,
    searched: String,
    results: Vec<Place>,
    selected: usize,
    status: String,
    pending: Option<Receiver<Result<Vec<Place>, String>>>,
    /// Jump straight to the first result (used for the command-line query).
    auto_jump: bool,
}

struct App {
    view: View,
    zoom_anim: Option<ZoomAnim>,
    tiles: TileManager,
    filter: Option<Category>,
    show_panel: bool,
    show_help: bool,
    focus: Focus,
    panel: ListState,
    pois: Vec<Poi>,
    marker: Option<Marker>,
    search: SearchState,
    map_area: Rect,
    drag: Option<(u16, u16)>,
    quit: bool,
}

impl App {
    fn new() -> Self {
        App {
            view: View::new(30.0, 10.0, 1.3),
            zoom_anim: None,
            tiles: TileManager::new(6),
            filter: None,
            show_panel: false,
            show_help: false,
            focus: Focus::Map,
            panel: ListState::default(),
            pois: Vec::new(),
            marker: None,
            search: SearchState::default(),
            map_area: Rect::default(),
            drag: None,
            quit: false,
        }
    }

    /// Queue a smooth zoom by `delta` around the pixel (`px`,`py`) from the map centre.
    fn zoom_by(&mut self, delta: f64, px: f64, py: f64) {
        let from = self.zoom_anim.map_or(self.view.zoom, |a| a.target);
        let now = self.view.zoom;
        let target = (from + delta)
            .clamp(now - MAX_PENDING_ZOOM, now + MAX_PENDING_ZOOM)
            .clamp(MIN_ZOOM, MAX_VIEW_ZOOM);
        self.zoom_anim = Some(ZoomAnim { target, px, py });
    }

    /// Advance the zoom animation by `dt`. Returns true if the view changed.
    fn step_zoom(&mut self, dt: Duration) -> bool {
        let Some(anim) = self.zoom_anim else {
            return false;
        };
        let remaining = anim.target - self.view.zoom;
        let delta = if remaining.abs() < 0.01 {
            self.zoom_anim = None;
            remaining
        } else {
            remaining * (1.0 - (-dt.as_secs_f64() * ZOOM_EASE).exp())
        };
        self.view.zoom_around(delta, anim.px, anim.py);
        true
    }

    fn set_view(&mut self, view: View) {
        self.view = view;
        self.zoom_anim = None;
    }

    fn start_search(&mut self) {
        let query = self.search.input.trim().to_owned();
        if query.is_empty() {
            return;
        }
        let (tx, rx) = channel();
        self.search.searched = self.search.input.clone();
        self.search.status = format!("Searching for “{query}”…");
        self.search.results.clear();
        self.search.selected = 0;
        self.search.pending = Some(rx);
        std::thread::spawn(move || {
            let _ = tx.send(search::search(&query).map_err(|e| format!("{e:#}")));
        });
    }

    /// Returns true when a pending search finished.
    fn poll_search(&mut self) -> bool {
        let Some(rx) = &self.search.pending else {
            return false;
        };
        let Ok(res) = rx.try_recv() else {
            return false;
        };
        self.search.pending = None;
        match res {
            Ok(results) if results.is_empty() => self.search.status = "No results".into(),
            Ok(results) => {
                self.search.status.clear();
                self.search.results = results;
                if std::mem::take(&mut self.search.auto_jump) {
                    self.jump_to_result();
                }
            }
            Err(e) => self.search.status = format!("Search failed: {e}"),
        }
        true
    }

    fn jump_to_result(&mut self) {
        let Some(place) = self.search.results.get(self.search.selected).cloned() else {
            return;
        };
        self.set_view(View::new(place.lat, place.lon, place.zoom));
        let (x, y) = to_world(place.lat, place.lon);
        let label = place.name.split(',').next().unwrap_or(&place.name).trim();
        self.marker = Some(Marker {
            x,
            y,
            label: label.to_owned(),
        });
        self.focus = Focus::Map;
    }

    fn selected_poi(&self) -> Option<&Poi> {
        (self.focus == Focus::Panel)
            .then(|| self.panel.selected().and_then(|i| self.pois.get(i)))
            .flatten()
    }

    fn cycle_filter(&mut self, forward: bool) {
        let all = Category::ALL;
        let idx = self.filter.and_then(|f| all.iter().position(|&c| c == f));
        self.filter = match (idx, forward) {
            (None, true) => Some(all[0]),
            (None, false) => Some(all[all.len() - 1]),
            (Some(i), true) => all.get(i + 1).copied(),
            (Some(0), false) => None,
            (Some(i), false) => Some(all[i - 1]),
        };
        self.panel.select(Some(0));
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            self.quit = true;
            return;
        }
        if self.show_help {
            self.show_help = false;
            return;
        }
        match self.focus {
            Focus::Search => self.handle_search_key(key),
            Focus::Panel => self.handle_panel_key(key),
            Focus::Map => self.handle_map_key(key),
        }
    }

    fn handle_search_key(&mut self, key: KeyEvent) {
        let n = self.search.results.len();
        match key.code {
            KeyCode::Esc => self.focus = Focus::Map,
            KeyCode::Enter => {
                if n > 0 && self.search.input == self.search.searched {
                    self.jump_to_result();
                } else {
                    self.start_search();
                }
            }
            KeyCode::Down | KeyCode::Tab if n > 0 => {
                self.search.selected = (self.search.selected + 1) % n;
            }
            KeyCode::Up | KeyCode::BackTab if n > 0 => {
                self.search.selected = (self.search.selected + n - 1) % n;
            }
            KeyCode::Backspace => {
                self.search.input.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.search.input.clear();
            }
            KeyCode::Char(c) => self.search.input.push(c),
            _ => {}
        }
    }

    fn handle_panel_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Esc | KeyCode::Tab => self.focus = Focus::Map,
            KeyCode::Down | KeyCode::Char('j') => self.panel.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.panel.select_previous(),
            KeyCode::PageDown => self.panel.scroll_down_by(10),
            KeyCode::PageUp => self.panel.scroll_up_by(10),
            KeyCode::Home | KeyCode::Char('g') => self.panel.select_first(),
            KeyCode::Enter => {
                if let Some(poi) = self.selected_poi().cloned() {
                    self.set_view(View {
                        x: poi.x,
                        y: poi.y,
                        zoom: self.view.zoom,
                    });
                    if self.view.zoom < 17.0 {
                        self.zoom_by(17.0 - self.view.zoom, 0.0, 0.0);
                    }
                    self.marker = Some(Marker {
                        x: poi.x,
                        y: poi.y,
                        label: poi.name,
                    });
                    self.panel.select(Some(0));
                }
            }
            KeyCode::Char('f') => self.cycle_filter(true),
            KeyCode::Char('F') => self.cycle_filter(false),
            KeyCode::Char('q') => self.quit = true,
            _ => {}
        }
    }

    fn handle_map_key(&mut self, key: KeyEvent) {
        let step_x = self.map_area.width as f64 * 2.0 / 6.0;
        let step_y = self.map_area.height as f64 * 4.0 / 6.0;
        match key.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Esc => self.marker = None,
            KeyCode::Left | KeyCode::Char('h') => self.view.pan_px(-step_x, 0.0),
            KeyCode::Right | KeyCode::Char('l') => self.view.pan_px(step_x, 0.0),
            KeyCode::Up | KeyCode::Char('k') => self.view.pan_px(0.0, -step_y),
            KeyCode::Down | KeyCode::Char('j') => self.view.pan_px(0.0, step_y),
            KeyCode::Char('+' | '=' | 'a') => self.zoom_by(KEY_ZOOM_STEP, 0.0, 0.0),
            KeyCode::Char('-' | '_' | 'z') => self.zoom_by(-KEY_ZOOM_STEP, 0.0, 0.0),
            KeyCode::Char('/' | 's') => self.focus = Focus::Search,
            KeyCode::Char('p') => self.show_panel = !self.show_panel,
            KeyCode::Tab => {
                self.show_panel = true;
                self.focus = Focus::Panel;
                if self.panel.selected().is_none() {
                    self.panel.select(Some(0));
                }
            }
            KeyCode::Char('f') => self.cycle_filter(true),
            KeyCode::Char('F') => self.cycle_filter(false),
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('r') => self.set_view(View::new(30.0, 10.0, 1.3)),
            _ => {}
        }
    }

    fn handle_mouse(&mut self, m: MouseEvent) {
        let pos = Position::new(m.column, m.row);
        if !self.map_area.contains(pos) {
            if self.show_panel {
                match m.kind {
                    MouseEventKind::ScrollDown => self.panel.scroll_down_by(3),
                    MouseEventKind::ScrollUp => self.panel.scroll_up_by(3),
                    _ => {}
                }
            }
            return;
        }
        let area = self.map_area;
        // Offset of the cursor from the map centre, in braille pixels.
        let px = (m.column - area.x) as f64 * 2.0 + 1.0 - area.width as f64;
        let py = (m.row - area.y) as f64 * 4.0 + 2.0 - area.height as f64 * 2.0;
        match m.kind {
            MouseEventKind::ScrollUp => self.zoom_by(WHEEL_ZOOM_STEP, px, py),
            MouseEventKind::ScrollDown => self.zoom_by(-WHEEL_ZOOM_STEP, px, py),
            MouseEventKind::Down(MouseButton::Left) => self.drag = Some((m.column, m.row)),
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some((c, r)) = self.drag {
                    let dx = (c as f64 - m.column as f64) * 2.0;
                    let dy = (r as f64 - m.row as f64) * 4.0;
                    self.view.pan_px(dx, dy);
                }
                self.drag = Some((m.column, m.row));
            }
            MouseEventKind::Up(_) => self.drag = None,
            _ => {}
        }
    }
}

fn main() -> Result<()> {
    let mut app = App::new();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.iter().any(|a| a == "-h" || a == "--help") {
        println!("Usage: osomty [LAT LON [ZOOM] | PLACE NAME]\n\nBrowse OpenStreetMap in the terminal. Press ? in the app for keys.");
        return Ok(());
    }
    let nums: Vec<f64> = args.iter().filter_map(|a| a.trim_end_matches(',').parse().ok()).collect();
    if !args.is_empty() && nums.len() == args.len() && (2..=3).contains(&nums.len()) {
        app.view = View::new(nums[0], nums[1], nums.get(2).copied().unwrap_or(16.0));
    } else if !args.is_empty() {
        app.search.input = args.join(" ");
        app.search.auto_jump = true;
        app.start_search();
    }

    let mut terminal = ratatui::init();
    execute!(std::io::stdout(), EnableMouseCapture)?;
    let result = run(&mut terminal, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

fn run(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let mut dirty = true;
    let mut last = Instant::now();
    while !app.quit {
        let now = Instant::now();
        dirty |= app.step_zoom(now - last);
        last = now;
        if dirty {
            terminal.draw(|f| ui(f, app))?;
            dirty = false;
        }
        let timeout = if app.zoom_anim.is_some() {
            FRAME
        } else {
            Duration::from_millis(50)
        };
        if event::poll(timeout)? {
            // Drain everything queued so fast scrolling/dragging renders once.
            loop {
                match event::read()? {
                    Event::Key(k) => app.handle_key(k),
                    Event::Mouse(m) => app.handle_mouse(m),
                    _ => {}
                }
                if !event::poll(Duration::ZERO)? {
                    break;
                }
            }
            dirty = true;
        }
        dirty |= app.tiles.poll();
        dirty |= app.poll_search();
    }
    Ok(())
}

fn ui(f: &mut Frame, app: &mut App) {
    let [main, status] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(f.area());
    let (map_area, panel_area) = if app.show_panel && main.width > PANEL_WIDTH + 20 {
        let [m, p] = Layout::horizontal([Constraint::Min(20), Constraint::Length(PANEL_WIDTH)])
            .areas(main);
        (m, Some(p))
    } else {
        (main, None)
    };
    app.map_area = map_area;

    let mut markers = Vec::new();
    if let Some(m) = &app.marker {
        markers.push(Marker {
            x: m.x,
            y: m.y,
            label: m.label.clone(),
        });
    }
    if let Some(p) = app.selected_poi() {
        markers.push(Marker {
            x: p.x,
            y: p.y,
            label: p.name.clone(),
        });
    }
    let mut canvas = Canvas::new(map_area.width, map_area.height);
    app.pois = map::draw(&mut canvas, &app.view, &mut app.tiles, app.filter, &markers);
    canvas.render(map_area, f.buffer_mut());

    if let Some(area) = panel_area {
        render_panel(f, app, area);
    }
    render_status(f, app, status);
    if app.focus == Focus::Search {
        render_search(f, app, map_area);
    }
    if app.show_help {
        render_help(f, map_area);
    }
}

fn render_panel(f: &mut Frame, app: &mut App, area: Rect) {
    let filter = app.filter.map_or("all".into(), |c| c.name().to_string());
    let title = format!(" Nearby ({}) · {filter} ", app.pois.len());
    let focused = app.focus == Focus::Panel;
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(title)
        .border_style(if focused { Style::new().cyan() } else { Style::new().dark_gray() });

    let items: Vec<ListItem> = if app.pois.is_empty() {
        let hint = if app.view.zoom < 14.0 {
            "Zoom in (z ≥ 14) to list places"
        } else {
            "No named places here"
        };
        vec![ListItem::new(Line::from(hint).dark_gray())]
    } else {
        app.pois
            .iter()
            .map(|p| {
                ListItem::new(Line::from(vec![
                    Span::styled(format!("{} ", p.category.symbol()), Style::new().fg(p.category.color())),
                    Span::raw(p.name.clone()),
                    Span::styled(format!("  {}", p.kind), Style::new().dark_gray()),
                ]))
            })
            .collect()
    };
    if let Some(sel) = app.panel.selected()
        && sel >= app.pois.len()
    {
        app.panel.select((!app.pois.is_empty()).then(|| app.pois.len() - 1));
    }
    let list = List::new(items)
        .block(block)
        .highlight_style(Style::new().add_modifier(Modifier::REVERSED));
    if focused {
        f.render_stateful_widget(list, area, &mut app.panel);
    } else {
        let mut state = ListState::default().with_offset(app.panel.offset());
        f.render_stateful_widget(list, area, &mut state);
    }
}

fn render_status(f: &mut Frame, app: &App, area: Rect) {
    let (lat, lon) = app.view.lat_lon();
    let mut spans = vec![
        Span::styled(format!(" {lat:.5}, {lon:.5} "), Style::new().bold()),
        Span::raw(format!(" z{:.1} ", app.view.zoom)),
    ];
    let loading = app.tiles.loading();
    if loading > 0 {
        spans.push(Span::styled(format!(" ⟳ {loading} tiles "), Style::new().yellow()));
    } else if let Some(err) = &app.tiles.last_error {
        spans.push(Span::styled(format!(" ⚠ {err} "), Style::new().red()));
    }
    if let Some(c) = app.filter {
        spans.push(Span::styled(format!(" filter: {} ", c.name()), Style::new().fg(c.color())));
    }
    spans.push(Span::styled(
        " ?:help /:search p:places f:filter q:quit",
        Style::new().dark_gray(),
    ));
    let [left, right] = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(ATTRIBUTION.chars().count() as u16),
    ])
    .areas(area);
    f.render_widget(Paragraph::new(Line::from(spans)).on_black(), left);
    f.render_widget(
        Paragraph::new(ATTRIBUTION).dark_gray().on_black().alignment(Alignment::Right),
        right,
    );
}

fn render_search(f: &mut Frame, app: &App, area: Rect) {
    let s = &app.search;
    let width = area.width.saturating_sub(4).min(80);
    let rows = if s.results.is_empty() { 1 } else { s.results.len() as u16 };
    let height = (rows + 4).min(area.height);
    let popup = Rect::new(area.x + (area.width - width) / 2, area.y + 1, width, height);
    f.render_widget(Clear, popup);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .title(" Search place or “lat, lon” ")
        .title_bottom(Line::from(" Enter: search/go · ↑↓: select · Esc: close ").right_aligned())
        .border_style(Style::new().cyan());
    let inner = block.inner(popup);
    f.render_widget(block, popup);
    if inner.height == 0 {
        return;
    }

    let input = Line::from(vec![Span::raw("› ").cyan(), Span::raw(s.input.as_str())]);
    f.render_widget(Paragraph::new(input), Rect { height: 1, ..inner });
    f.set_cursor_position(Position::new(
        (inner.x + 2 + s.input.chars().count() as u16).min(inner.right().saturating_sub(1)),
        inner.y,
    ));

    let body = Rect {
        y: inner.y + 2,
        height: inner.height.saturating_sub(2),
        ..inner
    };
    if s.results.is_empty() {
        f.render_widget(Paragraph::new(s.status.as_str()).dark_gray().wrap(Wrap { trim: true }), body);
        return;
    }
    let items: Vec<ListItem> = s
        .results
        .iter()
        .map(|p| {
            ListItem::new(Line::from(vec![
                Span::raw(p.name.clone()),
                Span::styled(format!("  {}", p.kind), Style::new().dark_gray()),
            ]))
        })
        .collect();
    let mut state = ListState::default().with_selected(Some(s.selected));
    f.render_stateful_widget(
        List::new(items).highlight_style(Style::new().reversed()),
        body,
        &mut state,
    );
}

fn render_help(f: &mut Frame, area: Rect) {
    let key = |k: &str, d: &str| {
        Line::from(vec![
            Span::styled(format!("{k:>16}  "), Style::new().cyan()),
            Span::raw(d.to_string()),
        ])
    };
    let mut lines = vec![
        key("←↑↓→ / hjkl", "pan"),
        key("+ - / a z", "zoom in / out"),
        key("mouse", "drag to pan, wheel to zoom"),
        key("/ or s", "search a place (Nominatim)"),
        key("p", "toggle nearby places panel"),
        key("Tab", "focus panel (Enter: go to place)"),
        key("f / F", "filter place category"),
        key("Esc", "clear marker / close"),
        key("r", "reset to world view"),
        key("q", "quit"),
        Line::raw(""),
        Line::from("Shop & restaurant names appear from zoom 15.").dark_gray(),
        Line::raw(""),
    ];
    for c in Category::ALL {
        lines.push(Line::from(vec![
            Span::styled(format!("{:>16}  ", c.symbol()), Style::new().fg(c.color())),
            Span::raw(c.name()),
        ]));
    }
    let height = (lines.len() as u16 + 2).min(area.height);
    let width = 52.min(area.width);
    let popup = Rect::new(
        area.x + (area.width - width) / 2,
        area.y + (area.height - height) / 2,
        width,
        height,
    );
    f.render_widget(Clear, popup);
    f.render_widget(
        Paragraph::new(lines).block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .title(" osomty — keys ")
                .border_style(Style::new().cyan()),
        ),
        popup,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Renders a few views to text. Needs network: `cargo test -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn render_snapshots() {
        let mut app = App::new();
        for (lat, lon, zoom) in [(30.0, 10.0, 1.3), (48.8566, 2.3522, 11.0), (48.8530, 2.3499, 17.0)] {
            app.view = View::new(lat, lon, zoom);
            let mut term = Terminal::new(TestBackend::new(120, 40)).unwrap();
            let start = std::time::Instant::now();
            loop {
                term.draw(|f| ui(f, &mut app)).unwrap();
                app.tiles.poll();
                if app.tiles.loading() == 0 && start.elapsed() > Duration::from_millis(300) {
                    break;
                }
                assert!(start.elapsed() < Duration::from_secs(60), "tiles did not load");
                std::thread::sleep(Duration::from_millis(100));
            }
            let t = std::time::Instant::now();
            term.draw(|f| ui(f, &mut app)).unwrap();
            let buf = term.backend().buffer();
            let filled = buf.content.iter().filter(|c| c.bg != ratatui::style::Color::Reset).count();
            println!(
                "--- z{zoom} rendered in {:?}, {} pois, {filled} filled cells",
                t.elapsed(),
                app.pois.len()
            );
            for y in 0..buf.area.height {
                let line: String = (0..buf.area.width).map(|x| buf[(x, y)].symbol()).collect();
                println!("{line}");
            }
        }
    }
}
