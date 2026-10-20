//! Original Mojo obligations and finite written structural controls.
//! The original field Read and method Calls remain required independently.
use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/mojo/llama2.mojo");
const CASE: LanguageCase = LanguageCase {
    name: "mojo",
    path: "written.mojo",
    frontend: "mojo-native-written-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(CASE, bytes);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(CASE),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, CASE),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(bytes));
        assert_eq!(occurrence.source.repository(), fixture.source.repository());
        assert_eq!(occurrence.source.generation(), fixture.source.generation());
        assert_eq!(
            occurrence.source.span().file(),
            fixture.source.span().file()
        );
        assert!(occurrence.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
    }
    result
}

fn entity_at(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
) -> SymbolId {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(
        definitions.len(),
        1,
        "one exact {kind:?} definition: {name}@{start}..{end}"
    );
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("definition must resolve to its own materialized entity")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.language, "mojo");
    symbol
}

fn contains(
    document: &rootlight_ir::NormalizedIrDocument,
    owner: SymbolId,
    member: SymbolId,
    start: u64,
    end: u64,
) {
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == member)
        .unwrap();
    assert_eq!(
        entity.container,
        Some(rootlight_ir::ContainerRef::Entity(owner))
    );
    let relations: Vec<_> = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner)
                && relation.object == RelationEndpoint::Entity(member)
        })
        .collect();
    assert_eq!(relations.len(), 1);
    let source = relations[0]
        .evidence
        .source
        .as_ref()
        .expect("native ownership has exact source evidence");
    assert!(source.span().start_byte() <= start && source.span().end_byte() >= end);
}

#[test]
fn original_mojo_four_definitions_three_owners_and_source_negatives_are_exact() {
    assert_eq!(ORIGINAL.len(), 29_869);
    let result = output(ORIGINAL, EncodingId::utf8());
    let document = result.document();
    let matrix = entity_at(document, "Matrix", EntityKind::Struct, 587, 593);
    for (name, kind, start, end) in [
        ("data", EntityKind::Field, 613, 617),
        ("size", EntityKind::Method, 1633, 1637),
        ("num_elements", EntityKind::Method, 4150, 4162),
    ] {
        assert_eq!(
            &ORIGINAL[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()],
            name.as_bytes()
        );
        let member = entity_at(document, name, kind, start, end);
        contains(document, matrix, member, start, end);
    }
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 4198
                && site.source.span().end_byte() == 4202)
    );
    let size = entity_at(document, "size", EntityKind::Method, 1633, 1637);
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(size)
                && relation.object == RelationEndpoint::Entity(matrix))
    );
    assert_eq!(
        ORIGINAL
            .windows(b"Matrix".len())
            .filter(|part| *part == b"Matrix")
            .count(),
        38
    );
    assert!(!ORIGINAL.windows(b"__rootlight_absent_818cf8708e7d7cac12ac578634e9bc09bb4de53a78d5206ce8582465012512af__".len())
        .any(|part| part == b"__rootlight_absent_818cf8708e7d7cac12ac578634e9bc09bb4de53a78d5206ce8582465012512af__"));
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        assert!(
            document
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == domain)
        );
    }
}

#[test]
fn original_mojo_field_read_remains_an_applicable_resolved_requirement() {
    let result = output(ORIGINAL, EncodingId::utf8());
    let document = result.document();
    let field = entity_at(document, "data", EntityKind::Field, 613, 617);
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 2117
                && site.source.span().end_byte() == 2121
                && site.target == OccurrenceTarget::Resolved { symbol: field })
    );
}

#[test]
fn original_mojo_method_call_remains_an_applicable_exact_target_requirement() {
    let result = output(ORIGINAL, EncodingId::utf8());
    let document = result.document();
    let caller = entity_at(document, "num_elements", EntityKind::Method, 4150, 4162);
    let target = entity_at(document, "size", EntityKind::Method, 1633, 1637);
    assert!(document.relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.subject == RelationEndpoint::Entity(caller)
            && relation.object == RelationEndpoint::Entity(target)
            && relation.evidence.source.as_ref().is_some_and(|source| {
                source.span().start_byte() <= 4198 && source.span().end_byte() >= 4202
            })
    }));
}

