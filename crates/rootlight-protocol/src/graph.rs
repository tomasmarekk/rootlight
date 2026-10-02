//! Validation and integrity sealing for compact source-free graph pages.
//!
//! The same closed bounds are applied before daemon transport and after client
//! decoding so malformed length, ordinal, and dictionary combinations fail closed.

use std::fmt;

use prost::Message as _;

use crate::generated::ui::graph::v1 as graph;

/// Current compact graph page schema version.
pub const UI_GRAPH_SCHEMA_VERSION: u32 = 1;
/// Hard maximum nodes carried by one graph page.
pub const MAX_GRAPH_PAGE_NODES: usize = 200;
/// Hard maximum edges carried by one graph page.
pub const MAX_GRAPH_PAGE_EDGES: usize = 500;
/// Hard maximum nodes retained by one bounded graph projection.
pub const MAX_GRAPH_AGGREGATE_NODES: u32 = 512;
/// Hard maximum edges retained by one bounded graph projection.
pub const MAX_GRAPH_AGGREGATE_EDGES: u32 = 2_048;
/// Hard maximum entries in one page-local string dictionary.
pub const MAX_GRAPH_DICTIONARY_ENTRIES: usize = 512;
/// Hard maximum UTF-8 bytes in one graph dictionary string.
pub const MAX_GRAPH_STRING_BYTES: usize = 1_024;
/// Hard maximum aggregate UTF-8 bytes in one graph dictionary.
pub const MAX_GRAPH_DICTIONARY_BYTES: usize = 256 * 1_024;
/// Hard maximum encoded bytes in one compact graph page.
pub const MAX_GRAPH_PAGE_BYTES: usize = 1024 * 1_024;

const GRAPH_PAGE_CHECKSUM_BYTES: usize = 32;

/// Compact graph page validation failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphPageError {
    /// The page selected an unsupported schema revision.
    Schema,
    /// A page collection or string exceeded a closed bound.
    Bounds,
    /// A dictionary index did not address a valid entry.
    Dictionary,
    /// A node ordinal or edge endpoint violated cumulative ordering.
    Ordinals,
    /// A closed enum or fixed-point value was invalid.
    Value,
    /// The page integrity checksum was missing or did not match.
    Checksum,
}

impl fmt::Display for GraphPageError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Schema => "graph page schema is invalid",
            Self::Bounds => "graph page bounds are invalid",
            Self::Dictionary => "graph page dictionary is invalid",
            Self::Ordinals => "graph page ordinals are invalid",
            Self::Value => "graph page value is invalid",
            Self::Checksum => "graph page checksum is invalid",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for GraphPageError {}

/// Validates and seals one graph page with a deterministic BLAKE3 checksum.
///
/// # Errors
///
/// Returns [`GraphPageError`] when any collection, dictionary, ordinal, enum,
/// count, or encoded-size bound is invalid.
pub fn seal_graph_page(page: &mut graph::GraphPage) -> Result<(), GraphPageError> {
    page.checksum.clear();
    validate_graph_page_body(page)?;
    let encoded = page.encode_to_vec();
    if encoded.len() > MAX_GRAPH_PAGE_BYTES {
        return Err(GraphPageError::Bounds);
    }
    page.checksum = blake3::hash(&encoded).as_bytes().to_vec();
    if page.encoded_len() > MAX_GRAPH_PAGE_BYTES {
        return Err(GraphPageError::Bounds);
    }
    Ok(())
}

/// Validates one sealed graph page and verifies its deterministic checksum.
///
/// # Errors
///
/// Returns [`GraphPageError`] when the page violates a closed bound or its
/// checksum does not match its canonical encoded content.
pub fn validate_graph_page(page: &graph::GraphPage) -> Result<(), GraphPageError> {
    if page.checksum.len() != GRAPH_PAGE_CHECKSUM_BYTES {
        return Err(GraphPageError::Checksum);
    }
    validate_graph_page_body(page)?;
    let mut canonical = page.clone();
    canonical.checksum.clear();
    let encoded = canonical.encode_to_vec();
    if encoded.len() > MAX_GRAPH_PAGE_BYTES {
        return Err(GraphPageError::Bounds);
    }
    let expected = blake3::hash(&encoded);
    let difference = page
        .checksum
        .iter()
        .zip(expected.as_bytes())
        .fold(0_u8, |difference, (left, right)| {
            difference | (*left ^ *right)
        });
    if difference != 0 {
        return Err(GraphPageError::Checksum);
    }
    Ok(())
}

