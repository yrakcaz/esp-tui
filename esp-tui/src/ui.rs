use std::time::Duration;

use esp_agent_msg as agent_msg;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap,
};
use ratatui::Frame;

use crate::app::{App, Pane};
use crate::filter;
use crate::flash;

const INSPECTOR_BAR_W: usize = 10;

/// Renders the full TUI to the given frame.
///
/// # Arguments
///
/// * `frame` - The ratatui frame to render into.
/// * `app` - Shared reference to the current application state.
pub(crate) fn draw(frame: &mut Frame, app: &App) {
    let outer = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(0),
        Constraint::Length(3),
    ])
    .split(frame.area());

    let main = Layout::horizontal([
        Constraint::Percentage(app.monitor_pct()),
        Constraint::Percentage(100 - app.monitor_pct()),
    ])
    .split(outer[1]);

    render_menu_bar(frame, outer[0], app);
    render_monitor(frame, main[0], app, app.focused_pane() == Pane::Monitor);
    render_inspector(frame, main[1], app, app.focused_pane() == Pane::Inspector);
    render_status_bar(frame, outer[2], app, app.focused_pane() == Pane::Status);

    if app.is_quit_confirm_open() {
        render_quit_confirm_popup(frame, frame.area());
    } else if app.is_erase_confirm_open() {
        render_erase_confirm_popup(frame, frame.area());
    } else if app.is_elf_selector_open() {
        render_elf_selector_popup(frame, frame.area(), app);
    } else if let Some(sel) = app.port_selector() {
        render_port_selector(frame, frame.area(), sel);
    } else if app.filter().is_popup_open() {
        render_filter_popup(frame, frame.area(), app);
    }
}

fn render_menu_bar(frame: &mut Frame, area: Rect, app: &App) {
    let port_name = app.port_name();
    let port_label: std::borrow::Cow<str> =
        port_name.map_or("none".into(), std::borrow::Cow::Borrowed);
    let port_color = if port_name.is_some() {
        Color::Green
    } else {
        Color::Red
    };
    let right_text = format!("Port: {port_label}");

    let left = Line::from(vec![
        hint("[C]onnect"),
        Span::raw("  "),
        hint("[D]isconnect"),
        Span::raw("  "),
        hint("[F]lash"),
        Span::raw("  "),
        hint("[E]rase"),
        Span::raw("  "),
        hint("[R]eset"),
        Span::raw("  "),
        hint("[Q]uit"),
    ]);

    let right_len = u16::try_from(right_text.len()).unwrap_or(u16::MAX);
    let right =
        Line::from(Span::styled(right_text, Style::default().fg(port_color)));
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_len)])
            .areas(area);

    frame.render_widget(
        Paragraph::new(truncate_line_spans(left, usize::from(left_area.width))),
        left_area,
    );
    frame.render_widget(
        Paragraph::new(right).alignment(Alignment::Right),
        right_area,
    );
}

fn focused_border(is_focused: bool) -> Style {
    if is_focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default()
    }
}

fn scroll_footer(
    w: usize,
    is_scrolled: bool,
    scroll_hint: &str,
    nav_hint: &str,
) -> Line<'static> {
    const BADGE: &str = " SCROLL ";
    if is_scrolled {
        let tail = truncate_line(scroll_hint, w.saturating_sub(BADGE.len()));
        Line::from(vec![
            Span::styled(
                BADGE,
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            ),
            Span::styled(tail, Style::default().fg(Color::Yellow)),
        ])
    } else {
        Line::from(Span::styled(
            truncate_line(nav_hint, w),
            Style::default().fg(Color::DarkGray),
        ))
    }
}

