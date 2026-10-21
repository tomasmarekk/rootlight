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
    analyze_observed(bytes, encoding, |_, _, _| Ok(())).unwrap()
}

fn analyze_observed(
    bytes: &[u8],
    encoding: &str,
    observe: impl FnOnce(
        &TreeSitterAnalyzer,
        &TreeSitterProvider,
        &AnalysisRequest<'_>,
    ) -> Result<(), rootlight_adapter_sdk::AdapterError>,
) -> Result<AnalysisOutput, rootlight_adapter_sdk::AdapterError> {
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
        provider.clone(),
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
    observe(&analyzer, provider.as_ref(), &request)?;
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &Cancellation::new(),
    )?;
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
    Ok(output)
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
        let source = if std::ptr::eq(output, &before) {
            ORIGINAL
        } else {
            changed.as_str()
        };
        php_original_written_call_set(output, source);
    }
}

// These relations identify written declarations, not a unique runtime override.
fn php_written_call(
    output: &AnalysisOutput,
    caller: SymbolId,
    target: SymbolId,
    start: usize,
    end: usize,
) {
    let calls = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.role == OccurrenceRole::CallSite
                && occurrence.source.span().start_byte() == u64::try_from(start).unwrap()
                && occurrence.source.span().end_byte() == u64::try_from(end).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1, "one exact written member call");
    assert_eq!(
        calls[0].target,
        OccurrenceTarget::Resolved { symbol: target }
    );
    assert_eq!(calls[0].enclosing, Some(caller));
    let relations = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::Calls
                && relation.subject == RelationEndpoint::Entity(caller)
                && relation.object == RelationEndpoint::Entity(target)
        })
        .collect::<Vec<_>>();
    assert_eq!(relations.len(), 1, "one static declaration dependency");
    let evidence = relations[0].evidence.source.as_ref().unwrap();
    assert_eq!(evidence, &calls[0].source);
    assert_eq!(
        relations[0].id,
        derive_relation_record_id(relations[0]).unwrap()
    );
    assert!(
        !output.document().relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::DispatchCandidate
                && relation.subject == RelationEndpoint::Entity(caller)
        }),
        "this finite source proof does not invent runtime implementations"
    );
}

#[test]
fn original_virtual_member_call_keeps_its_statically_written_declaration_dependency() {
    assert_eq!(ORIGINAL.len(), 1145);
    assert_eq!(&ORIGINAL[1118..1124], "toHtml");
    let output = analyze(ORIGINAL);
    let class = definition_at(&output, "HtmlString", EntityKind::Class, 111, 121);
    let target = definition_at(&output, "toHtml", EntityKind::Method, 541, 547);
    let caller = definition_at(&output, "__toString", EntityKind::Method, 1075, 1085);
    class_contains(&output, class, target, 541, 547);
    class_contains(&output, class, caller, 1075, 1085);
    php_written_call(&output, caller, target, 1118, 1124);
    assert!(!output.document().occurrences.iter().any(|occurrence| {
        occurrence.role == OccurrenceRole::Definition
            && occurrence.source.span().start_byte() == 1118
    }));
}

#[test]
fn nonfinal_class_and_known_override_keep_the_lexical_declaration_target() {
    let source = "<?php class Base { public function target() { return 'base'; } public function caller() { return $this->TARGET(); } } class Child extends Base { public function target() { return 'child'; } }";
    let output = analyze(source);
    let target_start = source.find("target()").unwrap();
    let caller_start = source.find("caller()").unwrap();
    let child_start = source.rfind("target()").unwrap();
    let target = definition_at(
        &output,
        "target",
        EntityKind::Method,
        target_start,
        target_start + 6,
    );
    let caller = definition_at(
        &output,
        "caller",
        EntityKind::Method,
        caller_start,
        caller_start + 6,
    );
    let child = definition_at(
        &output,
        "target",
        EntityKind::Method,
        child_start,
        child_start + 6,
    );
    assert_ne!(target, child);
    let site = source.find("TARGET()").unwrap();
    php_written_call(&output, caller, target, site, site + 6);
    assert!(!output.document().relations.iter().any(|relation| {
        relation.predicate == RelationPredicate::Calls
            && relation.subject == RelationEndpoint::Entity(caller)
            && relation.object == RelationEndpoint::Entity(child)
    }));
}

