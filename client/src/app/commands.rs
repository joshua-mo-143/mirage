use super::{App, FocusArea, TranscriptScrollMode};
use crate::backend::ClientBackend;
use mirage_core::session::TranscriptEntry;

impl App {
    /// Handles Enter based on the current focus, either toggling transcript state or submitting input.
    pub(crate) fn process_enter(&mut self, backend: &mut ClientBackend) {
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
            self.handle_command(backend, &input);
        } else {
            self.submit_prompt(backend, input);
        }
    }

    /// Submits a normal user prompt through the active backend.
    fn submit_prompt(&mut self, backend: &mut ClientBackend, prompt: String) {
        backend.submit_prompt(&mut self.service, prompt);
        self.follow_transcript_tail_if_composing();
    }

    /// Executes a slash command entered into the composer.
    fn handle_command(&mut self, backend: &mut ClientBackend, command: &str) {
        match command {
            "/help" => {
                self.push_session_entry(TranscriptEntry::meta(
                    "Commands",
                    "/help\n/status\n/clear\n/quit\n\nNavigation:\n- Ctrl+G toggles selection mode for native terminal drag-to-select\n- Tab toggles composer/transcript focus\n- Up/Down moves transcript selection\n- PageUp/PageDown scroll the transcript\n- Home/End jump to the top or bottom\n- Left/Right collapses or expands a selected subagent\n- Enter/Space toggles a selected subagent\n- y copies the selected transcript item\n- Y copies the full transcript\n\nLocal built-in tools:\n- bash(command, cwd?)\n- playwright(action, session_id?, url?, selector?, text?, key?, timeout_ms?, path?, wait_until?)\n- prompt_cursor(prompt, cwd?)\n- subagent(prompt, cwd?, model?, mode?)\n- read_file(path, start_line?, line_count?)\n- edit_file(path, old_text, new_text, replace_all?)\n- write_file(path, content, append?, overwrite_existing?, create_parent_directories?)\n\nRemote/server workflow:\n- Use `--server-url` or saved config to connect remotely\n- Use `--start-server` to launch a local Mirage server before opening the TUI",
                ));
            }
            "/status" => {
                let status = self.service.status_snapshot();
                self.push_session_entry(TranscriptEntry::meta(
                    "Status",
                    format!(
                        "backend: {}\nmodel: {}\nauthority: {}\nbase path: {}\nmax turns: {}\nvenice system prompt: {}\nuser system prompt: {}\nhistory messages: {}\ncursor sessions: {}\nselection mode: {}\nfocus: {}",
                        self.backend_description,
                        status.model,
                        status.authority,
                        status.base_path,
                        status.max_turns,
                        if status.uncensored { "enabled" } else { "disabled" },
                        if status.system_prompt_configured { "configured" } else { "unset" },
                        status.history_messages,
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
                backend.clear_conversation(&mut self.service);
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
