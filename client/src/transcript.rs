use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

#[derive(Clone, Copy)]
pub(crate) enum TranscriptKind {
    Meta,
    User,
    Assistant,
    Tool,
    Error,
}

pub(crate) struct TranscriptEntry {
    pub(crate) kind: TranscriptKind,
    pub(crate) title: String,
    pub(crate) body: String,
}

impl TranscriptEntry {
    pub(crate) fn meta(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Meta,
            title: title.into(),
            body: body.into(),
        }
    }

    pub(crate) fn user(body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::User,
            title: "You".to_owned(),
            body: body.into(),
        }
    }

    pub(crate) fn assistant(body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Assistant,
            title: "Assistant".to_owned(),
            body: body.into(),
        }
    }

    pub(crate) fn tool(title: impl Into<String>, body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Tool,
            title: title.into(),
            body: body.into(),
        }
    }

    pub(crate) fn error(body: impl Into<String>) -> Self {
        Self {
            kind: TranscriptKind::Error,
            title: "Error".to_owned(),
            body: body.into(),
        }
    }

    pub(crate) fn title_style(&self) -> Style {
        match self.kind {
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

    pub(crate) fn body_style(&self) -> Style {
        match self.kind {
            TranscriptKind::Meta => Style::default().fg(Color::Gray),
            TranscriptKind::Error => Style::default().fg(Color::Red),
            _ => Style::default(),
        }
    }

    pub(crate) fn to_plaintext(&self, title_indent: &str, body_indent: &str) -> String {
        let mut lines = vec![format!("{title_indent}{}", self.title)];
        if self.body.is_empty() {
            return lines.join("\n");
        }

        for line in self.body.lines() {
            lines.push(format!("{body_indent}{line}"));
        }
        lines.join("\n")
    }
}

pub(crate) enum TranscriptItem {
    Entry(TranscriptEntry),
    SubagentGroup(SubagentGroup),
}

impl TranscriptItem {
    pub(crate) fn entry_mut(&mut self) -> Option<&mut TranscriptEntry> {
        match self {
            Self::Entry(entry) => Some(entry),
            Self::SubagentGroup(_) => None,
        }
    }

    pub(crate) fn entry(&self) -> Option<&TranscriptEntry> {
        match self {
            Self::Entry(entry) => Some(entry),
            Self::SubagentGroup(_) => None,
        }
    }

    pub(crate) fn to_plaintext(&self) -> String {
        match self {
            Self::Entry(entry) => entry.to_plaintext("", "  "),
            Self::SubagentGroup(group) => group.to_plaintext(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum SubagentStatus {
    Running,
    Complete,
    Failed,
}

pub(crate) struct SubagentGroup {
    pub(crate) summary: String,
    pub(crate) status: SubagentStatus,
    pub(crate) expanded: bool,
    pub(crate) entries: Vec<TranscriptEntry>,
}

impl SubagentGroup {
    pub(crate) fn new(summary: impl Into<String>) -> Self {
        Self {
            summary: summary.into(),
            status: SubagentStatus::Running,
            expanded: false,
            entries: Vec::new(),
        }
    }

    pub(crate) fn to_plaintext(&self) -> String {
        let mut parts = vec![subagent_group_title(self)];
        for entry in &self.entries {
            parts.push(entry.to_plaintext("  ", "    "));
        }
        parts.join("\n")
    }
}

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
            selectable_style(entry.title_style(), selected),
        ),
    ]));

    if entry.body.is_empty() {
        lines.push(Line::raw(body_indent.to_owned()));
    } else {
        for line in entry.body.lines() {
            lines.push(Line::from(vec![
                Span::styled(body_indent.to_owned(), Style::default().fg(Color::DarkGray)),
                Span::styled(line.to_owned(), entry.body_style()),
            ]));
        }
    }

    if trailing_blank {
        lines.push(Line::raw(String::new()));
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
