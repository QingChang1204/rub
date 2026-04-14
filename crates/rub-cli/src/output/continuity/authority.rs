use serde_json::{Map, Value, json};
use std::path::Path;

pub(super) fn workflow_navigation_authority_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    let subject = data.get("subject")?.as_object()?;
    let result = data.get("result")?.as_object()?;
    let page = result.get("page")?.as_object()?;
    let active_tab = result.get("active_tab")?.as_object()?;
    let action = subject
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or(command);
    let requested_url = subject.get("requested_url").and_then(Value::as_str);
    let normalized_url = subject
        .get("normalized_url")
        .and_then(Value::as_str)
        .or(requested_url);
    let final_url = page
        .get("final_url")
        .and_then(Value::as_str)
        .or_else(|| page.get("url").and_then(Value::as_str))?;
    let active_tab_url = active_tab.get("url").and_then(Value::as_str)?;
    let observation = navigation_authority_observation(
        action,
        requested_url,
        normalized_url,
        final_url,
        page.get("title").and_then(Value::as_str),
        active_tab_url,
        active_tab.get("title").and_then(Value::as_str),
    );

    if final_url != active_tab_url {
        let signal = if is_new_tab_like_url(active_tab_url) {
            "active_tab_new_tab_drift"
        } else {
            "active_tab_page_mismatch"
        };
        let summary = if signal == "active_tab_new_tab_drift" {
            format!(
                "The {action} navigation committed to {final_url}, but the active tab is now {active_tab_url}. Re-establish tab authority in this same runtime before continuing."
            )
        } else {
            format!(
                "The {action} navigation committed to {final_url}, but the active tab is now {active_tab_url}. Re-establish which tab owns the workflow before continuing."
            )
        };
        return Some(super::workflow_same_runtime_projection_with_observation(
            command,
            session,
            rub_home,
            super::workflow_same_runtime_descriptor(
                signal,
                &summary,
                vec![
                    super::command_hint(
                        "rub tabs",
                        "inspect the current tab registry and confirm which tab is active",
                    ),
                    super::command_hint(
                        "rub switch <index>",
                        "re-select the tab that owns the committed page before continuing",
                    ),
                    super::command_hint(
                        "rub state compact",
                        "reacquire a fresh page snapshot from the currently selected tab",
                    ),
                    super::command_hint(
                        "rub runtime interference",
                        "inspect the live interference surface for popup or navigation drift evidence",
                    ),
                ],
                super::same_runtime_roles(
                    "tab_authority_runtime",
                    "Keep using the current runtime while you re-establish which tab and page currently own the workflow.",
                ),
                Some(observation),
            ),
        ));
    }

    if let Some(intended_url) = normalized_url
        && intended_url != final_url
    {
        let summary = format!(
            "The {action} navigation committed to {final_url} instead of the requested destination {intended_url}. Continue from the committed page in this same runtime."
        );
        return Some(super::workflow_same_runtime_projection_with_observation(
            command,
            session,
            rub_home,
            super::workflow_same_runtime_descriptor(
                "requested_navigation_redirected",
                &summary,
                vec![
                    super::command_hint(
                        "rub state compact",
                        "inspect the committed destination page before taking the next action",
                    ),
                    super::command_hint(
                        "rub explain blockers",
                        "confirm whether the redirected destination has a blocker before continuing",
                    ),
                ],
                super::same_runtime_roles(
                    "observation_runtime",
                    "Keep using the current runtime as the observation surface while you confirm the redirected destination page.",
                ),
                Some(observation),
            ),
        ));
    }

    if let Some(warning) = page.get("navigation_warning").and_then(Value::as_str) {
        let summary = format!(
            "The {action} navigation completed with a browser warning: {warning}. Re-check the committed page in this same runtime before continuing."
        );
        return Some(super::workflow_same_runtime_projection_with_observation(
            command,
            session,
            rub_home,
            super::workflow_same_runtime_descriptor(
                "navigation_warning",
                &summary,
                vec![
                    super::command_hint(
                        "rub state compact",
                        "inspect the committed page and confirm whether the warning changed the destination",
                    ),
                    super::command_hint(
                        "rub runtime interference",
                        "inspect the live interference surface if the warning may have changed active-tab or route ownership",
                    ),
                ],
                super::same_runtime_roles(
                    "observation_runtime",
                    "Keep using the current runtime while you verify the committed page and any browser warning side effects.",
                ),
                Some(observation),
            ),
        ));
    }

    None
}

