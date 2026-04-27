use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::dom::{
    BackendNodeId, DescribeNodeParams, GetDocumentParams, Node as CdpNode, ResolveNodeParams,
};
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use chromiumoxide::cdp::js_protocol::runtime::ExecutionContextId;
use rub_core::error::RubError;
use rub_core::model::{BoundingBox, SnapshotProjection};
use std::collections::HashMap;
use std::sync::Arc;

pub(crate) struct ProjectionResolution {
    pub refs: Vec<Option<String>>,
    pub projection: SnapshotProjection,
}

#[derive(Debug, Clone)]
pub(crate) struct ProjectionElementInput {
    pub index: u32,
    pub dom_index: u32,
    pub depth: u32,
    pub tag: String,
    pub text: String,
    pub attributes: HashMap<String, String>,
    pub bounding_box: Option<BoundingBox>,
}

#[derive(Debug, serde::Deserialize)]
struct LiveElementFingerprint {
    tag: String,
    text: String,
    attributes: HashMap<String, String>,
    bounding_box: Option<BoundingBox>,
    depth: u32,
}

pub(crate) async fn resolve_backend_refs_for_frame(
    page: &Arc<Page>,
    frame_id: &str,
    projected_elements: &[ProjectionElementInput],
    js_traversal_count: u32,
    js_traversal_root_backend_node_id: Option<i64>,
) -> Result<ProjectionResolution, RubError> {
    let frame_document = load_frame_document(page, frame_id).await?;
    let traversal_root = find_traversal_root(&frame_document)
        .ok_or_else(|| RubError::Internal("Could not determine DOM traversal root".to_string()))?;
    let mut backend_ids = Vec::new();
    collect_element_backend_ids(traversal_root, &mut backend_ids);

    let dom_indices = projected_elements
        .iter()
        .map(|element| element.dom_index)
        .collect::<Vec<_>>();
    let mut resolution = resolve_from_counts(
        frame_id,
        &dom_indices,
        js_traversal_count,
        &backend_ids,
        js_traversal_root_backend_node_id,
        *traversal_root.backend_node_id.inner(),
    );
    if !resolution.projection.verified {
        return Ok(resolution);
    }
    if let Some(warning) =
        verify_projected_element_fingerprints(page, frame_id, projected_elements, &resolution.refs)
            .await
    {
        resolution.projection.verified = false;
        resolution.projection.warning = Some(warning);
    }
    Ok(resolution)
}

pub(crate) async fn capture_js_traversal_root_backend_node_id(
    page: &Arc<Page>,
    execution_context_id: Option<ExecutionContextId>,
) -> Option<i64> {
    let traversal_root_object_id = crate::js::evaluate_returning_object_id_in_context(
        page,
        execution_context_id,
        "document.body || document.documentElement",
    )
    .await
    .ok()?;
    let described = page
        .execute(
            DescribeNodeParams::builder()
                .object_id(traversal_root_object_id)
                .depth(0)
                .build(),
        )
        .await
        .ok()?;
    Some(*described.node.backend_node_id.inner())
}

async fn load_frame_document(page: &Arc<Page>, frame_id: &str) -> Result<CdpNode, RubError> {
    let main_frame = page
        .mainframe()
        .await
        .map_err(|e| RubError::Internal(format!("Read main frame failed: {e}")))?
        .ok_or_else(|| RubError::Internal("Main frame is unavailable".to_string()))?;

    if frame_id == main_frame.as_ref() {
        let document = page
            .execute(GetDocumentParams::builder().depth(-1).pierce(true).build())
            .await
            .map_err(|e| RubError::Internal(format!("DOM.getDocument failed: {e}")))?;
        return Ok(document.root.clone());
    }

    let execution_context_id = page
        .frame_execution_context(FrameId::new(frame_id.to_string()))
        .await
        .map_err(|e| RubError::Internal(format!("Resolve frame execution context failed: {e}")))?
        .ok_or_else(|| {
            RubError::Internal(format!(
                "Frame '{frame_id}' does not have a live execution context"
            ))
        })?;
    let document_object_id = crate::js::evaluate_returning_object_id_in_context(
        page,
        Some(execution_context_id),
        "document",
    )
    .await?;
    let described = page
        .execute(
            DescribeNodeParams::builder()
                .object_id(document_object_id)
                .depth(-1)
                .pierce(true)
                .build(),
        )
        .await
        .map_err(|e| RubError::Internal(format!("DOM.describeNode failed: {e}")))?;
    if described.node.node_type == 9 {
        return Ok(described.node.clone());
    }
    match_frame_document(&described.node, main_frame.as_ref(), frame_id)
        .cloned()
        .ok_or_else(|| RubError::Internal(format!("Could not locate frame document {frame_id}")))
}

