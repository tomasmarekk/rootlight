//! Shell parameter reads require native expansion context and binding evidence.
//! Assignment dependencies use the original name leaf and canonical proof;
//! dynamic function scopes remain outside the module-only binding contract.

use super::*;
use rootlight_ir::{FactRef, derive_relation_record_id};

const ORIGINAL: &str = include_str!("../fixtures/bash_bindings/find_git_conflicts.sh");

fn run(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(BASH_CASE, source.as_bytes());
    let output = analyze(
        &analyzer(&provider, BASH_CASE),
        &request(&fixture.snapshot, &fixture.source, BASH_CASE, &limits()),
        &ExtensionSupport::default(),
    );
    validate_ir_document(
        output.document(),
        limits().ir(),
        &ExtensionSupport::default(),
    )
    .unwrap();
    output
}

fn check_dependency(output: &AnalysisOutput, source: &str, start: usize) {
    let document = output.document();
    let variable = |name: &str| {
        let found: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == name)
            .collect();
        assert_eq!(found.len(), 1, "unique variable {name}");
        found[0]
    };
    let owner = variable("prs");
    let target = variable("REPO");
    let start = u64::try_from(start).unwrap();
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == start + 4
                && site.enclosing == Some(owner.id)
                && site.target == (OccurrenceTarget::Resolved { symbol: target.id })
        })
        .collect();
    let relations: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Entity(owner.id)
                && edge.object == RelationEndpoint::Entity(target.id)
                && edge.evidence.source.as_ref().is_some_and(|witness| {
                    witness.span().start_byte() == start && witness.span().end_byte() == start + 4
                })
        })
        .collect();
    assert_eq!(
        (reads.len(), relations.len()),
        (1, 1),
        "missing exact module Read and assignment-owner RefersTo"
    );
    let read = reads[0];
    let relation = relations[0];
    assert_eq!(read.source, *relation.evidence.source.as_ref().unwrap());
    assert_eq!(read.source.content_hash(), content_hash(source.as_bytes()));
    let mut proof = vec![
        FactRef::Entity(owner.id),
        FactRef::Fact(read.id),
        FactRef::Entity(target.id),
    ];
    proof.sort_unstable();
    assert_eq!(relation.evidence.derivation, proof);
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
}

#[test]
fn original_module_assignment_reads_inherited_binding_and_keeps_dependency() {
    assert_eq!(ORIGINAL.len(), 613);
    assert_eq!(&ORIGINAL[68..72], "REPO");
    assert_eq!(&ORIGINAL[222..225], "prs");
    assert_eq!(&ORIGINAL[248..252], "REPO");
    let output = run(ORIGINAL);
    for (name, start, end) in [("REPO", 68, 72), ("prs", 222, 225)] {
        let entity = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == EntityKind::Variable)
            .unwrap();
        assert_eq!(
            output
                .document()
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                        && site.source.span().start_byte() == start
                        && site.source.span().end_byte() == end
                })
                .count(),
            1
        );
    }
    check_dependency(&output, ORIGINAL, 248);
}

#[test]
fn dynamic_scope_and_mutations_never_become_exact_module_bindings() {
    for source in [
        "REPO=outer\ncallee(){ prs=$REPO; }\ncaller(){ local REPO=inner; callee; }\ncaller\n",
        "REPO=outer\ncallee(){ prs=$REPO; }\ncallee\n",
        "REPO=outer\nif flag; then REPO=inner; fi\nprs=$REPO\n",
        "REPO=outer\nREPO=temporary command\nprs=$REPO\n",
        "REPO=outer\nunset REPO\nprs=$REPO\n",
        "REPO=outer\ndeclare -n REPO=other\nprs=$REPO\n",
        "REPO=outer\neval 'REPO=other'\nprs=$REPO\n",
        "REPO=outer\n. other.sh\nprs=$REPO\n",
        "REPO=outer\nsource other.sh\nprs=$REPO\n",
        "REPO=(one two)\nprs=$REPO\n",
        "REPO=outer\nREPO[0]=inner\nprs=$REPO\n",
        "REPO=1\n((REPO++))\nprs=$REPO\n",
        "REPO=outer\nchange(){ unset REPO; }\nchange\nprs=$REPO\n",
        "REPO=outer\n$unknown\nprs=$REPO\n",
        "prs=$REPO\nREPO=outer\n",
        "REPO=outer\nprs=$REPO\nREPO=later\n",
        "REPO=outer\n(prs=$REPO)\n",
    ] {
        let output = run(source);
        let start = u64::try_from(source.rfind("$REPO").unwrap() + 1).unwrap();
        let reads: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Read && site.source.span().start_byte() == start
            })
            .collect();
        assert_eq!(
            reads.len(),
            1,
            "read role survives unavailable binding: {source}"
        );
        assert!(
            !matches!(reads[0].target, OccurrenceTarget::Resolved { .. }),
            "{source}"
        );
        assert!(
            output.document().skipped_regions.iter().any(|gap| {
                gap.detail == "bash-module-parameter-binding-unavailable"
                    && gap.source.span() == reads[0].source.span()
            }),
            "missing explicit gap: {source}"
        );
        assert!(
            !output.document().relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && matches!(edge.subject, RelationEndpoint::Entity(_))
                    && edge
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|reference| reference.span().start_byte() == start)
            }),
            "invented assignment dependency: {source}"
        );
    }
}

