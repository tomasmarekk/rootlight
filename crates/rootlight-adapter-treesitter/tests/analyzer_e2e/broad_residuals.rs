//! Full original witnesses distinguish source structure from runtime execution.
//! Native observations record the uncertain boundary before any Bash/C++ repair.

use super::*;
use rootlight_adapter_sdk::execute_parse;

const BASH: &[u8] = include_bytes!("../fixtures/written_residuals/bash/run");
const CPP: &[u8] = include_bytes!("../fixtures/written_residuals/cpp/study.cpp");
const PYTHON: &[u8] = include_bytes!("../fixtures/written_residuals/python/query_client.py");

fn case(language: &'static str) -> LanguageCase {
    LanguageCase {
        name: language,
        path: match language {
            "bash" => "written.sh",
            "cpp" => "written.cpp",
            _ => "written.py",
        },
        frontend: "written-source-regression",
        source: "",
        generated: false,
        body_before: "",
        body_after: "",
    }
}

fn output(language: &'static str, bytes: &[u8]) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let case = case(language);
    let fixture = Fixture::new(case, bytes);
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files[0].content_hash, content_hash(bytes));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(bytes.len()).unwrap()
    );
    result
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
    start: u64,
    end: u64,
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
    assert_eq!(sites.len(), 1, "one written {kind:?} {name}@{start}..{end}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("a written definition resolves to its own source entity")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.kind, kind);
    symbol
}

// Distinct written declarations can share one symbol but retain different
// containment evidence. Select the requested token without discarding any proof.
pub(super) fn source_anchored_contains(
    document: &rootlight_ir::NormalizedIrDocument,
    owner: SymbolId,
    child: SymbolId,
    start: u64,
    end: u64,
) -> Vec<&rootlight_ir::RelationRecord> {
    let definitions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
                && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == child)
        })
        .collect();
    assert_eq!(definitions.len(), 1);
    let selected = definitions[0];
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.id == child)
            .count(),
        1
    );
    let edges: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(owner)
                && edge.object == RelationEndpoint::Entity(child)
        })
        .collect();
    let mut ids = BTreeSet::new();
    let mut anchors = BTreeSet::new();
    for edge in &edges {
        assert!(ids.insert(edge.id), "duplicate containment fact identity");
        assert_eq!(
            rootlight_ir::derive_relation_record_id(edge).unwrap(),
            edge.id
        );
        let source = edge.evidence.source.as_ref().expect("containment source");
        assert!(
            anchors.insert(source.clone()),
            "duplicate containment source anchor"
        );
        assert_eq!(edge.repository, selected.source.repository());
        assert_eq!(edge.generation, selected.source.generation());
        assert_eq!(source.span().file(), selected.source.span().file());
        assert_eq!(
            *source,
            SourceRef::new(
                selected.source.repository(),
                selected.source.generation(),
                source.span(),
                selected.source.content_hash(),
                selected.source.line_hint(),
            )
        );
        assert_eq!(edge.provenance, selected.provenance);
        assert_eq!(edge.evidence_kind, rootlight_ir::EvidenceKind::Syntax);
        assert!(edge.evidence.derivation.is_empty());
        let provenance = document
            .provenance
            .iter()
            .find(|value| value.id == edge.provenance)
            .unwrap();
        assert_eq!(provenance.repository, source.repository());
        assert_eq!(provenance.generation, source.generation());
        assert!(provenance.input_sources.iter().any(|input| {
            input.repository() == source.repository()
                && input.generation() == source.generation()
                && input.content_hash() == source.content_hash()
                && input.span().file() == source.span().file()
                && input.span().start_byte() <= source.span().start_byte()
                && input.span().end_byte() >= source.span().end_byte()
        }));
        let anchored_definitions = document.occurrences.iter().filter(|site| {
            site.role == OccurrenceRole::Definition
                && matches!(site.target, OccurrenceTarget::Resolved { symbol } if symbol == child)
                && site.provenance == edge.provenance
                && site.source.repository() == source.repository()
                && site.source.generation() == source.generation()
                && site.source.content_hash() == source.content_hash()
                && site.source.span().file() == source.span().file()
                && source.span().start_byte() <= site.source.span().start_byte()
                && source.span().end_byte() >= site.source.span().end_byte()
        }).count();
        assert_eq!(
            anchored_definitions, 1,
            "one written definition per containment anchor"
        );
    }
    edges
        .into_iter()
        .filter(|edge| {
            let source = edge.evidence.source.as_ref().unwrap();
            source.span().start_byte() <= start && source.span().end_byte() >= end
        })
        .collect()
}

