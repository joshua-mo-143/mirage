use super::{App, FocusArea, TranscriptScrollMode};
use mirage_core::{VeniceAgent, session::TranscriptEntry};
use tokio::sync::mpsc;

impl App {
    pub(crate) fn process_enter(
        &mut self,
        agent: VeniceAgent,
        tx: mpsc::UnboundedSender<mirage_core::session::StreamEvent>,
    ) {
        if matches!(self.focus, FocusArea::Transcript) {
            self.toggle_selected_subagent_group();
            return;
        }

        if !self.can_submit() {
            return;
        }

        let input = self.input.trim().to_owned();
        self.input.clear();
        self.cursor = 0;

        if input.starts_with('/') {
            self.handle_command(&input);
        } else {
            self.submit_prompt(agent, tx, input);
        }
    }

    fn submit_prompt(
        &mut self,
        agent: VeniceAgent,
        tx: mpsc::UnboundedSender<mirage_core::session::StreamEvent>,
        prompt: String,
    ) {
        let history = self.session.history.clone();
        self.session.begin_prompt(prompt.clone());
        self.follow_transcript_tail_if_composing();

        let max_turns = self.max_turns;
        tokio::spawn(async move {
            crate::streaming::stream_agent_response(agent, prompt, history, max_turns, tx).await;
        });
    }

    fn handle_command(&mut self, command: &str) {
        match command {
            "/help" => {
                self.push_session_entry(TranscriptEntry::meta(
                    "Commands",
                    "/help\n/status\n/clear\n/quit\n\nNavigation:\n- Ctrl+G toggles selection mode for native terminal drag-to-select\n- Tab toggles composer/transcript focus\n- Up/Down moves transcript selection\n- PageUp/PageDown scroll the transcript\n- Home/End jump to the top or bottom\n- Left/Right collapses or expands a selected subagent\n- Enter/Space toggles a selected subagent\n- y copies the selected transcript item\n- Y copies the full transcript\n\nBuilt-in tools:\n- bash(command, cwd?)\n- prompt_cursor(prompt, cwd?)\n- subagent(prompt, cwd?, model?, mode?)\n- read_file(path, start_line?, line_count?)\n- edit_file(path, old_text, new_text, replace_all?)\n- write_file(path, content, append?, overwrite_existing?, create_parent_directories?)",
                ));
            }
            "/status" => {
                self.push_session_entry(TranscriptEntry::meta(
                    "Status",
                    format!(
                        "model: {}\nauthority: {}\nbase path: {}\nmax turns: {}\nvenice system prompt: {}\nuser system prompt: {}\nhistory messages: {}\ncursor sessions: {}\nselection mode: {}\nfocus: {}",
                        self.model,
                        self.authority,
                        self.base_path,
                        self.max_turns,
                        if self.uncensored { "enabled" } else { "disabled" },
                        if self.system_prompt_configured { "configured" } else { "unset" },
                        self.session.history.len(),
                        self.cursor_sessions.len(),
                        if self.selection_mode { "enabled" } else { "disabled" },
                        match self.focus {
                            FocusArea::Composer => "composer",
                            FocusArea::Transcript => "transcript",
                        }
                    ),
                ));
            }
            "/clear" => {
                self.cursor_sessions.clear();
                self.session.clear_with_notice(
                    "Conversation cleared, including Cursor session state.",
                    "Cleared conversation history and Cursor session state.",
                );
                self.focus = FocusArea::Composer;
                self.selection_mode = false;
                self.selected_transcript = 0;
                self.transcript_scroll = 0;
                self.transcript_scroll_mode = TranscriptScrollMode::FollowTail;
                self.last_transcript_scroll = 0;
                self.last_transcript_max_scroll = 0;
                self.last_transcript_page_height = 0;
            }
            "/quit" | "/exit" => {
                self.should_quit = true;
            }
            other => {
                self.push_session_entry(TranscriptEntry::error(format!(
                    "Unknown command: {other}"
                )));
            }
        }
    }
}
