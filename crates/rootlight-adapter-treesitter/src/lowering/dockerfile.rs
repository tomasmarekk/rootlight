//! Join written stage aliases to earlier unique stages in the same source frame.
//! Native header/COPY witnesses and raw multiplicity are required in addition to
//! materialized identities; generic name scoring cannot supply missing authority.

use super::MaterializedEntity;
use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef, SourceSpan,
};
use std::collections::{HashMap, HashSet};

pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "dockerfile.copy_from.reference"
}

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    (kind == EntityKind::BuildTarget && label == "dockerfile.stage.declaration")
        || (kind == EntityKind::Variable && label == "dockerfile.env.declaration")
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Occurrence, "dockerfile.copy_from.reference") => {
            Some(OccurrenceRole::Reference)
        }
        (SyntaxFactKind::Comment, "dockerfile.comment.comment") => {
            Some(OccurrenceRole::Documentation)
        }
        _ => None,
    }
}

pub(super) fn source_gap(fact: &SyntaxFact) -> Option<&'static str> {
    required(fact).then_some("dockerfile-copy-source-is-not-a-unique-prior-written-stage")
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "dockerfile.source_file.root") => Some(
            "dockerfile-written-stage-aliases-literal-env-names-and-prior-copy-only-values-lifetime-runtime-and-other-directives-unavailable",
        ),
        (SyntaxFactKind::Signature, "dockerfile.unproved_stage_alias.signature") => {
            Some("dockerfile-stage-alias-shape-or-case-binding-not-certified")
        }
        (SyntaxFactKind::Signature, "dockerfile.unproved_env.signature") => {
            Some("dockerfile-env-written-name-or-current-source-context-unavailable")
        }
        (SyntaxFactKind::Signature, "dockerfile.unproved_copy.signature") => {
            Some("dockerfile-copy-flags-or-literal-stage-source-not-certified")
        }
        (SyntaxFactKind::Signature, "dockerfile.recovered_region.signature") => {
            Some("dockerfile-native-recovered-region-not-certified")
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

fn header_name(
    header: SourceSpan,
    definition: SourceSpan,
    bytes: &[u8],
    name: &[u8],
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(at) = definition
        .start_byte()
        .checked_sub(header.start_byte())
        .and_then(|offset| usize::try_from(offset).ok())
    else {
        return Ok(false);
    };
    let Some(prefix) = bytes.get(..at) else {
        return Ok(false);
    };
    let mut as_end = prefix.len();
    while as_end > 0 {
        cancel.check()?;
        let start = as_end.saturating_sub(64);
        let Some(chunk) = prefix.get(start..as_end) else {
            return Ok(false);
        };
        if let Some(last) = chunk.iter().rposition(|byte| !byte.is_ascii_whitespace()) {
            as_end = start + last + 1;
            break;
        }
        as_end = start;
    }
    let Some(as_at) = as_end.checked_sub(2) else {
        return Ok(false);
    };
    Ok(header.file() == definition.file()
        && definition.end_byte() == header.end_byte()
        && bytes
            .get(..4)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"FROM"))
        && bytes.get(4).is_some_and(u8::is_ascii_whitespace)
        && bytes.get(at..) == Some(name)
        && prefix
            .get(as_at..as_end)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"AS"))
        && as_at
            .checked_sub(1)
            .and_then(|index| prefix.get(index))
            .is_some_and(u8::is_ascii_whitespace))
}

fn copy_name(context: SourceSpan, site: SourceSpan, bytes: &[u8], name: &[u8]) -> bool {
    let Some(at) = site
        .start_byte()
        .checked_sub(context.start_byte())
        .and_then(|offset| usize::try_from(offset).ok())
    else {
        return false;
    };
    let Some(flag) = at.checked_sub(7) else {
        return false;
    };
    let Some(end) = at.checked_add(name.len()) else {
        return false;
    };
    context.file() == site.file()
        && site.end_byte().checked_sub(site.start_byte()) == u64::try_from(name.len()).ok()
        && bytes
            .get(..4)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"COPY"))
        && bytes.get(4).is_some_and(u8::is_ascii_whitespace)
        && bytes.get(flag..at) == Some(b"--from=")
        && flag
            .checked_sub(1)
            .and_then(|index| bytes.get(index))
            .is_some_and(u8::is_ascii_whitespace)
        && bytes.get(at..end) == Some(name)
        && bytes.get(end).is_some_and(u8::is_ascii_whitespace)
}

