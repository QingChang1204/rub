use chromiumoxide::Page;
use chromiumoxide::cdp::browser_protocol::dom::{
    DescribeNodeParams, GetDocumentParams, Node as CdpNode,
};
use chromiumoxide::cdp::browser_protocol::page::FrameId;
use rub_core::error::RubError;
use rub_core::model::SnapshotProjection;
use std::sync::Arc;

pub(crate) struct ProjectionResolution {
    pub refs: Vec<Option<String>>,
    pub projection: SnapshotProjection,
}

pub(crate) async fn resolve_backend_refs_for_frame(
    page: &Arc<Page>,
    frame_id: &str,
    dom_indices: &[u32],
    js_traversal_count: u32,
) -> Result<ProjectionResolution, RubError> {
    let frame_document = load_frame_document(page, frame_id).await?;
    let traversal_root = find_traversal_root(&frame_document)
        .ok_or_else(|| RubError::Internal("Could not determine DOM traversal root".to_string()))?;
    let mut backend_ids = Vec::new();
    collect_element_backend_ids(traversal_root, &mut backend_ids);

    Ok(resolve_from_counts(
        frame_id,
        dom_indices,
        js_traversal_count,
        &backend_ids,
    ))
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
) -> ProjectionResolution {
    let backend_traversal_count = backend_ids.len() as u32;
    let in_range = dom_indices
        .iter()
        .all(|dom_index| (*dom_index as usize) < backend_ids.len());

    let verified = js_traversal_count == backend_traversal_count && in_range;
    let warning = if verified {
        None
    } else if js_traversal_count != backend_traversal_count {
        Some(format!(
            "DOM projection mismatch: JS traversed {js_traversal_count} elements but DOM.getDocument returned {backend_traversal_count}"
        ))
    } else {
        Some(
            "DOM projection mismatch: at least one dom_index could not be mapped to a backend node"
                .to_string(),
        )
    };

    let refs = if verified {
        dom_indices
            .iter()
            .map(|dom_index| {
                backend_ids
                    .get(*dom_index as usize)
                    .map(|backend_id| format!("{frame_id}:{backend_id}"))
            })
            .collect()
    } else {
        vec![None; dom_indices.len()]
    };

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
        collect_element_backend_ids, find_frame_document, match_frame_document, resolve_from_counts,
    };
    use chromiumoxide::cdp::browser_protocol::dom::{BackendNodeId, Node as CdpNode, NodeId};
    use chromiumoxide::cdp::browser_protocol::page::FrameId;

    #[test]
    fn projection_resolution_verifies_when_counts_and_indices_align() {
        let resolution = resolve_from_counts("frame-1", &[0, 2], 3, &[101, 102, 103]);
        assert!(resolution.projection.verified);
        assert_eq!(resolution.projection.resolved_ref_count, 2);
        assert_eq!(resolution.refs[0].as_deref(), Some("frame-1:101"));
        assert_eq!(resolution.refs[1].as_deref(), Some("frame-1:103"));
    }

    #[test]
    fn projection_resolution_drops_refs_when_counts_mismatch() {
        let resolution = resolve_from_counts("frame-1", &[0, 1], 4, &[101, 102, 103]);
        assert!(!resolution.projection.verified);
        assert_eq!(resolution.projection.resolved_ref_count, 0);
        assert!(resolution.refs.iter().all(|value| value.is_none()));
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
