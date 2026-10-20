//! Thrift links written names within this document, independently of codegen scopes.

use super::*;

/// Add a field edge only for an already resolved, source-owned type use.
/// Other owners and unproven targets produce no relation; the occurrence edge
/// remains separate. Cancellation, allocation and identity errors propagate.
pub(super) fn field_type_relation(
    fact: &SyntaxFact,
    occurrence: &OccurrenceRecord,
    owner: Option<&MaterializedEntity>,
    symbol: SymbolId,
    cancellation: &Cancellation,
) -> Result<Option<RelationRecord>, AdapterError> {
    cancellation.check()?;
    let Some(owner) = owner.filter(|owner| {
        owner.record.kind == EntityKind::Field && owner.record.language == "thrift"
    }) else {
        return Ok(None);
    };
    if fact.kind() != SyntaxFactKind::Occurrence
        || fact.syntax_kind().as_str() != "thrift.type.reference"
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
    // Normalization must not change semantic fields after the ID is derived.
    relation.evidence.derivation.sort_unstable();
    relation.id = derive_relation_record_id(&relation)
        .map_err(|_| provider_failure("treesitter-relation-identity"))?;
    cancellation.check()?;
    Ok(Some(relation))
}

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Namespace, "thrift.namespace.declaration")
            | (EntityKind::TypeAlias, "thrift.typedef.declaration")
            | (EntityKind::Enum, "thrift.enum.declaration")
            | (
                EntityKind::Constant,
                "thrift.enum_value.declaration" | "thrift.constant.declaration"
            )
            | (
                EntityKind::Struct,
                "thrift.struct.declaration" | "thrift.exception.declaration"
            )
            | (EntityKind::Union, "thrift.union.declaration")
            | (EntityKind::Interface, "thrift.service.declaration")
            | (EntityKind::Method, "thrift.function.declaration")
            | (EntityKind::Field, "thrift.field.declaration")
            | (
                EntityKind::Parameter,
                "thrift.parameter.declaration" | "thrift.throws_parameter.declaration"
            )
    )
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Module, "thrift.file.module") => Some((
            FactDomain::Relations,
            "thrift-import-wire-id-codegen-namespace-annotation-and-value-validation-unavailable",
        )),
        (
            SyntaxFactKind::Signature,
            "thrift.declaration_unavailable.signature" | "thrift.unsupported_construct.signature",
        ) => Some((
            FactDomain::Entities,
            "thrift-written-owner-or-construct-unavailable",
        )),
        (SyntaxFactKind::Signature, "thrift.uuid_unavailable.signature") => Some((
            FactDomain::Relations,
            "thrift-uuid-primitive-grammar-unavailable",
        )),
        (SyntaxFactKind::Signature, "thrift.string_validation_unavailable.signature") => Some((
            FactDomain::Occurrences,
            "thrift-string-escape-and-newline-validation-unavailable",
        )),
        (SyntaxFactKind::Signature, "thrift.source_semantics_unavailable.signature") => Some((
            FactDomain::Relations,
            "thrift-written-directive-annotation-or-value-semantics-unavailable",
        )),
        _ => None,
    }
}

pub(super) fn owner_gap(fact: &SyntaxFact) -> Option<&'static str> {
    source_gap(fact)
        .filter(|(domain, _)| *domain == FactDomain::Entities)
        .map(|(_, code)| code)
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "thrift.type.reference"
            | "thrift.service_base.reference"
            | "thrift.uuid_type.reference",
        ) => Some(OccurrenceRole::TypeUse),
        (SyntaxFactKind::Import, "thrift.written_include.import") => {
            Some(OccurrenceRole::ImportUse)
        }
        (SyntaxFactKind::Comment, "thrift.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "thrift.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

pub(super) fn reference_gap(fact: &SyntaxFact) -> Option<&'static str> {
    match fact.syntax_kind().as_str() {
        "thrift.type.reference" | "thrift.service_base.reference" => {
            Some("thrift-written-type-target-unavailable")
        }
        "thrift.uuid_type.reference" => Some("thrift-uuid-primitive-grammar-unavailable"),
        "thrift.written_include.import" => Some("thrift-include-target-unavailable"),
        _ => None,
    }
}

/// Owns keys so later common-lowering visibility mutations do not share borrows.
pub(super) struct Bindings {
    targets: HashMap<String, Option<(SymbolId, bool)>>,
}

impl Bindings {
    pub(super) fn new(
        materialized: &HashMap<u64, MaterializedEntity>,
        maximum: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            targets: HashMap::new(),
        };
        result
            .targets
            .try_reserve(materialized.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in materialized.values() {
            cancellation.check()?;
            let record = &entity.record;
            if record.language != "thrift" {
                continue;
            }
            let service = match record.kind {
                EntityKind::Struct
                | EntityKind::Enum
                | EntityKind::Union
                | EntityKind::TypeAlias => false,
                EntityKind::Interface => true,
                _ => continue,
            };
            // Physical Module names and namespace selectors deliberately do not
            // participate in the document-global IDL declaration namespace.
            require_resource_limit(
                ResourceKind::StringBytes,
                record.canonical_name.len(),
                maximum,
            )?;
            let mut name = String::new();
            name.try_reserve_exact(record.canonical_name.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            name.push_str(&record.canonical_name);
            result
                .targets
                .entry(name)
                .and_modify(|target| *target = None)
                .or_insert(Some((record.id, service)));
        }
        Ok(result)
    }

    pub(super) fn resolve(&self, fact: &SyntaxFact, name: &str) -> Option<SymbolId> {
        if name == "uuid" || fact.syntax_kind().as_str() == "thrift.uuid_type.reference" {
            return None;
        }
        let service = fact.syntax_kind().as_str() == "thrift.service_base.reference";
        self.targets
            .get(name)
            .copied()
            .flatten()
            .filter(|(_, candidate_service)| *candidate_service == service)
            .map(|(symbol, _)| symbol)
    }
}
