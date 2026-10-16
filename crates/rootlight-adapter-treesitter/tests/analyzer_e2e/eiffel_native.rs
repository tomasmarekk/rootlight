//! Eiffel written ownership remains separate from inherited or dynamic binding.

use super::*;

const EIFFEL: LanguageCase = LanguageCase {
    name: "eiffel",
    path: "src/written.e",
    frontend: "tree-sitter-eiffel-1.0.0",
    source: "class BOX [G -> ANY]\ninherit PARENT\ncreate make\nfeature\n count, capacity: INTEGER\n answer: INTEGER = 42\n item: G\n make (value: G)\n  local temporary: G\n  do\n   temporary := value\n   target.update (value)\n  end\nend\n",
    generated: false,
    body_before: "temporary := value",
    body_after: "temporary := item",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(EIFFEL, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, EIFFEL),
        &request(&fixture.snapshot, &fixture.source, EIFFEL, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &document.occurrences {
        let span = occurrence.source.span();
        let bytes = &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        assert_eq!(occurrence.syntactic_text_hash, content_hash(bytes));
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(occurrence.source.repository(), document.repository);
        assert_eq!(occurrence.source.generation(), document.generation);
        if occurrence.role != OccurrenceRole::Definition {
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
    }
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    result
}

#[test]
fn eiffel_class_generic_features_and_bindings_have_exact_source_names() {
    let result = output(EIFFEL.source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind) in [
        ("BOX", EntityKind::Class),
        ("G", EntityKind::TypeParameter),
        ("count", EntityKind::Field),
        ("capacity", EntityKind::Field),
        ("answer", EntityKind::Constant),
        ("item", EntityKind::Field),
        ("make", EntityKind::Function),
        ("value", EntityKind::Parameter),
        ("temporary", EntityKind::Variable),
    ] {
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name && entity.kind == kind),
            "missing {name} {kind:?}: {:?}; gaps {:?}",
            document
                .entities
                .iter()
                .map(|entity| (&entity.canonical_name, entity.kind))
                .collect::<Vec<_>>(),
            document.skipped_regions
        );
    }
    for occurrence in document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
    {
        let OccurrenceTarget::Resolved { symbol } = occurrence.target else {
            panic!("unresolved definition")
        };
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        let span = occurrence.source.span();
        assert_eq!(
            &EIFFEL.source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            entity.canonical_name
        );
    }
    assert!(document.occurrences.iter().any(|occurrence| occurrence.role
        == OccurrenceRole::CallSite
        && &EIFFEL.source[usize::try_from(occurrence.source.span().start_byte()).unwrap()
            ..usize::try_from(occurrence.source.span().end_byte()).unwrap()]
            == "update"));
    for (owner, child) in [
        ("BOX", "G"),
        ("BOX", "count"),
        ("BOX", "capacity"),
        ("BOX", "answer"),
        ("BOX", "item"),
        ("BOX", "make"),
        ("make", "value"),
        ("make", "temporary"),
    ] {
        let parent = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner)
            .unwrap();
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == child)
            .unwrap();
        let containers: Vec<_> = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(entity.id)
            })
            .collect();
        assert_eq!(containers.len(), 1, "ambiguous owner of {child}");
        assert_eq!(
            containers[0].subject,
            RelationEndpoint::Entity(parent.id),
            "wrong owner of {child}"
        );
    }
}

#[test]
fn eiffel_literal_decoys_and_known_verbatim_gaps_do_not_invent_owners() {
    let source = "class ACTUAL\nfeature\n -- class COMMENT_FAKE feature forged: INTEGER end\n text: STRING = \"class STRING_FAKE feature invented: INTEGER end\"\n block: STRING = \"[\n class VERBATIM_FAKE feature phantom: INTEGER end\n ]\"\n tagged: STRING = \"tag[\nclass TAG_FAKE feature false_name: INTEGER end\n]tag\"\n tail: INTEGER\nend\n";
    let result = output(source);
    for forbidden in [
        "COMMENT_FAKE",
        "STRING_FAKE",
        "VERBATIM_FAKE",
        "TAG_FAKE",
        "forged",
        "invented",
        "phantom",
        "false_name",
    ] {
        assert!(
            !result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == forbidden)
        );
    }
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "tail")
    );
    assert!(result.document().skipped_regions.iter().any(|gap| gap.detail == "eiffel-tagged-and-braced-verbatim-classification-unavailable"));
}

