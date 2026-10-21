//! Physical patches retain documentary structure and exact source evidence.
//! These public analyzer tests distinguish physical patch owners from either
//! unapplied target program, including persisted syntax replay and encodings.

use super::*;

const PATCH: LanguageCase = LanguageCase {
    name: "cmake",
    path: "patches/written.cmake",
    frontend: "tree-sitter-cmake-0.7.5",
    source: "",
    generated: false,
    body_before: "old",
    body_after: "new",
};

const SOURCE: &str = concat!(
    "diff --git a/build.cmake b/build.cmake\n",
    "index 1234..5678 100644\n",
    "--- a/build.cmake\n",
    "+++ b/build.cmake\n",
    "@@ -1,3 +1,3 @@ outer\n",
    " keep\n",
    "-set(removed old)\n",
    "+set(added new)\n",
    " set(context value)\n",
    "@@ -7 +7 @@ next\n",
    "-set(later old)\n",
    "+set(later new)\n",
);

fn selected<'a>(source: &'a str, reference: &SourceRef) -> &'a str {
    source
        .get(
            usize::try_from(reference.span().start_byte()).unwrap()
                ..usize::try_from(reference.span().end_byte()).unwrap(),
        )
        .unwrap()
}

fn output(source: &str) -> AnalysisOutput {
    output_with_limits(source, &limits())
}

fn output_with_limits(source: &str, budget: &AnalysisLimits) -> AnalysisOutput {
    let provider = Arc::new(provider());
    let fixture = Fixture::new(PATCH, source.as_bytes());
    let result = analyze(
        &analyzer(&provider, PATCH),
        &request(&fixture.snapshot, &fixture.source, PATCH, budget),
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    let document = result.document();
    assert_eq!(document.files.len(), 1);
    assert_eq!(document.files[0].language, "cmake");
    assert_eq!(
        document.files[0].content_hash,
        content_hash(source.as_bytes())
    );
    assert_eq!(
        document.files[0].byte_length,
        u64::try_from(source.len()).unwrap()
    );
    for reference in document
        .entities
        .iter()
        .filter_map(|entity| entity.evidence.source.as_ref())
        .chain(document.occurrences.iter().map(|site| &site.source))
        .chain(document.skipped_regions.iter().map(|gap| &gap.source))
    {
        assert_eq!(reference.repository(), document.repository);
        assert_eq!(reference.generation(), document.generation);
        assert_eq!(reference.content_hash(), document.files[0].content_hash);
        assert_eq!(reference.span().file(), document.files[0].id);
        let _ = selected(source, reference);
    }
    result
}

fn definition_owner(
    document: &rootlight_ir::NormalizedIrDocument,
    source: &str,
    heading: &str,
) -> SymbolId {
    let definition = document
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition && selected(source, &site.source) == heading
        })
        .unwrap_or_else(|| panic!("missing physical heading {heading}"));
    match definition.target {
        OccurrenceTarget::Resolved { symbol } => symbol,
        _ => panic!("physical heading has no resolved document owner"),
    }
}

fn assert_whole_gap(document: &rootlight_ir::NormalizedIrDocument, source: &str, detail: &str) {
    for domain in [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ] {
        let gaps: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.domain == domain && gap.detail == detail)
            .collect();
        assert_eq!(gaps.len(), 1, "missing exact gap for {domain:?}");
        assert_eq!(selected(source, &gaps[0].source), source);
    }
}