fn hint(text: &'static str) -> Span<'static> {
    Span::styled(
        text,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
}

fn render_monitor(frame: &mut Frame, area: Rect, app: &App, is_focused: bool) {
    let block = Block::default()
        .title(clip_title(" Serial Monitor ", area.width))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused_border(is_focused));

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let filter = app.filter();
    let hidden_level_count = filter::State::levels()
        .iter()
        .filter(|&&l| filter.is_level_hidden(l))
        .count();
    let hidden_tag_count = filter
        .known_tags()
        .iter()
        .filter(|t| filter.is_tag_hidden(t))
        .count();
    let search_query = filter.search_query();
    let show_filter_bar =
        hidden_level_count > 0 || hidden_tag_count > 0 || !search_query.is_empty();

    let [content_area, filter_bar_area, footer_area] = Layout::vertical([
        Constraint::Min(0),
        Constraint::Length(u16::from(show_filter_bar)),
        Constraint::Length(1),
    ])
    .areas(inner);

    let height = content_area.height as usize;
    let entries = app.visible_entries(height);

    let text: ratatui::text::Text = entries
        .iter()
        .map(|e| {
            if e.tag().is_empty() {
                Line::from(Span::styled(
                    e.message(),
                    Style::default().fg(Color::DarkGray),
                ))
            } else {
                Line::from(vec![
                    Span::styled(
                        format!("[{}]", e.level().label()),
                        Style::default()
                            .fg(e.level().color())
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("{}: ", e.tag()),
                        Style::default().fg(Color::DarkGray),
                    ),
                    Span::raw(e.message()),
                ])
            }
        })
        .collect();

    frame.render_widget(
        Paragraph::new(text).wrap(Wrap { trim: false }),
        content_area,
    );

    if show_filter_bar {
        render_filter_bar(
            frame,
            filter_bar_area,
            hidden_level_count,
            hidden_tag_count,
            search_query,
        );
    }

    if is_focused {
        let w = usize::from(footer_area.width);
        let footer = scroll_footer(
            w,
            app.scroll() > 0,
            "  q/Esc to follow live",
            "[↑/↓  PgUp/PgDn] scroll  [^L] clear  [^F] filter  [^←/→] resize  [Tab] focus",
        );
        frame.render_widget(Paragraph::new(footer), footer_area);
    }
}

fn render_filter_bar(
    frame: &mut Frame,
    area: Rect,
    hidden_level_count: usize,
    hidden_tag_count: usize,
    search_query: &str,
) {
    let hidden_text = match (hidden_level_count, hidden_tag_count) {
        (0, 0) => None,
        (l, 0) => Some(format!("{l} level{} hidden", if l == 1 { "" } else { "s" })),
        (0, t) => Some(format!("{t} tag{} hidden", if t == 1 { "" } else { "s" })),
        (l, t) => Some(format!(
            "{l} level{}, {t} tag{} hidden",
            if l == 1 { "" } else { "s" },
            if t == 1 { "" } else { "s" },
        )),
    };
    let search_text =
        (!search_query.is_empty()).then(|| format!("Search: \"{search_query}\""));
    let w = usize::from(area.width);
    let line = match (hidden_text, search_text) {
        (Some(h), Some(s)) => {
            truncate_line(format!("  Active filters: {h}  •  {s}"), w)
        }
        (Some(h), None) => truncate_line(format!("  Active filters: {h}"), w),
        (None, Some(s)) => truncate_line(format!("  {s}"), w),
        (None, None) => String::new(),
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            line,
            Style::default().fg(Color::Yellow),
        ))),
        area,
    );
}

fn board_info_lines(app: &App, label: Style, col_width: usize) -> Vec<Line<'_>> {
    if let Some(info) = app.device_info() {
        vec![
            truncate_line_spans(
                Line::from(vec![
                    Span::styled("Board  ", label),
                    Span::raw(info.chip()),
                ]),
                col_width,
            ),
            truncate_line_spans(
                Line::from(vec![
                    Span::styled("Flash  ", label),
                    Span::raw(info.flash_size()),
                ]),
                col_width,
            ),
            truncate_line_spans(
                Line::from(vec![
                    Span::styled("MAC    ", label),
                    Span::raw(info.mac_address()),
                ]),
                col_width,
            ),
        ]
    } else if let Some(s) = app.agent_startup() {
        vec![
            truncate_line_spans(
                Line::from(vec![
                    Span::styled("Board  ", label),
                    Span::raw(s.chip.as_str()),
                ]),
                col_width,
            ),
            truncate_line_spans(
                Line::from(vec![
                    Span::styled("Flash  ", label),
                    Span::raw(format_bytes(s.flash_size)),
                ]),
                col_width,
            ),
            truncate_line_spans(
                Line::from(vec![
                    Span::styled("MAC    ", label),
                    Span::raw(format_mac(s.mac)),
                ]),
                col_width,
            ),
        ]
    } else {
        vec![]
    }
}

