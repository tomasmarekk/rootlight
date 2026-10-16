//! Apex source identities through the public analyzer and validated IR.
//! Exact owners, headers and source bytes do not imply Salesforce compilation,
//! inferred platform members or overload dispatch. Only native-proven written
//! parameters and ordinary block locals acquire exact lexical targets.

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
    for relation in &result.document().relations {
        if relation.predicate == RelationPredicate::Contains {
            continue;
        }
        assert_eq!(relation.predicate, RelationPredicate::RefersTo);
        let RelationEndpoint::Occurrence(occurrence_id) = relation.subject else {
            panic!("a lexical relation must start at an occurrence");
        };
        let occurrence = result
            .document()
            .occurrences
            .iter()
            .find(|occurrence| occurrence.id == occurrence_id)
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
            panic!("a lexical relation must retain its proven target");
        };
        assert!(matches!(
            occurrence.syntax_kind.as_str(),
            "apex.formal_identifier.reference" | "apex.local_identifier.reference"
        ));
        assert_eq!(relation.object, RelationEndpoint::Entity(symbol));
        assert_eq!(relation.evidence.source.as_ref(), Some(&occurrence.source));
    }
    assert!(
        result
            .document()
            .occurrences
            .iter()
            .all(|occurrence| occurrence.role != OccurrenceRole::CallSite)
    );
    for occurrence in result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role != OccurrenceRole::Definition)
    {
        let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
            continue;
        };
        assert!(matches!(
            occurrence.syntax_kind.as_str(),
            "apex.formal_identifier.reference" | "apex.local_identifier.reference"
        ));
        assert_eq!(occurrence.role, OccurrenceRole::Reference);
        let formal = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(
            formal.kind,
            if occurrence.syntax_kind == "apex.formal_identifier.reference" {
                EntityKind::Parameter
            } else {
                EntityKind::Variable
            }
        );
        assert_eq!(
            formal.container,
            Some(rootlight_ir::ContainerRef::Entity(lexical_callable(
                result,
                occurrence.enclosing.unwrap()
            )))
        );
        let formal_source = formal.evidence.source.as_ref().unwrap();
        assert_eq!(formal_source.repository(), occurrence.source.repository());
        assert_eq!(formal_source.generation(), occurrence.source.generation());
        assert_eq!(
            formal_source.content_hash(),
            occurrence.source.content_hash()
        );
        assert_eq!(formal_source.span().file(), occurrence.source.span().file());
        assert!(result.document().relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::RefersTo
                && relation.subject == RelationEndpoint::Occurrence(occurrence.id)
                && relation.object == RelationEndpoint::Entity(symbol)
        }));
    }
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "apex-compiler-and-platform-semantics-unavailable")
    );
}

fn lexical_callable(result: &AnalysisOutput, mut enclosing: SymbolId) -> SymbolId {
    for _ in 0..result.document().entities.len() {
        let entity = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == enclosing)
            .unwrap();
        if matches!(entity.kind, EntityKind::Method | EntityKind::Constructor) {
            return enclosing;
        }
        let Some(rootlight_ir::ContainerRef::Entity(parent)) = entity.container else {
            panic!("a lexical use must remain callable-owned");
        };
        enclosing = parent;
    }
    panic!("a lexical owner chain must be acyclic");
}

fn lexical_uses(result: &AnalysisOutput) -> Vec<&rootlight_ir::OccurrenceRecord> {
    result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role != OccurrenceRole::Definition
                && matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
        })
        .collect()
}