fn contains(
    document: &rootlight_ir::NormalizedIrDocument,
    owner: SymbolId,
    child: SymbolId,
    start: u64,
    end: u64,
) {
    let edges = source_anchored_contains(document, owner, child, start, end);
    assert_eq!(edges.len(), 1);
    assert_eq!(
        document
            .entities
            .iter()
            .find(|entity| entity.id == child)
            .unwrap()
            .container,
        Some(rootlight_ir::ContainerRef::Entity(owner))
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
fn original_bash_named_function_owns_its_written_local_assignment() {
    assert_eq!(BASH.len(), 3598);
    assert_eq!(&BASH[2987..2997], b"select_vim");
    assert_eq!(&BASH[3011..3014], b"vim");
    let result = output("bash", BASH);
    let document = result.document();
    let owner = definition(document, "select_vim", EntityKind::Function, 2987, 2997);
    let child = definition(document, "vim", EntityKind::Variable, 3011, 3014);
    contains(document, owner, child, 3011, 3014);
}

#[test]
fn original_cpp_named_function_owns_its_ordinary_named_formal() {
    assert_eq!(CPP.len(), 9196);
    assert_eq!(&CPP[703..723], b"GetUserAuthorization");
    assert_eq!(&CPP[744..746], b"sc");
    let result = output("cpp", CPP);
    let document = result.document();
    let owner = definition(
        document,
        "GetUserAuthorization",
        EntityKind::Function,
        703,
        723,
    );
    let child = definition(document, "sc", EntityKind::Parameter, 744, 746);
    contains(document, owner, child, 744, 746);
}

#[test]
fn original_python_module_assignment_keeps_its_exact_variable_definition() {
    assert_eq!(PYTHON.len(), 1245);
    assert_eq!(&PYTHON[61..65], b"sock");
    assert_eq!(&PYTHON[95..104], b"localhost");
    let result = output("python", PYTHON);
    definition(result.document(), "sock", EntityKind::Variable, 61, 65);
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 95
                && site.source.span().end_byte() == 104)
    );
}

fn observe(
    language: &'static str,
    bytes: &[u8],
    native: tree_sitter::Language,
    spans: &[(usize, usize)],
) {
    let mut parser = tree_sitter::Parser::new();
    parser.set_language(&native).unwrap();
    let tree = parser.parse(bytes, None).unwrap();
    let root = tree.root_node();
    println!(
        "native {language} bytes={} root={} {}..{} error={} missing={}",
        bytes.len(),
        root.kind(),
        root.start_byte(),
        root.end_byte(),
        root.has_error(),
        root.is_missing()
    );
    assert_eq!(root.end_byte(), bytes.len());
    for &(start, end) in spans {
        println!(
            "selected {language} {start}..{end} {:?}",
            &bytes[start..end]
        );
        let mut current = root.descendant_for_byte_range(start, end);
        while let Some(node) = current {
            println!(
                "ancestor {} {}..{} error={} missing={}",
                node.kind(),
                node.start_byte(),
                node.end_byte(),
                node.has_error(),
                node.is_missing()
            );
            for field in ["name", "body", "declarator", "type", "left", "right"] {
                if let Some(child) = node.child_by_field_name(field) {
                    println!(
                        "field {field}={} {}..{}",
                        child.kind(),
                        child.start_byte(),
                        child.end_byte()
                    );
                }
            }
            current = node.parent();
        }
    }
    drop(parser);
    let provider = provider();
    let case = case(language);
    let fixture = Fixture::new(case, bytes);
    let budget = limits();
    let request = request(&fixture.snapshot, &fixture.source, case, &budget).to_parse_request();
    let parsed = execute_parse(
        &provider,
        &request,
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )
    .unwrap();
    for fact in parsed.facts() {
        if spans.iter().any(|&(start, end)| {
            fact.span().start_byte() <= u64::try_from(start).unwrap()
                && fact.span().end_byte() >= u64::try_from(end).unwrap()
        }) {
            println!(
                "capture id={} parent={:?} {:?} {} {}..{}",
                fact.local_id(),
                fact.parent(),
                fact.kind(),
                fact.syntax_kind().as_str(),
                fact.span().start_byte(),
                fact.span().end_byte()
            );
        }
    }
}

#[test]
fn native_bash_original_and_sdk_captures_record_the_missing_owner_boundary() {
    observe(
        "bash",
        BASH,
        tree_sitter_bash::LANGUAGE.into(),
        &[(2987, 2997), (3011, 3014)],
    );
}

#[test]
fn native_cpp_original_and_sdk_captures_record_the_ordinary_formal_boundary() {
    observe(
        "cpp",
        CPP,
        tree_sitter_cpp::LANGUAGE.into(),
        &[(703, 723), (744, 746)],
    );
}

#[test]
fn native_python_original_and_sdk_captures_record_the_module_assignment_boundary() {
    observe(
        "python",
        PYTHON,
        tree_sitter_python::LANGUAGE.into(),
        &[(61, 65)],
    );
}

