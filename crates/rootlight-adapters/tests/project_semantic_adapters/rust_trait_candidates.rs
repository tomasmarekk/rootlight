//! Written trait-object candidates retain lexical and module identity without
//! claiming an exhaustive implementation set or a selected runtime target.

use super::*;
use rootlight_ir::{NormalizedIrDocument, OccurrenceRecord, RelationEndpoint};

fn member_call<'a>(document: &'a NormalizedIrDocument, spelling: &str) -> &'a OccurrenceRecord {
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|call| {
            call.role == OccurrenceRole::CallSite
                && call.syntactic_text_hash == content_hash(spelling.as_bytes())
        })
        .collect();
    assert_eq!(calls.len(), 1, "one authored {spelling}");
    calls[0]
}

fn candidates(document: &NormalizedIrDocument, spelling: &str, expected: usize) {
    let call = member_call(document, spelling);
    let OccurrenceTarget::Candidates {
        symbols,
        total_count,
        completeness,
    } = &call.target
    else {
        panic!("written implementations remain candidates: {call:?}");
    };
    assert_eq!(*completeness, CoverageStatus::Unknown);
    assert_eq!(*total_count, u64::try_from(expected).unwrap());
    assert_eq!(symbols.len(), expected);
    for symbol in symbols {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == *symbol)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Method);
        assert!(call.evidence.derivation.contains(&FactRef::Entity(*symbol)));
        assert!(document.relations.iter().any(|relation| {
            relation.subject == RelationEndpoint::Occurrence(call.id)
                && relation.predicate == RelationPredicate::DispatchCandidate
                && relation.object == RelationEndpoint::Entity(*symbol)
                && relation.evidence == call.evidence
        }));
    }
    assert!(!document.relations.iter().any(|relation| {
        relation.subject == RelationEndpoint::Occurrence(call.id)
            && relation.predicate == RelationPredicate::Calls
    }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.source == call.source
            && gap.detail == "rust-trait-dispatch-runtime-implementation-unverified"
    }));
}

#[test]
fn complete_holdout_keeps_eight_exact_two_trait_candidates_and_two_unknown_calls() {
    let fixture = ProjectFixture::new(
        ["holdout/main.rs", "holdout/dep_a.rs", "holdout/dep_b.rs"],
        [
            include_str!(
                "../../../rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/rust/main.rs"
            ),
            include_str!(
                "../../../rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/rust/dep_a.rs"
            ),
            include_str!(
                "../../../rootlight-bench/tests/fixtures/semantic-holdout/v1/sources/rust/dep_b.rs"
            ),
        ],
        SemanticProjectLanguage::Rust,
    );
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|call| call.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 12);
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call.target, OccurrenceTarget::Resolved { .. }))
            .count(),
        8
    );
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call.target, OccurrenceTarget::Unresolved { .. }))
            .count(),
        2
    );
    candidates(document, "receiver.fixture_node_09", 2);
    candidates(document, "receiver.fixture_node_10", 2);
    for file in &document.files {
        assert!(
            member_call(document, "receiver.fixture_node_09")
                .evidence
                .derivation
                .contains(&FactRef::File(file.id))
        );
    }
}

#[test]
fn renamed_explicit_trait_imports_preserve_unknown_candidates_even_for_one_impl() {
    for (trait_name, receiver, method) in [
        ("Protocol", "receiver", "perform"),
        ("Service", "value", "execute"),
    ] {
        let source = format!(
            "mod api; mod implementation; use crate::api::{trait_name} as Selected; fn run({receiver}: &dyn Selected) {{ {receiver}.{method}(); }}"
        );
        let api = format!("pub trait {trait_name} {{ fn {method}(&self); }}");
        let implementation = format!(
            "use crate::api::{trait_name} as Bound; pub struct Worker; impl Bound for Worker {{ fn {method}(&self) {{}} }}"
        );
        let fixture = ProjectFixture::new(
            [
                "program/main.rs",
                "program/api.rs",
                "program/implementation.rs",
            ],
            [&source, &api, &implementation],
            SemanticProjectLanguage::Rust,
        );
        let output = analyze_with_real_parser(&fixture);
        candidates(output.document(), &format!("{receiver}.{method}"), 1);
    }
}