#[test]
fn apex_formal_value_uses_bind_only_their_exact_callable() {
    let source = "// λ preserves original UTF-8 offsets.\r\npublic class BindBox {\r\n\
        private Integer Value;\r\n\
        public BindBox(Integer Seed) { Value = Seed; }\r\n\
        public Integer First(Integer Value) {\r\n\
        Consume(Value); Value.compute(Value); /* Value */\r\n\
        return Value.member + Value; }\r\n\
        public Integer Second(Integer Value) { return Value; }\r\n\
        public Integer Third() { return Value; }\r\n\
        public class OtherBox { public Integer Third() { return Value; } }\r\n}";
    let result = output(source);
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    assert_unqualified(&result);
    let uses = lexical_uses(&result);
    assert_eq!(uses.len(), 7);
    let mut counts = BTreeMap::<&str, usize>::new();
    for occurrence in uses {
        let owner = result
            .document()
            .entities
            .iter()
            .find(|entity| Some(entity.id) == occurrence.enclosing)
            .unwrap();
        *counts.entry(&owner.canonical_name).or_default() += 1;
        let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
            unreachable!()
        };
        let target = result
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(text(source, &occurrence.source), target.canonical_name);
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(target.canonical_name.as_bytes())
        );
        let owner_source = owner.evidence.source.as_ref().unwrap();
        assert!(owner_source.span().start_byte() <= occurrence.source.span().start_byte());
        assert!(occurrence.source.span().end_byte() <= owner_source.span().end_byte());
    }
    assert_eq!(
        counts,
        BTreeMap::from([("BindBox", 1), ("First", 5), ("Second", 1)])
    );
    for occurrence in result.document().occurrences.iter().filter(|occurrence| {
        matches!(
            occurrence.syntax_kind.as_str(),
            "apex.member.reference" | "apex.unproven_call.reference"
        ) || (occurrence.role != OccurrenceRole::Definition
            && text(source, &occurrence.source) == "Value"
            && occurrence.enclosing.is_some_and(|owner| {
                result
                    .document()
                    .entities
                    .iter()
                    .any(|entity| entity.id == owner && entity.canonical_name == "Third")
            }))
    }) {
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
}

#[test]
fn apex_omitted_shadow_bindings_and_case_varied_duplicates_decline_formal_proofs() {
    for (parameters, body) in [
        ("Integer Value", "Integer value = 1; return Value;"),
        (
            "Integer Value",
            "try { Consume(Value); } catch (Exception value) { Consume(Value); } return Value;",
        ),
        (
            "Integer Value",
            "for (Integer value : Values) { Consume(Value); } return Value;",
        ),
        (
            "Integer Value",
            "for (Integer value = 0; value < 1; value++) { Consume(Value); } return Value;",
        ),
        ("Integer Value, Integer Value", "return Value;"),
        ("Integer Value, Integer value", "return Value;"),
        (
            "Integer Value",
            "switch on Value { when Account value { Consume(Value); } when else {} } return Value;",
        ),
    ] {
        let source =
            format!("public class ShadowBox {{ public Integer Read({parameters}) {{ {body} }} }}");
        let result = output(&source);
        assert_unqualified(&result);
        assert!(lexical_uses(&result).is_empty(), "{parameters}: {body}");
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "apex-binding-and-dispatch-target-unavailable")
        );
    }
}

#[test]
fn apex_query_names_and_constructor_property_names_are_not_formal_value_uses() {
    let query = "public class QueryBox { public Object Read(String Value) {\
        return [SELECT Value FROM Value WHERE Value = :Value]; } }";
    let result = output(query);
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    assert_unqualified(&result);
    assert!(lexical_uses(&result).is_empty());

    let source = "public class CreateBox { public Object Read(String Value) {\
        return new Account(Value = Value); } }";
    let result = output(source);
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    assert_unqualified(&result);
    let uses = lexical_uses(&result);
    assert_eq!(uses.len(), 1);
    assert_eq!(text(source, &uses[0].source), "Value");
    assert_eq!(
        usize::try_from(uses[0].source.span().start_byte()).unwrap(),
        source.rfind("Value").unwrap()
    );
}

#[test]
fn apex_formal_binding_is_exact_spelling_and_does_not_guess_unicode_case_folding() {
    for (formal, use_name) in [("Value", "value"), ("Ämount", "Ämount")] {
        let source = format!(
            "public class NameBox {{ public Integer Read(Integer {formal}) {{ return {use_name}; }} }}"
        );
        let result = output(&source);
        assert_unqualified(&result);
        assert!(lexical_uses(&result).is_empty());
    }
}

#[test]
fn apex_switch_label_expressions_do_not_become_formal_value_uses() {
    let source = "public class LabelBox { public Integer Read(Integer Value) {\
        switch on Value + 0 { when (Value + 1) { return Value; }\
        when else { return Value; } } } }";
    let result = output(source);
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    assert_unqualified(&result);
    assert_eq!(lexical_uses(&result).len(), 3);
    let label_start = source.find("when (Value").unwrap() + "when (".len();
    assert!(!result.document().occurrences.iter().any(|occurrence| {
        usize::try_from(occurrence.source.span().start_byte()).unwrap() == label_start
    }));
}

