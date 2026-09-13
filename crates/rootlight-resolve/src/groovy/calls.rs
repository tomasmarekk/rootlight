//! Static-import, value and call selection over native declaration evidence.
//! Results identify authored declarations in the admitted snapshot, not runtime
//! dispatch. Imports retain overload sets; calls require a unique eligible target.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::Arc;

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, CoverageRecord, CoverageScope, CoverageStatus, EntityKind, EntityRecord,
    EntityVisibility, ExtensionEnvelope, FactDomain, FactRef, GroovyCallLookup, GroovyCallable,
    GroovyCallableDispatch, GroovyCallableVisibility, GroovyStaticImport, OccurrenceRecord,
    OccurrenceRole, OccurrenceTarget, SourceRef, SourceSpan,
};

use super::hierarchy::{CandidateSet, Entry, Selection};
use super::{GroovyIndex, is_type};
use crate::{MAX_CANDIDATE_LIMIT, MAX_RESOLUTION_WORK_LIMIT, ResolutionError};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Scope {
    File(FileId),
    Type(SymbolId),
}

#[derive(Clone, Copy)]
enum ValueAccess {
    Read,
    Assignment,
}

impl ValueAccess {
    fn prefixes(self) -> &'static [&'static str] {
        match self {
            Self::Read => &["is", "get"],
            Self::Assignment => &["set"],
        }
    }

    fn arity(self) -> u32 {
        match self {
            Self::Read => 0,
            Self::Assignment => 1,
        }
    }
}

enum ValueName<'a> {
    Property(&'a str),
    Accessor(&'a str),
}

struct Method<'a> {
    envelope: &'a ExtensionEnvelope,
    shape: GroovyCallable,
}

struct Member<'a> {
    envelope: &'a ExtensionEnvelope,
    shape: rootlight_ir::GroovyMember,
}

struct Parent {
    target: SymbolId,
    sources: BTreeSet<SourceRef>,
    facts: BTreeSet<FactRef>,
}

struct ImportedCall<'a> {
    envelope: &'a ExtensionEnvelope,
    import: &'a GroovyStaticImport,
    name: &'a str,
    arguments: u32,
    accessor: bool,
}

