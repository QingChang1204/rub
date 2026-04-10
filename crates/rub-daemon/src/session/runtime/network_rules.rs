use super::*;

impl SessionState {
    /// Register a session-scoped network rule in the integration runtime.
    pub async fn register_network_rule(&self, spec: NetworkRuleSpec) -> NetworkRule {
        let id = self.next_network_rule_id.fetch_add(1, Ordering::SeqCst);
        let rule = NetworkRule {
            id,
            status: NetworkRuleStatus::Configured,
            spec,
        };
        let mut integration = self.integration_runtime.write().await;
        integration.request_rules.push(rule.clone());
        integration.sync_request_rule_count();
        integration.status = IntegrationRuntimeStatus::Active;
        rule
    }

    /// List all configured network rules in stable registration order.
    pub async fn network_rules(&self) -> Vec<NetworkRule> {
        self.integration_runtime.read().await.request_rules.clone()
    }

    /// Update the runtime attachment status for a configured network rule.
    pub async fn set_network_rule_status(
        &self,
        id: u32,
        status: NetworkRuleStatus,
    ) -> Option<NetworkRule> {
        let mut integration = self.integration_runtime.write().await;
        let rule = integration
            .request_rules
            .iter_mut()
            .find(|rule| rule.id == id)?;
        rule.status = status;
        Some(rule.clone())
    }

    /// Remove a configured network rule.
    pub async fn remove_network_rule(&self, id: u32) -> Option<NetworkRule> {
        let mut integration = self.integration_runtime.write().await;
        let index = integration
            .request_rules
            .iter()
            .position(|rule| rule.id == id)?;
        let removed = integration.request_rules.remove(index);
        integration.sync_request_rule_count();
        if integration.request_rules.is_empty() {
            integration.status = IntegrationRuntimeStatus::Inactive;
        }
        Some(removed)
    }

    /// Clear all configured network rules.
    pub async fn clear_network_rules(&self) -> Vec<NetworkRule> {
        let mut integration = self.integration_runtime.write().await;
        let removed = std::mem::take(&mut integration.request_rules);
        integration.sync_request_rule_count();
        integration.status = IntegrationRuntimeStatus::Inactive;
        removed
    }

    /// Allocate the next stable session-scoped network rule id.
    pub fn next_network_rule_id(&self) -> u32 {
        self.next_network_rule_id.fetch_add(1, Ordering::SeqCst)
    }

    /// Replace the canonical session-scoped network rule list.
    pub async fn replace_network_rules(&self, rules: Vec<NetworkRule>) {
        let mut integration = self.integration_runtime.write().await;
        integration.request_rules = rules;
        integration.sync_request_rule_count();
        integration.status = if integration.request_rules.is_empty() {
            IntegrationRuntimeStatus::Inactive
        } else {
            IntegrationRuntimeStatus::Active
        };
    }
}
