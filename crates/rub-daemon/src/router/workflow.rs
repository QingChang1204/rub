use std::sync::Arc;

mod args;
mod command_build;
mod execution;
mod projection;
mod spec;
mod validate;

#[cfg(test)]
use self::args::{FillArgs, PipeArgs, submit_args};
#[cfg(test)]
use self::spec::{parse_pipe_spec, resolve_step_references};
use super::*;
use rub_core::error::RubError;

pub(super) async fn cmd_fill(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    execution::cmd_fill(router, args, deadline, state).await
}

pub(super) async fn cmd_fill_validate(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    validate::cmd_fill_validate(router, args, deadline, state).await
}

pub(super) async fn cmd_trigger_fill(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    execution::cmd_trigger_fill(router, args, deadline, state).await
}

pub(super) async fn cmd_pipe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    execution::cmd_pipe(router, args, deadline, state).await
}

pub(super) async fn cmd_trigger_pipe(
    router: &DaemonRouter,
    args: &serde_json::Value,
    deadline: TransactionDeadline,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    execution::cmd_trigger_pipe(router, args, deadline, state).await
}

#[cfg(test)]
mod tests {
    use super::args::SubmitLocatorArgs;
    use super::{
        FillArgs, PipeArgs,
        command_build::{
            build_atomic_rollback_command_for_resolved_target,
            build_fill_step_command_for_resolved_target, build_fill_step_locator_args,
            build_submit_command_for_resolved_target,
        },
        execution::{OrchestrationMetadataInheritancePolicy, inherit_orchestration_metadata},
        parse_pipe_spec, resolve_step_references,
        spec::build_embedded_orchestration_args,
        spec::resolve_template_string,
        submit_args,
    };
    use crate::router::automation_fence::ensure_committed_automation_result;
    use crate::router::request_args::parse_json_args;
    use rub_core::error::ErrorCode;
    use rub_core::json_spec::NormalizedJsonSpec;
    use rub_core::model::{Element, ElementTag};
    use serde_json::json;
    use std::path::Path;

    #[test]
    fn parse_pipe_spec_accepts_legacy_steps_array_shorthand() {
        let parsed = parse_pipe_spec(
            &NormalizedJsonSpec::from_raw_str(
                r#"[{"command":"open","args":{"url":"https://example.com"}}]"#,
                "pipe",
            )
            .expect("legacy array shorthand should parse as normalized spec"),
            Path::new("/tmp/rub-workflow-parse-array"),
        )
        .expect("legacy pipe array shorthand should be normalized");
        assert_eq!(parsed.value.steps.len(), 1);
        assert!(parsed.value.orchestrations.is_empty());
    }

