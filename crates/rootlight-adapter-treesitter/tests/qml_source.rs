//! QML original obligations retain their authored kinds and physical witnesses.
//! Native receiver barriers must survive both adapter and generic resolution.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, IrLimits,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const SOURCE: &str = include_str!("fixtures/qml/SettingsUiState.qml");

fn analyze(text: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    analyze_raw(text.as_bytes(), "utf-8")
}

fn analyze_raw(raw: &[u8], encoding: &str) -> rootlight_adapter_sdk::AnalysisOutput {
    execute(raw, encoding, 4096).unwrap().0
}

fn execute(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    execute_with_cancel(raw, encoding, maximum, &Cancellation::new())
}

fn execute_with_cancel(
    raw: &[u8],
    encoding: &str,
    maximum: usize,
    cancel: &Cancellation,
) -> Result<(rootlight_adapter_sdk::AnalysisOutput, usize), AdapterError> {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.qml"), raw).unwrap();
    let repository = derive_repository(b"qml-written-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.qml")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(raw.len()).unwrap()).unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(
        TreeSitterProvider::new(
            RuntimeConfig::new(
                1024 * 1024,
                16_384,
                128,
                16,
                64,
                1,
                16 * 1024 * 1024,
                ParserSettings::new(256).unwrap(),
            )
            .unwrap(),
        )
        .unwrap(),
    );
    let analyzer = TreeSitterAnalyzer::new(
        provider.clone(),
        ProducerIdentity::new(
            "qml-source-contract",
            "1.0",
            content_hash(b"qml-contract-configuration"),
        )
        .unwrap(),
        LanguageId::new("qml").unwrap(),
        "qml-static-contract",
        content_hash(b"qml-contract-binary"),
    )
    .unwrap();
    let batch = BatchThresholds::new(maximum.min(64), 1024 * 1024, 32, 16 * 1024).unwrap();
    let stream = StreamLimits::new(
        128,
        maximum,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        batch,
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream,
        StreamLimits::new(
            128,
            4096,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            1024 * 1024,
            BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
        )
        .unwrap(),
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("qml").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"qml-contract-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let required = provider.required_syntax_fact_count(&request.to_parse_request(), cancel)?;
    let result = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancel,
    )?;
    validate_ir_document(result.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(result.document().files.len(), 1);
    assert_eq!(result.document().files[0].content_hash, content_hash(raw));
    assert_eq!(
        result.document().files[0].byte_length,
        u64::try_from(raw.len()).unwrap()
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
    for occurrence in &result.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), content_hash(raw));
        assert_eq!(occurrence.source.repository(), repository);
        assert_eq!(occurrence.source.generation(), source.generation());
        assert!(occurrence.source.span().end_byte() <= u64::try_from(raw.len()).unwrap());
    }
    Ok((result, required))
}

fn entity_at(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    name: &str,
    kind: EntityKind,
    start: usize,
    length: usize,
) -> rootlight_ids::SymbolId {
    let definitions = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                && occurrence.source.span().end_byte() == u64::try_from(start + length).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        definitions.len(),
        1,
        "{name}: {:?}",
        output.document().occurrences
    );
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("definition must be resolved")
    };
    let entity = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.display_name, name);
    assert_eq!(
        definitions[0].syntactic_text_hash,
        content_hash(name.as_bytes())
    );
    symbol
}

fn contains(
    output: &rootlight_adapter_sdk::AnalysisOutput,
    owner: rootlight_ids::SymbolId,
    member: rootlight_ids::SymbolId,
    name_start: usize,
    name_end: usize,
) {
    let matches = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(owner)
                && relation.object == RelationEndpoint::Entity(member)
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1);
    let relation = matches[0];
    let reference = relation.evidence.source.as_ref().unwrap();
    assert!(reference.span().start_byte() <= u64::try_from(name_start).unwrap());
    assert!(reference.span().end_byte() >= u64::try_from(name_end).unwrap());
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
}

