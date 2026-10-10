//! Public Common Lisp facts preserve written spans without evaluated bindings.

use super::*;

const COMMON_LISP: LanguageCase = LanguageCase {
    name: "common-lisp",
    path: "src/definitions.lisp",
    frontend: "tree-sitter-commonlisp-0.4.1",
    source: "(defun measure (value) (+ value 1))\n",
    generated: false,
    body_before: "value 1",
    body_after: "value 2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(COMMON_LISP, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, COMMON_LISP),
        &request(&fixture.snapshot, &fixture.source, COMMON_LISP, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(
        result.document().files[0].content_hash,
        content_hash(source.as_bytes())
    );
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(source.len()).unwrap()
    );
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
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "common-lisp-reader-and-runtime-binding-unavailable")
    );
    result
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

fn assert_definition(result: &AnalysisOutput, source: &str, name: &str, kind: EntityKind) {
    let entity = named(result, name);
    assert_eq!(entity.kind, kind);
    let occurrences = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
        })
        .collect::<Vec<_>>();
    assert_eq!(occurrences.len(), 1, "{name}");
    assert_eq!(text(source, &occurrences[0].source), name);
    assert_eq!(
        occurrences[0].syntactic_text_hash,
        content_hash(name.as_bytes())
    );
    assert_eq!(
        occurrences[0].source.generation(),
        result.document().generation
    );
    assert_eq!(
        occurrences[0].source.content_hash(),
        content_hash(source.as_bytes())
    );
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
            "unexpected read {name}"
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
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == domain
                && gap.detail == detail
                && text(source, &gap.source) == region),
        "missing {domain:?} gap {detail} at {region:?}"
    );
}

#[test]
fn common_lisp_written_declarations_and_required_formals_keep_exact_owners() {
    let source = "(defpackage :written-api (:export :measure))\n\
        (in-package :written-api)\n\
        (defclass box () ((slot :initarg :slot)))\n\
        (defvar *written-state* nil)\n\
        (defparameter *written-limit* 8)\n\
        (defconstant +written-unit+ 1)\n\
        (defun measure (value) (+ value 1))\n";
    let result = output(source);
    for (name, kind) in [
        (":written-api", EntityKind::Namespace),
        ("box", EntityKind::Class),
        ("*written-state*", EntityKind::Variable),
        ("*written-limit*", EntityKind::Variable),
        ("+written-unit+", EntityKind::Constant),
        ("measure", EntityKind::Function),
        ("value", EntityKind::Parameter),
    ] {
        assert_definition(&result, source, name, kind);
    }
    assert_eq!(
        named(&result, "value").container,
        Some(rootlight_ir::ContainerRef::Entity(
            named(&result, "measure").id
        ))
    );
    assert_eq!(signatures(&result, "measure"), ["(defun measure (value)"]);
    assert_eq!(signatures(&result, "box"), ["(defclass box ()"]);
    assert_absent(&result, &["slot", "written-api"]);
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "common-lisp-written-owner-analysis-unavailable",
        "((slot :initarg :slot))",
    );
    let reads = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Reference)
        .collect::<Vec<_>>();
    assert!(
        reads
            .iter()
            .any(|occurrence| text(source, &occurrence.source) == "value")
    );
    assert!(
        reads
            .iter()
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
}

#[test]
fn common_lisp_preserves_raw_escaped_and_package_qualified_names() {
    let names = [
        "|Mixed Name|",
        "pkg:written",
        "pkg::internal",
        "|Mixed Package|::|Mixed Name|",
        r"escaped\ name",
        r"|pipe\|name|",
        "prefix|two words|tail",
        "|start|middle|end|",
        r"\|literal",
        r"|package:part|::|name:part|",
        r"package\ part:member\ name",
        "+",
        "name#tail",
    ];
    for name in names {
        let source = format!("(defun {name} () nil)\n");
        let result = output(&source);
        assert_definition(&result, &source, name, EntityKind::Function);
        assert_eq!(signatures(&result, name), [format!("(defun {name} ()")]);
    }
}

#[test]
fn common_lisp_same_named_written_kinds_and_methods_are_not_merged() {
    let source = "(defun same (a) a)\n(defmacro same (b) b)\n(defgeneric same (c))\n\
        (defmethod same :around ((d box)) (call-next-method))\n\
        (defmethod same ((e other)) e)\n";
    let result = output(source);
    let entities = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "same")
        .collect::<Vec<_>>();
    assert_eq!(entities.len(), 5);
    assert_eq!(
        entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Function)
            .count(),
        3
    );
    assert_eq!(
        entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Method)
            .count(),
        2
    );
    assert_eq!(
        entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
            .len(),
        5
    );
    for entity in entities {
        let definitions = result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(text(source, &definitions[0].source), "same");
    }
    assert_absent(&result, &["d", "box", "other", "around"]);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "e")
    );
    let reads = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Reference
                && occurrence.syntactic_text_hash == content_hash(b"e")
        })
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    assert_eq!(text(source, &read.source), "e");
    assert_eq!(
        read.source.span().start_byte(),
        u64::try_from(source.rfind(" e)").unwrap() + 1).unwrap()
    );
    assert_eq!(read.source.generation(), result.document().generation);
    assert_eq!(read.source.content_hash(), content_hash(source.as_bytes()));
    assert_eq!(
        read.target,
        OccurrenceTarget::Unresolved {
            text_hash: content_hash(b"e")
        }
    );
    let owner = result
        .document()
        .entities
        .iter()
        .find(|entity| Some(entity.id) == read.enclosing)
        .unwrap();
    assert_eq!(owner.kind, EntityKind::Method);
    assert_eq!(owner.canonical_name, "same");
    assert!(result.document().occurrences.iter().any(|definition| {
        definition.role == OccurrenceRole::Definition
            && definition.target == OccurrenceTarget::Resolved { symbol: owner.id }
            && definition.source.span().start_byte()
                == u64::try_from(source.rfind("same ((e other))").unwrap()).unwrap()
    }));
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "common-lisp-parameter-owner-analysis-unavailable",
        "((e other))",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Relations,
        "common-lisp-binding-target-unavailable",
        "e",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "common-lisp-parameter-owner-analysis-unavailable",
        "((d box))",
    );
}

