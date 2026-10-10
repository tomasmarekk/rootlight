//! Public Scheme facts preserve physical owners and raw written source names.
//! Reader data cannot fabricate declarations or evaluated binding relationships.

use super::*;

const SCHEME: LanguageCase = LanguageCase {
    name: "scheme",
    path: "src/definitions.scm",
    frontend: "tree-sitter-scheme-native-local-0.24.7-1",
    source: "(define (measure value) (+ value 1))\n",
    generated: false,
    body_before: "value 1",
    body_after: "value 2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(SCHEME, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, SCHEME),
        &request(&fixture.snapshot, &fixture.source, SCHEME, &budget),
        &ExtensionSupport::default(),
    );
    assert_source_contract(&result, source, &budget);
    result
}

fn assert_source_contract(result: &AnalysisOutput, source: &str, budget: &AnalysisLimits) {
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    let file = &result.document().files[0];
    assert_eq!(file.content_hash, content_hash(source.as_bytes()));
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        result
            .document()
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|occurrence| occurrence.role != OccurrenceRole::CallSite)
    );
    assert_gap(
        result,
        source,
        FactDomain::Relations,
        "scheme-reader-library-and-runtime-binding-unavailable",
        source,
    );
}

fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

fn named<'a>(result: &'a AnalysisOutput, name: &str) -> &'a rootlight_ir::EntityRecord {
    let entities = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name)
        .collect::<Vec<_>>();
    assert_eq!(
        entities.len(),
        1,
        "{name}: {:?}",
        result.document().entities
    );
    entities[0]
}

fn assert_entity_definition(
    result: &AnalysisOutput,
    source: &str,
    entity: &rootlight_ir::EntityRecord,
) {
    let definitions = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1, "{}", entity.canonical_name);
    let definition = definitions[0];
    assert_eq!(text(source, &definition.source), entity.canonical_name);
    assert_eq!(entity.display_name, entity.canonical_name);
    assert_eq!(
        definition.syntactic_text_hash,
        content_hash(entity.canonical_name.as_bytes())
    );
    assert_eq!(definition.source.generation(), result.document().generation);
    assert_eq!(
        definition.source.content_hash(),
        content_hash(source.as_bytes())
    );
}

fn assert_definition(result: &AnalysisOutput, source: &str, name: &str, kind: EntityKind) {
    let entity = named(result, name);
    assert_eq!(entity.kind, kind, "{name}");
    assert_entity_definition(result, source, entity);
}

fn assert_absent(result: &AnalysisOutput, names: &[&str]) {
    for name in names {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == *name),
            "unexpected entity {name}"
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Reference
                    && occurrence.syntactic_text_hash == content_hash(name.as_bytes())),
            "unexpected reference {name}"
        );
    }
}

fn signatures(result: &AnalysisOutput, name: &str) -> Vec<String> {
    let id = named(result, name).id;
    result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .filter_map(|extension| {
            let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            (lexical.kind() == rootlight_ir::LexicalEvidenceKind::Signature
                && lexical.subject() == rootlight_ir::FactRef::Entity(id))
            .then(|| {
                assert!(!lexical.is_truncated());
                lexical.text().to_owned()
            })
        })
        .collect()
}

fn assert_gap(
    result: &AnalysisOutput,
    source: &str,
    domain: FactDomain,
    detail: &str,
    region: &str,
) {
    let matching = result.document().skipped_regions.iter().find(|gap| {
        gap.domain == domain && gap.detail == detail && text(source, &gap.source) == region
    });
    let gap = matching.unwrap_or_else(|| panic!("missing {domain:?} gap {detail} at {region:?}"));
    assert_eq!(gap.source.generation(), result.document().generation);
    assert_eq!(gap.source.content_hash(), content_hash(source.as_bytes()));
    assert_eq!(gap.reason, SkippedRegionReason::UnsupportedConstruct);
}

