use mirage_core::session::{
    SubagentGroup, SubagentStatus, TranscriptEntry, TranscriptItem, TranscriptKind,
};
use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

pub(crate) struct RenderedTranscript {
    pub(crate) lines: Vec<Line<'static>>,
    pub(crate) selected_line_index: Option<usize>,
}

pub(crate) fn build_transcript_lines(
    entries: &[TranscriptItem],
    selected_index: Option<usize>,
) -> RenderedTranscript {
    let mut lines = Vec::new();
    let mut selected_line_index = None;

    for (index, entry) in entries.iter().enumerate() {
        let is_selected = selected_index == Some(index);
        if is_selected {
            selected_line_index = Some(lines.len());
        }

        match entry {
            TranscriptItem::Entry(entry) => {
                push_entry_lines(&mut lines, entry, is_selected, "", "  ", true);
            }
            TranscriptItem::SubagentGroup(group) => {
                lines.push(Line::from(Span::styled(
                    subagent_group_title(group),
                    selectable_style(subagent_group_style(group), is_selected),
                )));

                if group.expanded {
                    for child in &group.entries {
                        push_entry_lines(&mut lines, child, false, "  ", "    ", false);
                    }
                }

                lines.push(Line::raw(String::new()));
            }
        }
    }

    RenderedTranscript {
        lines,
        selected_line_index,
    }
}

pub(crate) fn wrapped_line_count(lines: &[Line<'_>], width: u16) -> u16 {
    if width == 0 {
        return 0;
    }

    lines
        .iter()
        .map(|line| {
            let visual_width = line.width();
            let wrapped = if visual_width == 0 {
                1
            } else {
                visual_width.div_ceil(width as usize)
            };
            wrapped.min(u16::MAX as usize) as u16
        })
        .sum()
}

pub(crate) fn subagent_group_title(group: &SubagentGroup) -> String {
    let marker = if group.expanded { "[-]" } else { "[+]" };
    let status = match group.status {
        SubagentStatus::Running => "running",
        SubagentStatus::Complete => "complete",
        SubagentStatus::Failed => "failed",
    };
    format!(
        "{marker} Subagent {status} ({} entries): {}",
        group.entries.len(),
        group.summary
    )
}

fn push_entry_lines(
    lines: &mut Vec<Line<'static>>,
    entry: &TranscriptEntry,
    selected: bool,
    title_indent: &str,
    body_indent: &str,
    trailing_blank: bool,
) {
    lines.push(Line::from(vec![
        Span::styled(
            title_indent.to_owned(),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            entry.title.clone(),
            selectable_style(entry_title_style(entry.kind), selected),
        ),
    ]));

    if entry.body.is_empty() {
        lines.push(Line::raw(body_indent.to_owned()));
    } else {
        for line in entry.body.lines() {
            lines.push(Line::from(vec![
                Span::styled(body_indent.to_owned(), Style::default().fg(Color::DarkGray)),
                Span::styled(line.to_owned(), entry_body_style(entry.kind)),
            ]));
        }
    }

    if trailing_blank {
        lines.push(Line::raw(String::new()));
    }
}

fn entry_title_style(kind: TranscriptKind) -> Style {
    match kind {
        TranscriptKind::Meta => Style::default()
            .fg(Color::DarkGray)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::User => Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::Assistant => Style::default()
            .fg(Color::Green)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::Tool => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        TranscriptKind::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}

fn entry_body_style(kind: TranscriptKind) -> Style {
    match kind {
        TranscriptKind::Meta => Style::default().fg(Color::Gray),
        TranscriptKind::Error => Style::default().fg(Color::Red),
        _ => Style::default(),
    }
}

fn selectable_style(style: Style, selected: bool) -> Style {
    if selected {
        style.fg(Color::Black).bg(Color::White)
    } else {
        style
    }
}

fn subagent_group_style(group: &SubagentGroup) -> Style {
    match group.status {
        SubagentStatus::Running => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        SubagentStatus::Complete => Style::default()
            .fg(Color::Magenta)
            .add_modifier(Modifier::BOLD),
        SubagentStatus::Failed => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
    }
}
