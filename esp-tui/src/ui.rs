use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Clear, List, ListItem, Paragraph, Wrap,
};
use ratatui::Frame;

use crate::app::{App, PortSelector};

/// Renders the full TUI to the given frame.
///
/// # Arguments
///
/// * `frame` - The ratatui frame to render into.
/// * `app` - Shared reference to the current application state.
pub fn draw(frame: &mut Frame, app: &App) {
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
    render_inspector(frame, main[1]);
    render_status_bar(frame, outer[2], app);

    if let Some(sel) = app.port_selector() {
        render_port_selector(frame, frame.area(), sel);
    } else if app.filter().is_popup_open() {
        render_filter_popup(frame, frame.area(), app);
    }
}

fn render_menu_bar(frame: &mut Frame, area: Rect, app: &App) {
    let port_label = app
        .port_name()
        .map_or_else(|| "Port: none".to_owned(), |p| format!("Port: {p}"));

    let left = Line::from(vec![
        hint("C", "onnect"),
        Span::raw("  "),
        hint("D", "isconnect"),
        Span::raw("  "),
        hint("F", "lash"),
        Span::raw("  "),
        hint("R", "eset"),
        Span::raw("  "),
        hint("E", "rase"),
        Span::raw("  "),
        hint("Q", "uit"),
        Span::raw("  "),
        hint("Tab", "Filter"),
    ]);

    let right_len = u16::try_from(port_label.len()).unwrap_or(u16::MAX);
    let right =
        Line::from(Span::styled(port_label, Style::default().fg(Color::Cyan)));
    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(right_len)])
            .areas(area);

    frame.render_widget(Paragraph::new(left), left_area);
    frame.render_widget(
        Paragraph::new(right).alignment(Alignment::Right),
        right_area,
    );
}

fn hint<'a>(key: &'a str, label: &'a str) -> Span<'a> {
    Span::styled(
        format!("[{key}]{label}"),
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

    let lines: Vec<Line> = entries
        .iter()
        .map(|e| {
            if e.tag().is_empty() {
                Line::from(Span::styled(
                    e.message().to_owned(),
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
                    Span::raw(e.message().to_owned()),
                ])
            }
        })
        .collect();

    frame.render_widget(
        Paragraph::new(lines).wrap(Wrap { trim: false }),
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
            "[↑/↓  PgUp/PgDn] scroll",
            Style::default().fg(Color::DarkGray),
        ))
    };
    frame.render_widget(Paragraph::new(footer), footer_area);
}

fn render_inspector(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" System Inspector ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new("(Phase 3)").style(Style::default().fg(Color::DarkGray)),
        inner,
    );
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let block = Block::default()
        .title(" Status ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(area);
    frame.render_widget(block, area);

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

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn render_filter_popup(frame: &mut Frame, area: Rect, app: &App) {
    use crate::filter;
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

    let mut items: Vec<ListItem> = Vec::new();

    items.push(ListItem::new(" Severity").style(section_style));
    for (i, &level) in levels.iter().enumerate() {
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
        items.push(
            ListItem::new(format!("  {marker} {}", level.label())).style(style),
        );
    }

    if !tags.is_empty() {
        items.push(ListItem::new(" Tags").style(section_style));
        for (i, tag) in tags.iter().enumerate() {
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
            items.push(ListItem::new(format!("  {marker} {tag}")).style(style));
        }
    }

    items.push(
        ListItem::new(" [Space] toggle  [^A] toggle all  [q/Esc] close")
            .style(Style::default().fg(Color::DarkGray)),
    );

    let list = List::new(items).block(block);
    frame.render_widget(list, popup);
}

fn render_port_selector(frame: &mut Frame, area: Rect, sel: &PortSelector) {
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

    let mut items: Vec<ListItem> = ports
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
        .collect();

    items.push(ListItem::new(HINT).style(Style::default().fg(Color::DarkGray)));

    let list = List::new(items).block(block);
    frame.render_widget(list, popup);
}

#[cfg(test)]
mod tests {
    use ratatui::layout::Rect;

    use super::centered_rect;

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
        assert_eq!(r.x, 40); // 10 + (80-20)/2
        assert_eq!(r.y, 20); // 5 + (40-10)/2
        assert_eq!(r.width, 20);
        assert_eq!(r.height, 10);
    }
}
