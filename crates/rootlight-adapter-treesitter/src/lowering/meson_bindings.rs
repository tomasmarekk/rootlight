//! Join the native adjacent-assignment certificate to one current file variable.
//! Recovery elsewhere remains visible; raw duplicate, missing or foreign witnesses
//! cannot be hidden by the normalized entity selection.

use super::MaterializedEntity;
use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef, SourceSpan,
};
use std::collections::HashMap;

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "meson.adjacent_iterable.reference"
}

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    kind == EntityKind::Variable
        && matches!(
            label,
            "meson.assignment.declaration" | "meson.foreach_variable.declaration"
        )
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Comment, _) => Some(OccurrenceRole::Documentation),
        (SyntaxFactKind::StringLiteral, _) => Some(OccurrenceRole::StringEvidence),
        (
            SyntaxFactKind::Occurrence,
            "meson.adjacent_iterable.reference" | "meson.unproved_name.reference",
        ) => Some(OccurrenceRole::Reference),
        _ => None,
    }
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<&'static str> {
    (fact.kind() == SyntaxFactKind::Occurrence
        && matches!(
            fact.syntax_kind().as_str(),
            "meson.adjacent_iterable.reference" | "meson.unproved_name.reference"
        ))
    .then_some("meson-source-name-binding-not-certified")
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "meson.build_definition.root") => Some(
            "meson-clean-written-top-level-bindings-and-adjacent-iterables-only-project-evaluation-and-recovered-regions-unavailable",
        ),
        (SyntaxFactKind::Signature, "meson.recovered_region.signature") => {
            Some("meson-native-recovered-syntax-region-not-certified")
        }
        _ => None,
    };
    detail.into_iter().flat_map(|detail| {
        [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ]
        .into_iter()
        .map(move |domain| (domain, detail))
    })
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    *left == super::source_for_span(right, left.span())
}

fn raw_role(fact: &SyntaxFact) -> Option<u8> {
    if fact.kind() == SyntaxFactKind::Declaration {
        return Some(0);
    }
    if fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str().ends_with(".reference")
    {
        return Some(2);
    }
    (fact.kind() == SyntaxFactKind::Occurrence
        && (fact.syntax_kind().as_str().ends_with(".definition")
            || fact.syntax_kind().as_str().ends_with(".definition_part")))
    .then_some(1)
}

pub(super) struct VariableTargets<'a> {
    facts: HashMap<u64, Option<&'a SyntaxFact>>,
    sites: HashMap<SourceSpan, Option<&'a MaterializedEntity>>,
    source: &'a SourceRef,
    root: Option<u64>,
}

