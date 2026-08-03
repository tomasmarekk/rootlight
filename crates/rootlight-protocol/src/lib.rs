//! Versioned protobuf wire contracts for local Rootlight process boundaries.
//!
//! Generated messages are checked in during the protocol generation workflow so ordinary builds never
//! require network access or a protobuf compiler.

#![forbid(unsafe_code)]

pub mod adapter_contract;
mod graph;

pub use graph::{
    GraphPageError, MAX_GRAPH_AGGREGATE_EDGES, MAX_GRAPH_AGGREGATE_NODES,
    MAX_GRAPH_DICTIONARY_BYTES, MAX_GRAPH_DICTIONARY_ENTRIES, MAX_GRAPH_PAGE_BYTES,
    MAX_GRAPH_PAGE_EDGES, MAX_GRAPH_PAGE_NODES, MAX_GRAPH_STRING_BYTES, UI_GRAPH_SCHEMA_VERSION,
    seal_graph_page, validate_graph_page,
};

/// Generated messages compiled from the checked protocol sources.
pub mod generated;

/// Canonical descriptor set for compatibility tooling and reflection-free checks.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!("../../../schemas/generated/protobuf/rootlight.desc");

/// Earliest daemon protocol accepted by the current client and server.
///
/// Protocol 1.0 remains a frozen wire-compatibility baseline, but it predates
/// authenticated operation submission and cannot satisfy the current contract.
pub const MINIMUM_PROTOCOL_MINOR: u32 = 1;
/// Latest daemon protocol implemented by the current client and server.
pub const CURRENT_PROTOCOL_MINOR: u32 = 10;
/// Current production protocol contract version.
pub const PROTOCOL_VERSION: &str = "1.10";
/// Schema version for a complete effective first-slice request budget.
pub const FIRST_SLICE_EFFECTIVE_BUDGET_SCHEMA_VERSION: u32 = 1;
/// Hard transport admission maximum for logical rows.
pub const MAX_FIRST_SLICE_BUDGET_ROWS: u64 = 1_000_000;
/// Hard transport admission maximum for traversed edges.
pub const MAX_FIRST_SLICE_BUDGET_EDGES: u64 = 1_000_000;
/// Hard transport admission maximum for returned results.
pub const MAX_FIRST_SLICE_BUDGET_RESULTS: u64 = 10_000;
/// Hard transport admission maximum for returned source bytes.
pub const MAX_FIRST_SLICE_BUDGET_SOURCE_BYTES: u64 = 512 * 1024;
/// Hard transport admission maximum for serialized JSON bytes.
pub const MAX_FIRST_SLICE_BUDGET_JSON_BYTES: u64 = 4 * 1024 * 1024;
/// Hard transport admission maximum for conservative estimated tokens.
pub const MAX_FIRST_SLICE_BUDGET_ESTIMATED_TOKENS: u64 = 4 * 1024 * 1024;
/// Hard transport admission maximum for owned response memory.
pub const MAX_FIRST_SLICE_BUDGET_MEMORY_BYTES: u64 = 128 * 1024 * 1024;
/// Hard transport admission maximum for query duration.
pub const MAX_FIRST_SLICE_BUDGET_DURATION_MICROS: u64 = 10_000_000;
/// Hard transport admission maximum for traversal depth.
pub const MAX_FIRST_SLICE_BUDGET_DEPTH: u64 = 16;
/// Hard transport admission maximum for returned paths.
pub const MAX_FIRST_SLICE_BUDGET_PATHS: u64 = 1_000;
/// Hard transport admission maximum for one dead-code classification label.
pub const MAX_CODE_DEAD_CLASSIFICATION_BYTES: usize = 64;
/// Hard transport admission maximum for `code.locate` language filters.
pub const MAX_CODE_LOCATE_LANGUAGES: usize = 32;
/// Hard transport admission maximum for one canonical language filter.
pub const MAX_CODE_LOCATE_LANGUAGE_BYTES: usize = 64;

#[cfg(test)]
mod tests {
    use prost::Message;
    use prost_types::FileDescriptorSet;

    use super::*;

    #[test]
    fn descriptor_contains_only_versioned_protocol_packages() {
        let descriptor = FileDescriptorSet::decode(FILE_DESCRIPTOR_SET)
            .expect("checked descriptor set is valid protobuf");
        let packages: Vec<_> = descriptor
            .file
            .iter()
            .filter_map(|file| file.package.as_deref())
            .collect();

        assert_eq!(
            packages,
            [
                "rootlight.common.v1",
                "rootlight.ui.graph.v1",
                "rootlight.daemon.v1",
                "rootlight.adapter.v1",
            ]
        );
        assert!(descriptor.file.iter().all(|file| file.service.is_empty()));
    }

    #[test]
    fn generated_messages_round_trip_unknown_additive_fields() {
        let mut encoded = generated::common::v1::ContractVersion {
            major: 1,
            minor: CURRENT_PROTOCOL_MINOR,
        }
        .encode_to_vec();
        encoded.extend_from_slice(&[0x98, 0x06, 0x07]);

        let decoded = generated::common::v1::ContractVersion::decode(encoded.as_slice())
            .expect("unknown protobuf field is skipped");
        assert_eq!(decoded.major, 1);
        assert_eq!(decoded.minor, CURRENT_PROTOCOL_MINOR);
    }

