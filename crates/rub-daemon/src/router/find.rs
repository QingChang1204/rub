use super::addressing::resolve_elements;
use super::element_semantics::{accessible_label, semantic_role, test_id};
use super::request_args::{
    LocatorParseOptions, canonical_locator_json, parse_canonical_locator, require_live_locator,
};
use super::*;
use rub_core::locator::CanonicalLocator;

#[derive(Debug, serde::Serialize)]
struct FindMatchEntry<'a> {
    index: u32,
    tag: rub_core::model::ElementTag,
    text: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    element_ref: Option<&'a str>,
    role: String,
    label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    testid: Option<&'a str>,
}

#[derive(Debug, serde::Serialize)]
struct ContentFindMatchEntry<'a> {
    tag_name: &'a str,
    text: &'a str,
    role: &'a str,
    label: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    testid: Option<&'a str>,
}

pub(super) async fn cmd_find(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
) -> Result<serde_json::Value, RubError> {
    let locator = parse_canonical_locator(args, LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS)?;
    let limit = args
        .get("limit")
        .and_then(|value| value.as_u64())
        .map(|value| value as usize);
    let content = args
        .get("content")
        .and_then(|value| value.as_bool())
        .unwrap_or(false);

    if content {
        return cmd_find_content(router, args, state, limit).await;
    }

    let resolved = resolve_elements(router, args, state, "find").await?;
    let total_matches = resolved.elements.len();
    let matches = resolved
        .elements
        .iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|element| FindMatchEntry {
            index: element.index,
            tag: element.tag,
            text: &element.text,
            element_ref: element.element_ref.as_deref(),
            role: semantic_role(element),
            label: accessible_label(element),
            testid: test_id(element),
        })
        .collect::<Vec<_>>();

    Ok(find_payload(
        find_subject("interactive_snapshot", locator.as_ref(), None),
        serde_json::json!({
            "snapshot_id": resolved.snapshot_id,
            "match_count": total_matches,
            "returned_count": matches.len(),
            "truncated": total_matches > matches.len(),
            "matches": matches,
        }),
    ))
}

async fn cmd_find_content(
    router: &DaemonRouter,
    args: &serde_json::Value,
    state: &Arc<SessionState>,
    limit: Option<usize>,
) -> Result<serde_json::Value, RubError> {
    if args
        .get("snapshot_id")
        .and_then(|value| value.as_str())
        .is_some()
    {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::InvalidInput,
            "find --content requires a live DOM locator, not --snapshot",
            serde_json::json!({
                "content": true,
                "snapshot_id": args.get("snapshot_id").and_then(|value| value.as_str()),
            }),
            "Drop --snapshot and use --selector, --target-text, --role, --label, or --testid for content-anchor discovery",
        ));
    }

    let locator =
        parse_canonical_locator(args, LocatorParseOptions::OPTIONAL_ELEMENT_ADDRESS)?.ok_or_else(
            || {
                RubError::domain_with_context_and_suggestion(
                    ErrorCode::InvalidInput,
                    "find --content requires a selector, target_text, role, label, or testid locator",
                    serde_json::json!({
                        "content": true,
                    }),
                    "Provide --selector, --target-text, --role, --label, or --testid to search content anchors",
                )
            },
        )?;
    let locator = require_live_locator(
        locator,
        serde_json::json!({
            "content": true,
        }),
        "find --content requires a selector, target_text, role, label, or testid locator",
        "Provide --selector, --target-text, --role, --label, or --testid to search content anchors",
    )?;

    let selected_frame_id =
        super::frame_scope::effective_request_frame_id(router, args, state).await?;
    let content_matches = router
        .browser
        .find_content_matches(selected_frame_id.as_deref(), &locator)
        .await?;
    let total_matches = content_matches.len();
    let matches = content_matches
        .iter()
        .take(limit.unwrap_or(usize::MAX))
        .map(|entry| ContentFindMatchEntry {
            tag_name: &entry.tag_name,
            text: &entry.text,
            role: &entry.role,
            label: &entry.label,
            testid: entry.testid.as_deref(),
        })
        .collect::<Vec<_>>();

    if total_matches == 0 {
        return Err(RubError::domain_with_context_and_suggestion(
            ErrorCode::ElementNotFound,
            "find --content did not resolve to any live DOM content anchor",
            serde_json::json!({
                "content": true,
                "locator": locator,
            }),
            "Run 'rub observe' to see all interactive elements on the page, or broaden the locator. Add --first/--last/--nth if multiple matches are expected",
        ));
    }

    Ok(find_payload(
        find_subject("content", Some(&locator), selected_frame_id.as_deref()),
        serde_json::json!({
            "match_count": total_matches,
            "returned_count": matches.len(),
            "truncated": total_matches > matches.len(),
            "matches": matches,
        }),
    ))
}

fn find_payload(subject: serde_json::Value, result: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "subject": subject,
        "result": result,
    })
}

fn find_subject(
    surface: &str,
    locator: Option<&CanonicalLocator>,
    frame_id: Option<&str>,
) -> serde_json::Value {
    serde_json::json!({
        "kind": "find_query",
        "surface": surface,
        "frame_id": frame_id,
        "locator": locator.map(canonical_locator_json),
    })
}

#[cfg(test)]
mod tests {
    use super::find_subject;
    use rub_core::locator::CanonicalLocator;

    #[test]
    fn find_subject_projects_surface_and_locator() {
        let locator = CanonicalLocator::Role {
            role: "button".to_string(),
            selection: None,
        };
        let subject = find_subject("interactive_snapshot", Some(&locator), None);
        assert_eq!(subject["kind"], "find_query");
        assert_eq!(subject["surface"], "interactive_snapshot");
        assert_eq!(subject["locator"]["role"], "button");
    }
}
