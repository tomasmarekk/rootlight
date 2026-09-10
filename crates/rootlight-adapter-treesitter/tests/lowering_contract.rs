//! End-to-end contracts for parser-independent Tree-sitter lowering.
//!
//! Fake syntax providers exercise stable IDs, conservative relations, evidence,
//! coverage gaps, cancellation, and canonical normalized-IR decoding.

use std::{
    fs,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AdapterDiagnostic, AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds,
    CoverageReport, DiagnosticCode, EncodingId, GenerationBoundSnapshot, IncludedRange,
    LanguageAnalyzer, LanguageId, MemoryAdmissionPolicy, MemoryEnforcement, ParseCapabilities,
    RequestError, SinkError, StreamLimits, SyntaxFact, SyntaxFactKind, SyntaxKindLabel,
    execute_analysis, testkit::MockParseProvider,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, DiagnosticSeverity, EntityFlag,
    ExtensionSupport, FactEvidence, IrDocument, IrLimits, LEXICAL_EXTENSION_NAMESPACE,
    LexicalEvidenceKind, OccurrenceRole, ProducerIdentity, RelationPredicate, SourceRef,
    SourceSpan, decode_ir_document, decode_lexical_evidence_envelope,
};
use rootlight_vfs::{RelativePath, RepositoryRoot, SourceSnapshot};
use tempfile::{TempDir, tempdir_in};

const SOURCE: &str =
    "mod api {\n    /// docs for alpha\n    pub fn alpha() { beta(); }\n    use crate::dep;\n}\n";

#[path = "lowering_contract/yaml_ownership.rs"]
mod yaml_ownership;

