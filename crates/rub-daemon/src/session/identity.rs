use super::*;

/// Atomic launch-time identity snapshot: always read/written as a unit to
/// prevent the TOCTOU window that existed when the two fields had separate locks.
#[derive(Debug, Clone, Default)]
pub(crate) struct LaunchIdentity {
    pub(crate) attachment_identity: Option<String>,
    pub(crate) connection_target: Option<ConnectionTarget>,
}

impl SessionState {
    /// Return an atomic snapshot of both launch-time identity fields.
    pub(crate) async fn launch_identity(&self) -> LaunchIdentity {
        self.launch_identity.read().await.clone()
    }

    pub async fn set_attachment_identity(&self, identity: Option<String>) {
        self.launch_identity.write().await.attachment_identity = identity;
    }

    pub async fn attachment_identity(&self) -> Option<String> {
        self.launch_identity
            .read()
            .await
            .attachment_identity
            .clone()
    }

    pub async fn set_connection_target(&self, target: Option<ConnectionTarget>) {
        self.launch_identity.write().await.connection_target = target;
    }

    pub async fn connection_target(&self) -> Option<ConnectionTarget> {
        self.launch_identity.read().await.connection_target.clone()
    }
}
