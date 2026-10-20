//! Sass adapter controls exercise real native ASTs and normalized source proofs.
//! Binding is local to clean source-order scopes; unsupported compiler effects
//! and malformed/over-budget analysis cannot manufacture a resolved target.

use std::{fs, path::Path, sync::Arc};

use rootlight_adapter_sdk::{
    AdapterError, AnalysisLimits, AnalysisOutput, AnalysisRequest, BatchThresholds, EncodingId,
    GenerationBoundSnapshot, LanguageId, MemoryAdmissionPolicy, StreamLimits, execute_analysis,
};
use rootlight_adapter_treesitter::TreeSitterAnalyzer;
use rootlight_adapters::SassParseProvider;
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, CoverageStatus, EntityKind, ExtensionSupport, FactDomain,
    IrLimits, NormalizedIrDocument, OccurrenceRecord, OccurrenceRole, OccurrenceTarget,
    ProducerIdentity, RelationEndpoint, RelationPredicate, SourceRef, SourceSpan,
    validate_ir_document,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};

fn analyze(
    source: &str,
    nodes: usize,
    depth: usize,
    cancellation: &Cancellation,
) -> Result<AnalysisOutput, AdapterError> {
    let directory = tempfile::tempdir().unwrap();
    fs::write(directory.path().join("sample.sass"), source).unwrap();
    let repository = derive_repository(b"sass-native-source-contract").id();
    let root = RepositoryRoot::open(repository, directory.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("sample.sass")).unwrap(),
            262_144,
        )
        .unwrap();
    let reference = SourceRef::new(
        repository,
        GenerationId::from_bytes([31; 20]),
        SourceSpan::new(
            snapshot.file(),
            0,
            snapshot.content().len().try_into().unwrap(),
        )
        .unwrap(),
        snapshot.content_hash(),
        None,
    );
    let provider = Arc::new(SassParseProvider::new(262_144, nodes).unwrap());
    let analyzer = TreeSitterAnalyzer::new(
        provider,
        ProducerIdentity::new(
            "sass-source-contract",
            "1.0",
            content_hash(b"sass-source-config"),
        )
        .unwrap(),
        LanguageId::new("sass").unwrap(),
        "raffia-sass-source-contract",
        SassParseProvider::binary_digest(),
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
        262_144,
        nodes,
        depth,
        1,
        16 * 1024 * 1024,
        stream.clone(),
        stream,
        IrLimits::default(),
    )
    .unwrap();
    let request = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&snapshot, &reference).unwrap(),
        LanguageId::new("sass").unwrap(),
        EncodingId::utf8(),
        Vec::new(),
        AnalysisTier::TierD,
        BuildContextIdentity::new(content_hash(b"sass-source-build")),
        &limits,
    )
    .unwrap()
    .with_generated_status(false);
    let output = execute_analysis(
        &analyzer,
        &request,
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        cancellation,
    )?;
    validate_ir_document(output.document(), limits.ir(), &ExtensionSupport::default()).unwrap();
    Ok(output)
}

fn successful(source: &str) -> AnalysisOutput {
    analyze(source, 16_384, 128, &Cancellation::new()).unwrap()
}

fn read<'a>(document: &'a NormalizedIrDocument, start: usize, name: &str) -> &'a OccurrenceRecord {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == start as u64
                && site.source.span().end_byte() == (start + name.len()) as u64
        })
        .collect();
    assert_eq!(sites.len(), 1, "native Read {start}:{name}");
    sites[0]
}

fn symbol(
    document: &NormalizedIrDocument,
    source: &str,
    marker: &str,
    name: &str,
) -> rootlight_ids::SymbolId {
    let start = source.find(marker).unwrap() + marker.find(name).unwrap();
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start as u64
                && site.source.span().end_byte() == (start + name.len()) as u64
        })
        .collect();
    assert_eq!(sites.len(), 1, "definition {marker:?}:{name:?}: {sites:#?}");
    let OccurrenceTarget::Resolved { symbol } = &sites[0].target else {
        panic!("definition target");
    };
    *symbol
}