fn match_frame_document<'a>(
    root: &'a CdpNode,
    main_frame_id: &str,
    frame_id: &str,
) -> Option<&'a CdpNode> {
    if frame_id == main_frame_id {
        return Some(root);
    }

    find_frame_document(root, frame_id)
}

fn resolve_from_counts(
    frame_id: &str,
    dom_indices: &[u32],
    js_traversal_count: u32,
    backend_ids: &[i64],
    js_traversal_root_backend_node_id: Option<i64>,
    projected_traversal_root_backend_node_id: i64,
) -> ProjectionResolution {
    let backend_traversal_count = backend_ids.len() as u32;
    let in_range = dom_indices
        .iter()
        .all(|dom_index| (*dom_index as usize) < backend_ids.len());
    let same_document = js_traversal_root_backend_node_id
        .is_some_and(|backend_node_id| backend_node_id == projected_traversal_root_backend_node_id);

    let verified = same_document && js_traversal_count == backend_traversal_count && in_range;
    let warning = if verified {
        None
    } else if let Some(js_traversal_root_backend_node_id) = js_traversal_root_backend_node_id {
        if js_traversal_root_backend_node_id != projected_traversal_root_backend_node_id {
            Some(format!(
                "DOM projection mismatch: JS traversal root backend node {js_traversal_root_backend_node_id} but DOM traversal resolved backend node {projected_traversal_root_backend_node_id}"
            ))
        } else if js_traversal_count != backend_traversal_count {
            Some(format!(
                "DOM projection mismatch: JS traversed {js_traversal_count} elements but DOM.getDocument returned {backend_traversal_count}"
            ))
        } else {
            Some(
                "DOM projection mismatch: at least one dom_index could not be mapped to a backend node"
                    .to_string(),
            )
        }
    } else {
        Some(
            "DOM projection mismatch: could not capture JS traversal-root authority for backend projection"
                .to_string(),
        )
    };

    let refs = dom_indices
        .iter()
        .map(|dom_index| {
            backend_ids
                .get(*dom_index as usize)
                .map(|backend_id| format!("{frame_id}:{backend_id}"))
        })
        .collect::<Vec<_>>();

    let resolved_ref_count = refs.iter().filter(|value| value.is_some()).count() as u32;

    ProjectionResolution {
        refs,
        projection: SnapshotProjection {
            verified,
            js_traversal_count,
            backend_traversal_count,
            resolved_ref_count,
            warning,
        },
    }
}

async fn verify_projected_element_fingerprints(
    page: &Arc<Page>,
    frame_id: &str,
    projected_elements: &[ProjectionElementInput],
    refs: &[Option<String>],
) -> Option<String> {
    for (position, projected) in projected_elements.iter().enumerate() {
        let Some(element_ref) = refs.get(position).and_then(|value| value.as_ref()) else {
            continue;
        };
        let Some(backend_node_id) =
            crate::targeting::parse_backend_node_id(Some(element_ref.as_str()))
        else {
            return Some(format!(
                "DOM projection mismatch: could not parse backend node id for snapshot element {}",
                projected.index
            ));
        };
        let live_fingerprint = match capture_live_element_fingerprint(
            page,
            frame_id,
            backend_node_id,
        )
        .await
        {
            Ok(fingerprint) => fingerprint,
            Err(error) => {
                return Some(format!(
                    "DOM projection mismatch: live fingerprint probe failed for snapshot element {} (dom_index={}): {error}",
                    projected.index, projected.dom_index
                ));
            }
        };
        if let Some(reason) = live_projection_fingerprint_mismatch(projected, &live_fingerprint) {
            return Some(format!(
                "DOM projection mismatch: snapshot element {} (dom_index={}) backend ref diverged after extraction: {reason}",
                projected.index, projected.dom_index
            ));
        }
    }
    None
}

