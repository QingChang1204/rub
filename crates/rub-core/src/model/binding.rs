mod capture;
mod execution;
mod live;
mod storage;

pub use capture::*;
pub use execution::*;
pub use live::*;
pub use storage::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_registry_default_uses_v1_schema() {
        let registry = BindingRegistryData::default();
        assert_eq!(registry.schema_version, 1);
        assert!(registry.bindings.is_empty());
    }

    #[test]
    fn remembered_binding_alias_registry_default_uses_v1_schema() {
        let registry = RememberedBindingAliasRegistryData::default();
        assert_eq!(registry.schema_version, 1);
        assert!(registry.aliases.is_empty());
    }

    #[test]
    fn binding_status_serializes_snake_case() {
        let json = serde_json::to_string(&BindingStatus::ExternalReattachmentRequired).unwrap();
        assert_eq!(json, "\"external_reattachment_required\"");
    }

    #[test]
    fn binding_capture_fence_status_serializes_snake_case() {
        let json = serde_json::to_string(&BindingCaptureFenceStatus::BindCurrentOnly).unwrap();
        assert_eq!(json, "\"bind_current_only\"");
    }

    #[test]
    fn binding_resolution_serializes_with_tagged_kind() {
        let json = serde_json::to_value(BindingResolution::NoLiveMatch).unwrap();
        assert_eq!(json["kind"], "no_live_match");
    }

    #[test]
    fn remembered_binding_alias_target_serializes_with_tagged_kind() {
        let json = serde_json::to_value(RememberedBindingAliasTarget::MissingBinding {
            binding_alias: "old-admin".to_string(),
        })
        .unwrap();
        assert_eq!(json["kind"], "missing_binding");
    }

    #[test]
    fn binding_execution_mode_serializes_snake_case() {
        let runtime_json =
            serde_json::to_string(&BindingExecutionMode::LaunchBoundRuntime).unwrap();
        assert_eq!(runtime_json, "\"launch_bound_runtime\"");
        let profile_json =
            serde_json::to_string(&BindingExecutionMode::LaunchBoundProfile).unwrap();
        assert_eq!(profile_json, "\"launch_bound_profile\"");
    }
}
