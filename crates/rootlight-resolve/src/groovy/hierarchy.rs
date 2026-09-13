//! Source-owned Groovy parent graphs and inherited member selection.
//! Lookup results are shared by lexical owner/name; each result retains its
//! inspected declarations, native headers and coverage instead of name scores.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use rootlight_cancel::Cancellation;
use rootlight_ids::{FactId, FileId, SymbolId};
use rootlight_ir::{
    ContainerRef, CoverageRecord, CoverageScope, CoverageStatus, EntityKind, EntityRecord,
    EntityVisibility, ExtensionEnvelope, FactDomain, FactRef, GroovyTypeLookup, OccurrenceRecord,
    OccurrenceRole, OccurrenceTarget, SourceRef, SourceSpan,
};

use super::{GroovyIndex, is_type};
use crate::{MAX_RESOLUTION_WORK_LIMIT, ResolutionError};

pub(crate) struct Entry<'a> {
    pub(crate) envelope: &'a ExtensionEnvelope,
    pub(crate) selection: Arc<Selection<'a>>,
}

#[derive(Default)]
pub(crate) struct Selection<'a> {
    pub(crate) target: Option<&'a EntityRecord>,
    pub(crate) candidates: Option<CandidateSet>,
    pub(crate) sources: BTreeSet<SourceRef>,
    pub(crate) facts: BTreeSet<FactRef>,
    pub(crate) work: usize,
    uncertain: bool,
}

pub(crate) struct CandidateSet {
    pub(crate) symbols: Vec<SymbolId>,
    pub(crate) total_count: u64,
}

struct Parent<'a> {
    target: SymbolId,
    header: &'a OccurrenceRecord,
}

#[derive(Default)]
struct Parents<'a> {
    superclass: Option<Parent<'a>>,
    interfaces: Vec<Parent<'a>>,
}

impl Parents<'_> {
    fn all(&self) -> impl Iterator<Item = &Parent<'_>> {
        self.superclass.iter().chain(self.interfaces.iter())
    }
}

struct Graph<'a, 'index, 'sites> {
    index: &'index GroovyIndex<'a>,
    entities: &'index BTreeMap<SymbolId, &'a EntityRecord>,
    children: BTreeMap<SymbolId, BTreeMap<&'a str, Option<&'a EntityRecord>>>,
    parents: BTreeMap<SymbolId, Option<Parents<'sites>>>,
    complete: BTreeSet<SymbolId>,
    coverage: BTreeMap<FileId, &'a CoverageRecord>,
}

