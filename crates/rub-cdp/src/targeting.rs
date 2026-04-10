use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, GetContentQuadsParams, RequestNodeParams, ResolveNodeParams,
};
use chromiumoxide::cdp::js_protocol::runtime::RemoteObjectId;
use chromiumoxide::element::Element as CdpElement;
use chromiumoxide::layout::Point;
use rub_core::error::{ErrorCode, RubError};
use rub_core::model::{BoundingBox, Element, ElementTag};
use std::collections::HashMap;
use std::sync::Arc;

const READ_FALLBACK_SELECTOR: &str = "*";

#[derive(Debug, Clone)]
pub(crate) struct ResolvedElement {
    pub remote_object_id: RemoteObjectId,
    pub backend_node_id: Option<BackendNodeId>,
    pub verified: bool,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum CandidateMatchRank {
    Unscored,
    Scored(f64),
}

pub(crate) async fn resolve_element(
    page: &Arc<Page>,
    element: &Element,
) -> Result<ResolvedElement, RubError> {
    let Some(backend_node_id) = parse_backend_node_id(element.element_ref.as_deref()) else {
        return Err(unverified_write_target_error(
            element,
            "snapshot element does not carry a verified backend node id",
        ));
    };

    let remote_object_id = resolve_remote_object(page, backend_node_id)
        .await
        .map_err(|_| {
            unverified_write_target_error(
                element,
                "snapshot element backend node id no longer resolves in the live DOM",
            )
        })?;

    Ok(ResolvedElement {
        remote_object_id,
        backend_node_id: Some(backend_node_id),
        verified: true,
    })
}

pub(crate) async fn resolve_read_element(
    page: &Arc<Page>,
    element: &Element,
) -> Result<ResolvedElement, RubError> {
    resolve_element_with_fallback_selector(
        page,
        element,
        READ_FALLBACK_SELECTOR,
        "Fallback element query failed",
    )
    .await
}

async fn resolve_element_with_fallback_selector(
    page: &Arc<Page>,
    element: &Element,
    fallback_selector: &str,
    query_failure_label: &str,
) -> Result<ResolvedElement, RubError> {
    let frame_id = parse_element_ref_frame_id(element.element_ref.as_deref());
    if let Some(backend_node_id) = parse_backend_node_id(element.element_ref.as_deref())
        && let Ok(remote_object_id) = resolve_remote_object(page, backend_node_id).await
    {
        return Ok(ResolvedElement {
            remote_object_id,
            backend_node_id: Some(backend_node_id),
            verified: true,
        });
    }

    if frame_id.is_some()
        && let Some(resolved) =
            resolve_element_within_frame_snapshot(page, element, frame_id).await?
    {
        return Ok(resolved);
    }

    let candidates = page
        .find_elements(fallback_selector)
        .await
        .map_err(|e| RubError::Internal(format!("{query_failure_label}: {e}")))?;

    let mut best_match: Option<(CandidateMatchRank, ResolvedElement)> = None;

    for candidate in candidates {
        if let Some(rank) = candidate_match_rank(page, element, &candidate).await? {
            let resolved = ResolvedElement {
                remote_object_id: candidate.remote_object_id.clone(),
                backend_node_id: Some(candidate.backend_node_id),
                verified: false,
            };

            if best_match
                .as_ref()
                .is_none_or(|(best_rank, _)| candidate_rank_precedes(rank, *best_rank))
            {
                best_match = Some((rank, resolved));
            }
        }
    }

    if let Some((_, resolved)) = best_match {
        return Ok(resolved);
    }

    Err(RubError::domain(
        ErrorCode::ElementNotFound,
        format!("Could not resolve element index {}", element.index),
    ))
}

fn unverified_write_target_error(element: &Element, reason: &str) -> RubError {
    RubError::domain_with_context(
        ErrorCode::StaleSnapshot,
        "Mutating interactions require a verified target from the current snapshot authority",
        serde_json::json!({
            "reason": "unverified_write_target",
            "detail": reason,
            "element_index": element.index,
            "element_ref": element.element_ref,
            "tag": element.tag,
        }),
    )
}

async fn resolve_element_within_frame_snapshot(
    page: &Arc<Page>,
    expected: &Element,
    frame_id: Option<&str>,
) -> Result<Option<ResolvedElement>, RubError> {
    let snapshot = crate::dom::build_snapshot_for_frame(page, 0, Some(0), frame_id).await?;
    let mut best_match: Option<(CandidateMatchRank, ResolvedElement)> = None;

    for candidate in snapshot.elements {
        let Some(rank) = snapshot_candidate_match_rank(expected, &candidate) else {
            continue;
        };
        let Some(backend_node_id) = parse_backend_node_id(candidate.element_ref.as_deref()) else {
            continue;
        };
        let Ok(remote_object_id) = resolve_remote_object(page, backend_node_id).await else {
            continue;
        };
        let resolved = ResolvedElement {
            remote_object_id,
            backend_node_id: Some(backend_node_id),
            verified: false,
        };
        if best_match
            .as_ref()
            .is_none_or(|(best_rank, _)| candidate_rank_precedes(rank, *best_rank))
        {
            best_match = Some((rank, resolved));
        }
    }

    Ok(best_match.map(|(_, resolved)| resolved))
}

pub(crate) async fn resolve_activation_target(
    page: &Arc<Page>,
    resolved: &ResolvedElement,
    tag: ElementTag,
) -> Result<ResolvedElement, RubError> {
    if !matches!(tag, ElementTag::Checkbox | ElementTag::Radio) {
        return Ok(resolved.clone());
    }

    let remote_object_id = call_function_returning_object_id(
        page,
        &resolved.remote_object_id,
        r#"function() {
            const el = this;
            const id = typeof el.id === 'string' && el.id ? el.id : null;
            const viaFor = id ? document.querySelector(`label[for="${CSS.escape(id)}"]`) : null;
            const viaClosest = typeof el.closest === 'function' ? el.closest('label') : null;
            return viaClosest || viaFor || el;
        }"#,
    )
    .await
    .unwrap_or_else(|_| resolved.remote_object_id.clone());

