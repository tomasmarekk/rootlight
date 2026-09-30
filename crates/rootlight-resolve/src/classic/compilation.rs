//! Conditional activity gates over verified, immutable classic BASIC sources.
//! Unknown declarations remain lookup barriers; only excluded declarations are
//! removed from inventories, and existing parser-owned facts are never rewritten.

use super::*;
use rootlight_ir::{
    ExtensionEnvelope, SourceSpan, VBA_COMPILATION_NAMESPACE, VbaCompilation,
    VbaCompilationActivity, decode_vba_compilation_envelope,
};

#[cfg(test)]
mod tests;

#[derive(Default)]
pub(super) struct CompilationIndex {
    files: BTreeMap<FileId, FileActivity>,
    declarations: BTreeMap<SymbolId, VbaCompilationActivity>,
    uses: BTreeSet<FactId>,
    declaration_sources: BTreeSet<SourceRef>,
}

struct FileActivity {
    source: SourceRef,
    conditional: bool,
    blocked: bool,
    inventories: Vec<Inventory>,
    maximum_activity_work: usize,
}

struct Inventory {
    id: FactId,
    model: VbaCompilation,
    activity: ActivityIndex,
}

// A per-use branch scan can exhaust work and discard the entire resolution
// transaction. Compile intervals once so lookup is logarithmic in inventory size.
struct ActivityIndex {
    inactive: Intervals,
    unproven: Intervals,
    gaps: Intervals,
}

#[derive(Default)]
struct Intervals {
    prefixes: Vec<(u64, u64)>,
}