pub(super) fn workflow_runtime_interference_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    let runtime = data.get("runtime")?.as_object()?;
    if runtime.get("status").and_then(Value::as_str) != Some("active") {
        return None;
    }
    let interference = runtime.get("current_interference")?.as_object()?;
    let kind = interference.get("kind").and_then(Value::as_str)?;
    let base_summary = interference
        .get("summary")
        .and_then(Value::as_str)
        .unwrap_or("runtime interference detected");
    let observation = json!({
        "kind": "runtime_interference_observation",
        "interference_kind": kind,
        "current_url": interference.get("current_url").cloned().unwrap_or(Value::Null),
        "primary_url": interference.get("primary_url").cloned().unwrap_or(Value::Null),
    });

    match kind {
        "popup_hijack" => Some(super::workflow_same_runtime_projection_with_observation(
            command,
            session,
            rub_home,
            super::workflow_same_runtime_descriptor(
                "popup_hijack",
                "An unexpected active-tab drift was detected in this runtime. Re-establish tab authority before continuing the workflow.",
                vec![
                    super::command_hint(
                        "rub tabs",
                        "inspect the current tab registry and confirm which tab became active",
                    ),
                    super::command_hint(
                        "rub switch <index>",
                        "switch back to the workflow-owning tab before retrying any action",
                    ),
                    super::command_hint(
                        "rub runtime interference",
                        "re-check the live interference surface after restoring the intended tab",
                    ),
                ],
                super::same_runtime_roles(
                    "tab_authority_runtime",
                    "Keep using the current runtime while you restore the intended active tab.",
                ),
                Some(observation),
            ),
        )),
        "interstitial_navigation" => {
            Some(super::workflow_same_runtime_projection_with_observation(
                command,
                session,
                rub_home,
                super::workflow_same_runtime_descriptor(
                    "interstitial_navigation",
                    &format!(
                        "{base_summary}. Keep recovery in this same runtime and clear the interstitial before continuing."
                    ),
                    vec![
                        super::command_hint(
                            "rub interference recover",
                            "let the runtime recovery surface attempt the authoritative navigation recovery",
                        ),
                        super::command_hint(
                            "rub state compact",
                            "re-read the current page after recovery to confirm the primary page is restored",
                        ),
                        super::command_hint(
                            "rub tabs",
                            "confirm the active tab still matches the primary workflow tab after recovery",
                        ),
                    ],
                    super::same_runtime_roles(
                        "manual_recovery_runtime",
                        "Keep using the current runtime as the recovery surface while you clear the interstitial drift.",
                    ),
                    Some(observation),
                ),
            ))
        }
        "unknown_navigation_drift" => {
            Some(super::workflow_same_runtime_projection_with_observation(
                command,
                session,
                rub_home,
                super::workflow_same_runtime_descriptor(
                    "unknown_navigation_drift",
                    "Unexpected navigation drift was detected in this runtime. Re-establish the active tab and committed page before continuing.",
                    vec![
                        super::command_hint(
                            "rub tabs",
                            "inspect the current tab registry and identify the tab that now owns the workflow",
                        ),
                        super::command_hint(
                            "rub state compact",
                            "reacquire a fresh page snapshot from the currently selected tab",
                        ),
                        super::command_hint(
                            "rub runtime interference",
                            "re-check the live interference surface after restoring tab/page authority",
                        ),
                    ],
                    super::same_runtime_roles(
                        "tab_authority_runtime",
                        "Keep using the current runtime while you restore tab and page authority after the drift.",
                    ),
                    Some(observation),
                ),
            ))
        }
        _ => None,
    }
}

