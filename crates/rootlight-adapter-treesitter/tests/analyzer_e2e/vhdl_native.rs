//! VHDL source contracts exercise the real analyzer and immutable physical spans.
//! They distinguish written ownership from unresolved language binding semantics.

use super::*;

const VHDL: LanguageCase = LanguageCase {
    name: "vhdl",
    path: "rtl/written.vhdl",
    frontend: "tree-sitter-vhdl-2.0.3",
    source: "entity Written is port (Clock, Reset : in bit; Data : out bit); end entity Written;\n",
    generated: false,
    body_before: "out bit",
    body_after: "inout bit",
};

fn output(source: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(VHDL, source.as_bytes());
    let budget = limits();
    let result = analyze(
        &analyzer(&provider, VHDL),
        &request(&fixture.snapshot, &fixture.source, VHDL, &budget),
        &ExtensionSupport::default(),
    );
    let document = result.document();
    validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
    for occurrence in &document.occurrences {
        let span = occurrence.source.span();
        let range =
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap();
        assert_eq!(
            occurrence.syntactic_text_hash,
            content_hash(&source.as_bytes()[range])
        );
        assert_eq!(
            occurrence.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(occurrence.source.repository(), document.repository);
        assert_eq!(occurrence.source.generation(), document.generation);
    }
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    result
}

fn entity<'a>(
    document: &'a rootlight_ir::NormalizedIrDocument,
    name: &str,
    kind: EntityKind,
) -> &'a rootlight_ir::EntityRecord {
    let found = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == name && entity.kind == kind)
        .collect::<Vec<_>>();
    assert_eq!(found.len(), 1, "{name}: {:?}", document.entities);
    found[0]
}

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
    name: &str,
    written: &str,
    kind: EntityKind,
    start: usize,
) -> SymbolId {
    let declaration = entity(document, name, kind);
    assert_eq!(declaration.display_name, written);
    let occurrences = document.occurrences.iter().filter(|occurrence|
        occurrence.role == OccurrenceRole::Definition
        && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == declaration.id)).collect::<Vec<_>>();
    assert_eq!(occurrences.len(), 1, "{name}");
    let reference = &occurrences[0].source;
    assert_eq!(reference.span().start_byte(), u64::try_from(start).unwrap());
    assert_eq!(
        reference.span().end_byte(),
        u64::try_from(start + written.len()).unwrap()
    );
    assert_eq!(
        &source.as_bytes()[start..start + written.len()],
        written.as_bytes()
    );
    assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
    declaration.id
}

fn contains(document: &rootlight_ir::NormalizedIrDocument, parent: SymbolId, child: SymbolId) {
    assert!(
        document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent)
                && relation.object == RelationEndpoint::Entity(child))
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Contains
                && relation.object == RelationEndpoint::Entity(child))
            .count(),
        1
    );
}

#[test]
fn entity_and_grouped_ports_have_exact_names_and_entity_ownership() {
    let source = "-- λ before physical byte ranges\nentity Controller is\n generic (Width, Depth : positive := 8);\n port (Clock, Reset : in bit; Data : out bit; Bus_IO : inout bit; Probe : buffer bit; Link : linkage bit);\nend entity CONTROLLER;\n";
    let result = output(source);
    let document = result.document();
    let owner = definition(
        document,
        source,
        "controller",
        "Controller",
        EntityKind::Module,
        source.find("Controller").unwrap(),
    );
    for written in ["Clock", "Reset", "Data", "Bus_IO", "Probe", "Link"] {
        let child = definition(
            document,
            source,
            &written.to_ascii_lowercase(),
            written,
            EntityKind::Field,
            source.find(written).unwrap(),
        );
        contains(document, owner, child);
    }
    for written in ["Width", "Depth"] {
        let child = definition(
            document,
            source,
            &written.to_ascii_lowercase(),
            written,
            EntityKind::Constant,
            source.find(written).unwrap(),
        );
        contains(document, owner, child);
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "controller")
            .count(),
        1
    );
    assert!(!document.entities.iter().any(|entity| matches!(
        entity.canonical_name.as_str(),
        "in" | "out" | "inout" | "bit" | "positive"
    )));
}