    #[test]
    fn parse_pipe_spec_rejects_watch_alias() {
        let error = parse_pipe_spec(
            &NormalizedJsonSpec::from_raw_str(
                r##"{
              "steps": [{"command":"state","args":{"format":"compact"}}],
              "watch": [{
                "label": "reply",
                "spec": {
                  "source": {"session_id":"source-session"},
                  "target": {"session_id":"target-session"},
                  "mode": "once",
                  "condition": {"kind":"text_present","text":"Ready"},
                  "actions": [{
                    "kind":"browser_command",
                    "command":"click",
                    "payload":{"selector":"#apply"}
                  }]
                }
              }]
            }"##,
                "pipe",
            )
            .expect("workflow object should parse as normalized spec"),
            Path::new("/tmp/rub-workflow-parse-object"),
        )
        .expect_err("watch alias should be rejected");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn parse_pipe_spec_rejects_empty_workflow_object() {
        let error = parse_pipe_spec(
            &NormalizedJsonSpec::from_raw_str(r#"{"steps":[],"orchestrations":[]}"#, "pipe")
                .expect("empty workflow object should parse as normalized spec"),
            Path::new("/tmp/rub-workflow-parse-empty"),
        )
        .expect_err("empty workflow object should fail");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn parse_pipe_spec_rejects_unknown_step_fields() {
        let error = parse_pipe_spec(
            &NormalizedJsonSpec::from_raw_str(
                r##"{
              "steps": [
                {"command":"click","args":{"selector":"#go"},"argz":{"selector":"#wrong"}}
              ]
            }"##,
                "pipe",
            )
            .expect("workflow object should parse as normalized spec"),
            Path::new("/tmp/rub-workflow-parse-unknown"),
        )
        .expect_err("unknown step fields should fail closed");
        assert_eq!(error.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn embedded_orchestration_args_preserve_structured_spec() {
        let args = build_embedded_orchestration_args(
            Some(&json!({
                "kind": "file",
                "path": "/tmp/workflow.json"
            })),
            &super::args::PipeEmbeddedOrchestrationSpec {
                label: Some("watch rule".to_string()),
                spec: json!({
                    "source": { "session_id": "source" },
                    "target": { "session_id": "target" },
                    "condition": { "kind": "text_present", "text": "Ready" },
                    "actions": [{ "kind": "browser_command", "command": "reload" }]
                }),
            },
            2,
        )
        .expect("embedded orchestration args should build");

        assert_eq!(
            args["spec"],
            json!({
                "source": { "session_id": "source" },
                "target": { "session_id": "target" },
                "condition": { "kind": "text_present", "text": "Ready" },
                "actions": [{ "kind": "browser_command", "command": "reload" }]
            })
        );
        assert_eq!(args["spec_source"]["kind"], "workflow_embedded");
        assert_eq!(args["spec_source"]["block_index"], 2);
    }

    #[test]
    fn automation_step_commit_fence_fails_closed_on_degraded_interaction() {
        let error = ensure_committed_automation_result(
            "click",
            Some(&serde_json::json!({
                "interaction": {
                    "confirmation_status": "degraded",
                    "confirmation_kind": "value_applied",
                }
            })),
        )
        .expect_err("non-confirmed interaction must stop workflow automation");
        assert_eq!(error.code, ErrorCode::WaitTimeout);
    }

    #[test]
    fn fill_args_parse_submit_locator_and_wait_after() {
        let parsed: FillArgs = parse_json_args(
            &json!({
                "spec": "[]",
                "snapshot_id": "snap-123",
                "atomic": true,
                "submit_label": "Send",
                "submit_first": true,
                "wait_after": {"selector":"#done"},
                "_trigger": {"kind": "trigger_action"},
                "_orchestration": {"frame_id": "frame-target"},
            }),
            "fill",
        )
        .expect("fill args should parse through typed envelope");

        assert_eq!(parsed.submit.label.as_deref(), Some("Send"));
        assert!(parsed.submit.first);
        assert!(parsed.atomic);
        assert_eq!(parsed._snapshot_id.as_deref(), Some("snap-123"));
        assert!(parsed._wait_after.is_some());
        assert!(parsed._trigger.is_some());
        assert_eq!(
            parsed
                ._orchestration
                .as_ref()
                .and_then(|value| value.get("frame_id")),
            Some(&json!("frame-target"))
        );
    }

    #[test]
    fn pipe_args_accept_trigger_and_orchestration_metadata_but_reject_unknown_fields() {
        let parsed: PipeArgs = parse_json_args(
            &json!({
                "spec": "[]",
                "_trigger": {
                    "kind": "trigger_action",
                },
                "_orchestration": {
                    "kind": "orchestration_action",
                }
            }),
            "pipe",
        )
        .expect("pipe args should accept trigger and orchestration metadata");
        assert_eq!(parsed.spec.as_value(), &json!([]));
        assert!(parsed._trigger.is_some());
        assert!(parsed._orchestration.is_some());

        let error = parse_json_args::<PipeArgs>(
            &json!({
                "spec": "[]",
                "mystery": true,
            }),
            "pipe",
        )
        .expect_err("unknown pipe fields should still fail")
        .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn inherit_orchestration_metadata_preserves_child_frame_by_default() {
        let mut args = json!({
            "selector": "#submit",
            "_orchestration": {
                "frame_id": "child-frame",
                "command_id": "child-command",
            }
        });
        let inherited = json!({
            "frame_id": "target-frame",
            "command_id": "parent-command",
            "phase": "trigger_action"
        });

        inherit_orchestration_metadata(
            &mut args,
            Some(&inherited),
            OrchestrationMetadataInheritancePolicy::PreserveChildOverrides,
        );

        assert_eq!(args["_orchestration"]["frame_id"], "child-frame");
        assert_eq!(args["_orchestration"]["command_id"], "child-command");
        assert_eq!(args["_orchestration"]["phase"], "trigger_action");
    }

    #[test]
    fn inherit_orchestration_metadata_overwrites_frame_for_trigger_owned_workflow() {
        let mut args = json!({
            "selector": "#submit",
            "_orchestration": {
                "frame_id": "child-frame",
                "command_id": "child-command",
            }
        });
        let inherited = json!({
            "frame_id": "target-frame",
            "command_id": "parent-command",
            "phase": "trigger_action"
        });

        inherit_orchestration_metadata(
            &mut args,
            Some(&inherited),
            OrchestrationMetadataInheritancePolicy::TriggerAuthoritativeFrame,
        );

        assert_eq!(args["_orchestration"]["frame_id"], "target-frame");
        assert_eq!(args["_orchestration"]["command_id"], "child-command");
        assert_eq!(args["_orchestration"]["phase"], "trigger_action");
    }

    #[test]
    fn inherit_orchestration_metadata_normalizes_non_object_child_metadata() {
        let mut args = json!({
            "selector": "#submit",
            "_orchestration": "bad-shape"
        });
        let inherited = json!({
            "frame_id": "target-frame",
        });

        inherit_orchestration_metadata(
            &mut args,
            Some(&inherited),
            OrchestrationMetadataInheritancePolicy::PreserveChildOverrides,
        );

        assert_eq!(args["_orchestration"]["frame_id"], "target-frame");
    }

    #[test]
    fn fill_step_locator_args_receive_inherited_frame_metadata_before_resolution() {
        let step: super::args::FillStepSpec = serde_json::from_value(json!({
            "selector": "#submit",
            "value": "hello"
        }))
        .expect("fill step should deserialize");
        let mut locator_args = build_fill_step_locator_args(&step);

        inherit_orchestration_metadata(
            &mut locator_args,
            Some(&json!({
                "frame_id": "target-frame",
            })),
            OrchestrationMetadataInheritancePolicy::TriggerAuthoritativeFrame,
        );

        assert_eq!(locator_args["_orchestration"]["frame_id"], "target-frame");
    }

    #[test]
    fn snapshot_fill_preflight_uses_element_ref_for_live_step_execution() {
        let step: super::args::FillStepSpec = serde_json::from_value(json!({
            "label": "Email",
            "value": "user@example.com"
        }))
        .expect("fill step should deserialize");
        let element = Element {
            index: 3,
            tag: ElementTag::Input,
            text: String::new(),
            attributes: std::collections::HashMap::new(),
            element_ref: Some("frame-main:42".to_string()),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: None,
        };

        let (command, args) = build_fill_step_command_for_resolved_target(&step, &element)
            .expect("snapshot preflight should build a live command");
        assert_eq!(command, "type");
        assert_eq!(args["element_ref"], "frame-main:42");
        assert!(args.get("snapshot_id").is_none());
    }

    #[test]
    fn snapshot_fill_preflight_accepts_contenteditable_editor_target() {
        let step: super::args::FillStepSpec = serde_json::from_value(json!({
            "label": "Body",
            "value": "Hello editor"
        }))
        .expect("fill step should deserialize");
        let mut element = Element {
            index: 9,
            tag: ElementTag::Other,
            text: String::new(),
            attributes: std::collections::HashMap::new(),
            element_ref: Some("frame-main:99".to_string()),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: None,
        };
        element
            .attributes
            .insert("contenteditable".to_string(), "true".to_string());
        element
            .attributes
            .insert("role".to_string(), "textbox".to_string());

        let (command, args) = build_fill_step_command_for_resolved_target(&step, &element)
            .expect("contenteditable targets should use the safe text path");
        assert_eq!(command, "type");
        assert_eq!(args["element_ref"], "frame-main:99");
        assert_eq!(args["text"], "Hello editor");
    }

    #[test]
    fn snapshot_fill_preflight_rejection_includes_safe_path_diagnostics() {
        let step: super::args::FillStepSpec = serde_json::from_value(json!({
            "label": "Submit",
            "value": "wrong"
        }))
        .expect("fill step should deserialize");
        let element = Element {
            index: 5,
            tag: ElementTag::Button,
            text: "Submit".to_string(),
            attributes: std::collections::HashMap::new(),
            element_ref: Some("frame-main:55".to_string()),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: None,
        };

        let envelope = build_fill_step_command_for_resolved_target(&step, &element)
            .expect_err("button value writes should fail closed")
            .into_envelope();
        let context = envelope.context.expect("safe-path rejection context");
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert_eq!(context["target"]["tag"], "button");
        assert_eq!(context["rejection_reason"], "activation_only_control");
        assert_eq!(
            context["recommended_safe_fallback"],
            "Use `activate: true` or a click step instead of a value write for activation-only targets."
        );
    }

    #[test]
    fn snapshot_fill_preflight_rejects_targets_without_stable_identity() {
        let step: super::args::FillStepSpec = serde_json::from_value(json!({
            "label": "Email",
            "value": "user@example.com"
        }))
        .expect("fill step should deserialize");
        let element = Element {
            index: 3,
            tag: ElementTag::Input,
            text: String::new(),
            attributes: std::collections::HashMap::new(),
            element_ref: None,
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: None,
        };

        let error = build_fill_step_command_for_resolved_target(&step, &element)
            .expect_err("snapshot continuity should fail closed without element_ref")
            .into_envelope();
        assert_eq!(error.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn atomic_fill_rollback_uses_prior_text_value() {
        let element = Element {
            index: 3,
            tag: ElementTag::Input,
            text: String::new(),
            attributes: std::collections::HashMap::new(),
            element_ref: Some("frame-main:42".to_string()),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: None,
        };

        let (command, args) = build_atomic_rollback_command_for_resolved_target(
            &element,
            "type_text",
            "previous@example.com",
        )
        .expect("text rollback should build");
        assert_eq!(command, "type");
        assert_eq!(args["element_ref"], "frame-main:42");
        assert_eq!(args["text"], "previous@example.com");
        assert_eq!(args["clear"], true);
    }

    #[test]
    fn atomic_fill_rollback_rejects_editor_safe_surface_in_v1() {
        let mut element = Element {
            index: 9,
            tag: ElementTag::Other,
            text: String::new(),
            attributes: std::collections::HashMap::new(),
            element_ref: Some("frame-main:99".to_string()),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: None,
        };
        element
            .attributes
            .insert("contenteditable".to_string(), "true".to_string());

        let envelope = build_atomic_rollback_command_for_resolved_target(
            &element,
            "type_editor_text",
            "hello",
        )
        .expect_err("atomic v1 should fail closed on editor-safe rollback")
        .into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        let context = envelope.context.expect("atomic rollback rejection context");
        assert_eq!(context["write_mode"], "type_editor_text");
    }

    #[test]
    fn snapshot_submit_preflight_uses_element_ref_for_live_submit_click() {
        let element = Element {
            index: 7,
            tag: ElementTag::Button,
            text: "Submit".to_string(),
            attributes: std::collections::HashMap::new(),
            element_ref: Some("frame-main:77".to_string()),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: None,
        };

        let args = build_submit_command_for_resolved_target(&element)
            .expect("submit preflight should build a live click target");
        assert_eq!(args["element_ref"], "frame-main:77");
        assert!(args.get("snapshot_id").is_none());
    }

    #[test]
    fn typed_submit_locator_ignores_selection_without_locator() {
        let submit = SubmitLocatorArgs {
            first: true,
            ..SubmitLocatorArgs::default()
        };
        assert!(
            submit_args(&submit).is_none(),
            "selection-only submit args should not fabricate a locator"
        );
    }

    fn mock_completed_steps() -> Vec<serde_json::Value> {
        vec![
            json!({
                "step_index": 0,
                "status": "committed",
                "action": { "kind": "command", "command": "extract", "label": "get_title" },
                "result": {
                    "field_count": 1,
                    "fields": { "title": "Hello World", "count": 42 },
                    "items": [{ "name": "A" }, { "name": "B" }]
                }
            }),
            json!({
                "step_index": 1,
                "status": "committed",
                "action": { "kind": "command", "command": "exec" },
                "result": { "value": "computed" }
            }),
        ]
    }

    #[test]
    fn resolve_prev_result_field() {
        let completed = mock_completed_steps();
        let mut args = json!({ "code": "console.log('{{prev.result.value}}')" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["code"], "console.log('computed')");
    }

    #[test]
    fn resolve_steps_by_index() {
        let completed = mock_completed_steps();
        let mut args = json!({ "url": "https://example.com/{{steps[0].result.fields.title}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["url"], "https://example.com/Hello World");
    }

    #[test]
    fn resolve_steps_by_label() {
        let completed = mock_completed_steps();
        let mut args = json!({ "text": "{{steps[get_title].result.fields.title}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["text"], "Hello World");
    }

    #[test]
    fn resolve_array_index_in_path() {
        let completed = mock_completed_steps();
        let mut args = json!({ "name": "{{steps[0].result.items[1].name}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["name"], "B");
    }

    #[test]
    fn resolve_whole_placeholder_preserves_json_type() {
        let completed = mock_completed_steps();
        let mut args = json!({ "count": "{{steps[0].result.fields.count}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        // Whole-placeholder resolution preserves the original JSON type (number, not string).
        assert_eq!(args["count"], 42);
    }

    #[test]
    fn resolve_number_embedded_in_string_stringifies() {
        let completed = mock_completed_steps();
        let mut args = json!({ "msg": "Count is {{steps[0].result.fields.count}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["msg"], "Count is 42");
    }

    #[test]
    fn resolve_multiple_references_in_one_string() {
        let completed = mock_completed_steps();
        let mut args = json!({ "msg": "Title: {{steps[0].result.fields.title}}, Value: {{prev.result.value}}" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["msg"], "Title: Hello World, Value: computed");
    }

    #[test]
    fn resolve_no_references_passthrough() {
        let completed = mock_completed_steps();
        let mut args = json!({ "url": "https://example.com" });
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args["url"], "https://example.com");
    }

    #[test]
    fn resolve_prev_at_step_0_fails() {
        let completed = vec![];
        let mut args = json!({ "code": "{{prev.result.value}}" });
        let err = resolve_step_references(&mut args, &completed, 0).unwrap_err();
        assert_eq!(err.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_forward_reference_fails() {
        let completed = mock_completed_steps();
        let mut args = json!({ "code": "{{steps[2].result.value}}" });
        let err = resolve_step_references(&mut args, &completed, 2).unwrap_err();
        assert_eq!(err.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_unknown_label_fails() {
        let completed = mock_completed_steps();
        let mut args = json!({ "code": "{{steps[nonexistent].result.value}}" });
        let err = resolve_step_references(&mut args, &completed, 2).unwrap_err();
        assert_eq!(err.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_invalid_path_fails() {
        let completed = mock_completed_steps();
        let mut args = json!({ "code": "{{prev.result.missing_key}}" });
        let err = resolve_step_references(&mut args, &completed, 2).unwrap_err();
        assert_eq!(err.into_envelope().code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_template_string_passthrough_no_braces() {
        let result = resolve_template_string("hello world", &[], 0).unwrap();
        assert_eq!(result, "hello world");
    }

    #[test]
    fn resolve_template_string_unclosed_braces_literal() {
        let completed = mock_completed_steps();
        let result = resolve_template_string("prefix {{ no close", &completed, 2).unwrap();
        assert_eq!(result, "prefix {{ no close");
    }

    #[test]
    fn resolve_nested_array_in_args() {
        let completed = mock_completed_steps();
        let mut args = json!([{"value": "{{prev.result.value}}"}]);
        resolve_step_references(&mut args, &completed, 2).unwrap();
        assert_eq!(args[0]["value"], "computed");
    }

    #[test]
    fn resolve_whole_placeholder_preserves_bool() {
        let completed =
            vec![json!({ "action": { "command": "exec" }, "result": { "done": true } })];
        let mut args = json!({ "flag": "{{prev.result.done}}" });
        resolve_step_references(&mut args, &completed, 1).unwrap();
        assert_eq!(args["flag"], true);
    }

    #[test]
    fn resolve_whole_placeholder_preserves_object() {
        let completed = vec![
            json!({ "action": { "command": "state" }, "result": { "snapshot": { "url": "https://example.com", "elements": [] } } }),
        ];
        let mut args = json!({ "snap": "{{prev.result.snapshot}}" });
        resolve_step_references(&mut args, &completed, 1).unwrap();
        assert!(args["snap"].is_object());
        assert_eq!(args["snap"]["url"], "https://example.com");
    }

    #[test]
    fn parse_pipe_spec_rejects_duplicate_labels() {
        let spec = r#"[
            {"command": "state", "label": "fetch"},
            {"command": "observe", "label": "fetch"}
        ]"#;
        let rub_home = std::path::PathBuf::from("/tmp");
        let spec =
            NormalizedJsonSpec::from_raw_str(spec, "pipe").expect("duplicate label spec parses");
        let err = parse_pipe_spec(&spec, &rub_home).unwrap_err();
        let envelope = err.into_envelope();
        assert_eq!(envelope.code, ErrorCode::InvalidInput);
        assert!(envelope.message.contains("Duplicate step label"));
    }
}
