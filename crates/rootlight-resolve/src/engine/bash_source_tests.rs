//! Shell wrapper certificates require current native and source-owned joins.
//! Generic spelling recovery cannot repair a refused command-context proof.

use super::*;
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::{FactEvidence, SourceRef, SourceSpan};

#[test]
fn shell_wrapper_calls_require_current_native_source_binding() {
    for label in [
        "bash.direct_wrapper_call.call",
        "bash.direct_wrapper_call.extra",
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
            role: OccurrenceRole::CallSite,
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
        site.target = OccurrenceTarget::Unresolved { text_hash: hash };
        site.syntax_kind = "bash.call.call".to_owned();
        assert!(resolvable_occurrence(&site));
    }
}