#[test]
fn same_named_methods_in_distinct_classes_and_self_recursion_keep_written_owners() {
    let source = "<?php class First { function target() {} function caller() { return $this->target(); } } class Second { function target() {} function caller() { return $this->target(); } function again() { return $this->again(); } }";
    let output = analyze(source);
    for (class_start, class_end) in [
        (0, source.find("class Second").unwrap()),
        (source.find("class Second").unwrap(), source.len()),
    ] {
        let segment = &source[class_start..class_end];
        let target_start = class_start + segment.find("target()").unwrap();
        let caller_start = class_start + segment.find("caller()").unwrap();
        let site = class_start + segment.find("$this->target()").unwrap() + "$this->".len();
        let target = definition_at(
            &output,
            "target",
            EntityKind::Method,
            target_start,
            target_start + 6,
        );
        let caller = definition_at(
            &output,
            "caller",
            EntityKind::Method,
            caller_start,
            caller_start + 6,
        );
        php_written_call(&output, caller, target, site, site + 6);
    }
    let declaration = source.find("again()").unwrap();
    let again = definition_at(
        &output,
        "again",
        EntityKind::Method,
        declaration,
        declaration + 5,
    );
    let site = source.rfind("again()").unwrap();
    php_written_call(&output, again, again, site, site + 5);
}

#[test]
fn dynamic_receivers_traits_inherited_lookup_and_nested_or_static_contexts_stay_unproved() {
    for source in [
        "<?php class Owner { function target() {} function caller($other) { return $other->target(); } }",
        "<?php class Owner { function target() {} function caller($method) { return $this->$method(); } }",
        "<?php class Owner { function target() {} function caller($method) { return $this->{$method}(); } }",
        "<?php class Owner { function target() {} function caller() { return $this?->target(); } }",
        "<?php trait Extension { function target() {} } class Owner { use Extension; function caller() { return $this->target(); } }",
        "<?php class Base { function target() {} } class Owner extends Base { function caller() { return $this->target(); } }",
        "<?php class Owner { function target() {} static function caller() { return $this->target(); } }",
        "<?php class Owner { static function target() {} function caller() { return $this->target(); } }",
        "<?php class Owner { function target() {} function caller() { return function () { return $this->target(); }; } }",
        "<?php class Owner { function target() {} function caller() { return fn() => $this->target(); } }",
        "<?php class Owner { function target() {} function TARGET() {} function caller() { return $this->target(); } }",
        "<?php class Owner { function __call($name, $arguments) {} function caller() { return $this->missing(); } }",
        "<?php class Owner { function target() {} function caller() { return $this->target( ; } }",
    ] {
        let output = analyze(source);
        assert!(
            !output.document().relations.iter().any(|relation| {
                matches!(
                    relation.predicate,
                    RelationPredicate::Calls | RelationPredicate::DispatchCandidate
                )
            }),
            "unsupported member source must not borrow a target: {source}"
        );
        assert!(
            !output.document().occurrences.iter().any(|occurrence| {
                occurrence.role == OccurrenceRole::CallSite
                    && matches!(occurrence.target, OccurrenceTarget::Resolved { .. })
            }),
            "unproved receiver/member must remain unresolved: {source}"
        );
    }
}

