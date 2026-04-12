use super::App;
use mirage_core::session::{StreamEvent, SubagentProgressEvent};

impl App {
    pub(crate) fn apply_stream_event(&mut self, event: StreamEvent) {
        let previous_len = self.session.transcript.len();
        self.session.apply_stream_event(event);
        self.sync_after_session_mutation(previous_len);
    }

    pub(crate) fn apply_subagent_event(&mut self, event: SubagentProgressEvent) {
        let previous_len = self.session.transcript.len();
        self.session.apply_subagent_event(event);
        self.sync_after_session_mutation(previous_len);
    }

    fn sync_after_session_mutation(&mut self, previous_len: usize) {
        if self.session.transcript.len() != previous_len {
            self.follow_transcript_tail_if_composing();
        }
        self.clamp_selected_transcript();
    }
}