    let backend_node_id = backend_node_id_for_object(page, &remote_object_id)
        .await
        .ok();

    Ok(ResolvedElement {
        remote_object_id,
        backend_node_id,
        verified: resolved.verified,
    })
}

pub(crate) async fn resolve_pointer_point(
    page: &Arc<Page>,
    target: &ResolvedElement,
) -> Result<Point, RubError> {
    let backend_node_id = target.backend_node_id.ok_or_else(|| {
        RubError::domain(
            ErrorCode::ElementNotInteractable,
            "Element has no backend node id",
        )
    })?;

    scroll_target_into_view(page, &target.remote_object_id).await?;

    let response = page
        .execute(
            GetContentQuadsParams::builder()
                .backend_node_id(backend_node_id)
                .build(),
        )
        .await
        .map_err(|e| RubError::Internal(format!("GetContentQuads failed: {e}")))?;

    for quad in response.quads.iter().filter(|quad| quad.inner().len() == 8) {
        for point in candidate_points(quad.inner()) {
            if hit_test_matches(page, &target.remote_object_id, point).await? {
                return Ok(point);
            }
        }
    }

    Err(RubError::domain(
        ErrorCode::ElementNotInteractable,
        "Element does not expose a hittable clickable point",
    ))
}

pub(crate) fn parse_backend_node_id(element_ref: Option<&str>) -> Option<BackendNodeId> {
    let (_, backend_id) = element_ref?.split_once(':')?;
    let parsed = backend_id.parse::<i64>().ok()?;
    Some(BackendNodeId::new(parsed))
}

pub(crate) fn parse_element_ref_frame_id(element_ref: Option<&str>) -> Option<&str> {
    let (frame_id, _) = element_ref?.split_once(':')?;
    (!frame_id.trim().is_empty()).then_some(frame_id)
}

async fn resolve_remote_object(
    page: &Arc<Page>,
    backend_node_id: BackendNodeId,
) -> Result<RemoteObjectId, RubError> {
    let response = page
        .execute(
            ResolveNodeParams::builder()
                .backend_node_id(backend_node_id)
                .build(),
        )
        .await
        .map_err(|e| RubError::Internal(format!("ResolveNode failed: {e}")))?;

    response.result.object.object_id.ok_or_else(|| {
        RubError::domain(ErrorCode::ElementNotFound, "Resolved node has no objectId")
    })
}

