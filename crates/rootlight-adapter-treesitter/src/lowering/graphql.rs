//! GraphQL type links are unique written names within one physical document.
//! Schema merging, validation, resolver dispatch and operation execution stay gaps.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    EntityKind, FactDomain, FactRef, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    RelationEndpoint, RelationPredicate, RelationRecord,
};

use super::{
    MaterializedEntity, derive_relation_record_id, lexical_reference_relation, provider_failure,
};

/// Publishes a field type link only from an admitted, resolved written type use.
/// The lexical occurrence remains a separate fact; proof parents are canonical
/// before identity derivation so normalization cannot invalidate the relation.
pub(super) fn field_type_relation(
    fact: &SyntaxFact,
    occurrence: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    symbol: SymbolId,
    cancellation: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancellation.check()?;
    let Some(owner) = owner.filter(|owner| {
        owner.record.kind == EntityKind::Field && owner.record.language == "graphql"
    }) else {
        return Ok(None);
    };
    if fact.kind() != SyntaxFactKind::Occurrence
        || fact.syntax_kind().as_str() != "graphql.named_type.reference"
        || occurrence.role != OccurrenceRole::TypeUse
        || occurrence.enclosing != Some(owner.record.id)
        || occurrence.target != (OccurrenceTarget::Resolved { symbol })
    {
        return Ok(None);
    }
    let mut relation = lexical_reference_relation(occurrence, symbol)?;
    relation.subject = RelationEndpoint::Entity(owner.record.id);
    relation.predicate = RelationPredicate::UsesType;
    relation
        .evidence
        .derivation
        .try_reserve_exact(3)
        .map_err(|_| SinkError::AllocationFailed)?;
    relation.evidence.derivation.extend([
        FactRef::Entity(owner.record.id),
        FactRef::Fact(occurrence.id),
        FactRef::Entity(symbol),
    ]);
    relation.evidence.derivation.sort_unstable();
    relation.evidence.derivation.dedup();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("treesitter-relation-identity"))?;
    cancellation.check()?;
    Ok(Some(relation))
}