#[test]
fn clean_literal_types_decorated_methods_and_formals_have_native_owners() {
    let source = "struct Written:\n    var value: Int\n    @always_inline\n    fn measure(self, input: Int) -> Int:\n        return input\n\nclass Other:\n    var field: Int\n\nfn plain(argument: Int) -> Int:\n    return argument\n";
    let result = output(source.as_bytes(), EncodingId::utf8());
    let document = result.document();
    let at = |name: &str, anchor: &str| {
        u64::try_from(source.find(anchor).unwrap() + anchor.find(name).unwrap()).unwrap()
    };
    let written = entity_at(document, "Written", EntityKind::Struct, 7, 14);
    let value_start = at("value", "var value");
    let value = entity_at(
        document,
        "value",
        EntityKind::Field,
        value_start,
        value_start + 5,
    );
    contains(document, written, value, value_start, value_start + 5);
    let method_start = at("measure", "fn measure");
    let method = entity_at(
        document,
        "measure",
        EntityKind::Method,
        method_start,
        method_start + 7,
    );
    contains(document, written, method, method_start, method_start + 7);
    let formal_start = at("input", "input: Int");
    let formal = entity_at(
        document,
        "input",
        EntityKind::Parameter,
        formal_start,
        formal_start + 5,
    );
    contains(document, method, formal, formal_start, formal_start + 5);
    let class_start = at("Other", "class Other");
    entity_at(
        document,
        "Other",
        EntityKind::Class,
        class_start,
        class_start + 5,
    );
    let function_start = at("plain", "fn plain");
    let function = entity_at(
        document,
        "plain",
        EntityKind::Function,
        function_start,
        function_start + 5,
    );
    let argument_start = at("argument", "plain(argument");
    let argument = entity_at(
        document,
        "argument",
        EntityKind::Parameter,
        argument_start,
        argument_start + 8,
    );
    contains(
        document,
        function,
        argument,
        argument_start,
        argument_start + 8,
    );
}

#[test]
fn field_roles_do_not_adopt_locals_selectors_or_conditional_declarations() {
    let source = "struct Written:\n    var field: Int\n    fn measure(self) -> Int:\n        var local = 1\n        return self.field\n\nif True:\n    struct Conditional:\n        var hidden: Int\n";
    let result = output(source.as_bytes(), EncodingId::utf8());
    let document = result.document();
    for name in ["local", "Conditional", "hidden"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.language == "mojo" && entity.canonical_name == name),
            "unproved owner cannot donate {name}"
        );
    }
    let use_start = u64::try_from(source.find("self.field").unwrap() + 5).unwrap();
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == use_start)
    );
}

#[test]
fn unrelated_recovery_does_not_erase_an_independent_clean_struct() {
    let source = "struct Written:\n    var field: Int\n\nfn broken( ->:\n";
    let result = output(source.as_bytes(), EncodingId::utf8());
    let owner = entity_at(result.document(), "Written", EntityKind::Struct, 7, 14);
    let member = entity_at(result.document(), "field", EntityKind::Field, 24, 29);
    contains(result.document(), owner, member, 24, 29);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "broken")
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::ParseError)
    );
}

#[test]
fn bom_crlf_and_both_utf16_orders_preserve_exact_physical_definitions() {
    let text = "struct Written:\r\n    var field: Int\r\n";
    let field_start = text.find("field").unwrap();
    let mut utf8 = b"\xef\xbb\xbf".to_vec();
    utf8.extend_from_slice(text.as_bytes());
    let result = output(&utf8, EncodingId::utf8());
    let start = u64::try_from(3 + field_start).unwrap();
    entity_at(
        result.document(),
        "field",
        EntityKind::Field,
        start,
        start + 5,
    );
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let encoding = EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap();
        let result = output(&raw, encoding);
        let start = u64::try_from(2 + 2 * field_start).unwrap();
        entity_at(
            result.document(),
            "field",
            EntityKind::Field,
            start,
            start + 10,
        );
    }
}

