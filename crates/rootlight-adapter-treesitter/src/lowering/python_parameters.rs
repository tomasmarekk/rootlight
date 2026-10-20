//! Join native-certified Python body reads to their own written formal entity.
//! Other function bodies and Python environments retain explicit binding gaps.

use std::collections::HashMap;

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
    left.repository() == right.repository()
        && left.generation() == right.generation()
        && left.content_hash() == right.content_hash()
        && left.span().file() == right.span().file()
}

impl<'a> ParameterTargets<'a> {
    /// Index current materialized formals by direct owner; duplicate names refuse adoption.
    /// Fallible allocation and cancellation precede publication of the index.
    pub(super) fn new(
        entities: &'a HashMap<u64, MaterializedEntity>,
        source: &'a SourceRef,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancel.check()?;
        let mut names: HashMap<(SymbolId, &'a str), Option<&'a MaterializedEntity>> =
            HashMap::new();
        names
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for parameter in entities.values() {
            cancel.check()?;
            if parameter.record.language != "python"
                || parameter.record.kind != EntityKind::Parameter
                || parameter.definition_local_id.is_none()
                || !parameter.record.canonical_name.is_ascii()
                || parameter
                    .record
                    .evidence
                    .source
                    .as_ref()
                    .is_none_or(|s| !same_source(s, source))
            {
                continue;
            }
            let ContainerRef::Entity(owner) = parameter.direct_parent else {
                continue;
            };
            names
                .entry((owner, parameter.record.canonical_name.as_str()))
                .and_modify(|entry| *entry = None)
                .or_insert(Some(parameter));
        }
        Ok(Self { names, source })
    }

    /// Adopt only the issuer-certified, current-source same-owner formal binding.
    /// The caller maps decoded native coordinates to physical source bytes.
    /// Cancellation and allocation failure remain ordinary checked errors.
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
        let Some(owner) = owner.filter(|owner| {
            owner.record.language == "python"
                && matches!(owner.record.kind, EntityKind::Function | EntityKind::Method)
        }) else {
            return Ok(None);
        };
        let Some(owner_source) = owner.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "python.parameter_body.reference"
            || occurrence.syntax_kind != "python.parameter_body.reference"
            || occurrence.role != OccurrenceRole::Reference
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.enclosing != Some(owner.record.id)
            || owner.record.repository != self.source.repository()
            || owner.record.generation != self.source.generation()
            || owner.record.provenance != occurrence.provenance
            || occurrence.repository != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.file != self.source.span().file()
            || !same_source(&occurrence.source, self.source)
            || !same_source(owner_source, self.source)
            || occurrence.source.span() != physical_span
            || owner_source.span().start_byte() > physical_span.start_byte()
            || owner_source.span().end_byte() < physical_span.end_byte()
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
        if parameter.record.repository != self.source.repository()
            || parameter.record.generation != self.source.generation()
            || parameter.record.provenance != occurrence.provenance
            || parameter_source.span().start_byte() < owner_source.span().start_byte()
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

    const TEXT: &str = "def owned(value):\n    return value\n";

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
            language: "python".to_owned(),
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
                language: "python".to_owned(),
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
            10,
            15,
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
            SyntaxKindLabel::new("python.parameter_body.reference").unwrap(),
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
            syntax_kind: "python.parameter_body.reference".to_owned(),
            provenance: FactId::from_bytes([4; 20]),
            confidence: rootlight_ir::Confidence::new(850).unwrap(),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        (fact, site)
    }

    #[test]
    fn current_formal_join_refuses_missing_duplicate_and_other_owned_witnesses() {
        let source = full();
        let original = entities();
        let (fact, occurrence) = site(original[&1].record.id);
        let resolve = |records: &HashMap<u64, MaterializedEntity>| {
            ParameterTargets::new(records, &source, &Cancellation::new())
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
        let targets = ParameterTargets::new(&original, &source, &Cancellation::new()).unwrap();
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
        for case in 0..10 {
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
                _ => {
                    records.get_mut(&3).unwrap().record.evidence.source = Some(at(
                        TEXT.rfind("value").unwrap(),
                        TEXT.rfind("value").unwrap() + 5,
                    ))
                }
            }
            let targets = ParameterTargets::new(&records, &source, &Cancellation::new()).unwrap();
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
        let targets = ParameterTargets::new(&original, &source, &Cancellation::new()).unwrap();
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            targets.resolve(
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
            ParameterTargets::new(&original, &source, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
