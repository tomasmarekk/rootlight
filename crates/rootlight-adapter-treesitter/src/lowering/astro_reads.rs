//! Consume exact native frontmatter-to-template target certificates.
//! A declaration endpoint selects a current-source TypeScript Variable in the
//! certified server module; spelling equality alone never selects a target.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::MaterializedEntity;

/// Keep certified references atomic beside their mandatory exact-target metadata.
pub(super) fn required(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "astro.template_binding_read.reference"
}

struct Binding {
    target: SymbolId,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

pub(super) struct ReadTargets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.span().file() == right.span().file()
        && left.content_hash() == right.content_hash()
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|old| *old = None)
        .or_insert(Some(value));
}

impl ReadTargets {
    /// Join one clean host/server context to exact Variable definition and read endpoints.
    /// Duplicates, foreign frames and client-module ancestry withhold binding rather than guess a donor.
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        by_id: &HashMap<u64, &SyntaxFact>,
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut host = None;
        let mut server = None;
        let mut server_signature_span = None;
        let mut clean_hosts = 0;
        let mut clean_servers = 0;
        let mut invalid = by_id.len() != facts.len();
        let mut native_declarations = HashMap::new();
        let mut native_definitions = HashMap::new();
        for fact in facts {
            cancel.check()?;
            invalid |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == "typescript.variable.declaration"
            {
                native_declarations
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                unique(&mut native_declarations, physical(fact.span())?, fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "typescript.identifier.definition"
                && let Some(parent) = fact.parent()
            {
                native_definitions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                unique(
                    &mut native_definitions,
                    (parent, physical(fact.span())?),
                    fact,
                );
            }
            match (fact.kind(), fact.syntax_kind().as_str()) {
                (SyntaxFactKind::Signature, "astro.template_clean_host.signature") => {
                    clean_hosts += 1;
                    invalid |= physical(fact.span())? != source.span();
                }
                (SyntaxFactKind::Module, "astro.file.module")
                    if physical(fact.span())? == source.span() =>
                {
                    invalid |= host.is_some();
                    host = Some(fact.local_id());
                }
                (SyntaxFactKind::Signature, "astro.template_clean_server.signature") => {
                    clean_servers += 1;
                    server = fact.parent();
                    server_signature_span = Some(fact.span());
                }
                _ => {}
            }
        }
        let mut result = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        let Some(server) = server.and_then(|local| by_id.get(&local).copied()) else {
            return Ok(result);
        };
        let Some(host) = host else {
            return Ok(result);
        };
        if invalid
            || clean_hosts != 1
            || clean_servers != 1
            || server.kind() != SyntaxFactKind::Module
            || server.syntax_kind().as_str() != "typescript.file.module"
            || server.parent() != Some(host)
        {
            return Ok(result);
        }
        let server_span = physical(server.span())?;
        if server_signature_span != Some(server.span()) {
            return Ok(result);
        }
        let Some(server_entity) = entities.get(&server.local_id()) else {
            return Ok(result);
        };
        if server_entity.record.kind != EntityKind::Module
            || server_entity.record.language != "typescript"
            || server_entity.record.repository != source.repository()
            || server_entity.record.generation != source.generation()
            || server_entity
                .record
                .evidence
                .source
                .as_ref()
                .is_none_or(|evidence| {
                    !same_source(evidence, source) || evidence.span() != server_span
                })
        {
            return Ok(result);
        }
        let mut declarations = HashMap::new();
        let mut reads = HashMap::new();
        let mut targets = HashMap::new();
        for (local, variable) in entities {
            cancel.check()?;
            if variable.record.kind != EntityKind::Variable
                || variable.record.language != "typescript"
                || variable.direct_parent != ContainerRef::Entity(server_entity.record.id)
            {
                continue;
            }
            let Some(declaration) = by_id.get(local).copied() else {
                continue;
            };
            let Some(definition) = variable
                .definition_local_id
                .and_then(|id| by_id.get(&id).copied())
            else {
                continue;
            };
            let Some(evidence) = variable.record.evidence.source.as_ref() else {
                continue;
            };
            let span = physical(definition.span())?;
            if declaration.kind() != SyntaxFactKind::Declaration
                || declaration.syntax_kind().as_str() != "typescript.variable.declaration"
                || physical(declaration.span())? != evidence.span()
                || native_declarations
                    .get(&evidence.span())
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(*local)
                || definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != "typescript.identifier.definition"
                || native_definitions
                    .get(&(*local, span))
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(definition.local_id())
                || definition.parent() != Some(*local)
                || !same_source(evidence, source)
                || span.file() != source.span().file()
                || evidence.span().start_byte() > span.start_byte()
                || evidence.span().end_byte() < span.end_byte()
                || variable.record.provenance != server_entity.record.provenance
                || span.start_byte() < server_span.start_byte()
                || span.end_byte() > server_span.end_byte()
                || variable.record.repository != source.repository()
                || variable.record.generation != source.generation()
                || text(definition.span())? != variable.record.canonical_name
            {
                continue;
            }
            if !reaches_module(
                declaration.parent(),
                server.local_id(),
                by_id,
                facts.len(),
                cancel,
            )? {
                continue;
            }
            declarations
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            unique(&mut declarations, span.start_byte(), (variable, definition));
        }
        for fact in facts {
            cancel.check()?;
            let span = physical(fact.span())?;
            if required(fact) {
                reads
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                unique(&mut reads, span.end_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "astro.template_read_target.signature"
            {
                targets
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                unique(&mut targets, span.end_byte(), fact);
            }
        }
        result
            .bindings
            .try_reserve(targets.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for target in targets.values().copied().flatten() {
            cancel.check()?;
            let target_span = physical(target.span())?;
            let Some((variable, definition)) = declarations
                .get(&target_span.start_byte())
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some(read) = reads.get(&target_span.end_byte()).copied().flatten() else {
                continue;
            };
            let read_span = physical(read.span())?;
            if target.parent() != Some(host)
                || read_span.start_byte() < server_span.end_byte()
                || read_span.end_byte() > source.span().end_byte()
                || target_span.file() != source.span().file()
                || read_span.file() != source.span().file()
                || text(read.span())? != text(definition.span())?
                || text(read.span())? != variable.record.canonical_name
            {
                continue;
            }
            if !reaches_module(read.parent(), host, by_id, facts.len(), cancel)? {
                continue;
            }
            result.bindings.insert(
                read.local_id(),
                Binding {
                    target: variable.record.id,
                    span: read_span,
                    spelling: content_hash(text(read.span())?.as_bytes()),
                    provenance: variable.record.provenance,
                },
            );
        }
        Ok(result)
    }

    /// Adopt only the same current-source unresolved Reference retained by the checked certificate index.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if !required(fact)
            || occurrence.syntax_kind != "astro.template_binding_read.reference"
            || occurrence.role != OccurrenceRole::Reference
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || !same_source(&occurrence.source, &self.source)
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
        {
            return Ok(None);
        }
        Ok(self
            .bindings
            .get(&fact.local_id())
            .filter(|binding| {
                binding.span == physical
                    && physical == occurrence.source.span()
                    && binding.spelling == occurrence.syntactic_text_hash
                    && binding.provenance == occurrence.provenance
            })
            .map(|binding| binding.target))
    }
}

fn reaches_module(
    mut parent: Option<u64>,
    expected: u64,
    by_id: &HashMap<u64, &SyntaxFact>,
    maximum: usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    for _ in 0..maximum {
        cancel.check()?;
        let Some(ancestor) = parent.and_then(|id| by_id.get(&id).copied()) else {
            return Ok(false);
        };
        if ancestor.local_id() == expected {
            return Ok(true);
        }
        if ancestor.kind() == SyntaxFactKind::Module {
            return Ok(false);
        }
        parent = ancestor.parent();
    }
    Ok(false)
}

#[cfg(test)]
mod qualification_tests {
    use super::super::{
        Lowering, NativeSourceView, TreeSitterAnalyzer, exclude_ambiguous_entities,
        materialize_entity,
    };
    use super::*;
    use rootlight_adapter_sdk::{
        AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId, GenerationBoundSnapshot,
        LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis, execute_parse,
    };
    use rootlight_ids::{GenerationId, RepositoryId, derive_repository};
    use rootlight_ir::{
        AnalysisTier, BuildContextIdentity, ExtensionSupport, IrLimits, ProducerIdentity,
    };
    use rootlight_vfs::{RelativePath, RepositoryRoot};
    use std::{fs, path::Path, sync::Arc};

    const ORIGINAL: &[u8] = include_bytes!("../../tests/fixtures/astro_source_fields/Grid.astro");

    fn with_native(
        raw: &[u8],
        operation: impl FnOnce(
            &Lowering<'_, '_>,
            &HashMap<u64, MaterializedEntity>,
            &SyntaxFact,
            &OccurrenceRecord,
            SymbolId,
            u64,
        ),
    ) {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("written.astro"), raw).unwrap();
        let repository = derive_repository(b"astro-native-consumer-qualification").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(Path::new("written.astro")).unwrap(),
                1024 * 1024,
            )
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([7; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let provider = Arc::new(
            crate::TreeSitterProvider::new(
                crate::RuntimeConfig::new(
                    1024 * 1024,
                    16_384,
                    128,
                    16,
                    64,
                    1,
                    16 * 1024 * 1024,
                    crate::ParserSettings::new(256).unwrap(),
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let analyzer = TreeSitterAnalyzer::new(
            provider.clone(),
            ProducerIdentity::new(
                "astro-native-consumer-qualification",
                "1.0",
                content_hash(b"astro-consumer-config"),
            )
            .unwrap(),
            LanguageId::new("astro").unwrap(),
            "astro-native-consumer-qualification",
            content_hash(b"astro-consumer-binary"),
        )
        .unwrap();
        let stream = || {
            StreamLimits::new(
                128,
                4096,
                16 * 1024 * 1024,
                128,
                128 * 1024,
                1024 * 1024,
                BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
            )
            .unwrap()
        };
        let limits = AnalysisLimits::new(
            1024 * 1024,
            16_384,
            128,
            16,
            16 * 1024 * 1024,
            stream(),
            stream(),
            IrLimits::default(),
        )
        .unwrap();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("astro").unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"astro-consumer-build")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let cancel = Cancellation::new();
        let parsed = execute_parse(
            provider.as_ref(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel,
        )
        .unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            rootlight_ir::CoverageStatus::Complete
        );
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel,
        )
        .unwrap();
        let read = output
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "astro.template_binding_read.reference"
                    && site.source.span().start_byte() == 267
                    && site.source.span().end_byte() == 272
            })
            .expect(
                "the actual original must supply its native certified reference before tampering",
            );
        let OccurrenceTarget::Resolved { symbol } = read.target else {
            panic!("the baseline actual original must bind before consumer tampering");
        };
        let view = NativeSourceView::new(
            raw,
            "utf-8",
            limits.max_source_bytes(),
            limits.max_reported_memory_bytes(),
            &cancel,
        )
        .unwrap();
        let lowering = Lowering::new(&analyzer, &request, &parsed, &view, &cancel).unwrap();
        let mut strings = 0;
        let plan = lowering.entity_drafts(&cancel, &mut strings).unwrap();
        let provenance = lowering.provenance().unwrap().id;
        let mut entities = HashMap::new();
        for draft in &plan.drafts {
            let entity = materialize_entity(
                draft,
                &source,
                &lowering.coordinates,
                draft
                    .authored_display_span
                    .map(|span| lowering.text_for_span(span))
                    .transpose()
                    .unwrap(),
                request.build_context(),
                provenance,
                analyzer.descriptor.tier(),
                limits.ir().max_string_bytes,
                &entities,
            )
            .unwrap();
            entities.insert(draft.local_id, entity);
        }
        let _excluded = exclude_ambiguous_entities(&mut entities);
        let (&local, variable) = entities
            .iter()
            .find(|(_, entity)| entity.record.id == symbol)
            .unwrap();
        assert_eq!(variable.record.kind, EntityKind::Variable);
        let definition = variable.definition_local_id.unwrap();
        assert!(
            lowering
                .coordinates
                .facts()
                .iter()
                .any(|fact| fact.local_id() == definition
                    && fact.span().start_byte() == 161
                    && fact.span().end_byte() == 166)
        );
        let fact = lowering
            .coordinates
            .facts()
            .iter()
            .find(|fact| {
                required(fact) && fact.span().start_byte() == 267 && fact.span().end_byte() == 272
            })
            .unwrap();
        let mut unresolved = read.clone();
        unresolved.target = OccurrenceTarget::Unresolved {
            text_hash: read.syntactic_text_hash,
        };
        operation(&lowering, &entities, fact, &unresolved, symbol, local);
    }

    fn join(
        lowering: &Lowering<'_, '_>,
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        cancel: &Cancellation,
    ) -> Result<ReadTargets, AdapterError> {
        let by_id = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
        ReadTargets::new(
            facts,
            &by_id,
            entities,
            source,
            |span| lowering.text_for_span(span),
            |span| lowering.coordinates.physical(span),
            cancel,
        )
    }

    fn duplicate(facts: &mut Vec<SyntaxFact>, original: &SyntaxFact) {
        let local = facts
            .iter()
            .map(SyntaxFact::local_id)
            .max()
            .unwrap()
            .checked_add(1)
            .unwrap();
        facts.push(SyntaxFact::new(
            local,
            original.parent(),
            original.kind(),
            original.span(),
            original.depth(),
            original.syntax_kind().clone(),
        ));
    }

    #[test]
    fn actual_native_template_join_refuses_missing_or_duplicate_required_witnesses() {
        with_native(
            ORIGINAL,
            |lowering, entities, read, occurrence, symbol, local| {
                let cancel = Cancellation::new();
                let source = lowering.full_source;
                let facts = lowering.coordinates.facts();
                let baseline = join(lowering, facts, entities, source, &cancel).unwrap();
                assert_eq!(
                    baseline
                        .resolve(read, occurrence.source.span(), occurrence, &cancel)
                        .unwrap(),
                    Some(symbol)
                );
                let definition = entities[&local].definition_local_id.unwrap();
                let mut witnesses: Vec<_> = facts
                    .iter()
                    .filter(|fact| {
                        matches!(
                            fact.syntax_kind().as_str(),
                            "astro.template_clean_host.signature"
                                | "astro.template_clean_server.signature"
                                | "astro.template_read_target.signature"
                        ) || fact.local_id() == definition
                    })
                    .collect();
                witnesses.push(read);
                for witness in witnesses {
                    let kept: Vec<_> = facts
                        .iter()
                        .filter(|fact| fact.local_id() != witness.local_id())
                        .cloned()
                        .collect();
                    let missing = join(lowering, &kept, entities, source, &cancel).unwrap();
                    assert_eq!(
                        missing
                            .resolve(read, occurrence.source.span(), occurrence, &cancel)
                            .unwrap(),
                        None,
                        "missing native witness {}",
                        witness.syntax_kind().as_str()
                    );
                    let mut doubled = facts.to_vec();
                    duplicate(&mut doubled, witness);
                    let repeated = join(lowering, &doubled, entities, source, &cancel).unwrap();
                    assert_eq!(
                        repeated
                            .resolve(read, occurrence.source.span(), occurrence, &cancel)
                            .unwrap(),
                        None,
                        "duplicate native witness {}",
                        witness.syntax_kind().as_str()
                    );
                }
                let marker = facts
                    .iter()
                    .find(|fact| {
                        fact.syntax_kind().as_str() == "astro.template_read_target.signature"
                            && fact.span().end_byte() == read.span().end_byte()
                    })
                    .unwrap();
                let field = facts
                    .iter()
                    .find(|fact| {
                        fact.kind() == SyntaxFactKind::Occurrence
                            && fact.syntax_kind().as_str().ends_with(".definition")
                            && fact.span().start_byte() == 127
                            && fact.span().end_byte() == 132
                    })
                    .unwrap();
                let wrong = SyntaxFact::new(
                    marker.local_id(),
                    marker.parent(),
                    marker.kind(),
                    SourceSpan::new(
                        marker.span().file(),
                        field.span().start_byte(),
                        marker.span().end_byte(),
                    )
                    .unwrap(),
                    marker.depth(),
                    marker.syntax_kind().clone(),
                );
                let altered: Vec<_> = facts
                    .iter()
                    .map(|fact| {
                        if fact.local_id() == marker.local_id() {
                            wrong.clone()
                        } else {
                            fact.clone()
                        }
                    })
                    .collect();
                let field_donor = join(lowering, &altered, entities, source, &cancel).unwrap();
                assert_eq!(
                    field_donor
                        .resolve(read, occurrence.source.span(), occurrence, &cancel)
                        .unwrap(),
                    None
                );
            },
        );
    }

    #[test]
    fn actual_native_template_join_requires_its_own_unique_declaration() {
        with_native(
            ORIGINAL,
            |lowering, entities, read, occurrence, symbol, local| {
                let cancel = Cancellation::new();
                let source = lowering.full_source;
                let facts = lowering.coordinates.facts();
                let baseline = join(lowering, facts, entities, source, &cancel).unwrap();
                assert_eq!(
                    baseline
                        .resolve(read, occurrence.source.span(), occurrence, &cancel)
                        .unwrap(),
                    Some(symbol)
                );
                let declaration = facts.iter().find(|fact| fact.local_id() == local).unwrap();
                assert_eq!(declaration.kind(), SyntaxFactKind::Declaration);
                let kept: Vec<_> = facts
                    .iter()
                    .filter(|fact| fact.local_id() != local)
                    .cloned()
                    .collect();
                let missing = join(lowering, &kept, entities, source, &cancel).unwrap();
                assert_eq!(
                    missing
                        .resolve(read, occurrence.source.span(), occurrence, &cancel)
                        .unwrap(),
                    None,
                    "materialized state cannot replace its absent own native Declaration"
                );
                let mut doubled = facts.to_vec();
                duplicate(&mut doubled, declaration);
                let repeated = join(lowering, &doubled, entities, source, &cancel).unwrap();
                assert_eq!(
                    repeated
                        .resolve(read, occurrence.source.span(), occurrence, &cancel)
                        .unwrap(),
                    None,
                    "a duplicate own Declaration cannot donate a unique template target"
                );
            },
        );
    }

    #[test]
    fn actual_native_template_join_refuses_foreign_stale_other_module_and_cancelled_inputs() {
        let client_start = u64::try_from(ORIGINAL.len() + b"\n<script>".len()).unwrap();
        let client_end = u64::try_from(
            ORIGINAL.len() + b"\n<script>const posts = []; console.log(posts);".len(),
        )
        .unwrap();
        let raw = [
            ORIGINAL,
            b"\n<script>const posts = []; console.log(posts);</script>",
        ]
        .concat();
        with_native(
            &raw,
            |lowering, entities, read, occurrence, symbol, local| {
                let cancel = Cancellation::new();
                let source = lowering.full_source;
                let facts = lowering.coordinates.facts();
                let baseline = join(lowering, facts, entities, source, &cancel).unwrap();
                assert_eq!(
                    baseline
                        .resolve(read, occurrence.source.span(), occurrence, &cancel)
                        .unwrap(),
                    Some(symbol)
                );
                let client = entities
                    .values()
                    .find(|entity| {
                        entity.record.kind == EntityKind::Module
                            && entity.record.language == "typescript"
                            && entity
                                .record
                                .evidence
                                .source
                                .as_ref()
                                .is_some_and(|evidence| {
                                    evidence.span().start_byte() == client_start
                                        && evidence.span().end_byte() == client_end
                                })
                    })
                    .expect("real client native module prerequisite");
                for mode in 0..4 {
                    let mut changed = entities.clone();
                    let variable = changed.get_mut(&local).unwrap();
                    match mode {
                        0 => variable.record.repository = RepositoryId::from_bytes([99; 16]),
                        1 => variable.record.generation = GenerationId::from_bytes([99; 20]),
                        2 => {
                            let evidence = variable.record.evidence.source.as_ref().unwrap();
                            variable.record.evidence.source = Some(SourceRef::new(
                                source.repository(),
                                source.generation(),
                                evidence.span(),
                                content_hash(b"stale bytes"),
                                None,
                            ));
                        }
                        _ => variable.direct_parent = ContainerRef::Entity(client.record.id),
                    }
                    let rejected = join(lowering, facts, &changed, source, &cancel).unwrap();
                    assert_eq!(
                        rejected
                            .resolve(read, occurrence.source.span(), occurrence, &cancel)
                            .unwrap(),
                        None,
                        "mode{mode}"
                    );
                }
                let mut wrong_role = occurrence.clone();
                wrong_role.role = OccurrenceRole::CallSite;
                assert_eq!(
                    baseline
                        .resolve(read, wrong_role.source.span(), &wrong_role, &cancel)
                        .unwrap(),
                    None
                );
                let mut stale = occurrence.clone();
                stale.source = SourceRef::new(
                    source.repository(),
                    source.generation(),
                    occurrence.source.span(),
                    content_hash(b"stale read"),
                    None,
                );
                assert_eq!(
                    baseline
                        .resolve(read, stale.source.span(), &stale, &cancel)
                        .unwrap(),
                    None
                );
                let wrong = SourceSpan::new(source.span().file(), 268, 272).unwrap();
                assert_eq!(
                    baseline.resolve(read, wrong, occurrence, &cancel).unwrap(),
                    None
                );
                let cancelled = Cancellation::new();
                cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
                assert!(matches!(
                    join(lowering, facts, entities, source, &cancelled),
                    Err(AdapterError::Cancelled { .. })
                ));
                assert!(matches!(
                    baseline.resolve(read, occurrence.source.span(), occurrence, &cancelled),
                    Err(AdapterError::Cancelled { .. })
                ));
            },
        );
    }
}