fn validate_graph_page_body(page: &graph::GraphPage) -> Result<(), GraphPageError> {
    if page.schema_version != UI_GRAPH_SCHEMA_VERSION
        || page.nodes.len() > MAX_GRAPH_PAGE_NODES
        || page.edges.len() > MAX_GRAPH_PAGE_EDGES
        || page.strings.is_empty()
        || page.strings.len() > MAX_GRAPH_DICTIONARY_ENTRIES
    {
        return Err(GraphPageError::Bounds);
    }
    let Some(empty) = page.strings.first() else {
        return Err(GraphPageError::Dictionary);
    };
    if !empty.is_empty() {
        return Err(GraphPageError::Dictionary);
    }
    let dictionary_bytes = page.strings.iter().try_fold(0_usize, |total, value| {
        if value.len() > MAX_GRAPH_STRING_BYTES || value.as_bytes().contains(&0) {
            return Err(GraphPageError::Bounds);
        }
        total.checked_add(value.len()).ok_or(GraphPageError::Bounds)
    })?;
    if dictionary_bytes > MAX_GRAPH_DICTIONARY_BYTES
        || page.strings.iter().skip(1).any(|value| value.is_empty())
    {
        return Err(GraphPageError::Dictionary);
    }

    let node_count = u64::try_from(page.nodes.len()).map_err(|_| GraphPageError::Bounds)?;
    let edge_count = u64::try_from(page.edges.len()).map_err(|_| GraphPageError::Bounds)?;
    if page.returned_nodes_cumulative < node_count
        || page.returned_edges_cumulative < edge_count
        || page
            .total_matching_nodes
            .is_some_and(|total| total < page.returned_nodes_cumulative)
        || page
            .total_matching_edges
            .is_some_and(|total| total < page.returned_edges_cumulative)
        || matches!(
            (page.total_known_nodes, page.total_matching_nodes),
            (Some(known), Some(matching)) if known < matching
        )
        || matches!(
            (page.total_known_edges, page.total_matching_edges),
            (Some(known), Some(matching)) if known < matching
        )
    {
        return Err(GraphPageError::Bounds);
    }

    let mut previous_ordinal = None;
    for node in &page.nodes {
        if previous_ordinal.is_some_and(|previous| node.ordinal <= previous)
            || u64::from(node.ordinal) >= page.returned_nodes_cumulative
        {
            return Err(GraphPageError::Ordinals);
        }
        previous_ordinal = Some(node.ordinal);
        validate_node(node, page.strings.len())?;
    }
    for edge in &page.edges {
        if u64::from(edge.source_ordinal) >= page.returned_nodes_cumulative
            || u64::from(edge.target_ordinal) >= page.returned_nodes_cumulative
        {
            return Err(GraphPageError::Ordinals);
        }
        validate_edge(edge)?;
    }
    Ok(())
}

fn validate_node(node: &graph::GraphNode, dictionary_len: usize) -> Result<(), GraphPageError> {
    if node.stable_id.is_empty()
        || node.stable_id.len() > 512
        || node.stable_id.as_bytes().contains(&0)
        || node.label_index == 0
        || !valid_dictionary_index(node.label_index, dictionary_len)
        || node
            .path_index
            .is_some_and(|index| index == 0 || !valid_dictionary_index(index, dictionary_len))
        || node
            .community_index
            .is_some_and(|index| index == 0 || !valid_dictionary_index(index, dictionary_len))
        || node
            .component_index
            .is_some_and(|index| index == 0 || !valid_dictionary_index(index, dictionary_len))
        || node.confidence > 1_000
    {
        return Err(GraphPageError::Dictionary);
    }
    // Unknown non-zero values remain valid so newer peers can extend closed
    // client enums without making the wire page structurally invalid.
    if node.id_kind == 0 || node.kind == 0 || node.evidence == 0 {
        return Err(GraphPageError::Value);
    }
    Ok(())
}

fn validate_edge(edge: &graph::GraphEdge) -> Result<(), GraphPageError> {
    if edge.weight == 0 || edge.confidence > 1_000 || edge.evidence_count == 0 {
        return Err(GraphPageError::Value);
    }
    if edge.relation == 0 || edge.overlay == 0 {
        return Err(GraphPageError::Value);
    }
    Ok(())
}