#[test]
fn scheme_written_declarations_and_formals_preserve_exact_raw_names() {
    let source = "#!fold-case\n(define λ 1)\n\
        (define (|Mixed Name| |arg space| . rest) |arg space|)\n\
        (define \\x61;Name 2)\n(define delayed (lambda args args))\n";
    let result = output(source);
    for (name, kind) in [
        ("λ", EntityKind::Variable),
        ("|Mixed Name|", EntityKind::Function),
        ("|arg space|", EntityKind::Parameter),
        ("rest", EntityKind::Parameter),
        (r"\x61;Name", EntityKind::Variable),
        ("delayed", EntityKind::Function),
        ("args", EntityKind::Parameter),
    ] {
        assert_definition(&result, source, name, kind);
    }
    for name in ["|arg space|", "rest"] {
        assert_eq!(
            named(&result, name).container,
            Some(rootlight_ir::ContainerRef::Entity(
                named(&result, "|Mixed Name|").id
            ))
        );
    }
    assert_eq!(
        named(&result, "args").container,
        Some(rootlight_ir::ContainerRef::Entity(
            named(&result, "delayed").id
        ))
    );
    assert_eq!(
        signatures(&result, "|Mixed Name|"),
        ["(define (|Mixed Name| |arg space| . rest)"]
    );
    assert_eq!(
        signatures(&result, "delayed"),
        ["(define delayed (lambda args"]
    );
    assert_absent(&result, &["mixed name", "Mixed Name", "arg space", "aName"]);
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "scheme-reader-environment-unavailable",
        "#!fold-case",
    );
}

#[test]
fn scheme_internal_and_local_names_have_distinct_physical_owners() {
    let source = "(define (first item) (define copy item) (let loop ((next item)) (let* ((temp next)) temp)))\n\
        (define (second item) (define copy item) (letrec ((again item)) again))\n\
        (let-values (((left right) (values 1 2)) ((head . tail) (values 3 4))) left)\n";
    let result = output(source);
    for (name, kind) in [
        ("first", EntityKind::Function),
        ("second", EntityKind::Function),
        ("loop", EntityKind::Function),
        ("next", EntityKind::Variable),
        ("temp", EntityKind::Variable),
        ("again", EntityKind::Variable),
        ("left", EntityKind::Variable),
        ("right", EntityKind::Variable),
        ("head", EntityKind::Variable),
        ("tail", EntityKind::Variable),
    ] {
        assert_definition(&result, source, name, kind);
    }
    let expected_owners = ["first", "second"]
        .map(|name| rootlight_ir::ContainerRef::Entity(named(&result, name).id))
        .into_iter()
        .collect::<BTreeSet<_>>();
    for (name, kind) in [
        ("item", EntityKind::Parameter),
        ("copy", EntityKind::Variable),
    ] {
        let entities = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect::<Vec<_>>();
        assert_eq!(entities.len(), 2, "{name}");
        assert_ne!(entities[0].id, entities[1].id);
        assert_eq!(
            entities
                .iter()
                .map(|entity| entity.container.unwrap())
                .collect::<BTreeSet<_>>(),
            expected_owners
        );
        for entity in entities {
            assert_eq!(entity.kind, kind);
            assert_entity_definition(&result, source, entity);
        }
    }
    assert_eq!(
        named(&result, "next").container,
        Some(rootlight_ir::ContainerRef::Entity(
            named(&result, "loop").id
        ))
    );
}

#[test]
fn scheme_quote_comment_and_macro_data_cannot_fabricate_facts() {
    let source = "(define-syntax build (syntax-rules () ((_ name) (define name hidden))))\n\
        '(define quoted secret)\n(quote (define explicit secret))\n\
        `(define template ,(define unquoted secret))\n#;(define discarded secret)\n\
        #((define vector-data secret))\n#1=(define labelled secret)\n#1#\n\
        ; (define line-comment secret)\n#| (define block-comment secret) |#\n\
        (syntax-case input () ((_ name) (define name hidden)))\n\
        (define live visible)\n";
    let result = output(source);
    assert_definition(&result, source, "build", EntityKind::Function);
    assert_definition(&result, source, "live", EntityKind::Variable);
    assert_absent(
        &result,
        &[
            "name",
            "hidden",
            "quoted",
            "explicit",
            "template",
            "unquoted",
            "discarded",
            "vector-data",
            "labelled",
            "line-comment",
            "block-comment",
            "secret",
            "input",
        ],
    );
    let reads = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .map(|occurrence| text(source, &occurrence.source))
        .collect::<Vec<_>>();
    assert_eq!(reads, ["visible"]);
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "scheme-macro-expansion-unavailable",
        "(define-syntax build (syntax-rules () ((_ name) (define name hidden))))",
    );
}

