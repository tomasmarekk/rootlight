//! Reject ownership/depth substitutions at the independent consumer boundary.
//! Account adversarial endpoint geometry with a finite shared preparation budget.

use super::*;
use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId, GenerationBoundSnapshot,
    LanguageId, MemoryAdmissionPolicy, StreamLimits, SyntaxKindLabel, execute_analysis,
    execute_parse,
};
use rootlight_ids::{GenerationId, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ExtensionSupport, IrLimits, ProducerIdentity,
    decode_symbol_identity_claim_envelope,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use std::{fs, path::Path, sync::Arc};

fn fact(
    id: u64,
    kind: SyntaxFactKind,
    label: &str,
    file: FileId,
    start: u64,
    end: u64,
) -> SyntaxFact {
    SyntaxFact::new(
        id,
        None,
        kind,
        SourceSpan::new(file, start, end).unwrap(),
        0,
        SyntaxKindLabel::new(label).unwrap(),
    )
}

fn endpoint_facts(count: usize) -> Vec<SyntaxFact> {
    let file = FileId::from_bytes([1; 20]);
    let mut facts = Vec::new();
    for index in (0..count).rev() {
        let end = (index as u64 + 1) * 4;
        facts.push(fact(
            index as u64 * 2 + 1,
            SyntaxFactKind::Signature,
            READ_TARGET,
            file,
            0,
            end,
        ));
        facts.push(fact(
            index as u64 * 2 + 2,
            SyntaxFactKind::Occurrence,
            "cuda.identifier.reference",
            file,
            end - 2,
            end,
        ));
    }
    facts
}

#[test]
fn indexed_scope_accounting_preserves_full_native_identifier_geometry_and_files() {
    let cancel = Cancellation::new();
    let file = FileId::from_bytes([1; 20]);
    let facts = endpoint_facts(128);
    let index = ends(&facts, 100_000, &cancel).unwrap();
    for position in 1..=128 {
        let end = position * 4;
        let full = fact(
            999,
            SyntaxFactKind::Scope,
            "cuda.block.scope",
            file,
            end - 2,
            end,
        );
        let clipped = fact(
            999,
            SyntaxFactKind::Scope,
            "cuda.block.scope",
            file,
            end - 1,
            end,
        );
        let foreign = fact(
            999,
            SyntaxFactKind::Scope,
            "cpp.block.scope",
            FileId::from_bytes([2; 20]),
            end - 2,
            end,
        );
        assert!(index.scope_required(&full, &cancel).unwrap());
        assert!(
            !index.scope_required(&clipped, &cancel).unwrap(),
            "an end alone cannot certify partial containment"
        );
        assert!(!index.scope_required(&foreign, &cancel).unwrap());
    }
}

#[test]
fn indexed_scope_accounting_is_subquadratic_and_refuses_exact_one_less_work() {
    let cancel = Cancellation::new();
    let file = FileId::from_bytes([1; 20]);
    let mut previous = 0;
    for size in [256, 512, 1024, 2048] {
        let facts = endpoint_facts(size);
        let scopes: Vec<_> = (1..=size)
            .map(|position| {
                fact(
                    999,
                    SyntaxFactKind::Scope,
                    "cuda.block.scope",
                    file,
                    position as u64 * 4 - 2,
                    position as u64 * 4,
                )
            })
            .collect();
        let run = |maximum| -> Result<usize, AdapterError> {
            let index = ends(&facts, maximum, &cancel)?;
            for scope in &scopes {
                assert!(index.scope_required(scope, &cancel)?);
            }
            Ok(maximum - index.remaining.get())
        };
        let required = run(1_000_000).unwrap();
        assert_eq!(run(required).unwrap(), required);
        assert!(run(required - 1).is_err());
        assert!(
            required < size * 100,
            "finite indexed work: {size}/{required}"
        );
        if previous != 0 {
            assert!(
                required < previous * 3,
                "doubling input must not quadruple work"
            );
        }
        previous = required;
    }
    let cancelled = Cancellation::new();
    cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        ends(&endpoint_facts(1), 0, &cancelled),
        Err(AdapterError::Cancelled { .. })
    ));
}

fn limits() -> AnalysisLimits {
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
        16,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap()
}