pub(super) fn build<'a>(
    index: &GroovyIndex<'a>,
    entities: &BTreeMap<SymbolId, &'a EntityRecord>,
    coverage: &'a [CoverageRecord],
    extensions: &'a [ExtensionEnvelope],
    occurrences: &[OccurrenceRecord],
    cancellation: &Cancellation,
) -> Result<BTreeMap<FactId, Entry<'a>>, ResolutionError> {
    let mut output = BTreeMap::new();
    if index.unavailable
        || !extensions
            .iter()
            .any(|item| item.namespace == rootlight_ir::GROOVY_TYPE_LOOKUP_NAMESPACE)
    {
        return Ok(output);
    }
    let mut graph = Graph {
        index,
        entities,
        children: BTreeMap::new(),
        parents: BTreeMap::new(),
        complete: BTreeSet::new(),
        coverage: BTreeMap::new(),
    };
    let mut counts = BTreeMap::<FileId, u64>::new();
    let mut invalid_files = BTreeSet::new();
    let mut by_source = BTreeMap::new();
    for site in occurrences {
        cancellation.check()?;
        let Some(context) = index.contexts.get(&site.file) else {
            continue;
        };
        if !index.has_source_owned_provenance(site)
            || site.source.content_hash() != context.file.content_hash
        {
            invalid_files.insert(site.file);
        }
        let count = counts.entry(site.file).or_default();
        *count = count.checked_add(1).ok_or(ResolutionError::CountOverflow)?;
        by_source
            .entry(site.source.span())
            .and_modify(|prior| *prior = None)
            .or_insert(Some(site));
    }
    for record in coverage {
        cancellation.check()?;
        let CoverageScope::File(file) = record.scope else {
            continue;
        };
        let Some(context) = index.contexts.get(&file) else {
            continue;
        };
        if record.domain != FactDomain::Occurrences {
            continue;
        }
        if record.status != CoverageStatus::Complete
            || record.skipped != 0
            || record.discovered != record.indexed
            || record.indexed != counts.get(&file).copied().unwrap_or(0)
            || record.provenance != context.file.provenance
            || record.evidence.source != context.file.evidence.source
            || graph.coverage.insert(file, record).is_some()
        {
            invalid_files.insert(file);
        }
    }
    let mut owners = BTreeMap::new();
    for entity in entities
        .values()
        .copied()
        .filter(|entity| entity.language == "groovy" && is_type(entity.kind))
    {
        cancellation.check()?;
        let Some(source) = entity.evidence.source.as_ref() else {
            continue;
        };
        let file = source.span().file();
        owners
            .entry(source.span())
            .and_modify(|prior| *prior = None)
            .or_insert(Some(entity));
        graph.parents.insert(
            entity.id,
            (graph.coverage.contains_key(&file) && !invalid_files.contains(&file))
                .then(Parents::default),
        );
        if let Some(ContainerRef::Entity(parent)) = entity.container {
            graph
                .children
                .entry(parent)
                .or_default()
                .entry(entity.canonical_name.as_str())
                .and_modify(|prior| *prior = None)
                .or_insert(Some(entity));
        }
    }
    let mut headers: Vec<_> = occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.role,
                OccurrenceRole::InheritanceUse | OccurrenceRole::ImplementationUse
            )
        })
        .collect();
    headers.sort_unstable_by_key(|site| (site.file, site.source.span().start_byte()));
    for header in headers {
        cancellation.check()?;
        let Some(owner) = header.enclosing.and_then(|id| entities.get(&id)).copied() else {
            continue;
        };
        if !graph.parents.contains_key(&owner.id) {
            continue;
        }
        let valid = owner.evidence.source.as_ref().is_some_and(|source| {
            source.span().file() == header.file
                && source.span().start_byte() <= header.source.span().start_byte()
                && source.span().end_byte() >= header.source.span().end_byte()
        });
        let target = valid.then(|| graph.header_target(header)).flatten();
        let Some(target) = target else {
            graph.parents.insert(owner.id, None);
            continue;
        };
        let Some(Some(parents)) = graph.parents.get_mut(&owner.id) else {
            continue;
        };
        let parent = Parent {
            target: target.id,
            header,
        };
        if header.role == OccurrenceRole::ImplementationUse
            || matches!(owner.kind, EntityKind::Interface | EntityKind::Trait)
        {
            parents.interfaces.push(parent);
        } else if parents.superclass.replace(parent).is_some() {
            graph.parents.insert(owner.id, None);
        }
    }
    graph.complete_hierarchies(cancellation)?;
    let mut cache = BTreeMap::new();
    let mut remaining = MAX_RESOLUTION_WORK_LIMIT;
    let mut duplicated = BTreeSet::new();
    for envelope in extensions
        .iter()
        .filter(|item| item.namespace == rootlight_ir::GROOVY_TYPE_LOOKUP_NAMESPACE)
    {
        cancellation.check()?;
        let Some(source) = envelope.evidence.source.as_ref() else {
            continue;
        };
        let Some(site) = by_source.get(&source.span()).copied().flatten() else {
            continue;
        };
        if output.remove(&site.id).is_some() {
            duplicated.insert(site.id);
        }
        if duplicated.contains(&site.id) {
            continue;
        }
        let Ok(lookup) = rootlight_ir::decode_groovy_type_lookup_envelope(envelope) else {
            continue;
        };
        let Some(owner) = owners.get(&lookup.owner).copied().flatten() else {
            continue;
        };
        let Some(context) = index.contexts.get(&site.file) else {
            continue;
        };
        if site.role != OccurrenceRole::TypeUse
            || lookup.fallback_local.is_some()
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
            || !matches!(
                site.syntax_kind.as_str(),
                "groovy.type_identifier.reference" | "groovy.qualified_type.reference"
            )
            || site.source != *source
            || envelope.provenance != context.file.provenance
            || site.provenance != context.file.provenance
            || rootlight_ids::content_hash(lookup.name.as_bytes()) != site.syntactic_text_hash
            || source.content_hash() != context.file.content_hash
            || lookup.owner.end_byte() > context.file.byte_length
            || !graph.owns_site(owner.id, site, cancellation)?
        {
            continue;
        }
        let key = (
            owner.id,
            lookup.name.clone(),
            lookup
                .fallback_import
                .map(|item| (item.import, item.target)),
        );
        let selection = if let Some(selection) = cache.get(&key) {
            Arc::clone(selection)
        } else {
            let selection = Arc::new(graph.select(
                owner,
                &lookup,
                source,
                &by_source,
                &mut remaining,
                cancellation,
            )?);
            cache.insert(key, Arc::clone(&selection));
            selection
        };
        output.insert(
            site.id,
            Entry {
                envelope,
                selection,
            },
        );
    }
    Ok(output)
}

