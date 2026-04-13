use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Layout, Position, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text as TuiText},
    widgets::{Paragraph, Wrap},
};
use std::io::{self, Stdout};

use crate::{
    app::{App, FocusArea, TranscriptScrollMode},
    transcript::{build_transcript_lines, wrapped_line_count},
};

/// Owns terminal setup, rendering, and cleanup for the Mirage TUI.
pub(crate) struct Tui {
    terminal: Terminal<CrosstermBackend<Stdout>>,
    mouse_capture_enabled: bool,
}

impl Tui {
    /// Enters the alternate screen, enables raw mode, and constructs the TUI wrapper.
    pub(crate) fn new() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
        let terminal = Terminal::new(CrosstermBackend::new(stdout))?;
        Ok(Self {
            terminal,
            mouse_capture_enabled: true,
        })
    }

    /// Renders one application frame and updates mouse-capture state.
    pub(crate) fn draw(&mut self, app: &mut App) -> io::Result<()> {
        self.set_mouse_capture(!app.selection_mode)?;
        self.terminal.draw(|frame| render(frame, app))?;
        Ok(())
    }

    /// Enables or disables terminal mouse capture to support native text selection mode.
    fn set_mouse_capture(&mut self, enabled: bool) -> io::Result<()> {
        if self.mouse_capture_enabled == enabled {
            return Ok(());
        }

        if enabled {
            execute!(self.terminal.backend_mut(), EnableMouseCapture)?;
        } else {
            execute!(self.terminal.backend_mut(), DisableMouseCapture)?;
        }

        self.mouse_capture_enabled = enabled;
        Ok(())
    }
}

impl Drop for Tui {
    /// Restores terminal state when the TUI is dropped.
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(
            self.terminal.backend_mut(),
            LeaveAlternateScreen,
            DisableMouseCapture
        );
        let _ = self.terminal.show_cursor();
    }
}