fn mline(spans: Vec<Span<'static>>, col_width: usize) -> Line<'static> {
    truncate_line_spans(Line::from(spans), col_width)
}

fn cpu_bar_color(usage: u8) -> Color {
    if usage > 80 {
        Color::Red
    } else if usage > 50 {
        Color::Yellow
    } else {
        Color::Green
    }
}

fn frame_metric_lines(
    f: &agent_msg::Frame,
    label: Style,
    value_style: Style,
    is_stale: bool,
    col_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();

    let heap_ratio = f64::from(f.heap_free) / f64::from(f.heap_total.max(1));
    lines.push(mline(
        vec![
            Span::styled("Heap  ", label),
            Span::styled(
                inspector_bar(heap_ratio, INSPECTOR_BAR_W),
                agent_bar_style(is_stale, Color::Green),
            ),
            Span::styled(
                format!(
                    "  {}/{}",
                    format_bytes(f.heap_free),
                    format_bytes(f.heap_total)
                ),
                value_style,
            ),
        ],
        col_width,
    ));
    lines.push(mline(
        vec![
            Span::styled("Min   ", label),
            Span::styled(format_bytes(f.heap_min_free), value_style),
            Span::styled(" low-water", label),
        ],
        col_width,
    ));
    if f.heap_iram > 0 {
        lines.push(mline(
            vec![
                Span::styled("IRAM  ", label),
                Span::styled(format_bytes(f.heap_iram), value_style),
                Span::styled(" free", label),
            ],
            col_width,
        ));
    }
    if f.heap_psram > 0 {
        lines.push(mline(
            vec![
                Span::styled("PSRAM ", label),
                Span::styled(format_bytes(f.heap_psram), value_style),
                Span::styled(" free", label),
            ],
            col_width,
        ));
    }
    lines.push(Line::from(""));
    lines.extend(f.cpu_usage.iter().enumerate().map(|(i, &usage)| {
        let cpu_ratio = f64::from(usage) / 100.0;
        let cpu_color = cpu_bar_color(usage);
        mline(
            vec![
                Span::styled(format!("CPU{i}  "), label),
                Span::styled(
                    inspector_bar(cpu_ratio, INSPECTOR_BAR_W),
                    agent_bar_style(is_stale, cpu_color),
                ),
                Span::styled(format!("  {usage}%"), value_style),
            ],
            col_width,
        )
    }));
    if let Some(rssi) = f.wifi_rssi {
        lines.push(Line::from(""));
        lines.push(mline(
            vec![
                Span::styled("WiFi  ", label),
                Span::styled(format!("{rssi} dBm"), value_style),
            ],
            col_width,
        ));
    }
    if let Some((used, total)) = f.nvs {
        lines.push(mline(
            vec![
                Span::styled("NVS   ", label),
                Span::styled(format!("{used}/{total} entries"), value_style),
            ],
            col_width,
        ));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Tasks",
        label.add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        truncate_line(
            format!("{:<16}  {:<9}  {:<7}  {}", "Name", "State", "Stack", "Prio"),
            col_width,
        ),
        label,
    )));
    lines
}

fn build_inspector_lines<'a>(app: &'a App, col_width: usize) -> Vec<Line<'a>> {
    let is_stale = app
        .agent_last_seen()
        .is_some_and(|t| t.elapsed() > Duration::from_secs(5));
    let label = Style::default().fg(Color::DarkGray);
    let value_style = if is_stale { label } else { Style::default() };
    let mut lines: Vec<Line<'a>> = board_info_lines(app, label, col_width);

    if let Some(parts) = app.agent_partitions() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Partitions",
            label.add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from(Span::styled(
            truncate_line(
                format!("{:<16}  {:<6}  {:<10}  Size", "Label", "Type", "Offset"),
                col_width,
            ),
            label,
        )));
        lines.extend(parts.iter().map(|p| {
            let type_str = match p.part_type {
                agent_msg::PartType::App => "app",
                agent_msg::PartType::Data => "data",
                agent_msg::PartType::Unknown => "?",
            };
            Line::from(Span::styled(
                truncate_line(
                    format!(
                        "{:<16}  {:<6}  0x{:08x}  {}",
                        p.label.as_str(),
                        type_str,
                        p.offset,
                        format_bytes(p.size),
                    ),
                    col_width,
                ),
                value_style,
            ))
        }));
    }

    if let Some(f) = app.agent_frame() {
        lines.push(Line::from(""));
        lines.extend(frame_metric_lines(
            f,
            label,
            value_style,
            is_stale,
            col_width,
        ));
        lines.extend(f.tasks.iter().map(|t| {
            Line::from(Span::styled(
                truncate_line(
                    format!(
                        "{:<16}  {:<9}  {:<7}  {}",
                        t.name.as_str(),
                        task_state_label(t.state),
                        format_bytes(t.hwm),
                        t.priority,
                    ),
                    col_width,
                ),
                value_style,
            ))
        }));
    } else {
        let baseline = app.agent_last_seen().or_else(|| app.connected_at());
        let timed_out =
            baseline.is_some_and(|t| t.elapsed() > Duration::from_secs(10));
        let msg = if timed_out {
            "esp-agent not detected. Add the esp-agent library to your \
             firmware to see live telemetry here."
        } else {
            "Waiting for esp-agent..."
        };
        if !lines.is_empty() {
            lines.push(Line::from(""));
        }
        lines.extend(
            word_wrap(msg, col_width)
                .into_iter()
                .map(|l| Line::from(Span::styled(l, label))),
        );
    }
    lines
}

