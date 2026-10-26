//! Fresh consumer parsing must reject plausible forged producer certificates.
//! Synthetic structural joins are controlled separately from real source bytes;
//! public analyzer tests cover the actual producer-to-consumer fact geometry.

use super::*;
use crate::lowering::*;
use rootlight_adapter_sdk::SyntaxKindLabel;
use rootlight_ids::{FactId, FileId, GenerationId, RepositoryId};

struct Fixture {
    text: Vec<u8>,
    source: SourceRef,
    facts: Vec<SyntaxFact>,
    entities: HashMap<u64, MaterializedEntity>,
    site: OccurrenceRecord,
}

fn fact(
    source: &SourceRef,
    id: u64,
    parent: Option<u64>,
    kind: SyntaxFactKind,
    range: (usize, usize),
    depth: usize,
    label: &str,
) -> SyntaxFact {
    SyntaxFact::new(
        id,
        parent,
        kind,
        SourceSpan::new(
            source.span().file(),
            u64::try_from(range.0).unwrap(),
            u64::try_from(range.1).unwrap(),
        )
        .unwrap(),
        depth,
        SyntaxKindLabel::new(label).unwrap(),
    )
}

impl Fixture {
    fn new(text: &[u8]) -> Self {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        let provenance = FactId::from_bytes([4; 20]);
        let source = SourceRef::new(
            repository,
            generation,
            SourceSpan::new(file, 0, u64::try_from(text.len()).unwrap()).unwrap(),
            content_hash(text),
            None,
        );
        let assignment_end = text.iter().position(|byte| *byte == b'\n').unwrap();
        let start = text
            .windows(b"$OWNED".len())
            .rposition(|part| part == b"$OWNED")
            .unwrap()
            + 1;
        let facts = vec![
            fact(
                &source,
                1,
                None,
                SyntaxFactKind::Root,
                (0, text.len()),
                0,
                "bash.file.root",
            ),
            fact(
                &source,
                2,
                Some(1),
                SyntaxFactKind::Module,
                (0, text.len()),
                1,
                "bash.file.module",
            ),
            fact(
                &source,
                3,
                Some(2),
                SyntaxFactKind::Scope,
                (0, text.len()),
                2,
                "bash.file.scope",
            ),
            fact(
                &source,
                4,
                Some(3),
                SyntaxFactKind::Declaration,
                (0, assignment_end),
                3,
                "bash.variable.declaration",
            ),
            fact(
                &source,
                5,
                Some(4),
                SyntaxFactKind::Occurrence,
                (0, 5),
                4,
                "bash.variable.definition",
            ),
            fact(
                &source,
                6,
                Some(3),
                SyntaxFactKind::Occurrence,
                (start, start + 5),
                3,
                PROVEN,
            ),
        ];
        let mut entities = HashMap::new();
        for (local, definition, kind, name) in [
            (2, None, EntityKind::Module, "module"),
            (4, Some(5), EntityKind::Variable, "OWNED"),
        ] {
            let parent = if kind == EntityKind::Module {
                ContainerRef::File(file)
            } else {
                ContainerRef::Entity(
                    entities
                        .get(&2)
                        .map(|entity: &MaterializedEntity| entity.record.id)
                        .unwrap(),
                )
            };
            let mut claim = SymbolIdentityClaim {
                symbol: SymbolId::from_bytes([0; 20]),
                repository,
                language: "bash".to_owned(),
                kind,
                container: Some(parent),
                container_identity: b"module-reference-container".to_vec(),
                declared_identity: name.to_owned(),
                signature_discriminator: Vec::new(),
                build_context_discriminator: Vec::new(),
            };
            claim.symbol = claim.derived_symbol();
            let record = EntityRecord {
                id: claim.symbol,
                repository,
                generation,
                kind,
                language: "bash".to_owned(),
                tier: AnalysisTier::TierD,
                canonical_name: name.to_owned(),
                display_name: name.to_owned(),
                qualified_name: name.to_owned(),
                container: Some(parent),
                visibility: EntityVisibility::Unknown,
                flags: Vec::new(),
                provenance,
                evidence: FactEvidence {
                    source: Some(source_for_span(
                        &source,
                        facts
                            .iter()
                            .find(|fact| fact.local_id() == local)
                            .unwrap()
                            .span(),
                    )),
                    derivation: Vec::new(),
                },
            };
            entities.insert(
                local,
                MaterializedEntity {
                    record,
                    identity_claim: claim,
                    direct_parent: parent,
                    identity_guard: [0; 32],
                    definition_local_id: definition,
                    signature_evidence: None,
                    signature_span: None,
                },
            );
        }
        let occurrence_source = source_for_span(&source, facts[5].span());
        let mut site = OccurrenceRecord {
            id: FactId::from_bytes([0; 20]),
            repository,
            generation,
            file,
            source: occurrence_source.clone(),
            role: OccurrenceRole::Reference,
            enclosing: Some(entities[&2].record.id),
            target: rootlight_ir::OccurrenceTarget::Unresolved {
                text_hash: content_hash(b"OWNED"),
            },
            syntactic_text_hash: content_hash(b"OWNED"),
            syntax_kind: PROVEN.to_owned(),
            provenance,
            confidence: Confidence::new(399).unwrap(),
            evidence: FactEvidence {
                source: Some(occurrence_source),
                derivation: Vec::new(),
            },
        };
        site.id = derive_occurrence_record_id(&site).unwrap();
        Self {
            text: text.to_vec(),
            source,
            facts,
            entities,
            site,
        }
    }