pub(crate) async fn backend_node_id_for_object(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
) -> Result<BackendNodeId, RubError> {
    let node = page
        .execute(RequestNodeParams::new(object_id.clone()))
        .await
        .map_err(|e| RubError::Internal(format!("RequestNode failed: {e}")))?;
    let described = page
        .execute(
            DescribeNodeParams::builder()
                .node_id(node.node_id)
                .depth(0)
                .build(),
        )
        .await
        .map_err(|e| RubError::Internal(format!("DescribeNode failed: {e}")))?;
    Ok(described.node.backend_node_id)
}

async fn scroll_target_into_view(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
) -> Result<(), RubError> {
    crate::js::call_function(
        page,
        object_id,
        "function() { this.scrollIntoView({ block: 'center', inline: 'center', behavior: 'instant' }); }",
        true,
    )
    .await
}

async fn hit_test_matches(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    point: Point,
) -> Result<bool, RubError> {
    let value = crate::js::call_function_returning_value(
        page,
        object_id,
        &format!(
            "function() {{
                // GetContentQuads returns top-level viewport coordinates regardless
                // of whether the element lives in an iframe. Use the top-level
                // document for hit-testing to avoid execution-context /
                // coordinate-space mismatches that arise when the remote_object_id
                // is resolved via the top-level page context (making `window` in
                // this function refer to the top window even for iframe elements).
                const x = {:.3};
                const y = {:.3};
                try {{
                    const topDoc = window.top.document;
                    const hit = topDoc.elementFromPoint(x, y);
                    if (!hit) return false;
                    // Fast path: hit is the element or an ancestor/descendant.
                    if (hit === this || this.contains(hit) || hit.contains(this)) return true;
                    // Cross-iframe path: if the top-level hit is an <iframe>,
                    // descend into its contentDocument and re-test using
                    // frame-local coordinates.
                    if (hit.contentDocument) {{
                        try {{
                            const fr = hit.getBoundingClientRect();
                            const inner = hit.contentDocument.elementFromPoint(
                                x - fr.left, y - fr.top
                            );
                            if (inner && (inner === this || this.contains(inner) || inner.contains(this))) return true;
                        }} catch (_inner) {{}}
                    }}
                    return false;
                }} catch (_error) {{
                    return false;
                }}
            }}",
            point.x, point.y
        ),
    )
    .await?;

    Ok(value.as_bool().unwrap_or(false))
}

fn candidate_points(values: &[f64]) -> Vec<Point> {
    let xs = [values[0], values[2], values[4], values[6]];
    let ys = [values[1], values[3], values[5], values[7]];
    let min_x = xs.iter().copied().fold(f64::INFINITY, f64::min);
    let max_x = xs.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let min_y = ys.iter().copied().fold(f64::INFINITY, f64::min);
    let max_y = ys.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let center = Point {
        x: xs.iter().sum::<f64>() / 4.0,
        y: ys.iter().sum::<f64>() / 4.0,
    };
    let inset_x = ((max_x - min_x) * 0.2).clamp(1.0, 8.0);
    let inset_y = ((max_y - min_y) * 0.2).clamp(1.0, 8.0);

    vec![
        center,
        Point {
            x: min_x + inset_x,
            y: min_y + inset_y,
        },
        Point {
            x: max_x - inset_x,
            y: min_y + inset_y,
        },
        Point {
            x: min_x + inset_x,
            y: max_y - inset_y,
        },
        Point {
            x: max_x - inset_x,
            y: max_y - inset_y,
        },
    ]
}

async fn call_function_returning_object_id(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
    function_declaration: &str,
) -> Result<RemoteObjectId, RubError> {
    crate::js::call_function_returning_object_id(page, object_id, function_declaration).await
}