#[test]
fn complete_original_written_call_uses_physical_utf8_bom_and_utf16_coordinates() {
    for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
        let (raw, label, unit, prefix) = match encoding {
            "utf-8" => (ORIGINAL.as_bytes().to_vec(), "utf-8", 1, 0),
            "utf-8-bom" => (
                [b"\xef\xbb\xbf".as_slice(), ORIGINAL.as_bytes()].concat(),
                "utf-8",
                1,
                3,
            ),
            "utf-16le" => (
                std::iter::once(0xfeff_u16)
                    .chain(ORIGINAL.encode_utf16())
                    .flat_map(u16::to_le_bytes)
                    .collect(),
                "utf-16le",
                2,
                2,
            ),
            "utf-16be" => (
                std::iter::once(0xfeff_u16)
                    .chain(ORIGINAL.encode_utf16())
                    .flat_map(u16::to_be_bytes)
                    .collect(),
                "utf-16be",
                2,
                2,
            ),
            _ => unreachable!(),
        };
        let output = analyze_raw(&raw, label);
        let target = definition_at(
            &output,
            "toHtml",
            EntityKind::Method,
            prefix + unit * 541,
            prefix + unit * 547,
        );
        let caller = definition_at(
            &output,
            "__toString",
            EntityKind::Method,
            prefix + unit * 1075,
            prefix + unit * 1085,
        );
        php_written_call(
            &output,
            caller,
            target,
            prefix + unit * 1118,
            prefix + unit * 1124,
        );
        let expected = match unit {
            1 => b"toHtml".to_vec(),
            _ if label == "utf-16le" => {
                "toHtml".encode_utf16().flat_map(u16::to_le_bytes).collect()
            }
            _ => "toHtml".encode_utf16().flat_map(u16::to_be_bytes).collect(),
        };
        assert_eq!(&raw[prefix + unit * 1118..prefix + unit * 1124], expected);
    }
}

fn php_original_written_call_set(output: &AnalysisOutput, source: &str) {
    let mut expected = std::collections::BTreeSet::new();
    for (caller_name, target_name) in [("isNotEmpty", "isEmpty"), ("__toString", "toHtml")] {
        let caller_start =
            source.find(&format!("function {caller_name}")).unwrap() + "function ".len();
        let target_start =
            source.find(&format!("function {target_name}")).unwrap() + "function ".len();
        let site = source.find(&format!("$this->{target_name}()")).unwrap() + "$this->".len();
        let caller = definition_at(
            output,
            caller_name,
            EntityKind::Method,
            caller_start,
            caller_start + caller_name.len(),
        );
        let target = definition_at(
            output,
            target_name,
            EntityKind::Method,
            target_start,
            target_start + target_name.len(),
        );
        php_written_call(output, caller, target, site, site + target_name.len());
        expected.insert((
            caller,
            target,
            u64::try_from(site).unwrap(),
            u64::try_from(site + target_name.len()).unwrap(),
        ));
    }
    let actual = output
        .document()
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Calls)
        .map(|relation| {
            let (RelationEndpoint::Entity(caller), RelationEndpoint::Entity(target)) =
                (&relation.subject, &relation.object)
            else {
                panic!("written method edges must retain declaration endpoints");
            };
            let source = relation.evidence.source.as_ref().unwrap();
            (
                *caller,
                *target,
                source.span().start_byte(),
                source.span().end_byte(),
            )
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        actual, expected,
        "constructor-body edits preserve the exact two written method dependencies"
    );
}