/// Renders the entire Mirage terminal UI for the current application state.
fn render(frame: &mut Frame, app: &mut App) {
    let area = centered_content_area(frame.area());
    let [
        header_area,
        transcript_area,
        divider_area,
        composer_area,
        footer_area,
    ] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Min(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
    ])
    .areas(area);

    let [header_top_area, header_bottom_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(header_area);
    let [header_top_left_area, header_top_right_area] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(12)]).areas(header_top_area);
    let [header_bottom_left_area, header_bottom_right_area] =
        Layout::horizontal([Constraint::Min(1), Constraint::Length(18)]).areas(header_bottom_area);

    let header_top_left = Paragraph::new(Line::from(vec![
        Span::styled("Mirage", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled(
            app.service.model().to_owned(),
            Style::default().fg(Color::Cyan),
        ),
        Span::raw("  "),
        Span::styled(
            truncate_for_width(
                &format!("Backend: {}", app.backend_description),
                header_top_left_area.width.saturating_sub(2),
            ),
            Style::default().fg(Color::Gray),
        ),
    ]));
    frame.render_widget(header_top_left, header_top_left_area);

    let header_top_right = Paragraph::new(Line::from(Span::styled(
        if app.service.uncensored() {
            "uncensored"
        } else {
            "guarded"
        },
        if app.service.uncensored() {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default().fg(Color::DarkGray)
        },
    )))
    .alignment(Alignment::Right);
    frame.render_widget(header_top_right, header_top_right_area);

    let header_bottom_left = Paragraph::new(Line::from(Span::styled(
        truncate_for_width(&app.service.session().status, header_bottom_left_area.width),
        Style::default().fg(Color::Gray),
    )));
    frame.render_widget(header_bottom_left, header_bottom_left_area);

    let usage_text = app
        .service
        .session()
        .usage
        .map(|usage| {
            format!(
                "{} in / {} out",
                format_token_count(usage.input_tokens),
                format_token_count(usage.output_tokens)
            )
        })
        .unwrap_or_default();
    let header_bottom_right = Paragraph::new(Line::from(Span::styled(
        truncate_for_width(&usage_text, header_bottom_right_area.width),
        Style::default().fg(Color::DarkGray),
    )))
    .alignment(Alignment::Right);
    frame.render_widget(header_bottom_right, header_bottom_right_area);

    let rendered_transcript = build_transcript_lines(
        &app.service.session().transcript,
        matches!(app.focus, FocusArea::Transcript)
            .then_some(app.selected_transcript)
            .filter(|_| matches!(app.focus, FocusArea::Transcript)),
    );
    app.last_transcript_area = transcript_area;
    let transcript_height = transcript_area.height;
    let transcript_visual_height =
        wrapped_line_count(&rendered_transcript.lines, transcript_area.width);
    let transcript_max_scroll = transcript_visual_height.saturating_sub(transcript_height);
    let selection_scroll = rendered_transcript
        .selected_line_index
        .map(|line_index| {
            let selected_visual_start = wrapped_line_count(
                &rendered_transcript.lines[..line_index],
                transcript_area.width,
            );
            selected_visual_start.saturating_sub(1)
        })
        .unwrap_or(0);
    let transcript_scroll = match app.transcript_scroll_mode {
        TranscriptScrollMode::FollowTail => transcript_max_scroll,
        TranscriptScrollMode::FollowSelection => selection_scroll.min(transcript_max_scroll),
        TranscriptScrollMode::Manual => app.transcript_scroll.min(transcript_max_scroll),
    };
    app.last_transcript_scroll = transcript_scroll;
    app.last_transcript_max_scroll = transcript_max_scroll;
    app.last_transcript_page_height = transcript_height;
    let transcript = Paragraph::new(TuiText::from(rendered_transcript.lines))
        .wrap(Wrap { trim: false })
        .scroll((transcript_scroll, 0));
    frame.render_widget(transcript, transcript_area);

    let divider = Paragraph::new(Line::from(Span::styled(
        "─".repeat(divider_area.width as usize),
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(divider, divider_area);

    let composer_prompt = if app.service.session().streaming {
        "… "
    } else {
        "> "
    };
    let prompt_width = composer_prompt.chars().count() as u16;
    let composer_width = composer_area.width.saturating_sub(prompt_width);
    let (visible_input, cursor_offset) = app.input_view(composer_width);
    let composer_text = if visible_input.is_empty() && !app.service.session().streaming {
        Line::from(vec![
            Span::styled(
                composer_prompt,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("Message Mirage...", Style::default().fg(Color::DarkGray)),
        ])
    } else {
        Line::from(vec![
            Span::styled(
                composer_prompt,
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(visible_input),
        ])
    };
    let composer = Paragraph::new(composer_text);
    frame.render_widget(composer, composer_area);

    let footer = Paragraph::new(Line::from(Span::styled(
        if app.selection_mode {
            "Selection mode: drag to select text, use terminal copy, Ctrl+G or Esc returns"
        } else {
            "Ctrl+G selection mode, Tab focus, PageUp/PageDown scroll, y copy item, Y copy all, Esc quits"
        },
        Style::default().fg(Color::DarkGray),
    )));
    frame.render_widget(footer, footer_area);

    if !app.service.session().streaming && matches!(app.focus, FocusArea::Composer) {
        let cursor_x = composer_area.x + prompt_width + cursor_offset;
        let cursor_y = composer_area.y;
        frame.set_cursor_position(Position::new(cursor_x, cursor_y));
    }
}

/// Narrows the terminal frame to a centered content column for a chat-like layout.
fn centered_content_area(area: Rect) -> Rect {
    let horizontal_margin = if area.width > 112 {
        (area.width - 104) / 2
    } else {
        3.min(area.width.saturating_sub(1) / 2)
    };
    let vertical_margin = 1.min(area.height.saturating_sub(1) / 2);

    Rect::new(
        area.x + horizontal_margin,
        area.y + vertical_margin,
        area.width.saturating_sub(horizontal_margin * 2),
        area.height.saturating_sub(vertical_margin * 2),
    )
}

/// Truncates text so it fits within a single visual row.
fn truncate_for_width(value: &str, width: u16) -> String {
    let width = width as usize;
    if width == 0 {
        return String::new();
    }
    let char_count = value.chars().count();
    if char_count <= width {
        return value.to_owned();
    }
    if width == 1 {
        return ".".to_owned();
    }
    let kept = value.chars().take(width - 1).collect::<String>();
    format!("{kept}.")
}

/// Formats token counts compactly so the header stays visually stable.
fn format_token_count(value: u64) -> String {
    match value {
        0..=999 => value.to_string(),
        1_000..=9_999 => format!("{:.1}k", value as f64 / 1_000.0),
        10_000..=999_999 => format!("{}k", value / 1_000),
        1_000_000..=9_999_999 => format!("{:.1}m", value as f64 / 1_000_000.0),
        _ => format!("{}m", value / 1_000_000),
    }
}