#[test]
fn common_lisp_mixed_case_operators_preserve_written_name_case() {
    let source = "(DEFUN MEASURE-UPPER (ITEM) ITEM)\n\
        (CL:DEFMACRO BUILD-UPPER (ARGUMENT) ARGUMENT)\n\
        (COMMON-LISP:DEFUN Other (Parameter) Parameter)\n";
    let result = output(source);
    for name in ["MEASURE-UPPER", "BUILD-UPPER", "Other"] {
        assert_definition(&result, source, name, EntityKind::Function);
    }
    for name in ["ITEM", "ARGUMENT", "Parameter"] {
        assert_definition(&result, source, name, EntityKind::Parameter);
    }
    assert_absent(&result, &["measure-upper", "build-upper", "other"]);
    assert_eq!(
        signatures(&result, "BUILD-UPPER"),
        ["(CL:DEFMACRO BUILD-UPPER (ARGUMENT)"]
    );
}

#[test]
fn common_lisp_quoted_strings_comments_and_templates_do_not_create_entities() {
    let source = "'(defun quoted () nil)\n(quote (defun explicit-quote () nil))\n\
        (function (lambda (anonymous) anonymous))\n\
        `(defun template (,name) ,body)\n#_(defun discarded () nil)\n\
        \"(defun string-phantom () nil)\"\n; (defun comment-phantom () nil)\n\
        #| outer #| inner |# (defun block-phantom () nil) |#\n\
        (defun live (value) value)\n";
    let result = output(source);
    assert_definition(&result, source, "live", EntityKind::Function);
    assert_absent(
        &result,
        &[
            "quoted",
            "explicit-quote",
            "anonymous",
            "template",
            "name",
            "body",
            "discarded",
            "string-phantom",
            "comment-phantom",
            "block-phantom",
        ],
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.domain == FactDomain::Entities)
            .all(|gap| text(source, &gap.source) == "#_(defun discarded () nil)")
    );
}

#[test]
fn common_lisp_reader_targets_are_retained_without_selecting_features() {
    let source = "#+feature (defun conditional-one () nil)\n\
        #-feature (defun conditional-two () nil)\n\
        #+(defun condition-data () nil) (defun physical-target () nil)\n";
    let result = output(source);
    for name in ["conditional-one", "conditional-two", "physical-target"] {
        assert_definition(&result, source, name, EntityKind::Function);
    }
    assert_absent(&result, &["condition-data", "feature"]);
    for region in [
        "#+feature (defun conditional-one () nil)",
        "#-feature (defun conditional-two () nil)",
        "#+(defun condition-data () nil) (defun physical-target () nil)",
    ] {
        assert_gap(
            &result,
            source,
            FactDomain::Relations,
            "common-lisp-reader-conditional-selection-unavailable",
            region,
        );
    }
}

#[test]
fn common_lisp_local_macros_and_complex_formals_have_scoped_identity_gaps() {
    let source = "(macrolet ((defun (name args &body body) `(list ',name ',args))) (defun not-global (item) item))\n\
        (symbol-macrolet ((alias 1)) (defun also-not-global () alias))\n\
        (defun combine (first &optional (second default supplied) &rest rest &key ((:external internal) initial) &aux scratch) first)\n\
        (lambda (anonymous) anonymous)\n";
    let result = output(source);
    assert_definition(&result, source, "combine", EntityKind::Function);
    assert_definition(&result, source, "first", EntityKind::Parameter);
    assert_absent(
        &result,
        &[
            "not-global",
            "also-not-global",
            "alias",
            "second",
            "default",
            "supplied",
            "rest",
            "external",
            "internal",
            "initial",
            "scratch",
            "anonymous",
            "&optional",
            "&rest",
            "&key",
            "&aux",
        ],
    );
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "common-lisp-local-macro-owner-analysis-unavailable",
        "(symbol-macrolet ((alias 1)) (defun also-not-global () alias))",
    );
    assert_gap(
        &result,
        source,
        FactDomain::Entities,
        "common-lisp-parameter-owner-analysis-unavailable",
        "(first &optional (second default supplied) &rest rest &key ((:external internal) initial) &aux scratch)",
    );
}

#[test]
fn common_lisp_invisible_dispatch_and_non_atomic_designators_cannot_borrow_names() {
    for rejected in [
        "#.(defun evaluated () nil)",
        "(defun #:uninterned () nil)",
        "(defun (setf cell) (value) value)",
        "(defpackage \"string-package\" (:export :item))",
    ] {
        let source = format!("{rejected}\n(defun stable () nil)\n");
        let result = output(&source);
        assert_definition(&result, &source, "stable", EntityKind::Function);
        assert_absent(
            &result,
            &["evaluated", "uninterned", "cell", "value", "string-package"],
        );
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == FactDomain::Entities
                    && matches!(
                        gap.detail.as_str(),
                        "common-lisp-written-owner-analysis-unavailable"
                            | "common-lisp-reader-context-analysis-unavailable"
                    )),
            "missing identity gap for {rejected}"
        );
    }
}
