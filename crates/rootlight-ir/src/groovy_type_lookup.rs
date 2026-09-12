//! Native lexical context for project-wide Groovy type lookup.
//! Source spans survive generation rebinding; the producer attests eligibility,
//! while the resolver must separately prove the ancestry and selected target.

use rootlight_ids::{FactId, GenerationId, RepositoryId, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, IrLimits, SourceRef, SourceSpan,
};

/// Namespace for source-owned inherited type and qualified header lookup context.
pub const GROOVY_TYPE_LOOKUP_NAMESPACE: &str = "rootlight.groovy_type_lookup";

/// Parser-attested lexical context, without a claim about the selected type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroovyTypeLookup {
    /// Full declaration span of the nearest enclosing type.
    pub owner: SourceSpan,
    /// Language-canonical simple or dotted type name, without source trivia.
    pub name: String,
    /// Explicit simple-name import considered only after lexical hierarchy lookup.
    pub fallback_import: Option<crate::GroovyImportedType>,
    /// Source-local declaration considered only after qualified package lookup.
    pub fallback_local: Option<SourceSpan>,
}

/// A malformed or inconsistent native type lookup envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid groovy type lookup evidence")]
pub struct GroovyTypeLookupError;

/// Encodes source-owned lookup context under the existing IR string/payload bounds.
///
/// The producer must establish native type syntax, lexical precedence and owner
/// identity. Encoding validation does not establish those language semantics.
///
/// # Errors
/// Rejects foreign repository/generation, empty or noncontained use spans, invalid
/// name shape, oversized names/payloads and unencodable evidence.
pub fn new_groovy_type_lookup_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    lookup: GroovyTypeLookup,
) -> Result<ExtensionEnvelope, GroovyTypeLookupError> {
    let limits = IrLimits::default();
    let span = source.span();
    if source.repository() != repository
        || source.generation() != generation
        || span.file() != lookup.owner.file()
        || span.start_byte() == span.end_byte()
        || lookup.owner.start_byte() > span.start_byte()
        || lookup.owner.end_byte() < span.end_byte()
        || lookup.name.len() > limits.max_string_bytes
        || lookup.name.split('.').any(str::is_empty)
        || lookup.fallback_local.is_some_and(|local| {
            local.file() != span.file()
                || local.start_byte() == local.end_byte()
                || !lookup.name.contains('.')
                || lookup.fallback_import.is_some()
        })
        || lookup
            .name
            .chars()
            .any(|ch| ch.is_whitespace() || ch.is_control())
    {
        return Err(GroovyTypeLookupError);
    }
    let payload = serde_json::to_string(&lookup).map_err(|_| GroovyTypeLookupError)?;
    if let Some(import) = lookup.fallback_import {
        crate::new_groovy_binding_envelope(
            repository,
            generation,
            provenance,
            source.clone(),
            import,
        )
        .map_err(|_| GroovyTypeLookupError)?;
    }
    if payload.len() > limits.max_extension_payload_bytes {
        return Err(GroovyTypeLookupError);
    }
    let subject = FactRef::File(span.file());
    let identity = serde_json::to_vec(&(
        GROOVY_TYPE_LOOKUP_NAMESPACE,
        "1",
        repository,
        generation,
        provenance,
        &source,
        &payload,
        subject,
    ))
    .map_err(|_| GroovyTypeLookupError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.groovy_type_lookup/v1", &identity).id(),
        repository,
        generation,
        namespace: GROOVY_TYPE_LOOKUP_NAMESPACE.to_owned(),
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

/// Verifies canonical encoding, identity and source ownership of lookup context.
///
/// # Errors
/// Rejects oversized, altered, unknown-version and noncanonical envelopes.
/// This is an integrity check, not authentication of the producing parser.
pub fn decode_groovy_type_lookup_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<GroovyTypeLookup, GroovyTypeLookupError> {
    if envelope.namespace != GROOVY_TYPE_LOOKUP_NAMESPACE
        || envelope.payload.len() > IrLimits::default().max_extension_payload_bytes
    {
        return Err(GroovyTypeLookupError);
    }
    let lookup: GroovyTypeLookup =
        serde_json::from_str(&envelope.payload).map_err(|_| GroovyTypeLookupError)?;
    let expected = new_groovy_type_lookup_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(GroovyTypeLookupError)?,
        lookup.clone(),
    )?;
    if expected != *envelope {
        return Err(GroovyTypeLookupError);
    }
    Ok(lookup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, content_hash};

    fn envelope(name: &str) -> Result<ExtensionEnvelope, GroovyTypeLookupError> {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        new_groovy_type_lookup_envelope(
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
            GroovyTypeLookup {
                owner: SourceSpan::new(file, 10, 60).unwrap(),
                name: name.to_owned(),
                fallback_import: None,
                fallback_local: None,
            },
        )
    }

    #[test]
    fn groovy_type_lookup_validates_name_bounds_without_losing_unicode() {
        for name in ["Item", "Outer.Inner", "Δelta.Σhape", "Outer.$Inner"] {
            let value = envelope(name).unwrap();
            assert_eq!(
                decode_groovy_type_lookup_envelope(&value).unwrap().name,
                name
            );
        }
        let max = IrLimits::default().max_string_bytes;
        assert!(envelope(&"a".repeat(max)).is_ok());
        for name in ["", ".Item", "Item.", "A..B", "A B", "A\nB", "A\0B"] {
            assert!(envelope(name).is_err(), "{name:?}");
        }
        assert!(envelope(&"a".repeat(max + 1)).is_err());
    }

    #[test]
    fn groovy_type_lookup_rejects_mutated_envelopes() {
        let original = envelope("Outer.Inner").unwrap();
        for mutate in [
            |value: &mut ExtensionEnvelope| value.version = "2".into(),
            |value: &mut ExtensionEnvelope| value.namespace = "unknown".into(),
            |value: &mut ExtensionEnvelope| value.criticality = ExtensionCriticality::Critical,
            |value: &mut ExtensionEnvelope| value.payload.push(' '),
            |value: &mut ExtensionEnvelope| value.payload = "{".into(),
            |value: &mut ExtensionEnvelope| {
                value.payload = " ".repeat(IrLimits::default().max_extension_payload_bytes + 1)
            },
            |value: &mut ExtensionEnvelope| value.evidence.derivation.clear(),
            |value: &mut ExtensionEnvelope| value.evidence.source = None,
            |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([9; 20]),
            |value: &mut ExtensionEnvelope| value.provenance = FactId::from_bytes([9; 20]),
            |value: &mut ExtensionEnvelope| value.repository = RepositoryId::from_bytes([9; 16]),
            |value: &mut ExtensionEnvelope| value.generation = GenerationId::from_bytes([9; 20]),
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(decode_groovy_type_lookup_envelope(&changed).is_err());
        }
    }

    #[test]
    fn groovy_type_lookup_validates_local_fallback_identity_and_shape() {
        let original = envelope("Outer.Inner").unwrap();
        let source = original.evidence.source.clone().unwrap();
        let file = source.span().file();
        let mut lookup = decode_groovy_type_lookup_envelope(&original).unwrap();
        lookup.fallback_local = Some(SourceSpan::new(file, 0, 9).unwrap());
        let encode = |lookup| {
            new_groovy_type_lookup_envelope(
                original.repository,
                original.generation,
                original.provenance,
                source.clone(),
                lookup,
            )
        };
        let with_fallback = encode(lookup.clone()).unwrap();
        assert_eq!(
            decode_groovy_type_lookup_envelope(&with_fallback).unwrap(),
            lookup
        );
        assert_ne!(with_fallback.id, original.id);
        let mut tampered = original.clone();
        tampered.payload = with_fallback.payload;
        assert!(decode_groovy_type_lookup_envelope(&tampered).is_err());

        for local in [
            SourceSpan::new(FileId::from_bytes([9; 20]), 0, 9).unwrap(),
            SourceSpan::new(file, 9, 9).unwrap(),
        ] {
            let mut invalid = lookup.clone();
            invalid.fallback_local = Some(local);
            assert!(encode(invalid).is_err());
        }
        let mut simple = lookup.clone();
        simple.name = "Inner".into();
        assert!(encode(simple).is_err());
        lookup.fallback_import = Some(crate::GroovyImportedType {
            import: SourceSpan::new(file, 0, 9).unwrap(),
            target: content_hash(b"sample.Inner"),
        });
        assert!(encode(lookup).is_err());
    }

    #[test]
    fn groovy_type_lookup_validates_import_fallback_source_ownership() {
        let original = envelope("Item").unwrap();
        let source = original.evidence.source.clone().unwrap();
        let file = source.span().file();
        let mut lookup = decode_groovy_type_lookup_envelope(&original).unwrap();
        let encode = |lookup| {
            new_groovy_type_lookup_envelope(
                original.repository,
                original.generation,
                original.provenance,
                source.clone(),
                lookup,
            )
        };
        lookup.fallback_import = Some(crate::GroovyImportedType {
            import: SourceSpan::new(file, 0, 9).unwrap(),
            target: content_hash(b"sample.Item"),
        });
        let with_fallback = encode(lookup.clone()).unwrap();
        assert_eq!(
            decode_groovy_type_lookup_envelope(&with_fallback).unwrap(),
            lookup
        );
        assert_ne!(with_fallback.id, original.id);
        for import in [
            SourceSpan::new(FileId::from_bytes([9; 20]), 0, 9).unwrap(),
            SourceSpan::new(file, 9, 9).unwrap(),
            SourceSpan::new(file, 29, 31).unwrap(),
            SourceSpan::new(file, 35, 37).unwrap(),
            source.span(),
        ] {
            lookup.fallback_import.as_mut().unwrap().import = import;
            assert!(encode(lookup.clone()).is_err());
        }
    }

    #[test]
    fn groovy_type_lookup_requires_nonempty_use_inside_same_source_owner() {
        let original = envelope("Item").unwrap();
        let source = original.evidence.source.clone().unwrap();
        for owner in [
            SourceSpan::new(source.span().file(), 31, 60).unwrap(),
            SourceSpan::new(source.span().file(), 10, 35).unwrap(),
            SourceSpan::new(source.span().file(), 30, 30).unwrap(),
            SourceSpan::new(FileId::from_bytes([9; 20]), 10, 60).unwrap(),
        ] {
            assert!(
                new_groovy_type_lookup_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    source.clone(),
                    GroovyTypeLookup {
                        owner,
                        name: "Item".into(),
                        fallback_import: None,
                        fallback_local: None,
                    },
                )
                .is_err()
            );
        }
        let empty = SourceRef::new(
            original.repository,
            original.generation,
            SourceSpan::new(source.span().file(), 30, 30).unwrap(),
            source.content_hash(),
            None,
        );
        assert!(
            new_groovy_type_lookup_envelope(
                original.repository,
                original.generation,
                original.provenance,
                empty,
                decode_groovy_type_lookup_envelope(&original).unwrap(),
            )
            .is_err()
        );
    }
}
