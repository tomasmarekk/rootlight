//! Ordinary native data-record ancestry adds datatype membership to written fields.
//! Constructor ownership and all existing facts remain unchanged. A native field
//! certificate is mandatory because generic constructor labels also cover GADTs.

use super::*;

pub(super) const ORDINARY_FIELD: &str = "haskell.ordinary_record_field.signature";

pub(super) fn eligible(request: &AnalysisRequest<'_>, parsed: &ParseOutput) -> bool {
    request.language().as_str() == "haskell"
        && request.encoding().as_str() == "utf-8"
        && request.included_ranges().is_empty()
        && request.source().source_ref().span().start_byte() == 0
        && u64::try_from(request.source().bytes().len()).ok()
            == Some(request.source().source_ref().span().end_byte())
        && parsed.report().coverage().status() == CoverageStatus::Complete
}

pub(super) fn field_signature(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Signature
        && matches!(
            fact.syntax_kind().as_str(),
            "haskell.field_signature.signature" | ORDINARY_FIELD
        )
}

#[derive(Default)]
pub(super) struct Plan {
    owners: HashMap<u64, (SymbolId, SymbolId)>,
}

type Markers = HashMap<(u64, SourceSpan), Option<u64>>;

impl Plan {
    /// Certifies only admitted field, constructor and ordinary datatype identities.
    /// The caller admits a complete standalone UTF-8 frame before planning.
    /// Cancellation or failed bounded allocation aborts rather than dropping edges.
    pub(super) fn new(
        facts: &HashMap<u64, &SyntaxFact>,
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut markers = Markers::new();
        for fact in facts.values() {
            cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == ORDINARY_FIELD
                && let Some(parent) = fact.parent()
            {
                markers
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                markers
                    .entry((parent, fact.span()))
                    .and_modify(|proof| *proof = None)
                    .or_insert(Some(fact.local_id()));
            }
        }
        let mut owners = HashMap::new();
        for (local, field) in entities {
            cancellation.check()?;
            let Some(fact) = facts.get(local).copied() else {
                continue;
            };
            let Some(signature) = field.signature_span else {
                continue;
            };
            let Some((datatype_local, constructor_local)) =
                ordinary_owner(fact, signature, facts, &markers)
            else {
                continue;
            };
            let (Some(datatype), Some(constructor)) = (
                entities.get(&datatype_local),
                entities.get(&constructor_local),
            ) else {
                continue;
            };
            if field.record.kind != EntityKind::Field
                || constructor.record.kind != EntityKind::Constructor
                || datatype.record.kind != EntityKind::Enum
                || field.direct_parent != ContainerRef::Entity(constructor.record.id)
                || constructor.direct_parent != ContainerRef::Entity(datatype.record.id)
                || field.record.container != Some(field.direct_parent)
                || constructor.record.container != Some(constructor.direct_parent)
            {
                continue;
            }
            let same_frame = [
                (field, fact),
                (constructor, facts[&constructor_local]),
                (datatype, facts[&datatype_local]),
            ]
            .into_iter()
            .all(|(entity, declaration)| {
                entity.record.language == "haskell"
                    && entity.definition_local_id.is_some()
                    && entity.record.provenance == field.record.provenance
                    && entity
                        .record
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|written| {
                            written.repository() == source.repository()
                                && written.generation() == source.generation()
                                && written.content_hash() == source.content_hash()
                                && written.span() == declaration.span()
                                && span_contains(source.span(), written.span())
                        })
            });
            if !same_frame {
                continue;
            }
            owners
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            owners.insert(*local, (datatype.record.id, constructor.record.id));
        }
        Ok(Self { owners })
    }

    /// Adds an ancestor relation with the original written field as its witness.
    /// Entity records are borrowed immutably, including immediate containers.
    pub(super) fn relation(
        &self,
        local: u64,
        field: &MaterializedEntity,
        cancellation: &Cancellation,
    ) -> Result<Option<RelationRecord>, AdapterError> {
        let Some(&(datatype, constructor)) = self.owners.get(&local) else {
            return Ok(None);
        };
        cancellation.check()?;
        let Some(source) = field.record.evidence.source.as_ref() else {
            return Ok(None);
        };
        let mut evidence = direct_evidence(source.clone());
        evidence
            .derivation
            .try_reserve_exact(3)
            .map_err(|_| SinkError::AllocationFailed)?;
        evidence.derivation.extend([
            FactRef::Entity(datatype),
            FactRef::Entity(constructor),
            FactRef::Entity(field.record.id),
        ]);
        evidence.derivation.sort_unstable();
        evidence.derivation.dedup();
        let mut relation = RelationRecord {
            id: FactId::from_bytes([0; 20]),
            repository: source.repository(),
            generation: source.generation(),
            subject: RelationEndpoint::Entity(datatype),
            predicate: RelationPredicate::Contains,
            object: RelationEndpoint::Entity(field.record.id),
            confidence: confidence(CONTAINMENT_CONFIDENCE)?,
            evidence_kind: EvidenceKind::Derived,
            provenance: field.record.provenance,
            evidence,
        };
        relation.id = derive_relation_record_id(&relation)
            .map_err(|_| provider_failure("treesitter-relation-identity"))?;
        cancellation.check()?;
        Ok(Some(relation))
    }
}