pub(super) struct StageTargets<'a> {
    facts: HashMap<u64, Option<&'a SyntaxFact>>,
    sites: HashMap<SourceSpan, Option<(&'a MaterializedEntity, &'a MaterializedEntity)>>,
    source: &'a SourceRef,
}

impl<'a> StageTargets<'a> {
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
        let mut multiplicity = HashMap::<(SourceSpan, SyntaxFactKind), usize>::new();
        let mut names = HashMap::<&str, Option<(u64, &MaterializedEntity)>>::new();
        let mut headers = HashMap::<u64, Option<&SyntaxFact>>::new();
        let mut copies = Vec::new();
        let mut sites = HashMap::new();
        let mut validated_headers = HashSet::new();
        validated_headers
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        multiplicity
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        headers
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        copies
            .try_reserve_exact(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        sites
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut root = None;
        let mut root_count = 0_usize;
        let mut clean = true;
        let end = u64::try_from(projected_length).map_err(|_| SinkError::InvalidIr)?;
        for fact in facts {
            cancel.check()?;
            by_id
                .entry(fact.local_id())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(fact));
            multiplicity
                .entry((fact.span(), fact.kind()))
                .and_modify(|count| *count = 2)
                .or_insert(1);
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Root, _) => {
                    root_count = root_count.saturating_add(1);
                    if fact.syntax_kind().as_str() == "dockerfile.source_file.root"
                        && fact.parent().is_none()
                        && fact.span().start_byte() == 0
                        && fact.span().end_byte() == end
                        && physical(fact.span())? == source.span()
                    {
                        root = Some(fact.local_id());
                    }
                }
                (SyntaxFactKind::Signature, "dockerfile.stage_header.signature") => {
                    headers
                        .entry(fact.span().start_byte())
                        .and_modify(|entry| *entry = None)
                        .or_insert(Some(fact));
                }
                (SyntaxFactKind::Signature, "dockerfile.copy_from_context.signature") => {
                    copies.push(fact)
                }
                (
                    SyntaxFactKind::Signature,
                    "dockerfile.unproved_stage_alias.signature"
                    | "dockerfile.recovered_region.signature",
                ) => clean = false,
                _ => {}
            }
        }
        if root_count != 1 {
            root = None;
        }
        let mut boundaries = Vec::new();
        let mut stage_ends = HashMap::new();
        boundaries
            .try_reserve_exact(headers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        stage_ends
            .try_reserve(headers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for header in headers.values() {
            cancel.check()?;
            let Some(header) = header else {
                clean = false;
                continue;
            };
            if by_id.get(&header.local_id()).copied().flatten() != Some(*header)
                || header.span().file() != source.span().file()
                || header.span().start_byte() >= header.span().end_byte()
                || header.span().end_byte() > end
            {
                clean = false;
            }
            boundaries.push(*header);
        }
        crate::runtime::sort_cancellable_by(&mut boundaries, cancel, |left, right| {
            left.span().start_byte().cmp(&right.span().start_byte())
        })?;
        for (index, header) in boundaries.iter().enumerate() {
            cancel.check()?;
            let next = boundaries
                .get(index + 1)
                .map_or(end, |next| next.span().start_byte());
            if header.span().end_byte() > next {
                clean = false;
            }
            stage_ends.insert(header.span().start_byte(), next);
        }
        crate::runtime::sort_cancellable_by(&mut copies, cancel, |left, right| {
            (
                left.span().start_byte(),
                left.span().end_byte(),
                left.local_id(),
            )
                .cmp(&(
                    right.span().start_byte(),
                    right.span().end_byte(),
                    right.local_id(),
                ))
        })?;
        let mut previous_end = 0_u64;
        for context in &copies {
            cancel.check()?;
            if context.span().start_byte() < previous_end {
                clean = false;
            }
            previous_end = previous_end.max(context.span().end_byte());
        }
        // Count raw definitions before normalized entity selection. A withheld
        // or duplicate declaration cannot make the remaining spelling unique.
        let mut raw_names = HashMap::<&str, usize>::new();
        raw_names
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "dockerfile.stage_alias.definition"
            {
                raw_names
                    .entry(text(fact.span())?)
                    .and_modify(|count| *count = 2)
                    .or_insert(1);
            }
        }
        for (local, entity) in entities {
            cancel.check()?;
            if entity.record.language == "dockerfile"
                && entity.record.kind == EntityKind::BuildTarget
            {
                names
                    .entry(entity.record.canonical_name.as_str())
                    .and_modify(|entry| *entry = None)
                    .or_insert(Some((*local, entity)));
                if let Some(declaration) = by_id.get(local).copied().flatten()
                    && let Some(definition) = entity
                        .definition_local_id
                        .and_then(|id| by_id.get(&id).copied().flatten())
                    && let Some(header) = headers
                        .get(&declaration.span().start_byte())
                        .copied()
                        .flatten()
                    && header_name(
                        header.span(),
                        definition.span(),
                        text(header.span())?.as_bytes(),
                        entity.record.canonical_name.as_bytes(),
                        cancel,
                    )?
                {
                    validated_headers.insert(*local);
                }
            }
        }
        for fact in facts {
            cancel.check()?;
            if !required(fact) {
                continue;
            }
            let mut pair = None;
            let name = text(fact.span())?;
            if let Some(root) = root.filter(|_| clean)
                && by_id.get(&fact.local_id()).copied().flatten() == Some(fact)
                && multiplicity.get(&(fact.span(), SyntaxFactKind::Occurrence)) == Some(&1)
                && raw_names.get(name) == Some(&1)
                && let Some((target_local, target)) = names.get(name).copied().flatten()
                && let Some(current_local) = fact.parent()
                && let Some(current) = entities.get(&current_local)
                && current.record.kind == EntityKind::BuildTarget
                && current.record.language == "dockerfile"
                && let Some(target_decl) = by_id.get(&target_local).copied().flatten()
                && let Some(current_decl) = by_id.get(&current_local).copied().flatten()
                && let Some(target_def) = target
                    .definition_local_id
                    .and_then(|id| by_id.get(&id).copied().flatten())
                && let Some(current_def) = current
                    .definition_local_id
                    .and_then(|id| by_id.get(&id).copied().flatten())
                && let Some(target_header) = headers
                    .get(&target_decl.span().start_byte())
                    .copied()
                    .flatten()
                && let Some(current_header) = headers
                    .get(&current_decl.span().start_byte())
                    .copied()
                    .flatten()
                && by_id.get(&target_header.local_id()).copied().flatten() == Some(target_header)
                && by_id.get(&current_header.local_id()).copied().flatten() == Some(current_header)
                && target_decl.kind() == SyntaxFactKind::Declaration
                && current_decl.kind() == SyntaxFactKind::Declaration
                && written_identity(target.record.kind, target_decl.syntax_kind().as_str())
                && written_identity(current.record.kind, current_decl.syntax_kind().as_str())
                && target_decl.parent() == Some(root)
                && current_decl.parent() == Some(root)
                && target_decl.span().file() == fact.span().file()
                && current_decl.span().file() == fact.span().file()
                && stage_ends.get(&target_decl.span().start_byte())
                    == Some(&target_decl.span().end_byte())
                && stage_ends.get(&current_decl.span().start_byte())
                    == Some(&current_decl.span().end_byte())
                && target_decl.span().end_byte() <= current_decl.span().start_byte()
                && target_decl.span().start_byte() < target_decl.span().end_byte()
                && current_decl.span().start_byte() < fact.span().start_byte()
                && super::span_contains(current_decl.span(), fact.span())
                && target_def.parent() == Some(target_local)
                && target_def.kind() == SyntaxFactKind::Occurrence
                && target_def.syntax_kind().as_str() == "dockerfile.stage_alias.definition"
                && text(target_def.span())? == name
                && current_def.parent() == Some(current_local)
                && current_def.kind() == SyntaxFactKind::Occurrence
                && current_def.syntax_kind().as_str() == "dockerfile.stage_alias.definition"
                && text(current_def.span())? == current.record.canonical_name
                && raw_names.get(current.record.canonical_name.as_str()) == Some(&1)
                && super::span_contains(target_header.span(), target_def.span())
                && super::span_contains(current_header.span(), current_def.span())
                && validated_headers.contains(&target_local)
                && validated_headers.contains(&current_local)
                && target_header.parent() == Some(target_local)
                && current_header.parent() == Some(current_local)
                && super::span_contains(target_decl.span(), target_header.span())
                && super::span_contains(current_decl.span(), current_header.span())
                && multiplicity.get(&(target_decl.span(), SyntaxFactKind::Declaration)) == Some(&1)
                && multiplicity.get(&(current_decl.span(), SyntaxFactKind::Declaration)) == Some(&1)
                && multiplicity.get(&(target_def.span(), SyntaxFactKind::Occurrence)) == Some(&1)
                && multiplicity.get(&(current_def.span(), SyntaxFactKind::Occurrence)) == Some(&1)
                && multiplicity.get(&(target_header.span(), SyntaxFactKind::Signature)) == Some(&1)
                && multiplicity.get(&(current_header.span(), SyntaxFactKind::Signature)) == Some(&1)
            {
                let mut low = 0_usize;
                let mut high = copies.len();
                while low < high {
                    cancel.check()?;
                    let middle = low + (high - low) / 2;
                    if copies.get(middle).is_some_and(|context| {
                        context.span().start_byte() <= fact.span().start_byte()
                    }) {
                        low = middle + 1;
                    } else {
                        high = middle;
                    }
                }
                let context = low
                    .checked_sub(1)
                    .and_then(|index| copies.get(index))
                    .copied();
                let mut conserved = context.is_some_and(|context| {
                    context.parent() == Some(current_local)
                        && super::span_contains(context.span(), fact.span())
                        && super::span_contains(current_decl.span(), context.span())
                        && multiplicity.get(&(context.span(), SyntaxFactKind::Signature))
                            == Some(&1)
                        && by_id.get(&context.local_id()).copied().flatten() == Some(context)
                });
                if let Some(context) = context {
                    conserved &= copy_name(
                        context.span(),
                        fact.span(),
                        text(context.span())?.as_bytes(),
                        name.as_bytes(),
                    );
                }
                for (declaration, entity) in [(target_decl, target), (current_decl, current)] {
                    cancel.check()?;
                    conserved &= entity.record.repository == source.repository()
                        && entity.record.generation == source.generation()
                        && entity.direct_parent == ContainerRef::File(source.span().file())
                        && entity.record.container == Some(entity.direct_parent)
                        && entity
                            .record
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|evidence| same_source(evidence, source));
                    if let Some(evidence) = entity.record.evidence.source.as_ref() {
                        conserved &= physical(declaration.span())? == evidence.span();
                    }
                }
                if conserved {
                    pair = Some((current, target));
                }
            }
            sites
                .entry(physical(fact.span())?)
                .and_modify(|entry| *entry = None)
                .or_insert(pair);
        }
        Ok(Self {
            facts: by_id,
            sites,
            source,
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical_span: SourceSpan,
        occurrence: &OccurrenceRecord,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<(SymbolId, SymbolId)>, AdapterError> {
        cancel.check()?;
        let Some((owner, target)) = self.sites.get(&physical_span).copied().flatten() else {
            return Ok(None);
        };
        if !required(fact)
            || self.facts.get(&fact.local_id()).copied().flatten() != Some(fact)
            || occurrence.syntax_kind != "dockerfile.copy_from.reference"
            || occurrence.role != OccurrenceRole::Reference
            || occurrence.enclosing != Some(owner.record.id)
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.source.span() != physical_span
            || !same_source(&occurrence.source, self.source)
            || occurrence.provenance != target.record.provenance
            || occurrence.provenance != owner.record.provenance
            || occurrence.syntactic_text_hash != rootlight_ids::content_hash(text.as_bytes())
            || target.record.canonical_name != text
        {
            return Ok(None);
        }
        Ok(Some((owner.record.id, target.record.id)))
    }
}
