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

pub(crate) const TOP_LEVEL_BOUNDING_BOX_FUNCTION: &str = r#"function() {
    const rect = this.getBoundingClientRect();
    let x = Number.isFinite(rect.x) ? rect.x : 0;
    let y = Number.isFinite(rect.y) ? rect.y : 0;
    let current = window;
    while (current !== current.top) {
        try {
            const frameEl = current.frameElement;
            if (!frameEl) break;
            const frameRect = frameEl.getBoundingClientRect();
            x += Number.isFinite(frameRect.x) ? frameRect.x : 0;
            y += Number.isFinite(frameRect.y) ? frameRect.y : 0;
            current = current.parent;
        } catch (_) {
            break;
        }
    }
    return {
        x,
        y,
        width: Number.isFinite(rect.width) ? rect.width : 0,
        height: Number.isFinite(rect.height) ? rect.height : 0
    };
}"#;

pub(crate) const TOP_LEVEL_HIT_TEST_HELPERS: &str = r#"
const topLevelBoundingBox = (el) => {
    const rect = el.getBoundingClientRect();
    let x = Number.isFinite(rect.x) ? rect.x : 0;
    let y = Number.isFinite(rect.y) ? rect.y : 0;
    let current = window;
    while (current !== current.top) {
        try {
            const frameEl = current.frameElement;
            if (!frameEl) break;
            const frameRect = frameEl.getBoundingClientRect();
            x += Number.isFinite(frameRect.x) ? frameRect.x : 0;
            y += Number.isFinite(frameRect.y) ? frameRect.y : 0;
            current = current.parent;
        } catch (_) {
            break;
        }
    }
    return {
        x,
        y,
        width: Number.isFinite(rect.width) ? rect.width : 0,
        height: Number.isFinite(rect.height) ? rect.height : 0
    };
};
const topLevelHitPointMatches = (el, x, y) => {
    try {
        let currentX = x;
        let currentY = y;
        let hit = window.top.document.elementFromPoint(currentX, currentY);
        while (hit) {
            if (
                hit === el
                || (typeof el.contains === 'function' && el.contains(hit))
                || (typeof hit.contains === 'function' && hit.contains(el))
            ) {
                return true;
            }
            if (!hit.contentDocument) {
                return false;
            }
            try {
                const frameRect = hit.getBoundingClientRect();
                currentX -= frameRect.left;
                currentY -= frameRect.top;
                hit = hit.contentDocument.elementFromPoint(currentX, currentY);
            } catch (_) {
                return false;
            }
        }
        return false;
    } catch (_) {
        return false;
    }
};
const topLevelHitMatches = (el) => {
    const rect = topLevelBoundingBox(el);
    if (!(rect.width > 0 && rect.height > 0)) return false;
    const insetX = Math.min(Math.max(rect.width * 0.2, 1), 8);
    const insetY = Math.min(Math.max(rect.height * 0.2, 1), 8);
    const points = [
        { x: rect.x + rect.width / 2, y: rect.y + rect.height / 2 },
        { x: rect.x + insetX, y: rect.y + insetY },
        { x: rect.x + rect.width - insetX, y: rect.y + insetY },
        { x: rect.x + insetX, y: rect.y + rect.height - insetY },
        { x: rect.x + rect.width - insetX, y: rect.y + rect.height - insetY },
    ];
    return points.some((point) => topLevelHitPointMatches(el, point.x, point.y));
};
"#;

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

    if !allow_global_read_fallback(frame_id) {
        return Err(frame_scoped_read_target_error(element));
    }

    let candidates = page
        .find_elements(fallback_selector)
        .await
        .map_err(|e| RubError::Internal(format!("{query_failure_label}: {e}")))?;

    let mut matching_candidates = Vec::new();

    for candidate in candidates {
        if let Some(rank) = candidate_match_rank(page, element, &candidate).await? {
            let resolved = ResolvedElement {
                remote_object_id: candidate.remote_object_id.clone(),
                backend_node_id: Some(candidate.backend_node_id),
                verified: false,
            };
            matching_candidates.push((rank, resolved));
        }
    }

    if matching_candidates.len() > 1 {
        return Err(ambiguous_read_target_error(
            element,
            "global_read_fallback_ambiguous",
            matching_candidates.len(),
        ));
    }

    if let Some((_, resolved)) = matching_candidates.into_iter().next() {
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

fn frame_scoped_read_target_error(element: &Element) -> RubError {
    RubError::domain_with_context(
        ErrorCode::StaleSnapshot,
        "Frame-scoped reads require a target that can still be resolved inside the selected frame authority",
        serde_json::json!({
            "authority_state": "frame_scoped_read_target_stale",
            "element_index": element.index,
        }),
    )
}

fn ambiguous_read_target_error(
    element: &Element,
    authority_state: &str,
    candidate_count: usize,
) -> RubError {
    RubError::domain_with_context(
        ErrorCode::StaleSnapshot,
        "Read fallback found multiple live candidates and cannot choose a non-authoritative target",
        serde_json::json!({
            "authority_state": authority_state,
            "element_index": element.index,
            "candidate_count": candidate_count,
            "element_ref": element.element_ref,
        }),
    )
}

fn allow_global_read_fallback(frame_id: Option<&str>) -> bool {
    frame_id.is_none()
}

async fn resolve_element_within_frame_snapshot(
    page: &Arc<Page>,
    expected: &Element,
    frame_id: Option<&str>,
) -> Result<Option<ResolvedElement>, RubError> {
    let snapshot = crate::dom::build_snapshot_for_frame(page, 0, Some(0), frame_id).await?;
    let mut matching_candidates = Vec::new();

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
        matching_candidates.push((rank, resolved));
    }

    if matching_candidates.len() > 1 {
        return Err(ambiguous_read_target_error(
            expected,
            "frame_scoped_read_fallback_ambiguous",
            matching_candidates.len(),
        ));
    }

    Ok(matching_candidates
        .into_iter()
        .next()
        .map(|(_, resolved)| resolved))
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

pub(crate) async fn filter_snapshot_elements_by_hit_test(
    page: &Arc<Page>,
    _snapshot: &rub_core::model::Snapshot,
    elements: &[Element],
) -> Result<Vec<Element>, RubError> {
    let mut filtered = Vec::with_capacity(elements.len());
    for element in elements {
        let Ok(resolved) = resolve_element(page, element).await else {
            continue;
        };
        if resolve_pointer_point(page, &resolved).await.is_ok() {
            filtered.push(element.clone());
        }
    }
    Ok(filtered)
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
                {helpers}
                const x = {:.3};
                const y = {:.3};
                return topLevelHitPointMatches(this, x, y);
            }}",
            point.x,
            point.y,
            helpers = TOP_LEVEL_HIT_TEST_HELPERS
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
    let value =
        crate::js::call_function_returning_value(page, object_id, TOP_LEVEL_BOUNDING_BOX_FUNCTION)
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
mod tests;
