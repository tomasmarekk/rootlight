//! Apex source identities through the public analyzer and validated IR.
//! Exact owners, headers and source bytes do not imply Salesforce compilation,
//! inferred platform members, overload dispatch or resolved reference bindings.

use super::*;

const APEX: LanguageCase = LanguageCase {
    name: "apex",
    path: "src/definitions.cls",
    frontend: "tree-sitter-sfapex-3.0.1",
    source: "public class CountBox { public Integer Add(Integer Value) { return Value+1; } }",
    generated: false,
    body_before: "Value+1",
    body_after: "Value+2",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(APEX, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, APEX),
        &request(&fixture.snapshot, &fixture.source, APEX, &budget),
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
    result
}

fn named<'a>(
    result: &'a AnalysisOutput,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::EntityRecord {
    let matches = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name && entity.kind == kind)
        .collect::<Vec<_>>();
    assert_eq!(
        matches.len(),
        1,
        "{name}/{kind:?}: {:?}",
        result.document().entities
    );
    matches[0]
}

fn text<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

fn assert_definition(result: &AnalysisOutput, source: &str, entity: &rootlight_ir::EntityRecord) {
    let definitions = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.target == (OccurrenceTarget::Resolved { symbol: entity.id })
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), 1, "{}", entity.canonical_name);
    let definition = definitions[0];
    assert_eq!(text(source, &definition.source), entity.canonical_name);
    assert_eq!(
        definition.syntactic_text_hash,
        content_hash(entity.canonical_name.as_bytes())
    );
    assert_eq!(definition.source.repository(), result.document().repository);
    assert_eq!(definition.source.generation(), result.document().generation);
    assert_eq!(
        definition.source.content_hash(),
        content_hash(source.as_bytes())
    );
    let evidence = entity.evidence.source.as_ref().unwrap();
    assert_eq!(definition.source.span().file(), evidence.span().file());
    assert!(evidence.span().start_byte() <= definition.source.span().start_byte());
    assert!(definition.source.span().end_byte() <= evidence.span().end_byte());
}

fn assert_child(
    result: &AnalysisOutput,
    parent: &rootlight_ir::EntityRecord,
    child: &rootlight_ir::EntityRecord,
) {
    assert_eq!(
        child.container,
        Some(rootlight_ir::ContainerRef::Entity(parent.id))
    );
    assert!(result.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Contains
            && relation.subject == RelationEndpoint::Entity(parent.id)
            && relation.object == RelationEndpoint::Entity(child.id)
    }));
}

fn signatures(
    result: &AnalysisOutput,
    source: &str,
    entity: &rootlight_ir::EntityRecord,
) -> Vec<String> {
    result
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::LEXICAL_EXTENSION_NAMESPACE)
        .filter_map(|extension| {
            let lexical = rootlight_ir::decode_lexical_evidence_envelope(extension).unwrap();
            if lexical.kind() != rootlight_ir::LexicalEvidenceKind::Signature
                || lexical.subject() != rootlight_ir::FactRef::Entity(entity.id)
            {
                return None;
            }
            assert!(!lexical.is_truncated());
            let reference = extension.evidence.source.as_ref().unwrap();
            lexical
                .verify_source_bytes(text(source, reference).as_bytes())
                .unwrap();
            assert_eq!(reference.generation(), result.document().generation);
            assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
            Some(lexical.text().to_owned())
        })
        .collect()
}

fn assert_unqualified(result: &AnalysisOutput) {
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
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role != OccurrenceRole::Definition)
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "apex-compiler-and-platform-semantics-unavailable")
    );
}

#[test]
fn apex_written_types_callables_formals_and_single_fields_keep_exact_owners_and_crlf() {
    let source = "public with sharing class CounterBox {\r\n\
        public enum Mode { Ready, Waiting }\r\n\
        private Integer total;\r\n\
        public CounterBox(Integer Start) { total = Start; }\r\n\
        public Integer Add(Integer Amount) { return total + Amount; }\r\n}";
    let result = output(source);
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    for (name, kind) in [
        ("CounterBox", EntityKind::Class),
        ("CounterBox", EntityKind::Constructor),
        ("Mode", EntityKind::Enum),
        ("Ready", EntityKind::Constant),
        ("Waiting", EntityKind::Constant),
        ("total", EntityKind::Field),
        ("Add", EntityKind::Method),
        ("Start", EntityKind::Parameter),
        ("Amount", EntityKind::Parameter),
    ] {
        assert_definition(&result, source, named(&result, name, kind));
    }
    let class = named(&result, "CounterBox", EntityKind::Class);
    let enumeration = named(&result, "Mode", EntityKind::Enum);
    let constructor = named(&result, "CounterBox", EntityKind::Constructor);
    let method = named(&result, "Add", EntityKind::Method);
    let field = named(&result, "total", EntityKind::Field);
    for child in [enumeration, constructor, method, field] {
        assert_child(&result, class, child);
    }
    for name in ["Ready", "Waiting"] {
        assert_child(
            &result,
            enumeration,
            named(&result, name, EntityKind::Constant),
        );
    }
    assert_child(
        &result,
        constructor,
        named(&result, "Start", EntityKind::Parameter),
    );
    assert_child(
        &result,
        method,
        named(&result, "Amount", EntityKind::Parameter),
    );
    for (entity, expected) in [
        (class, "public with sharing class CounterBox"),
        (enumeration, "public enum Mode"),
        (constructor, "public CounterBox(Integer Start)"),
        (method, "public Integer Add(Integer Amount)"),
        (field, "private Integer total;"),
        (
            named(&result, "Amount", EntityKind::Parameter),
            "Integer Amount",
        ),
    ] {
        assert_eq!(signatures(&result, source, entity), [expected]);
    }
    assert_eq!(
        text(source, class.evidence.source.as_ref().unwrap()),
        source
    );
    assert_eq!(
        text(source, field.evidence.source.as_ref().unwrap()),
        "private Integer total;"
    );
    assert_unqualified(&result);
}

