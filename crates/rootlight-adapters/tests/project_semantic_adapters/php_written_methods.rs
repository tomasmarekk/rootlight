//! Project PHP call adoption requires current native class and method witnesses.
//! Native baselines precede deliberate valid-graph mutations; unsupported lookup
//! and callable acquisition remain unresolved rather than borrowing a spelling.

use super::*;
use rootlight_adapter_sdk::{SyntaxFactBatch, execute_parse};

fn bound_calls(output: &rootlight_adapter_sdk::ProjectAnalysisOutput) -> usize {
    output
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "php.this_method.reference"
                && site.role == OccurrenceRole::CallSite
                && matches!(site.target, OccurrenceTarget::Resolved { .. })
        })
        .count()
}

#[test]
fn php_project_own_class_calls_keep_exact_written_method_and_source() {
    for (method, call) in [("Target", "TARGET"), ("inspect", "inspect")] {
        let source = format!(
            "<?php class Owner {{ private function {method}($value) {{ return $value; }} function Run() {{ return $this->{call}(1); }} }}\n"
        );
        let fixture = ProjectFixture::new(
            ["src/Other.php", "src/Owner.php"],
            ["<?php class Other {}\n", source.as_str()],
            SemanticProjectLanguage::Php,
        );
        let output = analyze_with_real_parser(&fixture);
        assert_eq!(bound_calls(&output), 1);
        let document = output.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Method && entity.display_name == method)
            .unwrap();
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Method && entity.display_name == "Run")
            .unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "php.this_method.reference"
                    && site.role == OccurrenceRole::CallSite
                    && site.target == (OccurrenceTarget::Resolved { symbol: target.id })
            })
            .unwrap();
        assert_eq!(site.enclosing, Some(owner.id));
        assert_eq!(
            site.source.content_hash(),
            fixture.snapshots[1].content_hash()
        );
        let span = site.source.span();
        assert_eq!(
            source.get(
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()
            ),
            Some(call)
        );
        let relation = document
            .relations
            .iter()
            .find(|edge| {
                edge.subject == rootlight_ir::RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == RelationPredicate::Calls
                    && edge.object == rootlight_ir::RelationEndpoint::Entity(target.id)
            })
            .unwrap();
        assert_eq!(relation.evidence.source.as_ref(), Some(&site.source));
        for parent in [
            FactRef::Entity(owner.id),
            FactRef::Entity(target.id),
            FactRef::File(site.file),
        ] {
            assert!(site.evidence.derivation.contains(&parent));
            assert!(relation.evidence.derivation.contains(&parent));
        }
    }
}

#[test]
fn php_project_static_inherited_duplicate_dynamic_and_complex_calls_stay_unproved() {
    for source in [
        "<?php class Owner { static function Target() {} function Run() { $this->Target(); } }",
        "<?php class Base { function Target() {} } class Owner extends Base { function Run() { $this->Target(); } }",
        "<?php trait Shared { function Target() {} } class Owner { use Shared; function Run() { $this->Target(); } }",
        "<?php class Owner { function Target() {} function TARGET() {} function Run() { $this->Target(); } }",
        "<?php class Owner { function Target() {} function Run($peer) { $peer->Target(); } }",
        "<?php class Owner { function Target() {} function Run($peer) { $this = $peer; $this->Target(); } }",
        "<?php class Owner { function Target() {} function Run() { $this->Target(...); } }",
        "<?php class Owner { function Target() {} function Run() { $callback = function() { $this->Target(); }; } }",
    ] {
        let fixture =
            ProjectFixture::new(["src/Owner.php"], [source], SemanticProjectLanguage::Php);
        assert_eq!(
            bound_calls(&analyze_with_real_parser(&fixture)),
            0,
            "{source}"
        );
    }
}

#[derive(Clone, Copy)]
enum Mutation {
    None,
    MissingClassContext,
    DuplicateMethodContext,
    ReparentSite,
    ShiftContext,
    ShiftDefinition,
    DuplicateDeclaration,
    RelabelUnproved,
}

struct NativeThenMutate {
    parser: TreeSitterProvider,
    mutation: Mutation,
}