#[test]
fn cmake_patch_owners_and_rows_describe_only_the_physical_document() {
    let result = output(SOURCE);
    let document = result.document();
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module && entity.language == "diff")
        .expect("physical diff module");
    assert_eq!(
        selected(SOURCE, module.evidence.source.as_ref().unwrap()),
        SOURCE
    );
    assert_eq!(document.entities.len(), 4);
    assert!(document.entities.iter().all(|entity| {
        entity.language == "diff"
            && matches!(
                entity.kind,
                EntityKind::Module | EntityKind::DocumentSection
            )
    }));
    let section = definition_owner(document, SOURCE, "diff --git a/build.cmake b/build.cmake");
    let first = definition_owner(document, SOURCE, "@@ -1,3 +1,3 @@ outer");
    let second = definition_owner(document, SOURCE, "@@ -7 +7 @@ next");
    for (owner, expected) in [
        (section, SOURCE),
        (
            first,
            SOURCE
                .get(SOURCE.find("@@ -1").unwrap()..SOURCE.find("@@ -7").unwrap())
                .unwrap(),
        ),
        (second, SOURCE.get(SOURCE.find("@@ -7").unwrap()..).unwrap()),
    ] {
        let entity = document
            .entities
            .iter()
            .find(|entity| entity.id == owner)
            .unwrap();
        assert_eq!(entity.kind, EntityKind::DocumentSection);
        assert_eq!(
            selected(SOURCE, entity.evidence.source.as_ref().unwrap()),
            expected
        );
    }
    for (child, parent) in [(section, module.id), (first, section), (second, section)] {
        assert!(document.relations.iter().any(|relation| {
            relation.predicate == RelationPredicate::Contains
                && relation.subject == RelationEndpoint::Entity(parent)
                && relation.object == RelationEndpoint::Entity(child)
        }));
    }
    for (syntax, row, owner) in [
        (
            "patch.deleted_row.documentation",
            "-set(removed old)\n",
            first,
        ),
        ("patch.added_row.documentation", "+set(added new)\n", first),
        (
            "patch.context_row.documentation",
            " set(context value)\n",
            first,
        ),
        (
            "patch.deleted_row.documentation",
            "-set(later old)\n",
            second,
        ),
        ("patch.added_row.documentation", "+set(later new)\n", second),
    ] {
        let site = document
            .occurrences
            .iter()
            .find(|site| site.syntax_kind == syntax && selected(SOURCE, &site.source) == row)
            .unwrap_or_else(|| panic!("missing exact physical row {row}"));
        assert_eq!(site.role, OccurrenceRole::Documentation);
        assert_eq!(site.enclosing, Some(owner));
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: owner });
        assert_eq!(site.syntactic_text_hash, content_hash(row.as_bytes()));
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Documentation)
            .count(),
        SOURCE.lines().count()
    );
    assert!(document.occurrences.iter().all(|site| matches!(
        site.role,
        OccurrenceRole::Definition | OccurrenceRole::Documentation
    )));
    assert!(
        document
            .relations
            .iter()
            .all(|relation| relation.predicate == RelationPredicate::Contains)
    );
    assert_whole_gap(
        document,
        SOURCE,
        "patch-target-fragment-analysis-and-applicability-unavailable",
    );
    assert_ne!(
        result.report().coverage().status(),
        CoverageStatus::Complete
    );
}

#[test]
fn cmake_patch_multiple_files_and_no_newline_annotations_keep_separate_owners() {
    let source = concat!(
        "--- old/one.cmake\n+++ new/one.cmake\n@@ -1 +1 @@\n",
        "-set(old value)\n\\ No newline at end of file\n",
        "+set(new value)\n\\ No newline at end of file\n",
        "--- /dev/null\n+++ new/two.cmake\n@@ -0,0 +1 @@\n+set(created yes)\n",
    );
    let result = output(source);
    let document = result.document();
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::DocumentSection)
            .count(),
        4
    );
    let first = definition_owner(document, source, "--- old/one.cmake");
    let second = definition_owner(document, source, "--- /dev/null");
    assert_ne!(first, second);
    let annotations: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "patch.no_newline_row.documentation")
        .collect();
    assert_eq!(annotations.len(), 2);
    let first_hunk = definition_owner(document, source, "@@ -1 +1 @@");
    for annotation in annotations {
        assert_eq!(
            selected(source, &annotation.source),
            "\\ No newline at end of file\n"
        );
        assert_eq!(
            annotation.target,
            OccurrenceTarget::Resolved { symbol: first_hunk }
        );
    }
    assert_whole_gap(
        document,
        source,
        "patch-target-fragment-analysis-and-applicability-unavailable",
    );
}

