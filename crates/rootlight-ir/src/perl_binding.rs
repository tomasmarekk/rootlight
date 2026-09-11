//! Source-bound Perl package-storage evidence for project composition.
//! Payloads contain stable hashes and symbols, never occurrence IDs, so a
//! generation change can rebind the envelope without guessing hidden identities.

use rootlight_ids::{ContentHash, FactId, GenerationId, RepositoryId, SymbolId, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef, SourceSpan,
};

/// Namespace for native Perl package-binding evidence.
pub const PERL_BINDING_NAMESPACE: &str = "rootlight.perl_binding";
/// Frozen payload and envelope version.
pub const PERL_BINDING_VERSION: &str = "1";
/// Maximum encoded bytes for one fixed-shape binding record.
pub const MAX_PERL_BINDING_PAYLOAD_BYTES: usize = 2 * 1024;

/// Canonical package CODE storage, independent of a particular source file.
///
/// Producers hash the canonical package and leaf separately. Neither digest
/// proves module loading or the runtime value currently occupying the slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PerlCallableStorage {
    /// Hash of the language-canonical package name.
    pub package: ContentHash,
    /// Hash of the callable's leaf name, without a sigil.
    pub name: ContentHash,
}

/// Native evidence classes retained for Perl project binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PerlBinding {
    /// A complete native capture context, not a claim of runtime completeness.
    ModuleContext,
    /// A written declaration assigned to package CODE storage.
    Definition {
        /// Canonical package storage assigned by language-owned lowering.
        storage: PerlCallableStorage,
        /// Stable entity identity of the declaration.
        symbol: SymbolId,
    },
    /// A proven invocation of package CODE storage.
    Call {
        /// Storage selected by native lexical and package binding rules.
        storage: PerlCallableStorage,
    },
    /// A native write or localization that may replace this CODE slot.
    Write {
        /// Storage affected by the written operation.
        storage: PerlCallableStorage,
    },
    /// A dynamic effect whose package CODE target cannot be established.
    DynamicWrite,
    /// A native use-module field; this does not prove loader search paths.
    ModuleLoad {
        /// Hash of the language-canonical package name.
        package: ContentHash,
    },
}

/// A binding claim scoped to one native module root, including embedded roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct PerlBindingEvidence {
    module: SourceSpan,
    binding: PerlBinding,
}

impl PerlBindingEvidence {
    /// Associates native evidence with its source-file module root.
    #[must_use]
    pub const fn new(module: SourceSpan, binding: PerlBinding) -> Self {
        Self { module, binding }
    }

    /// Returns the native source module boundary, not a guessed package path.
    #[must_use]
    pub const fn module(self) -> SourceSpan {
        self.module
    }

    /// Returns the source producer's typed binding claim.
    #[must_use]
    pub const fn binding(self) -> PerlBinding {
        self.binding
    }
}

/// Invalid, oversized or inconsistent package-binding evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PerlBindingError {
    /// The bounded canonical payload contract was violated.
    #[error("invalid perl binding payload")]
    Payload,
    /// Source ownership or native module containment was violated.
    #[error("invalid perl binding source ownership")]
    Source,
    /// The envelope identity, version, criticality or derivation was altered.
    #[error("invalid perl binding envelope")]
    Envelope,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEvidence {
    module: SourceSpan,
    binding: PerlBinding,
}

/// Builds a canonical source-bound package-binding envelope.
///
/// # Errors
/// Rejects source outside the native module, mismatched repository/generation,
/// or an oversized or unencodable payload. Definitions derive from their stable
/// entity; other claims derive only from the owning file, not mutable site IDs.
pub fn new_perl_binding_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    evidence: PerlBindingEvidence,
) -> Result<ExtensionEnvelope, PerlBindingError> {
    let span = source.span();
    let module = evidence.module;
    if source.repository() != repository
        || source.generation() != generation
        || span.file() != module.file()
        || span.start_byte() < module.start_byte()
        || span.end_byte() > module.end_byte()
        || (matches!(evidence.binding, PerlBinding::ModuleContext) && span != module)
    {
        return Err(PerlBindingError::Source);
    }
    let payload = serde_json::to_string(&evidence).map_err(|_| PerlBindingError::Payload)?;
    if payload.len() > MAX_PERL_BINDING_PAYLOAD_BYTES {
        return Err(PerlBindingError::Payload);
    }
    let subject = match evidence.binding {
        PerlBinding::Definition { symbol, .. } => FactRef::Entity(symbol),
        _ => FactRef::File(span.file()),
    };
    let identity = serde_json::to_vec(&(
        PERL_BINDING_NAMESPACE,
        PERL_BINDING_VERSION,
        repository,
        generation,
        provenance,
        &source,
        &payload,
        subject,
    ))
    .map_err(|_| PerlBindingError::Payload)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.perl_binding/v1", &identity).id(),
        repository,
        generation,
        namespace: PERL_BINDING_NAMESPACE.to_owned(),
        version: PERL_BINDING_VERSION.to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload,
        provenance,
        evidence: FactEvidence {
            source: Some(source),
            derivation: vec![subject],
        },
    })
}

