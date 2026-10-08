//! Source-owned Groovy callable headers for indexed declaration selection.
//! Native parameter shape is not an overload decision or a runtime dispatch proof.

use rootlight_ids::{ContentHash, FactId, GenerationId, RepositoryId, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
};

/// Namespace for native Groovy callable declaration evidence.
pub const GROOVY_CALLABLE_NAMESPACE: &str = "rootlight.groovy_callable";

/// Dispatch mode established by an untransformed native declaration header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroovyCallableDispatch {
    /// Authored static method.
    Static,
    /// Instance or script method without a static modifier.
    Instance,
    /// An annotation or inconsistent modifier prevents a semantic assertion.
    Unknown,
}

/// Native access distinction retained independently of the common IR projection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroovyCallableVisibility {
    /// Public by default or by an explicit modifier.
    Public,
    /// Explicitly protected.
    Protected,
    /// Explicitly private.
    Private,
    /// Access may depend on an unexecuted transform or invalid header.
    Unknown,
}

/// Native return classification needed to distinguish boolean property accessors.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroovyCallableReturnType {
    /// The authored return type is exactly the primitive `boolean` type.
    PrimitiveBoolean,
    /// A different native type, including an untyped method's dynamic return.
    Other,
    /// No classification is available, including legacy callable envelopes.
    #[default]
    Unknown,
}

impl GroovyCallableReturnType {
    fn is_unknown(&self) -> bool {
        *self == Self::Unknown
    }
}

/// Paired parser evidence binding an authored spelling to its decoded name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroovyCallableName {
    /// Hash of authored spelling expressed as UTF-8, including quotes and escapes.
    /// Physical byte ownership remains in the envelope source and name span.
    pub spelling_hash: ContentHash,
    /// Hash of the independently decoded, parser-attested canonical name.
    pub canonical_hash: ContentHash,
}

/// Parser-attested header shape, without inferred parameter types or targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroovyCallable {
    /// Exact written method-name span, including quotes when present.
    pub name: SourceSpan,
    /// Paired written/canonical name evidence; absent in legacy envelopes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_identity: Option<GroovyCallableName>,
    /// Complete parenthesized formal parameter list.
    pub parameters: SourceSpan,
    /// Number of authored formal parameters, excluding nested closure parameters.
    pub parameter_count: u32,
    /// Number of parameters without authored defaults; includes a vararg parameter.
    pub required_parameter_count: u32,
    /// The final parameter has native array or vararg type syntax.
    pub variadic: bool,
    /// Source-established method dispatch mode, or an explicit unknown.
    pub dispatch: GroovyCallableDispatch,
    /// Source-established access mode, or an explicit unknown.
    pub visibility: GroovyCallableVisibility,
    /// Native return distinction; absence in legacy payloads remains unknown.
    #[serde(default, skip_serializing_if = "GroovyCallableReturnType::is_unknown")]
    pub return_type: GroovyCallableReturnType,
}

/// A malformed or inconsistent native callable envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid groovy callable evidence")]
pub struct GroovyCallableError;