impl Intervals {
    fn build(
        ranges: Vec<(u64, u64)>,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Self, ResolutionError> {
        let mut ordered = BTreeMap::<u64, u64>::new();
        for (start, end) in ranges {
            cancellation.check()?;
            work.consume()?;
            ordered
                .entry(start)
                .and_modify(|prior| *prior = (*prior).max(end))
                .or_insert(end);
        }
        let mut prefixes = Vec::new();
        let mut maximum_end = 0;
        for (start, end) in ordered {
            cancellation.check()?;
            work.consume()?;
            maximum_end = maximum_end.max(end);
            prefixes.push((start, maximum_end));
        }
        Ok(Self { prefixes })
    }

    fn matches<E>(
        &self,
        boundary: u64,
        inclusive: bool,
        end_boundary: u64,
        inclusive_end: bool,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<bool, E> {
        let mut left = 0;
        let mut right = self.prefixes.len();
        while left < right {
            check()?;
            let middle = left + (right - left) / 2;
            let (start, _) = self.prefixes[middle];
            if start < boundary || (inclusive && start == boundary) {
                left = middle + 1;
            } else {
                right = middle;
            }
        }
        let Some(index) = left.checked_sub(1) else {
            return Ok(false);
        };
        check()?;
        let (_, maximum_end) = self.prefixes[index];
        Ok(maximum_end > end_boundary || (inclusive_end && maximum_end == end_boundary))
    }

    fn contains<E>(
        &self,
        span: SourceSpan,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<bool, E> {
        self.matches(span.start_byte(), true, span.end_byte(), true, check)
    }

    fn overlaps<E>(
        &self,
        span: SourceSpan,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<bool, E> {
        self.matches(span.end_byte(), false, span.start_byte(), false, check)
    }

    fn maximum_work(&self) -> usize {
        if self.prefixes.is_empty() {
            0
        } else {
            usize::try_from(usize::BITS - self.prefixes.len().leading_zeros())
                .expect("usize bit count fits usize")
                + 1
        }
    }
}

impl ActivityIndex {
    fn build(
        model: &VbaCompilation,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Self, ResolutionError> {
        let mut inactive = Vec::new();
        let mut unproven = Vec::new();
        let mut gaps = Vec::new();
        let mut current: Option<(SourceSpan, u64)> = None;
        for branch in &model.branches {
            cancellation.check()?;
            work.consume()?;
            if let Some((group, end)) = current.filter(|(group, _)| *group != branch.group) {
                gaps.push((end, group.end_byte()));
                current = None;
            }
            let preceding = current.map_or(branch.group.start_byte(), |(_, end)| end);
            // Empty gaps retain strict crossing barriers between adjacent bodies.
            // A span ending or starting at the barrier still belongs to its body.
            gaps.push((preceding, branch.region.start_byte()));
            current = Some((branch.group, branch.region.end_byte()));
            match branch.activity {
                VbaCompilationActivity::Inactive => {
                    inactive.push((branch.region.start_byte(), branch.region.end_byte()));
                }
                VbaCompilationActivity::Unproven => {
                    unproven.push((branch.region.start_byte(), branch.region.end_byte()));
                }
                VbaCompilationActivity::Active => {}
            }
        }
        if let Some((group, end)) = current {
            gaps.push((end, group.end_byte()));
        }
        Ok(Self {
            inactive: Intervals::build(inactive, cancellation, work)?,
            unproven: Intervals::build(unproven, cancellation, work)?,
            gaps: Intervals::build(gaps, cancellation, work)?,
        })
    }

    fn activity<E>(
        &self,
        model: &VbaCompilation,
        span: SourceSpan,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<VbaCompilationActivity, E> {
        if !model.complete || !contains(model.scope, span) {
            return Ok(VbaCompilationActivity::Unproven);
        }
        // Whole-span exclusion outranks nested directive and alternative gaps.
        if self.inactive.contains(span, check)? {
            return Ok(VbaCompilationActivity::Inactive);
        }
        if self.unproven.contains(span, check)? || self.gaps.overlaps(span, check)? {
            return Ok(VbaCompilationActivity::Unproven);
        }
        Ok(VbaCompilationActivity::Active)
    }

    fn maximum_work(&self) -> usize {
        self.inactive.maximum_work() + self.unproven.maximum_work() + self.gaps.maximum_work()
    }
}

impl CompilationIndex {
    pub(super) fn build(
        index: &CandidateIndex<'_>,
        extensions: &[ExtensionEnvelope],
        occurrences: &[OccurrenceRecord],
        sources: &BTreeMap<FileId, &[u8]>,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<Self, ResolutionError> {
        let mut result = Self::default();
        for (&id, bytes) in sources {
            cancellation.check()?;
            work.consume()?;
            let Some(file) = index
                .files
                .get(&id)
                .filter(|file| matches!(file.language.as_str(), "vba" | "visual-basic-classic"))
            else {
                continue;
            };
            let Some(source) = file.evidence.source.as_ref().filter(|source| {
                source.repository() == file.repository
                    && source.generation() == file.generation
                    && source.span().file() == id
                    && source.content_hash() == file.content_hash
                    && source.span().start_byte() == 0
                    && source.span().end_byte() == file.byte_length
            }) else {
                continue;
            };
            let mut conditional = false;
            for line in bytes.split(|byte| *byte == b'\n') {
                cancellation.check()?;
                work.consume_many(line.len().div_ceil(64).max(1))?;
                let line = line.trim_ascii_start();
                if let Some(directive) = line.strip_prefix(b"#") {
                    let word = directive
                        .trim_ascii_start()
                        .split(u8::is_ascii_whitespace)
                        .next()
                        .unwrap_or_default();
                    // #Const names are compile-time-only declarations, not
                    // conditional runtime membership. Other missing directives
                    // cannot be silently interpreted as an ordinary file.
                    conditional |= !word.eq_ignore_ascii_case(b"Const");
                }
            }
            result.files.insert(
                id,
                FileActivity {
                    source: source.clone(),
                    conditional,
                    blocked: false,
                    inventories: Vec::new(),
                    maximum_activity_work: 0,
                },
            );
        }
        for envelope in extensions
            .iter()
            .filter(|envelope| envelope.namespace == VBA_COMPILATION_NAMESPACE)
        {
            cancellation.check()?;
            work.consume()?;
            let Some(source) = &envelope.evidence.source else {
                for file in result.files.values_mut() {
                    file.blocked = true;
                }
                continue;
            };
            let Some(file) = result.files.get_mut(&source.span().file()) else {
                continue;
            };
            work.consume_many(envelope.payload.len().div_ceil(64).max(1))?;
            let compilation = decode_vba_compilation_envelope(envelope);
            let parser = index.provenance.get(&envelope.provenance);
            if let Some(parser) = parser {
                work.consume_many(parser.input_sources.len())?;
            }
            let valid = same_identity(&file.source, source)
                && contains(file.source.span(), source.span())
                && parser.is_some_and(|parser| {
                    parser.repository == source.repository()
                        && parser.generation == source.generation()
                        && parser.producer_kind == ProducerKind::Parser
                        && index
                            .files
                            .get(&source.span().file())
                            .is_some_and(|file| parser.language == file.language)
                        && parser.input_sources.iter().any(|input| {
                            same_identity(input, source) && contains(input.span(), source.span())
                        })
                });
            let Ok(compilation) = compilation else {
                file.blocked = true;
                continue;
            };
            work.consume_many(compilation.branches.len().max(1))?;
            if !valid
                || file
                    .inventories
                    .iter()
                    .any(|prior| overlaps(prior.model.scope, compilation.scope))
            {
                file.blocked = true;
                continue;
            }
            let activity = ActivityIndex::build(&compilation, cancellation, work)?;
            file.maximum_activity_work = file.maximum_activity_work.max(activity.maximum_work());
            file.inventories.push(Inventory {
                id: envelope.id,
                model: compilation,
                activity,
            });
        }
        let mut definitions: BTreeMap<SymbolId, Option<&SourceRef>> = BTreeMap::new();
        for occurrence in occurrences {
            cancellation.check()?;
            work.consume()?;
            if occurrence.role == OccurrenceRole::Definition
                && let OccurrenceTarget::Resolved { symbol } = occurrence.target
            {
                definitions
                    .entry(symbol)
                    .and_modify(|prior| {
                        if *prior != Some(&occurrence.source) {
                            *prior = None;
                        }
                    })
                    .or_insert(Some(&occurrence.source));
            }
        }
        for entity in index.entities.values() {
            cancellation.check()?;
            work.consume()?;
            let activity = if !matches!(entity.language.as_str(), "vba" | "visual-basic-classic") {
                VbaCompilationActivity::Active
            } else if let Some(source) = &entity.evidence.source {
                let declaration = match definitions.get(&entity.id) {
                    Some(Some(definition))
                        if same_identity(source, definition)
                            && contains(source.span(), definition.span()) =>
                    {
                        *definition
                    }
                    Some(_) => {
                        result
                            .declarations
                            .insert(entity.id, VbaCompilationActivity::Unproven);
                        continue;
                    }
                    None => source,
                };
                let activity = result.activity_with_work(declaration, cancellation, work)?;
                if activity == VbaCompilationActivity::Unproven
                    && matches!(entity.kind, EntityKind::Module | EntityKind::Class)
                    && !matches!(entity.container, Some(ContainerRef::Entity(_)))
                    && result.module_scope_complete(source)
                {
                    // A source-written module is the lexical container of its
                    // groups, not a declaration selected by an inner branch.
                    VbaCompilationActivity::Active
                } else if activity == VbaCompilationActivity::Unproven
                    && matches!(
                        entity.kind,
                        EntityKind::Function | EntityKind::Method | EntityKind::Property
                    )
                {
                    let bytes = sources.get(&source.span().file()).and_then(|bytes| {
                        bytes.get(
                            usize::try_from(source.span().start_byte()).ok()?
                                ..usize::try_from(source.span().end_byte()).ok()?,
                        )
                    });
                    let header = if let Some(bytes) = bytes {
                        rootlight_vba::procedure_header(bytes, &mut || {
                            cancellation.check()?;
                            work.consume()
                        })?
                    } else {
                        None
                    };
                    if let Some(header) = header.filter(|header| {
                        header
                            .name()
                            .eq_ignore_ascii_case(entity.canonical_name.as_bytes())
                    }) {
                        let name = header.name_span();
                        let start = source
                            .span()
                            .start_byte()
                            .checked_add(
                                u64::try_from(name.start)
                                    .map_err(|_| ResolutionError::CountOverflow)?,
                            )
                            .ok_or(ResolutionError::CountOverflow)?;
                        let end = source
                            .span()
                            .start_byte()
                            .checked_add(
                                u64::try_from(name.end)
                                    .map_err(|_| ResolutionError::CountOverflow)?,
                            )
                            .ok_or(ResolutionError::CountOverflow)?;
                        result.activity_with_work(
                            &SourceRef::new(
                                source.repository(),
                                source.generation(),
                                SourceSpan::new(source.span().file(), start, end)
                                    .map_err(ResolutionError::InvalidProducer)?,
                                source.content_hash(),
                                None,
                            ),
                            cancellation,
                            work,
                        )?
                    } else {
                        activity
                    }
                } else {
                    activity
                }
            } else if index
                .provenance
                .get(&entity.provenance)
                .is_some_and(|parser| parser.rule.as_deref() == Some(standard::CATALOG_RULE))
            {
                VbaCompilationActivity::Active
            } else {
                VbaCompilationActivity::Unproven
            };
            result.declarations.insert(entity.id, activity);
        }
        // A child cannot acquire membership excluded or unknown at an owner.
        // The bounded parent walk also fails closed on malformed cycles.
        let own = result.declarations.clone();
        for entity in index.entities.values() {
            cancellation.check()?;
            work.consume()?;
            let mut activity = own[&entity.id];
            let mut owner = entity.container;
            let mut remaining = index.entities.len();
            while let Some(ContainerRef::Entity(id)) = owner {
                cancellation.check()?;
                work.consume()?;
                if remaining == 0 {
                    activity = VbaCompilationActivity::Unproven;
                    break;
                }
                remaining -= 1;
                let Some(parent) = index.entities.get(&id) else {
                    activity = VbaCompilationActivity::Unproven;
                    break;
                };
                activity = combine(activity, own[&id]);
                owner = parent.container;
            }
            result.declarations.insert(entity.id, activity);
            if activity == VbaCompilationActivity::Active
                && let Some(source) = &entity.evidence.source
            {
                result.declaration_sources.insert(source.clone());
            }
        }
        for occurrence in occurrences {
            cancellation.check()?;
            if result.activity_with_work(&occurrence.source, cancellation, work)?
                == VbaCompilationActivity::Active
                && occurrence
                    .enclosing
                    .is_none_or(|id| result.entity_active(id))
                && match occurrence.target {
                    OccurrenceTarget::Resolved { symbol } => result.entity_active(symbol),
                    _ => true,
                }
            {
                result.uses.insert(occurrence.id);
            }
        }
        Ok(result)
    }

    pub(super) fn entity_active(&self, id: SymbolId) -> bool {
        self.declarations.get(&id) == Some(&VbaCompilationActivity::Active)
    }

    pub(super) fn entity_inactive(&self, id: SymbolId) -> bool {
        self.declarations.get(&id) == Some(&VbaCompilationActivity::Inactive)
    }

    pub(super) fn use_active(&self, id: FactId) -> bool {
        self.uses.contains(&id)
    }

    pub(super) fn support_active(&self, source: &SourceRef) -> bool {
        // A full declaration is retained as source.read context; its runtime
        // membership is proved by the definition token, not every body byte.
        self.declaration_sources.contains(source)
            || self.activity(source) == VbaCompilationActivity::Active
    }

    pub(super) fn activity(&self, source: &SourceRef) -> VbaCompilationActivity {
        match self.activity_checked(source, &mut || Ok::<(), std::convert::Infallible>(())) {
            Ok(activity) => activity,
            Err(error) => match error {},
        }
    }

    fn activity_with_work(
        &self,
        source: &SourceRef,
        cancellation: &Cancellation,
        work: &mut ResolutionWorkBudget,
    ) -> Result<VbaCompilationActivity, ResolutionError> {
        self.activity_checked(source, &mut || {
            cancellation.check()?;
            work.consume()
        })
    }

    fn activity_checked<E>(
        &self,
        source: &SourceRef,
        check: &mut impl FnMut() -> Result<(), E>,
    ) -> Result<VbaCompilationActivity, E> {
        check()?;
        let Some(file) = self.files.get(&source.span().file()) else {
            return Ok(VbaCompilationActivity::Unproven);
        };
        if file.blocked
            || !same_identity(&file.source, source)
            || !contains(file.source.span(), source.span())
        {
            return Ok(VbaCompilationActivity::Unproven);
        }
        for inventory in &file.inventories {
            check()?;
            if contains(inventory.model.scope, source.span()) {
                return inventory
                    .activity
                    .activity(&inventory.model, source.span(), check);
            }
        }
        if file.conditional {
            return Ok(VbaCompilationActivity::Unproven);
        }
        for inventory in &file.inventories {
            check()?;
            if overlaps(inventory.model.scope, source.span()) {
                return Ok(VbaCompilationActivity::Unproven);
            }
        }
        Ok(VbaCompilationActivity::Active)
    }

    pub(super) fn source_cost(&self, source: &SourceRef) -> usize {
        // Pure proof/support lookups lack a work callback. Their callers reserve
        // the binary-search bound; importer queries charge actual comparisons.
        self.files.get(&source.span().file()).map_or(1, |file| {
            file.maximum_activity_work + 2 * file.inventories.len() + 1
        })
    }

    fn module_scope_complete(&self, source: &SourceRef) -> bool {
        self.files.get(&source.span().file()).is_some_and(|file| {
            !file.blocked
                && same_identity(&file.source, source)
                && file.inventories.iter().any(|inventory| {
                    inventory.model.complete && inventory.model.scope == source.span()
                })
        })
    }

    pub(super) fn evidence(&self, file: FileId) -> impl Iterator<Item = FactRef> + '_ {
        self.files
            .get(&file)
            .into_iter()
            .filter(|file| !file.blocked)
            .flat_map(|file| {
                file.inventories
                    .iter()
                    .map(|inventory| FactRef::Fact(inventory.id))
            })
    }
}

fn same_identity(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

fn contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    outer.file() == inner.file()
        && inner.start_byte() < inner.end_byte()
        && outer.start_byte() <= inner.start_byte()
        && inner.end_byte() <= outer.end_byte()
}

fn overlaps(left: SourceSpan, right: SourceSpan) -> bool {
    left.file() == right.file()
        && left.start_byte() < right.end_byte()
        && right.start_byte() < left.end_byte()
}

fn combine(left: VbaCompilationActivity, right: VbaCompilationActivity) -> VbaCompilationActivity {
    match (left, right) {
        (VbaCompilationActivity::Inactive, _) | (_, VbaCompilationActivity::Inactive) => {
            VbaCompilationActivity::Inactive
        }
        (VbaCompilationActivity::Active, VbaCompilationActivity::Active) => {
            VbaCompilationActivity::Active
        }
        _ => VbaCompilationActivity::Unproven,
    }
}