    #[test]
    fn legacy_operation_status_decoder_ignores_current_additive_fields() {
        use generated::{common::v1 as common, daemon::v1 as daemon};

        // This local shape freezes the fields known to protocol 1.8 so an
        // additive current response is exercised through an actual old decoder.
        #[derive(Clone, PartialEq, Message)]
        struct LegacyRepositoryOperationStatusResponse {
            #[prost(message, optional, tag = "1")]
            schema_version: Option<common::ContractVersion>,
            #[prost(message, optional, tag = "2")]
            operation: Option<daemon::OperationStatus>,
            #[prost(message, optional, tag = "3")]
            published_generation: Option<common::GenerationId>,
            #[prost(uint64, tag = "4")]
            started_unix_ms: u64,
            #[prost(uint64, tag = "5")]
            peak_rss_bytes: u64,
            #[prost(uint64, tag = "6")]
            written_bytes: u64,
            #[prost(uint64, tag = "7")]
            files_examined: u64,
            #[prost(uint32, optional, tag = "8")]
            retry_after_ms: Option<u32>,
        }

        let response = daemon::RepositoryOperationStatusResponse {
            schema_version: Some(common::ContractVersion { major: 1, minor: 0 }),
            operation: Some(daemon::OperationStatus {
                operation: Some(common::OperationId { value: vec![7; 20] }),
                state: daemon::OperationState::Succeeded as i32,
                revision: 9,
                completed_units: 4,
                total_units: 4,
                error: None,
                kind: daemon::OperationKind::RepositoryIndex as i32,
                stage: daemon::OperationStage::Cleanup as i32,
                plan_hash: vec![3; 32],
                detached: true,
                cancellation_requested: false,
                deadline_unix_ms: None,
                lease_expires_unix_ms: None,
                recovery_class: daemon::RecoveryClass::NotApplicable as i32,
            }),
            published_generation: Some(common::GenerationId {
                value: vec![11; 24],
            }),
            started_unix_ms: 1,
            peak_rss_bytes: 2,
            written_bytes: 3,
            files_examined: 4,
            retry_after_ms: None,
            bytes_examined: 5,
            index_stage: "complete".to_owned(),
            semantic_operation: Some(common::OperationId {
                value: vec![13; 20],
            }),
        };

        let decoded =
            LegacyRepositoryOperationStatusResponse::decode(response.encode_to_vec().as_slice())
                .expect("a protocol 1.8 decoder skips protocol 1.9 fields");
        assert_eq!(decoded.schema_version, response.schema_version);
        assert_eq!(decoded.operation, response.operation);
        assert_eq!(decoded.published_generation, response.published_generation);
        assert_eq!(decoded.started_unix_ms, 1);
        assert_eq!(decoded.files_examined, 4);
    }

    #[test]
    fn generated_error_codes_match_the_normative_domain_registry() {
        use generated::common::v1;

        for definition in rootlight_error::ERROR_REGISTRY {
            let wire = v1::ErrorCode::try_from(definition.wire_number)
                .expect("registry wire number is generated");
            assert_eq!(wire.as_str_name(), definition.name);
        }
        assert!(v1::ErrorCode::try_from(23).is_err());
    }

    #[test]
    fn effective_budget_and_usage_extensions_round_trip() {
        use generated::daemon::v1;

        let envelope = v1::RequestEnvelope {
            request_id: 7,
            instance_nonce: vec![3; 16],
            timeout_ms: Some(1_000),
            effective_budget: Some(v1::FirstSliceEffectiveBudget {
                schema_version: FIRST_SLICE_EFFECTIVE_BUDGET_SCHEMA_VERSION,
                rows: 10,
                edges: 20,
                results: 3,
                source_bytes: 4_096,
                json_bytes: 8_192,
                estimated_tokens: 2_048,
                memory_bytes: 16_384,
                duration_micros: 500_000,
                depth: Some(4),
                paths: Some(5),
            }),
            request: Some(v1::request_envelope::Request::Health(v1::HealthRequest {})),
        };
        let encoded = envelope.encode_to_vec();
        let decoded =
            v1::RequestEnvelope::decode(encoded.as_slice()).expect("request envelope decodes");
        assert_eq!(decoded, envelope);

        let usage = v1::FirstSliceQueryUsage {
            rows: 1,
            edges: 2,
            results: 3,
            source_bytes: 4,
            json_bytes: 5,
            estimated_tokens: 6,
            elapsed_micros: 7,
            token_accounting: Some(
                v1::FirstSliceTokenAccountingProfile::FirstSliceTokenAccountingUtf8ByteUpperBoundV1
                    as i32,
            ),
            memory_bytes: Some(8),
        };
        let encoded = usage.encode_to_vec();
        let decoded =
            v1::FirstSliceQueryUsage::decode(encoded.as_slice()).expect("query usage decodes");
        assert_eq!(decoded, usage);
    }
}
