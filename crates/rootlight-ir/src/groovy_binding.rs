//! Native Groovy import-use evidence with stable source spans rather than site IDs.
//! The envelope survives generation rebinding and retains the import that selected
//! a canonical type name; it does not claim classpath or runtime completeness.

use rootlight_ids::{ContentHash, FactId, GenerationId, RepositoryId, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
};

/// Namespace for native explicit-import type-use evidence.
pub const GROOVY_BINDING_NAMESPACE: &str = "rootlight.groovy_binding";

/// Native selection of an explicit type import, before project target resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroovyImportedType {
    /// Exact source range of the import's qualified target name.
    pub import: SourceSpan,
    /// Hash of the language-canonical fully qualified target name.
    pub target: ContentHash,
}

/// A malformed or inconsistent native import-use envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid groovy import-use evidence")]
pub struct GroovyBindingError;

/// Encodes a bounded native import selection with exact use-site source ownership.
///
/// # Errors
/// Rejects foreign repository/generation, empty or overlapping source ranges,
/// imports in a different file, and unencodable payloads. The native producer
/// must independently establish lexical precedence and complete parse coverage.
pub fn new_groovy_binding_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    binding: GroovyImportedType,
) -> Result<ExtensionEnvelope, GroovyBindingError> {
    let span = source.span();
    if source.repository() != repository
        || source.generation() != generation
        || span.file() != binding.import.file()
        || span.start_byte() == span.end_byte()
        || binding.import.start_byte() == binding.import.end_byte()
        || (span.start_byte() < binding.import.end_byte()
            && binding.import.start_byte() < span.end_byte())
    {
        return Err(GroovyBindingError);
    }
    let payload = serde_json::to_string(&binding).map_err(|_| GroovyBindingError)?;
    let subject = FactRef::File(span.file());
    let identity = serde_json::to_vec(&(
        GROOVY_BINDING_NAMESPACE,
        "1",
        repository,
        generation,
        provenance,
        &source,
        &payload,
        subject,
    ))
    .map_err(|_| GroovyBindingError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.groovy_binding/v1", &identity).id(),
        repository,
        generation,
        namespace: GROOVY_BINDING_NAMESPACE.to_owned(),
        version: "1".to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: vec![subject],
        },
    })
}

/// Decodes and authenticates the canonical shape and identity of native evidence.
///
/// # Errors
/// Rejects oversized, unknown-version, noncanonical or altered envelopes. This
/// verifies encoding, not the trustworthiness of the producing parser.
pub fn decode_groovy_binding_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<GroovyImportedType, GroovyBindingError> {
    if envelope.namespace != GROOVY_BINDING_NAMESPACE || envelope.payload.len() > 1024 {
        return Err(GroovyBindingError);
    }
    let binding = serde_json::from_str(&envelope.payload).map_err(|_| GroovyBindingError)?;
    let expected = new_groovy_binding_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope.evidence.source.clone().ok_or(GroovyBindingError)?,
        binding,
    )?;
    if expected != *envelope {
        return Err(GroovyBindingError);
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
        new_groovy_binding_envelope(
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
            GroovyImportedType {
                import: SourceSpan::new(file, 7, 20).unwrap(),
                target: content_hash(b"sample.Widget"),
            },
        )
        .unwrap()
    }

    #[test]
    fn groovy_binding_rejects_mutation_and_noncanonical_payloads() {
        let original = envelope();
        assert!(decode_groovy_binding_envelope(&original).is_ok());
        for mutate in [
            |value: &mut ExtensionEnvelope| value.version = "2".into(),
            |value: &mut ExtensionEnvelope| value.namespace = "unknown".into(),
            |value: &mut ExtensionEnvelope| value.criticality = ExtensionCriticality::Critical,
            |value: &mut ExtensionEnvelope| value.payload.push(' '),
            |value: &mut ExtensionEnvelope| value.payload = " ".repeat(1025),
            |value: &mut ExtensionEnvelope| value.evidence.derivation.clear(),
            |value: &mut ExtensionEnvelope| value.evidence.source = None,
            |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([9; 20]),
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(decode_groovy_binding_envelope(&changed).is_err());
        }
    }

    #[test]
    fn groovy_binding_enforces_source_and_generation_ownership() {
        let original = envelope();
        let source = original.evidence.source.clone().unwrap();
        for import in [
            source.span(),
            SourceSpan::new(source.span().file(), 2, 2).unwrap(),
            SourceSpan::new(FileId::from_bytes([9; 20]), 7, 20).unwrap(),
        ] {
            let binding = GroovyImportedType {
                import,
                target: content_hash(b"sample.Widget"),
            };
            assert!(
                new_groovy_binding_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    source.clone(),
                    binding
                )
                .is_err()
            );
        }
        let binding = decode_groovy_binding_envelope(&original).unwrap();
        assert!(
            new_groovy_binding_envelope(
                original.repository,
                GenerationId::from_bytes([8; 20]),
                original.provenance,
                source,
                binding
            )
            .is_err()
        );
    }
}