    fn resolve(&self) -> Option<SymbolId> {
        let cancel = Cancellation::new();
        let mut targets = Targets::new(
            &self.text,
            &self.text,
            &self.source,
            BindingFacts {
                syntax: &self.facts,
                entities: &self.entities,
            },
            65_536,
            true,
            &cancel,
        )
        .unwrap()
        .unwrap();
        targets
            .resolve(&self.facts[5], &self.site, Ok, &cancel)
            .unwrap()
    }
}

#[test]
fn current_source_reparse_accepts_only_the_exact_native_assignment_and_owner_joins() {
    let fixture = Fixture::new(
        b"OWNED=value\nif [[ -n \"$OWNED\" ]]; then printf '%s' \"$OWNED\" 1>&2; fi\n",
    );
    assert_eq!(fixture.resolve(), Some(fixture.entities[&4].record.id));
    for change in 0..8 {
        let mut changed = Fixture::new(&fixture.text);
        match change {
            0 => changed.site.role = OccurrenceRole::Read,
            1 => changed.site.syntax_kind = UNPROVEN.to_owned(),
            2 => changed.site.enclosing = Some(SymbolId::from_bytes([9; 20])),
            3 => changed.entities.get_mut(&4).unwrap().definition_local_id = None,
            4 => {
                changed.entities.get_mut(&4).unwrap().direct_parent =
                    ContainerRef::File(changed.site.file)
            }
            5 => {
                changed.facts[4] = fact(
                    &changed.source,
                    5,
                    Some(3),
                    SyntaxFactKind::Occurrence,
                    (0, 5),
                    4,
                    "bash.variable.definition",
                )
            }
            6 => changed.facts.push(changed.facts[4].clone()),
            _ => {
                changed.facts[3] = fact(
                    &changed.source,
                    4,
                    Some(3),
                    SyntaxFactKind::Declaration,
                    (0, 5),
                    3,
                    "bash.variable.declaration",
                )
            }
        }
        assert_eq!(changed.resolve(), None, "forged join {change}");
    }
}

#[test]
fn a_positive_label_cannot_hide_actual_source_effects_or_numeric_evaluation() {
    for text in [
        b"OWNED=value\neval other\nprintf '%s' \"$OWNED\"\n".as_slice(),
        b"OWNED=value\nARITH='OWNED++'\n[[ $ARITH -eq 0 ]]\nprintf '%s' \"$OWNED\"\n".as_slice(),
        b"OWNED=value\nprintf -v OWNED changed\nprintf '%s' \"$OWNED\"\n".as_slice(),
        b"OWNED=value\nprintf '%n' OWNED\nprintf '%s' \"$OWNED\"\n".as_slice(),
        b"OWNED=value\nsource other.sh\nprintf '%s' \"$OWNED\"\n".as_slice(),
    ] {
        assert_eq!(Fixture::new(text).resolve(), None);
    }
}

