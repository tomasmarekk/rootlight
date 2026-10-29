//! Native ERB hosts and one combined Ruby syntax tree at original coordinates.

use super::*;

const ERB: LanguageCase = LanguageCase {
    name: "erb",
    path: "written.html.erb",
    frontend: "erb-native-host",
    source: "",
    generated: false,
    body_before: "",
    body_after: "",
};
const ORIGINAL: &[u8] =
    include_bytes!("../../../../tests/native-grammars/fixtures/erb/layout_render_object.erb");
const ESCAPED: &[u8] =
    include_bytes!("../../../../tests/native-grammars/fixtures/erb/sessions_new.html.erb");
const COMMENT: &[u8] =
    include_bytes!("../../../../tests/native-grammars/fixtures/erb/comment_peek.html.erb");

fn output(
    raw: &[u8],
    path: &'static str,
    encoding: &str,
) -> (AnalysisOutput, rootlight_adapter_sdk::ParseOutput) {
    let case = LanguageCase { path, ..ERB };
    let provider = Arc::new(provider());
    let fixture = Fixture::new(case, raw);
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
    .with_generated_status(false);
    let parsed = rootlight_adapter_sdk::execute_parse(
        &*provider,
        &request.to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let result = analyze(
        &analyzer(&provider, case),
        &request,
        &ExtensionSupport::default(),
    );
    validate_ir_document(result.document(), budget.ir(), &ExtensionSupport::default()).unwrap();
    let doc = result.document();
    assert_eq!(doc.files.len(), 1);
    assert_eq!(doc.files[0].content_hash, content_hash(raw));
    let modules: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Module && entity.language == "erb")
        .collect();
    assert_eq!(
        modules.len(),
        1,
        "one source-owned ERB host Module is preserved"
    );
    let module = modules[0];
    assert_eq!(module.kind, EntityKind::Module);
    assert_eq!(module.language, "erb");
    assert_eq!(module.canonical_name, path);
    let source = module.evidence.source.as_ref().unwrap();
    assert_eq!(source.content_hash(), content_hash(raw));
    assert_eq!(
        source.span().start_byte(),
        if encoding == "utf-8" { 0 } else { 2 }
    );
    assert_eq!(source.span().end_byte(), raw.len() as u64);
    assert_eq!(
        module.container,
        Some(rootlight_ir::ContainerRef::File(source.span().file()))
    );
    assert!(
        !doc.skipped_regions.is_empty(),
        "host syntax is not embedded Ruby or rendering qualification"
    );
    (result, parsed)
}

#[test]
fn original_host_module_keeps_erb_identity_and_explicit_ruby_gaps() {
    for path in [
        "written.html.erb",
        "written.js.erb",
        "written.text.erb",
        "written.erb",
    ] {
        let (result, parsed) = output(ORIGINAL, path, "utf-8");
        assert_eq!(
            parsed
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "erb.output_ruby_parsed.expression")
                .count(),
            2
        );
        assert_eq!(parsed.facts().iter().filter(|fact| fact.syntax_kind().as_str() == "erb.statement_ruby_parsed.expression").count(), 1);
        let doc = result.document();
        let parameter = doc
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.language == "ruby"
                    && entity.canonical_name == "customer"
            })
            .expect("native cross-tag block parameter");
        assert!(
            doc.occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Definition
                    && site.source.span().start_byte() == 43
                    && site.source.span().end_byte() == 51
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: parameter.id
                        })
        );
        assert!(
            doc.occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Reference
                    && site.source.span().start_byte() == 59
                    && site.source.span().end_byte() == 67
                    && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
        );
        assert!(
            doc.occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite
                    && site.syntax_kind.starts_with("ruby."))
        );
        assert!(
            doc.occurrences
                .iter()
                .filter(|site| site.role != OccurrenceRole::Definition)
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "syntax alone cannot bind helper dispatch or cross-tag values"
        );
        assert!(doc.skipped_regions.iter().any(|gap| gap.detail == "erb-native-ruby-syntax-without-runtime-or-helper-binding"));
    }
}

#[test]
fn escaped_future_tags_and_comments_never_create_ruby_or_html_entities() {
    let (escaped, parsed) = output(ESCAPED, "future.html.erb", "utf-8");
    assert_eq!(escaped.document().entities.len(), 1);
    assert!(
        escaped
            .document()
            .occurrences
            .iter()
            .all(|site| site.role == OccurrenceRole::StringEvidence
                && site.syntax_kind.starts_with("erb.")),
        "escaped templates retain literal evidence only"
    );
    assert!(!parsed.facts().iter().any(|fact| matches!(
        fact.syntax_kind().as_str(),
        "erb.output_ruby_parsed.expression" | "erb.statement_ruby_parsed.expression"
    )));
    let (comment, parsed) = output(COMMENT, "comment.erb", "utf-8");
    let comments: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.kind() == rootlight_adapter_sdk::SyntaxFactKind::Comment)
        .collect();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].span().start_byte(), 0);
    let end = COMMENT.windows(2).position(|part| part == b"%>").unwrap() as u64 + 2;
    assert_eq!(comments[0].span().end_byte(), end);
    assert!(
        comment
            .document()
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind.starts_with("ruby."))
            .all(|site| site.source.span().start_byte() >= end),
        "comment body never enters Ruby source"
    );
}

#[test]
fn malformed_host_preserves_module_and_reports_native_recovery() {
    let (_, parsed) = output(b"before <% missing", "broken.erb", "utf-8");
    assert_ne!(
        parsed.report().coverage().status(),
        CoverageStatus::Complete
    );
    assert!(
        parsed
            .diagnostics()
            .iter()
            .any(|diagnostic| diagnostic.code().as_str() == "syntax-error-recovery")
    );
}

#[test]
fn utf16_host_module_evidence_uses_original_physical_bytes() {
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
        output(
            &raw,
            "encoded.html.erb",
            if little { "utf-16le" } else { "utf-16be" },
        );
    }
}
