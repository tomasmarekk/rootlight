//! Bind Flux formal Reads only with an exact native local-target certificate.
//! The consumer rechecks the Parameter, its Function owner and current source frame.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    SourceRef, SourceSpan,
};

use super::MaterializedEntity;

pub(super) fn written_identity(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Function, "flux.function.declaration")
            | (EntityKind::Parameter, "flux.parameter.declaration")
    )
}

pub(super) fn signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "flux.header.signature"
}

pub(super) fn occurrence_role(fact: &SyntaxFact) -> Option<OccurrenceRole> {
    match (fact.kind(), fact.syntax_kind().as_str()) {
        (
            SyntaxFactKind::Occurrence,
            "flux.parameter_read.reference" | "flux.unresolved_read.reference",
        ) => Some(OccurrenceRole::Read),
        _ => None,
    }
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let reason = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "flux.source_file") => {
            Some("flux-general-declarations-imports-types-flow-and-runtime-bindings-unavailable")
        }
        (SyntaxFactKind::Signature, "flux.construct_unavailable.expression") => {
            Some("flux-native-construct-analysis-unavailable")
        }
        (SyntaxFactKind::ErrorRecovery, "flux.native_recovery") => {
            Some("flux-native-parse-recovery")
        }
        (SyntaxFactKind::ErrorRecovery, "flux.regex_validation_unavailable") => {
            Some("flux-regex-validation-unavailable")
        }
        (SyntaxFactKind::ErrorRecovery, "flux.time_precision_unavailable") => {
            Some("flux-time-precision-unavailable")
        }
        (SyntaxFactKind::ErrorRecovery, "flux.native_depth_unavailable") => {
            Some("flux-native-depth-unavailable")
        }
        _ => None,
    };
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| reason.map(|reason| (domain, reason)))
}

pub(super) fn read_ends(
    facts: &[SyntaxFact],
    cancel: &Cancellation,
) -> Result<HashMap<(rootlight_ids::FileId, u64), ()>, AdapterError> {
    let mut ends = HashMap::new();
    for fact in facts {
        cancel.check()?;
        if fact.kind() == SyntaxFactKind::Signature
            && fact.syntax_kind().as_str() == "flux.parameter_target.signature"
        {
            ends.try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            ends.insert((fact.span().file(), fact.span().end_byte()), ());
        }
    }
    Ok(ends)
}

struct Binding {
    target: SymbolId,
    method: SymbolId,
    span: SourceSpan,
    spelling: ContentHash,
    provenance: FactId,
}

pub(super) struct ReadTargets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

fn unique<K: std::hash::Hash + Eq, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|previous| *previous = None)
        .or_insert(Some(value));
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.span().file() == right.span().file()
        && left.content_hash() == right.content_hash()
}

