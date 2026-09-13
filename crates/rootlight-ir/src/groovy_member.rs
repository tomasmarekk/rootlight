//! Native Groovy field and property headers, bound to authored declarators.
//! A property identifies its source declaration, not a synthesized getter or field.

use rootlight_ids::{FactId, GenerationId, RepositoryId, derive_fact};
use serde::{Deserialize, Serialize};

use crate::{
    EntityVisibility, ExtensionCriticality, ExtensionEnvelope, FactEvidence, FactRef, SourceRef,
    SourceSpan,
};

/// Namespace for source-owned Groovy field and property storage evidence.
pub const GROOVY_MEMBER_NAMESPACE: &str = "rootlight.groovy_member";

/// Storage established by a native, untransformed declaration header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GroovyMemberStorage {
    /// Static storage, including implicit interface constants.
    Static,
    /// Instance storage without a static modifier.
    Instance,
    /// A transform or inconsistent header prevents a storage assertion.
    Unknown,
}

/// Authored member identity and modifiers within one native field statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroovyMember {
    /// Complete native enclosing type declaration, excluding lexical outer types.
    pub owner: SourceSpan,
    /// One complete declarator, excluding sibling declarators in the statement.
    pub declaration: SourceSpan,
    /// Exact identifier within that declarator.
    pub name: SourceSpan,
    /// Source-established static or instance storage, or an explicit unknown.
    pub storage: GroovyMemberStorage,
    /// Access to the authored field or property, not its generated backing field.
    pub visibility: EntityVisibility,
}

/// Malformed or inconsistent native member evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("invalid groovy member evidence")]
pub struct GroovyMemberError;

/// Encodes a member within its full declaration statement, including modifiers.
///
/// # Errors
/// Rejects foreign source identity and empty or noncontained
/// spans. Native ownership and complete parser coverage are producer obligations.
pub fn new_groovy_member_envelope(
    repository: RepositoryId,
    generation: GenerationId,
    provenance: FactId,
    source: SourceRef,
    member: GroovyMember,
) -> Result<ExtensionEnvelope, GroovyMemberError> {
    if source.repository() != repository
        || source.generation() != generation
        || !contains(member.owner, source.span())
        || !contains(source.span(), member.declaration)
        || !contains(member.declaration, member.name)
    {
        return Err(GroovyMemberError);
    }
    let payload = serde_json::to_string(&member).map_err(|_| GroovyMemberError)?;
    let subject = FactRef::File(source.span().file());
    let identity = serde_json::to_vec(&(
        GROOVY_MEMBER_NAMESPACE,
        "1",
        repository,
        generation,
        provenance,
        &source,
        &payload,
        subject,
    ))
    .map_err(|_| GroovyMemberError)?;
    Ok(ExtensionEnvelope {
        id: derive_fact("rootlight.groovy_member/v1", &identity).id(),
        repository,
        generation,
        namespace: GROOVY_MEMBER_NAMESPACE.to_owned(),
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

/// Decodes bounded member metadata and verifies canonical source and fact identity.
///
/// # Errors
/// Rejects oversized payloads, unknown versions, malformed spans and any altered
/// or noncanonical envelope. Integrity does not authenticate language semantics.
pub fn decode_groovy_member_envelope(
    envelope: &ExtensionEnvelope,
) -> Result<GroovyMember, GroovyMemberError> {
    if envelope.namespace != GROOVY_MEMBER_NAMESPACE || envelope.payload.len() > 1024 {
        return Err(GroovyMemberError);
    }
    let member = serde_json::from_str(&envelope.payload).map_err(|_| GroovyMemberError)?;
    let expected = new_groovy_member_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope.evidence.source.clone().ok_or(GroovyMemberError)?,
        member,
    )?;
    if expected != *envelope {
        return Err(GroovyMemberError);
    }
    Ok(member)
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
        new_groovy_member_envelope(
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
            GroovyMember {
                owner: SourceSpan::new(file, 0, 50).unwrap(),
                declaration: SourceSpan::new(file, 10, 25).unwrap(),
                name: SourceSpan::new(file, 10, 15).unwrap(),
                storage: GroovyMemberStorage::Static,
                visibility: EntityVisibility::Public,
            },
        )
        .unwrap()
    }

    #[test]
    fn member_metadata_round_trips_and_rejects_changed_identity() {
        let original = envelope();
        assert_eq!(
            decode_groovy_member_envelope(&original).unwrap().storage,
            GroovyMemberStorage::Static
        );
        for mutate in [
            |value: &mut ExtensionEnvelope| value.payload.push(' '),
            |value: &mut ExtensionEnvelope| value.version = "2".into(),
            |value: &mut ExtensionEnvelope| value.id = FactId::from_bytes([99; 20]),
            |value: &mut ExtensionEnvelope| value.provenance = FactId::from_bytes([99; 20]),
            |value: &mut ExtensionEnvelope| value.evidence.derivation.clear(),
            |value: &mut ExtensionEnvelope| value.payload = "x".repeat(1025),
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert!(decode_groovy_member_envelope(&changed).is_err());
        }
    }

    #[test]
    fn member_names_and_declarators_must_belong_to_the_statement() {
        let original = envelope();
        let member = decode_groovy_member_envelope(&original).unwrap();
        let file = member.name.file();
        for owner in [
            SourceSpan::new(file, 1, 50).unwrap(),
            SourceSpan::new(file, 0, 39).unwrap(),
            SourceSpan::new(FileId::from_bytes([9; 20]), 0, 50).unwrap(),
        ] {
            assert!(
                new_groovy_member_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    original.evidence.source.clone().unwrap(),
                    GroovyMember { owner, ..member }
                )
                .is_err()
            );
        }
        for name in [
            SourceSpan::new(file, 9, 15).unwrap(),
            SourceSpan::new(file, 24, 26).unwrap(),
            SourceSpan::new(file, 12, 12).unwrap(),
            SourceSpan::new(FileId::from_bytes([9; 20]), 10, 15).unwrap(),
        ] {
            assert!(
                new_groovy_member_envelope(
                    original.repository,
                    original.generation,
                    original.provenance,
                    original.evidence.source.clone().unwrap(),
                    GroovyMember { name, ..member }
                )
                .is_err()
            );
        }
        assert!(
            new_groovy_member_envelope(
                original.repository,
                original.generation,
                original.provenance,
                original.evidence.source.clone().unwrap(),
                GroovyMember {
                    declaration: SourceSpan::new(file, 10, 41).unwrap(),
                    ..member
                }
            )
            .is_err()
        );
        assert!(
            new_groovy_member_envelope(
                original.repository,
                GenerationId::from_bytes([9; 20]),
                original.provenance,
                original.evidence.source.clone().unwrap(),
                member
            )
            .is_err()
        );
    }
}