#[test]
fn json_key_captures_lower_to_exact_source_bound_properties() {
    assert_data_key_captures(
        "json",
        &[
            (r#""a""#, r#""a""#),
            (r#""\u0061""#, r#""a""#),
            (r#""""#, r#""""#),
            (r#"" a/b~c ""#, r#"" a/b~c ""#),
            (r#""\u0000""#, r#""\u0000""#),
            (r#""\\u0000""#, r#""\\u0000""#),
            (r#""\uD800""#, r#""\ud800""#),
            (r#""\uD83C\uDF0D""#, r#""🌍""#),
            (r#""🌍""#, r#""🌍""#),
        ],
    );
}

#[test]
fn toml_key_paths_lower_without_merging_quoted_dots_or_losing_source() {
    assert_data_key_captures(
        "toml",
        &[
            ("a", r#""a""#),
            ("'a'", r#""a""#),
            (r#""\x61""#, r#""a""#),
            ("a.b", r#""a"."b""#),
            (" 'a' . \"b\" ", r#""a"."b""#),
            ("'a.b'", r#""a.b""#),
            ("''", r#""""#),
            ("' '", r#"" ""#),
            (r#""\u0000""#, r#""\u0000""#),
            (r#"'\u0000'"#, r#""\\u0000""#),
            (r#""\U0001F30D""#, r#""🌍""#),
            ("'🌍'", r#""🌍""#),
        ],
    );
}

#[test]
fn yaml_flow_keys_lower_with_typed_identity_and_exact_source() {
    assert_data_key_captures(
        "yaml",
        &[
            ("name", "str:\"name\""),
            ("'name'", "str:\"name\""),
            (r#""\u006eame""#, "str:\"name\""),
            ("''", "str:\"\""),
            ("' '", "str:\" \""),
            (r#""\0""#, r#"str:"\u0000""#),
            (r#"'\0'"#, r#"str:"\\0""#),
            ("true", "bool:true"),
            ("TRUE", "bool:true"),
            ("'true'", "str:\"true\""),
            ("null", "null:null"),
            ("~", "null:null"),
            ("11", "int:11"),
            ("0xB", "int:11"),
            ("11.0", "float:11e0"),
            ("1.1e1", "float:11e0"),
            ("'🌍'", "str:\"🌍\""),
            (r#""\U0001F30D""#, "str:\"🌍\""),
        ],
    );
}

#[test]
fn invalid_toml_scalar_names_remain_explicit_source_bound_gaps() {
    assert_unavailable_data_names("toml", &[r#""\uD800""#, r#""\U00110000""#, "a..b"]);
}

#[test]
fn yaml_context_dependent_keys_and_invalid_scalars_remain_explicit_gaps() {
    assert_unavailable_data_names(
        "yaml",
        &[
            r#""\uD800""#,
            "!!str true",
            "*key",
            "[one, two]",
            "|\n  key",
        ],
    );
}

fn assert_unavailable_data_names(language: &str, keys: &[&str]) {
    for key in keys {
        let separator = if language == "yaml" { ":" } else { " =" };
        let text = format!("{key}{separator} 1\n");
        let (_directory, snapshot, source) =
            source_fixture_for(&text, &format!("data.{language}"), b"invalid-data-key");
        let output = analyze_custom(
            &snapshot,
            &source,
            LanguageId::new(language).unwrap(),
            &limits(IrLimits::default()),
            vec![
                SyntaxFact::new(
                    1,
                    None,
                    SyntaxFactKind::Root,
                    source.span(),
                    0,
                    label(&format!("{language}.file.root")),
                ),
                SyntaxFact::new(
                    2,
                    Some(1),
                    SyntaxFactKind::Declaration,
                    source.span(),
                    1,
                    label(&format!("{language}.property.declaration")),
                ),
                SyntaxFact::new(
                    3,
                    Some(2),
                    SyntaxFactKind::Occurrence,
                    span_in(&text, &source, key, 0),
                    2,
                    label(&format!("{language}.property.definition")),
                ),
            ],
        )
        .expect("invalid key is accounted, not fatal");
        let document = output.document();
        assert!(document.entities.is_empty());
        assert!(
            document.skipped_regions.iter().any(|region| {
                region.detail == "declaration-name-unavailable" && region.source == source
            }),
            "{:?}",
            document.skipped_regions
        );
    }
}

fn assert_data_key_captures(language: &str, cases: &[(&str, &str)]) {
    let mut identities = Vec::new();
    let values = if language == "json" {
        ["1", "[true, null]"]
    } else {
        ["1", "[true, false]"]
    };
    for &(key, canonical) in cases {
        for value in values {
            let (member, text) = if language == "json" {
                let member = format!("{key}:{value}");
                let text = format!("{{{member}}}");
                (member, text)
            } else {
                let separator = if language == "yaml" { ":" } else { " =" };
                let member = format!("{key}{separator} {value}");
                let text = format!("{member}\n");
                (member, text)
            };
            let (_directory, snapshot, source) =
                source_fixture_for(&text, &format!("data.{language}"), b"data-key-lowering");
            let captured = vec![
                SyntaxFact::new(
                    1,
                    None,
                    SyntaxFactKind::Root,
                    source.span(),
                    0,
                    label(&format!("{language}.file.root")),
                ),
                SyntaxFact::new(
                    2,
                    Some(1),
                    SyntaxFactKind::Declaration,
                    span_in(&text, &source, &member, 0),
                    1,
                    label(&format!("{language}.property.declaration")),
                ),
                SyntaxFact::new(
                    3,
                    Some(2),
                    SyntaxFactKind::Occurrence,
                    span_in(&text, &source, key, 0),
                    2,
                    label(&format!("{language}.property.definition")),
                ),
            ];
            let output = analyze_custom(
                &snapshot,
                &source,
                LanguageId::new(language).expect("language"),
                &limits(IrLimits::default()),
                captured,
            )
            .expect("property lowering");
            let document = output.document();
            assert_eq!(document.entities.len(), 1, "{text}");
            assert!(
                document.skipped_regions.is_empty(),
                "{:?}",
                document.skipped_regions
            );
            let entity = &document.entities[0];
            assert_eq!(entity.kind, rootlight_ir::EntityKind::Property);
            if language == "toml" {
                assert_eq!(entity.qualified_name, canonical);
            } else {
                assert_eq!(entity.canonical_name, canonical);
            }
            assert_eq!(entity.language, language);
            assert_eq!(
                entity.evidence.source,
                Some(source_for_span(
                    &source,
                    span_in(&text, &source, &member, 0)
                ))
            );
            let definition = document
                .occurrences
                .iter()
                .find(|occurrence| occurrence.role == OccurrenceRole::Definition)
                .expect("definition evidence");
            assert_eq!(definition.syntactic_text_hash, content_hash(key.as_bytes()));
            assert_eq!(
                definition.source,
                source_for_span(&source, span_in(&text, &source, key, 0))
            );
            assert_eq!(definition.evidence.source, entity.evidence.source);
            let encoded = serde_json::to_vec(document).expect("canonical document encodes");
            let decoded =
                decode_ir_document(&encoded, &IrLimits::default(), &ExtensionSupport::default())
                    .expect("canonical IR round trip");
            assert_eq!(decoded, IrDocument::NormalizedV1_1(document.clone()));
            identities.push((canonical, entity.id));
        }
    }
    for (name, identity) in &identities {
        for (other_name, other_identity) in &identities {
            assert_eq!(
                name == other_name,
                identity == other_identity,
                "identity equality must follow canonical key equality"
            );
        }
    }
}

#[derive(Clone, Copy)]
struct LocalIds {
    root: u64,
    module: u64,
    comment: u64,
    function: u64,
    call: u64,
    import: u64,
}

#[test]
fn toml_nested_array_tables_follow_the_latest_parent_occurrence() {
    let entries = [
        ("array", "items", ""),
        ("pair", "name", "'first'"),
        ("table", "items.detail", ""),
        ("pair", "id", "1"),
        ("array", "items.children", ""),
        ("pair", "name", "'child'"),
        ("array", "items", ""),
        ("pair", "name", "'second'"),
        ("table", "items.detail", ""),
        ("pair", "id", "2"),
        ("array", "items.children", ""),
        ("pair", "name", "'other'"),
    ];
    let document = toml_table_fixture(&entries);
    assert!(
        document.skipped_regions.is_empty(),
        "{:?}",
        document.skipped_regions
    );
    assert_eq!(document.entities.len(), 12);
    for index in [0, 1] {
        let prefix = format!("\"items\"[{index}]");
        let owner = document
            .entities
            .iter()
            .find(|entity| entity.qualified_name == prefix)
            .unwrap();
        for suffix in ["\"name\"", "\"detail\"", "\"children\"[0]"] {
            let child = document
                .entities
                .iter()
                .find(|entity| entity.qualified_name == format!("{prefix}.{suffix}"))
                .unwrap();
            assert_eq!(
                child.container,
                Some(rootlight_ir::ContainerRef::Entity(owner.id))
            );
        }
        let detail = document
            .entities
            .iter()
            .find(|entity| entity.qualified_name == format!("{prefix}.\"detail\""))
            .unwrap();
        let id = document
            .entities
            .iter()
            .find(|entity| entity.qualified_name == format!("{prefix}.\"detail\".\"id\""))
            .unwrap();
        assert_eq!(
            id.container,
            Some(rootlight_ir::ContainerRef::Entity(detail.id))
        );
    }
    let mut edited = entries;
    edited[1].2 = "'body changed'";
    let after = toml_table_fixture(&edited);
    let ids = |document: &rootlight_ir::NormalizedIrDocument| {
        document
            .entities
            .iter()
            .map(|entity| (entity.qualified_name.clone(), entity.id))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    assert_eq!(ids(&document), ids(&after));
}

#[test]
fn toml_dotted_and_explicit_tables_share_addresses_even_with_late_parent_headers() {
    let dotted = toml_table_fixture(&[("pair", "a.b.value", "1")]);
    let headers = toml_table_fixture(&[
        ("table", "a.b", ""),
        ("pair", "value", "2"),
        ("table", "a", ""),
    ]);
    assert!(headers.skipped_regions.is_empty());
    let first = &dotted.entities[0];
    let value = headers
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "\"value\"")
        .unwrap();
    assert_eq!(first.id, value.id);
    assert_eq!(first.qualified_name, value.qualified_name);
    let a = headers
        .entities
        .iter()
        .find(|entity| entity.qualified_name == "\"a\"")
        .unwrap();
    let b = headers
        .entities
        .iter()
        .find(|entity| entity.qualified_name == "\"a\".\"b\"")
        .unwrap();
    assert_eq!(b.container, Some(rootlight_ir::ContainerRef::Entity(a.id)));
    assert_eq!(
        value.container,
        Some(rootlight_ir::ContainerRef::Entity(b.id))
    );
}

#[test]
fn toml_inline_tables_in_nested_arrays_count_scalar_siblings() {
    let text = "items = [0, { name = 'first' }, [0, { name = 'second' }]]\n";
    let (_directory, snapshot, source) =
        source_fixture_for(text, "data.toml", b"toml-inline-ownership");
    let mut facts = vec![SyntaxFact::new(
        1,
        None,
        SyntaxFactKind::Root,
        source.span(),
        0,
        label("toml.file.root"),
    )];
    for (id, parent, kind, syntax, needle, ordinal, depth) in [
        (
            2,
            1,
            SyntaxFactKind::Declaration,
            "toml.property.declaration",
            text,
            0,
            1,
        ),
        (
            3,
            2,
            SyntaxFactKind::Occurrence,
            "toml.key.definition",
            "items",
            0,
            2,
        ),
        (
            4,
            2,
            SyntaxFactKind::Scope,
            "toml.array.scope",
            "[0, { name = 'first' }, [0, { name = 'second' }]]",
            0,
            2,
        ),
        (
            5,
            4,
            SyntaxFactKind::Scope,
            "toml.array_element.scope",
            "0",
            0,
            3,
        ),
        (
            6,
            4,
            SyntaxFactKind::Scope,
            "toml.array_element.scope",
            "{ name = 'first' }",
            0,
            3,
        ),
        (
            7,
            6,
            SyntaxFactKind::Declaration,
            "toml.property.declaration",
            "name = 'first'",
            0,
            4,
        ),
        (
            8,
            7,
            SyntaxFactKind::Occurrence,
            "toml.key.definition",
            "name",
            0,
            5,
        ),
        (
            9,
            4,
            SyntaxFactKind::Scope,
            "toml.array_element.scope",
            "[0, { name = 'second' }]",
            0,
            3,
        ),
        (
            10,
            9,
            SyntaxFactKind::Scope,
            "toml.array.scope",
            "[0, { name = 'second' }]",
            0,
            4,
        ),
        (
            11,
            10,
            SyntaxFactKind::Scope,
            "toml.array_element.scope",
            "0",
            1,
            5,
        ),
        (
            12,
            10,
            SyntaxFactKind::Scope,
            "toml.array_element.scope",
            "{ name = 'second' }",
            0,
            5,
        ),
        (
            13,
            12,
            SyntaxFactKind::Declaration,
            "toml.property.declaration",
            "name = 'second'",
            0,
            6,
        ),
        (
            14,
            13,
            SyntaxFactKind::Occurrence,
            "toml.key.definition",
            "name",
            1,
            7,
        ),
    ] {
        facts.push(SyntaxFact::new(
            id,
            Some(parent),
            kind,
            span_in(text, &source, needle, ordinal),
            depth,
            label(syntax),
        ));
    }
    let output = analyze_custom(
        &snapshot,
        &source,
        LanguageId::new("toml").unwrap(),
        &limits(IrLimits::default()),
        facts,
    )
    .unwrap();
    let document = output.document();
    assert!(document.skipped_regions.is_empty());
    assert_eq!(document.entities.len(), 3);
    for expected in ["\"items\"[1].\"name\"", "\"items\"[2][1].\"name\""] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.qualified_name == expected)
            .unwrap();
        assert_eq!(entity.display_name, "name");
    }
}

#[test]
fn toml_duplicate_table_ambiguity_propagates_to_descendants() {
    let document = toml_table_fixture(&[
        ("table", "a", ""),
        ("pair", "x", "1"),
        ("table", "a", ""),
        ("pair", "y", "2"),
        ("table", "a.b", ""),
        ("pair", "z", "3"),
    ]);
    assert!(document.entities.is_empty(), "{:?}", document.entities);
    assert_eq!(
        document
            .skipped_regions
            .iter()
            .filter(|region| region.detail == "stable-scope-identity-unavailable")
            .count(),
        6
    );
}

fn toml_table_fixture(entries: &[(&str, &str, &str)]) -> rootlight_ir::NormalizedIrDocument {
    toml_table_fixture_with_limits(entries, &limits(IrLimits::default()))
}

#[test]
fn toml_qualified_address_uses_its_actual_separator_at_the_byte_boundary() {
    let key = "x".repeat(60);
    let mut ir = IrLimits::default();
    ir.max_string_bytes = 66;
    let document =
        toml_table_fixture_with_limits(&[("table", "a", ""), ("pair", &key, "1")], &limits(ir));
    assert!(document.skipped_regions.is_empty());
    assert_eq!(document.entities.len(), 2);
    assert_eq!(
        document
            .entities
            .iter()
            .map(|entity| entity.qualified_name.len())
            .max(),
        Some(66)
    );
}

fn toml_table_fixture_with_limits(
    entries: &[(&str, &str, &str)],
    analysis_limits: &AnalysisLimits,
) -> rootlight_ir::NormalizedIrDocument {
    struct Entry {
        start: usize,
        end: usize,
        key_start: usize,
        key_end: usize,
        parent: Option<usize>,
        label: &'static str,
    }
    let mut text = String::new();
    let mut recorded: Vec<Entry> = Vec::new();
    let mut current = None;
    for &(kind, key, value) in entries {
        let start = text.len();
        let (prefix, suffix, label) = match kind {
            "table" => ("[", "]\n", "toml.table.declaration"),
            "array" => ("[[", "]]\n", "toml.table_array_element.declaration"),
            "pair" => ("", "", "toml.property.declaration"),
            _ => panic!("unknown fixture entry"),
        };
        text.push_str(prefix);
        let key_start = text.len();
        text.push_str(key);
        let key_end = text.len();
        text.push_str(suffix);
        if kind == "pair" {
            text.push_str(&format!(" = {value}\n"));
        }
        let parent = if kind == "pair" { current } else { None };
        recorded.push(Entry {
            start,
            end: text.len(),
            key_start,
            key_end,
            parent,
            label,
        });
        if kind != "pair" {
            current = Some(recorded.len() - 1);
        }
        if let Some(parent) = parent {
            recorded[parent].end = text.len();
        }
    }
    let (_directory, snapshot, source) =
        source_fixture_for(&text, "data.toml", b"toml-table-ownership");
    let mut facts = vec![SyntaxFact::new(
        1,
        None,
        SyntaxFactKind::Root,
        source.span(),
        0,
        label("toml.file.root"),
    )];
    for (index, entry) in recorded.iter().enumerate() {
        let id = u64::try_from(index).unwrap() * 2 + 2;
        let parent = entry
            .parent
            .map_or(1, |parent| u64::try_from(parent).unwrap() * 2 + 2);
        let depth = if entry.parent.is_some() { 2 } else { 1 };
        let range = |start, end| {
            let needle = &text[start..end];
            let ordinal = text[..start].match_indices(needle).count();
            span_in(&text, &source, needle, ordinal)
        };
        facts.push(SyntaxFact::new(
            id,
            Some(parent),
            SyntaxFactKind::Declaration,
            range(entry.start, entry.end),
            depth,
            label(entry.label),
        ));
        facts.push(SyntaxFact::new(
            id + 1,
            Some(id),
            SyntaxFactKind::Occurrence,
            range(entry.key_start, entry.key_end),
            depth + 1,
            label("toml.key.definition"),
        ));
    }
    let output = analyze_custom(
        &snapshot,
        &source,
        LanguageId::new("toml").unwrap(),
        analysis_limits,
        facts,
    )
    .unwrap();
    output.document().clone()
}

#[test]
fn lowering_is_independent_of_local_ids_and_emission_order() {
    let (_temporary, snapshot, source) = source_fixture();
    let analysis_limits = limits(IrLimits::default());
    let first = analyze(
        &snapshot,
        &source,
        &analysis_limits,
        facts(
            &source,
            LocalIds {
                root: 10,
                module: 20,
                comment: 30,
                function: 40,
                call: 50,
                import: 60,
            },
            false,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("first lowering commits");
    let second = analyze(
        &snapshot,
        &source,
        &analysis_limits,
        facts(
            &source,
            LocalIds {
                root: 601,
                module: 509,
                comment: 407,
                function: 307,
                call: 211,
                import: 101,
            },
            true,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("reordered lowering commits");

    assert_eq!(first.document(), second.document());
    assert_eq!(first.report().coverage(), second.report().coverage());
}

#[test]
fn duplicate_captures_deduplicate_without_reclassifying_definitions() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let ids = LocalIds {
        root: 1,
        module: 2,
        comment: 3,
        function: 4,
        call: 5,
        import: 6,
    };
    let mut captured = facts(&source, ids, false);
    captured.push(SyntaxFact::new(
        70,
        Some(ids.function),
        SyntaxFactKind::Occurrence,
        span_for_nth(&source, "alpha", 1),
        3,
        label("rust.function.definition"),
    ));
    captured.push(SyntaxFact::new(
        71,
        Some(ids.function.wrapping_add(4_000_000)),
        SyntaxFactKind::Occurrence,
        span_for(&source, "beta()"),
        4,
        label("rust.call.reference"),
    ));

    let output = analyze(
        &snapshot,
        &source,
        &limits,
        captured.clone(),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("duplicate semantic captures lower deterministically");
    assert_eq!(output.document().entities.len(), 2);
    assert_eq!(
        output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::Definition)
            .count(),
        2
    );
    assert_eq!(
        output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .count(),
        1
    );
}

#[test]
fn ambiguous_definition_capture_becomes_an_explicit_gap() {
    let (_temporary, snapshot, source) = source_fixture();
    let default_limits = limits(IrLimits::default());
    let ids = LocalIds {
        root: 1,
        module: 2,
        comment: 3,
        function: 4,
        call: 5,
        import: 6,
    };
    let mut captured = facts(&source, ids, false);
    captured.push(SyntaxFact::new(
        70,
        Some(ids.function),
        SyntaxFactKind::Occurrence,
        span_for(&source, "beta"),
        3,
        label("rust.function.definition"),
    ));

    let output = analyze(
        &snapshot,
        &source,
        &default_limits,
        captured.clone(),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("ambiguous definition is omitted without inventing an entity");
    assert_eq!(output.document().entities.len(), 1);
    assert!(output.document().skipped_regions.iter().any(|region| {
        region.detail == "declaration-name-unavailable"
            && region.domain == rootlight_ir::FactDomain::Entities
    }));
    let entity_coverage = output
        .report()
        .coverage()
        .domains()
        .iter()
        .find(|domain| domain.domain() == rootlight_ir::FactDomain::Entities)
        .expect("entity coverage is reported");
    assert_eq!(entity_coverage.status(), CoverageStatus::Bounded);
    assert_eq!(entity_coverage.skipped(), 1);

    let mut constrained_ir = IrLimits::default();
    constrained_ir.max_skipped_regions = 0;
    let error = analyze(
        &snapshot,
        &source,
        &limits(constrained_ir),
        captured,
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect_err("tight skipped-region quota rejects before lowering growth");
    assert!(matches!(
        error,
        AdapterError::Sink(SinkError::StreamLimit {
            resource: rootlight_adapter_sdk::ResourceKind::Records,
            ..
        })
    ));
}

#[test]
fn missing_java_field_definition_is_reserved_in_preflight_quotas() {
    const JAVA: &str = "class Example { int first, second; }\n";
    let (_temporary, snapshot, source) =
        source_fixture_for(JAVA, "src/Example.java", b"java-multi-field-fixture");
    let facts = vec![
        SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.span(),
            0,
            label("java.source.root"),
        ),
        SyntaxFact::new(
            2,
            Some(1),
            SyntaxFactKind::Declaration,
            span_in(JAVA, &source, "int first, second", 0),
            1,
            label("java.field.declaration"),
        ),
    ];
    let language = LanguageId::new("java").expect("Java language is valid");

    let mut skipped_limit = IrLimits::default();
    skipped_limit.max_skipped_regions = 0;
    let error = analyze_custom(
        &snapshot,
        &source,
        language.clone(),
        &limits(skipped_limit),
        facts.clone(),
    )
    .expect_err("missing definition is admitted against skipped quota");
    assert!(
        matches!(
            error,
            AdapterError::Sink(SinkError::StreamLimit {
                resource: rootlight_adapter_sdk::ResourceKind::Records,
                observed: 1,
                limit: 0,
            })
        ),
        "{error:?}"
    );

    let mut total_limit = IrLimits::default();
    // Without the reserved name-unavailable record this conservative upper
    // bound would be 15 and would incorrectly pass the preflight.
    total_limit.max_total_records = 15;
    let error = analyze_custom(&snapshot, &source, language, &limits(total_limit), facts)
        .expect_err("missing definition is included in total-record preflight");
    assert!(
        matches!(
            error,
            AdapterError::Sink(SinkError::StreamLimit {
                resource: rootlight_adapter_sdk::ResourceKind::Records,
                observed: 16,
                limit: 15,
            })
        ),
        "{error:?}"
    );
}

#[test]
fn markdown_reference_edges_are_reserved_before_ir_materialization() {
    const MARKDOWN: &str = "[ref]: target.md\n\n[ref]\n";
    let (_temporary, snapshot, source) = source_fixture_for(
        MARKDOWN,
        "docs/guide.md",
        b"markdown-relation-quota-fixture",
    );
    let facts: Vec<_> = [
        (
            1,
            None,
            SyntaxFactKind::Root,
            MARKDOWN,
            0,
            0,
            "markdown.file.root",
        ),
        (
            2,
            Some(1),
            SyntaxFactKind::Module,
            MARKDOWN,
            0,
            1,
            "markdown.file.module",
        ),
        (
            3,
            Some(2),
            SyntaxFactKind::Declaration,
            "[ref]: target.md\n",
            0,
            2,
            "markdown.link_definition.declaration",
        ),
        (
            4,
            Some(3),
            SyntaxFactKind::Occurrence,
            "[ref]",
            0,
            3,
            "markdown.link_label.definition",
        ),
        (
            5,
            Some(2),
            SyntaxFactKind::Occurrence,
            "[ref]",
            1,
            2,
            "markdown.shortcut_link.reference",
        ),
    ]
    .into_iter()
    .map(|(id, parent, kind, text, nth, depth, syntax)| {
        SyntaxFact::new(
            id,
            parent,
            kind,
            span_in(MARKDOWN, &source, text, nth),
            depth,
            label(syntax),
        )
    })
    .collect();
    let language = LanguageId::new("markdown").unwrap();
    let output = analyze_custom(
        &snapshot,
        &source,
        language.clone(),
        &limits(IrLimits::default()),
        facts.clone(),
    )
    .unwrap();
    assert_eq!(
        output
            .document()
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
            .count(),
        1
    );
    let mut ir = IrLimits::default();
    ir.max_relations = 2;
    let error = analyze_custom(&snapshot, &source, language, &limits(ir), facts).unwrap_err();
    assert!(
        matches!(
            error,
            AdapterError::Sink(SinkError::StreamLimit {
                resource: rootlight_adapter_sdk::ResourceKind::Records,
                observed: 3,
                limit: 2,
            })
        ),
        "{error:?}"
    );
}

#[test]
fn lua_reference_relations_are_reserved_in_relation_and_total_record_quotas() {
    const LUA: &str = "local value = 1\nreturn value\n";
    let (_temporary, snapshot, source) =
        source_fixture_for(LUA, "src/module.lua", b"lua-lexical-quota-fixture");
    let facts = [
        (1, None, SyntaxFactKind::Root, LUA, 0, 0, "lua.file.root"),
        (
            2,
            Some(1),
            SyntaxFactKind::Module,
            LUA,
            0,
            1,
            "lua.file.module",
        ),
        (
            3,
            Some(2),
            SyntaxFactKind::Scope,
            LUA,
            0,
            2,
            "lua.file.scope",
        ),
        (
            4,
            Some(3),
            SyntaxFactKind::Scope,
            "local value = 1",
            0,
            3,
            "lua.local_binding.scope",
        ),
        (
            5,
            Some(4),
            SyntaxFactKind::Declaration,
            "local value = 1",
            0,
            4,
            "lua.variable.declaration",
        ),
        (
            6,
            Some(5),
            SyntaxFactKind::Occurrence,
            "value",
            0,
            5,
            "lua.identifier.definition",
        ),
        (
            7,
            Some(3),
            SyntaxFactKind::Occurrence,
            "value",
            1,
            3,
            "lua.identifier.reference",
        ),
    ]
    .into_iter()
    .map(|(id, parent, kind, text, nth, depth, syntax)| {
        SyntaxFact::new(
            id,
            parent,
            kind,
            span_in(LUA, &source, text, nth),
            depth,
            label(syntax),
        )
    })
    .collect::<Vec<_>>();
    for host in ["lua", "markdown"] {
        let language = LanguageId::new(host).expect("host language is valid");
        let output = analyze_custom(
            &snapshot,
            &source,
            language.clone(),
            &limits(IrLimits::default()),
            facts.clone(),
        )
        .expect("source-backed Lua references lower");
        assert_eq!(
            output
                .document()
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
                .count(),
            1
        );
        for total_quota in [false, true] {
            let mut ir = IrLimits::default();
            let (expected_observed, expected_limit) = if total_quota {
                ir.max_total_records = 22;
                (23, 22)
            } else {
                ir.max_relations = 2;
                (3, 2)
            };
            let error = analyze_custom(
                &snapshot,
                &source,
                language.clone(),
                &limits(ir),
                facts.clone(),
            )
            .expect_err("lexical relation is reserved before output materializes");
            assert!(
                matches!(error, AdapterError::Sink(SinkError::StreamLimit {
            resource: rootlight_adapter_sdk::ResourceKind::Records, observed, limit,
        }) if observed == expected_observed && limit == expected_limit),
                "{error:?}"
            );
        }
    }
}

#[test]
fn nix_implicit_path_owners_reserve_entities_occurrences_and_relations() {
    let nix = "let value.part = 1; in value";
    let (_temporary, snapshot, source) =
        source_fixture_for(nix, "src/module.nix", b"nix-path-quota-fixture");
    let facts = [
        (1, None, SyntaxFactKind::Root, nix, 0, 0, "nix.file.root"),
        (
            2,
            Some(1),
            SyntaxFactKind::Module,
            nix,
            0,
            1,
            "nix.file.module",
        ),
        (
            3,
            Some(2),
            SyntaxFactKind::Scope,
            nix,
            0,
            2,
            "nix.let.scope",
        ),
        (
            4,
            Some(3),
            SyntaxFactKind::Declaration,
            "value.part = 1;",
            0,
            3,
            "nix.variable.declaration",
        ),
        (
            5,
            Some(4),
            SyntaxFactKind::Occurrence,
            "value.part",
            0,
            4,
            "nix.binding_name.definition",
        ),
        (
            6,
            Some(4),
            SyntaxFactKind::Occurrence,
            "value",
            0,
            4,
            "nix.path_segment.definition_part",
        ),
        (
            7,
            Some(4),
            SyntaxFactKind::Occurrence,
            "part",
            0,
            4,
            "nix.path_segment.definition_part",
        ),
        (
            8,
            Some(3),
            SyntaxFactKind::Occurrence,
            "value",
            1,
            3,
            "nix.identifier.reference",
        ),
    ]
    .into_iter()
    .map(|(id, parent, kind, text, nth, depth, syntax)| {
        SyntaxFact::new(
            id,
            parent,
            kind,
            span_in(nix, &source, text, nth),
            depth,
            label(syntax),
        )
    })
    .collect::<Vec<_>>();
    for host in ["nix", "markdown"] {
        let language = LanguageId::new(host).unwrap();
        let output = analyze_custom(
            &snapshot,
            &source,
            language.clone(),
            &limits(IrLimits::default()),
            facts.clone(),
        )
        .unwrap();
        assert_eq!(output.document().entities.len(), 3);
        assert_eq!(output.document().occurrences.len(), 3);
        assert_eq!(output.document().relations.len(), 4);
        for quota in ["entities", "occurrences", "relations"] {
            let mut ir = IrLimits::default();
            let (observed, maximum) = match quota {
                "entities" => {
                    ir.max_entities = 3;
                    (4, 3)
                }
                "occurrences" => {
                    ir.max_occurrences = 4;
                    (5, 4)
                }
                _ => {
                    ir.max_relations = 4;
                    (5, 4)
                }
            };
            let error = analyze_custom(
                &snapshot,
                &source,
                language.clone(),
                &limits(ir),
                facts.clone(),
            )
            .expect_err("implicit owners require reservations before materialization");
            assert!(
                matches!(error, AdapterError::Sink(SinkError::StreamLimit { resource: rootlight_adapter_sdk::ResourceKind::Records, observed: actual, limit }) if actual == observed && limit == maximum),
                "{quota}: {error:?}"
            );
        }
    }
}

#[test]
fn nix_lexical_relations_and_gaps_are_reserved_before_materialization() {
    assert_nix_lexical_quotas("let value = 1; in value", "value = 1;", "value");
}

#[test]
fn nix_quoted_lexical_relations_obey_the_same_preflight_quotas() {
    assert_nix_lexical_quotas(
        r#"let "\value" = 1; in value"#,
        r#""\value" = 1;"#,
        r#""\value""#,
    );
}

#[test]
fn nix_selected_attribute_relations_reserve_existing_output_quotas() {
    let nix = "({ value = 1; }).value";
    let (_temporary, snapshot, source) =
        source_fixture_for(nix, "src/module.nix", b"nix-selection-quotas");
    let facts: Vec<_> = [
        (1, None, SyntaxFactKind::Root, nix, 0, 0, "nix.file.root"),
        (
            2,
            Some(1),
            SyntaxFactKind::Module,
            nix,
            0,
            1,
            "nix.file.module",
        ),
        (
            3,
            Some(2),
            SyntaxFactKind::Scope,
            nix,
            0,
            2,
            "nix.selection.scope",
        ),
        (
            4,
            Some(3),
            SyntaxFactKind::Scope,
            "{ value = 1; }",
            0,
            3,
            "nix.attrset.scope",
        ),
        (
            5,
            Some(4),
            SyntaxFactKind::Signature,
            "{ value = 1; }",
            0,
            4,
            "nix.selection_base_set.expression",
        ),
        (
            6,
            Some(4),
            SyntaxFactKind::Declaration,
            "value = 1;",
            0,
            4,
            "nix.variable.declaration",
        ),
        (
            7,
            Some(6),
            SyntaxFactKind::Occurrence,
            "value",
            0,
            5,
            "nix.binding_name.definition",
        ),
        (
            8,
            Some(3),
            SyntaxFactKind::Occurrence,
            "value",
            1,
            3,
            "nix.selected_attribute.reference",
        ),
    ]
    .into_iter()
    .map(|(id, parent, kind, text, nth, depth, syntax)| {
        SyntaxFact::new(
            id,
            parent,
            kind,
            span_in(nix, &source, text, nth),
            depth,
            label(syntax),
        )
    })
    .collect();
    for host in ["nix", "markdown"] {
        let language = LanguageId::new(host).unwrap();
        let output = analyze_custom(
            &snapshot,
            &source,
            language.clone(),
            &limits(IrLimits::default()),
            facts.clone(),
        )
        .unwrap();
        assert_eq!(
            output
                .document()
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
                .count(),
            1
        );
        let mut ir = IrLimits::default();
        ir.max_relations = 2;
        let error = analyze_custom(&snapshot, &source, language, &limits(ir), facts.clone())
            .expect_err("selected reference must be reserved before materialization");
        assert!(
            matches!(
                error,
                AdapterError::Sink(SinkError::StreamLimit {
                    resource: rootlight_adapter_sdk::ResourceKind::Records,
                    observed: 3,
                    limit: 2
                })
            ),
            "{error:?}"
        );
    }
}

fn assert_nix_lexical_quotas(nix: &str, declaration: &str, definition: &str) {
    let (_temporary, snapshot, source) =
        source_fixture_for(nix, "src/module.nix", b"nix-lexical-quota-fixture");
    let facts = [
        (1, None, SyntaxFactKind::Root, nix, 0, 0, "nix.file.root"),
        (
            2,
            Some(1),
            SyntaxFactKind::Module,
            nix,
            0,
            1,
            "nix.file.module",
        ),
        (
            3,
            Some(2),
            SyntaxFactKind::Scope,
            nix,
            0,
            2,
            "nix.let.scope",
        ),
        (
            4,
            Some(3),
            SyntaxFactKind::Declaration,
            declaration,
            0,
            3,
            "nix.variable.declaration",
        ),
        (
            5,
            Some(4),
            SyntaxFactKind::Occurrence,
            definition,
            0,
            4,
            "nix.binding_name.definition",
        ),
        (
            6,
            Some(3),
            SyntaxFactKind::Occurrence,
            "value",
            1,
            3,
            "nix.identifier.reference",
        ),
    ]
    .into_iter()
    .map(|(id, parent, kind, text, nth, depth, syntax)| {
        SyntaxFact::new(
            id,
            parent,
            kind,
            span_in(nix, &source, text, nth),
            depth,
            label(syntax),
        )
    })
    .collect::<Vec<_>>();
    for host in ["nix", "markdown"] {
        let language = LanguageId::new(host).unwrap();
        let output = analyze_custom(
            &snapshot,
            &source,
            language.clone(),
            &limits(IrLimits::default()),
            facts.clone(),
        )
        .unwrap();
        assert_eq!(
            output
                .document()
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
                .count(),
            1
        );
        for quota in ["relations", "gaps", "total"] {
            let mut ir = IrLimits::default();
            let (observed, limit) = match quota {
                "relations" => {
                    ir.max_relations = 2;
                    (3, 2)
                }
                "gaps" => {
                    ir.max_skipped_regions = 3;
                    (4, 3)
                }
                _ => {
                    ir.max_total_records = 24;
                    (25, 24)
                }
            };
            let error = analyze_custom(
                &snapshot,
                &source,
                language.clone(),
                &limits(ir),
                facts.clone(),
            )
            .expect_err("Nix relation and uncertainty require preflight reservations");
            assert!(
                matches!(error, AdapterError::Sink(SinkError::StreamLimit {
                resource: rootlight_adapter_sdk::ResourceKind::Records, observed: actual, limit: maximum,
            }) if actual == observed && maximum == limit),
                "{quota}: {error:?}"
            );
        }
    }
}

#[test]
fn rust_impl_without_reviewed_owner_capture_becomes_an_explicit_gap() {
    const RUST: &str = "impl A { fn same(&self) {} }\n";
    let (_temporary, snapshot, source) =
        source_fixture_for(RUST, "src/lib.rs", b"missing-impl-owner-fixture");
    let facts = vec![
        SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.span(),
            0,
            label("rust.file.root"),
        ),
        SyntaxFact::new(
            2,
            Some(1),
            SyntaxFactKind::Scope,
            span_in(RUST, &source, RUST.trim_end(), 0),
            1,
            label("rust.impl.scope"),
        ),
        SyntaxFact::new(
            3,
            Some(2),
            SyntaxFactKind::Declaration,
            span_in(RUST, &source, "fn same(&self) {}", 0),
            2,
            label("rust.function.declaration"),
        ),
        SyntaxFact::new(
            4,
            Some(3),
            SyntaxFactKind::Occurrence,
            span_in(RUST, &source, "same", 0),
            3,
            label("rust.function.definition"),
        ),
    ];
    let output = analyze_custom(
        &snapshot,
        &source,
        language(),
        &limits(IrLimits::default()),
        facts,
    )
    .expect("unsupported impl identity commits an explicit partial document");

    assert!(
        output
            .document()
            .entities
            .iter()
            .all(|entity| entity.canonical_name != "same")
    );
    assert!(output.document().skipped_regions.iter().any(|region| {
        region.domain == rootlight_ir::FactDomain::Entities
            && region.detail == "stable-scope-identity-unavailable"
    }));
}

#[test]
fn lowering_emits_only_evidence_backed_conservative_relations() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let output = analyze(
        &snapshot,
        &source,
        &limits,
        facts(
            &source,
            LocalIds {
                root: 1,
                module: 2,
                comment: 3,
                function: 4,
                call: 5,
                import: 6,
            },
            false,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("lowering commits");
    let document = output.document();

    assert_eq!(document.entities.len(), 2);
    let function = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "alpha")
        .expect("definition capture names the function");
    assert_eq!(
        function
            .evidence
            .source
            .as_ref()
            .expect("entity has direct definition evidence")
            .span(),
        span_for(&source, "pub fn alpha() { beta(); }")
    );
    assert!(document.occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && occurrence.source.span() == span_for_nth(&source, "alpha", 1)
            && occurrence.syntactic_text_hash == content_hash(b"alpha")
    }));
    assert!(document.occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::CallSite
            && occurrence.enclosing == Some(function.id)
            && matches!(
                occurrence.target,
                rootlight_ir::OccurrenceTarget::Unresolved { .. }
            )
    }));
    assert!(document.occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::ImportUse
            && matches!(
                occurrence.target,
                rootlight_ir::OccurrenceTarget::Unresolved { .. }
            )
    }));
    assert!(!document.relations.is_empty());
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate != RelationPredicate::Calls)
    );
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate != RelationPredicate::Imports)
    );
    assert!(document.skipped_regions.iter().any(|region| {
        region.domain == rootlight_ir::FactDomain::Relations
            && region.detail == "unresolved-import-target"
    }));

    assert_every_record_has_evidence(document);
    let lexical_kinds: Vec<_> = document
        .extensions
        .iter()
        .filter(|extension| extension.namespace == LEXICAL_EXTENSION_NAMESPACE)
        .map(|extension| {
            decode_lexical_evidence_envelope(extension)
                .expect("first-party lexical envelope validates")
                .kind()
        })
        .collect();
    assert!(lexical_kinds.contains(&LexicalEvidenceKind::Signature));
    assert!(lexical_kinds.contains(&LexicalEvidenceKind::DocumentationSummary));
}

#[test]
fn included_ranges_and_parser_recovery_remain_explicit_coverage_gaps() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let module = span_for(&source, SOURCE.trim_end());
    let call = span_for(&source, "beta()");
    let diagnostic = AdapterDiagnostic::new(
        DiagnosticCode::new("syntax-error-recovery").expect("diagnostic code is valid"),
        DiagnosticSeverity::Warning,
        Some(source_for_span(&source, call)),
        CoverageStatus::Unknown,
    );
    let covered = usize::try_from(module.end_byte() - module.start_byte())
        .expect("fixture range length fits");
    let provider = provider(
        facts(
            &source,
            LocalIds {
                root: 1,
                module: 2,
                comment: 3,
                function: 4,
                call: 5,
                import: 6,
            },
            false,
        ),
        vec![diagnostic],
        CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Unknown,
            SOURCE.len(),
            covered,
            1,
            Vec::new(),
        )
        .expect("partial coverage is valid"),
    );
    let analyzer = analyzer(provider, &source);
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).expect("snapshot binds"),
        language(),
        EncodingId::utf8(),
        vec![IncludedRange::new(module, language())],
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"build-context")),
        &limits,
    )
    .expect("included-range analysis request is valid")
    .with_generated_status(false);
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("partial lowering commits");

    assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
    assert!(output.report().coverage().skipped_regions() >= 2);
    assert_eq!(output.document().diagnostics.len(), 1);
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|region| { region.detail == "outside-included-ranges" })
    );
    assert!(
        output
            .document()
            .skipped_regions
            .iter()
            .any(|region| { region.detail == "syntax-error-recovery" })
    );
    let file_coverage = output
        .report()
        .coverage()
        .domains()
        .iter()
        .find(|domain| domain.domain() == rootlight_ir::FactDomain::Files)
        .expect("file coverage is reported");
    let diagnostic_coverage = output
        .report()
        .coverage()
        .domains()
        .iter()
        .find(|domain| domain.domain() == rootlight_ir::FactDomain::Diagnostics)
        .expect("diagnostic coverage is reported");
    assert_eq!(file_coverage.status(), CoverageStatus::Unknown);
    assert_eq!(file_coverage.skipped(), 1);
    assert_eq!(diagnostic_coverage.status(), CoverageStatus::Unknown);
    assert_eq!(diagnostic_coverage.skipped(), 1);
}

#[test]
fn canonical_document_round_trips_through_bounded_decoder() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let output = analyze(
        &snapshot,
        &source,
        &limits,
        facts(
            &source,
            LocalIds {
                root: 1,
                module: 2,
                comment: 3,
                function: 4,
                call: 5,
                import: 6,
            },
            false,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect("lowering commits");
    let encoded = serde_json::to_vec(output.document()).expect("canonical document encodes");
    let decoded = decode_ir_document(&encoded, limits.ir(), &ExtensionSupport::default())
        .expect("bounded decoder accepts lowering output");

    assert_eq!(
        decoded,
        IrDocument::NormalizedV1_1(output.document().clone())
    );
}

#[test]
fn cancellation_and_ir_limits_abort_without_committed_output() {
    let (_temporary, snapshot, source) = source_fixture();
    let analysis_limits = limits(IrLimits::default());
    let provider = provider(
        facts(
            &source,
            LocalIds {
                root: 1,
                module: 2,
                comment: 3,
                function: 4,
                call: 5,
                import: 6,
            },
            false,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .with_cancellation_after_batches(0, CancellationReason::ClientRequest);
    let analyzer = analyzer(provider, &source);
    let request = request(&snapshot, &source, &analysis_limits);
    assert!(matches!(
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));

    let mut constrained_ir = IrLimits::default();
    constrained_ir.max_entities = 1;
    let constrained_limits = limits(constrained_ir);
    let error = analyze(
        &snapshot,
        &source,
        &constrained_limits,
        facts(
            &source,
            LocalIds {
                root: 11,
                module: 12,
                comment: 13,
                function: 14,
                call: 15,
                import: 16,
            },
            false,
        ),
        Vec::new(),
        complete_coverage(SOURCE.len()),
    )
    .expect_err("entity quota aborts the transaction");
    assert!(matches!(
        error,
        AdapterError::Sink(SinkError::StreamLimit { .. })
    ));
}

#[test]
fn malformed_fact_errors_and_analyzer_debug_are_source_free() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let function = span_for(&source, "pub fn alpha() { beta(); }");
    let malformed = SyntaxFact::new(
        7,
        Some(999),
        SyntaxFactKind::Declaration,
        function,
        1,
        SyntaxKindLabel::new("function_item").expect("syntax label is valid"),
    );
    let provider = provider(vec![malformed], Vec::new(), complete_coverage(SOURCE.len()));
    let analyzer = analyzer(provider, &source);
    let request = request(&snapshot, &source, &limits);
    let error = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect_err("missing parent is rejected");

    assert!(matches!(error, AdapterError::ProviderFailed { .. }));
    assert!(!format!("{error:?}").contains("docs for alpha"));
    assert!(!format!("{analyzer:?}").contains("docs for alpha"));
}

#[test]
fn non_utf8_analysis_identity_is_rejected_before_parser_execution() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let provider = provider(Vec::new(), Vec::new(), complete_coverage(SOURCE.len()));
    let analyzer = analyzer(provider, &source);
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).expect("snapshot binds"),
        language(),
        EncodingId::new("utf-16").expect("test encoding label is valid"),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"build-context")),
        &limits,
    )
    .expect("analysis request carries an explicit encoding")
    .with_generated_status(false);

    assert_eq!(
        execute_analysis(
            &analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::UnsupportedEncoding
        ))
    );
}

#[test]
fn analyzer_requires_source_classification_and_records_exact_frontend() {
    let (_temporary, snapshot, source) = source_fixture();
    let limits = limits(IrLimits::default());
    let provider = provider(Vec::new(), Vec::new(), complete_coverage(SOURCE.len()));
    let analyzer = analyzer(provider, &source);
    let unclassified = AnalysisRequest::new(
        GenerationBoundSnapshot::new(&snapshot, &source).expect("snapshot binds"),
        language(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"build-context")),
        &limits,
    )
    .expect("compatibility request remains constructible");

    assert_eq!(
        analyzer.descriptor().memory_enforcement(),
        MemoryEnforcement::Unavailable
    );
    assert_eq!(
        execute_analysis(
            &analyzer,
            &unclassified,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        ),
        Err(AdapterError::RejectedRequest(
            RequestError::GeneratedStatusRequired
        ))
    );

    let classified = unclassified.with_generated_status(true);
    let output = execute_analysis(
        &analyzer,
        &classified,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("classified request lowers");
    assert!(output.document().files[0].generated);
    assert_eq!(
        output.document().provenance[0].frontend_version.as_deref(),
        Some("tree-sitter-rust-0.24.2")
    );
}

#[test]
fn annotated_java_uses_definition_and_signature_captures_only() {
    const JAVA: &str =
        "@interface Marker { String value(); } class Example { @Override() void foo() {} }\n";
    let (_temporary, snapshot, source) =
        source_fixture_for(JAVA, "src/Example.java", b"java-lowering-fixture");
    let limits = limits(IrLimits::default());
    let facts = vec![
        SyntaxFact::new(
            1,
            None,
            SyntaxFactKind::Root,
            source.span(),
            0,
            label("java.source.root"),
        ),
        SyntaxFact::new(
            2,
            Some(1),
            SyntaxFactKind::Declaration,
            span_in(JAVA, &source, "@interface Marker { String value(); }", 0),
            1,
            label("java.annotation.declaration"),
        ),
        SyntaxFact::new(
            3,
            Some(2),
            SyntaxFactKind::Occurrence,
            span_in(JAVA, &source, "Marker", 0),
            2,
            label("java.annotation.definition"),
        ),
        SyntaxFact::new(
            4,
            Some(2),
            SyntaxFactKind::Declaration,
            span_in(JAVA, &source, "String value()", 0),
            2,
            label("java.annotation_element.declaration"),
        ),
        SyntaxFact::new(
            5,
            Some(4),
            SyntaxFactKind::Occurrence,
            span_in(JAVA, &source, "value", 0),
            3,
            label("java.annotation_element.definition"),
        ),
        SyntaxFact::new(
            6,
            Some(4),
            SyntaxFactKind::Signature,
            span_in(JAVA, &source, "()", 0),
            3,
            label("java.annotation_element.signature"),
        ),
        SyntaxFact::new(
            7,
            Some(1),
            SyntaxFactKind::Declaration,
            span_in(
                JAVA,
                &source,
                "class Example { @Override() void foo() {} }",
                0,
            ),
            1,
            label("java.class.declaration"),
        ),
        SyntaxFact::new(
            8,
            Some(7),
            SyntaxFactKind::Occurrence,
            span_in(JAVA, &source, "Example", 0),
            2,
            label("java.class.definition"),
        ),
        SyntaxFact::new(
            9,
            Some(7),
            SyntaxFactKind::Declaration,
            span_in(JAVA, &source, "@Override() void foo() {}", 0),
            2,
            label("java.method.declaration"),
        ),
        SyntaxFact::new(
            10,
            Some(9),
            SyntaxFactKind::Occurrence,
            span_in(JAVA, &source, "foo", 0),
            3,
            label("java.method.definition"),
        ),
        SyntaxFact::new(
            11,
            Some(9),
            SyntaxFactKind::Signature,
            span_in(JAVA, &source, "()", 2),
            3,
            label("java.method.signature"),
        ),
        SyntaxFact::new(
            12,
            Some(9),
            SyntaxFactKind::Occurrence,
            span_in(JAVA, &source, "Override", 0),
            3,
            label("java.annotation.reference"),
        ),
    ];

    let output = analyze_custom(
        &snapshot,
        &source,
        LanguageId::new("java").expect("Java language is valid"),
        &limits,
        facts,
    )
    .expect("annotated Java lowers");
    let names: Vec<_> = output
        .document()
        .entities
        .iter()
        .map(|entity| entity.canonical_name.as_str())
        .collect();
    assert!(names.contains(&"Example"));
    assert!(names.contains(&"foo"));
    assert!(names.contains(&"Marker"));
    assert!(names.contains(&"value"));
    assert!(!names.contains(&"Override"));
    assert!(output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && occurrence.source.span() == span_in(JAVA, &source, "foo", 0)
            && occurrence.syntactic_text_hash == content_hash(b"foo")
    }));
}

#[test]
fn overloads_with_distinct_signature_captures_keep_distinct_symbols() {
    const OVERLOADS: &str = "fn alpha(x: i32) {}\nfn alpha(x: u64) {}\n";
    let (_temporary, snapshot, source) =
        source_fixture_for(OVERLOADS, "src/lib.rs", b"overload-lowering-fixture");
    let limits = limits(IrLimits::default());
    let mut facts = vec![SyntaxFact::new(
        1,
        None,
        SyntaxFactKind::Root,
        source.span(),
        0,
        label("rust.source.root"),
    )];
    for (offset, signature) in ["(x: i32)", "(x: u64)"].into_iter().enumerate() {
        let local = u64::try_from(offset)
            .expect("fixture offset fits")
            .checked_mul(10)
            .and_then(|value| value.checked_add(2))
            .expect("fixture local ID fits");
        let declaration_text = if offset == 0 {
            "fn alpha(x: i32) {}"
        } else {
            "fn alpha(x: u64) {}"
        };
        facts.extend([
            SyntaxFact::new(
                local,
                Some(1),
                SyntaxFactKind::Declaration,
                span_in(OVERLOADS, &source, declaration_text, 0),
                1,
                label("rust.function.declaration"),
            ),
            SyntaxFact::new(
                local + 1,
                Some(local),
                SyntaxFactKind::Occurrence,
                span_in(OVERLOADS, &source, "alpha", offset),
                2,
                label("rust.function.definition"),
            ),
            SyntaxFact::new(
                local + 2,
                Some(local),
                SyntaxFactKind::Signature,
                span_in(OVERLOADS, &source, signature, 0),
                2,
                label("rust.function.signature"),
            ),
        ]);
    }

    let output = analyze_custom(&snapshot, &source, language(), &limits, facts)
        .expect("overloads lower from exact captures");
    assert_eq!(output.document().entities.len(), 2);
    assert_eq!(output.document().entities[0].canonical_name, "alpha");
    assert_eq!(output.document().entities[1].canonical_name, "alpha");
    assert_ne!(
        output.document().entities[0].id,
        output.document().entities[1].id
    );
}

#[test]
fn python_and_javascript_file_modules_use_repository_paths() {
    for (language_name, module_label, path, source_text, repository_seed) in [
        (
            "python",
            "python.file.module",
            "src/main.py",
            "print('x')\n",
            b"python-file-module".as_slice(),
        ),
        (
            "javascript",
            "javascript.file.module",
            "src/main.js",
            "console.log('x');\n",
            b"javascript-file-module".as_slice(),
        ),
    ] {
        let (_temporary, snapshot, source) = source_fixture_for(source_text, path, repository_seed);
        let limits = limits(IrLimits::default());
        let facts = vec![
            SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Root,
                source.span(),
                0,
                label("source.root"),
            ),
            SyntaxFact::new(
                2,
                Some(1),
                SyntaxFactKind::Module,
                source.span(),
                1,
                label(module_label),
            ),
        ];
        let output = analyze_custom(
            &snapshot,
            &source,
            LanguageId::new(language_name).expect("fixture language is valid"),
            &limits,
            facts,
        )
        .expect("file module lowers from explicit path rule");

        assert_eq!(output.document().entities.len(), 1);
        assert_eq!(output.document().entities[0].canonical_name, path);
        assert_eq!(
            output.document().entities[0].kind,
            rootlight_ir::EntityKind::Module
        );
        assert_eq!(
            output.document().entities[0].flags,
            vec![EntityFlag::Synthetic]
        );
        assert!(
            output.document().occurrences.is_empty(),
            "an implicit file module has no declaration spelling"
        );
    }
}

#[test]
fn oversized_lexical_captures_truncate_or_become_explicit_gaps() {
    let comment_text = format!("// {}", "a".repeat(700));
    let source_text = format!("{comment_text}\n");
    let (_temporary, snapshot, source) =
        source_fixture_for(&source_text, "src/lib.rs", b"large-comment-fixture");
    let default_limits = limits(IrLimits::default());
    let comment_output = analyze_custom(
        &snapshot,
        &source,
        language(),
        &default_limits,
        vec![
            SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Root,
                source.span(),
                0,
                label("rust.source.root"),
            ),
            SyntaxFact::new(
                2,
                Some(1),
                SyntaxFactKind::Comment,
                span_in(&source_text, &source, &comment_text, 0),
                1,
                label("rust.comment"),
            ),
        ],
    )
    .expect("large comment uses bounded lexical truncation");
    let evidence = decode_lexical_evidence_envelope(&comment_output.document().extensions[0])
        .expect("truncated comment evidence validates");
    assert!(evidence.is_truncated());

    let parameters = format!("({})", "parameter_name: i32,".repeat(20));
    let declaration_text = format!("fn alpha{parameters} {{}}\n");
    let (_temporary, snapshot, source) =
        source_fixture_for(&declaration_text, "src/lib.rs", b"large-signature-fixture");
    let mut ir = IrLimits::default();
    ir.max_string_bytes = 100;
    let constrained = limits(ir);
    let signature_output = analyze_custom(
        &snapshot,
        &source,
        language(),
        &constrained,
        vec![
            SyntaxFact::new(
                1,
                None,
                SyntaxFactKind::Root,
                source.span(),
                0,
                label("rust.source.root"),
            ),
            SyntaxFact::new(
                2,
                Some(1),
                SyntaxFactKind::Declaration,
                span_in(&declaration_text, &source, declaration_text.trim_end(), 0),
                1,
                label("rust.function.declaration"),
            ),
            SyntaxFact::new(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                span_in(&declaration_text, &source, "alpha", 0),
                2,
                label("rust.function.definition"),
            ),
            SyntaxFact::new(
                4,
                Some(2),
                SyntaxFactKind::Signature,
                span_in(&declaration_text, &source, &parameters, 0),
                2,
                label("rust.function.signature"),
            ),
        ],
    )
    .expect("oversized signature is omitted without failing analysis");
    assert!(
        signature_output
            .document()
            .extensions
            .iter()
            .all(|extension| extension.namespace != LEXICAL_EXTENSION_NAMESPACE)
    );
    assert!(
        signature_output
            .document()
            .skipped_regions
            .iter()
            .any(|region| region.detail == "signature-capture-unavailable")
    );
}

#[test]
fn symbol_ids_survive_body_crlf_changes_and_incremental_style_reparse() {
    const BEFORE: &str = "fn alpha() {\n    beta();\n}\n";
    const AFTER: &str = "fn alpha() {\r\n        beta();   \r\n}\r\n";
    let current = std::env::current_dir().expect("current directory is available");
    let temporary = tempdir_in(current).expect("local temporary directory is available");
    fs::create_dir(temporary.path().join("src")).expect("fixture directory is created");
    let absolute = temporary.path().join("src").join("lib.rs");
    fs::write(&absolute, BEFORE).expect("initial fixture is written");
    let repository_id = derive_repository(b"incremental-lowering-fixture").id();
    let repository =
        RepositoryRoot::open(repository_id, temporary.path()).expect("temporary root opens");
    let path = RelativePath::parse(Path::new("src/lib.rs")).expect("fixture path is valid");
    let before_snapshot = repository
        .snapshot(&path, 4096)
        .expect("initial snapshot is stable");
    let before_source = source_ref_for_snapshot(repository_id, &before_snapshot);

    fs::write(&absolute, AFTER).expect("edited fixture is written");
    let after_snapshot = repository
        .snapshot(&path, 4096)
        .expect("edited snapshot is stable");
    let after_source = source_ref_for_snapshot(repository_id, &after_snapshot);
    assert_eq!(before_source.span().file(), after_source.span().file());

    let limits = limits(IrLimits::default());
    let before = analyze_custom(
        &before_snapshot,
        &before_source,
        language(),
        &limits,
        single_function_facts(BEFORE, &before_source, 10, false),
    )
    .expect("initial syntax facts lower");
    let after = analyze_custom(
        &after_snapshot,
        &after_source,
        language(),
        &limits,
        single_function_facts(AFTER, &after_source, 900, true),
    )
    .expect("incremental-style facts lower after CRLF body edit");

    assert_eq!(before.document().entities.len(), 1);
    assert_eq!(after.document().entities.len(), 1);
    assert_eq!(
        before.document().entities[0].id,
        after.document().entities[0].id
    );
}

fn analyze(
    snapshot: &SourceSnapshot,
    source: &SourceRef,
    limits: &AnalysisLimits,
    facts: Vec<SyntaxFact>,
    diagnostics: Vec<AdapterDiagnostic>,
    coverage: CoverageReport,
) -> Result<rootlight_adapter_sdk::AnalysisOutput, AdapterError> {
    let analyzer = analyzer(provider(facts, diagnostics, coverage), source);
    execute_analysis(
        &analyzer,
        &request(snapshot, source, limits),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
}

fn analyzer(provider: MockParseProvider, source: &SourceRef) -> TreeSitterAnalyzer {
    TreeSitterAnalyzer::new(
        Arc::new(provider),
        ProducerIdentity::new(
            "rootlight-treesitter-lowering",
            "1.0",
            content_hash(b"lowering-configuration"),
        )
        .expect("producer identity is valid"),
        language(),
        "tree-sitter-rust-0.24.2",
        content_hash(source.content_hash().as_bytes()),
    )
    .expect("analyzer configuration is valid")
}

fn provider(
    facts: Vec<SyntaxFact>,
    diagnostics: Vec<AdapterDiagnostic>,
    coverage: CoverageReport,
) -> MockParseProvider {
    MockParseProvider::new(capabilities(), facts, diagnostics, coverage)
}

fn capabilities() -> ParseCapabilities {
    ParseCapabilities::new(
        vec![language()],
        vec![EncodingId::utf8()],
        4096,
        4096,
        64,
        8,
        true,
        true,
        true,
        4,
        MemoryEnforcement::AccountedInProcess,
    )
    .expect("parser capabilities are valid")
}

fn facts(source: &SourceRef, ids: LocalIds, reverse: bool) -> Vec<SyntaxFact> {
    let root = SyntaxFact::new(
        ids.root,
        None,
        SyntaxFactKind::Root,
        source.span(),
        0,
        label("source_file"),
    );
    let module_span = span_for(source, SOURCE.trim_end());
    let module = SyntaxFact::new(
        ids.module,
        Some(ids.root),
        SyntaxFactKind::Module,
        module_span,
        1,
        label("rust.module.declaration"),
    );
    let module_definition = SyntaxFact::new(
        ids.module.wrapping_add(1_000_000),
        Some(ids.module),
        SyntaxFactKind::Occurrence,
        span_for(source, "api"),
        2,
        label("rust.module.definition"),
    );
    let comment = SyntaxFact::new(
        ids.comment,
        Some(ids.module),
        SyntaxFactKind::Comment,
        span_for(source, "/// docs for alpha"),
        2,
        label("doc_comment"),
    );
    let function = SyntaxFact::new(
        ids.function,
        Some(ids.module),
        SyntaxFactKind::Declaration,
        span_for(source, "pub fn alpha() { beta(); }"),
        2,
        label("rust.function.declaration"),
    );
    let function_definition = SyntaxFact::new(
        ids.function.wrapping_add(2_000_000),
        Some(ids.function),
        SyntaxFactKind::Occurrence,
        span_for_nth(source, "alpha", 1),
        3,
        label("rust.function.definition"),
    );
    let function_signature = SyntaxFact::new(
        ids.function.wrapping_add(3_000_000),
        Some(ids.function),
        SyntaxFactKind::Signature,
        span_for(source, "()"),
        3,
        label("rust.function.signature"),
    );
    let scope = SyntaxFact::new(
        ids.function.wrapping_add(4_000_000),
        Some(ids.function),
        SyntaxFactKind::Scope,
        span_for(source, "{ beta(); }"),
        3,
        label("rust.block.scope"),
    );
    let call = SyntaxFact::new(
        ids.call,
        Some(ids.function.wrapping_add(4_000_000)),
        SyntaxFactKind::Occurrence,
        span_for(source, "beta()"),
        4,
        label("rust.call.reference"),
    );
    let import = SyntaxFact::new(
        ids.import,
        Some(ids.module),
        SyntaxFactKind::Import,
        span_for(source, "use crate::dep;"),
        2,
        label("use_declaration"),
    );
    let mut facts = vec![
        root,
        module,
        module_definition,
        comment,
        function,
        function_definition,
        function_signature,
        scope,
        call,
        import,
    ];
    if reverse {
        facts.reverse();
    }
    facts
}

fn assert_every_record_has_evidence(document: &rootlight_ir::NormalizedIrDocument) {
    assert!(
        document
            .files
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .entities
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .occurrences
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .relations
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .coverage_records
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .skipped_regions
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .diagnostics
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(
        document
            .extensions
            .iter()
            .all(|record| has_evidence(&record.evidence))
    );
    assert!(document.provenance.iter().all(|record| {
        !record.input_sources.is_empty()
            || !record.evidence_sources.is_empty()
            || !record.derivation_parents.is_empty()
    }));
}

fn has_evidence(evidence: &FactEvidence) -> bool {
    evidence.source.is_some() || !evidence.derivation.is_empty()
}

fn source_fixture() -> (TempDir, SourceSnapshot, SourceRef) {
    source_fixture_for(SOURCE, "src/lib.rs", b"treesitter-lowering-fixture")
}

fn source_fixture_for(
    source_text: &str,
    relative_path: &str,
    repository_seed: &[u8],
) -> (TempDir, SourceSnapshot, SourceRef) {
    let current = std::env::current_dir().expect("current directory is available");
    let temporary = tempdir_in(current).expect("local temporary directory is available");
    let relative = Path::new(relative_path);
    if let Some(parent) = relative.parent() {
        fs::create_dir_all(temporary.path().join(parent))
            .expect("fixture source directory is created");
    }
    fs::write(temporary.path().join(relative), source_text).expect("fixture source is written");
    let repository_id = derive_repository(repository_seed).id();
    let repository =
        RepositoryRoot::open(repository_id, temporary.path()).expect("temporary root opens");
    let path = RelativePath::parse(relative).expect("fixture relative path is valid");
    let snapshot = repository
        .snapshot(&path, 4096)
        .expect("fixture snapshot is stable");
    let byte_length = u64::try_from(snapshot.content().len()).expect("fixture length fits");
    let span = SourceSpan::new(snapshot.file(), 0, byte_length).expect("full-file span is ordered");
    let source = SourceRef::new(
        repository_id,
        GenerationId::from_bytes([7; 20]),
        span,
        snapshot.content_hash(),
        None,
    );
    (temporary, snapshot, source)
}

fn analyze_custom(
    snapshot: &SourceSnapshot,
    source: &SourceRef,
    language: LanguageId,
    limits: &AnalysisLimits,
    facts: Vec<SyntaxFact>,
) -> Result<rootlight_adapter_sdk::AnalysisOutput, AdapterError> {
    let analyzer = custom_analyzer(snapshot, language.clone(), facts);
    let request = AnalysisRequest::new(
        GenerationBoundSnapshot::new(snapshot, source).expect("custom snapshot binds"),
        language,
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"build-context")),
        limits,
    )
    .expect("custom analysis request is valid")
    .with_generated_status(false);
    execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
}

fn custom_analyzer(
    snapshot: &SourceSnapshot,
    language: LanguageId,
    facts: Vec<SyntaxFact>,
) -> TreeSitterAnalyzer {
    custom_analyzer_with_coverage(language, facts, complete_coverage(snapshot.content().len()))
}

fn custom_analyzer_with_coverage(
    language: LanguageId,
    facts: Vec<SyntaxFact>,
    coverage: CoverageReport,
) -> TreeSitterAnalyzer {
    let capabilities = ParseCapabilities::new(
        vec![language.clone()],
        vec![EncodingId::utf8()],
        4096,
        4096,
        64,
        8,
        true,
        true,
        true,
        4,
        MemoryEnforcement::AccountedInProcess,
    )
    .expect("custom parser capabilities are valid");
    let provider = MockParseProvider::new(capabilities, facts, Vec::new(), coverage);
    TreeSitterAnalyzer::new(
        Arc::new(provider),
        ProducerIdentity::new(
            "rootlight-treesitter-lowering",
            "1.0",
            content_hash(b"lowering-configuration"),
        )
        .expect("producer identity is valid"),
        language,
        "tree-sitter-fixture-1.0",
        content_hash(b"fixture-binary"),
    )
    .expect("custom analyzer is valid")
}

fn source_ref_for_snapshot(
    repository: rootlight_ids::RepositoryId,
    snapshot: &SourceSnapshot,
) -> SourceRef {
    let byte_length = u64::try_from(snapshot.content().len()).expect("fixture length fits");
    SourceRef::new(
        repository,
        GenerationId::from_bytes([7; 20]),
        SourceSpan::new(snapshot.file(), 0, byte_length).expect("full-file span is ordered"),
        snapshot.content_hash(),
        None,
    )
}

fn single_function_facts(
    source_text: &str,
    source: &SourceRef,
    first_local_id: u64,
    reverse: bool,
) -> Vec<SyntaxFact> {
    let root = first_local_id;
    let declaration = first_local_id.wrapping_add(1);
    let mut facts = vec![
        SyntaxFact::new(
            root,
            None,
            SyntaxFactKind::Root,
            source.span(),
            0,
            label("rust.source.root"),
        ),
        SyntaxFact::new(
            declaration,
            Some(root),
            SyntaxFactKind::Declaration,
            span_in(source_text, source, source_text.trim_end(), 0),
            1,
            label("rust.function.declaration"),
        ),
        SyntaxFact::new(
            first_local_id.wrapping_add(2),
            Some(declaration),
            SyntaxFactKind::Occurrence,
            span_in(source_text, source, "alpha", 0),
            2,
            label("rust.function.definition"),
        ),
        SyntaxFact::new(
            first_local_id.wrapping_add(3),
            Some(declaration),
            SyntaxFactKind::Signature,
            span_in(source_text, source, "()", 0),
            2,
            label("rust.function.signature"),
        ),
    ];
    if reverse {
        facts.reverse();
    }
    facts
}

fn request<'a>(
    snapshot: &'a SourceSnapshot,
    source: &SourceRef,
    limits: &'a AnalysisLimits,
) -> AnalysisRequest<'a> {
    AnalysisRequest::new(
        GenerationBoundSnapshot::new(snapshot, source).expect("snapshot binds"),
        language(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"build-context")),
        limits,
    )
    .expect("analysis request is valid")
    .with_generated_status(false)
}

