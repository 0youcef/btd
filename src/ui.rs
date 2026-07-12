use crate::aggregator::SharedState;
use crate::model::{Category, DirRow, MapCell};
use crossterm::event::{self, Event, KeyCode, KeyEventKind};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::Terminal;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const TICK: Duration = Duration::from_millis(16);

#[derive(PartialEq, Eq, Clone, Copy)]
enum ViewMode {
    Browser,
    Map,
}

fn category_color(cat: Category) -> Color {
    match cat {
        Category::Exclusive => Color::Green,
        Category::Shared => Color::Yellow,
        Category::Metadata => Color::Cyan,
        Category::System => Color::Magenta,
        Category::Unallocated => Color::DarkGray,
    }
}

pub fn run(state: Arc<SharedState>, shutdown: Arc<AtomicBool>, paused: Arc<AtomicBool>) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = run_loop(&mut terminal, state, shutdown.clone(), paused);

    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    shutdown.store(true, Ordering::Relaxed);

    result
}

struct UiState {
    current_path: Vec<String>,
    selected: usize,
    last_draws_seen: u64,
    last_rate_sample: Instant,
    draws_per_sec: f64,
    mode: ViewMode,
}

impl UiState {
    fn new() -> Self {
        Self {
            current_path: Vec::new(),
            selected: 0,
            last_draws_seen: 0,
            last_rate_sample: Instant::now(),
            draws_per_sec: 0.0,
            mode: ViewMode::Browser,
        }
    }
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    state: Arc<SharedState>,
    shutdown: Arc<AtomicBool>,
    paused: Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut ui = UiState::new();