#[test]
fn written_method_calls_agree_at_exact_provider_artifact_admission_and_one_less() {
    analyze_observed(
        ORIGINAL.as_bytes(),
        "utf-8",
        |analyzer, provider, request| {
            let cancel = Cancellation::new();
            let required =
                provider.required_syntax_fact_count(&request.to_parse_request(), &cancel)?;
            let artifact = analyzer.prepare_artifact(request, &cancel)?;
            assert_eq!(artifact.required_syntax_fact_count(&cancel)?, required);
            let syntax = request.limits().syntax_stream();
            let for_capacity = |capacity| {
                AnalysisLimits::new(
                    request.limits().max_source_bytes(),
                    request.limits().max_syntax_nodes(),
                    request.limits().max_syntax_depth(),
                    request.limits().max_embedded_ranges(),
                    request.limits().max_reported_memory_bytes(),
                    StreamLimits::new(
                        syntax.max_batches(),
                        capacity,
                        syntax.max_output_bytes(),
                        syntax.max_diagnostics(),
                        syntax.max_diagnostic_bytes(),
                        syntax.max_string_bytes(),
                        BatchThresholds::new(
                            capacity.min(syntax.batch().max_records()),
                            syntax.batch().max_output_bytes(),
                            syntax.batch().max_diagnostics(),
                            syntax.batch().max_diagnostic_bytes(),
                        )
                        .unwrap(),
                    )
                    .unwrap(),
                    request.limits().ir_stream().clone(),
                    request.limits().ir().clone(),
                )
                .unwrap()
            };
            let exact_limits = for_capacity(required);
            let exact = AnalysisRequest::new_with_parse_context(
                request.source().clone(),
                request.language().clone(),
                request.encoding().clone(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &exact_limits,
            )
            .unwrap()
            .with_generated_status(false);
            let exact_artifact = analyzer.prepare_artifact(&exact, &cancel)?;
            assert_eq!(
                exact_artifact.required_syntax_fact_count(&cancel)?,
                required
            );
            assert_eq!(exact_artifact.syntax_fact_count(), required);
            let output = execute_analysis(
                analyzer,
                &exact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            let replay = analyzer.analyze_from_artifact(
                &exact,
                &exact_artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )?;
            assert_eq!(output.document(), replay.document());
            php_original_written_call_set(&output, ORIGINAL);
            let small_limits = for_capacity(required - 1);
            let small = AnalysisRequest::new_with_parse_context(
                request.source().clone(),
                request.language().clone(),
                request.encoding().clone(),
                Vec::new(),
                AnalysisTier::TierD,
                request.build_context(),
                &small_limits,
            )
            .unwrap()
            .with_generated_status(false);
            let expected = rootlight_adapter_sdk::AdapterError::Sink(
                rootlight_adapter_sdk::SinkError::StreamLimit {
                    resource: rootlight_adapter_sdk::ResourceKind::RequiredSyntaxFacts,
                    observed: required,
                    limit: required - 1,
                },
            );
            assert_eq!(
                analyzer.prepare_artifact(&small, &cancel).unwrap_err(),
                expected
            );
            // Preflight measures demand independently of output admission.
            assert_eq!(
                provider.required_syntax_fact_count(&small.to_parse_request(), &cancel)?,
                required
            );
            assert_eq!(
                rootlight_adapter_sdk::execute_parse(
                    provider,
                    &small.to_parse_request(),
                    MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                    &cancel,
                )
                .unwrap_err(),
                expected
            );
            Ok(())
        },
    )
    .unwrap();
}

#[test]
fn raw_owner_definition_and_context_cardinality_is_required_before_binding() {
    use rootlight_adapter_sdk::{
        ParseProvider, SyntaxFact, SyntaxFactKind, SyntaxKindLabel, execute_parse,
        testkit::MockParseProvider,
    };
    analyze_observed(ORIGINAL.as_bytes(), "utf-8", |_, provider, request| {
        let cancel = Cancellation::new();
        let parsed = execute_parse(
            provider,
            &request.to_parse_request(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel,
        )?;
        let replay = |facts| {
            let mock = MockParseProvider::new(
                provider.capabilities().clone(),
                facts,
                parsed.diagnostics().to_vec(),
                parsed.report().coverage().clone(),
            );
            let analyzer = TreeSitterAnalyzer::new(
                Arc::new(mock),
                ProducerIdentity::new("php-call-witness", "1.0", content_hash(b"php-call-witness"))
                    .unwrap(),
                LanguageId::new("php").unwrap(),
                "php-call-replay",
                content_hash(b"php-call-replay"),
            )
            .unwrap();
            execute_analysis(
                &analyzer,
                request,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancel,
            )
        };
        let baseline = replay(parsed.facts().to_vec())?;
        php_original_written_call_set(&baseline, ORIGINAL);
        let method = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.kind() == SyntaxFactKind::Declaration
                    && fact.syntax_kind().as_str() == "php.method.declaration"
                    && fact.span().start_byte() == 525
            })
            .unwrap();
        let definition = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.parent() == Some(method.local_id())
                    && fact.syntax_kind().as_str() == "php.name.definition"
            })
            .unwrap();
        let method_context = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == "php.this_method_context.signature"
                    && fact.span() == method.span()
            })
            .unwrap();
        let class_context = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == "php.this_class_context.signature"
                    && fact.span().start_byte() == 105
            })
            .unwrap();
        for witness in [method, definition, method_context, class_context] {
            for mode in 0..3 {
                let mut facts = parsed.facts().to_vec();
                if mode == 0 {
                    facts.retain(|fact| fact.local_id() != witness.local_id());
                    for child in &mut facts {
                        if child.parent() == Some(witness.local_id()) {
                            *child = SyntaxFact::new(
                                child.local_id(),
                                witness.parent(),
                                child.kind(),
                                child.span(),
                                child.depth(),
                                child.syntax_kind().clone(),
                            );
                        }
                    }
                } else if mode == 1 {
                    let id = facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1;
                    facts.push(SyntaxFact::new(
                        id,
                        witness.parent(),
                        witness.kind(),
                        witness.span(),
                        witness.depth(),
                        witness.syntax_kind().clone(),
                    ));
                } else {
                    let current = facts
                        .iter_mut()
                        .find(|fact| fact.local_id() == witness.local_id())
                        .unwrap();
                    *current = SyntaxFact::new(
                        current.local_id(),
                        current.parent(),
                        current.kind(),
                        current.span(),
                        current.depth(),
                        SyntaxKindLabel::new("php.foreign.signature").unwrap(),
                    );
                }
                let output = replay(facts)?;
                assert!(
                    !output
                        .document()
                        .occurrences
                        .iter()
                        .any(|site| site.syntax_kind == "php.this_method.reference"
                            && site.source.span().start_byte() == 1118
                            && matches!(site.target, OccurrenceTarget::Resolved { .. })),
                    "raw witness {} mode {mode}",
                    witness.syntax_kind().as_str()
                );
                assert!(!output.document().relations.iter().any(|edge| {
                    edge.predicate == RelationPredicate::Calls
                        && edge
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().start_byte() == 1118)
                }));
            }
        }
        let mut facts = parsed.facts().to_vec();
        let root = facts
            .iter()
            .find(|fact| fact.kind() == SyntaxFactKind::Root)
            .unwrap()
            .local_id();
        let site = facts
            .iter_mut()
            .find(|fact| {
                fact.syntax_kind().as_str() == "php.this_method.reference"
                    && fact.span().start_byte() == 1118
            })
            .unwrap();
        *site = SyntaxFact::new(
            site.local_id(),
            Some(root),
            site.kind(),
            site.span(),
            site.depth(),
            site.syntax_kind().clone(),
        );
        let output = replay(facts)?;
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "php.this_method.reference"
                    && site.source.span().start_byte() == 1118
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
        Ok(())
    })
    .unwrap();
}

