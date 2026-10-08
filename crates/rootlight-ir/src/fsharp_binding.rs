//! Source-bound F# export, signature, qualified-use, import and project evidence.
//! Claims retain stable files, entities and source spans, never occurrence IDs,
//! so repository resolution can rebind sites without invalidating its inputs.

use rootlight_ids::{
    ContentHash, FactId, FileId, GenerationId, RepositoryId, SymbolId, derive_fact,
};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
};

/// Namespace for native F# binding and literal project composition evidence.
pub const FSHARP_BINDING_NAMESPACE: &str = "rootlight.fsharp_binding";
/// Frozen canonical binding payload version.
pub const FSHARP_BINDING_VERSION: &str = "1";
/// Canonical import manifest, container, open and import-dependent use version.
pub const FSHARP_IMPORT_BINDING_VERSION: &str = "2";
/// Canonical signature value visibility evidence, without inferred call arity.
pub const FSHARP_SIGNATURE_BINDING_VERSION: &str = "3";
/// Canonical signature record-name evidence, without inferred type or member semantics.
pub const FSHARP_SIGNATURE_TYPE_BINDING_VERSION: &str = "4";
/// Canonical signature alias-name evidence, without inferred alias target or type semantics.
pub const FSHARP_SIGNATURE_ALIAS_BINDING_VERSION: &str = "5";
/// Canonical plain-name uses awaiting authenticated owned-open lookup.
pub const FSHARP_OPENED_BINDING_VERSION: &str = "6";
/// Maximum path components, including the final authored declaration name.
pub const MAX_FSHARP_BINDING_COMPONENTS: usize = 32;
/// Maximum project members, leaving one derivation slot for the project file.
pub const MAX_FSHARP_PROJECT_MEMBERS: usize = 4_095;
/// Maximum encoded payload, within the normalized IR extension allowance.
pub const MAX_FSHARP_BINDING_PAYLOAD_BYTES: usize = 1024 * 1024;

/// Native or source-declared evidence, not an assertion of evaluated build closure.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum FSharpBinding {
    /// Complete native claims for this source, emitted only after conservation succeeds.
    Context,
    /// A public authored module value or function with its complete owner path.
    Export {
        /// Case-sensitive hashes of unquoted identifier components, ending in the value name.
        path: Vec<ContentHash>,
        /// Exact materialized declaration identity.
        symbol: SymbolId,
        /// Proven top-level argument slots; absence authorizes references only.
        slots: Option<u32>,
    },
    /// A signature-declared public value, not an assertion of callable implementation.
    SignatureValue {
        /// Case-sensitive hashes of unquoted identifier components, ending in the value name.
        path: Vec<ContentHash>,
        /// Exact materialized signature declaration identity, not its implementation target.
        symbol: SymbolId,
    },
    /// A signature-declared public record name, not an executable export or inferred type.
    SignatureType {
        /// Case-sensitive hashes of unquoted identifier components, ending in the record name.
        path: Vec<ContentHash>,
        /// Exact materialized signature record identity, not an implementation target.
        symbol: SymbolId,
    },
    /// A signature-declared public alias name, not an executable export or inferred target.
    SignatureAlias {
        /// Case-sensitive hashes of unquoted identifier components, ending in the alias name.
        path: Vec<ContentHash>,
        /// Exact materialized signature alias identity, not an implementation target.
        symbol: SymbolId,
    },
    /// An owned qualified read whose root cannot be a nearer value/module binding.
    Reference {
        /// The exact canonical authored component path, including the final name.
        path: Vec<ContentHash>,
    },
    /// A complete qualified application with the same root-selection proof as a read.
    Call {
        /// The exact canonical authored component path, including the final name.
        path: Vec<ContentHash>,
        /// Complete top-level application argument slots, not named formal leaves.
        slots: u32,
    },
    /// Ordered literal Compile membership from one complete unambiguous project document.
    Project {
        /// Admitted source identities in authored order; duplicates are forbidden.
        members: Vec<FileId>,
    },
    /// Complete native container and open-clause inventories for one source file.
    ImportContext {
        /// Nonempty sorted unique exact container declaration spans.
        containers: Vec<SourceSpan>,
        /// Sorted unique exact open-clause spans; empty is valid.
        opens: Vec<SourceSpan>,
    },
    /// A complete native container and its directly introduced root names.
    Container {
        /// Canonical explicit container path, without an authored value suffix.
        path: Vec<ContentHash>,
        /// Sorted unique direct value, module, type and unqualified case names; empty is valid.
        roots: Vec<ContentHash>,
    },
    /// An ordinary open clause within an exact native container boundary.
    Open {
        /// Canonical explicitly written opened container path.
        path: Vec<ContentHash>,
        /// Same-file container declaration containing the complete open clause.
        owner: SourceSpan,
    },
    /// A qualified read with local absence proof awaiting preceding-open resolution.
    ImportedReference {
        /// The exact canonical authored component path, including the final name.
        path: Vec<ContentHash>,
    },
    /// A complete qualified application awaiting the same preceding-open proof.
    ImportedCall {
        /// The exact canonical authored component path, including the final name.
        path: Vec<ContentHash>,
        /// Complete top-level application argument slots, not named formal leaves.
        slots: u32,
    },
    /// An exact plain read with native local absence awaiting owned-open lookup.
    OpenedReference {
        /// Case-sensitive hash of the single canonical authored name.
        name: ContentHash,
    },
    /// A complete plain application awaiting the same owned-open lookup.
    OpenedCall {
        /// Case-sensitive hash of the single canonical authored callee name.
        name: ContentHash,
        /// Complete top-level application argument slots, not inferred target arity.
        slots: u32,
    },
}

/// A typed claim owned by an original-source file boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FSharpBindingEvidence {
    scope: SourceSpan,
    binding: FSharpBinding,
}

impl FSharpBindingEvidence {
    /// Associates a claim with its complete original-source file scope.
    #[must_use]
    pub const fn new(scope: SourceSpan, binding: FSharpBinding) -> Self {
        Self { scope, binding }
    }

    /// Returns the original-source boundary, including any byte-order mark.
    #[must_use]
    pub const fn scope(&self) -> SourceSpan {
        self.scope
    }

    /// Returns the source producer's claim without copying its paths or members.
    #[must_use]
    pub const fn binding(&self) -> &FSharpBinding {
        &self.binding
    }
}

/// An invalid canonical payload, source boundary or envelope identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FSharpBindingError {
    /// Payload shape, path length or member conservation failed.
    #[error("invalid fsharp binding payload")]
    Payload,
    /// Source identity or ownership does not match the declared scope.
    #[error("invalid fsharp binding source")]
    Source,
    /// The envelope version, identity, criticality or derivation was altered.
    #[error("invalid fsharp binding envelope")]
    Envelope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEvidence {
    scope: SourceSpan,
    binding: FSharpBinding,
}