fn valid_dictionary_index(index: u32, dictionary_len: usize) -> bool {
    usize::try_from(index).is_ok_and(|index| index < dictionary_len)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn graph_page_errors_preserve_public_traits_and_exact_messages() {
        fn assert_traits<T: std::error::Error + Copy + Clone + Eq + Send + Sync + 'static>() {}
        assert_traits::<GraphPageError>();
        let cases: [(GraphPageError, &str, &str); 6] = [
            (
                GraphPageError::Schema,
                "Schema",
                "graph page schema is invalid",
            ),
            (
                GraphPageError::Bounds,
                "Bounds",
                "graph page bounds are invalid",
            ),
            (
                GraphPageError::Dictionary,
                "Dictionary",
                "graph page dictionary is invalid",
            ),
            (
                GraphPageError::Ordinals,
                "Ordinals",
                "graph page ordinals are invalid",
            ),
            (
                GraphPageError::Value,
                "Value",
                "graph page value is invalid",
            ),
            (
                GraphPageError::Checksum,
                "Checksum",
                "graph page checksum is invalid",
            ),
        ];
        for (index, (error, variant, message)) in cases.into_iter().enumerate() {
            assert!(
                cases[..index]
                    .iter()
                    .all(|(previous, _, _)| previous != &error)
            );
            assert_eq!(format!("{error:?}"), variant);
            assert_eq!(error.to_string(), message);
            assert_eq!(format!("{error:#}"), message);
            let erased: &(dyn std::error::Error + Send + Sync) = &error;
            assert!(erased.source().is_none());
            assert!(erased.is::<GraphPageError>());
            assert_eq!(erased.downcast_ref::<GraphPageError>(), Some(&error));
            let boxed: Box<dyn std::error::Error + Send + Sync> = Box::new(error);
            assert_eq!(*boxed.downcast::<GraphPageError>().unwrap(), error);
        }
    }

    fn valid_page() -> graph::GraphPage {
        graph::GraphPage {
            schema_version: UI_GRAPH_SCHEMA_VERSION,
            strings: vec![String::new(), "src/lib.rs".to_owned()],
            nodes: vec![graph::GraphNode {
                ordinal: 0,
                stable_id: "file:1".to_owned(),
                id_kind: graph::NodeIdKind::File as i32,
                label_index: 1,
                path_index: Some(1),
                kind: graph::NodeKind::File as i32,
                confidence: 1_000,
                generated: Some(false),
                community_index: None,
                component_index: None,
                symbol_count: Some(1),
                fan_in: Some(0),
                fan_out: Some(0),
                hotspot_score: None,
                evidence: graph::EvidenceClass::Structural as i32,
            }],
            edges: Vec::new(),
            returned_nodes_cumulative: 1,
            returned_edges_cumulative: 0,
            total_matching_nodes: Some(1),
            total_matching_edges: Some(0),
            total_known_nodes: Some(1),
            total_known_edges: Some(0),
            edges_omitted_for_unavailable_endpoints: 0,
            skipped_for_coverage: 0,
            checksum: Vec::new(),
        }
    }

    #[test]
    fn sealed_page_round_trips_and_detects_tampering() {
        let mut page = valid_page();
        seal_graph_page(&mut page).expect("bounded page seals");
        validate_graph_page(&page).expect("sealed page validates");

        page.nodes[0].confidence = 999;
        assert_eq!(validate_graph_page(&page), Err(GraphPageError::Checksum));
    }

    #[test]
    fn page_rejects_dangling_and_unspecified_values() {
        let mut page = valid_page();
        page.edges.push(graph::GraphEdge {
            source_ordinal: 0,
            target_ordinal: 1,
            relation: graph::RelationKind::Calls as i32,
            weight: 1,
            confidence: 1_000,
            exact: true,
            inferred: false,
            evidence_count: 1,
            overlay: graph::OverlayRole::None as i32,
        });
        page.returned_edges_cumulative = 1;
        page.total_matching_edges = Some(1);
        page.total_known_edges = Some(1);
        assert_eq!(seal_graph_page(&mut page), Err(GraphPageError::Ordinals));

        let mut page = valid_page();
        page.nodes[0].kind = graph::NodeKind::Unspecified as i32;
        assert_eq!(seal_graph_page(&mut page), Err(GraphPageError::Value));

        let mut page = valid_page();
        page.nodes[0].kind = 99;
        seal_graph_page(&mut page).expect("unknown non-zero enum remains forward compatible");
        validate_graph_page(&page).expect("sealed additive enum validates");
    }
}
