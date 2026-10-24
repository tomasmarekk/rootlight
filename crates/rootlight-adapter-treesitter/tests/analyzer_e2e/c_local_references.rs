//! Ordinary local definitions and address References retain physical source proof.

use super::*;

const ORIGINAL: &[u8] = include_bytes!("../fixtures/c_local_references/test_outlier_detector.c");

fn case() -> LanguageCase {
    *CASES.iter().find(|case| case.name == "c").unwrap()
}

fn output(bytes: &[u8], encoding: EncodingId) -> AnalysisOutput {
    let case = case();
    let native = Arc::new(provider());
    let fixture = Fixture::new(case, bytes);
    let bounds = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(case),
        encoding,
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &bounds,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&native, case),
        &input,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
    for site in &result.document().occurrences {
        assert_eq!(site.source.content_hash(), content_hash(bytes));
        assert_eq!(site.source.repository(), fixture.source.repository());
        assert_eq!(site.source.generation(), fixture.source.generation());
        assert_eq!(site.source.span().file(), fixture.source.span().file());
    }
    result
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
    name: &str,
    kind: EntityKind,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact written definition for {name}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written definition")
    };
    let entity = document.entities.iter().find(|e| e.id == symbol).unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.provenance, sites[0].provenance);
    symbol
}

fn address(
    document: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
    target: SymbolId,
    owner: SymbolId,
) {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && site.syntax_kind == "c.local_address.reference"
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .collect();
    assert_eq!(sites.len(), 1, "one address Reference at {start}..{end}");
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert_eq!(sites[0].enclosing, Some(owner));
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
                && edge.object == RelationEndpoint::Entity(target)
        })
        .collect();
    assert_eq!(edges.len(), 1);
    assert_eq!(edges[0].evidence.source.as_ref(), Some(&sites[0].source));
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Read
            && site.source.span().start_byte() == start
            && site.source.span().end_byte() == end
    }));
}

fn no_address(document: &rootlight_ir::NormalizedIrDocument) {
    assert!(!document.occurrences.iter().any(|site| {
        site.syntax_kind == "c.local_address.reference"
            && matches!(site.target, OccurrenceTarget::Resolved { .. })
    }));
}

#[test]
fn complete_original_keeps_main_local_contains_and_exact_address_reference() {
    assert_eq!(ORIGINAL.len(), 1778);
    assert_eq!(ORIGINAL.windows(2).filter(|b| *b == b"\r\n").count(), 52);
    for (start, end, name) in [
        (256, 260, b"main".as_slice()),
        (290, 298, b"detector"),
        (320, 328, b"detector"),
        (1732, 1738, b"printf"),
    ] {
        assert_eq!(&ORIGINAL[start..end], name);
    }
    let result = output(ORIGINAL, EncodingId::utf8());
    let document = result.document();
    let main = definition(document, 256, 260, "main", EntityKind::Function);
    let detector = definition(document, 290, 298, "detector", EntityKind::Variable);
    let local = document
        .entities
        .iter()
        .find(|entity| entity.id == detector)
        .unwrap();
    assert_eq!(
        local.container,
        Some(rootlight_ir::ContainerRef::Entity(main))
    );
    let contains: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(main)
                && edge.object == RelationEndpoint::Entity(detector)
        })
        .collect();
    assert_eq!(contains.len(), 1);
    let source = contains[0].evidence.source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(ORIGINAL));
    assert!(source.span().start_byte() <= 290 && source.span().end_byte() >= 298);
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(detector)
            && edge.object == RelationEndpoint::Entity(main)
    }));
    address(document, 320, 328, detector, main);
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 1732
            && site.source.span().end_byte() == 1738
    }));
    assert!(!document.entities.iter().any(|e| matches!(
        e.canonical_name.as_str(),
        "OutlierDetector" | "init_detector" | "update_detector"
    )));
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn ordinary_names_keep_initialized_reads_separate_from_uninitialized_address_uses() {
    let text = "int owned(void){ External local; use(&local); int ready=7; use(ready); ready += 1; use(&ready); return local; }\n";
    let result = output(text.as_bytes(), EncodingId::utf8());
    let document = result.document();
    let owner = definition(document, 4, 9, "owned", EntityKind::Function);
    let local_at = text.find("local;").unwrap() as u64;
    let local = definition(
        document,
        local_at,
        local_at + 5,
        "local",
        EntityKind::Variable,
    );
    let ready_at = text.find("ready=7").unwrap() as u64;
    let ready = definition(
        document,
        ready_at,
        ready_at + 5,
        "ready",
        EntityKind::Variable,
    );
    let address_at = text.find("&local").unwrap() as u64 + 1;
    address(document, address_at, address_at + 5, local, owner);
    let initialized_address = text.find("&ready").unwrap() as u64 + 1;
    address(
        document,
        initialized_address,
        initialized_address + 5,
        ready,
        owner,
    );
    let value_at = text.find("use(ready)").unwrap() as u64 + 4;
    let values: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read && site.source.span().start_byte() == value_at
        })
        .collect();
    assert_eq!(values.len(), 1);
    assert_eq!(
        values[0].target,
        OccurrenceTarget::Resolved { symbol: ready }
    );
    for at in [text.find("ready +=").unwrap(), text.rfind("local").unwrap()] {
        assert!(
            !document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == at as u64)
        );
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::Calls)
    );
}