async fn capture_live_element_fingerprint(
    page: &Arc<Page>,
    frame_id: &str,
    backend_node_id: BackendNodeId,
) -> Result<LiveElementFingerprint, RubError> {
    let frame_context = crate::frame_runtime::resolve_frame_context(page, Some(frame_id)).await?;
    let mut params = ResolveNodeParams::builder().backend_node_id(backend_node_id);
    if let Some(execution_context_id) = frame_context.execution_context_id {
        params = params.execution_context_id(execution_context_id);
    }
    let response = page
        .execute(params.build())
        .await
        .map_err(|e| RubError::Internal(format!("ResolveNode failed: {e}")))?;
    let object_id = response.result.object.object_id.ok_or_else(|| {
        RubError::Internal("Resolved node has no objectId for projection verification".to_string())
    })?;
    let fingerprint_json = crate::js::call_function_returning_string(
        page,
        &object_id,
        crate::dom::live_element_projection_fingerprint_function(),
    )
    .await?;
    serde_json::from_str(&fingerprint_json)
        .map_err(|e| RubError::Internal(format!("Live projection fingerprint parse failed: {e}")))
}

fn live_projection_fingerprint_mismatch(
    projected: &ProjectionElementInput,
    live: &LiveElementFingerprint,
) -> Option<&'static str> {
    if projected.tag != live.tag {
        return Some("tag");
    }
    if projected.depth != live.depth {
        return Some("depth");
    }
    if projected.attributes != live.attributes {
        return Some("attributes");
    }
    if projected.text != live.text {
        return Some("text");
    }
    match (projected.bounding_box, live.bounding_box) {
        (Some(expected), Some(actual)) if !bounding_box_matches(expected, actual) => {
            return Some("bounding_box");
        }
        (Some(_), None) | (None, Some(_)) => {
            return Some("bounding_box");
        }
        _ => {}
    }
    None
}

fn bounding_box_matches(expected: BoundingBox, actual: BoundingBox) -> bool {
    const TOLERANCE: f64 = 1.0;
    (expected.x - actual.x).abs() <= TOLERANCE
        && (expected.y - actual.y).abs() <= TOLERANCE
        && (expected.width - actual.width).abs() <= TOLERANCE
        && (expected.height - actual.height).abs() <= TOLERANCE
}

fn find_frame_document<'a>(node: &'a CdpNode, frame_id: &str) -> Option<&'a CdpNode> {
    if node.node_type == 9
        && node
            .frame_id
            .as_ref()
            .is_some_and(|candidate| candidate.as_ref() == frame_id)
    {
        return Some(node);
    }

    if let Some(content_document) = node.content_document.as_ref()
        && let Some(found) = find_frame_document(content_document, frame_id)
    {
        return Some(found);
    }

    if let Some(template_content) = node.template_content.as_ref()
        && let Some(found) = find_frame_document(template_content, frame_id)
    {
        return Some(found);
    }

    if let Some(shadow_roots) = node.shadow_roots.as_ref() {
        for shadow_root in shadow_roots {
            if let Some(found) = find_frame_document(shadow_root, frame_id) {
                return Some(found);
            }
        }
    }

    if let Some(children) = node.children.as_ref() {
        for child in children {
            if let Some(found) = find_frame_document(child, frame_id) {
                return Some(found);
            }
        }
    }

    None
}

fn find_traversal_root(node: &CdpNode) -> Option<&CdpNode> {
    if node.node_type == 1 && node.local_name.eq_ignore_ascii_case("body") {
        return Some(node);
    }

    if let Some(children) = node.children.as_ref() {
        for child in children {
            if child.node_type == 1 && child.local_name.eq_ignore_ascii_case("body") {
                return Some(child);
            }
            if let Some(found) = find_body_descendant(child) {
                return Some(found);
            }
        }
        if node.node_type == 9 {
            return children.iter().find(|child| child.node_type == 1);
        }
    }

    if node.node_type == 1 {
        Some(node)
    } else {
        None
    }
}

fn find_body_descendant(node: &CdpNode) -> Option<&CdpNode> {
    if node.node_type == 1 && node.local_name.eq_ignore_ascii_case("body") {
        return Some(node);
    }
    node.children
        .as_ref()
        .and_then(|children| children.iter().find_map(find_body_descendant))
}