#[test]
fn apex_unmodeled_trigger_scope_invalidates_the_containing_formal_proof() {
    let source = "public class ScopeBox { public Integer Read(Integer Value) {\
        trigger NestedHook on Account (before insert) { Consume(Value); }\
        return Value; } }";
    let result = output(source);
    assert!(
        result.document().diagnostics.is_empty(),
        "{:?}",
        result.document().diagnostics
    );
    assert_unqualified(&result);
    assert!(lexical_uses(&result).is_empty());
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

#[test]
fn apex_single_block_local_keeps_a_header_and_exact_lexical_reads() {
    let source = "public class LocalBox { public Integer Read(Integer Input) {\
        Integer Result = Input + 1; return Result; } }";
    let result = output(source);
    let local = named(&result, "Result", EntityKind::Variable);
    assert_definition(&result, source, local);
    assert_child(&result, named(&result, "Read", EntityKind::Method), local);
    assert_eq!(
        text(source, local.evidence.source.as_ref().unwrap()),
        "Integer Result = Input + 1;"
    );
    assert_eq!(signatures(&result, source, local), ["Integer Result"]);
    let uses = lexical_uses(&result);
    assert_eq!(uses.len(), 2);
    assert!(uses.iter().any(|occurrence| {
        text(source, &occurrence.source) == "Result"
            && occurrence.target == (OccurrenceTarget::Resolved { symbol: local.id })
    }));
    assert_unqualified(&result);
    let formal = named(&result, "Input", EntityKind::Parameter);
    assert!(uses.iter().any(|occurrence| {
        text(source, &occurrence.source) == "Input"
            && occurrence.target == (OccurrenceTarget::Resolved { symbol: formal.id })
    }));
}

#[test]
fn apex_unrelated_block_local_does_not_suppress_a_parameter_read() {
    let source = "public class MixedBox { public Integer Read(Integer Value) {\
        Integer Other = 1; Consume(Other); return Value; } }";
    let result = output(source);
    let parameter = named(&result, "Value", EntityKind::Parameter);
    assert!(result.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Reference
            && text(source, &occurrence.source) == "Value"
            && occurrence.target
                == (OccurrenceTarget::Resolved {
                    symbol: parameter.id,
                })
    }));
    let local = named(&result, "Other", EntityKind::Variable);
    assert!(result.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Reference
            && text(source, &occurrence.source) == "Other"
            && occurrence.target == (OccurrenceTarget::Resolved { symbol: local.id })
    }));
    assert_unqualified(&result);
}

#[test]
fn apex_nested_blocks_keep_outer_bindings_and_separate_same_name_siblings() {
    let source = "public class BlockBox { public Integer Read(Integer Input) {\
        Integer Outer = Input;\
        { Integer Item = Outer; Consume(Item); Consume(Input); }\
        { Integer Item = Outer; Consume(Item); } Consume(Item); return Outer; } }";
    let result = output(source);
    assert_unqualified(&result);
    let siblings = result
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "Item")
        .collect::<Vec<_>>();
    assert_eq!(siblings.len(), 2);
    assert_ne!(siblings[0].id, siblings[1].id);
    assert_eq!(lexical_uses(&result).len(), 7);
    let outside = source.rfind("Item").unwrap();
    assert!(result.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Reference
            && usize::try_from(occurrence.source.span().start_byte()).unwrap() == outside
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    }));
    for sibling in siblings {
        assert_definition(&result, source, sibling);
        assert_eq!(signatures(&result, source, sibling), ["Integer Item"]);
        assert!(
            lexical_uses(&result)
                .iter()
                .any(|occurrence| occurrence.target
                    == (OccurrenceTarget::Resolved { symbol: sibling.id }))
        );
    }
}

#[test]
fn apex_local_reads_before_declaration_and_in_self_initializer_stay_unresolved() {
    let source = "public class OrderBox { public Integer Read(Integer Input) {\
        Consume(Ready); Integer Ready = Ready + Input; Consume(Ready); return Ready; } }";
    let result = output(source);
    assert_unqualified(&result);
    let local = named(&result, "Ready", EntityKind::Variable);
    let mut reads = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Reference
                && text(source, &occurrence.source) == "Ready"
        })
        .collect::<Vec<_>>();
    reads.sort_by_key(|occurrence| occurrence.source.span().start_byte());
    assert_eq!(reads.len(), 4);
    assert!(
        reads
            .iter()
            .take(2)
            .all(|occurrence| matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }))
    );
    assert!(
        reads.iter().skip(2).all(
            |occurrence| occurrence.target == (OccurrenceTarget::Resolved { symbol: local.id })
        )
    );
    assert_eq!(lexical_uses(&result).len(), 3);
}