    loop {
        if shutdown.load(Ordering::Relaxed) {
            return Ok(());
        }

        let total_draws = state.total_draws.load(Ordering::Relaxed);
        update_rate(&mut ui, total_draws);

        let (rows, hidden) = read_current_level(&state, &ui.current_path, total_draws);
        if ui.selected >= rows.len() && !rows.is_empty() {
            ui.selected = rows.len() - 1;
        }

        let is_paused = paused.load(Ordering::Relaxed);

        terminal.draw(|f| {
            draw(
                f,
                &ui,
                &rows,
                hidden,
                total_draws,
                state.universe_bytes,
                is_paused,
                &state,
            )
        })?;

        if event::poll(TICK)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }
                match key.code {
                    KeyCode::Char('q') => {
                        shutdown.store(true, Ordering::Relaxed);
                        return Ok(());
                    }
                    KeyCode::Esc => {

                        if ui.mode == ViewMode::Map {
                            ui.mode = ViewMode::Browser;
                        } else {
                            shutdown.store(true, Ordering::Relaxed);
                            return Ok(());
                        }
                    }
                    KeyCode::Char('m') => {
                        ui.mode = match ui.mode {
                            ViewMode::Browser => ViewMode::Map,
                            ViewMode::Map => ViewMode::Browser,
                        };
                    }
                    KeyCode::Char('p') => {
                        paused.fetch_xor(true, Ordering::Relaxed);
                    }
                    KeyCode::Up | KeyCode::Char('k') if ui.mode == ViewMode::Browser => {
                        ui.selected = ui.selected.saturating_sub(1);
                    }
                    KeyCode::Down | KeyCode::Char('j') if ui.mode == ViewMode::Browser => {
                        if ui.selected + 1 < rows.len() {
                            ui.selected += 1;
                        }
                    }
                    KeyCode::Enter | KeyCode::Right | KeyCode::Char('l')
                        if ui.mode == ViewMode::Browser =>
                    {
                        if let Some(row) = rows.get(ui.selected) {
                            if row.has_children {
                                ui.current_path.push(row.name.clone());
                                ui.selected = 0;
                            }
                        }
                    }
                    KeyCode::Backspace | KeyCode::Left | KeyCode::Char('h')
                        if ui.mode == ViewMode::Browser =>
                    {
                        if ui.current_path.pop().is_some() {
                            ui.selected = 0;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

fn update_rate(ui: &mut UiState, total_draws: u64) {
    let elapsed = ui.last_rate_sample.elapsed();
    if elapsed >= Duration::from_millis(500) {
        let delta = total_draws.saturating_sub(ui.last_draws_seen);
        ui.draws_per_sec = delta as f64 / elapsed.as_secs_f64();
        ui.last_draws_seen = total_draws;
        ui.last_rate_sample = Instant::now();
    }
}

const MAX_DISPLAYED_ROWS: usize = 300;

fn read_current_level(
    state: &SharedState,
    path: &[String],
    total_draws: u64,
) -> (Vec<DirRow>, Option<(usize, f64)>) {
    let tree = state.tree.lock().unwrap();

    let mut node = &*tree;
    for component in path {
        match node.children.get(component) {
            Some(child) => node = child,
            None => return (Vec::new(), None),
        }
    }

    let total_children = node.children.len();

    let mut rows: Vec<DirRow> = node
        .children
        .iter()
        .map(|(name, child)| {
            let mut bytes_by_category = std::collections::HashMap::new();
            let mut samples_by_category = std::collections::HashMap::new();
            for cat in Category::ALL {
                bytes_by_category.insert(
                    cat,
                    hits_to_bytes(child.hits_for(cat), total_draws, state.universe_bytes),
                );
                samples_by_category.insert(cat, child.count_for(cat));
            }
            DirRow {
                name: name.clone(),
                total_bytes: hits_to_bytes(child.total_hits(), total_draws, state.universe_bytes),
                total_samples: child.total_count(),
                bytes_by_category,
                samples_by_category,
                has_children: !child.children.is_empty(),
                inode: child.inode,
            }
        })
        .collect();

    drop(tree);

    if rows.len() <= MAX_DISPLAYED_ROWS {
        rows.sort_by(|a, b| b.total_bytes.partial_cmp(&a.total_bytes).unwrap());
        return (rows, None);
    }

    let level_total: f64 = rows.iter().map(|r| r.total_bytes).sum();
    rows.select_nth_unstable_by(MAX_DISPLAYED_ROWS - 1, |a, b| {
        b.total_bytes.partial_cmp(&a.total_bytes).unwrap()
    });
    rows.truncate(MAX_DISPLAYED_ROWS);
    rows.sort_by(|a, b| b.total_bytes.partial_cmp(&a.total_bytes).unwrap());

    let shown_total: f64 = rows.iter().map(|r| r.total_bytes).sum();
    let hidden = (
        total_children - MAX_DISPLAYED_ROWS,
        (level_total - shown_total).max(0.0),
    );

    (rows, Some(hidden))
}

fn hits_to_bytes(hits: f64, total_draws: u64, universe_bytes: u64) -> f64 {
    if total_draws == 0 {
        return 0.0;
    }
    hits / total_draws as f64 * universe_bytes as f64
}

fn draw(
    f: &mut ratatui::Frame,
    ui: &UiState,
    rows: &[DirRow],
    hidden: Option<(usize, f64)>,
    total_draws: u64,
    universe_bytes: u64,
    is_paused: bool,
    state: &SharedState,
) {
    let area = f.area();
    let outer = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(1),
        ])
        .split(area);

    draw_header(f, outer[0], ui, total_draws, universe_bytes, is_paused);

    match ui.mode {
        ViewMode::Browser => {
            let body = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Percentage(65), Constraint::Percentage(35)])
                .split(outer[1]);

            draw_list(f, body[0], ui, rows, hidden, universe_bytes);
            draw_info_panel(f, body[1], rows.get(ui.selected), total_draws, universe_bytes);
        }
        ViewMode::Map => {
            draw_map(f, outer[1], state, total_draws);
        }
    }

    draw_footer(f, outer[2], is_paused, ui.mode);
}

