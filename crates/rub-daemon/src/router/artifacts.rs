use rub_core::fs::FileCommitOutcome;
use serde_json::{Value, json};

pub(super) const INPUT_ARTIFACT_DURABILITY: &str = "external_input_reference";

pub(super) fn annotate_file_artifact_state(
    artifact: &mut Value,
    artifact_authority: &str,
    upstream_truth: &str,
    durability: &str,
) {
    let Some(object) = artifact.as_object_mut() else {
        return;
    };

    object.insert(
        "artifact_state".to_string(),
        json!({
            "truth_level": "command_artifact",
            "artifact_authority": artifact_authority,
            "upstream_truth": upstream_truth,
            "control_role": "display_only",
            "durability": durability,
        }),
    );
}

pub(super) fn annotate_path_reference_state(
    payload: &mut Value,
    path_authority: &str,
    upstream_truth: &str,
    path_kind: &str,
) {
    annotate_named_path_reference_state(
        payload,
        "path_state",
        "input_path_reference",
        path_authority,
        upstream_truth,
        path_kind,
    );
}

pub(super) fn annotate_operator_path_reference_state(
    payload: &mut Value,
    state_field: &str,
    path_authority: &str,
    upstream_truth: &str,
    path_kind: &str,
) {
    annotate_named_path_reference_state(
        payload,
        state_field,
        "operator_path_reference",
        path_authority,
        upstream_truth,
        path_kind,
    );
}

fn annotate_named_path_reference_state(
    payload: &mut Value,
    state_field: &str,
    truth_level: &str,
    path_authority: &str,
    upstream_truth: &str,
    path_kind: &str,
) {
    let Some(object) = payload.as_object_mut() else {
        return;
    };

    object.insert(
        state_field.to_string(),
        json!({
            "truth_level": truth_level,
            "path_authority": path_authority,
            "upstream_truth": upstream_truth,
            "path_kind": path_kind,
            "control_role": "display_only",
        }),
    );
}

pub(super) fn output_artifact_durability(outcome: FileCommitOutcome) -> &'static str {
    match outcome {
        FileCommitOutcome::Durable => "durable",
        FileCommitOutcome::Published => "published",
    }
}

#[cfg(test)]
mod tests {
    use super::{
        INPUT_ARTIFACT_DURABILITY, annotate_file_artifact_state,
        annotate_operator_path_reference_state, annotate_path_reference_state,
        output_artifact_durability,
    };
    use rub_core::fs::FileCommitOutcome;
    use serde_json::json;

    #[test]
    fn annotate_file_artifact_state_marks_command_artifact_boundary() {
        let mut artifact = json!({
            "kind": "storage_snapshot",
            "path": "/tmp/storage.json",
            "direction": "output",
        });

        annotate_file_artifact_state(
            &mut artifact,
            "router.storage_export_artifact",
            "storage_snapshot_result",
            "durable",
        );

        assert_eq!(
            artifact["artifact_state"]["truth_level"],
            "command_artifact"
        );
        assert_eq!(
            artifact["artifact_state"]["artifact_authority"],
            "router.storage_export_artifact"
        );
        assert_eq!(
            artifact["artifact_state"]["upstream_truth"],
            "storage_snapshot_result"
        );
        assert_eq!(artifact["artifact_state"]["control_role"], "display_only");
        assert_eq!(artifact["artifact_state"]["durability"], "durable");
    }

    #[test]
    fn output_artifact_durability_matches_commit_outcome() {
        assert_eq!(
            output_artifact_durability(FileCommitOutcome::Durable),
            "durable"
        );
        assert_eq!(
            output_artifact_durability(FileCommitOutcome::Published),
            "published"
        );
        assert_eq!(INPUT_ARTIFACT_DURABILITY, "external_input_reference");
    }

    #[test]
    fn annotate_path_reference_state_marks_external_input_reference() {
        let mut payload = json!({
            "path": "/tmp/upload.txt",
        });

        annotate_path_reference_state(
            &mut payload,
            "router.upload.input_path",
            "upload_command_request",
            "external_input_file",
        );

        assert_eq!(payload["path_state"]["truth_level"], "input_path_reference");
        assert_eq!(
            payload["path_state"]["path_authority"],
            "router.upload.input_path"
        );
        assert_eq!(
            payload["path_state"]["upstream_truth"],
            "upload_command_request"
        );
        assert_eq!(payload["path_state"]["path_kind"], "external_input_file");
        assert_eq!(payload["path_state"]["control_role"], "display_only");
    }

    #[test]
    fn annotate_operator_path_reference_state_marks_operator_projection_reference() {
        let mut payload = json!({
            "rub_home": "/tmp/rub-home",
        });

        annotate_operator_path_reference_state(
            &mut payload,
            "rub_home_state",
            "router.doctor.rub_home",
            "doctor_disk_report",
            "daemon_home_directory",
        );

        assert_eq!(
            payload["rub_home_state"]["truth_level"],
            "operator_path_reference"
        );
        assert_eq!(
            payload["rub_home_state"]["path_authority"],
            "router.doctor.rub_home"
        );
        assert_eq!(
            payload["rub_home_state"]["upstream_truth"],
            "doctor_disk_report"
        );
        assert_eq!(
            payload["rub_home_state"]["path_kind"],
            "daemon_home_directory"
        );
        assert_eq!(payload["rub_home_state"]["control_role"], "display_only");
    }
}