async fn candidate_match_rank(
    page: &Arc<Page>,
    expected: &Element,
    candidate: &CdpElement,
) -> Result<Option<CandidateMatchRank>, RubError> {
    let description = candidate
        .description()
        .await
        .map_err(|e| RubError::Internal(format!("Describe fallback element failed: {e}")))?;
    let attrs = attrs_to_map(description.attributes.as_deref().unwrap_or(&[]));
    let local_name = description.local_name.to_lowercase();
    if !tag_matches(
        expected.tag,
        &local_name,
        attrs.get("type").map(String::as_str),
    ) {
        return Ok(None);
    }

    for key in [
        "href",
        "placeholder",
        "aria-label",
        "type",
        "name",
        "value",
        "role",
        "title",
        "alt",
    ] {
        if let Some(expected_value) = expected.attributes.get(key)
            && attrs.get(key) != Some(expected_value)
        {
            return Ok(None);
        }
    }

    if !expected.text.trim().is_empty() {
        let candidate_text = candidate
            .inner_text()
            .await
            .map_err(|e| RubError::Internal(format!("Read fallback element text failed: {e}")))?
            .unwrap_or_default();

        if normalize_text(&candidate_text) != normalize_text(&expected.text) {
            return Ok(None);
        }
    }

    if let Some(expected_box) = expected.bounding_box {
        let candidate_box = candidate_bounding_box(page, &candidate.remote_object_id).await?;
        if let Some(score) = bounding_box_match_score(expected_box, candidate_box) {
            return Ok(Some(CandidateMatchRank::Scored(score)));
        }
        return Ok(None);
    }

    Ok(Some(CandidateMatchRank::Unscored))
}

fn snapshot_candidate_match_rank(
    expected: &Element,
    candidate: &Element,
) -> Option<CandidateMatchRank> {
    if !snapshot_tag_matches(expected.tag, candidate.tag) {
        return None;
    }

    for key in [
        "href",
        "placeholder",
        "aria-label",
        "type",
        "name",
        "value",
        "role",
        "title",
        "alt",
    ] {
        if let Some(expected_value) = expected.attributes.get(key)
            && candidate.attributes.get(key) != Some(expected_value)
        {
            return None;
        }
    }

    if !expected.text.trim().is_empty()
        && normalize_text(&candidate.text) != normalize_text(&expected.text)
    {
        return None;
    }

    if let Some(expected_box) = expected.bounding_box {
        let candidate_box = candidate.bounding_box?;
        let score = bounding_box_match_score(expected_box, candidate_box)?;
        return Some(CandidateMatchRank::Scored(score));
    }

    Some(CandidateMatchRank::Unscored)
}

async fn candidate_bounding_box(
    page: &Arc<Page>,
    object_id: &RemoteObjectId,
) -> Result<BoundingBox, RubError> {
    let value = crate::js::call_function_returning_value(
        page,
        object_id,
        r#"function() {
            const r = this.getBoundingClientRect();
            return {
                x: Number.isFinite(r.x) ? r.x : 0,
                y: Number.isFinite(r.y) ? r.y : 0,
                width: Number.isFinite(r.width) ? r.width : 0,
                height: Number.isFinite(r.height) ? r.height : 0
            };
        }"#,
    )
    .await?;
    serde_json::from_value(value)
        .map_err(|e| RubError::Internal(format!("Candidate bounding box parse failed: {e}")))
}

fn attrs_to_map(flat: &[String]) -> HashMap<String, String> {
    let mut attrs = HashMap::new();
    for pair in flat.chunks_exact(2) {
        attrs.insert(pair[0].clone(), pair[1].clone());
    }
    attrs
}

fn normalize_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn bounding_box_shape_matches(expected: BoundingBox, actual: BoundingBox) -> bool {
    let width_tolerance = shape_tolerance(expected.width);
    let height_tolerance = shape_tolerance(expected.height);

    (expected.width - actual.width).abs() <= width_tolerance
        && (expected.height - actual.height).abs() <= height_tolerance
}

fn bounding_box_match_score(expected: BoundingBox, actual: BoundingBox) -> Option<f64> {
    if !bounding_box_shape_matches(expected, actual) {
        return None;
    }

    Some(bounding_box_center_distance(expected, actual))
}

