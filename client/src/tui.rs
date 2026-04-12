use crossterm::{
    event::{DisableMouseCapture, EnableMouseCapture},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout, Position, Rect},
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

    let status_text = app
        .service
        .session()
        .usage
        .map(|usage| format!("  {} in / {} out", usage.input_tokens, usage.output_tokens))
        .unwrap_or_default();
    let mode_style = if app.service.session().streaming {
        Style::default().fg(Color::Yellow)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let header = Paragraph::new(TuiText::from(vec![
        Line::from(vec![
            Span::styled("Mirage", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("  ", Style::default()),
            Span::styled(app.service.model(), Style::default().fg(Color::Cyan)),
            Span::styled("  ", Style::default()),
            Span::styled(
                if app.service.session().streaming {
                    "streaming"
                } else {
                    "ready"
                },
                mode_style,
            ),
            Span::styled("  ", Style::default()),
            Span::styled(
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
            ),
            Span::raw(status_text),
        ]),
        Line::from(Span::styled(
            format!(
                "{}  Backend: {}  Focus: {}  Selection: {}",
                app.service.session().status,
                app.backend_description,
                match app.focus {
                    FocusArea::Composer => "composer",
                    FocusArea::Transcript => "transcript",
                },
                if app.selection_mode { "on" } else { "off" }
            ),
            Style::default().fg(Color::Gray),
        )),
    ]));
    frame.render_widget(header, header_area);

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