#[test]
fn source_order_shadowing_reassignment_and_identifier_equivalence_are_distinct() {
    let source = "$global-width: 1px\n$before: $later\n$alias: $global_width\n.one\n  $global-width: $global_width + 1px\n  width: $global-width\n  $global-width: 3px\n  height: $global-width\n.two\n  outside: $global-width\n$case: $GLOBAL-width\n$later: 2px\n$after: $later\n";
    let output = successful(source);
    let document = output.document();
    let global = symbol(document, source, "$global-width: 1px", "$global-width");
    let local = symbol(
        document,
        source,
        "$global-width: $global_width",
        "$global-width",
    );
    let later = symbol(document, source, "$later: 2px", "$later");
    for (marker, name, target) in [
        ("$alias: $global_width", "$global_width", global),
        ("$global-width: $global_width +", "$global_width", global),
        ("width: $global-width", "$global-width", local),
        ("height: $global-width", "$global-width", local),
        ("outside: $global-width", "$global-width", global),
        ("$after: $later", "$later", later),
    ] {
        let start = source.find(marker).unwrap() + marker.rfind(name).unwrap();
        assert_eq!(
            read(document, start, name).target,
            OccurrenceTarget::Resolved { symbol: target },
            "{marker}"
        );
    }
    for (marker, name) in [
        ("$before: $later", "$later"),
        ("$case: $GLOBAL-width", "$GLOBAL-width"),
    ] {
        let start = source.find(marker).unwrap() + marker.rfind(name).unwrap();
        assert!(matches!(
            read(document, start, name).target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable
                && entity.canonical_name == "$global-width")
            .count(),
        2
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Write)
            .count(),
        1
    );
}

#[test]
fn imports_includes_flags_functions_and_control_flow_block_unproved_bindings() {
    for barrier in [
        "@use \"other\"\n",
        "+may-write\n",
        "$known: 2px !global\n",
        "$known: 2px !default\n",
        "$effect: unknown-function()\n",
        "@if true\n  $known: 2px\n",
    ] {
        let source = format!("$known: 1px\n{barrier}$after: $known\n");
        let output = successful(&source);
        let start = source.rfind("$known").unwrap();
        assert!(
            matches!(
                read(output.document(), start, "$known").target,
                OccurrenceTarget::Unresolved { .. }
            ),
            "{barrier}"
        );
        assert!(!output.document().skipped_regions.is_empty());
    }
}

#[test]
fn mixin_parameters_never_resolve_to_same_spelling_globals_or_fake_functions() {
    let source = "$value: red\n=paint($value)\n  color: $value\n.item\n  +paint(blue)\n";
    let output = successful(source);
    assert!(matches!(
        read(output.document(), source.rfind("$value").unwrap(), "$value").target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert!(
        !output
            .document()
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Function || entity.canonical_name == "+paint")
    );
}

#[test]
fn unicode_bom_and_crlf_keep_physical_name_and_read_coordinates() {
    for source in [
        "$色: red\n.item\n  color: $色\n",
        "// λ😀\r\n$色: red\r\n.item\r\n  color: $色\r\n",
        "\u{feff}// λ😀\r\n$色: red\r\n.item\r\n  color: $色\r\n",
    ] {
        let output = successful(source);
        let target = symbol(output.document(), source, "$色: red", "$色");
        let site = read(output.document(), source.rfind("$色").unwrap(), "$色");
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
        assert_eq!(site.source.content_hash(), content_hash(source.as_bytes()));
        assert_eq!(
            &source.as_bytes()[usize::try_from(site.source.span().start_byte()).unwrap()
                ..usize::try_from(site.source.span().end_byte()).unwrap()],
            "$色".as_bytes()
        );
    }
}

#[test]
fn comment_multidedent_and_leading_plus_selectors_keep_real_rule_ownership() {
    let source = ".outer\n  .inner\n    color: red\n  /* first\n     second */\n\n// top\n.after\n  + sibling\n    color: blue\n";
    let output = successful(source);
    let document = output.document();
    let after = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::StyleRule && entity.canonical_name == ".after")
        .unwrap();
    assert!(!matches!(
        after.container,
        Some(rootlight_ir::ContainerRef::Entity(_))
    ));
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::StyleRule)
            .count(),
        4
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.kind == EntityKind::Function)
    );
}

