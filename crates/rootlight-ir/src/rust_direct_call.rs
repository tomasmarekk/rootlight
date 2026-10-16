//! Native Rust direct-call proof over an exact source definition witness.
//! The call stays unresolved until the resolver validates the parser-owned claim;
//! physical spans and entity identities survive occurrence-ID remapping.

use rootlight_ids::{ContentHash, FactId, GenerationId, RepositoryId, SymbolId, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
};

/// Namespace for source-owned Rust identifier-call proof.
pub const RUST_DIRECT_CALL_NAMESPACE: &str = "rootlight.rust_direct_call";

/// One exact free-function definition selected in the same written module.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RustDirectCall {
    /// Exact physical source range of the function definition name.
    pub definition: SourceSpan,
    /// Source-proven function entity selected by the native binding model.
    pub target: SymbolId,
    /// Language-canonical identifier identity, separate from authored spelling hashes.
    pub canonical_name: ContentHash,
}

/// A malformed or inconsistent source-owned Rust call proof.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid rust direct-call evidence")]
pub struct RustDirectCallError;

/// Encodes an exact source-owned free-function call claim.
///
/// # Errors
/// Rejects foreign repository/generation, empty or overlapping source ranges,
/// definitions in a different file, and unencodable payloads. The native producer
/// must prove complete written-module context and absence of lexical shadowing.
pub fn new_rust_direct_call_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    binding: RustDirectCall,
) -> Result<ExtensionEnvelope, RustDirectCallError> {
    let span = source.span();
    if source.repository() != repository
        || source.generation() != generation
        || span.file() != binding.definition.file()
        || span.start_byte() == span.end_byte()
        || binding.definition.start_byte() == binding.definition.end_byte()
        || (span.start_byte() < binding.definition.end_byte()
            && binding.definition.start_byte() < span.end_byte())
    {
        return Err(RustDirectCallError);
    }
    let payload = serde_json::to_string(&binding).map_err(|_| RustDirectCallError)?;
    let subjects = vec![FactRef::File(span.file()), FactRef::Entity(binding.target)];
    if payload.len() > 1024 {
        return Err(RustDirectCallError);
    }
    let identity = serde_json::to_vec(&(
        RUST_DIRECT_CALL_NAMESPACE,
        "1",
        repository,
        generation,
        provenance,
        &source,
        &payload,
        &subjects,
    ))
    .map_err(|_| RustDirectCallError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.rust_direct_call/v1", &identity).id(),
        repository,
        generation,
        namespace: RUST_DIRECT_CALL_NAMESPACE.to_owned(),
        version: "1".to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: subjects,
        },
    })
}

/// Decodes and authenticates the canonical shape and identity of native evidence.
///
/// # Errors
/// Rejects oversized, unknown-version, noncanonical or altered envelopes. This
/// verifies encoding, not the trustworthiness of the producing parser.
pub fn decode_rust_direct_call_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<RustDirectCall, RustDirectCallError> {
    if envelope.namespace != RUST_DIRECT_CALL_NAMESPACE || envelope.payload.len() > 1024 {
        return Err(RustDirectCallError);
    }
    let binding = serde_json::from_str(&envelope.payload).map_err(|_| RustDirectCallError)?;
    let expected = new_rust_direct_call_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(RustDirectCallError)?,
        binding,
    )?;
    if expected != *envelope {
        return Err(RustDirectCallError);
    }
    Ok(binding)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, content_hash};

    fn envelope() -> ExtensionEnvelope {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        new_rust_direct_call_envelope(
            repository,
            generation,
            FactId::from_bytes([4; 20]),
            SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, 30, 36).unwrap(),
                content_hash(b"source"),
                None,
            ),
            RustDirectCall {
                target: SymbolId::from_bytes([5; 20]),
                definition: SourceSpan::new(file, 3, 9).unwrap(),
                canonical_name: content_hash(b"target"),
            },
        )
        .unwrap()
    }

    #[test]
    fn direct_call_codec_authenticates_canonical_shape_and_derivation() {
        let original = envelope();
        let proof = decode_rust_direct_call_envelope(&original).unwrap();
        assert_eq!(
            original.evidence.derivation,
            vec![
                FactRef::File(proof.definition.file()),
                FactRef::Entity(proof.target)
            ]
        );
        for mutate in [
            |e: &mut ExtensionEnvelope| e.version = "2".into(),
            |e: &mut ExtensionEnvelope| e.namespace = "foreign".into(),
            |e: &mut ExtensionEnvelope| e.criticality = ExtensionCriticality::Critical,
            |e: &mut ExtensionEnvelope| e.payload.push(' '),
            |e: &mut ExtensionEnvelope| e.payload = " ".repeat(1025),
            |e: &mut ExtensionEnvelope| e.evidence.derivation.reverse(),
            |e: &mut ExtensionEnvelope| e.evidence.derivation.clear(),
            |e: &mut ExtensionEnvelope| e.evidence.source = None,
            |e: &mut ExtensionEnvelope| e.id = FactId::from_bytes([9; 20]),
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(decode_rust_direct_call_envelope(&changed).is_err());
        }
    }

    #[test]
    fn direct_call_codec_rejects_foreign_empty_and_overlapping_definition_sources() {
        let original = envelope();
        let source = original.evidence.source.clone().unwrap();
        let proof = decode_rust_direct_call_envelope(&original).unwrap();
        for definition in [
            source.span(),
            SourceSpan::new(source.span().file(), 2, 2).unwrap(),
            SourceSpan::new(FileId::from_bytes([9; 20]), 3, 9).unwrap(),
        ] {
            assert!(
                new_rust_direct_call_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    source.clone(),
                    RustDirectCall {
                        definition,
                        ..proof
                    }
                )
                .is_err()
            );
        }
        assert!(
            new_rust_direct_call_envelope(
                original.repository,
                GenerationId::from_bytes([8; 20]),
                original.provenance,
                source,
                proof
            )
            .is_err()
        );
    }
}
