//! Join a retained native C formal-read certificate to its exact current owner.
//! Required full-function metadata is not part of symbol identity. Missing,
//! duplicate or foreign declaration/definition witnesses leave an explicit gap.

use std::collections::{HashMap, hash_map::Entry};

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceRole, OccurrenceTarget, SourceRef,
    SourceSpan,
};

use super::MaterializedEntity;

pub(super) struct ParameterTargets<'a> {
    names: HashMap<(SymbolId, &'a str), Option<&'a MaterializedEntity>>,
    source: &'a SourceRef,
}

fn same_source(left: &SourceRef, right: &SourceRef) -> bool {
    let mut selected = right.clone();
    selected = super::source_for_span(&selected, left.span());
    *left == selected
}

/// Reserve a possible RefersTo only for a retained, source-contained certificate.
/// Unknown/recovered native output is excluded by the caller on both paths.
pub(super) fn certified_reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "c.formal_value.reference"
}

impl<'a> ParameterTargets<'a> {
    /// Index exact declaration/Definition pairs after current source projection.
    /// All allocations and native-fact joins remain cancellable and fallible.
    pub(super) fn new<'text>(
        facts: &[SyntaxFact],
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut by_id = HashMap::new();
        let mut contexts = HashMap::<SourceSpan, usize>::new();
        let mut owners = HashMap::<SymbolId, Option<(u64, &MaterializedEntity)>>::new();
        owners
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, entity) in entities {
            cancel.check()?;
            owners
                .entry(entity.record.id)
                .and_modify(|entry| *entry = None)
                .or_insert(Some((*local, entity)));
        }
        let mut roots = 0_usize;
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            by_id
                .entry(fact.local_id())
                .and_modify(|entry| *entry = None)
                .or_insert(Some(fact));
            if fact.kind() == SyntaxFactKind::Root {
                let prefix = SourceSpan::new(fact.span().file(), 0, fact.span().start_byte())
                    .map_err(|_| SinkError::InvalidIr)?;
                let physical_root = physical(fact.span())?;
                if fact.syntax_kind().as_str() != "c.file.root"
                    || fact.parent().is_some()
                    || source.span().start_byte() != 0
                    || physical_root.file() != source.span().file()
                    || physical_root.end_byte() != source.span().end_byte()
                    || !matches!(text(prefix)?, "" | "\u{feff}")
                {
                    return Ok(Self {
                        names: HashMap::new(),
                        source,
                    });
                }
                roots = roots.saturating_add(1);
            }
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == "c.formal_binding_context.signature"
            {
                contexts
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let span = physical(fact.span())?;
                contexts
                    .entry(span)
                    .and_modify(|count| *count = 2)
                    .or_insert(1);
            }
        }
        let mut names = HashMap::new();
        if roots != 1 {
            return Ok(Self { names, source });
        }
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (local, parameter) in entities {
            cancel.check()?;
            let ContainerRef::Entity(owner_id) = parameter.direct_parent else {
                continue;
            };
            if parameter.record.language != "c" || parameter.record.kind != EntityKind::Parameter {
                continue;
            }
            let key = (owner_id, parameter.record.canonical_name.as_str());
            // Count every same-owner declaration before validation, including a
            // malformed competitor; it must not disappear from ambiguity checks.
            match names.entry(key) {
                Entry::Occupied(mut entry) => {
                    entry.insert(None);
                    continue;
                }
                Entry::Vacant(entry) => {
                    entry.insert(None);
                }
            }
            let Some((owner_local, owner)) = owners.get(&owner_id).copied().flatten() else {
                continue;
            };
            let Some(owner_source) = owner.record.evidence.source.as_ref() else {
                continue;
            };
            let Some(parameter_source) = parameter.record.evidence.source.as_ref() else {
                continue;
            };
            if owner.record.language != "c"
                || owner.record.kind != EntityKind::Function
                || owner.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || parameter.record.repository != source.repository()
                || parameter.record.generation != source.generation()
                || owner.record.provenance != parameter.record.provenance
                || parameter.record.container != Some(parameter.direct_parent)
                || !same_source(owner_source, source)
                || !same_source(parameter_source, source)
                || contexts.get(&owner_source.span()) != Some(&1)
                || !super::span_contains(owner_source.span(), parameter_source.span())
            {
                continue;
            }
            let Some(declaration) = by_id.get(local).copied().flatten().filter(|fact| {
                fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "c.parameter.declaration"
            }) else {
                continue;
            };
            let Some(owner_declaration) =
                by_id.get(&owner_local).copied().flatten().filter(|fact| {
                    fact.kind() == SyntaxFactKind::Declaration
                        && fact.syntax_kind().as_str() == "c.function.declaration"
                })
            else {
                continue;
            };
            let Some(owner_definition) = owner
                .definition_local_id
                .and_then(|id| by_id.get(&id).copied().flatten())
            else {
                continue;
            };
            let Some(definition) = parameter
                .definition_local_id
                .and_then(|id| by_id.get(&id).copied().flatten())
            else {
                continue;
            };
            if owner_definition.kind() != SyntaxFactKind::Occurrence
                || owner_definition.syntax_kind().as_str() != "c.identifier.definition"
                || owner_definition.parent() != Some(owner_local)
                || physical(owner_declaration.span())? != owner_source.span()
                || !super::span_contains(owner_source.span(), physical(owner_definition.span())?)
                || text(owner_definition.span())? != owner.record.canonical_name
                || definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != "c.identifier.definition"
                || definition.parent() != Some(*local)
                || physical(declaration.span())? != parameter_source.span()
                || !super::span_contains(parameter_source.span(), physical(definition.span())?)
                || text(definition.span())? != parameter.record.canonical_name
                || !parameter.record.canonical_name.is_ascii()
            {
                continue;
            }
            names.insert(key, Some(parameter));
        }
        Ok(Self { names, source })
    }

    /// Resolve only the native certificate against a unique same-owner formal.
    /// Physical spans come from the existing SourceCoordinates projection.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical_span: SourceSpan,
        occurrence: &OccurrenceRecord,
        owner: Option<&MaterializedEntity>,
        text: &str,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        let Some(owner) = owner else { return Ok(None) };
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if !certified_reference(fact)
            || occurrence.syntax_kind != "c.formal_value.reference"
            || occurrence.role != OccurrenceRole::Reference
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.enclosing != Some(owner.record.id)
            || owner.record.language != "c"
            || owner.record.kind != EntityKind::Function
            || owner.record.repository != self.source.repository()
            || owner.record.generation != self.source.generation()
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || occurrence.provenance != owner.record.provenance
            || !same_source(&occurrence.source, self.source)
            || !same_source(owner_source, self.source)
            || occurrence.source.span() != physical_span
            || !super::span_contains(owner_source.span(), physical_span)
            || occurrence.syntactic_text_hash != rootlight_ids::content_hash(text.as_bytes())
        {
            return Ok(None);
        }
        let Some(parameter) = self.names.get(&(owner.record.id, text)).copied().flatten() else {
            return Ok(None);
        };
        let Some(parameter_source) = parameter.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if parameter.record.provenance != occurrence.provenance
            || parameter_source.span().end_byte() > physical_span.start_byte()
        {
            return Ok(None);
        }
        Ok(Some(parameter.record.id))
    }
}