fn render_inspector(frame: &mut Frame, area: Rect, app: &App, is_focused: bool) {
    let block = Block::default()
        .title(clip_title(" System Inspector ", area.width))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused_border(is_focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [content_area, footer_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

    if is_focused {
        let w = usize::from(footer_area.width);
        let footer = scroll_footer(
            w,
            app.inspector_scroll().min(app.inspector_max_scroll()) > 0,
            "  q/Esc to scroll top",
            "[↑/↓  PgUp/PgDn] scroll  [^←/→] resize  [Tab] focus",
        );
        frame.render_widget(Paragraph::new(footer), footer_area);
    }

    let col_width = usize::from(content_area.width);
    if app.port_name().is_none() {
        frame.render_widget(
            Paragraph::new("Connect a device to begin.")
                .style(Style::default().fg(Color::DarkGray))
                .wrap(ratatui::widgets::Wrap { trim: false }),
            content_area,
        );
    } else {
        let lines = build_inspector_lines(app, col_width);
        let viewport = usize::from(content_area.height);
        let max_scroll = lines.len().saturating_sub(viewport);
        app.set_inspector_max_scroll(max_scroll);
        let skip = app.inspector_scroll().min(max_scroll);
        frame.render_widget(Paragraph::new(lines[skip..].to_vec()), content_area);
    }
}

fn word_wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let needed = if current.is_empty() {
            word.len()
        } else {
            current.len() + 1 + word.len()
        };
        if !current.is_empty() && needed > width {
            lines.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
    }
    if !current.is_empty() {
        lines.push(current);
    }
    lines
}

fn clip_title(title: &str, area_width: u16) -> String {
    truncate_line(title, usize::from(area_width).saturating_sub(2))
}

fn truncate_line(s: impl Into<String>, max_chars: usize) -> String {
    let mut s = s.into();
    if s.chars().count() > max_chars {
        let cut = s
            .char_indices()
            .nth(max_chars.saturating_sub(1))
            .map_or(0, |(i, _)| i);
        s.truncate(cut);
        s.push('…');
    }
    s
}

fn truncate_line_spans(line: Line<'_>, max_chars: usize) -> Line<'_> {
    let total: usize = line.spans.iter().map(|s| s.content.chars().count()).sum();
    if total <= max_chars {
        line
    } else {
        let budget = max_chars.saturating_sub(1);
        let mut out: Vec<Span<'_>> = Vec::new();
        let mut used = 0usize;
        for span in line.spans {
            if used >= budget {
                break;
            }
            let count = span.content.chars().count();
            if used + count <= budget {
                used += count;
                out.push(span);
            } else {
                let need = budget - used;
                let cut = span
                    .content
                    .char_indices()
                    .nth(need)
                    .map_or(span.content.len(), |(i, _)| i);
                out.push(Span::styled(span.content[..cut].to_string(), span.style));
                used = budget;
            }
        }
        out.push(Span::raw("…"));
        Line::from(out)
    }
}

fn format_bytes(bytes: u32) -> String {
    if bytes >= 1024 * 1024 {
        format!("{}MB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{}KB", bytes / 1024)
    } else {
        format!("{bytes}B")
    }
}

#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss
)]
fn inspector_bar(ratio: f64, width: usize) -> String {
    let filled = (ratio.clamp(0.0, 1.0) * width as f64).round() as usize;
    let empty = width.saturating_sub(filled);
    format!("{}{}", "█".repeat(filled), "░".repeat(empty))
}