impl<'a> VariableTargets<'a> {
    pub(super) fn new<'text>(
        facts: &'a [SyntaxFact],
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        projected_length: usize,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut by_id = HashMap::new();
        let mut raw_counts = HashMap::<(SourceSpan, u8), usize>::new();
        let mut names = HashMap::<&str, Option<(u64, &MaterializedEntity)>>::new();
        let mut starts = HashMap::<u64, Option<(u64, &MaterializedEntity)>>::new();
        let mut contexts = HashMap::<SourceSpan, Option<&SyntaxFact>>::new();
        let mut signatures = HashMap::<SourceSpan, usize>::new();
        signatures
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut sites = HashMap::new();
        let mut recovery = Vec::new();
        recovery
            .try_reserve_exact(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        raw_counts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        contexts
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        starts
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        sites
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut root = None;
        let mut root_count = 0_usize;
        let projected_end = u64::try_from(projected_length).map_err(|_| SinkError::InvalidIr)?;
        for fact in facts {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "meson.recovered_region.signature"
            {
                recovery.push((fact.span().start_byte(), fact.span().end_byte()));
            }
            by_id
                .entry(fact.local_id())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(fact));
            if let Some(role) = raw_role(fact) {
                raw_counts
                    .entry((fact.span(), role))
                    .and_modify(|n| *n = 2)
                    .or_insert(1);
            }
            if fact.kind() == SyntaxFactKind::Signature {
                signatures
                    .entry(fact.span())
                    .and_modify(|count| *count = 2)
                    .or_insert(1);
            }
            if fact.kind() == SyntaxFactKind::Root {
                root_count = root_count.saturating_add(1);
                let full = SourceSpan::new(fact.span().file(), 0, projected_end)
                    .map_err(|_| SinkError::InvalidIr)?;
                let prefix = SourceSpan::new(fact.span().file(), 0, fact.span().start_byte())
                    .map_err(|_| SinkError::InvalidIr)?;
                let suffix =
                    SourceSpan::new(fact.span().file(), fact.span().end_byte(), projected_end)
                        .map_err(|_| SinkError::InvalidIr)?;
                let mut layout = true;
                for part in [text(prefix)?, text(suffix)?] {
                    for chunk in part
                        .strip_prefix('\u{feff}')
                        .unwrap_or(part)
                        .as_bytes()
                        .chunks(64)
                    {
                        cancel.check()?;
                        layout &= chunk.iter().all(u8::is_ascii_whitespace);
                    }
                }
                if fact.syntax_kind().as_str() == "meson.build_definition.root"
                    && fact.parent().is_none()
                    && source.span().start_byte() == 0
                    && physical(full)? == source.span()
                    && layout
                {
                    root = Some(fact.local_id());
                }
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "meson.adjacent_iterable_context.signature"
            {
                contexts
                    .entry(fact.span())
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some(fact));
            }
        }
        if root_count != 1 {
            root = None;
        }
        crate::runtime::sort_cancellable_by(&mut recovery, cancel, |left, right| left.cmp(right))?;
        let mut maximum_end = 0_u64;
        for (_, end) in &mut recovery {
            cancel.check()?;
            maximum_end = maximum_end.max(*end);
            *end = maximum_end;
        }
        for (local, entity) in entities {
            cancel.check()?;
            if entity.record.language != "meson" || entity.record.kind != EntityKind::Variable {
                continue;
            }
            names
                .entry(entity.record.canonical_name.as_str())
                .and_modify(|entry| *entry = None)
                .or_insert(Some((*local, entity)));
            let Some(evidence) = entity.record.evidence.source.as_ref() else {
                continue;
            };
            starts
                .entry(evidence.span().start_byte())
                .and_modify(|entry| *entry = None)
                .or_insert(Some((*local, entity)));
        }
        for fact in facts {
            cancel.check()?;
            if !required(fact) {
                continue;
            }
            let mut target = None;
            if let Some(root) = root
                && fact.parent() == Some(root)
                && by_id.get(&fact.local_id()).copied().flatten() == Some(fact)
                && raw_counts.get(&(fact.span(), 2)) == Some(&1)
            {
                let name = text(fact.span())?;
                if let Some((local, entity)) = names.get(name).copied().flatten()
                    && let Some(declaration) = by_id.get(&local).copied().flatten()
                    && let Some(definition) = entity
                        .definition_local_id
                        .and_then(|id| by_id.get(&id).copied().flatten())
                    && let Some(evidence) = entity.record.evidence.source.as_ref()
                    && declaration.span().file() == fact.span().file()
                    && declaration.span().end_byte() < fact.span().start_byte()
                {
                    let window = SourceSpan::new(
                        fact.span().file(),
                        declaration.span().start_byte(),
                        fact.span().end_byte(),
                    )
                    .map_err(|_| SinkError::InvalidIr)?;
                    let context = contexts.get(&window).copied().flatten();
                    let mut low = 0_usize;
                    let mut high = recovery.len();
                    while low < high {
                        cancel.check()?;
                        let middle = low + (high - low) / 2;
                        if recovery
                            .get(middle)
                            .is_some_and(|item| item.0 < window.end_byte())
                        {
                            low = middle + 1;
                        } else {
                            high = middle;
                        }
                    }
                    let clean_window = low
                        .checked_sub(1)
                        .and_then(|index| recovery.get(index))
                        .is_none_or(|item| item.1 <= window.start_byte());
                    if declaration.kind() == SyntaxFactKind::Declaration
                        && declaration.syntax_kind().as_str() == "meson.assignment.declaration"
                        && declaration.parent() == Some(root)
                        && definition.kind() == SyntaxFactKind::Occurrence
                        && definition.syntax_kind().as_str() == "meson.name.definition"
                        && definition.parent() == Some(local)
                        && raw_counts.get(&(declaration.span(), 0)) == Some(&1)
                        && raw_counts.get(&(definition.span(), 1)) == Some(&1)
                        && clean_window
                        && signatures.get(&window) == Some(&1)
                        && context.is_some_and(|context| {
                            context.parent() == Some(root)
                                && by_id.get(&context.local_id()).copied().flatten()
                                    == Some(context)
                        })
                        && physical(declaration.span())? == evidence.span()
                        && starts
                            .get(&evidence.span().start_byte())
                            .copied()
                            .flatten()
                            .is_some_and(|(id, _)| id == local)
                        && super::span_contains(declaration.span(), definition.span())
                        && declaration.span().end_byte() < fact.span().start_byte()
                        && text(definition.span())? == name
                        && entity.record.repository == source.repository()
                        && entity.record.generation == source.generation()
                        && entity.direct_parent == ContainerRef::File(source.span().file())
                        && entity.record.container == Some(entity.direct_parent)
                        && same_source(evidence, source)
                    {
                        target = Some(entity);
                    }
                }
            }
            sites
                .entry(physical(fact.span())?)
                .and_modify(|entry| *entry = None)
                .or_insert(target);
        }
        Ok(Self {
            facts: by_id,
            sites,
            source,
            root,
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical_span: SourceSpan,
        occurrence: &OccurrenceRecord,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(root) = self.root else {
            return Ok(None);
        };
        let Some(target) = self.sites.get(&physical_span).copied().flatten() else {
            return Ok(None);
        };
        if !required(fact)
            || self.facts.get(&fact.local_id()).copied().flatten() != Some(fact)
            || fact.parent() != Some(root)
            || occurrence.syntax_kind != "meson.adjacent_iterable.reference"
            || occurrence.role != OccurrenceRole::Reference
            || occurrence.enclosing.is_some()
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.source.span() != physical_span
            || !same_source(&occurrence.source, self.source)
            || occurrence.provenance != target.record.provenance
            || occurrence.syntactic_text_hash != rootlight_ids::content_hash(text.as_bytes())
            || target.record.canonical_name != text
        {
            return Ok(None);
        }
        Ok(Some(target.record.id))
    }
}
