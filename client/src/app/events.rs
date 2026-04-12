use super::App;
use mirage_core::session::{StreamEvent, SubagentProgressEvent};
use mirage_service::api::SessionSnapshot;

impl App {
    /// Applies a parent-agent stream event and updates any dependent UI state.
    pub(crate) fn apply_stream_event(&mut self, event: StreamEvent) {
        let previous_len = self.service.session().transcript.len();
        self.service.apply_stream_event(event);
        self.sync_after_session_mutation(previous_len);
    }

    /// Applies a child-agent progress event and updates any dependent UI state.
    pub(crate) fn apply_subagent_event(&mut self, event: SubagentProgressEvent) {
        let previous_len = self.service.session().transcript.len();
        self.service.apply_subagent_event(event);
        self.sync_after_session_mutation(previous_len);
    }

    /// Replaces local reducer state with a remote snapshot and updates any dependent UI state.
    pub(crate) fn apply_remote_snapshot(&mut self, snapshot: SessionSnapshot) {
        let previous_len = self.service.session().transcript.len();
        self.service.apply_remote_snapshot(snapshot);
        self.sync_after_session_mutation(previous_len);
    }

    /// Records a remote backend error in the transcript and status line.
    pub(crate) fn apply_remote_error(&mut self, error: String) {
        self.service.session_mut().streaming = false;
        self.service.session_mut().status = "Remote request failed.".to_owned();
        self.push_session_entry(mirage_core::session::TranscriptEntry::error(error));
    }

    /// Synchronizes selection and scroll state after the session transcript changes.
    fn sync_after_session_mutation(&mut self, previous_len: usize) {
        if self.service.session().transcript.len() != previous_len {
            self.follow_transcript_tail_if_composing();
        }
        self.clamp_selected_transcript();
    }
}