#[test]
fn eiffel_body_changes_preserve_written_identity_and_error_recovery_keeps_clean_heads() {
    let before = output(EIFFEL.source);
    let after = output(&EIFFEL.source.replace(EIFFEL.body_before, EIFFEL.body_after));
    let identities = |output: &AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .map(|entity| (entity.canonical_name.clone(), entity.id))
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(identities(&before), identities(&after));
    let recovered = output("class RECOVERED feature stable: INTEGER = 42 @ tail: INTEGER end");
    assert!(
        recovered
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
    );
    for name in ["RECOVERED", "stable", "tail"] {
        assert!(
            recovered
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "missing recovered {name}"
        );
    }
}

#[test]
fn eiffel_native_literals_preserve_following_definitions_without_recovery() {
    let result = output(
        "class LITERALS feature number: INTEGER = 0xFF character: CHARACTER_32 = '%/0xFFFD/' space: CHARACTER = ' ' tail: INTEGER end",
    );
    assert!(
        result
            .document()
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "syntax-error-recovery")
    );
    for name in ["LITERALS", "number", "character", "space", "tail"] {
        assert!(
            result
                .document()
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "missing {name}"
        );
    }
}

#[test]
fn eiffel_unproven_class_owners_do_not_promote_features_into_the_file() {
    for source in [
        "class OPEN feature kept: INTEGER",
        "class 123 feature kept: INTEGER end",
    ] {
        let result = output(source);
        assert!(
            result
                .document()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "syntax-error-recovery")
        );
        assert!(
            result
                .document()
                .entities
                .iter()
                .all(|entity| entity.canonical_name != "kept"),
            "unproven class retained a promoted feature: {:?}",
            result.document().relations
        );
    }
}

#[test]
fn eiffel_crlf_unicode_text_keeps_definition_spans_and_unresolved_calls() {
    let result = output(
        "class ENCODED\r\nfeature\r\n text: STRING = \"λ😀\"\r\n run (value: INTEGER) do target.call (value) end\r\nend\r\n",
    );
    assert!(
        result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "run")
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "eiffel-feature-dispatch-target-unavailable")
    );
}

#[test]
fn eiffel_declaration_semicolons_preserve_exact_definitions_and_class_ownership() {
    let source = "class SEPARATED\r\nfeature\r\n count, capacity: INTEGER;;\r\n answer: INTEGER = 42;\r\n text: STRING = \"; λ😀\";\r\n run do end;;\r\n tail: INTEGER;\r\nend\r\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "SEPARATED")
        .unwrap();
    for (name, kind) in [
        ("count", EntityKind::Field),
        ("capacity", EntityKind::Field),
        ("answer", EntityKind::Constant),
        ("text", EntityKind::Constant),
        ("run", EntityKind::Function),
        ("tail", EntityKind::Field),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap();
        assert!(
            document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner.id)
                    && relation.object == RelationEndpoint::Entity(entity.id))
        );
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "definition of {name}");
        let span = definitions[0].source.span();
        assert_eq!(
            &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name.as_bytes()
        );
    }
}

