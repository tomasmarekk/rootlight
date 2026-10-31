//! Join native local reads to their own written function variables.
//! Other variable uses and runtime behavior retain explicit binding gaps.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef, SourceSpan,
};

use super::MaterializedEntity;

/// Keep native local-read certificates in identity and exact relation accounting.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "python.local_read.reference"
}

/// Current-source declarations indexed by their exact written function owner.
pub(super) struct LocalTargets<'a> {
    names: HashMap<(SymbolId, &'a str), Option<&'a MaterializedEntity>>,
    functions: HashMap<SymbolId, &'a MaterializedEntity>,
    source: &'a SourceRef,
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

impl<'a> LocalTargets<'a> {
    /// Index only native local declarations; duplicate owner/name pairs refuse adoption.
    /// Allocation and cancellation failures prevent publication of a partial index.
    pub(super) fn new(
        facts: &[SyntaxFact],
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut names = HashMap::new();
        let mut functions = HashMap::new();
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        functions
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for entity in entities.values() {
            cancel.check()?;
            if entity.record.language == "python" && entity.record.kind == EntityKind::Function {
                functions.insert(entity.record.id, entity);
            }
        }
        for fact in facts {
            cancel.check()?;
            if fact.kind() != SyntaxFactKind::Declaration
                || fact.syntax_kind().as_str() != "python.local_variable.declaration"
            {
                continue;
            }
            let Some(local) = entities.get(&fact.local_id()) else {
                continue;
            };
            let ContainerRef::Entity(owner) = local.direct_parent else {
                continue;
            };
            if local.record.language != "python"
                || local.record.kind != EntityKind::Variable
                || local.definition_local_id.is_none()
                || !local.record.canonical_name.is_ascii()
                || !functions.contains_key(&owner)
                || local
                    .record
                    .evidence
                    .source
                    .as_ref()
                    .is_none_or(|current| !same_source(current, source))
            {
                continue;
            }
            names
                .entry((owner, local.record.canonical_name.as_str()))
                .and_modify(|entry| *entry = None)
                .or_insert(Some(local));
        }
        Ok(Self {
            names,
            functions,
            source,
        })
    }

    /// Skip a containing assignment while preserving its direct function owner.
    pub(super) fn owner(
        &self,
        nearest: Option<&'a MaterializedEntity>,
    ) -> Option<&'a MaterializedEntity> {
        let nearest = nearest?;
        if nearest.record.kind == EntityKind::Function {
            return self.functions.get(&nearest.record.id).copied();
        }
        let ContainerRef::Entity(owner) = nearest.direct_parent else {
            return None;
        };
        self.functions.get(&owner).copied()
    }

    /// Adopt a native-certified lexical read only in the current same-owner frame.
    /// The caller supplies its physical span; values and dispatch remain unqualified.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(owner) = owner.filter(|owner| owner.record.kind == EntityKind::Function) else {
            return Ok(None);
        };
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if !required(fact)
            || occurrence.syntax_kind != "python.local_read.reference"
            || occurrence.role != OccurrenceRole::Reference
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.enclosing != Some(owner.record.id)
            || owner.record.language != "python"
            || owner.record.repository != self.source.repository()
            || owner.record.generation != self.source.generation()
            || owner.record.provenance != occurrence.provenance
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || !same_source(&occurrence.source, self.source)
            || !same_source(owner_source, self.source)
            || occurrence.source.span() != physical
            || owner_source.span().start_byte() > physical.start_byte()
            || owner_source.span().end_byte() < physical.end_byte()
            || occurrence.syntactic_text_hash != rootlight_ids::content_hash(text.as_bytes())
        {
            return Ok(None);
        }
        let Some(local) = self.names.get(&(owner.record.id, text)).copied().flatten() else {
            return Ok(None);
        };
        let Some(local_source) = local.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if local.record.provenance != occurrence.provenance
            || local.record.repository != self.source.repository()
            || local.record.generation != self.source.generation()
            || !same_source(local_source, self.source)
            || local_source.span().start_byte() < owner_source.span().start_byte()
            || local_source.span().end_byte() > physical.start_byte()
        {
            return Ok(None);
        }
        Ok(Some(local.record.id))
    }
}

pub(super) fn metadata(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "python.module_assignment_unavailable.signature"
                | "python.assignment_binding_unavailable.signature"
                | "python.augmented_assignment_binding_unavailable.signature"
                | "python.module_binding_context.signature"
                | "python.module_target.signature"
                | "python.local_assignment_context.signature"
        )
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let local_semantics = matches!(
        fact.syntax_kind().as_str(),
        "python.local_assignment_context.signature"
            | "python.local_binding_unavailable.reference"
            | "python.local_call_unavailable.call"
    );
    let detail = if local_semantics {
        Some("python-written-function-local-values-execution-and-reference-binding-unavailable")
    } else if fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "python.assignment_binding_unavailable.signature"
    {
        Some(
            "python-assignment-general-binding-and-nonmodule-or-complex-target-identity-unavailable",
        )
    } else if fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str()
            == "python.augmented_assignment_binding_unavailable.signature"
    {
        Some("python-augmented-write-value-effects-execution-and-read-target-binding-unavailable")
    } else if fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "python.module_binding_context.signature" | "python.module_target.signature"
        )
    {
        Some(
            "python-module-read-proof-is-lexical-only-values-calls-and-dynamic-namespace-semantics-unqualified",
        )
    } else if metadata(fact) {
        Some("python-module-assignment-name-uniqueness-syntax-or-complete-native-proof-unavailable")
    } else if fact.kind() == SyntaxFactKind::Declaration
        && fact.syntax_kind().as_str() == "python.module_variable.declaration"
    {
        Some(
            "python-written-module-variable-runtime-rebinding-aliases-and-general-reference-resolution-unavailable",
        )
    } else if matches!(
        fact.syntax_kind().as_str(),
        "python.module_binding_unavailable.reference" | "python.module_call_unavailable.call"
    ) {
        Some("python-written-module-variable-use-and-call-target-binding-unavailable")
    } else {
        None
    };
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| {
        if local_semantics && domain == FactDomain::Entities {
            None
        } else {
            detail.map(|detail| (domain, detail))
        }
    })
}
