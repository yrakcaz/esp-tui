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

    render_status_bar(frame, outer[0], app);
    render_monitor(frame, main[0], app);
    render_inspector(frame, main[1]);
    render_flash_bar(frame, outer[2]);

    if let Some(sel) = app.port_selector() {
        render_port_selector(frame, frame.area(), sel);
    } else if app.filter().is_popup_open() {
        render_filter_popup(frame, frame.area(), app);
    }
}

fn render_status_bar(frame: &mut Frame, area: Rect, app: &App) {
    let port_label = app
        .port_name()
        .map_or_else(|| "Port: none".to_owned(), |p| format!("Port: {p}"));

    let status = app.status_msg().unwrap_or("");

    let left = Line::from(vec![
        hint("r", "Reset"),
        Span::raw("  "),
        hint("f", "Flash"),
        Span::raw("  "),
        hint("e", "Erase"),
        Span::raw("  "),
        hint("c", "Connect"),
        Span::raw("  "),
        hint("Tab", "Filter"),
        if status.is_empty() {
            Span::raw("")
        } else {
            Span::styled(format!("  {status}"), Style::default().fg(Color::Yellow))
        },
    ]);

    let right =
        Line::from(Span::styled(port_label, Style::default().fg(Color::Cyan)));

    let [left_area, right_area] =
        Layout::horizontal([Constraint::Min(0), Constraint::Length(30)]).areas(area);

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

    let height = inner.height as usize;
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

    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
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

fn render_flash_bar(frame: &mut Frame, area: Rect) {
    let block = Block::default()
        .title(" Flash Progress ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    frame.render_widget(
        Paragraph::new("(Phase 2)").style(Style::default().fg(Color::DarkGray)),
        inner,
    );
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn render_filter_popup(frame: &mut Frame, area: Rect, app: &App) {
    let filter = app.filter();
    let tags = filter.known_tags();
    let height = (u16::try_from(tags.len())
        .unwrap_or(u16::MAX)
        .saturating_add(4))
    .max(6)
    .min(area.height);
    let popup = centered_rect(40, height, area);

    frame.render_widget(Clear, popup);

    let block = Block::default()
        .title(" Filter by Tag ")
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded);

    let items: Vec<ListItem> = tags
        .iter()
        .enumerate()
        .map(|(i, tag)| {
            let marker = if filter.is_tag_hidden(tag) {
                "[ ]"
            } else {
                "[x]"
            };
            let style = if i == filter.cursor() {
                Style::default().add_modifier(Modifier::REVERSED)
            } else {
                Style::default()
            };
            ListItem::new(format!("  {marker} {tag}")).style(style)
        })
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, popup);
}

fn render_port_selector(frame: &mut Frame, area: Rect, sel: &PortSelector) {
    let ports = sel.ports();
    let height = (u16::try_from(ports.len())
        .unwrap_or(u16::MAX)
        .saturating_add(4))
    .max(5)
    .min(area.height);
    let popup = centered_rect(44, height, area);

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
        .collect();

    let list = List::new(items).block(block);
    frame.render_widget(list, popup);
}