fn format_mac(mac: [u8; 6]) -> String {
    format!(
        "{:02X}:{:02X}:{:02X}:{:02X}:{:02X}:{:02X}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

fn task_state_label(state: agent_msg::TaskState) -> &'static str {
    match state {
        agent_msg::TaskState::Running => "Running",
        agent_msg::TaskState::Ready => "Ready",
        agent_msg::TaskState::Blocked => "Blocked",
        agent_msg::TaskState::Suspended => "Suspend",
        agent_msg::TaskState::Deleted => "Deleted",
    }
}

fn agent_bar_style(is_stale: bool, color: Color) -> Style {
    if is_stale {
        Style::default().fg(Color::DarkGray)
    } else {
        Style::default().fg(color)
    }
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App, is_focused: bool) {
    let block = Block::default()
        .title(clip_title(" Status ", area.width))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused_border(is_focused));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match app.flash_state() {
        flash::State::Flashing {
            addr,
            current,
            total,
        } => {
            if let Some(msg) = app.status_msg() {
                frame.render_widget(
                    Paragraph::new(msg).style(Style::default().fg(Color::Yellow)),
                    inner,
                );
            } else {
                let ratio = if *total == 0 {
                    0.0_f64
                } else {
                    let cur = f64::from(u32::try_from(*current).unwrap_or(u32::MAX));
                    let tot = f64::from(u32::try_from(*total).unwrap_or(u32::MAX));
                    cur / tot
                };
                let addr_str = format!(" Writing at 0x{addr:08x}...");
                let pct_str = format!("{:.0}%", ratio * 100.0);
                let width = inner.width as usize;
                // Build a label as wide as the gauge area so the gauge positions
                // it at x=0; every character then goes through the gauge's own
                // colour-inversion logic at the fill boundary for free.
                let pct_start = (width / 2).saturating_sub(pct_str.len() / 2);
                let mid = pct_start.saturating_sub(addr_str.len());
                let right =
                    width.saturating_sub(addr_str.len() + mid + pct_str.len());
                let label = format!("{addr_str}{:mid$}{pct_str}{:right$}", "", "");
                let gauge = Gauge::default()
                    .gauge_style(Style::default().fg(Color::Green))
                    .ratio(ratio)
                    .label(label);
                frame.render_widget(gauge, inner);
            }
        }
        flash::State::Erasing => {
            if let Some(msg) = app.status_msg() {
                frame.render_widget(
                    Paragraph::new(msg).style(Style::default().fg(Color::Yellow)),
                    inner,
                );
            } else {
                frame.render_widget(
                    Paragraph::new("Erasing flash...")
                        .style(Style::default().fg(Color::Yellow)),
                    inner,
                );
            }
        }
        flash::State::Idle | flash::State::Reconnecting => {
            let content = app.status_msg().unwrap_or("");
            let style = if content.is_empty() {
                Style::default().fg(Color::DarkGray)
            } else {
                Style::default().fg(Color::Yellow)
            };
            frame.render_widget(
                Paragraph::new(if content.is_empty() { "Ready" } else { content })
                    .style(style),
                inner,
            );
        }
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn render_quit_confirm_popup(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(52, 7, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Confirm Quit ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Are you sure you want to quit?",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "[Y]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" confirm   "),
            Span::styled("[N] / [q/Esc]", Style::default().fg(Color::DarkGray)),
            Span::raw(" cancel"),
        ]),
    ];
    frame.render_widget(Paragraph::new(text), inner);
}

fn render_erase_confirm_popup(frame: &mut Frame, area: Rect) {
    let popup = centered_rect(52, 7, area);
    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Confirm Erase ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(popup);
    frame.render_widget(block, popup);

    let text = vec![
        Line::from(""),
        Line::from(Span::styled(
            "This will erase ALL flash data.",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        )),
        Line::from("This operation cannot be undone."),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "[Y]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" confirm   "),
            Span::styled("[N] / [q/Esc]", Style::default().fg(Color::DarkGray)),
            Span::raw(" cancel"),
        ]),
    ];
    frame.render_widget(Paragraph::new(text), inner);
}

/// Splits `value` at `cursor_pos` and returns spans with the cursor
/// character (or a space when at end) rendered in reverse video.
fn text_cursor_spans(value: &str, cursor_pos: usize) -> Vec<Span<'_>> {
    let before = &value[..cursor_pos];
    let rest = &value[cursor_pos..];
    if let Some(c) = rest.chars().next() {
        let len = c.len_utf8();
        vec![
            Span::raw(before),
            Span::styled(
                &rest[..len],
                Style::default().add_modifier(Modifier::REVERSED),
            ),
            Span::raw(&rest[len..]),
        ]
    } else {
        vec![
            Span::raw(before),
            Span::styled(" ", Style::default().add_modifier(Modifier::REVERSED)),
        ]
    }
}

fn render_elf_input(frame: &mut Frame, area: Rect, value: &str, cursor_pos: usize) {
    let before = &value[..cursor_pos];
    let cursor_chars = before.chars().count();
    let visible_width = area.width as usize;
    let scroll = cursor_chars.saturating_sub(visible_width.saturating_sub(1));
    let display_before = before
        .char_indices()
        .nth(scroll)
        .map_or("", |(i, _)| &before[i..]);

    let rest = &value[cursor_pos..];
    let (cursor_str, after): (&str, &str) = if let Some(c) = rest.chars().next() {
        let len = c.len_utf8();
        (&rest[..len], &rest[len..])
    } else {
        (" ", "")
    };

    let input_line = Line::from(vec![
        Span::raw(display_before),
        Span::styled(
            cursor_str,
            Style::default().add_modifier(Modifier::REVERSED),
        ),
        Span::raw(after),
    ]);
    frame.render_widget(Paragraph::new(input_line), area);

    let display_cursor_col = u16::try_from(cursor_chars - scroll).unwrap_or(0);
    if display_cursor_col < area.width {
        frame.set_cursor_position((area.x + display_cursor_col, area.y));
    }
}

