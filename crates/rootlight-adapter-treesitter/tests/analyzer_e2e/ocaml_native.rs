//! Public OCaml analysis retains written implementation and interface identities.
//! Source evidence is generation-bound; calls and abstract types do not imply binding.

use super::*;

/// Written implementation with a container, callable and physical application.
pub(super) const OCAML: LanguageCase = LanguageCase {
    name: "ocaml",
    path: "src/written.ml",
    frontend: "tree-sitter-ocaml-0.25.0",
    source: "module Worker = struct\nlet increment x = x + 1\nlet result = increment 2\nend\n",
    generated: false,
    body_before: "x + 1",
    body_after: "x + 100",
};

/// Written interface declarations with an intentionally abstract type.
pub(super) const OCAML_INTERFACE: LanguageCase = LanguageCase {
    name: "ocaml-interface",
    path: "src/written.mli",
    frontend: "tree-sitter-ocaml-0.25.0",
    source: "type item\nmodule type Reader = sig\nval read : item -> int\nend\nmodule Worker : sig\nval run : item -> int\nend\nval increment : int -> int\nval count : int\n",
    generated: false,
    body_before: "int -> int",
    body_after: "int -> int -> int",
};

fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

fn output(case: LanguageCase, source: &str) -> AnalysisOutput {
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
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.language, case.name);
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    for reference in document
        .entities
        .iter()
        .filter_map(|entity| entity.evidence.source.as_ref())
        .chain(
            document
                .occurrences
                .iter()
                .map(|occurrence| &occurrence.source),
        )
        .chain(
            document
                .occurrences
                .iter()
                .filter_map(|occurrence| occurrence.evidence.source.as_ref()),
        )
        .chain(document.skipped_regions.iter().map(|gap| &gap.source))
        .chain(
            document
                .extensions
                .iter()
                .filter_map(|extension| extension.evidence.source.as_ref()),
        )
    {
        assert_eq!(reference.repository(), document.repository);
        assert_eq!(reference.generation(), document.generation);
        assert_eq!(reference.span().file(), file.id);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        let _ = text(source, reference);
    }
    for occurrence in &document.occurrences {
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(text(source, &occurrence.source).as_bytes())
        );
        let evidence = occurrence.evidence.source.as_ref().unwrap();
        assert!(evidence.span().start_byte() <= occurrence.source.span().start_byte());
        assert!(occurrence.source.span().end_byte() <= evidence.span().end_byte());
        if occurrence.role != OccurrenceRole::Definition {
            assert_eq!(evidence, &occurrence.source);
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
    }
    for coverage in &document.coverage_records {
        assert_eq!(
            coverage.discovered,
            coverage.indexed.checked_add(coverage.skipped).unwrap()
        );
    }
    result
}

fn entity<'a>(
    result: &'a AnalysisOutput,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::EntityRecord {
    let mut matches = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name && entity.kind == kind);
    let entity = matches
        .next()
        .unwrap_or_else(|| panic!("missing {kind:?} {name}"));
    assert!(matches.next().is_none(), "duplicate {kind:?} {name}");
    entity
}

fn signature(result: &AnalysisOutput, source: &str, symbol: SymbolId) -> Vec<String> {
    result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .filter_map(|extension| {
            let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                && lexical.subject() == rootlight_ir::FactRef::Entity(symbol))
            .then(|| {
                assert!(!lexical.is_truncated());
                lexical
                    .verify_source_bytes(
                        text(source, extension.evidence.source.as_ref().unwrap()).as_bytes(),
                    )
                    .unwrap();
                lexical.text().to_owned()
            })
        })
        .collect()
}

#[test]
fn ocaml_written_implementation_names_headers_and_containment_are_exact() {
    let result = output(OCAML, OCAML.source);
    let worker = entity(&result, "Worker", EntityKind::Module);
    let function = entity(&result, "increment", EntityKind::Function);
    let parameter = entity(&result, "x", EntityKind::Parameter);
    let variable = entity(&result, "result", EntityKind::Variable);
    for (owner, expected_name, expected_header) in [
        (worker, "Worker", "Worker ="),
        (function, "increment", "increment x ="),
        (parameter, "x", "x"),
        (variable, "result", "result ="),
    ] {
        let definitions = result.document().occurrences.iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Definition
                && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == owner.id))
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{expected_name}");
        assert_eq!(text(OCAML.source, &definitions[0].source), expected_name);
        assert!(
            signature(&result, OCAML.source, owner.id)
                .iter()
                .any(|header| header == expected_header)
        );
    }
    for (parent, child) in [
        (worker, function),
        (function, parameter),
        (worker, variable),
    ] {
        assert!(
            result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(parent.id)
                    && relation.object == RelationEndpoint::Entity(child.id))
        );
    }
}