#[test]
fn trait_identity_mismatch_and_unattached_implementation_files_do_not_supply_candidates() {
    for include_wrong in [true, false] {
        let main = if include_wrong {
            "mod api; mod other; mod implementation; use crate::api::Protocol; fn run(receiver: &dyn Protocol) { receiver.perform(); }"
        } else {
            "mod api; use crate::api::Protocol; fn run(receiver: &dyn Protocol) { receiver.perform(); }"
        };
        let fixture = ProjectFixture::new(
            [
                "src/main.rs",
                "src/api.rs",
                "src/other.rs",
                "src/implementation.rs",
            ],
            [
                main,
                "pub trait Protocol { fn perform(&self); }",
                "pub trait Protocol { fn perform(&self); }",
                "use crate::other::Protocol; pub struct Worker; impl Protocol for Worker { fn perform(&self) {} }",
            ],
            SemanticProjectLanguage::Rust,
        );
        let output = analyze_with_real_parser(&fixture);
        assert!(matches!(
            member_call(output.document(), "receiver.perform").target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
}

#[test]
fn shadowing_generic_contexts_and_duplicate_impls_cannot_become_runtime_targets() {
    let api = "pub trait Protocol { fn perform(&self); }";
    let ordinary = "use crate::api::Protocol; pub struct Worker; impl Protocol for Worker { fn perform(&self) {} }";
    for (function, implementation) in [
        (
            "fn run(receiver: &dyn Protocol) { let receiver = unknown; receiver.perform(); }",
            ordinary,
        ),
        (
            "fn run(receiver: &dyn Protocol) { let closure = |receiver| { receiver.perform(); }; }",
            ordinary,
        ),
        (
            "fn run<T>(receiver: &dyn Protocol) { receiver.perform(); }",
            ordinary,
        ),
        (
            "fn run(receiver: &dyn Protocol) { receiver.perform::<u8>(); }",
            ordinary,
        ),
        (
            "fn run(receiver: &dyn Protocol) { receiver.perform(1); }",
            ordinary,
        ),
        (
            "fn run(receiver: &dyn Protocol) { receiver.perform(); }",
            "use crate::api::Protocol; pub struct Worker; impl Protocol for Worker { fn perform(&self) {} } impl Protocol for Worker {}",
        ),
        (
            "fn run(receiver: &dyn Protocol) { receiver.perform(); }",
            "use crate::api::Protocol; pub struct Worker; #[cfg(feature=\"optional\")] impl Protocol for Worker { fn perform(&self) {} }",
        ),
    ] {
        let source = format!("mod api; mod implementation; use crate::api::Protocol; {function}");
        let fixture = ProjectFixture::new(
            ["src/main.rs", "src/api.rs", "src/implementation.rs"],
            [&source, api, implementation],
            SemanticProjectLanguage::Rust,
        );
        let output = analyze_with_real_parser(&fixture);
        let calls: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|call| call.role == OccurrenceRole::CallSite)
            .collect();
        assert!(!calls.is_empty(), "authored call stays visible: {function}");
        assert!(
            calls
                .iter()
                .all(|call| matches!(call.target, OccurrenceTarget::Unresolved { .. })),
            "{function}: {calls:?}"
        );
        assert!(!output.document().relations.iter().any(|relation| matches!(
            relation.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        )));
    }
}

#[test]
fn a_nested_block_function_does_not_relax_module_namespace_opacity() {
    let fixture = ProjectFixture::new(
        ["src/main.rs", "src/api.rs"],
        [
            "mod api; fn run() { crate::api::helper(); }",
            "pub fn helper() {} fn other() { fn nested() {} }",
        ],
        SemanticProjectLanguage::Rust,
    );
    let output = analyze_with_real_parser(&fixture);
    assert!(matches!(
        member_call(output.document(), "crate::api::helper").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

struct CompetingWitnessParser {
    inner: TreeSitterProvider,
    mode: u8,
}

impl ParseProvider for CompetingWitnessParser {
    fn capabilities(&self) -> &ParseCapabilities {
        self.inner.capabilities()
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let output = rootlight_adapter_sdk::execute_parse(
            &self.inner,
            request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )?;
        let mut facts = output.facts().to_vec();
        let label = if self.mode == 1 {
            "rust.trait.declaration"
        } else {
            "rust.impl_trait.scope_trait"
        };
        if self.mode != 0
            && let Some(fact) = facts
                .iter()
                .find(|fact| fact.syntax_kind().as_str() == label)
                .cloned()
        {
            if self.mode == 3 {
                let slot = facts
                    .iter_mut()
                    .find(|candidate| candidate.local_id() == fact.local_id())
                    .unwrap();
                *slot = SyntaxFact::new(
                    fact.local_id(),
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    SyntaxKindLabel::new("rust.foreign_trait.scope_trait").unwrap(),
                );
            } else {
                let id = next_local_id(&facts);
                facts.push(SyntaxFact::new(
                    id,
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                ));
            }
        }
        for chunk in facts.chunks(256) {
            sink.push(rootlight_adapter_sdk::SyntaxFactBatch::new(
                sink.next_sequence(),
                chunk.to_vec(),
                Vec::new(),
            ))?;
        }
        let length = request.source().bytes().len();
        let usage = sink.staged_usage();
        WorkReport::new(
            CoverageReport::new(
                AnalysisTier::TierD,
                CoverageStatus::Complete,
                length,
                length,
                0,
                Vec::new(),
            )?,
            ResourceUsage::new(
                length,
                facts.len(),
                output.report().resources().syntax_nodes(),
                facts.iter().map(SyntaxFact::depth).max().unwrap_or(0),
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )
        .map_err(AdapterError::from)
    }
}

#[test]
fn competing_raw_declarations_and_impl_roles_cannot_promote_a_surviving_definition() {
    let fixture = ProjectFixture::new(
        ["src/main.rs", "src/api.rs", "src/implementation.rs"],
        [
            "mod api; mod implementation; use crate::api::Protocol; fn run(receiver: &dyn Protocol) { receiver.perform(); }",
            "pub trait Protocol { fn perform(&self); }",
            "use crate::api::Protocol; pub struct Worker; impl Protocol for Worker { fn perform(&self) {} }",
        ],
        SemanticProjectLanguage::Rust,
    );
    for mode in 0..=3 {
        let analyzer = SemanticProjectAnalyzer::new(
            SemanticProjectLanguage::Rust,
            Arc::new(CompetingWitnessParser {
                inner: real_parser(),
                mode,
            }),
            producer_identity(),
            content_hash(b"real-parser-binary"),
            fixture.build_context,
        )
        .unwrap();
        let limits = real_parser_limits();
        let output = execute_project_analysis(
            &analyzer,
            &fixture.request(&limits, AnalysisTier::TierB),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        if mode == 0 {
            candidates(output.document(), "receiver.perform", 1);
        } else {
            assert!(
                matches!(
                    member_call(output.document(), "receiver.perform").target,
                    OccurrenceTarget::Unresolved { .. }
                ),
                "mode {mode}"
            );
        }
    }
}