#[test]
fn exact_required_syntax_admits_structure_and_one_less_refuses_without_partial_commit() {
    let bytes = b"struct Written:\n    var field: Int\n";
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CASE);
    let fixture = Fixture::new(CASE, bytes);
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, CASE, &budget);
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 1);
    let exact = limits_with_syntax_records(required);
    let exact_request = request(&fixture.snapshot, &fixture.source, CASE, &exact);
    let (result, _) = analyzer
        .analyze_and_capture(
            &exact_request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let owner = entity_at(result.document(), "Written", EntityKind::Struct, 7, 14);
    let field = entity_at(result.document(), "field", EntityKind::Field, 24, 29);
    contains(result.document(), owner, field, 24, 29);
    let small = limits_with_syntax_records(required - 1);
    let error = analyzer
        .analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, CASE, &small),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err();
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit {
        resource: ResourceKind::RequiredSyntaxFacts, observed, limit,
    }) if observed == required && limit == required - 1),
        "{error:?}"
    );
    let cancelled = Cancellation::new();
    assert!(cancelled.cancel(CancellationReason::ClientRequest));
    assert!(matches!(
        analyzer.prepare_artifact(&initial, &cancelled),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

const SELF_MEMBERS: &str = "struct Written:\n    var data: List[Int]\n    fn size(self) -> Int:\n        return 1\n    fn read(self, index: Int) -> Int:\n        return self.data[index]\n    fn total(self) -> Int:\n        return self.size()\n";

fn assert_self_members(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
    mut physical: impl FnMut(usize) -> u64,
) {
    let at = |anchor: &str, name: &str| source.find(anchor).unwrap() + anchor.find(name).unwrap();
    let structure = entity_at(
        document,
        "Written",
        EntityKind::Struct,
        physical(at("struct Written", "Written")),
        physical(at("struct Written", "Written") + 7),
    );
    let field_start = at("var data", "data");
    let field = entity_at(
        document,
        "data",
        EntityKind::Field,
        physical(field_start),
        physical(field_start + 4),
    );
    let size_start = at("fn size", "size");
    let size = entity_at(
        document,
        "size",
        EntityKind::Method,
        physical(size_start),
        physical(size_start + 4),
    );
    let caller_start = at("fn total", "total");
    let caller = entity_at(
        document,
        "total",
        EntityKind::Method,
        physical(caller_start),
        physical(caller_start + 5),
    );
    for (member, start, len) in [
        (field, field_start, 4),
        (size, size_start, 4),
        (caller, caller_start, 5),
    ] {
        contains(
            document,
            structure,
            member,
            physical(start),
            physical(start + len),
        );
    }
    for (anchor, role, label, target) in [
        (
            "self.data",
            OccurrenceRole::Read,
            "mojo.self_field_read.reference",
            field,
        ),
        (
            "self.size",
            OccurrenceRole::CallSite,
            "mojo.self_method_call.call",
            size,
        ),
    ] {
        let start = source.rfind(anchor).unwrap() + 5;
        let end = start + anchor.len() - 5;
        let matches: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == physical(start)
                    && site.source.span().end_byte() == physical(end)
                    && site.role == role
            })
            .collect();
        assert_eq!(matches.len(), 1, "exact native {anchor} source occurrence");
        let site = matches[0];
        assert_eq!(site.syntax_kind, label);
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
        assert!(
            document
                .provenance
                .iter()
                .any(|item| item.id == site.provenance)
        );
        let predicate = if role == OccurrenceRole::Read {
            RelationPredicate::RefersTo
        } else {
            RelationPredicate::Calls
        };
        let subject = if role == OccurrenceRole::Read {
            RelationEndpoint::Occurrence(site.id)
        } else {
            RelationEndpoint::Entity(caller)
        };
        let relations: Vec<_> = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == predicate
                    && relation.subject == subject
                    && relation.object == RelationEndpoint::Entity(target)
                    && relation.evidence.source.as_ref() == Some(&site.source)
            })
            .collect();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].provenance, site.provenance);
        if role == OccurrenceRole::CallSite {
            for required in [
                rootlight_ir::FactRef::Entity(caller),
                rootlight_ir::FactRef::Fact(site.id),
                rootlight_ir::FactRef::Entity(target),
            ] {
                assert!(relations[0].evidence.derivation.contains(&required));
            }
        }
    }
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.domain == FactDomain::Relations),
        "two finite bindings do not erase the language's broader relationship gap"
    );
}

#[test]
fn actual_native_self_members_join_current_declarations_formals_and_exact_relations() {
    let result = output(SELF_MEMBERS.as_bytes(), EncodingId::utf8());
    assert_self_members(result.document(), SELF_MEMBERS, |byte| {
        u64::try_from(byte).unwrap()
    });
    let provider = provider();
    let fixture = Fixture::new(CASE, SELF_MEMBERS.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, CASE, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &provider,
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    for label in [
        "mojo.source_file.root",
        "mojo.struct.declaration",
        "mojo.field.declaration",
        "mojo.method.declaration",
        "mojo.parameter.declaration",
        "mojo.name.definition",
        "mojo.self_field_read.reference",
        "mojo.self_method_call.call",
        "mojo.self_field_target.signature",
        "mojo.self_method_target.signature",
        "mojo.self_formal_target.signature",
    ] {
        assert!(
            parsed
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == label),
            "actual native {label}"
        );
    }
    for (marker, target, use_anchor) in [
        ("mojo.self_field_target.signature", "var data", "self.data"),
        ("mojo.self_method_target.signature", "fn size", "self.size"),
    ] {
        let name = &use_anchor[5..];
        let start = SELF_MEMBERS.find(target).unwrap() + target.find(name).unwrap();
        let end = SELF_MEMBERS.rfind(use_anchor).unwrap() + use_anchor.len();
        assert!(
            parsed
                .facts()
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == marker
                    && fact.span().start_byte() == u64::try_from(start).unwrap()
                    && fact.span().end_byte() == u64::try_from(end).unwrap())
        );
    }
}

