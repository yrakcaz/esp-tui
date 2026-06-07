use std::time::Duration;

use esp_agent_msg as agent_msg;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap,
};
use ratatui::Frame;

use crate::app::{App, MappableAction, Pane};
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

    render_menu_bar(frame, outer[0], app);
    let main = Layout::horizontal([
        Constraint::Percentage(app.monitor_pct()),
        Constraint::Percentage(100 - app.monitor_pct()),
    ])
    .split(outer[1]);
    render_monitor(frame, main[0], app, app.focused_pane() == Pane::Monitor);
    render_inspector(frame, main[1], app, app.focused_pane() == Pane::Inspector);
    render_status_bar(frame, outer[2], app, app.focused_pane() == Pane::Status);

    if app.is_quit_confirm_open() {
        render_quit_confirm_popup(frame, frame.area(), app);
    } else if app.is_erase_confirm_open() {
        render_erase_confirm_popup(frame, frame.area(), app);
    } else if app.is_elf_selector_open() {
        render_elf_selector_popup(frame, frame.area(), app);
    } else if let Some(sel) = app.port_selector() {
        render_port_selector(frame, frame.area(), sel, app);
    } else if app.filter().is_popup_open() {
        render_filter_popup(frame, frame.area(), app);
    }
}

fn render_menu_bar(frame: &mut Frame, area: Rect, app: &App) {
    let colors = &app.config().colors;
    let hc = colors.chrome.hint_text;
    let port_name = app.port_name();
    let port_label: std::borrow::Cow<str> =
        port_name.map_or("none".into(), std::borrow::Cow::Borrowed);
    let port_color = if port_name.is_some() {
        colors.chrome.port_connected
    } else {
        colors.chrome.port_disconnected
    };
    let right_text = format!("Port: {port_label}");

    let left = Line::from(vec![
        hint(app.key_hint(MappableAction::ScanPorts, "Connect"), hc),
        Span::raw("  "),
        hint(app.key_hint(MappableAction::Disconnect, "Disconnect"), hc),
        Span::raw("  "),
        hint(app.key_hint(MappableAction::Flash, "Flash"), hc),
        Span::raw("  "),
        hint(app.key_hint(MappableAction::ErasePrompt, "Erase"), hc),
        Span::raw("  "),
        hint(app.key_hint(MappableAction::ResetDevice, "Reset"), hc),
        Span::raw("  "),
        hint(app.key_hint(MappableAction::QuitPrompt, "Quit"), hc),
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

fn focused_border(is_focused: bool, color: Color) -> Style {
    if is_focused {
        Style::default().fg(color)
    } else {
        Style::default()
    }
}

fn scroll_footer(
    w: usize,
    is_scrolled: bool,
    scroll_hint: &str,
    nav_hint: &str,
    indicator_color: Color,
    nav_color: Color,
) -> Line<'static> {
    const BADGE: &str = " SCROLL ";
    if is_scrolled {
        let tail = truncate_line(scroll_hint, w.saturating_sub(BADGE.len()));
        Line::from(vec![
            Span::styled(
                BADGE,
                Style::default()
                    .fg(indicator_color)
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            ),
            Span::styled(tail, Style::default().fg(indicator_color)),
        ])
    } else {
        Line::from(Span::styled(
            truncate_line(nav_hint, w),
            Style::default().fg(nav_color),
        ))
    }
}

fn hint(text: String, color: Color) -> Span<'static> {
    Span::styled(
        text,
        Style::default().fg(color).add_modifier(Modifier::BOLD),
    )
}

fn render_monitor(frame: &mut Frame, area: Rect, app: &App, is_focused: bool) {
    let colors = &app.config().colors;
    let block = Block::default()
        .title(clip_title(" Serial Monitor ", area.width))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused_border(is_focused, colors.chrome.focused_border));

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
                    Style::default().fg(colors.chrome.log_unparsed),
                ))
            } else {
                let level_color = level_color(e.level(), colors);
                Line::from(vec![
                    Span::styled(
                        format!("[{}]", e.level().label()),
                        Style::default()
                            .fg(level_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("{}: ", e.tag()),
                        Style::default().fg(colors.chrome.log_tag),
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
            colors.chrome.filter_bar,
        );
    }

    if is_focused {
        let w = usize::from(footer_area.width);
        let scroll_hint = format!(
            "  [{}] to follow live",
            app.key_display(MappableAction::QuitPrompt)
        );
        let nav_hint = format!(
            "[{}/{}  {}/{}] scroll  [{}] clear  [{}] filter  [{}/{}] resize  [{}] focus",
            app.key_display(MappableAction::ScrollUp),
            app.key_display(MappableAction::ScrollDown),
            app.key_display(MappableAction::PageUp),
            app.key_display(MappableAction::PageDown),
            app.key_display(MappableAction::ClearLog),
            app.key_display(MappableAction::ToggleFilter),
            app.key_display(MappableAction::ShrinkMonitor),
            app.key_display(MappableAction::GrowMonitor),
            app.key_display(MappableAction::SwitchPane),
        );
        let footer = scroll_footer(
            w,
            app.scroll() > 0,
            &scroll_hint,
            &nav_hint,
            colors.chrome.scroll_indicator,
            colors.chrome.hint_text,
        );
        frame.render_widget(Paragraph::new(footer), footer_area);
    }
}

fn level_color(
    level: crate::log::Level,
    colors: &crate::config::ColorsConfig,
) -> Color {
    match level {
        crate::log::Level::Error => colors.log.error,
        crate::log::Level::Warn => colors.log.warn,
        crate::log::Level::Info => colors.log.info,
        crate::log::Level::Debug => colors.log.debug,
        crate::log::Level::Verbose => colors.log.verbose,
    }
}

fn render_filter_bar(
    frame: &mut Frame,
    area: Rect,
    hidden_level_count: usize,
    hidden_tag_count: usize,
    search_query: &str,
    color: Color,
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
        Paragraph::new(Line::from(Span::styled(line, Style::default().fg(color)))),
        area,
    );
}

fn mline(spans: Vec<Span<'static>>, col_width: usize) -> Line<'static> {
    truncate_line_spans(Line::from(spans), col_width)
}

fn reset_line(
    s: &agent_msg::Startup,
    label: Style,
    col_width: usize,
) -> Line<'static> {
    mline(
        vec![
            Span::styled("Reset  ", label),
            Span::raw(reset_reason_label(s.reason)),
        ],
        col_width,
    )
}

fn cores_line(cores: u8, label: Style, col_width: usize) -> Line<'static> {
    mline(
        vec![Span::styled("Cores  ", label), Span::raw(cores.to_string())],
        col_width,
    )
}

