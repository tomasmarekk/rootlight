//! Nushell written declarations and source-proven local bindings in normalized IR.
//! Fixtures are parsed as source; shell commands and completers never execute.

use super::*;

const NU: LanguageCase = LanguageCase {
    name: "nu",
    path: "written.nu",
    frontend: "nushell-native-source",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
const ORIGINAL: &[u8] = include_bytes!("../fixtures/nushell/after.nu");

fn output(raw: &[u8]) -> AnalysisOutput {
    output_encoded(raw, "utf-8")
}

fn output_encoded(raw: &[u8], encoding: &str) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(NU, raw);
    let budget = limits();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(NU),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        build_context(),
        &budget,
    )
    .unwrap()
    .with_generated_status(false);
    let result = analyze(
        &analyzer(&provider, NU),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
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
    assert_eq!(
        sites.len(),
        1,
        "{name} at {start}..{end}: {:?}",
        document.entities
    );
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("definition is unresolved")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.language, "nu");
    symbol
}

#[test]
fn original_after_retains_quoted_command_and_owned_parameters() {
    let result = output(ORIGINAL);
    let doc = result.document();
    let completer = definition(doc, 5, 19, "nu-complete ps", EntityKind::Function);
    let main = definition(doc, 154, 158, "main", EntityKind::Function);
    for (start, end, name) in [(166, 169, "pid"), (200, 206, "action")] {
        let param = definition(doc, start, end, name, EntityKind::Parameter);
        assert_eq!(
            doc.relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(main)
                    && edge.object == RelationEndpoint::Entity(param))
                .count(),
            1
        );
    }
    assert!(
        !doc.occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 237)
    );
    let pid = definition(doc, 166, 169, "pid", EntityKind::Parameter);
    let reads: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == 237
                && site.source.span().end_byte() == 240
                && site.target == OccurrenceTarget::Resolved { symbol: pid }
        })
        .collect();
    assert_eq!(
        reads.len(),
        1,
        "exact native formal read: {:?}",
        doc.occurrences
    );
    let links: Vec<_> = doc
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Entity(pid)
                && edge.object == RelationEndpoint::Entity(completer)
                && edge.evidence.source.as_ref().is_some_and(|source| {
                    source.span().start_byte() == 179 && source.span().end_byte() == 193
                })
        })
        .collect();
    assert_eq!(
        links.len(),
        1,
        "static parameter completer: {:?}",
        doc.relations
    );
}

#[test]
fn other_quoted_commands_and_captured_formals_use_exact_native_endpoints() {
    let raw = b"def \"complete task\" [] { [] }\ndef \"launch task\" [ticket: string@\"complete task\"] { do {|other| print $ticket } }\n";
    let result = output(raw);
    let doc = result.document();
    let find = |text: &str| {
        raw.windows(text.len())
            .position(|part| part == text.as_bytes())
            .unwrap() as u64
    };
    let completer_at = find("complete task");
    let command = definition(
        doc,
        completer_at,
        completer_at + 13,
        "complete task",
        EntityKind::Function,
    );
    let ticket_at = find("ticket:");
    let ticket = definition(
        doc,
        ticket_at,
        ticket_at + 6,
        "ticket",
        EntityKind::Parameter,
    );
    let read_at = find("$ticket") + 1;
    assert!(
        doc.occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == read_at
                && site.source.span().end_byte() == read_at + 6
                && site.target == OccurrenceTarget::Resolved { symbol: ticket })
    );
    assert!(
        doc.relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Entity(ticket)
                && edge.object == RelationEndpoint::Entity(command))
    );
    assert!(
        !doc.entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Parameter && entity.canonical_name == "other")
    );
}

#[test]
fn unmodeled_binders_and_assignment_targets_do_not_guess_formal_reads() {
    for raw in [
        "def outer [value] { do {|value| print $value } }",
        "def outer [value] { do {|...value| print $value } }",
        "def outer [value] { do {|$value| print $value } }",
        "def outer [value] { let value = 1; print $value }",
        "def outer [value] { def inner [] { print $value } }",
        "def outer [value] { $value = 3 }",
        "def outer [value] { for value in [1] { print $value } }",
        "def outer [value] { print $missing }",
        "def outer [value] { source helpers.nu; print $value }",
    ] {
        let result = output(format!("{raw}\n").as_bytes());
        let doc = result.document();
        assert!(
            !doc.occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{raw}"
        );
        assert!(
            doc.skipped_regions
                .iter()
                .any(|gap| gap.reason == SkippedRegionReason::UnsupportedConstruct),
            "explicit binding or runtime gap: {raw}"
        );
    }
}

#[test]
fn imports_duplicates_and_scope_commands_do_not_guess_completers() {
    for raw in [
        "use helpers.nu *\ndef complete [] { [] }\ndef main [arg: string@complete] { print $arg }",
        "def complete [] { [] }\ndef complete [] { [] }\ndef main [arg: string@complete] { print $arg }",
        "def complete [] { [] }\nhide complete\ndef main [arg: string@complete] { print $arg }",
        "def complete [] { [] }\nsource helpers.nu\ndef main [arg: string@complete] { print $arg }",
        "def complete [] { [] }\noverlay use helpers.nu\ndef main [arg: string@complete] { print $arg }",
        "const choices = [one two]\ndef main [arg: string@$choices] { print $arg }",
        "def complete [] { [] }\ndef main [arg: record<field: string@complete>] { print $arg }",
    ] {
        let result = output(raw.as_bytes());
        assert!(
            !result
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && matches!(edge.subject, RelationEndpoint::Entity(_))),
            "{raw}"
        );
    }
}

