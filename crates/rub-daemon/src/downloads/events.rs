use super::*;

pub(super) fn event_kind_for_state(state: DownloadState) -> DownloadEventKind {
    match state {
        DownloadState::Started => DownloadEventKind::Started,
        DownloadState::InProgress => DownloadEventKind::Progress,
        DownloadState::Completed => DownloadEventKind::Completed,
        DownloadState::Failed => DownloadEventKind::Failed,
        DownloadState::Canceled => DownloadEventKind::Canceled,
    }
}

pub(super) fn is_terminal(state: DownloadState) -> bool {
    matches!(
        state,
        DownloadState::Completed | DownloadState::Failed | DownloadState::Canceled
    )
}

impl DownloadRuntimeState {
    pub(super) fn record_event(&mut self, kind: DownloadEventKind, download: DownloadEntry) {
        let sequence = self.next_sequence.max(1);
        self.next_sequence = sequence + 1;
        self.timeline.push_back(DownloadEvent {
            sequence,
            kind,
            download,
        });
        while self.timeline.len() > DOWNLOAD_EVENT_LIMIT {
            self.timeline.pop_front();
        }
    }
}