fn board_info_lines(app: &App, label: Style, col_width: usize) -> Vec<Line<'_>> {
    if let Some(info) = app.device_info() {
        let startup = app.agent_startup();
        let mut lines = vec![truncate_line_spans(
            Line::from(vec![Span::styled("Board  ", label), Span::raw(info.chip())]),
            col_width,
        )];
        if let Some(s) = startup {
            lines.push(cores_line(s.cores, label, col_width));
        }
        lines.push(truncate_line_spans(
            Line::from(vec![
                Span::styled("Flash  ", label),
                Span::raw(info.flash_size()),
            ]),
            col_width,
        ));
        lines.push(truncate_line_spans(
            Line::from(vec![
                Span::styled("MAC    ", label),
                Span::raw(info.mac_address()),
            ]),
            col_width,
        ));
        if let Some(s) = startup {
            lines.push(reset_line(s, label, col_width));
        }
        lines
    } else if let Some(s) = app.agent_startup() {
        let board_label = if s.revision > 0 {
            format!(
                "{} (rev v{}.{})",
                s.chip,
                s.revision / 100,
                s.revision % 100
            )
        } else {
            s.chip.as_str().to_owned()
        };
        vec![
            mline(
                vec![Span::styled("Board  ", label), Span::raw(board_label)],
                col_width,
            ),
            cores_line(s.cores, label, col_width),
            mline(
                vec![
                    Span::styled("Flash  ", label),
                    Span::raw(format_bytes(s.flash_size)),
                ],
                col_width,
            ),
            mline(
                vec![Span::styled("MAC    ", label), Span::raw(format_mac(s.mac))],
                col_width,
            ),
            reset_line(s, label, col_width),
        ]
    } else {
        vec![]
    }
}