#[test]
fn invalid_indentation_width_and_malformed_syntax_do_not_publish_certificates() {
    for source in [
        "  $x: 1\n".to_owned(),
        ".one\n \tcolor: red\n".to_owned(),
        ".one\n\tcolor: red\n.two\n  color: blue\n".to_owned(),
        ".one\n    color: red\n  width: 2px\n".to_owned(),
        format!(".one\n{}color: red\n", " ".repeat(65536)),
        "+unknown(\n".to_owned(),
    ] {
        let output = successful(&source);
        assert_rejected_source(&output, &source);
    }
}

#[test]
fn finite_fact_depth_work_and_cancel_ceilings_are_not_silently_truncated() {
    assert!(analyze("$x: 1\n$y: $x\n", 4, 128, &Cancellation::new()).is_err());
    assert!(
        analyze(
            ".one\n  .two\n    .three\n      color: red\n",
            16_384,
            3,
            &Cancellation::new()
        )
        .is_err()
    );
    let source = format!("$x: \"{}\"\n", "a".repeat(100_000));
    let Err(error) = analyze(&source, 32, 128, &Cancellation::new()) else {
        panic!("long token must exhaust guarded work");
    };
    assert!(
        matches!(error, AdapterError::ProviderFailed { code } if code.as_str() == "sass-parser-work-limit")
    );
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyze("$x: 1\n", 16_384, 128, &cancellation),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}

#[test]
fn recovered_native_parse_is_partial_and_cannot_publish_binding_certificates() {
    let output = successful("$x: red !default !default\n$y: $x\n");
    assert!(output.document().entities.is_empty());
    assert!(!output.document().skipped_regions.is_empty());
    assert!(
        !output
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn namespace_and_flags_emit_unresolved_writes_without_false_local_members() {
    for statement in [
        "$accent: blue !global",
        "$accent: blue !default",
        "theme.$accent: blue",
        "$accent: $accent !global",
        "$accent: $accent !default",
        "theme.$accent: $accent",
    ] {
        let source =
            format!("$accent: red\n.item\n  {statement}\n  $accent: green\n  color: $accent\n");
        let output = successful(&source);
        let document = output.document();
        let start = source.find(statement).unwrap() + statement.find("$accent").unwrap();
        let end = start + "$accent".len();
        assert!(
            !document.occurrences.iter().any(|site| {
                site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == start as u64
                    && site.source.span().end_byte() == end as u64
            }),
            "unsupported write cannot define a local: {statement}"
        );
        let writes: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Write
                    && site.source.span().start_byte() == start as u64
                    && site.source.span().end_byte() == end as u64
            })
            .collect();
        assert_eq!(writes.len(), 1, "{statement}");
        let write = writes[0];
        assert!(matches!(write.target, OccurrenceTarget::Unresolved { .. }));
        assert_eq!(write.source.content_hash(), content_hash(source.as_bytes()));
        assert_eq!(&source.as_bytes()[start..end], b"$accent");
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| { edge.subject == RelationEndpoint::Occurrence(write.id) })
        );
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.detail == "sass-source-construct-semantics-unavailable"
                && gap.source.span().start_byte() <= start as u64
                && gap.source.span().end_byte() >= end as u64
                && gap.source.content_hash() == write.source.content_hash()
                && gap.source.generation() == write.source.generation()
                && gap.source.span().file() == write.source.span().file()
        }));
        let global = symbol(document, &source, "$accent: red", "$accent");
        let local = symbol(document, &source, "$accent: green", "$accent");
        let rule = symbol(document, &source, ".item", ".item");
        let variables: Vec<_> = document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Variable)
            .collect();
        assert_eq!(variables.len(), 2, "{statement}");
        assert!(
            variables
                .iter()
                .all(|entity| entity.id == global || entity.id == local)
        );
        let members: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(rule)
                    && variables
                        .iter()
                        .any(|entity| edge.object == RelationEndpoint::Entity(entity.id))
            })
            .collect();
        assert_eq!(members.len(), 1, "{statement}");
        assert_eq!(members[0].object, RelationEndpoint::Entity(local));
        for site in document.occurrences.iter().filter(|site| {
            site.role == OccurrenceRole::Read && site.source.span().start_byte() > start as u64
        }) {
            assert!(
                matches!(site.target, OccurrenceTarget::Unresolved { .. }),
                "{statement}"
            );
        }
    }
}