#[test]
fn original_member_certificates_keep_exact_field_method_and_owner_prerequisites() {
    let result = output(ORIGINAL, EncodingId::utf8());
    let document = result.document();
    let structure = entity_at(document, "Matrix", EntityKind::Struct, 587, 593);
    let field = entity_at(document, "data", EntityKind::Field, 613, 617);
    let method = entity_at(document, "size", EntityKind::Method, 1633, 1637);
    let caller = entity_at(document, "num_elements", EntityKind::Method, 4150, 4162);
    for (member, start, end) in [
        (field, 613, 617),
        (method, 1633, 1637),
        (caller, 4150, 4162),
    ] {
        contains(document, structure, member, start, end);
    }
    for (start, end, role, target, label) in [
        (
            2117,
            2121,
            OccurrenceRole::Read,
            field,
            "mojo.self_field_read.reference",
        ),
        (
            4198,
            4202,
            OccurrenceRole::CallSite,
            method,
            "mojo.self_method_call.call",
        ),
    ] {
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == end
                    && site.role == role
            })
            .collect();
        assert_eq!(sites.len(), 1);
        assert_eq!(
            sites[0].target,
            OccurrenceTarget::Resolved { symbol: target }
        );
        assert_eq!(sites[0].syntax_kind, label);
        assert_eq!(sites[0].source.content_hash(), content_hash(ORIGINAL));
    }
}

#[test]
fn complete_native_scope_barriers_leave_valid_written_members_unresolved() {
    for changed in [
        SELF_MEMBERS.replace("struct Written", "class Written"),
        SELF_MEMBERS.replace("fn total(self)", "fn total(other)"),
        SELF_MEMBERS.replace("fn size(self)", "fn size(self, extra: Int)"),
        SELF_MEMBERS.replace("self.size()", "self.size(1)"),
        SELF_MEMBERS.replace("self.size()", "self.size(1, 2)"),
        SELF_MEMBERS.replace(
            "        return self.size()",
            "        var self = other\n        return self.size()",
        ),
        SELF_MEMBERS.replace("    fn size", "    @staticmethod\n    fn size"),
    ] {
        let result = output(changed.as_bytes(), EncodingId::utf8());
        let start = u64::try_from(changed.rfind("self.size").unwrap() + 5).unwrap();
        let target_start = u64::try_from(changed.find("size(self").unwrap()).unwrap();
        entity_at(
            result.document(),
            "size",
            EntityKind::Method,
            target_start,
            target_start + 4,
        );
        let sites: Vec<_> = result
            .document()
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == start
                    && site.source.span().end_byte() == start + 4
                    && site.role != OccurrenceRole::Definition
            })
            .collect();
        assert!(
            !sites.is_empty(),
            "unproved selector remains an accounted occurrence: {changed}"
        );
        assert!(
            sites
                .iter()
                .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{changed}"
        );
        assert!(!result.document().relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Calls
                && relation
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().start_byte() == start)
        }));
    }
}