/// Indexes unique admitted type owners while withholding unavailable declarations.
///
/// The caller must first prove a complete standalone UTF-8 fact frame.
///
/// # Errors
/// Returns cooperative cancellation or fallible index allocation errors.
pub(super) fn type_targets<'a>(
    entities: &'a HashMap<u64, MaterializedEntity>,
    facts: &[SyntaxFact],
    cancellation: &Cancellation,
) -> Result<HashMap<&'a str, Option<SymbolId>>, AdapterError> {
    cancellation.check()?;
    let mut targets = HashMap::new();
    // A withheld owner can hide a duplicate even when native parsing succeeded.
    for fact in facts {
        cancellation.check()?;
        if fact.syntax_kind().as_str() == "graphql.declaration_unavailable.signature" {
            return Ok(targets);
        }
    }
    targets
        .try_reserve(entities.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for entity in entities.values() {
        cancellation.check()?;
        let record = &entity.record;
        if record.language == "graphql"
            && matches!(
                record.kind,
                EntityKind::Struct
                    | EntityKind::Interface
                    | EntityKind::Enum
                    | EntityKind::Union
                    | EntityKind::TypeAlias
            )
        {
            // Count authored owners, even when identity coalesces duplicate names.
            targets
                .entry(record.canonical_name.as_str())
                .and_modify(|target| *target = None)
                .or_insert(Some(record.id));
        }
    }
    Ok(targets)
}

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (
            EntityKind::Struct,
            "graphql.object.declaration" | "graphql.input.declaration"
        ) | (EntityKind::Interface, "graphql.interface.declaration")
            | (EntityKind::Enum, "graphql.enum.declaration")
            | (EntityKind::Union, "graphql.union.declaration")
            | (EntityKind::TypeAlias, "graphql.scalar.declaration")
            | (
                EntityKind::Field,
                "graphql.field.declaration" | "graphql.input_field.declaration"
            )
            | (EntityKind::Parameter, "graphql.argument.declaration")
            | (EntityKind::Constant, "graphql.enum_value.declaration")
            | (EntityKind::Variable, "graphql.variable.declaration")
            | (
                EntityKind::Function,
                "graphql.operation.declaration"
                    | "graphql.fragment.declaration"
                    | "graphql.directive.declaration"
            )
    )
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "graphql.file.module") => Some((
            FactDomain::Relations,
            "graphql-schema-validation-merging-and-execution-unavailable",
        )),
        (SyntaxFactKind::Signature, "graphql.extension_unavailable.signature") => Some((
            FactDomain::Entities,
            "graphql-extension-lowering-unavailable",
        )),
        (SyntaxFactKind::Signature, "graphql.declaration_unavailable.signature") => Some((
            FactDomain::Entities,
            "graphql-declaration-name-or-owner-unavailable",
        )),
        (SyntaxFactKind::Signature, "graphql.string_validation_unavailable.signature") => Some((
            FactDomain::Occurrences,
            "graphql-escape-and-block-string-validation-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn owner_gap(fact: &SyntaxFact) -> Option<&'static str> {
    source_gap(fact)
        .filter(|(domain, _)| *domain == FactDomain::Entities)
        .map(|(_, code)| code)
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact
            .syntax_kind()
            .as_str()
            .strip_suffix(".signature")
            .is_some_and(|label| {
                matches!(
                    label,
                    "graphql.object"
                        | "graphql.interface"
                        | "graphql.input"
                        | "graphql.scalar"
                        | "graphql.union"
                        | "graphql.enum"
                        | "graphql.directive"
                        | "graphql.field"
                        | "graphql.argument"
                        | "graphql.input_field"
                        | "graphql.enum_value"
                        | "graphql.operation"
                        | "graphql.fragment"
                        | "graphql.variable"
                )
            })
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "graphql.named_type.reference") => {
            Some(OccurrenceRole::TypeUse)
        }
        (SyntaxFactKind::Occurrence, "graphql.written_name.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, "graphql.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "graphql.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match fact.syntax_kind().as_str() {
        "graphql.named_type.reference" => Some("graphql-named-type-target-unavailable"),
        "graphql.written_name.reference" => {
            Some("graphql-selection-directive-variable-or-fragment-binding-unavailable")
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, content_hash};
    use rootlight_ir::{
        AnalysisTier, ContainerRef, EntityRecord, EntityVisibility, FactEvidence, SourceRef,
        SourceSpan, SymbolIdentityClaim,
    };

    fn admitted_item() -> HashMap<u64, MaterializedEntity> {
        let repository = RepositoryId::from_bytes([1; 16]);
        let file = FileId::from_bytes([2; 20]);
        let generation = GenerationId::from_bytes([3; 20]);
        let container = ContainerRef::File(file);
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository,
            language: "graphql".to_owned(),
            kind: EntityKind::Struct,
            container: Some(container),
            container_identity: file.as_bytes().to_vec(),
            declared_identity: "Item".to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: content_hash(b"graphql-test-context").as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, 4).unwrap(),
            content_hash(b"Item"),
            None,
        );
        let record = EntityRecord {
            id: claim.symbol,
            repository,
            generation,
            kind: EntityKind::Struct,
            language: "graphql".to_owned(),
            tier: AnalysisTier::TierD,
            canonical_name: "Item".to_owned(),
            display_name: "Item".to_owned(),
            qualified_name: "Item".to_owned(),
            container: Some(container),
            visibility: EntityVisibility::Unknown,
            flags: Vec::new(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        HashMap::from([(
            1,
            MaterializedEntity {
                record,
                identity_claim: claim,
                direct_parent: container,
                identity_guard: [0; 32],
                definition_local_id: Some(1),
                signature_evidence: None,
                signature_span: None,
            },
        )])
    }

    fn signature(label: &str) -> SyntaxFact {
        SyntaxFact::new(
            2,
            None,
            SyntaxFactKind::Signature,
            SourceSpan::new(FileId::from_bytes([2; 20]), 0, 4).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn graphql_unadmitted_declaration_disables_an_otherwise_unique_stored_type() {
        let entities = admitted_item();
        let cancellation = Cancellation::new();
        let ordinary = type_targets(&entities, &[], &cancellation).unwrap();
        assert_eq!(ordinary.get("Item"), Some(&Some(entities[&1].record.id)));
        let unrelated = [signature("graphql.string_validation_unavailable.signature")];
        assert_eq!(
            type_targets(&entities, &unrelated, &cancellation).unwrap(),
            ordinary
        );
        let unavailable = [signature("graphql.declaration_unavailable.signature")];
        assert!(
            type_targets(&entities, &unavailable, &cancellation)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn graphql_type_target_admission_observes_cancellation_before_allocation() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        let unavailable = [signature("graphql.declaration_unavailable.signature")];
        let entities = HashMap::new();
        let result = type_targets(&entities, &unavailable, &cancellation);
        assert!(matches!(
            result,
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}