#[test]
fn eiffel_keyword_case_keeps_written_names_and_exact_class_ownership() {
    let source = "cLaSs MiXeD\r\nfEaTuRe\r\n Flag: BOOLEAN = tRuE\r\n Other: BOOLEAN = fAlSe\r\n classify: STRING = \"class EnD TRUE λ😀\"\r\n Run do target.call (1) end\r\n end_marker: INTEGER\r\neNd\r\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "MiXeD" && entity.kind == EntityKind::Class)
        .unwrap();
    for (name, kind) in [
        ("Flag", EntityKind::Constant),
        ("Other", EntityKind::Constant),
        ("classify", EntityKind::Constant),
        ("Run", EntityKind::Function),
        ("end_marker", EntityKind::Field),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap();
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(entity.id)
        }));
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "definition of {name}");
        let span = definitions[0].source.span();
        assert_eq!(
            &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name.as_bytes()
        );
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| { gap.detail == "eiffel-feature-dispatch-target-unavailable" })
    );
    assert!(document.occurrences.iter().all(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            || !matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
    }));
}

#[test]
fn eiffel_combined_header_and_aliases_preserve_exact_definitions_and_owners() {
    let source = "frozen expanded class ORDERED\r\ninherit PARENT rename old_less as parent_less alias \"<=\" alias \"≤\" end\r\nfeature\r\n less alias \"<=\" alias \"≤\" (other: like Current): BOOLEAN do Result := True end\r\n tail: INTEGER\r\nend\r\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "ORDERED" && entity.kind == EntityKind::Class)
        .unwrap();
    let less = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "less" && entity.kind == EntityKind::Function)
        .unwrap();
    for (name, kind, parent) in [
        ("less", EntityKind::Function, owner.id),
        ("other", EntityKind::Parameter, less.id),
        ("tail", EntityKind::Field, owner.id),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap();
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent)
                && relation.object == RelationEndpoint::Entity(entity.id)
        }));
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "definition of {name}");
        let span = definitions[0].source.span();
        assert_eq!(
            &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name.as_bytes()
        );
    }
    assert!(document.entities.iter().all(|entity| {
        !["<=", "≤", "parent_less", "old_less"].contains(&entity.canonical_name.as_str())
    }));
}

#[test]
fn eiffel_note_sections_preserve_class_owners_and_exclude_metadata_names() {
    let source = "note names: alpha, beta; description: \"class FAKE feature invented: INTEGER end λ😀\";\r\nclass BOX\r\nfeature\r\n run note description: \"routine metadata\"; do end\r\n tail: INTEGER\r\nnote names: closing; description: \"class metadata\";\r\nend\r\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "BOX" && entity.kind == EntityKind::Class)
        .unwrap();
    for (name, kind) in [("run", EntityKind::Function), ("tail", EntityKind::Field)] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap();
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(entity.id)
        }));
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "definition of {name}");
        let span = definitions[0].source.span();
        assert_eq!(
            &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name.as_bytes()
        );
    }
    assert!(document.entities.iter().all(|entity| {
        ![
            "alpha",
            "beta",
            "closing",
            "description",
            "FAKE",
            "invented",
        ]
        .contains(&entity.canonical_name.as_str())
    }));
    let malformed = "note names: ; class BOX feature tail: INTEGER end";
    let recovered = output(malformed);
    let recovered = recovered.document();
    let diagnostic = recovered
        .diagnostics
        .iter()
        .find(|diagnostic| diagnostic.code == "syntax-error-recovery")
        .expect("an invisible missing note value must retain recovery evidence");
    let source = diagnostic.source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(malformed.as_bytes()));
    assert!(source.span().start_byte() <= source.span().end_byte());
    assert!(source.span().end_byte() <= u64::try_from(malformed.find(';').unwrap()).unwrap());
    let owner = recovered
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "BOX" && entity.kind == EntityKind::Class)
        .unwrap();
    let tail = recovered
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "tail" && entity.kind == EntityKind::Field)
        .unwrap();
    assert!(recovered.relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Contains
            && relation.subject == RelationEndpoint::Entity(owner.id)
            && relation.object == RelationEndpoint::Entity(tail.id)
    }));
}