#[test]
fn apex_local_groups_keep_conflicting_names_unresolved() {
    let source = "public class GroupBox { public Integer Read(Integer Input, Integer Kept) {\
        Integer Input = 1, Extra = 2; Consume(Input); return Kept; } }";
    let result = output(source);
    assert_unqualified(&result);
    for name in ["Input", "Extra"] {
        assert_definition(&result, source, named(&result, name, EntityKind::Variable));
    }
    let uses = lexical_uses(&result);
    assert_eq!(uses.len(), 1);
    assert_eq!(text(source, &uses[0].source), "Kept");
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "apex-shared-local-header-association-unavailable")
    );
}

#[test]
fn apex_local_groups_keep_separate_sources_and_sequential_initializer_reads() {
    let source = "public class GroupBox { public Integer Read(Integer Input) {\
        Integer First = Input, Second = First + 1; return Second; } }";
    let result = output(source);
    let method = named(&result, "Read", EntityKind::Method);
    let first = named(&result, "First", EntityKind::Variable);
    let second = named(&result, "Second", EntityKind::Variable);
    assert_ne!(first.id, second.id);
    for (local, witness) in [(first, "First = Input"), (second, "Second = First + 1")] {
        assert_definition(&result, source, local);
        assert_child(&result, method, local);
        assert_eq!(
            text(source, local.evidence.source.as_ref().unwrap()),
            witness
        );
        assert!(signatures(&result, source, local).is_empty());
        assert!(lexical_uses(&result).iter().any(|occurrence| {
            text(source, &occurrence.source) == local.canonical_name
                && occurrence.target == (OccurrenceTarget::Resolved { symbol: local.id })
        }));
    }
    assert_eq!(lexical_uses(&result).len(), 3);
    let gaps = result
        .document()
        .skipped_regions
        .iter()
        .filter(|gap| gap.detail == "apex-shared-local-header-association-unavailable")
        .collect::<Vec<_>>();
    assert!(!gaps.is_empty());
    for gap in gaps {
        assert_eq!(
            text(source, gap.evidence.source.as_ref().unwrap()),
            "Integer First = Input, Second = First + 1;"
        );
    }
    assert!(!result.document().skipped_regions.iter().any(|gap| {
        gap.detail == "apex-local-owner-and-binding-analysis-unavailable"
            || gap.detail == "signature-capture-unavailable"
                && gap.evidence.source.as_ref().is_some_and(|reference| {
                    ["First = Input", "Second = First + 1"].contains(&text(source, reference))
                })
    }));
    assert_unqualified(&result);
}

#[test]
fn apex_group_reads_before_declaration_and_self_initializers_stay_unresolved() {
    let source = "public class OrderBox { public Integer Read(Integer Input) {\
        Consume(First); Integer First = First + Second + Input, Second = First;\
        Consume(Second); return First; } }";
    let result = output(source);
    assert_unqualified(&result);
    let second_start = source.find("Second = First").unwrap();
    let references = result
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Reference
                && ["First", "Second"].contains(&text(source, &occurrence.source))
        })
        .collect::<Vec<_>>();
    assert_eq!(references.len(), 6);
    for reference in references {
        let position = usize::try_from(reference.source.span().start_byte()).unwrap();
        if position < second_start {
            assert!(matches!(
                reference.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        } else {
            let local = named(
                &result,
                text(source, &reference.source),
                EntityKind::Variable,
            );
            assert_eq!(
                reference.target,
                OccurrenceTarget::Resolved { symbol: local.id }
            );
        }
    }
    assert_eq!(lexical_uses(&result).len(), 4);
}

#[test]
fn apex_malformed_and_loop_local_groups_remain_explicit_gaps() {
    for body in [
        "Integer First = 1, = 2; return First;",
        "for (Integer First = 0, Second = 1; First < Second; First++) { Consume(First); } return 0;",
    ] {
        let source = format!("class Rejected {{ Integer Read() {{ {body} }} }}");
        let result = output(&source);
        assert_unqualified(&result);
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| { entity.kind == EntityKind::Variable })
        );
        assert!(lexical_uses(&result).is_empty());
        assert!(result.document().skipped_regions.iter().any(|gap| {
            gap.detail == "apex-local-owner-and-binding-analysis-unavailable"
                || gap.detail == "apex-source-identity-unavailable"
        }));
    }
}