#[test]
fn architecture_target_and_repeated_end_names_never_declare_entities() {
    let source = "entity Device is port (Input : in bit); end Device;\narchitecture RTL of Device is\n signal Value : bit;\n type State is (Idle, Active);\n function Convert(Arg : bit) return bit is begin return Arg; end function Convert;\nbegin Value <= Input; end architecture RTL;\n";
    let result = output(source);
    let document = result.document();
    let module = definition(
        document,
        source,
        "device",
        "Device",
        EntityKind::Module,
        source.find("Device").unwrap(),
    );
    let architecture = definition(
        document,
        source,
        "rtl",
        "RTL",
        EntityKind::Namespace,
        source.find("RTL").unwrap(),
    );
    assert_ne!(module, architecture);
    let value = definition(
        document,
        source,
        "value",
        "Value",
        EntityKind::Variable,
        source.find("Value").unwrap(),
    );
    contains(document, architecture, value);
    let function = definition(
        document,
        source,
        "convert",
        "Convert",
        EntityKind::Function,
        source.find("Convert").unwrap(),
    );
    let parameter = definition(
        document,
        source,
        "arg",
        "Arg",
        EntityKind::Parameter,
        source.find("Arg").unwrap(),
    );
    contains(document, architecture, function);
    contains(document, function, parameter);
    let kind = definition(
        document,
        source,
        "state",
        "State",
        EntityKind::TypeAlias,
        source.find("State").unwrap(),
    );
    contains(document, architecture, kind);
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == "device")
            .count(),
        1
    );
    assert!(!document.relations.iter().any(|relation| relation.subject
        == RelationEndpoint::Entity(module)
        && relation.object == RelationEndpoint::Entity(architecture)));
}