#[test]
fn direct_consumer_joins_file_owners_materialized_containers_and_structural_depths() {
    const TEXT: &str = "__global__ void kernel(int value) {}\nvoid launch(int scalar) { int next = scalar * 2; kernel<<<1, 1>>>(next); }\n";
    for (language, path) in [("cuda", "sample.cu"), ("cpp", "sample.cuh")] {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join(path), TEXT).unwrap();
        let repository = derive_repository(b"cuda-direct-consumer-proof").id();
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&RelativePath::parse(Path::new(path)).unwrap(), 4096)
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([1; 20]),
            SourceSpan::new(snapshot.file(), 0, TEXT.len() as u64).unwrap(),
            snapshot.content_hash(),
            None,
        );
        let limits = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new(language).unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            BuildContextIdentity::new(content_hash(b"cuda-direct-consumer-context")),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
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
                    crate::ParserSettings::new(4096).unwrap(),
                )
                .unwrap(),
            )
            .unwrap(),
        );
        let analyzer = super::super::TreeSitterAnalyzer::new(
            provider.clone(),
            ProducerIdentity::new(
                "cuda-proof-test",
                "1.0",
                content_hash(b"cuda-proof-configuration"),
            )
            .unwrap(),
            LanguageId::new(language).unwrap(),
            "cuda-proof-test",
            content_hash(b"cuda-proof-binary"),
        )
        .unwrap();
        let cancel = Cancellation::new();
        let parsed = execute_parse(
            provider.as_ref(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel,
        )
        .unwrap();
        let output = execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel,
        )
        .unwrap();
        let mut materialized = HashMap::new();
        for raw in parsed
            .facts()
            .iter()
            .filter(|raw| raw.kind() == SyntaxFactKind::Declaration)
        {
            let Some(record) = output.document().entities.iter().find(|record| {
                record
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span() == raw.span())
                    && matches!(record.kind, EntityKind::Function | EntityKind::Parameter)
            }) else {
                continue;
            };
            let definition = parsed
                .facts()
                .iter()
                .find(|fact| {
                    fact.parent() == Some(raw.local_id())
                        && fact
                            .syntax_kind()
                            .as_str()
                            .ends_with(".identifier.definition")
                })
                .unwrap();
            let identity_claim = output
                .document()
                .extensions
                .iter()
                .filter_map(|envelope| decode_symbol_identity_claim_envelope(envelope).ok())
                .find(|claim| claim.symbol == record.id)
                .unwrap();
            // Only consumer inputs are rehydrated; collision/header caches are
            // deliberately absent and cannot supply independent proof.
            materialized.insert(
                raw.local_id(),
                MaterializedEntity {
                    record: record.clone(),
                    identity_claim,
                    direct_parent: record.container.unwrap(),
                    identity_guard: [0; 32],
                    definition_local_id: Some(definition.local_id()),
                    signature_evidence: None,
                    signature_span: None,
                },
            );
        }
        let run = |facts: &[SyntaxFact], entities: &HashMap<u64, MaterializedEntity>| {
            Targets::new(
                facts,
                entities,
                &source,
                TEXT.as_bytes(),
                TEXT.as_bytes(),
                1_048_576,
                128,
                Ok,
                &cancel,
            )
            .unwrap()
        };
        let positive = run(parsed.facts(), &materialized);
        assert!(
            positive
                .bindings
                .values()
                .any(|binding| binding.facet == Facet::Read)
        );
        assert!(
            positive
                .bindings
                .values()
                .any(|binding| binding.facet == Facet::Call)
        );
        for name in ["launch", "kernel", "scalar"] {
            let selected = *materialized
                .iter()
                .find(|(_, entity)| entity.record.canonical_name == name)
                .unwrap()
                .0;
            for mutation in 0..4 {
                let mut entities = materialized.clone();
                let entity = entities.get_mut(&selected).unwrap();
                let wrong = ContainerRef::File(FileId::from_bytes([99; 20]));
                if mutation == 0 || mutation == 3 {
                    entity.direct_parent = wrong;
                }
                if mutation == 1 || mutation == 3 {
                    entity.record.container = Some(wrong);
                }
                if mutation == 2 || mutation == 3 {
                    entity.identity_claim.container = Some(wrong);
                }
                let result = run(parsed.facts(), &entities);
                assert!(
                    !result.bindings.values().any(|binding| name == "launch"
                        || binding.facet
                            == if name == "kernel" {
                                Facet::Call
                            } else {
                                Facet::Read
                            }),
                    "materialized {name}/{mutation}"
                );
            }
        }
        for name in ["launch", "kernel"] {
            let selected = *materialized
                .iter()
                .find(|(_, entity)| entity.record.canonical_name == name)
                .unwrap()
                .0;
            let mut facts = parsed.facts().to_vec();
            let position = facts
                .iter()
                .position(|fact| fact.local_id() == selected)
                .unwrap();
            let old = facts[position].clone();
            facts[position] = SyntaxFact::new(
                old.local_id(),
                None,
                old.kind(),
                old.span(),
                old.depth(),
                old.syntax_kind().clone(),
            );
            let result = run(&facts, &materialized);
            assert!(
                !result
                    .bindings
                    .values()
                    .any(|binding| name == "launch" || binding.facet == Facet::Call),
                "raw function parent {name}"
            );
        }
        let same_binding = |left: &Binding, right: &Binding| {
            left.target == right.target
                && left.owner == right.owner
                && left.facet == right.facet
                && left.source == right.source
                && left.spelling == right.spelling
                && left.provenance == right.provenance
        };
        let assert_unchanged = |actual: &Targets| {
            assert_eq!(actual.bindings.len(), positive.bindings.len());
            for (id, expected) in &positive.bindings {
                assert!(
                    actual
                        .bindings
                        .get(id)
                        .is_some_and(|binding| same_binding(binding, expected)),
                    "unrelated role changed binding {language}/{id}"
                );
            }
        };
        let root_fact = parsed
            .facts()
            .iter()
            .find(|fact| fact.kind() == SyntaxFactKind::Root)
            .unwrap();
        let next_id = parsed
            .facts()
            .iter()
            .map(SyntaxFact::local_id)
            .max()
            .unwrap()
            .checked_add(1)
            .unwrap();
        let (kernel_id, _) = materialized
            .iter()
            .find(|(_, entity)| entity.record.canonical_name == "kernel")
            .unwrap();
        let kernel = parsed
            .facts()
            .iter()
            .find(|fact| fact.local_id() == *kernel_id)
            .unwrap();
        let contexts: Vec<_> = parsed
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Signature
                    && fact.syntax_kind().as_str() == "cuda.owned_function.signature"
                    && fact.span() == kernel.span()
            })
            .collect();
        assert_eq!(contexts.len(), 1);
        let context = contexts[0];
        assert_eq!(context.parent(), Some(*kernel_id));
        assert_eq!(context.depth(), kernel.depth() + 1);
        for mutation in 0..6 {
            let mut facts = parsed.facts().to_vec();
            let position = facts
                .iter()
                .position(|fact| fact.local_id() == context.local_id())
                .unwrap();
            if mutation == 0 {
                facts.remove(position);
            } else if mutation == 1 {
                facts.push(SyntaxFact::new(
                    next_id,
                    context.parent(),
                    context.kind(),
                    context.span(),
                    context.depth(),
                    context.syntax_kind().clone(),
                ));
            } else {
                let span = if mutation == 4 {
                    SourceSpan::new(
                        context.span().file(),
                        context.span().start_byte(),
                        context.span().end_byte() - 1,
                    )
                    .unwrap()
                } else if mutation == 5 {
                    let foreign = FileId::from_bytes([99; 20]);
                    assert_ne!(foreign, context.span().file());
                    SourceSpan::new(
                        foreign,
                        context.span().start_byte(),
                        context.span().end_byte(),
                    )
                    .unwrap()
                } else {
                    context.span()
                };
                facts[position] = SyntaxFact::new(
                    context.local_id(),
                    if mutation == 2 {
                        Some(root_fact.local_id())
                    } else {
                        context.parent()
                    },
                    context.kind(),
                    span,
                    if mutation == 2 {
                        root_fact.depth() + 1
                    } else if mutation == 3 {
                        context.depth() + 1
                    } else {
                        context.depth()
                    },
                    context.syntax_kind().clone(),
                );
            }
            if mutation != 5 {
                super::super::validate_fact_graph(&facts, &[], &source, &cancel).unwrap();
            }
            let result = run(&facts, &materialized);
            assert!(
                !result
                    .bindings
                    .values()
                    .any(|binding| binding.facet == Facet::Call),
                "callee context {language}/{mutation}"
            );
            for (id, expected) in positive
                .bindings
                .iter()
                .filter(|(_, binding)| binding.facet == Facet::Read)
            {
                assert!(
                    result
                        .bindings
                        .get(id)
                        .is_some_and(|binding| same_binding(binding, expected)),
                    "callee context must preserve the independent Read {language}/{mutation}"
                );
            }
        }
        // Coincident geometry supplies no authority for an unused raw role.
        for name in ["launch", "kernel"] {
            let (id, _) = materialized
                .iter()
                .find(|(_, entity)| entity.record.canonical_name == name)
                .unwrap();
            let declaration = parsed
                .facts()
                .iter()
                .find(|fact| fact.local_id() == *id)
                .unwrap();
            for (kind, label) in [
                (SyntaxFactKind::Signature, "function.expression"),
                (SyntaxFactKind::Scope, "block.scope"),
            ] {
                for depth in [declaration.depth() + 1, declaration.depth() + 2] {
                    let mut facts = parsed.facts().to_vec();
                    facts.push(SyntaxFact::new(
                        next_id,
                        Some(*id),
                        kind,
                        declaration.span(),
                        depth,
                        SyntaxKindLabel::new(&format!("{language}.{label}")).unwrap(),
                    ));
                    super::super::validate_fact_graph(&facts, &[], &source, &cancel).unwrap();
                    assert_unchanged(&run(&facts, &materialized));
                }
            }
        }
        let span = |start: usize, end: usize| {
            SourceSpan::new(
                source.span().file(),
                u64::try_from(start).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap()
        };
        let header_start = TEXT.find("(int scalar)").unwrap();
        let owner_header = span(header_start, header_start + "(int scalar)".len());
        let owner_body = span(
            TEXT.find("{ int next").unwrap(),
            TEXT.rfind('}').unwrap() + 1,
        );
        let next_statement = span(
            TEXT.find("int next").unwrap(),
            TEXT.find("; kernel").unwrap() + 1,
        );
        let read_marker = span(
            TEXT.find("scalar").unwrap(),
            TEXT.rfind("scalar").unwrap() + "scalar".len(),
        );
        for witness in parsed.facts() {
            let label = witness.syntax_kind().as_str();
            let mut facts = parsed.facts().to_vec();
            let position = facts
                .iter()
                .position(|fact| fact.local_id() == witness.local_id())
                .unwrap();
            facts[position] = SyntaxFact::new(
                witness.local_id(),
                witness.parent(),
                witness.kind(),
                witness.span(),
                witness.depth() + 1,
                witness.syntax_kind().clone(),
            );
            let result = run(&facts, &materialized);
            for (reference_id, binding) in &positive.bindings {
                let site = parsed
                    .facts()
                    .iter()
                    .find(|fact| fact.local_id() == *reference_id)
                    .unwrap();
                let (owner_id, owner) = materialized
                    .iter()
                    .find(|(_, entity)| entity.record.id == binding.owner)
                    .unwrap();
                let (target_id, target) = materialized
                    .iter()
                    .find(|(_, entity)| entity.record.id == binding.target)
                    .unwrap();
                let owner_span = owner.record.evidence.source.as_ref().unwrap().span();
                let target_span = target.record.evidence.source.as_ref().unwrap().span();
                let endpoint_declaration = witness.kind() == SyntaxFactKind::Declaration
                    && (witness.local_id() == *owner_id
                        && label.ends_with(".function.declaration")
                        || witness.local_id() == *target_id
                            && if binding.facet == Facet::Call {
                                label.ends_with(".function.declaration")
                            } else {
                                label.ends_with(".parameter.declaration")
                                    || label.ends_with(".ordinary_parameter.declaration")
                            });
                let endpoint_definition = witness.kind() == SyntaxFactKind::Occurrence
                    && label.ends_with(".identifier.definition")
                    && (owner.definition_local_id == Some(witness.local_id())
                        || target.definition_local_id == Some(witness.local_id()));
                let function_context = witness.kind() == SyntaxFactKind::Signature
                    && label == "cuda.owned_function.signature"
                    && (witness.span() == owner_span
                        || binding.facet == Facet::Call && witness.span() == target_span);
                let parameters = witness.kind() == SyntaxFactKind::Signature
                    && label.ends_with(".parameters.signature")
                    && witness.span() == owner_header;
                let ancestor = witness.kind() == SyntaxFactKind::Scope
                    && label.ends_with(".block.scope")
                    && witness.span() == owner_body
                    || binding.facet == Facet::Read
                        && witness.kind() == SyntaxFactKind::Declaration
                        && label.ends_with(".declaration.declaration")
                        && witness.span() == next_statement;
                let marker = witness.kind() == SyntaxFactKind::Signature
                    && if binding.facet == Facet::Read {
                        label == READ_TARGET && witness.span() == read_marker
                    } else {
                        label == CALL_TARGET && witness.span() == site.span()
                    };
                let current_reference = reference(witness) && witness.local_id() == *reference_id;
                let proof_relevant = witness.kind() == SyntaxFactKind::Root
                    || witness.kind() == SyntaxFactKind::Signature
                        && label == "cuda.clean_file.signature"
                    || endpoint_declaration
                    || endpoint_definition
                    || function_context
                    || parameters
                    || ancestor
                    || marker
                    || current_reference;
                if proof_relevant {
                    assert!(
                        !result
                            .bindings
                            .values()
                            .any(|entry| entry.facet == binding.facet),
                        "raw depth {language}/{label}; original={witness:?}; mutated={:?}; facet={}; site={site:?}; owner={:?}; target={:?}",
                        facts[position],
                        if binding.facet == Facet::Read {
                            "Read"
                        } else {
                            "Call"
                        },
                        binding.owner,
                        binding.target,
                    );
                } else {
                    assert!(
                        result
                            .bindings
                            .get(reference_id)
                            .is_some_and(|entry| same_binding(entry, binding)),
                        "unused raw role must preserve its independent facet {language}/{label}; original={witness:?}"
                    );
                }
            }
        }
    }
}
