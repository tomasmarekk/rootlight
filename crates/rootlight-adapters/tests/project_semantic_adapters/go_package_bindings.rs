//! Static package-function joins use native baselines before malformed carriers.
//! These tests preserve unresolved imports, shadowing and runtime uncertainty;
//! only a paired bare invocation can produce a project Calls relation.

use super::*;
use rootlight_adapter_sdk::{SyntaxFactBatch, execute_parse};

fn bound_calls(output: &rootlight_adapter_sdk::ProjectAnalysisOutput, target: &str) -> usize {
    let symbols: BTreeSet<_> = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.display_name == target)
        .map(|entity| entity.id)
        .collect();
    output.document().occurrences.iter().filter(|site| {
        site.role == OccurrenceRole::CallSite
            && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbols.contains(&symbol))
    }).count()
}

#[test]
fn go_project_package_and_file_calls_keep_source_owned_targets() {
    for (target, caller) in [
        ("GenerateHandler", "TestGenerateHandler"),
        ("produce", "inspect"),
    ] {
        let production = format!("package sample\nfunc {target}() {{}}\n");
        let source = format!(
            "package sample\nimport \"testing\"\nfunc {caller}(t *testing.T) {{ {target}() }}\n"
        );
        let fixture = ProjectFixture::new(
            ["server/production.go", "server/check_test.go"],
            [production.as_str(), source.as_str()],
            SemanticProjectLanguage::Go,
        );
        let output = analyze_with_real_parser(&fixture);
        assert_eq!(bound_calls(&output, target), 1);
        let call = output
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "go.package_function_candidate.reference"
                    && site.role == OccurrenceRole::CallSite
            })
            .expect("paired cross-file site is promoted only after the project proof");
        assert!(
            call.evidence
                .derivation
                .iter()
                .any(|fact| matches!(fact, FactRef::Entity(_)))
        );
        assert!(output.document().relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == rootlight_ir::RelationEndpoint::Occurrence(call.id)
        }));
    }
    let fixture = ProjectFixture::new(
        ["server/worker.go"],
        ["package sample\nfunc work(){}\nfunc caller(){work()}\n"],
        SemanticProjectLanguage::Go,
    );
    assert_eq!(bound_calls(&analyze_with_real_parser(&fixture), "work"), 1);
}

#[test]
fn go_project_binding_refuses_other_packages_directories_and_namespace_competitors() {
    for (path, production, source) in [
        (
            "other/target.go",
            "package sample\nfunc Target(){}\n",
            "package sample\nfunc Caller(){Target()}\n",
        ),
        (
            "server/target.go",
            "package foreign\nfunc Target(){}\n",
            "package sample\nfunc Caller(){Target()}\n",
        ),
        (
            "server/target.go",
            "package sample\nfunc Target(){}\nfunc Target(){}\n",
            "package sample\nfunc Caller(){Target()}\n",
        ),
        (
            "server/target.go",
            "package sample\nvar Target = 1\n",
            "package sample\nfunc Caller(){Target()}\n",
        ),
        (
            "server/target.go",
            "package sample\ntype Target int\n",
            "package sample\nfunc Caller(){Target()}\n",
        ),
    ] {
        let fixture = ProjectFixture::new(
            [path, "server/caller.go"],
            [production, source],
            SemanticProjectLanguage::Go,
        );
        assert_eq!(
            bound_calls(&analyze_with_real_parser(&fixture), "Target"),
            0,
            "{path}: {production}"
        );
    }
    for caller in [
        "func Caller(Target func()){Target()}",
        "func Caller(){Target := other; Target()}",
        "func Caller(){Target = other; Target()}",
        "func Caller(){for Target := range values {Target()}}",
        "func Caller(){local := func(){Target()}; local()}",
        "func Caller(){Target[int]()}",
        "func Caller(){receiver.Target()}",
    ] {
        let source = format!("package sample\nfunc Target(){{}}\n{caller}\n");
        let fixture = ProjectFixture::new(
            ["server/caller.go"],
            [source.as_str()],
            SemanticProjectLanguage::Go,
        );
        assert_eq!(
            bound_calls(&analyze_with_real_parser(&fixture), "Target"),
            0,
            "{caller}"
        );
    }
    for import in ["import . \"unknown\"", "import Target \"unknown\""] {
        let caller = format!("package sample\n{import}\nfunc Caller(){{Target()}}\n");
        let fixture = ProjectFixture::new(
            ["server/target.go", "server/caller.go"],
            ["package sample\nfunc Target(){}\n", caller.as_str()],
            SemanticProjectLanguage::Go,
        );
        assert_eq!(
            bound_calls(&analyze_with_real_parser(&fixture), "Target"),
            0,
            "{import}"
        );
    }
}