#[test]
fn cmake_patch_invalid_and_unsupported_files_publish_no_partial_target_facts() {
    for (source, detail) in [
        (
            "--- a\n+++ b\n@@ -1,2 +1 @@\n-set(removed yes)\n+set(added yes)\n",
            "patch-shape-validation-unavailable",
        ),
        (
            "diff --git a/a b/a\nGIT binary patch\nliteral 12\n",
            "patch-format-analysis-unavailable",
        ),
        (
            "diff --combined a.cmake\n@@@ -1 -1 +1 @@@\n",
            "patch-format-analysis-unavailable",
        ),
        (
            "---\n-set(removed yes)\n+set(added yes)\n",
            "patch-shape-validation-unavailable",
        ),
        (
            "+++ new.cmake\n+set(added yes)\n",
            "patch-shape-validation-unavailable",
        ),
        (
            "@@ -1 +1 @@\n-set(removed yes)\n+set(added yes)\n",
            "patch-shape-validation-unavailable",
        ),
        (
            "*** old.cmake\n--- new.cmake\n***************\n",
            "patch-format-analysis-unavailable",
        ),
    ] {
        let result = output(source);
        let document = result.document();
        assert_eq!(document.entities.len(), 1);
        assert_eq!(document.entities[0].kind, EntityKind::Module);
        assert_eq!(document.entities[0].language, "diff");
        assert!(document.occurrences.is_empty());
        assert_whole_gap(document, source, detail);
    }
}

#[test]
fn cmake_patch_markers_in_native_comments_and_strings_preserve_cmake_variables() {
    for source in [
        "# diff --git a/a b/a\nset(written value)\n",
        "message(\"--- a\\n+++ b\")\nset(written value)\n",
        "#[=[\n--- a\n+++ b\n@@ -1 +1 @@\n-set(fake yes)\n+set(fake no)\n]=]\nset(written value)\n",
    ] {
        let result = output(source);
        let document = result.document();
        assert!(
            document
                .entities
                .iter()
                .any(|entity| entity.canonical_name == "written"
                    && entity.kind == EntityKind::Variable
                    && entity.language == "cmake")
        );
        assert!(
            document
                .entities
                .iter()
                .all(|entity| entity.language != "diff")
        );
        assert!(
            document
                .occurrences
                .iter()
                .all(|site| !site.syntax_kind.starts_with("patch."))
        );
        assert!(
            document
                .skipped_regions
                .iter()
                .all(|gap| !gap.detail.starts_with("patch-"))
        );
    }
}