fn cpu_bar_color(usage: u8, ic: &crate::config::InspectorColors) -> Color {
    if usage > 80 {
        ic.cpu_high
    } else if usage > 50 {
        ic.cpu_medium
    } else {
        ic.cpu_low
    }
}

struct InspectorStyle {
    label: Style,
    value_style: Style,
    is_stale: bool,
    metric_bar_color: Color,
    hint_text_color: Color,
}

fn heap_section_lines(
    f: &agent_msg::Frame,
    col_width: usize,
    heap_history: &std::collections::VecDeque<u32>,
    sparkline_w: usize,
    s: &InspectorStyle,
) -> Vec<Line<'static>> {
    let (label, value_style, is_stale, metric_bar_color, hint_text_color) = (
        s.label,
        s.value_style,
        s.is_stale,
        s.metric_bar_color,
        s.hint_text_color,
    );
    let heap_ratio = f64::from(f.heap_free) / f64::from(f.heap_total.max(1));
    let mut lines = vec![mline(
        vec![
            Span::styled("Heap  ", label),
            Span::styled(
                inspector_bar(heap_ratio, INSPECTOR_BAR_W),
                agent_bar_style(is_stale, metric_bar_color, hint_text_color),
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
    )];
    if !heap_history.is_empty() {
        lines.push(mline(
            vec![
                Span::styled(" hist ", label),
                Span::styled(
                    sparkline_str(heap_history, f.heap_total, sparkline_w),
                    agent_bar_style(is_stale, metric_bar_color, hint_text_color),
                ),
            ],
            col_width,
        ));
    }
    lines.push(mline(
        vec![
            Span::styled("Min   ", label),
            Span::styled(format_bytes(f.heap_min_free), value_style),
            Span::styled(" low-water", label),
        ],
        col_width,
    ));
    if f.heap_frag > 0 {
        lines.push(mline(
            vec![
                Span::styled("Lrg   ", label),
                Span::styled(format_bytes(f.heap_frag), value_style),
                Span::styled(" largest block", label),
            ],
            col_width,
        ));
    }
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
    lines
}

fn wifi_nvs_lines(
    f: &agent_msg::Frame,
    label: Style,
    value_style: Style,
    col_width: usize,
) -> Vec<Line<'static>> {
    let mut lines: Vec<Line<'static>> = Vec::new();
    if let Some(rssi) = f.wifi_rssi {
        let ch_suffix = f
            .wifi_channel
            .map_or_else(String::new, |ch| format!("  ch {ch}"));
        lines.push(Line::from(""));
        lines.push(mline(
            vec![
                Span::styled("WiFi  ", label),
                Span::styled(format!("{rssi} dBm{ch_suffix}"), value_style),
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
    lines
}

fn frame_metric_lines(
    f: &agent_msg::Frame,
    col_width: usize,
    heap_history: &std::collections::VecDeque<u32>,
    cpu_history: &[std::collections::VecDeque<u32>; 2],
    ic: &crate::config::InspectorColors,
    s: &InspectorStyle,
) -> Vec<Line<'static>> {
    let sparkline_w = (col_width.saturating_sub(6)).min(30);
    let mut lines = heap_section_lines(f, col_width, heap_history, sparkline_w, s);
    lines.push(Line::from(""));
    f.cpu_usage.iter().enumerate().for_each(|(i, &usage)| {
        let cpu_ratio = f64::from(usage) / 100.0;
        let cpu_color = cpu_bar_color(usage, ic);
        lines.push(mline(
            vec![
                Span::styled(format!("CPU{i}  "), s.label),
                Span::styled(
                    inspector_bar(cpu_ratio, INSPECTOR_BAR_W),
                    agent_bar_style(s.is_stale, cpu_color, s.hint_text_color),
                ),
                Span::styled(format!("  {usage}%"), s.value_style),
            ],
            col_width,
        ));
        if !cpu_history[i].is_empty() {
            lines.push(mline(
                vec![
                    Span::styled(" hist ", s.label),
                    Span::styled(
                        sparkline_str(&cpu_history[i], 100, sparkline_w),
                        agent_bar_style(s.is_stale, cpu_color, s.hint_text_color),
                    ),
                ],
                col_width,
            ));
        }
    });
    lines.extend(wifi_nvs_lines(f, s.label, s.value_style, col_width));
    lines
}

fn partition_lines(
    parts: &heapless::Vec<agent_msg::Partition, { agent_msg::MAX_PARTITIONS }>,
    label: Style,
    value_style: Style,
    col_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "Partitions",
            label.add_modifier(Modifier::BOLD),
        )),
        Line::from(Span::styled(
            truncate_line(
                format!("{:<16}  {:<6}  {:<10}  Size", "Label", "Type", "Offset"),
                col_width,
            ),
            label,
        )),
    ];
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
    lines
}