/// Encodes canonical binding evidence tied to exact source and stable derivations.
///
/// # Errors
/// Rejects foreign source identity, empty/out-of-scope claims, incomplete paths,
/// duplicate/oversized project membership, noncanonical root inventories, invalid
/// open ownership, and invalid or oversized payloads. Legacy variants retain v1;
/// import manifests, containers, opens and import-dependent uses select v2;
/// signature values select v3 without granting callable slots, and structural
/// signature record names select v4 without granting type or member semantics;
/// signature alias names select v5 without granting alias target or type semantics.
/// Plain-name uses select v6 without widening the frozen qualified-use paths.
/// The caller must establish native lexical truth or literal project membership;
/// this constructor validates the envelope contract, not language semantics.
pub fn new_fsharp_binding_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    evidence: FSharpBindingEvidence,
) -> Result<ExtensionEnvelope, FSharpBindingError> {
    let span = source.span();
    let scope = evidence.scope;
    if source.repository() != repository
        || source.generation() != generation
        || scope.start_byte() != 0
        || span.file() != scope.file()
        || span.start_byte() >= span.end_byte()
        || span.end_byte() > scope.end_byte()
        || (matches!(
            evidence.binding,
            FSharpBinding::Context
                | FSharpBinding::Project { .. }
                | FSharpBinding::ImportContext { .. }
        ) && span != scope)
    {
        return Err(FSharpBindingError::Source);
    }
    let mut derivation = match &evidence.binding {
        FSharpBinding::Context => vec![FactRef::File(span.file())],
        FSharpBinding::Export {
            path,
            symbol,
            slots,
        } => {
            validate_path(path, 2)?;
            if *slots == Some(0) {
                return Err(FSharpBindingError::Payload);
            }
            vec![FactRef::Entity(*symbol)]
        }
        FSharpBinding::SignatureValue { path, symbol } => {
            validate_path(path, 2)?;
            vec![FactRef::Entity(*symbol)]
        }
        FSharpBinding::SignatureType { path, symbol }
        | FSharpBinding::SignatureAlias { path, symbol } => {
            validate_path(path, 2)?;
            vec![FactRef::Entity(*symbol)]
        }
        FSharpBinding::Reference { path } | FSharpBinding::ImportedReference { path } => {
            validate_path(path, 2)?;
            vec![FactRef::File(span.file())]
        }
        FSharpBinding::Call { path, slots } | FSharpBinding::ImportedCall { path, slots } => {
            validate_path(path, 2)?;
            if *slots == 0 {
                return Err(FSharpBindingError::Payload);
            }
            vec![FactRef::File(span.file())]
        }
        FSharpBinding::OpenedReference { .. } => vec![FactRef::File(span.file())],
        FSharpBinding::OpenedCall { slots, .. } => {
            if *slots == 0 {
                return Err(FSharpBindingError::Payload);
            }
            vec![FactRef::File(span.file())]
        }
        FSharpBinding::Project { members } => {
            if members.is_empty() || members.len() > MAX_FSHARP_PROJECT_MEMBERS {
                return Err(FSharpBindingError::Payload);
            }
            let mut sorted = members.clone();
            sorted.sort_unstable();
            if sorted.contains(&span.file()) || sorted.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(FSharpBindingError::Payload);
            }
            std::iter::once(FactRef::File(span.file()))
                .chain(members.iter().copied().map(FactRef::File))
                .collect()
        }
        FSharpBinding::Container { path, roots } => {
            validate_path(path, 1)?;
            // The canonical text of a hash is longer than its binary width. This
            // precheck bounds serialization work; the encoded cap remains exact.
            if roots.len() > MAX_FSHARP_BINDING_PAYLOAD_BYTES / size_of::<ContentHash>()
                || roots.windows(2).any(|pair| pair[0] >= pair[1])
            {
                return Err(FSharpBindingError::Payload);
            }
            vec![FactRef::File(span.file())]
        }
        FSharpBinding::ImportContext { containers, opens } => {
            let count = containers
                .len()
                .checked_add(opens.len())
                .ok_or(FSharpBindingError::Payload)?;
            if containers.is_empty()
                || count > MAX_FSHARP_BINDING_PAYLOAD_BYTES / size_of::<SourceSpan>()
            {
                return Err(FSharpBindingError::Payload);
            }
            validate_manifest_spans(containers, scope)?;
            validate_manifest_spans(opens, scope)?;
            vec![FactRef::File(span.file())]
        }
        FSharpBinding::Open { path, owner } => {
            validate_path(path, 1)?;
            if owner.file() != scope.file()
                || owner.start_byte() >= owner.end_byte()
                || owner.end_byte() > scope.end_byte()
                || owner.start_byte() > span.start_byte()
                || owner.end_byte() < span.end_byte()
            {
                return Err(FSharpBindingError::Source);
            }
            vec![FactRef::File(span.file())]
        }
    };
    // IR canonicalization treats derivations as sets; compilation order belongs
    // only to the payload and must not alter this envelope after hashing.
    derivation.sort_unstable();
    derivation.dedup();
    let payload = serde_json::to_string(&evidence).map_err(|_| FSharpBindingError::Payload)?;
    if payload.len() > MAX_FSHARP_BINDING_PAYLOAD_BYTES {
        return Err(FSharpBindingError::Payload);
    }
    let (version, identity_domain) = match &evidence.binding {
        FSharpBinding::Context
        | FSharpBinding::Export { .. }
        | FSharpBinding::Reference { .. }
        | FSharpBinding::Call { .. }
        | FSharpBinding::Project { .. } => (FSHARP_BINDING_VERSION, "rootlight.fsharp_binding/v1"),
        FSharpBinding::ImportContext { .. }
        | FSharpBinding::Container { .. }
        | FSharpBinding::Open { .. }
        | FSharpBinding::ImportedReference { .. }
        | FSharpBinding::ImportedCall { .. } => {
            (FSHARP_IMPORT_BINDING_VERSION, "rootlight.fsharp_binding/v2")
        }
        FSharpBinding::SignatureValue { .. } => (
            FSHARP_SIGNATURE_BINDING_VERSION,
            "rootlight.fsharp_binding/v3",
        ),
        FSharpBinding::SignatureType { .. } => (
            FSHARP_SIGNATURE_TYPE_BINDING_VERSION,
            "rootlight.fsharp_binding/v4",
        ),
        FSharpBinding::SignatureAlias { .. } => (
            FSHARP_SIGNATURE_ALIAS_BINDING_VERSION,
            "rootlight.fsharp_binding/v5",
        ),
        FSharpBinding::OpenedReference { .. } | FSharpBinding::OpenedCall { .. } => {
            (FSHARP_OPENED_BINDING_VERSION, "rootlight.fsharp_binding/v6")
        }
    };
    let identity = serde_json::to_vec(&(
        FSHARP_BINDING_NAMESPACE,
        version,
        repository,
        generation,
        provenance,
        &source,
        &payload,
        &derivation,
    ))
    .map_err(|_| FSharpBindingError::Payload)?;
    Ok(ExtensionEnvelope {
        id: derive_fact(identity_domain, &identity).id(),
        repository,
        generation,
        namespace: FSHARP_BINDING_NAMESPACE.to_owned(),
        version: version.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation,
        },
    })
}