fn draw_header(
    f: &mut ratatui::Frame,
    area: Rect,
    ui: &UiState,
    total_draws: u64,
    universe_bytes: u64,
    is_paused: bool,
) {

    let confidence = if total_draws > 0 {
        100.0 / (total_draws as f64).sqrt()
    } else {
        100.0
    };

    let breadcrumb = if ui.current_path.is_empty() {
        "/".to_string()
    } else {
        format!("/{}", ui.current_path.join("/"))
    };

    let status = if is_paused {
        Span::styled(" PAUSED ", Style::default().bg(Color::Red).fg(Color::White).add_modifier(Modifier::BOLD))
    } else {
        Span::styled(" sampling ", Style::default().bg(Color::Green).fg(Color::Black))
    };

    let text = vec![Line::from(vec![
        status,
        Span::raw("  "),
        Span::styled(breadcrumb, Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("   "),
        Span::raw(format!("universe: {}", human_bytes(universe_bytes as f64))),
        Span::raw("   "),
        Span::raw(format!("draws: {total_draws}")),
        Span::raw(format!(" ({:.0}/s)", ui.draws_per_sec)),
        Span::raw("   "),
        Span::raw(format!("~±{confidence:.1}%")),
    ])];

    f.render_widget(
        Paragraph::new(text).block(Block::default().borders(Borders::ALL).title("btd")),
        area,
    );
}

fn draw_list(
    f: &mut ratatui::Frame,
    area: Rect,
    ui: &UiState,
    rows: &[DirRow],
    hidden: Option<(usize, f64)>,
    universe_bytes: u64,
) {
    let bar_width: usize = 20;

    let items: Vec<ListItem> = rows
        .iter()
        .map(|row| {
            let bar = composition_bar(row, universe_bytes, bar_width);
            let pct = if universe_bytes > 0 {
                row.total_bytes / universe_bytes as f64 * 100.0
            } else {
                0.0
            };

            let name_color = row
                .dominant_category()
                .map(category_color)
                .unwrap_or(Color::White);

            let mut spans = vec![
                Span::raw(format!("{:>10}  ", human_bytes(row.total_bytes))),
            ];
            spans.extend(bar);
            spans.push(Span::raw(format!(
                " {:>5.1}%  n={:<5}  ",
                pct, row.total_samples
            )));
            spans.push(Span::styled(
                format!("{}{}", row.name, if row.has_children { "/" } else { "" }),
                Style::default().fg(name_color),
            ));

            ListItem::new(Line::from(spans))
        })
        .collect();

    let mut list_state = ListState::default();
    if !rows.is_empty() {
        list_state.select(Some(ui.selected));
    }

    let title = match hidden {
        Some((count, bytes)) => format!(
            "size (live estimate) — top {} shown, +{} more (~{} not shown)  [green=exclusive yellow=shared cyan=metadata magenta=system gray=unallocated]",
            rows.len(), count, human_bytes(bytes)
        ),
        None => "size (live estimate)  [green=exclusive yellow=shared cyan=metadata magenta=system gray=unallocated]".to_string(),
    };

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(title))
        .highlight_style(Style::default().add_modifier(Modifier::REVERSED));

    f.render_stateful_widget(list, area, &mut list_state);
}

fn composition_bar(row: &DirRow, universe_bytes: u64, width: usize) -> Vec<Span<'static>> {
    if universe_bytes == 0 || row.total_bytes <= 0.0 {
        return vec![Span::raw("-".repeat(width))];
    }

    let mut spans = Vec::new();
    let mut used = 0usize;

    for cat in Category::ALL {
        let frac = row.bytes_for(cat) / universe_bytes as f64;
        let mut chars = (frac * width as f64).round() as usize;
        chars = chars.min(width - used);
        if chars > 0 {
            spans.push(Span::styled(
                "#".repeat(chars),
                Style::default().fg(category_color(cat)),
            ));
            used += chars;
        }
    }

    if used < width {
        spans.push(Span::styled(
            "-".repeat(width - used),
            Style::default().fg(Color::DarkGray),
        ));
    }

    spans
}

fn draw_info_panel(
    f: &mut ratatui::Frame,
    area: Rect,
    selected: Option<&DirRow>,
    total_draws: u64,
    universe_bytes: u64,
) {
    let mut lines: Vec<Line> = Vec::new();

    match selected {
        None => {
            lines.push(Line::from("No entry selected."));
        }
        Some(row) => {
            lines.push(Line::from(Span::styled(
                row.name.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));

            lines.push(Line::from(format!(
                "total: {}  ({} samples)",
                human_bytes(row.total_bytes),
                row.total_samples
            )));

            if let Some(inode) = row.inode {
                lines.push(Line::from(format!("inode: {inode}")));
            }

            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "breakdown",
                Style::default().add_modifier(Modifier::UNDERLINED),
            )));

            for cat in Category::ALL {
                let bytes = row.bytes_for(cat);
                let n = row.samples_for(cat);
                if n == 0 {
                    continue;
                }
                lines.push(Line::from(vec![
                    Span::styled("● ", Style::default().fg(category_color(cat))),
                    Span::raw(format!(
                        "{:<11} {:>10}  n={}",
                        cat.label(),
                        human_bytes(bytes),
                        n
                    )),
                ]));
            }

            if let Some(dom) = row.dominant_category() {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    dom.label(),
                    Style::default().fg(category_color(dom)).add_modifier(Modifier::BOLD),
                )));
                lines.push(Line::from(dom.explanation()));
            }

            if row.name.starts_with("<subvol") || row.name.starts_with("<unresolved-subvol") {
                lines.push(Line::from(""));
                lines.push(Line::from(
                    "This is a subvolume boundary. Data below it may also be reachable from other subvolumes (snapshots) — see 'shared' above.",
                ));
            }
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(format!(
        "(overall: {total_draws} draws, universe {})",
        human_bytes(universe_bytes as f64)
    )));

    f.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: true })
            .block(Block::default().borders(Borders::ALL).title("info")),
        area,
    );
}

