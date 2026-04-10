use super::*;

impl DownloadRuntimeState {
    pub fn projection(&self) -> DownloadRuntimeInfo {
        DownloadRuntimeInfo {
            status: self.projection.status,
            mode: self.projection.mode,
            download_dir: self.projection.download_dir.clone(),
            active_downloads: self
                .active_order
                .iter()
                .filter_map(|guid| self.entries.get(guid))
                .cloned()
                .collect(),
            completed_downloads: self
                .completed_order
                .iter()
                .filter_map(|guid| self.entries.get(guid))
                .cloned()
                .collect(),
            last_download: self.projection.last_download.clone(),
            degraded_reason: self.projection.degraded_reason.clone(),
        }
    }

    pub fn cursor(&self) -> u64 {
        self.next_sequence.saturating_sub(1)
    }

    pub fn events_after(&self, cursor: u64) -> Vec<DownloadEvent> {
        self.timeline
            .iter()
            .filter(|event| event.sequence > cursor)
            .cloned()
            .collect()
    }

    pub fn get(&self, guid: &str) -> Option<DownloadEntry> {
        self.entries.get(guid).cloned()
    }
}