#[test]
fn documentary_and_complex_parameter_text_never_gets_simple_read_proof() {
    for source in [
        "REPO=outer\nprs='$REPO'\n",
        "REPO=outer\nprs=\"\\$REPO\"\n",
        "REPO=outer\n# prs=$REPO\n",
        "REPO=outer\ncat <<'END'\n$REPO\nEND\n",
        "REPO=outer\nprs=${!REPO}\n",
        "REPO=outer\nprs=${REPO:=default}\n",
        "REPO=outer\nprs=${REPO[0]}\n",
    ] {
        let output = run(source);
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read),
            "{source}"
        );
        assert!(
            !output.document().relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && matches!(edge.subject, RelationEndpoint::Entity(_))
            }),
            "{source}"
        );
    }
}

#[test]
fn brace_reads_and_unrelated_later_bindings_preserve_stable_owner_identity() {
    let source = "REPO=outer\nprs=$(printf '%s' \"$REPO\")\nOTHER=later\n";
    let first = run(source);
    check_dependency(&first, source, source.find("$REPO").unwrap() + 1);
    let changed = source.replace("OTHER=later", "OTHER=different\n# body evidence");
    let second = run(&changed);
    check_dependency(&second, &changed, changed.find("$REPO").unwrap() + 1);
    for name in ["REPO", "prs"] {
        let id = |result: &AnalysisOutput| {
            result
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap()
                .id
        };
        assert_eq!(id(&first), id(&second), "{name}");
    }
    let braced = "REPO=outer\nprs=${REPO}\n";
    let output = run(braced);
    check_dependency(&output, braced, braced.find("${REPO}").unwrap() + 2);
}

#[test]
fn encoded_module_reads_keep_exact_physical_source_and_proof() {
    let source = "# marker λ😀\r\nREPO=outer\r\nprs=$(printf '%s' \"$REPO\")\r\n";
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let provider = Arc::new(provider());
        let fixture = Fixture::new(BASH_CASE, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            LanguageId::new("bash").unwrap(),
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(BASH_CASE.generated);
        let result = analyze(
            &analyzer(&provider, BASH_CASE),
            &request,
            &ExtensionSupport::default(),
        );
        validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
        let start = 2 + 2 * source[..source.rfind("$REPO").unwrap() + 1]
            .encode_utf16()
            .count();
        let read = result
            .document()
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
            })
            .unwrap();
        assert!(matches!(read.target, OccurrenceTarget::Resolved { .. }));
        assert_eq!(
            read.source.span().end_byte(),
            u64::try_from(start + 8).unwrap()
        );
        assert_eq!(read.source.content_hash(), content_hash(&raw));
        let expected: Vec<_> = "REPO"
            .encode_utf16()
            .flat_map(|unit| {
                if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect();
        assert_eq!(&raw[start..start + 8], expected);
        let relation = result
            .document()
            .relations
            .iter()
            .find(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && matches!(edge.subject, RelationEndpoint::Entity(_))
            })
            .unwrap();
        assert_eq!(relation.evidence.source.as_ref(), Some(&read.source));
        assert_eq!(relation.evidence.derivation.len(), 3);
        assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
    }
}