fn draw_map(f: &mut ratatui::Frame, area: Rect, state: &SharedState, _total_draws: u64) {
    let map = state.disk_map.lock().unwrap();

    let global_max_count = map
        .iter()
        .flat_map(|d| d.cells.iter())
        .map(|c| c.count)
        .max()
        .unwrap_or(0);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(Span::styled(
        "Disk map — physical layout, one row per device",
        Style::default().add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    if map.is_empty() {
        lines.push(Line::from(
            "No device size information available for the disk map.",
        ));
    }

    let display_width = (area.width as usize).saturating_sub(2).max(1);

    for dev in map.iter() {
        lines.push(Line::from(Span::styled(
            format!(
                "devid {}  {}  ({})",
                dev.devid,
                dev.path,
                human_bytes(dev.total_bytes as f64)
            ),
            Style::default().add_modifier(Modifier::UNDERLINED),
        )));

        let mut spans = Vec::with_capacity(display_width);
        for i in 0..display_width {
            let src_start = i * dev.cells.len() / display_width;
            let src_end = ((i + 1) * dev.cells.len() / display_width)
                .max(src_start + 1)
                .min(dev.cells.len());
            let (glyph, color) = aggregate_cells(&dev.cells[src_start..src_end], global_max_count);
            spans.push(Span::styled(glyph.to_string(), Style::default().fg(color)));
        }
        lines.push(Line::from(spans));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(
        "shade = sample density (space = no samples yet, █ = most-sampled region); color = dominant category there:",
    ));
    lines.push(Line::from(vec![
        Span::styled("■ ", Style::default().fg(Color::Green)),
        Span::raw("exclusive   "),
        Span::styled("■ ", Style::default().fg(Color::Yellow)),
        Span::raw("shared   "),
        Span::styled("■ ", Style::default().fg(Color::Cyan)),
        Span::raw("metadata   "),
        Span::styled("■ ", Style::default().fg(Color::Magenta)),
        Span::raw("system   "),
        Span::styled("■ ", Style::default().fg(Color::DarkGray)),
        Span::raw("unallocated"),
    ]));

    drop(map);

    f.render_widget(
        Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("physical disk map")),
        area,
    );
}

fn aggregate_cells(cells: &[MapCell], global_max_count: u64) -> (char, Color) {
    let mut combined_hits: std::collections::HashMap<Category, f64> = std::collections::HashMap::new();
    let mut combined_count = 0u64;

    for c in cells {
        combined_count += c.count;
        for cat in Category::ALL {
            *combined_hits.entry(cat).or_insert(0.0) += *c.hits.get(&cat).unwrap_or(&0.0);
        }
    }

    if combined_count == 0 {
        return (' ', Color::DarkGray);
    }

    let dominant = Category::ALL
        .into_iter()
        .max_by(|a, b| {
            combined_hits
                .get(a)
                .unwrap_or(&0.0)
                .partial_cmp(combined_hits.get(b).unwrap_or(&0.0))
                .unwrap()
        })
        .unwrap();

    let density = if global_max_count == 0 {
        0.0
    } else {
        combined_count as f64 / global_max_count as f64
    };
    let glyph = if density > 0.75 {
        '█'
    } else if density > 0.5 {
        '▓'
    } else if density > 0.25 {
        '▒'
    } else {
        '░'
    };

    (glyph, category_color(dominant))
}

fn draw_footer(f: &mut ratatui::Frame, area: Rect, is_paused: bool, mode: ViewMode) {
    let pause_hint = if is_paused { "p resume" } else { "p pause" };
    let text = match mode {
        ViewMode::Browser => Line::from(format!(
            "↑/↓ move   →/Enter descend   ←/Backspace up   m map   {pause_hint}   q quit"
        )),
        ViewMode::Map => Line::from(format!(
            "m/Esc back to browser   {pause_hint}   q quit"
        )),
    };
    f.render_widget(Paragraph::new(text), area);
}

fn human_bytes(bytes: f64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut val = bytes;
    let mut unit = 0;
    while val >= 1024.0 && unit < UNITS.len() - 1 {
        val /= 1024.0;
        unit += 1;
    }
    format!("{val:.1}{}", UNITS[unit])
}
