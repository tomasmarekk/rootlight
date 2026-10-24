//! CUDA source uncertainty cannot be promoted by generic name scores.
//! Resolved language-owned facets and ordinary Cpp admission retain their routes.

use super::*;
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::{FactEvidence, SourceRef, SourceSpan};

#[test]
fn cuda_unproved_names_require_native_binding_and_cpp_keeps_generic_route() {
    for (label, role) in [
        ("cuda.identifier.reference", OccurrenceRole::Reference),
        ("cuda.call.call", OccurrenceRole::CallSite),
        ("cuda.scalar_formal.read", OccurrenceRole::Read),
        ("cuda.kernel_launch.call", OccurrenceRole::CallSite),
    ] {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        let hash = content_hash(b"target");
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, 6).unwrap(),
            hash,
            None,
        );
        let symbol = SymbolId::from_bytes([6; 20]);
        let mut site = OccurrenceRecord {
            id: FactId::from_bytes([4; 20]),
            repository,
            generation,
            file,
            source: source.clone(),
            role,
            enclosing: None,
            target: OccurrenceTarget::Unresolved { text_hash: hash },
            syntactic_text_hash: hash,
            syntax_kind: label.to_owned(),
            provenance: FactId::from_bytes([5; 20]),
            confidence: Confidence::new(0).unwrap(),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        assert!(!resolvable_occurrence(&site));
        site.target = OccurrenceTarget::Candidates {
            symbols: vec![symbol],
            total_count: 1,
            completeness: CoverageStatus::Complete,
        };
        assert!(!resolvable_occurrence(&site));
        site.target = OccurrenceTarget::Resolved { symbol };
        assert!(resolvable_occurrence(&site));
        site.role = OccurrenceRole::Reference;
        site.syntax_kind = "cpp.identifier.reference".to_owned();
        site.target = OccurrenceTarget::Unresolved { text_hash: hash };
        assert!(resolvable_occurrence(&site));
    }
}