#[test]
fn original_qml_four_definitions_and_three_relations_keep_exact_kinds_and_witnesses() {
    assert_eq!(SOURCE.len(), 502);
    let output = analyze(SOURCE);
    let root = entity_at(&output, "root", EntityKind::Variable, 96, 4);
    let property = entity_at(&output, "selectedBarId", EntityKind::Property, 124, 13);
    let method = entity_at(&output, "normalizeSelectedBar", EntityKind::Method, 165, 20);
    let handler = entity_at(&output, "onBarConfigsChanged", EntityKind::Method, 414, 19);
    contains(&output, root, property, 124, 137);
    contains(&output, root, method, 165, 185);
    let calls: Vec<_> = output
        .document()
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && edge.subject == RelationEndpoint::Entity(handler)
                && edge.object == RelationEndpoint::Entity(method)
        })
        .collect();
    assert_eq!(calls.len(), 1, "{:?}", output.document().relations);
    let witness = calls[0].evidence.source.as_ref().unwrap();
    assert_eq!(witness.span().start_byte(), 456);
    assert_eq!(witness.span().end_byte(), 476);
    assert_eq!(witness.content_hash(), content_hash(SOURCE.as_bytes()));
    assert_eq!(calls[0].id, derive_relation_record_id(calls[0]).unwrap());
    assert!(!output.document().occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && site.source.span().start_byte() == 456
            && site.source.span().end_byte() == 476
    }));
    assert!(
        !output.document().relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::Contains
                && edge.subject == RelationEndpoint::Entity(root)
                && edge.object == RelationEndpoint::Entity(handler)
        }),
        "anonymous Connections cannot donate the outer object as owner"
    );
    for detail in [
        "qml-signals-enums-components-unicode-and-general-javascript-declarations-unavailable",
        "qml-properties-parameters-locals-aliases-and-general-name-occurrences-unavailable",
        "qml-imported-types-implicit-components-dynamic-qt-runtime-and-general-binding-unavailable",
    ] {
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == detail)
        );
    }
}

fn simple(parameters: &str, body: &str, extra: &str) -> String {
    format!(
        "import QtQuick\nItem {{ id: root; function normalizeSelectedBar() {{}} Connections {{ function onBarConfigsChanged({parameters}) {{ {body} }} }} {extra} }}"
    )
}

fn calls(output: &rootlight_adapter_sdk::AnalysisOutput) -> Vec<&rootlight_ir::RelationRecord> {
    output
        .document()
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect()
}

#[test]
fn parameter_local_duplicate_id_and_method_barriers_never_become_resolved_calls() {
    for (parameters, body, extra) in [
        ("root", "root.normalizeSelectedBar();", ""),
        ("{root}", "root.normalizeSelectedBar();", ""),
        ("{nested: {root}}", "root.normalizeSelectedBar();", ""),
        ("[root]", "root.normalizeSelectedBar();", ""),
        ("root = other", "root.normalizeSelectedBar();", ""),
        ("{root} = other", "root.normalizeSelectedBar();", ""),
        ("...root", "root.normalizeSelectedBar();", ""),
        ("", "function inner() { root.normalizeSelectedBar(); }", ""),
        ("", "function* inner() { root.normalizeSelectedBar(); }", ""),
        (
            "",
            "const inner = () => { root.normalizeSelectedBar(); };",
            "",
        ),
        ("", "let root = other; root.normalizeSelectedBar();", ""),
        ("", "const {root} = other; root.normalizeSelectedBar();", ""),
        (
            "",
            "try {} catch(root) { root.normalizeSelectedBar(); }",
            "",
        ),
        (
            "",
            "for (let root of values) { root.normalizeSelectedBar(); }",
            "",
        ),
        ("", "root.normalizeSelectedBar();", "Item { id: root }"),
        (
            "",
            "root.normalizeSelectedBar();",
            "Item { function normalizeSelectedBar() {} }",
        ),
        ("", "root.normalizeSelectedBar();", "property var root"),
        (
            "",
            "root.normalizeSelectedBar = replacement; root.normalizeSelectedBar();",
            "",
        ),
        ("", "root = other; root.normalizeSelectedBar();", ""),
        ("", "root['normalizeSelectedBar']();", ""),
        ("", "root?.normalizeSelectedBar();", ""),
        ("", "other.normalizeSelectedBar();", ""),
        ("", "eval('replace'); root.normalizeSelectedBar();", ""),
    ] {
        let text = simple(parameters, body, extra);
        let output = analyze(&text);
        assert!(calls(&output).is_empty(), "{text}");
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "{text}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "qml-native-id-receiver-binding-unavailable"),
            "{text}"
        );
    }
}