fn task_lines(
    tasks: &heapless::Vec<agent_msg::Task, { agent_msg::MAX_TASKS }>,
    label: Style,
    value_style: Style,
    col_width: usize,
) -> Vec<Line<'static>> {
    let mut lines = vec![
        Line::from(""),
        Line::from(Span::styled("Tasks", label.add_modifier(Modifier::BOLD))),
        Line::from(Span::styled(
            truncate_line(
                format!(
                    "{:<16}  {:<9}  {:<7}  {}",
                    "Name", "State", "Stack", "Prio"
                ),
                col_width,
            ),
            label,
        )),
    ];
    lines.extend(tasks.iter().map(|t| {
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
    lines
}

fn build_inspector_lines<'a>(app: &'a App, col_width: usize) -> Vec<Line<'a>> {
    let colors = &app.config().colors;
    let hint_text = colors.chrome.hint_text;
    let is_stale = app
        .agent_last_seen()
        .is_some_and(|t| t.elapsed() > Duration::from_secs(5));
    let label = Style::default().fg(hint_text);
    let value_style = if is_stale { label } else { Style::default() };
    let s = InspectorStyle {
        label,
        value_style,
        is_stale,
        metric_bar_color: colors.inspector.metric_bar,
        hint_text_color: hint_text,
    };
    let frame = app.agent_frame();
    let mut lines: Vec<Line<'a>> = board_info_lines(app, label, col_width);

    if let Some(f) = frame {
        lines.push(mline(
            vec![
                Span::styled("Up     ", label),
                Span::styled(format_uptime(f.timestamp_ms), value_style),
            ],
            col_width,
        ));
        lines.push(Line::from(""));
        lines.extend(frame_metric_lines(
            f,
            col_width,
            app.heap_history(),
            app.cpu_history(),
            &colors.inspector,
            &s,
        ));
        if let Some(parts) = app.agent_partitions() {
            lines.extend(partition_lines(parts, label, value_style, col_width));
        }
        lines.extend(task_lines(&f.tasks, label, value_style, col_width));
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
    let colors = &app.config().colors;
    let block = Block::default()
        .title(clip_title(" System Inspector ", area.width))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused_border(is_focused, colors.chrome.focused_border));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [content_area, footer_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

    if is_focused {
        let w = usize::from(footer_area.width);
        let scroll_hint = format!(
            "  [{}] to scroll top",
            app.key_display(MappableAction::QuitPrompt)
        );
        let nav_hint = format!(
            "[{}/{}  {}/{}] scroll  [{}/{}] resize  [{}] focus",
            app.key_display(MappableAction::ScrollUp),
            app.key_display(MappableAction::ScrollDown),
            app.key_display(MappableAction::PageUp),
            app.key_display(MappableAction::PageDown),
            app.key_display(MappableAction::ShrinkMonitor),
            app.key_display(MappableAction::GrowMonitor),
            app.key_display(MappableAction::SwitchPane),
        );
        let footer = scroll_footer(
            w,
            app.inspector_scroll().min(app.inspector_max_scroll()) > 0,
            &scroll_hint,
            &nav_hint,
            colors.chrome.scroll_indicator,
            colors.chrome.hint_text,
        );
        frame.render_widget(Paragraph::new(footer), footer_area);
    }

    let col_width = usize::from(content_area.width);
    if app.port_name().is_none() {
        frame.render_widget(
            Paragraph::new("Connect a device to begin.")
                .style(Style::default().fg(colors.chrome.hint_text))
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

fn clip_title(title: &str, area_width: u16) -> std::borrow::Cow<'_, str> {
    let max = usize::from(area_width).saturating_sub(2);
    if title.chars().count() <= max {
        std::borrow::Cow::Borrowed(title)
    } else {
        std::borrow::Cow::Owned(truncate_line(title, max))
    }
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

fn reset_reason_label(reason: agent_msg::ResetReason) -> &'static str {
    match reason {
        agent_msg::ResetReason::PowerOn => "PowerOn",
        agent_msg::ResetReason::Software => "Software",
        agent_msg::ResetReason::Panic => "Panic",
        agent_msg::ResetReason::IntWatchdog => "IntWatchdog",
        agent_msg::ResetReason::TaskWatchdog => "TaskWatchdog",
        agent_msg::ResetReason::Watchdog => "Watchdog",
        agent_msg::ResetReason::Brownout => "Brownout",
        agent_msg::ResetReason::DeepSleep => "DeepSleep",
        agent_msg::ResetReason::External => "External",
        agent_msg::ResetReason::Unknown => "Unknown",
    }
}

fn format_uptime(ms: u32) -> String {
    let total_secs = ms / 1000;
    let h = total_secs / 3600;
    let m = (total_secs % 3600) / 60;
    let s = total_secs % 60;
    if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

fn sparkline_str(
    data: &std::collections::VecDeque<u32>,
    max_val: u32,
    width: usize,
) -> String {
    const LEVELS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
    let max = u64::from(max_val.max(1));
    let pad = width.saturating_sub(data.len().min(width));
    // Newest sample is on the left; old data scrolls right and falls off.
    data.iter()
        .rev()
        .take(width)
        .map(|&v| {
            let raw = (u64::from(v) * 8 / max).min(8) as usize;
            let idx = if v > 0 { raw.max(1) } else { raw };
            LEVELS[idx]
        })
        .chain(std::iter::repeat_n(' ', pad))
        .collect()
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

fn agent_bar_style(
    is_stale: bool,
    active_color: Color,
    stale_color: Color,
) -> Style {
    if is_stale {
        Style::default().fg(stale_color)
    } else {
        Style::default().fg(active_color)
    }
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App, is_focused: bool) {
    let colors = &app.config().colors;
    let block = Block::default()
        .title(clip_title(" Status ", area.width))
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(focused_border(is_focused, colors.chrome.focused_border));
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
                    Paragraph::new(msg)
                        .style(Style::default().fg(colors.flash.message)),
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
                    .gauge_style(Style::default().fg(colors.flash.gauge))
                    .ratio(ratio)
                    .label(label);
                frame.render_widget(gauge, inner);
            }
        }
        flash::State::Erasing => {
            if let Some(msg) = app.status_msg() {
                frame.render_widget(
                    Paragraph::new(msg)
                        .style(Style::default().fg(colors.flash.message)),
                    inner,
                );
            } else {
                frame.render_widget(
                    Paragraph::new("Erasing flash...")
                        .style(Style::default().fg(colors.flash.message)),
                    inner,
                );
            }
        }
        flash::State::Idle | flash::State::Reconnecting => {
            let content = app.status_msg().unwrap_or("");
            let style = if content.is_empty() {
                Style::default().fg(colors.chrome.hint_text)
            } else {
                Style::default().fg(colors.chrome.status_message)
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

fn render_quit_confirm_popup(frame: &mut Frame, area: Rect, app: &App) {
    let fc = &app.config().colors.flash;
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
            Style::default()
                .fg(fc.confirm_title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "[Y]",
                Style::default()
                    .fg(fc.confirm_ok)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" confirm   "),
            Span::styled(
                "[N] / [Esc]".to_owned(),
                Style::default().fg(fc.confirm_cancel),
            ),
            Span::raw(" cancel"),
        ]),
    ];
    frame.render_widget(Paragraph::new(text), inner);
}

fn render_erase_confirm_popup(frame: &mut Frame, area: Rect, app: &App) {
    let fc = &app.config().colors.flash;
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
            Style::default()
                .fg(fc.confirm_title)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("This operation cannot be undone."),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "[Y]",
                Style::default()
                    .fg(fc.confirm_ok)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" confirm   "),
            Span::styled(
                "[N] / [Esc]".to_owned(),
                Style::default().fg(fc.confirm_cancel),
            ),
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
        let hint_color = app.config().colors.chrome.hint_text;
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
                            Style::default().fg(hint_color),
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
                            Style::default().fg(hint_color),
                        )),
                        hint_area,
                    );
                }
            }
        }
    }
}