#[test]
fn eiffel_type_marks_unique_values_and_addresses_keep_written_owners() {
    let source = "class MARKED [G -> detachable separate ANY]\r\nfeature\r\n first, second: INTEGER = unique;\r\n item: detachable separate G\r\n pair: attached separate TUPLE [label: STRING]\r\n anchor: detachable separate like Current\r\n value: expanded VALUE\r\n address: POINTER do consume ($Result) consume ($Current) end\r\n tail: INTEGER\r\nend\r\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "MARKED" && entity.kind == EntityKind::Class)
        .unwrap();
    for (name, kind) in [
        ("G", EntityKind::TypeParameter),
        ("first", EntityKind::Constant),
        ("second", EntityKind::Constant),
        ("item", EntityKind::Field),
        ("pair", EntityKind::Field),
        ("anchor", EntityKind::Field),
        ("value", EntityKind::Field),
        ("address", EntityKind::Function),
        ("tail", EntityKind::Field),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap_or_else(|| panic!("missing written definition of {name}"));
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(entity.id)
        }));
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "definition of {name}");
        let span = definitions[0].source.span();
        assert_eq!(
            &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name.as_bytes()
        );
    }
    assert!(document.entities.iter().all(|entity| {
        !["unique", "Current", "Result", "label"].contains(&entity.canonical_name.as_str())
    }));
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .count(),
        2
    );
}

#[test]
fn eiffel_list_separators_keep_parameters_locals_and_written_owners() {
    let source = "note ;; title: \"Lists λ😀\";;;\r\nclass BOX\r\ninherit ;; PARENT export ;; {ANY} item;;; {NONE} hidden;; end;;; OTHER;;\r\nfeature ;;\r\n run (;; first: INTEGER;;; second: INTEGER;;)\r\n require ;; ready: True;;;\r\n local ;; temporary: INTEGER;;; extra: INTEGER;;\r\n do temporary := first check ;; valid: True;;; end\r\n ensure ;; done: True;;; end;;;\r\n pair: TUPLE [;; left: INTEGER;;; right: INTEGER;;]\r\n tail: INTEGER\r\ninvariant ;; valid: True;;;\r\nend\r\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for (name, kind, owner_name) in [
        ("run", EntityKind::Function, "BOX"),
        ("pair", EntityKind::Field, "BOX"),
        ("tail", EntityKind::Field, "BOX"),
        ("first", EntityKind::Parameter, "run"),
        ("second", EntityKind::Parameter, "run"),
        ("temporary", EntityKind::Variable, "run"),
        ("extra", EntityKind::Variable, "run"),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap_or_else(|| panic!("missing written definition of {name}"));
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner_name)
            .unwrap();
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(entity.id)
        }));
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1, "definition of {name}");
        let span = definitions[0].source.span();
        assert_eq!(
            &source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name.as_bytes()
        );
    }
    assert!(document.entities.iter().all(|entity| {
        ![
            "title", "left", "right", "ready", "valid", "done", "item", "hidden",
        ]
        .contains(&entity.canonical_name.as_str())
    }));
}