#[test]
fn incomplete_input_and_zero_work_refuse_before_consumer_parser_admission() {
    let fixture = Fixture::new(b"OWNED=value\nprintf '%s' \"$OWNED\"\n");
    assert!(
        Targets::new(
            &fixture.text,
            &fixture.text,
            &fixture.source,
            BindingFacts {
                syntax: &fixture.facts,
                entities: &fixture.entities
            },
            0,
            false,
            &Cancellation::new()
        )
        .unwrap()
        .is_none()
    );
    assert!(
        matches!(Targets::new(&fixture.text, &fixture.text, &fixture.source,
        BindingFacts { syntax: &fixture.facts, entities: &fixture.entities },
        0, true, &Cancellation::new()),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "bash-reference-consumer-work-limit")
    );
}

fn resource_limits(ir: IrLimits) -> AnalysisLimits {
    use rootlight_adapter_sdk::{BatchThresholds, StreamLimits};
    let stream = StreamLimits::new(
        128,
        16_384,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        4 * 1024 * 1024,
        BatchThresholds::new(128, 1024 * 1024, 32, 128 * 1024).unwrap(),
    )
    .unwrap();
    AnalysisLimits::new(
        1024 * 1024,
        16_384,
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
fn real_companion_fact_has_separate_relation_record_string_and_nested_admission() {
    use rootlight_adapter_sdk::{GenerationBoundSnapshot, LanguageId};
    use rootlight_ids::derive_repository;
    use rootlight_ir::BuildContextIdentity;
    use rootlight_vfs::{RelativePath, RepositoryRoot};
    use std::{fs, path::Path, sync::Arc};
    let source = b"OWNED=value\nprintf '%s' \"$OWNED\"\n";
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("module.sh"), source).unwrap();
    let repository = derive_repository(b"bash-reference-resource").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("module.sh")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let full_source = SourceRef::new(
        repository,
        GenerationId::from_bytes([7; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(source.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let producer =
        ProducerIdentity::new("bash-reference-resource", "1.0", content_hash(b"resource")).unwrap();
    let provider = Arc::new(
        crate::TreeSitterProvider::new(
            crate::RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                32,
                64,
                1,
                16 * 1024 * 1024,
                crate::ParserSettings::new(4096).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider,
        producer.clone(),
        LanguageId::new("bash").unwrap(),
        "native-reference-resource",
        content_hash(b"resource-binary"),
    )
    .unwrap();
    let defaults = resource_limits(IrLimits::default());
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &full_source).unwrap(),
        LanguageId::new("bash").unwrap(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"resource-build")),
        &defaults,
    )
    .unwrap()
    .with_generated_status(false);
    let cancel = Cancellation::new();
    let parsed = analyzer.parse_request(&request, &cancel).unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        rootlight_ir::CoverageStatus::Complete
    );
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == PROVEN)
            .count(),
        1
    );
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "bash.module_read.reference")
            .count(),
        1
    );
    let entities = parsed
        .facts()
        .iter()
        .filter(|fact| structural_entity_kind(fact).is_some())
        .count();
    let baseline = preflight_lowering_limits(
        &request,
        &parsed,
        &producer,
        "native-reference-resource",
        &[],
        &cancel,
    )
    .unwrap();
    for resource in 0..4 {
        for exact in [false, true] {
            let mut ir = IrLimits::default();
            match resource {
                0 => ir.max_relations = entities + if exact { 2 } else { 1 },
                1 => ir.max_total_records = baseline.records - usize::from(!exact),
                2 => ir.max_total_string_bytes = baseline.string_bytes - usize::from(!exact),
                _ => ir.max_total_nested_items = baseline.nested_items - usize::from(!exact),
            }
            let budget = resource_limits(ir);
            let request = AnalysisRequest::new(
                GenerationBoundSnapshot::new(&snapshot, &full_source).unwrap(),
                LanguageId::new("bash").unwrap(),
                AnalysisTier::TierD,
                BuildContextIdentity::new(content_hash(b"resource-build")),
                &budget,
            )
            .unwrap()
            .with_generated_status(false);
            let result = preflight_lowering_limits(
                &request,
                &parsed,
                &producer,
                "native-reference-resource",
                &[],
                &cancel,
            );
            if exact {
                assert!(result.is_ok(), "exact resource {resource}")
            } else {
                assert!(
                    matches!(
                        result,
                        Err(AdapterError::Sink(SinkError::StreamLimit { .. }))
                    ),
                    "below resource {resource}"
                )
            }
        }
    }
}