#[test]
fn asynchronous_assignments_never_certify_module_environment_bindings() {
    for (source, expected_reads) in [
        ("REPO=outer &\nprs=$REPO\n", 1),
        ("REPO=outer &\nwait\nprs=$REPO\n", 1),
        ("REPO=outer\nprs=$REPO &\nlater=$prs\n", 2),
    ] {
        let output = run(source);
        let reads: Vec<_> = output
            .document()
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Read)
            .collect();
        assert_eq!(reads.len(), expected_reads, "{source}");
        for read in reads {
            let start = usize::try_from(read.source.span().start_byte()).unwrap();
            let end = usize::try_from(read.source.span().end_byte()).unwrap();
            assert!(matches!(&source[start..end], "REPO" | "prs"));
            assert_eq!(read.source.content_hash(), content_hash(source.as_bytes()));
            assert!(
                !matches!(read.target, OccurrenceTarget::Resolved { .. }),
                "async assignment cannot certify a parent-shell binding: {source}"
            );
            assert_eq!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.domain == FactDomain::Relations
                            && gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && gap.detail == "bash-module-parameter-binding-unavailable"
                            && gap.source == read.source
                    })
                    .count(),
                1,
                "missing exact async binding gap: {source}"
            );
        }
        assert!(
            !output.document().relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && matches!(edge.subject, RelationEndpoint::Entity(_))
            }),
            "invented module assignment dependency: {source}"
        );
    }
}

#[test]
fn literal_ampersands_and_synchronous_reads_keep_module_binding_proof() {
    for source in [
        "REPO=outer\n# documentary &\nprs=$REPO\n",
        "REPO=outer\nOTHER='&'\nprs=$REPO\n",
        "REPO=outer\nOTHER=\"&\"\nprs=$REPO\n",
        "REPO=outer\nOTHER=\\&\nprs=$REPO\n",
        "REPO=outer; prs=$REPO\n",
        "REPO=outer\nprs=$(printf '%s' \"$REPO\")\n",
    ] {
        let output = run(source);
        check_dependency(&output, source, source.rfind("$REPO").unwrap() + 1);
    }
}

fn written_shell_owner(output: &AnalysisOutput, source: &str, owner: &str, child: &str) {
    let document = output.document();
    let definition = |name: &str, kind, start: usize| {
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + name.len()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "one written {name}");
        let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
            panic!("written definition is resolved")
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(entity.canonical_name, name);
        assert_eq!(entity.kind, kind);
        assert_eq!(
            sites[0].source.content_hash(),
            content_hash(source.as_bytes())
        );
        symbol
    };
    let owner_start = source.rfind(&format!("{owner}()")).unwrap();
    let child_start = source.rfind(&format!("local {child}=")).unwrap() + "local ".len();
    let owner = definition(owner, EntityKind::Function, owner_start);
    let child_end = child_start + child.len();
    let child = definition(child, EntityKind::Variable, child_start);
    let child_record = document
        .entities
        .iter()
        .find(|entity| entity.id == child)
        .unwrap();
    assert_eq!(
        child_record.container,
        Some(rootlight_ir::ContainerRef::Entity(owner))
    );
    let edges = super::broad_residuals::source_anchored_contains(
        document,
        owner,
        child,
        u64::try_from(child_start).unwrap(),
        u64::try_from(child_end).unwrap(),
    );
    assert_eq!(edges.len(), 1);
    assert_eq!(
        edges[0].evidence.source.as_ref().unwrap().content_hash(),
        content_hash(source.as_bytes())
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(child)
                && edge.object == RelationEndpoint::Entity(owner))
    );
}

#[test]
fn source_after_exact_lf_or_crlf_heredoc_delimiter_keeps_written_function_ownership() {
    let source = "cat <<END\ntext\nEND\nowned() { local item=seed; }\n";
    for source in [source.to_owned(), source.replace('\n', "\r\n")] {
        let output = run(&source);
        written_shell_owner(&output, &source, "owned", "item");
    }
}

#[test]
fn declaration_looking_heredoc_text_never_donates_a_function_or_local() {
    for suffix in [
        "END\nowned() { local item=seed; }\n",
        "END trailing\nowned() { local item=seed; }\n",
    ] {
        let source = format!("cat <<END\nfake() {{ local hidden=seed; }}\n{suffix}");
        for source in [source.clone(), source.replace('\n', "\r\n")] {
            let output = run(&source);
            assert!(
                !output
                    .document()
                    .entities
                    .iter()
                    .any(|entity| matches!(entity.canonical_name.as_str(), "fake" | "hidden"))
            );
            if suffix.starts_with("END\n") {
                written_shell_owner(&output, &source, "owned", "item");
            } else {
                assert!(
                    !output
                        .document()
                        .entities
                        .iter()
                        .any(|entity| matches!(entity.canonical_name.as_str(), "owned" | "item"))
                );
            }
        }
    }
}

#[test]
fn written_local_assignment_ownership_does_not_certify_its_dynamic_value() {
    let source = "owned() { local item=seed; if condition; then item=later; fi; echo $item; }\n";
    let output = run(source);
    written_shell_owner(&output, source, "owned", "item");
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Relations
                && gap.detail == "bash-module-parameter-binding-unavailable")
    );
}
