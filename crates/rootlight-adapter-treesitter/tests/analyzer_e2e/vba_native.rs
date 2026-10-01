//! VBA authored declarations through the production parser and normalized IR.
//! Source-bound identities are distinct from later project and runtime resolution.

use super::*;
use rootlight_ir::{ContainerRef, FactRef};

fn application_proof_output(
    source: &str,
    language: &'static str,
    ir: IrLimits,
    cancellation: &Cancellation,
) -> Result<AnalysisOutput, AdapterError> {
    let provider = Arc::new(provider());
    let case = LanguageCase {
        name: language,
        ..VBA
    };
    let fixture = Fixture::new(case, source.as_bytes());
    let base = limits();
    let budget = AnalysisLimits::new(
        base.max_source_bytes(),
        base.max_syntax_nodes(),
        base.max_syntax_depth(),
        base.max_embedded_ranges(),
        base.max_reported_memory_bytes(),
        base.syntax_stream().clone(),
        base.ir_stream().clone(),
        ir,
    )
    .unwrap();
    let request = request(&fixture.snapshot, &fixture.source, case, &budget);
    execute_analysis(
        &analyzer(&provider, case),
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancellation,
    )
}

fn radix_own_proof_output(source: &str, language: &'static str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let case = LanguageCase {
        name: language,
        ..VBA
    };
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, case, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        provider.as_ref(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        parsed.report().coverage().covered_source_bytes(),
        source.len()
    );
    assert!(parsed.diagnostics().is_empty());
    let output = execute_analysis(
        &analyzer(&provider, case),
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(provider.stats().pooled_parsers, 0);
    assert_eq!(provider.stats().checked_out_parsers, 0);
    assert_eq!(provider.stats().cache.entries, 0);
    output
}

fn assert_radix_ir_source_identity(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
    language: &str,
) {
    let file = &document.files[0];
    let hash = content_hash(source.as_bytes());
    for site in &document.occurrences {
        assert_eq!(site.source.repository(), document.repository);
        assert_eq!(site.source.generation(), document.generation);
        assert_eq!(site.source.span().file(), file.id);
        assert_eq!(site.source.content_hash(), hash);
        assert!(site.source.span().end_byte() <= u64::try_from(source.len()).unwrap());
    }
    let dialect = document
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "visual-basic-dialect-unproven")
        .collect::<Vec<_>>();
    assert_eq!(
        dialect.len(),
        usize::from(language == "visual-basic-classic")
    );
    if let Some(gap) = dialect.first() {
        assert_eq!(gap.source.repository(), document.repository);
        assert_eq!(gap.source.generation(), document.generation);
        assert_eq!(gap.source.span().file(), file.id);
        assert_eq!(gap.source.span().start_byte(), 0);
        assert_eq!(
            gap.source.span().end_byte(),
            u64::try_from(source.len()).unwrap()
        );
        assert_eq!(gap.source.content_hash(), hash);
        assert_eq!(gap.provenance, file.provenance);
    }
}

#[test]
fn vba_radix_atoms_retain_exact_declarations_and_complete_formal_source_proofs() {
    for literal in ["&Hf", "&h0A", "&O7", "&o0007", "&77"] {
        assert_radix_atom_source_proofs(literal, "\n");
    }
}

#[test]
fn vba_integer_suffixes_retain_exact_atoms_and_complete_formal_source_proofs() {
    for literal in ["007", "&Hf", "&h0A", "&O7", "&o0007", "&77"] {
        for suffix in ['%', '&', '^'] {
            for newline in ["\n", "\r\n"] {
                assert_radix_atom_source_proofs(&format!("{literal}{suffix}"), newline);
            }
        }
    }
}

#[test]
fn vba_float_atoms_retain_exact_declarations_and_complete_formal_source_proofs() {
    for literal in [
        "1.5", ".5", "5.", "1E3", "1e+3", "1D-3", "1d+3", "7!", "7#", "7@", "1.5!", "1e-3#",
        "1D+3@",
    ] {
        for newline in ["\n", "\r\n"] {
            assert_radix_atom_source_proofs(literal, newline);
        }
    }
}

fn assert_radix_atom_source_proofs(literal: &str, newline: &str) {
    assert_numeric_initializer_source_proofs(literal, literal, newline);
}

fn assert_numeric_initializer_source_proofs(literal: &str, initializer: &str, newline: &str) {
    let optional = format!("Optional second As Long = {initializer}");
    let atomic = format!("Call Target({literal})");
    let nested = format!("Inner({literal})");
    let expression = format!("Call Target(local + {literal}, {nested} * -{literal})");
    let source = format!(
            "Attribute VB_Name = \"Radix\"\nConst ModuleSeed As Long = {initializer}\nSub Target(ByVal first As Long, {optional})\nEnd Sub\nFunction Inner(ByVal value As Long) As Long\nInner = {literal}\nEnd Function\nSub Run()\nDim local As Long\nConst LocalSeed As Long = {initializer}\nlocal = {literal}\nlocal = LocalSeed\nlocal = ModuleSeed\n{atomic}\n{expression}\nEnd Sub\n"
        ).replace('\n', newline);
    let optional = optional.replace('\n', newline);
    let initializer_source = initializer.replace('\n', newline);
    for language in ["vba", "visual-basic-classic"] {
        let provider = Arc::new(provider());
        let case = LanguageCase {
            name: language,
            ..VBA
        };
        let fixture = Fixture::new(case, source.as_bytes());
        let budget = limits();
        let request = request(&fixture.snapshot, &fixture.source, case, &budget);
        let parsed = rootlight_adapter_sdk::execute_parse(
            provider.as_ref(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_eq!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert_eq!(
            parsed.report().coverage().covered_source_bytes(),
            source.len()
        );
        assert!(parsed.diagnostics().is_empty());
        let output = execute_analysis(
            &analyzer(&provider, case),
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let document = output.document();
        assert_radix_ir_source_identity(document, &source, language);
        let parser = document.files[0].provenance;
        let run = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "Run")
            .unwrap();
        let module = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Radix")
            .unwrap();
        for (clause, name, owner) in [
            (
                format!("ModuleSeed As Long = {initializer}").replace('\n', newline),
                "ModuleSeed",
                module.id,
            ),
            (
                format!("LocalSeed As Long = {initializer}").replace('\n', newline),
                "LocalSeed",
                run.id,
            ),
        ] {
            let start = source.find(&clause).unwrap();
            let constant = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Constant
                        && entity.canonical_name == name
                        && entity.container == Some(ContainerRef::Entity(owner))
                })
                .unwrap();
            let context = constant.evidence.source.as_ref().unwrap();
            assert_eq!(context.repository(), document.repository);
            assert_eq!(context.generation(), document.generation);
            assert_eq!(context.span().file(), document.files[0].id);
            assert_eq!(context.span().start_byte(), u64::try_from(start).unwrap());
            assert_eq!(
                context.span().end_byte(),
                u64::try_from(start + clause.len()).unwrap()
            );
            assert_eq!(context.content_hash(), content_hash(source.as_bytes()));
            assert_eq!(constant.provenance, parser);
            let definition = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target
                            == OccurrenceTarget::Resolved {
                                symbol: constant.id,
                            }
                })
                .unwrap();
            assert_eq!(
                definition.source.span().start_byte(),
                u64::try_from(start).unwrap()
            );
            assert_eq!(
                definition.source.span().end_byte(),
                u64::try_from(start + name.len()).unwrap()
            );
            assert_eq!(definition.evidence.source.as_ref(), Some(context));
            assert_eq!(definition.enclosing, Some(owner));
            assert_eq!(definition.provenance, parser);
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|edge| edge.predicate == RelationPredicate::Contains
                        && edge.subject == RelationEndpoint::Entity(owner)
                        && edge.object == RelationEndpoint::Entity(constant.id)
                        && edge.evidence.source.as_ref() == Some(context)
                        && edge.provenance == parser)
                    .count(),
                1
            );
            let read_start = source.find(&format!("local = {name}")).unwrap() + "local = ".len();
            let read = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == u64::try_from(read_start).unwrap()
                        && site.syntax_kind == "vba.value_identifier.reference"
                })
                .unwrap();
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved {
                    symbol: constant.id
                }
            );
            assert_eq!(read.enclosing, Some(run.id));
            assert_eq!(read.provenance, parser);
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|edge| edge.subject == RelationEndpoint::Occurrence(read.id)
                        && edge.predicate == RelationPredicate::RefersTo
                        && edge.object == RelationEndpoint::Entity(constant.id))
                    .count(),
                1
            );
        }
        for (start, _) in source.match_indices(literal) {
            assert!(
                !document
                    .occurrences
                    .iter()
                    .any(
                        |site| site.source.span().start_byte() >= u64::try_from(start).unwrap()
                            && site.source.span().end_byte()
                                <= u64::try_from(start + literal.len()).unwrap()
                    ),
                "no numeric identifier occurrence: {language}, {literal}"
            );
        }
        for (start, _) in source.match_indices(initializer_source.as_str()) {
            let end = start + initializer_source.len();
            assert!(!document.occurrences.iter().any(|site| {
                site.source.span().start_byte() >= u64::try_from(start).unwrap()
                    && site.source.span().end_byte() <= u64::try_from(end).unwrap()
            }));
        }
        for site in &document.occurrences {
            assert_eq!(site.source.repository(), document.repository);
            assert_eq!(site.source.generation(), document.generation);
            assert_eq!(site.source.span().file(), document.files[0].id);
            assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
        }
        assert_explicit_expression_application_proof(
            document,
            &source,
            &atomic,
            "Target",
            "vba.invoked_identifier.reference",
            &["ByVal first As Long", &optional],
        );
        assert_explicit_expression_application_proof(
            document,
            &source,
            &expression,
            "Target",
            "vba.invoked_identifier.reference",
            &["ByVal first As Long", &optional],
        );
        assert_explicit_expression_application_proof(
            document,
            &source,
            &nested,
            "Inner",
            "vba.invoked_get_identifier.reference",
            &["ByVal value As Long"],
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Calls)
                .count(),
            3
        );
        if language == "vba" {
            assert_eq!(
                output.report().coverage().status(),
                CoverageStatus::Complete
            );
            assert!(document.skipped_regions.is_empty());
        } else {
            assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
            assert_eq!(document.skipped_regions.len(), 1);
            let gap = &document.skipped_regions[0];
            assert_eq!(gap.detail, "visual-basic-dialect-unproven");
            assert_eq!(gap.source, fixture.source);
            assert_eq!(gap.provenance, parser);
        }
        let repeated = execute_analysis(
            &analyzer(&provider, case),
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_eq!(repeated.document(), document);
        assert_eq!(provider.stats().pooled_parsers, 0);
        assert_eq!(provider.stats().checked_out_parsers, 0);
        assert_eq!(provider.stats().cache.entries, 0);
    }
}

#[test]
fn vba_signed_numeric_initializers_keep_source_owned_formals_and_calls() {
    for literal in [
        "007", "7%", "&Hf&", "&o7^", "&77", "1.5", ".5#", "0.#", "1e-3", "1D+3", "7!", "1D+3@",
    ] {
        for trivia in ["", " _\n "] {
            let initializer = format!("-{trivia}{literal}");
            for newline in ["\n", "\r\n"] {
                assert_numeric_initializer_source_proofs(literal, &initializer, newline);
            }
        }
    }
}

#[test]
fn vba_signed_defaults_preserve_written_arity_and_local_shadow_boundaries() {
    let clauses = ["ByVal required As Long", "Optional extra As Long = -1D+3@"];
    for shadow in [false, true] {
        let source = format!(
            "Const Seed As Long = -7\nSub Target({}, {})\nEnd Sub\nSub Run()\n{}Call Target()\nCall Target(1)\nCall Target(1, 2)\nCall Target(1, 2, 3)\nEnd Sub\n",
            clauses[0],
            clauses[1],
            if shadow { "Dim Target As Long\n" } else { "" },
        );
        for newline in ["\n", "\r\n"] {
            let source = source.replace('\n', newline);
            for language in ["vba", "visual-basic-classic"] {
                let output = radix_own_proof_output(&source, language);
                let document = output.document();
                assert_radix_ir_source_identity(document, &source, language);
                let parser = document.files[0].provenance;
                let run = document
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == "Run")
                    .unwrap();
                let target = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.canonical_name == "Target" && entity.kind == EntityKind::Function
                    })
                    .unwrap();
                for (clause, name) in [(clauses[0], "required"), (clauses[1], "extra")] {
                    let start = source.find(clause).unwrap();
                    let formal = document
                        .entities
                        .iter()
                        .find(|entity| {
                            entity.kind == EntityKind::Parameter
                                && entity.canonical_name == name
                                && entity.container == Some(ContainerRef::Entity(target.id))
                        })
                        .unwrap();
                    assert_eq!(formal.provenance, parser);
                    let reference = formal.evidence.source.as_ref().unwrap();
                    assert_eq!(reference.repository(), document.repository);
                    assert_eq!(reference.generation(), document.generation);
                    assert_eq!(reference.span().file(), document.files[0].id);
                    assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                    assert_eq!(reference.span().start_byte(), u64::try_from(start).unwrap());
                    assert_eq!(
                        reference.span().end_byte(),
                        u64::try_from(start + clause.len()).unwrap()
                    );
                    let definitions = document
                        .occurrences
                        .iter()
                        .filter(|site| {
                            site.role == OccurrenceRole::Definition
                                && site.target == OccurrenceTarget::Resolved { symbol: formal.id }
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(definitions.len(), 1);
                    assert_eq!(definitions[0].enclosing, Some(target.id));
                    assert_eq!(definitions[0].provenance, parser);
                    assert_eq!(definitions[0].evidence.source.as_ref(), Some(reference));
                    let name_start = start + clause.find(name).unwrap();
                    assert_eq!(
                        definitions[0].source.span().start_byte(),
                        u64::try_from(name_start).unwrap()
                    );
                    assert_eq!(
                        definitions[0].source.span().end_byte(),
                        u64::try_from(name_start + name.len()).unwrap()
                    );
                    assert_eq!(
                        document
                            .relations
                            .iter()
                            .filter(|edge| {
                                edge.predicate == RelationPredicate::Contains
                                    && edge.subject == RelationEndpoint::Entity(target.id)
                                    && edge.object == RelationEndpoint::Entity(formal.id)
                                    && edge.evidence.source.as_ref() == Some(reference)
                                    && edge.provenance == parser
                            })
                            .count(),
                        1
                    );
                }
                let callee_target = if shadow {
                    document
                        .entities
                        .iter()
                        .find(|entity| {
                            entity.canonical_name == "Target"
                                && entity.kind == EntityKind::Variable
                                && entity.container == Some(ContainerRef::Entity(run.id))
                        })
                        .unwrap()
                } else {
                    target
                };
                for (written, arity_matches) in [
                    ("Call Target()", false),
                    ("Call Target(1)", true),
                    ("Call Target(1, 2)", true),
                    ("Call Target(1, 2, 3)", false),
                ] {
                    let start = source.find(written).unwrap();
                    let application = document
                        .occurrences
                        .iter()
                        .find(|site| {
                            site.syntax_kind == "vba.application.reference"
                                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                                && site.source.span().end_byte()
                                    == u64::try_from(start + written.len()).unwrap()
                        })
                        .unwrap();
                    assert_eq!(application.enclosing, Some(run.id));
                    let proven = arity_matches && !shadow;
                    if proven {
                        assert_explicit_expression_application_proof(
                            document,
                            &source,
                            written,
                            "Target",
                            "vba.invoked_identifier.reference",
                            &clauses,
                        );
                    } else {
                        assert!(matches!(
                            application.target,
                            OccurrenceTarget::Unresolved { .. }
                        ));
                        assert_eq!(application.provenance, parser);
                        assert!(!document.relations.iter().any(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(application.id)
                                && matches!(
                                    edge.predicate,
                                    RelationPredicate::RefersTo | RelationPredicate::Calls
                                )
                        }));
                    }
                    let callee = document
                        .occurrences
                        .iter()
                        .find(|site| {
                            site.syntax_kind == "vba.invoked_identifier.reference"
                                && site.source.span().start_byte()
                                    == u64::try_from(start + "Call ".len()).unwrap()
                                && site.source.span().end_byte()
                                    == u64::try_from(start + "Call Target".len()).unwrap()
                        })
                        .unwrap();
                    assert_eq!(callee.enclosing, Some(run.id));
                    assert_eq!(
                        callee.target,
                        OccurrenceTarget::Resolved {
                            symbol: callee_target.id
                        }
                    );
                    assert_eq!(callee.provenance, parser);
                    assert_eq!(
                        callee.role,
                        if shadow {
                            OccurrenceRole::Reference
                        } else {
                            OccurrenceRole::CallSite
                        }
                    );
                    for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
                        let edges = document
                            .relations
                            .iter()
                            .filter(|edge| {
                                edge.subject == RelationEndpoint::Occurrence(callee.id)
                                    && edge.predicate == predicate
                            })
                            .collect::<Vec<_>>();
                        let expected = if shadow {
                            predicate == RelationPredicate::RefersTo
                        } else {
                            predicate == RelationPredicate::Calls
                        };
                        assert_eq!(edges.len(), usize::from(expected));
                        if expected {
                            assert_eq!(edges[0].object, RelationEndpoint::Entity(callee_target.id));
                            assert_eq!(edges[0].provenance, parser);
                            assert_eq!(edges[0].evidence.source.as_ref(), Some(&callee.source));
                        }
                    }
                }
                assert_eq!(
                    document
                        .relations
                        .iter()
                        .filter(|edge| edge.predicate == RelationPredicate::Calls)
                        .count(),
                    if shadow { 0 } else { 4 }
                );
                for initializer in ["-7", "-1D+3@"] {
                    let start = source.find(initializer).unwrap();
                    assert!(!document.occurrences.iter().any(|site| {
                        site.source.span().start_byte() >= u64::try_from(start).unwrap()
                            && site.source.span().end_byte()
                                <= u64::try_from(start + initializer.len()).unwrap()
                    }));
                }
            }
        }
    }
}

#[test]
fn vba_radix_array_rank_keeps_local_shadow_and_independent_calls_source_bound() {
    for (lower, upper, bound, indices, rank_matches) in [
        ("&H1", "&h2", "&H3", "&H0, &h1", true),
        ("&O1", "&o2", "&O3", "&O0, &o1", true),
        ("&1", "&2", "&3", "&0, &1", true),
        ("&H1", "&O2", "&3", "&H0", false),
    ] {
        assert_radix_array_source_proofs(lower, upper, bound, indices, rank_matches, "\n");
    }
}

#[test]
fn vba_integer_suffix_arrays_preserve_rank_shadow_and_independent_call_proofs() {
    for (lower, upper, bound) in [
        ("1", "2", "3"),
        ("&H1", "&h2", "&H3"),
        ("&O1", "&o2", "&O3"),
        ("&1", "&2", "&3"),
    ] {
        for suffix in ['%', '&', '^'] {
            let lower = format!("{lower}{suffix}");
            let upper = format!("{upper}{suffix}");
            let bound = format!("{bound}{suffix}");
            for rank_matches in [true, false] {
                let indices = if rank_matches {
                    format!("{lower}, {upper}")
                } else {
                    lower.clone()
                };
                for newline in ["\n", "\r\n"] {
                    assert_radix_array_source_proofs(
                        &lower,
                        &upper,
                        &bound,
                        &indices,
                        rank_matches,
                        newline,
                    );
                }
            }
        }
    }
}

#[test]
fn vba_float_array_sources_preserve_rank_shadow_and_independent_call_proofs() {
    for (lower, upper, bound) in [
        ("1.5", "2.5", "3.5"),
        (".1", ".2", ".3"),
        ("1E-1", "2e+1", "3E0"),
        ("1D-1", "2d+1", "3D0"),
        ("1!", "2#", "3@"),
        ("1.5!", "2D+1#", "3e-1@"),
    ] {
        for rank_matches in [true, false] {
            let indices = if rank_matches {
                format!("{lower}, {upper}")
            } else {
                lower.to_owned()
            };
            for newline in ["\n", "\r\n"] {
                assert_radix_array_source_proofs(
                    lower,
                    upper,
                    bound,
                    &indices,
                    rank_matches,
                    newline,
                );
            }
        }
    }
}

fn assert_radix_array_source_proofs(
    lower: &str,
    upper: &str,
    bound: &str,
    indices: &str,
    rank_matches: bool,
    newline: &str,
) {
    let clause = format!("grid(-{lower} To +{upper}, {bound}) As Long");
    let indexed = format!("grid({indices})");
    let ready = format!("Call Ready({bound})");
    let source = format!(
            "Function grid(ByVal index As Long) As Long\nEnd Function\nSub Ready(ByVal value As Long)\nEnd Sub\nSub Run()\nDim result, {clause}\nresult = {indexed}\n{ready}\nEnd Sub\n"
        ).replace('\n', newline);
    for language in ["vba", "visual-basic-classic"] {
        let output = radix_own_proof_output(&source, language);
        let document = output.document();
        assert_radix_ir_source_identity(document, &source, language);
        let run = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "Run")
            .unwrap();
        let array = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Variable
                    && entity.canonical_name == "grid"
                    && entity.container == Some(ContainerRef::Entity(run.id))
            })
            .unwrap();
        let function = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "grid")
            .unwrap();
        let parser = document.files[0].provenance;
        let context = array.evidence.source.as_ref().unwrap();
        let declaration_start = source.find(&clause).unwrap();
        assert_eq!(
            context.span().start_byte(),
            u64::try_from(declaration_start).unwrap()
        );
        assert_eq!(
            context.span().end_byte(),
            u64::try_from(declaration_start + clause.len()).unwrap()
        );
        assert_eq!(context.content_hash(), content_hash(source.as_bytes()));
        assert_eq!(array.provenance, parser);
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(run.id)
                    && edge.object == RelationEndpoint::Entity(array.id)
                    && edge.evidence.source.as_ref() == Some(context))
                .count(),
            1
        );
        let start = source.find(&indexed).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(start + indexed.len()).unwrap()
            })
            .unwrap();
        if rank_matches {
            assert_eq!(
                application.target,
                OccurrenceTarget::Resolved { symbol: array.id }
            );
        } else {
            assert!(matches!(
                application.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
        assert_eq!(application.provenance, parser);
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(application.id)
                    && edge.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), usize::from(rank_matches));
        if rank_matches {
            assert_eq!(edges[0].object, RelationEndpoint::Entity(array.id));
            assert_eq!(edges[0].evidence.source.as_ref(), Some(&application.source));
            assert_eq!(edges[0].provenance, parser);
        }
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .filter(|gap| gap.source == application.source
                    && gap.detail == "vba-call-or-index-target-unavailable")
                .count(),
            usize::from(!rank_matches)
        );
        let callee = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.invoked_get_identifier.reference"
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + "grid".len()).unwrap()
            })
            .unwrap();
        assert_eq!(
            callee.target,
            OccurrenceTarget::Resolved { symbol: array.id }
        );
        assert_eq!(callee.role, OccurrenceRole::Reference);
        assert_eq!(callee.provenance, parser);
        assert!(!document.relations.iter().any(|edge| edge.predicate == RelationPredicate::Calls && matches!(edge.object, RelationEndpoint::Entity(id) if id == array.id || id == function.id)));
        for literal in [lower, upper, bound] {
            for (offset, _) in source.match_indices(literal) {
                assert!(
                    !document
                        .occurrences
                        .iter()
                        .any(|site| site.source.span().start_byte()
                            >= u64::try_from(offset).unwrap()
                            && site.source.span().end_byte()
                                <= u64::try_from(offset + literal.len()).unwrap())
                );
            }
        }
        assert_explicit_expression_application_proof(
            document,
            &source,
            &ready,
            "Ready",
            "vba.invoked_identifier.reference",
            &["ByVal value As Long"],
        );
    }
}

#[test]
fn vba_radix_duplicate_arrays_keep_written_witnesses_and_unresolved_uses() {
    for literal in ["&H2", "&O2", "&2"] {
        assert_radix_duplicate_source_proofs(literal, "\n");
    }
}

#[test]
fn vba_integer_suffix_duplicate_arrays_preserve_written_ambiguity_witnesses() {
    for literal in ["2", "&H2", "&O2", "&2"] {
        for suffix in ['%', '&', '^'] {
            for newline in ["\n", "\r\n"] {
                assert_radix_duplicate_source_proofs(&format!("{literal}{suffix}"), newline);
            }
        }
    }
}

#[test]
fn vba_float_duplicate_arrays_preserve_written_ambiguity_witnesses() {
    for literal in ["2.5", "2E+1", "2D-1", "2!", "2#", "2@"] {
        for newline in ["\n", "\r\n"] {
            assert_radix_duplicate_source_proofs(literal, newline);
        }
    }
}

fn assert_radix_duplicate_source_proofs(literal: &str, newline: &str) {
    let first = format!("stored(-{literal} To +{literal}) As Long");
    let second = format!("[stored]({literal}) As Long");
    let ready = format!("Call Ready({literal})");
    let source = format!(
            "Public stored As Long\nSub Ready(ByVal value As Long)\nEnd Sub\nSub Run()\nDim {first}, {second}, other\nstored = stored\n{ready}\nEnd Sub\n"
        ).replace('\n', newline);
    for language in ["vba", "visual-basic-classic"] {
        let output = radix_own_proof_output(&source, language);
        let document = output.document();
        assert_radix_ir_source_identity(document, &source, language);
        let run = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "Run")
            .unwrap();
        let locals = document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Variable
                    && entity.container == Some(ContainerRef::Entity(run.id))
            })
            .collect::<Vec<_>>();
        assert_eq!(locals.len(), 2);
        let stored = locals
            .iter()
            .find(|entity| entity.canonical_name == "stored")
            .unwrap();
        for (clause, token) in [(&first, "stored"), (&second, "[stored]")] {
            let start = source.find(clause).unwrap();
            let definition = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: stored.id }
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                })
                .unwrap();
            assert_eq!(
                definition.source.span().end_byte(),
                u64::try_from(start + token.len()).unwrap()
            );
            assert_eq!(definition.enclosing, Some(run.id));
            assert_eq!(definition.provenance, document.files[0].provenance);
            let context = definition.evidence.source.as_ref().unwrap();
            assert_eq!(context.span().start_byte(), u64::try_from(start).unwrap());
            assert_eq!(
                context.span().end_byte(),
                u64::try_from(start + clause.len()).unwrap()
            );
            assert_eq!(context.content_hash(), content_hash(source.as_bytes()));
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|edge| edge.subject == RelationEndpoint::Entity(run.id)
                        && edge.object == RelationEndpoint::Entity(stored.id)
                        && edge.predicate == RelationPredicate::Contains
                        && edge.evidence.source.as_ref() == Some(context))
                    .count(),
                1
            );
        }
        let start = source.find("stored = stored").unwrap();
        let uses = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() >= u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        <= u64::try_from(start + "stored = stored".len()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(uses.len(), 2);
        for site in uses {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(!document.relations.iter().any(|edge| edge.subject
                == RelationEndpoint::Occurrence(site.id)
                && matches!(
                    edge.predicate,
                    RelationPredicate::RefersTo | RelationPredicate::Calls
                )));
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source
                        && gap.detail == "vba-binding-target-unavailable")
            );
        }
        assert_explicit_expression_application_proof(
            document,
            &source,
            &ready,
            "Ready",
            "vba.invoked_identifier.reference",
            &["ByVal value As Long"],
        );
        for (offset, _) in source.match_indices(literal) {
            assert!(
                !document.occurrences.iter().any(|site| {
                    site.source.span().start_byte() >= u64::try_from(offset).unwrap()
                        && site.source.span().end_byte()
                            <= u64::try_from(offset + literal.len()).unwrap()
                }),
                "no numeric or suffix type occurrence: {language}, {literal}"
            );
        }
    }
}

#[test]
fn vba_application_proofs_retain_exact_materialized_formals_and_parser_parent() {
    const RULE: &str = "vba-procedure-application-v1.source";
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            for (parameters, arguments, clauses) in [
                ("", "", Vec::<&str>::new()),
                ("ByVal value As Long", "1", vec!["ByVal value As Long"]),
                (
                    "ByVal value As Long, ByRef other As Long",
                    "1, local",
                    vec!["ByVal value As Long", "ByRef other As Long"],
                ),
                (
                    "Optional ByVal value As Long = 7",
                    "",
                    vec!["Optional ByVal value As Long = 7"],
                ),
                (
                    "Optional value As Variant",
                    "",
                    vec!["Optional value As Variant"],
                ),
                (
                    "ByVal value As Long, Optional ByRef other As String = \"a,\"\"b\"\":()\"",
                    "1",
                    vec![
                        "ByVal value As Long",
                        "Optional ByRef other As String = \"a,\"\"b\"\":()\"",
                    ],
                ),
                (
                    "ByVal value As Long, Optional ByRef other As String = \"a,\"\"b\"\":()\"",
                    "1, \"written\"",
                    vec![
                        "ByVal value As Long",
                        "Optional ByRef other As String = \"a,\"\"b\"\":()\"",
                    ],
                ),
                (
                    "Optional first As Boolean = False, Optional second As String = \"text\"",
                    "",
                    vec![
                        "Optional first As Boolean = False",
                        "Optional second As String = \"text\"",
                    ],
                ),
                (
                    "Optional first As Boolean = False, Optional second As String = \"text\"",
                    "True, \"written\"",
                    vec![
                        "Optional first As Boolean = False",
                        "Optional second As String = \"text\"",
                    ],
                ),
                ("value", "1", vec!["value"]),
                ("Rem", "", Vec::<&str>::new()),
                (
                    "Rem[value], Optional other = 7",
                    "1",
                    vec!["[value]", "Optional other = 7"],
                ),
                (
                    "\x0cByVal\x0cvalue\x0cAs\x0cLong\x0c",
                    "1",
                    vec!["ByVal\x0cvalue\x0cAs\x0cLong"],
                ),
                (
                    "ByVal Rem[value], Optional other = 7",
                    "1",
                    vec!["ByVal Rem[value]", "Optional other = 7"],
                ),
                (
                    "ByVal value, ByRef other As Long",
                    "1, local",
                    vec!["ByVal value", "ByRef other As Long"],
                ),
                (
                    "ByVal value As Variant, ByRef other",
                    "1, local",
                    vec!["ByVal value As Variant", "ByRef other"],
                ),
                ("Optional value", "", vec!["Optional value"]),
                (
                    "Optional ByVal value = 7",
                    "",
                    vec!["Optional ByVal value = 7"],
                ),
                (
                    "ByVal value, Optional ByRef other = \"a,\"\"b\"\":()\"",
                    "1",
                    vec!["ByVal value", "Optional ByRef other = \"a,\"\"b\"\":()\""],
                ),
                (
                    "ByVal value, Optional ByRef other = \"a,\"\"b\"\":()\"",
                    "1, \"written\"",
                    vec!["ByVal value", "Optional ByRef other = \"a,\"\"b\"\":()\""],
                ),
                (
                    "Optional first = False, Optional second = \"text\"",
                    "",
                    vec!["Optional first = False", "Optional second = \"text\""],
                ),
                (
                    "Optional first = False, Optional second = \"text\"",
                    "True, \"written\"",
                    vec!["Optional first = False", "Optional second = \"text\""],
                ),
            ] {
                let body = if native {
                    "local = #1/1/2000#\n"
                } else {
                    "local = 2\n"
                };
                let source = format!(
                    "Sub Accept({parameters})\nEnd Sub\nSub Run()\nDim local As Long\n{body}Call Accept({arguments})\nCall Accept({arguments})\nEnd Sub\n"
                );
                let result =
                    application_proof_output(&source, language, IrLimits::default(), &deadline())
                        .unwrap();
                let document = result.document();
                let target = document
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == "Accept")
                    .unwrap();
                let parser = document
                    .provenance
                    .iter()
                    .find(|proof| proof.id == document.files[0].provenance)
                    .unwrap();
                assert_eq!(parser.rule, None);
                assert!(parser.derivation_parents.is_empty());
                let applications: Vec<_> = document.occurrences.iter().filter(|site|
                    site.syntax_kind == "vba.application.reference"
                    && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == target.id)
                ).collect();
                assert_eq!(applications.len(), 2, "{language}: {source}");
                let mut proof_ids = BTreeSet::new();
                for application in applications {
                    let proof = document
                        .provenance
                        .iter()
                        .find(|proof| proof.id == application.provenance)
                        .unwrap();
                    assert!(proof_ids.insert(proof.id));
                    assert_eq!(proof.rule.as_deref(), Some(RULE));
                    assert_eq!(proof.producer_kind, ProducerKind::Parser);
                    assert_eq!(proof.producer, parser.producer);
                    assert_eq!(proof.binary_digest, parser.binary_digest);
                    assert_eq!(proof.frontend_version, parser.frontend_version);
                    assert_eq!(proof.build_context, parser.build_context);
                    assert_eq!(proof.tier, parser.tier);
                    assert_eq!(proof.language, parser.language);
                    assert!(proof.derivation_parents.contains(&FactRef::Fact(parser.id)));
                    assert!(
                        proof
                            .derivation_parents
                            .contains(&FactRef::Entity(target.id))
                    );
                    assert!(proof.input_sources.contains(&parser.input_sources[0]));
                    assert!(proof.evidence_sources.contains(&application.source));
                    assert_eq!(proof.derivation_parents.len(), 2 + clauses.len());
                    for clause in &clauses {
                        let start = u64::try_from(source.find(clause).unwrap()).unwrap();
                        let end = start + u64::try_from(clause.len()).unwrap();
                        let formal = document
                            .entities
                            .iter()
                            .find(|entity| {
                                entity.kind == EntityKind::Parameter
                                    && entity.container == Some(ContainerRef::Entity(target.id))
                                    && entity.evidence.source.as_ref().is_some_and(|reference| {
                                        reference.span().start_byte() == start
                                            && reference.span().end_byte() == end
                                    })
                            })
                            .unwrap();
                        assert!(
                            proof
                                .derivation_parents
                                .contains(&FactRef::Entity(formal.id))
                        );
                        let clause_source = formal.evidence.source.as_ref().unwrap();
                        assert!(proof.input_sources.contains(clause_source));
                        assert!(proof.evidence_sources.contains(clause_source));
                        let definition = document.occurrences.iter().find(|site| site.role == OccurrenceRole::Definition
                            && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == formal.id)).unwrap();
                        assert!(proof.evidence_sources.contains(&definition.source));
                    }
                    for reference in proof.input_sources.iter().chain(&proof.evidence_sources) {
                        assert_eq!(reference.repository(), application.source.repository());
                        assert_eq!(reference.generation(), application.source.generation());
                        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                        assert_eq!(reference.span().file(), application.source.span().file());
                    }
                    let relation = document
                        .relations
                        .iter()
                        .find(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(application.id)
                                && edge.predicate == RelationPredicate::RefersTo
                                && edge.object == RelationEndpoint::Entity(target.id)
                        })
                        .unwrap();
                    assert_eq!(relation.provenance, proof.id);
                    assert!(!document.relations.iter().any(|edge| edge.subject
                        == RelationEndpoint::Occurrence(application.id)
                        && edge.predicate == RelationPredicate::Calls));
                }
                let calls: Vec<_> = document.occurrences.iter().filter(|site| site.role == OccurrenceRole::CallSite
                    && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == target.id)).collect();
                assert_eq!(calls.len(), 2);
                assert!(calls.iter().all(|site| site.provenance == parser.id));
                assert_eq!(document.provenance.len(), 3);
                let coverage = result
                    .report()
                    .coverage()
                    .domains()
                    .iter()
                    .find(|entry| entry.domain() == FactDomain::Provenance)
                    .unwrap();
                assert_eq!(coverage.discovered(), 3);
                assert_eq!(coverage.indexed(), 3);
                assert_eq!(coverage.skipped(), 0);
            }
        }
    }
}

#[test]
fn vba_application_proofs_do_not_attribute_wrong_arity_arrays_or_other_references() {
    for language in ["vba", "visual-basic-classic"] {
        for invocation in ["Call Accept()", "Call Accept(1)", "Call Accept(1, 2, 3)"] {
            let source = format!(
                "Sub Accept(ByVal first As Long, ByVal second As Long)\nEnd Sub\nSub Run()\n{invocation}\nEnd Sub\n"
            );
            let result =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            assert_eq!(result.document().provenance.len(), 1);
            let application = result
                .document()
                .occurrences
                .iter()
                .find(|site| site.syntax_kind == "vba.application.reference")
                .unwrap();
            assert!(matches!(
                application.target,
                OccurrenceTarget::Unresolved { .. }
            ));
            assert_eq!(
                application.provenance,
                result.document().files[0].provenance
            );
        }
        for source in [
            "Sub Accept(ByVal value As Long, ByVal VALUE As Long)\nEnd Sub\nSub Run()\nCall Accept(1, 2)\nEnd Sub\n",
            "Sub Run()\nDim values(0) As Long\nDim value As Long\nvalue = values(0)\nEnd Sub\n",
            "Sub Accept(ByVal value As Long)\nEnd Sub\nSub Run()\nDim value As Long\nvalue = 1\nEnd Sub\n",
        ] {
            let result =
                application_proof_output(source, language, IrLimits::default(), &deadline())
                    .unwrap();
            assert_eq!(result.document().provenance.len(), 1, "{source}");
            assert!(
                result
                    .document()
                    .occurrences
                    .iter()
                    .all(|site| site.provenance == result.document().files[0].provenance)
            );
        }
    }
}

#[test]
fn vba_application_proofs_obey_provenance_nested_and_cancellation_limits() {
    use rootlight_adapter_sdk::{ResourceKind, SinkError};
    use rootlight_cancel::CancellationReason;
    let source = "Sub Accept(ByVal first As Long, ByVal second As Long)\nEnd Sub\nSub Run()\nCall Accept(1, 2)\nCall Accept(3, 4)\nEnd Sub\n";
    for language in ["vba", "visual-basic-classic"] {
        let mut exact = IrLimits::default();
        exact.max_provenance_records = 3;
        let result = application_proof_output(source, language, exact, &deadline()).unwrap();
        assert_eq!(result.document().provenance.len(), 3);
        for maximum in [1, 2] {
            let mut short = IrLimits::default();
            short.max_provenance_records = maximum;
            assert!(
                matches!(application_proof_output(source, language, short, &deadline()),
                Err(AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::Records, observed, limit }))
                if observed == maximum + 1 && limit == maximum)
            );
        }
        let mut short = IrLimits::default();
        short.max_nested_items_per_record = 5;
        assert!(matches!(
            application_proof_output(source, language, short, &deadline()),
            Err(AdapterError::Sink(SinkError::StreamLimit {
                resource: ResourceKind::NestedItems,
                observed: 6,
                limit: 5
            }))
        ));
        let cancelled = deadline();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            application_proof_output(source, language, IrLimits::default(), &cancelled),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }
}

#[test]
fn vba_application_proofs_preserve_the_matched_getter_after_accessor_merging() {
    let source = "Property Let Item(ByVal index As Long, ByVal value As Long)\nEnd Property\nProperty Get Item(ByVal index As Long) As Long\nItem = index\nEnd Property\nSub Run()\nDim result As Long\nresult = Item(1)\nEnd Sub\n";
    for language in ["vba", "visual-basic-classic"] {
        let result =
            application_proof_output(source, language, IrLimits::default(), &deadline()).unwrap();
        let document = result.document();
        let start = u64::try_from(source.rfind("Item(1)").unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = application.target else {
            panic!("complete getter application must resolve")
        };
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == application.provenance)
            .unwrap();
        assert_eq!(
            proof.rule.as_deref(),
            Some("vba-procedure-application-v1.source")
        );
        assert!(proof.derivation_parents.contains(&FactRef::Entity(symbol)));
        let getter_start = u64::try_from(source.find("Property Get").unwrap()).unwrap();
        let getter_end = getter_start
            + u64::try_from(
                source[source.find("Property Get").unwrap()..]
                    .find("End Property")
                    .unwrap()
                    + "End Property".len(),
            )
            .unwrap();
        assert!(
            proof
                .evidence_sources
                .iter()
                .any(|reference| reference.span().start_byte() == getter_start
                    && reference.span().end_byte() == getter_end)
        );
        let clause_start = u64::try_from(source.rfind("ByVal index As Long").unwrap()).unwrap();
        let clause_end = clause_start + u64::try_from("ByVal index As Long".len()).unwrap();
        let clause = proof
            .input_sources
            .iter()
            .find(|reference| {
                reference.span().start_byte() == clause_start
                    && reference.span().end_byte() == clause_end
            })
            .unwrap();
        assert!(proof.evidence_sources.contains(clause));
        assert!(
            !proof
                .input_sources
                .iter()
                .any(|reference| reference.span().start_byte()
                    == u64::try_from(source.find("ByVal value").unwrap()).unwrap())
        );
    }
}

pub(super) const VBA: LanguageCase = LanguageCase {
    name: "vba",
    path: "src/Measure.bas",
    frontend: "tree-sitter-vba-0.14.1",
    source: "Public Function Measure(ByVal value As Long) As Long\nMeasure = value + 1\nEnd Function\n",
    generated: false,
    body_before: "value + 1",
    body_after: "value + 2",
};

fn output(source: &str) -> AnalysisOutput {
    output_with_native_kind(source, None)
}

fn output_with_native_kind(
    source: &str,
    kind: Option<rootlight_adapter_sdk::NativeVbaModuleKind>,
) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(VBA, source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, VBA, &budget);
    let request = match kind {
        Some(kind) => request.with_native_vba_module(kind).unwrap(),
        None => request,
    };
    let result = analyze(
        &analyzer(&provider, VBA),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    result
}

#[test]
fn vba_compilation_constants_use_a_separate_module_namespace() {
    use rootlight_ir::{
        VBA_COMPILATION_NAMESPACE, VbaCompilationActivity, VbaCompilationBranch,
        decode_vba_compilation_envelope, new_vba_compilation_envelope,
    };

    for (initializer, selected) in [("True", 0), ("False", 1)] {
        let source = format!(
            "Attribute VB_Name = \"Options\"\n#Const Enabled = {initializer}\nConst Enabled = 7\nSub Run(ByVal Enabled As Long)\n#If eNaBlEd Then\nDim included As Long\nincluded = Enabled\n#Else\nDim excluded As Long\nexcluded = Enabled\n#End If\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let envelopes: Vec<_> = document
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == VBA_COMPILATION_NAMESPACE)
            .collect();
        assert_eq!(envelopes.len(), 1);
        let envelope = envelopes[0];
        let inventory = decode_vba_compilation_envelope(envelope).unwrap();
        let module_source = envelope.evidence.source.as_ref().unwrap();
        assert_eq!(module_source.span().start_byte(), 0);
        assert_eq!(
            module_source.span().end_byte(),
            u64::try_from(source.len()).unwrap()
        );
        assert_eq!(
            module_source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(inventory.scope, module_source.span());
        assert!(inventory.complete);
        assert_eq!(
            new_vba_compilation_envelope(
                envelope.repository,
                envelope.generation,
                envelope.provenance,
                module_source.clone(),
                inventory.clone()
            )
            .unwrap(),
            *envelope
        );
        let parser = document
            .provenance
            .iter()
            .find(|parser| parser.id == envelope.provenance)
            .unwrap();
        assert_eq!(parser.producer_kind, ProducerKind::Parser);
        assert_eq!(
            parser.input_sources.as_slice(),
            std::slice::from_ref(module_source)
        );
        let source_at = |needle: &str| {
            let start = u64::try_from(source.find(needle).unwrap()).unwrap();
            SourceRef::new(
                module_source.repository(),
                module_source.generation(),
                SourceSpan::new(
                    module_source.span().file(),
                    start,
                    start + u64::try_from(needle.len()).unwrap(),
                )
                .unwrap(),
                module_source.content_hash(),
                None,
            )
        };
        let group = source_at("#If eNaBlEd Then\nDim included As Long\nincluded = Enabled\n#Else\nDim excluded As Long\nexcluded = Enabled\n#End If").span();
        let expected: Vec<_> = [
            "Dim included As Long\nincluded = Enabled\n",
            "Dim excluded As Long\nexcluded = Enabled\n",
        ]
        .into_iter()
        .enumerate()
        .map(|(index, body)| VbaCompilationBranch {
            group,
            region: source_at(body).span(),
            activity: if index == selected {
                VbaCompilationActivity::Active
            } else {
                VbaCompilationActivity::Inactive
            },
        })
        .collect();
        assert_eq!(inventory.branches, expected);
        assert!(
            document
                .skipped_regions
                .iter()
                .all(|gap| gap.detail != "vba-conditional-compilation-evaluation-unavailable")
        );

        let compiler_name = source_at(&format!("#Const Enabled = {initializer}"));
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == compiler_name.span().start_byte() + 7
            })
            .unwrap();
        assert_eq!(
            definition.source.span().end_byte(),
            definition.source.span().start_byte() + 7
        );
        let OccurrenceTarget::Resolved {
            symbol: compiler_symbol,
        } = definition.target
        else {
            panic!("compiler constant definition is retained")
        };
        let compiler = document
            .entities
            .iter()
            .find(|entity| entity.id == compiler_symbol)
            .unwrap();
        assert_eq!(compiler.kind, EntityKind::Constant);
        assert_eq!(compiler.evidence.source.as_ref(), Some(&compiler_name));
        let condition_source = source_at("eNaBlEd");
        let condition = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.compilation_identifier.reference"
                    && site.source == condition_source
            })
            .unwrap();
        assert_eq!(condition.target, definition.target);
        assert_eq!(
            inventory.activity_at(condition.source.span()),
            VbaCompilationActivity::Unproven
        );
        assert!(document.relations.iter().any(|edge| edge.subject
            == RelationEndpoint::Occurrence(condition.id)
            && edge.predicate == RelationPredicate::RefersTo
            && edge.object == RelationEndpoint::Entity(compiler_symbol)
            && edge.evidence.source.as_ref() == Some(&condition.source)));
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter && entity.canonical_name == "Enabled"
            })
            .unwrap();
        assert_eq!(
            parameter.evidence.source.as_ref(),
            Some(&source_at("ByVal Enabled As Long"))
        );
        let runtime_constant = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Constant
                    && entity.evidence.source.as_ref() == Some(&source_at("Enabled = 7"))
            })
            .unwrap();
        assert_ne!(runtime_constant.id, compiler_symbol);
        assert_ne!(parameter.id, compiler_symbol);
        assert_ne!(parameter.id, runtime_constant.id);
        for (index, name, marker) in [
            (0, "included", "Enabled\n#Else"),
            (1, "excluded", "Enabled\n#End"),
        ] {
            let variable = document
                .entities
                .iter()
                .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == name)
                .unwrap();
            assert_eq!(
                variable.evidence.source.as_ref(),
                Some(&source_at(&format!("{name} As Long")))
            );
            let variable_definition = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source.span().start_byte()
                            == source_at(&format!("{name} As Long")).span().start_byte()
                })
                .unwrap();
            assert_eq!(
                variable_definition.target,
                OccurrenceTarget::Resolved {
                    symbol: variable.id
                }
            );
            for (site, symbol) in [
                (
                    reference_at(document, &source, &format!("{name} =")),
                    variable.id,
                ),
                (reference_at(document, &source, marker), parameter.id),
            ] {
                let edges: Vec<_> = document
                    .relations
                    .iter()
                    .filter(|edge| {
                        edge.subject == RelationEndpoint::Occurrence(site.id)
                            && edge.predicate == RelationPredicate::RefersTo
                    })
                    .collect();
                if index == selected {
                    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
                    assert_eq!(edges.len(), 1);
                    assert_eq!(edges[0].object, RelationEndpoint::Entity(symbol));
                    assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
                    assert_eq!(
                        inventory.activity_at(site.source.span()),
                        VbaCompilationActivity::Active
                    );
                } else {
                    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
                    assert!(edges.is_empty());
                    assert_eq!(
                        inventory.activity_at(site.source.span()),
                        VbaCompilationActivity::Inactive
                    );
                }
            }
        }
    }
}

#[test]
fn vba_compilation_constants_retain_all_sources_and_select_only_closed_branches() {
    use rootlight_ir::{
        VBA_COMPILATION_NAMESPACE,
        VbaCompilationActivity::{Active, Inactive, Unproven},
        VbaCompilationBranch, decode_vba_compilation_envelope, new_vba_compilation_envelope,
    };

    const EMPTY_GROUP: &str = "#If ENABLED Then\n#End If";
    const SPLIT: &str = "#If ENABLED Then\nSub Run(ByVal value As Long)\n#Else\nSub Run(ByVal value As Integer)\n#End If\nEnd Sub";
    const UNKNOWN_SPLIT: &str = "#If Unknown Then\nSub Run(ByVal value As Long)\n#ElseIf ENABLED Then\nSub Run(ByVal value As Integer)\n#End If\nEnd Sub";
    const UNKNOWN_GROUP: &str = "#If Unknown Then\n#Const Enabled = True\n#End If";
    const LATE_UNKNOWN: &str = "#If ENABLED Then\n#ElseIf Unknown Then\n#End If";
    for (source, complete, branches, warnings) in [
        (
            "#Const Enabled = True\n#If ENABLED Then\n#End If\n",
            true,
            vec![(EMPTY_GROUP, "#End If", true, Active)],
            vec![],
        ),
        (
            "#If ENABLED Then\n#End If\n#Const Enabled = True\n",
            true,
            vec![(EMPTY_GROUP, "#End If", true, Unproven)],
            vec![EMPTY_GROUP],
        ),
        (
            "#If Unknown Then\n#Const Enabled = True\n#End If\n#If ENABLED Then\n#End If\n",
            true,
            vec![
                (UNKNOWN_GROUP, "#Const Enabled = True\n", false, Unproven),
                (EMPTY_GROUP, "#End If", true, Active),
            ],
            vec![UNKNOWN_GROUP],
        ),
        (
            "Sub Setup()\n#Const Enabled = True\nEnd Sub\n#If ENABLED Then\n#End If\n",
            true,
            vec![(EMPTY_GROUP, "#End If", true, Active)],
            vec![],
        ),
        (
            "#Const Enabled% = True\n#If ENABLED Then\n#End If\n",
            true,
            vec![(EMPTY_GROUP, "#End If", true, Active)],
            vec![],
        ),
        (
            "#Const Enabled = True\n#Const Other = ENABLED\n",
            true,
            vec![],
            vec!["#Const Enabled = True", "#Const Other = ENABLED"],
        ),
        (
            "#Const Enabled = True\n#If ENABLED Then\nSub Run(ByVal value As Long)\n#Else\nSub Run(ByVal value As Integer)\n#End If\nEnd Sub\n",
            false,
            vec![],
            vec![SPLIT],
        ),
        (
            "#Const Enabled = True\n#If Unknown Then\nSub Run(ByVal value As Long)\n#ElseIf ENABLED Then\nSub Run(ByVal value As Integer)\n#End If\nEnd Sub\n",
            false,
            vec![],
            vec![UNKNOWN_SPLIT],
        ),
        (
            "#Const Enabled = False\n#If ENABLED Then\n#End If\n",
            true,
            vec![(EMPTY_GROUP, "#End If", true, Inactive)],
            vec![],
        ),
        (
            "#Const Enabled = HostChoice\n#If ENABLED Then\n#End If\n",
            true,
            vec![(EMPTY_GROUP, "#End If", true, Unproven)],
            vec!["#Const Enabled = HostChoice", EMPTY_GROUP],
        ),
        (
            "#Const Enabled = 1\n#If ENABLED Then\n#End If\n",
            true,
            vec![(EMPTY_GROUP, "#End If", true, Unproven)],
            vec!["#Const Enabled = 1", EMPTY_GROUP],
        ),
        (
            "#Const Enabled = True\n#If ENABLED Then\n#ElseIf Unknown Then\n#End If\n",
            true,
            vec![
                (LATE_UNKNOWN, "#ElseIf", true, Unproven),
                (LATE_UNKNOWN, "#End If", true, Unproven),
            ],
            vec![LATE_UNKNOWN],
        ),
    ] {
        let result = output(source);
        let document = result.document();
        let envelopes: Vec<_> = document
            .extensions
            .iter()
            .filter(|envelope| envelope.namespace == VBA_COMPILATION_NAMESPACE)
            .collect();
        assert_eq!(envelopes.len(), 1, "{source}");
        let envelope = envelopes[0];
        let inventory = decode_vba_compilation_envelope(envelope).unwrap();
        let module_source = envelope.evidence.source.as_ref().unwrap();
        assert_eq!(module_source.span().start_byte(), 0);
        assert_eq!(
            module_source.span().end_byte(),
            u64::try_from(source.len()).unwrap()
        );
        assert_eq!(
            module_source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(inventory.scope, module_source.span());
        assert_eq!(inventory.complete, complete, "{source}");
        assert_eq!(
            new_vba_compilation_envelope(
                envelope.repository,
                envelope.generation,
                envelope.provenance,
                module_source.clone(),
                inventory.clone()
            )
            .unwrap(),
            *envelope
        );
        let parser = document
            .provenance
            .iter()
            .find(|parser| parser.id == envelope.provenance)
            .unwrap();
        assert_eq!(parser.producer_kind, ProducerKind::Parser);
        assert_eq!(
            parser.input_sources.as_slice(),
            std::slice::from_ref(module_source)
        );
        let source_range = |start: usize, length: usize| {
            SourceRef::new(
                module_source.repository(),
                module_source.generation(),
                SourceSpan::new(
                    module_source.span().file(),
                    u64::try_from(start).unwrap(),
                    u64::try_from(start + length).unwrap(),
                )
                .unwrap(),
                module_source.content_hash(),
                None,
            )
        };
        let source_at = |needle: &str| source_range(source.find(needle).unwrap(), needle.len());
        let expected: Vec<_> = branches
            .into_iter()
            .map(|(group, region, empty, activity)| {
                let start = source.find(group).unwrap() + group.find(region).unwrap();
                VbaCompilationBranch {
                    group: source_at(group).span(),
                    region: source_range(start, if empty { 0 } else { region.len() }).span(),
                    activity,
                }
            })
            .collect();
        assert_eq!(inventory.branches, expected, "{source}");
        let mut actual_warnings: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "vba-conditional-compilation-evaluation-unavailable")
            .map(|gap| {
                assert_eq!(gap.domain, FactDomain::Relations);
                assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
                gap.source.clone()
            })
            .collect();
        let mut expected_warnings: Vec<_> = warnings.into_iter().map(source_at).collect();
        actual_warnings.sort();
        expected_warnings.sort();
        assert_eq!(actual_warnings, expected_warnings, "{source}");
        let mut offset = 0;
        let mut compiler_symbols = BTreeSet::new();
        for line in source.lines() {
            if let Some(header) = line.strip_prefix("#Const ") {
                let spelling = header.split_whitespace().next().unwrap();
                let name = spelling.trim_end_matches('%');
                let definition_source = source_range(offset + 7, spelling.len());
                let definitions: Vec<_> = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Definition && site.source == definition_source
                    })
                    .collect();
                assert_eq!(definitions.len(), 1, "{line}");
                let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
                    panic!("compiler constant declaration retains its identity: {line}")
                };
                assert!(compiler_symbols.insert(symbol));
                let entity = document
                    .entities
                    .iter()
                    .find(|entity| entity.id == symbol)
                    .unwrap();
                assert_eq!(entity.kind, EntityKind::Constant);
                assert_eq!(entity.canonical_name, name);
                assert_eq!(entity.visibility, rootlight_ir::EntityVisibility::Private);
                assert_eq!(
                    entity.evidence.source.as_ref(),
                    Some(&source_range(offset, line.len()))
                );
                if let Some(ContainerRef::Entity(owner)) = entity.container {
                    assert!(
                        document
                            .entities
                            .iter()
                            .any(|entity| entity.id == owner && entity.kind == EntityKind::Module)
                    );
                }
            }
            offset += line.len() + 1;
        }
        assert_eq!(
            compiler_symbols.len(),
            source
                .lines()
                .filter(|line| line.starts_with("#Const "))
                .count()
        );
        let reference = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.compilation_identifier.reference"
                    && site.source.span().start_byte()
                        == u64::try_from(source.find("ENABLED").unwrap()).unwrap()
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = reference.target else {
            panic!("{source}")
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::Constant);
        assert_eq!(entity.visibility, rootlight_ir::EntityVisibility::Private);
        assert!(compiler_symbols.contains(&symbol));
        assert_eq!(
            reference.source,
            source_range(source.find("ENABLED").unwrap(), "ENABLED".len())
        );
        if let Some(ContainerRef::Entity(owner)) = entity.container {
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.id == owner && entity.kind == EntityKind::Module)
            );
        }
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(reference.id)
                && edge.object == RelationEndpoint::Entity(symbol)
                && edge.evidence.source.as_ref() == Some(&reference.source)
        }));
    }
}

#[test]
fn vba_compilation_constants_do_not_bind_runtime_or_duplicate_names() {
    for source in [
        "Const Enabled = True\n#If ENABLED Then\n#End If\n",
        "#If ENABLED Then\n#End If\n",
        "#Const Enabled = True\n#Const enabled = False\n#If ENABLED Then\n#End If\n",
        "#If Unknown Then\n#Const Enabled = True\n#Else\n#Const Enabled = False\n#End If\n#If ENABLED Then\n#End If\n",
    ] {
        let result = output(source);
        let reference = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.compilation_identifier.reference"
                    && site.source.span().start_byte()
                        == u64::try_from(source.find("ENABLED").unwrap()).unwrap()
            })
            .unwrap();
        assert!(
            matches!(reference.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.source == reference.source
                && gap.detail == "vba-compilation-constant-target-unavailable"
        }));
    }
    let source =
        "#Const Enabled = True\nSub Run()\nDim output As Long\noutput = Enabled\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        reference_at(result.document(), source, "Enabled\nEnd").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_procedure_pointers_bind_without_calling_or_reading_locals() {
    // MS-VBAL 5.6.10 skips the procedure-local tier in pointer context.
    for (declaration, body, target_name) in [
        (
            "Private Sub Notify(ByVal value As Long)\nEnd Sub\n",
            "Dim Notify As Long\nAccept AddressOf nOtIfY\n",
            "Notify",
        ),
        (
            "Private Function Notify(ByVal value As Long) As Long\nAccept AddressOf [Notify]\nEnd Function\n",
            "",
            "Notify",
        ),
        (
            "Private Property Get Notify(ByVal value As Long) As Long\nEnd Property\nPrivate Property Let Notify(ByVal value As Long, ByVal nextValue As Long)\nEnd Property\n",
            "Accept AddressOf Notify\n",
            "Notify",
        ),
    ] {
        let source = format!("{declaration}Sub Run()\n{body}End Sub\n");
        let result = output(&source);
        let document = result.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.procedure_pointer.reference")
            .expect("native procedure pointer occurrence");
        let target = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == target_name
                    && !matches!(entity.kind, rootlight_ir::EntityKind::Variable)
                    && document.occurrences.iter().any(|definition| {
                        definition.role == OccurrenceRole::Definition
                            && definition.source.span().start_byte()
                                == u64::try_from(source.find(target_name).unwrap()).unwrap()
                            && definition.target == OccurrenceTarget::Resolved { symbol: entity.id }
                    })
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{source}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == rootlight_ir::RelationPredicate::RefersTo
                && edge.object == rootlight_ir::RelationEndpoint::Entity(target.id)
                && edge.evidence.source.as_ref() == Some(&site.source)
        }));
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == rootlight_ir::RelationPredicate::Calls
                && edge.evidence.source.as_ref() == Some(&site.source)
        }));
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.source == site.source && gap.detail == "vba-procedure-pointer-target-unavailable"
        }));
    }
}

#[test]
fn vba_procedure_pointers_preserve_unproven_targets() {
    for declaration in [
        "Dim Notify As Long\n",
        "Declare Sub Notify Lib \"callbacks\" ()\n",
        "Private Property Let Notify(ByVal value As Long)\nEnd Property\n",
        "Sub Notify()\nEnd Sub\nSub Notify()\nEnd Sub\n",
        "#If FEATURE Then\nSub Notify()\nEnd Sub\n#End If\n",
        "VERSION 1.0 CLASS\nAttribute VB_Name = \"Listener\"\nSub Notify()\nEnd Sub\n",
        "",
    ] {
        let source = format!("{declaration}Sub Run()\nAccept AddressOf Notify\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.procedure_pointer.reference")
            .expect("native procedure pointer occurrence");
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.source == site.source
                    && gap.detail == "vba-procedure-pointer-target-unavailable"
            }),
            "{source}"
        );
    }
}

#[test]
fn vba_procedure_pointers_do_not_bind_partial_or_type_suffixed_names() {
    for expression in [
        "Notify.Other",
        "Notify .Other",
        "Notify _\n.Other",
        "Notify$",
        "Notify!Other",
    ] {
        let source =
            format!("Sub Notify()\nEnd Sub\nSub Run()\nAccept AddressOf {expression}\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                matches!(
                    site.syntax_kind.as_str(),
                    "vba.procedure_pointer.reference"
                        | "vba.qualified_procedure_pointer.reference"
                        | "vba.unsupported_procedure_pointer.reference"
                )
            })
            .expect("native pointer target retained");
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{expression}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.source == site.source
                    && gap.detail == "vba-procedure-pointer-target-unavailable"
            }),
            "{expression}"
        );
    }
}

#[test]
fn vba_qualified_procedure_pointers_preserve_exact_targets_and_argument_forms() {
    for target in [
        "Callbacks.Notify",
        "[Callbacks].[Notify]",
        "cAlLbAcKs.nOtIfY",
        "Callbacks _\n.Notify",
        "Callbacks. _\r\nNotify",
    ] {
        for call in [
            format!("Accept AddressOf {target}"),
            format!("Call Accept(AddressOf {target})"),
            format!("value = Accept(AddressOf {target})"),
        ] {
            let source = format!(
                "Attribute VB_Name = \"Callbacks\"\nSub Notify()\nEnd Sub\nSub Run()\nDim Callbacks As Long\n{call}\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let site = document
                .occurrences
                .iter()
                .find(|site| site.syntax_kind == "vba.qualified_procedure_pointer.reference")
                .unwrap_or_else(|| panic!("complete target absent: {call}"));
            let symbol = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Notify")
                .unwrap()
                .id;
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol }, "{call}");
            assert_eq!(site.role, OccurrenceRole::Reference);
            let start = usize::try_from(site.source.span().start_byte()).unwrap();
            let end = usize::try_from(site.source.span().end_byte()).unwrap();
            assert_eq!(source.get(start..end), Some(target));
            assert!(document.relations.iter().any(|edge| {
                edge.predicate == rootlight_ir::RelationPredicate::RefersTo
                    && edge.evidence.source.as_ref() == Some(&site.source)
                    && edge.object == rootlight_ir::RelationEndpoint::Entity(symbol)
            }));
            assert!(
                !document.occurrences.iter().any(|other| {
                    other.id != site.id
                        && other.source.span().start_byte() >= site.source.span().start_byte()
                        && other.source.span().end_byte() <= site.source.span().end_byte()
                }),
                "counterfeit target fragment: {call}"
            );
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source
                        && gap.detail == "vba-procedure-pointer-target-unavailable")
            );
        }
    }
}

#[test]
fn vba_pointer_normalization_preserves_real_argument_calls() {
    let source = "Attribute VB_Name = \"Callbacks\"\nSub Notify()\nEnd Sub\nFunction ReadValue() As Long\nEnd Function\nSub Run()\nAccept AddressOf Callbacks.Notify, ReadValue()\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let symbol = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "ReadValue")
        .unwrap()
        .id;
    let start = u64::try_from(source.rfind("ReadValue()").unwrap()).unwrap();
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() == start
                && site.target == OccurrenceTarget::Resolved { symbol }
                && site.role == OccurrenceRole::CallSite)
    );
    let pointer = document
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.qualified_procedure_pointer.reference")
        .unwrap();
    assert!(matches!(pointer.target, OccurrenceTarget::Resolved { .. }));
    assert!(
        !document
            .occurrences
            .iter()
            .any(
                |site| site.source.span().start_byte() > pointer.source.span().start_byte()
                    && site.source.span().start_byte() < pointer.source.span().end_byte()
            )
    );
}

#[test]
fn vba_qualified_procedure_pointers_preserve_namespace_and_target_barriers() {
    for (header, declaration, target) in [
        ("", "Sub Notify()\nEnd Sub\n", "Callbacks.Notify"),
        (
            "Attribute VB_Name = \"Other\"\n",
            "Sub Notify()\nEnd Sub\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "Dim Notify As Long\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "Declare Sub Notify Lib \"native\" ()\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "Property Get Notify() As Long\nEnd Property\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "Sub Notify()\nEnd Sub\nSub Callbacks()\nEnd Sub\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "#If Enabled Then\nSub Notify()\nEnd Sub\n#End If\n",
            "Callbacks.Notify",
        ),
        (
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Callbacks\"\n",
            "Sub Notify()\nEnd Sub\n",
            "Callbacks.Notify",
        ),
        (
            "Attribute VB_Name = \"Callbacks\"\n",
            "Sub Notify()\nEnd Sub\n",
            "Project.Callbacks.Notify",
        ),
    ] {
        let source =
            format!("{header}{declaration}Sub Run()\nCall Accept(AddressOf {target})\nEnd Sub\n");
        let result = output(&source);
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.qualified_procedure_pointer.reference")
            .expect("whole qualified target retained");
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source
                    && gap.detail == "vba-procedure-pointer-target-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn vba_pointer_recovery_never_hides_unrelated_or_real_expression_errors() {
    for body in [
        "Call Accept(AddressOf Callbacks.Notify)\n@\n",
        "Call Accept(AddressOf Callbacks.Notify + 1)\n",
        "Call Accept(AddressOf Callbacks.Notify(1))\n",
    ] {
        let source = format!(
            "Attribute VB_Name = \"Callbacks\"\nSub Notify()\nEnd Sub\nSub Run()\n{body}End Sub\n"
        );
        let fixture = Fixture::new(VBA, source.as_bytes());
        let budget = limits();
        let request = request(&fixture.snapshot, &fixture.source, VBA, &budget);
        let parsed = rootlight_adapter_sdk::execute_parse(
            &provider(),
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        assert_ne!(
            parsed.report().coverage().status(),
            CoverageStatus::Complete,
            "{body}"
        );
        assert!(
            parsed
                .diagnostics()
                .iter()
                .any(|diagnostic| diagnostic.code().as_str() == "syntax-error-recovery"),
            "{body}"
        );
    }
}

#[test]
fn vba_procedure_pointer_continuations_do_not_consume_following_statements() {
    for body in [
        "Accept AddressOf Notify _\n, 1\n",
        "Accept AddressOf Notify _\r\n, 1\r\n",
        "With receiver\nAccept AddressOf Notify\n.Value = 1\nEnd With\n",
    ] {
        let source = format!("Sub Notify()\nEnd Sub\nSub Run()\n{body}End Sub\n");
        let result = output(&source);
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.procedure_pointer.reference")
            .expect("complete simple pointer target");
        assert!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            "{body}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
    }
}

#[test]
fn vba_procedure_pointers_replay_with_native_module_context() {
    assert_vba_pointer_replay(
        "Attribute VB_Name = \"Callbacks\"\nSub Notify()\nEnd Sub\nSub Run()\nAccept AddressOf Notify\nEnd Sub\n",
        "vba.procedure_pointer.reference",
    );
}

#[test]
fn vba_qualified_procedure_pointers_replay_with_native_module_context() {
    assert_vba_pointer_replay(
        "Attribute VB_Name = \"Callbacks\"\nSub Notify()\nEnd Sub\nSub Run()\nCall Accept(AddressOf Callbacks.Notify)\nEnd Sub\n",
        "vba.qualified_procedure_pointer.reference",
    );
}

fn assert_vba_pointer_replay(source: &str, syntax: &str) {
    use rootlight_adapter_sdk::NativeVbaModuleKind;

    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, VBA);
    let fixture = Fixture::new(VBA, source.as_bytes());
    let budget = limits();
    for kind in [NativeVbaModuleKind::Procedural, NativeVbaModuleKind::Object] {
        let initial = request(&fixture.snapshot, &fixture.source, VBA, &budget)
            .with_native_vba_module(kind)
            .unwrap();
        let (first, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let site = first
            .document()
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == syntax)
            .unwrap();
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            kind == NativeVbaModuleKind::Procedural
        );
        let changed = fixture.next_generation();
        let next = request(&changed.snapshot, &changed.source, VBA, &budget)
            .with_native_vba_module(kind)
            .unwrap();
        let replay = analyzer
            .analyze_from_artifact(
                &next,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let (fresh, _) = analyzer
            .analyze_and_capture(
                &next,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(replay.document(), fresh.document());
        assert_eq!(replay.report(), fresh.report());
        assert!(
            replay
                .document()
                .occurrences
                .iter()
                .all(|site| { site.source.generation() == changed.source.generation() })
        );
    }
}

#[test]
fn vba_native_context_binds_artifacts_and_required_fact_budgets() {
    use rootlight_adapter_sdk::NativeVbaModuleKind;
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, VBA);
    let fixture = Fixture::new(
        VBA,
        b"Attribute VB_Name = \"Counter\"\nPrivate total As Long\nPublic Sub Run()\n#If Enabled Then\nGoTo Finish\nFinish:\nGoTo 10\n10: Me.total = 1\n#End If\nEnd Sub\n",
    );
    let budget = limits();
    let extensions = ExtensionSupport::default();
    let contexts = [
        None,
        Some(NativeVbaModuleKind::Procedural),
        Some(NativeVbaModuleKind::Object),
    ];
    for kind in contexts {
        let initial = request(&fixture.snapshot, &fixture.source, VBA, &budget);
        let initial = match kind {
            Some(kind) => initial.with_native_vba_module(kind).unwrap(),
            None => initial,
        };
        let (_, artifact) = analyzer
            .analyze_and_capture(
                &initial,
                extensions.clone(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        let demand = provider
            .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
            .unwrap();
        assert_eq!(
            demand,
            artifact.required_syntax_fact_count(&deadline()).unwrap()
        );
        let successor = fixture.next_generation();
        for next_kind in contexts {
            let next = request(&successor.snapshot, &successor.source, VBA, &budget);
            let next = match next_kind {
                Some(kind) => next.with_native_vba_module(kind).unwrap(),
                None => next,
            };
            let replay = analyzer.analyze_from_artifact(
                &next,
                &artifact,
                extensions.clone(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            );
            if next_kind == kind {
                let replay = replay.unwrap();
                let fresh = analyze(&analyzer, &next, &extensions);
                assert_eq!(replay.document(), fresh.document());
                assert_eq!(replay.report(), fresh.report());
            } else {
                assert!(
                    replay.is_err(),
                    "changed module category must invalidate artifacts"
                );
            }
        }
        let bounded = limits_with_syntax_records(demand);
        let request = request(&fixture.snapshot, &fixture.source, VBA, &bounded);
        let request = match kind {
            Some(kind) => request.with_native_vba_module(kind).unwrap(),
            None => request,
        };
        let (fresh, bounded_artifact) = analyzer
            .analyze_and_capture(
                &request,
                extensions.clone(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            demand,
            bounded_artifact
                .required_syntax_fact_count(&deadline())
                .unwrap()
        );
        let replay = analyzer
            .analyze_from_artifact(
                &request,
                &bounded_artifact,
                extensions.clone(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
        assert_eq!(fresh.document(), replay.document());
    }
}

#[test]
fn vba_preprocessor_branches_keep_duplicate_private_types_source_distinct() {
    let source = "Attribute VB_Name = \"Options\"\n#If VBA7 Then\nPrivate Type Configuration\nValue As Long\nEnd Type\n#Else\nPrivate Type Configuration\nValue As Integer\nEnd Type\n#End If\n";
    let result = output_with_native_kind(
        source,
        Some(rootlight_adapter_sdk::NativeVbaModuleKind::Procedural),
    );
    let document = result.document();
    let types: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Struct && entity.canonical_name == "Configuration"
        })
        .collect();
    assert_eq!(types.len(), 2);
    assert_ne!(types[0].id, types[1].id);
    let mut actual_starts = types
        .iter()
        .map(|entity| entity.evidence.source.as_ref().unwrap().span().start_byte())
        .collect::<Vec<_>>();
    actual_starts.sort_unstable();
    assert_eq!(
        actual_starts,
        source
            .match_indices("Private Type Configuration")
            .map(|(offset, _)| u64::try_from(offset).unwrap())
            .collect::<Vec<_>>()
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .all(|gap| gap.domain != FactDomain::Entities),
        "{:?}",
        document.skipped_regions
    );
}

#[test]
fn vba_nested_preprocessor_branches_preserve_private_type_inventory() {
    let source = "Attribute VB_Name = \"Options\"\n#If PLATFORM Then\n#If VBA7 Then\nPrivate Type Configuration\nValue As Long\nEnd Type\n#Else\nPrivate Type Configuration\nValue As Integer\nEnd Type\n#End If\n#End If\n";
    let result = output_with_native_kind(
        source,
        Some(rootlight_adapter_sdk::NativeVbaModuleKind::Procedural),
    );
    let document = result.document();
    let types: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Struct && entity.canonical_name == "Configuration"
        })
        .collect();
    assert_eq!(types.len(), 2);
    assert_ne!(types[0].id, types[1].id);
    assert!(
        document
            .skipped_regions
            .iter()
            .all(|gap| gap.domain != FactDomain::Entities)
    );
}

#[test]
fn vba_enum_members_bind_in_module_and_qualified_value_contexts() {
    let source = "Private Enum Phase\nReady = 1\nDone = Ready + 1\nEnd Enum\nSub Run(Optional ByVal selected As Long = Done)\nDim answer As Long\nanswer = Ready\nanswer = Phase.Done\nanswer = [Phase].[Ready]\nanswer = Phase.Done(1)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (text, name, ordinal) in [
        ("Ready", "Ready", 1),
        ("Done", "Done", 1),
        ("Ready", "Ready", 2),
        ("Phase.Done", "Done", 0),
        ("[Phase].[Ready]", "Ready", 0),
        ("Phase.Done", "Done", 1),
    ] {
        let start = source.match_indices(text).nth(ordinal).unwrap().0;
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + text.len()).unwrap()
                    && site.syntax_kind != "vba.application.reference"
            })
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == rootlight_ir::EntityKind::Constant && entity.canonical_name == name
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{text} at {start}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == rootlight_ir::RelationPredicate::RefersTo
                && edge.object == rootlight_ir::RelationEndpoint::Entity(target.id)
                && edge.evidence.source.as_ref() == Some(&site.source)
        }));
    }
    assert!(
        document
            .relations
            .iter()
            .all(|edge| edge.predicate != rootlight_ir::RelationPredicate::Calls)
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vba-call-or-index-target-unavailable")
    );
}

#[test]
fn vba_enum_members_preserve_shadowing_duplicates_and_initializer_order() {
    for (source, text, ordinal, target_kind, resolved) in [
        (
            "Enum Phase\nReady = 1\nEnd Enum\nSub Run()\nDim Ready As Long\nanswer = Ready\nEnd Sub\n",
            "Ready",
            2,
            rootlight_ir::EntityKind::Variable,
            true,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nSub Run(ByVal Phase As Object)\nanswer = Phase.Ready\nEnd Sub\n",
            "Phase.Ready",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nSub Run()\nanswer = Phase.Ready.Other\nEnd Sub\n",
            "Phase.Ready.Other",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nready = 2\nEnd Enum\nSub Run()\nanswer = Phase.Ready\nEnd Sub\n",
            "Phase.Ready",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nEnum Phase\nDone = 2\nEnd Enum\nSub Run()\nanswer = Phase.Ready\nEnd Sub\n",
            "Phase.Ready",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = Ready + 1\nEnd Enum\n",
            "Ready",
            1,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = Done\nDone = 2\nEnd Enum\n",
            "Done",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = Phase.Done\nDone = 2\nEnd Enum\n",
            "Phase.Done",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nEnum Result\nDone = Phase.Ready\nEnd Enum\n",
            "Phase.Ready",
            0,
            rootlight_ir::EntityKind::Constant,
            true,
        ),
        (
            "#If FEATURE Then\nEnum Phase\nReady = 1\nEnd Enum\n#End If\nSub Run()\nanswer = Ready\nEnd Sub\n",
            "Ready",
            1,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nPrivate Ready As Long\nSub Run()\nanswer = Ready\nEnd Sub\n",
            "Ready",
            2,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum First\nReady = 1\nEnd Enum\nEnum Second\nReady = 2\nEnd Enum\nSub Run()\nanswer = Ready\nEnd Sub\n",
            "Ready",
            2,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Worker\"\nPrivate Enum Phase\nReady = 1\nEnd Enum\nSub Run()\nanswer = Me.Ready\nEnd Sub\n",
            "Ready",
            1,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
        (
            "Enum Phase\nReady = 1\nEnd Enum\nSub Run()\nanswer = Ready$\nEnd Sub\n",
            "Ready$",
            0,
            rootlight_ir::EntityKind::Constant,
            false,
        ),
    ] {
        let result = output(source);
        let start = source.match_indices(text).nth(ordinal).unwrap().0;
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + text.len()).unwrap()
                    && site.syntax_kind != "vba.application.reference"
            })
            .unwrap_or_else(|| panic!("missing {text} in {source}"));
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            resolved,
            "{text} in {source}"
        );
        if let OccurrenceTarget::Resolved { symbol } = site.target {
            let target = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap();
            assert_eq!(target.kind, target_kind, "{source}");
        }
        assert_ne!(site.role, OccurrenceRole::CallSite);
    }
}

#[test]
fn vba_enum_type_member_collisions_use_source_order() {
    for (declarations, expression, resolved, name) in [
        (
            "Enum Phase\nReady = 1\nEnd Enum\nEnum Extra\nPhase = 2\nEnd Enum\nEnum Other\nPhase = 3\nEnd Enum\n",
            "Phase.Ready",
            true,
            "Ready",
        ),
        (
            "Enum Extra\nPhase = 2\nEnd Enum\nEnum Phase\nReady = 1\nEnd Enum\n",
            "Phase",
            true,
            "Phase",
        ),
        (
            "Enum Extra\nPhase = 2\nEnd Enum\nEnum Other\nPhase = 3\nEnd Enum\nEnum Phase\nReady = 1\nEnd Enum\n",
            "Phase",
            false,
            "Phase",
        ),
        (
            "Enum Phase\nPhase = 1\nEnd Enum\n",
            "Phase.Phase",
            true,
            "Phase",
        ),
    ] {
        let source = format!("{declarations}Sub Run()\nanswer = {expression}\nEnd Sub\n");
        let result = output(&source);
        let start = source.rfind(expression).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(start + expression.len()).unwrap()
                    && site.syntax_kind != "vba.application.reference"
            })
            .unwrap();
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            resolved,
            "{source}"
        );
        if let OccurrenceTarget::Resolved { symbol } = site.target {
            let target = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.id == symbol)
                .unwrap();
            assert_eq!(target.kind, rootlight_ir::EntityKind::Constant);
            assert_eq!(target.canonical_name, name);
        }
    }
}

#[test]
fn vba_member_invocation_context_does_not_prove_a_call() {
    let source = "Sub Run(ByVal worker As Object, ByVal other As Object)\nCall worker.Apply(1)\nworker.Apply 2\nanswer = worker.Compute(3)\nworker.Item(1) = 4\nSet worker.Item(2) = other\nanswer = worker.Item\nCall worker.[Apply](5)\nworker.Check 1 = 2\nCall worker.Check((1) = 2)\nworker.Check (1) = 2, 3\nworker.Item (1) = 4\nEnd Sub\n";
    let result = output(source);
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_vba::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    for (text, syntax, ordinal) in [
        ("worker.Apply", "vba.invoked_member.reference", 0),
        ("worker.Apply", "vba.invoked_member.reference", 1),
        ("worker.Compute", "vba.invoked_get_member.reference", 0),
        ("worker.Item", "vba.member.reference", 0),
        ("worker.Item", "vba.invoked_set_member.reference", 1),
        ("worker.Item", "vba.get_member.reference", 2),
        ("worker.[Apply]", "vba.invoked_member.reference", 0),
        ("worker.Check", "vba.invoked_member.reference", 0),
        ("worker.Check", "vba.invoked_member.reference", 1),
        ("worker.Check", "vba.invoked_member.reference", 2),
        ("worker.Item", "vba.member.reference", 3),
    ] {
        let start = source.match_indices(text).nth(ordinal).unwrap().0;
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                usize::try_from(site.source.span().start_byte()).unwrap() == start
                    && usize::try_from(site.source.span().end_byte()).unwrap() == start + text.len()
                    && site.syntax_kind != "vba.application.reference"
            })
            .unwrap();
        assert_eq!(
            site.syntax_kind,
            syntax,
            "{text} at {start}: {}",
            tree.root_node().to_sexp()
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.source == site.source && gap.detail == "vba-member-target-unavailable"
        }));
    }
    assert!(
        result
            .document()
            .relations
            .iter()
            .all(|relation| { relation.predicate != rootlight_ir::RelationPredicate::Calls })
    );
}

#[test]
fn vba_explicit_declaration_visibility_survives_lowering() {
    use rootlight_ir::EntityVisibility;
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Boundary\"\nPublic exposed As Long, another As Long\nPrivate hidden As Long\nPublic Sub Visible()\nEnd Sub\nPrivate Sub Internal()\nEnd Sub\nFriend Sub ProjectOnly()\nEnd Sub\nPrivate Type Coordinate\nX As Long\nEnd Type\n";
    let result = output(source);
    for (name, expected) in [
        ("exposed", EntityVisibility::Public),
        ("another", EntityVisibility::Public),
        ("hidden", EntityVisibility::Private),
        ("Visible", EntityVisibility::Public),
        ("Internal", EntityVisibility::Private),
        ("ProjectOnly", EntityVisibility::Restricted),
        ("Coordinate", EntityVisibility::Private),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.visibility, expected, "{name}");
    }
}

#[test]
fn vba_visibility_does_not_infer_defaults_from_body_text() {
    use rootlight_ir::EntityVisibility;
    let source = "Attribute VB_Name = \"Boundary\"\n' Public\nDim stored As Long\nSub Run(ByVal value As Long)\nDim text As String\ntext = \"Private Public Friend\"\nEnd Sub\nFriend Sub InvalidFriend()\nEnd Sub\n";
    let result = output(source);
    for name in ["stored", "Run", "value", "text", "InvalidFriend"] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.visibility, EntityVisibility::Unknown, "{name}");
    }
}

#[test]
fn vba_instance_udt_fields_follow_class_ownership() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate Type Coordinate\nX As Long\nEnd Type\nPrivate Type Box\nLocation As Coordinate\nEnd Type\nPrivate position As Box\nSub Run(ByVal position As Variant)\nMe.position.Location.X = 1\nmE.[position].Location.X = 2\nEnd Sub\n";
    let result = output(source);
    for expression in [
        "Me.position.Location",
        "Me.position.Location.X",
        "mE.[position].Location.X",
    ] {
        let name = expression.rsplit('.').next().unwrap();
        let target = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == name)
            .unwrap();
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind
                    == if expression.ends_with(".X") {
                        "vba.let_member.reference"
                    } else {
                        "vba.get_member.reference"
                    }
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(expression.len()).unwrap()
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{expression}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(
            !result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-member-target-unavailable"
                    && gap.source.span() == site.source.span())
        );
    }
}

#[test]
fn vba_instance_udt_fields_preserve_unproven_receiver_barriers() {
    for (header, declaration) in [
        ("", "Private position As Coordinate\n"),
        ("VERSION 1.0 CLASS\n", "Private position As Variant\n"),
        ("VERSION 1.0 CLASS\n", "Private position() As Coordinate\n"),
        (
            "VERSION 1.0 CLASS\n",
            "Private position As Coordinate\nPrivate POSITION As Coordinate\n",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "#If FLAG Then\nPrivate position As Coordinate\n#Else\nPrivate position As Variant\n#End If\n",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Property Get position() As Coordinate\nEnd Property\n",
        ),
    ] {
        let source = format!(
            "{header}Attribute VB_Name = \"Holder\"\nPrivate Type Coordinate\nX As Long\nEnd Type\n{declaration}Sub Run()\nDim position As Coordinate\nMe.position.X = 1\nEnd Sub\n"
        );
        let result = output(&source);
        let expression = "Me.position.X";
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.let_member.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(expression.len()).unwrap()
            })
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-member-target-unavailable"
                    && gap.source.span() == site.source.span())
        );
    }
}

#[test]
fn vba_declared_udt_fields_follow_local_types_and_nested_fields() {
    let source = "Private Type Coordinate\nX As Long\nEnd Type\nPrivate Type Box\nLocation As Coordinate\nEnd Type\nSub Run(ByRef value As Box)\nDim snapshot As Coordinate\nsnapshot.X = value.Location.X\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (expression, field) in [
        ("snapshot.X", "X"),
        ("value.Location", "Location"),
        ("value.Location.X", "X"),
    ] {
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == field)
            .unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind
                    == if expression == "snapshot.X" {
                        "vba.let_member.reference"
                    } else {
                        "vba.get_member.reference"
                    }
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(expression.len()).unwrap()
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{expression}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.detail == "vba-member-target-unavailable" && gap.source.span() == site.source.span()
        }));
    }
}

#[test]
fn vba_declared_type_references_ignore_value_namespace_shadowing() {
    let source = "Private Type Coordinate\nX As Long\nEnd Type\nSub Run(ByVal Coordinate As Long)\nDim point As Coordinate\npoint.X = Coordinate\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Struct && entity.canonical_name == "Coordinate")
        .unwrap();
    let start = u64::try_from(source.find("As Coordinate").unwrap() + 3).unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| site.source.span().start_byte() == start)
        .unwrap();
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert_eq!(site.role, OccurrenceRole::TypeUse);
    assert!(
        document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::UsesType
                && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id)
                && relation.evidence.source.as_ref() == Some(&site.source))
    );
}

#[test]
fn vba_conditional_types_and_fields_require_implied_unique_declarations() {
    for (definition, between, guard, resolves) in [
        (
            "#If Enabled Then\n{type}#End If\n",
            "",
            "#If ENABLED Then\n{body}#End If\n",
            true,
        ),
        (
            "#If Enabled Then\n#Else\n{type}#End If\n",
            "",
            "#If Enabled Then\n#Else\n{body}#End If\n",
            true,
        ),
        (
            "#If Enabled Then\n{type}#Else\n{type}#End If\n",
            "",
            "#If Enabled Then\n{body}#End If\n",
            true,
        ),
        (
            "#If Enabled Then\n{type}#End If\n",
            "",
            "#If Other Then\n{body}#End If\n",
            false,
        ),
        ("#If Enabled Then\n{type}#End If\n", "", "{body}", false),
        (
            "#If Enabled Then\n{type}#End If\n",
            "",
            "#If Enabled Then\n#Else\n{body}#End If\n",
            false,
        ),
        (
            "#If Enabled Then\n{type}{type}#End If\n",
            "",
            "#If Enabled Then\n{body}#End If\n",
            false,
        ),
        (
            "{type}#If Enabled Then\n{type}#End If\n",
            "",
            "#If Enabled Then\n{body}#End If\n",
            false,
        ),
        (
            "{type}#If Enabled Then\n{type}#End If\n",
            "",
            "#If Enabled Then\n#Else\n{body}#End If\n",
            true,
        ),
        (
            "#If Enabled Then\n#If Other Then\n{type}#End If\n#End If\n",
            "",
            "#If Enabled Then\n{body}#End If\n",
            false,
        ),
        (
            "#If Enabled Then\n{type}#End If\n",
            "#Const Enabled = False\n",
            "#If Enabled Then\n{body}#End If\n",
            false,
        ),
        (
            "#If Version >= 7 Then\n{type}#End If\n",
            "",
            "#If Version >= 7 Then\n{body}#End If\n",
            false,
        ),
        ("#If Version >= 7 Then\n{type}{body}#End If\n", "", "", true),
    ] {
        let body =
            "Sub Run(ByVal Record As Long)\nDim item As Record\nitem.Value = Record\nEnd Sub\n";
        let source = format!(
            "{}{between}{}",
            definition
                .replace("{type}", "Private Type Record\nValue As Long\nEnd Type\n")
                .replace("{body}", body),
            guard.replace("{body}", body),
        );
        let result = output(&source);
        let document = result.document();
        for (marker, offset, name, role, gap) in [
            (
                "As Record",
                3,
                "Record",
                OccurrenceRole::TypeUse,
                "vba-type-target-unavailable",
            ),
            (
                "item.Value",
                0,
                "Value",
                OccurrenceRole::Reference,
                "vba-member-target-unavailable",
            ),
        ] {
            let start = u64::try_from(source.find(marker).unwrap() + offset).unwrap();
            let end = start + u64::try_from(marker.len() - offset).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == end
                        && site.role == role
                })
                .unwrap();
            assert_eq!(
                matches!(site.target, OccurrenceTarget::Resolved { .. }),
                resolves,
                "{marker}: {source}"
            );
            if let OccurrenceTarget::Resolved { symbol } = site.target {
                let target = document
                    .entities
                    .iter()
                    .find(|entity| entity.id == symbol)
                    .unwrap();
                assert_eq!(target.canonical_name, name);
                // The positive branch selects the first definition, not its sibling.
                assert!(
                    target.evidence.source.as_ref().unwrap().span().start_byte()
                        < u64::try_from(source.find("End Type").unwrap()).unwrap()
                );
            }
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .any(|region| region.source == site.source && region.detail == gap),
                !resolves
            );
        }
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-conditional-compilation-evaluation-unavailable")
        );
    }
}

#[test]
fn vba_qualified_types_retain_one_path_without_local_name_fallback() {
    for (declaration, syntax) in [
        (
            "Sub Run(ByVal item As Primary.Target)\nEnd Sub\n",
            "vba.direct_type_identifier.reference",
        ),
        (
            "Private item As New Primary.Target\n",
            "vba.automatic_type_identifier.reference",
        ),
        (
            "Function Make() As Primary.Target\nEnd Function\n",
            "vba.type_identifier.reference",
        ),
        (
            "Sub Run(ByVal item As Object)\nIf TypeOf item Is Primary.Target Then\nEnd If\nEnd Sub\n",
            "vba.tested_type_identifier.reference",
        ),
        (
            "Sub Run(ByVal item() As Primary.Target)\nEnd Sub\n",
            "vba.type_identifier.reference",
        ),
    ] {
        for path in [
            "Primary.Target",
            "[Primary].[Target]",
            "Primary _\r\n .Target",
            "Foreign.Target",
        ] {
            let source = format!(
                "Attribute VB_Name = \"Consumer\"\nPrivate Type Target\nValue As Long\nEnd Type\n{}",
                declaration.replace("Primary.Target", path)
            );
            let result = output(&source);
            let start = u64::try_from(source.find(path).unwrap()).unwrap();
            let end = start + u64::try_from(path.len()).unwrap();
            let sites: Vec<_> = result
                .document()
                .occurrences
                .iter()
                .filter(|site| {
                    site.source.span().start_byte() >= start && site.source.span().end_byte() <= end
                })
                .collect();
            assert_eq!(sites.len(), 1, "{source}: {sites:?}");
            let site = sites[0];
            assert_eq!(site.source.span().start_byte(), start);
            assert_eq!(site.source.span().end_byte(), end);
            assert_eq!(site.syntax_kind, syntax, "{source}");
            assert_eq!(site.role, OccurrenceRole::TypeUse);
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(result.document().skipped_regions.iter().any(
                |gap| gap.source == site.source && gap.detail == "vba-type-target-unavailable"
            ));
        }
    }
}

#[test]
fn vba_implemented_types_retain_whole_names_without_runtime_bindings() {
    for name in ["Contract", "Primary.Contract", "[Primary].[Contract]"] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Worker\"\nImplements {name}\nPrivate Sub Contract_Run()\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find(name).unwrap()).unwrap();
        let end = start + u64::try_from(name.len()).unwrap();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() >= start && site.source.span().end_byte() <= end
            })
            .collect();
        assert_eq!(sites.len(), 1, "{name}");
        let site = sites[0];
        assert_eq!(site.syntax_kind, "vba.implemented_type.reference");
        assert_eq!(site.role, OccurrenceRole::ImplementationUse);
        assert_eq!(site.source.span().start_byte(), start);
        assert_eq!(site.source.span().end_byte(), end);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        let owner = document
            .entities
            .iter()
            .find(|entity| Some(entity.id) == site.enclosing)
            .unwrap();
        assert_eq!(owner.kind, EntityKind::Class);
        assert_eq!(owner.canonical_name, "Worker");
        assert!(document.skipped_regions.iter().any(|gap| gap.source == site.source && gap.detail == "vba-type-target-unavailable"));
    }
}

#[test]
fn vba_implementation_recovery_never_admits_a_prefix_contract() {
    for statement in [
        "Implements Contract()",
        "Implements Contract Extra",
        "Implements Primary.Contract()",
        "Implements Primary.Contract.Extra",
        "#If Enabled Then\nImplements Contract\n#End If",
    ] {
        let source = format!("VERSION 1.0 CLASS\nAttribute VB_Name = \"Worker\"\n{statement}\n");
        let result = output(&source);
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::ImplementationUse)
            .collect();
        if statement.starts_with("#If") {
            assert!(sites.is_empty());
            assert!(
                result
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "vba-conditional-compilation-evaluation-unavailable")
            );
            continue;
        }
        assert_eq!(sites.len(), 1, "{statement}");
        assert_eq!(
            sites[0].syntax_kind, "vba.unsupported_implemented_type.reference",
            "{statement}"
        );
        assert!(matches!(
            sites[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
}

#[test]
fn vba_returned_index_receivers_read_getters_without_selecting_setters() {
    for statement in [
        "result = Values(1)(2)",
        "Values(1)(2) = 3",
        "Set Values(1)(2) = other",
        "Call Values(1)(2)",
        "Use value:=Values(1)(2)",
    ] {
        let source = format!(
            "Private Property Get Values(ByVal index As Long) As Object\nEnd Property\nPrivate Property Let Values(ByVal index As Long, ByVal value As Variant)\nEnd Property\nPrivate Property Set Values(ByVal index As Long, ByVal value As Object)\nEnd Property\nSub Run()\nDim result As Variant\nDim other As Object\n{statement}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.rfind("Values(1)(2)").unwrap()).unwrap();
        let callee = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 6
            })
            .unwrap();
        assert_eq!(
            callee.syntax_kind, "vba.invoked_get_identifier.reference",
            "{statement}"
        );
        let OccurrenceTarget::Resolved { symbol } = callee.target else {
            panic!("getter must be resolved: {statement}");
        };
        let target = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        let span = target.evidence.source.as_ref().unwrap().span();
        assert!(
            source
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                )
                .unwrap()
                .contains("Property Get Values")
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(callee.id)
                        && edge.predicate == RelationPredicate::Calls
                        && edge.object == RelationEndpoint::Entity(symbol)
                })
                .count(),
            1,
            "{statement}"
        );
        let outer = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().end_byte() == start + 12
                    && site.source.span().start_byte() <= start
            })
            .unwrap();
        assert!(
            matches!(outer.target, OccurrenceTarget::Unresolved { .. }),
            "{statement}"
        );
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.source == outer.source && gap.detail == "vba-call-or-index-target-unavailable"
        }));
    }
}

#[test]
fn vba_conditional_udt_fields_require_implied_unique_declarations() {
    for (fields, between, guard, target) in [
        (
            "#If Enabled Then\nValue As Long\n#End If\n",
            "",
            "#If ENABLED Then\n{body}#End If\n",
            Some("Value As Long"),
        ),
        (
            "#If Enabled Then\nValue As Long\n#Else\nValue As String\n#End If\n",
            "",
            "#If Enabled Then\n{body}#End If\n",
            Some("Value As Long"),
        ),
        (
            "#If Enabled Then\nValue As Long\n#Else\nValue As String\n#End If\n",
            "",
            "#If Enabled Then\n#Else\n{body}#End If\n",
            Some("Value As String"),
        ),
        (
            "#If Enabled Then\nValue As Long\n#End If\n",
            "",
            "{body}",
            None,
        ),
        (
            "#If Enabled Then\nValue As Long\n#End If\n",
            "",
            "#If Other Then\n{body}#End If\n",
            None,
        ),
        (
            "#If Enabled Then\nValue As Long\n#End If\n",
            "",
            "#If Enabled Then\n#Else\n{body}#End If\n",
            None,
        ),
        (
            "Value As String\n#If Enabled Then\nValue As Long\n#End If\n",
            "",
            "#If Enabled Then\n{body}#End If\n",
            None,
        ),
        (
            "Value As String\n#If Enabled Then\nValue As Long\n#End If\n",
            "",
            "#If Enabled Then\n#Else\n{body}#End If\n",
            Some("Value As String"),
        ),
        (
            "#If Enabled Then\nValue As Long\nValue As String\n#End If\n",
            "",
            "#If Enabled Then\n{body}#End If\n",
            None,
        ),
        (
            "#If Enabled Then\n#If Other Then\nValue As Long\n#End If\n#End If\n",
            "",
            "#If Enabled Then\n{body}#End If\n",
            None,
        ),
        (
            "#If Enabled Then\n#If Other Then\nValue As Long\n#End If\n#End If\n",
            "",
            "#If Enabled Then\n#If Other Then\n{body}#End If\n#End If\n",
            Some("Value As Long"),
        ),
        (
            "#If Enabled Then\nValue As Long\n#End If\n",
            "#Const Enabled = False\n",
            "#If Enabled Then\n{body}#End If\n",
            None,
        ),
        (
            "#If Version >= 7 Then\nValue As Long\n#End If\n",
            "",
            "#If Version >= 7 Then\n{body}#End If\n",
            None,
        ),
    ] {
        let expression = "item.[vAlUe]";
        let source = format!(
            "Private Type Record\nAnchor As Long\n{fields}End Type\n{between}Sub Run(ByRef item As Record)\n{}End Sub\n",
            guard.replace("{body}", &format!("{expression} = 1\n")),
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start && site.source.span().end_byte() == end
            })
            .unwrap();
        if let Some(definition) = target {
            let definition_start = u64::try_from(source.find(definition).unwrap()).unwrap();
            let target = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source.span().start_byte() == definition_start
                })
                .unwrap();
            assert!(
                matches!(site.target, OccurrenceTarget::Resolved { .. }),
                "{source}"
            );
            assert_eq!(site.target, target.target, "{source}");
        } else {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{source}"
            );
        }
        assert_eq!(
            document.skipped_regions.iter().any(|gap| {
                gap.source == site.source && gap.detail == "vba-member-target-unavailable"
            }),
            target.is_none(),
            "{source}"
        );
        assert_ne!(site.role, OccurrenceRole::CallSite);
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| { gap.detail == "vba-conditional-compilation-evaluation-unavailable" })
        );
    }
}

#[test]
fn vba_conditional_udt_fields_follow_nested_and_result_receivers() {
    for (procedure, terminator) in [("Function", "Function"), ("Property Get", "Property")] {
        for (condition, array, resolves) in [
            ("Enabled", "", true),
            ("Other", "", false),
            ("Enabled", "()", false),
        ] {
            let source = format!(
                "Private Type Inner\n#If Enabled Then\nValue As Long\n#Else\nValue As String\n#End If\nEnd Type\nPrivate Type Record\n#If Enabled Then\nNested{array} As Inner\n#Else\nNested As Variant\n#End If\nEnd Type\nPrivate {procedure} Build() As Record\n#If {condition} Then\nBuild.Nested.Value = 1\n#End If\nEnd {terminator}\nSub Run(ByRef item As Record)\n#If {condition} Then\nitem.Nested.Value = Build().Nested.Value\n#End If\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let definition_start = u64::try_from(source.find("Value As Long").unwrap()).unwrap();
            let target = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source.span().start_byte() == definition_start
                })
                .unwrap();
            for expression in [
                "Build.Nested.Value",
                "item.Nested.Value",
                "Build().Nested.Value",
            ] {
                let start = u64::try_from(source.find(expression).unwrap()).unwrap();
                let end = start + u64::try_from(expression.len()).unwrap();
                let site = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.source.span().start_byte() == start
                            && site.source.span().end_byte() == end
                    })
                    .unwrap();
                if resolves {
                    assert_eq!(site.target, target.target, "{expression}: {source}");
                    assert!(matches!(site.target, OccurrenceTarget::Resolved { .. }));
                } else {
                    assert!(
                        matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                        "{expression}: {source}"
                    );
                }
                assert_eq!(
                    document.skipped_regions.iter().any(|gap| {
                        gap.source == site.source && gap.detail == "vba-member-target-unavailable"
                    }),
                    !resolves,
                    "{expression}: {source}"
                );
                assert_ne!(site.role, OccurrenceRole::CallSite);
            }
        }
    }
}

#[test]
fn vba_conditional_type_fields_follow_local_and_returned_receivers() {
    let source = "#If Enabled Then\nPrivate Type Inner\nValue As Long\nEnd Type\nPrivate Type Record\nNested As Inner\nEnd Type\nFunction Create() As Record\nEnd Function\n#End If\nSub Run()\n#If Enabled Then\nDim item As Record\nitem.Nested.Value = Create().Nested.Value\n#Else\nDim item As Variant\nitem.Nested.Value = 0\n#End If\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "Value")
        .unwrap();
    for expression in ["item.Nested.Value", "Create().Nested.Value"] {
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start && site.source.span().end_byte() == end
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{expression}"
        );
    }
    let start = u64::try_from(source.rfind("item.Nested.Value").unwrap()).unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start && site.source.span().end_byte() == start + 17
        })
        .unwrap();
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
}

#[test]
fn vba_conditional_enum_types_keep_the_type_and_value_namespaces_separate() {
    for (other, resolves) in [
        ("", true),
        ("Private Type Mode\nValue As Long\nEnd Type\n", false),
    ] {
        let source = format!(
            "#If Enabled Then\nPrivate Enum Mode\nReady = 1\nEnd Enum\n{other}#End If\n#If Enabled Then\nSub Run(ByVal Mode As Long)\nDim choice As Mode\nEnd Sub\n#End If\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find("choice As Mode").unwrap() + 10).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start && site.role == OccurrenceRole::TypeUse
            })
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Enum && entity.canonical_name == "Mode")
            .unwrap();
        assert_eq!(
            site.target == OccurrenceTarget::Resolved { symbol: target.id },
            resolves
        );
        assert_eq!(document.skipped_regions.iter().any(|gap| gap.source == site.source && gap.detail == "vba-type-target-unavailable"), !resolves);
    }
}

#[test]
fn vba_type_tests_reference_udts_without_declaring_function_return_types() {
    let source = "Private Type Shape\nX As Long\nEnd Type\nPrivate candidate As Variant\nFunction Item() As Object\nIf TypeOf candidate Is Shape Then\nEnd If\nEnd Function\nSub Run()\nItem().X = 1\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Struct && entity.canonical_name == "Shape")
        .unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.tested_type_identifier.reference")
        .unwrap();
    assert_eq!(site.role, OccurrenceRole::TypeUse);
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert!(
        document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::UsesType
                && edge.object == rootlight_ir::RelationEndpoint::Entity(target.id)
                && edge.evidence.source.as_ref() == Some(&site.source))
    );
    let member = document
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.let_member.reference")
        .unwrap();
    assert!(matches!(member.target, OccurrenceTarget::Unresolved { .. }));
}

#[test]
fn vba_construction_types_never_bind_to_values_or_udts() {
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_vba::LANGUAGE.into())
        .unwrap();
    for declaration in [
        "",
        "Private Type Worker\nValue As Long\nEnd Type\n",
        "Private Enum Worker\nReady = 1\nEnd Enum\n",
    ] {
        let source = format!(
            "{declaration}Sub Run(ByVal Worker As Long)\nDim value As Object\nSet value = New Worker\nSet value = New [Worker]\nSet value = New Missing.Worker\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let types: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind.contains("constructed_type_identifier"))
            .collect();
        assert!(types.len() >= 3, "{:#?}", document.occurrences);
        for site in types {
            assert_eq!(site.role, OccurrenceRole::TypeUse);
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(document.skipped_regions.iter().any(
                |gap| gap.source == site.source && gap.detail == "vba-type-target-unavailable"
            ));
        }
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| site.syntax_kind == "vba.construction.reference")
                .count(),
            3
        );
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "vba.member.reference"
                    || site.syntax_kind.ends_with("_member.reference")),
            "{}",
            parser.parse(&source, None).unwrap().root_node().to_sexp()
        );
    }
}

#[test]
fn vba_declared_udt_members_preserve_ambiguous_dynamic_and_array_barriers() {
    for (types, globals, parameter, body, unresolved) in [
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As Variant",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As Object",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As External",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point() As Coordinate",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As New Coordinate",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nx As Long\nEnd Type\n",
            "",
            "point As Coordinate",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\nPrivate Type Coordinate\nY As Long\nEnd Type\n",
            "",
            "point As Coordinate",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\n#If FLAG Then\nX As Long\n#Else\nX As Long\n#End If\nEnd Type\n",
            "",
            "point As Coordinate",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "Private point As Coordinate\n",
            "point As Variant",
            "point.X = 1",
            "point.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\nPrivate Type Box\nLocation() As Coordinate\nEnd Type\n",
            "",
            "value As Box",
            "value.Location.X = 1",
            "value.Location.X",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As Coordinate",
            "point.X$ = 1",
            "point.X$",
        ),
        (
            "Private Type Coordinate\nX As Long\nEnd Type\n",
            "",
            "point As Coordinate",
            "point!X = 1",
            "point!X",
        ),
    ] {
        let source = format!("{types}{globals}Sub Run(ByRef {parameter})\n{body}\nEnd Sub\n");
        let result = output(&source);
        let start = u64::try_from(source.find(unresolved).unwrap()).unwrap();
        let end = start + u64::try_from(unresolved.len()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.let_member.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-member-target-unavailable"
                    && gap.source.span() == site.source.span()),
            "{source}"
        );
    }
}

#[test]
fn vba_declared_udt_field_references_do_not_invent_calls() {
    let source = "Private Type Coordinate\nX As Long\nEnd Type\nSub Run(ByRef point As Coordinate)\nCall point.X(1)\nEnd Sub\n";
    let result = output(source);
    let field = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "X")
        .unwrap();
    let site = result
        .document()
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.invoked_member.reference")
        .unwrap();
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
    assert_eq!(site.role, OccurrenceRole::Reference);
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vba-call-or-index-target-unavailable")
    );
}

#[test]
fn vba_declared_udt_types_and_fields_survive_generation_replay() {
    let source = "Private Type Coordinate\r\n[X] As Long\r\nEnd Type\r\nSub Run(ByRef point As _\r\n Coordinate)\r\npoint.[X] = 1\r\nEnd Sub\r\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, VBA);
    let fixture = Fixture::new(VBA, source.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, VBA, &budget);
    let (first, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let field = first
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "X")
        .unwrap();
    let site = first
        .document()
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.let_member.reference")
        .unwrap();
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
    let changed = fixture.next_generation();
    let next = request(&changed.snapshot, &changed.source, VBA, &budget);
    let replay = analyzer
        .analyze_from_artifact(
            &next,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let (fresh, _) = analyzer
        .analyze_and_capture(
            &next,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(replay.document(), fresh.document());
    assert_eq!(replay.report(), fresh.report());
    assert!(
        replay
            .document()
            .occurrences
            .iter()
            .all(|site| site.source.generation() == changed.source.generation())
    );
}

#[test]
fn vba_declared_udt_fields_do_not_rebind_instance_keywords() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate Type Coordinate\nX As Long\nEnd Type\nPrivate Type Box\nLocation As Coordinate\nEnd Type\nPrivate Location As Variant\nSub Run()\nDim [Me] As Box\nMe.Location.X = 1\n[Me].Location.X = 2\nEnd Sub\n";
    let result = output(source);
    let field = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "X")
        .unwrap();
    for (expression, resolved) in [("Me.Location.X", false), ("[Me].Location.X", true)] {
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.let_member.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        if resolved {
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
        } else {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{expression}"
            );
            assert!(
                result
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "vba-member-target-unavailable"
                        && gap.source.span() == site.source.span())
            );
        }
    }
}

#[test]
fn vba_declared_udt_fields_remain_unresolved_when_capture_budget_is_clipped() {
    let source = format!(
        "Private Type Coordinate\nX As Long\nEnd Type\nSub Run(ByRef point As Coordinate)\npoint.X = 1\n{}End Sub\n",
        "unknown = another\n".repeat(100)
    );
    let provider = Arc::new(provider());
    let fixture = Fixture::new(VBA, source.as_bytes());
    let defaults = limits();
    let initial = request(&fixture.snapshot, &fixture.source, VBA, &defaults);
    let demand = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    let budget = limits_with_syntax_records(demand + 8);
    let result = analyze(
        &analyzer(&provider, VBA),
        &request(&fixture.snapshot, &fixture.source, VBA, &budget),
        &ExtensionSupport::default(),
    );
    assert!(
        result
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-extraction-limit")
    );
    let members: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "vba.let_member.reference")
        .collect();
    assert!(!members.is_empty());
    assert!(
        members
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn visual_basic_classic_preserves_structure_and_dialect_uncertainty() {
    let case = LanguageCase {
        name: "visual-basic-classic",
        ..VBA
    };
    let source = "Attribute VB_Name = \"Meter\"\nPublic Function Measure(ByVal value As Long) As Long\nMeasure = value + 1\nEnd Function\n";
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    for name in ["Meter", "Measure", "value"] {
        assert!(
            document.entities.iter().any(|entity| {
                entity.canonical_name == name && entity.language == "visual-basic-classic"
            }),
            "missing {name}"
        );
    }
    let start = u64::try_from(source.find("value +").unwrap()).unwrap();
    let value = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "value")
        .unwrap();
    assert!(document.occurrences.iter().any(|site| {
        site.source.span().start_byte() == start
            && site.target == OccurrenceTarget::Resolved { symbol: value.id }
    }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "visual-basic-dialect-unproven"
            && gap.source.span().start_byte() == 0
            && gap.source.span().end_byte() == u64::try_from(source.len()).unwrap()
    }));
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn visual_basic_classic_preserves_class_accessor_and_instance_identity() {
    let case = LanguageCase {
        name: "visual-basic-classic",
        ..VBA
    };
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate stored As Variant\nPublic Property Get Item() As Variant\nItem = stored\nEnd Property\nPublic Property Let Item(ByVal value As Variant)\nstored = value\nEnd Property\nSub Run(ByVal target As Variant)\nSet target = Me\nEnd Sub\n";
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    let class = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Holder")
        .unwrap();
    let accessors: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property && entity.canonical_name == "Item")
        .collect();
    assert_eq!(accessors.len(), 2);
    assert_ne!(accessors[0].id, accessors[1].id);
    assert!(
        document
            .entities
            .iter()
            .all(|entity| entity.language == case.name)
    );
    let start = u64::try_from(source.find("Me").unwrap()).unwrap();
    assert!(document.occurrences.iter().any(|site| {
        site.source.span().start_byte() == start
            && site.source.span().end_byte() == start + 2
            && site.target == OccurrenceTarget::Resolved { symbol: class.id }
    }));
}

#[test]
fn visual_basic_classic_partial_analysis_scopes_dialect_uncertainty() {
    let case = LanguageCase {
        name: "visual-basic-classic",
        ..VBA
    };
    let body = "Sub Run()\nEnd Sub\n";
    let source = format!("excluded prefix\n{body}excluded suffix\n");
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, source.as_bytes());
    let budget = limits();
    let start = u64::try_from(source.find(body).unwrap()).unwrap();
    let span = SourceSpan::new(
        fixture.source.span().file(),
        start,
        start + u64::try_from(body.len()).unwrap(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        EncodingId::utf8(),
        vec![rootlight_adapter_sdk::IncludedRange::new(
            span,
            language(case),
        )],
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, case),
        &request,
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "visual-basic-dialect-unproven")
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].source.span(), span);
    assert!(
        document
            .entities
            .iter()
            .any(|entity| { entity.language == case.name && entity.canonical_name == "Run" })
    );
}

#[test]
fn vba_instance_expressions_reference_the_source_class() {
    for statement in [
        "Set value = mE",
        "value = mE.Item",
        "Visit mE",
        "With mE\n.Item = 1\nEnd With",
        "If value Is mE Then\nvalue = 1\nEnd If",
        "value = mE(0)",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPublic Item As Variant\nSub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let class = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Holder")
            .unwrap();
        let start = u64::try_from(source.find("mE").unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 2
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: class.id },
            "{statement}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(class.id)
        }));
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(site.id)
        }));
        if statement.contains("mE(0)") {
            assert!(document.skipped_regions.iter().any(|gap| {
                gap.source.span() == site.source.span()
                    && gap.detail == "vba-call-or-index-target-unavailable"
            }));
        }
    }
}

#[test]
fn vba_instance_expressions_require_class_bodies_and_keyword_spelling() {
    for (prefix, body, suffix, marker) in [
        ("", "Sub Run()\nSet value = mE\nEnd Sub\n", "", "mE"),
        (
            "VERSION 1.0 FORM\n",
            "Sub Run()\nSet value = mE\nEnd Sub\n",
            "",
            "mE",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Sub Run(Optional value As Variant = mE)\nEnd Sub\n",
            "",
            "mE",
        ),
        (
            "VERSION 1.0 CLASS\nSub Run()\n",
            "Const value = mE\n",
            "End Sub\n",
            "mE",
        ),
        (
            "VERSION 1.0 CLASS\nSub Run()\n",
            "value = New mE\n",
            "End Sub\n",
            "mE",
        ),
        (
            "VERSION 1.0 CLASS\nSub Run()\n",
            "value = AddressOf mE\n",
            "End Sub\n",
            "mE",
        ),
        (
            "VERSION 1.0 CLASS\nSub Run()\n",
            "value = [mE]\n",
            "End Sub\n",
            "[mE]",
        ),
        (
            "VERSION 1.0 CLASS\nSub Run()\n",
            "value = mE$\n",
            "End Sub\n",
            "mE$",
        ),
    ] {
        let source = format!("{prefix}{body}{suffix}");
        let result = output(&source);
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + u64::try_from(marker.len()).unwrap()
            })
            .unwrap_or_else(|| panic!("instance-context reference is missing: {source}"));
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result.document().skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
            }),
            "{source}"
        );
    }
    let source = "VERSION 1.0 CLASS\nSet value = mE\n";
    let result = output(source);
    let start = u64::try_from(source.find("mE").unwrap()).unwrap();
    let statement_start = u64::try_from(source.find("Set").unwrap()).unwrap();
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    // Recovery diagnoses the executable statement at Set, not at its value.
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.domain == FactDomain::Diagnostics
            && gap.reason == rootlight_ir::SkippedRegionReason::ParseError
            && gap.source.span().start_byte() == statement_start
            && gap.source.span().end_byte() == statement_start + 3
    }));
    assert!(!result.document().occurrences.iter().any(|site| {
        site.source.span().start_byte() == start
            && matches!(site.target, OccurrenceTarget::Resolved { .. })
    }));
}

#[test]
fn vba_result_references_bind_the_enclosing_function_or_getter() {
    for (header, end, kind) in [
        (
            "Function Measure(ByVal value As Variant) As Variant",
            "Function",
            EntityKind::Function,
        ),
        (
            "Property Get Measure(ByVal value As Variant) As Variant",
            "Property",
            EntityKind::Property,
        ),
    ] {
        for statement in [
            "[mEaSuRe] = value",
            "Let [mEaSuRe] = value",
            "Set [mEaSuRe] = value",
            "value = [mEaSuRe]",
            "value = 1 + [mEaSuRe]",
            "Visit [mEaSuRe]",
        ] {
            let source = format!("{header}\n{statement}\nEnd {end}\n");
            let result = output(&source);
            let document = result.document();
            let target = document
                .entities
                .iter()
                .find(|entity| entity.kind == kind && entity.canonical_name == "Measure")
                .unwrap();
            let site = reference_at(document, &source, "[mEaSuRe]");
            assert_eq!(
                site.source.span().end_byte() - site.source.span().start_byte(),
                9
            );
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{source}"
            );
            assert_eq!(site.role, OccurrenceRole::Reference, "{source}");
            assert!(document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(target.id)
            }));
            assert!(!document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
            }));
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
            }));
        }
    }
}

#[test]
fn vba_result_references_keep_type_scope_and_index_barriers() {
    for (before, body, after, marker) in [
        (
            "Function Measure() As Variant\n",
            "mEaSuRe$ = 1",
            "End Function\n",
            "mEaSuRe$",
        ),
        (
            "Function Measure() As Variant\nDim Measure As Variant\n",
            "mEaSuRe = 1",
            "End Function\n",
            "mEaSuRe",
        ),
        (
            "Property Get Measure() As Variant\nDim value As Variant\n",
            "value = mEaSuRe(0)",
            "End Property\n",
            "mEaSuRe",
        ),
        (
            "Property Get Measure() As Variant\n",
            "mEaSuRe(0) = 1",
            "End Property\n",
            "mEaSuRe",
        ),
        (
            "Property Get Measure() As Variant\n",
            "Call mEaSuRe()",
            "End Property\n",
            "mEaSuRe",
        ),
        (
            "Property Get Measure() As Variant\nDim Measure As Variant\n",
            "mEaSuRe = 1",
            "End Property\n",
            "mEaSuRe",
        ),
        (
            "Function Measure(Optional value As Variant = mEaSuRe) As Variant\n",
            "",
            "End Function\n",
            "mEaSuRe",
        ),
    ] {
        let source = format!("{before}{body}\n{after}");
        let result = output(&source);
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + u64::try_from(marker.len()).unwrap()
            })
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert_ne!(site.role, OccurrenceRole::CallSite, "{source}");
        assert!(
            result.document().skipped_regions.iter().any(|gap| {
                gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
            }),
            "{source}"
        );
    }
}

#[test]
fn vba_result_fields_bind_the_enclosing_udt_without_recursive_calls() {
    for (header, end) in [
        ("Function Build(ByVal seed As Long) As Record", "Function"),
        (
            "Property Get Build(ByVal seed As Long) As Record",
            "Property",
        ),
    ] {
        for conditional in [false, true] {
            let body = format!(
                "{header}\nDim value As Long\n[bUiLd].Nested.Value = seed\nvalue = Build.Nested.Value\nSet Build.Link = Nothing\nEnd {end}\n"
            );
            let types = "Private Type Inner\nValue As Long\nEnd Type\nPrivate Type Record\nNested As Inner\nLink As Object\nEnd Type\n";
            let source = if conditional {
                format!("#If Enabled Then\n{types}{body}#End If\n")
            } else {
                format!("{types}{body}")
            };
            let result = output(&source);
            let document = result.document();
            for (expression, field) in [
                ("[bUiLd].Nested.Value", "Value"),
                ("Build.Nested.Value", "Value"),
                ("Build.Link", "Link"),
            ] {
                let start = u64::try_from(source.find(expression).unwrap()).unwrap();
                let end = start + u64::try_from(expression.len()).unwrap();
                let site = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.source.span().start_byte() == start
                            && site.source.span().end_byte() == end
                    })
                    .unwrap();
                let target = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.kind == EntityKind::Field && entity.canonical_name == field
                    })
                    .unwrap();
                assert_eq!(
                    site.target,
                    OccurrenceTarget::Resolved { symbol: target.id },
                    "{source}: {expression}"
                );
                assert_eq!(site.role, OccurrenceRole::Reference);
                assert!(
                    !document
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.source == site.source
                            && gap.detail == "vba-member-target-unavailable")
                );
                for part in document.occurrences.iter().filter(|part| {
                    part.source.span().start_byte() >= start && part.source.span().end_byte() <= end
                }) {
                    assert_ne!(part.role, OccurrenceRole::CallSite);
                    assert!(
                        !document
                            .relations
                            .iter()
                            .any(|edge| edge.predicate == RelationPredicate::Calls
                                && edge.subject == RelationEndpoint::Occurrence(part.id))
                    );
                }
            }
        }
    }
}

#[test]
fn vba_result_fields_preserve_return_shape_and_scope_barriers() {
    for (return_type, local, expression, after, resolves) in [
        ("Record", "", "Build.Value", "", true),
        ("Variant", "", "Build.Value", "", false),
        ("Object", "", "Build.Value", "", false),
        ("Missing", "", "Build.Value", "", false),
        ("Record()", "", "Build.Value", "", false),
        ("Record()", "", "Build(1).Value", "", false),
        ("Record", "Dim Build As Record\n", "Build.Value", "", false),
        ("Record", "", "Build$.Value", "", false),
        (
            "Record",
            "",
            "Build.Value",
            "End Function\nSub Other()\n",
            false,
        ),
    ] {
        let source = format!(
            "Private Type Record\nValue As Long\nEnd Type\nFunction Build(ByVal seed As Long) As {return_type}\n{local}{after}Dim result As Long\nresult = {expression}\nEnd {}\n",
            if after.is_empty() { "Function" } else { "Sub" }
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.get_member.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        assert_eq!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            resolves,
            "{source}"
        );
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source
                    && gap.detail == "vba-member-target-unavailable"),
            !resolves
        );
    }
}

#[test]
fn vba_conditional_procedure_results_bind_within_their_body() {
    for (header, end, kind) in [
        (
            "Private Function Measure() As Variant",
            "Function",
            EntityKind::Function,
        ),
        (
            "Private Property Get Measure() As Variant",
            "Property",
            EntityKind::Property,
        ),
    ] {
        let source = format!("#If Mac Then\n\n{header}\n    Measure = 1\nEnd {end}\n\n#End If\n");
        let result = output(&source);
        let document = result.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.kind == kind && entity.canonical_name == "Measure")
            .unwrap();
        let site = reference_at(document, &source, "Measure = 1");
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
        }));
    }
}

#[test]
fn vba_conditional_procedure_result_does_not_escape_or_override_a_local() {
    let source = "#If Mac Then\n\nPrivate Function Measure() As Variant\n    Dim Measure As Variant\n    Measure = 1\nEnd Function\n\n#End If\nPublic Sub Consumer()\n    Dim result As Variant\n    result = Measure\nEnd Sub\n";
    let result = output(source);
    for marker in ["Measure = 1", "Measure\nEnd Sub"] {
        let site = reference_at(result.document(), source, marker);
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
        }));
    }
}

#[test]
fn vba_unqualified_properties_select_source_bound_accessors() {
    for (parameters, argument) in [("", ""), ("ByVal index As Long", "(0)")] {
        let setter_parameters = if parameters.is_empty() {
            "ByVal value As Variant".to_owned()
        } else {
            format!("{parameters}, ByVal value As Variant")
        };
        for (statement, accessor) in [
            (format!("value = [iTeM]{argument}"), "Get Item"),
            (format!("[iTeM]{argument} = value"), "Let Item"),
            (format!("Let [iTeM]{argument} = value"), "Let Item"),
            (format!("Set [iTeM]{argument} = value"), "Set Item"),
            (format!("value = 1 + [iTeM]{argument}"), "Get Item"),
            (format!("Visit [iTeM]{argument}"), "Get Item"),
        ] {
            let source = format!(
                "Property Get Item({parameters}) As Variant\nItem = 1\nEnd Property\nProperty Let Item({setter_parameters})\nEnd Property\nProperty Set Item({setter_parameters})\nEnd Property\nSub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let definition_start = u64::try_from(source.find(accessor).unwrap() + 4).unwrap();
            let target = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source.span().start_byte() == definition_start
                })
                .unwrap()
                .target
                .clone();
            assert!(matches!(target, OccurrenceTarget::Resolved { .. }));
            let start = u64::try_from(source.find("[iTeM]").unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == start + 6
                })
                .unwrap();
            assert_eq!(site.target, target, "{statement}");
            assert_eq!(site.role, OccurrenceRole::CallSite, "{statement}");
            let OccurrenceTarget::Resolved { symbol } = target else {
                unreachable!()
            };
            assert!(document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(symbol)
            }));
        }
    }
}

#[test]
fn vba_unqualified_properties_respect_local_array_proofs_and_dynamic_index_gaps() {
    for (parameter, local) in [
        ("", "Dim Item(2) As Variant\n"),
        ("ByVal Item As Variant", ""),
    ] {
        for statement in [
            "value = iTeM",
            "iTeM = value",
            "Set iTeM = value",
            "iTeM(0) = value",
            "value = iTeM(0)",
        ] {
            let source = format!(
                "Property Get Item() As Variant\nItem = 1\nEnd Property\nSub Run({parameter})\n{local}Dim value As Variant\n{statement}\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let value = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Item"
                        && matches!(entity.kind, EntityKind::Variable | EntityKind::Parameter)
                })
                .unwrap();
            let start = u64::try_from(source.find("iTeM").unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == start + 4
                })
                .unwrap();
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: value.id },
                "{statement}"
            );
            assert_eq!(site.role, OccurrenceRole::Reference);
            if statement.contains('(') {
                let proven_array_access = parameter.is_empty();
                assert_eq!(
                    document.skipped_regions.iter().any(|gap| {
                        gap.domain == FactDomain::Relations
                            && gap.source.span() == site.source.span()
                            && gap.detail == "vba-call-or-index-target-unavailable"
                    }),
                    !proven_array_access,
                    "{source}"
                );
                assert!(!document.relations.iter().any(|edge| {
                    edge.predicate == RelationPredicate::Calls
                        && edge.subject == RelationEndpoint::Occurrence(site.id)
                }));
            }
        }
    }
}

#[test]
fn vba_unqualified_properties_keep_result_slots_and_unproven_calls_explicit() {
    let get = "Property Get Item() As Variant\nItem = 1\nEnd Property\n";
    for (declarations, statement) in [
        (get.to_owned(), "iTeM = value"),
        (get.to_owned(), "Set iTeM = value"),
        (get.to_owned(), "Call iTeM()"),
        (get.to_owned(), "ReDim iTeM(2)"),
        (get.to_owned(), "value = iTeM$"),
        (get.repeat(2), "value = iTeM"),
        (format!("#If Enabled Then\n{get}#End If\n"), "value = iTeM"),
        (
            "Function Item(ByVal index As Long) As Variant\nEnd Function\n".to_owned(),
            "iTeM(0) = value",
        ),
    ] {
        let source =
            format!("{declarations}Sub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n");
        let result = output(&source);
        let start = u64::try_from(source.find("iTeM").unwrap()).unwrap();
        let name_length = if statement.contains("iTeM$") { 5 } else { 4 };
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + name_length
            })
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.domain == FactDomain::Relations && gap.source.span() == site.source.span()
        }));
    }
    for statement in ["iTeM = value", "Set iTeM = value", "value = iTeM"] {
        let source = format!(
            "Property Get Item() As Variant\nDim value As Variant\n{statement}\nEnd Property\nProperty Let Item(ByVal value As Variant)\nEnd Property\nProperty Set Item(ByVal value As Variant)\nEnd Property\n"
        );
        let result = output(&source);
        let document = result.document();
        let getter = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition && site.source.span().start_byte() == 13
            })
            .unwrap();
        assert!(matches!(getter.target, OccurrenceTarget::Resolved { .. }));
        assert_eq!(
            reference_at(document, &source, "iTeM").target,
            getter.target
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .all(|site| site.role != OccurrenceRole::CallSite)
        );
    }
}

#[test]
fn vba_function_values_without_parentheses_accept_omittable_parameters() {
    for parameters in [
        "",
        "()",
        "(Optional ByVal input As Long = 1)",
        "(ParamArray items() As Variant)",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Meter\"\nFunction Measure{parameters} As Long\nMeasure = 1\nEnd Function\nSub Run()\nDim Measure As Variant\nMeasure = Me.[mEaSuRe]\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let method = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Method && entity.canonical_name == "Measure")
            .unwrap();
        let start = u64::try_from(source.find("[mEaSuRe]").unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 9
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: method.id },
            "{parameters}"
        );
        assert_eq!(site.role, OccurrenceRole::CallSite, "{parameters}");
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(method.id))
        );
    }
}

#[test]
fn vba_bare_function_values_resolve_in_expression_contexts() {
    for expression in ["mEaSuRe", "1 + mEaSuRe", "Abs(mEaSuRe)"] {
        let source = format!(
            "Function Measure() As Long\nMeasure = 1\nEnd Function\nSub Run()\nDim value As Long\nvalue = {expression}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let function = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Function && entity.canonical_name == "Measure"
            })
            .unwrap();
        let site = reference_at(document, &source, "mEaSuRe");
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved {
                symbol: function.id
            }
        );
        assert_eq!(site.role, OccurrenceRole::CallSite);
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(function.id)
        }));
    }
}

#[test]
fn vba_nested_function_argument_resolves_to_local_declaration() {
    let source = "Attribute VB_Name = \"Utility\"\nPublic Function Normalize(ByVal value As Long) As Long\nNormalize = value\nEnd Function\nPublic Sub Run(ByVal input As Object, ByVal output As Object)\nIf input.Visible Then\nDebug.Print \"header\" & Format(Now, \"Long Time\")\nDebug.Print Normalize(input.Status) & \" \" & output.Render(input)\nEnd If\nEnd Sub\n".replace('\n', "\r\n");
    let result = output(&source);
    let document = result.document();
    let function = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "Normalize")
        .unwrap();
    let start = u64::try_from(source.find("Normalize(input.Status)").unwrap()).unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start
                && site.source.span().end_byte() == start + 9
                && site.syntax_kind.ends_with("identifier.reference")
        })
        .unwrap();
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_vba::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(&source, None).unwrap();
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved {
            symbol: function.id
        },
        "{}",
        tree.root_node().to_sexp()
    );
    assert_eq!(site.role, OccurrenceRole::CallSite);
    assert!(document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Calls
            && edge.subject == RelationEndpoint::Occurrence(site.id)
            && edge.object == RelationEndpoint::Entity(function.id)
    }));
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source.span() == site.source.span()
            && gap.detail == "vba-call-or-index-target-unavailable"
    }));
}

#[test]
fn vba_nested_index_argument_does_not_call_shadowed_function() {
    let source = "Attribute VB_Name = \"Utility\"\nPublic Function Normalize(ByVal value As Long) As Long\nNormalize = value\nEnd Function\nPublic Sub Run(ByVal input As Object, ByVal output As Object)\nDim Normalize(2) As Long\nIf input.Visible Then\nDebug.Print \"header\" & Format(Now, \"Long Time\")\nDebug.Print Normalize(0) & \" \" & output.Render(input)\nEnd If\nEnd Sub\n".replace('\n', "\r\n");
    let result = output(&source);
    let document = result.document();
    let variable = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "Normalize")
        .unwrap();
    let start = u64::try_from(source.find("Normalize(0)").unwrap()).unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start
                && site.source.span().end_byte() == start + 9
                && site.syntax_kind.ends_with("identifier.reference")
        })
        .unwrap();
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved {
            symbol: variable.id
        }
    );
    assert_eq!(site.role, OccurrenceRole::Reference);
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Calls
            && edge.subject == RelationEndpoint::Occurrence(site.id)
    }));
}

#[test]
fn vba_variadic_applications_validate_the_complete_parameter_list() {
    for (parameters, arguments, expected) in [
        ("ParamArray items()", "", true),
        ("ParamArray items() As Variant", "1, 2, 3", true),
        ("ParamArray _\r\n items() As Variant", "1, 2", true),
        ("ParamArray items() As [Variant]", "1,, 3", true),
        ("ByVal first As Long, ParamArray items()", "1", true),
        (
            "ByVal first As Long, ParamArray items()",
            "1, 2, 3, 4",
            true,
        ),
        ("ByVal first As Long, ParamArray items()", "", false),
        ("ByVal first As Long, ParamArray items()", ", 2", false),
        ("ByVal first As Long, ParamArray items()", "first:=1", false),
        ("ParamArray items()", "items:=1", false),
        ("ParamArray items()", "ByVal 1", false),
        ("ParamArray items() As Long", "1, 2", false),
        ("ParamArray items", "1", false),
        ("ByVal first As Long, ParamArray FIRST()", "1, 2", false),
        ("ParamArray items(), ByVal last As Long", "1, 2", false),
        (
            "Optional first As Long = 0, ParamArray items()",
            "1, 2",
            false,
        ),
    ] {
        let source = format!(
            "Function Gather({parameters}) As Long\nEnd Function\nSub Run()\nDim result As Long\nresult = Gather({arguments})\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.rfind("Gather(").unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Gather")
            .unwrap();
        assert_eq!(
            site.target == OccurrenceTarget::Resolved { symbol: target.id },
            expected,
            "{source}"
        );
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source
                    && gap.detail == "vba-call-or-index-target-unavailable"),
            !expected,
            "{source}"
        );
        if arguments.contains(":=") {
            assert!(
                document
                    .occurrences
                    .iter()
                    .filter(
                        |site| site.syntax_kind == "vba.nonvalue_identifier.reference"
                            && site.source.span().start_byte() > start
                    )
                    .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
                "{source}"
            );
        }
    }
}

#[test]
fn vba_external_applications_preserve_binding_and_signature_barriers() {
    for (declaration, invocation) in [
        (
            "Private Declare Sub Accept Lib \"host\" (value As Long)",
            "result = Accept(1)",
        ),
        (
            "Private Declare Function Accept Lib \"host\" (value As Long) As Long",
            "Dim Accept As Object\nresult = Accept(1)",
        ),
        (
            "Private Declare Function Accept Lib \"host\" (value As Long, VALUE As Long) As Long",
            "result = Accept(1, 2)",
        ),
        (
            "Private Declare Function Accept Lib \"host\" (value As Long) As Long\nPrivate Declare Function Accept Lib \"other\" (value As Long) As Long",
            "result = Accept(1)",
        ),
        (
            "Private Declare Function Accept Lib \"host\" (value As Long) As Long",
            "result = Accept(value:=1, value:=2)",
        ),
        (
            "Private Declare Function Accept Lib \"host\" (value As Long) As Long",
            "result = Accept(1, value:=2)",
        ),
        (
            "Private Declare Function Accept Lib \"host\" (value As Long) As Long",
            "result = Me.Accept(1)",
        ),
        (
            "Private Declare Function Accept Lib \"host\" (value As Long) As Long",
            "result = Accept(1)(2)",
        ),
        (
            "Function Accept(value As Long) As Long\nEnd Function",
            "result = Accept(ByVal(1))",
        ),
    ] {
        let source =
            format!("{declaration}\nSub Run()\nDim result As Variant\n{invocation}\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.rfind("Accept").unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() <= start
                    && site.source.span().end_byte() > start
            })
            .max_by_key(|site| site.source.span().end_byte())
            .unwrap();
        assert!(
            matches!(application.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == application.source
                    && gap.detail == "vba-call-or-index-target-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn vba_external_malformed_arguments_retain_errors_without_complete_bindings() {
    for arguments in ["ByVal", "ByVal _\n", "ByVal AddressOf Handler"] {
        let expression = format!("Accept({arguments})");
        let source = format!(
            "Private Declare Function Accept Lib \"host\" (value As Any) As Long\nSub Handler()\nEnd Sub\nSub Run()\nDim result As Long\nresult = {expression}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.rfind(&expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        assert!(
            !document.occurrences.iter().any(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            }),
            "{source}"
        );
        assert!(
            document.skipped_regions.iter().any(|gap| {
                gap.source.span().start_byte() < end && gap.source.span().end_byte() > start
            }),
            "{source}"
        );
    }
}

#[test]
fn vba_local_applications_bind_complete_argument_lists() {
    for (parameters, arguments) in [
        ("ByVal value As Long", "1"),
        ("ByVal value As Long, ByVal other As Long", "Inner(1, 2), 3"),
        ("ByVal value As Long, Optional other As Long = 0", "1"),
        (
            "ByVal value As Long, Optional other As Long = 0",
            "other:=2, value:=1",
        ),
        (
            "Optional first As Long = 0, Optional last As Long = 0",
            ", 2",
        ),
        ("ByVal value As Long", " _\r\n 1"),
    ] {
        for (declaration, expression) in [
            (
                format!("Sub Accept({parameters})\nEnd Sub"),
                format!("Accept {arguments}"),
            ),
            (
                format!("Function Accept({parameters}) As Long\nEnd Function"),
                format!("result = Accept({arguments})"),
            ),
        ] {
            let source =
                format!("{declaration}\nSub Run()\nDim result As Long\n{expression}\nEnd Sub\n");
            let result = output(&source);
            let document = result.document();
            let target = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Accept")
                .unwrap();
            let start = u64::try_from(source.rfind("Accept").unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == start
                })
                .unwrap();
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{source}"
            );
            assert_eq!(site.role, OccurrenceRole::Reference);
            assert!(
                document
                    .relations
                    .iter()
                    .any(|edge| edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.predicate == RelationPredicate::RefersTo
                        && edge.object == RelationEndpoint::Entity(target.id))
            );
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source
                        && gap.detail == "vba-call-or-index-target-unavailable")
            );
        }
    }
}

#[test]
fn vba_local_applications_reject_argument_and_dispatch_ambiguity() {
    for (declaration, invocation) in [
        ("Sub Accept(ByVal value As Long)\nEnd Sub", "Accept"),
        ("Sub Accept(ByVal value As Long)\nEnd Sub", "Accept 1, 2"),
        (
            "Sub Accept(ByVal value As Long, Optional other As Long = 0)\nEnd Sub",
            "Accept , 2",
        ),
        (
            "Sub Accept(ByVal value As Long)\nEnd Sub",
            "Accept value:=1, value:=2",
        ),
        (
            "Sub Accept(ByVal value As Long)\nEnd Sub",
            "Accept 1, value:=2",
        ),
        (
            "Sub Accept(ByVal value As Long)\nEnd Sub",
            "Accept missing:=1",
        ),
        ("Sub Accept(ByVal value As Long)\nEnd Sub", "Accept ByVal 1"),
        (
            "Sub Accept(ByVal value As Long, ByVal VALUE As Long)\nEnd Sub",
            "Accept 1, 2",
        ),
        (
            "Sub Accept(ByVal value As Long)\nEnd Sub\nSub Accept(ByVal value As Long)\nEnd Sub",
            "Accept 1",
        ),
        (
            "Function Accept() As Object\nEnd Function",
            "result = Accept()(1)",
        ),
        (
            "Sub Accept(ByVal value As Long)\nEnd Sub",
            "Dim Accept As Object\nAccept 1",
        ),
    ] {
        let source =
            format!("{declaration}\nSub Run()\nDim result As Variant\n{invocation}\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.rfind("Accept").unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
            })
            .max_by_key(|site| site.source.span().end_byte())
            .unwrap();
        assert!(
            matches!(application.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == application.source
                    && gap.detail == "vba-call-or-index-target-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn vba_explicit_calls_and_call_prefixed_names_keep_exact_application_sources() {
    for invocation in [
        "Call Callback(1)",
        "Callback 1",
        "Callback _\n value:=1",
        "Call _\r\n Callback(value:=1)",
    ] {
        let source = format!(
            "Sub Callback(ByVal value As Long)\nEnd Sub\nSub Run()\n{invocation}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let symbol = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Callback")
            .unwrap()
            .id;
        let start = u64::try_from(source.find(invocation).unwrap()).unwrap();
        let end = start + u64::try_from(invocation.len()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        assert_eq!(
            application.target,
            OccurrenceTarget::Resolved { symbol },
            "{source}"
        );
        assert_eq!(application.role, OccurrenceRole::Reference);
    }
}

#[test]
fn vba_declared_array_application_binds_without_guessing_object_defaults() {
    let source = "Sub Run()\nDim values(2) As Long\nDim dynamic() As Long\nDim objectValue As Object\nDim output As Long\nReDim dynamic(2)\nvalues(0) = Len(\"x\")\noutput = values(1)\noutput = dynamic(1)\noutput = objectValue(1)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let array = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "values")
        .unwrap();
    let dynamic = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "dynamic")
        .unwrap();
    for (expression, target) in [
        ("values(1)", Some(array.id)),
        ("dynamic(1)", Some(dynamic.id)),
        ("objectValue(1)", None),
    ] {
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap_or_else(|| panic!("missing application: {expression}"));
        match target {
            Some(symbol) => {
                assert_eq!(application.target, OccurrenceTarget::Resolved { symbol });
                assert_eq!(application.role, OccurrenceRole::Reference);
                assert!(document.relations.iter().any(|edge| {
                    edge.predicate == RelationPredicate::RefersTo
                        && edge.subject == RelationEndpoint::Occurrence(application.id)
                        && edge.object == RelationEndpoint::Entity(symbol)
                }));
                assert!(!document.skipped_regions.iter().any(|gap| {
                    gap.detail == "vba-call-or-index-target-unavailable"
                        && gap.source.span().start_byte() == start
                        && (gap.source.span().end_byte() == end
                            || gap.source.span().end_byte()
                                == start + u64::try_from(expression.find('(').unwrap()).unwrap())
                }));
            }
            None => {
                assert!(matches!(
                    application.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert!(document.skipped_regions.iter().any(|gap| {
                    gap.detail == "vba-call-or-index-target-unavailable"
                        && gap.source.span() == application.source.span()
                }));
                assert!(document.skipped_regions.iter().any(|gap| {
                    gap.detail == "vba-call-or-index-target-unavailable"
                        && gap.source.span().start_byte() == start
                        && gap.source.span().end_byte()
                            == start + u64::try_from(expression.find('(').unwrap()).unwrap()
                }));
            }
        }
    }
    let assignment_start = u64::try_from(source.find("values(0) = Len").unwrap()).unwrap();
    let assignment_receiver = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.invoked_identifier.reference"
                && site.source.span().start_byte() == assignment_start
        })
        .unwrap();
    assert_eq!(
        assignment_receiver.target,
        OccurrenceTarget::Resolved { symbol: array.id }
    );
    let assignment_application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == assignment_start
        })
        .unwrap();
    assert_eq!(
        assignment_application.target,
        OccurrenceTarget::Resolved { symbol: array.id }
    );
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source.span() == assignment_receiver.source.span()
            && gap.detail == "vba-call-or-index-target-unavailable"
    }));
    let value_start = u64::try_from(source.find("Len(\"x\")").unwrap()).unwrap();
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source.span().start_byte() == value_start
                && gap.detail == "vba-call-or-index-target-unavailable")
    );
}

#[test]
fn vba_array_applications_reject_procedure_argument_syntax() {
    for indices in ["slot:=0", "[slot]:=0", ", 0", "0, , 1", "ByVal 0"] {
        for receiver in ["items", "Me.items"] {
            for statement in [
                format!("result = {receiver}({indices})"),
                format!("{receiver}({indices}) = 1"),
            ] {
                let source = format!(
                    "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate items(2) As Long\nSub Run()\nDim result As Long\n{statement}\nEnd Sub\n"
                );
                let result = output(&source);
                let document = result.document();
                let start =
                    u64::try_from(source.find(&format!("{receiver}({indices})")).unwrap()).unwrap();
                let applications: Vec<_> = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.syntax_kind == "vba.application.reference"
                            && site.source.span().start_byte() == start
                    })
                    .collect();
                assert!(!applications.is_empty(), "missing application: {statement}");
                for application in applications {
                    assert!(
                        matches!(application.target, OccurrenceTarget::Unresolved { .. }),
                        "invalid array application resolved: {statement}: {application:?}"
                    );
                    assert!(document.skipped_regions.iter().any(|gap| {
                        gap.source == application.source
                            && gap.detail == "vba-call-or-index-target-unavailable"
                    }));
                }
                assert!(!document.relations.iter().any(|edge| {
                    edge.predicate == RelationPredicate::Calls
                        && edge.evidence.source.as_ref().is_some_and(|reference| {
                            reference.span().start_byte() >= start
                                && reference.span().end_byte()
                                    <= start + u64::try_from(statement.len()).unwrap()
                        })
                }));
            }
        }
    }
}

#[test]
fn vba_fixed_array_applications_require_the_declared_rank() {
    for (bounds, indices, proven) in [
        ("2", "0", true),
        ("2", "0, 1", false),
        ("0 To 2, 1 To 3", "0, 1", true),
        ("0 To 2, 1 To 3", "0", false),
        ("0 To 2, 1 To 3", "", true),
        ("", "0, 1", true),
    ] {
        for receiver in ["items", "Me.items"] {
            for write in [false, true] {
                let expression = format!("{receiver}({indices})");
                let statement = if write {
                    format!("{expression} = 1")
                } else {
                    format!("result = {expression}")
                };
                let source = format!(
                    "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate items({bounds}) As Long\nSub Run()\nDim result As Variant\n{statement}\nEnd Sub\n"
                );
                let result = output(&source);
                let document = result.document();
                let start = u64::try_from(source.find(&expression).unwrap()).unwrap();
                let application = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.application.reference"
                            && site.source.span().start_byte() == start
                            && (site.source.span().end_byte()
                                == start + u64::try_from(expression.len()).unwrap()
                                || (write
                                    && site.source.span().end_byte()
                                        == start + u64::try_from(statement.len()).unwrap()))
                    })
                    .unwrap();
                let array = document
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == "items")
                    .unwrap();
                assert_eq!(
                    application.target == OccurrenceTarget::Resolved { symbol: array.id },
                    proven,
                    "{bounds}: {expression}: {application:?}"
                );
                assert_eq!(
                    document.skipped_regions.iter().any(|gap| {
                        gap.source == application.source
                            && gap.detail == "vba-call-or-index-target-unavailable"
                    }),
                    !proven
                );
                assert!(!document.relations.iter().any(|edge| {
                    edge.predicate == RelationPredicate::Calls
                        && edge.subject == RelationEndpoint::Occurrence(application.id)
                }));
            }
        }
    }
}

#[test]
fn vba_array_writes_bind_exact_local_and_self_targets_without_calls() {
    for prefix in ["", "Let ", "Set "] {
        for receiver in ["items", "Me.items"] {
            let expression = format!("{receiver}(0)");
            let source = format!(
                "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate items(2) As Variant\nSub Run(ByVal value As Variant)\nDim items(2) As Variant\n{prefix}{expression} = value\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let kind = if receiver.starts_with("Me.") {
                EntityKind::Field
            } else {
                EntityKind::Variable
            };
            let target = document
                .entities
                .iter()
                .find(|entity| entity.kind == kind && entity.canonical_name == "items")
                .unwrap();
            let start = u64::try_from(source.find(&expression).unwrap()).unwrap();
            let end = start + u64::try_from(expression.len()).unwrap();
            let statement_end = end + u64::try_from(" = value".len()).unwrap();
            let application = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == start
                        && matches!(site.source.span().end_byte(), at if at == end || at == statement_end)
                })
                .unwrap();
            assert_eq!(
                application.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{source}"
            );
            assert_eq!(application.role, OccurrenceRole::Reference);
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.detail == "vba-call-or-index-target-unavailable"
                        && gap.source.span().start_byte() >= start
                        && gap.source.span().end_byte() <= end),
                "{source}"
            );
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Calls
                        && edge.subject == RelationEndpoint::Occurrence(application.id))
            );
        }
    }
}

#[test]
fn vba_array_write_resolution_keeps_dynamic_and_procedure_barriers() {
    for declaration in ["Dim items As Object", "Dim items As Variant"] {
        for prefix in ["", "Let ", "Set "] {
            let source = format!(
                "Sub Run(ByVal value As Variant)\n{declaration}\n{prefix}items(0) = value\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let start = u64::try_from(source.find("items(0)").unwrap()).unwrap();
            let application = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == start
                })
                .unwrap();
            assert!(
                matches!(application.target, OccurrenceTarget::Unresolved { .. }),
                "{source}"
            );
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == application.source
                        && gap.detail == "vba-call-or-index-target-unavailable")
            );
        }
    }
    let source = "Sub Accept(ByVal flag As Boolean)\nEnd Sub\nSub Run(ByVal value As Long)\nAccept (0) = value\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let start = u64::try_from(source.find("Accept (0)").unwrap()).unwrap();
    let application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == start
        })
        .unwrap();
    assert!(matches!(
        application.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source == application.source
                && gap.detail == "vba-call-or-index-target-unavailable")
    );
}

#[test]
fn vba_array_parameters_bind_indexed_reads_to_their_declarations() {
    for parameter in [
        "ByRef items() As Long",
        "items() As String",
        "ParamArray items() As Variant",
        "ByRef items%()",
        "bYrEf items( ) As Long",
        "ByRef _\nitems() As Long",
        "ByRef items _\n( _\n ) As Long",
        "ByRef items _\r\n( _\r\n ) As Long",
        "ParamArray items( _\n ) As Variant",
        "Optional ByRef items( _\n ) As Variant",
    ] {
        let source =
            format!("Sub Run({parameter})\nDim first As Variant\nfirst = items(0)\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let parameter = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "items")
            .unwrap();
        let start = u64::try_from(source.find("items(0)").unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 8
            })
            .unwrap();
        assert_eq!(
            application.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            },
            "{source}"
        );
        assert_eq!(application.role, OccurrenceRole::Reference);
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(application.id)
                && edge.object == RelationEndpoint::Entity(parameter.id)
        }));
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(application.id)
        }));
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.source.span().start_byte() == start
                && matches!(gap.source.span().end_byte() - start, 5 | 8)
                && gap.detail == "vba-call-or-index-target-unavailable"
        }));
    }
}

#[test]
fn vba_proven_array_targets_preserve_argument_and_returned_value_gaps() {
    let source = "Sub Run(ByRef values() As Long)\nDim first As Variant\nfirst = values(Missing(0))\nfirst = values(0)(1)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let argument_start = u64::try_from(source.find("Missing(0)").unwrap()).unwrap();
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "vba-call-or-index-target-unavailable"
            && gap.source.span().start_byte() == argument_start
    }));
    let outer_start = u64::try_from(source.find("values(0)(1)").unwrap()).unwrap();
    let outer = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == outer_start
                && site.source.span().end_byte() == outer_start + 12
        })
        .unwrap();
    assert!(matches!(outer.target, OccurrenceTarget::Unresolved { .. }));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "vba-call-or-index-target-unavailable"
            && gap.source.span() == outer.source.span()
    }));
    for expression in ["values(Missing(0))", "values(0)(1)"] {
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.detail == "vba-call-or-index-target-unavailable"
                && gap.source.span().start_byte() == start
                && gap.source.span().end_byte() == start + 6
        }));
    }
}

#[test]
fn vba_array_parameter_binding_preserves_unknown_or_invalid_shapes() {
    for parameters in [
        "ByVal items() As Long",
        "ByRef items As Variant",
        "ByVal items As Object",
        "ByRef items(2) As Long",
        "ByVal items( _\n ) As Long",
        "ByRef items( _\n 2) As Long",
        "ByRef items() As Long, items As Variant",
    ] {
        let source =
            format!("Sub Run({parameters})\nDim first As Variant\nfirst = items(0)\nEnd Sub\n");
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find("items(0)").unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        assert!(
            matches!(application.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.source.span() == application.source.span()
                && gap.detail == "vba-call-or-index-target-unavailable"
        }));
    }
}

#[test]
fn vba_conditional_array_declaration_does_not_choose_a_compile_branch() {
    let source = "Sub Run()\n#If Flag Then\nDim values(2) As Long\n#Else\nDim values As Object\n#End If\nDim output As Long\noutput = values(1)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let start = u64::try_from(source.find("values(1)").unwrap()).unwrap();
    let application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == start
        })
        .unwrap();
    assert!(matches!(
        application.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "vba-call-or-index-target-unavailable"
            && gap.source.span() == application.source.span()
    }));
}

#[test]
fn vba_function_values_do_not_guess_required_arguments_or_sub_results() {
    for declaration in [
        "Function Measure(ByVal input As Long) As Long\nEnd Function",
        "Function Measure(ByVal input As Long, Optional extra As Long = 1) As Long\nEnd Function",
        "Function Measure(ByVal input As Long, ParamArray rest() As Variant) As Long\nEnd Function",
        "Sub Measure()\nEnd Sub",
    ] {
        for expression in ["mEaSuRe", "Me.mEaSuRe"] {
            let source = format!(
                "VERSION 1.0 CLASS\nAttribute VB_Name = \"Meter\"\n{declaration}\nSub Run()\nDim value As Long\nvalue = {expression}\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let start = u64::try_from(source.find("mEaSuRe").unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == start + 7
                })
                .unwrap();
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{declaration}: {expression}"
            );
            assert!(!document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
            }));
        }
    }
}

#[test]
fn vba_function_values_keep_ambiguous_and_nonvalue_uses_unproven() {
    for (declaration, statement, marker) in [
        (
            "Function Measure() As Long\nEnd Function",
            "mEaSuRe = 1",
            "mEaSuRe",
        ),
        (
            "Function Measure() As Long\nEnd Function",
            "value = mEaSuRe$",
            "mEaSuRe$",
        ),
        (
            "#If Available Then\nFunction Measure() As Long\nEnd Function\n#End If",
            "value = mEaSuRe",
            "mEaSuRe",
        ),
        (
            "Function Measure() As Long\nEnd Function\nFunction Measure() As Long\nEnd Function",
            "value = mEaSuRe",
            "mEaSuRe",
        ),
    ] {
        let source = format!("{declaration}\nSub Run()\nDim value As Long\n{statement}\nEnd Sub\n");
        let result = output(&source);
        let site = reference_at(result.document(), &source, marker);
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert_ne!(site.role, OccurrenceRole::CallSite);
    }
}

#[test]
fn vba_function_values_preserve_result_and_local_shadowing() {
    let source = "Function Measure() As Long\nMeasure = 1\nMeasure = mEaSuRe + 1\nEnd Function\nSub Run(Optional Measure As Long = 1)\nDim value As Long\nvalue = MEASURE\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let result_read = reference_at(document, source, "mEaSuRe +");
    let function = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "Measure")
        .unwrap();
    assert_eq!(
        result_read.target,
        OccurrenceTarget::Resolved {
            symbol: function.id
        }
    );
    assert_eq!(result_read.role, OccurrenceRole::Reference);
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "Measure")
        .unwrap();
    let local_read = reference_at(document, source, "MEASURE\n");
    assert_eq!(
        local_read.target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
    assert_eq!(local_read.role, OccurrenceRole::Reference);
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::CallSite)
    );
}

#[test]
fn vba_self_property_access_selects_get_let_and_set_sources() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate stored As Variant\nPublic Property Get Item() As Variant\nItem = stored\nEnd Property\nPublic Property Let Item(ByVal value As Variant)\nstored = value\nEnd Property\nPublic Property Set Item(ByVal value As Object)\nSet stored = value\nEnd Property\nPublic Sub Run(ByVal value As Object)\nDim Item As Variant\nItem = Me.iTeM\nLet Me.[Item] = 4\nSet Me.ITEM = value\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (declaration, access, spelling) in [
        ("Get Item", "Me.iTeM", "iTeM"),
        ("Let Item", "Me.[Item]", "[Item]"),
        ("Set Item", "Me.ITEM", "ITEM"),
    ] {
        let definition_start = u64::try_from(source.find(declaration).unwrap() + 4).unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == definition_start
            })
            .unwrap();
        let start = u64::try_from(source.find(access).unwrap() + 3).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(spelling.len()).unwrap()
            })
            .unwrap();
        assert_eq!(site.target, definition.target, "{access}");
        assert_eq!(site.role, OccurrenceRole::CallSite, "{access}");
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("accessor must be source-bound")
        };
        assert!(document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(site.id)
                && edge.object == RelationEndpoint::Entity(symbol)
        }));
    }
}

#[test]
fn vba_self_call_forms_resolve_only_the_declared_callee() {
    for invocation in [
        "Me.Visit 1",
        "Call Me.Visit(1)",
        "Me.Visit(1)",
        "Call Me.[Visit](1)",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Counter\"\nPrivate Sub Visit(ByVal value As Long)\nEnd Sub\nPublic Sub Run()\n{invocation}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Visit")
            .unwrap();
        let start = u64::try_from(source.rfind("Me.").unwrap() + 3).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{invocation}"
        );
        assert_eq!(site.role, OccurrenceRole::CallSite, "{invocation}");
        assert_eq!(
            site.source.span().end_byte() - start,
            if invocation.contains('[') { 7 } else { 5 }
        );
    }
}

#[test]
fn vba_self_properties_preserve_read_and_indexed_assignment_contexts() {
    let declarations = "Public Property Get Item(ByVal index As Long) As Variant\nItem = index\nEnd Property\nPublic Property Let Item(ByVal index As Long, ByVal value As Variant)\nEnd Property\nPublic Property Set Item(ByVal index As Long, ByVal value As Object)\nEnd Property\n";
    for (statement, accessor) in [
        ("value = Me.Item(0)", "Get Item"),
        ("Let Me.Item(0) = value", "Let Item"),
        ("Me.Item(0) = value", "Let Item"),
        ("Set Me.Item(0) = value", "Set Item"),
        ("If Me.Item(0) Then\nEnd If", "Get Item"),
        ("value = 1 + Me.Item(0)", "Get Item"),
        ("value = (Me.Item(0))", "Get Item"),
        ("Visit Me.Item(0)", "Get Item"),
        ("Set value = Me.Item(0)", "Get Item"),
        ("Me.Item(0).Name = value", "Get Item"),
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\n{declarations}Sub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let definition_start = u64::try_from(source.find(accessor).unwrap() + 4).unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == definition_start
            })
            .unwrap();
        let start = u64::try_from(source.rfind("Me.Item").unwrap() + 3).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 4
            })
            .unwrap();
        assert_eq!(site.target, definition.target, "{statement}");
        assert_eq!(site.role, OccurrenceRole::CallSite, "{statement}");
    }
}

#[test]
fn vba_self_value_calls_and_indexed_fields_keep_distinct_roles() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Holder\"\nPrivate items(2) As Variant\nFunction Measure(ByVal index As Long) As Long\nMeasure = index\nEnd Function\nSub Run(ByVal value As Variant)\nvalue = Me.Measure(1)\nLet Me.items(0) = value\nSet Me.items(1) = value\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (marker, role) in [
        ("Measure(1)", OccurrenceRole::CallSite),
        ("items(0)", OccurrenceRole::Reference),
        ("items(1)", OccurrenceRole::Reference),
    ] {
        let start = u64::try_from(source.rfind(marker).unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Resolved { .. }),
            "{marker}"
        );
        assert_eq!(site.role, role, "{marker}");
        if role == OccurrenceRole::Reference {
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span().start_byte() == start
                        && gap.detail == "vba-call-or-index-target-unavailable")
            );
        }
    }
}

#[test]
fn vba_self_properties_do_not_guess_missing_or_ambiguous_accessors() {
    let get = "Public Property Get Item() As Variant\nItem = 1\nEnd Property\n";
    let set = "Public Property Set Item(ByVal value As Object)\nEnd Property\n";
    for (class, declarations, statement) in [
        (false, get.to_owned(), "value = Me.Item"),
        (true, get.to_owned(), "Me.Item = 2"),
        (true, get.to_owned(), "Set Me.Item = value"),
        (true, set.to_owned(), "value = Me.Item"),
        (true, get.repeat(2), "value = Me.Item"),
        (
            true,
            format!("{get}Private Item As Variant\n"),
            "value = Me.Item",
        ),
        (
            true,
            format!("Private Item As Variant\n{get}"),
            "value = Me.Item",
        ),
        (
            true,
            format!("#If Enabled Then\n{get}#End If\n"),
            "value = Me.Item",
        ),
        (true, get.to_owned(), "ReDim Me.Item(3)"),
        (true, get.to_owned(), "value = Me.Item$"),
        (true, get.to_owned(), "Call Me.Item()"),
    ] {
        let prefix = if class { "VERSION 1.0 CLASS\n" } else { "" };
        let source = format!(
            "{prefix}Attribute VB_Name = \"Holder\"\n{declarations}Sub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.rfind("Me.Item").unwrap() + 3).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == start
                    && gap.domain == FactDomain::Relations),
            "{source}"
        );
    }
}

#[test]
fn vba_self_members_keep_unproven_owners_and_targets_explicit() {
    for (prefix, declarations, access) in [
        ("", "Private total As Long\n", "Me.total"),
        (
            "VERSION 1.0 CLASS\n",
            "Private total As Long\n",
            "other.total",
        ),
        ("VERSION 1.0 CLASS\n", "Private total As Long\n", "Me!total"),
        (
            "VERSION 1.0 CLASS\n",
            "Private total As Long\n",
            "Me.total$",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private total As Long\nPrivate TOTAL As Long\n",
            "Me.total",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "#If Wide Then\nPrivate total As Long\n#End If\n",
            "Me.total",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private Const total = 1\n",
            "Me.total",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private Declare PtrSafe Function total Lib \"clock\" () As LongPtr\n",
            "Me.total()",
        ),
    ] {
        let source = format!(
            "{prefix}Attribute VB_Name = \"Counter\"\n{declarations}Public Sub Run()\nDim value As Variant\nvalue = {access}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.rfind(access).unwrap()).unwrap();
        let end = start + u64::try_from(access.len()).unwrap();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() >= start
                    && site.source.span().end_byte() <= end
                    && site.syntax_kind.as_str() != "vba.instance.reference"
            })
            .collect();
        assert!(!sites.is_empty(), "{source}");
        assert!(
            sites
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() >= start
                    && gap.source.span().end_byte() <= end
                    && matches!(
                        gap.detail.as_str(),
                        "vba-member-target-unavailable" | "vba-call-or-index-target-unavailable"
                    )),
            "{source}"
        );
    }
}

#[test]
fn vba_self_indexed_fields_bind_array_applications_without_method_calls() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Counter\"\nPrivate items(1) As Long\nPublic Sub Run()\nDim value As Long\nvalue = Me.items(0)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let start = u64::try_from(source.rfind("items(0)").unwrap()).unwrap();
    let field = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "items")
        .unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| site.source.span().start_byte() == start)
        .unwrap();
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
    assert_eq!(site.role, OccurrenceRole::Reference);
    let application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == start - 3
                && site.source.span().end_byte() == start + 8
        })
        .unwrap();
    assert_eq!(
        application.target,
        OccurrenceTarget::Resolved { symbol: field.id }
    );
    assert_eq!(application.role, OccurrenceRole::Reference);
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Calls
            && matches!(edge.subject, RelationEndpoint::Occurrence(id) if id == site.id || id == application.id)
    }));
    assert!(document.skipped_regions.iter().all(
        |gap| !(matches!(gap.source.span().start_byte(), at if at == start || at == start - 3)
            && gap.detail == "vba-call-or-index-target-unavailable")
    ));
}

#[test]
fn vba_self_array_reads_preserve_spelling_and_bypass_local_shadows() {
    for expression in [
        "Me.items(0)",
        "mE . [items](0)",
        "Me. _\nitems(0)",
        "Me.items(Me.items(0))",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Counter\"\nPrivate items(1) As Long\nPublic Sub Run(ByVal items As Object)\nDim value As Long\nvalue = {expression}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let field = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "items")
            .unwrap();
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        assert_eq!(
            application.target,
            OccurrenceTarget::Resolved { symbol: field.id },
            "{source}"
        );
        assert_eq!(application.role, OccurrenceRole::Reference);
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-call-or-index-target-unavailable"
                    && gap.source.span().start_byte() >= start
                    && gap.source.span().end_byte() <= end),
            "{source}"
        );
    }
}

#[test]
fn vba_self_array_reads_require_class_array_and_direct_application_evidence() {
    for (header, declarations, expression) in [
        ("", "Private items(1) As Long", "Me.items(0)"),
        (
            "VERSION 1.0 CLASS\n",
            "Private items As Object",
            "Me.items(0)",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private items(1) As Long\nPrivate items(2) As Long",
            "Me.items(0)",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private items(1) As Long",
            "[Me].items(0)",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private items(1) As Long",
            "Me.items(0)(1)",
        ),
    ] {
        let source = format!(
            "{header}Attribute VB_Name = \"Counter\"\n{declarations}\nPublic Sub Run()\nDim value As Long\nvalue = {expression}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find(expression).unwrap()).unwrap();
        let end = start + u64::try_from(expression.len()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
            })
            .unwrap();
        assert!(
            matches!(application.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-call-or-index-target-unavailable"
                    && gap.source == application.source),
            "{source}"
        );
    }
}

#[test]
fn vba_self_members_bypass_locals_and_preserve_exact_name_sources() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Counter\"\nPrivate total As Long\nPrivate Sub Refresh()\nEnd Sub\nPublic Sub Run()\nDim total As Long\nDim Refresh As Long\ntotal = 1\nMe.total = total\nCall mE.ReFrEsH()\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let class = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class)
        .unwrap();
    for (marker, name, kind, role) in [
        (
            "total = total",
            "total",
            EntityKind::Field,
            OccurrenceRole::Reference,
        ),
        (
            "ReFrEsH()",
            "Refresh",
            EntityKind::Method,
            OccurrenceRole::CallSite,
        ),
    ] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap();
        assert_eq!(target.container, Some(ContainerRef::Entity(class.id)));
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + u64::try_from(name.len()).unwrap()
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(site.role, role);
    }
}

#[test]
fn vba_native_class_headers_preserve_members_sources_and_body_identity() {
    let source = "Attribute VB_Name = \"Counter\"\nAttribute VB_GlobalNameSpace = False\nAttribute VB_Creatable = False\nAttribute VB_PredeclaredId = False\nAttribute VB_Exposed = False\nPrivate total As Long\nPublic Sub Refresh()\ntotal = 1\nEnd Sub\nPublic Sub Run()\nMe.Refresh\nEnd Sub\n";
    let kind = Some(rootlight_adapter_sdk::NativeVbaModuleKind::Object);
    let first = output_with_native_kind(source, kind);
    let second_source = source.replace("total = 1", "total = 2 + 3");
    let second = output_with_native_kind(&second_source, kind);
    for (result, bytes) in [
        (&first, source.as_bytes()),
        (&second, second_source.as_bytes()),
    ] {
        let document = result.document();
        let owner = document
            .entities
            .iter()
            .find(|e| e.canonical_name == "Counter")
            .unwrap();
        assert_eq!(owner.kind, EntityKind::Class);
        for (name, kind) in [
            ("Counter", EntityKind::Class),
            ("total", EntityKind::Field),
            ("Refresh", EntityKind::Method),
            ("Run", EntityKind::Method),
        ] {
            let member = document
                .entities
                .iter()
                .find(|e| e.canonical_name == name)
                .unwrap();
            assert_eq!(member.kind, kind);
            assert_eq!(
                member.id,
                first
                    .document()
                    .entities
                    .iter()
                    .find(|e| e.canonical_name == name)
                    .unwrap()
                    .id
            );
            if name != "Counter" {
                assert_eq!(member.container, Some(ContainerRef::Entity(owner.id)));
            }
            let definition = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: member.id }
                })
                .unwrap();
            let span = definition.source.span();
            let text = &bytes[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()];
            assert_eq!(std::str::from_utf8(text).unwrap().trim_matches('"'), name);
        }
        let refresh = document
            .entities
            .iter()
            .find(|e| e.canonical_name == "Refresh")
            .unwrap();
        assert!(document.occurrences.iter().any(|site| site.target
            == OccurrenceTarget::Resolved { symbol: refresh.id }
            && site.role == OccurrenceRole::CallSite));
    }
}

#[test]
fn vba_native_module_category_requires_explicit_project_context() {
    use rootlight_adapter_sdk::NativeVbaModuleKind;
    for attributes in [
        "",
        "Attribute VB_Exposed = False\nAttribute VB_PredeclaredId = True\n",
    ] {
        let source = format!(
            "Attribute VB_Name = \"Worker\"\n{attributes}Public value As Long\nPublic Sub Run()\nEnd Sub\n"
        );
        for (context, expected) in [
            (None, EntityKind::Module),
            (Some(NativeVbaModuleKind::Procedural), EntityKind::Module),
            (Some(NativeVbaModuleKind::Object), EntityKind::Class),
        ] {
            let result = output_with_native_kind(&source, context);
            let entity = result
                .document()
                .entities
                .iter()
                .find(|e| e.canonical_name == "Worker")
                .unwrap();
            assert_eq!(entity.kind, expected);
        }
    }
}

#[test]
fn vba_class_exports_preserve_typed_members_and_body_independent_identity() {
    let source = "VERSION 1.0 CLASS\nBEGIN\n  MultiUse = -1\nEND\nAttribute VB_Name = \"Counter\"\nOption Explicit\nPrivate total As Long\nPrivate Declare PtrSafe Function ReadClock Lib \"clock\" () As LongPtr\nPublic Event Changed(ByVal value As Long)\nPublic Property Get Current() As Long\nCurrent = total\nEnd Property\nPublic Sub Refresh()\nDim localValue As Long\nlocalValue = 1\ntotal = localValue\nEnd Sub\nPublic Sub Run()\nRefresh\nEnd Sub\n";
    let first = output(source);
    let second = output(&source.replace("localValue = 1", "localValue = 2 + 3"));
    for result in [&first, &second] {
        let document = result.document();
        let class = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Counter")
            .unwrap();
        assert_eq!(class.kind, EntityKind::Class);
        for (name, kind) in [
            ("total", EntityKind::Field),
            ("ReadClock", EntityKind::Function),
            ("Changed", EntityKind::Event),
            ("Current", EntityKind::Property),
            ("Refresh", EntityKind::Method),
            ("Run", EntityKind::Method),
        ] {
            let member = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            assert_eq!(member.kind, kind, "{name}");
            assert_eq!(
                member.container,
                Some(ContainerRef::Entity(class.id)),
                "{name}"
            );
            let original = first
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            assert_eq!(member.id, original.id, "{name}");
        }
        assert_eq!(
            class.id,
            first
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Counter")
                .unwrap()
                .id
        );
        let refresh = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Refresh")
            .unwrap();
        let local = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "localValue")
            .unwrap();
        assert_eq!(local.kind, EntityKind::Variable);
        assert_eq!(local.container, Some(ContainerRef::Entity(refresh.id)));
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && site.target == OccurrenceTarget::Resolved { symbol: refresh.id })
        );
        let definition = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.module_name.definition")
            .unwrap();
        assert_eq!(
            definition.target,
            OccurrenceTarget::Resolved { symbol: class.id }
        );
        assert_eq!(
            definition.source.span().start_byte(),
            u64::try_from(source.find("\"Counter\"").unwrap()).unwrap()
        );
    }
}

#[test]
fn vba_exported_module_names_preserve_authored_identity_and_source() {
    for (prefix, kind) in [
        ("", EntityKind::Module),
        (
            "VERSION 1.0 CLASS\r\nBEGIN\r\n  MultiUse = -1\r\nEND\r\n",
            EntityKind::Class,
        ),
    ] {
        let source = format!(
            "{prefix}Attribute VB_Name = \"Metrics\"\r\nOption Explicit\r\nSub Run()\r\nEnd Sub\r\n"
        );
        let result = output(&source);
        let document = result.document();
        let module = document
            .entities
            .iter()
            .find(|entity| entity.kind == kind)
            .unwrap();
        assert_eq!(module.canonical_name, "Metrics");
        let definition = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.module_name.definition")
            .unwrap();
        assert_eq!(
            definition.target,
            OccurrenceTarget::Resolved { symbol: module.id }
        );
        let start = source.find("\"Metrics\"").unwrap();
        assert_eq!(
            definition.source.span().start_byte(),
            u64::try_from(start).unwrap()
        );
        assert_eq!(
            definition.source.span().end_byte(),
            u64::try_from(start + "\"Metrics\"".len()).unwrap()
        );
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "Run")
        );
    }
}

#[test]
fn vba_procedural_module_qualifier_resolves_own_member_without_hiding_a_local() {
    for shadow in [
        "",
        "Dim Utility As Variant\n",
        "#If FEATURE Then\nDim Utility As Variant\n#End If\n",
    ] {
        let source = format!(
            "Attribute VB_Name = \"Utility\"\nPublic Function Measure(ByVal value As Long) As Long\nMeasure = value\nEnd Function\nPublic Sub Run()\n{shadow}Dim result As Long\nresult = Utility.Measure(1)\nEnd Sub\n"
        );
        let document = output(&source);
        let document = document.document();
        let module = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Utility")
            .unwrap();
        let measure = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Function && entity.canonical_name == "Measure"
            })
            .unwrap();
        let start = u64::try_from(source.find("Utility.Measure(1)").unwrap()).unwrap();
        let qualifier = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 7
                    && site.syntax_kind == "vba.value_identifier.reference"
            })
            .unwrap();
        let member = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 15
                    && site.syntax_kind == "vba.invoked_get_member.reference"
            })
            .unwrap();
        if shadow.is_empty() {
            assert_eq!(
                qualifier.target,
                OccurrenceTarget::Resolved { symbol: module.id }
            );
            assert_eq!(
                member.target,
                OccurrenceTarget::Resolved { symbol: measure.id }
            );
            assert_eq!(member.role, OccurrenceRole::CallSite);
            assert!(document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::Calls
                    && relation.subject == RelationEndpoint::Occurrence(member.id)
                    && relation.object == RelationEndpoint::Entity(measure.id)
            }));
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.source == qualifier.source && gap.detail == "vba-binding-target-unavailable"
                    || gap.source == member.source && gap.detail == "vba-member-target-unavailable"
            }));
        } else {
            assert_ne!(
                qualifier.target,
                OccurrenceTarget::Resolved { symbol: module.id }
            );
            assert_ne!(
                member.target,
                OccurrenceTarget::Resolved { symbol: measure.id }
            );
        }
    }
}

#[test]
fn vba_module_qualified_applications_bind_exact_arguments() {
    for (statement, application) in [
        (
            "result = Utility.Measure(value:=1)",
            "Utility.Measure(value:=1)",
        ),
        (
            "Call Utility.Measure(value:=1)",
            "Call Utility.Measure(value:=1)",
        ),
        ("Utility.Measure value:=1", "Utility.Measure value:=1"),
        (
            "Call [Utility].Measure _\n ([VALUE]:=1)",
            "Call [Utility].Measure _\n ([VALUE]:=1)",
        ),
        (
            "result = Utility.Measure(value:=Utility.Measure(value:=2))",
            "Utility.Measure(value:=Utility.Measure(value:=2))",
        ),
    ] {
        let source = format!(
            "Attribute VB_Name = \"Utility\"\nPublic Function Measure(ByVal value As Long) As Long\nMeasure = value\nEnd Function\nPublic Sub Run(ByVal value As Long)\nDim result As Long\n{statement}\nEnd Sub\n"
        );
        let output = output(&source);
        let document = output.document();
        let target = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Function && entity.canonical_name == "Measure"
            })
            .unwrap();
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == "value"
                    && entity.container == Some(ContainerRef::Entity(target.id))
            })
            .unwrap();
        let start = u64::try_from(source.find(application).unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(application.len()).unwrap()
            })
            .unwrap_or_else(|| panic!("missing application: {statement}"));
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{statement}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(!document.skipped_regions.iter().any(|gap| {
            gap.source == site.source && gap.detail == "vba-call-or-index-target-unavailable"
        }));
        let labels: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "vba.nonvalue_identifier.reference")
            .collect();
        assert_eq!(labels.len(), statement.matches(":=").count(), "{statement}");
        for label in labels {
            assert_eq!(
                label.target,
                OccurrenceTarget::Resolved {
                    symbol: parameter.id
                },
                "{statement}"
            );
            assert_eq!(label.role, OccurrenceRole::Reference);
            assert!(document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.object == RelationEndpoint::Entity(parameter.id)
                    && edge.evidence.source.as_ref() == Some(&label.source)
            }));
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.source == label.source && gap.detail == "vba-nonvalue-target-unavailable"
            }));
        }
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Occurrence(site.id)
        }));
    }
}

#[test]
fn vba_module_qualified_applications_reject_unproven_contracts() {
    for (shadow, arguments) in [
        ("", "value:=1, value:=2"),
        ("", "value:=1, missing:=2"),
        ("", ""),
        ("", "value:=1, 2"),
        ("Dim Utility As Variant\n", "value:=1"),
        (
            "#If FEATURE Then\nDim Utility As Variant\n#End If\n",
            "value:=1",
        ),
    ] {
        let invocation = format!("Utility.Measure({arguments})");
        let source = format!(
            "Attribute VB_Name = \"Utility\"\nPublic Function Measure(ByVal value As Long) As Long\nMeasure = value\nEnd Function\nPublic Sub Run()\n{shadow}Dim result As Long\nresult = {invocation}\nEnd Sub\n"
        );
        let output = output(&source);
        let document = output.document();
        let start = u64::try_from(source.find(&invocation).unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        for site in std::iter::once(application).chain(
            document
                .occurrences
                .iter()
                .filter(|site| site.syntax_kind == "vba.nonvalue_identifier.reference"),
        ) {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{invocation}: {shadow}: {site:?}"
            );
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source)
            );
        }
    }
}

#[test]
fn vba_module_qualified_return_fields_require_complete_applications() {
    for (arguments, proven) in [
        ("value:=1", true),
        ("1", true),
        ("", false),
        ("missing:=1", false),
        ("value:=1, value:=2", false),
    ] {
        let expression = format!("Utility.Measure({arguments}).X");
        let source = format!(
            "Attribute VB_Name = \"Utility\"\nPrivate Type Coordinate\nX As Long\nEnd Type\nPublic Function Measure(ByVal value As Long) As Coordinate\nEnd Function\nPublic Sub Run()\nDim result As Long\nresult = {expression}\nEnd Sub\n"
        );
        let output = output(&source);
        let document = output.document();
        let field = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Field && entity.canonical_name == "X")
            .unwrap();
        let start = u64::try_from(source.find(&expression).unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.get_member.reference"
                    && site.source.span().start_byte() == start
                    && site.source.span().end_byte()
                        == start + u64::try_from(expression.len()).unwrap()
            })
            .unwrap();
        if proven {
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: field.id },
                "{expression}"
            );
        } else {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{expression}"
            );
        }
    }
}

#[test]
fn vba_procedural_module_qualifier_requires_an_unambiguous_header_and_member() {
    for (prefix, target) in [
        ("Attribute VB_Name = \"Invalid.Name\"\n", "Measure"),
        (
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Utility\"\n",
            "Measure",
        ),
        ("Attribute VB_Name = \"Utility\"\n", "Missing"),
    ] {
        let source = format!(
            "{prefix}Public Function Measure(ByVal value As Long) As Long\nMeasure = value\nEnd Function\nPublic Sub Run()\nDim result As Long\nresult = Utility.{target}(1)\nresult = Utility\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find(&format!("Utility.{target}(1)")).unwrap()).unwrap();
        let member = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 15
                    && site.syntax_kind == "vba.invoked_get_member.reference"
            })
            .unwrap();
        assert!(matches!(member.target, OccurrenceTarget::Unresolved { .. }));
        let bare =
            u64::try_from(source.find("result = Utility\n").unwrap() + "result = ".len()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == bare
                    && site.source.span().end_byte() == bare + 7
                    && site.syntax_kind == "vba.value_identifier.reference"
            })
            .unwrap();
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    }
}

#[test]
fn vba_member_paths_preserve_logical_continuations() {
    for separator in [".", " _\n.", ". _\r\n ", " _\r\n. _\n "] {
        let call = format!("Utility{separator}[Measure]");
        let field = format!("item{separator}nested{separator}[Value]");
        let constant = format!("Phase{separator}[Ready]");
        let source = format!(
            "Attribute VB_Name = \"Utility\"\nPrivate Type Inner\nValue As Long\nEnd Type\nPrivate Type Outer\nnested As Inner\nEnd Type\nPrivate Enum Phase\nReady = 1\nEnd Enum\nFunction Measure() As Long\nMeasure = 1\nEnd Function\nSub Run()\nDim item As Outer\nDim result As Long\nresult = {call}()\nresult = {field}\nresult = {constant}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        for (text, name, role) in [
            ("Utility", "Utility", OccurrenceRole::Reference),
            (call.as_str(), "Measure", OccurrenceRole::CallSite),
            (field.as_str(), "Value", OccurrenceRole::Reference),
            (constant.as_str(), "Ready", OccurrenceRole::Reference),
        ] {
            let start = u64::try_from(source.rfind(text).unwrap()).unwrap();
            let end = start + u64::try_from(text.len()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == end
                        && matches!(
                            site.syntax_kind.as_str(),
                            "vba.get_member.reference"
                                | "vba.invoked_get_member.reference"
                                | "vba.value_identifier.reference"
                        )
                })
                .expect("complete authored member path");
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            assert_eq!(
                site.target,
                OccurrenceTarget::Resolved { symbol: entity.id },
                "{text:?}"
            );
            assert_eq!(site.role, role, "{text:?}");
            assert!(
                !document.skipped_regions.iter().any(|gap| {
                    gap.source == site.source && gap.detail == "vba-member-target-unavailable"
                }),
                "{text:?}"
            );
        }
    }
}

#[test]
fn vba_continued_self_field_paths_bypass_local_receivers() {
    let text = "Me _\n.item. _\r\n [Value]";
    let source = format!(
        "VERSION 1.0 CLASS\nAttribute VB_Name = \"Reader\"\nPrivate Type Data\nValue As Long\nEnd Type\nPrivate item As Data\nSub Run(ByVal item As Object)\nDim result As Long\nresult = {text}\nEnd Sub\n"
    );
    let result = output(&source);
    let document = result.document();
    let start = u64::try_from(source.find(text).unwrap()).unwrap();
    let end = start + u64::try_from(text.len()).unwrap();
    let site = document
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start && site.source.span().end_byte() == end
        })
        .unwrap();
    let field = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Value")
        .unwrap();
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: field.id });
    assert_eq!(site.role, OccurrenceRole::Reference);
}

#[test]
fn vba_continued_member_paths_preserve_shadowing_and_unknown_targets() {
    for (prefix, local, path) in [
        ("", "Dim Utility As Object\n", "Utility _\n.Measure"),
        ("", "", "Unknown _\n.Measure"),
        ("", "", "Utility. _\n Missing"),
        ("", "Dim item As Object\n", "item _\n.Value"),
        ("", "Dim item As Data\n", "item _\n.Missing"),
        ("#If Enabled Then\n", "", "Utility _\n.Measure"),
    ] {
        let conditional_end = if prefix.is_empty() { "" } else { "#End If\n" };
        let source = format!(
            "Attribute VB_Name = \"Utility\"\nPrivate Type Data\nValue As Long\nEnd Type\n{prefix}Function Measure() As Long\nEnd Function\n{conditional_end}Sub Run()\n{local}Dim result As Variant\nresult = {path}(0)\nEnd Sub\n"
        );
        let result = output(&source);
        let start = u64::try_from(source.find(path).unwrap()).unwrap();
        let members: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == start
                    && matches!(
                        site.syntax_kind.as_str(),
                        "vba.invoked_get_member.reference"
                    )
            })
            .collect();
        assert!(!members.is_empty(), "{path}");
        assert!(
            members
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "{path}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == start),
            "{path}"
        );
    }
}

#[test]
fn vba_exported_module_header_uses_last_attribute_not_body_attributes() {
    let source = "Attribute VB_Name = \"OldName\"\nAttribute vb_name = \"CurrentName\"\nSub Run()\nAttribute Run.VB_Name = \"NotModule\"\nEnd Sub\nAttribute VB_Name = \"AfterBody\"\n";
    let result = output(source);
    let document = result.document();
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    assert_eq!(module.canonical_name, "CurrentName");
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "vba.module_name.definition")
            .count(),
        1
    );
}

#[test]
fn vba_unproven_module_names_keep_file_identity_and_scoped_gaps() {
    for name in [
        "\"Invalid.Name\"",
        "\"Name With Space\"",
        "\"9Name\"",
        "\"Δelta\"",
        "\"abcdefghijklmnopqrstuvwxyzABCDEF\"",
        "42",
    ] {
        let source = format!(
            "Attribute VB_Name = \"Earlier\"\nAttribute VB_Name = {name}\nSub Run()\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let module = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Module)
            .unwrap();
        assert_eq!(module.canonical_name, VBA.path, "{name}");
        assert!(
            document
                .occurrences
                .iter()
                .all(|site| site.syntax_kind != "vba.module_name.definition")
        );
        let start = u64::try_from(source.find(&format!("= {name}")).unwrap() + 2).unwrap();
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-module-name-unavailable"
                    && gap.source.span().start_byte() == start),
            "{name}: {:?}",
            document.skipped_regions
        );
    }
}

#[test]
fn vba_direct_procedure_calls_bind_written_names_across_invocation_forms() {
    let source = "Sub Visit(ByVal value As Long)\nEnd Sub\nFunction Measure(ByVal value As Long) As Long\nMeasure = value\nEnd Function\nSub Clear()\nEnd Sub\nSub Run()\nCall VISIT(1)\nvisit 2\nDim result As Long\nresult = MEASURE(3)\nClear\nCall CLEAR\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 5, "{:?}", document.occurrences);
    for (site, (marker, target)) in calls.into_iter().zip([
        ("VISIT(1)", "Visit"),
        ("visit 2", "Visit"),
        ("MEASURE(3)", "Measure"),
        ("Clear\n", "Clear"),
        ("CLEAR\n", "Clear"),
    ]) {
        let symbol = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == target)
            .unwrap()
            .id;
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
        let start = source.find(marker).unwrap();
        assert_eq!(
            site.source.span().start_byte(),
            u64::try_from(start).unwrap()
        );
        assert_eq!(
            site.source.span().end_byte(),
            u64::try_from(start + target.len()).unwrap()
        );
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.object == RelationEndpoint::Entity(symbol)
                    && edge.subject == RelationEndpoint::Occurrence(site.id))
        );
    }
}

#[test]
fn vba_recursive_function_invocation_is_not_its_result_variable() {
    let source = "Function Reduce(ByVal value As Long) As Long\nIf value > 0 Then\nReduce = REDUCE(value - 1)\nElse\nReduce = 0\nEnd If\nEnd Function\n";
    let result = output(source);
    let document = result.document();
    let symbol = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Reduce")
        .unwrap()
        .id;
    let calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::CallSite)
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].target, OccurrenceTarget::Resolved { symbol });
    assert_eq!(
        calls[0].source.span().start_byte(),
        u64::try_from(source.find("REDUCE(").unwrap()).unwrap()
    );
    for site in document.occurrences.iter().filter(|site| {
        matches!(
            site.syntax_kind.as_str(),
            "vba.identifier.reference" | "vba.let_identifier.reference"
        ) && source.get(
            usize::try_from(site.source.span().start_byte()).unwrap()
                ..usize::try_from(site.source.span().end_byte()).unwrap(),
        ) == Some("Reduce")
    }) {
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
        assert_eq!(site.role, OccurrenceRole::Reference);
    }
}

#[test]
fn vba_conditional_procedure_declarations_do_not_become_unconditional_targets() {
    let source =
        "#If Available Then\nSub Visit()\nEnd Sub\n#End If\nSub Run()\nCall VISIT()\nEnd Sub\n";
    let result = output(source);
    let start = u64::try_from(source.find("VISIT()").unwrap()).unwrap();
    let site = result
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start && site.source.span().end_byte() == start + 5
        })
        .unwrap();
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn vba_callable_spelling_does_not_bypass_local_value_shadowing() {
    let source = "Sub Visit(ByVal value As Long)\nEnd Sub\nSub Run(ByVal visit As Object)\nCall VISIT(1)\nEnd Sub\nSub Read()\nDim Visit(2) As Long\nVisit(1) = 2\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::CallSite)
    );
    for marker in ["VISIT(1)", "Visit(1)"] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 5
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("callee value must retain its lexical binding")
        };
        assert!(document.entities.iter().any(|entity| entity.id == symbol
            && matches!(entity.kind, EntityKind::Variable | EntityKind::Parameter)));
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == start
                    && gap.domain == FactDomain::Relations),
            marker == "VISIT(1)"
        );
    }
}

#[test]
fn vba_authored_declarations_keep_exact_utf8_name_spans() {
    let source = "' λ😀 preserves UTF-8 coordinates\nOption Explicit\nPrivate total As Long, other As String\nPublic Const Limit As Long = 4\nPublic Type Sample\nValue As Long\nEnd Type\nPublic Enum Mode\nIdle = 0\nActive = 1\nEnd Enum\nPublic Event Changed(ByVal current As Long)\nPublic Sub Reset()\nLet total = 0\nEnd Sub\nPublic Function Measure(ByVal value As Long) As Long\nMeasure = value + Limit\nEnd Function\n";
    for source in [source.to_owned(), source.replace('\n', "\r\n")] {
        let result = output(&source);
        for (name, kind) in [
            ("total", EntityKind::Variable),
            ("other", EntityKind::Variable),
            ("Limit", EntityKind::Constant),
            ("Sample", EntityKind::Struct),
            ("Value", EntityKind::Field),
            ("Mode", EntityKind::Enum),
            ("Idle", EntityKind::Constant),
            ("Active", EntityKind::Constant),
            ("Changed", EntityKind::Event),
            ("current", EntityKind::Parameter),
            ("Reset", EntityKind::Function),
            ("Measure", EntityKind::Function),
            ("value", EntityKind::Parameter),
        ] {
            let entities: Vec<_> = result
                .document()
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == name && entity.kind == kind)
                .collect();
            assert_eq!(
                entities.len(),
                1,
                "{name}: {:?}",
                result.document().entities
            );
            let definitions: Vec<_> = result
                .document()
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target
                            == OccurrenceTarget::Resolved {
                                symbol: entities[0].id,
                            }
                })
                .collect();
            assert_eq!(definitions.len(), 1, "definition for {name}");
            let span = definitions[0].source.span();
            assert_eq!(
                &source[usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap()],
                name
            );
        }
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| !matches!(entity.canonical_name.as_str(), "Let" | "Long" | "String"))
        );
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .all(|site| site.role != OccurrenceRole::CallSite)
        );
    }
}

#[test]
fn vba_property_accessors_have_distinct_source_bound_identities() {
    let source = "Private total As Long\nPublic Property Get Value() As Long\nValue = total\nEnd Property\nPublic Property Let Value(ByVal nextValue As Long)\ntotal = nextValue\nEnd Property\nPublic Property Set Item(ByVal nextItem As Object)\nSet stored = nextItem\nEnd Property\n";
    let result = output(source);
    let properties: Vec<_> = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Property)
        .collect();
    assert_eq!(properties.len(), 3, "{:?}", result.document().entities);
    assert_eq!(
        properties
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        3
    );
    assert_eq!(
        properties
            .iter()
            .filter(|entity| entity.canonical_name == "Value")
            .count(),
        2
    );
}

#[test]
fn vba_body_only_changes_preserve_declared_symbol_identities() {
    let before = output(VBA.source);
    let after = output(&VBA.source.replace(VBA.body_before, VBA.body_after));
    assert_eq!(symbol_ids(before.document()), symbol_ids(after.document()));
    let fixture = Fixture::new(VBA, VBA.source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, VBA, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let signatures: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "vba.function.signature")
        .map(|fact| {
            &VBA.source[usize::try_from(fact.span().start_byte()).unwrap()
                ..usize::try_from(fact.span().end_byte()).unwrap()]
        })
        .collect();
    assert_eq!(
        signatures,
        ["Public Function Measure(ByVal value As Long) As Long"]
    );
    let visibility = parsed
        .facts()
        .iter()
        .find(|fact| fact.syntax_kind().as_str() == "vba.public.expression")
        .unwrap();
    assert_eq!(
        &VBA.source[usize::try_from(visibility.span().start_byte()).unwrap()
            ..usize::try_from(visibility.span().end_byte()).unwrap()],
        signatures[0]
    );
}

#[test]
fn vba_quoted_and_type_suffixed_names_preserve_written_occurrences() {
    let source = "Private [Count] As Long\nPrivate total&\nPublic Function [ReadValue]() As Long\n[ReadValue] = [Count] + total&\nEnd Function\n";
    let result = output(source);
    for (name, written) in [
        ("Count", "[Count]"),
        ("total", "total&"),
        ("ReadValue", "[ReadValue]"),
    ] {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap_or_else(|| panic!("missing {name}: {:?}", result.document().entities));
        let occurrence = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .unwrap();
        let span = occurrence.source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            written
        );
    }
}

#[test]
fn vba_module_and_parameter_containment_preserve_source_owners() {
    let result = output(VBA.source);
    let document = result.document();
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    assert_eq!(module.canonical_name, VBA.path);
    let function = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Measure")
        .unwrap();
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "value")
        .unwrap();
    assert_eq!(
        function.container,
        Some(rootlight_ir::ContainerRef::Entity(module.id))
    );
    assert_eq!(
        parameter.container,
        Some(rootlight_ir::ContainerRef::Entity(function.id))
    );
}

#[test]
fn vba_continued_headers_exclude_bodies_and_preserve_parameter_names() {
    let source = "Public Function Sum( _\nByVal left As Long, _\nByVal right As Long) As Long\nSum = left + right\nEnd Function\n";
    let before = output(source);
    let after = output(&source.replace("left + right", "left - right"));
    assert_eq!(symbol_ids(before.document()), symbol_ids(after.document()));
    for name in ["Sum", "left", "right"] {
        assert!(
            before
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
}

#[test]
fn vba_incomplete_declaration_retains_explicit_parse_gaps() {
    let result = output("Public Function Broken(ByVal value As Long\n");
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(!result.document().skipped_regions.is_empty());
}

#[test]
fn vba_conditional_headers_do_not_silently_choose_one_identity() {
    let source = "#If Wide Then\nPublic Function ReadValue() As Long\n#Else\nPublic Function ReadValue() As Integer\n#End If\nReadValue = 1\nEnd Function\n";
    let fixture = Fixture::new(VBA, source.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, VBA, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider(),
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    assert_eq!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert_eq!(
        parsed
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.identifier.definition")
            .count(),
        2
    );
    let result = output(source);
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(!result.document().skipped_regions.is_empty());
    assert!(
        result
            .document()
            .entities
            .iter()
            .all(|entity| entity.canonical_name != "ReadValue")
    );
}

#[test]
fn vba_external_procedures_retain_names_without_executing_libraries() {
    let source = "Private Declare PtrSafe Function ReadClock Lib \"platform\" Alias \"Clock\" () As LongPtr\nPrivate Declare PtrSafe Sub PauseClock Lib \"platform\" (ByVal ticks As Long)\n";
    let result = output(source);
    for name in ["ReadClock", "PauseClock"] {
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Function && entity.canonical_name == name)
        );
    }
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|site| site.role != OccurrenceRole::CallSite)
    );
}

fn reference_at<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    source: &str,
    marker: &str,
) -> &'a rootlight_ir::OccurrenceRecord {
    let start = u64::try_from(source.find(marker).expect("reference marker exists")).unwrap();
    document
        .occurrences
        .iter()
        .find(|site| {
            matches!(
                site.syntax_kind.as_str(),
                "vba.identifier.reference"
                    | "vba.value_identifier.reference"
                    | "vba.let_identifier.reference"
                    | "vba.set_identifier.reference"
            ) && site.source.span().start_byte() == start
        })
        .expect("written reference exists")
}

#[test]
fn vba_parameter_shadows_module_binding_independently_of_written_case() {
    let source = "Private Value As Long\nPublic Sub Update(ByVal value As Long)\nVALUE = value + 1\nEnd Sub\nPublic Sub Other()\nvalue = 2\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "value")
        .unwrap();
    let module_value = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "Value")
        .unwrap();
    for marker in ["VALUE =", "value +"] {
        assert_eq!(
            reference_at(document, source, marker).target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
    }
    assert_eq!(
        reference_at(document, source, "value = 2").target,
        OccurrenceTarget::Resolved {
            symbol: module_value.id
        }
    );
}

#[test]
fn vba_procedure_locals_cover_uses_before_their_declaration() {
    let source =
        "Private Count As Long\nPublic Sub Update()\nCOUNT = 1\nDim [count] As Long\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let local = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "count")
        .unwrap();
    assert_eq!(
        reference_at(document, source, "COUNT =").target,
        OccurrenceTarget::Resolved { symbol: local.id }
    );
}

#[test]
fn vba_duplicate_local_names_do_not_expose_an_outer_exact_spelling() {
    let source = "Private Count As Long\nPublic Sub Update()\nDim count As Long\nDim COUNT As Long\nCount = 1\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        reference_at(result.document(), source, "Count =").target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vba-binding-target-unavailable")
    );
}

#[test]
fn vba_unproven_applications_and_members_keep_scoped_relation_gaps() {
    let source = "Public Sub Update(ByVal source As Object)\nDim result As Long\nresult = source.Value(1)\nEnd Sub\n";
    let result = output(source);
    for detail in [
        "vba-member-target-unavailable",
        "vba-call-or-index-target-unavailable",
    ] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == detail),
            "missing {detail}"
        );
    }
}

#[test]
fn vba_raise_event_binds_its_class_declaration_not_a_local_or_runtime_handler() {
    use rootlight_ir::{RelationEndpoint, RelationPredicate};
    for header in ["VERSION 1.0 CLASS\n", ""] {
        let source = format!(
            "{header}Attribute VB_Name = \"Emitter\"\nPublic Event Changed(ByVal value As Long)\nPublic Sub Send(ByVal changed As Long)\nRaiseEvent CHANGED(changed)\nEnd Sub\n"
        );
        let result = output_with_native_kind(
            &source,
            header
                .is_empty()
                .then_some(rootlight_adapter_sdk::NativeVbaModuleKind::Object),
        );
        let document = result.document();
        let event = document
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Event && entity.canonical_name == "Changed")
            .unwrap();
        let start = u64::try_from(source.find("CHANGED(changed)").unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: event.id });
        assert_ne!(site.role, OccurrenceRole::CallSite);
        assert_eq!(site.syntax_kind, "vba.event_identifier.reference");
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.object == RelationEndpoint::Entity(event.id)
                    && edge.evidence.source.as_ref() == Some(&site.source))
        );
        assert!(!document.relations.iter().any(|edge| matches!(
            edge.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        ) && edge.evidence.source.as_ref()
            == Some(&site.source)));
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span() == site.source.span()
                    && gap.detail == "vba-event-handlers-unavailable")
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span() == site.source.span()
                    && gap.detail == "vba-event-target-unavailable")
        );
        let argument_start = u64::try_from(source.find("changed)\nEnd Sub").unwrap()).unwrap();
        let argument = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == argument_start)
            .unwrap();
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter && entity.canonical_name == "changed"
            })
            .unwrap();
        assert_eq!(
            argument.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            }
        );
    }
}

#[test]
fn vba_raise_event_rejects_missing_ambiguous_conditional_or_nonclass_targets() {
    for (header, declarations) in [
        ("", "Public Event Changed()\n"),
        ("VERSION 1.0 CLASS\n", ""),
        ("VERSION 1.0 CLASS\n", "Private Changed As Long\n"),
        (
            "VERSION 1.0 CLASS\n",
            "Public Event Changed()\nPublic Event changed()\n",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "#If Enabled Then\nPublic Event Changed()\n#End If\n",
        ),
    ] {
        let source = format!(
            "{header}Attribute VB_Name = \"Emitter\"\n{declarations}Public Sub Send()\nRaiseEvent Changed\nEnd Sub\n"
        );
        let result = output(&source);
        let start = u64::try_from(source.rfind("Changed\n").unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span() == site.source.span()
                    && gap.detail == "vba-event-target-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn vba_events_are_not_ordinary_values_or_callable_procedures() {
    let source = "VERSION 1.0 CLASS\nAttribute VB_Name = \"Emitter\"\nPublic Event Changed()\nPublic Sub Send()\nRaiseEvent Changed\nCall Changed()\nDim item As Variant\nitem = Changed\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let event = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Event)
        .unwrap();
    let raises: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "vba.event_identifier.reference")
        .collect();
    assert_eq!(raises.len(), 1);
    assert_eq!(
        raises[0].target,
        OccurrenceTarget::Resolved { symbol: event.id }
    );
    for marker in ["Changed()\nDim", "Changed\nEnd"] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.source.span().start_byte() == start)
            .collect();
        assert!(!sites.is_empty());
        assert!(
            sites
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
    }
}

#[test]
fn vba_event_names_are_independent_of_value_and_procedure_names() {
    for (declaration, use_site, marker) in [
        ("Private Status As Long\n", "Status = 1", "Status = 1"),
        (
            "Private Const Status As Long = 1\n",
            "item = Status",
            "Status\nEnd",
        ),
        (
            "Private Enum Phase\nStatus = 1\nEnd Enum\n",
            "item = Status",
            "Status\nEnd",
        ),
        (
            "Public Sub Status()\nEnd Sub\n",
            "Call Status()",
            "Status()\nEnd",
        ),
        (
            "Public Function Status() As Long\nStatus = 1\nEnd Function\n",
            "item = Status()",
            "Status()\nEnd",
        ),
        (
            "Public Property Get Status() As Long\nStatus = 1\nEnd Property\n",
            "item = Status",
            "Status\nEnd",
        ),
    ] {
        for event_first in [false, true] {
            let event = "Public Event Status()\n";
            let declarations = if event_first {
                format!("{event}{declaration}")
            } else {
                format!("{declaration}{event}")
            };
            let source = format!(
                "VERSION 1.0 CLASS\nAttribute VB_Name = \"Emitter\"\n{declarations}Public Sub Send()\nDim item As Long\nRaiseEvent STATUS\n{use_site}\nEnd Sub\n"
            );
            let result = output(&source);
            let document = result.document();
            let event = document
                .entities
                .iter()
                .find(|entity| entity.kind == EntityKind::Event)
                .unwrap();
            let ordinary = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Status" && entity.kind != EntityKind::Event
                })
                .unwrap();
            let raised = document
                .occurrences
                .iter()
                .find(|site| site.syntax_kind == "vba.event_identifier.reference")
                .unwrap();
            assert_eq!(
                raised.target,
                OccurrenceTarget::Resolved { symbol: event.id },
                "{source}"
            );
            let start = u64::try_from(source.rfind(marker).unwrap()).unwrap();
            let ordinary_use = document
                .occurrences
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start
                        && site.source.span().end_byte() == start + 6
                })
                .unwrap();
            assert_eq!(
                ordinary_use.target,
                OccurrenceTarget::Resolved {
                    symbol: ordinary.id
                },
                "{source}"
            );
            assert_ne!(raised.target, ordinary_use.target);
        }
    }
}

#[test]
fn vba_event_and_value_barriers_stay_in_their_own_namespaces() {
    for (declarations, event_resolves, value_resolves) in [
        (
            "#If Enabled Then\nPublic Event Status()\n#End If\nPrivate Status As Long\n",
            false,
            true,
        ),
        (
            "Public Event Status()\n#If Enabled Then\nPrivate Status As Long\n#End If\n",
            true,
            false,
        ),
        (
            "Public Event Status()\nPublic Event status()\nPrivate Status As Long\n",
            false,
            true,
        ),
        (
            "Public Event Status()\nPrivate Status As Long\nPrivate status As Long\n",
            true,
            false,
        ),
        (
            "Public Event Status()\nPublic Event État()\nPrivate Status As Long\n",
            false,
            true,
        ),
        (
            "Public Event Status()\nPrivate Status As Long\nPrivate État As Long\n",
            true,
            false,
        ),
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Emitter\"\n{declarations}Public Sub Send()\nRaiseEvent STATUS\nStatus = 1\nEnd Sub\n"
        );
        let result = output(&source);
        for (marker, expected) in [("STATUS\n", event_resolves), ("Status = 1", value_resolves)] {
            let start = u64::try_from(source.rfind(marker).unwrap()).unwrap();
            let site = result
                .document()
                .occurrences
                .iter()
                .find(|site| site.source.span().start_byte() == start)
                .unwrap();
            assert_eq!(
                matches!(site.target, OccurrenceTarget::Resolved { .. }),
                expected,
                "{source}: {marker}"
            );
        }
    }
}

#[test]
fn vba_conditional_local_declarations_block_outer_fallthrough() {
    let source = "Private Count As Long\nPublic Sub Update()\n#If Wide Then\nDim count As Long\n#End If\nCount = 1\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        reference_at(result.document(), source, "Count =").target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vba-binding-target-unavailable")
    );
}

#[test]
fn vba_conditional_bindings_reject_possible_coexisting_declarations() {
    for declarations in [
        "#If First Then\nDim payload As Long\npayload = 1\n#End If\n#If Second Then\nDim payload As String\n#End If\n",
        "#If First Then\nDim payload As Long\npayload = 1\n#If Second Then\nDim payload As String\n#End If\n#End If\n",
        "#If First Then\n#If Second Then\nDim payload As String\n#End If\nDim payload As Long\npayload = 1\n#End If\n",
    ] {
        let source = format!("Sub Update()\n{declarations}End Sub\n");
        let result = output(&source);
        let site = reference_at(result.document(), &source, "payload = 1");
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.source.span() == site.source.span()
                && gap.detail == "vba-binding-target-unavailable"
        }));
    }
}

#[test]
fn vba_conditional_procedure_calls_bind_only_in_the_declaring_branch() {
    let source = "#If Enabled Then\nPrivate Sub Visit(ByVal value As Long)\nEnd Sub\nPrivate Sub Run()\nCall Visit(value:=1)\nEnd Sub\n#Else\nPrivate Sub Visit(ByVal value As String)\nEnd Sub\nPrivate Sub Run()\nCall Visit(value:=\"text\")\nEnd Sub\n#End If\nSub Outside()\nCall Visit(2)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (definition, marker) in [
        ("Visit(ByVal value As Long)", "Visit(value:=1)"),
        ("Visit(ByVal value As String)", "Visit(value:=\"text\")"),
    ] {
        let start = u64::try_from(source.find(definition).unwrap()).unwrap();
        let target = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition && site.source.span().start_byte() == start
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = target.target else {
            panic!("definition")
        };
        let call = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.invoked_identifier.reference"
                    && site.source.span().start_byte()
                        == u64::try_from(source.find(marker).unwrap()).unwrap()
            })
            .unwrap();
        assert_eq!(call.target, target.target, "{marker}");
        assert_eq!(call.role, OccurrenceRole::CallSite);
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(call.id)
                    && edge.object == RelationEndpoint::Entity(symbol))
        );
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() + 5 == call.source.span().start_byte()
            })
            .unwrap();
        assert_eq!(application.target, target.target);
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Relations
                    && gap.source.span() == application.source.span())
        );
    }
    assert!(matches!(
        document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.invoked_identifier.reference"
                    && site.source.span().start_byte()
                        == u64::try_from(source.find("Visit(2)").unwrap()).unwrap()
            })
            .unwrap()
            .target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "vba-conditional-compilation-evaluation-unavailable")
    );
}

#[test]
fn vba_conditional_local_values_resolve_within_their_branch() {
    let source = "Public Sub Update()\n#If Enabled Then\nDim payload As Long\npayload = 1\n#End If\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let local = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "payload")
        .unwrap();
    let site = reference_at(document, source, "payload = 1");
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: local.id });
    assert!(!document.skipped_regions.iter().any(|gap| {
        gap.source.span() == site.source.span() && gap.detail == "vba-binding-target-unavailable"
    }));
}

#[test]
fn vba_conditional_calls_preserve_ambiguity_and_argument_barriers() {
    for (source, callee_resolves) in [
        (
            "#If Enabled Then\nSub Visit(ByVal value As Long)\nEnd Sub\nSub Run()\nCall VISIT()\nEnd Sub\n#End If\n",
            true,
        ),
        (
            "#If Enabled Then\nSub Visit(ByVal value As Long)\nEnd Sub\nSub Run()\nCall VISIT(other:=1)\nEnd Sub\n#End If\n",
            true,
        ),
        (
            "Sub Visit()\nEnd Sub\n#If Enabled Then\nSub Visit()\nEnd Sub\nSub Run()\nCall VISIT()\nEnd Sub\n#End If\n",
            false,
        ),
        (
            "#If Enabled Then\nSub Visit()\nEnd Sub\nSub Run()\nCall VISIT()\nEnd Sub\n#If Extra Then\nSub Visit()\nEnd Sub\n#End If\n#End If\n",
            false,
        ),
        (
            "#If Enabled Then\nSub Visit()\nEnd Sub\nSub Run()\nCall VISIT()\nEnd Sub\n#End If\n#If Extra Then\nSub Visit()\nEnd Sub\n#End If\n",
            false,
        ),
        (
            "#If Enabled Then\nSub Visit()\nEnd Sub\n#End If\n#If Other Then\nSub Run()\nCall VISIT()\nEnd Sub\n#End If\n",
            false,
        ),
    ] {
        let result = output(source);
        let document = result.document();
        let start = u64::try_from(source.find("VISIT(").unwrap()).unwrap();
        let callee = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.invoked_identifier.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        assert_eq!(
            matches!(callee.target, OccurrenceTarget::Resolved { .. }),
            callee_resolves,
            "{source}"
        );
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() + 5 == start
            })
            .unwrap();
        assert!(
            matches!(application.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span() == application.source.span()
                    && gap.detail == "vba-call-or-index-target-unavailable")
        );
    }
}

#[test]
fn vba_conditional_functions_and_getters_bind_without_selecting_a_platform() {
    for (header, end) in [
        ("Function Measure() As Long", "Function"),
        ("Property Get Measure() As Long", "Property"),
    ] {
        let source = format!(
            "#If Enabled Then\n{header}\nMeasure = 1\nEnd {end}\nSub Run()\nDim result As Long\nresult = Measure\nEnd Sub\n#End If\n"
        );
        let result = output(&source);
        let document = result.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Measure")
            .unwrap();
        let site = reference_at(document, &source, "Measure\nEnd Sub");
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(site.role, OccurrenceRole::CallSite);
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-conditional-compilation-evaluation-unavailable")
        );
    }
}

#[test]
fn vba_conditional_property_accessors_share_the_name_not_the_target() {
    let get = "Property Get Payload() As Variant\nEnd Property\n";
    let put = "Property Let Payload(ByVal value As Variant)\nEnd Property\n";
    let set = "Property Set Payload(ByVal value As Variant)\nEnd Property\n";
    for declarations in [
        format!("#If Enabled Then\n{get}{put}{set}#End If\n"),
        format!("{get}#If Enabled Then\n{put}{set}#End If\n"),
        format!("#If Enabled Then\n{get}#End If\n{put}{set}"),
        format!("#If Enabled Then\n{set}#End If\n#If ENABLED Then\n{get}{put}#End If\n"),
    ] {
        for qualifier in ["", "Me."] {
            let source = format!(
                "Attribute VB_Name = \"Container\"\n{declarations}#If Enabled Then\nSub Run()\nDim result As Variant\nDim instance As Object\nresult = {qualifier}PAYLOAD\n{qualifier}PAYLOAD = 1\nSet {qualifier}PAYLOAD = instance\nEnd Sub\n#End If\n"
            );
            let result = output_with_native_kind(
                &source,
                Some(rootlight_adapter_sdk::NativeVbaModuleKind::Object),
            );
            let document = result.document();
            for (header, marker) in [
                ("Get Payload", "PAYLOAD\n"),
                ("Let Payload", "PAYLOAD = 1"),
                ("Set Payload", "PAYLOAD = instance"),
            ] {
                let definition_start = u64::try_from(source.find(header).unwrap() + 4).unwrap();
                let definition = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.source.span().start_byte() == definition_start
                    })
                    .unwrap();
                let start = u64::try_from(source.find(marker).unwrap()).unwrap();
                let site = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.source.span().start_byte() == start
                            && site.syntax_kind != "vba.application.reference"
                    })
                    .unwrap();
                assert_eq!(site.target, definition.target, "{source}\n{marker}");
                assert_eq!(site.role, OccurrenceRole::CallSite);
                let OccurrenceTarget::Resolved { symbol } = definition.target else {
                    panic!("definition")
                };
                assert!(document.relations.iter().any(|edge| {
                    edge.predicate == RelationPredicate::Calls
                        && edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.object == RelationEndpoint::Entity(symbol)
                }));
            }
            assert!(
                document.skipped_regions.iter().any(|gap| {
                    gap.detail == "vba-conditional-compilation-evaluation-unavailable"
                })
            );
        }
    }
}

#[test]
fn vba_conditional_properties_keep_duplicate_and_presence_barriers() {
    let get = "Property Get Payload() As Variant\nEnd Property\n";
    let put = "Property Let Payload(ByVal value As Variant)\nEnd Property\n";
    for (declarations, guard, resolves) in [
        (
            format!("#If Enabled Then\n{get}{get}{put}#End If\n"),
            "Enabled",
            false,
        ),
        (
            format!("{get}#If Enabled Then\n{get}{put}#End If\n"),
            "Enabled",
            false,
        ),
        (
            format!("#If Enabled Then\n{get}{put}{put}#End If\n"),
            "Enabled",
            false,
        ),
        (
            format!("Dim Payload As Variant\n#If Enabled Then\n{get}{put}#End If\n"),
            "Enabled",
            false,
        ),
        (
            format!("#If Enabled Then\n{get}{put}Sub Payload()\nEnd Sub\n#End If\n"),
            "Enabled",
            false,
        ),
        (
            format!("#If Enabled Then\n{get}{put}#End If\n"),
            "Other",
            false,
        ),
        (
            format!("#If Enabled Then\n{get}{put}#If Extra Then\n{get}#End If\n#End If\n"),
            "Enabled",
            false,
        ),
        (
            format!("#If Enabled Then\n{get}{put}#Else\n{get}{put}#End If\n"),
            "Enabled",
            true,
        ),
        (
            format!("#If Enabled Then\n{get}#End If\n#If Other Then\n{put}#End If\n"),
            "Enabled",
            false,
        ),
    ] {
        for qualifier in ["", "Me."] {
            let source = format!(
                "Attribute VB_Name = \"Container\"\n{declarations}#If {guard} Then\nSub Run()\nDim result As Variant\nresult = {qualifier}PAYLOAD\nEnd Sub\n#End If\n"
            );
            let result = output_with_native_kind(
                &source,
                Some(rootlight_adapter_sdk::NativeVbaModuleKind::Object),
            );
            let document = result.document();
            let start = u64::try_from(source.find("PAYLOAD\n").unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| site.source.span().start_byte() == start)
                .unwrap();
            assert_eq!(
                matches!(site.target, OccurrenceTarget::Resolved { .. }),
                resolves,
                "{source}"
            );
        }
    }
}

#[test]
fn vba_conditional_indexed_getters_validate_arguments_after_accessor_merging() {
    for qualifier in ["", "Me."] {
        for (arguments, resolves) in [
            ("1", true),
            ("index:=1", true),
            ("", false),
            ("other:=1", false),
            ("1, 2", false),
        ] {
            let source = format!(
                "Attribute VB_Name = \"Container\"\n#If Enabled Then\nProperty Get Payload(ByVal index As Long) As Variant\nEnd Property\nProperty Let Payload(ByVal index As Long, ByVal value As Variant)\nEnd Property\n#End If\n#If Enabled Then\nSub Run()\nDim result As Variant\nresult = {qualifier}PAYLOAD({arguments})\nEnd Sub\n#End If\n"
            );
            let result = output_with_native_kind(
                &source,
                Some(rootlight_adapter_sdk::NativeVbaModuleKind::Object),
            );
            let document = result.document();
            let start =
                u64::try_from(source.find(&format!("{qualifier}PAYLOAD(")).unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == start
                })
                .unwrap_or_else(|| {
                    let mut parser = tree_sitter::Parser::new();
                    parser
                        .set_language(&tree_sitter_vba::LANGUAGE.into())
                        .unwrap();
                    panic!(
                        "missing application at {start}: {source}; {}",
                        parser.parse(&source, None).unwrap().root_node().to_sexp()
                    );
                });
            assert_eq!(
                matches!(site.target, OccurrenceTarget::Resolved { .. }),
                resolves,
                "{source}"
            );
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source
                        && gap.detail == "vba-call-or-index-target-unavailable"),
                !resolves
            );
        }
    }
}

#[test]
fn vba_conditional_instance_lookup_preserves_constant_and_local_namespaces() {
    let source = "Attribute VB_Name = \"Container\"\n#If Enabled Then\nPrivate Const Limit = 7\nProperty Get Payload() As Variant\nEnd Property\nProperty Let Payload(ByVal value As Variant)\nEnd Property\nSub Run()\nDim Payload As Variant\nDim result As Variant\nresult = PAYLOAD\nresult = Me.PAYLOAD\nresult = Me.LIMIT\nEnd Sub\n#End If\n";
    let result = output_with_native_kind(
        source,
        Some(rootlight_adapter_sdk::NativeVbaModuleKind::Object),
    );
    let document = result.document();
    for (marker, definition) in [
        ("PAYLOAD\n", "Payload As Variant"),
        ("PAYLOAD\nresult = Me.LIMIT", "Payload() As Variant"),
    ] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let definition_start = u64::try_from(source.find(definition).unwrap()).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        let target = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == definition_start
            })
            .unwrap();
        assert_eq!(site.target, target.target);
    }
    let start = u64::try_from(source.find("LIMIT\n").unwrap()).unwrap();
    assert!(matches!(
        document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap()
            .target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_repeated_conditions_require_matching_branch_and_constant_environment() {
    for (definition_guard, reference_guard, between, resolved) in [
        ("#If Enabled Then\n", "#If Enabled Then\n", "", true),
        (
            "#If Enabled Or Not Legacy Then\n",
            "#If eNaBlEd Or Not [Legacy] Then\n",
            "",
            true,
        ),
        (
            "#If Enabled Or Not Legacy Then\n",
            "#If Enabled And Not Legacy Then\n",
            "",
            false,
        ),
        (
            "#If Enabled Then\n#Else\n",
            "#If Enabled Then\n#Else\n",
            "",
            true,
        ),
        ("#If Enabled Then\n", "#If Enabled Then\n#Else\n", "", false),
        (
            "#If First Then\n#ElseIf Second Then\n",
            "#If First Then\n#ElseIf Second Then\n",
            "",
            true,
        ),
        (
            "#If First Then\n#ElseIf Second Then\n",
            "#If Other Then\n#ElseIf Second Then\n",
            "",
            false,
        ),
        (
            "#If First Then\n#ElseIf Second Then\n#Else\n",
            "#If First Then\n#ElseIf Second Then\n#Else\n",
            "",
            true,
        ),
        (
            "#If First Then\n#ElseIf Second Then\n#Else\n",
            "#If First Then\n#Else\n",
            "",
            false,
        ),
        (
            "#If Enabled Then\n",
            "#If Enabled Then\n",
            "#Const Enabled = True\n",
            false,
        ),
        (
            "#If Enabled Then\n",
            "#If Enabled Then\n",
            "#Const Other = False\n",
            false,
        ),
        (
            "#If Enabled Then\n",
            "#If Enabled Then\n",
            "' #Const marker remains conservative\n",
            false,
        ),
        (
            "#If Version >= 7 Then\n",
            "#If Version >= 7 Then\n",
            "",
            false,
        ),
    ] {
        let source = format!(
            "{definition_guard}Sub Visit(ByVal item As Long)\nEnd Sub\n#End If\n{between}{reference_guard}Sub Run()\nCall VISIT(item:=1)\nEnd Sub\n#End If\n"
        );
        let result = output(&source);
        let document = result.document();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Visit")
            .unwrap();
        for (kind, marker) in [
            ("vba.invoked_identifier.reference", "VISIT("),
            ("vba.application.reference", "Call VISIT("),
        ] {
            let start = u64::try_from(source.find(marker).unwrap()).unwrap();
            let site = document
                .occurrences
                .iter()
                .find(|site| site.syntax_kind == kind && site.source.span().start_byte() == start)
                .unwrap();
            assert_eq!(
                site.target == OccurrenceTarget::Resolved { symbol: target.id },
                resolved,
                "{kind}: {source}"
            );
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span() == site.source.span()
                        && gap.detail == "vba-call-or-index-target-unavailable"),
                !resolved,
                "{kind}: {source}"
            );
        }
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-conditional-compilation-evaluation-unavailable")
        );
    }
}

#[test]
fn vba_repeated_conditions_keep_duplicate_declarations_ambiguous() {
    for prefix in ["", "#Const Enabled = True\n#Const ENABLED = False\n"] {
        let source = format!(
            "{prefix}#If Enabled Then\nSub Visit()\nEnd Sub\n#End If\n#If Enabled Then\nSub Visit()\nEnd Sub\nSub Run()\nCall VISIT()\nEnd Sub\n#End If\n"
        );
        let result = output(&source);
        let start = u64::try_from(source.find("VISIT()").unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.invoked_identifier.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    }
}

#[test]
fn vba_repeated_conditions_bind_labels_without_activating_the_branch() {
    let source = "Sub Run()\n#If Enabled Then\nFinish:\n#End If\n#If ENABLED Then\nGoTo Finish\n#End If\nEnd Sub\n";
    let result = output(source);
    let target = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::StatementLabel)
        .unwrap();
    let site = result
        .document()
        .occurrences
        .iter()
        .find(|site| site.syntax_kind == "vba.statement_label.reference")
        .unwrap();
    assert_eq!(
        site.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
}

#[test]
fn vba_repeated_conditions_do_not_use_recovered_directive_groups() {
    let source = "#If Enabled Then\nSub Visit()\nEnd Sub\nDim broken As\n#End If\n#If Enabled Then\nSub Run()\nCall VISIT()\nEnd Sub\n#End If\n";
    let result = output(source);
    let start = u64::try_from(source.find("VISIT()").unwrap()).unwrap();
    let site = result
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.invoked_identifier.reference"
                && site.source.span().start_byte() == start
        })
        .unwrap();
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
}

#[test]
fn vba_conditional_sibling_branches_keep_distinct_local_bindings() {
    let source = "Public Sub Update()\n#If Enabled Then\nDim payload As Long\npayload = 1\n#Else\nDim payload As String\npayload = \"text\"\n#End If\npayload = 2\nEnd Sub\n";
    let result = output(source);
    let first = reference_at(result.document(), source, "payload = 1");
    let second = reference_at(result.document(), source, "payload = \"text\"");
    let outside = reference_at(result.document(), source, "payload = 2");
    let (
        OccurrenceTarget::Resolved {
            symbol: first_symbol,
        },
        OccurrenceTarget::Resolved {
            symbol: second_symbol,
        },
    ) = (&first.target, &second.target)
    else {
        panic!("each conditional branch must resolve its own local variable");
    };
    assert_ne!(first_symbol, second_symbol);
    assert!(matches!(
        outside.target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_conditional_local_collision_with_unconditional_local_stays_unresolved() {
    let source = "Public Sub Update()\nDim payload As Long\n#If Enabled Then\nDim payload As Long\npayload = 1\n#End If\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        reference_at(result.document(), source, "payload = 1").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_nested_conditional_duplicate_locals_stay_unresolved() {
    let source = "Public Sub Update()\n#If Outer Then\nDim payload As Long\n#If Inner Then\nDim payload As Long\npayload = 1\n#End If\n#End If\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        reference_at(result.document(), source, "payload = 1").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_type_label_and_argument_names_do_not_bind_to_local_values() {
    let source = "Public Type Model\nValue As Long\nEnd Type\nPublic Sub Update(ByVal Model As Long, ByVal Finish As Long, ByVal size As Long)\nDim item As Model\nGoTo Finish\nFinish:\nCall Visit(size:=size)\nEnd Sub\n";
    let result = output(source);
    let model = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Struct && entity.canonical_name == "Model")
        .unwrap();
    for (marker, is_type) in [
        ("Model\nGoTo", true),
        ("Finish\nFinish:", false),
        ("size:=", false),
    ] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let site = result
            .document()
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        if is_type {
            assert_eq!(site.syntax_kind, "vba.direct_type_identifier.reference");
            assert_eq!(site.role, OccurrenceRole::TypeUse);
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: model.id });
        } else if marker == "Finish\nFinish:" {
            let label = result
                .document()
                .entities
                .iter()
                .find(|entity| entity.kind == EntityKind::StatementLabel)
                .unwrap();
            assert_eq!(site.syntax_kind, "vba.statement_label.reference");
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: label.id });
        } else {
            assert_eq!(site.syntax_kind, "vba.nonvalue_identifier.reference");
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        }
    }
    let parameter = result
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "size")
        .unwrap();
    assert_eq!(
        reference_at(result.document(), source, "size)").target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
}

#[test]
fn vba_statement_labels_bind_control_transfers_in_their_own_namespace() {
    let source = "Sub Run(ByVal Finish As Long, ByVal choice As Long)\nOn Error GoTo Recover\nGoTo Finish\nGoSub Finish\nOn choice GoTo Finish, Recover\nOn choice GoSub Finish, Recover\nFinish:\nFinish = choice\nResume Recover\nRecover:\nResume Finish\nEnd Sub\nSub Other()\nGoTo Finish\nFinish:\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    assert_eq!(document.version, rootlight_ir::NormalizedIrVersion::V1_7);
    let labels: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::StatementLabel)
        .collect();
    assert_eq!(labels.len(), 3, "{:#?}", document.entities);
    let transfers: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.syntax_kind.as_str(),
                "vba.statement_label.reference"
                    | "vba.error_label.reference"
                    | "vba.resume_label.reference"
            )
        })
        .collect();
    assert_eq!(transfers.len(), 10, "{:#?}", document.occurrences);
    for site in transfers {
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("unresolved control transfer: {site:#?}");
        };
        let label = labels.iter().find(|label| label.id == symbol).unwrap();
        let span = site.source.span();
        let text = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert!(label.canonical_name.eq_ignore_ascii_case(text));
        let owner = if span.start_byte() < u64::try_from(source.find("Sub Other").unwrap()).unwrap()
        {
            "Run"
        } else {
            "Other"
        };
        let procedure = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner)
            .unwrap();
        assert_eq!(label.container, Some(ContainerRef::Entity(procedure.id)));
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert!(
            document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(symbol))
        );
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls
                    && edge.subject == RelationEndpoint::Occurrence(site.id))
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source)
        );
    }
    let parameter = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "Finish")
        .unwrap();
    assert_eq!(
        reference_at(document, source, "Finish = choice").target,
        OccurrenceTarget::Resolved {
            symbol: parameter.id
        }
    );
}

#[test]
fn vba_numeric_statement_labels_compare_values_and_preserve_error_policy_zero() {
    let source = "Sub Run()\nGoTo 0010\nGoSub &HA\nOn Error GoTo 10\nResume &H0A\nGoTo 0\n10: Resume 0\n0 On Error GoTo 0\n20\nGoTo 20\nEnd Sub\n";
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_vba::LANGUAGE.into())
        .unwrap();
    let tree = parser.parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{}",
        tree.root_node().to_sexp()
    );
    let result = output(source);
    let document = result.document();
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail.contains("parse")),
        "{:#?}",
        document.skipped_regions
    );
    let labels: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::StatementLabel)
        .collect();
    assert_eq!(labels.len(), 3, "{:#?}", document.entities);
    for (marker, target) in [
        ("0010", "10"),
        ("&HA", "10"),
        ("10\nResume", "10"),
        ("&H0A", "10"),
        ("0\n10:", "0"),
        ("20\nEnd", "20"),
    ] {
        let site = label_reference_at(document, source, marker);
        let label = labels
            .iter()
            .find(|label| label.canonical_name == target)
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: label.id },
            "{marker}: {site:#?}"
        );
        assert_eq!(site.role, OccurrenceRole::Reference);
    }
    for marker in ["0\n0 On", "0\n20"] {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.source.span().start_byte() == start)
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source.span().start_byte() == start)
        );
    }
}

#[test]
fn vba_unparsed_octal_labels_remain_explicit_without_fabricated_bindings() {
    let source = "Sub Run()\nGoTo &O12\n10:\nEnd Sub\n";
    let result = output(source);
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == rootlight_ir::SkippedRegionReason::ParseError)
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role != OccurrenceRole::Definition
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn vba_statement_label_ambiguity_is_procedure_local_and_never_binds_values() {
    for body in [
        "",
        "Finish:\nfinish:\n",
        "#If DEBUG Then\nFinish:\n#End If\n",
    ] {
        let source = format!(
            "Sub Run(ByVal Finish As Long)\nGoTo Finish\n{body}End Sub\nSub Other()\nGoTo Finish\nFinish:\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let site = label_reference_at(document, &source, "Finish\n");
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "vba-statement-label-target-unavailable"
                    && gap.source == site.source)
        );
        let other_start =
            u64::try_from(source.rfind("GoTo Finish").unwrap() + "GoTo ".len()).unwrap();
        let other = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == other_start)
            .unwrap();
        assert!(
            matches!(other.target, OccurrenceTarget::Resolved { .. }),
            "{source}"
        );
    }
    let source = "Sub Run()\nGoTo 10\n10:\n&H0A:\nEnd Sub\n";
    let result = output(source);
    assert!(matches!(
        label_reference_at(result.document(), source, "10\n").target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_statement_labels_resolve_only_within_compatible_compile_branches() {
    let source = "Sub Run()\n#If Host Then\nOn Error GoTo Finish\n#If Trace Then\nGoTo Finish\n#End If\nFinish:\n#ElseIf Alternate Then\nGoSub Finish\nFinish:\n#Else\nResume Finish\nFinish:\n#End If\nGoTo Finish\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let transfers: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            matches!(
                site.syntax_kind.as_str(),
                "vba.statement_label.reference"
                    | "vba.error_label.reference"
                    | "vba.resume_label.reference"
            )
        })
        .collect();
    assert_eq!(transfers.len(), 5);
    let mut transfers = transfers;
    transfers.sort_by_key(|site| site.source.span().start_byte());
    let symbols: Vec<_> = transfers[..4]
        .iter()
        .map(|site| {
            let OccurrenceTarget::Resolved { symbol } = site.target else {
                panic!("same-branch transfer must resolve: {site:#?}");
            };
            assert!(
                !document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == site.source)
            );
            symbol
        })
        .collect();
    assert_eq!(symbols[0], symbols[1]);
    assert_ne!(symbols[0], symbols[2]);
    assert_ne!(symbols[0], symbols[3]);
    assert_ne!(symbols[2], symbols[3]);
    for ((start, _), symbol) in source
        .match_indices("Finish:")
        .zip([symbols[0], symbols[2], symbols[3]])
    {
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
            })
            .unwrap();
        assert_eq!(definition.target, OccurrenceTarget::Resolved { symbol });
    }
    assert!(matches!(
        transfers[4].target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_statement_labels_do_not_guess_independent_or_nested_compile_conditions() {
    for body in [
        "#If Enabled Then\nFinish:\n#Else\nGoTo Finish\n#End If\n",
        "#If Enabled Then\nFinish:\n#End If\n#If Other Then\nGoTo Finish\n#End If\n",
        "Finish:\n#If Enabled Then\nFinish:\nGoTo Finish\n#End If\n",
        "#If Enabled Then\nFinish:\n#If Trace Then\nFinish:\n#End If\nGoTo Finish\n#End If\n",
        "#If Enabled Then\nFinish:\n#If Trace Then\nFinish:\nGoTo Finish\n#End If\n#End If\n",
        "#If Enabled Then\nFinish:\nfinish:\nGoTo Finish\n#End If\n",
        "#If Enabled Then\n10:\n&H0A:\nGoTo 10\n#End If\n",
    ] {
        let source = format!("Sub Run()\n{body}End Sub\n");
        let result = output(&source);
        let document = result.document();
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == "vba.statement_label.reference")
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == site.source
                    && gap.detail == "vba-statement-label-target-unavailable")
        );
    }
}

#[test]
fn vba_statement_labels_exclude_duplicates_from_incompatible_nested_branches() {
    let source = "Sub Run()\n#If Enabled Then\nFinish:\n#If Trace Then\nFinish:\n#Else\nGoTo Finish\n#End If\n#End If\nEnd Sub\n";
    let result = output(source);
    let site = label_reference_at(result.document(), source, "Finish\n#End If");
    assert!(matches!(site.target, OccurrenceTarget::Resolved { .. }));
    let start = u64::try_from(source.find("Finish:").unwrap()).unwrap();
    let definition = result
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition && site.source.span().start_byte() == start
        })
        .unwrap();
    assert_eq!(site.target, definition.target);
}

#[test]
fn vba_statement_labels_resolve_inside_conditionally_declared_procedures() {
    let source = "#If Host Then\nSub Run()\nGoTo 0010\n10:\nEnd Sub\n#Else\nSub Run()\nGoTo &HA\n10:\nEnd Sub\n#End If\n";
    let result = output(source);
    let first = label_reference_at(result.document(), source, "0010\n");
    let second = label_reference_at(result.document(), source, "&HA\n");
    assert!(matches!(first.target, OccurrenceTarget::Resolved { .. }));
    assert!(matches!(second.target, OccurrenceTarget::Resolved { .. }));
    assert_ne!(first.target, second.target);
}

fn label_reference_at<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    source: &str,
    marker: &str,
) -> &'a rootlight_ir::OccurrenceRecord {
    let start = u64::try_from(source.find(marker).unwrap()).unwrap();
    document
        .occurrences
        .iter()
        .find(|site| {
            site.source.span().start_byte() == start
                && matches!(
                    site.syntax_kind.as_str(),
                    "vba.statement_label.reference"
                        | "vba.error_label.reference"
                        | "vba.resume_label.reference"
                )
        })
        .unwrap_or_else(|| {
            panic!(
                "missing label reference {marker}: {:#?}",
                document.occurrences
            )
        })
}

#[test]
fn vba_statement_named_arguments_bind_without_parentheses() {
    for invocation in [
        "Visit size:=size",
        "Visit size:=Inner(value:=size), label:=size",
        "Visit Inner(value:=size), label:=size",
        "Visit (size), label:=size",
        "Visit _\n size:=size",
        "Visit _\r\n size:=size",
        "Visit size _\n :=size",
        "Me.Visit size:=size",
        "Me.Visit _\r\n size:=size",
        "Me.Visit Inner(value:=size), label:=size",
        "Call Visit _\n (size:=size)",
        "Call Me.Visit _\n (size:=size)",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Visitor\"\nPublic Function Inner(ByVal value As Long) As Long\nInner = value\nEnd Function\nPublic Sub Visit(ByVal size As Long, Optional label As Long = 0)\nEnd Sub\nPublic Sub Run(ByVal size As Long)\n{invocation}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        for (marker, owner_name, parameter_name) in [
            ("size:=", "Visit", "size"),
            ("size _\n :=", "Visit", "size"),
            ("label:=", "Visit", "label"),
            ("value:=", "Inner", "value"),
        ] {
            let Some(start) = source.find(marker) else {
                continue;
            };
            let owner = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == owner_name)
                .unwrap();
            let parameter = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.canonical_name == parameter_name
                        && entity.container == Some(ContainerRef::Entity(owner.id))
                })
                .unwrap();
            let label = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.nonvalue_identifier.reference"
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                })
                .unwrap_or_else(|| panic!("missing named label: {invocation}: {marker}"));
            assert_eq!(
                label.target,
                OccurrenceTarget::Resolved {
                    symbol: parameter.id
                },
                "{invocation}: {marker}"
            );
            assert!(document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.object == RelationEndpoint::Entity(parameter.id)
                    && edge.evidence.source.as_ref() == Some(&label.source)
            }));
            assert!(!document.skipped_regions.iter().any(|gap| {
                gap.source == label.source && gap.detail == "vba-nonvalue-target-unavailable"
            }));
        }
        let start = u64::try_from(source.find(invocation).unwrap()).unwrap();
        let application = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        let target = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Visit")
            .unwrap();
        assert_eq!(
            application.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{invocation}"
        );
        assert!(
            !document
                .skipped_regions
                .iter()
                .any(|gap| gap.source == application.source
                    && gap.detail == "vba-call-or-index-target-unavailable")
        );
    }
}

#[test]
fn vba_named_arguments_bind_only_to_the_called_procedure_parameters() {
    let source = "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run(ByVal size As Long)\nCall Visit(size:=size)\nCall Visit(missing:=size)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    let callee = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Visit")
        .unwrap();
    let callee_parameter = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == "size"
                && entity.container == Some(ContainerRef::Entity(callee.id))
        })
        .unwrap();
    let caller = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Run")
        .unwrap();
    let caller_parameter = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == "size"
                && entity.container == Some(ContainerRef::Entity(caller.id))
        })
        .unwrap();
    let label_at = |marker: &str| {
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.nonvalue_identifier.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap()
    };
    let label = label_at("size:=size");
    assert_eq!(label.syntax_kind, "vba.nonvalue_identifier.reference");
    assert_eq!(
        label.target,
        OccurrenceTarget::Resolved {
            symbol: callee_parameter.id
        }
    );
    assert_eq!(label.role, OccurrenceRole::Reference);
    assert!(document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.object == RelationEndpoint::Entity(callee_parameter.id)
            && edge.evidence.source.as_ref() == Some(&label.source)
    }));
    let value_start = u64::try_from(source.find("size:=size").unwrap() + "size:=".len()).unwrap();
    let value = document
        .occurrences
        .iter()
        .find(|site| site.source.span().start_byte() == value_start)
        .unwrap();
    assert_eq!(
        value.target,
        OccurrenceTarget::Resolved {
            symbol: caller_parameter.id
        }
    );
    assert_ne!(callee_parameter.id, caller_parameter.id);
    let missing = label_at("missing:=size");
    assert!(matches!(
        missing.target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn vba_self_named_arguments_bind_the_method_parameter_not_caller_shadows() {
    for invocation in [
        "Call Me.Visit(size:=size)",
        "Me.Visit(size:=size)",
        "Call mE . [Visit](size:=size)",
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Receiver\"\nPublic Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run(ByVal size As Long, ByVal Visit As Object)\n{invocation}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let method = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Visit" && entity.kind == EntityKind::Method)
            .unwrap();
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == "size"
                    && entity.container == Some(ContainerRef::Entity(method.id))
            })
            .unwrap();
        let label_start = u64::try_from(source.find("size:=size").unwrap()).unwrap();
        let label = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.nonvalue_identifier.reference"
                    && site.source.span().start_byte() == label_start
            })
            .unwrap();
        assert_eq!(
            label.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            },
            "{source}"
        );
        assert_eq!(label.role, OccurrenceRole::Reference);
        assert_eq!(label.source.span().end_byte(), label_start + 4);
        assert!(!document.skipped_regions.iter().any(
            |gap| gap.source == label.source && gap.detail == "vba-nonvalue-target-unavailable"
        ));
        let value = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == label_start + 6)
            .unwrap();
        assert!(
            matches!(value.target, OccurrenceTarget::Resolved { symbol } if symbol != parameter.id)
        );
    }
}

#[test]
fn vba_self_named_arguments_follow_nested_calls_and_accessor_contexts() {
    let declarations = "Public Function Inner(ByVal value As Long) As Long\nInner = value\nEnd Function\nPublic Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Property Get Item(ByVal index As Long) As Variant\nItem = index\nEnd Property\nPublic Property Let Item(ByVal index As Long, ByVal value As Variant)\nEnd Property\nPublic Property Set Item(ByVal index As Long, ByVal value As Object)\nEnd Property\n";
    for (statement, marker, owner_marker, parameter_name) in [
        (
            "Call Me.Visit(size:=Me.Inner(value:=1))",
            "size:=",
            "Sub Visit",
            "size",
        ),
        (
            "Call Me.Visit(size:=Me.Inner(value:=1))",
            "value:=",
            "Function Inner",
            "value",
        ),
        ("value = Me.Item(index:=1)", "index:=", "Get Item", "index"),
        (
            "Let Me.Item(index:=1) = value",
            "index:=",
            "Let Item",
            "index",
        ),
        (
            "Set Me.Item(index:=1) = value",
            "index:=",
            "Set Item",
            "index",
        ),
    ] {
        let source = format!(
            "VERSION 1.0 CLASS\nAttribute VB_Name = \"Receiver\"\n{declarations}Sub Run(ByVal value As Variant)\n{statement}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let name_offset = owner_marker.rfind(' ').unwrap() + 1;
        let definition_start =
            u64::try_from(source.find(owner_marker).unwrap() + name_offset).unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == definition_start
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol: owner } = definition.target else {
            panic!("accessor definition must resolve")
        };
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == parameter_name
                    && entity.container == Some(ContainerRef::Entity(owner))
            })
            .unwrap();
        let label_start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let label = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.nonvalue_identifier.reference"
                    && site.source.span().start_byte() == label_start
            })
            .unwrap();
        assert_eq!(
            label.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            },
            "{statement}: {marker}"
        );
    }
}

#[test]
fn vba_self_named_arguments_preserve_unknown_and_ambiguous_receivers() {
    let method = "Public Sub Visit(ByVal size As Long)\nEnd Sub\n";
    for (class_header, declarations, invocation, marker) in [
        (
            "VERSION 1.0 CLASS\n",
            "Public Property Let Item(ByVal value As Long, ByVal value As Long)\nEnd Property\n"
                .to_owned(),
            "Let Me.Item(value:=1) = 2",
            "value:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Public Property Let Item(ByVal index As Long, ByVal value As Long)\nEnd Property\n"
                .to_owned(),
            "Let Me.Item(value:=1) = 2",
            "value:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Public Property Set Item(ByVal index As Long, ByVal value As Object)\nEnd Property\n"
                .to_owned(),
            "Set Me.Item(value:=1) = Nothing",
            "value:=",
        ),
        ("", method.to_owned(), "Call Me.Visit(size:=1)", "size:="),
        (
            "VERSION 1.0 CLASS\n",
            method.to_owned(),
            "Call [Me].Visit(size:=1)",
            "size:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            method.repeat(2),
            "Call Me.Visit(size:=1)",
            "size:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            format!("{method}Public Function Factory() As Object\nEnd Function\n"),
            "Call Me.Factory().Visit(size:=1)",
            "size:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            "Private items(2) As Long\n".to_owned(),
            "Call Me.items(size:=1)",
            "size:=",
        ),
        (
            "VERSION 1.0 CLASS\n",
            method.to_owned(),
            "Call Me.Visit(missing:=1)",
            "missing:=",
        ),
    ] {
        let source = format!(
            "{class_header}Attribute VB_Name = \"Receiver\"\n{declarations}Sub Run()\n{invocation}\nEnd Sub\n"
        );
        let result = output(&source);
        let document = result.document();
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let label = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.nonvalue_identifier.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        assert!(
            matches!(label.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(document.skipped_regions.iter().any(
            |gap| gap.source == label.source && gap.detail == "vba-nonvalue-target-unavailable"
        ));
    }
}

#[test]
fn vba_nested_named_arguments_follow_their_own_application() {
    let source = "Public Function Inner(ByVal value As Long) As Long\nInner = value\nEnd Function\nPublic Sub Visit(ByVal size As Long, ByVal label As Long)\nEnd Sub\nPublic Sub Run(ByVal size As Long)\nCall Visit(size:=Inner(value:=size), label:=size)\nCall Visit(size:=Len(\"a)\"\"(\"), label:=size)\nEnd Sub\n";
    let result = output(source);
    let document = result.document();
    for (marker, owner_name, parameter_name) in [
        ("size:=Inner", "Visit", "size"),
        ("value:=size", "Inner", "value"),
        ("label:=size", "Visit", "label"),
    ] {
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner_name)
            .unwrap();
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == parameter_name
                    && entity.container == Some(ContainerRef::Entity(owner.id))
            })
            .unwrap();
        let start = u64::try_from(source.find(marker).unwrap()).unwrap();
        let label = document
            .occurrences
            .iter()
            .find(|site| site.source.span().start_byte() == start)
            .unwrap();
        assert_eq!(
            label.target,
            OccurrenceTarget::Resolved {
                symbol: parameter.id
            },
            "{marker}"
        );
    }
    let visit = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Visit")
        .unwrap();
    let parameter = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == "label"
                && entity.container == Some(ContainerRef::Entity(visit.id))
        })
        .unwrap();
    let second_label = u64::try_from(source.rfind("label:=size").unwrap()).unwrap();
    assert!(document.occurrences.iter().any(|site| {
        site.source.span().start_byte() == second_label
            && site.target
                == OccurrenceTarget::Resolved {
                    symbol: parameter.id,
                }
    }));
}

#[test]
fn vba_named_arguments_do_not_guess_ambiguous_or_receiver_callees() {
    for source in [
        "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run()\nCall Visit(size:=1)\nEnd Sub\n",
        "Public Function Factory(ByVal size As Long) As Object\nEnd Function\nPublic Sub Run()\nCall Factory().Visit(size:=1)\nEnd Sub\n",
        "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run()\nVisit size:=1\nEnd Sub\n",
        "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run(ByVal Visit As Object)\nVisit size:=1\nEnd Sub\n",
        "Public Function Factory(ByVal size As Long) As Object\nEnd Function\nPublic Sub Run()\nFactory().Visit size:=1\nEnd Sub\n",
        "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run(ByVal receiver As Object)\nreceiver.Visit size:=1\nEnd Sub\n",
        "Public Sub Visit(ByVal size As Long)\nEnd Sub\nPublic Sub Run(ByVal receiver As Object)\nVisit receiver.Visit(size:=1)\nEnd Sub\n",
    ] {
        let result = output(source);
        let start = u64::try_from(source.find("size:=1").unwrap()).unwrap();
        let label = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == "vba.nonvalue_identifier.reference"
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        assert!(
            matches!(label.target, OccurrenceTarget::Unresolved { .. }),
            "{source}"
        );
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.source == label.source && gap.detail == "vba-nonvalue-target-unavailable"
        }));
    }
}

#[test]
fn vba_accessor_parameters_remain_separate_environments() {
    let source = "Private stored As Long\nPublic Property Let Value(ByVal item As Long)\nstored = ITEM\nEnd Property\nPublic Property Let Other(ByVal item As Long)\nstored = item\nEnd Property\n";
    let result = output(source);
    let document = result.document();
    let first = reference_at(document, source, "ITEM\n");
    let second = reference_at(document, source, "item\n");
    assert!(matches!(first.target, OccurrenceTarget::Resolved { .. }));
    assert!(matches!(second.target, OccurrenceTarget::Resolved { .. }));
    assert_ne!(first.target, second.target);
}

#[test]
fn vba_duplicate_module_variables_retain_written_definitions_without_unique_value_binding() {
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            for (declaration, declarator, written_name) in [
                ("Dim stored As Long", "stored As Long", "stored"),
                ("Public stored As Long", "stored As Long", "stored"),
                ("Dim stored", "stored", "stored"),
                ("Private [stored]", "[stored]", "[stored]"),
            ] {
                let body = if native {
                    "local = #1/1/2000#"
                } else {
                    "local = 3"
                };
                let source = format!(
                    "{declaration}\n{declaration}\nSub Update(Optional value = stored)\nEnd Sub\nSub Run()\nDim local As Long\nlocal = stored\n{body}\nCall Update()\nEnd Sub\n"
                );
                let result =
                    application_proof_output(&source, language, IrLimits::default(), &deadline())
                        .unwrap();
                let document = result.document();
                let variables = document
                    .entities
                    .iter()
                    .filter(|entity| {
                        entity.kind == EntityKind::Variable && entity.canonical_name == "stored"
                    })
                    .collect::<Vec<_>>();
                assert_eq!(variables.len(), 1);
                let variable = variables[0];
                let parser = document.files[0].provenance;
                let definitions = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.target
                                == OccurrenceTarget::Resolved {
                                    symbol: variable.id,
                                }
                    })
                    .collect::<Vec<_>>();
                assert_eq!(definitions.len(), 2);
                for (start, _) in source.match_indices(declaration) {
                    let start = start + declaration.len() - declarator.len();
                    let definition = definitions
                        .iter()
                        .find(|site| {
                            site.source.span().start_byte() == u64::try_from(start).unwrap()
                        })
                        .unwrap();
                    assert_eq!(
                        definition.source.span().end_byte(),
                        u64::try_from(start + written_name.len()).unwrap()
                    );
                    assert_eq!(
                        definition.source.content_hash(),
                        content_hash(source.as_bytes())
                    );
                    assert_eq!(definition.provenance, parser);
                    let context = definition.evidence.source.as_ref().unwrap();
                    assert_eq!(context.span().start_byte(), u64::try_from(start).unwrap());
                    assert_eq!(
                        context.span().end_byte(),
                        u64::try_from(start + declarator.len()).unwrap()
                    );
                    let containment = document
                        .relations
                        .iter()
                        .filter(|edge| {
                            edge.predicate == RelationPredicate::Contains
                                && edge.object == RelationEndpoint::Entity(variable.id)
                                && edge.evidence.source.as_ref() == Some(context)
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(containment.len(), 1);
                    assert_eq!(containment[0].provenance, parser);
                }
                let default = document
                    .occurrences
                    .iter()
                    .find(|site| site.syntax_kind == "vba.default_constant_identifier.reference")
                    .unwrap();
                let read_start =
                    u64::try_from(source.find("local = stored").unwrap() + "local = ".len())
                        .unwrap();
                let read = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.value_identifier.reference"
                            && site.source.span().start_byte() == read_start
                    })
                    .unwrap();
                for site in [default, read] {
                    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
                    assert!(document.skipped_regions.iter().any(
                        |gap| gap.source == site.source && gap.domain == FactDomain::Relations
                    ));
                    assert!(
                        !document
                            .relations
                            .iter()
                            .any(|edge| edge.subject == RelationEndpoint::Occurrence(site.id)
                                && edge.predicate == RelationPredicate::RefersTo)
                    );
                }
            }
        }
    }
}

#[test]
fn vba_module_variable_constant_collisions_do_not_choose_a_default_target() {
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            for declarations in [
                "Dim stored As Long\nConst stored = 7",
                "Const stored = 7\nDim stored As Long",
            ] {
                let body = if native {
                    "local = #1/1/2000#"
                } else {
                    "local = 3"
                };
                let source = format!(
                    "{declarations}\nSub Update(Optional value = stored)\nEnd Sub\nSub Run()\nDim local As Long\n{body}\nCall Update()\nEnd Sub\n"
                );
                let result =
                    application_proof_output(&source, language, IrLimits::default(), &deadline())
                        .unwrap();
                let document = result.document();
                let values = document
                    .entities
                    .iter()
                    .filter(|entity| entity.canonical_name == "stored")
                    .collect::<Vec<_>>();
                assert_eq!(values.len(), 2);
                assert!(
                    values
                        .iter()
                        .any(|entity| entity.kind == EntityKind::Variable)
                );
                assert!(
                    values
                        .iter()
                        .any(|entity| entity.kind == EntityKind::Constant)
                );
                let default = document
                    .occurrences
                    .iter()
                    .find(|site| site.syntax_kind == "vba.default_constant_identifier.reference")
                    .unwrap();
                assert!(matches!(
                    default.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert_eq!(
                    document
                        .skipped_regions
                        .iter()
                        .filter(|gap| gap.source == default.source
                            && gap.detail == "vba-default-constant-target-unavailable")
                        .count(),
                    1
                );
                assert!(!document.relations.iter().any(|edge| edge.subject
                    == RelationEndpoint::Occurrence(default.id)
                    && edge.predicate == RelationPredicate::RefersTo));
            }
        }
    }
}

#[test]
fn vba_grouped_module_variables_keep_independent_evidence_and_ambiguous_duplicates() {
    use rootlight_ir::EntityVisibility;
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            for (prefix, visibility) in [
                ("Dim", EntityVisibility::Unknown),
                ("Public", EntityVisibility::Public),
                ("Private", EntityVisibility::Private),
            ] {
                for (declarators, duplicate) in [
                    (
                        [
                            ("stored", "stored"),
                            ("other As Long", "other"),
                            ("grid(2)", "grid"),
                            ("pending()", "pending"),
                        ],
                        false,
                    ),
                    (
                        [
                            ("stored As String", "stored"),
                            ("other", "other"),
                            ("grid(-2 To +3, 004) As Long", "grid"),
                            ("pending()", "pending"),
                        ],
                        false,
                    ),
                    (
                        [
                            ("stored As Long", "stored"),
                            ("[STORED](2)", "[STORED]"),
                            ("grid(2)", "grid"),
                            ("pending()", "pending"),
                        ],
                        true,
                    ),
                    (
                        [
                            ("stored(2)", "stored"),
                            ("[stored] As Variant", "[stored]"),
                            ("grid(-2 To +3, 004)", "grid"),
                            ("pending()", "pending"),
                        ],
                        true,
                    ),
                ] {
                    let declarations = declarators
                        .iter()
                        .map(|(clause, _)| *clause)
                        .collect::<Vec<_>>()
                        .join(", ");
                    let body = if native {
                        "scratch=#1/1/2000#"
                    } else {
                        "scratch=7"
                    };
                    let source = format!(
                        "Attribute VB_Name = \"Grouped\"\n{prefix} {declarations}\nSub Accept(Optional value=stored)\nEnd Sub\nSub Run()\nDim scratch As Long\nscratch=stored\n{body}\nCall stored()\nEnd Sub\n"
                    );
                    let result = application_proof_output(
                        &source,
                        language,
                        IrLimits::default(),
                        &deadline(),
                    )
                    .unwrap();
                    let document = result.document();
                    let module = document
                        .entities
                        .iter()
                        .find(|entity| entity.kind == EntityKind::Module)
                        .unwrap();
                    let variables = document
                        .entities
                        .iter()
                        .filter(|entity| {
                            entity.kind == EntityKind::Variable
                                && entity.container == Some(ContainerRef::Entity(module.id))
                        })
                        .collect::<Vec<_>>();
                    // Declared identity preserves case; VBA binding lookup does not.
                    let canonical_names = declarators
                        .iter()
                        .map(|(_, name)| name.trim_matches(['[', ']']))
                        .collect::<std::collections::BTreeSet<_>>();
                    assert_eq!(variables.len(), canonical_names.len());
                    let parser = document.files[0].provenance;
                    for (clause, name) in declarators {
                        let canonical = name.trim_matches(['[', ']']);
                        let variable = variables
                            .iter()
                            .find(|entity| entity.canonical_name == canonical)
                            .unwrap();
                        assert_eq!(variable.visibility, visibility);
                        assert_eq!(variable.provenance, parser);
                        let start = u64::try_from(source.find(clause).unwrap()).unwrap();
                        let definition = document
                            .occurrences
                            .iter()
                            .find(|site| {
                                site.role == OccurrenceRole::Definition
                                    && site.source.span().start_byte() == start
                            })
                            .unwrap();
                        assert_eq!(
                            definition.target,
                            OccurrenceTarget::Resolved {
                                symbol: variable.id
                            }
                        );
                        assert_eq!(definition.enclosing, Some(module.id));
                        assert_eq!(
                            definition.source.span().end_byte(),
                            start + u64::try_from(name.len()).unwrap()
                        );
                        assert_eq!(definition.provenance, parser);
                        let context = definition.evidence.source.as_ref().unwrap();
                        assert_eq!(context.span().start_byte(), start);
                        assert_eq!(
                            context.span().end_byte(),
                            start + u64::try_from(clause.len()).unwrap()
                        );
                        for reference in [&definition.source, context] {
                            assert_eq!(reference.repository(), document.repository);
                            assert_eq!(reference.generation(), document.generation);
                            assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                            assert_eq!(reference.span().file(), document.files[0].id);
                        }
                        let containment = document
                            .relations
                            .iter()
                            .filter(|edge| {
                                edge.predicate == RelationPredicate::Contains
                                    && edge.subject == RelationEndpoint::Entity(module.id)
                                    && edge.object == RelationEndpoint::Entity(variable.id)
                                    && edge.evidence.source.as_ref() == Some(context)
                            })
                            .collect::<Vec<_>>();
                        assert_eq!(containment.len(), 1);
                        assert_eq!(containment[0].provenance, parser);
                    }
                    let stored = variables
                        .iter()
                        .find(|entity| entity.canonical_name == "stored")
                        .unwrap();
                    assert_eq!(
                        document
                            .occurrences
                            .iter()
                            .filter(|site| site.role == OccurrenceRole::Definition
                                && site.target == OccurrenceTarget::Resolved { symbol: stored.id })
                            .count(),
                        declarators
                            .iter()
                            .filter(|(_, name)| name.trim_matches(['[', ']']) == "stored")
                            .count()
                    );
                    let read_start =
                        u64::try_from(source.find("scratch=stored").unwrap() + "scratch=".len())
                            .unwrap();
                    let read = document
                        .occurrences
                        .iter()
                        .find(|site| {
                            site.syntax_kind == "vba.value_identifier.reference"
                                && site.source.span().start_byte() == read_start
                        })
                        .unwrap();
                    if duplicate {
                        assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
                    } else {
                        assert_eq!(
                            read.target,
                            OccurrenceTarget::Resolved { symbol: stored.id }
                        );
                    }
                    assert_eq!(
                        document
                            .relations
                            .iter()
                            .filter(|edge| edge.subject == RelationEndpoint::Occurrence(read.id)
                                && edge.predicate == RelationPredicate::RefersTo
                                && edge.object == RelationEndpoint::Entity(stored.id))
                            .count(),
                        usize::from(!duplicate)
                    );
                    assert_eq!(
                        document
                            .skipped_regions
                            .iter()
                            .filter(|gap| gap.source == read.source
                                && gap.detail == "vba-binding-target-unavailable")
                            .count(),
                        usize::from(duplicate)
                    );
                    let call_start = u64::try_from(source.find("Call stored()").unwrap()).unwrap();
                    let call = document
                        .occurrences
                        .iter()
                        .find(|site| {
                            site.syntax_kind == "vba.application.reference"
                                && site.source.span().start_byte() == call_start
                        })
                        .unwrap();
                    let default = document
                        .occurrences
                        .iter()
                        .find(|site| {
                            site.syntax_kind == "vba.default_constant_identifier.reference"
                        })
                        .unwrap();
                    for (site, detail) in [
                        (call, "vba-call-or-index-target-unavailable"),
                        (default, "vba-default-constant-target-unavailable"),
                    ] {
                        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
                        assert_eq!(
                            document
                                .skipped_regions
                                .iter()
                                .filter(|gap| gap.source == site.source && gap.detail == detail)
                                .count(),
                            1
                        );
                        assert!(!document.relations.iter().any(|edge| edge.subject
                            == RelationEndpoint::Occurrence(site.id)
                            && matches!(
                                edge.predicate,
                                RelationPredicate::RefersTo | RelationPredicate::Calls
                            )));
                    }
                }
            }
        }
    }
}

#[test]
fn vba_module_numeric_arrays_preserve_call_and_index_boundaries() {
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            for (designator, indices, proven) in [
                ("", "", false),
                ("()", "", true),
                ("()", "0,1", true),
                ("(2)", "0", true),
                ("(2)", "True", true),
                ("(2)", "\"written\"", true),
                ("(2)", "0,1", false),
                ("(-2 To +3)", "0", true),
                ("(-2 To +3, 004)", "0", false),
                ("(-2 To +3, 004)", "0,1", true),
                ("(-2 To +3, 004)", "", true),
                ("(2,3,4)", "0,1,2", true),
            ] {
                let declaration = format!("items{designator} As Long");
                let invocation = format!("Call items({indices})");
                let indexed = format!("items({indices})");
                let body = if native {
                    format!("scratch = #1/1/2000#\nscratch = {indexed}")
                } else {
                    format!("scratch = 3\nscratch = {indexed}")
                };
                let source = format!(
                    "Attribute VB_Name = \"Arrays\"\nPrivate {declaration}\nSub Accept(Optional value=items)\nEnd Sub\nSub Run()\nDim scratch As Long\n{body}\n{invocation}\nEnd Sub\n"
                );
                let result =
                    application_proof_output(&source, language, IrLimits::default(), &deadline())
                        .unwrap();
                let document = result.document();
                let module = document
                    .entities
                    .iter()
                    .find(|entity| entity.kind == EntityKind::Module)
                    .unwrap();
                let variable = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.kind == EntityKind::Variable
                            && entity.canonical_name == "items"
                            && entity.container == Some(ContainerRef::Entity(module.id))
                    })
                    .unwrap();
                let definition = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.target
                                == OccurrenceTarget::Resolved {
                                    symbol: variable.id,
                                }
                    })
                    .unwrap();
                for (reference, written) in [
                    (
                        variable.evidence.source.as_ref().unwrap(),
                        declaration.as_str(),
                    ),
                    (&definition.source, "items"),
                    (
                        definition.evidence.source.as_ref().unwrap(),
                        declaration.as_str(),
                    ),
                ] {
                    let span = reference.span();
                    assert_eq!(
                        source
                            .get(
                                usize::try_from(span.start_byte()).unwrap()
                                    ..usize::try_from(span.end_byte()).unwrap()
                            )
                            .unwrap(),
                        written
                    );
                    assert_eq!(reference.repository(), document.repository);
                    assert_eq!(reference.generation(), document.generation);
                    assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                    assert_eq!(span.file(), document.files[0].id);
                }
                assert_eq!(definition.enclosing, Some(module.id));
                // Explicit Call syntax is not evidence of an array read.
                for (expression, expected) in
                    [(invocation.as_str(), false), (indexed.as_str(), proven)]
                {
                    let start = if expression == invocation {
                        source.find(expression).unwrap()
                    } else {
                        source.find(&format!("scratch = {expression}")).unwrap()
                            + "scratch = ".len()
                    };
                    let start = u64::try_from(start).unwrap();
                    let application = document
                        .occurrences
                        .iter()
                        .find(|site| {
                            site.syntax_kind == "vba.application.reference"
                                && site.source.span().start_byte() == start
                        })
                        .unwrap();
                    assert_eq!(
                        application.source.span().end_byte(),
                        start + u64::try_from(expression.len()).unwrap()
                    );
                    assert_eq!(application.role, OccurrenceRole::Reference);
                    if expected {
                        assert_eq!(
                            application.target,
                            OccurrenceTarget::Resolved {
                                symbol: variable.id
                            },
                            "{language}: {designator}: {expression}"
                        );
                    } else {
                        assert!(matches!(
                            application.target,
                            OccurrenceTarget::Unresolved { .. }
                        ));
                    }
                    assert_eq!(
                        document
                            .relations
                            .iter()
                            .filter(|edge| {
                                edge.subject == RelationEndpoint::Occurrence(application.id)
                                    && edge.predicate == RelationPredicate::RefersTo
                                    && edge.object == RelationEndpoint::Entity(variable.id)
                            })
                            .count(),
                        usize::from(expected)
                    );
                    assert_eq!(
                        document
                            .skipped_regions
                            .iter()
                            .filter(|gap| {
                                gap.source == application.source
                                    && gap.detail == "vba-call-or-index-target-unavailable"
                            })
                            .count(),
                        usize::from(!expected)
                    );
                    assert!(!document.relations.iter().any(|edge| {
                        edge.subject == RelationEndpoint::Occurrence(application.id)
                            && edge.predicate == RelationPredicate::Calls
                    }));
                }
                let default = document
                    .occurrences
                    .iter()
                    .find(|site| site.syntax_kind == "vba.default_constant_identifier.reference")
                    .unwrap();
                assert!(matches!(
                    default.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert!(!document.relations.iter().any(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(default.id)
                        && edge.predicate == RelationPredicate::RefersTo
                }));
                assert_eq!(
                    document
                        .skipped_regions
                        .iter()
                        .filter(|gap| {
                            gap.source == default.source
                                && gap.detail == "vba-default-constant-target-unavailable"
                        })
                        .count(),
                    1
                );
            }
        }
    }
}

#[test]
fn vba_rhs_array_applications_preserve_missing_duplicate_and_shadow_barriers() {
    for (declarations, parameters, locals, callee_kind, module_target) in [
        ("", "", "", None, false),
        (
            "Private items As Variant\n",
            "",
            "",
            Some(EntityKind::Variable),
            true,
        ),
        (
            "Private items(2) As Long\nPrivate [items](2) As Long\n",
            "",
            "",
            None,
            false,
        ),
        (
            "Private items(2) As Long\n",
            "",
            "Dim items As Variant\n",
            Some(EntityKind::Variable),
            false,
        ),
        (
            "Private items(2) As Long\n",
            "ByVal items As Variant",
            "",
            Some(EntityKind::Parameter),
            false,
        ),
    ] {
        for language in ["vba", "visual-basic-classic"] {
            for native in [false, true] {
                let native_tail = if native { "result = #1/1/2000#\n" } else { "" };
                let source = format!(
                    "Attribute VB_Name = \"Arrays\"\n{declarations}Sub Run({parameters})\nDim result As Variant\n{locals}{native_tail}result = items(0)\nEnd Sub\n"
                );
                let output =
                    application_proof_output(&source, language, IrLimits::default(), &deadline())
                        .unwrap();
                let document = output.document();
                let parser = document.files[0].provenance;
                let module = document
                    .entities
                    .iter()
                    .find(|entity| entity.kind == EntityKind::Module)
                    .unwrap();
                let run = document
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == "Run")
                    .unwrap();
                let start = source.find("result = items(0)").unwrap() + "result = ".len();
                let application = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.application.reference"
                            && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    application.source.span().end_byte(),
                    u64::try_from(start + "items(0)".len()).unwrap()
                );
                assert_eq!(application.role, OccurrenceRole::Reference);
                assert_eq!(application.enclosing, Some(run.id));
                assert_eq!(application.provenance, parser);
                assert!(
                    matches!(application.target, OccurrenceTarget::Unresolved { .. }),
                    "{language}: {source}"
                );
                assert_eq!(
                    document
                        .skipped_regions
                        .iter()
                        .filter(|gap| {
                            gap.source == application.source
                                && gap.domain == FactDomain::Relations
                                && gap.detail == "vba-call-or-index-target-unavailable"
                        })
                        .count(),
                    1
                );
                assert!(!document.relations.iter().any(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(application.id)
                        && matches!(
                            edge.predicate,
                            RelationPredicate::RefersTo | RelationPredicate::Calls
                        )
                }));
                let callee = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.invoked_get_identifier.reference"
                            && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    callee.source.span().end_byte(),
                    u64::try_from(start + "items".len()).unwrap()
                );
                assert_eq!(callee.enclosing, Some(run.id));
                assert_ne!(callee.role, OccurrenceRole::CallSite);
                if let Some(kind) = callee_kind {
                    let owner = if module_target { module.id } else { run.id };
                    let target = document
                        .entities
                        .iter()
                        .find(|entity| {
                            entity.kind == kind
                                && entity.canonical_name == "items"
                                && entity.container == Some(ContainerRef::Entity(owner))
                        })
                        .unwrap();
                    assert_eq!(
                        callee.target,
                        OccurrenceTarget::Resolved { symbol: target.id }
                    );
                } else {
                    assert!(matches!(callee.target, OccurrenceTarget::Unresolved { .. }));
                }
                for reference in [&application.source, &callee.source] {
                    assert_eq!(reference.repository(), document.repository);
                    assert_eq!(reference.generation(), document.generation);
                    assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                    assert_eq!(reference.span().file(), document.files[0].id);
                }
                assert!(
                    !document
                        .relations
                        .iter()
                        .any(|edge| edge.predicate == RelationPredicate::Calls)
                );
            }
        }
    }
}

#[test]
fn vba_rhs_array_reads_keep_case_bracket_and_scalar_argument_source_identity() {
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            let native_tail = if native { "result = #1/1/2000#\n" } else { "" };
            let expression = "[ItEmS](slot, [slot])";
            let source = format!(
                "Attribute VB_Name = \"Arrays\"\nPrivate items(-2 To +3, 004) As Long\nSub Run(ByVal slot As Long)\nDim result As Variant\n{native_tail}result = {expression}\nEnd Sub\n"
            );
            let output =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            let document = output.document();
            let parser = document.files[0].provenance;
            let module = document
                .entities
                .iter()
                .find(|entity| entity.kind == EntityKind::Module)
                .unwrap();
            let run = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Run")
                .unwrap();
            let array = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Variable
                        && entity.canonical_name == "items"
                        && entity.container == Some(ContainerRef::Entity(module.id))
                })
                .unwrap();
            let slot = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.canonical_name == "slot"
                        && entity.container == Some(ContainerRef::Entity(run.id))
                })
                .unwrap();
            let start = source.find(expression).unwrap();
            let application = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                        && site.source.span().end_byte()
                            == u64::try_from(start + expression.len()).unwrap()
                })
                .unwrap();
            let callee = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.invoked_get_identifier.reference"
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                })
                .unwrap();
            assert_eq!(
                callee.source.span().end_byte(),
                u64::try_from(start + "[ItEmS]".len()).unwrap()
            );
            for site in [application, callee] {
                assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: array.id });
                assert_eq!(site.role, OccurrenceRole::Reference);
                assert_eq!(site.enclosing, Some(run.id));
                assert_eq!(site.provenance, parser);
                assert_eq!(
                    document
                        .relations
                        .iter()
                        .filter(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(site.id)
                                && edge.predicate == RelationPredicate::RefersTo
                                && edge.object == RelationEndpoint::Entity(array.id)
                        })
                        .count(),
                    1
                );
                assert!(!document.skipped_regions.iter().any(|gap| {
                    gap.source == site.source
                        && gap.detail == "vba-call-or-index-target-unavailable"
                }));
            }
            for written in ["slot", "[slot]"] {
                let name_start = start + expression.find(written).unwrap();
                let read = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.value_identifier.reference"
                            && site.source.span().start_byte() == u64::try_from(name_start).unwrap()
                            && site.source.span().end_byte()
                                == u64::try_from(name_start + written.len()).unwrap()
                    })
                    .unwrap();
                assert_eq!(read.target, OccurrenceTarget::Resolved { symbol: slot.id });
                assert_eq!(read.role, OccurrenceRole::Reference);
                assert_eq!(read.enclosing, Some(run.id));
                assert_eq!(read.syntactic_text_hash, content_hash(written.as_bytes()));
                assert_eq!(read.provenance, parser);
                assert_eq!(
                    document
                        .relations
                        .iter()
                        .filter(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(read.id)
                                && edge.predicate == RelationPredicate::RefersTo
                                && edge.object == RelationEndpoint::Entity(slot.id)
                        })
                        .count(),
                    1
                );
            }
            for site in document.occurrences.iter().filter(|site| {
                site.source.span().start_byte() >= u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        <= u64::try_from(start + expression.len()).unwrap()
            }) {
                assert_eq!(site.source.repository(), document.repository);
                assert_eq!(site.source.generation(), document.generation);
                assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
                assert_eq!(site.source.span().file(), document.files[0].id);
            }
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|edge| edge.predicate == RelationPredicate::Calls)
            );
            assert!(!document.provenance.iter().any(|proof| {
                proof.rule.as_deref() == Some("vba-procedure-application-v1.source")
            }));
        }
    }
}

#[test]
fn vba_rhs_function_applications_require_written_formal_proofs() {
    const RULE: &str = "vba-procedure-application-v1.source";
    for (parameter, arguments, proven) in [
        ("ByVal value As Long", "1", true),
        ("ByVal value As Long", "", false),
        ("Optional value As Long = 7", "", true),
    ] {
        for language in ["vba", "visual-basic-classic"] {
            for native in [false, true] {
                let native_tail = if native { "result = #1/1/2000#\n" } else { "" };
                let expression = format!("Measure({arguments})");
                let source = format!(
                    "Function Measure({parameter}) As Long\nMeasure = 7\nEnd Function\nSub Run()\nDim result As Long\n{native_tail}result = {expression}\nEnd Sub\n"
                );
                let output =
                    application_proof_output(&source, language, IrLimits::default(), &deadline())
                        .unwrap();
                let document = output.document();
                let target = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.kind == EntityKind::Function && entity.canonical_name == "Measure"
                    })
                    .unwrap();
                let start =
                    source.find(&format!("result = {expression}")).unwrap() + "result = ".len();
                let application = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.application.reference"
                            && site.source.span().start_byte() == u64::try_from(start).unwrap()
                            && site.source.span().end_byte()
                                == u64::try_from(start + expression.len()).unwrap()
                    })
                    .unwrap();
                assert_eq!(application.role, OccurrenceRole::Reference);
                assert_eq!(
                    application.target == OccurrenceTarget::Resolved { symbol: target.id },
                    proven,
                    "{language}: {source}"
                );
                assert!(!document.relations.iter().any(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(application.id)
                        && edge.predicate == RelationPredicate::Calls
                }));
                if proven {
                    let parser = document.files[0].provenance;
                    let formal = document
                        .entities
                        .iter()
                        .find(|entity| {
                            entity.kind == EntityKind::Parameter
                                && entity.canonical_name == "value"
                                && entity.container == Some(ContainerRef::Entity(target.id))
                        })
                        .unwrap();
                    let proof = document
                        .provenance
                        .iter()
                        .find(|proof| proof.id == application.provenance)
                        .unwrap();
                    assert_eq!(proof.rule.as_deref(), Some(RULE));
                    assert_eq!(proof.derivation_parents.len(), 3);
                    for parent in [
                        FactRef::Fact(parser),
                        FactRef::Entity(target.id),
                        FactRef::Entity(formal.id),
                    ] {
                        assert!(proof.derivation_parents.contains(&parent));
                    }
                    let written_formal = formal.evidence.source.as_ref().unwrap();
                    assert_eq!(
                        written_formal.span().start_byte(),
                        u64::try_from(source.find(parameter).unwrap()).unwrap()
                    );
                    assert_eq!(
                        written_formal.span().end_byte(),
                        u64::try_from(source.find(parameter).unwrap() + parameter.len()).unwrap()
                    );
                    assert!(proof.input_sources.contains(written_formal));
                    assert!(proof.evidence_sources.contains(written_formal));
                    assert!(proof.evidence_sources.contains(&application.source));
                    let relation = document
                        .relations
                        .iter()
                        .find(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(application.id)
                                && edge.predicate == RelationPredicate::RefersTo
                                && edge.object == RelationEndpoint::Entity(target.id)
                        })
                        .unwrap();
                    assert_eq!(relation.provenance, proof.id);
                    let callee = document
                        .occurrences
                        .iter()
                        .find(|site| {
                            site.syntax_kind == "vba.invoked_get_identifier.reference"
                                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                                && site.source.span().end_byte()
                                    == u64::try_from(start + "Measure".len()).unwrap()
                        })
                        .unwrap();
                    assert_eq!(callee.role, OccurrenceRole::CallSite);
                    assert_eq!(
                        callee.target,
                        OccurrenceTarget::Resolved { symbol: target.id }
                    );
                    assert_eq!(callee.provenance, parser);
                    assert!(document.relations.iter().any(|edge| {
                        edge.subject == RelationEndpoint::Occurrence(callee.id)
                            && edge.predicate == RelationPredicate::Calls
                            && edge.object == RelationEndpoint::Entity(target.id)
                    }));
                    for reference in proof.input_sources.iter().chain(&proof.evidence_sources) {
                        assert_eq!(reference.repository(), document.repository);
                        assert_eq!(reference.generation(), document.generation);
                        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                        assert_eq!(reference.span().file(), document.files[0].id);
                    }
                } else {
                    assert!(matches!(
                        application.target,
                        OccurrenceTarget::Unresolved { .. }
                    ));
                    assert!(document.skipped_regions.iter().any(|gap| {
                        gap.source == application.source
                            && gap.detail == "vba-call-or-index-target-unavailable"
                    }));
                    assert!(
                        !document
                            .provenance
                            .iter()
                            .any(|proof| proof.rule.as_deref() == Some(RULE))
                    );
                    assert!(!document.relations.iter().any(|edge| {
                        edge.subject == RelationEndpoint::Occurrence(application.id)
                            && edge.predicate == RelationPredicate::RefersTo
                    }));
                }
            }
        }
    }
}

#[test]
fn vba_expression_operands_preserve_values_and_implicit_function_calls() {
    let expression = "-(stored + local) * Seed + Count + Maybe + Needs";
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            let native_tail = if native { "result = #1/1/2000#\n" } else { "" };
            let source = format!(
                "Attribute VB_Name = \"Values\"\nPrivate stored As Long\nConst Seed As Long = 7\nFunction Count() As Long\nCount = Count + Seed\nEnd Function\nFunction Maybe(Optional amount As Long = 1) As Long\nMaybe = amount + 1\nEnd Function\nFunction Needs(ByVal amount As Long) As Long\nNeeds = amount + 1\nEnd Function\nSub Run(ByVal stored As Long)\nDim local As Long\nDim result As Variant\n{native_tail}result = {expression}\nEnd Sub\n"
            );
            let output =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            let document = output.document();
            let parser = document.files[0].provenance;
            let run = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Run")
                .unwrap();
            let start = source.find(&format!("result = {expression}")).unwrap() + "result = ".len();
            for (written, kind, local_owner, is_call) in [
                ("stored", EntityKind::Parameter, true, false),
                ("local", EntityKind::Variable, true, false),
                ("Seed", EntityKind::Constant, false, false),
                ("Count", EntityKind::Function, false, true),
                ("Maybe", EntityKind::Function, false, true),
            ] {
                let target = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.canonical_name == written
                            && entity.kind == kind
                            && (!local_owner
                                || entity.container == Some(ContainerRef::Entity(run.id)))
                    })
                    .unwrap();
                let offset = start + expression.find(written).unwrap();
                let site = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.value_identifier.reference"
                            && site.source.span().start_byte() == u64::try_from(offset).unwrap()
                            && site.source.span().end_byte()
                                == u64::try_from(offset + written.len()).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    site.target,
                    OccurrenceTarget::Resolved { symbol: target.id },
                    "{language}: {source}"
                );
                assert_eq!(
                    site.role,
                    if is_call {
                        OccurrenceRole::CallSite
                    } else {
                        OccurrenceRole::Reference
                    }
                );
                assert_eq!(site.enclosing, Some(run.id));
                assert_eq!(site.syntactic_text_hash, content_hash(written.as_bytes()));
                assert_eq!(site.provenance, parser);
                let predicate = if is_call {
                    RelationPredicate::Calls
                } else {
                    RelationPredicate::RefersTo
                };
                assert_eq!(
                    document
                        .relations
                        .iter()
                        .filter(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(site.id)
                                && edge.predicate == predicate
                                && edge.object == RelationEndpoint::Entity(target.id)
                        })
                        .count(),
                    1
                );
                assert!(!document.relations.iter().any(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.predicate
                            == if is_call {
                                RelationPredicate::RefersTo
                            } else {
                                RelationPredicate::Calls
                            }
                }));
                assert_eq!(site.source.repository(), document.repository);
                assert_eq!(site.source.generation(), document.generation);
                assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
                assert_eq!(site.source.span().file(), document.files[0].id);
            }
            let needs_start = start + expression.find("Needs").unwrap();
            let needs = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.value_identifier.reference"
                        && site.source.span().start_byte() == u64::try_from(needs_start).unwrap()
                })
                .unwrap();
            assert_eq!(
                needs.source.span().end_byte(),
                u64::try_from(needs_start + "Needs".len()).unwrap()
            );
            assert!(matches!(needs.target, OccurrenceTarget::Unresolved { .. }));
            assert_eq!(needs.role, OccurrenceRole::Reference);
            assert!(!document.relations.iter().any(|edge| {
                edge.subject == RelationEndpoint::Occurrence(needs.id)
                    && matches!(
                        edge.predicate,
                        RelationPredicate::RefersTo | RelationPredicate::Calls
                    )
            }));
            let count = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function && entity.canonical_name == "Count"
                })
                .unwrap();
            let result_start = source.find("Count = Count + Seed").unwrap() + "Count = ".len();
            let self_result = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.value_identifier.reference"
                        && site.source.span().start_byte() == u64::try_from(result_start).unwrap()
                        && site.source.span().end_byte()
                            == u64::try_from(result_start + "Count".len()).unwrap()
                })
                .unwrap();
            assert_eq!(
                self_result.target,
                OccurrenceTarget::Resolved { symbol: count.id }
            );
            assert_eq!(self_result.enclosing, Some(count.id));
            assert_eq!(self_result.role, OccurrenceRole::Reference);
            assert_eq!(self_result.provenance, parser);
            assert!(document.relations.iter().any(|edge| {
                edge.subject == RelationEndpoint::Occurrence(self_result.id)
                    && edge.predicate == RelationPredicate::RefersTo
                    && edge.object == RelationEndpoint::Entity(count.id)
            }));
            assert!(!document.relations.iter().any(|edge| {
                edge.subject == RelationEndpoint::Occurrence(self_result.id)
                    && edge.predicate == RelationPredicate::Calls
            }));
            assert!(!document.occurrences.iter().any(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() >= u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        <= u64::try_from(start + expression.len()).unwrap()
            }));
        }
    }
}

#[test]
fn vba_expression_reads_preserve_missing_duplicate_and_shadow_barriers() {
    for (declarations, parameters, locals, expected_kind) in [
        ("", "", "", None),
        (
            "Public stored As Long\nPrivate [stored] As Long\n",
            "",
            "",
            None,
        ),
        (
            "Private stored As Long\n",
            "",
            "Dim stored As Long\n",
            Some(EntityKind::Variable),
        ),
        (
            "Private stored As Long\n",
            "ByVal stored As Long",
            "",
            Some(EntityKind::Parameter),
        ),
    ] {
        for language in ["vba", "visual-basic-classic"] {
            for native in [false, true] {
                let native_tail = if native { "result = #1/1/2000#\n" } else { "" };
                let source = format!(
                    "Attribute VB_Name = \"Values\"\n{declarations}Sub Run({parameters})\nDim result As Variant\n{locals}{native_tail}result = -([stored] + 1) * 2\nEnd Sub\n"
                );
                let output =
                    application_proof_output(&source, language, IrLimits::default(), &deadline())
                        .unwrap();
                let document = output.document();
                let run = document
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == "Run")
                    .unwrap();
                let start = source.find("-([stored]").unwrap() + 2;
                let site = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.value_identifier.reference"
                            && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    site.source.span().end_byte(),
                    u64::try_from(start + "[stored]".len()).unwrap()
                );
                assert_eq!(site.syntactic_text_hash, content_hash(b"[stored]"));
                assert_eq!(site.role, OccurrenceRole::Reference);
                assert_eq!(site.enclosing, Some(run.id));
                assert_eq!(site.provenance, document.files[0].provenance);
                if let Some(kind) = expected_kind {
                    let target = document
                        .entities
                        .iter()
                        .find(|entity| {
                            entity.canonical_name == "stored"
                                && entity.kind == kind
                                && entity.container == Some(ContainerRef::Entity(run.id))
                        })
                        .unwrap();
                    assert_eq!(
                        site.target,
                        OccurrenceTarget::Resolved { symbol: target.id }
                    );
                    assert_eq!(
                        document
                            .relations
                            .iter()
                            .filter(|edge| {
                                edge.subject == RelationEndpoint::Occurrence(site.id)
                                    && edge.predicate == RelationPredicate::RefersTo
                                    && edge.object == RelationEndpoint::Entity(target.id)
                            })
                            .count(),
                        1
                    );
                } else {
                    assert!(
                        matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                        "{language}: {source}"
                    );
                    assert!(!document.relations.iter().any(|edge| {
                        edge.subject == RelationEndpoint::Occurrence(site.id)
                            && edge.predicate == RelationPredicate::RefersTo
                    }));
                }
                assert!(!document.relations.iter().any(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.predicate == RelationPredicate::Calls
                }));
                assert_eq!(site.source.repository(), document.repository);
                assert_eq!(site.source.generation(), document.generation);
                assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
                assert_eq!(site.source.span().file(), document.files[0].id);
            }
        }
    }
}

#[test]
fn vba_comparison_chains_and_logical_mixes_bind_exact_scalar_reads() {
    for expression in [
        "slot < 1 < 2",
        "slot = 1 And slot <> 2",
        "slot = 1 Or slot <> 2",
        "slot = 1 Xor slot <> 2",
        "slot = 1 Eqv slot <> 2",
        "slot = 1 Imp slot <> 2",
    ] {
        let statement = format!("result = {expression}");
        let source = format!(
            "Sub Run(ByVal slot As Variant)\r\nDim result As Variant\r\n{statement}\r\nEnd Sub\r\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let provider = Arc::new(provider());
            let case = LanguageCase {
                name: language,
                ..VBA
            };
            let fixture = Fixture::new(case, source.as_bytes());
            let budget = limits();
            let request = request(&fixture.snapshot, &fixture.source, case, &budget);
            let parsed = rootlight_adapter_sdk::execute_parse(
                provider.as_ref(),
                &request.to_parse_request(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
            assert_eq!(
                parsed.report().coverage().status(),
                CoverageStatus::Complete,
                "{language}: {expression}"
            );
            assert_eq!(
                parsed.report().coverage().covered_source_bytes(),
                source.len()
            );
            assert!(parsed.diagnostics().is_empty());
            let output = execute_analysis(
                &analyzer(&provider, case),
                &request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
            let document = output.document();
            let parser = document.files[0].provenance;
            if language == "vba" {
                assert_eq!(
                    output.report().coverage().status(),
                    CoverageStatus::Complete
                );
                assert!(document.skipped_regions.is_empty());
            } else {
                assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
                assert_eq!(document.skipped_regions.len(), 1);
                let gap = &document.skipped_regions[0];
                assert_eq!(gap.detail, "visual-basic-dialect-unproven");
                assert_eq!(gap.domain, FactDomain::Relations);
                assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
                assert_eq!(gap.provenance, parser);
                assert_eq!(gap.source, fixture.source);
                assert_eq!(gap.source.repository(), document.repository);
                assert_eq!(gap.source.generation(), document.generation);
                assert_eq!(gap.source.content_hash(), content_hash(source.as_bytes()));
                assert_eq!(gap.source.span().start_byte(), 0);
                assert_eq!(
                    gap.source.span().end_byte(),
                    u64::try_from(source.len()).unwrap()
                );
            }
            let run = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Run")
                .unwrap();
            let statement_start = source.find(&statement).unwrap();
            let expression_start = statement_start + "result = ".len();
            let mut expected = vec![(
                "vba.let_identifier.reference",
                "result",
                EntityKind::Variable,
                statement_start,
            )];
            for (offset, written) in expression.match_indices("slot") {
                expected.push((
                    "vba.value_identifier.reference",
                    written,
                    EntityKind::Parameter,
                    expression_start + offset,
                ));
            }
            let occurrences = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.source.span().start_byte() >= u64::try_from(statement_start).unwrap()
                        && site.source.span().end_byte()
                            <= u64::try_from(statement_start + statement.len()).unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(occurrences.len(), expected.len());
            for (syntax, written, kind, offset) in expected {
                let target = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.canonical_name == written
                            && entity.kind == kind
                            && entity.container == Some(ContainerRef::Entity(run.id))
                    })
                    .unwrap();
                let site = occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == syntax
                            && site.source.span().start_byte() == u64::try_from(offset).unwrap()
                            && site.source.span().end_byte()
                                == u64::try_from(offset + written.len()).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    site.target,
                    OccurrenceTarget::Resolved { symbol: target.id }
                );
                assert_eq!(site.role, OccurrenceRole::Reference);
                assert_eq!(site.enclosing, Some(run.id));
                assert_eq!(site.syntactic_text_hash, content_hash(written.as_bytes()));
                assert_eq!(site.provenance, parser);
                assert_eq!(site.source.repository(), document.repository);
                assert_eq!(site.source.generation(), document.generation);
                assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
                assert_eq!(site.source.span().file(), document.files[0].id);
                assert_eq!(
                    document
                        .relations
                        .iter()
                        .filter(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(site.id)
                                && edge.predicate == RelationPredicate::RefersTo
                                && edge.object == RelationEndpoint::Entity(target.id)
                                && edge.provenance == parser
                        })
                        .count(),
                    1
                );
                assert!(!document.relations.iter().any(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(site.id)
                        && edge.predicate == RelationPredicate::Calls
                }));
            }
            assert_eq!(provider.stats().pooled_parsers, 0);
            assert_eq!(provider.stats().checked_out_parsers, 0);
            assert_eq!(provider.stats().cache.entries, 0);
        }
    }
}

#[test]
fn vba_nested_expression_applications_retain_formal_and_array_provenance() {
    const RULE: &str = "vba-procedure-application-v1.source";
    let arithmetic = "Outer(Inner(slot + 1), items(-(slot - 2))) + items(slot * 2)";
    let arithmetic_applications = [
        (
            "Outer(Inner(slot + 1), items(-(slot - 2)))",
            "Outer",
            EntityKind::Function,
            2,
        ),
        ("Inner(slot + 1)", "Inner", EntityKind::Function, 1),
        ("items(-(slot - 2))", "items", EntityKind::Variable, 0),
        ("items(slot * 2)", "items", EntityKind::Variable, 0),
    ];
    let comparative =
        "Outer(Inner(slot < 1 < 2), items(slot = 1 Xor slot <> 2)) + items(slot > 1 Imp slot < 2)";
    let comparative_applications = [
        (
            "Outer(Inner(slot < 1 < 2), items(slot = 1 Xor slot <> 2))",
            "Outer",
            EntityKind::Function,
            2,
        ),
        ("Inner(slot < 1 < 2)", "Inner", EntityKind::Function, 1),
        (
            "items(slot = 1 Xor slot <> 2)",
            "items",
            EntityKind::Variable,
            0,
        ),
        (
            "items(slot > 1 Imp slot < 2)",
            "items",
            EntityKind::Variable,
            0,
        ),
    ];
    // The native grammar is not an oracle for the admitted comparison-chain forms.
    for (expression, applications, native) in [
        (arithmetic, arithmetic_applications, false),
        (arithmetic, arithmetic_applications, true),
        (comparative, comparative_applications, false),
    ] {
        for language in ["vba", "visual-basic-classic"] {
            let native_tail = if native { "result = #1/1/2000#\n" } else { "" };
            let source = format!(
                "Attribute VB_Name = \"Expressions\"\nPrivate items(-2 To +2) As Long\nFunction Inner(ByVal value As Long) As Long\nInner = value + 1\nEnd Function\nFunction Outer(ByVal value As Long, Optional ByVal other As Long = 0) As Long\nOuter = value + other\nEnd Function\nSub Run(ByVal slot As Long)\nDim result As Long\n{native_tail}result = {expression}\nEnd Sub\n"
            );
            let provider = Arc::new(provider());
            let case = LanguageCase {
                name: language,
                ..VBA
            };
            let fixture = Fixture::new(case, source.as_bytes());
            let budget = limits();
            let request = request(&fixture.snapshot, &fixture.source, case, &budget);
            if !native {
                let parsed = rootlight_adapter_sdk::execute_parse(
                    provider.as_ref(),
                    &request.to_parse_request(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
                assert_eq!(
                    parsed.report().coverage().status(),
                    CoverageStatus::Complete,
                    "{language}: {expression}"
                );
                assert_eq!(
                    parsed.report().coverage().covered_source_bytes(),
                    source.len()
                );
                assert!(parsed.diagnostics().is_empty());
            }
            let output = execute_analysis(
                &analyzer(&provider, case),
                &request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .unwrap();
            if !native {
                assert_eq!(provider.stats().pooled_parsers, 0);
                assert_eq!(provider.stats().checked_out_parsers, 0);
                assert_eq!(provider.stats().cache.entries, 0);
            }
            let document = output.document();
            let parser = document.files[0].provenance;
            if !native {
                if language == "vba" {
                    assert_eq!(
                        output.report().coverage().status(),
                        CoverageStatus::Complete
                    );
                    assert!(document.skipped_regions.is_empty());
                } else {
                    assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
                    assert_eq!(document.skipped_regions.len(), 1);
                    let gap = &document.skipped_regions[0];
                    assert_eq!(gap.detail, "visual-basic-dialect-unproven");
                    assert_eq!(gap.domain, FactDomain::Relations);
                    assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
                    assert_eq!(gap.provenance, parser);
                    assert_eq!(gap.source, fixture.source);
                    assert_eq!(gap.source.repository(), document.repository);
                    assert_eq!(gap.source.generation(), document.generation);
                    assert_eq!(gap.source.content_hash(), content_hash(source.as_bytes()));
                    assert_eq!(gap.source.span().start_byte(), 0);
                    assert_eq!(
                        gap.source.span().end_byte(),
                        u64::try_from(source.len()).unwrap()
                    );
                }
            }
            let run = document
                .entities
                .iter()
                .find(|entity| entity.canonical_name == "Run")
                .unwrap();
            let start = source.find(&format!("result = {expression}")).unwrap() + "result = ".len();
            for (written, name, kind, formal_count) in applications {
                let offset = start + expression.find(written).unwrap();
                let target = document
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == name && entity.kind == kind)
                    .unwrap();
                let application = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.application.reference"
                            && site.source.span().start_byte() == u64::try_from(offset).unwrap()
                            && site.source.span().end_byte()
                                == u64::try_from(offset + written.len()).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    application.target,
                    OccurrenceTarget::Resolved { symbol: target.id },
                    "{language}: {written}: {source}"
                );
                assert_eq!(application.role, OccurrenceRole::Reference);
                assert_eq!(application.enclosing, Some(run.id));
                assert_eq!(
                    application.syntactic_text_hash,
                    content_hash(written.as_bytes())
                );
                let refers = document
                    .relations
                    .iter()
                    .find(|edge| {
                        edge.subject == RelationEndpoint::Occurrence(application.id)
                            && edge.predicate == RelationPredicate::RefersTo
                            && edge.object == RelationEndpoint::Entity(target.id)
                    })
                    .unwrap();
                assert_eq!(refers.provenance, application.provenance);
                assert!(!document.relations.iter().any(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(application.id)
                        && edge.predicate == RelationPredicate::Calls
                }));
                assert!(!document.skipped_regions.iter().any(|gap| {
                    gap.source == application.source
                        && gap.detail == "vba-call-or-index-target-unavailable"
                }));
                let callee = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.invoked_get_identifier.reference"
                            && site.source.span().start_byte() == u64::try_from(offset).unwrap()
                            && site.source.span().end_byte()
                                == u64::try_from(offset + name.len()).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    callee.target,
                    OccurrenceTarget::Resolved { symbol: target.id }
                );
                assert_eq!(callee.provenance, parser);
                assert_eq!(callee.enclosing, Some(run.id));
                assert_eq!(
                    callee.role,
                    if kind == EntityKind::Function {
                        OccurrenceRole::CallSite
                    } else {
                        OccurrenceRole::Reference
                    }
                );
                assert_eq!(
                    document
                        .relations
                        .iter()
                        .filter(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(callee.id)
                                && edge.predicate
                                    == if kind == EntityKind::Function {
                                        RelationPredicate::Calls
                                    } else {
                                        RelationPredicate::RefersTo
                                    }
                                && edge.object == RelationEndpoint::Entity(target.id)
                        })
                        .count(),
                    1
                );
                if kind == EntityKind::Function {
                    let proof = document
                        .provenance
                        .iter()
                        .find(|proof| proof.id == application.provenance)
                        .unwrap();
                    assert_eq!(proof.rule.as_deref(), Some(RULE));
                    let formals = document
                        .entities
                        .iter()
                        .filter(|entity| {
                            entity.kind == EntityKind::Parameter
                                && entity.container == Some(ContainerRef::Entity(target.id))
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(formals.len(), formal_count);
                    assert_eq!(proof.derivation_parents.len(), formal_count + 2);
                    assert!(proof.derivation_parents.contains(&FactRef::Fact(parser)));
                    assert!(
                        proof
                            .derivation_parents
                            .contains(&FactRef::Entity(target.id))
                    );
                    assert!(proof.evidence_sources.contains(&application.source));
                    for formal in formals {
                        assert!(
                            proof
                                .derivation_parents
                                .contains(&FactRef::Entity(formal.id))
                        );
                        let clause = formal.evidence.source.as_ref().unwrap();
                        let written = source
                            .get(
                                usize::try_from(clause.span().start_byte()).unwrap()
                                    ..usize::try_from(clause.span().end_byte()).unwrap(),
                            )
                            .unwrap();
                        assert_eq!(
                            written,
                            if formal.canonical_name == "other" {
                                "Optional ByVal other As Long = 0"
                            } else {
                                "ByVal value As Long"
                            }
                        );
                        assert!(proof.input_sources.contains(clause));
                        assert!(proof.evidence_sources.contains(clause));
                    }
                    for reference in proof.input_sources.iter().chain(&proof.evidence_sources) {
                        assert_eq!(reference.repository(), document.repository);
                        assert_eq!(reference.generation(), document.generation);
                        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                        assert_eq!(reference.span().file(), document.files[0].id);
                    }
                } else {
                    assert_eq!(application.provenance, parser);
                    let declaration = target.evidence.source.as_ref().unwrap();
                    assert_eq!(
                        source
                            .get(
                                usize::try_from(declaration.span().start_byte()).unwrap()
                                    ..usize::try_from(declaration.span().end_byte()).unwrap()
                            )
                            .unwrap(),
                        "items(-2 To +2) As Long"
                    );
                }
                for reference in [&application.source, &callee.source] {
                    assert_eq!(reference.repository(), document.repository);
                    assert_eq!(reference.generation(), document.generation);
                    assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                    assert_eq!(reference.span().file(), document.files[0].id);
                }
            }
            let slot = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.canonical_name == "slot"
                        && entity.container == Some(ContainerRef::Entity(run.id))
                })
                .unwrap();
            assert_eq!(
                document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.syntax_kind == "vba.value_identifier.reference"
                            && site.source.span().start_byte() >= u64::try_from(start).unwrap()
                            && site.source.span().end_byte()
                                <= u64::try_from(start + expression.len()).unwrap()
                    })
                    .count(),
                expression.match_indices("slot").count()
            );
            for (offset, _) in expression.match_indices("slot") {
                let position = start + offset;
                let read = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.value_identifier.reference"
                            && site.source.span().start_byte() == u64::try_from(position).unwrap()
                            && site.source.span().end_byte()
                                == u64::try_from(position + "slot".len()).unwrap()
                    })
                    .unwrap();
                assert_eq!(read.target, OccurrenceTarget::Resolved { symbol: slot.id });
                assert_eq!(read.role, OccurrenceRole::Reference);
                assert_eq!(read.enclosing, Some(run.id));
                assert_eq!(read.provenance, parser);
                assert_eq!(read.syntactic_text_hash, content_hash(b"slot"));
                assert_eq!(read.source.repository(), document.repository);
                assert_eq!(read.source.generation(), document.generation);
                assert_eq!(read.source.content_hash(), content_hash(source.as_bytes()));
                assert_eq!(read.source.span().file(), document.files[0].id);
                assert_eq!(
                    document
                        .relations
                        .iter()
                        .filter(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(read.id)
                                && edge.predicate == RelationPredicate::RefersTo
                                && edge.object == RelationEndpoint::Entity(slot.id)
                                && edge.provenance == parser
                        })
                        .count(),
                    1
                );
                assert!(!document.relations.iter().any(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(read.id)
                        && edge.predicate == RelationPredicate::Calls
                }));
            }
        }
    }
}

#[test]
fn vba_explicit_expression_calls_retain_exact_formal_proofs_without_duplicate_calls() {
    for statement in [
        "Call Target(value + 1, Inner(other) * -2)",
        "Call Target(value < 1 < 2, Inner(other = 1 Xor other <> 2))",
    ] {
        for language in ["vba", "visual-basic-classic"] {
            for native in [false, true] {
                if native && statement.contains("< 1 < 2") {
                    continue;
                }
                let native_tail = if native {
                    "Dim scratch As Long\nscratch = #1/1/2000#\n"
                } else {
                    ""
                };
                let source = format!(
                    "Sub Target(ByVal first As Long, ByVal second As Long)\nEnd Sub\nFunction Inner(ByVal item As Long) As Long\nEnd Function\nSub Run(ByVal value As Long, ByVal other As Long)\n{native_tail}{statement}\nEnd Sub\n"
                );
                let provider = Arc::new(provider());
                let case = LanguageCase {
                    name: language,
                    ..VBA
                };
                let fixture = Fixture::new(case, source.as_bytes());
                let budget = limits();
                let request = request(&fixture.snapshot, &fixture.source, case, &budget);
                if !native {
                    let parsed = rootlight_adapter_sdk::execute_parse(
                        provider.as_ref(),
                        &request.to_parse_request(),
                        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                        &deadline(),
                    )
                    .unwrap();
                    assert_eq!(
                        parsed.report().coverage().status(),
                        CoverageStatus::Complete
                    );
                    assert_eq!(
                        parsed.report().coverage().covered_source_bytes(),
                        source.len()
                    );
                    assert!(parsed.diagnostics().is_empty());
                }
                let output = execute_analysis(
                    &analyzer(&provider, case),
                    &request,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
                let document = output.document();
                let parser = document.files[0].provenance;
                if !native {
                    assert_eq!(provider.stats().pooled_parsers, 0);
                    assert_eq!(provider.stats().checked_out_parsers, 0);
                    assert_eq!(provider.stats().cache.entries, 0);
                    if language == "vba" {
                        assert_eq!(
                            output.report().coverage().status(),
                            CoverageStatus::Complete
                        );
                        assert!(document.skipped_regions.is_empty());
                    } else {
                        assert_eq!(output.report().coverage().status(), CoverageStatus::Bounded);
                        assert_eq!(document.skipped_regions.len(), 1);
                        let gap = &document.skipped_regions[0];
                        assert_eq!(gap.detail, "visual-basic-dialect-unproven");
                        assert_eq!(gap.domain, FactDomain::Relations);
                        assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
                        assert_eq!(gap.source, fixture.source);
                        assert_eq!(gap.provenance, parser);
                    }
                }
                assert_explicit_expression_application_proof(
                    document,
                    &source,
                    statement,
                    "Target",
                    "vba.invoked_identifier.reference",
                    &["ByVal first As Long", "ByVal second As Long"],
                );
                let nested = if statement.contains("< 1 < 2") {
                    "Inner(other = 1 Xor other <> 2)"
                } else {
                    "Inner(other)"
                };
                assert_explicit_expression_application_proof(
                    document,
                    &source,
                    nested,
                    "Inner",
                    "vba.invoked_get_identifier.reference",
                    &["ByVal item As Long"],
                );
                let start = source.find(statement).unwrap();
                let end = start + statement.len();
                assert_eq!(
                    document
                        .occurrences
                        .iter()
                        .filter(|site| {
                            site.syntax_kind == "vba.application.reference"
                                && site.source.span().start_byte() >= u64::try_from(start).unwrap()
                                && site.source.span().end_byte() <= u64::try_from(end).unwrap()
                        })
                        .count(),
                    2
                );
                let run = document
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == "Run")
                    .unwrap();
                for name in ["value", "other"] {
                    let formal = document
                        .entities
                        .iter()
                        .find(|entity| {
                            entity.kind == EntityKind::Parameter
                                && entity.canonical_name == name
                                && entity.container == Some(ContainerRef::Entity(run.id))
                        })
                        .unwrap();
                    for (offset, written) in statement.match_indices(name) {
                        let position = start + offset;
                        let read = document
                            .occurrences
                            .iter()
                            .find(|site| {
                                site.syntax_kind == "vba.value_identifier.reference"
                                    && site.source.span().start_byte()
                                        == u64::try_from(position).unwrap()
                                    && site.source.span().end_byte()
                                        == u64::try_from(position + written.len()).unwrap()
                            })
                            .unwrap();
                        assert_eq!(
                            read.target,
                            OccurrenceTarget::Resolved { symbol: formal.id }
                        );
                        assert_eq!(read.enclosing, Some(run.id));
                        assert_eq!(read.provenance, parser);
                        assert_eq!(read.syntactic_text_hash, content_hash(written.as_bytes()));
                        assert_eq!(
                            document
                                .relations
                                .iter()
                                .filter(|edge| {
                                    edge.subject == RelationEndpoint::Occurrence(read.id)
                                        && edge.predicate == RelationPredicate::RefersTo
                                        && edge.object == RelationEndpoint::Entity(formal.id)
                                })
                                .count(),
                            1
                        );
                        assert!(!document.relations.iter().any(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(read.id)
                                && edge.predicate == RelationPredicate::Calls
                        }));
                    }
                }
                assert_eq!(document.provenance.len(), 3);
            }
        }
    }
}

fn assert_explicit_expression_application_proof(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
    written: &str,
    name: &str,
    callee_syntax: &str,
    clauses: &[&str],
) {
    let start = source.find(written).unwrap();
    let end = start + written.len();
    let target = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == name && entity.kind == EntityKind::Function)
        .unwrap();
    let run = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Run")
        .unwrap();
    let parser = document
        .provenance
        .iter()
        .find(|proof| proof.id == document.files[0].provenance)
        .unwrap();
    let matches = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(end).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{written}");
    let application = matches[0];
    assert_eq!(
        application.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert_eq!(application.role, OccurrenceRole::Reference);
    assert_eq!(application.enclosing, Some(run.id));
    assert_eq!(
        application.syntactic_text_hash,
        content_hash(written.as_bytes())
    );
    let proof = document
        .provenance
        .iter()
        .find(|proof| proof.id == application.provenance)
        .unwrap();
    assert_eq!(
        proof.rule.as_deref(),
        Some("vba-procedure-application-v1.source")
    );
    assert_eq!(proof.producer, parser.producer);
    assert_eq!(proof.producer_kind, ProducerKind::Parser);
    assert_eq!(proof.binary_digest, parser.binary_digest);
    assert_eq!(proof.frontend_version, parser.frontend_version);
    assert_eq!(proof.language, parser.language);
    assert_eq!(proof.build_context, parser.build_context);
    assert!(proof.derivation_parents.contains(&FactRef::Fact(parser.id)));
    assert!(
        proof
            .derivation_parents
            .contains(&FactRef::Entity(target.id))
    );
    assert_eq!(proof.derivation_parents.len(), clauses.len() + 2);
    assert!(proof.input_sources.contains(&parser.input_sources[0]));
    assert!(proof.evidence_sources.contains(&application.source));
    let formals = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.container == Some(ContainerRef::Entity(target.id))
        })
        .collect::<Vec<_>>();
    assert_eq!(formals.len(), clauses.len());
    for clause in clauses {
        let offset = source.find(clause).unwrap();
        let formal = formals
            .iter()
            .find(|formal| {
                formal.evidence.source.as_ref().is_some_and(|reference| {
                    reference.span().start_byte() == u64::try_from(offset).unwrap()
                        && reference.span().end_byte()
                            == u64::try_from(offset + clause.len()).unwrap()
                })
            })
            .unwrap();
        let reference = formal.evidence.source.as_ref().unwrap();
        assert!(
            proof
                .derivation_parents
                .contains(&FactRef::Entity(formal.id))
        );
        assert!(proof.input_sources.contains(reference));
        assert!(proof.evidence_sources.contains(reference));
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: formal.id }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        let definition = definitions[0];
        assert_eq!(definition.enclosing, Some(target.id));
        assert!(proof.evidence_sources.contains(&definition.source));
        assert_eq!(
            source
                .get(
                    usize::try_from(definition.source.span().start_byte()).unwrap()
                        ..usize::try_from(definition.source.span().end_byte()).unwrap()
                )
                .unwrap(),
            formal.canonical_name
        );
    }
    for reference in proof
        .input_sources
        .iter()
        .chain(&proof.evidence_sources)
        .chain([&application.source])
    {
        assert_eq!(reference.repository(), document.repository);
        assert_eq!(reference.generation(), document.generation);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        assert_eq!(reference.span().file(), document.files[0].id);
        assert!(reference.span().end_byte() <= u64::try_from(source.len()).unwrap());
    }
    let refers = document
        .relations
        .iter()
        .filter(|edge| {
            edge.subject == RelationEndpoint::Occurrence(application.id)
                && edge.predicate == RelationPredicate::RefersTo
                && edge.object == RelationEndpoint::Entity(target.id)
        })
        .collect::<Vec<_>>();
    assert_eq!(refers.len(), 1);
    assert_eq!(refers[0].provenance, proof.id);
    assert_eq!(
        refers[0].evidence.source.as_ref(),
        Some(&application.source)
    );
    assert!(!document.relations.iter().any(|edge| {
        edge.subject == RelationEndpoint::Occurrence(application.id)
            && edge.predicate == RelationPredicate::Calls
    }));
    let callee_start = start + written.find(name).unwrap();
    let callees = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == callee_syntax
                && site.source.span().start_byte() == u64::try_from(callee_start).unwrap()
                && site.source.span().end_byte()
                    == u64::try_from(callee_start + name.len()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(callees.len(), 1);
    let callee = callees[0];
    assert_eq!(callee.role, OccurrenceRole::CallSite);
    assert_eq!(
        callee.target,
        OccurrenceTarget::Resolved { symbol: target.id }
    );
    assert_eq!(callee.enclosing, Some(run.id));
    assert_eq!(callee.provenance, parser.id);
    assert_eq!(callee.syntactic_text_hash, content_hash(name.as_bytes()));
    assert_eq!(callee.source.repository(), document.repository);
    assert_eq!(callee.source.generation(), document.generation);
    assert_eq!(
        callee.source.content_hash(),
        content_hash(source.as_bytes())
    );
    assert_eq!(callee.source.span().file(), document.files[0].id);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(callee.id)
                    && edge.predicate == RelationPredicate::Calls
                    && edge.object == RelationEndpoint::Entity(target.id)
            })
            .count(),
        1
    );
}

#[test]
fn vba_explicit_expression_calls_keep_missing_duplicate_shadowed_and_wrong_arity_unproven() {
    const TARGET: &str = "Sub Target(ByVal first As Long, ByVal second As Long)\nEnd Sub\n";
    for (prefix, local, statement) in [
        (String::new(), "", "Call Target(value + 1, Inner(other))"),
        (TARGET.repeat(2), "", "Call Target(value + 1, Inner(other))"),
        (
            TARGET.to_owned(),
            "Dim Target As Long\n",
            "Call Target(value + 1, Inner(other))",
        ),
        (TARGET.to_owned(), "", "Call Target(Inner(other) + 1)"),
        (
            TARGET.to_owned(),
            "",
            "Call Target(value + 1, Inner(other), other + 2)",
        ),
    ] {
        let source = format!(
            "{prefix}Function Inner(ByVal item As Long) As Long\nEnd Function\nSub Run(ByVal value As Long, ByVal other As Long)\n{local}{statement}\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let output =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            let document = output.document();
            let start = source.find(statement).unwrap();
            let application = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                        && site.source.span().end_byte()
                            == u64::try_from(start + statement.len()).unwrap()
                })
                .unwrap();
            assert!(
                matches!(application.target, OccurrenceTarget::Unresolved { .. }),
                "{language}: {source}"
            );
            assert_eq!(application.provenance, document.files[0].provenance);
            assert!(!document.relations.iter().any(|edge| {
                edge.subject == RelationEndpoint::Occurrence(application.id)
                    && matches!(
                        edge.predicate,
                        RelationPredicate::RefersTo | RelationPredicate::Calls
                    )
            }));
            assert_explicit_expression_application_proof(
                document,
                &source,
                "Inner(other)",
                "Inner",
                "vba.invoked_get_identifier.reference",
                &["ByVal item As Long"],
            );
            if prefix.is_empty() || !local.is_empty() || prefix == TARGET.repeat(2) {
                let callee_start = start + "Call ".len();
                let callee = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.invoked_identifier.reference"
                            && site.source.span().start_byte()
                                == u64::try_from(callee_start).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    callee.source.span().end_byte(),
                    u64::try_from(callee_start + "Target".len()).unwrap()
                );
                assert_eq!(callee.provenance, document.files[0].provenance);
                if !local.is_empty() {
                    let shadow = document
                        .entities
                        .iter()
                        .find(|entity| {
                            entity.kind == EntityKind::Variable && entity.canonical_name == "Target"
                        })
                        .unwrap();
                    assert_eq!(
                        shadow.container,
                        application.enclosing.map(ContainerRef::Entity)
                    );
                    assert_eq!(
                        callee.target,
                        OccurrenceTarget::Resolved { symbol: shadow.id }
                    );
                    assert_eq!(callee.role, OccurrenceRole::Reference);
                    let refers = document
                        .relations
                        .iter()
                        .filter(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(callee.id)
                                && edge.predicate == RelationPredicate::RefersTo
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(refers.len(), 1);
                    assert_eq!(refers[0].object, RelationEndpoint::Entity(shadow.id));
                    assert_eq!(refers[0].evidence.source.as_ref(), Some(&callee.source));
                    assert_eq!(refers[0].provenance, document.files[0].provenance);
                } else {
                    assert!(matches!(callee.target, OccurrenceTarget::Unresolved { .. }));
                    assert!(!document.relations.iter().any(|edge| {
                        edge.subject == RelationEndpoint::Occurrence(callee.id)
                            && edge.predicate == RelationPredicate::RefersTo
                    }));
                }
                assert!(!document.relations.iter().any(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(callee.id)
                        && edge.predicate == RelationPredicate::Calls
                }));
            }
        }
    }
}

#[test]
fn vba_explicit_expression_array_arguments_preserve_rank_barriers_without_callable_facts() {
    for (arguments, valid) in [("value + 1", true), ("value + 1, other", false)] {
        let nested = format!("items({arguments})");
        let statement = format!("Call Target({nested} + 1)");
        let source = format!(
            "Private items(2) As Long\nSub Target(ByVal first As Long)\nEnd Sub\nSub Run(ByVal value As Long, ByVal other As Long)\n{statement}\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let output =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            let document = output.document();
            assert_explicit_expression_application_proof(
                document,
                &source,
                &statement,
                "Target",
                "vba.invoked_identifier.reference",
                &["ByVal first As Long"],
            );
            let start = source.find(&nested).unwrap();
            let application = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                        && site.source.span().end_byte()
                            == u64::try_from(start + nested.len()).unwrap()
                })
                .unwrap();
            let array = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Variable && entity.canonical_name == "items"
                })
                .unwrap();
            if valid {
                assert_eq!(
                    application.target,
                    OccurrenceTarget::Resolved { symbol: array.id }
                );
            } else {
                assert!(matches!(
                    application.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert!(!document.relations.iter().any(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(application.id)
                        && edge.predicate == RelationPredicate::RefersTo
                }));
            }
            assert_eq!(application.provenance, document.files[0].provenance);
            assert!(!document.relations.iter().any(|edge| {
                edge.object == RelationEndpoint::Entity(array.id)
                    && edge.predicate == RelationPredicate::Calls
            }));
        }
    }
}

#[test]
fn vba_local_dim_lists_preserve_exact_lexical_identity_without_sibling_type_inference() {
    let source = "Attribute VB_Name = \"LocalLists\"\nPublic stored As Long\nSub Target(ByVal first As Long, ByVal second As Long)\nEnd Sub\nSub Run()\nDim stored, typed As Long, grid(-1 To +2,003) As Long, pending()\nDim [Rem] As String, _\n [other]\nstored = typed: typed = stored\ntyped = grid(0,1)\nCall Target(typed, grid(0,1))\nEnd Sub\n";
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            let source = if native {
                source.replace("typed = grid(0,1)", "typed = grid(0,1)\ntyped = #1/1/2000#")
            } else {
                source.to_owned()
            };
            let result =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            let document = result.document();
            let parser = document.files[0].provenance;
            let run = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function && entity.canonical_name == "Run"
                })
                .unwrap();
            let locals = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Variable
                        && entity.container == Some(ContainerRef::Entity(run.id))
                })
                .collect::<Vec<_>>();
            assert_eq!(locals.len(), 6);
            let start = source.find("Dim stored").unwrap() + "Dim ".len();
            for (clause, name) in [
                ("stored", "stored"),
                ("typed As Long", "typed"),
                ("grid(-1 To +2,003) As Long", "grid"),
                ("pending()", "pending"),
                ("[Rem] As String", "[Rem]"),
                ("[other]", "[other]"),
            ] {
                let offset = start + source[start..].find(clause).unwrap();
                let variable = locals
                    .iter()
                    .find(|entity| entity.canonical_name == name.trim_matches(['[', ']']))
                    .unwrap();
                assert_eq!(variable.visibility, rootlight_ir::EntityVisibility::Unknown);
                assert_eq!(variable.provenance, parser);
                let definition = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.source.span().start_byte() == u64::try_from(offset).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    definition.target,
                    OccurrenceTarget::Resolved {
                        symbol: variable.id
                    }
                );
                assert_eq!(definition.enclosing, Some(run.id));
                assert_eq!(definition.provenance, parser);
                let context = definition.evidence.source.as_ref().unwrap();
                assert_eq!(variable.evidence.source.as_ref(), Some(context));
                for (reference, written) in [(&definition.source, name), (context, clause)] {
                    assert_eq!(
                        reference.span().start_byte(),
                        u64::try_from(offset).unwrap()
                    );
                    assert_eq!(
                        reference.span().end_byte(),
                        u64::try_from(offset + written.len()).unwrap()
                    );
                    assert_eq!(reference.repository(), document.repository);
                    assert_eq!(reference.generation(), document.generation);
                    assert_eq!(reference.span().file(), document.files[0].id);
                    assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                    assert_eq!(
                        &source[usize::try_from(reference.span().start_byte()).unwrap()
                            ..usize::try_from(reference.span().end_byte()).unwrap()],
                        written
                    );
                }
                let contains = document
                    .relations
                    .iter()
                    .filter(|edge| {
                        edge.subject == RelationEndpoint::Entity(run.id)
                            && edge.object == RelationEndpoint::Entity(variable.id)
                            && edge.predicate == RelationPredicate::Contains
                            && edge.evidence.source.as_ref() == Some(context)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(contains.len(), 1);
                assert_eq!(contains[0].provenance, parser);
                assert!(
                    !document
                        .occurrences
                        .iter()
                        .any(|site| site.role == OccurrenceRole::TypeUse
                            && context.span().start_byte() <= site.source.span().start_byte()
                            && site.source.span().end_byte() <= context.span().end_byte())
                );
                assert!(
                    !document
                        .relations
                        .iter()
                        .any(|edge| edge.subject == RelationEndpoint::Entity(variable.id)
                            && edge.predicate == RelationPredicate::UsesType)
                );
            }
            let stored = locals
                .iter()
                .find(|entity| entity.canonical_name == "stored")
                .unwrap();
            let module_stored = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Variable
                        && entity.canonical_name == "stored"
                        && entity.id != stored.id
                })
                .unwrap();
            for statement in ["stored = typed", "typed = stored"] {
                let offset = source.find(statement).unwrap();
                let sites = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Reference
                            && u64::try_from(offset).unwrap() <= site.source.span().start_byte()
                            && site.source.span().end_byte()
                                <= u64::try_from(offset + statement.len()).unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(sites.len(), 2);
                for site in sites {
                    let name = &source[usize::try_from(site.source.span().start_byte()).unwrap()
                        ..usize::try_from(site.source.span().end_byte()).unwrap()];
                    let variable = locals
                        .iter()
                        .find(|entity| entity.canonical_name == name)
                        .unwrap();
                    assert_eq!(
                        site.target,
                        OccurrenceTarget::Resolved {
                            symbol: variable.id
                        }
                    );
                    assert_ne!(
                        site.target,
                        OccurrenceTarget::Resolved {
                            symbol: module_stored.id
                        }
                    );
                    assert_eq!(site.provenance, parser);
                    assert_eq!(site.source.repository(), document.repository);
                    assert_eq!(site.source.generation(), document.generation);
                    assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
                    let edges = document
                        .relations
                        .iter()
                        .filter(|edge| {
                            edge.subject == RelationEndpoint::Occurrence(site.id)
                                && edge.object == RelationEndpoint::Entity(variable.id)
                                && edge.predicate == RelationPredicate::RefersTo
                                && edge.evidence.source.as_ref() == Some(&site.source)
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(edges.len(), 1);
                    assert_eq!(edges[0].provenance, parser);
                }
            }
            assert_explicit_expression_application_proof(
                document,
                &source,
                "Call Target(typed, grid(0,1))",
                "Target",
                "vba.invoked_identifier.reference",
                &["ByVal first As Long", "ByVal second As Long"],
            );
            let dialect = document
                .skipped_regions
                .iter()
                .filter(|gap| gap.detail == "visual-basic-dialect-unproven")
                .collect::<Vec<_>>();
            assert_eq!(
                dialect.len(),
                usize::from(language == "visual-basic-classic")
            );
            for gap in dialect {
                assert_eq!(
                    gap.source.span(),
                    document.files[0].evidence.source.as_ref().unwrap().span()
                );
                assert_eq!(gap.provenance, parser);
            }
            if language == "vba" {
                assert_eq!(
                    result.report().coverage().status(),
                    CoverageStatus::Complete
                );
                assert!(document.skipped_regions.is_empty());
            }
        }
    }
}

#[test]
fn vba_local_dim_list_duplicate_definitions_remain_written_but_uses_are_ambiguous() {
    for declaration in [
        "Dim stored As Long, [STORED](2), other",
        "Dim stored As Long, stored(2), other",
    ] {
        let source = format!(
            "Public stored As Long\nSub Ready()\nEnd Sub\nSub Run()\n{declaration}\nstored = stored\nCall Ready()\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let result =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            let document = result.document();
            let run = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function && entity.canonical_name == "Run"
                })
                .unwrap();
            let start = source.find(declaration).unwrap() + "Dim ".len();
            let end = start + declaration.len() - "Dim ".len();
            let definitions = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.enclosing == Some(run.id)
                        && u64::try_from(start).unwrap() <= site.source.span().start_byte()
                        && site.source.span().end_byte() <= u64::try_from(end).unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 3);
            let mut contexts = std::collections::BTreeSet::new();
            for definition in definitions {
                let OccurrenceTarget::Resolved { symbol } = definition.target else {
                    panic!("each written duplicate remains a declaration witness");
                };
                let variable = document
                    .entities
                    .iter()
                    .find(|entity| entity.id == symbol)
                    .unwrap();
                assert_eq!(variable.kind, EntityKind::Variable);
                assert_eq!(variable.container, Some(ContainerRef::Entity(run.id)));
                let context = definition.evidence.source.as_ref().unwrap();
                assert_eq!(context.content_hash(), content_hash(source.as_bytes()));
                assert_eq!(context.repository(), document.repository);
                assert_eq!(context.generation(), document.generation);
                assert_eq!(context.span().file(), document.files[0].id);
                assert_eq!(
                    definition.source.span().start_byte(),
                    context.span().start_byte()
                );
                assert_eq!(definition.provenance, document.files[0].provenance);
                assert!(contexts.insert(
                    &source[usize::try_from(context.span().start_byte()).unwrap()
                        ..usize::try_from(context.span().end_byte()).unwrap()]
                ));
                assert_eq!(
                    document
                        .relations
                        .iter()
                        .filter(|edge| edge.subject == RelationEndpoint::Entity(run.id)
                            && edge.object == RelationEndpoint::Entity(symbol)
                            && edge.predicate == RelationPredicate::Contains
                            && edge.evidence.source.as_ref() == Some(context))
                        .count(),
                    1
                );
            }
            assert_eq!(
                contexts,
                declaration["Dim ".len()..]
                    .split(", ")
                    .collect::<std::collections::BTreeSet<_>>()
            );
            let read_start = source.find("stored = stored").unwrap();
            let uses = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Reference
                        && u64::try_from(read_start).unwrap() <= site.source.span().start_byte()
                        && site.source.span().end_byte()
                            <= u64::try_from(read_start + "stored = stored".len()).unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(uses.len(), 2);
            for site in uses {
                assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
                assert!(!document.relations.iter().any(|edge| edge.subject
                    == RelationEndpoint::Occurrence(site.id)
                    && matches!(
                        edge.predicate,
                        RelationPredicate::RefersTo | RelationPredicate::Calls
                    )));
                assert!(
                    document
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.source == site.source
                            && gap.detail == "vba-binding-target-unavailable")
                );
            }
            assert_explicit_expression_application_proof(
                document,
                &source,
                "Call Ready()",
                "Ready",
                "vba.invoked_identifier.reference",
                &[],
            );
        }
    }
}

#[test]
fn vba_local_dim_array_rank_shadows_functions_without_fabricated_call_edges() {
    for indices in ["0,1", "0"] {
        let source = format!(
            "Function grid(ByVal index As Long) As Long\nEnd Function\nSub Ready()\nEnd Sub\nSub Run()\nDim result, grid(-1 To +2,003) As Long, pending()\nresult = grid({indices})\nCall Ready()\nEnd Sub\n"
        );
        for language in ["vba", "visual-basic-classic"] {
            let result =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            let document = result.document();
            let run = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function && entity.canonical_name == "Run"
                })
                .unwrap();
            let array = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Variable
                        && entity.canonical_name == "grid"
                        && entity.container == Some(ContainerRef::Entity(run.id))
                })
                .unwrap();
            let function = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function && entity.canonical_name == "grid"
                })
                .unwrap();
            let expression = format!("grid({indices})");
            let start = source.find(&expression).unwrap();
            let application = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                        && site.source.span().end_byte()
                            == u64::try_from(start + expression.len()).unwrap()
                })
                .unwrap();
            if indices == "0,1" {
                assert_eq!(
                    application.target,
                    OccurrenceTarget::Resolved { symbol: array.id }
                );
                assert_eq!(
                    document
                        .relations
                        .iter()
                        .filter(
                            |edge| edge.subject == RelationEndpoint::Occurrence(application.id)
                                && edge.object == RelationEndpoint::Entity(array.id)
                                && edge.predicate == RelationPredicate::RefersTo
                                && edge.evidence.source.as_ref() == Some(&application.source)
                        )
                        .count(),
                    1
                );
            } else {
                assert!(matches!(
                    application.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert!(!document.relations.iter().any(|edge| edge.subject
                    == RelationEndpoint::Occurrence(application.id)
                    && edge.predicate == RelationPredicate::RefersTo));
                assert!(
                    document
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.source == application.source
                            && gap.detail == "vba-call-or-index-target-unavailable")
                );
            }
            assert_eq!(application.provenance, document.files[0].provenance);
            let callee = document
                .occurrences
                .iter()
                .find(|site| {
                    site.syntax_kind == "vba.invoked_get_identifier.reference"
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                })
                .unwrap();
            assert_eq!(
                callee.target,
                OccurrenceTarget::Resolved { symbol: array.id }
            );
            assert_eq!(callee.role, OccurrenceRole::Reference);
            assert!(!document.relations.iter().any(|edge| edge.predicate == RelationPredicate::Calls && matches!(edge.object, RelationEndpoint::Entity(id) if id == array.id || id == function.id)));
            assert_explicit_expression_application_proof(
                document,
                &source,
                "Call Ready()",
                "Ready",
                "vba.invoked_identifier.reference",
                &[],
            );
        }
    }
}

#[test]
fn vba_module_variables_preserve_value_ownership_without_constant_or_call_proofs() {
    use rootlight_ir::EntityVisibility;
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            for (declaration, declarator, name, visibility) in [
                (
                    "Public stored As Long",
                    "stored As Long",
                    "stored",
                    EntityVisibility::Public,
                ),
                (
                    "Private [stored]",
                    "[stored]",
                    "[stored]",
                    EntityVisibility::Private,
                ),
                ("Dim stored", "stored", "stored", EntityVisibility::Unknown),
                (
                    "Dim stored As Variant",
                    "stored As Variant",
                    "stored",
                    EntityVisibility::Unknown,
                ),
            ] {
                let body = if native {
                    "local = #1/1/2000#"
                } else {
                    "local = 3"
                };
                let source = format!(
                    "Attribute VB_Name = \"Authored\"\n{declaration}\nSub Update(Optional ByVal stored = [STORED])\nstored = 2\nEnd Sub\nSub Read()\nDim local As Long\nlocal = stored\nEnd Sub\nSub Shadow()\nDim stored As Long\nstored = stored\nEnd Sub\nSub Run()\nDim local As Long\n{body}\nCall Update()\nCall stored()\nEnd Sub\n"
                );
                let result =
                    application_proof_output(&source, language, IrLimits::default(), &deadline())
                        .unwrap();
                let document = result.document();
                let module = document
                    .entities
                    .iter()
                    .find(|entity| entity.kind == EntityKind::Module)
                    .unwrap();
                let variable = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.kind == EntityKind::Variable
                            && entity.canonical_name == "stored"
                            && entity.container == Some(ContainerRef::Entity(module.id))
                    })
                    .unwrap();
                let parameter = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.kind == EntityKind::Parameter && entity.canonical_name == "stored"
                    })
                    .unwrap();
                let shadow = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.kind == EntityKind::Variable
                            && entity.canonical_name == "stored"
                            && entity.id != variable.id
                    })
                    .unwrap();
                let parser = document.files[0].provenance;
                assert_eq!(variable.visibility, visibility);
                assert_eq!(variable.provenance, parser);
                let definition = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.target
                                == OccurrenceTarget::Resolved {
                                    symbol: variable.id,
                                }
                    })
                    .unwrap();
                let default = document
                    .occurrences
                    .iter()
                    .find(|site| site.syntax_kind == "vba.default_constant_identifier.reference")
                    .unwrap();
                for (reference, written) in [
                    (variable.evidence.source.as_ref().unwrap(), declarator),
                    (&definition.source, name),
                    (&default.source, "[STORED]"),
                ] {
                    let span = reference.span();
                    assert_eq!(
                        source
                            .get(
                                usize::try_from(span.start_byte()).unwrap()
                                    ..usize::try_from(span.end_byte()).unwrap()
                            )
                            .unwrap(),
                        written
                    );
                    assert_eq!(reference.repository(), document.repository);
                    assert_eq!(reference.generation(), document.generation);
                    assert_eq!(span.file(), document.files[0].id);
                    assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                }
                assert_eq!(definition.enclosing, Some(module.id));
                assert_eq!(definition.provenance, parser);
                assert_eq!(default.enclosing, Some(parameter.id));
                assert!(matches!(
                    default.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert_eq!(
                    document
                        .skipped_regions
                        .iter()
                        .filter(|gap| gap.source == default.source
                            && gap.detail == "vba-default-constant-target-unavailable")
                        .count(),
                    1
                );
                assert!(!document.relations.iter().any(|edge| edge.subject
                    == RelationEndpoint::Occurrence(default.id)
                    && edge.predicate == RelationPredicate::RefersTo));
                assert_eq!(
                    reference_at(document, &source, "stored = 2").target,
                    OccurrenceTarget::Resolved {
                        symbol: parameter.id
                    }
                );
                let read_start = source.find("local = stored").unwrap() + "local = ".len();
                let read = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.role == OccurrenceRole::Reference
                            && site.source.span().start_byte() == u64::try_from(read_start).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    read.target,
                    OccurrenceTarget::Resolved {
                        symbol: variable.id
                    }
                );
                let shadow_start = source.find("stored = stored").unwrap();
                let shadow_end = shadow_start + "stored = stored".len();
                let shadow_reads = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Reference
                            && site.source.span().start_byte()
                                >= u64::try_from(shadow_start).unwrap()
                            && site.source.span().end_byte() <= u64::try_from(shadow_end).unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(shadow_reads.len(), 2);
                assert!(shadow_reads.iter().all(|site| site.target == OccurrenceTarget::Resolved { symbol: shadow.id }));
                let callee_start =
                    u64::try_from(source.find("Call stored()").unwrap() + "Call ".len()).unwrap();
                let callee = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.invoked_identifier.reference"
                            && site.source.span().start_byte() == callee_start
                            && site.source.span().end_byte()
                                == callee_start + u64::try_from("stored".len()).unwrap()
                    })
                    .unwrap();
                assert_eq!(
                    callee.target,
                    OccurrenceTarget::Resolved {
                        symbol: variable.id
                    }
                );
                assert_eq!(callee.role, OccurrenceRole::Reference);
                let application = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.application.reference"
                            && site.source.span().start_byte() <= callee.source.span().start_byte()
                            && site.source.span().end_byte() >= callee.source.span().end_byte()
                    })
                    .unwrap();
                assert!(matches!(
                    application.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
                assert!(!document.relations.iter().any(|edge| matches!(edge.subject, RelationEndpoint::Occurrence(id) if id == callee.id || id == application.id) && edge.predicate == RelationPredicate::Calls));
                let update_start = u64::try_from(source.find("Call Update()").unwrap()).unwrap();
                let update = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.syntax_kind == "vba.application.reference"
                            && site.source.span().start_byte() == update_start
                    })
                    .unwrap();
                assert!(matches!(update.target, OccurrenceTarget::Resolved { .. }));
                let proof = document
                    .provenance
                    .iter()
                    .find(|proof| proof.id == update.provenance)
                    .unwrap();
                assert_eq!(
                    proof.rule.as_deref(),
                    Some("vba-procedure-application-v1.source")
                );
                assert!(
                    proof
                        .derivation_parents
                        .contains(&FactRef::Entity(parameter.id))
                );
                assert!(
                    !proof
                        .derivation_parents
                        .contains(&FactRef::Entity(variable.id))
                );
            }
        }
    }
}

#[test]
fn vba_parameter_defaults_bind_module_constants_not_body_parameters() {
    use rootlight_ir::EntityVisibility;
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            for (declaration, name, declarator, visibility, clause) in [
                (
                    "Private Const Fallback As Long = 3",
                    "Fallback",
                    "Fallback As Long = 3",
                    EntityVisibility::Private,
                    "Optional ByVal Fallback As Long = [FALLBACK]",
                ),
                (
                    "Public Const [Fallback] = 3",
                    "[Fallback]",
                    "[Fallback] = 3",
                    EntityVisibility::Public,
                    "Optional Fallback = [FALLBACK]",
                ),
                (
                    "Const Fallback = 3",
                    "Fallback",
                    "Fallback = 3",
                    EntityVisibility::Unknown,
                    "Optional ByRef Fallback = [FALLBACK]",
                ),
                (
                    "Const Fallback As Variant = True",
                    "Fallback",
                    "Fallback As Variant = True",
                    EntityVisibility::Unknown,
                    "Optional Fallback As Variant = [FALLBACK]",
                ),
            ] {
                let body = if native {
                    "local = #1/1/2000#"
                } else {
                    "local = 3"
                };
                let source = format!(
                    "Attribute VB_Name = \"Authored\"\n{declaration}\nPublic Sub Update({clause})\nFALLBACK = 2\nEnd Sub\nSub Run()\nDim local As Long\n{body}\nCall Update()\nEnd Sub\n"
                );
                let result =
                    application_proof_output(&source, language, IrLimits::default(), &deadline())
                        .unwrap();
                let document = result.document();
                let parser = document
                    .provenance
                    .iter()
                    .find(|proof| proof.id == document.files[0].provenance)
                    .unwrap();
                let constant = document
                    .entities
                    .iter()
                    .find(|entity| entity.kind == EntityKind::Constant)
                    .unwrap();
                let parameter = document
                    .entities
                    .iter()
                    .find(|entity| entity.kind == EntityKind::Parameter)
                    .unwrap();
                assert_eq!(constant.canonical_name, "Fallback");
                assert_eq!(constant.visibility, visibility);
                assert_eq!(constant.provenance, parser.id);
                assert_eq!(parameter.provenance, parser.id);
                let module = document
                    .entities
                    .iter()
                    .find(|entity| entity.kind == EntityKind::Module)
                    .unwrap();
                assert_eq!(constant.container, Some(ContainerRef::Entity(module.id)));
                let default = document
                    .occurrences
                    .iter()
                    .find(|site| site.syntax_kind == "vba.default_constant_identifier.reference")
                    .unwrap();
                assert_eq!(
                    default.target,
                    OccurrenceTarget::Resolved {
                        symbol: constant.id
                    }
                );
                assert_eq!(default.provenance, parser.id);
                assert_eq!(default.enclosing, Some(parameter.id));
                assert_eq!(
                    reference_at(document, &source, "FALLBACK = 2").target,
                    OccurrenceTarget::Resolved {
                        symbol: parameter.id
                    }
                );
                let definition = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.target
                                == OccurrenceTarget::Resolved {
                                    symbol: constant.id,
                                }
                    })
                    .unwrap();
                for (reference, written) in [
                    (constant.evidence.source.as_ref().unwrap(), declarator),
                    (&definition.source, name),
                    (parameter.evidence.source.as_ref().unwrap(), clause),
                    (&default.source, "[FALLBACK]"),
                ] {
                    let span = reference.span();
                    assert_eq!(
                        source
                            .get(
                                usize::try_from(span.start_byte()).unwrap()
                                    ..usize::try_from(span.end_byte()).unwrap()
                            )
                            .unwrap(),
                        written
                    );
                    assert_eq!(reference.generation(), document.files[0].generation);
                    assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                    assert_eq!(span.file(), document.files[0].id);
                    assert_eq!(reference.repository(), document.files[0].repository);
                }
                let relation = document
                    .relations
                    .iter()
                    .find(|edge| {
                        edge.subject == RelationEndpoint::Occurrence(default.id)
                            && edge.predicate == RelationPredicate::RefersTo
                            && edge.object == RelationEndpoint::Entity(constant.id)
                    })
                    .unwrap();
                assert_eq!(relation.provenance, parser.id);
                assert!(
                    !document
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.source == default.source
                            && gap.detail == "vba-default-constant-target-unavailable")
                );
                let application = document
                    .occurrences
                    .iter()
                    .find(|site| site.syntax_kind == "vba.application.reference")
                    .unwrap();
                assert!(matches!(
                    application.target,
                    OccurrenceTarget::Resolved { .. }
                ));
                let proof = document
                    .provenance
                    .iter()
                    .find(|proof| proof.id == application.provenance)
                    .unwrap();
                assert_eq!(
                    proof.rule.as_deref(),
                    Some("vba-procedure-application-v1.source")
                );
                assert!(proof.derivation_parents.contains(&FactRef::Fact(parser.id)));
                assert!(
                    proof
                        .derivation_parents
                        .contains(&FactRef::Entity(parameter.id))
                );
                assert!(
                    proof
                        .evidence_sources
                        .contains(parameter.evidence.source.as_ref().unwrap())
                );
                assert!(
                    !proof
                        .derivation_parents
                        .contains(&FactRef::Entity(constant.id)),
                    "written argument omission is not a proof of evaluating the default"
                );
            }
        }
    }
}

#[test]
fn vba_duplicate_module_constants_keep_written_definitions_and_default_ambiguity() {
    for language in ["vba", "visual-basic-classic"] {
        for native in [false, true] {
            let body = if native {
                "local = #1/1/2000#"
            } else {
                "local = 3"
            };
            let source = format!(
                "Private Const Fallback As Long = 7\nPrivate Const Fallback As Long = 8\nSub Notify(Optional value = Fallback)\nEnd Sub\nSub Run()\nDim local As Long\n{body}\nCall Notify()\nEnd Sub\n"
            );
            let result =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            let document = result.document();
            let constants: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Constant && entity.canonical_name == "Fallback"
                })
                .collect();
            assert_eq!(
                constants.len(),
                1,
                "equal semantic projections share identity, not a unique binding"
            );
            let constant = constants[0];
            let parser = document.files[0].provenance;
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target
                            == OccurrenceTarget::Resolved {
                                symbol: constant.id,
                            }
                })
                .collect();
            assert_eq!(definitions.len(), 2);
            for (ordinal, (start, _)) in source.match_indices("Fallback As Long").enumerate() {
                let definition = definitions
                    .iter()
                    .find(|site| site.source.span().start_byte() == u64::try_from(start).unwrap())
                    .unwrap();
                assert_eq!(
                    definition.source.span().end_byte(),
                    u64::try_from(start + "Fallback".len()).unwrap()
                );
                assert_eq!(
                    definition.source.content_hash(),
                    content_hash(source.as_bytes())
                );
                assert_eq!(definition.provenance, parser);
                let clause = format!("Fallback As Long = {}", 7 + ordinal);
                let containment: Vec<_> = document
                    .relations
                    .iter()
                    .filter(|edge| {
                        edge.object == RelationEndpoint::Entity(constant.id)
                            && edge.predicate == RelationPredicate::Contains
                            && edge.evidence.source.as_ref().is_some_and(|reference| {
                                reference.span().start_byte() == u64::try_from(start).unwrap()
                                    && reference.span().end_byte()
                                        == u64::try_from(start + clause.len()).unwrap()
                            })
                    })
                    .collect();
                assert_eq!(
                    containment.len(),
                    1,
                    "each written declaration retains source-bound containment"
                );
                assert_eq!(containment[0].provenance, parser);
                assert_eq!(
                    containment[0]
                        .evidence
                        .source
                        .as_ref()
                        .unwrap()
                        .content_hash(),
                    content_hash(source.as_bytes())
                );
            }
            let default = document
                .occurrences
                .iter()
                .find(|site| site.syntax_kind == "vba.default_constant_identifier.reference")
                .unwrap();
            assert!(matches!(
                default.target,
                OccurrenceTarget::Unresolved { .. }
            ));
            assert_eq!(
                document
                    .skipped_regions
                    .iter()
                    .filter(|gap| gap.source == default.source
                        && gap.detail == "vba-default-constant-target-unavailable")
                    .count(),
                1
            );
            assert!(!document.relations.iter().any(|edge| edge.subject
                == RelationEndpoint::Occurrence(default.id)
                && edge.predicate == RelationPredicate::RefersTo));
            let application = document
                .occurrences
                .iter()
                .find(|site| site.syntax_kind == "vba.application.reference")
                .unwrap();
            assert!(
                matches!(application.target, OccurrenceTarget::Resolved { .. }),
                "written Optional omission remains distinct from default evaluation"
            );
        }
    }
}

#[test]
fn vba_conditional_header_conditions_do_not_bind_runtime_variables() {
    let source = "Private Wide As Boolean\n#If Wide Then\nPublic Sub Update(ByVal value As Long)\n#Else\nPublic Sub Update(ByVal value As Integer)\n#End If\nvalue = 1\nEnd Sub\n";
    let result = output(source);
    let start = u64::try_from(source.find("Wide Then").unwrap()).unwrap();
    let condition = result
        .document()
        .occurrences
        .iter()
        .find(|site| site.source.span().start_byte() == start)
        .unwrap();
    assert_eq!(
        condition.syntax_kind,
        "vba.compilation_identifier.reference"
    );
    assert!(matches!(
        condition.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "vba-conditional-compilation-evaluation-unavailable" })
    );
}

#[test]
fn vba_duplicate_variable_visibility_conflict_is_unknown_with_both_written_witnesses() {
    use rootlight_ir::EntityVisibility;
    for language in ["vba", "visual-basic-classic"] {
        for body in ["scratch = 7", "scratch = #1/1/2000#"] {
            let source = format!(
                "Attribute VB_Name = \"Visibility\"\nPublic stored As Long\nPrivate [stored] As Long\nSub Run()\nDim scratch As Long\nscratch = stored\n{body}\nEnd Sub\n"
            );
            let result =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            let document = result.document();
            let module = document
                .entities
                .iter()
                .find(|entity| entity.kind == EntityKind::Module)
                .unwrap();
            let variables = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Variable
                        && entity.canonical_name == "stored"
                        && entity.container == Some(ContainerRef::Entity(module.id))
                })
                .collect::<Vec<_>>();
            assert_eq!(variables.len(), 1);
            let variable = variables[0];
            assert_eq!(
                variable.visibility,
                EntityVisibility::Unknown,
                "{language}: {body}"
            );
            let parser = document.files[0].provenance;
            assert_eq!(variable.provenance, parser);
            assert!(document.provenance.iter().any(|proof| proof.id == parser));
            let definitions = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target
                            == OccurrenceTarget::Resolved {
                                symbol: variable.id,
                            }
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 2);
            for (statement, clause, name) in [
                ("Public stored As Long", "stored As Long", "stored"),
                ("Private [stored] As Long", "[stored] As Long", "[stored]"),
            ] {
                let start = source.find(statement).unwrap() + statement.len() - clause.len();
                let definition = definitions
                    .iter()
                    .find(|site| site.source.span().start_byte() == u64::try_from(start).unwrap())
                    .unwrap();
                assert_eq!(
                    definition.source.span().end_byte(),
                    u64::try_from(start + name.len()).unwrap()
                );
                assert_eq!(definition.enclosing, Some(module.id));
                assert_eq!(definition.provenance, parser);
                let context = definition.evidence.source.as_ref().unwrap();
                assert_eq!(context.span().start_byte(), u64::try_from(start).unwrap());
                assert_eq!(
                    context.span().end_byte(),
                    u64::try_from(start + clause.len()).unwrap()
                );
                for reference in [&definition.source, context] {
                    assert_eq!(reference.repository(), document.repository);
                    assert_eq!(reference.generation(), document.generation);
                    assert_eq!(reference.span().file(), document.files[0].id);
                    assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
                }
                let contains = document
                    .relations
                    .iter()
                    .filter(|edge| {
                        edge.predicate == RelationPredicate::Contains
                            && edge.subject == RelationEndpoint::Entity(module.id)
                            && edge.object == RelationEndpoint::Entity(variable.id)
                            && edge.evidence.source.as_ref() == Some(context)
                    })
                    .collect::<Vec<_>>();
                assert_eq!(contains.len(), 1);
                assert_eq!(contains[0].provenance, parser);
            }
            let read = reference_at(document, &source, "stored\n");
            assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source == read.source && gap.domain == FactDomain::Relations)
            );
            assert!(!document.relations.iter().any(|edge| {
                edge.subject == RelationEndpoint::Occurrence(read.id)
                    && matches!(
                        edge.predicate,
                        RelationPredicate::RefersTo | RelationPredicate::Calls
                    )
            }));
            assert!(!document.relations.iter().any(|edge| {
                edge.object == RelationEndpoint::Entity(variable.id)
                    && edge.predicate == RelationPredicate::Calls
            }));
        }
    }
}

fn duplicate_variable_visibility_source(declarations: &[(&str, &str)], body: &str) -> String {
    let mut source = String::from("Attribute VB_Name = \"Visibility\"\n");
    for (prefix, name) in declarations {
        source.push_str(&format!("{prefix} {name} As Long\n"));
    }
    source.push_str(&format!(
        "Sub Run()\nDim scratch As Long\nscratch = stored\n{body}\nEnd Sub\n"
    ));
    source
}

fn assert_duplicate_variable_visibility_witnesses(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
    declarations: &[(&str, &str)],
    expected: &[(&str, rootlight_ir::EntityVisibility)],
) {
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module)
        .unwrap();
    let variables = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Variable
                && entity.container == Some(ContainerRef::Entity(module.id))
        })
        .collect::<Vec<_>>();
    assert_eq!(variables.len(), expected.len(), "{source}");
    let parser = document.files[0].provenance;
    assert!(document.provenance.iter().any(|proof| proof.id == parser));
    for (name, visibility) in expected {
        let variable = variables
            .iter()
            .find(|entity| entity.canonical_name == *name)
            .unwrap();
        assert_eq!(variable.visibility, *visibility, "{source}");
        assert_eq!(variable.provenance, parser);
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target
                            == OccurrenceTarget::Resolved {
                                symbol: variable.id,
                            }
                })
                .count(),
            declarations
                .iter()
                .filter(|(_, written_name)| {
                    written_name.trim_start_matches('[').trim_end_matches(']') == *name
                })
                .count()
        );
        assert!(!document.relations.iter().any(|edge| {
            edge.object == RelationEndpoint::Entity(variable.id)
                && edge.predicate == RelationPredicate::Calls
        }));
    }
    let mut cursor = source.find('\n').unwrap() + 1;
    for (prefix, written_name) in declarations {
        let start = cursor + prefix.len() + 1;
        let end = start + written_name.len() + " As Long".len();
        assert_eq!(
            &source[cursor..end],
            format!("{prefix} {written_name} As Long")
        );
        let canonical_name = written_name.trim_start_matches('[').trim_end_matches(']');
        let variable = variables
            .iter()
            .find(|entity| entity.canonical_name == canonical_name)
            .unwrap();
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: variable.id,
                        }
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{source}");
        let definition = definitions[0];
        assert_eq!(
            definition.source.span().end_byte(),
            u64::try_from(start + written_name.len()).unwrap()
        );
        assert_eq!(definition.enclosing, Some(module.id));
        assert_eq!(definition.provenance, parser);
        let context = definition.evidence.source.as_ref().unwrap();
        assert_eq!(context.span().start_byte(), u64::try_from(start).unwrap());
        assert_eq!(context.span().end_byte(), u64::try_from(end).unwrap());
        for reference in [&definition.source, context] {
            assert_eq!(reference.repository(), document.repository);
            assert_eq!(reference.generation(), document.generation);
            assert_eq!(reference.span().file(), document.files[0].id);
            assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        }
        let contains = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(module.id)
                    && edge.object == RelationEndpoint::Entity(variable.id)
                    && edge.evidence.source.as_ref() == Some(context)
            })
            .collect::<Vec<_>>();
        assert_eq!(contains.len(), 1, "{source}");
        assert_eq!(contains[0].provenance, parser);
        cursor = end + 1;
    }
}

fn assert_duplicate_variable_read_is_ambiguous(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
) {
    let read = reference_at(document, source, "stored\n");
    assert!(
        matches!(read.target, OccurrenceTarget::Unresolved { .. }),
        "{source}"
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.source == read.source && gap.domain == FactDomain::Relations })
    );
    assert!(!document.relations.iter().any(|edge| {
        edge.subject == RelationEndpoint::Occurrence(read.id)
            && matches!(
                edge.predicate,
                RelationPredicate::RefersTo | RelationPredicate::Calls
            )
    }));
}

#[test]
fn vba_duplicate_variable_visibility_agreement_preserves_every_source_order() {
    use rootlight_ir::EntityVisibility;
    let cases: &[(&[&str], EntityVisibility)] = &[
        (&["Public", "Public"], EntityVisibility::Public),
        (&["Private", "Private"], EntityVisibility::Private),
        (&["Dim", "Dim"], EntityVisibility::Unknown),
        (&["Public", "Private"], EntityVisibility::Unknown),
        (&["Private", "Public"], EntityVisibility::Unknown),
        (&["Public", "Dim"], EntityVisibility::Unknown),
        (&["Dim", "Public"], EntityVisibility::Unknown),
        (&["Private", "Dim"], EntityVisibility::Unknown),
        (&["Dim", "Private"], EntityVisibility::Unknown),
        (&["Public", "Private", "Public"], EntityVisibility::Unknown),
    ];
    for language in ["vba", "visual-basic-classic"] {
        for body in ["scratch = 7", "scratch = #1/1/2000#"] {
            for (prefixes, visibility) in cases {
                let declarations = prefixes
                    .iter()
                    .enumerate()
                    .map(|(index, prefix)| {
                        (*prefix, if index == 1 { "[stored]" } else { "stored" })
                    })
                    .collect::<Vec<_>>();
                let source = duplicate_variable_visibility_source(&declarations, body);
                let result =
                    application_proof_output(&source, language, IrLimits::default(), &deadline())
                        .unwrap();
                assert_duplicate_variable_visibility_witnesses(
                    result.document(),
                    &source,
                    &declarations,
                    &[("stored", *visibility)],
                );
                assert_duplicate_variable_read_is_ambiguous(result.document(), &source);
            }
        }
    }
}

#[test]
fn vba_case_preserved_variable_identities_keep_independent_visibility_votes() {
    use rootlight_ir::EntityVisibility;
    let declarations = [("Public", "stored"), ("Private", "[STORED]")];
    for language in ["vba", "visual-basic-classic"] {
        for body in ["scratch = 7", "scratch = #1/1/2000#"] {
            let source = duplicate_variable_visibility_source(&declarations, body);
            let result =
                application_proof_output(&source, language, IrLimits::default(), &deadline())
                    .unwrap();
            assert_duplicate_variable_visibility_witnesses(
                result.document(),
                &source,
                &declarations,
                &[
                    ("stored", EntityVisibility::Public),
                    ("STORED", EntityVisibility::Private),
                ],
            );
            let stored = result
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Variable && entity.canonical_name == "stored"
                })
                .unwrap();
            let upper = result
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Variable && entity.canonical_name == "STORED"
                })
                .unwrap();
            assert_ne!(stored.id, upper.id);
            assert_duplicate_variable_read_is_ambiguous(result.document(), &source);
        }
    }
}

#[test]
fn vba_bounded_required_fact_stream_retains_unknown_visibility_and_all_definitions() {
    use rootlight_ir::EntityVisibility;
    let cases = [
        [("Public", "stored"), ("Public", "[stored]")],
        [("Private", "stored"), ("Private", "[stored]")],
        [("Public", "stored"), ("Private", "[stored]")],
    ];
    for language in ["vba", "visual-basic-classic"] {
        for body in ["scratch = 7", "scratch = #1/1/2000#"] {
            for declarations in cases {
                let source = duplicate_variable_visibility_source(&declarations, body);
                let provider = Arc::new(provider());
                let case = LanguageCase {
                    name: language,
                    ..VBA
                };
                let fixture = Fixture::new(case, source.as_bytes());
                let defaults = limits();
                let initial = request(&fixture.snapshot, &fixture.source, case, &defaults);
                let required = provider
                    .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
                    .unwrap();
                let bounded = limits_with_syntax_records(required);
                let request = request(&fixture.snapshot, &fixture.source, case, &bounded);
                let parsed = rootlight_adapter_sdk::execute_parse(
                    provider.as_ref(),
                    &request.to_parse_request(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
                assert_eq!(parsed.facts().len(), required);
                assert_eq!(parsed.report().coverage().status(), CoverageStatus::Bounded);
                assert!(parsed.facts().iter().all(|fact| !matches!(
                    fact.syntax_kind().as_str(),
                    "vba.public.expression"
                        | "vba.private.expression"
                        | "vba.project_restricted.expression"
                        | "vba.visibility_unknown.expression"
                )));
                let result = execute_analysis(
                    &analyzer(&provider, case),
                    &request,
                    ExtensionSupport::default(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &deadline(),
                )
                .unwrap();
                assert!(
                    result
                        .document()
                        .diagnostics
                        .iter()
                        .any(|diagnostic| { diagnostic.code == "syntax-extraction-limit" })
                );
                // Bounded parsing bypasses binding; this is not COMPLETE missing-vote evidence.
                assert_duplicate_variable_visibility_witnesses(
                    result.document(),
                    &source,
                    &declarations,
                    &[("stored", EntityVisibility::Unknown)],
                );
            }
        }
    }
}