#[test]
fn unsupported_first_assignment_does_not_poison_later_plain_declaration() {
    for statement in [
        "$accent_tone: blue !global",
        "$accent_tone: blue !default",
        "theme.$accent_tone: blue",
    ] {
        for nested in [false, true] {
            let source = if nested {
                format!(".item\n  {statement}\n  $accent-tone: green\n  color: $accent_tone\n")
            } else {
                format!("{statement}\n$accent-tone: green\n$after: $accent_tone\n")
            };
            let output = successful(&source);
            let document = output.document();
            let target = symbol(document, &source, "$accent-tone: green", "$accent-tone");
            let candidates: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Variable
                        && matches!(
                            entity.canonical_name.as_str(),
                            "$accent-tone" | "$accent_tone"
                        )
                })
                .collect();
            assert_eq!(candidates.len(), 1, "{statement} nested={nested}");
            assert_eq!(candidates[0].id, target);
            let read = read(
                document,
                source.rfind("$accent_tone").unwrap(),
                "$accent_tone",
            );
            assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
            assert!(!document.skipped_regions.is_empty());
        }
    }
}

#[test]
fn variables_rules_and_properties_have_one_resolved_definition_per_written_name() {
    let source = "$value: red\n.item\n  color: $value\n";
    let output = successful(source);
    let document = output.document();
    for (marker, name, kind) in [
        ("$value: red", "$value", EntityKind::Variable),
        (".item", ".item", EntityKind::StyleRule),
        ("color:", "color", EntityKind::Property),
    ] {
        let target = symbol(document, source, marker, name);
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == target)
            .unwrap();
        assert_eq!(entity.kind, kind);
        assert_eq!(entity.canonical_name, name);
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition)
            .count(),
        3
    );
    assert!(!document.occurrences.iter().any(|site| {
        site.role == OccurrenceRole::Definition
            && matches!(site.target, OccurrenceTarget::Unresolved { .. })
    }));
}

fn assert_rejected_source(output: &AnalysisOutput, source: &str) {
    let document = output.document();
    assert!(document.entities.is_empty());
    assert!(document.occurrences.is_empty());
    assert!(document.relations.is_empty());
    assert_eq!(output.report().coverage().status(), CoverageStatus::Unknown);
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        let coverage = output
            .report()
            .coverage()
            .domains()
            .iter()
            .find(|entry| entry.domain() == domain)
            .unwrap();
        assert_eq!(coverage.status(), CoverageStatus::Unknown);
        assert!(document.skipped_regions.iter().any(|gap| {
            gap.domain == domain
                && gap.source.content_hash() == content_hash(source.as_bytes())
                && gap.source.span().end_byte() <= source.len() as u64
        }));
    }
}

#[test]
fn native_indentation_rejection_has_exact_physical_source_and_no_prefix_bindings() {
    for prefix in ["", "// λ😀\r\n", "\u{feff}// λ😀\r\n"] {
        let source = format!("{prefix}$known: red\r\n.item\r\n \tcolor: $known\r\n");
        let start = source.find(" \t").unwrap();
        let output = successful(&source);
        assert_rejected_source(&output, &source);
        for domain in [
            FactDomain::Entities,
            FactDomain::Occurrences,
            FactDomain::Relations,
        ] {
            let gaps: Vec<_> = output
                .document()
                .skipped_regions
                .iter()
                .filter(|gap| gap.domain == domain && gap.detail == "sass-invalid-indentation")
                .collect();
            assert_eq!(gaps.len(), 1);
            assert_eq!(gaps[0].source.span().start_byte(), start as u64);
            assert_eq!(gaps[0].source.span().end_byte(), (start + 2) as u64);
            assert_eq!(
                gaps[0].source.content_hash(),
                content_hash(source.as_bytes())
            );
        }
    }
    assert!(analyze(".one\n \tcolor: red\n", 2, 128, &Cancellation::new()).is_err());
}