#[test]
fn scheme_unresolved_reads_keep_exact_source_and_scoped_relationship_gaps() {
    let source = "(define (read value) (consume value))\n";
    let result = output(source);
    let owner = named(&result, "read");
    let reads = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 2);
    assert_eq!(
        reads
            .iter()
            .map(|read| text(source, &read.source))
            .collect::<BTreeSet<_>>(),
        BTreeSet::from(["consume", "value"])
    );
    for read in reads {
        let spelling = text(source, &read.source);
        assert_eq!(read.enclosing, Some(owner.id));
        assert_eq!(read.syntactic_text_hash, content_hash(spelling.as_bytes()));
        assert_eq!(
            read.target,
            OccurrenceTarget::Unresolved {
                text_hash: content_hash(spelling.as_bytes())
            }
        );
        assert_eq!(read.source.generation(), result.document().generation);
        assert_eq!(read.source.content_hash(), content_hash(source.as_bytes()));
        assert_gap(
            &result,
            source,
            FactDomain::Relations,
            "scheme-binding-target-unavailable",
            spelling,
        );
    }
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "scheme-binding-and-call-analysis-unavailable",
        "(define (read value) (consume value))",
    );
    assert!(result.report().coverage().domains().iter().any(|domain| {
        domain.domain() == FactDomain::Relations && domain.status() != CoverageStatus::Complete
    }));
}

#[test]
fn scheme_records_preserve_explicit_fields_and_reject_scalar_tail_owners() {
    let source = "(define-record-type node (fields e (mutable count get-count set-count)))\n\
        (define-record-type <pair> (make-pair left) pair? (left pair-left))\n\
        (define-record-type row)\n(define-record-type (named make-named named?))\n\
        (define-record-type numeric-tail 123)\n";
    let result = output(source);
    for name in ["node", "<pair>", "row", "named"] {
        assert_definition(&result, source, name, EntityKind::Struct);
    }
    for (name, owner) in [("e", "node"), ("count", "node"), ("left", "<pair>")] {
        assert_definition(&result, source, name, EntityKind::Field);
        assert_eq!(
            named(&result, name).container,
            Some(rootlight_ir::ContainerRef::Entity(named(&result, owner).id))
        );
    }
    assert_absent(
        &result,
        &[
            "get-count",
            "set-count",
            "make-pair",
            "pair?",
            "pair-left",
            "make-named",
            "named?",
            "numeric-tail",
        ],
    );
    for region in [
        "(define-record-type row)",
        "(define-record-type (named make-named named?))",
    ] {
        assert_gap(
            &result,
            source,
            FactDomain::Entities,
            "scheme-record-clause-and-generated-member-analysis-unavailable",
            region,
        );
    }
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "scheme-written-owner-analysis-unavailable",
        "(define-record-type numeric-tail 123)",
    );
}

#[test]
fn scheme_body_only_edits_keep_symbol_identity_and_refresh_source_generation() {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(SCHEME, SCHEME.source.as_bytes());
    let budget = limits();
    let analyzer = analyzer(&provider, SCHEME);
    let initial = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, SCHEME, &budget),
        &ExtensionSupport::default(),
    );
    let changed_source = SCHEME
        .source
        .replacen(SCHEME.body_before, SCHEME.body_after, 1);
    assert_ne!(changed_source, SCHEME.source);
    let changed_fixture = fixture.rewrite(changed_source.as_bytes());
    let changed = analyze(
        &analyzer,
        &request(
            &changed_fixture.snapshot,
            &changed_fixture.source,
            SCHEME,
            &budget,
        ),
        &ExtensionSupport::default(),
    );
    assert_source_contract(&initial, SCHEME.source, &budget);
    assert_source_contract(&changed, &changed_source, &budget);
    assert_ne!(initial.document().generation, changed.document().generation);
    assert_ne!(
        initial.document().files[0].content_hash,
        changed.document().files[0].content_hash
    );
    assert_eq!(
        initial.document().entities.len(),
        changed.document().entities.len()
    );
    for name in ["measure", "value"] {
        assert_eq!(named(&initial, name).id, named(&changed, name).id);
        assert_eq!(
            named(&initial, name).container,
            named(&changed, name).container
        );
        assert_entity_definition(&initial, SCHEME.source, named(&initial, name));
        assert_entity_definition(&changed, &changed_source, named(&changed, name));
    }
    assert_eq!(signatures(&initial, "measure"), ["(define (measure value)"]);
    assert_eq!(
        signatures(&initial, "measure"),
        signatures(&changed, "measure")
    );
}