#[test]
fn nested_ordinary_objects_preserve_component_id_access_and_direct_owners() {
    let source = "import QtQuick\nItem { id: root; function normalizeSelectedBar() {} Item { id: child; function handler(value) { let unrelated = value; root.normalizeSelectedBar(); } } }";
    let output = analyze(source);
    assert_eq!(calls(&output).len(), 1);
    let child = entity_at(
        &output,
        "child",
        EntityKind::Variable,
        source.find("child;").unwrap(),
        5,
    );
    let handler = entity_at(
        &output,
        "handler",
        EntityKind::Method,
        source.find("handler(").unwrap(),
        7,
    );
    contains(
        &output,
        child,
        handler,
        source.find("handler(").unwrap(),
        source.find("handler(").unwrap() + 7,
    );
}

#[test]
fn explicit_components_implicit_binding_objects_and_unproved_types_block_outer_id_proof() {
    for nested in [
        "component Other: Item { function handler() { root.normalizeSelectedBar(); } }",
        "Component { Item { function handler() { root.normalizeSelectedBar(); } } }",
        "delegate: Item { function handler() { root.normalizeSelectedBar(); } }",
        "UnprovedType { function handler() { root.normalizeSelectedBar(); } }",
    ] {
        let source = format!(
            "import QtQuick\nItem {{ id: root; function normalizeSelectedBar() {{}} {nested} }}"
        );
        let output = analyze(&source);
        assert!(calls(&output).is_empty(), "{source}");
    }
}

#[test]
fn target_ids_inside_implicit_components_and_unknown_types_are_not_visible_to_outer_callers() {
    for target in [
        "delegate: Item { id: targetRoot; function target() {} }",
        "UnprovedType { id: targetRoot; function target() {} }",
        "property Component factory: Item { id: targetRoot; function target() {} }",
        "Component { Item { id: targetRoot; function target() {} } }",
        "component Other: Item { id: targetRoot; function target() {} }",
    ] {
        let source = format!(
            "import QtQuick\nListView {{ {target}\n function handler() {{ targetRoot.target(); }} }}"
        );
        let output = analyze(&source);
        assert!(calls(&output).is_empty(), "{source}");
        assert!(
            output.document().entities.iter().any(
                |entity| entity.kind == EntityKind::Method && entity.canonical_name == "target"
            ),
            "the written target definition remains available: {source}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "qml-native-id-receiver-binding-unavailable"),
            "{source}"
        );
    }
}

#[test]
fn documentary_tokens_global_javascript_and_grouped_properties_do_not_invent_qml_methods() {
    for source in [
        "import QtQuick\nItem { property string text: 'function fake() {} id: phantom'; /* function ghost() {} */ }",
        "function globalFunction() { return 1; }",
        "import QtQuick\nItem { font { function groupedFunction() {} } }",
    ] {
        let output = analyze(source);
        assert!(!output.document().entities.iter().any(|entity| matches!(
            entity.canonical_name.as_str(),
            "fake" | "ghost" | "phantom" | "globalFunction" | "groupedFunction"
        )));
    }
}