#[test]
fn apex_multiple_fields_keep_separate_declarators_and_a_shared_source_header() {
    let source = "public class OuterCase {\n\
        private Integer First = 1, Second = 2;\n\
        public class InnerCase { public InnerCase() {}\n\
        public Integer Echo(Integer Raw) { return Raw; } }\n}";
    let result = output(source);
    let outer = named(&result, "OuterCase", EntityKind::Class);
    let inner = named(&result, "InnerCase", EntityKind::Class);
    let first = named(&result, "First", EntityKind::Field);
    let second = named(&result, "Second", EntityKind::Field);
    assert_ne!(first.id, second.id);
    for (entity, witness) in [(first, "First = 1"), (second, "Second = 2")] {
        assert_definition(&result, source, entity);
        assert_child(&result, outer, entity);
        assert_eq!(
            text(source, entity.evidence.source.as_ref().unwrap()),
            witness
        );
        assert!(signatures(&result, source, entity).is_empty());
    }
    assert_child(&result, outer, inner);
    let method = named(&result, "Echo", EntityKind::Method);
    assert_child(&result, inner, method);
    assert_child(
        &result,
        method,
        named(&result, "Raw", EntityKind::Parameter),
    );
    assert_child(
        &result,
        inner,
        named(&result, "InnerCase", EntityKind::Constructor),
    );
    let gaps = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "apex-shared-field-header-association-unavailable")
        .collect::<Vec<_>>();
    assert!(!gaps.is_empty());
    for gap in gaps {
        let reference = gap.evidence.source.as_ref().unwrap();
        assert_eq!(
            text(source, reference),
            "private Integer First = 1, Second = 2;"
        );
        assert_eq!(reference.generation(), result.document().generation);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
    }
    assert!(
        !result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "signature-capture-unavailable"
                && gap.evidence.source.as_ref().is_some_and(|reference| {
                    ["First = 1", "Second = 2"].contains(&text(source, reference))
                }))
    );
    assert_unqualified(&result);
}

#[test]
fn apex_interface_prototype_keeps_a_written_header_without_a_fabricated_body() {
    let source = "public interface ValueSource { Integer Read(String Key); }";
    let result = output(source);
    let interface = named(&result, "ValueSource", EntityKind::Interface);
    let method = named(&result, "Read", EntityKind::Method);
    let formal = named(&result, "Key", EntityKind::Parameter);
    for entity in [interface, method, formal] {
        assert_definition(&result, source, entity);
    }
    assert_child(&result, interface, method);
    assert_child(&result, method, formal);
    assert_eq!(
        signatures(&result, source, method),
        ["Integer Read(String Key)"]
    );
    assert_eq!(
        text(source, method.evidence.source.as_ref().unwrap()),
        "Integer Read(String Key);"
    );
    assert_eq!(signatures(&result, source, formal), ["String Key"]);
    assert_unqualified(&result);
}

#[test]
fn apex_same_name_written_overload_headers_have_distinct_identities() {
    let source = "public class OverloadBox {\n\
        public OverloadBox(Integer Count) {} public OverloadBox(String Label) {}\n\
        public Integer Echo(Integer Value) { return Value; }\n\
        public String Echo(String Value) { return Value; }\n}";
    let result = output(source);
    let class = named(&result, "OverloadBox", EntityKind::Class);
    for (name, kind, expected) in [
        (
            "OverloadBox",
            EntityKind::Constructor,
            [
                "public OverloadBox(Integer Count)",
                "public OverloadBox(String Label)",
            ],
        ),
        (
            "Echo",
            EntityKind::Method,
            [
                "public Integer Echo(Integer Value)",
                "public String Echo(String Value)",
            ],
        ),
    ] {
        let entities = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.kind == kind)
            .collect::<Vec<_>>();
        assert_eq!(entities.len(), 2, "{name}/{kind:?}");
        assert_ne!(entities[0].id, entities[1].id);
        let mut headers = Vec::new();
        for entity in entities {
            assert_definition(&result, source, entity);
            assert_child(&result, class, entity);
            headers.extend(signatures(&result, source, entity));
        }
        headers.sort();
        let mut expected = expected.map(str::to_owned).to_vec();
        expected.sort();
        assert_eq!(headers, expected);
    }
    assert_unqualified(&result);
}