#[test]
fn first_class_callable_acquisition_stays_unresolved_while_argument_unpacking_calls() {
    for arguments in ["...", " /* acquisition */ ... /* closure */ "] {
        let source = format!(
            "<?php class Owner {{ function target() {{}} function caller() {{ return $this->target({arguments}); }} }}"
        );
        let output = analyze(&source);
        let class_start = source.find("Owner").unwrap();
        let class = definition_at(
            &output,
            "Owner",
            EntityKind::Class,
            class_start,
            class_start + 5,
        );
        for name in ["target", "caller"] {
            let at = source.find(&format!("function {name}")).unwrap() + "function ".len();
            let method = definition_at(&output, name, EntityKind::Method, at, at + name.len());
            class_contains(&output, class, method, at, at + name.len());
        }
        let site = u64::try_from(source.find("$this->target").unwrap() + "$this->".len()).unwrap();
        assert!(
            !output
                .document()
                .relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::Calls)
        );
        assert!(!output.document().occurrences.iter().any(|occurrence| {
            occurrence.source.span().start_byte() == site
                && (occurrence.syntax_kind == "php.this_method.reference"
                    || matches!(occurrence.target, OccurrenceTarget::Resolved { .. }))
        }));
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.domain == rootlight_ir::FactDomain::Relations
                    && gap.source.span().start_byte() == site
                    && gap.source.content_hash() == content_hash(source.as_bytes())
                    && gap.detail
                        == "php-method-runtime-implementation-and-external-semantics-unavailable")
        );
    }
    let source = "<?php class Owner { function target(...$values) {} function caller($args) { return $this->target(...$args); } }";
    let output = analyze(source);
    let target_start = source.find("function target").unwrap() + "function ".len();
    let caller_start = source.find("function caller").unwrap() + "function ".len();
    let target = definition_at(
        &output,
        "target",
        EntityKind::Method,
        target_start,
        target_start + 6,
    );
    let caller = definition_at(
        &output,
        "caller",
        EntityKind::Method,
        caller_start,
        caller_start + 6,
    );
    let site = source.find("$this->target").unwrap() + "$this->".len();
    php_written_call(&output, caller, target, site, site + 6);
}