#[test]
fn cmake_patch_utf16_and_utf8_boms_keep_headings_and_rows_on_original_bytes() {
    let source =
        "--- a/λ.cmake\r\n+++ b/λ.cmake\r\n@@ -1 +1 @@ 😀\r\n-set(old λ😀)\r\n+set(new μ😀)\r\n";
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let encode = |text: &str| -> Vec<u8> {
            if encoding == "utf-8" {
                text.as_bytes().to_vec()
            } else {
                text.encode_utf16()
                    .flat_map(|unit| {
                        if encoding == "utf-16le" {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect()
            }
        };
        let mut bytes = match encoding {
            "utf-8" => vec![0xef, 0xbb, 0xbf],
            "utf-16le" => vec![0xff, 0xfe],
            _ => vec![0xfe, 0xff],
        };
        let bom_len = bytes.len();
        bytes.extend(encode(source));
        let provider = Arc::new(provider());
        let fixture = Fixture::new(PATCH, &bytes);
        let budget = limits();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(PATCH),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &budget,
        )
        .unwrap()
        .with_generated_status(PATCH.generated);
        let result = analyze(
            &analyzer(&provider, PATCH),
            &input,
            &ExtensionSupport::default(),
        );
        let document = result.document();
        validate_ir_document(document, budget.ir(), &ExtensionSupport::default()).unwrap();
        assert_eq!(document.files[0].encoding, encoding);
        assert_eq!(document.files[0].content_hash, content_hash(&bytes));
        for (role, written) in [
            (OccurrenceRole::Definition, "@@ -1 +1 @@ 😀"),
            (OccurrenceRole::Documentation, "-set(old λ😀)\r\n"),
            (OccurrenceRole::Documentation, "+set(new μ😀)\r\n"),
        ] {
            let expected = encode(written);
            let occurrence = document
                .occurrences
                .iter()
                .find(|site| {
                    let start = usize::try_from(site.source.span().start_byte()).unwrap();
                    let end = usize::try_from(site.source.span().end_byte()).unwrap();
                    site.role == role && bytes.get(start..end) == Some(expected.as_slice())
                })
                .unwrap_or_else(|| panic!("missing original {encoding} bytes for {written}"));
            assert_eq!(occurrence.source.content_hash(), content_hash(&bytes));
            let prefix = source.get(..source.find(written).unwrap()).unwrap();
            assert_eq!(
                usize::try_from(occurrence.source.span().start_byte()).unwrap(),
                bom_len + encode(prefix).len()
            );
        }
        assert!(
            document
                .entities
                .iter()
                .all(|entity| entity.language == "diff")
        );
    }
}

#[test]
fn cmake_patch_oversize_hunk_heading_keeps_exact_rows_and_a_resource_gap() {
    let defaults = limits();
    let mut ir = defaults.ir().clone();
    ir.max_string_bytes = 128;
    let budget = AnalysisLimits::new(
        defaults.max_source_bytes(),
        defaults.max_syntax_nodes(),
        defaults.max_syntax_depth(),
        defaults.max_embedded_ranges(),
        defaults.max_reported_memory_bytes(),
        defaults.syntax_stream().clone(),
        defaults.ir_stream().clone(),
        ir,
    )
    .unwrap();
    let maximum = budget.ir().max_string_bytes;
    let heading = format!("@@ -1 +1 @@ {}", "h".repeat(maximum + 1));
    let source = format!("--- a\n+++ b\n{heading}\n-set(old yes)\n+set(new yes)\n");
    let result = output_with_limits(&source, &budget);
    let document = result.document();
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::DocumentSection)
            .count(),
        1
    );
    assert!(document.entities.iter().all(|entity| {
        entity.canonical_name.len() <= maximum
            && !entity.canonical_name.starts_with("@@")
            && !entity.display_name.starts_with("@@")
    }));
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.domain == FactDomain::Entities
                && gap.reason == SkippedRegionReason::ResourceLimit
                && gap.detail == "patch-section-heading-resource-limit"
        })
        .collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(
        selected(&source, &gaps[0].source),
        source.get(source.find("@@").unwrap()..).unwrap()
    );
    for row in [
        format!("{heading}\n"),
        "-set(old yes)\n".to_owned(),
        "+set(new yes)\n".to_owned(),
    ] {
        let documentation = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Documentation && selected(&source, &site.source) == row
            })
            .unwrap_or_else(|| panic!("missing complete physical row of {} bytes", row.len()));
        assert_eq!(
            documentation.source.content_hash(),
            content_hash(source.as_bytes())
        );
        assert_eq!(
            documentation.syntactic_text_hash,
            content_hash(row.as_bytes())
        );
    }
    assert_whole_gap(
        document,
        &source,
        "patch-target-fragment-analysis-and-applicability-unavailable",
    );
}

#[test]
fn cmake_patch_preflight_requires_every_documentary_row_and_replay_preserves_it() {
    let provider = Arc::new(provider());
    let analyzer = analyzer(&provider, PATCH);
    let fixture = Fixture::new(PATCH, SOURCE.as_bytes());
    let budget = limits();
    let input = request(&fixture.snapshot, &fixture.source, PATCH, &budget);
    let cancellation = Cancellation::new();
    let required = provider
        .required_syntax_fact_count(&input.to_parse_request(), &cancellation)
        .unwrap();
    let (fresh, artifact) = analyzer
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
    assert_eq!(
        required,
        artifact.required_syntax_fact_count(&cancellation).unwrap()
    );
    assert_eq!(required, artifact.syntax_fact_count());
    assert!(required >= SOURCE.lines().count());
    let replay = analyzer
        .analyze_from_artifact(
            &input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancellation,
        )
        .unwrap();
    assert_eq!(fresh.document(), replay.document());
    let bounded = limits_with_syntax_records(required - 1);
    assert!(
        analyzer
            .analyze_and_capture(
                &request(&fixture.snapshot, &fixture.source, PATCH, &bounded),
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancellation,
            )
            .is_err()
    );
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(
        provider
            .required_syntax_fact_count(&input.to_parse_request(), &cancellation)
            .is_err()
    );
    assert!(
        analyzer
            .analyze_from_artifact(
                &input,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &cancellation,
            )
            .is_err()
    );
    assert_eq!(provider.stats().checked_out_parsers, 0);
}
