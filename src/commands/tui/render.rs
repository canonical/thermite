//! ratatui rendering for the TUI.
//!
//! Pure drawing code: no I/O, no state mutation. All layout/styling decisions
//! live here so `app.rs` can stay terminal-independent.

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols::border;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap};

use super::app::{App, Mode, UiStyle};

/// ASCII border set.
///
/// Plain `+ - |` borders: no UTF-8 required, and screen readers enumerate a
/// run of identical dashes/pipes as one word instead of lookalike glyphs.
const ASCII_BORDER: border::Set<'static> = border::Set {
    top_left: "+",
    top_right: "+",
    bottom_left: "+",
    bottom_right: "+",
    vertical_left: "|",
    vertical_right: "|",
    horizontal_top: "-",
    horizontal_bottom: "-",
};

/// Build a bordered, titled block for the given UI style.
///
/// - `Ascii`: the `+ - |` set.
/// - `Polished`: rounded Unicode box-drawing borders in a dimmed style.
///
/// The accessible style never reaches the ratatui loop: it is a linear
/// plain-text REPL (see [`super::repl`]).
fn block_for(style: UiStyle, title: &str) -> Block<'static> {
    match style {
        UiStyle::Ascii => Block::default()
            .title(title.to_owned())
            .borders(Borders::ALL)
            .border_set(ASCII_BORDER),
        UiStyle::Polished => Block::default()
            .title(title.to_owned())
            .borders(Borders::ALL)
            .border_set(border::ROUNDED)
            .border_style(Style::default().fg(Color::DarkGray)),
        UiStyle::Accessible => unreachable!("accessible style runs the REPL, not the ratatui loop"),
    }
}