#[test]
fn nearest_plain_shadow_and_for_initializer_have_distinct_source_owned_targets() {
    let text = "int owned(void){ int local; use(&local); { int local; use(&local); } use(&local); for(int index=0;index<1;++index){ use(&index); } use(&index); }\n";
    let result = output(text.as_bytes(), EncodingId::utf8());
    let document = result.document();
    let owner = definition(document, 4, 9, "owned", EntityKind::Function);
    let first_at = text.find("local;").unwrap();
    let second_at = text.match_indices("local;").nth(1).unwrap().0;
    assert_ne!(first_at, second_at);
    assert_eq!(&text[first_at..first_at + 5], "local");
    assert_eq!(&text[second_at..second_at + 5], "local");
    let first = first_at as u64;
    let second = second_at as u64;
    let outer = definition(document, first, first + 5, "local", EntityKind::Variable);
    let inner = definition(document, second, second + 5, "local", EntityKind::Variable);
    assert_ne!(outer, inner);
    let local_uses: Vec<_> = text
        .match_indices("&local")
        .map(|(at, _)| at as u64 + 1)
        .collect();
    for (at, target) in local_uses.into_iter().zip([outer, inner, outer]) {
        address(document, at, at + 5, target, owner);
    }
    let at = text.find("index=0").unwrap() as u64;
    let index = definition(document, at, at + 5, "index", EntityKind::Variable);
    let inside = text.find("&index").unwrap() as u64 + 1;
    address(document, inside, inside + 5, index, owner);
    let outside = text.rfind("&index").unwrap() as u64 + 1;
    assert!(
        !document
            .occurrences
            .iter()
            .any(|site| site.source.span().start_byte() == outside
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn duplicate_parameter_macro_recovery_and_complex_declarators_withhold_address_proof() {
    for text in [
        "int owned(void){ int local; int local; use(&local); }\n",
        "int owned(int local){ int local; use(&local); }\n",
        "int owned(int *other){ int local; use(&local); }\n",
        "#define local other\nint owned(void){ int local; use(&local); }\n",
        "#if FLAG\nint owned(void){ int local; use(&local); }\n#endif\n",
        "typedef int External; int owned(void){ External local; use(&local); }\n",
        "int owned(void){ int local[2]; use(&local); }\n",
        "int owned(void){ int *local; use(&local); }\n",
        "int owned(void){ int local, other; use(&local); }\n",
        "int local; int owned(void){ use(&local); }\n",
        "int owned(void){ int local; use(&local); } broken @\n",
        "int owned(void){ int local; return sizeof(use(&local)); }\n",
        "int owned(void){ int local; int other=use(&local); return other; }\n",
    ] {
        let result = output(text.as_bytes(), EncodingId::utf8());
        no_address(result.document());
    }
}

#[test]
fn selected_and_split_input_never_acquire_complete_file_address_authority() {
    let case = case();
    let native = Arc::new(provider());
    let frontend = analyzer(&native, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let bounds = limits();
    for selected in [vec![(252, 1776)], vec![(0, 299), (300, 1778)]] {
        let ranges = selected
            .into_iter()
            .map(|(start, end)| {
                IncludedRange::new(
                    SourceSpan::new(fixture.source.span().file(), start, end).unwrap(),
                    language(case),
                )
            })
            .collect();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::utf8(),
            ranges,
            AnalysisTier::TierD,
            build_context(),
            &bounds,
        )
        .unwrap()
        .with_generated_status(false);
        let result = analyze(&frontend, &input, &ExtensionSupport::default());
        no_address(result.document());
    }
}

#[test]
fn complete_original_address_uses_exact_bom_and_utf16_physical_coordinates() {
    let mut bom = b"\xef\xbb\xbf".to_vec();
    bom.extend_from_slice(ORIGINAL);
    let result = output(&bom, EncodingId::utf8());
    let main = definition(result.document(), 259, 263, "main", EntityKind::Function);
    let detector = definition(
        result.document(),
        293,
        301,
        "detector",
        EntityKind::Variable,
    );
    address(result.document(), 323, 331, detector, main);
    for little in [true, false] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in std::str::from_utf8(ORIGINAL).unwrap().encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let result = output(
            &bytes,
            EncodingId::new(if little { "utf-16le" } else { "utf-16be" }).unwrap(),
        );
        let main = definition(result.document(), 514, 522, "main", EntityKind::Function);
        let detector = definition(
            result.document(),
            582,
            598,
            "detector",
            EntityKind::Variable,
        );
        address(result.document(), 642, 658, detector, main);
    }
}

#[test]
fn materialization_requires_unique_root_context_owner_target_and_name_witnesses() {
    use rootlight_adapter_sdk::{SyntaxFact, testkit::MockParseProvider};
    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts| {
        let mock = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(mock),
            ProducerIdentity::new(
                "c-local-address-replay",
                "1",
                content_hash(b"c-local-address-replay"),
            )
            .unwrap(),
            language(case),
            case.frontend,
            content_hash(b"c-local-address-binary"),
        )
        .unwrap();
        execute_analysis(
            &frontend,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap()
    };
    let baseline = replay(parsed.facts().to_vec());
    let main = definition(baseline.document(), 256, 260, "main", EntityKind::Function);
    let detector = definition(
        baseline.document(),
        290,
        298,
        "detector",
        EntityKind::Variable,
    );
    address(baseline.document(), 320, 328, detector, main);
    for (label, start, end, duplicate) in [
        ("c.variable_file_context.signature", 0, 1778, false),
        ("c.local_address_target.signature", 274, 328, false),
        ("c.local_address_target.signature", 274, 328, true),
        ("c.variable.declaration", 274, 299, true),
        ("c.variable_name.definition", 290, 298, true),
        ("c.function.declaration", 252, 1776, true),
        ("c.local_address.reference", 320, 328, true),
    ] {
        let mut facts = parsed.facts().to_vec();
        let matches: Vec<_> = facts
            .iter()
            .filter(|fact| {
                fact.syntax_kind().as_str() == label
                    && fact.span().start_byte() == start
                    && fact.span().end_byte() == end
            })
            .cloned()
            .collect();
        assert_eq!(matches.len(), 1, "one native witness {label}");
        if duplicate {
            let fact = &matches[0];
            let id = facts.iter().map(|fact| fact.local_id()).max().unwrap() + 1;
            facts.push(SyntaxFact::new(
                id,
                fact.parent(),
                fact.kind(),
                fact.span(),
                fact.depth(),
                fact.syntax_kind().clone(),
            ));
        } else {
            facts.retain(|fact| fact.local_id() != matches[0].local_id());
        }
        let result = replay(facts);
        no_address(result.document());
        assert!(
            result
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "c-native-written-variable-binding-unavailable")
        );
    }
}

fn refuse_substituted_function_definition(
    source: &[u8],
    owner_name: (u64, u64),
    substituted_name: (u64, u64),
    address_site: (u64, u64),
) {
    use rootlight_adapter_sdk::{SyntaxFact, testkit::MockParseProvider};
    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, source);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay = |facts| {
        let mock = MockParseProvider::new(
            native.capabilities().clone(),
            facts,
            parsed.diagnostics().to_vec(),
            parsed.report().coverage().clone(),
        );
        let frontend = TreeSitterAnalyzer::new(
            Arc::new(mock),
            ProducerIdentity::new(
                "c-local-address-owner-name",
                "1",
                content_hash(b"c-local-address-owner-name"),
            )
            .unwrap(),
            language(case),
            case.frontend,
            content_hash(b"c-local-address-owner-name-binary"),
        )
        .unwrap();
        let result = execute_analysis(
            &frontend,
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        validate_ir_document(result.document(), bounds.ir(), &ExtensionSupport::default()).unwrap();
        result
    };
    let baseline = replay(parsed.facts().to_vec());
    let start = usize::try_from(owner_name.0).unwrap();
    let end = usize::try_from(owner_name.1).unwrap();
    let name = std::str::from_utf8(&source[start..end]).unwrap();
    let owner = definition(
        baseline.document(),
        owner_name.0,
        owner_name.1,
        name,
        EntityKind::Function,
    );
    let original_site = baseline
        .document()
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "c.local_address.reference"
                && site.source.span().start_byte() == address_site.0
                && site.source.span().end_byte() == address_site.1
        })
        .unwrap();
    let OccurrenceTarget::Resolved { symbol } = original_site.target else {
        panic!("untouched native address must be certified")
    };
    address(
        baseline.document(),
        address_site.0,
        address_site.1,
        symbol,
        owner,
    );
    let mut facts = parsed.facts().to_vec();
    let matches: Vec<_> = facts
        .iter()
        .enumerate()
        .filter(|(_, fact)| {
            fact.syntax_kind().as_str() == "c.identifier.definition"
                && fact.span().start_byte() == owner_name.0
                && fact.span().end_byte() == owner_name.1
        })
        .map(|(index, _)| index)
        .collect();
    assert_eq!(matches.len(), 1, "one actual native function name");
    let index = matches[0];
    let original = &facts[index];
    assert_eq!(
        original.kind(),
        rootlight_adapter_sdk::SyntaxFactKind::Occurrence
    );
    facts[index] = SyntaxFact::new(
        original.local_id(),
        original.parent(),
        original.kind(),
        rootlight_ir::SourceSpan::new(
            original.span().file(),
            substituted_name.0,
            substituted_name.1,
        )
        .unwrap(),
        original.depth(),
        original.syntax_kind().clone(),
    );
    let forged = replay(facts);
    no_address(forged.document());
    let sites: Vec<_> = forged
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "c.local_address.reference"
                && site.source.span().start_byte() == address_site.0
                && site.source.span().end_byte() == address_site.1
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(sites[0].role, OccurrenceRole::Reference);
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(sites[0].source.content_hash(), content_hash(source));
    assert!(!forged.document().relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::RefersTo
            && edge.subject == RelationEndpoint::Occurrence(sites[0].id)
    }));
    assert!(
        forged
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "c-native-written-variable-binding-unavailable")
    );
}