fn collect_element_backend_ids(node: &CdpNode, output: &mut Vec<i64>) {
    if node.node_type == 1 {
        output.push(*node.backend_node_id.inner());
    }

    if let Some(children) = node.children.as_ref() {
        for child in children {
            collect_element_backend_ids(child, output);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        ProjectionElementInput, bounding_box_matches, collect_element_backend_ids,
        find_frame_document, live_projection_fingerprint_mismatch, match_frame_document,
        resolve_from_counts,
    };
    use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, Node as CdpNode, NodeId};
    use chromiumoxide::cdp::browser_protocol::page::FrameId;
    use rub_core::model::BoundingBox;
    use std::collections::HashMap;

    #[test]
    fn projection_resolution_verifies_when_counts_and_indices_align() {
        let resolution =
            resolve_from_counts("frame-1", &[0, 2], 3, &[101, 102, 103], Some(500), 500);
        assert!(resolution.projection.verified);
        assert_eq!(resolution.projection.resolved_ref_count, 2);
        assert_eq!(resolution.refs[0].as_deref(), Some("frame-1:101"));
        assert_eq!(resolution.refs[1].as_deref(), Some("frame-1:103"));
    }

    #[test]
    fn projection_resolution_drops_refs_when_counts_mismatch() {
        let resolution =
            resolve_from_counts("frame-1", &[0, 1], 4, &[101, 102, 103], Some(500), 500);
        assert!(!resolution.projection.verified);
        assert_eq!(resolution.projection.resolved_ref_count, 2);
        assert_eq!(resolution.refs[0].as_deref(), Some("frame-1:101"));
        assert_eq!(resolution.refs[1].as_deref(), Some("frame-1:102"));
        assert!(
            resolution
                .projection
                .warning
                .as_deref()
                .unwrap_or_default()
                .contains("JS traversed 4 elements")
        );
    }

    #[test]
    fn projection_resolution_preserves_in_range_refs_when_some_indices_are_missing() {
        let resolution =
            resolve_from_counts("frame-1", &[0, 3], 3, &[101, 102, 103], Some(500), 500);
        assert!(!resolution.projection.verified);
        assert_eq!(resolution.projection.resolved_ref_count, 1);
        assert_eq!(resolution.refs[0].as_deref(), Some("frame-1:101"));
        assert_eq!(resolution.refs[1], None);
        assert!(
            resolution
                .projection
                .warning
                .as_deref()
                .unwrap_or_default()
                .contains("dom_index")
        );
    }

    #[test]
    fn projection_resolution_drops_verification_when_document_fence_mismatches() {
        let resolution = resolve_from_counts("frame-1", &[0, 1], 2, &[101, 102], Some(500), 900);
        assert!(!resolution.projection.verified);
        assert_eq!(resolution.projection.resolved_ref_count, 2);
        assert_eq!(resolution.refs[0].as_deref(), Some("frame-1:101"));
        assert_eq!(resolution.refs[1].as_deref(), Some("frame-1:102"));
        assert!(
            resolution
                .projection
                .warning
                .as_deref()
                .unwrap_or_default()
                .contains("traversal root backend node")
        );
    }

    #[test]
    fn projection_resolution_drops_verification_when_js_document_fence_is_missing() {
        let resolution = resolve_from_counts("frame-1", &[0, 1], 2, &[101, 102], None, 900);
        assert!(!resolution.projection.verified);
        assert_eq!(resolution.projection.resolved_ref_count, 2);
        assert_eq!(resolution.refs[0].as_deref(), Some("frame-1:101"));
        assert_eq!(resolution.refs[1].as_deref(), Some("frame-1:102"));
        assert_eq!(
            resolution.projection.warning.as_deref(),
            Some(
                "DOM projection mismatch: could not capture JS traversal-root authority for backend projection"
            )
        );
    }

    #[test]
    fn live_projection_fingerprint_accepts_exact_match() {
        let projected = ProjectionElementInput {
            index: 3,
            dom_index: 5,
            depth: 2,
            tag: "button".to_string(),
            text: "Save".to_string(),
            attributes: HashMap::from([
                ("type".to_string(), "button".to_string()),
                ("aria-label".to_string(), "Save".to_string()),
            ]),
            bounding_box: Some(BoundingBox {
                x: 10.0,
                y: 20.0,
                width: 40.0,
                height: 18.0,
            }),
        };
        let live = super::LiveElementFingerprint {
            tag: "button".to_string(),
            text: "Save".to_string(),
            attributes: projected.attributes.clone(),
            bounding_box: projected.bounding_box,
            depth: 2,
        };

        assert_eq!(
            live_projection_fingerprint_mismatch(&projected, &live),
            None
        );
    }

    #[test]
    fn live_projection_fingerprint_rejects_backend_alias_with_same_counts() {
        let projected = ProjectionElementInput {
            index: 1,
            dom_index: 1,
            depth: 0,
            tag: "link".to_string(),
            text: "Open".to_string(),
            attributes: HashMap::from([("href".to_string(), "/a".to_string())]),
            bounding_box: Some(BoundingBox {
                x: 10.0,
                y: 10.0,
                width: 80.0,
                height: 20.0,
            }),
        };
        let live = super::LiveElementFingerprint {
            tag: "link".to_string(),
            text: "Open".to_string(),
            attributes: HashMap::from([("href".to_string(), "/b".to_string())]),
            bounding_box: Some(BoundingBox {
                x: 220.0,
                y: 10.0,
                width: 80.0,
                height: 20.0,
            }),
            depth: 0,
        };

        assert_eq!(
            live_projection_fingerprint_mismatch(&projected, &live),
            Some("attributes")
        );
    }

    #[test]
    fn bounding_box_match_tolerates_subpixel_drift_only() {
        assert!(bounding_box_matches(
            BoundingBox {
                x: 10.0,
                y: 20.0,
                width: 30.0,
                height: 40.0,
            },
            BoundingBox {
                x: 10.5,
                y: 20.5,
                width: 30.5,
                height: 40.5,
            }
        ));
        assert!(!bounding_box_matches(
            BoundingBox {
                x: 10.0,
                y: 20.0,
                width: 30.0,
                height: 40.0,
            },
            BoundingBox {
                x: 20.5,
                y: 20.5,
                width: 30.5,
                height: 40.5,
            }
        ));
    }

    #[test]
    fn collect_element_backend_ids_walks_preorder_elements_only() {
        let tree = CdpNode {
            node_id: NodeId::new(1),
            parent_id: None,
            backend_node_id: BackendNodeId::new(100),
            node_type: 1,
            node_name: "BODY".to_string(),
            local_name: "body".to_string(),
            node_value: String::new(),
            child_node_count: Some(2),
            children: Some(vec![
                CdpNode {
                    node_id: NodeId::new(2),
                    parent_id: Some(NodeId::new(1)),
                    backend_node_id: BackendNodeId::new(101),
                    node_type: 1,
                    node_name: "DIV".to_string(),
                    local_name: "div".to_string(),
                    node_value: String::new(),
                    child_node_count: Some(0),
                    children: Some(vec![]),
                    attributes: None,
                    document_url: None,
                    base_url: None,
                    public_id: None,
                    system_id: None,
                    internal_subset: None,
                    xml_version: None,
                    name: None,
                    value: None,
                    pseudo_type: None,
                    pseudo_identifier: None,
                    shadow_root_type: None,
                    frame_id: None,
                    content_document: None,
                    shadow_roots: None,
                    template_content: None,
                    pseudo_elements: None,
                    distributed_nodes: None,
                    is_svg: None,
                    compatibility_mode: None,
                    assigned_slot: None,
                    is_scrollable: None,
                    affected_by_starting_styles: None,
                    adopted_style_sheets: None,
                },
                CdpNode {
                    node_id: NodeId::new(3),
                    parent_id: Some(NodeId::new(1)),
                    backend_node_id: BackendNodeId::new(102),
                    node_type: 3,
                    node_name: "#text".to_string(),
                    local_name: String::new(),
                    node_value: "hello".to_string(),
                    child_node_count: Some(0),
                    children: Some(vec![]),
                    attributes: None,
                    document_url: None,
                    base_url: None,
                    public_id: None,
                    system_id: None,
                    internal_subset: None,
                    xml_version: None,
                    name: None,
                    value: None,
                    pseudo_type: None,
                    pseudo_identifier: None,
                    shadow_root_type: None,
                    frame_id: None,
                    content_document: None,
                    shadow_roots: None,
                    template_content: None,
                    pseudo_elements: None,
                    distributed_nodes: None,
                    is_svg: None,
                    compatibility_mode: None,
                    assigned_slot: None,
                    is_scrollable: None,
                    affected_by_starting_styles: None,
                    adopted_style_sheets: None,
                },
            ]),
            attributes: None,
            document_url: None,
            base_url: None,
            public_id: None,
            system_id: None,
            internal_subset: None,
            xml_version: None,
            name: None,
            value: None,
            pseudo_type: None,
            pseudo_identifier: None,
            shadow_root_type: None,
            frame_id: None,
            content_document: None,
            shadow_roots: None,
            template_content: None,
            pseudo_elements: None,
            distributed_nodes: None,
            is_svg: None,
            compatibility_mode: None,
            assigned_slot: None,
            is_scrollable: None,
            affected_by_starting_styles: None,
            adopted_style_sheets: None,
        };

        let mut ids = Vec::new();
        collect_element_backend_ids(&tree, &mut ids);
        assert_eq!(ids, vec![100, 101]);
    }

    #[test]
    fn find_frame_document_descends_into_content_documents() {
        let child_document = CdpNode {
            node_id: NodeId::new(3),
            parent_id: Some(NodeId::new(2)),
            backend_node_id: BackendNodeId::new(300),
            node_type: 9,
            node_name: "#document".to_string(),
            local_name: String::new(),
            node_value: String::new(),
            child_node_count: Some(0),
            children: Some(vec![]),
            attributes: None,
            document_url: None,
            base_url: None,
            public_id: None,
            system_id: None,
            internal_subset: None,
            xml_version: None,
            name: None,
            value: None,
            pseudo_type: None,
            pseudo_identifier: None,
            shadow_root_type: None,
            frame_id: Some(FrameId::new("child")),
            content_document: None,
            shadow_roots: None,
            template_content: None,
            pseudo_elements: None,
            distributed_nodes: None,
            is_svg: None,
            compatibility_mode: None,
            assigned_slot: None,
            is_scrollable: None,
            affected_by_starting_styles: None,
            adopted_style_sheets: None,
        };

        let tree = CdpNode {
            node_id: NodeId::new(1),
            parent_id: None,
            backend_node_id: BackendNodeId::new(100),
            node_type: 9,
            node_name: "#document".to_string(),
            local_name: String::new(),
            node_value: String::new(),
            child_node_count: Some(1),
            children: Some(vec![CdpNode {
                node_id: NodeId::new(2),
                parent_id: Some(NodeId::new(1)),
                backend_node_id: BackendNodeId::new(200),
                node_type: 1,
                node_name: "IFRAME".to_string(),
                local_name: "iframe".to_string(),
                node_value: String::new(),
                child_node_count: Some(0),
                children: Some(vec![]),
                attributes: None,
                document_url: None,
                base_url: None,
                public_id: None,
                system_id: None,
                internal_subset: None,
                xml_version: None,
                name: None,
                value: None,
                pseudo_type: None,
                pseudo_identifier: None,
                shadow_root_type: None,
                frame_id: None,
                content_document: Some(Box::new(child_document)),
                shadow_roots: None,
                template_content: None,
                pseudo_elements: None,
                distributed_nodes: None,
                is_svg: None,
                compatibility_mode: None,
                assigned_slot: None,
                is_scrollable: None,
                affected_by_starting_styles: None,
                adopted_style_sheets: None,
            }]),
            attributes: None,
            document_url: None,
            base_url: None,
            public_id: None,
            system_id: None,
            internal_subset: None,
            xml_version: None,
            name: None,
            value: None,
            pseudo_type: None,
            pseudo_identifier: None,
            shadow_root_type: None,
            frame_id: Some(FrameId::new("main")),
            content_document: None,
            shadow_roots: None,
            template_content: None,
            pseudo_elements: None,
            distributed_nodes: None,
            is_svg: None,
            compatibility_mode: None,
            assigned_slot: None,
            is_scrollable: None,
            affected_by_starting_styles: None,
            adopted_style_sheets: None,
        };

        let found = find_frame_document(&tree, "child").expect("child frame document");
        assert_eq!(found.node_id, NodeId::new(3));
    }

    #[test]
    fn match_frame_document_uses_root_for_primary_frame_even_without_root_frame_id() {
        let root = CdpNode {
            node_id: NodeId::new(1),
            parent_id: None,
            backend_node_id: BackendNodeId::new(100),
            node_type: 9,
            node_name: "#document".to_string(),
            local_name: String::new(),
            node_value: String::new(),
            child_node_count: Some(0),
            children: Some(vec![]),
            attributes: None,
            document_url: None,
            base_url: None,
            public_id: None,
            system_id: None,
            internal_subset: None,
            xml_version: None,
            name: None,
            value: None,
            pseudo_type: None,
            pseudo_identifier: None,
            shadow_root_type: None,
            frame_id: None,
            content_document: None,
            shadow_roots: None,
            template_content: None,
            pseudo_elements: None,
            distributed_nodes: None,
            is_svg: None,
            compatibility_mode: None,
            assigned_slot: None,
            is_scrollable: None,
            affected_by_starting_styles: None,
            adopted_style_sheets: None,
        };

        let found = match_frame_document(&root, "main-frame", "main-frame")
            .expect("primary frame should resolve to root document");
        assert_eq!(found.node_id, NodeId::new(1));
    }
}