impl<'a> Graph<'a, '_, '_> {
    fn header_target(&self, header: &OccurrenceRecord) -> Option<&'a EntityRecord> {
        if !matches!(
            header.syntax_kind.as_str(),
            "groovy.extends_type.reference" | "groovy.implements_type.reference"
        ) {
            return None;
        }
        let context = self.index.contexts.get(&header.file)?;
        if !self.index.has_source_owned_provenance(header)
            || header.source.content_hash() != context.file.content_hash
        {
            return None;
        }
        if let OccurrenceTarget::Resolved { symbol } = header.target {
            if header.provenance != context.file.provenance {
                return self
                    .index
                    .proof(header)
                    .map(|proof| proof.target)
                    .filter(|target| target.id == symbol);
            }
            return self
                .entities
                .get(&symbol)
                .copied()
                .filter(|entity| is_type(entity.kind) && entity.language == "groovy");
        }
        self.index.proof(header).map(|proof| proof.target)
    }

    fn complete_hierarchies(&mut self, cancellation: &Cancellation) -> Result<(), ResolutionError> {
        let mut pending = BTreeMap::new();
        let mut dependents = BTreeMap::<SymbolId, Vec<SymbolId>>::new();
        let mut ready = VecDeque::new();
        for (&owner, parents) in &self.parents {
            cancellation.check()?;
            let Some(parents) = parents else { continue };
            let mut count = 0_usize;
            for parent in parents.all() {
                cancellation.check()?;
                count = count.checked_add(1).ok_or(ResolutionError::CountOverflow)?;
                dependents.entry(parent.target).or_default().push(owner);
            }
            pending.insert(owner, count);
            if count == 0 {
                ready.push_back(owner);
            }
        }
        while let Some(owner) = ready.pop_front() {
            cancellation.check()?;
            self.complete.insert(owner);
            for child in dependents.get(&owner).into_iter().flatten() {
                cancellation.check()?;
                if let Some(count) = pending.get_mut(child) {
                    *count = count.checked_sub(1).ok_or(ResolutionError::CountOverflow)?;
                    if *count == 0 {
                        ready.push_back(*child);
                    }
                }
            }
        }
        Ok(())
    }

    fn owns_site(
        &self,
        owner: SymbolId,
        site: &OccurrenceRecord,
        cancellation: &Cancellation,
    ) -> Result<bool, ResolutionError> {
        let mut current = site.enclosing;
        for _ in 0..crate::engine::MAX_SCOPE_DEPTH {
            cancellation.check()?;
            let Some(entity) = current.and_then(|id| self.entities.get(&id)).copied() else {
                return Ok(false);
            };
            if is_type(entity.kind) {
                return Ok(entity.id == owner);
            }
            current = match entity.container {
                Some(ContainerRef::Entity(parent)) => Some(parent),
                _ => None,
            };
        }
        Ok(false)
    }

    fn select(
        &self,
        owner: &'a EntityRecord,
        lookup: &GroovyTypeLookup,
        source: &SourceRef,
        by_source: &BTreeMap<SourceSpan, Option<&OccurrenceRecord>>,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<Selection<'a>, ResolutionError> {
        let mut selection = Selection::default();
        if !self.complete.contains(&owner.id) {
            return Ok(selection);
        }
        let mut owners = vec![owner];
        let mut current = owner;
        let mut module = None;
        for _ in 0..crate::engine::MAX_SCOPE_DEPTH {
            step(&mut selection, remaining, cancellation)?;
            let Some(ContainerRef::Entity(parent)) = current.container else {
                break;
            };
            let Some(parent) = self.entities.get(&parent).copied() else {
                return Ok(selection);
            };
            if parent.kind == EntityKind::Module {
                module = Some(parent);
                break;
            }
            if !is_type(parent.kind) {
                return Ok(selection);
            }
            owners.push(parent);
            current = parent;
        }
        for &owner in owners.iter().take(1).chain(owners.iter().skip(1).rev()) {
            if !self.complete.contains(&owner.id) {
                return Ok(selection);
            }
            if let Some(target) = self.hierarchy(
                owner.id,
                &lookup.name,
                owners.first().is_some_and(|first| first.id == owner.id),
                &mut selection,
                remaining,
                cancellation,
            )? {
                selection.target = Some(target);
                return Ok(selection);
            }
            if selection.uncertain {
                return Ok(selection);
            }
        }
        if let Some(module) = module
            && let Some(target) = self.member(
                module.id,
                &lookup.name,
                &mut selection,
                remaining,
                cancellation,
            )?
        {
            selection.target = Some(target);
            return Ok(selection);
        }
        if selection.uncertain {
            return Ok(selection);
        }
        if let Some(import) = lookup.fallback_import
            && let Some(site) = by_source.get(&import.import).copied().flatten()
            && site.source.content_hash() == source.content_hash()
            && site.role == OccurrenceRole::ImportUse
            && site.syntactic_text_hash == import.target
            && let Some(proof) = self.index.proof(site)
        {
            self.record_header(site, &mut selection);
            self.record_entity(proof.target, &mut selection);
            selection.target = Some(proof.target);
        }
        Ok(selection)
    }

    fn hierarchy(
        &self,
        origin: SymbolId,
        name: &str,
        superclass: bool,
        selection: &mut Selection<'a>,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<Option<&'a EntityRecord>, ResolutionError> {
        let mut current = origin;
        loop {
            step(selection, remaining, cancellation)?;
            if let Some(target) = self.member(current, name, selection, remaining, cancellation)? {
                if current == origin
                    || matches!(
                        target.visibility,
                        EntityVisibility::Public | EntityVisibility::Restricted
                    )
                {
                    return Ok(Some(target));
                }
                if target.visibility == EntityVisibility::Unknown {
                    selection.uncertain = true;
                    return Ok(None);
                }
            }
            if selection.uncertain {
                return Ok(None);
            }
            let Some(Some(parents)) = self.parents.get(&current) else {
                return Ok(None);
            };
            let mut stack: Vec<_> = parents.interfaces.iter().rev().collect();
            let mut visited = BTreeSet::new();
            while let Some(parent) = stack.pop() {
                step(selection, remaining, cancellation)?;
                self.record_header(parent.header, selection);
                if !visited.insert(parent.target) {
                    continue;
                }
                if let Some(target) =
                    self.member(parent.target, name, selection, remaining, cancellation)?
                {
                    return Ok(Some(target));
                }
                if selection.uncertain {
                    return Ok(None);
                }
                if let Some(Some(parents)) = self.parents.get(&parent.target) {
                    stack.extend(parents.interfaces.iter().rev());
                }
            }
            let Some(parent) = parents.superclass.as_ref().filter(|_| superclass) else {
                return Ok(None);
            };
            self.record_header(parent.header, selection);
            current = parent.target;
        }
    }

    fn member(
        &self,
        mut owner: SymbolId,
        name: &str,
        selection: &mut Selection<'a>,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<Option<&'a EntityRecord>, ResolutionError> {
        let mut target = None;
        for component in name.split('.') {
            step(selection, remaining, cancellation)?;
            if let Some(entity) = self.entities.get(&owner) {
                self.record_entity(entity, selection);
            }
            let Some(child) = self
                .children
                .get(&owner)
                .and_then(|children| children.get(component))
            else {
                return Ok(None);
            };
            let Some(child) = *child else {
                selection.uncertain = true;
                return Ok(None);
            };
            self.record_entity(child, selection);
            owner = child.id;
            target = Some(child);
        }
        Ok(target)
    }

    fn record_entity(&self, entity: &EntityRecord, selection: &mut Selection<'a>) {
        selection.facts.insert(FactRef::Entity(entity.id));
        if let Some(source) = &entity.evidence.source {
            selection.sources.insert(source.clone());
            if let Some(context) = self.index.contexts.get(&source.span().file()) {
                for coverage in context
                    .coverage
                    .into_iter()
                    .chain(self.coverage.get(&source.span().file()).copied())
                {
                    selection.facts.insert(FactRef::Fact(coverage.id));
                    if let Some(source) = &coverage.evidence.source {
                        selection.sources.insert(source.clone());
                    }
                }
            }
        }
    }

    fn record_header(&self, header: &OccurrenceRecord, selection: &mut Selection<'a>) {
        // Header occurrence IDs change in this pass; exact source and immutable
        // parser envelopes retain the proof without referencing replaced IDs.
        selection.sources.insert(header.source.clone());
        if let Some(proof) = self.index.proof(header) {
            if let Some((envelope, owner)) = proof.qualified_header {
                selection.facts.insert(FactRef::Fact(envelope.id));
                self.record_entity(owner, selection);
            }
            for owner in proof.owners {
                self.record_entity(owner, selection);
            }
            if let Some(binding) = proof.binding {
                selection.facts.insert(FactRef::Fact(binding.id));
                if let Ok(import) = rootlight_ir::decode_groovy_binding_envelope(binding) {
                    selection.sources.insert(SourceRef::new(
                        header.repository,
                        header.generation,
                        import.import,
                        header.source.content_hash(),
                        None,
                    ));
                }
            }
        }
    }
}

fn step(
    selection: &mut Selection<'_>,
    remaining: &mut usize,
    cancellation: &Cancellation,
) -> Result<(), ResolutionError> {
    cancellation.check()?;
    *remaining = remaining.checked_sub(1).ok_or(ResolutionError::WorkLimit {
        maximum: MAX_RESOLUTION_WORK_LIMIT,
    })?;
    selection.work = selection
        .work
        .checked_add(1)
        .ok_or(ResolutionError::CountOverflow)?;
    Ok(())
}