fn ordinary_owner(
    field: &SyntaxFact,
    signature: SourceSpan,
    facts: &HashMap<u64, &SyntaxFact>,
    markers: &Markers,
) -> Option<(u64, u64)> {
    if field.kind() != SyntaxFactKind::Declaration
        || field.syntax_kind().as_str() != "haskell.field.declaration"
    {
        return None;
    }
    let constructor_scope = *facts.get(&field.parent()?)?;
    let constructor = *facts.get(&constructor_scope.parent()?)?;
    let datatype_scope = *facts.get(&constructor.parent()?)?;
    let datatype = *facts.get(&datatype_scope.parent()?)?;
    if constructor_scope.kind() != SyntaxFactKind::Scope
        || constructor_scope.syntax_kind().as_str() != "haskell.constructor.scope"
        || constructor.kind() != SyntaxFactKind::Declaration
        || constructor.syntax_kind().as_str() != "haskell.constructor.declaration"
        || datatype_scope.kind() != SyntaxFactKind::Scope
        || datatype_scope.syntax_kind().as_str() != "haskell.data_type.scope"
        || datatype.kind() != SyntaxFactKind::Declaration
        || datatype.syntax_kind().as_str() != "haskell.data_type.declaration"
        || constructor_scope.span() != constructor.span()
        || datatype_scope.span() != datatype.span()
        || !span_contains(datatype.span(), constructor.span())
        || !span_contains(constructor.span(), signature)
        || !span_contains(signature, field.span())
        || !matches!(
            markers.get(&(constructor_scope.local_id(), signature)),
            Some(Some(_))
        )
    {
        return None;
    }
    Some((datatype.local_id(), constructor.local_id()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{GenerationId, derive_repository};
    use rootlight_ir::BuildContextIdentity;

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        span: (u64, u64),
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(FileId::from_bytes([1; 20]), span.0, span.1).unwrap(),
            usize::try_from(id).unwrap(),
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn chain() -> Vec<SyntaxFact> {
        vec![
            fact(
                1,
                None,
                SyntaxFactKind::Declaration,
                (0, 100),
                "haskell.data_type.declaration",
            ),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Scope,
                (0, 100),
                "haskell.data_type.scope",
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Declaration,
                (20, 90),
                "haskell.constructor.declaration",
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Scope,
                (20, 90),
                "haskell.constructor.scope",
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Declaration,
                (40, 45),
                "haskell.field.declaration",
            ),
        ]
    }

    #[test]
    fn exact_marker_and_four_native_parent_links_are_mandatory() {
        let mut chain = chain();
        let signature = SourceSpan::new(FileId::from_bytes([1; 20]), 40, 60).unwrap();
        let markers = HashMap::from([((4, signature), Some(9))]);
        let facts = chain.iter().map(|fact| (fact.local_id(), fact)).collect();
        assert_eq!(
            ordinary_owner(&chain[4], signature, &facts, &markers),
            Some((1, 3))
        );
        assert_eq!(
            ordinary_owner(&chain[4], signature, &facts, &HashMap::new()),
            None
        );
        assert_eq!(
            ordinary_owner(
                &chain[4],
                signature,
                &facts,
                &HashMap::from([((4, signature), None)])
            ),
            None
        );
        assert_eq!(
            ordinary_owner(
                &chain[4],
                signature,
                &facts,
                &HashMap::from([((99, signature), Some(9))])
            ),
            None
        );
        chain[1] = fact(
            2,
            Some(1),
            SyntaxFactKind::Scope,
            (0, 100),
            "haskell.newtype.scope",
        );
        let facts = chain.iter().map(|fact| (fact.local_id(), fact)).collect();
        assert_eq!(ordinary_owner(&chain[4], signature, &facts, &markers), None);
        chain[1] = fact(
            2,
            Some(1),
            SyntaxFactKind::Scope,
            (0, 100),
            "haskell.data_type.scope",
        );
        chain[3] = fact(
            4,
            Some(99),
            SyntaxFactKind::Scope,
            (20, 90),
            "haskell.constructor.scope",
        );
        let facts = chain.iter().map(|fact| (fact.local_id(), fact)).collect();
        assert_eq!(ordinary_owner(&chain[4], signature, &facts, &markers), None);
    }

    #[test]
    fn cross_file_or_partition_spans_do_not_supply_ancestry() {
        let mut chain = chain();
        let signature = SourceSpan::new(FileId::from_bytes([1; 20]), 40, 60).unwrap();
        let markers = HashMap::from([((4, signature), Some(9))]);
        chain[0] = fact(
            1,
            None,
            SyntaxFactKind::Declaration,
            (100, 200),
            "haskell.data_type.declaration",
        );
        let facts = chain.iter().map(|fact| (fact.local_id(), fact)).collect();
        assert_eq!(ordinary_owner(&chain[4], signature, &facts, &markers), None);
        chain[0] = fact(
            1,
            None,
            SyntaxFactKind::Declaration,
            (0, 100),
            "haskell.data_type.declaration",
        );
        let facts = chain.iter().map(|fact| (fact.local_id(), fact)).collect();
        let cross_file = SourceSpan::new(FileId::from_bytes([2; 20]), 40, 60).unwrap();
        let cross_markers = HashMap::from([((4, cross_file), Some(9))]);
        assert_eq!(
            ordinary_owner(&chain[4], cross_file, &facts, &cross_markers),
            None
        );
    }

    #[test]
    fn cancelled_planning_fails_before_allocating_even_with_empty_input() {
        let repository = derive_repository(b"haskell-record-field-cancel").id();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([1; 20]),
            SourceSpan::new(FileId::from_bytes([1; 20]), 0, 1).unwrap(),
            content_hash(b"x"),
            None,
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(Plan::new(&HashMap::new(), &HashMap::new(), &source, &cancellation).is_err());
    }

    fn analysis_limits(ir: IrLimits) -> AnalysisLimits {
        let stream = rootlight_adapter_sdk::StreamLimits::new(
            128,
            16384,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            4 * 1024 * 1024,
            rootlight_adapter_sdk::BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap(),
        )
        .unwrap();
        AnalysisLimits::new(
            1024 * 1024,
            16384,
            128,
            32,
            16 * 1024 * 1024,
            stream.clone(),
            stream,
            ir,
        )
        .unwrap()
    }

    #[test]
    fn native_field_upper_bound_reserves_relations_total_records_and_all_derivations() {
        use rootlight_adapter_sdk::{GenerationBoundSnapshot, LanguageId};
        use rootlight_vfs::{RelativePath, RepositoryRoot};
        let text = "data Record = Record { first, second, third :: Int }\n";
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("Record.hs"), text).unwrap();
        let repository = derive_repository(b"haskell-record-field-capacity").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(
                &RelativePath::parse(std::path::Path::new("Record.hs")).unwrap(),
                1024 * 1024,
            )
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([9; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let producer =
            ProducerIdentity::new("haskell-record-capacity", "1.0", content_hash(b"capacity"))
                .unwrap();
        let provider = crate::TreeSitterProvider::new(
            crate::RuntimeConfig::new(
                1024 * 1024,
                16384,
                128,
                32,
                64,
                1,
                16 * 1024 * 1024,
                crate::ParserSettings::new(4096).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(provider),
            producer.clone(),
            LanguageId::new("haskell").unwrap(),
            "native-record-capacity",
            content_hash(b"capacity-binary"),
        )
        .unwrap();
        let parse_limits = analysis_limits(IrLimits::default());
        let request = AnalysisRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("haskell").unwrap(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"capacity-context")),
            &parse_limits,
        )
        .unwrap()
        .with_generated_status(false);
        let cancellation = Cancellation::new();
        let parsed = analyzer.parse_request(&request, &cancellation).unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        let partial_request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("haskell").unwrap(),
            rootlight_adapter_sdk::EncodingId::utf8(),
            vec![rootlight_adapter_sdk::IncludedRange::new(
                SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len() - 1).unwrap())
                    .unwrap(),
                LanguageId::new("haskell").unwrap(),
            )],
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"capacity-context")),
            &parse_limits,
        )
        .unwrap();
        assert!(
            !eligible(&partial_request, &parsed),
            "separately included partitions have no standalone certificate"
        );
        let fields = parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "haskell.field.declaration")
            .count();
        assert_eq!(fields, 3);
        let base_relations = parsed
            .facts()
            .iter()
            .filter(|fact| structural_entity_kind(fact).is_some())
            .count()
            + parsed
                .facts()
                .iter()
                .filter(|fact| {
                    occurrence_role(fact).is_some() && source_reference_gap(fact).is_some()
                })
                .count();
        let unbounded = preflight_lowering_limits(
            &request,
            &parsed,
            &producer,
            "native-record-capacity",
            &[],
            &cancellation,
        )
        .unwrap();
        assert_eq!(unbounded.nested_items, 2 + 3 * fields);
        for boundary in 0..4 {
            let mut ir = IrLimits::default();
            let (resource, expected) = match boundary {
                0 => {
                    ir.max_relations = base_relations + fields - 1;
                    (ResourceKind::Records, base_relations + fields)
                }
                1 => {
                    ir.max_total_records = unbounded.records - 1;
                    (ResourceKind::Records, unbounded.records)
                }
                2 => {
                    ir.max_total_nested_items = 2 + 3 * fields - 1;
                    (ResourceKind::NestedItems, 2 + 3 * fields)
                }
                _ => {
                    ir.max_nested_items_per_record = 2;
                    (ResourceKind::NestedItems, 3)
                }
            };
            let limits = analysis_limits(ir);
            let request = AnalysisRequest::new(
                GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
                LanguageId::new("haskell").unwrap(),
                AnalysisTier::TierD,
                BuildContextIdentity::new(content_hash(b"capacity-context")),
                &limits,
            )
            .unwrap()
            .with_generated_status(false);
            assert!(
                matches!(preflight_lowering_limits(&request,&parsed,&producer,"native-record-capacity",&[],&cancellation),
                Err(AdapterError::Sink(SinkError::StreamLimit{resource:actual,observed,limit})) if actual==resource && observed==expected && limit==expected-1)
            );
        }
    }
}