struct Graph<'a, 'index> {
    index: &'index GroovyIndex<'a>,
    entities: &'index BTreeMap<SymbolId, &'a EntityRecord>,
    coverage: BTreeMap<FileId, &'a CoverageRecord>,
    invalid: BTreeSet<FileId>,
    methods: BTreeMap<SymbolId, Option<Method<'a>>>,
    members: BTreeMap<SymbolId, Option<Member<'a>>>,
    names: BTreeMap<(Scope, ContentHash), Vec<&'a EntityRecord>>,
    imports: BTreeMap<FileId, Vec<(&'a ExtensionEnvelope, GroovyStaticImport)>>,
    parents: BTreeMap<SymbolId, Option<Vec<Parent>>>,
    complete: BTreeSet<SymbolId>,
}

pub(super) fn build<'a>(
    index: &GroovyIndex<'a>,
    entities: &BTreeMap<SymbolId, &'a EntityRecord>,
    coverage: &'a [CoverageRecord],
    extensions: &'a [ExtensionEnvelope],
    occurrences: &[OccurrenceRecord],
    cancellation: &Cancellation,
) -> Result<BTreeMap<FactId, Entry<'a>>, ResolutionError> {
    cancellation.check()?;
    let mut output = BTreeMap::new();
    if index.unavailable {
        return Ok(output);
    }
    let mut graph = Graph {
        index,
        entities,
        coverage: BTreeMap::new(),
        invalid: BTreeSet::new(),
        methods: BTreeMap::new(),
        members: BTreeMap::new(),
        names: BTreeMap::new(),
        imports: BTreeMap::new(),
        parents: BTreeMap::new(),
        complete: BTreeSet::new(),
    };
    let mut counts = BTreeMap::<FileId, u64>::new();
    let mut sites = BTreeMap::new();
    let mut static_names = BTreeMap::<FileId, BTreeSet<SourceSpan>>::new();
    for site in occurrences {
        cancellation.check()?;
        let Some(context) = index.contexts.get(&site.file) else {
            continue;
        };
        if !index.has_source_owned_provenance(site)
            || site.source.content_hash() != context.file.content_hash
        {
            graph.invalid.insert(site.file);
        }
        let count = counts.entry(site.file).or_default();
        *count = count.checked_add(1).ok_or(ResolutionError::CountOverflow)?;
        sites
            .entry((site.source.span(), site.role))
            .and_modify(|prior| *prior = None)
            .or_insert(Some(site));
        if matches!(
            site.syntax_kind.as_str(),
            "groovy.static_import.reference" | "groovy.static_star_import.reference"
        ) {
            static_names
                .entry(site.file)
                .or_default()
                .insert(site.source.span());
        }
    }
    for record in coverage {
        cancellation.check()?;
        let CoverageScope::File(file) = record.scope else {
            continue;
        };
        if record.domain != FactDomain::Occurrences {
            continue;
        }
        let Some(context) = index.contexts.get(&file) else {
            continue;
        };
        if record.status != CoverageStatus::Complete
            || record.skipped != 0
            || record.discovered != record.indexed
            || record.indexed != counts.get(&file).copied().unwrap_or(0)
            || record.provenance != context.file.provenance
            || record.evidence.source != context.file.evidence.source
            || graph.coverage.insert(file, record).is_some()
        {
            graph.invalid.insert(file);
        }
    }
    let mut shapes = BTreeMap::new();
    let mut member_shapes = BTreeMap::new();
    for envelope in extensions {
        cancellation.check()?;
        let Some(source) = envelope.evidence.source.as_ref() else {
            continue;
        };
        let file = source.span().file();
        let valid = graph.valid_envelope(envelope);
        if envelope.namespace == rootlight_ir::GROOVY_MEMBER_NAMESPACE {
            if let Ok(shape) = rootlight_ir::decode_groovy_member_envelope(envelope) {
                member_shapes
                    .entry(shape.declaration)
                    .and_modify(|prior| *prior = None)
                    .or_insert_with(|| valid.then_some(Member { envelope, shape }));
            } else {
                graph.invalid.insert(file);
            }
        }
        if envelope.namespace == rootlight_ir::GROOVY_CALLABLE_NAMESPACE {
            let shape = rootlight_ir::decode_groovy_callable_envelope(envelope)
                .ok()
                .filter(|shape| {
                    valid
                        && sites
                            .get(&(shape.name, OccurrenceRole::Definition))
                            .copied()
                            .flatten()
                            .is_some()
                });
            shapes
                .entry(source.span())
                .and_modify(|prior| *prior = None)
                .or_insert(shape.map(|shape| Method { envelope, shape }));
        }
        if envelope.namespace == rootlight_ir::GROOVY_STATIC_IMPORT_NAMESPACE {
            let import = rootlight_ir::decode_groovy_static_import_envelope(envelope)
                .ok()
                .filter(|import| {
                    valid
                        && sites
                            .get(&(source.span(), OccurrenceRole::ImportUse))
                            .copied()
                            .flatten()
                            .is_some_and(|site| {
                                site.syntax_kind == "groovy.import.import" && site.source == *source
                            })
                        && static_names
                            .get_mut(&file)
                            .is_some_and(|names| names.remove(&import.name))
                });
            if let Some(import) = import {
                graph
                    .imports
                    .entry(file)
                    .or_default()
                    .push((envelope, import));
            } else {
                graph.invalid.insert(file);
            }
        }
    }
    for (file, names) in static_names {
        if !names.is_empty() {
            graph.invalid.insert(file);
        }
    }
    for imports in graph.imports.values_mut() {
        imports.sort_unstable_by_key(|(envelope, _)| {
            envelope.evidence.source.as_ref().map(SourceRef::span)
        });
    }
    for entity in entities
        .values()
        .copied()
        .filter(|e| e.language == "groovy")
    {
        cancellation.check()?;
        let Some(source) = entity.evidence.source.as_ref() else {
            continue;
        };
        let file = source.span().file();
        if is_type(entity.kind) {
            graph
                .parents
                .insert(entity.id, graph.valid_file(file).then(Vec::new));
        }
        if matches!(entity.kind, EntityKind::Function | EntityKind::Method) {
            let shape = shapes.remove(&source.span()).flatten().filter(|method| {
                method.envelope.evidence.source.as_ref() == Some(source)
                    && sites
                        .get(&(method.shape.name, OccurrenceRole::Definition))
                        .copied()
                        .flatten()
                        .is_some_and(|site| {
                            site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                                && callable_name_matches(
                                    method.shape.name_identity,
                                    site.syntactic_text_hash,
                                    &entity.canonical_name,
                                )
                        })
            });
            graph.methods.insert(entity.id, shape);
        }
        if matches!(entity.kind, EntityKind::Field | EntityKind::Property) {
            let owner = match graph.entity_scope(entity, cancellation)? {
                Scope::Type(id) => entities.get(&id).copied(),
                Scope::File(_) => None,
            };
            let shape = member_shapes
                .remove(&source.span())
                .flatten()
                .filter(|member| {
                    member.shape.visibility == entity.visibility
                        && owner.is_some_and(|owner| {
                            owner.provenance == entity.provenance
                                && owner.evidence.source.as_ref().is_some_and(|owner_source| {
                                    owner_source.span() == member.shape.owner
                                        && owner_source.content_hash() == source.content_hash()
                                })
                        })
                        && sites
                            .get(&(member.shape.name, OccurrenceRole::Definition))
                            .copied()
                            .flatten()
                            .is_some_and(|site| {
                                site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                                    && site.syntactic_text_hash
                                        == content_hash(entity.canonical_name.as_bytes())
                            })
                });
            graph.members.insert(entity.id, shape);
        }
        if matches!(
            entity.kind,
            EntityKind::Function | EntityKind::Method | EntityKind::Field | EntityKind::Property
        ) {
            let scope = graph.entity_scope(entity, cancellation)?;
            graph
                .names
                .entry((scope, content_hash(entity.canonical_name.as_bytes())))
                .or_default()
                .push(entity);
        }
    }
    graph.build_parents(occurrences, cancellation)?;
    let mut remaining = MAX_RESOLUTION_WORK_LIMIT;
    let mut cache = BTreeMap::new();
    let mut duplicated = BTreeSet::new();
    for envelope in extensions
        .iter()
        .filter(|e| e.namespace == rootlight_ir::GROOVY_CALL_LOOKUP_NAMESPACE)
    {
        cancellation.check()?;
        let Some(source) = envelope.evidence.source.as_ref() else {
            continue;
        };
        let Some(site) = sites
            .get(&(source.span(), OccurrenceRole::CallSite))
            .copied()
            .flatten()
        else {
            continue;
        };
        if output.remove(&site.id).is_some() {
            duplicated.insert(site.id);
        }
        if duplicated.contains(&site.id) {
            continue;
        }
        let Ok(lookup) = rootlight_ir::decode_groovy_call_lookup_envelope(envelope) else {
            continue;
        };
        if !graph.valid_envelope(envelope)
            || site.source != *source
            || site.syntax_kind != "groovy.implicit_call.call"
            || site.syntactic_text_hash != lookup.name_hash
        {
            continue;
        }
        let args = lookup.arguments;
        let key = (
            site.file,
            site.enclosing,
            lookup.name_hash,
            lookup.enclosing_type,
            lookup.enclosing_callable,
            args.positional,
            args.named,
            args.spread,
            args.trailing_closures,
            lookup.lexical_binding,
            lookup.static_context,
        );
        let selection = if let Some(selection) = cache.get(&key) {
            Arc::clone(selection)
        } else {
            let selection = Arc::new(graph.select(site, &lookup, &mut remaining, cancellation)?);
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
    for (&file, imports) in &graph.imports {
        cancellation.check()?;
        if !graph.valid_file(file) {
            continue;
        }
        for (envelope, import) in imports {
            cancellation.check()?;
            let Some(site) = sites
                .get(&(import.name, OccurrenceRole::ImportUse))
                .copied()
                .flatten()
            else {
                continue;
            };
            let syntax = if import.member.is_some() {
                "groovy.static_import.reference"
            } else {
                "groovy.static_star_import.reference"
            };
            if site.syntax_kind != syntax {
                continue;
            }
            let selection =
                graph.select_import(file, envelope, import, &mut remaining, cancellation)?;
            output.insert(
                site.id,
                Entry {
                    envelope,
                    selection: Arc::new(selection),
                },
            );
        }
    }
    let mut value_sites = BTreeSet::new();
    let mut duplicate_values = BTreeSet::new();
    let mut value_cache = BTreeMap::new();
    for envelope in extensions
        .iter()
        .filter(|e| e.namespace == rootlight_ir::GROOVY_VALUE_LOOKUP_NAMESPACE)
    {
        cancellation.check()?;
        let Some(source) = envelope.evidence.source.as_ref() else {
            continue;
        };
        let Some(site) = sites
            .get(&(source.span(), OccurrenceRole::Reference))
            .copied()
            .flatten()
        else {
            continue;
        };
        if !value_sites.insert(site.id) {
            output.remove(&site.id);
            duplicate_values.insert(site.id);
        }
        if duplicate_values.contains(&site.id) {
            continue;
        }
        let Ok(lookup) = rootlight_ir::decode_groovy_value_lookup_envelope(envelope) else {
            continue;
        };
        if !graph.valid_envelope(envelope)
            || site.source != *source
            || site.syntax_kind != "groovy.identifier.reference"
            || site.syntactic_text_hash != lookup.name_hash
        {
            continue;
        }
        let key = (
            site.file,
            site.enclosing,
            lookup.name_hash,
            lookup.enclosing_type,
            lookup.enclosing_callable,
            lookup.lexical_binding,
            lookup.static_context,
            lookup.read_only,
            lookup.write_kind,
        );
        let selection = if let Some(selection) = value_cache.get(&key) {
            Arc::clone(selection)
        } else {
            let selection =
                Arc::new(graph.select_value(site, &lookup, &mut remaining, cancellation)?);
            value_cache.insert(key, Arc::clone(&selection));
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

impl<'a> Graph<'a, '_> {
    fn select_value(
        &self,
        site: &OccurrenceRecord,
        lookup: &rootlight_ir::GroovyValueLookup,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<Selection<'a>, ResolutionError> {
        let mut selection = Selection::default();
        self.retain_file(site.file, &mut selection);
        let access = if lookup.read_only {
            ValueAccess::Read
        } else if lookup.write_kind == Some(rootlight_ir::GroovyValueWrite::Assignment) {
            ValueAccess::Assignment
        } else {
            return Ok(selection);
        };
        if lookup.lexical_binding {
            return Ok(selection);
        }
        let mut parent = site.enclosing;
        let mut types = Vec::new();
        let mut callable = None;
        for _ in 0..=self.entities.len() {
            step(&mut selection, remaining, cancellation)?;
            let Some(id) = parent else { break };
            let Some(entity) = self.entities.get(&id).copied() else {
                return Ok(selection);
            };
            let Some(source) = &entity.evidence.source else {
                return Ok(selection);
            };
            if !contains(source.span(), site.source.span()) {
                return Ok(selection);
            }
            self.retain_entity(entity, &mut selection);
            if is_type(entity.kind) {
                types.push(entity);
            }
            if callable.is_none()
                && matches!(
                    entity.kind,
                    EntityKind::Method | EntityKind::Function | EntityKind::Constructor
                )
            {
                callable = Some(entity);
            }
            parent = match entity.container {
                Some(ContainerRef::Entity(id)) => Some(id),
                _ => None,
            };
        }
        if parent.is_some()
            || types
                .first()
                .and_then(|e| e.evidence.source.as_ref())
                .map(SourceRef::span)
                != lookup.enclosing_type
            || callable
                .and_then(|e| e.evidence.source.as_ref())
                .map(SourceRef::span)
                != lookup.enclosing_callable
        {
            return Ok(selection);
        }
        if let Some(callable) = callable.filter(|e| e.kind != EntityKind::Constructor) {
            let Some(Some(method)) = self.methods.get(&callable.id) else {
                return Ok(selection);
            };
            retain_envelope(method.envelope, &mut selection);
            if method.shape.dispatch == GroovyCallableDispatch::Unknown
                || (method.shape.dispatch == GroovyCallableDispatch::Static)
                    != lookup.static_context
            {
                return Ok(selection);
            }
        }
        // Only a dynamic variable is eligible for module import rewriting. A
        // field/property in a lexical type or its ancestry owns the spelling first.
        for owner in &types {
            let Some(scopes) = self.scopes(
                Scope::Type(owner.id),
                &mut selection,
                remaining,
                cancellation,
            )?
            else {
                return Ok(selection);
            };
            for scope in scopes {
                for entity in self
                    .names
                    .get(&(scope, lookup.name_hash))
                    .into_iter()
                    .flatten()
                {
                    step(&mut selection, remaining, cancellation)?;
                    self.retain_entity(entity, &mut selection);
                    if matches!(entity.kind, EntityKind::Field | EntityKind::Property) {
                        return Ok(selection);
                    }
                }
            }
        }
        let Some(imports) = self.imports.get(&site.file) else {
            return Ok(selection);
        };
        for (envelope, _) in imports {
            step(&mut selection, remaining, cancellation)?;
            retain_envelope(envelope, &mut selection);
        }
        for prefix in access.prefixes() {
            let accessor = format!("{prefix}{}", capitalize(&lookup.spelling));
            if let Some((_, import)) = imports
                .iter()
                .rfind(|(_, i)| i.binding_spelling.as_deref() == Some(accessor.as_str()))
                && let Some(name) = import.member_spelling.as_deref()
                && self.imported_value(
                    import,
                    ValueName::Accessor(name),
                    access,
                    &mut selection,
                    remaining,
                    cancellation,
                )?
            {
                return Ok(selection);
            }
        }
        if let Some((_, import)) = imports
            .iter()
            .rfind(|(_, i)| i.binding == Some(lookup.name_hash))
            && let Some(name) = import.member_spelling.as_deref()
            && self.imported_value(
                import,
                ValueName::Property(name),
                access,
                &mut selection,
                remaining,
                cancellation,
            )?
        {
            return Ok(selection);
        }
        for (_, import) in imports.iter().filter(|(_, i)| i.member.is_none()) {
            if self.imported_value(
                import,
                ValueName::Property(&lookup.spelling),
                access,
                &mut selection,
                remaining,
                cancellation,
            )? {
                break;
            }
        }
        Ok(selection)
    }

    fn imported_value(
        &self,
        import: &GroovyStaticImport,
        name: ValueName<'_>,
        access: ValueAccess,
        selection: &mut Selection<'a>,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<bool, ResolutionError> {
        step(selection, remaining, cancellation)?;
        let Some(owner) = self
            .index
            .declarations
            .get(&import.owner)
            .and_then(Option::as_ref)
        else {
            return Ok(true);
        };
        for parent in &owner.owners {
            self.retain_entity(parent, selection);
        }
        let Some(scopes) = self.scopes(
            Scope::Type(owner.entity.id),
            selection,
            remaining,
            cancellation,
        )?
        else {
            return Ok(true);
        };
        let name = match name {
            ValueName::Accessor(name) => {
                if self.value_accessor(&scopes, name, access, selection, remaining, cancellation)? {
                    return Ok(true);
                }
                if let Some((_, property)) = accessor_property(name) {
                    return self.read_field(
                        &scopes,
                        &property,
                        true,
                        selection,
                        remaining,
                        cancellation,
                    );
                }
                return Ok(false);
            }
            ValueName::Property(name) => name,
        };
        for prefix in access.prefixes() {
            if self.value_accessor(
                &scopes,
                &format!("{prefix}{}", capitalize(name)),
                access,
                selection,
                remaining,
                cancellation,
            )? {
                return Ok(true);
            }
        }
        self.read_field(&scopes, name, false, selection, remaining, cancellation)
    }

    fn value_accessor(
        &self,
        scopes: &[Scope],
        name: &str,
        access: ValueAccess,
        selection: &mut Selection<'a>,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<bool, ResolutionError> {
        let mut candidates = BTreeMap::new();
        for &scope in scopes {
            step(selection, remaining, cancellation)?;
            for entity in self
                .names
                .get(&(scope, content_hash(name.as_bytes())))
                .into_iter()
                .flatten()
            {
                step(selection, remaining, cancellation)?;
                self.retain_entity(entity, selection);
                let Some(Some(method)) = self.methods.get(&entity.id) else {
                    return Ok(true);
                };
                retain_envelope(method.envelope, selection);
                if method.shape.dispatch == GroovyCallableDispatch::Unknown {
                    return Ok(true);
                }
                if method.shape.dispatch != GroovyCallableDispatch::Static
                    || !possible(method.shape, access.arity())
                {
                    continue;
                }
                // Groovy's property rewrite looks up the exact empty signature
                // for is-getters, not an overload callable through defaults/varargs.
                if name.starts_with("is") {
                    if method.shape.parameter_count != 0 {
                        continue;
                    }
                    match method.shape.return_type {
                        rootlight_ir::GroovyCallableReturnType::PrimitiveBoolean => {}
                        rootlight_ir::GroovyCallableReturnType::Other => continue,
                        rootlight_ir::GroovyCallableReturnType::Unknown => return Ok(true),
                    }
                }
                if method.shape.visibility != GroovyCallableVisibility::Public {
                    return Ok(true);
                }
                candidates.insert(entity.id, *entity);
            }
        }
        let found = !candidates.is_empty();
        if candidates.len() == 1 {
            selection.target = candidates.into_values().next();
        }
        Ok(found)
    }

    fn read_field(
        &self,
        scopes: &[Scope],
        name: &str,
        property_only: bool,
        selection: &mut Selection<'a>,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<bool, ResolutionError> {
        let mut candidates = BTreeMap::new();
        for &scope in scopes {
            step(selection, remaining, cancellation)?;
            for entity in self
                .names
                .get(&(scope, content_hash(name.as_bytes())))
                .into_iter()
                .flatten()
            {
                step(selection, remaining, cancellation)?;
                if !matches!(entity.kind, EntityKind::Field | EntityKind::Property) {
                    continue;
                }
                self.retain_entity(entity, selection);
                let Some(Some(member)) = self.members.get(&entity.id) else {
                    return Ok(true);
                };
                retain_envelope(member.envelope, selection);
                if member.shape.storage != rootlight_ir::GroovyMemberStorage::Static
                    || member.shape.visibility != EntityVisibility::Public
                {
                    return Ok(true);
                }
                if !property_only || entity.kind == EntityKind::Property {
                    candidates.insert(entity.id, *entity);
                }
            }
        }
        let found = !candidates.is_empty();
        if candidates.len() == 1 {
            selection.target = candidates.into_values().next();
        }
        Ok(found)
    }

    fn select_import(
        &self,
        file: FileId,
        envelope: &ExtensionEnvelope,
        import: &GroovyStaticImport,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<Selection<'a>, ResolutionError> {
        let mut selection = Selection::default();
        step(&mut selection, remaining, cancellation)?;
        self.retain_file(file, &mut selection);
        retain_envelope(envelope, &mut selection);
        let Some(owner) = self
            .index
            .declarations
            .get(&import.owner)
            .and_then(Option::as_ref)
        else {
            return Ok(selection);
        };
        self.retain_entity(owner.entity, &mut selection);
        for parent in &owner.owners {
            self.retain_entity(parent, &mut selection);
        }
        let Some(name) = import.member else {
            // A star import identifies a namespace, not one member or a proof
            // that every member's classpath/runtime semantics were analyzed.
            selection.target = Some(owner.entity);
            return Ok(selection);
        };
        let Some(scopes) = self.scopes(
            Scope::Type(owner.entity.id),
            &mut selection,
            remaining,
            cancellation,
        )?
        else {
            return Ok(selection);
        };
        let mut candidates = BTreeMap::new();
        for scope in scopes {
            for entity in self.names.get(&(scope, name)).into_iter().flatten() {
                step(&mut selection, remaining, cancellation)?;
                self.retain_entity(entity, &mut selection);
                let is_static = if let Some(Some(method)) = self.methods.get(&entity.id) {
                    retain_envelope(method.envelope, &mut selection);
                    if method.shape.dispatch == GroovyCallableDispatch::Unknown
                        || method.shape.visibility != GroovyCallableVisibility::Public
                    {
                        return Ok(selection);
                    }
                    method.shape.dispatch == GroovyCallableDispatch::Static
                } else if let Some(Some(member)) = self.members.get(&entity.id) {
                    retain_envelope(member.envelope, &mut selection);
                    if member.shape.storage == rootlight_ir::GroovyMemberStorage::Unknown
                        || member.shape.visibility != EntityVisibility::Public
                    {
                        return Ok(selection);
                    }
                    member.shape.storage == rootlight_ir::GroovyMemberStorage::Static
                } else {
                    return Ok(selection);
                };
                if is_static {
                    candidates.insert(entity.id, *entity);
                }
            }
        }
        // An import has no argument list with which to select an overload.
        if candidates.len() == 1 {
            selection.target = candidates.into_values().next();
        } else if !candidates.is_empty() {
            selection.candidates = Some(CandidateSet {
                total_count: u64::try_from(candidates.len())
                    .map_err(|_| ResolutionError::CountOverflow)?,
                symbols: candidates.into_keys().take(MAX_CANDIDATE_LIMIT).collect(),
            });
        }
        Ok(selection)
    }

    fn valid_file(&self, file: FileId) -> bool {
        self.coverage.contains_key(&file)
            && !self.invalid.contains(&file)
            && self.index.contexts.contains_key(&file)
    }

    fn valid_envelope(&self, envelope: &ExtensionEnvelope) -> bool {
        envelope.evidence.source.as_ref().is_some_and(|source| {
            self.valid_file(source.span().file())
                && self
                    .index
                    .contexts
                    .get(&source.span().file())
                    .is_some_and(|context| {
                        source.content_hash() == context.file.content_hash
                            && envelope.provenance == context.file.provenance
                            && source.span().end_byte() <= context.file.byte_length
                    })
        })
    }

    fn entity_scope(
        &self,
        entity: &EntityRecord,
        cancellation: &Cancellation,
    ) -> Result<Scope, ResolutionError> {
        let file = entity
            .evidence
            .source
            .as_ref()
            .ok_or(ResolutionError::UnsupportedIdentityRemap)?
            .span()
            .file();
        let mut parent = entity.container;
        for _ in 0..=self.entities.len() {
            cancellation.check()?;
            let Some(ContainerRef::Entity(id)) = parent else {
                return Ok(Scope::File(file));
            };
            let owner = self
                .entities
                .get(&id)
                .ok_or(ResolutionError::UnsupportedIdentityRemap)?;
            if is_type(owner.kind) {
                return Ok(Scope::Type(id));
            }
            parent = owner.container;
        }
        Err(ResolutionError::UnsupportedIdentityRemap)
    }

    fn build_parents(
        &mut self,
        occurrences: &[OccurrenceRecord],
        cancellation: &Cancellation,
    ) -> Result<(), ResolutionError> {
        let mut header_spans = BTreeSet::new();
        let mut superclasses = BTreeSet::new();
        for header in occurrences.iter().filter(|site| {
            matches!(
                site.role,
                OccurrenceRole::InheritanceUse | OccurrenceRole::ImplementationUse
            ) || matches!(
                site.syntax_kind.as_str(),
                "groovy.extends_type.reference" | "groovy.implements_type.reference"
            )
        }) {
            cancellation.check()?;
            if !self.index.contexts.contains_key(&header.file) {
                continue;
            }
            let Some(owner) = header
                .enclosing
                .and_then(|id| self.entities.get(&id))
                .copied()
            else {
                self.invalid.insert(header.file);
                continue;
            };
            let valid_owner = self.parents.contains_key(&owner.id)
                && owner.evidence.source.as_ref().is_some_and(|source| {
                    source.span().file() == header.file
                        && contains(source.span(), header.source.span())
                });
            if !valid_owner {
                // An unattached edge is not evidence that the containing file's
                // types have no parents. Invalidate them before completing roots.
                self.invalid.insert(header.file);
                continue;
            }
            let class_superclass = header.role == OccurrenceRole::InheritanceUse
                && !matches!(owner.kind, EntityKind::Interface | EntityKind::Trait);
            if !super::is_header(header)
                || !header_spans.insert(header.source.span())
                || (class_superclass && !superclasses.insert(owner.id))
            {
                self.parents.insert(owner.id, None);
                continue;
            }
            let target = match header.target {
                OccurrenceTarget::Resolved { symbol }
                    if self
                        .index
                        .contexts
                        .get(&header.file)
                        .is_some_and(|context| context.file.provenance == header.provenance) =>
                {
                    self.entities.get(&symbol).copied()
                }
                OccurrenceTarget::Resolved { symbol } => self
                    .index
                    .proof(header)
                    .map(|proof| proof.target)
                    .filter(|target| target.id == symbol),
                _ => self.index.proof(header).map(|proof| proof.target),
            };
            let Some(target) = target.filter(|target| {
                is_type(target.kind) && target.language == "groovy" && self.valid_file(header.file)
            }) else {
                self.parents.insert(owner.id, None);
                continue;
            };
            let mut evidence = Selection::default();
            self.retain_entity(owner, &mut evidence);
            self.retain_entity(target, &mut evidence);
            evidence.sources.insert(header.source.clone());
            if let Some(proof) = self.index.proof(header) {
                for parent in proof.owners {
                    self.retain_entity(parent, &mut evidence);
                }
                if let Some(binding) = proof.binding {
                    retain_envelope(binding, &mut evidence);
                    let selection = rootlight_ir::decode_groovy_binding_envelope(binding)
                        .map_err(|_| ResolutionError::UnsupportedIdentityRemap)?;
                    if let Some(source) = &binding.evidence.source {
                        evidence.sources.insert(SourceRef::new(
                            source.repository(),
                            source.generation(),
                            selection.import,
                            source.content_hash(),
                            None,
                        ));
                    }
                }
                if let Some((binding, owner)) = proof.qualified_header {
                    retain_envelope(binding, &mut evidence);
                    self.retain_entity(owner, &mut evidence);
                }
            }
            if let Some(Some(parents)) = self.parents.get_mut(&owner.id) {
                parents.push(Parent {
                    target: target.id,
                    sources: evidence.sources,
                    facts: evidence.facts,
                });
            }
        }
        for (&owner, parents) in &mut self.parents {
            cancellation.check()?;
            if self.entities.get(&owner).is_some_and(|entity| {
                entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| self.invalid.contains(&source.span().file()))
            }) {
                *parents = None;
            }
        }
        let mut pending = BTreeMap::new();
        let mut dependents = BTreeMap::<SymbolId, Vec<SymbolId>>::new();
        let mut ready = VecDeque::new();
        for (&owner, parents) in &self.parents {
            cancellation.check()?;
            let Some(parents) = parents else { continue };
            pending.insert(owner, parents.len());
            if parents.is_empty() {
                ready.push_back(owner);
            }
            for parent in parents {
                dependents.entry(parent.target).or_default().push(owner);
            }
        }
        while let Some(owner) = ready.pop_front() {
            cancellation.check()?;
            self.complete.insert(owner);
            for dependent in dependents.get(&owner).into_iter().flatten() {
                let Some(count) = pending.get_mut(dependent) else {
                    continue;
                };
                *count = count.checked_sub(1).ok_or(ResolutionError::CountOverflow)?;
                if *count == 0 {
                    ready.push_back(*dependent);
                }
            }
        }
        Ok(())
    }

    fn retain_entity(&self, entity: &'a EntityRecord, selection: &mut Selection<'a>) {
        selection.facts.insert(FactRef::Entity(entity.id));
        selection
            .sources
            .extend(entity.evidence.source.iter().cloned());
        if let Some(source) = &entity.evidence.source {
            self.retain_file(source.span().file(), selection);
        }
    }

    fn retain_file(&self, file: FileId, selection: &mut Selection<'a>) {
        if let Some(context) = self.index.contexts.get(&file) {
            for coverage in context
                .coverage
                .into_iter()
                .chain(self.coverage.get(&file).copied())
            {
                selection.facts.insert(FactRef::Fact(coverage.id));
                selection
                    .sources
                    .extend(coverage.evidence.source.iter().cloned());
            }
        }
    }

    fn scopes(
        &self,
        scope: Scope,
        selection: &mut Selection<'a>,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<Option<Vec<Scope>>, ResolutionError> {
        if let Scope::File(file) = scope {
            return Ok(self.valid_file(file).then_some(vec![scope]));
        }
        let mut pending = vec![scope];
        let mut visited = BTreeSet::new();
        let mut output = Vec::new();
        while let Some(scope) = pending.pop() {
            step(selection, remaining, cancellation)?;
            if !visited.insert(scope) {
                continue;
            }
            let Scope::Type(id) = scope else {
                return Ok(None);
            };
            if !self.complete.contains(&id) {
                return Ok(None);
            }
            let Some(entity) = self.entities.get(&id).copied() else {
                return Ok(None);
            };
            self.retain_entity(entity, selection);
            output.push(scope);
            for parent in self
                .parents
                .get(&id)
                .and_then(Option::as_ref)
                .into_iter()
                .flatten()
            {
                selection.sources.extend(parent.sources.iter().cloned());
                selection.facts.extend(parent.facts.iter().copied());
                pending.push(Scope::Type(parent.target));
            }
        }
        Ok(Some(output))
    }

    fn select(
        &self,
        site: &OccurrenceRecord,
        lookup: &GroovyCallLookup,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<Selection<'a>, ResolutionError> {
        let mut selection = Selection::default();
        self.retain_file(site.file, &mut selection);
        if lookup.lexical_binding || lookup.arguments.spread != 0 {
            return Ok(selection);
        }
        let count = lookup
            .arguments
            .positional
            .checked_add(lookup.arguments.trailing_closures)
            .and_then(|count| count.checked_add(u32::from(lookup.arguments.named != 0)))
            .ok_or(ResolutionError::CountOverflow)?;
        let mut parent = site.enclosing;
        let mut types = Vec::new();
        let mut callable = None;
        for _ in 0..=self.entities.len() {
            step(&mut selection, remaining, cancellation)?;
            let Some(id) = parent else { break };
            let Some(entity) = self.entities.get(&id).copied() else {
                return Ok(selection);
            };
            let Some(source) = &entity.evidence.source else {
                return Ok(selection);
            };
            if !contains(source.span(), site.source.span()) {
                return Ok(selection);
            }
            self.retain_entity(entity, &mut selection);
            if is_type(entity.kind) {
                types.push(entity);
            }
            if callable.is_none()
                && matches!(
                    entity.kind,
                    EntityKind::Method | EntityKind::Function | EntityKind::Constructor
                )
            {
                callable = Some(entity);
            }
            parent = match entity.container {
                Some(ContainerRef::Entity(id)) => Some(id),
                _ => None,
            };
        }
        if parent.is_some()
            || types
                .first()
                .and_then(|entity| entity.evidence.source.as_ref())
                .map(SourceRef::span)
                != lookup.enclosing_type
            || callable
                .and_then(|entity| entity.evidence.source.as_ref())
                .map(SourceRef::span)
                != lookup.enclosing_callable
        {
            return Ok(selection);
        }
        if let Some(callable) = callable.filter(|entity| entity.kind != EntityKind::Constructor) {
            let Some(method) = self.methods.get(&callable.id).and_then(Option::as_ref) else {
                return Ok(selection);
            };
            retain_envelope(method.envelope, &mut selection);
            if method.shape.dispatch == GroovyCallableDispatch::Unknown
                || (method.shape.dispatch == GroovyCallableDispatch::Static)
                    != lookup.static_context
            {
                return Ok(selection);
            }
        }
        let lexical_scopes = if types.is_empty() {
            vec![Scope::File(site.file)]
        } else {
            types.iter().map(|entity| Scope::Type(entity.id)).collect()
        };
        for (depth, lexical) in lexical_scopes.into_iter().enumerate() {
            let Some(scopes) = self.scopes(lexical, &mut selection, remaining, cancellation)?
            else {
                return Ok(selection);
            };
            for scope in scopes {
                for entity in self
                    .names
                    .get(&(scope, lookup.name_hash))
                    .into_iter()
                    .flatten()
                {
                    step(&mut selection, remaining, cancellation)?;
                    self.retain_entity(entity, &mut selection);
                    if entity.visibility == EntityVisibility::Private && scope != lexical {
                        continue;
                    }
                    let Some(method) = self.methods.get(&entity.id) else {
                        return Ok(selection);
                    };
                    let Some(method) = method else {
                        return Ok(selection);
                    };
                    retain_envelope(method.envelope, &mut selection);
                    if method.shape.dispatch == GroovyCallableDispatch::Unknown
                        || (possible(method.shape, count)
                            && (depth != 0
                                || !lookup.static_context
                                || method.shape.dispatch == GroovyCallableDispatch::Static))
                    {
                        return Ok(selection);
                    }
                }
            }
        }
        let Some(imports) = self.imports.get(&site.file) else {
            return Ok(selection);
        };
        // ModuleNode keeps the last explicit binding; static stars retain their
        // insertion order. All competing import evidence remains in the proof.
        for (envelope, _) in imports {
            step(&mut selection, remaining, cancellation)?;
            retain_envelope(envelope, &mut selection);
        }
        let accessor = accessor_property(&lookup.spelling).is_some();
        if let Some((envelope, import)) = imports
            .iter()
            .rfind(|(_, import)| import.binding == Some(lookup.name_hash))
            && self.imported_call(
                ImportedCall {
                    envelope,
                    import,
                    name: import
                        .member_spelling
                        .as_deref()
                        .unwrap_or(&lookup.spelling),
                    arguments: count,
                    accessor,
                },
                &mut selection,
                remaining,
                cancellation,
            )?
        {
            return Ok(selection);
        }
        if let Some((prefix, property)) = accessor_property(&lookup.spelling)
            && let Some((envelope, import)) = imports
                .iter()
                .rfind(|(_, import)| import.binding_spelling.as_deref() == Some(property.as_str()))
            && let Some(member) = import.member_spelling.as_deref()
        {
            let name = format!("{prefix}{}", capitalize(member));
            if self.imported_call(
                ImportedCall {
                    envelope,
                    import,
                    name: &name,
                    arguments: count,
                    accessor,
                },
                &mut selection,
                remaining,
                cancellation,
            )? {
                return Ok(selection);
            }
        }
        // An enum's self-star import is an exclusive lookup path, even when it
        // finds no method; falling through would invent a foreign call target.
        if let Some(current) = types
            .first()
            .filter(|entity| entity.kind == EntityKind::Enum)
            && let Some((envelope, import)) = imports.iter().find(|(_, import)| {
                import.member.is_none()
                    && self
                        .index
                        .declarations
                        .get(&import.owner)
                        .and_then(Option::as_ref)
                        .is_some_and(|owner| owner.entity.id == current.id)
            })
        {
            self.imported_call(
                ImportedCall {
                    envelope,
                    import,
                    name: &lookup.spelling,
                    arguments: count,
                    accessor,
                },
                &mut selection,
                remaining,
                cancellation,
            )?;
            return Ok(selection);
        }
        for (envelope, import) in imports.iter().filter(|(_, import)| import.member.is_none()) {
            if self.imported_call(
                ImportedCall {
                    envelope,
                    import,
                    name: &lookup.spelling,
                    arguments: count,
                    accessor,
                },
                &mut selection,
                remaining,
                cancellation,
            )? {
                return Ok(selection);
            }
        }
        Ok(selection)
    }

    fn imported_call(
        &self,
        call: ImportedCall<'_>,
        selection: &mut Selection<'a>,
        remaining: &mut usize,
        cancellation: &Cancellation,
    ) -> Result<bool, ResolutionError> {
        step(selection, remaining, cancellation)?;
        retain_envelope(call.envelope, selection);
        let Some(owner) = self
            .index
            .declarations
            .get(&call.import.owner)
            .and_then(Option::as_ref)
        else {
            return Ok(true);
        };
        for parent in &owner.owners {
            self.retain_entity(parent, selection);
        }
        let Some(scopes) = self.scopes(
            Scope::Type(owner.entity.id),
            selection,
            remaining,
            cancellation,
        )?
        else {
            return Ok(true);
        };
        let name = content_hash(call.name.as_bytes());
        let mut candidates = BTreeMap::new();
        let mut authored = BTreeMap::new();
        for &scope in &scopes {
            for entity in self.names.get(&(scope, name)).into_iter().flatten() {
                step(selection, remaining, cancellation)?;
                self.retain_entity(entity, selection);
                let Some(Some(method)) = self.methods.get(&entity.id) else {
                    return Ok(true);
                };
                retain_envelope(method.envelope, selection);
                if method.shape.dispatch == GroovyCallableDispatch::Unknown
                    || method.shape.visibility == GroovyCallableVisibility::Unknown
                {
                    return Ok(true);
                }
                if method.shape.dispatch == GroovyCallableDispatch::Static {
                    if method.shape.visibility != GroovyCallableVisibility::Public {
                        return Ok(true);
                    }
                    authored.insert(entity.id, *entity);
                    if possible(method.shape, call.arguments) {
                        candidates.insert(entity.id, *entity);
                    }
                }
            }
        }
        if candidates.len() == 1 {
            selection.target = candidates.into_values().next();
            return Ok(true);
        }
        if !candidates.is_empty() {
            return Ok(true);
        }
        if call.accessor
            && let Some((_, property)) = accessor_property(call.name)
        {
            let getter = content_hash(format!("get{}", capitalize(&property)).as_bytes());
            for &scope in &scopes {
                step(selection, remaining, cancellation)?;
                for entity in self.names.get(&(scope, getter)).into_iter().flatten() {
                    step(selection, remaining, cancellation)?;
                    self.retain_entity(entity, selection);
                    let Some(Some(method)) = self.methods.get(&entity.id) else {
                        return Ok(true);
                    };
                    retain_envelope(method.envelope, selection);
                    if method.shape.dispatch == GroovyCallableDispatch::Static
                        && method.shape.visibility == GroovyCallableVisibility::Public
                        && possible(method.shape, 0)
                    {
                        // Groovy's property-accessor rewrite does not validate the
                        // invocation arity. This links one authored name, not runtime validity.
                        if authored.len() == 1 {
                            selection.target = authored.into_values().next();
                        }
                        return Ok(true);
                    }
                }
            }
        }
        // A callable property or generated accessor can precede the next import.
        // It cannot be projected as an authored method without further evidence.
        let capitalized = capitalize(call.name);
        let mut property_names = vec![format!("get{capitalized}"), format!("is{capitalized}")];
        if call.accessor
            && let Some((_, property)) = accessor_property(call.name)
        {
            property_names.push(format!("is{}", capitalize(&property)));
            property_names.push(property);
        }
        for property in property_names {
            let name = content_hash(property.as_bytes());
            for &scope in &scopes {
                step(selection, remaining, cancellation)?;
                if let Some(entity) = self
                    .names
                    .get(&(scope, name))
                    .and_then(|items| items.first())
                {
                    step(selection, remaining, cancellation)?;
                    self.retain_entity(entity, selection);
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn callable_name_matches(
    identity: Option<rootlight_ir::GroovyCallableName>,
    spelling_hash: ContentHash,
    canonical_name: &str,
) -> bool {
    let canonical_hash = content_hash(canonical_name.as_bytes());
    match identity {
        Some(identity) => {
            identity.spelling_hash == spelling_hash && identity.canonical_hash == canonical_hash
        }
        // Legacy evidence cannot establish a quoted spelling's decoded identity.
        None => spelling_hash == canonical_hash,
    }
}

fn accessor_property(name: &str) -> Option<(&'static str, String)> {
    for prefix in ["get", "set", "is"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            let mut chars = rest.chars();
            let first = chars.next()?;
            return Some((prefix, first.to_lowercase().chain(chars).collect()));
        }
    }
    None
}

fn capitalize(name: &str) -> String {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    if first.is_lowercase() && chars.clone().next().is_some_and(char::is_uppercase) {
        return name.to_owned();
    }
    first.to_uppercase().chain(chars).collect()
}

fn possible(shape: GroovyCallable, count: u32) -> bool {
    count == shape.parameter_count
        || (shape.variadic && count >= shape.parameter_count.saturating_sub(1))
        || (count >= shape.required_parameter_count && count < shape.parameter_count)
}

fn contains(outer: SourceSpan, inner: SourceSpan) -> bool {
    outer.file() == inner.file()
        && outer.start_byte() <= inner.start_byte()
        && outer.end_byte() >= inner.end_byte()
}

fn retain_envelope(envelope: &ExtensionEnvelope, selection: &mut Selection<'_>) {
    selection.facts.insert(FactRef::Fact(envelope.id));
    selection
        .sources
        .extend(envelope.evidence.source.iter().cloned());
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

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_ir::GroovyCallableName;

    #[test]
    fn callable_names_require_matching_spelling_and_canonical_evidence() {
        let bare = content_hash(b"fold");
        let quoted = content_hash(b"'fold'");
        let identity = GroovyCallableName {
            spelling_hash: quoted,
            canonical_hash: bare,
        };
        assert!(callable_name_matches(None, bare, "fold"));
        assert!(!callable_name_matches(None, quoted, "fold"));
        assert!(callable_name_matches(Some(identity), quoted, "fold"));
        assert!(!callable_name_matches(Some(identity), bare, "fold"));
        assert!(!callable_name_matches(Some(identity), quoted, "other"));
    }
}