/// Draw one frame.
pub fn draw(f: &mut Frame, app: &mut App) {
    let info_lines = app.info.display_lines();
    let info_height = (info_lines.len() as u16 + 2).min(f.area().height / 2);

    let outer = Layout::vertical([
        Constraint::Length(info_height),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .split(f.area());

    draw_info(f, outer[0], app, &info_lines);

    let middle = Layout::horizontal([
        Constraint::Min(0),
        Constraint::Length(max_command_width() + 2),
    ])
    .split(outer[1]);
    draw_log(f, middle[0], app);
    draw_command_menu(f, middle[1], app);
    draw_status_bar(f, outer[2], app);

    match &app.mode {
        Mode::Normal => {}
        Mode::Menu(menu) => draw_menu_modal(f, f.area(), app.style, menu.title.as_str(), menu),
        Mode::Input(input) => draw_input_modal(f, f.area(), app.style, input),
    }
}

/// The static command list shown in the sidebar.
const COMMANDS: &[(&str, &str)] = &[
    ("b", "build (sbuild)"),
    ("S", "source build"),
    ("l", "lintian"),
    ("v", "bump version"),
    ("V", "set version"),
    ("t", "tarballs"),
    ("g", "git fetch"),
    ("R", "refresh info"),
    ("q", "quit"),
];

/// Sidebar label for `q` while a command is running (cancels instead of quit).
const BUSY_Q_LABEL: &str = "cancel running";

/// Sidebar width: the widest command label plus the `key + label` padding.
/// Accounts for the busy-state `q` label ("cancel running"), which is longer
/// than any idle label.
fn max_command_width() -> u16 {
    COMMANDS
        .iter()
        .map(|(key, label)| key.chars().count() + label.chars().count() + 3)
        .chain(std::iter::once(
            "q".chars().count() + BUSY_Q_LABEL.len() + 3,
        ))
        .max()
        .unwrap_or(20) as u16
}

/// Info pane: package identity, parsed version, branch, repo path.
fn draw_info(f: &mut Frame, area: Rect, app: &App, info_lines: &[String]) {
    let title = if app.info.error.is_some() {
        " Package info (error) "
    } else {
        " Package info "
    };
    let block = block_for(app.style, title);
    let lines: Vec<Line> = info_lines.iter().map(|l| Line::from(l.as_str())).collect();
    f.render_widget(Paragraph::new(lines).block(block), area);
}

/// Log pane: streamed command output, scrollable.
fn draw_log(f: &mut Frame, area: Rect, app: &App) {
    let title = match app.log_scroll {
        0 => " Output ".to_owned(),
        n => format!(" Output (scrolled back {n} lines, End to follow) "),
    };
    let block = block_for(app.style, &title);

    let height = area.height.saturating_sub(2) as usize;
    let end = app.log.len().saturating_sub(app.log_scroll);
    let start = end.saturating_sub(height);
    let lines: Vec<Line> = app.log[start..end]
        .iter()
        .map(|l| Line::from(l.as_str()))
        .collect();
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            // Long lines are truncated horizontally; wrapping the tail of a
            // streaming log makes the window jumpy.
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Command sidebar.
fn draw_command_menu(f: &mut Frame, area: Rect, app: &App) {
    let dim = Style::default().fg(Color::DarkGray);
    let bold = Style::default().add_modifier(Modifier::BOLD);
    let busy = app.busy.is_some();
    let lines: Vec<Line> = COMMANDS
        .iter()
        .map(|(key, label)| {
            // `q` cancels while a command is running, quits when idle.
            let (key, label) = if busy && *key == "q" {
                (*key, BUSY_Q_LABEL)
            } else {
                (*key, *label)
            };
            // While busy, only `q` is actionable; the rest are dimmed.
            let actionable = !busy || key == "q";
            let key_style = if actionable { bold } else { dim };
            Line::from(vec![
                Span::styled(format!(" {key} "), key_style),
                Span::raw(" "),
                Span::styled(label, if actionable { Style::default() } else { dim }),
            ])
        })
        .collect();
    let block = block_for(app.style, " Commands ");
    f.render_widget(
        List::new(lines.iter().cloned().map(ListItem::new)).block(block),
        area,
    );
}

/// Status bar: hints, busy indicator, last outcome.
fn draw_status_bar(f: &mut Frame, area: Rect, app: &App) {
    let mut spans = Vec::new();
    match &app.busy {
        Some(desc) => {
            spans.push(Span::styled(
                format!(" running: {desc} (q: cancel) "),
                Style::default().fg(Color::Black).bg(Color::Yellow),
            ));
        }
        None => {
            spans.push(Span::styled(
                " idle ",
                Style::default().fg(Color::Black).bg(Color::Green),
            ));
        }
    }
    spans.push(Span::raw(" PgUp/PgDn: scroll  End: follow"));
    if let Some(status) = &app.last_status {
        let style = if status.contains("FAILED") {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::Green)
        };
        spans.push(Span::styled(format!("  {status}"), style));
    }
    f.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// Popup for a selection menu.
fn draw_menu_modal(
    f: &mut Frame,
    area: Rect,
    style: UiStyle,
    title: &str,
    menu: &super::app::MenuModal,
) {
    let width = menu
        .options
        .iter()
        .map(|o| o.chars().count())
        .max()
        .unwrap_or(20)
        .min(70) as u16;
    let height = menu.options.len() as u16 + 2;
    let modal = centered_rect(width + 4, height, area);

    f.render_widget(Clear, modal);
    let block = block_for(
        style,
        &format!(" {title} (up/down, Enter to run, Esc to cancel) "),
    );
    let items: Vec<ListItem> = menu
        .options
        .iter()
        .map(|o| ListItem::new(o.as_str()))
        .collect();
    let list = List::new(items).block(block).highlight_style(
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
    );
    let mut state = ListState::default().with_selected(Some(menu.selected));
    f.render_stateful_widget(list, modal, &mut state);
}

/// Popup for a single-line text input.
fn draw_input_modal(f: &mut Frame, area: Rect, style: UiStyle, input: &super::app::InputModal) {
    let width = (input.title.chars().count() + input.value.chars().count() + 8)
        .min(area.width.saturating_sub(4) as usize) as u16;
    let modal = centered_rect(width, 6, area);

    f.render_widget(Clear, modal);
    let block = block_for(style, &format!(" {} ", input.title));

    let mut lines = vec![Line::from(vec![
        Span::raw("> "),
        Span::styled(
            input.value.as_str(),
            Style::default().add_modifier(Modifier::BOLD),
        ),
        // Visible cursor at the end of the value.
        Span::styled("_", Style::default().fg(Color::Cyan)),
    ])];
    if let Some(error) = &input.error {
        lines.push(Line::styled(
            error.as_str(),
            Style::default().fg(Color::Red),
        ));
    }
    lines.push(Line::styled(
        "Enter: submit  |  Esc: cancel",
        Style::default().fg(Color::DarkGray),
    ));
    f.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        modal,
    );
}

/// A centered rectangle of the given size (clamped to the area).
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width.saturating_sub(2));
    let height = height.min(area.height.saturating_sub(2));
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect {
        x,
        y,
        width,
        height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::buffer::Buffer;
    use ratatui::widgets::Widget;

    /// Render a block into a 10x3 buffer and return its corner symbols.
    fn render_block(style: UiStyle, title: &str) -> Buffer {
        let mut buf = Buffer::empty(Rect::new(0, 0, 10, 3));
        block_for(style, title).render(Rect::new(0, 0, 10, 3), &mut buf);
        buf
    }

    #[test]
    fn ascii_style_renders_plus_minus_borders() {
        let buf = render_block(UiStyle::Ascii, " t ");
        assert_eq!(buf[(0, 0)].symbol(), "+");
        assert_eq!(buf[(9, 0)].symbol(), "+");
        assert_eq!(buf[(0, 2)].symbol(), "+");
        assert_eq!(buf[(9, 2)].symbol(), "+");
        assert_eq!(buf[(5, 0)].symbol(), "-");
    }

    #[test]
    fn polished_style_renders_rounded_borders() {
        let buf = render_block(UiStyle::Polished, " t ");
        assert_eq!(buf[(0, 0)].symbol(), "╭");
        assert_eq!(buf[(9, 0)].symbol(), "╮");
        assert_eq!(buf[(0, 2)].symbol(), "╰");
        assert_eq!(buf[(9, 2)].symbol(), "╯");
        assert_eq!(buf[(5, 0)].symbol(), "─");
    }

    #[test]
    fn ascii_style_title_is_rendered() {
        let buf = render_block(UiStyle::Ascii, " t ");
        assert!(buf.content().iter().any(|c| c.symbol() == "t"));
    }
}