#[test]
fn actual_member_artifact_counts_match_native_admission_and_bounded_replay() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CASE);
    let fixture = Fixture::new(CASE, SELF_MEMBERS.as_bytes());
    let budget = limits();
    let initial = request(&fixture.snapshot, &fixture.source, CASE, &budget);
    let (full, artifact) = analyzer
        .analyze_and_capture(
            &initial,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_self_members(full.document(), SELF_MEMBERS, |byte| {
        u64::try_from(byte).unwrap()
    });
    let required = provider
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    assert!(
        artifact.syntax_fact_count() > required,
        "optional names are a discriminating bounded partition"
    );
    let exact_limits = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, CASE, &exact_limits);
    let (bounded, retained) = analyzer
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        retained.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    assert_eq!(
        provider
            .required_syntax_fact_count(&exact.to_parse_request(), &deadline())
            .unwrap(),
        required
    );
    assert!(!retained.syntax_partition_complete());
    assert_eq!(
        bounded.report().coverage().status(),
        CoverageStatus::Bounded
    );
    assert_self_members(bounded.document(), SELF_MEMBERS, |byte| {
        u64::try_from(byte).unwrap()
    });
    let replay = analyzer
        .analyze_from_artifact(
            &exact,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(bounded.document(), replay.document());
    let next = fixture.next_generation();
    let next_request = request(&next.snapshot, &next.source, CASE, &exact_limits);
    let rebound = analyzer
        .analyze_from_artifact(
            &next_request,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let fresh = analyze(&analyzer, &next_request, &ExtensionSupport::default());
    assert_eq!(rebound.document(), fresh.document());
    assert_ne!(bounded.document().generation, rebound.document().generation);
    assert_self_members(rebound.document(), SELF_MEMBERS, |byte| {
        u64::try_from(byte).unwrap()
    });
    let short = limits_with_syntax_records(required - 1);
    let Err(error) = analyzer.prepare_artifact(
        &request(&fixture.snapshot, &fixture.source, CASE, &short),
        &deadline(),
    ) else {
        panic!("one missing required fact refuses atomically");
    };
    assert!(
        matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: ResourceKind::RequiredSyntaxFacts, observed, limit })
        if observed == required && limit == required - 1),
        "{error:?}"
    );
    let changed = fixture.rewrite(SELF_MEMBERS.replace("return 1", "return 2").as_bytes());
    assert!(
        matches!(analyzer.analyze_from_artifact(&request(&changed.snapshot, &changed.source, CASE, &exact_limits), &retained,
        ExtensionSupport::default(), MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback, &deadline()),
        Err(AdapterError::ProviderFailed { code }) if code.as_str() == "treesitter-structural-artifact-mismatch")
    );
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.analyze_from_artifact(
            &exact,
            &retained,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancelled
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn self_member_bom_crlf_and_utf16_proofs_keep_physical_reference_and_relation_spans() {
    let text = format!("# \u{03bb}\r\n{}", SELF_MEMBERS.replace('\n', "\r\n"));
    let mut utf8 = b"\xef\xbb\xbf".to_vec();
    utf8.extend_from_slice(text.as_bytes());
    let result = output(&utf8, EncodingId::utf8());
    assert_self_members(result.document(), &text, |byte| {
        u64::try_from(3 + byte).unwrap()
    });
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let encoding = EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap();
        let result = output(&raw, encoding);
        assert_self_members(result.document(), &text, |byte| {
            u64::try_from(2 + 2 * text[..byte].encode_utf16().count()).unwrap()
        });
    }
}

#[test]
fn member_body_edits_keep_symbols_but_refresh_source_generation_and_relations() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, CASE);
    let fixture = Fixture::new(CASE, SELF_MEMBERS.as_bytes());
    let budget = limits();
    let before = analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, CASE, &budget),
        &ExtensionSupport::default(),
    );
    let edited = SELF_MEMBERS.replace("return 1", "return 2");
    let changed = fixture.rewrite(edited.as_bytes());
    let after = analyze(
        &analyzer,
        &request(&changed.snapshot, &changed.source, CASE, &budget),
        &ExtensionSupport::default(),
    );
    let ids = |document: &rootlight_ir::NormalizedIrDocument| {
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    assert_eq!(ids(before.document()), ids(after.document()));
    assert_ne!(before.document().generation, after.document().generation);
    assert_ne!(
        before.document().files[0].content_hash,
        after.document().files[0].content_hash
    );
    assert_self_members(before.document(), SELF_MEMBERS, |byte| {
        u64::try_from(byte).unwrap()
    });
    assert_self_members(after.document(), &edited, |byte| {
        u64::try_from(byte).unwrap()
    });
    for relation in after
        .document()
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Calls)
    {
        let source = relation.evidence.source.as_ref().unwrap();
        assert_eq!(source.generation(), changed.source.generation());
        assert_eq!(source.content_hash(), content_hash(edited.as_bytes()));
    }
}