/// Decodes a bounded canonical envelope and verifies all source/identity links.
///
/// # Errors
/// Rejects malformed, oversized, noncanonical, foreign-version or altered
/// envelopes. Source-backed truth still requires trusting the native producer;
/// matching names alone cannot create this evidence.
pub fn decode_perl_binding_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<PerlBindingEvidence, PerlBindingError> {
    if envelope.namespace != PERL_BINDING_NAMESPACE
        || envelope.version != PERL_BINDING_VERSION
        || envelope.criticality != ExtensionCriticality::Noncritical
    {
        return Err(PerlBindingError::Envelope);
    }
    if envelope.payload.len() > MAX_PERL_BINDING_PAYLOAD_BYTES {
        return Err(PerlBindingError::Payload);
    }
    let wire: WireEvidence =
        serde_json::from_str(&envelope.payload).map_err(|_| PerlBindingError::Payload)?;
    let evidence = PerlBindingEvidence::new(wire.module, wire.binding);
    let expected = new_perl_binding_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope
            .evidence
            .source
            .clone()
            .ok_or(PerlBindingError::Source)?,
        evidence,
    )?;
    if expected != *envelope {
        return Err(PerlBindingError::Envelope);
    }
    Ok(evidence)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ids::{FileId, content_hash};

    fn envelope() -> ExtensionEnvelope {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let module = SourceSpan::new(FileId::from_bytes([3; 20]), 0, 100).unwrap();
        let source = SourceRef::new(
            repository,
            generation,
            module,
            content_hash(b"source"),
            None,
        );
        new_perl_binding_envelope(
            repository,
            generation,
            FactId::from_bytes([4; 20]),
            source,
            PerlBindingEvidence::new(module, PerlBinding::ModuleContext),
        )
        .unwrap()
    }

    #[test]
    fn perl_binding_envelope_rejects_mutation_and_noncanonical_payloads() {
        let original = envelope();
        assert!(decode_perl_binding_envelope(&original).is_ok());
        for mutate in [
            |value: &mut ExtensionEnvelope| value.version = "2".into(),
            |value: &mut ExtensionEnvelope| value.namespace = "unknown".into(),
            |value: &mut ExtensionEnvelope| value.criticality = ExtensionCriticality::Critical,
            |value: &mut ExtensionEnvelope| value.payload.push(' '),
            |value: &mut ExtensionEnvelope| {
                value.payload = " ".repeat(MAX_PERL_BINDING_PAYLOAD_BYTES + 1)
            },
            |value: &mut ExtensionEnvelope| value.evidence.derivation.clear(),
            |value: &mut ExtensionEnvelope| value.evidence.source = None,
            |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([9; 20]),
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(decode_perl_binding_envelope(&changed).is_err());
        }
    }

    #[test]
    fn perl_binding_source_must_belong_to_its_native_module_and_generation() {
        let original = envelope();
        let source = original.evidence.source.clone().unwrap();
        for module in [
            SourceSpan::new(source.span().file(), 1, 100).unwrap(),
            SourceSpan::new(source.span().file(), 0, 99).unwrap(),
            SourceSpan::new(FileId::from_bytes([8; 20]), 0, 100).unwrap(),
        ] {
            assert_eq!(
                new_perl_binding_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    source.clone(),
                    PerlBindingEvidence::new(module, PerlBinding::DynamicWrite)
                ),
                Err(PerlBindingError::Source)
            );
        }
        assert_eq!(
            new_perl_binding_envelope(
                original.repository,
                GenerationId::from_bytes([9; 20]),
                original.provenance,
                source.clone(),
                PerlBindingEvidence::new(source.span(), PerlBinding::ModuleContext)
            ),
            Err(PerlBindingError::Source)
        );
    }
}