#[cfg(test)]
mod qualification_tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId, content_hash};
    use rootlight_ir::{
        AnalysisTier, EntityRecord, EntityVisibility, FactEvidence, SourceSpan, SymbolIdentityClaim,
    };

    const TEXT: &str = "void owned(int value) { external(value); }\n";

    fn full() -> SourceRef {
        SourceRef::new(
            RepositoryId::from_bytes([1; 16]),
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(FileId::from_bytes([3; 20]), 0, TEXT.len() as u64).unwrap(),
            content_hash(TEXT.as_bytes()),
            None,
        )
    }

    fn at(start: usize, end: usize) -> SourceRef {
        let source = full();
        SourceRef::new(
            source.repository(),
            source.generation(),
            SourceSpan::new(source.span().file(), start as u64, end as u64).unwrap(),
            source.content_hash(),
            None,
        )
    }

    fn member(
        kind: EntityKind,
        name: &str,
        parent: ContainerRef,
        start: usize,
        end: usize,
        definition: u64,
    ) -> MaterializedEntity {
        let source = full();
        let mut claim = SymbolIdentityClaim {
            symbol: SymbolId::from_bytes([0; 20]),
            repository: source.repository(),
            language: "c".to_owned(),
            kind,
            container: Some(parent),
            container_identity: format!("{parent:?}").into_bytes(),
            declared_identity: name.to_owned(),
            signature_discriminator: Vec::new(),
            build_context_discriminator: Vec::new(),
        };
        claim.symbol = claim.derived_symbol();
        MaterializedEntity {
            record: EntityRecord {
                id: claim.symbol,
                repository: source.repository(),
                generation: source.generation(),
                kind,
                language: "c".to_owned(),
                tier: AnalysisTier::TierD,
                canonical_name: name.to_owned(),
                display_name: name.to_owned(),
                qualified_name: name.to_owned(),
                container: Some(parent),
                visibility: EntityVisibility::Unknown,
                flags: Vec::new(),
                provenance: FactId::from_bytes([4; 20]),
                evidence: FactEvidence {
                    source: Some(at(start, end)),
                    derivation: Vec::new(),
                },
            },
            identity_claim: claim,
            direct_parent: parent,
            identity_guard: [0; 32],
            definition_local_id: Some(definition),
            signature_evidence: None,
            signature_span: None,
        }
    }

    fn entities() -> HashMap<u64, MaterializedEntity> {
        let owner = member(
            EntityKind::Function,
            "owned",
            ContainerRef::File(full().span().file()),
            0,
            TEXT.len(),
            2,
        );
        let parameter = member(
            EntityKind::Parameter,
            "value",
            ContainerRef::Entity(owner.record.id),
            TEXT.find("int value").unwrap(),
            TEXT.find("int value").unwrap() + "int value".len(),
            4,
        );
        HashMap::from([(1, owner), (3, parameter)])
    }

    fn site(owner: SymbolId) -> (SyntaxFact, OccurrenceRecord) {
        let start = TEXT.rfind("value").unwrap();
        let source = at(start, start + 5);
        let fact = SyntaxFact::new(
            5,
            Some(1),
            SyntaxFactKind::Occurrence,
            source.span(),
            2,
            SyntaxKindLabel::new("c.formal_value.reference").unwrap(),
        );
        let site = OccurrenceRecord {
            id: FactId::from_bytes([5; 20]),
            repository: source.repository(),
            generation: source.generation(),
            file: source.span().file(),
            source: source.clone(),
            role: OccurrenceRole::Reference,
            enclosing: Some(owner),
            target: OccurrenceTarget::Unresolved {
                text_hash: content_hash(b"value"),
            },
            syntactic_text_hash: content_hash(b"value"),
            syntax_kind: "c.formal_value.reference".to_owned(),
            provenance: FactId::from_bytes([4; 20]),
            confidence: rootlight_ir::Confidence::new(850).unwrap(),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        (fact, site)
    }

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        start: usize,
        end: usize,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            at(start, end).span(),
            1,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn facts() -> Vec<SyntaxFact> {
        let formal = TEXT.find("value").unwrap();
        vec![
            fact(0, None, SyntaxFactKind::Root, 0, TEXT.len(), "c.file.root"),
            fact(
                1,
                Some(0),
                SyntaxFactKind::Declaration,
                0,
                TEXT.len(),
                "c.function.declaration",
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Occurrence,
                5,
                10,
                "c.identifier.definition",
            ),
            fact(
                3,
                Some(1),
                SyntaxFactKind::Declaration,
                TEXT.find("int value").unwrap(),
                formal + 5,
                "c.parameter.declaration",
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Occurrence,
                formal,
                formal + 5,
                "c.identifier.definition",
            ),
            fact(
                6,
                Some(1),
                SyntaxFactKind::Signature,
                0,
                TEXT.len(),
                "c.formal_binding_context.signature",
            ),
        ]
    }

    fn targets<'a>(
        records: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        cancel: &Cancellation,
    ) -> Result<ParameterTargets<'a>, AdapterError> {
        ParameterTargets::new(
            &facts(),
            records,
            source,
            |span| {
                let start = usize::try_from(span.start_byte()).unwrap();
                let end = usize::try_from(span.end_byte()).unwrap();
                Ok(&TEXT[start..end])
            },
            Ok,
            cancel,
        )
    }

    #[test]
    fn current_native_formal_join_requires_root_context_and_exact_definition_facts() {
        let records = entities();
        let source = full();
        let (site, occurrence) = site(records[&1].record.id);
        for case in 0..7 {
            let mut input = facts();
            match case {
                0 => input.retain(|f| f.local_id() != 0),
                1 => input.retain(|f| f.local_id() != 6),
                2 => input.push(input.last().unwrap().clone()),
                3 => input.retain(|f| f.local_id() != 4),
                4 => input.push(input[4].clone()),
                5 => {
                    input[4] = fact(
                        4,
                        Some(1),
                        SyntaxFactKind::Occurrence,
                        TEXT.find("value").unwrap(),
                        TEXT.find("value").unwrap() + 5,
                        "c.identifier.definition",
                    )
                }
                _ => {
                    input[4] = fact(
                        4,
                        Some(3),
                        SyntaxFactKind::Occurrence,
                        0,
                        4,
                        "c.identifier.definition",
                    )
                }
            }
            let targets = ParameterTargets::new(
                &input,
                &records,
                &source,
                |span| {
                    let start = usize::try_from(span.start_byte()).unwrap();
                    let end = usize::try_from(span.end_byte()).unwrap();
                    Ok(&TEXT[start..end])
                },
                Ok,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(
                targets
                    .resolve(
                        &site,
                        site.span(),
                        &occurrence,
                        records.get(&1),
                        "value",
                        &Cancellation::new()
                    )
                    .unwrap(),
                None,
                "case={case}"
            );
        }
    }

    #[test]
    fn current_formal_join_refuses_missing_duplicate_and_other_owned_witnesses() {
        let source = full();
        let original = entities();
        let (fact, occurrence) = site(original[&1].record.id);
        let resolve = |records: &HashMap<u64, MaterializedEntity>| {
            targets(records, &source, &Cancellation::new())
                .unwrap()
                .resolve(
                    &fact,
                    fact.span(),
                    &occurrence,
                    records.get(&1),
                    "value",
                    &Cancellation::new(),
                )
                .unwrap()
        };
        assert_eq!(resolve(&original), Some(original[&3].record.id));
        let targets = targets(&original, &source, &Cancellation::new()).unwrap();
        assert_eq!(
            targets
                .resolve(
                    &fact,
                    at(0, 1).span(),
                    &occurrence,
                    original.get(&1),
                    "value",
                    &Cancellation::new()
                )
                .unwrap(),
            None,
            "a different physical span must not adopt the formal"
        );
        for case in 0..7 {
            let mut records = original.clone();
            match case {
                0 => {
                    records.remove(&1);
                }
                1 => {
                    records.remove(&3);
                }
                2 => records.get_mut(&3).unwrap().definition_local_id = None,
                3 => {
                    records.insert(6, records[&3].clone());
                }
                4 => {
                    records.get_mut(&3).unwrap().direct_parent =
                        ContainerRef::Entity(SymbolId::from_bytes([9; 20]))
                }
                5 => records.get_mut(&3).unwrap().record.canonical_name = "other".to_owned(),
                _ => records.get_mut(&3).unwrap().record.evidence.source = None,
            }
            assert_eq!(resolve(&records), None, "case={case}");
        }
    }

    #[test]
    fn formal_join_refuses_foreign_source_owner_kind_role_and_spelling() {
        let source = full();
        let original = entities();
        let (fact, original_site) = site(original[&1].record.id);
        for case in 0..17 {
            let mut records = original.clone();
            let mut occurrence = original_site.clone();
            match case {
                0 => occurrence.repository = RepositoryId::from_bytes([8; 16]),
                1 => occurrence.generation = GenerationId::from_bytes([8; 20]),
                2 => occurrence.file = FileId::from_bytes([8; 20]),
                3 => {
                    occurrence.source = SourceRef::new(
                        source.repository(),
                        source.generation(),
                        occurrence.source.span(),
                        content_hash(b"foreign bytes"),
                        None,
                    )
                }
                4 => records.get_mut(&3).unwrap().record.provenance = FactId::from_bytes([8; 20]),
                5 => records.get_mut(&1).unwrap().record.kind = EntityKind::Class,
                6 => occurrence.role = OccurrenceRole::CallSite,
                7 => occurrence.syntactic_text_hash = content_hash(b"other"),
                8 => occurrence.enclosing = Some(SymbolId::from_bytes([8; 20])),
                9 => {
                    records.get_mut(&3).unwrap().record.evidence.source = Some(at(
                        TEXT.rfind("value").unwrap(),
                        TEXT.rfind("value").unwrap() + 5,
                    ))
                }
                10 => {
                    records.get_mut(&3).unwrap().record.repository =
                        RepositoryId::from_bytes([8; 16])
                }
                11 => {
                    records.get_mut(&3).unwrap().record.generation =
                        GenerationId::from_bytes([8; 20])
                }
                12 => records.get_mut(&3).unwrap().record.language = "other".to_owned(),
                13 => records.get_mut(&3).unwrap().record.kind = EntityKind::Field,
                14 => {
                    let span = records[&3].record.evidence.source.as_ref().unwrap().span();
                    records.get_mut(&3).unwrap().record.evidence.source = Some(SourceRef::new(
                        source.repository(),
                        source.generation(),
                        SourceSpan::new(
                            FileId::from_bytes([8; 20]),
                            span.start_byte(),
                            span.end_byte(),
                        )
                        .unwrap(),
                        source.content_hash(),
                        None,
                    ));
                }
                15 => {
                    let span = records[&3].record.evidence.source.as_ref().unwrap().span();
                    records.get_mut(&3).unwrap().record.evidence.source = Some(SourceRef::new(
                        source.repository(),
                        source.generation(),
                        span,
                        content_hash(b"foreign parameter bytes"),
                        None,
                    ));
                }
                _ => records.get_mut(&1).unwrap().record.provenance = FactId::from_bytes([8; 20]),
            }
            let targets = targets(&records, &source, &Cancellation::new()).unwrap();
            assert_eq!(
                targets
                    .resolve(
                        &fact,
                        fact.span(),
                        &occurrence,
                        records.get(&1),
                        "value",
                        &Cancellation::new()
                    )
                    .unwrap(),
                None,
                "case={case}"
            );
        }
        let prepared_targets = targets(&original, &source, &Cancellation::new()).unwrap();
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            prepared_targets.resolve(
                &fact,
                fact.span(),
                &original_site,
                original.get(&1),
                "value",
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            targets(&original, &source, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
    #[test]
    fn canonical_protocol_witnesses_refuse_native_node_name_aliases() {
        let records = entities();
        let source = full();
        let (site, occurrence) = site(records[&1].record.id);
        for case in 0..3 {
            let mut input = facts();
            match case {
                1 => {
                    input[0] = fact(
                        0,
                        None,
                        SyntaxFactKind::Root,
                        0,
                        TEXT.len(),
                        "c.translation_unit.root",
                    )
                }
                2 => {
                    input[1] = fact(
                        1,
                        Some(0),
                        SyntaxFactKind::Declaration,
                        0,
                        TEXT.len(),
                        "c.function_definition.declaration",
                    )
                }
                _ => {}
            }
            let prepared = ParameterTargets::new(
                &input,
                &records,
                &source,
                |span| {
                    let start = usize::try_from(span.start_byte()).unwrap();
                    let end = usize::try_from(span.end_byte()).unwrap();
                    Ok(&TEXT[start..end])
                },
                Ok,
                &Cancellation::new(),
            )
            .unwrap();
            let actual = prepared
                .resolve(
                    &site,
                    site.span(),
                    &occurrence,
                    records.get(&1),
                    "value",
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(
                actual,
                if case == 0 {
                    Some(records[&3].record.id)
                } else {
                    None
                },
                "case={case}"
            );
        }
    }
}
