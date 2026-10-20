//! PHP constructor classification requires an exact native method name and owner.
//! Original byte witnesses keep this distinct from call binding and runtime dispatch.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::{
    ParserSettings, RuntimeConfig, TreeSitterAnalyzer, TreeSitterProvider,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{GenerationId, SymbolId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, ContainerRef, EntityKind, ExtensionSupport, IrLimits,
    OccurrenceRole, OccurrenceTarget, ProducerIdentity, RelationEndpoint, RelationPredicate,
    SourceRef, SourceSpan, derive_relation_record_id, validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};
use tempfile::tempdir;

const ORIGINAL: &str = include_str!("fixtures/php_constructors/HtmlString.php");

fn analyze(source: &str) -> AnalysisOutput {
    analyze_raw(source.as_bytes(), "utf-8")
}

fn analyze_raw(bytes: &[u8], encoding: &str) -> AnalysisOutput {
    let directory = tempdir().unwrap();
    fs::write(directory.path().join("source.php"), bytes).unwrap();
    let repository = derive_repository(b"php-constructor-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("source.php")).unwrap(),
            1024 * 1024,
        )
        .unwrap();
    let source = SourceRef::new(
        repository,
        GenerationId::from_bytes([9; 20]),
        SourceSpan::new(snapshot.file(), 0, u64::try_from(bytes.len()).unwrap()).unwrap(),
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
        provider,
        ProducerIdentity::new(
            "php-constructor-contract",
            "1.0",
            content_hash(b"php-configuration"),
        )
        .unwrap(),
        LanguageId::new("php").unwrap(),
        "php-written-constructor",
        content_hash(b"php-contract-binary"),
    )
    .unwrap();
    let stream = StreamLimits::new(
        128,
        4096,
        16 * 1024 * 1024,
        128,
        128 * 1024,
        1024 * 1024,
        BatchThresholds::new(64, 1024 * 1024, 32, 16 * 1024).unwrap(),
    )
    .unwrap();
    let limits = AnalysisLimits::new(
        1024 * 1024,
        16_384,
        128,
        16,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
        LanguageId::new("php").unwrap(),
        EncodingId::new(encoding).unwrap(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"php-build-context")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )
    .unwrap();
    validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    assert_eq!(output.document().files.len(), 1);
    assert_eq!(output.document().files[0].content_hash, content_hash(bytes));
    assert_eq!(
        output.document().files[0].byte_length,
        u64::try_from(bytes.len()).unwrap()
    );
    for occurrence in &output.document().occurrences {
        assert_eq!(occurrence.source.content_hash(), source.content_hash());
        assert_eq!(occurrence.source.repository(), source.repository());
        assert_eq!(occurrence.source.generation(), source.generation());
        assert!(occurrence.source.span().end_byte() <= u64::try_from(bytes.len()).unwrap());
    }
    output
}

fn definition_at(
    output: &AnalysisOutput,
    name: &str,
    kind: EntityKind,
    start: usize,
    end: usize,
) -> SymbolId {
    let definitions = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition
                && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                && occurrence.source.span().end_byte() == u64::try_from(end).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        definitions.len(),
        1,
        "one exact written definition for {name}"
    );
    let OccurrenceTarget::Resolved { symbol } = definitions[0].target else {
        panic!("definition must retain its resolved source identity")
    };
    let entity = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind, "kind of {name}");
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.display_name, name);
    symbol
}

fn class_contains(
    output: &AnalysisOutput,
    class: SymbolId,
    constructor: SymbolId,
    start: usize,
    end: usize,
) {
    let entity = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.id == constructor)
        .unwrap();
    assert_eq!(entity.container, Some(ContainerRef::Entity(class)));
    let matches = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(class)
                && relation.object == RelationEndpoint::Entity(constructor)
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1);
    let relation = matches[0];
    let evidence = relation.evidence.source.as_ref().unwrap();
    assert!(evidence.span().start_byte() <= u64::try_from(start).unwrap());
    assert!(evidence.span().end_byte() >= u64::try_from(end).unwrap());
    assert_eq!(relation.id, derive_relation_record_id(relation).unwrap());
}

#[test]
fn original_constructor_retains_exact_written_kind() {
    assert_eq!(ORIGINAL.len(), 1145);
    assert_eq!(&ORIGINAL[372..383], "__construct");
    let output = analyze(ORIGINAL);
    definition_at(&output, "__construct", EntityKind::Constructor, 372, 383);
    definition_at(&output, "toHtml", EntityKind::Method, 541, 547);
    definition_at(&output, "__toString", EntityKind::Method, 1075, 1085);
    assert!(!output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && occurrence.source.span().start_byte() == 1118
    }));
}

#[test]
fn original_class_contains_its_typed_constructor() {
    let output = analyze(ORIGINAL);
    let class = definition_at(&output, "HtmlString", EntityKind::Class, 111, 121);
    let constructors = output
        .document()
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Constructor && entity.canonical_name == "__construct"
        })
        .collect::<Vec<_>>();
    assert_eq!(constructors.len(), 1);
    class_contains(&output, class, constructors[0].id, 372, 383);
}

