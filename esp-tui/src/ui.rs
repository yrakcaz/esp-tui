use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, Gauge, List, ListItem, Paragraph, Wrap,
};
use ratatui::Frame;

use crate::app::App;
use crate::filter;
use crate::flash;

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

    let main =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(outer[1]);

    render_menu_bar(frame, outer[0], app);
    render_monitor(frame, main[0], app);
    render_inspector(frame, main[1], app);
    render_status_bar(frame, outer[2], app);

    if app.is_erase_confirm_open() {
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
    let elf_label: std::borrow::Cow<str> = app
        .elf_path()
        .and_then(|p| p.file_name())
        .and_then(|n| n.to_str())
        .map_or("none".into(), std::borrow::Cow::Borrowed);

    let port_label: std::borrow::Cow<str> = app
        .port_name()
        .map_or("none".into(), std::borrow::Cow::Borrowed);

    let right_text = format!("ELF: {elf_label} | Port: {port_label}");

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
        Line::from(Span::styled(right_text, Style::default().fg(Color::Cyan)));
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_len)])
            .areas(area);

    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(
        Paragraph::new(right).alignment(Alignment::Right),
        right_area,
    );
}

fn hint(text: &'static str) -> Span<'static> {
    Span::styled(
        text,
        Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    )
}

fn render_monitor(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Serial Monitor ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    let [content_area, footer_area] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner);

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

    let footer = if app.scroll() > 0 {
        Line::from(vec![
            Span::styled(
                " SCROLL ",
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::REVERSED | Modifier::BOLD),
            ),
            Span::styled(
                "  q/Esc to follow live",
                Style::default().fg(Color::Yellow),
            ),
        ])
    } else {
        Line::from(Span::styled(
            "[↑/↓  PgUp/PgDn] scroll  [^L] clear",
            Style::default().fg(Color::DarkGray),
        ))
    };
    frame.render_widget(Paragraph::new(footer), footer_area);
}

fn render_inspector(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" System Inspector ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if let Some(info) = app.device_info() {
        let mut lines = vec![
            Line::from(vec![
                Span::styled("Board: ", Style::default().fg(Color::DarkGray)),
                Span::raw(info.chip().to_owned()),
            ]),
            Line::from(vec![
                Span::styled("Flash: ", Style::default().fg(Color::DarkGray)),
                Span::raw(info.flash_size().to_owned()),
            ]),
            Line::from(vec![
                Span::styled("MAC:   ", Style::default().fg(Color::DarkGray)),
                Span::raw(info.mac_address().to_owned()),
            ]),
        ];

        if !info.partitions().is_empty() {
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "Partitions:",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )));
            for p in info.partitions() {
                lines.push(Line::from(format!(
                    "{:<8} {}/{:<8} 0x{:06X}  {}",
                    p.name(),
                    p.partition_type(),
                    p.subtype(),
                    p.offset(),
                    format_bytes(p.size()),
                )));
            }
        }

        frame.render_widget(Paragraph::new(lines), inner);
    } else {
        frame.render_widget(
            Paragraph::new("(Phase 3)").style(Style::default().fg(Color::DarkGray)),
            inner,
        );
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

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Status ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    match app.flash_state() {
        flash::State::Flashing {
            addr,
            current,
            total,
        } => {
            #[allow(clippy::cast_precision_loss)]
            let ratio = if *total == 0 {
                0.0_f64
            } else {
                *current as f64 / *total as f64
            };
            let addr_str = format!(" Writing at 0x{addr:08x}...");
            let pct_str = format!("{:.0}%", ratio * 100.0);
            let width = inner.width as usize;
            // Build a label as wide as the gauge area so the gauge positions
            // it at x=0; every character then goes through the gauge's own
            // colour-inversion logic at the fill boundary for free.
            let pct_start = (width / 2).saturating_sub(pct_str.len() / 2);
            let mid = pct_start.saturating_sub(addr_str.len());
            let right = width.saturating_sub(addr_str.len() + mid + pct_str.len());
            let label = format!("{addr_str}{:mid$}{pct_str}{:right$}", "", "");
            let gauge = Gauge::default()
                .gauge_style(Style::default().fg(Color::Green))
                .ratio(ratio)
                .label(label);
            frame.render_widget(gauge, inner);
        }
        flash::State::Erasing => {
            frame.render_widget(
                Paragraph::new("Erasing flash...")
                    .style(Style::default().fg(Color::Yellow)),
                inner,
            );
        }
        flash::State::Idle => {
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

fn render_elf_selector_popup(frame: &mut Frame, area: Rect, app: &App) {
    let Some(sel) = app.elf_selector() else {
        return;
    };

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

    if inner.height == 0 {
        return;
    }

    let input_area = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };

    let input = sel.value();
    let cursor_byte = sel.cursor_pos();
    let before = &input[..cursor_byte];
    let rest = &input[cursor_byte..];
    let (cursor_str, after) = if let Some(c) = rest.chars().next() {
        let char_len = c.len_utf8();
        (&rest[..char_len], &rest[char_len..])
    } else {
        (" ", "")
    };

    let input_line = Line::from(vec![
        Span::raw(before),
        Span::styled(
            cursor_str,
            Style::default().add_modifier(Modifier::REVERSED),
        ),
        Span::raw(after),
    ]);
    frame.render_widget(Paragraph::new(input_line), input_area);

    let cursor_col = u16::try_from(before.chars().count()).unwrap_or(0);
    if cursor_col < inner.width {
        frame.set_cursor_position((input_area.x + cursor_col, input_area.y));
    }

    if inner.height <= 1 {
        return;
    }

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

fn render_filter_popup(frame: &mut Frame, area: Rect, app: &App) {
    const HINT: &str = " [Space] toggle  [^A] toggle all  [q/Esc] close";

    let filter = app.filter();
    let tags = filter.known_tags();
    let levels = filter::State::levels();
    let tag_rows = u16::try_from(tags.len()).unwrap_or(u16::MAX);
    let height = (2
        + 1
        + u16::try_from(levels.len()).unwrap_or(5)
        + if tags.is_empty() { 0 } else { 1 + tag_rows }
        + 1)
    .min(area.height);
    let width = (u16::try_from(HINT.len()).unwrap_or(60) + 3).min(area.width);
    let popup = centered_rect(width, height, area);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Filter ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);

    let section_style = Style::default()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);

    let items: Vec<ListItem> =
        std::iter::once(ListItem::new(" Severity").style(section_style))
            .chain(levels.iter().enumerate().map(|(i, &level)| {
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
            }))
            .chain(
                (!tags.is_empty())
                    .then_some(ListItem::new(" Tags").style(section_style)),
            )
            .chain(tags.iter().enumerate().map(|(i, tag)| {
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
            }))
            .chain(std::iter::once(
                ListItem::new(HINT).style(Style::default().fg(Color::DarkGray)),
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
    fn draw_demo_app_does_not_panic() {
        let mut app = App::new(None);
        app.set_demo();
        render(&app);
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
        let mut app = App::new(None);
        app.open_elf_selector(None);
        for ch in "/tmp/".chars() {
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
    fn draw_with_flash_state_erasing_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.set_flash_state(crate::flash::State::Erasing);
        render(&app);
    }

    #[test]
    fn draw_with_device_info_does_not_panic() {
        let mut app = App::new(Some("COM1".into()));
        app.set_device_info(crate::flash::DeviceInfo::new(
            "ESP32-S3 (rev v0.1)",
            "4MB",
            "AA:BB:CC:DD:EE:FF",
            Vec::new(),
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