impl ReadTargets {
    pub(super) fn new<'source>(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'source str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        // Certificates require one exact full-file clean signature. Included
        // partitions and recovered/unsupported parse frames never enter here.
        let mut root = None;
        let mut duplicate_root = false;
        for fact in facts {
            cancel.check()?;
            if fact.kind() == SyntaxFactKind::Root
                && fact.syntax_kind().as_str() == "flux.source_file"
                && physical(fact.span())? == source.span()
            {
                duplicate_root |= root.is_some();
                root = Some(fact.local_id());
            }
        }
        let mut clean_signatures = 0;
        let mut recovered = false;
        for fact in facts {
            cancel.check()?;
            recovered |= fact.kind() == SyntaxFactKind::ErrorRecovery;
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "flux.clean_parse.signature"
                && fact.parent() == root
                && physical(fact.span())? == source.span()
            {
                clean_signatures += 1;
            }
        }
        let clean = root.is_some() && !duplicate_root && clean_signatures == 1 && !recovered;
        if !clean {
            return Ok(Self {
                source: source.clone(),
                bindings: HashMap::new(),
            });
        }
        let mut declarations = HashMap::new();
        let mut definitions = HashMap::new();
        let mut reads = HashMap::new();
        let mut markers = HashMap::new();
        let mut methods = HashMap::new();
        for map in [
            &mut declarations,
            &mut definitions,
            &mut reads,
            &mut markers,
            &mut methods,
        ] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        let mut by_symbol = HashMap::new();
        by_symbol
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            unique(&mut by_symbol, entity.record.id, (*local, entity));
        }
        for fact in facts {
            cancel.check()?;
            let physical_span = physical(fact.span())?;
            if physical_span.file() != source.span().file()
                || physical_span.start_byte() < source.span().start_byte()
                || physical_span.end_byte() > source.span().end_byte()
            {
                continue;
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == "flux.parameter.declaration"
            {
                unique(&mut declarations, physical_span.start_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == "flux.function.declaration"
            {
                unique(&mut methods, physical_span.start_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "flux.name.definition"
                && let Some(parent) = fact.parent()
            {
                unique(&mut definitions, parent, fact);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == "flux.parameter_read.reference"
            {
                unique(&mut reads, physical_span.end_byte(), fact);
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "flux.parameter_target.signature"
            {
                unique(&mut markers, physical_span.end_byte(), fact);
            }
        }
        let mut bindings = HashMap::new();
        bindings
            .try_reserve(markers.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for marker in markers.values().copied().flatten() {
            cancel.check()?;
            let marker_span = physical(marker.span())?;
            let Some(declaration) = declarations
                .get(&marker_span.start_byte())
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some(definition) = definitions.get(&declaration.local_id()).copied().flatten()
            else {
                continue;
            };
            let Some(read) = reads.get(&marker_span.end_byte()).copied().flatten() else {
                continue;
            };
            let Some(variable) = entities.get(&declaration.local_id()) else {
                continue;
            };
            let Some(variable_source) = variable.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(ContainerRef::Entity(method_id)) = variable.record.container else {
                continue;
            };
            let Some((method_local, method)) = by_symbol.get(&method_id).copied().flatten() else {
                continue;
            };
            let Some(method_source) = method.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(method_declaration) = methods
                .get(&method_source.span().start_byte())
                .copied()
                .flatten()
            else {
                continue;
            };
            let Some(method_definition) = definitions.get(&method_local).copied().flatten() else {
                continue;
            };
            let declaration_span = physical(declaration.span())?;
            let definition_span = physical(definition.span())?;
            let read_span = physical(read.span())?;
            let method_declaration_span = physical(method_declaration.span())?;
            let method_definition_span = physical(method_definition.span())?;
            if method_declaration.parent() != root
                || marker.parent() != Some(method_local)
                || read.parent() != Some(method_local)
                || declaration.parent() != Some(method_local)
                || definition.parent() != Some(declaration.local_id())
                || method_definition.parent() != Some(method_local)
                || variable.record.language != "flux"
                || variable.record.kind != EntityKind::Parameter
                || method.record.language != "flux"
                || method.record.kind != EntityKind::Function
                || variable.record.repository != source.repository()
                || variable.record.generation != source.generation()
                || method.record.repository != source.repository()
                || method.record.generation != source.generation()
                || variable.definition_local_id != Some(definition.local_id())
                || method.definition_local_id != Some(method_definition.local_id())
                || !same_source(variable_source, source)
                || !same_source(method_source, source)
                || variable_source.span() != declaration_span
                || method_declaration.local_id() != method_local
                || method_declaration_span != method_source.span()
                || text(method_definition.span())? != method.record.canonical_name
                || definition_span.start_byte() < declaration_span.start_byte()
                || definition_span.end_byte() > declaration_span.end_byte()
                || method_definition_span.start_byte() < method_source.span().start_byte()
                || method_definition_span.end_byte() > method_source.span().end_byte()
                || declaration_span.start_byte() < method_source.span().start_byte()
                || read_span.end_byte() > method_source.span().end_byte()
                || read_span.start_byte() < declaration_span.end_byte()
                || text(definition.span())? != variable.record.canonical_name
                || text(read.span())? != variable.record.canonical_name
            {
                continue;
            }
            bindings.insert(
                read.local_id(),
                Binding {
                    target: variable.record.id,
                    method: method_id,
                    span: read_span,
                    spelling: content_hash(text(read.span())?.as_bytes()),
                    provenance: variable.record.provenance,
                },
            );
        }
        cancel.check()?;
        Ok(Self {
            source: source.clone(),
            bindings,
        })
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical_site_span: SourceSpan,
        site: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<(SymbolId, SymbolId)>, AdapterError> {
        cancel.check()?;
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "flux.parameter_read.reference"
            || site.syntax_kind != "flux.parameter_read.reference"
            || site.role != OccurrenceRole::Read
            || !matches!(site.target, OccurrenceTarget::Unresolved { .. })
            || !same_source(&site.source, &self.source)
            || site.repository != self.source.repository()
            || site.generation != self.source.generation()
            || site.file != self.source.span().file()
            || fact.span().file() != physical_site_span.file()
        {
            return Ok(None);
        }
        Ok(self
            .bindings
            .get(&fact.local_id())
            .filter(|binding| {
                binding.span == physical_site_span
                    && binding.span == site.source.span()
                    && binding.spelling == site.syntactic_text_hash
                    && binding.provenance == site.provenance
            })
            .map(|binding| (binding.target, binding.method)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lowering::unresolved_occurrence;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{FileId, GenerationId, RepositoryId};
    use rootlight_ir::{
        AnalysisTier, EntityRecord, EntityVisibility, FactEvidence, SymbolIdentityClaim,
    };

    const TEXT: &str = "measure = (value) => consume(v: value)";

    fn span(start: usize, end: usize) -> SourceSpan {
        SourceSpan::new(
            FileId::from_bytes([2; 20]),
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        )
        .unwrap()
    }

    fn source() -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([3; 20]),
            span(0, TEXT.len()),
            content_hash(TEXT.as_bytes()),
            None,
        )
    }

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        range: SourceSpan,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            range,
            usize::try_from(id).unwrap(),
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn witnesses() -> Vec<SyntaxFact> {
        let parameter = TEXT.find("value").unwrap();
        let read = TEXT.rfind("value").unwrap();
        vec![
            fact(
                1,
                Some(7),
                SyntaxFactKind::Declaration,
                span(0, TEXT.len()),
                "flux.function.declaration",
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Occurrence,
                span(0, 7),
                "flux.name.definition",
            ),
            fact(
                3,
                Some(1),
                SyntaxFactKind::Declaration,
                span(parameter, parameter + 5),
                "flux.parameter.declaration",
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Occurrence,
                span(parameter, parameter + 5),
                "flux.name.definition",
            ),
            fact(
                5,
                Some(1),
                SyntaxFactKind::Occurrence,
                span(read, read + 5),
                "flux.parameter_read.reference",
            ),
            fact(
                6,
                Some(1),
                SyntaxFactKind::Signature,
                span(parameter, read + 5),
                "flux.parameter_target.signature",
            ),
            fact(
                7,
                None,
                SyntaxFactKind::Root,
                span(0, TEXT.len()),
                "flux.source_file",
            ),
            fact(
                8,
                Some(7),
                SyntaxFactKind::Signature,
                span(0, TEXT.len()),
                "flux.clean_parse.signature",
            ),
        ]
    }

    fn materialized(
        kind: EntityKind,
        name: &str,
        parent: ContainerRef,
        range: SourceSpan,
        definition: u64,
    ) -> MaterializedEntity {
        let source = source();
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: source.repository(),
            language: "flux".to_owned(),
            kind,
            container: Some(parent),
            container_identity: b"native-method-owner".to_vec(),
            declared_identity: name.to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: content_hash(b"flux-source-control").as_bytes().to_vec(),
        };
        claim.symbol = claim.derived_symbol();
        let record = EntityRecord {
            id: claim.symbol,
            repository: source.repository(),
            generation: source.generation(),
            kind,
            language: "flux".to_owned(),
            tier: AnalysisTier::TierD,
            canonical_name: name.to_owned(),
            display_name: name.to_owned(),
            qualified_name: name.to_owned(),
            container: Some(parent),
            visibility: EntityVisibility::Unknown,
            flags: Vec::new(),
            provenance: FactId::from_bytes([4; 20]),
            evidence: FactEvidence {
                source: Some(SourceRef::new(
                    source.repository(),
                    source.generation(),
                    range,
                    source.content_hash(),
                    None,
                )),
                derivation: Vec::new(),
            },
        };
        MaterializedEntity {
            record,
            identity_claim: claim,
            direct_parent: parent,
            identity_guard: [0; 32],
            definition_local_id: Some(definition),
            signature_evidence: None,
            signature_span: None,
        }
    }

    fn entities() -> HashMap<u64, MaterializedEntity> {
        let facts = witnesses();
        let method = materialized(
            EntityKind::Function,
            "measure",
            ContainerRef::File(source().span().file()),
            facts[0].span(),
            2,
        );
        let variable = materialized(
            EntityKind::Parameter,
            "value",
            ContainerRef::Entity(method.record.id),
            facts[2].span(),
            4,
        );
        HashMap::from([(1, method), (3, variable)])
    }

    fn plan(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        frame: &SourceRef,
    ) -> ReadTargets {
        ReadTargets::new(
            facts,
            entities,
            frame,
            |span| {
                Ok(&TEXT[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()])
            },
            Ok,
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn exact_variable_method_definition_read_and_marker_are_jointly_required() {
        let original = witnesses();
        assert_eq!(plan(&original, &entities(), &source()).bindings.len(), 1);
        for missing in 0..original.len() {
            let mut facts = original.clone();
            facts.remove(missing);
            assert!(
                plan(&facts, &entities(), &source()).bindings.is_empty(),
                "missing={missing}"
            );
        }
        for duplicate in 0..original.len() {
            let mut facts = original.clone();
            let prior = &original[duplicate];
            facts.push(fact(
                99,
                prior.parent(),
                prior.kind(),
                prior.span(),
                prior.syntax_kind().as_str(),
            ));
            assert!(
                plan(&facts, &entities(), &source()).bindings.is_empty(),
                "duplicate={duplicate}"
            );
        }
        for range in [
            span(0, TEXT.rfind("value").unwrap() + 5),
            span(TEXT.find("value").unwrap(), TEXT.len()),
        ] {
            let mut facts = original.clone();
            facts[5] = fact(
                6,
                None,
                SyntaxFactKind::Signature,
                range,
                "flux.parameter_target.signature",
            );
            assert!(plan(&facts, &entities(), &source()).bindings.is_empty());
        }
    }

    #[test]
    fn materialized_kind_owner_definition_and_current_frame_cannot_be_substituted() {
        let facts = witnesses();
        for mutation in 0..7 {
            let mut entities = entities();
            let variable = entities.get_mut(&3).unwrap();
            match mutation {
                0 => variable.record.kind = EntityKind::Field,
                1 => variable.record.language = "other".to_owned(),
                2 => variable.record.container = Some(ContainerRef::File(source().span().file())),
                3 => variable.definition_local_id = Some(2),
                4 => variable.record.generation = GenerationId::from_bytes([9; 20]),
                5 => variable.record.canonical_name = "other".to_owned(),
                _ => {
                    variable.record.evidence.source = Some(SourceRef::new(
                        source().repository(),
                        source().generation(),
                        facts[2].span(),
                        content_hash(b"changed"),
                        None,
                    ));
                }
            }
            assert!(
                plan(&facts, &entities, &source()).bindings.is_empty(),
                "mutation={mutation}"
            );
        }
        let stale = SourceRef::new(
            source().repository(),
            GenerationId::from_bytes([9; 20]),
            source().span(),
            source().content_hash(),
            None,
        );
        assert!(plan(&facts, &entities(), &stale).bindings.is_empty());
    }

    #[test]
    fn read_adoption_rechecks_role_span_hash_provenance_and_cancellation() {
        let facts = witnesses();
        let entities = entities();
        let targets = plan(&facts, &entities, &source());
        let read = &facts[4];
        let reference = SourceRef::new(
            source().repository(),
            source().generation(),
            read.span(),
            source().content_hash(),
            None,
        );
        let site = unresolved_occurrence(
            read,
            OccurrenceRole::Read,
            Some(entities[&1].record.id),
            FactId::from_bytes([4; 20]),
            rootlight_ir::Confidence::new(500).unwrap(),
            reference,
            "value",
        );
        let site = site.unwrap();
        assert_eq!(
            targets
                .resolve(read, read.span(), &site, &Cancellation::new())
                .unwrap(),
            Some((entities[&3].record.id, entities[&1].record.id))
        );
        for mutation in 0..4 {
            let mut changed = site.clone();
            match mutation {
                0 => changed.role = OccurrenceRole::Reference,
                1 => changed.syntactic_text_hash = content_hash(b"other"),
                2 => changed.provenance = FactId::from_bytes([7; 20]),
                _ => changed.generation = GenerationId::from_bytes([9; 20]),
            }
            assert!(
                targets
                    .resolve(read, read.span(), &changed, &Cancellation::new())
                    .unwrap()
                    .is_none()
            );
        }
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            targets.resolve(read, read.span(), &site, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
    #[test]
    fn projected_text_and_physical_identity_joins_remain_separate() {
        let facts = witnesses();
        let mut materialized = entities();
        let raw: Vec<u8> = TEXT.encode_utf16().flat_map(u16::to_le_bytes).collect();
        let physical = |span: SourceSpan| {
            SourceSpan::new(span.file(), span.start_byte() * 2, span.end_byte() * 2)
                .map_err(|_| AdapterError::from(SinkError::SourceMismatch))
        };
        let original = source();
        let frame = SourceRef::new(
            original.repository(),
            original.generation(),
            physical(original.span()).unwrap(),
            content_hash(&raw),
            None,
        );
        for entity in materialized.values_mut() {
            let old = entity.record.evidence.source.as_ref().unwrap();
            entity.record.evidence.source = Some(SourceRef::new(
                frame.repository(),
                frame.generation(),
                physical(old.span()).unwrap(),
                frame.content_hash(),
                None,
            ));
        }
        let targets = ReadTargets::new(
            &facts,
            &materialized,
            &frame,
            |span| {
                Ok(&TEXT[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()])
            },
            physical,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(targets.bindings.len(), 1);
        let read = &facts[4];
        let range = physical(read.span()).unwrap();
        let reference = SourceRef::new(
            frame.repository(),
            frame.generation(),
            range,
            frame.content_hash(),
            None,
        );
        let site = unresolved_occurrence(
            read,
            OccurrenceRole::Read,
            Some(materialized[&1].record.id),
            FactId::from_bytes([4; 20]),
            rootlight_ir::Confidence::new(500).unwrap(),
            reference,
            "value",
        )
        .unwrap();
        assert_eq!(
            targets
                .resolve(read, range, &site, &Cancellation::new())
                .unwrap(),
            Some((materialized[&3].record.id, materialized[&1].record.id))
        );
        assert!(
            targets
                .resolve(read, read.span(), &site, &Cancellation::new())
                .unwrap()
                .is_none()
        );
        assert!(matches!(
            ReadTargets::new(
                &facts,
                &materialized,
                &frame,
                |span| Ok(&TEXT[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()]),
                |_| Err(SinkError::SourceMismatch.into()),
                &Cancellation::new()
            ),
            Err(AdapterError::Sink(SinkError::SourceMismatch))
        ));
    }
}