#[test]
fn ocaml_applications_and_qualified_reads_remain_written_unresolved_uses() {
    let source =
        "let increment x = x + 1\nlet result = increment 2\nlet foreign item = Reader.read item\n";
    let result = output(OCAML, source);
    let mut calls = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .map(|occurrence| text(source, &occurrence.source))
        .collect::<Vec<_>>();
    calls.sort_unstable();
    assert_eq!(calls, ["Reader.read", "increment"]);
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                && text(source, &occurrence.source) == "item")
    );
    assert!(
        !result
            .document()
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Calls)
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::UnsupportedConstruct)
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn ocaml_interfaces_keep_signatures_and_abstract_type_source_without_implementations() {
    let result = output(OCAML_INTERFACE, OCAML_INTERFACE.source);
    for name in ["read", "run", "increment"] {
        let owner = entity(&result, name, EntityKind::Function);
        assert!(!signature(&result, OCAML_INTERFACE.source, owner.id).is_empty());
    }
    let _ = entity(&result, "count", EntityKind::Variable);
    let _ = entity(&result, "Worker", EntityKind::Module);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "item")
    );
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|occurrence| matches!(
                occurrence.role,
                OccurrenceRole::CallSite | OccurrenceRole::Reference
            ))
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && text(OCAML_INTERFACE.source, &gap.source) == "item"
            )
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn ocaml_strings_comments_and_extension_payloads_cannot_fabricate_code_facts() {
    let source = "let visible x = x\nlet text = {aéz|let string_name x = x|aéz}\n(* let comment_name x = x *)\n[%%opaque let payload_name x = x]\n";
    let result = output(OCAML, source);
    let _ = entity(&result, "visible", EntityKind::Function);
    let _ = entity(&result, "text", EntityKind::Variable);
    for hidden in ["string_name", "comment_name", "payload_name"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == hidden)
        );
        assert!(!result.document().occurrences.iter().any(|occurrence| {
            !matches!(
                occurrence.role,
                OccurrenceRole::Documentation | OccurrenceRole::StringEvidence
            ) && text(source, &occurrence.source).contains(hidden)
        }));
    }
    for (role, authored) in [
        (
            OccurrenceRole::StringEvidence,
            "{aéz|let string_name x = x|aéz}",
        ),
        (
            OccurrenceRole::Documentation,
            "(* let comment_name x = x *)",
        ),
    ] {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == role
                    && text(source, &occurrence.source) == authored)
        );
    }
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(
                |gap| gap.reason == SkippedRegionReason::UnsupportedConstruct
                    && text(source, &gap.source) == "[%%opaque let payload_name x = x]"
            )
    );
}

#[test]
fn ocaml_body_only_changes_preserve_written_identity_and_lexical_headers() {
    let before = "let increment x = x + 1\n";
    let after = "let increment x = x + 100\n";
    let first = output(OCAML, before);
    let second = output(OCAML, after);
    for (name, kind) in [
        ("increment", EntityKind::Function),
        ("x", EntityKind::Parameter),
    ] {
        let first_owner = entity(&first, name, kind);
        let second_owner = entity(&second, name, kind);
        assert_eq!(first_owner.id, second_owner.id);
        assert_eq!(
            signature(&first, before, first_owner.id),
            signature(&second, after, second_owner.id)
        );
    }
}

#[test]
fn ocaml_concrete_types_keep_written_identity_and_narrow_abstract_gaps() {
    let source = "type point = { x: int; y: int }\ntype direction = North | South of int\ntype count = int\ntype abstract\ntype replacement := int\ntype extensible = ..\n";
    for case in [OCAML, OCAML_INTERFACE] {
        let result = output(case, source);
        for (name, kind) in [
            ("point", EntityKind::Struct),
            ("direction", EntityKind::Enum),
            ("count", EntityKind::TypeAlias),
        ] {
            let owner = entity(&result, name, kind);
            let definitions = result.document().occurrences.iter().filter(|occurrence|
                occurrence.role == OccurrenceRole::Definition
                && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == owner.id)
            ).collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1);
            assert_eq!(text(source, &definitions[0].source), name);
            assert!(!signature(&result, source, owner.id).is_empty());
        }
        for name in ["abstract", "replacement", "extensible"] {
            assert!(
                !result
                    .document()
                    .entities
                    .iter()
                    .any(|entity| entity.canonical_name == name)
            );
            assert!(
                result
                    .document()
                    .skipped_regions
                    .iter()
                    .any(
                        |gap| gap.reason == SkippedRegionReason::UnsupportedConstruct
                            && text(source, &gap.source).starts_with(name)
                    )
            );
        }
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Calls)
        );
    }
}