#[test]
fn raw_distinct_id_own_witness_duplicates_cannot_reuse_materialized_member_bindings() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxFactKind, testkit::MockParseProvider};

    let native = provider();
    let fixture = Fixture::new(CASE, SELF_MEMBERS.as_bytes());
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, CASE, &budget);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts: Vec<SyntaxFact>| {
        let parser = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let analyzer = TreeSitterAnalyzer::new(
            Arc::new(parser),
            ProducerIdentity::new(
                "mojo-witness-replay",
                "1.0",
                content_hash(b"mojo-witness-replay"),
            )
            .unwrap(),
            language(CASE),
            CASE.frontend,
            content_hash(b"mojo-witness-replay-binary"),
        )
        .unwrap();
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap()
    };
    let baseline = replay(parsed.facts().to_vec());
    assert_self_members(baseline.document(), SELF_MEMBERS, |byte| {
        u64::try_from(byte).unwrap()
    });
    let identities = |document: &rootlight_ir::NormalizedIrDocument| {
        document
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>()
    };
    let structure = baseline
        .document()
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Struct && entity.canonical_name == "Written")
        .unwrap();
    for (label, target_name, caller_name) in [
        ("mojo.self_field_read.reference", "data", "read"),
        ("mojo.self_method_call.call", "size", "total"),
    ] {
        let site = baseline
            .document()
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == label)
            .unwrap();
        let OccurrenceTarget::Resolved { symbol: target_id } = site.target else {
            panic!("native endpoint prerequisite")
        };
        let target = baseline
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == target_id)
            .unwrap();
        assert_eq!(target.canonical_name, target_name);
        let caller = baseline
            .document()
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Method
                    && entity.canonical_name == caller_name
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(structure.id))
            })
            .unwrap();
        assert_eq!(site.enclosing, Some(caller.id));
        let caller_self = baseline
            .document()
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == "self"
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(caller.id))
            })
            .unwrap();
        let mut endpoints = vec![structure, target, caller, caller_self];
        if site.role == OccurrenceRole::CallSite {
            let target_self = baseline
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.canonical_name == "self"
                        && entity.container == Some(rootlight_ir::ContainerRef::Entity(target.id))
                })
                .unwrap();
            endpoints.push(target_self);
        }
        for endpoint in endpoints {
            let source = endpoint.evidence.source.as_ref().unwrap();
            let declarations: Vec<_> = parsed
                .facts()
                .iter()
                .filter(|fact| {
                    fact.kind() == SyntaxFactKind::Declaration && fact.span() == source.span()
                })
                .collect();
            assert_eq!(declarations.len(), 1, "unique actual native declaration");
            let declaration = declarations[0];
            let definitions: Vec<_> = parsed
                .facts()
                .iter()
                .filter(|fact| {
                    fact.kind() == SyntaxFactKind::Occurrence
                        && fact.syntax_kind().as_str() == "mojo.name.definition"
                        && fact.parent() == Some(declaration.local_id())
                })
                .collect();
            assert_eq!(definitions.len(), 1, "unique actual native definition");
            for witness in [declaration, definitions[0]] {
                let mut facts = parsed.facts().to_vec();
                let next = facts
                    .iter()
                    .map(SyntaxFact::local_id)
                    .max()
                    .unwrap()
                    .checked_add(1)
                    .unwrap();
                assert_ne!(next, witness.local_id());
                facts.push(SyntaxFact::new(
                    next,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                ));
                let changed = replay(facts);
                assert_eq!(
                    identities(changed.document()),
                    identities(baseline.document()),
                    "duplicate raw evidence must exercise the existing materialized entities"
                );
                let sites: Vec<_> = changed
                    .document()
                    .occurrences
                    .iter()
                    .filter(|item| {
                        item.syntax_kind == label && item.source.span() == site.source.span()
                    })
                    .collect();
                assert_eq!(sites.len(), 1);
                assert_eq!(sites[0].source, site.source);
                assert_eq!(sites[0].provenance, site.provenance);
                assert_eq!(sites[0].role, site.role);
                assert!(
                    matches!(sites[0].target, OccurrenceTarget::Unresolved { .. }),
                    "distinct-ID duplicate {:?} for {} must withhold {label}",
                    witness.kind(),
                    endpoint.canonical_name
                );
                assert!(!changed.document().relations.iter().any(|relation| {
                    matches!(
                        relation.predicate,
                        RelationPredicate::Calls | RelationPredicate::RefersTo
                    ) && relation.evidence.source.as_ref() == Some(&site.source)
                }));
                assert!(changed.document().skipped_regions.iter().any(|gap| {
                    gap.domain == FactDomain::Relations
                        && gap.source == site.source
                        && gap.reason == SkippedRegionReason::UnsupportedConstruct
                        && gap.detail == "mojo-native-own-struct-self-member-binding-unavailable"
                }));
            }
        }
    }
}
