use super::{App, StreamEvent};
use crate::{
    app::helpers::truncate_text,
    tools::subagent_tool::SubagentProgressEvent,
    transcript::{SubagentStatus, TranscriptEntry, TranscriptItem, TranscriptKind},
};

impl App {
    pub(crate) fn apply_stream_event(&mut self, event: StreamEvent) {
        match event {
            StreamEvent::AssistantText(text) => {
                if self.pending_assistant.is_none() && text.trim().is_empty() {
                    return;
                }
                if let Some(index) = self.pending_assistant {
                    if let Some(entry) = self.transcript.get_mut(index) {
                        if let Some(entry) = entry.entry_mut() {
                            entry.body.push_str(&text);
                        }
                    }
                } else {
                    self.push_transcript_entry(TranscriptEntry::assistant(text));
                    self.pending_assistant = Some(self.transcript.len() - 1);
                }
            }
            StreamEvent::ToolCall { id, name, summary } => {
                self.pending_assistant = None;
                self.record_tool_call(id, name, summary);
            }
            StreamEvent::ToolResult { id } => {
                self.pending_assistant = None;
                self.record_tool_result(id);
            }
            StreamEvent::Final(final_response) => {
                if self.pending_assistant.is_none() && !final_response.response().is_empty() {
                    self.push_transcript_entry(TranscriptEntry::assistant(
                        final_response.response().to_owned(),
                    ));
                }
                self.pending_assistant = None;

                if let Some(history) = final_response.history() {
                    self.history = history.to_vec();
                }

                let usage = final_response.usage();
                self.usage = Some(usage);
                self.clear_active_tool_aggregates();
                self.streaming = false;
                self.status = format!(
                    "Ready. Last response used {} input / {} output tokens.",
                    usage.input_tokens, usage.output_tokens
                );
            }
            StreamEvent::Error(error) => {
                if let Some(index) = self.pending_assistant.take()
                    && self
                        .transcript
                        .get(index)
                        .and_then(TranscriptItem::entry)
                        .is_some_and(|entry| entry.body.is_empty())
                {
                    self.transcript.remove(index);
                    self.clamp_selected_transcript();
                }
                self.clear_active_tool_aggregates();
                self.pending_subagents.clear();
                self.streaming = false;
                self.status = "Last request failed.".to_owned();
                self.push_transcript_entry(TranscriptEntry::error(error));
            }
        }
    }

    pub(crate) fn apply_subagent_event(&mut self, event: SubagentProgressEvent) {
        match event {
            SubagentProgressEvent::Started { id, summary } => {
                self.push_subagent_group(id, summary.clone());
                self.status = format!("Subagent running: {}", truncate_text(&summary, 80));
            }
            SubagentProgressEvent::AssistantDelta { id, text } => {
                self.status = "Streaming subagent output...".to_owned();
                let _ = self.update_subagent_group(&id, |group, pending| {
                    if pending.pending_entry_index.is_none() && text.trim().is_empty() {
                        return;
                    }
                    if let Some(index) = pending.pending_entry_index {
                        if let Some(entry) = group.entries.get_mut(index) {
                            entry.body.push_str(&text);
                            return;
                        }
                    }

                    group.entries.push(TranscriptEntry {
                        kind: TranscriptKind::Assistant,
                        title: "Assistant".to_owned(),
                        body: text,
                    });
                    pending.pending_entry_index = Some(group.entries.len() - 1);
                });
            }
            SubagentProgressEvent::ToolStarted { id, description } => {
                let _ = self.update_subagent_group(&id, |group, pending| {
                    pending.pending_entry_index = None;
                    pending.tool_call_count += 1;
                    pending.pending_tool_calls += 1;
                    pending.latest_tool_description = description;
                    Self::update_subagent_tool_title(group, pending);
                });
            }
            SubagentProgressEvent::ToolCompleted {
                id,
                description,
                output: _,
            } => {
                let _ = self.update_subagent_group(&id, |group, pending| {
                    pending.pending_entry_index = None;
                    pending.pending_tool_calls = pending.pending_tool_calls.saturating_sub(1);
                    if pending.tool_call_count == 0 {
                        pending.tool_call_count = 1;
                    }
                    pending.latest_tool_description = description;
                    Self::update_subagent_tool_title(group, pending);
                });
            }
            SubagentProgressEvent::Finished { id } => {
                let _ = self.update_subagent_group(&id, |group, pending| {
                    pending.pending_entry_index = None;
                    pending.pending_tool_calls = 0;
                    Self::update_subagent_tool_title(group, pending);
                    group.status = SubagentStatus::Complete;
                });
                self.pending_subagents.remove(&id);
                if self.streaming {
                    self.status = "Subagent finished; waiting for parent agent...".to_owned();
                }
            }
            SubagentProgressEvent::Failed { id, error } => {
                let _ = self.update_subagent_group(&id, |group, pending| {
                    pending.pending_entry_index = None;
                    pending.pending_tool_calls = 0;
                    Self::update_subagent_tool_title(group, pending);
                    group.status = SubagentStatus::Failed;
                    group.entries.push(TranscriptEntry::error(error.clone()));
                });
                self.pending_subagents.remove(&id);
            }
        }
    }
}