#[test]
fn apex_local_groups_preserve_disjoint_scopes_and_reject_case_aliases() {
    for declaration in [
        "Integer Value = 1, Value = 2; return Value;",
        "Integer Value = 1, value = 2; return Value;",
    ] {
        let source = format!("class Aliases {{ Integer Read() {{ {declaration} }} }}");
        let result = output(&source);
        assert_unqualified(&result);
        assert_eq!(
            result
                .document()
                .entities
                .iter()
                .filter(|entity| { entity.kind == EntityKind::Variable })
                .count(),
            2
        );
        assert!(lexical_uses(&result).is_empty());
    }
    let source = "class Blocks { Integer Read(Integer Input) {\
        { Integer Item = Input, Next = Item; Consume(Next); }\
        { Integer Item = Input, Next = Item; Consume(Next); } return Item; } }";
    let result = output(source);
    assert_unqualified(&result);
    assert_eq!(lexical_uses(&result).len(), 6);
    let outside = source.rfind("Item").unwrap();
    assert!(result.document().occurrences.iter().any(|occurrence| {
        usize::try_from(occurrence.source.span().start_byte()).unwrap() == outside
            && matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
    }));
    for name in ["Item", "Next"] {
        let locals = result
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == name)
            .collect::<Vec<_>>();
        assert_eq!(locals.len(), 2);
        assert_ne!(locals[0].id, locals[1].id);
        for local in locals {
            assert_definition(&result, source, local);
            assert!(signatures(&result, source, local).is_empty());
        }
    }
}

#[test]
fn apex_local_group_initializer_edits_preserve_each_identity() {
    let source = "class Edits { Integer Read(Integer Input) {\
        Integer First = Input + 1, Second = First; return Second; } }";
    let changed_source = source.replace("Input + 1", "Input + 270");
    let before = output(source);
    let after = output(&changed_source);
    for name in ["First", "Second"] {
        let first = named(&before, name, EntityKind::Variable);
        let second = named(&after, name, EntityKind::Variable);
        assert_eq!(first.id, second.id);
        assert_definition(&after, &changed_source, second);
        assert_ne!(
            first.evidence.source.as_ref().unwrap().content_hash(),
            second.evidence.source.as_ref().unwrap().content_hash()
        );
    }
    assert_eq!(lexical_uses(&after).len(), 3);
    assert_unqualified(&after);
}

#[test]
fn apex_local_initializer_edits_keep_variable_identity_and_header() {
    let source = "public class EditBox { public Integer Read(Integer Input) {\
        Integer Result = Input + 1; return Result; } }";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, APEX);
    let fixture = Fixture::new(APEX, source.as_bytes());
    let budget = limits();
    let first = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, APEX, &budget),
        &ExtensionSupport::default(),
    );
    let bytes = source.replace("Input + 1", "Input + 27");
    let changed = fixture.rewrite(bytes.as_bytes());
    let next = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, APEX, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(next.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    let before = named(&first, "Result", EntityKind::Variable);
    let after = named(&next, "Result", EntityKind::Variable);
    assert_eq!(before.id, after.id);
    assert_eq!(signatures(&first, source, before), ["Integer Result"]);
    assert_eq!(signatures(&next, &bytes, after), ["Integer Result"]);
    assert_ne!(
        before.evidence.source.as_ref().unwrap().content_hash(),
        after.evidence.source.as_ref().unwrap().content_hash()
    );
    assert_ne!(fixture.source.generation(), changed.source.generation());
    for occurrence in lexical_uses(&next) {
        assert_eq!(occurrence.source.generation(), changed.source.generation());
        assert_eq!(
            occurrence.source.content_hash(),
            changed.source.content_hash()
        );
    }
    assert_unqualified(&next);
}

#[test]
fn apex_overlapping_same_name_local_scopes_and_case_aliases_stay_unresolved() {
    for body in [
        "Integer Value = 1; Integer Value = 2; return Value;",
        "Integer Value = 1; Integer value = 2; return Value;",
        "Integer Value = 1; { Integer value = 2; Consume(Value); } return Value;",
    ] {
        let source = format!("public class AliasBox {{ public Integer Read() {{ {body} }} }}");
        let result = output(&source);
        assert_unqualified(&result);
        let resolved = lexical_uses(&result);
        if body.contains('{') {
            assert_eq!(resolved.len(), 1);
            assert_eq!(
                usize::try_from(resolved[0].source.span().start_byte()).unwrap(),
                source.rfind("Value").unwrap()
            );
        } else {
            assert!(resolved.is_empty());
        }
    }
}