#[test]
fn existing_php_trait_fixture_keeps_ordinary_method_and_owner() {
    let source = include_str!("fixtures/structural/php.php");
    let output = analyze(source);
    let owner_start = source.find("Greeting").unwrap();
    let owner = definition_at(
        &output,
        "Greeting",
        EntityKind::Trait,
        owner_start,
        owner_start + 8,
    );
    let method_start = source.find("greet(").unwrap();
    let method = definition_at(
        &output,
        "greet",
        EntityKind::Method,
        method_start,
        method_start + 5,
    );
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == method)
            .unwrap()
            .container,
        Some(ContainerRef::Entity(owner))
    );
    assert!(
        output
            .document()
            .entities
            .iter()
            .all(|entity| entity.kind != EntityKind::Constructor)
    );
}

#[test]
fn constructor_name_comparison_is_exact_case_insensitive_and_method_only() {
    let source = "<?php namespace Sample; function __construct() {}\nclass First { function __construct() {} function First() {} function __constructExtra() {} function pre__construct() {} function __destruct() {} }\nclass Second { function __CONSTRUCT() {} }\nclass Third { function __ConStruct() {} }\n";
    let output = analyze(source);
    let constructors = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Constructor)
        .map(|entity| entity.canonical_name.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        constructors,
        std::collections::BTreeSet::from(["__construct", "__CONSTRUCT", "__ConStruct"])
    );
    for (name, kind) in [
        ("__construct", EntityKind::Function),
        ("First", EntityKind::Method),
        ("__constructExtra", EntityKind::Method),
        ("pre__construct", EntityKind::Method),
        ("__destruct", EntityKind::Method),
    ] {
        assert!(
            output
                .document()
                .entities
                .iter()
                .any(|entity| { entity.canonical_name == name && entity.kind == kind }),
            "ordinary declaration {name}"
        );
    }
}

#[test]
fn constructor_headers_keep_parameters_and_ignore_body_text_and_anonymous_owners() {
    let source = "<?php\n// __construct is only prose here.\nclass Outer {\n#[Marker] public function __construct(string $name = 'value', int ...$rest) { $value = '__construct'; $nested = new class { function __construct() {} }; }\nfunction ordinary($__construct) {}\n}\n";
    let output = analyze(source);
    let class_start = source.find("Outer").unwrap();
    let class = definition_at(
        &output,
        "Outer",
        EntityKind::Class,
        class_start,
        class_start + 5,
    );
    let start = source.find("function __construct").unwrap() + "function ".len();
    let constructor = definition_at(
        &output,
        "__construct",
        EntityKind::Constructor,
        start,
        start + 11,
    );
    class_contains(&output, class, constructor, start, start + 11);
    assert_eq!(
        output
            .document()
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Constructor)
            .count(),
        1
    );
    let ordinary = source.find("ordinary(").unwrap();
    definition_at(
        &output,
        "ordinary",
        EntityKind::Method,
        ordinary,
        ordinary + 8,
    );
}

#[test]
fn encoded_constructor_spans_and_containment_use_physical_source_bytes() {
    let source = "<?php\n// Unicode source prefix: \u{1f680}\nclass Item { function __construct(string $name = '\u{e9}') {} }\n";
    for (encoding, little_endian) in [("utf-16le", true), ("utf-16be", false)] {
        let raw = std::iter::once(0xfeff_u16)
            .chain(source.encode_utf16())
            .flat_map(|unit| {
                if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect::<Vec<_>>();
        let output = analyze_raw(&raw, encoding);
        let physical_start =
            |name: &str| 2 + source[..source.find(name).unwrap()].encode_utf16().count() * 2;
        let class_start = physical_start("Item");
        let class = definition_at(
            &output,
            "Item",
            EntityKind::Class,
            class_start,
            class_start + 8,
        );
        let start = physical_start("__construct");
        let constructor = definition_at(
            &output,
            "__construct",
            EntityKind::Constructor,
            start,
            start + 22,
        );
        class_contains(&output, class, constructor, start, start + 22);
        let expected = "__construct"
            .encode_utf16()
            .flat_map(|unit| {
                if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect::<Vec<_>>();
        assert_eq!(&raw[start..start + 22], expected);
    }
}

#[test]
fn constructor_body_edits_preserve_class_member_and_other_method_identities() {
    let changed = ORIGINAL.replace("$this->html = $html;", "$this->html = trim($html);");
    assert_ne!(changed.as_bytes(), ORIGINAL.as_bytes());
    let before = analyze(ORIGINAL);
    let after = analyze(&changed);
    for (name, kind, needle) in [
        ("HtmlString", EntityKind::Class, "class HtmlString"),
        (
            "__construct",
            EntityKind::Constructor,
            "function __construct",
        ),
        ("toHtml", EntityKind::Method, "function toHtml"),
        ("__toString", EntityKind::Method, "function __toString"),
    ] {
        let original_start = ORIGINAL.find(needle).unwrap() + needle.len() - name.len();
        let changed_start = changed.find(needle).unwrap() + needle.len() - name.len();
        let previous = definition_at(
            &before,
            name,
            kind,
            original_start,
            original_start + name.len(),
        );
        assert_eq!(
            previous,
            definition_at(
                &after,
                name,
                kind,
                changed_start,
                changed_start + name.len()
            )
        );
    }
    for output in [&before, &after] {
        let class = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Class)
            .unwrap();
        let constructor = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Constructor)
            .unwrap();
        assert_eq!(constructor.container, Some(ContainerRef::Entity(class.id)));
        assert!(
            output
                .document()
                .relations
                .iter()
                .all(|relation| relation.predicate != RelationPredicate::Calls)
        );
    }
}