#[test]
fn original_address_refuses_a_function_definition_moved_to_a_different_body_name() {
    assert_eq!(&ORIGINAL[256..260], b"main");
    assert_eq!(&ORIGINAL[543..547], b"rand");
    refuse_substituted_function_definition(ORIGINAL, (256, 260), (543, 547), (320, 328));
}

#[test]
fn equal_owner_spelling_cannot_replace_its_native_function_name_geometry() {
    let source = b"void owner(void) { int item; consume(&item); owner(); }\n";
    assert_eq!(&source[5..10], b"owner");
    assert_eq!(&source[45..50], b"owner");
    refuse_substituted_function_definition(source, (5, 10), (45, 50), (38, 42));
}

#[test]
fn copied_producer_certificates_cannot_forge_an_address_in_changed_current_source() {
    use rootlight_adapter_sdk::testkit::MockParseProvider;
    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let mut changed = ORIGINAL.to_vec();
    assert_eq!(changed[319], b'&');
    changed[319] = b' ';
    let current = fixture.rewrite(&changed);
    let current_input = request(&current.snapshot, &current.source, case, &bounds);
    let mock = MockParseProvider::new(
        native.capabilities().clone(),
        parsed.facts().to_vec(),
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let frontend = TreeSitterAnalyzer::new(
        Arc::new(mock),
        ProducerIdentity::new(
            "c-local-address-current-source",
            "1",
            content_hash(b"c-local-address-current-source"),
        )
        .unwrap(),
        language(case),
        case.frontend,
        content_hash(b"c-local-address-binary"),
    )
    .unwrap();
    let result = execute_analysis(
        &frontend,
        &current_input,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    no_address(result.document());
    let forged: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "c.local_address.reference"
                && site.source.span().start_byte() == 320
        })
        .collect();
    assert_eq!(forged.len(), 1);
    assert!(matches!(
        forged[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(forged[0].source.content_hash(), content_hash(&changed));
}

#[test]
fn changing_address_labels_to_value_read_cannot_initialize_the_bare_declaration() {
    use rootlight_adapter_sdk::{SyntaxFact, SyntaxKindLabel, testkit::MockParseProvider};
    let native = provider();
    let case = case();
    let fixture = Fixture::new(case, ORIGINAL);
    let bounds = limits();
    let input = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &native,
        &input.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let mut changed = 0;
    let facts = parsed
        .facts()
        .iter()
        .map(|fact| {
            let replacement = match fact.syntax_kind().as_str() {
                "c.local_address.reference" => "c.written_variable.reference",
                "c.local_address_target.signature" => "c.variable_target.signature",
                _ => return fact.clone(),
            };
            changed += 1;
            SyntaxFact::new(
                fact.local_id(),
                fact.parent(),
                fact.kind(),
                fact.span(),
                fact.depth(),
                SyntaxKindLabel::new(replacement).unwrap(),
            )
        })
        .collect();
    assert_eq!(changed, 2);
    let mock = MockParseProvider::new(
        native.capabilities().clone(),
        facts,
        parsed.diagnostics().to_vec(),
        parsed.report().coverage().clone(),
    );
    let frontend = TreeSitterAnalyzer::new(
        Arc::new(mock),
        ProducerIdentity::new(
            "c-local-address-role",
            "1",
            content_hash(b"c-local-address-role"),
        )
        .unwrap(),
        language(case),
        case.frontend,
        content_hash(b"c-local-address-binary"),
    )
    .unwrap();
    let result = execute_analysis(
        &frontend,
        &input,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let sites: Vec<_> = result
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Read && site.source.span().start_byte() == 320)
        .collect();
    assert_eq!(sites.len(), 1);
    assert!(matches!(
        sites[0].target,
        OccurrenceTarget::Unresolved { .. }
    ));
}

#[test]
fn exact_required_admission_and_real_artifact_replay_keep_original_address_proof() {
    let native = Arc::new(provider());
    let case = case();
    let frontend = analyzer(&native, case);
    let fixture = Fixture::new(case, ORIGINAL);
    let wide = limits();
    let initial = request(&fixture.snapshot, &fixture.source, case, &wide);
    let required = native
        .required_syntax_fact_count(&initial.to_parse_request(), &deadline())
        .unwrap();
    assert!(required > 4);
    let bounds = limits_with_syntax_records(required);
    let exact = request(&fixture.snapshot, &fixture.source, case, &bounds);
    let (result, artifact) = frontend
        .analyze_and_capture(
            &exact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let main = definition(result.document(), 256, 260, "main", EntityKind::Function);
    let detector = definition(
        result.document(),
        290,
        298,
        "detector",
        EntityKind::Variable,
    );
    address(result.document(), 320, 328, detector, main);
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        required
    );
    let rebound = frontend
        .analyze_from_artifact(
            &exact,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(rebound.document(), result.document());
    let next = fixture.next_generation();
    let next_input = request(&next.snapshot, &next.source, case, &bounds);
    let rebound = frontend
        .analyze_from_artifact(
            &next_input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        rebound.document(),
        analyze(&frontend, &next_input, &ExtensionSupport::default()).document()
    );
    let mut bytes = ORIGINAL.to_vec();
    bytes[319] = b' ';
    let changed = fixture.rewrite(&bytes);
    assert!(
        frontend
            .analyze_from_artifact(
                &request(&changed.snapshot, &changed.source, case, &bounds),
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
    let low = limits_with_syntax_records(required - 1);
    let small = request(&fixture.snapshot, &fixture.source, case, &low);
    for error in [
        frontend.prepare_artifact(&small, &deadline()).unwrap_err(),
        rootlight_adapter_sdk::execute_parse(
            native.as_ref(),
            &small.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap_err(),
    ] {
        assert!(
            matches!(error,AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,observed,limit }) if observed == required && limit == required-1),
            "{error:?}"
        );
    }
    let stopped = Cancellation::new();
    stopped.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        frontend.analyze_from_artifact(
            &exact,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &stopped
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn address_relations_and_records_use_the_real_exact_ir_admission_boundary() {
    let native = Arc::new(provider());
    let case = case();
    let frontend = analyzer(&native, case);
    let fixture = Fixture::new(case, b"int owned(void){ int local; use(&local); }\n");
    let base = limits();
    let run = |ir| {
        let bounds = AnalysisLimits::new(
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
        frontend
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, case, &bounds),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
            .map(|(output, _)| output)
    };
    let baseline = run(IrLimits::default()).unwrap();
    let owner = definition(baseline.document(), 4, 9, "owned", EntityKind::Function);
    let target = definition(baseline.document(), 21, 26, "local", EntityKind::Variable);
    address(baseline.document(), 33, 38, target, owner);
    for relations in [true, false] {
        let defaults = IrLimits::default();
        let mut low = 0;
        let mut high = if relations {
            defaults.max_relations
        } else {
            defaults.max_total_records
        };
        let set = |cap| {
            let mut ir = IrLimits::default();
            if relations {
                ir.max_relations = cap;
            } else {
                ir.max_total_records = cap;
            }
            ir
        };
        while low < high {
            let middle = low + (high - low) / 2;
            match run(set(middle)) {
                Ok(_) => high = middle,
                Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => low = middle + 1,
                Err(error) => panic!("unexpected admission failure: {error:?}"),
            }
        }
        assert!(low > 0);
        assert_eq!(run(set(low)).unwrap().document(), baseline.document());
        let error = run(set(low - 1)).unwrap_err();
        assert!(
            matches!(error,AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::Records,observed,limit }) if observed > limit && limit == low-1),
            "{error:?}"
        );
    }
}

#[test]
fn same_owner_initialized_and_plain_shadows_keep_separate_exact_address_targets() {
    let text = "int owned(void){ int local=1; use(&local); { int local; use(&local); } use(&local); return local; }\n";
    let result = output(text.as_bytes(), EncodingId::utf8());
    let document = result.document();
    let owner = definition(document, 4, 9, "owned", EntityKind::Function);
    let first = text.find("local=1").unwrap() as u64;
    let second = text.find("local;").unwrap() as u64;
    let outer = definition(document, first, first + 5, "local", EntityKind::Variable);
    let inner = definition(document, second, second + 5, "local", EntityKind::Variable);
    assert_ne!(outer, inner);
    let uses: Vec<_> = text
        .match_indices("&local")
        .map(|(at, _)| at as u64 + 1)
        .collect();
    assert_eq!(uses.len(), 3);
    for (at, target) in uses.into_iter().zip([outer, inner, outer]) {
        address(document, at, at + 5, target, owner);
    }
    let at = text.rfind("local").unwrap() as u64;
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == at
                && site.source.span().end_byte() == at + 5
        })
        .collect();
    assert_eq!(reads.len(), 1);
    assert_eq!(
        reads[0].target,
        OccurrenceTarget::Resolved { symbol: outer }
    );
    assert_eq!(reads[0].enclosing, Some(owner));
}

#[test]
fn sibling_plain_locals_with_equal_names_have_distinct_address_targets() {
    let text =
        "int owned(void){ { int local; use(&local); } { int local; use(&local); } return 0; }\n";
    let result = output(text.as_bytes(), EncodingId::utf8());
    let document = result.document();
    let owner = definition(document, 4, 9, "owned", EntityKind::Function);
    let definitions: Vec<_> = text
        .match_indices("local;")
        .map(|(at, _)| at as u64)
        .collect();
    assert_eq!(definitions.len(), 2);
    let first = definition(
        document,
        definitions[0],
        definitions[0] + 5,
        "local",
        EntityKind::Variable,
    );
    let second = definition(
        document,
        definitions[1],
        definitions[1] + 5,
        "local",
        EntityKind::Variable,
    );
    assert_ne!(first, second);
    let uses: Vec<_> = text
        .match_indices("&local")
        .map(|(at, _)| at as u64 + 1)
        .collect();
    assert_eq!(uses.len(), 2);
    for (at, target) in uses.into_iter().zip([first, second]) {
        address(document, at, at + 5, target, owner);
    }
}

#[test]
fn local_declaration_ids_survive_value_offset_and_unrelated_scope_edits() {
    let variants = [
        "int owned(void){ int local=1; use(&local); { int local; use(&local); } return 0; }\n",
        "/* shifted */\nint owned(void){ { int unrelated; use(&unrelated); } int local=77; use(&local); { int local; use(&local); } return 0; }\n",
    ];
    let mut identities = Vec::new();
    for text in variants {
        let result = output(text.as_bytes(), EncodingId::utf8());
        let document = result.document();
        let owner_at = text.find("owned").unwrap() as u64;
        let owner = definition(
            document,
            owner_at,
            owner_at + 5,
            "owned",
            EntityKind::Function,
        );
        let outer_at = text.find("local=").unwrap() as u64;
        let inner_at = text.find("local;").unwrap() as u64;
        let outer = definition(
            document,
            outer_at,
            outer_at + 5,
            "local",
            EntityKind::Variable,
        );
        let inner = definition(
            document,
            inner_at,
            inner_at + 5,
            "local",
            EntityKind::Variable,
        );
        assert_ne!(outer, inner);
        let uses: Vec<_> = text
            .match_indices("&local")
            .map(|(at, _)| at as u64 + 1)
            .collect();
        assert_eq!(uses.len(), 2);
        for (at, target) in uses.into_iter().zip([outer, inner]) {
            address(document, at, at + 5, target, owner);
        }
        identities.push((owner, outer, inner));
    }
    assert_eq!(identities[0], identities[1]);
}