fn validate_path(path: &[ContentHash], minimum: usize) -> Result<(), FSharpBindingError> {
    if !(minimum..=MAX_FSHARP_BINDING_COMPONENTS).contains(&path.len()) {
        return Err(FSharpBindingError::Payload);
    }
    Ok(())
}

fn validate_manifest_spans(
    spans: &[SourceSpan],
    scope: SourceSpan,
) -> Result<(), FSharpBindingError> {
    if spans.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(FSharpBindingError::Payload);
    }
    if spans.iter().any(|span| {
        span.file() != scope.file()
            || span.start_byte() >= span.end_byte()
            || span.end_byte() > scope.end_byte()
    }) {
        return Err(FSharpBindingError::Source);
    }
    Ok(())
}

/// Decodes bounded canonical evidence and authenticates every envelope field.
///
/// # Errors
/// Rejects oversized, malformed, noncanonical or altered envelopes. Callers still
/// have to corroborate producer identity, complete contexts and source hashes.
pub fn decode_fsharp_binding_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<FSharpBindingEvidence, FSharpBindingError> {
    if envelope.namespace != FSHARP_BINDING_NAMESPACE
        || !matches!(
            envelope.version.as_str(),
            FSHARP_BINDING_VERSION
                | FSHARP_IMPORT_BINDING_VERSION
                | FSHARP_SIGNATURE_BINDING_VERSION
                | FSHARP_SIGNATURE_TYPE_BINDING_VERSION
                | FSHARP_SIGNATURE_ALIAS_BINDING_VERSION
                | FSHARP_OPENED_BINDING_VERSION
        )
        || envelope.criticality != ExtensionCriticality::Noncritical
    {
        return Err(FSharpBindingError::Envelope);
    }
    if envelope.payload.len() > MAX_FSHARP_BINDING_PAYLOAD_BYTES {
        return Err(FSharpBindingError::Payload);
    }
    let wire: WireEvidence =
        serde_json::from_str(&envelope.payload).map_err(|_| FSharpBindingError::Payload)?;
    let evidence = FSharpBindingEvidence::new(wire.scope, wire.binding);
    let expected = new_fsharp_binding_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(FSharpBindingError::Source)?,
        evidence.clone(),
    )?;
    if expected != *envelope {
        return Err(FSharpBindingError::Envelope);
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::content_hash;

    fn envelope(binding: FSharpBinding) -> Result<ExtensionEnvelope, FSharpBindingError> {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let scope = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 100).unwrap();
        new_fsharp_binding_envelope(
            repository,
            generation,
            FactId::from_bytes([4; 20]),
            SourceRef::new(repository, generation, scope, content_hash(b"source"), None),
            FSharpBindingEvidence::new(scope, binding),
        )
    }

    #[test]
    fn fsharp_binding_canonical_payloads_reject_mutation() {
        let original = envelope(FSharpBinding::Reference {
            path: vec![content_hash(b"Library"), content_hash(b"carry")],
        })
        .unwrap();
        assert!(decode_fsharp_binding_envelope(&original).is_ok());
        for mutate in [
            |value: &mut ExtensionEnvelope| value.version = "2".into(),
            |value: &mut ExtensionEnvelope| value.namespace = "unknown".into(),
            |value: &mut ExtensionEnvelope| value.criticality = ExtensionCriticality::Critical,
            |value: &mut ExtensionEnvelope| value.payload.push(' '),
            |value: &mut ExtensionEnvelope| {
                value.payload = " ".repeat(MAX_FSHARP_BINDING_PAYLOAD_BYTES + 1)
            },
            |value: &mut ExtensionEnvelope| value.evidence.derivation.clear(),
            |value: &mut ExtensionEnvelope| value.evidence.source = None,
            |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([9; 20]),
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(decode_fsharp_binding_envelope(&changed).is_err());
        }
    }

    #[test]
    fn opened_names_have_distinct_canonical_v6_identity_and_file_derivation() {
        let name = content_hash(b"offset");
        for (binding, encoded) in [
            (
                FSharpBinding::OpenedReference { name },
                format!(r#"{{"kind":"opened_reference","name":"{name}"}}"#),
            ),
            (
                FSharpBinding::OpenedCall { name, slots: 1 },
                format!(r#"{{"kind":"opened_call","name":"{name}","slots":1}}"#),
            ),
        ] {
            let original = envelope(binding.clone()).unwrap();
            assert_eq!(original.version, FSHARP_OPENED_BINDING_VERSION);
            assert!(original.payload.ends_with(&format!("{encoded}}}")));
            assert_eq!(
                original.evidence.derivation,
                vec![FactRef::File(FileId::from_bytes([3; 20]))]
            );
            let identity = serde_json::to_vec(&(
                FSHARP_BINDING_NAMESPACE,
                FSHARP_OPENED_BINDING_VERSION,
                original.repository,
                original.generation,
                original.provenance,
                original.evidence.source.as_ref().unwrap(),
                &original.payload,
                &original.evidence.derivation,
            ))
            .unwrap();
            assert_eq!(
                original.id,
                derive_fact("rootlight.fsharp_binding/v6", &identity).id()
            );
            assert_eq!(
                decode_fsharp_binding_envelope(&original).unwrap().binding(),
                &binding
            );
            for version in ["1", "2", "3", "4", "5", "7"] {
                let mut changed = original.clone();
                changed.version = version.to_owned();
                assert_eq!(
                    decode_fsharp_binding_envelope(&changed),
                    Err(FSharpBindingError::Envelope),
                    "version {version}"
                );
            }
        }
        assert_eq!(
            envelope(FSharpBinding::OpenedCall { name, slots: 0 }),
            Err(FSharpBindingError::Payload)
        );
        assert!(
            envelope(FSharpBinding::OpenedCall {
                name,
                slots: u32::MAX
            })
            .is_ok()
        );
    }

    #[test]
    fn opened_name_evidence_rejects_payload_source_and_derivation_tampering() {
        let name = content_hash(b"carry");
        for binding in [
            FSharpBinding::OpenedReference { name },
            FSharpBinding::OpenedCall { name, slots: 2 },
        ] {
            let original = envelope(binding).unwrap();
            for mutate in [
                |value: &mut ExtensionEnvelope| value.payload.push(' '),
                |value: &mut ExtensionEnvelope| {
                    let source = value.evidence.source.as_ref().unwrap();
                    value.evidence.source = Some(SourceRef::new(
                        value.repository,
                        value.generation,
                        source.span(),
                        content_hash(b"altered source"),
                        None,
                    ));
                },
                |value: &mut ExtensionEnvelope| value.evidence.derivation.clear(),
                |value: &mut ExtensionEnvelope| {
                    value.evidence.derivation = vec![FactRef::Entity(SymbolId::from_bytes([9; 20]))]
                },
                |value: &mut ExtensionEnvelope| value.provenance = FactId::from_bytes([9; 20]),
                |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([9; 20]),
                |value: &mut ExtensionEnvelope| value.evidence.source = None,
                |value: &mut ExtensionEnvelope| value.criticality = ExtensionCriticality::Critical,
            ] {
                let mut changed = original.clone();
                mutate(&mut changed);
                assert!(decode_fsharp_binding_envelope(&changed).is_err());
            }
            let mut changed = original;
            let mut payload: serde_json::Value = serde_json::from_str(&changed.payload).unwrap();
            payload["binding"]["name"] = serde_json::json!(content_hash(b"other"));
            changed.payload = serde_json::to_string(&payload).unwrap();
            assert_eq!(
                decode_fsharp_binding_envelope(&changed),
                Err(FSharpBindingError::Envelope)
            );
            payload["binding"]["path"] = serde_json::json!([name]);
            changed.payload = serde_json::to_string(&payload).unwrap();
            assert_eq!(
                decode_fsharp_binding_envelope(&changed),
                Err(FSharpBindingError::Payload)
            );
        }
    }

    #[test]
    fn opened_name_claims_keep_exact_partial_source_without_relaxing_qualified_paths() {
        let name = content_hash(b"offset");
        let original = envelope(FSharpBinding::OpenedReference { name }).unwrap();
        let source = original.evidence.source.as_ref().unwrap();
        let scope = source.span();
        let partial = SourceRef::new(
            original.repository,
            original.generation,
            SourceSpan::new(scope.file(), 16, 24).unwrap(),
            source.content_hash(),
            None,
        );
        let encoded = new_fsharp_binding_envelope(
            original.repository,
            original.generation,
            original.provenance,
            partial.clone(),
            FSharpBindingEvidence::new(scope, FSharpBinding::OpenedReference { name }),
        )
        .unwrap();
        assert_eq!(encoded.evidence.source, Some(partial));
        assert_eq!(
            decode_fsharp_binding_envelope(&encoded).unwrap().scope(),
            scope
        );
        for binding in [
            FSharpBinding::Reference { path: vec![name] },
            FSharpBinding::Call {
                path: vec![name],
                slots: 1,
            },
            FSharpBinding::ImportedReference { path: vec![name] },
            FSharpBinding::ImportedCall {
                path: vec![name],
                slots: 1,
            },
        ] {
            assert_eq!(envelope(binding), Err(FSharpBindingError::Payload));
        }
    }

    #[test]
    fn fsharp_legacy_variants_preserve_frozen_v1_payloads_and_identity_recipe() {
        let file = FileId::from_bytes([3; 20]);
        let first = content_hash(b"Library");
        let second = content_hash(b"carry");
        let path = vec![first, second];
        let symbol = SymbolId::from_bytes([5; 20]);
        let member = FileId::from_bytes([8; 20]);
        for (binding, encoded_binding, derivation) in [
            (
                FSharpBinding::Context,
                "{\"kind\":\"context\"}".to_owned(),
                vec![FactRef::File(file)],
            ),
            (
                FSharpBinding::Export {
                    path: path.clone(),
                    symbol,
                    slots: Some(1),
                },
                format!(
                    "{{\"kind\":\"export\",\"path\":[\"{first}\",\"{second}\"],\"symbol\":\"{symbol}\",\"slots\":1}}"
                ),
                vec![FactRef::Entity(symbol)],
            ),
            (
                FSharpBinding::Reference { path: path.clone() },
                format!("{{\"kind\":\"reference\",\"path\":[\"{first}\",\"{second}\"]}}"),
                vec![FactRef::File(file)],
            ),
            (
                FSharpBinding::Call { path, slots: 1 },
                format!("{{\"kind\":\"call\",\"path\":[\"{first}\",\"{second}\"],\"slots\":1}}"),
                vec![FactRef::File(file)],
            ),
            (
                FSharpBinding::Project {
                    members: vec![member],
                },
                format!("{{\"kind\":\"project\",\"members\":[\"{member}\"]}}"),
                vec![FactRef::File(file), FactRef::File(member)],
            ),
        ] {
            let original = envelope(binding.clone()).unwrap();
            let payload = format!(
                "{{\"scope\":{{\"file\":\"{file}\",\"start_byte\":0,\"end_byte\":100}},\"binding\":{encoded_binding}}}"
            );
            let identity = serde_json::to_vec(&(
                "rootlight.fsharp_binding",
                "1",
                original.repository,
                original.generation,
                original.provenance,
                original.evidence.source.as_ref().unwrap(),
                &payload,
                &derivation,
            ))
            .unwrap();
            assert_eq!(original.version, "1");
            assert_eq!(original.payload, payload);
            assert_eq!(original.evidence.derivation, derivation);
            assert_eq!(
                original.id,
                derive_fact("rootlight.fsharp_binding/v1", &identity).id()
            );
            assert_eq!(
                decode_fsharp_binding_envelope(&original).unwrap().binding(),
                &binding
            );
            let mut wrong_version = original;
            wrong_version.version = FSHARP_IMPORT_BINDING_VERSION.to_owned();
            assert_eq!(
                decode_fsharp_binding_envelope(&wrong_version),
                Err(FSharpBindingError::Envelope)
            );
        }
    }

    #[test]
    fn fsharp_import_variants_select_and_authenticate_v2() {
        let scope = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 100).unwrap();
        let root = content_hash(b"Library");
        let path = vec![root, content_hash(b"carry")];
        for binding in [
            FSharpBinding::ImportContext {
                containers: vec![scope],
                opens: vec![],
            },
            FSharpBinding::Container {
                path: vec![root],
                roots: vec![],
            },
            FSharpBinding::Container {
                path: vec![root],
                roots: vec![content_hash(b"carry")],
            },
            FSharpBinding::Open {
                path: vec![root],
                owner: scope,
            },
            FSharpBinding::ImportedReference { path: path.clone() },
            FSharpBinding::ImportedCall { path, slots: 1 },
        ] {
            let original = envelope(binding.clone()).unwrap();
            assert_eq!(original.version, FSHARP_IMPORT_BINDING_VERSION);
            assert_eq!(
                original.evidence.derivation,
                vec![FactRef::File(scope.file())]
            );
            assert_eq!(
                decode_fsharp_binding_envelope(&original).unwrap().binding(),
                &binding
            );
            let identity = serde_json::to_vec(&(
                FSHARP_BINDING_NAMESPACE,
                "2",
                original.repository,
                original.generation,
                original.provenance,
                original.evidence.source.as_ref().unwrap(),
                &original.payload,
                &original.evidence.derivation,
            ))
            .unwrap();
            assert_eq!(
                original.id,
                derive_fact("rootlight.fsharp_binding/v2", &identity).id()
            );
            for mutate in [
                |value: &mut ExtensionEnvelope| value.version = "1".into(),
                |value: &mut ExtensionEnvelope| value.version = "3".into(),
                |value: &mut ExtensionEnvelope| value.payload.push(' '),
                |value: &mut ExtensionEnvelope| value.evidence.derivation.clear(),
                |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([9; 20]),
            ] {
                let mut changed = original.clone();
                mutate(&mut changed);
                assert_eq!(
                    decode_fsharp_binding_envelope(&changed),
                    Err(FSharpBindingError::Envelope)
                );
            }
        }
    }

    #[test]
    fn fsharp_signature_values_authenticate_v3_without_callable_slots() {
        let file = FileId::from_bytes([3; 20]);
        let first = content_hash(b"Library");
        let second = content_hash(b"carry");
        let symbol = SymbolId::from_bytes([5; 20]);
        let binding = FSharpBinding::SignatureValue {
            path: vec![first, second],
            symbol,
        };
        let original = envelope(binding.clone()).unwrap();
        let payload = format!(
            "{{\"scope\":{{\"file\":\"{file}\",\"start_byte\":0,\"end_byte\":100}},\"binding\":{{\"kind\":\"signature_value\",\"path\":[\"{first}\",\"{second}\"],\"symbol\":\"{symbol}\"}}}}"
        );
        let derivation = vec![FactRef::Entity(symbol)];
        let identity = serde_json::to_vec(&(
            FSHARP_BINDING_NAMESPACE,
            "3",
            original.repository,
            original.generation,
            original.provenance,
            original.evidence.source.as_ref().unwrap(),
            &payload,
            &derivation,
        ))
        .unwrap();
        assert_eq!(original.version, FSHARP_SIGNATURE_BINDING_VERSION);
        assert_eq!(original.payload, payload);
        assert_eq!(original.evidence.derivation, derivation);
        assert_eq!(
            original.id,
            derive_fact("rootlight.fsharp_binding/v3", &identity).id()
        );
        assert_eq!(
            decode_fsharp_binding_envelope(&original).unwrap().binding(),
            &binding
        );
        for mutate in [
            |value: &mut ExtensionEnvelope| value.version = "1".into(),
            |value: &mut ExtensionEnvelope| value.version = "2".into(),
            |value: &mut ExtensionEnvelope| value.version = "4".into(),
            |value: &mut ExtensionEnvelope| value.namespace = "unknown".into(),
            |value: &mut ExtensionEnvelope| value.criticality = ExtensionCriticality::Critical,
            |value: &mut ExtensionEnvelope| value.payload.push(' '),
            |value: &mut ExtensionEnvelope| value.provenance = FactId::from_bytes([9; 20]),
            |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([9; 20]),
            |value: &mut ExtensionEnvelope| {
                value.evidence.derivation = vec![FactRef::Fact(FactId::from_bytes([9; 20]))]
            },
            |value: &mut ExtensionEnvelope| value.evidence.source = None,
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(decode_fsharp_binding_envelope(&changed).is_err());
        }

        let mut wire: serde_json::Value = serde_json::from_str(&original.payload).unwrap();
        wire["binding"]["slots"] = serde_json::json!(1);
        let mut invented_slots = original.clone();
        invented_slots.payload = serde_json::to_string(&wire).unwrap();
        assert_eq!(
            decode_fsharp_binding_envelope(&invented_slots),
            Err(FSharpBindingError::Payload)
        );
        let mut wire: serde_json::Value = serde_json::from_str(&original.payload).unwrap();
        wire["binding"]["symbol"] = serde_json::to_value(SymbolId::from_bytes([9; 20])).unwrap();
        let mut changed_symbol = original;
        changed_symbol.payload = serde_json::to_string(&wire).unwrap();
        assert_eq!(
            decode_fsharp_binding_envelope(&changed_symbol),
            Err(FSharpBindingError::Envelope)
        );
    }

    #[test]
    fn fsharp_signature_values_keep_the_existing_path_bounds() {
        let root = content_hash(b"Library");
        let symbol = SymbolId::from_bytes([5; 20]);
        for path in [
            vec![],
            vec![root],
            vec![root; MAX_FSHARP_BINDING_COMPONENTS + 1],
        ] {
            assert_eq!(
                envelope(FSharpBinding::SignatureValue { path, symbol }),
                Err(FSharpBindingError::Payload)
            );
        }
        for count in [2, MAX_FSHARP_BINDING_COMPONENTS] {
            let binding = FSharpBinding::SignatureValue {
                path: vec![root; count],
                symbol,
            };
            let original = envelope(binding.clone()).unwrap();
            assert_eq!(
                decode_fsharp_binding_envelope(&original).unwrap().binding(),
                &binding
            );
        }
    }

    #[test]
    fn fsharp_signature_record_names_authenticate_distinct_v4_evidence() {
        let file = FileId::from_bytes([3; 20]);
        let first = content_hash(b"Library");
        let second = content_hash(b"Data");
        let symbol = SymbolId::from_bytes([5; 20]);
        let path = vec![first, second];
        let binding = FSharpBinding::SignatureType {
            path: path.clone(),
            symbol,
        };
        let original = envelope(binding.clone()).unwrap();
        let payload = format!(
            "{{\"scope\":{{\"file\":\"{file}\",\"start_byte\":0,\"end_byte\":100}},\"binding\":{{\"kind\":\"signature_type\",\"path\":[\"{first}\",\"{second}\"],\"symbol\":\"{symbol}\"}}}}"
        );
        let derivation = vec![FactRef::Entity(symbol)];
        let identity = |version: &str, payload: &str| {
            serde_json::to_vec(&(
                FSHARP_BINDING_NAMESPACE,
                version,
                original.repository,
                original.generation,
                original.provenance,
                original.evidence.source.as_ref().unwrap(),
                payload,
                &derivation,
            ))
            .unwrap()
        };
        assert_eq!(original.version, FSHARP_SIGNATURE_TYPE_BINDING_VERSION);
        assert_eq!(original.payload, payload);
        assert_eq!(original.evidence.derivation, derivation);
        assert_eq!(
            original.id,
            derive_fact("rootlight.fsharp_binding/v4", &identity("4", &payload)).id()
        );
        assert_eq!(
            decode_fsharp_binding_envelope(&original).unwrap().binding(),
            &binding
        );
        let legacy = envelope(FSharpBinding::SignatureValue { path, symbol }).unwrap();
        let legacy_payload = payload.replace("signature_type", "signature_value");
        assert_eq!(legacy.version, FSHARP_SIGNATURE_BINDING_VERSION);
        assert_eq!(legacy.payload, legacy_payload);
        assert_eq!(
            legacy.id,
            derive_fact(
                "rootlight.fsharp_binding/v3",
                &identity("3", &legacy_payload)
            )
            .id()
        );
        assert_ne!(legacy.id, original.id);
        let mut disguised = original.clone();
        disguised.payload = legacy_payload;
        disguised.id = derive_fact(
            "rootlight.fsharp_binding/v4",
            &identity("4", &disguised.payload),
        )
        .id();
        assert_eq!(
            decode_fsharp_binding_envelope(&disguised),
            Err(FSharpBindingError::Envelope)
        );
        for mutate in [
            |value: &mut ExtensionEnvelope| value.version = "1".into(),
            |value: &mut ExtensionEnvelope| value.version = "2".into(),
            |value: &mut ExtensionEnvelope| value.version = "3".into(),
            |value: &mut ExtensionEnvelope| value.version = "5".into(),
            |value: &mut ExtensionEnvelope| value.payload.push(' '),
            |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([9; 20]),
            |value: &mut ExtensionEnvelope| {
                value.evidence.derivation = vec![FactRef::Fact(FactId::from_bytes([9; 20]))]
            },
            |value: &mut ExtensionEnvelope| value.evidence.source = None,
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(decode_fsharp_binding_envelope(&changed).is_err());
        }
        for field in ["slots", "fields", "type"] {
            let mut wire: serde_json::Value = serde_json::from_str(&original.payload).unwrap();
            wire["binding"][field] = serde_json::json!(1);
            let mut inferred = original.clone();
            inferred.payload = serde_json::to_string(&wire).unwrap();
            assert_eq!(
                decode_fsharp_binding_envelope(&inferred),
                Err(FSharpBindingError::Payload)
            );
        }
    }

    #[test]
    fn fsharp_signature_record_names_keep_the_existing_path_bounds() {
        let root = content_hash(b"Library");
        let symbol = SymbolId::from_bytes([5; 20]);
        for path in [
            vec![],
            vec![root],
            vec![root; MAX_FSHARP_BINDING_COMPONENTS + 1],
        ] {
            assert_eq!(
                envelope(FSharpBinding::SignatureType { path, symbol }),
                Err(FSharpBindingError::Payload)
            );
        }
        for count in [2, MAX_FSHARP_BINDING_COMPONENTS] {
            let binding = FSharpBinding::SignatureType {
                path: vec![root; count],
                symbol,
            };
            let original = envelope(binding.clone()).unwrap();
            assert_eq!(
                decode_fsharp_binding_envelope(&original).unwrap().binding(),
                &binding
            );
        }
    }

    #[test]
    fn fsharp_signature_alias_names_authenticate_distinct_v5_evidence() {
        let file = FileId::from_bytes([3; 20]);
        let first = content_hash(b"Library");
        let second = content_hash(b"Alias");
        let symbol = SymbolId::from_bytes([5; 20]);
        let path = vec![first, second];
        let binding = FSharpBinding::SignatureAlias {
            path: path.clone(),
            symbol,
        };
        let original = envelope(binding.clone()).unwrap();
        let payload = format!(
            "{{\"scope\":{{\"file\":\"{file}\",\"start_byte\":0,\"end_byte\":100}},\"binding\":{{\"kind\":\"signature_alias\",\"path\":[\"{first}\",\"{second}\"],\"symbol\":\"{symbol}\"}}}}"
        );
        let derivation = vec![FactRef::Entity(symbol)];
        let identity = |version: &str, payload: &str| {
            serde_json::to_vec(&(
                FSHARP_BINDING_NAMESPACE,
                version,
                original.repository,
                original.generation,
                original.provenance,
                original.evidence.source.as_ref().unwrap(),
                payload,
                &derivation,
            ))
            .unwrap()
        };
        assert_eq!(original.version, FSHARP_SIGNATURE_ALIAS_BINDING_VERSION);
        assert_eq!(original.payload, payload);
        assert_eq!(original.evidence.derivation, derivation);
        assert_eq!(
            original.id,
            derive_fact("rootlight.fsharp_binding/v5", &identity("5", &payload)).id()
        );
        assert_eq!(
            decode_fsharp_binding_envelope(&original).unwrap().binding(),
            &binding
        );
        let record = envelope(FSharpBinding::SignatureType { path, symbol }).unwrap();
        let record_payload = payload.replace("signature_alias", "signature_type");
        assert_eq!(record.version, FSHARP_SIGNATURE_TYPE_BINDING_VERSION);
        assert_eq!(record.payload, record_payload);
        assert_eq!(
            record.id,
            derive_fact(
                "rootlight.fsharp_binding/v4",
                &identity("4", &record_payload)
            )
            .id()
        );
        assert_ne!(record.id, original.id);
        let mut disguised = original.clone();
        disguised.payload = record_payload;
        disguised.id = derive_fact(
            "rootlight.fsharp_binding/v5",
            &identity("5", &disguised.payload),
        )
        .id();
        assert_eq!(
            decode_fsharp_binding_envelope(&disguised),
            Err(FSharpBindingError::Envelope)
        );
        for mutate in [
            |value: &mut ExtensionEnvelope| value.version = "1".into(),
            |value: &mut ExtensionEnvelope| value.version = "2".into(),
            |value: &mut ExtensionEnvelope| value.version = "3".into(),
            |value: &mut ExtensionEnvelope| value.version = "4".into(),
            |value: &mut ExtensionEnvelope| value.namespace = "unknown".into(),
            |value: &mut ExtensionEnvelope| value.criticality = ExtensionCriticality::Critical,
            |value: &mut ExtensionEnvelope| value.payload.push(' '),
            |value: &mut ExtensionEnvelope| value.provenance = FactId::from_bytes([9; 20]),
            |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([9; 20]),
            |value: &mut ExtensionEnvelope| {
                value.evidence.derivation = vec![FactRef::File(FileId::from_bytes([9; 20]))]
            },
            |value: &mut ExtensionEnvelope| value.evidence.source = None,
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(decode_fsharp_binding_envelope(&changed).is_err());
        }
        for field in ["slots", "target", "type"] {
            let mut wire: serde_json::Value = serde_json::from_str(&original.payload).unwrap();
            wire["binding"][field] = serde_json::json!(1);
            let mut inferred = original.clone();
            inferred.payload = serde_json::to_string(&wire).unwrap();
            assert_eq!(
                decode_fsharp_binding_envelope(&inferred),
                Err(FSharpBindingError::Payload)
            );
        }
        let mut wire: serde_json::Value = serde_json::from_str(&original.payload).unwrap();
        wire["binding"]["symbol"] = serde_json::to_value(SymbolId::from_bytes([9; 20])).unwrap();
        let mut changed_symbol = original;
        changed_symbol.payload = serde_json::to_string(&wire).unwrap();
        assert_eq!(
            decode_fsharp_binding_envelope(&changed_symbol),
            Err(FSharpBindingError::Envelope)
        );
    }

    #[test]
    fn fsharp_signature_alias_names_keep_existing_path_and_source_bounds() {
        let root = content_hash(b"Library");
        let symbol = SymbolId::from_bytes([5; 20]);
        for path in [
            vec![],
            vec![root],
            vec![root; MAX_FSHARP_BINDING_COMPONENTS + 1],
        ] {
            assert_eq!(
                envelope(FSharpBinding::SignatureAlias { path, symbol }),
                Err(FSharpBindingError::Payload)
            );
        }
        for count in [2, MAX_FSHARP_BINDING_COMPONENTS] {
            let binding = FSharpBinding::SignatureAlias {
                path: vec![root; count],
                symbol,
            };
            let original = envelope(binding.clone()).unwrap();
            assert_eq!(
                decode_fsharp_binding_envelope(&original).unwrap().binding(),
                &binding
            );
            let source = original.evidence.source.clone().unwrap();
            let scope = source.span();
            let partial = SourceRef::new(
                source.repository(),
                source.generation(),
                SourceSpan::new(scope.file(), 16, 24).unwrap(),
                source.content_hash(),
                None,
            );
            assert!(
                new_fsharp_binding_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    partial,
                    FSharpBindingEvidence::new(scope, binding.clone()),
                )
                .is_ok()
            );
            assert_eq!(
                new_fsharp_binding_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    source,
                    FSharpBindingEvidence::new(
                        SourceSpan::new(scope.file(), 0, 99).unwrap(),
                        binding,
                    ),
                ),
                Err(FSharpBindingError::Source)
            );
        }
    }

    #[test]
    fn fsharp_import_paths_roots_and_slots_keep_closed_bounds() {
        let file = FileId::from_bytes([3; 20]);
        let owner = SourceSpan::new(file, 0, 100).unwrap();
        let root = content_hash(b"Library");
        for path in [vec![], vec![root; MAX_FSHARP_BINDING_COMPONENTS + 1]] {
            for binding in [
                FSharpBinding::Container {
                    path: path.clone(),
                    roots: vec![],
                },
                FSharpBinding::Open {
                    path: path.clone(),
                    owner,
                },
                FSharpBinding::ImportedReference { path: path.clone() },
                FSharpBinding::ImportedCall {
                    path: path.clone(),
                    slots: 1,
                },
            ] {
                assert_eq!(envelope(binding), Err(FSharpBindingError::Payload));
            }
        }
        for binding in [
            FSharpBinding::ImportedReference { path: vec![root] },
            FSharpBinding::ImportedCall {
                path: vec![root],
                slots: 1,
            },
            FSharpBinding::ImportedCall {
                path: vec![root, root],
                slots: 0,
            },
        ] {
            assert_eq!(envelope(binding), Err(FSharpBindingError::Payload));
        }
        let maximum = vec![root; MAX_FSHARP_BINDING_COMPONENTS];
        for binding in [
            FSharpBinding::Container {
                path: maximum.clone(),
                roots: vec![],
            },
            FSharpBinding::Open {
                path: maximum.clone(),
                owner,
            },
            FSharpBinding::ImportedReference {
                path: maximum.clone(),
            },
            FSharpBinding::ImportedCall {
                path: maximum,
                slots: 1,
            },
        ] {
            assert!(envelope(binding).is_ok());
        }
        let low = ContentHash::from_bytes([1; 32]);
        let high = ContentHash::from_bytes([2; 32]);
        for roots in [vec![low, low], vec![high, low]] {
            assert_eq!(
                envelope(FSharpBinding::Container {
                    path: vec![root],
                    roots
                }),
                Err(FSharpBindingError::Payload)
            );
        }
        let original = envelope(FSharpBinding::Container {
            path: vec![root],
            roots: vec![low, high],
        })
        .unwrap();
        let mut missing_roots = original.clone();
        let mut wire: serde_json::Value = serde_json::from_str(&original.payload).unwrap();
        wire["binding"].as_object_mut().unwrap().remove("roots");
        missing_roots.payload = serde_json::to_string(&wire).unwrap();
        assert_eq!(
            decode_fsharp_binding_envelope(&missing_roots),
            Err(FSharpBindingError::Payload)
        );

        let encoded_width = serde_json::to_string(&low).unwrap().len() + 1;
        let count = MAX_FSHARP_BINDING_PAYLOAD_BYTES / encoded_width + 1;
        let roots = (0..count)
            .map(|ordinal| {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&u64::try_from(ordinal).unwrap().to_be_bytes());
                ContentHash::from_bytes(bytes)
            })
            .collect();
        assert_eq!(
            envelope(FSharpBinding::Container {
                path: vec![root],
                roots
            }),
            Err(FSharpBindingError::Payload)
        );
    }

    #[test]
    fn fsharp_open_owner_contains_the_exact_nonempty_claim_within_scope() {
        let original = envelope(FSharpBinding::Context).unwrap();
        let scope = original.evidence.source.as_ref().unwrap().span();
        let claim = SourceSpan::new(scope.file(), 20, 40).unwrap();
        let build = |span, owner| {
            new_fsharp_binding_envelope(
                original.repository,
                original.generation,
                original.provenance,
                SourceRef::new(
                    original.repository,
                    original.generation,
                    span,
                    content_hash(b"source"),
                    None,
                ),
                FSharpBindingEvidence::new(
                    scope,
                    FSharpBinding::Open {
                        path: vec![content_hash(b"Library")],
                        owner,
                    },
                ),
            )
        };
        for owner in [scope, claim, SourceSpan::new(scope.file(), 10, 90).unwrap()] {
            let valid = build(claim, owner).unwrap();
            assert!(decode_fsharp_binding_envelope(&valid).is_ok());
        }
        for owner in [
            SourceSpan::new(FileId::from_bytes([8; 20]), 0, 100).unwrap(),
            SourceSpan::new(scope.file(), 0, 101).unwrap(),
            SourceSpan::new(scope.file(), 21, 40).unwrap(),
            SourceSpan::new(scope.file(), 20, 39).unwrap(),
            SourceSpan::new(scope.file(), 20, 20).unwrap(),
        ] {
            assert_eq!(build(claim, owner), Err(FSharpBindingError::Source));
        }
        assert_eq!(
            build(SourceSpan::new(scope.file(), 20, 20).unwrap(), scope),
            Err(FSharpBindingError::Source)
        );
    }

    #[test]
    fn fsharp_import_context_requires_complete_canonical_span_inventories() {
        let file = FileId::from_bytes([3; 20]);
        let scope = SourceSpan::new(file, 0, 100).unwrap();
        let first = SourceSpan::new(file, 10, 20).unwrap();
        let second = SourceSpan::new(file, 30, 40).unwrap();
        let original = envelope(FSharpBinding::ImportContext {
            containers: vec![scope],
            opens: vec![first, second],
        })
        .unwrap();
        assert!(decode_fsharp_binding_envelope(&original).is_ok());
        for field in ["containers", "opens"] {
            let mut missing = original.clone();
            let mut wire: serde_json::Value = serde_json::from_str(&missing.payload).unwrap();
            wire["binding"].as_object_mut().unwrap().remove(field);
            missing.payload = serde_json::to_string(&wire).unwrap();
            assert_eq!(
                decode_fsharp_binding_envelope(&missing),
                Err(FSharpBindingError::Payload)
            );
        }
        for (containers, opens) in [
            (vec![], vec![]),
            (vec![scope, scope], vec![]),
            (vec![second, first], vec![]),
            (vec![scope], vec![first, first]),
            (vec![scope], vec![second, first]),
        ] {
            assert_eq!(
                envelope(FSharpBinding::ImportContext { containers, opens }),
                Err(FSharpBindingError::Payload)
            );
        }
        for span in [
            SourceSpan::new(FileId::from_bytes([8; 20]), 0, 100).unwrap(),
            SourceSpan::new(file, 10, 10).unwrap(),
            SourceSpan::new(file, 90, 101).unwrap(),
        ] {
            for binding in [
                FSharpBinding::ImportContext {
                    containers: vec![span],
                    opens: vec![],
                },
                FSharpBinding::ImportContext {
                    containers: vec![scope],
                    opens: vec![span],
                },
            ] {
                assert_eq!(envelope(binding), Err(FSharpBindingError::Source));
            }
        }
        assert_eq!(
            new_fsharp_binding_envelope(
                original.repository,
                original.generation,
                original.provenance,
                SourceRef::new(
                    original.repository,
                    original.generation,
                    first,
                    content_hash(b"source"),
                    None
                ),
                FSharpBindingEvidence::new(
                    scope,
                    FSharpBinding::ImportContext {
                        containers: vec![scope],
                        opens: vec![]
                    }
                ),
            ),
            Err(FSharpBindingError::Source)
        );
    }

    #[test]
    fn fsharp_binding_paths_and_project_members_keep_closed_bounds() {
        for path in [
            vec![],
            vec![content_hash(b"x")],
            vec![content_hash(b"x"); MAX_FSHARP_BINDING_COMPONENTS + 1],
        ] {
            assert_eq!(
                envelope(FSharpBinding::Reference { path }),
                Err(FSharpBindingError::Payload)
            );
        }
        let member = FileId::from_bytes([8; 20]);
        for members in [
            vec![],
            vec![member, member],
            vec![FileId::from_bytes([3; 20])],
            vec![member; MAX_FSHARP_PROJECT_MEMBERS + 1],
        ] {
            assert_eq!(
                envelope(FSharpBinding::Project { members }),
                Err(FSharpBindingError::Payload)
            );
        }
        let original = envelope(FSharpBinding::Project {
            members: vec![member, FileId::from_bytes([9; 20])],
        })
        .unwrap();
        assert_eq!(
            original.evidence.derivation,
            vec![
                FactRef::File(FileId::from_bytes([3; 20])),
                FactRef::File(member),
                FactRef::File(FileId::from_bytes([9; 20]))
            ]
        );
        assert!(decode_fsharp_binding_envelope(&original).is_ok());
    }

    #[test]
    fn fsharp_binding_source_and_scope_require_exact_identity() {
        let original = envelope(FSharpBinding::Context).unwrap();
        let source = original.evidence.source.clone().unwrap();
        for scope in [
            SourceSpan::new(source.span().file(), 1, 100).unwrap(),
            SourceSpan::new(source.span().file(), 0, 99).unwrap(),
            SourceSpan::new(FileId::from_bytes([8; 20]), 0, 100).unwrap(),
        ] {
            assert_eq!(
                new_fsharp_binding_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    source.clone(),
                    FSharpBindingEvidence::new(scope, FSharpBinding::Context)
                ),
                Err(FSharpBindingError::Source)
            );
        }
        assert_eq!(
            new_fsharp_binding_envelope(
                original.repository,
                GenerationId::from_bytes([9; 20]),
                original.provenance,
                source.clone(),
                FSharpBindingEvidence::new(source.span(), FSharpBinding::Context)
            ),
            Err(FSharpBindingError::Source)
        );
    }

    #[test]
    fn fsharp_project_order_is_independent_of_canonical_derivation_order() {
        let first = FileId::from_bytes([9; 20]);
        let second = FileId::from_bytes([8; 20]);
        let binding = FSharpBinding::Project {
            members: vec![first, second],
        };
        let original = envelope(binding.clone()).unwrap();
        assert_eq!(
            original.evidence.derivation,
            vec![
                FactRef::File(FileId::from_bytes([3; 20])),
                FactRef::File(second),
                FactRef::File(first)
            ],
        );
        assert_eq!(
            decode_fsharp_binding_envelope(&original).unwrap().binding(),
            &binding
        );
        let reordered = envelope(FSharpBinding::Project {
            members: vec![second, first],
        })
        .unwrap();
        assert_eq!(reordered.evidence.derivation, original.evidence.derivation);
        assert_ne!(reordered.payload, original.payload);
        assert_ne!(reordered.id, original.id);
    }
}