fn render_elf_selector_popup(frame: &mut Frame, area: Rect, app: &App) {
    if let Some(sel) = app.elf_selector() {
        let completions = sel.completions();
        let comp_count = u16::try_from(completions.len()).unwrap_or(u16::MAX);
        let height = if completions.is_empty() {
            5u16
        } else {
            (4 + comp_count).min(area.height)
        };
        let width = 64u16.min(area.width);
        let popup = centered_rect(width, height, area);

        frame.render_widget(Clear, popup);

        let block = Block::default()
            .title(" ELF Path ")
            .borders(Borders::ALL)
            .border_type(BorderType::Rounded);
        let inner = block.inner(popup);
        frame.render_widget(block, popup);

        if inner.height > 0 {
            let input_area = Rect {
                x: inner.x,
                y: inner.y,
                width: inner.width,
                height: 1,
            };

            render_elf_input(frame, input_area, sel.value(), sel.cursor_pos());

            if inner.height > 1 {
                let hint_area = Rect {
                    x: inner.x,
                    y: inner.y + inner.height - 1,
                    width: inner.width,
                    height: 1,
                };

                if completions.is_empty() {
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            "[Tab] complete  [Enter] confirm  [Esc] cancel",
                            Style::default().fg(Color::DarkGray),
                        )),
                        hint_area,
                    );
                } else {
                    let comp_area = Rect {
                        x: inner.x,
                        y: inner.y + 1,
                        width: inner.width,
                        height: inner.height.saturating_sub(2),
                    };

                    let items: Vec<ListItem> = completions
                        .iter()
                        .enumerate()
                        .map(|(i, name)| {
                            let style = if i == sel.completion_cursor() {
                                Style::default().add_modifier(Modifier::REVERSED)
                            } else {
                                Style::default()
                            };
                            ListItem::new(format!("  {name}")).style(style)
                        })
                        .collect();

                    frame.render_widget(List::new(items), comp_area);
                    frame.render_widget(
                        Paragraph::new(Span::styled(
                            "[Tab] cycle  [↑/↓] navigate  [Enter] select  [Esc] cancel",
                            Style::default().fg(Color::DarkGray),
                        )),
                        hint_area,
                    );
                }
            }
        }
    }
}

fn filter_search_item(filter: &filter::State, search_focused: bool) -> ListItem<'_> {
    let label = Span::styled(" Search: ", Style::default().fg(Color::DarkGray));
    let query = filter.search_query();
    let content: Line<'_> = if search_focused {
        let mut spans = vec![label];
        spans.extend(text_cursor_spans(query, filter.search_cursor()));
        Line::from(spans)
    } else if query.is_empty() {
        Line::from(vec![
            label,
            Span::styled("type to search…", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            label,
            Span::styled(query, Style::default().fg(Color::Yellow)),
        ])
    };
    ListItem::new(content)
}

fn render_filter_popup(frame: &mut Frame, area: Rect, app: &App) {
    const HINT_NAV: &str = " [↑/↓] navigate  [Space] toggle  [^A] all  [Esc] close";
    const HINT_SEARCH: &str = " [↑/↓] navigate  [Esc] done";

    let filter = app.filter();
    let levels = filter::State::levels();
    let all_tags: Vec<&str> =
        filter.known_tags().iter().map(String::as_str).collect();
    let any_tags = !filter.known_tags().is_empty();
    let search_focused = filter.is_search_focused();

    let hint = if search_focused {
        HINT_SEARCH
    } else {
        HINT_NAV
    };
    let hint_width = HINT_NAV.len().max(HINT_SEARCH.len());

    let tag_section_rows: u16 = if any_tags {
        1 + u16::try_from(all_tags.len()).unwrap_or(u16::MAX)
    } else {
        0
    };
    let height = (2
        + 1
        + 1
        + u16::try_from(levels.len()).unwrap_or(5)
        + tag_section_rows
        + 1)
    .min(area.height);
    let width = (u16::try_from(hint_width).unwrap_or(70) + 3).min(area.width);
    let popup = centered_rect(width, height, area);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Filter ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);

    let section_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);

    let search_item = filter_search_item(filter, search_focused);

    let level_items = levels.iter().enumerate().map(|(i, &level)| {
        let marker = if filter.is_level_hidden(level) {
            "[ ]"
        } else {
            "[x]"
        };
        let style = if filter.cursor() == i {
            Style::default()
                .fg(level.color())
                .add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(level.color())
        };
        ListItem::new(format!("  {marker} {}", level.label())).style(style)
    });

    let tag_items: Box<dyn Iterator<Item = ListItem>> = if any_tags {
        Box::new(
            std::iter::once(ListItem::new(" Tags").style(section_style)).chain(
                all_tags.into_iter().enumerate().map(|(i, tag)| {
                    let marker = if filter.is_tag_hidden(tag) {
                        "[ ]"
                    } else {
                        "[x]"
                    };
                    let style = if filter.cursor() == levels.len() + i {
                        Style::default().add_modifier(Modifier::REVERSED)
                    } else {
                        Style::default()
                    };
                    ListItem::new(format!("  {marker} {tag}")).style(style)
                }),
            ),
        )
    } else {
        Box::new(std::iter::empty())
    };

    let items: Vec<ListItem> = std::iter::once(search_item)
        .chain(std::iter::once(
            ListItem::new(" Severity").style(section_style),
        ))
        .chain(level_items)
        .chain(tag_items)
        .chain(std::iter::once(
            ListItem::new(hint).style(Style::default().fg(Color::DarkGray)),
        ))
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, popup);
}