fn limits(ir: IrLimits) -> AnalysisLimits {
    let batch =
        BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).expect("batch thresholds are valid");
    let stream = StreamLimits::new(
        128,
        1024,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        batch,
    )
    .expect("stream limits are valid");
    AnalysisLimits::new(
        4096,
        4096,
        64,
        8,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        ir,
    )
    .expect("analysis limits are valid")
}

fn complete_coverage(source_bytes: usize) -> CoverageReport {
    CoverageReport::new(
        AnalysisTier::TierD,
        CoverageStatus::Complete,
        source_bytes,
        source_bytes,
        0,
        Vec::new(),
    )
    .expect("complete coverage is valid")
}

fn span_for(source: &SourceRef, needle: &str) -> SourceSpan {
    let start = SOURCE.find(needle).expect("fixture needle exists");
    let end = start
        .checked_add(needle.len())
        .expect("fixture span does not overflow");
    SourceSpan::new(
        source.span().file(),
        u64::try_from(start).expect("fixture start fits"),
        u64::try_from(end).expect("fixture end fits"),
    )
    .expect("fixture span is ordered")
}

fn span_for_nth(source: &SourceRef, needle: &str, index: usize) -> SourceSpan {
    span_in(SOURCE, source, needle, index)
}

fn span_in(source_text: &str, source: &SourceRef, needle: &str, index: usize) -> SourceSpan {
    let start = source_text
        .match_indices(needle)
        .nth(index)
        .map(|(start, _)| start)
        .expect("fixture occurrence exists");
    let end = start
        .checked_add(needle.len())
        .expect("fixture span does not overflow");
    SourceSpan::new(
        source.span().file(),
        u64::try_from(start).expect("fixture start fits"),
        u64::try_from(end).expect("fixture end fits"),
    )
    .expect("fixture span is ordered")
}

fn source_for_span(source: &SourceRef, span: SourceSpan) -> SourceRef {
    SourceRef::new(
        source.repository(),
        source.generation(),
        span,
        source.content_hash(),
        None,
    )
}

fn label(value: &str) -> SyntaxKindLabel {
    SyntaxKindLabel::new(value).expect("fixture syntax label is valid")
}

fn language() -> LanguageId {
    LanguageId::new("rust").expect("fixture language is valid")
}

fn deadline() -> Cancellation {
    Cancellation::with_deadline(
        Instant::now()
            .checked_add(Duration::from_secs(30))
            .expect("test deadline derives"),
    )
}