#[derive(Clone, Copy, Debug)]
enum Mutation {
    None,
    MissingPair,
    DuplicatePair,
    ForeignPair,
    ReparentPair,
    ValueAsCall,
    VariableAsFunction,
    MissingDuplicateDefinition,
    ForgeBareSite,
    HeaderPair,
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
        let root = facts
            .iter()
            .find(|fact| fact.kind() == SyntaxFactKind::Root)
            .unwrap()
            .local_id();
        let next = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
        let pair = facts.iter().position(|fact| {
            matches!(
                fact.syntax_kind().as_str(),
                "go.package_function_context.signature" | "go.function_value_context.signature"
            )
        });
        let replace = |fact: &SyntaxFact, label: &str, parent, depth| {
            SyntaxFact::new(
                fact.local_id(),
                parent,
                fact.kind(),
                fact.span(),
                depth,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        match self.mutation {
            Mutation::None => {}
            Mutation::HeaderPair => {
                if let Some(owner) = facts.iter().find(|fact| {
                    fact.syntax_kind().as_str() == "go.function.declaration"
                        && facts.iter().any(|name| {
                            name.parent() == Some(fact.local_id())
                                && name.syntax_kind().as_str() == "go.identifier.definition"
                                && request.source().bytes().get(
                                    usize::try_from(name.span().start_byte()).unwrap()
                                        ..usize::try_from(name.span().end_byte()).unwrap(),
                                ) == Some(b"Caller")
                        })
                }) && let Some(name) = facts
                    .iter()
                    .find(|fact| {
                        fact.parent() == Some(owner.local_id())
                            && fact.syntax_kind().as_str() == "go.identifier.definition"
                    })
                    .cloned()
                {
                    facts.push(SyntaxFact::new(
                        next,
                        name.parent(),
                        SyntaxFactKind::Occurrence,
                        name.span(),
                        name.depth(),
                        SyntaxKindLabel::new("go.package_function_candidate.reference").unwrap(),
                    ));
                    facts.push(SyntaxFact::new(
                        next + 1,
                        name.parent(),
                        SyntaxFactKind::Signature,
                        name.span(),
                        name.depth(),
                        SyntaxKindLabel::new("go.package_function_context.signature").unwrap(),
                    ));
                }
            }
            Mutation::ForgeBareSite => {
                if let Some(index) = facts.iter().position(|fact| {
                    fact.syntax_kind().as_str() == "go.unproven_direct_function.reference"
                }) {
                    let fact = facts[index].clone();
                    facts[index] = replace(
                        &fact,
                        "go.package_function_candidate.reference",
                        fact.parent(),
                        fact.depth(),
                    );
                    facts.push(SyntaxFact::new(
                        next,
                        fact.parent(),
                        SyntaxFactKind::Signature,
                        fact.span(),
                        fact.depth(),
                        SyntaxKindLabel::new("go.package_function_context.signature").unwrap(),
                    ));
                }
            }
            Mutation::MissingPair => {
                if let Some(index) = pair {
                    facts.remove(index);
                }
            }
            Mutation::DuplicatePair => {
                if let Some(index) = pair {
                    let fact = &facts[index];
                    facts.push(SyntaxFact::new(
                        next,
                        fact.parent(),
                        fact.kind(),
                        fact.span(),
                        fact.depth(),
                        fact.syntax_kind().clone(),
                    ));
                }
            }
            Mutation::ForeignPair => {
                if let Some(index) = pair {
                    let fact = &facts[index];
                    facts[index] = replace(
                        fact,
                        "go.foreign_context.signature",
                        fact.parent(),
                        fact.depth(),
                    );
                }
            }
            Mutation::ReparentPair => {
                if let Some(index) = pair {
                    let span = facts[index].span();
                    for fact in &mut facts {
                        if fact.span() == span
                            && (fact.kind() == SyntaxFactKind::Signature
                                || fact.syntax_kind().as_str().ends_with(".reference"))
                        {
                            *fact = replace(fact, fact.syntax_kind().as_str(), Some(root), 1);
                        }
                    }
                }
            }
            Mutation::ValueAsCall => {
                for fact in &mut facts {
                    let label = match fact.syntax_kind().as_str() {
                        "go.function_value.reference" => "go.package_function_candidate.reference",
                        "go.function_value_context.signature" => {
                            "go.package_function_context.signature"
                        }
                        _ => continue,
                    };
                    *fact = replace(fact, label, fact.parent(), fact.depth());
                }
            }
            Mutation::VariableAsFunction => {
                for fact in &mut facts {
                    if fact.syntax_kind().as_str() == "go.variable.declaration" {
                        *fact =
                            replace(fact, "go.function.declaration", fact.parent(), fact.depth());
                    }
                }
            }
            Mutation::MissingDuplicateDefinition => {
                if request
                    .source()
                    .bytes()
                    .windows(b"func Target".len())
                    .filter(|window| *window == b"func Target")
                    .count()
                    == 2
                    && let Some(index) = facts
                        .iter()
                        .position(|fact| fact.syntax_kind().as_str() == "go.identifier.definition")
                {
                    facts.remove(index);
                }
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

fn analyze_mutated(
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
        content_hash(b"real-native-go-baseline-and-explicit-mutation"),
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
fn go_project_site_mutants_cannot_borrow_a_current_package_target() {
    let fixture = ProjectFixture::new(
        ["server/target.go", "server/caller.go"],
        [
            "package sample\nfunc Target(){}\n",
            "package sample\nfunc Caller(){Target()}\n",
        ],
        SemanticProjectLanguage::Go,
    );
    assert_eq!(
        bound_calls(&analyze_mutated(&fixture, Mutation::None), "Target"),
        1
    );
    for mutation in [
        Mutation::MissingPair,
        Mutation::DuplicatePair,
        Mutation::ForeignPair,
        Mutation::ReparentPair,
    ] {
        let output = analyze_mutated(&fixture, mutation);
        assert!(
            output
                .document()
                .entities
                .iter()
                .any(|entity| entity.display_name == "Target")
        );
        assert_eq!(bound_calls(&output, "Target"), 0, "{mutation:?}");
    }
}

#[test]
fn go_project_current_source_rejects_forged_target_and_value_invocation() {
    let fixture = ProjectFixture::new(
        ["server/value.go"],
        ["package sample\nfunc Target(){}\nfunc Caller(){receiver.Accept(Target)}\n"],
        SemanticProjectLanguage::Go,
    );
    let baseline = analyze_mutated(&fixture, Mutation::None);
    assert!(
        baseline
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "go.function_value.reference"
                && site.role == OccurrenceRole::Reference
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert_eq!(bound_calls(&baseline, "Target"), 0);
    assert_eq!(
        bound_calls(&analyze_mutated(&fixture, Mutation::ValueAsCall), "Target"),
        0
    );
    let variable = ProjectFixture::new(
        ["server/target.go", "server/caller.go"],
        [
            "package sample\nvar Target = 1\n",
            "package sample\nfunc Caller(){Target()}\n",
        ],
        SemanticProjectLanguage::Go,
    );
    let forged = analyze_mutated(&variable, Mutation::VariableAsFunction);
    assert!(
        forged
            .document()
            .entities
            .iter()
            .any(|entity| entity.display_name == "Target" && entity.kind == EntityKind::Function)
    );
    assert_eq!(bound_calls(&forged, "Target"), 0);
    let duplicate = ProjectFixture::new(
        ["server/target.go", "server/caller.go"],
        [
            "package sample\nfunc Target(){}\nfunc Target(){}\n",
            "package sample\nfunc Caller(){Target()}\n",
        ],
        SemanticProjectLanguage::Go,
    );
    let withheld = analyze_mutated(&duplicate, Mutation::MissingDuplicateDefinition);
    assert!(
        withheld
            .document()
            .entities
            .iter()
            .any(|entity| entity.display_name == "Target")
    );
    assert_eq!(bound_calls(&withheld, "Target"), 0);
}

#[test]
fn go_project_forged_sites_cannot_make_dot_imports_init_or_blank_callable() {
    for name in ["init", "_"] {
        let source = format!("package sample\nfunc {name}(){{}}\nfunc Caller(){{{name}()}}\n");
        let fixture = ProjectFixture::new(
            ["server/caller.go"],
            [source.as_str()],
            SemanticProjectLanguage::Go,
        );
        assert_eq!(
            bound_calls(&analyze_mutated(&fixture, Mutation::ForgeBareSite), name),
            0,
            "{name}"
        );
    }
    let fixture = ProjectFixture::new(
        ["server/target.go", "server/caller.go"],
        [
            "package sample\nfunc Target(){}\n",
            "package sample\nimport . \"unknown\"\nfunc Caller(){Target()}\n",
        ],
        SemanticProjectLanguage::Go,
    );
    assert_eq!(
        bound_calls(
            &analyze_mutated(&fixture, Mutation::ForgeBareSite),
            "Target"
        ),
        0
    );
}

#[test]
fn go_project_header_pair_cannot_promote_the_written_function_definition() {
    let fixture = ProjectFixture::new(
        ["server/worker.go"],
        ["package sample\nfunc Target(){}\nfunc Caller(){Target()}\n"],
        SemanticProjectLanguage::Go,
    );
    let baseline = analyze_mutated(&fixture, Mutation::None);
    assert_eq!(bound_calls(&baseline, "Target"), 1);
    let mutant = analyze_mutated(&fixture, Mutation::HeaderPair);
    assert_eq!(bound_calls(&mutant, "Target"), 1);
    assert_eq!(bound_calls(&mutant, "Caller"), 0);
    let owner = mutant
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "Caller")
        .unwrap();
    assert!(
        mutant
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol: owner.id })
    );
}