fn render_port_selector(frame: &mut Frame, area: Rect, sel: &crate::port::Selector) {
    const HINT: &str = " [↑/↓] navigate  [Enter] connect  [q/Esc] close";

    let ports = sel.ports();
    let height = (u16::try_from(ports.len())
        .unwrap_or(u16::MAX)
        .saturating_add(3))
    .max(4)
    .min(area.height);
    let width =
        (u16::try_from(HINT.chars().count()).unwrap_or(50) + 4).min(area.width);
    let popup = centered_rect(width, height, area);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Select Port ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);

    let items: Vec<ListItem> = ports
        .iter()
        .enumerate()
        .map(|(i, port)| {
            let style = if i == sel.cursor() {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(format!("  {port}")).style(style)
        })
        .chain(std::iter::once(
            ListItem::new(HINT).style(Style::default().fg(Color::DarkGray)),
        ))
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, popup);
}

#[cfg(test)]
mod tests {
    use ratatui::backend::TestBackend;
    use ratatui::layout::Rect;
    use ratatui::Terminal;

    use super::centered_rect;
    use crate::app::App;

    fn render(app: &App) {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| super::draw(f, app)).unwrap();
    }

    #[test]
    fn draw_empty_app_does_not_panic() {
        render(&App::new(None));
    }

    #[test]
    fn draw_with_log_entries_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.push_line("I (1) wifi: Connected");
        app.push_line("E (1) i2c: Timeout");
        app.push_line("some raw line");
        render(&app);
    }

    #[test]
    fn draw_with_filter_popup_open_does_not_panic() {
        let mut app = App::new(None);
        app.push_line("I (1) wifi: msg");
        app.filter_mut().toggle_popup();
        render(&app);
    }

    #[test]
    fn draw_with_port_selector_open_does_not_panic() {
        let mut app = App::new(None);
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        render(&app);
    }

    #[test]
    fn draw_with_erase_confirm_open_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.open_erase_confirm();
        render(&app);
    }

    #[test]
    fn draw_with_quit_confirm_open_does_not_panic() {
        let mut app = App::new(None);
        app.open_quit_confirm();
        render(&app);
    }

    #[test]
    fn draw_with_quit_confirm_open_while_flashing_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        app.open_quit_confirm();
        render(&app);
    }

    #[test]
    fn draw_with_elf_selector_open_does_not_panic() {
        let mut app = App::new(None);
        app.open_elf_selector(None);
        render(&app);
    }

    #[test]
    fn draw_with_elf_selector_and_completions_does_not_panic() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        fn key(code: KeyCode) -> KeyEvent {
            KeyEvent::new(code, KeyModifiers::empty())
        }
        let dir = std::env::temp_dir().join(format!(
            "esp-tui-ui-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.subsec_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("fw_a.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        std::fs::write(dir.join("fw_b.elf"), b"\x7fELF\x00\x00\x00\x00").unwrap();
        let mut app = App::new(None);
        app.open_elf_selector(None);
        for ch in format!("{}/fw", dir.display()).chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Tab));
        render(&app);
    }

    #[test]
    fn draw_with_flash_state_flashing_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0x1000,
            current: 512,
            total: 1024,
        });
        render(&app);
    }

    #[test]
    fn draw_with_flash_state_flashing_and_status_overlay_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0,
            current: 0,
            total: 0,
        });
        app.set_status("Waiting for flash to complete...");
        render(&app);
    }

    #[test]
    fn draw_with_flash_state_erasing_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.set_flash_state(crate::flash::State::Erasing);
        render(&app);
    }

    #[test]
    fn draw_with_flash_state_erasing_and_status_overlay_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.set_flash_state(crate::flash::State::Erasing);
        app.set_status("Operation already in progress.");
        render(&app);
    }

    #[test]
    fn draw_with_flash_state_reconnecting_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.set_flash_state(crate::flash::State::Reconnecting);
        app.set_status("Flash complete. Reconnecting...");
        render(&app);
    }

    #[test]
    fn draw_with_device_info_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.set_device_info(crate::flash::DeviceInfo::new(
            "ESP32-S3 (rev v0.1)",
            "4MB",
            "AA:BB:CC:DD:EE:FF",
        ));
        render(&app);
    }

    #[test]
    fn draw_with_elf_path_set_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.set_elf_path(std::path::PathBuf::from("/tmp/firmware.elf"));
        render(&app);
    }

    #[test]
    fn draw_inspector_connected_no_agent_does_not_panic() {
        render(&App::new(Some("COM1".into())));
    }

    #[test]
    fn draw_inspector_with_agent_frame_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.push_line(
            "V (1) esp_agent: heap=100000/200000 min=50000 frag=10000 \
             iram=40000 psram=0 cpu=23,45 tasks=main:R:3200:1,wifi:B:1800:5",
        );
        render(&app);
    }

    #[test]
    fn draw_inspector_with_psram_and_wifi_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.push_line(
            "V (1) esp_agent: heap=100000/200000 min=50000 frag=10000 \
             iram=0 psram=524288 cpu=90 wifi=-65 nvs=45/512 tasks=",
        );
        render(&app);
    }

    #[test]
    fn draw_inspector_with_agent_startup_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.push_line(
            "V (1) esp_agent: start reason=poweron chip=esp32s3 cores=2 \
             rev=1 mac=AA:BB:CC:DD:EE:FF flash=0x400000",
        );
        render(&app);
    }

    #[test]
    fn draw_inspector_inspector_pane_focused_does_not_panic() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = App::new(Some("COM1".into()));
        app.push_line(
            "V (1) esp_agent: heap=100000/200000 min=50000 frag=10000 \
             iram=0 psram=0 cpu=50 tasks=t1:R:1024:1,t2:B:512:2,t3:r:256:3",
        );
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::empty()));
        render(&app);
    }

    #[test]
    fn inspector_bar_empty_is_all_blocks() {
        let bar = super::inspector_bar(0.0, 5);
        assert_eq!(bar, "░░░░░");
    }

    #[test]
    fn inspector_bar_full_is_all_filled() {
        let bar = super::inspector_bar(1.0, 5);
        assert_eq!(bar, "█████");
    }

    #[test]
    fn inspector_bar_half_is_mixed() {
        let bar = super::inspector_bar(0.5, 10);
        assert_eq!(bar, "█████░░░░░");
    }

    #[test]
    fn truncate_line_no_op_when_short() {
        assert_eq!(super::truncate_line("hello", 10), "hello");
    }

    #[test]
    fn truncate_line_no_op_when_exact() {
        assert_eq!(super::truncate_line("hello", 5), "hello");
    }

    #[test]
    fn truncate_line_appends_ellipsis_when_over() {
        assert_eq!(super::truncate_line("hello world", 8), "hello w…");
    }

    #[test]
    fn truncate_line_handles_multibyte() {
        let s = "héllo".to_string();
        assert_eq!(super::truncate_line(s, 4), "hél…");
    }

    #[test]
    fn format_mac_formats_correctly() {
        let mac = super::format_mac([0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF]);
        assert_eq!(mac, "AA:BB:CC:DD:EE:FF");
    }

    #[test]
    fn centered_rect_centers_within_area() {
        let area = Rect::new(0, 0, 100, 50);
        let r = centered_rect(40, 20, area);
        assert_eq!(r.x, 30);
        assert_eq!(r.y, 15);
        assert_eq!(r.width, 40);
        assert_eq!(r.height, 20);
    }

    #[test]
    fn centered_rect_clamps_to_area_size() {
        let area = Rect::new(0, 0, 30, 20);
        let r = centered_rect(40, 30, area);
        assert_eq!(r.width, 30);
        assert_eq!(r.height, 20);
        assert_eq!(r.x, 0);
        assert_eq!(r.y, 0);
    }

    #[test]
    fn centered_rect_with_offset_origin() {
        let area = Rect::new(10, 5, 80, 40);
        let r = centered_rect(20, 10, area);
        assert_eq!(r.x, 40);
        assert_eq!(r.y, 20);
        assert_eq!(r.width, 20);
        assert_eq!(r.height, 10);
    }
}