#[test]
fn body_edits_keep_written_qml_entity_ids_and_containers_stable() {
    let before = analyze(SOURCE);
    let changed = SOURCE.replace("return;", "return /* body-only edit */;");
    let after = analyze(&changed);
    let entities = |output: &rootlight_adapter_sdk::AnalysisOutput| {
        output
            .document()
            .entities
            .iter()
            .filter(|entity| {
                matches!(
                    entity.kind,
                    EntityKind::Variable | EntityKind::Property | EntityKind::Method
                )
            })
            .map(|entity| {
                (
                    entity.canonical_name.clone(),
                    entity.kind,
                    entity.id,
                    entity.container,
                )
            })
            .collect::<std::collections::BTreeSet<_>>()
    };
    assert_eq!(entities(&before), entities(&after));
    assert_eq!(calls(&before).len(), 1);
    assert_eq!(calls(&after).len(), 1);
}

#[test]
fn required_capture_quota_and_client_cancellation_cannot_publish_partial_identity() {
    let (_, required) = execute(SOURCE.as_bytes(), "utf-8", 4096).unwrap();
    assert!(required > 1);
    assert!(execute(SOURCE.as_bytes(), "utf-8", required - 1).is_err());
    let cancel = Cancellation::new();
    cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(matches!(
        execute_with_cancel(SOURCE.as_bytes(), "utf-8", 4096, &cancel),
        Err(AdapterError::Cancelled {
            reason: rootlight_cancel::CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn unavailable_duplicate_object_id_keeps_required_exact_domain_gaps() {
    let source = "import QtQuick\nItem { id: root; id: other; property int value: 1 }";
    let output = analyze(source);
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Variable)
    );
    let start = u64::try_from(source.find("Item {").unwrap()).unwrap();
    let end = u64::try_from(source.len()).unwrap();
    for detail in [
        "qml-written-object-id-identity-unavailable",
        "qml-written-object-id-definition-unavailable",
        "qml-written-object-id-ownership-unavailable",
    ] {
        let gaps: Vec<_> = output
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == detail)
            .collect();
        assert_eq!(gaps.len(), 1);
        let witness = gaps[0].evidence.source.as_ref().unwrap();
        assert_eq!(witness.span().start_byte(), start);
        assert_eq!(witness.span().end_byte(), end);
        assert_eq!(witness.content_hash(), content_hash(source.as_bytes()));
    }
}

#[test]
fn utf8_bom_and_utf16_orders_keep_exact_physical_definition_and_call_bytes() {
    let text = format!("// λ😀\r\n{SOURCE}");
    let prefix = text.find(SOURCE).unwrap();
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let raw: Vec<u8> = if encoding == "utf-8" {
            [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat()
        } else {
            let mut raw = if encoding == "utf-16le" {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in text.encode_utf16() {
                raw.extend_from_slice(&if encoding == "utf-16le" {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            raw
        };
        let output = analyze_raw(&raw, encoding);
        for (offset, name, role) in [
            (96, "root", OccurrenceRole::Definition),
            (124, "selectedBarId", OccurrenceRole::Definition),
            (165, "normalizeSelectedBar", OccurrenceRole::Definition),
            (414, "onBarConfigsChanged", OccurrenceRole::Definition),
            (456, "normalizeSelectedBar", OccurrenceRole::CallSite),
        ] {
            let written = prefix + offset;
            let start = if encoding == "utf-8" {
                3 + written
            } else {
                2 + 2 * text[..written].encode_utf16().count()
            };
            let expected: Vec<u8> = if encoding == "utf-8" {
                name.as_bytes().to_vec()
            } else {
                name.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            };
            let sites: Vec<_> = output
                .document()
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == role
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                        && site.source.span().end_byte()
                            == u64::try_from(start + expected.len()).unwrap()
                })
                .collect();
            assert_eq!(sites.len(), 1, "{encoding}: {name}");
            assert!(matches!(sites[0].target, OccurrenceTarget::Resolved { .. }));
            assert_eq!(sites[0].source.content_hash(), content_hash(&raw));
            assert_eq!(&raw[start..start + expected.len()], expected);
        }
        assert_eq!(calls(&output).len(), 1);
    }
}
