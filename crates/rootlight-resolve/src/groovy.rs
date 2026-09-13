//! Groovy imports, qualified parents and inherited types over source-owned declarations.
//! Links identify declarations in the admitted snapshot, not classpath selection
//! or dynamic dispatch; incomplete inventories and duplicate names veto promotion.

mod calls;
mod hierarchy;

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, CoverageRecord, CoverageScope, CoverageStatus, EntityKind, EntityRecord,
    FactDomain, FactRef, FileRecord, IrLimits, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    ProducerKind, ProvenanceRecord,
};

use crate::{
    CandidateExplanation, CompletenessAssumption, RESOLVER_PROVIDER_NAME,
    RESOLVER_PROVIDER_VERSION, ResolutionDecision, ResolutionError, ResolutionExplanation,
    ResolutionOutcome, ResolutionRule, ResolutionSignal, UnresolvedReason,
};

struct Context<'a> {
    file: &'a FileRecord,
    coverage: [&'a CoverageRecord; 2],
    retained_entities: u64,
}

struct Declaration<'a> {
    entity: &'a EntityRecord,
    owners: Vec<&'a EntityRecord>,
}

struct QualifiedHeader<'a> {
    envelope: &'a rootlight_ir::ExtensionEnvelope,
    owner: &'a EntityRecord,
    target: Option<Arc<Declaration<'a>>>,
}

pub(crate) struct Proof<'a, 'index> {
    pub(crate) target: &'a EntityRecord,
    pub(crate) owners: &'index [&'a EntityRecord],
    pub(crate) coverage: [&'a CoverageRecord; 4],
    pub(crate) binding: Option<&'a rootlight_ir::ExtensionEnvelope>,
    pub(crate) qualified_header: Option<(&'a rootlight_ir::ExtensionEnvelope, &'a EntityRecord)>,
}

#[derive(Default)]
pub(crate) struct GroovyIndex<'a> {
    static_imports: BTreeMap<FactId, hierarchy::Entry<'a>>,
    inherited: BTreeMap<FactId, hierarchy::Entry<'a>>,
    qualified_headers: BTreeMap<FactId, Option<QualifiedHeader<'a>>>,
    contexts: BTreeMap<FileId, Context<'a>>,
    declarations: BTreeMap<ContentHash, Option<Arc<Declaration<'a>>>>,
    ranked_declarations: BTreeMap<ContentHash, BTreeMap<usize, Option<Arc<Declaration<'a>>>>>,
    declaration_sources: BTreeMap<rootlight_ir::SourceSpan, Option<Arc<Declaration<'a>>>>,
    derived_sites: BTreeSet<FactId>,
    unavailable: bool,
    uses: BTreeMap<
        rootlight_ir::SourceSpan,
        Option<(
            &'a rootlight_ir::ExtensionEnvelope,
            rootlight_ir::GroovyImportedType,
        )>,
    >,
}