#[test]
fn apex_annotations_and_source_uses_do_not_establish_dispatch_or_platform_bindings() {
    let source = "@IsTest private inherited sharing class ChoiceBox {\n\
        private Integer stored;\n\
        @AuraEnabled public Integer Choose(Integer Value) {\n\
        stored = Value; this.stored = Value; Choose(Value); System.debug(Value);\n\
        switch on Value { when 1, 2 { return 3; } when else { return 0; } } }\n}";
    let result = output(source);
    let method = named(&result, "Choose", EntityKind::Method);
    assert_eq!(
        signatures(&result, source, method),
        ["@AuraEnabled public Integer Choose(Integer Value)"]
    );
    for name in ["IsTest", "AuraEnabled", "System", "debug"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
    for syntax in [
        "apex.identifier.reference",
        "apex.member.reference",
        "apex.unproven_call.reference",
    ] {
        assert!(
            result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| occurrence.syntax_kind == syntax),
            "{syntax}"
        );
    }
    for annotation in ["@IsTest", "@AuraEnabled"] {
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .filter_map(|gap| gap.evidence.source.as_ref())
                .any(|reference| text(source, reference) == annotation)
        );
    }
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role != OccurrenceRole::Definition)
            .any(|occurrence| ["IsTest", "AuraEnabled", "Integer"]
                .contains(&text(source, &occurrence.source)))
    );
    assert_unqualified(&result);
}

#[test]
fn apex_malformed_named_owners_cannot_borrow_a_following_class() {
    let source = "public class Cracked {\n\
        public Integer Broken( { Integer Leaked; }\n}\n\
        public class StableBox { public Integer Good(Integer Value) { return Value; } }";
    let result = output(source);
    for name in ["Cracked", "Broken", "Leaked"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
    }
    let class = named(&result, "StableBox", EntityKind::Class);
    let method = named(&result, "Good", EntityKind::Method);
    let formal = named(&result, "Value", EntityKind::Parameter);
    for entity in [class, method, formal] {
        assert_definition(&result, source, entity);
    }
    assert_child(&result, class, method);
    assert_child(&result, method, formal);
    assert_eq!(
        text(source, class.evidence.source.as_ref().unwrap()),
        "public class StableBox { public Integer Good(Integer Value) { return Value; } }"
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "apex-source-identity-unavailable")
    );
    assert_unqualified(&result);
}

#[test]
fn apex_comments_and_quoted_declarations_are_not_symbol_identities() {
    let source = "// public class Phantom {}\n\
        /** Public source documentation. */ public class NoteBox {\n\
        /* public Integer Bogus() { return 1; } */\n\
        private String Note = 'class Hidden { public void Fake() {} }';\n\
        public String Read() { return Note; }\n}";
    let result = output(source);
    for (name, kind) in [
        ("NoteBox", EntityKind::Class),
        ("Note", EntityKind::Field),
        ("Read", EntityKind::Method),
    ] {
        assert_definition(&result, source, named(&result, name, kind));
    }
    for name in ["Phantom", "Bogus", "Hidden", "Fake"] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name)
        );
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|occurrence| text(source, &occurrence.source) == name)
        );
    }
    assert_unqualified(&result);
}

#[test]
fn apex_accessor_fields_remain_explicit_source_gaps_without_invented_field_headers() {
    let source = "public class PropertyBox { public Integer Count { get; set; }\n\
        public Integer Read(Integer Value) { return Value; } }";
    let result = output(source);
    let class = named(&result, "PropertyBox", EntityKind::Class);
    let method = named(&result, "Read", EntityKind::Method);
    assert_child(&result, class, method);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Count")
    );
    assert!(result.document().skipped_regions.iter().any(|gap| {
        gap.detail == "apex-accessor-semantics-unavailable"
            && text(source, &gap.source) == "public Integer Count { get; set; }"
    }));
    assert_unqualified(&result);
}

#[test]
fn apex_body_only_edits_keep_declared_ids_but_update_all_source_generations() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, APEX);
    let fixture = Fixture::new(APEX, APEX.source.as_bytes());
    let budget = limits();
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, APEX, &budget),
        &ExtensionSupport::default(),
    );
    let bytes = APEX.source.replace(APEX.body_before, APEX.body_after);
    let changed = fixture.rewrite(bytes.as_bytes());
    let next = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, APEX, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(next.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(
        first
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>(),
        next.document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    );
    assert_ne!(fixture.source.generation(), changed.source.generation());
    for entity in &next.document().entities {
        let reference = entity.evidence.source.as_ref().unwrap();
        assert_eq!(reference.generation(), changed.source.generation());
        assert_eq!(reference.content_hash(), changed.source.content_hash());
    }
    for occurrence in &next.document().occurrences {
        assert_eq!(occurrence.source.generation(), changed.source.generation());
        assert_eq!(
            occurrence.source.content_hash(),
            changed.source.content_hash()
        );
    }
    assert_unqualified(&next);
}