/// Encodes a bounded callable header owned by the full declaration source.
///
/// # Errors
/// Rejects foreign repository/generation, empty or noncontained name/parameter
/// spans, inconsistent counts and unencodable payloads. The producer must
/// independently establish native ownership and complete parse coverage.
pub fn new_groovy_callable_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    callable: GroovyCallable,
) -> Result<ExtensionEnvelope, GroovyCallableError> {
    let span = source.span();
    if source.repository() != repository
        || source.generation() != generation
        || !contains(span, callable.name)
        || !contains(span, callable.parameters)
        || callable.name.end_byte() > callable.parameters.start_byte()
        || callable.parameters.end_byte() - callable.parameters.start_byte() < 2
        || callable.required_parameter_count > callable.parameter_count
        || u64::from(callable.parameter_count)
            > callable.parameters.end_byte() - callable.parameters.start_byte() - 2
        || (callable.variadic && callable.parameter_count == 0)
    {
        return Err(GroovyCallableError);
    }
    let payload = serde_json::to_string(&callable).map_err(|_| GroovyCallableError)?;
    let subject = FactRef::File(span.file());
    let identity = serde_json::to_vec(&(
        GROOVY_CALLABLE_NAMESPACE,
        "1",
        repository,
        generation,
        provenance,
        &source,
        &payload,
        subject,
    ))
    .map_err(|_| GroovyCallableError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.groovy_callable/v1", &identity).id(),
        repository,
        generation,
        namespace: GROOVY_CALLABLE_NAMESPACE.to_owned(),
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

/// Verifies canonical encoding, identity and source ownership of callable evidence.
///
/// # Errors
/// Rejects oversized, altered, unknown-version and noncanonical envelopes.
/// This integrity check does not authenticate the producer's language semantics.
pub fn decode_groovy_callable_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<GroovyCallable, GroovyCallableError> {
    if envelope.namespace != GROOVY_CALLABLE_NAMESPACE || envelope.payload.len() > 1024 {
        return Err(GroovyCallableError);
    }
    let callable = serde_json::from_str(&envelope.payload).map_err(|_| GroovyCallableError)?;
    let expected = new_groovy_callable_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(GroovyCallableError)?,
        callable,
    )?;
    if expected != *envelope {
        return Err(GroovyCallableError);
    }
    Ok(callable)
}

fn contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    outer.file() == inner.file()
        && outer.start_byte() <= inner.start_byte()
        && inner.start_byte() < inner.end_byte()
        && inner.end_byte() <= outer.end_byte()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, content_hash};

    fn envelope() -> ExtensionEnvelope {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        new_groovy_callable_envelope(
            repository,
            generation,
            FactId::from_bytes([4; 20]),
            SourceRef::new(
                repository,
                generation,
                SourceSpan::new(file, 0, 40).unwrap(),
                content_hash(b"source"),
                None,
            ),
            GroovyCallable {
                name: SourceSpan::new(file, 11, 15).unwrap(),
                name_identity: None,
                parameters: SourceSpan::new(file, 15, 35).unwrap(),
                parameter_count: 2,
                required_parameter_count: 1,
                variadic: false,
                dispatch: GroovyCallableDispatch::Static,
                visibility: GroovyCallableVisibility::Public,
                return_type: GroovyCallableReturnType::Unknown,
            },
        )
        .unwrap()
    }

    #[test]
    fn callable_return_types_preserve_legacy_unknown_and_canonical_identity() {
        let legacy = envelope();
        assert!(!legacy.payload.contains("return_type"));
        let mut shape = decode_groovy_callable_envelope(&legacy).unwrap();
        assert_eq!(shape.return_type, GroovyCallableReturnType::Unknown);
        for kind in [
            GroovyCallableReturnType::PrimitiveBoolean,
            GroovyCallableReturnType::Other,
        ] {
            shape.return_type = kind;
            let current = new_groovy_callable_envelope(
                legacy.repository,
                legacy.generation,
                legacy.provenance,
                legacy.evidence.source.clone().unwrap(),
                shape,
            )
            .unwrap();
            assert_ne!(current.id, legacy.id);
            assert_eq!(
                decode_groovy_callable_envelope(&current)
                    .unwrap()
                    .return_type,
                kind
            );
            let mut forged = legacy.clone();
            forged.payload = current.payload;
            assert!(decode_groovy_callable_envelope(&forged).is_err());
        }
    }

    #[test]
    fn callable_name_evidence_preserves_legacy_encoding_and_binds_both_hashes() {
        let legacy = envelope();
        assert!(!legacy.payload.contains("name_identity"));
        let mut shape = decode_groovy_callable_envelope(&legacy).unwrap();
        assert_eq!(shape.name_identity, None);
        shape.name_identity = Some(GroovyCallableName {
            spelling_hash: content_hash(b"'fold'"),
            canonical_hash: content_hash(b"fold"),
        });
        let current = new_groovy_callable_envelope(
            legacy.repository,
            legacy.generation,
            legacy.provenance,
            legacy.evidence.source.clone().unwrap(),
            shape,
        )
        .unwrap();
        assert_ne!(current.id, legacy.id);
        assert_eq!(decode_groovy_callable_envelope(&current).unwrap(), shape);
        for field in ["spelling_hash", "canonical_hash"] {
            let mut altered = current.clone();
            let mut payload: serde_json::Value = serde_json::from_str(&altered.payload).unwrap();
            payload["name_identity"][field] = serde_json::to_value(content_hash(b"other")).unwrap();
            let changed: GroovyCallable = serde_json::from_value(payload).unwrap();
            altered.payload = serde_json::to_string(&changed).unwrap();
            assert!(decode_groovy_callable_envelope(&altered).is_err());
        }
        let mut explicit_null = legacy.clone();
        explicit_null.payload = legacy.payload.replacen('{', "{\"name_identity\":null,", 1);
        assert!(decode_groovy_callable_envelope(&explicit_null).is_err());
        shape.name_identity = None;
        assert_eq!(
            new_groovy_callable_envelope(
                legacy.repository,
                legacy.generation,
                legacy.provenance,
                legacy.evidence.source.clone().unwrap(),
                shape,
            )
            .unwrap(),
            legacy
        );
    }

    #[test]
    fn callable_encoding_rejects_mutation_and_noncanonical_payloads() {
        let original = envelope();
        assert!(decode_groovy_callable_envelope(&original).is_ok());
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
            assert!(decode_groovy_callable_envelope(&changed).is_err());
        }
    }

    #[test]
    fn callable_encoding_checks_spans_counts_and_generation() {
        let original = envelope();
        let shape = decode_groovy_callable_envelope(&original).unwrap();
        let source = original.evidence.source.clone().unwrap();
        for changed in [
            GroovyCallable {
                name: source.span(),
                ..shape
            },
            GroovyCallable {
                parameters: SourceSpan::new(FileId::from_bytes([9; 20]), 15, 35).unwrap(),
                ..shape
            },
            GroovyCallable {
                parameters: SourceSpan::new(shape.name.file(), 15, 15).unwrap(),
                ..shape
            },
            GroovyCallable {
                parameter_count: u32::MAX,
                ..shape
            },
            GroovyCallable {
                required_parameter_count: 3,
                ..shape
            },
            GroovyCallable {
                parameter_count: 0,
                required_parameter_count: 0,
                variadic: true,
                ..shape
            },
        ] {
            assert!(
                new_groovy_callable_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    source.clone(),
                    changed
                )
                .is_err()
            );
        }
        assert!(
            new_groovy_callable_envelope(
                original.repository,
                GenerationId::from_bytes([8; 20]),
                original.provenance,
                source,
                shape
            )
            .is_err()
        );
    }
}