impl<'a> GroovyIndex<'a> {
    pub(crate) fn build(
        files: &BTreeMap<FileId, &'a FileRecord>,
        entities: &BTreeMap<SymbolId, &'a EntityRecord>,
        provenance: &BTreeMap<FactId, &'a ProvenanceRecord>,
        coverage: &'a [CoverageRecord],
        extensions: &'a [rootlight_ir::ExtensionEnvelope],
        occurrences: &[OccurrenceRecord],
        cancellation: &Cancellation,
    ) -> Result<Self, ResolutionError> {
        cancellation.check()?;
        let mut index = Self::default();
        if !files.values().any(|file| file.language == "groovy") {
            return Ok(index);
        }
        let mut domains = BTreeMap::new();
        for record in coverage {
            cancellation.check()?;
            if let CoverageScope::File(file) = record.scope
                && matches!(record.domain, FactDomain::Files | FactDomain::Entities)
            {
                domains
                    .entry((file, record.domain))
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(record));
            }
        }
        for file in files
            .values()
            .copied()
            .filter(|file| file.language == "groovy")
        {
            cancellation.check()?;
            let complete = |domain| {
                domains
                    .get(&(file.id, domain))
                    .copied()
                    .flatten()
                    .filter(|record| {
                        record.status == CoverageStatus::Complete
                            && record.skipped == 0
                            && record.discovered == record.indexed
                            && record.provenance == file.provenance
                            && record.evidence.source.is_some()
                            && record.evidence.source == file.evidence.source
                    })
            };
            if let (Some(file_coverage), Some(entity_coverage)) =
                (complete(FactDomain::Files), complete(FactDomain::Entities))
                && file_coverage.indexed == 1
                && provenance.get(&file.provenance).is_some_and(|record| {
                    record.producer_kind == ProducerKind::Parser && record.language == "groovy"
                })
            {
                index.contexts.insert(
                    file.id,
                    Context {
                        file,
                        coverage: [file_coverage, entity_coverage],
                        retained_entities: 0,
                    },
                );
            } else {
                // An omitted declaration could compete with an otherwise unique import.
                index.unavailable = true;
            }
        }
        if index.unavailable || index.contexts.is_empty() {
            return Ok(index);
        }
        for entity in entities
            .values()
            .copied()
            .filter(|entity| entity.language == "groovy")
        {
            cancellation.check()?;
            let context = entity
                .evidence
                .source
                .as_ref()
                .and_then(|source| index.contexts.get_mut(&source.span().file()));
            let Some(context) = context else {
                index.unavailable = true;
                continue;
            };
            if entity.provenance != context.file.provenance
                || entity
                    .evidence
                    .source
                    .as_ref()
                    .is_none_or(|source| source.content_hash() != context.file.content_hash)
            {
                index.unavailable = true;
                continue;
            }
            context.retained_entities = context
                .retained_entities
                .checked_add(1)
                .ok_or(ResolutionError::CountOverflow)?;
            if !is_type(entity.kind) {
                continue;
            }
            if let Some((name, declaration)) =
                declaration_path(entity, context, entities, cancellation)?
            {
                let depth = declaration
                    .owners
                    .iter()
                    .filter(|owner| is_type(owner.kind))
                    .count();
                let declaration = Arc::new(declaration);
                index
                    .ranked_declarations
                    .entry(name)
                    .or_default()
                    .entry(depth)
                    .and_modify(|prior| *prior = None)
                    .or_insert_with(|| Some(Arc::clone(&declaration)));
                if let Some(source) = &entity.evidence.source {
                    index
                        .declaration_sources
                        .entry(source.span())
                        .and_modify(|prior| *prior = None)
                        .or_insert_with(|| Some(Arc::clone(&declaration)));
                }
                index
                    .declarations
                    .entry(name)
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some(declaration));
            } else {
                index.unavailable = true;
            }
        }
        // A complete producer count is not proof that composition retained every declaration.
        if index
            .contexts
            .values()
            .any(|context| context.retained_entities != context.coverage[1].indexed)
        {
            index.unavailable = true;
        }
        for envelope in extensions {
            cancellation.check()?;
            if envelope.namespace != rootlight_ir::GROOVY_BINDING_NAMESPACE {
                continue;
            }
            let Some(source) = envelope.evidence.source.as_ref() else {
                continue;
            };
            let binding = rootlight_ir::decode_groovy_binding_envelope(envelope)
                .ok()
                .filter(|binding| {
                    index
                        .contexts
                        .get(&source.span().file())
                        .is_some_and(|context| {
                            envelope.provenance == context.file.provenance
                                && source.content_hash() == context.file.content_hash
                                && binding.import.end_byte() <= context.file.byte_length
                                && source.span().end_byte() <= context.file.byte_length
                        })
                })
                .map(|binding| (envelope, binding));
            index
                .uses
                .entry(source.span())
                .and_modify(|prior| *prior = None)
                .or_insert(binding);
        }
        for site in occurrences {
            cancellation.check()?;
            if let Some(context) = index.contexts.get(&site.file)
                && let Some(record) = provenance.get(&site.provenance)
                && source_owned_derived_site(site, context, record)
            {
                index.derived_sites.insert(site.id);
            }
        }
        index.collect_qualified_headers(entities, extensions, occurrences, cancellation)?;
        index.inherited = hierarchy::build(
            &index,
            entities,
            coverage,
            extensions,
            occurrences,
            cancellation,
        )?;
        index.static_imports = calls::build(
            &index,
            entities,
            coverage,
            extensions,
            occurrences,
            cancellation,
        )?;
        Ok(index)
    }

    fn collect_qualified_headers(
        &mut self,
        entities: &BTreeMap<SymbolId, &'a EntityRecord>,
        extensions: &'a [rootlight_ir::ExtensionEnvelope],
        occurrences: &[OccurrenceRecord],
        cancellation: &Cancellation,
    ) -> Result<(), ResolutionError> {
        let mut sites = BTreeMap::new();
        for site in occurrences.iter().filter(|site| is_header(site)) {
            cancellation.check()?;
            sites
                .entry(site.source.span())
                .and_modify(|prior| *prior = None)
                .or_insert(Some(site));
        }
        for envelope in extensions
            .iter()
            .filter(|item| item.namespace == rootlight_ir::GROOVY_TYPE_LOOKUP_NAMESPACE)
        {
            cancellation.check()?;
            let Some(source) = &envelope.evidence.source else {
                continue;
            };
            let Some(site) = sites.get(&source.span()).copied().flatten() else {
                continue;
            };
            let proof = (|| {
                let lookup = rootlight_ir::decode_groovy_type_lookup_envelope(envelope).ok()?;
                let context = self.contexts.get(&site.file)?;
                let owner = entities.get(&site.enclosing?).copied()?;
                let owner_source = owner.evidence.source.as_ref()?;
                (lookup.name.contains('.')
                    && lookup.fallback_import.is_none()
                    && content_hash(lookup.name.as_bytes()) == site.syntactic_text_hash
                    && site.source == *source
                    && self.has_source_owned_provenance(site)
                    && envelope.provenance == context.file.provenance
                    && source.content_hash() == context.file.content_hash
                    && owner.language == "groovy"
                    && is_type(owner.kind)
                    && owner.provenance == context.file.provenance
                    && owner_source.span() == lookup.owner
                    && owner_source.content_hash() == context.file.content_hash
                    && lookup.owner.end_byte() <= context.file.byte_length)
                    .then_some((owner, lookup))
            })();
            let proof = if let Some((owner, lookup)) = proof {
                Some(QualifiedHeader {
                    envelope,
                    owner,
                    target: self.select_qualified_header(owner, &lookup, cancellation)?,
                })
            } else {
                None
            };
            self.qualified_headers
                .entry(site.id)
                .and_modify(|prior| *prior = None)
                .or_insert(proof);
        }
        Ok(())
    }

    fn select_qualified_header(
        &self,
        owner: &EntityRecord,
        lookup: &rootlight_ir::GroovyTypeLookup,
        cancellation: &Cancellation,
    ) -> Result<Option<Arc<Declaration<'a>>>, ResolutionError> {
        cancellation.check()?;
        let Some(owner_declaration) = owner
            .evidence
            .source
            .as_ref()
            .and_then(|source| self.declaration_sources.get(&source.span()))
            .and_then(Option::as_ref)
        else {
            return Ok(None);
        };
        let fallback = if let Some(span) = lookup.fallback_local {
            let Some(declaration) = self.declaration_sources.get(&span).and_then(Option::as_ref)
            else {
                return Ok(None);
            };
            Some(declaration)
        } else {
            None
        };
        let literal = content_hash(lookup.name.as_bytes());
        if let Some(Some(target)) = self
            .ranked_declarations
            .get(&literal)
            .and_then(|ranks| ranks.get(&0))
            && target
                .entity
                .evidence
                .source
                .as_ref()
                .map(|source| source.span().file())
                == owner
                    .evidence
                    .source
                    .as_ref()
                    .map(|source| source.span().file())
        {
            return Ok(Some(Arc::clone(target)));
        }
        // Package-qualified source names are tried before right-to-left nested
        // class substitutions. Keep duplicate identities at the first matching
        // depth unavailable instead of falling through to a less preferred type.
        let mut package_ranks = None;
        if let Some(package) = owner_declaration.owners.iter().find(|item| {
            item.kind == EntityKind::Module
                && matches!(item.container, Some(ContainerRef::Entity(_)))
        }) {
            let length = package
                .canonical_name
                .len()
                .checked_add(1)
                .and_then(|length| length.checked_add(lookup.name.len()))
                .ok_or(ResolutionError::CountOverflow)?;
            if length > IrLimits::default().max_string_bytes {
                return Ok(None);
            }
            let prefixed =
                content_hash(format!("{}.{}", package.canonical_name, lookup.name).as_bytes());
            package_ranks = self.ranked_declarations.get(&prefixed);
            if let Some(target) = package_ranks.and_then(|ranks| ranks.get(&0)) {
                return Ok(target.as_ref().map(Arc::clone));
            }
        }
        if let Some((_, target)) = self
            .ranked_declarations
            .get(&literal)
            .and_then(BTreeMap::first_key_value)
        {
            return Ok(target.as_ref().map(Arc::clone));
        }
        if let Some(target) = fallback {
            return Ok(Some(Arc::clone(target)));
        }
        // Groovy retries nested names with module imports only after literal
        // binary names and enclosing lexical types have failed. The current
        // package can then supply a nested declaration from another source unit.
        Ok(package_ranks
            .and_then(BTreeMap::first_key_value)
            .and_then(|(_, target)| target.as_ref())
            .map(Arc::clone))
    }

    pub(crate) fn proof(&self, occurrence: &OccurrenceRecord) -> Option<Proof<'a, '_>> {
        if self.unavailable {
            return None;
        }
        let source = self.contexts.get(&occurrence.file)?;
        if !self.has_source_owned_provenance(occurrence)
            || occurrence.source.content_hash() != source.file.content_hash
        {
            return None;
        }
        let qualified_header = self
            .qualified_headers
            .get(&occurrence.id)
            .and_then(Option::as_ref);
        if self.qualified_headers.contains_key(&occurrence.id)
            && (qualified_header.is_none() || self.uses.contains_key(&occurrence.source.span()))
        {
            return None;
        }
        let (name, binding) = if (is_header(occurrence) && qualified_header.is_some())
            || (occurrence.role == OccurrenceRole::ImportUse
                && occurrence.syntax_kind == "groovy.type_import.reference")
        {
            (occurrence.syntactic_text_hash, None)
        } else if (occurrence.role == OccurrenceRole::TypeUse
            && matches!(
                occurrence.syntax_kind.as_str(),
                "groovy.type_identifier.reference" | "groovy.qualified_type.reference"
            ))
            || (occurrence.role == OccurrenceRole::InheritanceUse
                && occurrence.syntax_kind == "groovy.extends_type.reference")
            || (occurrence.role == OccurrenceRole::ImplementationUse
                && occurrence.syntax_kind == "groovy.implements_type.reference")
        {
            let (envelope, binding) = self.uses.get(&occurrence.source.span())?.as_ref()?;
            if envelope.evidence.source.as_ref() != Some(&occurrence.source) {
                return None;
            }
            (binding.target, Some(*envelope))
        } else {
            return None;
        };
        let target = if let Some(header) = qualified_header {
            header.target.as_ref()?
        } else {
            self.declarations.get(&name)?.as_ref()?
        };
        let destination = self
            .contexts
            .get(&target.entity.evidence.source.as_ref()?.span().file())?;
        Some(Proof {
            target: target.entity,
            binding,
            qualified_header: qualified_header.map(|header| (header.envelope, header.owner)),
            owners: &target.owners,
            coverage: [
                source.coverage[0],
                source.coverage[1],
                destination.coverage[0],
                destination.coverage[1],
            ],
        })
    }

    pub(crate) fn resolve(
        &self,
        occurrence: &OccurrenceRecord,
        candidate_limit: usize,
    ) -> Result<Option<ResolutionDecision>, ResolutionError> {
        let inherited = self.inherited.get(&occurrence.id);
        let (symbol, rule, signal) = if let Some(entry) = self.static_imports.get(&occurrence.id) {
            (
                entry.selection.target.map(|target| target.id),
                ResolutionRule::GroovyStaticImport,
                ResolutionSignal::NativeStaticImport,
            )
        } else if let Some(entry) = inherited {
            (
                entry.selection.target.map(|target| target.id),
                ResolutionRule::GroovyInheritedType,
                ResolutionSignal::NativeInheritance,
            )
        } else {
            let Some(proof) = self.proof(occurrence) else {
                return Ok(None);
            };
            (
                Some(proof.target.id),
                if proof.qualified_header.is_some() {
                    ResolutionRule::GroovyQualifiedHeader
                } else {
                    ResolutionRule::GroovyTypeImport
                },
                if proof.qualified_header.is_some() {
                    ResolutionSignal::NativeQualifiedHeader
                } else {
                    ResolutionSignal::NativeTypeImport
                },
            )
        };
        let confidence =
            rootlight_ir::Confidence::new(900).expect("fixed structural confidence is valid");
        // An unproven hierarchy is a language-owned negative decision, not
        // permission to recover a same-spelling candidate through generic scoring.
        let mut outcome = symbol.map_or(
            ResolutionOutcome::Unresolved {
                reason: UnresolvedReason::MissingDependency,
                confidence: rootlight_ir::Confidence::new(0)
                    .expect("zero unresolved confidence is valid"),
            },
            |symbol| ResolutionOutcome::Resolved { symbol, confidence },
        );
        let mut symbols: Vec<_> = symbol.into_iter().collect();
        if let Some(candidates) = self
            .static_imports
            .get(&occurrence.id)
            .and_then(|entry| entry.selection.candidates.as_ref())
        {
            symbols = candidates
                .symbols
                .iter()
                .copied()
                .take(candidate_limit)
                .collect();
            let retained =
                u64::try_from(symbols.len()).map_err(|_| ResolutionError::CountOverflow)?;
            outcome = ResolutionOutcome::Candidates {
                symbols: symbols.clone(),
                total_count: candidates.total_count,
                completeness: if retained == candidates.total_count {
                    CoverageStatus::Complete
                } else {
                    CoverageStatus::Bounded
                },
                confidence,
            };
        }
        Ok(Some(ResolutionDecision {
            occurrence: occurrence.id,
            outcome,
            explanation: ResolutionExplanation {
                rule,
                provider_name: RESOLVER_PROVIDER_NAME,
                provider_version: RESOLVER_PROVIDER_VERSION,
                candidates: symbols
                    .into_iter()
                    .map(|symbol| CandidateExplanation {
                        symbol,
                        score: confidence,
                        positive_signals: vec![signal],
                        penalties: Vec::new(),
                    })
                    .collect(),
                rejected_candidates: Vec::new(),
                rejected_total: 0,
                completeness_assumptions: vec![
                    CompletenessAssumption::ValidatedNormalizedDocument,
                    CompletenessAssumption::SingleGeneration,
                    CompletenessAssumption::NoRepositoryExecution,
                    CompletenessAssumption::IndexedPackageSnapshot,
                ],
            },
        }))
    }

    pub(crate) fn inherited_entry(
        &self,
        occurrence: &OccurrenceRecord,
    ) -> Option<&hierarchy::Entry<'a>> {
        self.inherited.get(&occurrence.id)
    }

    pub(crate) fn static_import_entry(
        &self,
        occurrence: &OccurrenceRecord,
    ) -> Option<&hierarchy::Entry<'a>> {
        self.static_imports.get(&occurrence.id)
    }

    fn has_source_owned_provenance(&self, site: &OccurrenceRecord) -> bool {
        self.contexts.get(&site.file).is_some_and(|context| {
            site.provenance == context.file.provenance || self.derived_sites.contains(&site.id)
        })
    }
}

