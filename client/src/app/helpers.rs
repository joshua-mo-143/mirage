use arboard::Clipboard;
use ratatui::layout::Rect;
use serde_json::Value;

pub(super) fn rect_contains_point(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && x < rect.x.saturating_add(rect.width)
        && y >= rect.y
        && y < rect.y.saturating_add(rect.height)
}

pub(super) fn copy_text_to_clipboard(text: &str) -> Result<(), String> {
    let mut clipboard = Clipboard::new().map_err(|error| error.to_string())?;
    clipboard
        .set_text(text.to_owned())
        .map_err(|error| error.to_string())
}

pub(crate) fn summarize_tool_call(name: &str, arguments: &impl std::fmt::Display) -> String {
    let arguments_text = arguments.to_string();
    let json = serde_json::from_str::<Value>(&arguments_text).ok();

    match name {
        "read_file" => format!(
            "File read: {}",
            summarize_argument_field(&json, "path", &arguments_text)
        ),
        "edit_file" => format!(
            "File edit: {}",
            summarize_argument_field(&json, "path", &arguments_text)
        ),
        "write_file" => format!(
            "File write: {}",
            summarize_argument_field(&json, "path", &arguments_text)
        ),
        "bash" => format!(
            "Bash: {}",
            summarize_argument_field(&json, "command", &arguments_text)
        ),
        "prompt_cursor" => format!(
            "Prompt Cursor: {}",
            summarize_argument_field(&json, "prompt", &arguments_text)
        ),
        "subagent" => format!(
            "Subagent: {}",
            summarize_argument_field(&json, "prompt", &arguments_text)
        ),
        _ => format!(
            "Tool {name}: {}",
            truncate_text(&single_line(&arguments_text), 80)
        ),
    }
}

pub(super) fn tool_label(name: &str) -> &'static str {
    match name {
        "read_file" => "File read",
        "edit_file" => "File edit",
        "write_file" => "File write",
        "bash" => "Bash",
        "prompt_cursor" => "Prompt Cursor",
        "subagent" => "Subagent",
        _ => "Tool",
    }
}

pub(super) fn tool_detail_from_summary(summary: &str) -> String {
    summary
        .split_once(": ")
        .map(|(_, detail)| detail.to_owned())
        .unwrap_or_else(|| summary.to_owned())
}

pub(super) fn render_tool_aggregate_title(
    label: &str,
    latest_detail: &str,
    total_calls: usize,
    pending_calls: usize,
) -> String {
    let count_suffix = if total_calls > 1 {
        format!(" x{total_calls}")
    } else {
        String::new()
    };
    let running_suffix = match pending_calls {
        0 => String::new(),
        1 => " (running)".to_owned(),
        count => format!(" ({count} running)"),
    };

    if latest_detail.is_empty() {
        format!("{label}{count_suffix}{running_suffix}")
    } else if total_calls > 1 {
        format!("{label}{count_suffix} (latest: {latest_detail}){running_suffix}")
    } else {
        format!("{label}: {latest_detail}{running_suffix}")
    }
}

pub(super) fn render_subagent_tool_aggregate_title(
    latest_description: &str,
    total_calls: usize,
    pending_calls: usize,
) -> String {
    let latest_description = if latest_description.is_empty() {
        "Child tool call"
    } else {
        latest_description
    };
    let count_suffix = if total_calls > 1 {
        format!(" x{total_calls}")
    } else {
        String::new()
    };
    let running_suffix = match pending_calls {
        0 => String::new(),
        1 => " (running)".to_owned(),
        count => format!(" ({count} running)"),
    };

    if total_calls > 1 {
        format!("Tools{count_suffix} (latest: {latest_description}){running_suffix}")
    } else {
        format!("Tool: {latest_description}{running_suffix}")
    }
}

fn summarize_argument_field(json: &Option<Value>, field: &str, fallback: &str) -> String {
    json.as_ref()
        .and_then(|value| value.get(field))
        .and_then(Value::as_str)
        .map(|value| truncate_text(&single_line(value), 80))
        .unwrap_or_else(|| truncate_text(&single_line(fallback), 80))
}

fn single_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub(super) fn truncate_text(value: &str, max_chars: usize) -> String {
    let total = value.chars().count();
    if total <= max_chars {
        return value.to_owned();
    }

    let head: String = value.chars().take(max_chars.saturating_sub(3)).collect();
    format!("{head}...")
}
