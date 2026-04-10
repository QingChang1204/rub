use rub_core::fs::FileCommitOutcome;
use serde_json::{Value, json};

pub(crate) fn annotate_local_persisted_artifact(
    artifact: &mut Value,
    projection_authority: &str,
    outcome: FileCommitOutcome,
) {
    let Some(object) = artifact.as_object_mut() else {
        return;
    };

    object.insert(
        "projection_state".to_string(),
        json!({
            "truth_level": "local_persistence_projection",
            "projection_kind": "cli_persisted_artifact",
            "projection_authority": projection_authority,
            "upstream_commit_truth": "daemon_response_committed",
            "control_role": "display_only",
            "durability": persisted_artifact_durability(outcome),
        }),
    );

    if !outcome.durability_confirmed() {
        object.insert("durability_confirmed".to_string(), Value::Bool(false));
    }
}

fn persisted_artifact_durability(outcome: FileCommitOutcome) -> &'static str {
    match outcome {
        FileCommitOutcome::Durable => "durable",
        FileCommitOutcome::Published => "published",
    }
}

#[cfg(test)]
mod tests {
    use super::annotate_local_persisted_artifact;
    use rub_core::fs::FileCommitOutcome;
    use serde_json::json;

    #[test]
    fn annotate_local_persisted_artifact_marks_durable_cli_projection() {
        let mut artifact = json!({
            "kind": "workflow_asset",
            "path": "/tmp/example.json",
        });

        annotate_local_persisted_artifact(
            &mut artifact,
            "cli.history_export_asset_persistence",
            FileCommitOutcome::Durable,
        );

        assert_eq!(
            artifact["projection_state"]["truth_level"],
            "local_persistence_projection"
        );
        assert_eq!(
            artifact["projection_state"]["projection_kind"],
            "cli_persisted_artifact"
        );
        assert_eq!(
            artifact["projection_state"]["projection_authority"],
            "cli.history_export_asset_persistence"
        );
        assert_eq!(
            artifact["projection_state"]["upstream_commit_truth"],
            "daemon_response_committed"
        );
        assert_eq!(artifact["projection_state"]["durability"], "durable");
        assert!(artifact.get("durability_confirmed").is_none(), "{artifact}");
    }

    #[test]
    fn annotate_local_persisted_artifact_marks_published_when_parent_sync_is_unconfirmed() {
        let mut artifact = json!({
            "kind": "workflow_asset",
            "path": "/tmp/example.json",
        });

        annotate_local_persisted_artifact(
            &mut artifact,
            "cli.history_export_asset_persistence",
            FileCommitOutcome::Published,
        );

        assert_eq!(artifact["projection_state"]["durability"], "published");
        assert_eq!(artifact["durability_confirmed"], false);
    }
}
