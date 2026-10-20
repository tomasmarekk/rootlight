//! Only uniquely named immediate nested procedures acquire lexical call edges.
//! Parameter types, overload selection and imported-unit lookup remain unproved.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{ContainerRef, EntityKind, FactDomain, OccurrenceRole};

use super::MaterializedEntity;

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Module, "ada.package.declaration")
            | (
                EntityKind::Function,
                "ada.procedure.declaration" | "ada.function.declaration"
            )
            | (EntityKind::Parameter, "ada.parameter.declaration")
    )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let root =
        fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str() == "ada.file.module";
    let unsupported = fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "ada.unsupported.expression";
    [
        root.then_some((
            FactDomain::Entities,
            "ada-types-objects-generics-tasks-and-extended-declarations-unavailable",
        )),
        root.then_some((
            FactDomain::Occurrences,
            "ada-general-value-type-attribute-and-selected-occurrences-unavailable",
        )),
        root.then_some((
            FactDomain::Relations,
            "ada-unit-import-overload-type-and-runtime-binding-unavailable",
        )),
        unsupported.then_some((
            FactDomain::Relations,
            "ada-source-construct-semantics-unavailable",
        )),
    ]
    .into_iter()
    .flatten()
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "ada.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "ada.direct_call.reference") => Some(OccurrenceRole::CallSite),
        (SyntaxFactKind::Comment, "ada.comment.comment") => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, "ada.string.string") => {
            Some(OccurrenceRole::StringEvidence)
        }
        _ => None,
    }
}

#[derive(Default)]
pub(super) struct NestedProcedures {
    names: HashMap<(SymbolId, String), Option<SymbolId>>,
}

impl NestedProcedures {
    pub(super) fn new(
        entities: &HashMap<u64, MaterializedEntity>,
        facts: &HashMap<u64, &SyntaxFact>,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self::default();
        for (&local_id, entity) in entities
            .iter()
            .filter(|(_, entity)| entity.record.language == "ada")
        {
            cancellation.check()?;
            let Some(ContainerRef::Entity(parent)) = entity.record.container else {
                continue;
            };
            let key = (parent, entity.record.canonical_name.to_ascii_lowercase());
            let procedure = facts
                .get(&local_id)
                .is_some_and(|owner| owner.syntax_kind().as_str() == "ada.procedure.declaration");
            let symbol = procedure.then_some(entity.record.id);
            // Same-spelled declarations can share an entity ID. Count source
            // declarations before coalescing so overloads never become unique.
            result
                .names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result
                .names
                .entry(key)
                .and_modify(|candidate| *candidate = None)
                .or_insert(symbol);
        }
        Ok(result)
    }

    pub(super) fn resolve(&self, caller: SymbolId, name: &str) -> Option<SymbolId> {
        self.names
            .get(&(caller, name.to_ascii_lowercase()))
            .copied()
            .flatten()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn immediate_lookup_folds_case_and_keeps_ambiguity_unresolved() {
        let caller = SymbolId::from_bytes([1; 20]);
        let callee = SymbolId::from_bytes([2; 20]);
        let mut table = NestedProcedures::default();
        table
            .names
            .insert((caller, "child".to_owned()), Some(callee));
        table.names.insert((caller, "overloaded".to_owned()), None);
        assert_eq!(table.resolve(caller, "cHiLd"), Some(callee));
        assert_eq!(table.resolve(caller, "Overloaded"), None);
        assert_eq!(table.resolve(SymbolId::from_bytes([3; 20]), "Child"), None);
        assert_eq!(table.resolve(caller, "Package.Child"), None);
    }
}