impl ParseProvider for NativeThenMutate {
    fn capabilities(&self) -> &ParseCapabilities {
        self.parser.capabilities()
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let output = execute_parse(
            &self.parser,
            request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            cancellation,
        )?;
        let mut facts = output.facts().to_vec();
        let mut diagnostics = output.diagnostics().to_vec();
        let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
        let root = facts
            .iter()
            .find(|fact| fact.kind() == SyntaxFactKind::Root)
            .unwrap()
            .local_id();
        let class_context = facts
            .iter()
            .position(|fact| fact.syntax_kind().as_str() == "php.this_class_context.signature");
        let method_context = facts
            .iter()
            .position(|fact| fact.syntax_kind().as_str() == "php.this_method_context.signature");
        match self.mutation {
            Mutation::None => {}
            Mutation::MissingClassContext => {
                facts.remove(class_context.expect("baseline class context"));
            }
            Mutation::DuplicateMethodContext => {
                let fact = &facts[method_context.expect("baseline method context")];
                facts.push(SyntaxFact::new(
                    next,
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                ));
            }
            Mutation::ReparentSite => {
                let index = facts
                    .iter()
                    .position(|fact| fact.syntax_kind().as_str() == "php.this_method.reference")
                    .unwrap();
                let fact = &facts[index];
                facts[index] = SyntaxFact::new(
                    fact.local_id(),
                    Some(root),
                    fact.kind(),
                    fact.span(),
                    1,
                    fact.syntax_kind().clone(),
                );
            }
            Mutation::ShiftContext => {
                let index = method_context.unwrap();
                let fact = &facts[index];
                let body = facts
                    .iter()
                    .find(|candidate| {
                        candidate.parent() == fact.parent()
                            && candidate.syntax_kind().as_str() == "php.block.scope"
                    })
                    .unwrap();
                facts[index] = SyntaxFact::new(
                    fact.local_id(),
                    fact.parent(),
                    fact.kind(),
                    body.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                );
            }
            Mutation::ShiftDefinition => {
                let index = facts
                    .iter()
                    .position(|fact| {
                        fact.syntax_kind().as_str() == "php.name.definition"
                            && request.source().bytes().get(
                                usize::try_from(fact.span().start_byte()).unwrap()
                                    ..usize::try_from(fact.span().end_byte()).unwrap(),
                            ) == Some(b"Target")
                    })
                    .unwrap();
                let start = request
                    .source()
                    .bytes()
                    .windows(b"return Target".len())
                    .position(|bytes| bytes == b"return Target")
                    .unwrap()
                    + 7;
                let fact = &facts[index];
                facts[index] = SyntaxFact::new(
                    fact.local_id(),
                    fact.parent(),
                    fact.kind(),
                    SourceSpan::new(
                        fact.span().file(),
                        u64::try_from(start).unwrap(),
                        u64::try_from(start + 6).unwrap(),
                    )
                    .unwrap(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                );
            }
            Mutation::DuplicateDeclaration => {
                let fact = facts
                    .iter()
                    .find(|fact| fact.syntax_kind().as_str() == "php.method.declaration")
                    .unwrap();
                facts.push(SyntaxFact::new(
                    next,
                    fact.parent(),
                    fact.kind(),
                    fact.span(),
                    fact.depth(),
                    fact.syntax_kind().clone(),
                ));
            }
            Mutation::RelabelUnproved => {
                let mut changed = 0;
                for fact in &mut facts {
                    if fact.syntax_kind().as_str() == "php.unproven_this_method.reference" {
                        *fact = SyntaxFact::new(
                            fact.local_id(),
                            fact.parent(),
                            fact.kind(),
                            fact.span(),
                            fact.depth(),
                            SyntaxKindLabel::new("php.this_method.reference").unwrap(),
                        );
                        changed += 1;
                    }
                }
                assert_eq!(
                    changed, 2,
                    "dynamic receiver and callable acquisition witnesses"
                );
            }
        }
        for batch in facts.chunks(128) {
            sink.push(SyntaxFactBatch::new(
                sink.next_sequence(),
                batch.to_vec(),
                std::mem::take(&mut diagnostics),
            ))?;
        }
        let usage = sink.staged_usage();
        WorkReport::new(
            output.report().coverage().clone(),
            ResourceUsage::new(
                request.source().bytes().len(),
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

fn mutated(
    fixture: &ProjectFixture,
    mutation: Mutation,
) -> rootlight_adapter_sdk::ProjectAnalysisOutput {
    let analyzer = SemanticProjectAnalyzer::new(
        fixture.language,
        Arc::new(NativeThenMutate {
            parser: real_parser(),
            mutation,
        }),
        producer_identity(),
        content_hash(b"php-project-native-and-explicit-mutant"),
        fixture.build_context,
    )
    .unwrap();
    execute_project_analysis(
        &analyzer,
        &fixture.request(&real_parser_limits(), AnalysisTier::TierB),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap()
}

#[test]
fn php_project_native_context_mutants_cannot_supply_a_written_target() {
    let source = "<?php class Owner { function Target() { return Target; } function Run() { $this->Target(); } }";
    let fixture = ProjectFixture::new(["src/Owner.php"], [source], SemanticProjectLanguage::Php);
    assert_eq!(bound_calls(&mutated(&fixture, Mutation::None)), 1);
    for mutation in [
        Mutation::MissingClassContext,
        Mutation::DuplicateMethodContext,
        Mutation::ReparentSite,
        Mutation::ShiftContext,
        Mutation::ShiftDefinition,
        Mutation::DuplicateDeclaration,
    ] {
        assert_eq!(bound_calls(&mutated(&fixture, mutation)), 0);
    }
}

#[test]
fn php_project_relabelled_dynamic_or_callable_sites_cannot_borrow_the_native_positive() {
    let source = "<?php class Owner { function Target() {} function Run() { $this->Target(); } function Other($peer) { $peer->Target(); } function Acquire() { $this->Target(...); } }";
    let fixture = ProjectFixture::new(["src/Owner.php"], [source], SemanticProjectLanguage::Php);
    assert_eq!(bound_calls(&mutated(&fixture, Mutation::None)), 1);
    assert_eq!(
        bound_calls(&mutated(&fixture, Mutation::RelabelUnproved)),
        1
    );
}