fn source_owned_derived_site(
    site: &OccurrenceRecord,
    context: &Context<'_>,
    record: &ProvenanceRecord,
) -> bool {
    let targets_proven = match &site.target {
        OccurrenceTarget::Resolved { symbol } => record
            .derivation_parents
            .contains(&FactRef::Entity(*symbol)),
        OccurrenceTarget::Candidates { symbols, .. } => {
            site.role == OccurrenceRole::ImportUse
                && site.syntax_kind == "groovy.static_import.reference"
                && record.rule.as_deref() == Some("groovy-static-import-v1.indexed_snapshot")
                && !symbols.is_empty()
                && symbols.iter().all(|symbol| {
                    record
                        .derivation_parents
                        .contains(&FactRef::Entity(*symbol))
                })
        }
        _ => false,
    };
    // Bounded passes replace native occurrence provenance. A later pass may
    // reuse that source inventory only when the persisted resolver identity,
    // exact source and complete file/entity coverage still agree. Parent targets
    // are independently selected again before they enter the hierarchy.
    let rule_matches = match (site.role, record.rule.as_deref()) {
        (OccurrenceRole::Reference, Some("groovy-static-import-v1.indexed_snapshot")) => {
            site.syntax_kind == "groovy.identifier.reference"
        }
        (OccurrenceRole::CallSite, Some("groovy-static-import-v1.indexed_snapshot")) => {
            site.syntax_kind == "groovy.implicit_call.call"
        }
        (OccurrenceRole::ImportUse, Some("groovy-static-import-v1.indexed_snapshot")) => matches!(
            site.syntax_kind.as_str(),
            "groovy.static_import.reference" | "groovy.static_star_import.reference"
        ),
        (
            OccurrenceRole::InheritanceUse | OccurrenceRole::ImplementationUse,
            Some("groovy-qualified-header-v1.indexed_snapshot"),
        ) => is_header(site),
        (OccurrenceRole::TypeUse, Some("groovy-inherited-type-v1.indexed_snapshot")) => matches!(
            site.syntax_kind.as_str(),
            "groovy.type_identifier.reference" | "groovy.qualified_type.reference"
        ),
        (OccurrenceRole::ImportUse, Some("groovy-type-import-v1.indexed_snapshot")) => {
            site.syntax_kind == "groovy.type_import.reference"
        }
        (
            OccurrenceRole::TypeUse
            | OccurrenceRole::InheritanceUse
            | OccurrenceRole::ImplementationUse,
            Some("groovy-type-import-v1.indexed_snapshot"),
        ) => {
            is_header(site)
                || matches!(
                    site.syntax_kind.as_str(),
                    "groovy.type_identifier.reference" | "groovy.qualified_type.reference"
                )
        }
        _ => false,
    };
    rule_matches
        && record.producer_kind == ProducerKind::Rule
        && record.producer.name() == RESOLVER_PROVIDER_NAME
        && record.producer.version() == env!("CARGO_PKG_VERSION")
        && record.frontend_version.as_deref() == Some(RESOLVER_PROVIDER_VERSION)
        && record.language == "groovy"
        && site.source.content_hash() == context.file.content_hash
        && site.evidence.source.as_ref() == Some(&site.source)
        && record.input_sources == record.evidence_sources
        && record.evidence_sources.contains(&site.source)
        && targets_proven
        && context.coverage.iter().all(|coverage| {
            record
                .derivation_parents
                .contains(&FactRef::Fact(coverage.id))
                && coverage
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| record.evidence_sources.contains(source))
        })
        && rootlight_ir::derive_provenance_record_id(record).ok() == Some(record.id)
        && rootlight_ir::derive_occurrence_record_id(site).ok() == Some(site.id)
}

