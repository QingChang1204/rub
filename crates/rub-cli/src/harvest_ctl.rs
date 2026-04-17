mod source;

use std::path::Path;
use std::time::{Duration, Instant};

use crate::commands::{Commands, EffectiveCli, InspectSubcommand};
use crate::daemon_ctl;
use crate::session_policy::{
    materialize_connection_request_with_deadline, parse_connection_request,
    requested_attachment_identity, requires_existing_session_validation,
    validate_existing_session_connection_request_with_deadline,
};
use rub_core::error::{ErrorCode, ErrorEnvelope, RubError};
use rub_ipc::protocol::{IpcRequest, ResponseStatus};
use serde::Serialize;
use uuid::Uuid;

use self::source::{load_extract_spec, load_harvest_sources};

#[derive(Debug, Serialize)]
struct FollowPageExtractSummary {
    complete: bool,
    source_count: u32,
    attempted_count: u32,
    harvested_count: u32,
    failed_count: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
}

#[derive(Debug, Serialize)]
struct FollowPageExtractSubject {
    kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    input_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name_field: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    limit: Option<u32>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum FollowPageExtractStatus {
    Harvested,
    Failed,
}

#[derive(Debug, Serialize)]
struct FollowPageExtractSourceInfo {
    url: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
    row: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct FollowPageExtractPageInfo {
    #[serde(skip_serializing_if = "Option::is_none")]
    final_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<String>,
}

#[derive(Debug, Serialize)]
struct FollowPageExtractEntryResult {
    fields: serde_json::Value,
}

#[derive(Debug, Serialize)]
struct FollowPageExtractEntry {
    index: u32,
    status: FollowPageExtractStatus,
    source: FollowPageExtractSourceInfo,
    page: FollowPageExtractPageInfo,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<FollowPageExtractEntryResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorEnvelope>,
}

#[derive(Debug)]
struct HarvestSource {
    index: u32,
    url: String,
    source_name: Option<String>,
    source_row: serde_json::Value,
}

struct HarvestDispatchContext {
    daemon_args: Vec<String>,
    connection_request: crate::session_policy::ConnectionRequest,
    attachment_identity: Option<String>,
}

pub(crate) async fn inspect_harvest(cli: &EffectiveCli) -> Result<serde_json::Value, RubError> {
    let Commands::Inspect(InspectSubcommand::Harvest {
        file,
        input_field,
        url_field,
        name_field,
        base_url,
        extract,
        extract_file,
        field,
        limit,
    }) = &cli.command
    else {
        return Err(RubError::Internal(
            "inspect_harvest called for a non-harvest command".to_string(),
        ));
    };
    let deadline = Instant::now() + Duration::from_millis(cli.timeout.max(1));

    let (extract_spec, extract_spec_source) =
        load_extract_spec(extract.as_deref(), extract_file.as_deref(), field).await?;
    let sources = load_harvest_sources(
        Path::new(file),
        input_field.as_deref(),
        url_field.as_deref(),
        name_field.as_deref(),
        base_url.as_deref(),
        *limit,
    )
    .await?;

    let source_count = sources.len() as u32;
    let subject = FollowPageExtractSubject {
        kind: "follow_page_extract",
        input_field: input_field.clone(),
        url_field: url_field.clone(),
        name_field: name_field.clone(),
        base_url: base_url.clone(),
        limit: *limit,
    };
    let dispatch = HarvestDispatchContext::new(cli, deadline).await?;
    let mut attempted_count = 0u32;
    let mut harvested_count = 0u32;
    let mut failed_count = 0u32;
    let mut entries = Vec::with_capacity(sources.len());

    for source in sources {
        attempted_count = attempted_count.saturating_add(1);
        let open_request = IpcRequest::new(
            "open",
            serde_json::json!({
                "url": source.url,
                "load_strategy": "load",
            }),
            cli.timeout,
        )
        .with_command_id(Uuid::now_v7().to_string())
        .expect("UUID command_id must be valid");
        let open_data = match dispatch.send_request(cli, &open_request, deadline).await {
            Ok(data) => data,
            Err(error) => {
                failed_count = failed_count.saturating_add(1);
                entries.push(FollowPageExtractEntry {
                    index: source.index,
                    status: FollowPageExtractStatus::Failed,
                    source: FollowPageExtractSourceInfo {
                        url: source.url,
                        name: source.source_name,
                        row: source.source_row,
                    },
                    page: FollowPageExtractPageInfo {
                        final_url: None,
                        title: None,
                    },
                    result: None,
                    error: Some(error),
                });
                if entries
                    .last()
                    .and_then(|entry| entry.error.as_ref())
                    .is_some_and(harvest_deadline_exhausted)
                {
                    break;
                }
                continue;
            }
        };

        let extract_request = IpcRequest::new(
            "extract",
            serde_json::json!({
                "spec": extract_spec,
                "spec_source": extract_spec_source,
            }),
            cli.timeout,
        )
        .with_command_id(Uuid::now_v7().to_string())
        .expect("UUID command_id must be valid");
        match dispatch.send_request(cli, &extract_request, deadline).await {
            Ok(extract_data) => {
                harvested_count = harvested_count.saturating_add(1);
                entries.push(FollowPageExtractEntry {
                    index: source.index,
                    status: FollowPageExtractStatus::Harvested,
                    source: FollowPageExtractSourceInfo {
                        url: source.url,
                        name: source.source_name,
                        row: source.source_row,
                    },
                    page: FollowPageExtractPageInfo {
                        final_url: open_data
                            .get("result")
                            .and_then(|value| value.get("page"))
                            .and_then(|value| value.get("final_url"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                        title: open_data
                            .get("result")
                            .and_then(|value| value.get("page"))
                            .and_then(|value| value.get("title"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                    },
                    result: extract_data
                        .get("result")
                        .and_then(|value| value.get("fields"))
                        .cloned()
                        .map(|fields| FollowPageExtractEntryResult { fields }),
                    error: None,
                });
            }
            Err(error) => {
                failed_count = failed_count.saturating_add(1);
                entries.push(FollowPageExtractEntry {
                    index: source.index,
                    status: FollowPageExtractStatus::Failed,
                    source: FollowPageExtractSourceInfo {
                        url: source.url,
                        name: source.source_name,
                        row: source.source_row,
                    },
                    page: FollowPageExtractPageInfo {
                        final_url: open_data
                            .get("result")
                            .and_then(|value| value.get("page"))
                            .and_then(|value| value.get("final_url"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                        title: open_data
                            .get("result")
                            .and_then(|value| value.get("page"))
                            .and_then(|value| value.get("title"))
                            .and_then(|value| value.as_str())
                            .map(str::to_string),
                    },
                    result: None,
                    error: Some(error),
                });
                if entries
                    .last()
                    .and_then(|entry| entry.error.as_ref())
                    .is_some_and(harvest_deadline_exhausted)
                {
                    break;
                }
            }
        }
    }

    Ok(serde_json::json!({
        "subject": subject,
        "result": {
            "summary": FollowPageExtractSummary {
                complete: failed_count == 0,
                source_count,
                attempted_count,
                harvested_count,
                failed_count,
                base_url: base_url.clone(),
            },
            "entries": entries,
        }
    }))
}

impl HarvestDispatchContext {
    async fn new(cli: &EffectiveCli, deadline: Instant) -> Result<Self, RubError> {
        let connection_request = materialize_connection_request_with_deadline(
            &parse_connection_request(cli)?,
            Some(deadline),
            Some(cli.timeout.max(1)),
        )
        .await?;
        Ok(Self {
            daemon_args: crate::daemon_args(cli, &connection_request),
            attachment_identity: requested_attachment_identity(cli, &connection_request),
            connection_request,
        })
    }

    async fn send_request(
        &self,
        cli: &EffectiveCli,
        request: &IpcRequest,
        deadline: Instant,
    ) -> Result<serde_json::Value, ErrorEnvelope> {
        if remaining_harvest_budget_ms(deadline).is_none() {
            return Err(harvest_timeout_envelope(cli.timeout));
        }
        let bootstrap = daemon_ctl::bootstrap_client(
            &cli.rub_home,
            &cli.session,
            deadline,
            request.timeout_ms,
            &self.daemon_args,
            self.attachment_identity.as_deref(),
        )
        .await
        .map_err(|error| error.into_envelope())?;
        let daemon_session_id = bootstrap.daemon_session_id;
        let mut client = bootstrap.client;
        if requires_existing_session_validation(
            bootstrap.connected_to_existing_daemon,
            &self.connection_request,
            cli,
        ) {
            validate_existing_session_connection_request_with_deadline(
                cli,
                &self.connection_request,
                &mut client,
                daemon_session_id.as_deref(),
                Some(deadline),
                Some(cli.timeout.max(1)),
            )
            .await
            .map_err(|error| error.into_envelope())?;
        }
        let response = daemon_ctl::send_request_with_replay_recovery(
            &mut client,
            request,
            deadline,
            daemon_ctl::ReplayRecoveryContext {
                rub_home: &cli.rub_home,
                session: &cli.session,
                daemon_args: &self.daemon_args,
                attachment_identity: self.attachment_identity.as_deref(),
                original_daemon_session_id: daemon_session_id.as_deref(),
            },
        )
        .await
        .map_err(|error| error.into_envelope())?;
        match response.status {
            ResponseStatus::Success => response.data.ok_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "missing success payload in success response",
                )
            }),
            ResponseStatus::Error => Err(response.error.unwrap_or_else(|| {
                ErrorEnvelope::new(
                    ErrorCode::IpcProtocolError,
                    "missing error envelope in error response",
                )
            })),
        }
    }
}

fn remaining_harvest_budget_ms(deadline: Instant) -> Option<u64> {
    let remaining = deadline.saturating_duration_since(Instant::now());
    let millis = remaining.as_millis() as u64;
    (millis > 0).then_some(millis)
}

fn harvest_timeout_envelope(timeout_ms: u64) -> ErrorEnvelope {
    ErrorEnvelope::new(
        ErrorCode::IpcTimeout,
        format!("inspect harvest exceeded overall timeout of {timeout_ms}ms"),
    )
    .with_context(serde_json::json!({
        "reason": "inspect_harvest_timeout_budget_exhausted",
        "timeout_ms": timeout_ms,
    }))
}

fn harvest_deadline_exhausted(error: &ErrorEnvelope) -> bool {
    error.code == ErrorCode::IpcTimeout
        && error
            .context
            .as_ref()
            .and_then(|value| value.get("reason"))
            .and_then(|value| value.as_str())
            == Some("inspect_harvest_timeout_budget_exhausted")
}

#[derive(Debug)]
struct ParsedHarvestSource {
    url: String,
    source_name: Option<String>,
    source_row: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::source::{
        harvest_extract_spec_file_state, harvest_source_file_state, load_extract_spec,
        parse_json_harvest_sources, resolve_json_harvest_root,
    };
    use super::{
        harvest_deadline_exhausted, harvest_timeout_envelope, remaining_harvest_budget_ms,
    };
    use serde_json::json;
    use std::path::Path;
    use std::time::{Duration, Instant};

    #[test]
    fn resolve_json_harvest_root_accepts_canonical_batch_object_root() {
        let value = json!({
            "items": [
                { "href": "/detail/a" }
            ],
            "item_count": 1
        });
        let resolved = resolve_json_harvest_root(Path::new("/tmp/rows.json"), &value, None)
            .expect("canonical batch root");
        assert_eq!(resolved, &value["items"]);
    }

    #[test]
    fn resolve_json_harvest_root_auto_detects_canonical_result_batch_root() {
        let value = json!({
            "data": {
                "result": {
                    "items": [
                        { "href": "/detail/a" }
                    ],
                    "item_count": 1
                }
            }
        });
        let resolved = resolve_json_harvest_root(Path::new("/tmp/rows.json"), &value, None)
            .expect("canonical result batch");
        assert_eq!(resolved, &value["data"]["result"]["items"]);
    }

    #[test]
    fn resolve_json_harvest_root_accepts_explicit_canonical_batch_path() {
        let value = json!({
            "data": {
                "result": {
                    "items": [
                        { "href": "/detail/a" }
                    ],
                    "item_count": 1
                }
            }
        });
        let resolved =
            resolve_json_harvest_root(Path::new("/tmp/rows.json"), &value, Some("data.result"))
                .expect("explicit canonical batch path");
        assert_eq!(resolved, &value["data"]["result"]["items"]);
    }

    #[test]
    fn remaining_harvest_budget_is_none_once_deadline_has_elapsed() {
        let deadline = Instant::now() - Duration::from_millis(1);
        assert_eq!(remaining_harvest_budget_ms(deadline), None);
    }

    #[test]
    fn harvest_timeout_envelope_is_marked_as_terminal_deadline_exhaustion() {
        let envelope = harvest_timeout_envelope(5_000);
        assert!(harvest_deadline_exhausted(&envelope));
        assert_eq!(envelope.code, rub_core::error::ErrorCode::IpcTimeout);
    }

    #[test]
    fn resolve_json_harvest_root_missing_input_field_preserves_file_state() {
        let value = json!({
            "data": {
                "result": {
                    "items": []
                }
            }
        });
        let error =
            resolve_json_harvest_root(Path::new("/tmp/rows.json"), &value, Some("data.missing"))
                .expect_err("missing input field should fail")
                .into_envelope();
        let context = error.context.expect("harvest root context");
        assert_eq!(context["file"], "/tmp/rows.json");
        assert_eq!(context["file_state"], json!(harvest_source_file_state()));
    }

    #[test]
    fn parse_json_harvest_sources_invalid_root_preserves_file_state() {
        let error = parse_json_harvest_sources(
            Path::new("/tmp/rows.json"),
            &json!({ "unexpected": true }),
            None,
            None,
            None,
        )
        .expect_err("invalid root should fail")
        .into_envelope();
        let context = error.context.expect("harvest parse context");
        assert_eq!(context["file"], "/tmp/rows.json");
        assert_eq!(context["file_state"], json!(harvest_source_file_state()));
    }

    #[tokio::test]
    async fn load_extract_spec_missing_file_preserves_path_state() {
        let error = load_extract_spec(None, Some("./missing-extract.json"), &[])
            .await
            .expect_err("missing extract spec file should fail")
            .into_envelope();
        let context = error.context.expect("extract spec context");
        assert_eq!(context["path"], "./missing-extract.json");
        assert_eq!(
            context["path_state"],
            json!(harvest_extract_spec_file_state())
        );
        assert_eq!(
            context["reason"],
            "inspect_harvest_extract_spec_file_not_found"
        );
    }

    #[tokio::test]
    async fn load_extract_spec_builder_preserves_structured_spec() {
        let (spec, spec_source) = load_extract_spec(None, None, &["title=h1".to_string()])
            .await
            .expect("builder extract spec should load");
        assert_eq!(
            spec.as_value(),
            &json!({
                "title": {
                    "selector": "h1",
                    "kind": "text"
                }
            })
        );
        assert_eq!(
            spec_source,
            json!({
                "kind": "builder",
                "fields": ["title"],
            })
        );
    }
}