#[test]
fn case_variants_share_basic_identity_without_merging_extended_names() {
    let upper = "entity CLOCK is port (VALUE : in bit); end CLOCK;\n";
    let lower = "entity clock is port (value : in bit); end clock;\n";
    let a = output(upper);
    let b = output(lower);
    assert_eq!(
        entity(a.document(), "clock", EntityKind::Module).id,
        entity(b.document(), "clock", EntityKind::Module).id
    );
    assert_eq!(
        entity(a.document(), "value", EntityKind::Field).id,
        entity(b.document(), "value", EntityKind::Field).id
    );
    let source = "entity Names is port (Basic, \\Basic\\, \\basic\\, \\space name\\, \\a\\\\b\\ : in bit); end Names;\n";
    let result = output(source);
    let document = result.document();
    let mut ids = BTreeSet::new();
    for (name, written) in [
        ("basic", "Basic"),
        (r"\Basic\", r"\Basic\"),
        (r"\basic\", r"\basic\"),
        (r"\space name\", r"\space name\"),
        (r"\a\\b\", r"\a\\b\"),
    ] {
        ids.insert(definition(
            document,
            source,
            name,
            written,
            EntityKind::Field,
            source.find(written).unwrap(),
        ));
    }
    assert_eq!(ids.len(), 5);
}

#[test]
fn comments_strings_and_subtype_references_do_not_become_definitions() {
    let source = "-- entity Decoy is port (Ghost : in bit); end Decoy;\npackage Texts is\n constant Message : string := \"entity Fake is port (Phantom : in bit); end Fake;\";\nend package Texts;\nentity Actual is port (Written : in bit); end Actual;\n";
    let result = output(source);
    let document = result.document();
    for name in ["decoy", "ghost", "fake", "phantom", "bit", "string"] {
        assert!(
            !document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "{name}"
        );
    }
    definition(
        document,
        source,
        "written",
        "Written",
        EntityKind::Field,
        source.find("Written").unwrap(),
    );
}

#[test]
fn malformed_owner_does_not_publish_orphaned_ports_as_valid_declarations() {
    let source = "entity Broken is port (First, : in bit); end Broken;\n";
    let result = output(source);
    assert!(
        !result
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Field)
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn repeated_written_subprograms_remain_distinct_without_overload_resolution() {
    let source = "package P is\n function Same(A : bit) return bit;\n function Same(A : integer) return integer;\nend P;\n";
    let result = output(source);
    let document = result.document();
    let functions = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "same" && entity.kind == EntityKind::Function)
        .collect::<Vec<_>>();
    assert_eq!(functions.len(), 2);
    assert_ne!(functions[0].id, functions[1].id);
    assert_eq!(document.occurrences.iter().filter(|occurrence| occurrence.role == OccurrenceRole::Definition
        && functions.iter().any(|function| matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if symbol == function.id))).count(), 2);
}

#[test]
fn tcp_engine_source_preserves_exact_entity_ports_and_containment() {
    let source = include_str!("../fixtures/vhdl/tcp_engine_content_memory.vhd");
    assert_eq!(source.len(), 1415);
    let result = output(source);
    let document = result.document();
    let owner = definition(
        document,
        source,
        "tcp_engine_content_memory",
        "tcp_engine_content_memory",
        EntityKind::Module,
        582,
    );
    for (name, start) in [("clk", 623), ("address", 655), ("data", 712)] {
        let member = definition(document, source, name, name, EntityKind::Field, start);
        contains(document, owner, member);
        let relation = document
            .relations
            .iter()
            .find(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(owner)
                    && relation.object == RelationEndpoint::Entity(member)
            })
            .unwrap();
        let source = relation.evidence.source.as_ref().unwrap();
        assert!(source.span().start_byte() <= u64::try_from(start).unwrap());
        assert!(source.span().end_byte() >= u64::try_from(start + name.len()).unwrap());
    }
    for (start, end) in [(189, 214), (760, 785), (817, 842)] {
        assert!(
            !document
                .occurrences
                .iter()
                .any(|occurrence| occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == start
                    && occurrence.source.span().end_byte() == end)
        );
    }
}

#[test]
fn utf16_names_and_contains_keep_original_physical_byte_identity() {
    let source = "-- marker λ😀\r\nentity Clock is port (\\VaLue Name\\ : in bit); end Clock;\r\n";
    for little_endian in [true, false] {
        let mut raw = if little_endian {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in source.encode_utf16() {
            let bytes = if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            };
            raw.extend_from_slice(&bytes);
        }
        let provider = Arc::new(provider());
        let fixture = Fixture::new(VHDL, &raw);
        let budget = limits();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            LanguageId::new("vhdl").unwrap(),
            EncodingId::new(if little_endian {
                "utf-16le"
            } else {
                "utf-16be"
            })
            .unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(VHDL.generated);
        let result = analyze(
            &analyzer(&provider, VHDL),
            &request,
            &ExtensionSupport::default(),
        );
        let document = result.document();
        validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
        assert_eq!(document.files[0].content_hash, content_hash(&raw));
        assert_eq!(
            document.files[0].byte_length,
            u64::try_from(raw.len()).unwrap()
        );
        let owner = entity(document, "clock", EntityKind::Module);
        let member = entity(document, r"\VaLue Name\", EntityKind::Field);
        contains(document, owner.id, member.id);
        for (record, written) in [(owner, "Clock"), (member, r"\VaLue Name\")] {
            assert_eq!(record.display_name, written);
            let projected_start = source.find(written).unwrap();
            let start =
                2 + 2 * u64::try_from(source[..projected_start].encode_utf16().count()).unwrap();
            let end = start + 2 * u64::try_from(written.len()).unwrap();
            let definitions = document
                .occurrences
                .iter()
                .filter(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: record.id }
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1);
            let occurrence = definitions[0];
            assert_eq!(occurrence.source.span().start_byte(), start);
            assert_eq!(occurrence.source.span().end_byte(), end);
            assert_eq!(occurrence.source.content_hash(), content_hash(&raw));
            let physical = &raw[usize::try_from(start).unwrap()..usize::try_from(end).unwrap()];
            let expected_physical = written
                .encode_utf16()
                .flat_map(|unit| {
                    if little_endian {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect::<Vec<_>>();
            assert_eq!(physical, expected_physical.as_slice());
            // Spelling hashes use decoded text; the SourceRef binds physical bytes.
            assert_eq!(
                occurrence.syntactic_text_hash,
                content_hash(written.as_bytes())
            );
        }
        assert_ne!(
            result.report().coverage().status(),
            CoverageStatus::Complete
        );
        assert!(
            document
                .relations
                .iter()
                .all(|relation| relation.predicate == RelationPredicate::Contains)
        );
    }
}