#[test]
fn eiffel_inline_separate_aliases_keep_exact_names_and_written_owners() {
    let source = "class BOX\r\nfeature\r\n run do -- λ😀\r\n separate target as held, other.item as second do\r\n  separate held.item as nested do nested.use (second) end\r\n end\r\n separate target as held do held.use (1) end\r\n end\r\n tail: INTEGER\r\nend\r\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let routine = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "run" && entity.kind == EntityKind::Function)
        .unwrap();
    let aliases: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable)
        .collect();
    assert_eq!(aliases.len(), 4);
    let mut actual = Vec::new();
    for entity in aliases {
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(routine.id)
                && relation.object == RelationEndpoint::Entity(entity.id)
        }));
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        let start = usize::try_from(span.start_byte()).unwrap();
        let end = usize::try_from(span.end_byte()).unwrap();
        assert_eq!(&source[start..end], entity.canonical_name);
        actual.push((start, end, entity.canonical_name.as_str()));
    }
    actual.sort_unstable();
    let expected: Vec<_> = ["held", "second", "nested", "held"]
        .into_iter()
        .zip(source.match_indices(" as ").map(|(offset, _)| offset + 4))
        .map(|(name, offset)| (offset, offset + name.len(), name))
        .collect();
    assert_eq!(actual, expected);
    assert!(document.entities.iter().all(|entity| {
        !["target", "other", "item", "use"].contains(&entity.canonical_name.as_str())
    }));
    assert_eq!(
        document
            .skipped_regions
            .iter()
            .filter(|gap| { gap.detail == "eiffel-separate-region-binding-unavailable" })
            .count(),
        3
    );
    let malformed = output("class BOX feature run do separate target as held do held.use (1) end");
    assert!(!malformed.document().diagnostics.is_empty());
    assert!(
        malformed
            .document()
            .entities
            .iter()
            .all(|entity| entity.canonical_name != "held")
    );
    let anonymous = output(
        "class BOX feature run do consume (agent do separate target as hidden do hidden.use (1) end end) end tail: INTEGER end",
    );
    assert!(
        anonymous.document().diagnostics.is_empty(),
        "{:?}",
        anonymous.document().diagnostics
    );
    assert!(
        anonymous
            .document()
            .entities
            .iter()
            .all(|entity| entity.canonical_name != "hidden")
    );
}

#[test]
fn eiffel_real_exponents_preserve_constant_definitions_and_recovery() {
    let mut source = "class REALS\r\nfeature -- λ😀\r\n".to_owned();
    for (index, literal) in [
        "-4.2E+7",
        "1.0E-10",
        "1.0e6",
        ".3e-10",
        "1.E3",
        "1_2.3_4E+5_6",
        "12.0",
        "12.",
        ".5",
    ]
    .into_iter()
    .enumerate()
    {
        source.push_str(&format!(" value_{index}: REAL = {literal}\r\n"));
    }
    source.push_str(" tail: INTEGER\r\nend\r\n");
    let result = output(&source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "REALS" && entity.kind == EntityKind::Class)
        .unwrap();
    for (name, kind) in (0..9)
        .map(|index| (format!("value_{index}"), EntityKind::Constant))
        .chain([(String::from("tail"), EntityKind::Field)])
    {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap_or_else(|| panic!("missing {name}"));
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(entity.id)
        }));
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
    }
    assert!(document.entities.iter().all(|entity| {
        entity.kind == EntityKind::Module
            || entity.canonical_name == "REALS"
            || entity.canonical_name == "tail"
            || entity.canonical_name.starts_with("value_")
    }));
    let malformed = "class BOX feature value: REAL = 1.0e+ 5 tail: INTEGER end";
    let recovered = output(malformed);
    assert!(recovered.document().diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "syntax-error-recovery"
            && diagnostic
                .source
                .as_ref()
                .is_some_and(|source| source.content_hash() == content_hash(malformed.as_bytes()))
    }));
    assert!(
        recovered
            .document()
            .entities
            .iter()
            .any(|entity| { entity.canonical_name == "tail" && entity.kind == EntityKind::Field })
    );
}