fn bounding_box_center_distance(expected: BoundingBox, actual: BoundingBox) -> f64 {
    let expected_center = bounding_box_center(expected);
    let actual_center = bounding_box_center(actual);
    let dx = expected_center.0 - actual_center.0;
    let dy = expected_center.1 - actual_center.1;
    (dx * dx + dy * dy).sqrt()
}

fn bounding_box_center(rect: BoundingBox) -> (f64, f64) {
    (rect.x + rect.width / 2.0, rect.y + rect.height / 2.0)
}

fn candidate_rank_precedes(candidate: CandidateMatchRank, best: CandidateMatchRank) -> bool {
    match (candidate, best) {
        (CandidateMatchRank::Scored(candidate), CandidateMatchRank::Scored(best)) => {
            candidate.total_cmp(&best).is_lt()
        }
        (CandidateMatchRank::Scored(_), CandidateMatchRank::Unscored) => true,
        (CandidateMatchRank::Unscored, CandidateMatchRank::Scored(_)) => false,
        (CandidateMatchRank::Unscored, CandidateMatchRank::Unscored) => false,
    }
}

fn shape_tolerance(expected_extent: f64) -> f64 {
    (expected_extent.abs() * 0.2).clamp(4.0, 24.0)
}

fn tag_matches(tag: ElementTag, local_name: &str, input_type: Option<&str>) -> bool {
    match tag {
        ElementTag::Button => local_name == "button",
        ElementTag::Link => local_name == "a",
        ElementTag::Input => {
            local_name == "input" && !matches!(input_type, Some("checkbox" | "radio"))
        }
        ElementTag::TextArea => local_name == "textarea",
        ElementTag::Select => local_name == "select",
        ElementTag::Checkbox => local_name == "input" && input_type == Some("checkbox"),
        ElementTag::Radio => local_name == "input" && input_type == Some("radio"),
        ElementTag::Option => local_name == "option",
        ElementTag::Other => true,
    }
}

fn snapshot_tag_matches(expected: ElementTag, candidate: ElementTag) -> bool {
    matches!(expected, ElementTag::Other) || expected == candidate
}

#[cfg(test)]
mod tests {
    use super::{
        CandidateMatchRank, bounding_box_center_distance, bounding_box_match_score,
        bounding_box_shape_matches, candidate_points, candidate_rank_precedes,
        parse_backend_node_id, parse_element_ref_frame_id, snapshot_candidate_match_rank,
        tag_matches, unverified_write_target_error,
    };
    use rub_core::error::ErrorCode;
    use rub_core::model::{BoundingBox, Element, ElementTag};
    use std::collections::HashMap;

    #[test]
    fn candidate_points_prioritize_center_and_insets() {
        let points = candidate_points(&[0.0, 0.0, 20.0, 0.0, 20.0, 10.0, 0.0, 10.0]);
        assert_eq!(points.len(), 5);
        assert_eq!(points[0].x, 10.0);
        assert_eq!(points[0].y, 5.0);
        assert!(points.iter().any(|p| p.x < 10.0 && p.y < 5.0));
        assert!(points.iter().any(|p| p.x > 10.0 && p.y > 5.0));
    }

    #[test]
    fn parse_backend_node_id_reads_backend_suffix() {
        let parsed = parse_backend_node_id(Some("dom:42")).expect("backend id");
        assert_eq!(*parsed.inner(), 42);
    }

    #[test]
    fn parse_element_ref_frame_id_reads_frame_prefix() {
        assert_eq!(
            parse_element_ref_frame_id(Some("frame-1:42")),
            Some("frame-1")
        );
        assert_eq!(parse_element_ref_frame_id(Some(":42")), None);
    }

    #[test]
    fn tag_matches_distinguishes_checkbox_and_radio_inputs() {
        assert!(tag_matches(ElementTag::Checkbox, "input", Some("checkbox")));
        assert!(tag_matches(ElementTag::Radio, "input", Some("radio")));
        assert!(tag_matches(ElementTag::Input, "input", Some("text")));
        assert!(!tag_matches(ElementTag::Input, "input", Some("checkbox")));
        assert!(!tag_matches(ElementTag::Checkbox, "input", Some("radio")));
    }