fn is_header(site: &OccurrenceRecord) -> bool {
    matches!(
        (site.role, site.syntax_kind.as_str()),
        (
            OccurrenceRole::InheritanceUse,
            "groovy.extends_type.reference"
        ) | (
            OccurrenceRole::ImplementationUse,
            "groovy.implements_type.reference"
        )
    )
}

fn is_type(kind: EntityKind) -> bool {
    matches!(
        kind,
        EntityKind::Class
            | EntityKind::Struct
            | EntityKind::Interface
            | EntityKind::Trait
            | EntityKind::Enum
    )
}

fn declaration_path<'a>(
    entity: &'a EntityRecord,
    context: &Context<'a>,
    entities: &BTreeMap<SymbolId, &'a EntityRecord>,
    cancellation: &Cancellation,
) -> Result<Option<(ContentHash, Declaration<'a>)>, ResolutionError> {
    let mut current = entity;
    let mut parts = Vec::new();
    let mut owners = Vec::new();
    let mut package_seen = false;
    let mut length = 0_usize;
    for _ in 0..super::engine::MAX_SCOPE_DEPTH {
        cancellation.check()?;
        let Some(source) = current.evidence.source.as_ref() else {
            return Ok(None);
        };
        if current.language != "groovy"
            || current.provenance != context.file.provenance
            || source.span().file() != context.file.id
            || source.content_hash() != context.file.content_hash
        {
            return Ok(None);
        }
        if current.kind == EntityKind::Module
            && current.container == Some(ContainerRef::File(context.file.id))
        {
            if current.canonical_name != context.file.path
                || source.span().start_byte() != 0
                || source.span().end_byte() != context.file.byte_length
            {
                return Ok(None);
            }
            owners.push(current);
            parts.reverse();
            return Ok(Some((
                content_hash(parts.join(".").as_bytes()),
                Declaration { entity, owners },
            )));
        }
        if current.kind == EntityKind::Module && !package_seen {
            package_seen = true;
        } else if package_seen || !is_type(current.kind) {
            return Ok(None);
        }
        length = length
            .checked_add(current.canonical_name.len())
            .and_then(|length| length.checked_add(usize::from(!parts.is_empty())))
            .ok_or(ResolutionError::CountOverflow)?;
        if length > IrLimits::default().max_string_bytes {
            return Ok(None);
        }
        parts.push(current.canonical_name.as_str());
        if current.id != entity.id {
            owners.push(current);
        }
        let Some(ContainerRef::Entity(parent)) = current.container else {
            return Ok(None);
        };
        let Some(parent) = entities.get(&parent).copied() else {
            return Ok(None);
        };
        current = parent;
    }
    Ok(None)
}