#[test]
fn eiffel_formal_representation_marks_keep_exact_parameters_and_explicit_binding_gaps() {
    let source = "class BOX [expanded G -> ANY, ReFeReNcE H, frozen K]\r\nfeature -- λ😀\r\n item: G\r\n other: H\r\n reference: INTEGER\r\n tail: INTEGER\r\nend\r\n";
    let result = output(source);
    let document = result.document();
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "BOX" && entity.kind == EntityKind::Class)
        .unwrap();
    for (name, kind) in [
        ("G", EntityKind::TypeParameter),
        ("H", EntityKind::TypeParameter),
        ("K", EntityKind::TypeParameter),
        ("item", EntityKind::Field),
        ("other", EntityKind::Field),
        ("reference", EntityKind::Field),
        ("tail", EntityKind::Field),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name && entity.kind == kind)
            .unwrap_or_else(|| panic!("missing {name}"));
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner.id)
                && relation.object == RelationEndpoint::Entity(entity.id)
        }));
        let definitions: Vec<_> = document
            .occurrences
            .iter()
            .filter(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
            })
            .collect();
        assert_eq!(definitions.len(), 1);
        let span = definitions[0].source.span();
        assert_eq!(
            &source[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            name
        );
    }
    assert!(document.entities.iter().all(|entity| {
        !["expanded", "ReFeReNcE", "frozen", "ANY"].contains(&entity.canonical_name.as_str())
    }));
    assert_eq!(
        document
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail
                == "eiffel-generic-constraint-and-representation-binding-unavailable")
            .count(),
        3
    );
    let identifier = output("class BOX [reference] feature item: reference end");
    assert!(identifier.document().diagnostics.is_empty());
    assert!(
        identifier
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "reference"
                && entity.kind == EntityKind::TypeParameter)
    );
    let malformed = output("class BOX [expanded] feature tail: INTEGER end");
    assert!(!malformed.document().diagnostics.is_empty());
    assert!(
        malformed
            .document()
            .entities
            .iter()
            .all(|entity| entity.kind != EntityKind::TypeParameter)
    );
}

#[test]
fn eiffel_written_operators_preserve_comment_spans_and_unresolved_dispatch() {
    let source = "class BOX\nfeature\n run (left, right: INTEGER): INTEGER\n  do\n   Result := left ∋-- first comment\n right\n   Result := left +-- second comment\n right\n   Result := left #abc-- third comment\n right\n   Result := left -* right\n  end\n tail: INTEGER\nend\n";
    let result = output(source);
    let document = result.document();
    let mut operators: Vec<_> = document
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.syntax_kind == "eiffel.written_operator.call")
        .collect();
    operators.sort_by_key(|occurrence| occurrence.source.span().start_byte());
    assert_eq!(
        operators.len(),
        3,
        "missing exact written operator occurrences: {:?}; gaps {:?}",
        document
            .occurrences
            .iter()
            .map(|site| (&site.syntax_kind, site.role))
            .collect::<Vec<_>>(),
        document
            .skipped_regions
            .iter()
            .map(|gap| (&gap.detail, gap.source.span()))
            .collect::<Vec<_>>()
    );
    for (occurrence, spelling) in operators.iter().zip(["∋", "#abc", "-*"]) {
        let start = source.find(spelling).unwrap();
        assert_eq!(occurrence.role, OccurrenceRole::CallSite);
        assert!(matches!(
            occurrence.target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert_eq!(occurrence.source.span().start_byte(), start as u64);
        assert_eq!(
            occurrence.source.span().end_byte(),
            (start + spelling.len()) as u64
        );
        for detail in [
            "eiffel-operator-dispatch-target-unavailable",
            "eiffel-operator-alias-binding-and-dispatch-unavailable",
        ] {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| { gap.detail == detail && gap.source == occurrence.source }),
                "missing source-bound {detail} for {spelling}"
            );
        }
    }
    assert!(
        document.diagnostics.is_empty(),
        "{:?}",
        document.diagnostics
    );
    for comment in [
        "-- first comment\n",
        "-- second comment\n",
        "-- third comment\n",
    ] {
        let start = source.find(comment).unwrap();
        assert!(
            document.occurrences.iter().any(|occurrence| {
                occurrence.role == OccurrenceRole::Documentation
                    && occurrence.source.span().start_byte() == start as u64
                    && occurrence.source.span().end_byte() == (start + comment.len()) as u64
            }),
            "comment span changed: {comment:?}"
        );
    }
    for (owner, name) in [
        ("BOX", "run"),
        ("BOX", "tail"),
        ("run", "left"),
        ("run", "right"),
    ] {
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == owner)
            .unwrap();
        let child = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        let owners: Vec<_> = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(child.id)
            })
            .collect();
        assert_eq!(owners.len(), 1, "ambiguous owner of {name}");
        assert_eq!(owners[0].subject, RelationEndpoint::Entity(owner.id));
    }
}

