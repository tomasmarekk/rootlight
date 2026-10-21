//! Generic name recovery cannot upgrade an unproved ThinkScript value dependency.
//! Resolved source certificates remain consumable without runtime inference.

use super::*;
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::{FactEvidence, SourceRef, SourceSpan};

#[test]
fn unproved_input_namespace_never_uses_generic_spelling_or_candidates() {
    let repository = RepositoryId::from_bytes([1; 16]);
    let generation = GenerationId::from_bytes([2; 20]);
    let file = FileId::from_bytes([3; 20]);
    let hash = content_hash(b"value");
    let source = SourceRef::new(
        repository,
        generation,
        SourceSpan::new(file, 0, 5).unwrap(),
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
        role: OccurrenceRole::Read,
        enclosing: None,
        target: OccurrenceTarget::Unresolved { text_hash: hash },
        syntactic_text_hash: hash,
        syntax_kind: "thinkscript.input_value.read".to_owned(),
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
}