#[test]
fn python_unique_written_module_identity_survives_value_change_without_per_write_symbols() {
    let provider = Arc::new(provider());
    let case = case("python");
    let fixture = Fixture::new(case, b"value = 1\n");
    let budget = limits();
    let before = analyze(
        &analyzer(&provider, case),
        &request(&fixture.snapshot, &fixture.source, case, &budget),
        &ExtensionSupport::default(),
    );
    let original = definition(before.document(), "value", EntityKind::Variable, 0, 5);
    let after_fixture = fixture.rewrite(b"value = 999\n");
    let after = analyze(
        &analyzer(&provider, case),
        &request(
            &after_fixture.snapshot,
            &after_fixture.source,
            case,
            &budget,
        ),
        &ExtensionSupport::default(),
    );
    let current = definition(after.document(), "value", EntityKind::Variable, 0, 5);
    assert_eq!(original, current);
    assert_ne!(
        before.document().files[0].content_hash,
        after.document().files[0].content_hash
    );
    assert_ne!(
        before.document().files[0].generation,
        after.document().files[0].generation
    );
}

#[test]
fn python_reassignment_complex_targets_and_nested_locals_are_explicitly_unavailable() {
    for source in [
        "value = 1\nvalue = 2\n",
        "value = 1\ndef owned():\n    value = 2\n",
        "value = other = 1\n",
        "value: int = 1\n",
        "holder.value = 1\n",
        "holder[0] = 1\n",
        "value, other = (1, 2)\n",
        "def owned():\n    localonly = 1\n",
        "class Owned:\n    localonly = 1\n",
    ] {
        let result = output("python", source.as_bytes());
        let document = result.document();
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.kind == EntityKind::Variable),
            "{source}"
        );
        for domain in [
            rootlight_ir::FactDomain::Entities,
            rootlight_ir::FactDomain::Occurrences,
            rootlight_ir::FactDomain::Relations,
        ] {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.domain == domain
                        && gap.detail.starts_with("python-")
                        && gap.detail.contains("unavailable")),
                "{source}"
            );
        }
    }
}

#[test]
fn python_comments_strings_and_module_uses_do_not_create_runtime_binding_claims() {
    let source =
        b"# fake = 1\ntext = 'fake = 2'\nvalue = 1\nvalue()\ndef owned(value):\n    return value\n";
    let result = output("python", source);
    let document = result.document();
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "fake")
    );
    let start = source
        .windows(7)
        .position(|part| part == b"value()")
        .unwrap();
    for role in [OccurrenceRole::Reference, OccurrenceRole::CallSite] {
        let sites: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == role
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + 5).unwrap()
            })
            .collect();
        assert!(!sites.is_empty());
        assert!(
            sites
                .iter()
                .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
    }
    let start = source
        .windows(5)
        .rposition(|part| part == b"value")
        .unwrap();
    assert!(
        document
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Reference
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.syntax_kind == "python.parameter_body.reference"
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn python_written_module_definition_projects_to_physical_bom_and_utf16_bytes() {
    let text = "# λ 😀\ntext = 'λ'\nvalue = 1\n";
    let logical = text.find("value").unwrap();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let mut raw = match encoding {
            "utf-8" => vec![0xef, 0xbb, 0xbf],
            "utf-16le" => vec![0xff, 0xfe],
            _ => vec![0xfe, 0xff],
        };
        if encoding == "utf-8" {
            raw.extend_from_slice(text.as_bytes());
        } else {
            for unit in text.encode_utf16() {
                raw.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
        }
        let (start, end) = if encoding == "utf-8" {
            (3 + logical, 3 + logical + 5)
        } else {
            let start = 2 + 2 * text[..logical].encode_utf16().count();
            (start, start + 10)
        };
        let case = case("python");
        let fixture = Fixture::new(case, &raw);
        let provider = Arc::new(provider());
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(case),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(case.generated);
        let result = analyze(
            &analyzer(&provider, case),
            &request,
            &ExtensionSupport::default(),
        );
        let document = result.document();
        validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
        definition(
            document,
            "value",
            EntityKind::Variable,
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        );
        assert_eq!(document.files[0].content_hash, content_hash(&raw));
        assert_eq!(
            document.files[0].byte_length,
            u64::try_from(raw.len()).unwrap()
        );
        let occurrence = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
            })
            .unwrap();
        assert_eq!(occurrence.source.content_hash(), content_hash(&raw));
        assert_eq!(
            occurrence.source.span().file(),
            fixture.source.span().file()
        );
        assert_eq!(occurrence.source.generation(), fixture.source.generation());
        assert_eq!(occurrence.syntactic_text_hash, content_hash(b"value"));
    }
}