#[test]
fn eiffel_encoded_operator_and_comment_spans_bind_to_original_bytes() {
    let source = "\u{feff}class BOX\r\nfeature -- marker λ😀\r\n run (left, right: INTEGER): INTEGER\r\n do\r\n Result := left ∋-- operator comment\r\n right\r\n end\r\n tail: INTEGER\r\nend\r\n";
    let provider = Arc::new(provider());
    let budget = limits();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                text.as_bytes().to_vec()
            } else {
                text.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            }
        };
        let raw = encode(source);
        let fixture = Fixture::new(EIFFEL, &raw);
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(EIFFEL),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(
            &analyzer(&provider, EIFFEL),
            &request,
            &ExtensionSupport::default(),
        );
        let document = result.document();
        validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
        assert!(
            document.diagnostics.is_empty(),
            "{encoding}: {:?}",
            document.diagnostics
        );
        for (spelling, role, label) in [
            (
                "∋",
                OccurrenceRole::CallSite,
                "eiffel.written_operator.call",
            ),
            (
                "-- operator comment\r\n",
                OccurrenceRole::Documentation,
                "eiffel.comment.comment",
            ),
        ] {
            let start = source.find(spelling).unwrap();
            let physical_start = encode(&source[..start]).len();
            let physical_end = physical_start + encode(spelling).len();
            let occurrences: Vec<_> = document
                .occurrences
                .iter()
                .filter(|occurrence| occurrence.syntax_kind == label)
                .collect();
            let occurrence = occurrences
                .iter()
                .find(|occurrence| occurrence.source.span().start_byte() == physical_start as u64)
                .unwrap_or_else(|| panic!("missing {encoding} {spelling:?}"));
            assert_eq!(occurrence.role, role);
            assert_eq!(occurrence.source.span().end_byte(), physical_end as u64);
            assert_eq!(occurrence.source.content_hash(), content_hash(&raw));
            assert_eq!(&raw[physical_start..physical_end], encode(spelling));
            // Occurrence identity uses decoded text; SourceRef authenticates raw bytes.
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(spelling.as_bytes())
            );
            assert_eq!(occurrence.source.repository(), document.repository);
            assert_eq!(occurrence.source.generation(), document.generation);
            assert!(matches!(
                occurrence.target,
                OccurrenceTarget::Unresolved { .. }
            ));
            if role == OccurrenceRole::CallSite {
                assert!(document.skipped_regions.iter().any(|gap| gap.detail
                    == "eiffel-operator-dispatch-target-unavailable"
                    && gap.source == occurrence.source));
            }
        }
    }
}

#[test]
fn eiffel_legacy_shift_and_arrow_calls_keep_exact_unresolved_spans() {
    for spelling in ["|<<", "|>>", "#abc->"] {
        let source = format!(
            "class BOX feature run (left, right: INTEGER): INTEGER do Result := left {spelling}--ignored\n right end tail: INTEGER end"
        );
        let result = output(&source);
        let document = result.document();
        let operators: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "eiffel.written_operator.call")
            .collect();
        assert_eq!(operators.len(), 1, "{spelling}");
        let start = source.find(spelling).unwrap();
        assert_eq!(operators[0].source.span().start_byte(), start as u64);
        assert_eq!(
            operators[0].source.span().end_byte(),
            (start + spelling.len()) as u64,
            "{spelling}"
        );
        assert!(matches!(
            operators[0].target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(
            document.diagnostics.is_empty(),
            "{spelling}: {:?}",
            document.diagnostics
        );
        assert!(document.skipped_regions.iter().any(|gap| gap.detail
            == "eiffel-operator-dispatch-target-unavailable"
            && gap.source == operators[0].source));
    }
}