pub(super) fn workflow_runtime_frame_projection(
    command: &str,
    session: &str,
    rub_home: &Path,
    data: &Map<String, Value>,
) -> Option<Value> {
    let runtime = data.get("runtime")?.as_object()?;
    let status = runtime.get("status").and_then(Value::as_str)?;
    let observation = json!({
        "kind": "frame_authority_observation",
        "frame_status": status,
        "current_frame_id": runtime
            .get("current_frame")
            .and_then(Value::as_object)
            .and_then(|frame| frame.get("frame_id"))
            .cloned()
            .unwrap_or(Value::Null),
        "current_frame_name": runtime
            .get("current_frame")
            .and_then(Value::as_object)
            .and_then(|frame| frame.get("name"))
            .cloned()
            .unwrap_or(Value::Null),
        "primary_frame_id": runtime
            .get("primary_frame")
            .and_then(Value::as_object)
            .and_then(|frame| frame.get("frame_id"))
            .cloned()
            .unwrap_or(Value::Null),
        "primary_frame_name": runtime
            .get("primary_frame")
            .and_then(Value::as_object)
            .and_then(|frame| frame.get("name"))
            .cloned()
            .unwrap_or(Value::Null),
        "degraded_reason": runtime
            .get("degraded_reason")
            .cloned()
            .unwrap_or(Value::Null),
    });

    match status {
        "stale" => Some(super::workflow_same_runtime_projection_with_observation(
            command,
            session,
            rub_home,
            super::workflow_same_runtime_descriptor(
                "frame_runtime_stale",
                "The selected frame is no longer live in this runtime. Re-establish frame authority and take a fresh snapshot before continuing.",
                vec![
                    super::command_hint(
                        "rub frames",
                        "inspect the live frame inventory and confirm which frames still exist",
                    ),
                    super::command_hint(
                        "rub frame --top or rub frame --name <frame-name>",
                        "restore a live frame selection before taking the next action",
                    ),
                    super::command_hint(
                        "rub state",
                        "capture a fresh snapshot after frame authority has been restored",
                    ),
                ],
                super::same_runtime_roles(
                    "frame_authority_runtime",
                    "Keep using the current runtime while you restore frame authority and refresh page observation.",
                ),
                Some(observation),
            ),
        )),
        "degraded" => {
            let degraded_reason = runtime
                .get("degraded_reason")
                .and_then(Value::as_str)
                .unwrap_or("frame_runtime_degraded");
            Some(super::workflow_same_runtime_projection_with_observation(
                command,
                session,
                rub_home,
                super::workflow_same_runtime_descriptor(
                    "frame_runtime_degraded",
                    &format!(
                        "Frame authority is degraded in this runtime ({degraded_reason}). Re-establish a live frame view before continuing."
                    ),
                    vec![
                        super::command_hint(
                            "rub frames",
                            "inspect the live frame inventory and confirm which frame contexts are currently available",
                        ),
                        super::command_hint(
                            "rub frame --top or rub frame --name <frame-name>",
                            "restore a known-good frame selection before retrying",
                        ),
                        super::command_hint(
                            "rub state",
                            "capture a fresh snapshot after frame authority has been re-established",
                        ),
                    ],
                    super::same_runtime_roles(
                        "frame_authority_runtime",
                        "Keep using the current runtime while you recover a usable frame context.",
                    ),
                    Some(observation),
                ),
            ))
        }
        _ => None,
    }
}

fn navigation_authority_observation(
    action: &str,
    requested_url: Option<&str>,
    normalized_url: Option<&str>,
    final_url: &str,
    page_title: Option<&str>,
    active_tab_url: &str,
    active_tab_title: Option<&str>,
) -> Value {
    json!({
        "kind": "navigation_authority_observation",
        "action": action,
        "requested_url": requested_url,
        "normalized_url": normalized_url,
        "final_page_url": final_url,
        "page_title": page_title,
        "active_tab_url": active_tab_url,
        "active_tab_title": active_tab_title,
    })
}

fn is_new_tab_like_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    lower.starts_with("about:blank")
        || lower.starts_with("chrome://newtab")
        || lower.starts_with("chrome-untrusted://new-tab-page")
        || lower.starts_with("about:srcdoc")
        || lower.starts_with("chrome-error://")
}