#[test]
fn utf16_original_uses_physical_source_spans_for_names_reads_and_completers() {
    for little in [true, false] {
        let mut raw = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in std::str::from_utf8(ORIGINAL).unwrap().encode_utf16() {
            raw.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let result = output_encoded(&raw, if little { "utf-16le" } else { "utf-16be" });
        let doc = result.document();
        let completer = definition(doc, 12, 40, "nu-complete ps", EntityKind::Function);
        let pid = definition(doc, 334, 340, "pid", EntityKind::Parameter);
        assert!(
            doc.occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == 476
                    && site.source.span().end_byte() == 482
                    && site.source.content_hash() == content_hash(&raw)
                    && site.target == OccurrenceTarget::Resolved { symbol: pid })
        );
        assert!(doc.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Entity(pid)
                && edge.object == RelationEndpoint::Entity(completer)
                && edge.evidence.source.as_ref().is_some_and(|source| {
                    source.span().start_byte() == 360
                        && source.span().end_byte() == 388
                        && source.content_hash() == content_hash(&raw)
                })
        }));
    }
}

#[test]
fn one_function_file_keeps_distinct_file_and_function_scopes() {
    for raw in [
        "def single [value] { print $value }",
        "def single [value] { print $value }\n",
    ] {
        let result = output(raw.as_bytes());
        let doc = result.document();
        let parameter = raw.find("value").unwrap() as u64;
        let value = definition(
            doc,
            parameter,
            parameter + 5,
            "value",
            EntityKind::Parameter,
        );
        let read = raw.rfind("$value").unwrap() as u64 + 1;
        assert!(
            doc.occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == read
                    && site.source.span().end_byte() == read + 5
                    && site.target == OccurrenceTarget::Resolved { symbol: value }),
            "{raw}"
        );
    }
}

#[test]
fn escaped_completer_does_not_bind_a_literal_backslash_command() {
    let raw = br#"def complete [] { [] }
def 'co\u{006d}plete' [] { [] }
def main [arg: string@"co\u{006d}plete"] { print $arg }
"#;
    let result = output(raw);
    let doc = result.document();
    definition(doc, 4, 12, "complete", EntityKind::Function);
    let name = br"co\u{006d}plete";
    let start = raw
        .windows(name.len())
        .position(|part| part == name)
        .unwrap() as u64;
    definition(
        doc,
        start,
        start + name.len() as u64,
        r"co\u{006d}plete",
        EntityKind::Function,
    );
    assert!(
        !doc.relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo
                && matches!(edge.subject, RelationEndpoint::Entity(_))),
        "escaped completer must remain unavailable"
    );
}

#[test]
fn escaped_command_name_does_not_masquerade_as_a_literal_command() {
    let raw = br#"def "co\u{006d}plete" [] { [] }
def main [arg: string@'co\u{006d}plete'] { print $arg }
"#;
    let result = output(raw);
    let doc = result.document();
    assert!(
        !doc.occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == 5),
        "escaped command identity requires unavailable decoding proof"
    );
    assert!(
        !doc.relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::RefersTo
                && matches!(edge.subject, RelationEndpoint::Entity(_))),
        "literal completer must not target an escaped declaration"
    );
}

#[test]
fn unavailable_escaped_owner_does_not_reparent_its_declarations() {
    let raw = br#"def "co\u{006d}plete" [hidden] {
    let local = 1
    def nested [] { [] }
    print $hidden
}
def main [arg] { print $arg }
"#;
    let result = output(raw);
    let doc = result.document();
    for name in ["hidden", "local", "nested"] {
        assert!(
            !doc.entities
                .iter()
                .any(|entity| entity.canonical_name == name),
            "unavailable owner must not leak {name} into its surrounding scope"
        );
    }
    let main = raw.windows(4).position(|part| part == b"main").unwrap() as u64;
    definition(doc, main, main + 4, "main", EntityKind::Function);
    let arg = raw.windows(3).position(|part| part == b"arg").unwrap() as u64;
    definition(doc, arg, arg + 3, "arg", EntityKind::Parameter);
}

#[test]
fn file_module_retains_original_top_level_owners() {
    let result = output(ORIGINAL);
    let doc = result.document();
    let modules: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Module)
        .collect();
    assert_eq!(
        modules.len(),
        1,
        "the native file scope must retain its Module"
    );
    let module = modules[0];
    assert_eq!(module.language, "nu");
    assert_eq!(module.canonical_name, "written.nu");
    assert!(
        !doc.occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Definition
                && site.target == (OccurrenceTarget::Resolved { symbol: module.id }))
    );
    for (start, end, name) in [(5, 19, "nu-complete ps"), (154, 158, "main")] {
        let symbol = definition(doc, start, end, name, EntityKind::Function);
        assert!(
            doc.relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(module.id)
                    && edge.object == RelationEndpoint::Entity(symbol))
        );
    }
}

#[test]
fn file_module_survives_unsupported_alias_import_and_external_module_sources() {
    for source in [
        &b"export alias bat = ^bat
"[..],
        &b"export use after.nu *
"[..],
        &b"export module after.nu
"[..],
    ] {
        let result = output(source);
        let doc = result.document();
        assert_eq!(
            doc.entities.len(),
            1,
            "unsupported declarations retain only their file Module: {source:?}"
        );
        assert_eq!(doc.entities[0].kind, EntityKind::Module);
        assert_eq!(doc.entities[0].canonical_name, "written.nu");
        assert_eq!(doc.entities[0].language, "nu");
        assert!(
            !doc.occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition)
        );
    }
}