    #[test]
    fn bounding_box_shape_matches_allows_small_drift() {
        let expected = BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 120.0,
            height: 40.0,
        };
        let actual = BoundingBox {
            x: 100.0,
            y: 200.0,
            width: 126.0,
            height: 44.0,
        };
        assert!(bounding_box_shape_matches(expected, actual));
    }

    #[test]
    fn bounding_box_shape_matches_rejects_shape_mismatch() {
        let expected = BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 120.0,
            height: 40.0,
        };
        let actual = BoundingBox {
            x: 12.0,
            y: 18.0,
            width: 220.0,
            height: 72.0,
        };
        assert!(!bounding_box_shape_matches(expected, actual));
    }

    #[test]
    fn bounding_box_match_score_prefers_nearest_center_for_same_shape() {
        let expected = BoundingBox {
            x: 40.0,
            y: 80.0,
            width: 120.0,
            height: 40.0,
        };
        let near = BoundingBox {
            x: 44.0,
            y: 84.0,
            width: 122.0,
            height: 42.0,
        };
        let far = BoundingBox {
            x: 240.0,
            y: 300.0,
            width: 118.0,
            height: 38.0,
        };

        let near_score = bounding_box_match_score(expected, near).expect("near score");
        let far_score = bounding_box_match_score(expected, far).expect("far score");

        assert!(near_score < far_score);
    }

    #[test]
    fn bounding_box_center_distance_reflects_geometric_drift() {
        let expected = BoundingBox {
            x: 10.0,
            y: 20.0,
            width: 100.0,
            height: 40.0,
        };
        let shifted = BoundingBox {
            x: 40.0,
            y: 60.0,
            width: 100.0,
            height: 40.0,
        };

        assert_eq!(bounding_box_center_distance(expected, expected), 0.0);
        assert!(bounding_box_center_distance(expected, shifted) > 0.0);
    }

    #[test]
    fn candidate_rank_precedes_prefers_scored_and_then_nearest_match() {
        assert!(candidate_rank_precedes(
            CandidateMatchRank::Scored(8.0),
            CandidateMatchRank::Unscored
        ));
        assert!(candidate_rank_precedes(
            CandidateMatchRank::Scored(4.0),
            CandidateMatchRank::Scored(12.0)
        ));
        assert!(!candidate_rank_precedes(
            CandidateMatchRank::Unscored,
            CandidateMatchRank::Scored(2.0)
        ));
        assert!(!candidate_rank_precedes(
            CandidateMatchRank::Unscored,
            CandidateMatchRank::Unscored
        ));
    }

    #[test]
    fn snapshot_candidate_match_rank_rejects_mismatched_attributes() {
        let expected = Element {
            index: 0,
            tag: ElementTag::Link,
            text: "Read more".to_string(),
            attributes: HashMap::from([("href".to_string(), "/detail".to_string())]),
            element_ref: Some("frame-1:10".to_string()),
            bounding_box: None,
            ax_info: None,
            listeners: None,
            depth: None,
        };
        let candidate = Element {
            attributes: HashMap::from([("href".to_string(), "/other".to_string())]),
            ..expected.clone()
        };
        assert!(snapshot_candidate_match_rank(&expected, &candidate).is_none());
    }

    #[test]
    fn unverified_write_target_error_is_reported_as_stale_snapshot() {
        let error = unverified_write_target_error(
            &Element {
                index: 7,
                tag: ElementTag::Button,
                text: "Save".to_string(),
                attributes: HashMap::new(),
                element_ref: None,
                bounding_box: None,
                ax_info: None,
                listeners: None,
                depth: None,
            },
            "snapshot element does not carry a verified backend node id",
        );

        let envelope = error.into_envelope();
        assert_eq!(envelope.code, ErrorCode::StaleSnapshot);
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx["reason"].as_str()),
            Some("unverified_write_target")
        );
        assert_eq!(
            envelope
                .context
                .as_ref()
                .and_then(|ctx| ctx["element_index"].as_u64()),
            Some(7)
        );
    }
}