fn filter_search_item(
    filter: &filter::State,
    search_focused: bool,
    search_label_color: Color,
    search_query_color: Color,
) -> ListItem<'_> {
    let label = Span::styled(" Search: ", Style::default().fg(search_label_color));
    let query = filter.search_query();
    let content: Line<'_> = if search_focused {
        let mut spans = vec![label];
        spans.extend(text_cursor_spans(query, filter.search_cursor()));
        Line::from(spans)
    } else if query.is_empty() {
        Line::from(vec![
            label,
            Span::styled("type to search…", Style::default().fg(search_label_color)),
        ])
    } else {
        Line::from(vec![
            label,
            Span::styled(query, Style::default().fg(search_query_color)),
        ])
    };
    ListItem::new(content)
}

fn render_filter_popup(frame: &mut Frame, area: Rect, app: &App) {
    const HINT_NAV: &str = " [↑/↓] navigate  [Space] toggle  [^A] all  [Esc] close";
    const HINT_SEARCH: &str = " [↑/↓] navigate  [Esc] done";

    let colors = &app.config().colors;
    let filter = app.filter();
    let levels = filter::State::levels();
    let all_tags: Vec<&str> =
        filter.known_tags().iter().map(String::as_str).collect();
    let any_tags = !filter.known_tags().is_empty();
    let search_focused = filter.is_search_focused();

    let popup_hint = if search_focused {
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
        .fg(colors.chrome.hint_text)
        .add_modifier(Modifier::BOLD);

    let search_item = filter_search_item(
        filter,
        search_focused,
        colors.chrome.search_label,
        colors.chrome.search_query,
    );

    let level_items = levels.iter().enumerate().map(|(i, &level)| {
        let marker = if filter.is_level_hidden(level) {
            "[ ]"
        } else {
            "[x]"
        };
        let lc = level_color(level, colors);
        let style = if filter.cursor() == i {
            Style::default().fg(lc).add_modifier(Modifier::REVERSED)
        } else {
            Style::default().fg(lc)
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
            ListItem::new(popup_hint)
                .style(Style::default().fg(colors.chrome.hint_text)),
        ))
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, popup);
}

fn render_port_selector(
    frame: &mut Frame,
    area: Rect,
    sel: &crate::port::Selector,
    app: &App,
) {
    let hint_color = app.config().colors.chrome.hint_text;
    let hint = format!(
        " [{}/{}] navigate  [Enter] connect  [Esc] / [{}] close",
        app.key_display(MappableAction::ScrollUp),
        app.key_display(MappableAction::ScrollDown),
        app.key_display(MappableAction::ScanPorts),
    );

    let ports = sel.ports();
    let height = (u16::try_from(ports.len())
        .unwrap_or(u16::MAX)
        .saturating_add(3))
    .max(4)
    .min(area.height);
    let width =
        (u16::try_from(hint.chars().count()).unwrap_or(50) + 4).min(area.width);
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
            ListItem::new(hint.as_str()).style(Style::default().fg(hint_color)),
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
    use crate::config::Config;

    fn app() -> App {
        App::new(None, Config::default())
    }

    fn app_with_port(port: &str) -> App {
        App::new(Some(port.into()), Config::default())
    }

    fn render(app: &App) {
        let backend = TestBackend::new(120, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|f| super::draw(f, app)).unwrap();
    }

    #[test]
    fn draw_empty_app_does_not_panic() {
        render(&app());
    }

    #[test]
    fn draw_with_log_entries_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.push_line("I (1) wifi: Connected");
        app.push_line("E (1) i2c: Timeout");
        app.push_line("some raw line");
        render(&app);
    }

    #[test]
    fn draw_with_filter_popup_open_does_not_panic() {
        let mut app = app();
        app.push_line("I (1) wifi: msg");
        app.filter_mut().toggle_popup();
        render(&app);
    }

    #[test]
    fn draw_with_port_selector_open_does_not_panic() {
        let mut app = app();
        app.open_port_selector(vec!["COM1".into(), "COM2".into()]);
        render(&app);
    }

    #[test]
    fn draw_with_erase_confirm_open_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.open_erase_confirm();
        render(&app);
    }

    #[test]
    fn draw_with_quit_confirm_open_does_not_panic() {
        let mut app = app();
        app.open_quit_confirm();
        render(&app);
    }

    #[test]
    fn draw_with_quit_confirm_open_while_flashing_does_not_panic() {
        let mut app = app_with_port("COM1");
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
        let mut app = app();
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
        let mut app = app();
        app.open_elf_selector(None);
        for ch in format!("{}/fw", dir.display()).chars() {
            app.handle_key(key(KeyCode::Char(ch)));
        }
        app.handle_key(key(KeyCode::Tab));
        render(&app);
    }

    #[test]
    fn draw_with_flash_state_flashing_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Flashing {
            addr: 0x1000,
            current: 512,
            total: 1024,
        });
        render(&app);
    }

    #[test]
    fn draw_with_flash_state_flashing_and_status_overlay_does_not_panic() {
        let mut app = app_with_port("COM1");
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
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Erasing);
        render(&app);
    }

    #[test]
    fn draw_with_flash_state_erasing_and_status_overlay_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Erasing);
        app.set_status("Operation already in progress.");
        render(&app);
    }

    #[test]
    fn draw_with_flash_state_reconnecting_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.set_flash_state(crate::flash::State::Reconnecting);
        app.set_status("Flash complete. Reconnecting...");
        render(&app);
    }

    #[test]
    fn draw_with_device_info_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.set_device_info(crate::flash::DeviceInfo::new(
            "ESP32-S3 (rev v0.1)",
            "4MB",
            "AA:BB:CC:DD:EE:FF",
        ));
        render(&app);
    }

    #[test]
    fn draw_with_elf_path_set_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.set_elf_path(std::path::PathBuf::from("/tmp/firmware.elf"));
        render(&app);
    }

    #[test]
    fn draw_inspector_connected_no_agent_does_not_panic() {
        render(&app_with_port("COM1"));
    }

    #[test]
    fn draw_inspector_with_agent_frame_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.push_line(
            "V (1) esp_agent: heap=100000/200000 min=50000 frag=10000 \
             iram=40000 psram=0 cpu=23,45 tasks=main:R:3200:1,wifi:B:1800:5",
        );
        render(&app);
    }

    #[test]
    fn draw_inspector_with_psram_and_wifi_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.push_line(
            "V (1) esp_agent: heap=100000/200000 min=50000 frag=10000 \
             iram=0 psram=524288 cpu=90 wifi=-65 nvs=45/512 tasks=",
        );
        render(&app);
    }

    #[test]
    fn draw_inspector_with_agent_startup_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.push_line(
            "V (1) esp_agent: start reason=poweron chip=esp32s3 cores=2 \
             rev=1 mac=AA:BB:CC:DD:EE:FF flash=0x400000",
        );
        render(&app);
    }

    #[test]
    fn draw_inspector_inspector_pane_focused_does_not_panic() {
        use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
        let mut app = app_with_port("COM1");
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
    fn clip_title_returns_borrowed_when_fits() {
        let title = " Status ";
        let result = super::clip_title(title, 20);
        assert_eq!(result, title);
    }

    #[test]
    fn clip_title_truncates_when_area_too_narrow() {
        let result = super::clip_title(" Serial Monitor ", 12);
        assert!(result.ends_with('…'));
        assert!(result.len() < " Serial Monitor ".len());
    }

    #[test]
    fn clip_title_zero_width_area_yields_ellipsis() {
        assert_eq!(super::clip_title(" System Inspector ", 0), "…");
    }

    #[test]
    fn clip_title_area_of_two_yields_ellipsis() {
        assert_eq!(super::clip_title(" System Inspector ", 2), "…");
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

    #[test]
    fn reset_reason_label_covers_all_variants() {
        use esp_agent_msg::ResetReason;
        let cases = [
            (ResetReason::PowerOn, "PowerOn"),
            (ResetReason::Software, "Software"),
            (ResetReason::Panic, "Panic"),
            (ResetReason::IntWatchdog, "IntWatchdog"),
            (ResetReason::TaskWatchdog, "TaskWatchdog"),
            (ResetReason::Watchdog, "Watchdog"),
            (ResetReason::Brownout, "Brownout"),
            (ResetReason::DeepSleep, "DeepSleep"),
            (ResetReason::External, "External"),
            (ResetReason::Unknown, "Unknown"),
        ];
        for (reason, expected) in cases {
            assert_eq!(super::reset_reason_label(reason), expected);
        }
    }

    #[test]
    fn format_uptime_seconds_only() {
        assert_eq!(super::format_uptime(45_000), "45s");
    }

    #[test]
    fn format_uptime_minutes_and_seconds() {
        assert_eq!(super::format_uptime(125_000), "2m 5s");
    }

    #[test]
    fn format_uptime_hours_minutes_seconds() {
        assert_eq!(super::format_uptime(3_723_000), "1h 2m 3s");
    }

    #[test]
    fn format_uptime_zero() {
        assert_eq!(super::format_uptime(0), "0s");
    }

    #[test]
    fn format_uptime_exactly_one_hour() {
        assert_eq!(super::format_uptime(3_600_000), "1h 0m 0s");
    }

    #[test]
    fn sparkline_str_empty_data_is_all_spaces() {
        let data = std::collections::VecDeque::new();
        let s = super::sparkline_str(&data, 100, 10);
        assert_eq!(s, "          ");
        assert_eq!(s.len(), 10);
    }

    #[test]
    fn sparkline_str_full_value_is_max_char() {
        let data: std::collections::VecDeque<u32> = vec![100].into();
        let s = super::sparkline_str(&data, 100, 1);
        assert_eq!(s, "█");
    }

    #[test]
    fn sparkline_str_zero_value_is_min_char() {
        let data: std::collections::VecDeque<u32> = vec![0].into();
        let s = super::sparkline_str(&data, 100, 1);
        assert_eq!(s, " ");
    }

    #[test]
    fn sparkline_str_ascending_data_newest_shown_first() {
        // data: oldest=0 .. newest=100; after reversal left=newest=100, right=oldest=0
        let data: std::collections::VecDeque<u32> = vec![0, 25, 50, 75, 100].into();
        let s = super::sparkline_str(&data, 100, 5);
        let chars: Vec<char> = s.chars().collect();
        for window in chars.windows(2) {
            assert!(window[0] >= window[1], "not descending (newest-left): {s}");
        }
    }

    #[test]
    fn sparkline_str_pads_right_when_less_data_than_width() {
        // newest item appears at position 0 (left); padding fills the right
        let data: std::collections::VecDeque<u32> = vec![100].into();
        let s = super::sparkline_str(&data, 100, 5);
        assert_eq!(s.chars().count(), 5);
        assert_eq!(s.chars().next().unwrap(), '█');
        assert_eq!(&s[3..], "    ");
    }

    #[test]
    fn sparkline_str_keeps_newest_when_truncating() {
        // only the two newest items (100, 0) fit; newest is on the left
        let data: std::collections::VecDeque<u32> = vec![0, 0, 0, 100].into();
        let s = super::sparkline_str(&data, 100, 2);
        assert_eq!(s.chars().count(), 2);
        assert_eq!(s.chars().next().unwrap(), '█');
    }

    #[test]
    fn draw_inspector_with_wifi_channel_does_not_panic() {
        let mut app = app_with_port("COM1");
        app.push_line(
            "V (5000) esp_agent: heap=100000/200000 min=50000 frag=8000 \
             iram=0 psram=0 cpu=30 wifi=-65 wifi_ch=6 tasks=",
        );
        render(&app);
    }

    #[test]
    fn draw_inspector_with_sparkline_history_does_not_panic() {
        let mut app = app_with_port("COM1");
        for _ in 0..10 {
            app.push_line(
                "V (1000) esp_agent: heap=100000/200000 min=50000 frag=8000 \
                 iram=0 psram=0 cpu=50 tasks=",
            );
        }
        render(&app);
    }
}
