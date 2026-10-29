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

fn definition_at(
    doc: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
) -> rootlight_ids::SymbolId {
    let site = doc
        .occurrences
        .iter()
        .find(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
        })
        .unwrap();
    let OccurrenceTarget::Resolved { symbol } = site.target else {
        panic!("native definition target")
    };
    symbol
}

fn read_at(
    doc: &rootlight_ir::NormalizedIrDocument,
    start: u64,
    end: u64,
    target: rootlight_ids::SymbolId,
) {
    let reads: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Read
                && site.source.span().start_byte() == start
                && site.source.span().end_byte() == end
                && site.target == OccurrenceTarget::Resolved { symbol: target }
        })
        .collect();
    assert_eq!(reads.len(), 1, "one source-proven read at {start}..{end}");
    let read = reads[0];
    assert_eq!(read.source.content_hash(), doc.files[0].content_hash);
    assert_eq!(
        doc.relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.subject == RelationEndpoint::Occurrence(read.id)
                && edge.object == RelationEndpoint::Entity(target)
                && edge.evidence.source.as_ref() == Some(&read.source))
            .count(),
        1
    );
}

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
        read_at(doc, 59, 67, parameter.id);
        let block = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == "ruby.block.scope"
                    && fact.span().start_byte() == 39
                    && fact.span().end_byte() == 81
            })
            .unwrap();
        let declaration = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == "ruby.parameter.declaration"
                    && fact.span().start_byte() == 43
            })
            .unwrap();
        assert_eq!(declaration.parent(), Some(block.local_id()));
        let reference = parsed
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == "ruby.identifier.reference"
                    && fact.span().start_byte() == 59
            })
            .unwrap();
        let body = parsed
            .facts()
            .iter()
            .find(|fact| Some(fact.local_id()) == reference.parent())
            .unwrap();
        assert_eq!(body.parent(), Some(block.local_id()));
        assert_eq!(reference.depth(), body.depth() + 1);
        assert_eq!(body.depth(), block.depth() + 1);
        assert!(
            !doc.entities.iter().any(|entity| matches!(
                entity.kind,
                EntityKind::Function | EntityKind::Method
            ) && entity.evidence.source.as_ref().is_some_and(
                |source| source.span().start_byte() == 39 && source.span().end_byte() == 81
            )),
            "an anonymous block is a native scope, not a fabricated named entity"
        );
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
                .any(|site| site.role == OccurrenceRole::Read
                    && site.source.span().start_byte() == 59
                    && site.source.span().end_byte() == 67
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: parameter.id
                        })
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
                .filter(|site| !matches!(
                    site.role,
                    OccurrenceRole::Definition | OccurrenceRole::Read
                ))
                .all(|site| matches!(site.target, OccurrenceTarget::Unresolved { .. })),
            "syntax alone cannot bind helper dispatch or cross-tag values"
        );
        assert!(doc.skipped_regions.iter().any(|gap| gap.detail == "erb-ruby-plain-block-parameter-reads-only-helper-property-and-runtime-binding-unavailable"));
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
        let (result, _) = output(
            &raw,
            "encoded.html.erb",
            if little { "utf-16le" } else { "utf-16be" },
        );
        let doc = result.document();
        read_at(
            doc,
            2 + 59 * 2,
            2 + 67 * 2,
            definition_at(doc, 2 + 43 * 2, 2 + 51 * 2),
        );
    }
}

#[test]
fn other_names_and_tag_layouts_use_their_native_block_parameter() {
    for (raw, name, needle) in [
        (
            "<% things.each do |item| %>\n<strong><%= item.title %></strong>\n<% end %>",
            "item",
            "item.title",
        ),
        (
            "<% data.each { |entry| %><%= entry.value %><% } %>",
            "entry",
            "entry.value",
        ),
    ] {
        let (result, _) = output(raw.as_bytes(), "other.erb", "utf-8");
        let declaration = raw.find(&format!("|{name}|")).unwrap() as u64 + 1;
        let read = raw.find(needle).unwrap() as u64;
        let target = definition_at(
            result.document(),
            declaration,
            declaration + name.len() as u64,
        );
        read_at(result.document(), read, read + name.len() as u64, target);
    }
}

#[test]
fn nested_same_name_parameters_refuse_ambiguous_targets_and_keep_native_scopes() {
    let raw = "<% outer.each do |item| %><%= item %><% inner.each do |item| %><%= item %><% end %><%= item %><% end %>";
    let (result, parsed) = output(raw.as_bytes(), "nested.erb", "utf-8");
    assert!(
        !result
            .document()
            .occurrences
            .iter()
            .any(|site| site.role == OccurrenceRole::Read
                && matches!(site.target, OccurrenceTarget::Resolved { .. })),
        "ambiguous materialized parameters cannot donate a target across native shadow scopes"
    );
    assert!(
        result
            .document()
            .skipped_regions
            .iter()
            .any(|gap| gap.reason == SkippedRegionReason::UnsupportedConstruct)
    );
    let declaration_facts: Vec<_> = parsed
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "ruby.parameter.declaration")
        .collect();
    assert_eq!(declaration_facts.len(), 2);
    assert_ne!(declaration_facts[0].parent(), declaration_facts[1].parent());
}

#[test]
fn unsupported_binders_scope_boundaries_and_nonvalue_uses_do_not_guess_reads() {
    for raw in [
        "<% items.each do |person| %><% end %><%= person %>",
        "<% items.each do |person| %><% other.each do %><%= person %><% end %><% end %>",
        "<% items.each do |person| %><% def show; person; end %><% end %>",
        "<% items.each do |person| %><% class View; person; end %><% end %>",
        "<% items.each do |(person, address)| %><%= person %><% end %>",
        "<% items.each do |*person| %><%= person %><% end %>",
        "<% items.each do |; person| %><%= person %><% end %>",
        "<% items.each do |person| %><% person = changed %><%= person %><% end %>",
        "<% items.each do |person| %><%= object.person %><% end %>",
        "<% items.each do |person| %><%= defined? person %><% end %>",
        "<% items.each do |person| %><%= person.foo( %><% end %>",
    ] {
        let (result, _) = output(raw.as_bytes(), "negative.erb", "utf-8");
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "no unsupported block read: {raw}"
        );
    }
}

#[test]
fn current_native_scope_and_capture_evidence_is_required_for_block_reads() {
    use rootlight_adapter_sdk::{
        AdapterDiagnostic, CoverageReport, DiagnosticCode, SyntaxFact, testkit::MockParseProvider,
    };
    let native = Arc::new(provider());
    let fixture = Fixture::new(ERB, ORIGINAL);
    let bounds = limits();
    let parsed = rootlight_adapter_sdk::execute_parse(
        &*native,
        &request(&fixture.snapshot, &fixture.source, ERB, &bounds).to_parse_request(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .unwrap();
    let replay =
        |fixture: &Fixture, facts: Vec<SyntaxFact>, diagnostic: Option<&str>, included: bool| {
            let mut diagnostics = parsed.diagnostics().to_vec();
            let coverage = if let Some(code) = diagnostic {
                diagnostics.push(AdapterDiagnostic::new(
                    DiagnosticCode::new(code).unwrap(),
                    rootlight_ir::DiagnosticSeverity::Warning,
                    None,
                    CoverageStatus::Bounded,
                ));
                CoverageReport::new(
                    AnalysisTier::TierD,
                    CoverageStatus::Bounded,
                    ORIGINAL.len(),
                    ORIGINAL.len(),
                    1,
                    Vec::new(),
                )
                .unwrap()
            } else {
                parsed.report().coverage().clone()
            };
            let mock =
                MockParseProvider::new(native.capabilities().clone(), facts, diagnostics, coverage);
            let frontend = TreeSitterAnalyzer::new(
                Arc::new(mock),
                producer_identity(),
                language(ERB),
                ERB.frontend,
                content_hash(BINARY_SEED),
            )
            .unwrap();
            let ranges = if included {
                vec![rootlight_adapter_sdk::IncludedRange::new(
                    SourceSpan::new(fixture.source.span().file(), 3, 53).unwrap(),
                    language(ERB),
                )]
            } else {
                Vec::new()
            };
            let input = AnalysisRequest::new_with_parse_context(
                GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
                language(ERB),
                EncodingId::utf8(),
                ranges,
                AnalysisTier::TierD,
                build_context(),
                &bounds,
            )
            .unwrap()
            .with_generated_status(false);
            execute_analysis(
                &frontend,
                &input,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline(),
            )
        };
    let baseline = replay(&fixture, parsed.facts().to_vec(), None, false).unwrap();
    read_at(
        baseline.document(),
        59,
        67,
        definition_at(baseline.document(), 43, 51),
    );
    for witness in parsed.facts().iter().filter(|fact| {
        matches!(
            fact.syntax_kind().as_str(),
            "erb.template.root"
                | "erb.file.module"
                | "erb.output_ruby_parsed.expression"
                | "erb.statement_ruby_parsed.expression"
                | "ruby.file.scope"
                | "ruby.block.scope"
                | "ruby.parameter.declaration"
                | "ruby.identifier.definition"
                | "ruby.identifier.reference"
        ) && (fact.syntax_kind().as_str() != "ruby.identifier.reference"
            || fact.span().start_byte() == 59)
    }) {
        for mode in 0..4 {
            let mut facts = parsed.facts().to_vec();
            if mode == 0 {
                facts.retain(|fact| fact.local_id() != witness.local_id());
            } else if mode == 1 {
                facts.push(SyntaxFact::new(
                    facts.iter().map(SyntaxFact::local_id).max().unwrap() + 1,
                    witness.parent(),
                    witness.kind(),
                    witness.span(),
                    witness.depth(),
                    witness.syntax_kind().clone(),
                ));
            } else {
                let fact = facts
                    .iter_mut()
                    .find(|fact| fact.local_id() == witness.local_id())
                    .unwrap();
                let span = if mode == 2 {
                    SourceSpan::new(
                        fact.span().file(),
                        fact.span().start_byte() + 1,
                        fact.span().end_byte(),
                    )
                    .unwrap()
                } else {
                    fact.span()
                };
                *fact = SyntaxFact::new(
                    fact.local_id(),
                    if mode == 3 { None } else { fact.parent() },
                    fact.kind(),
                    span,
                    fact.depth() + usize::from(mode == 3),
                    fact.syntax_kind().clone(),
                );
            }
            if let Ok(result) = replay(&fixture, facts, None, false) {
                assert!(
                    !result
                        .document()
                        .occurrences
                        .iter()
                        .any(|site| site.role == OccurrenceRole::Read
                            && matches!(site.target, OccurrenceTarget::Resolved { .. })),
                    "missing or ambiguous native evidence: {} mode{mode}",
                    witness.syntax_kind().as_str()
                );
            }
        }
    }
    for diagnostic in [
        "syntax-extraction-limit",
        "syntax-node-limit",
        "syntax-error-recovery",
    ] {
        let result = replay(&fixture, parsed.facts().to_vec(), Some(diagnostic), false).unwrap();
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
    }
    if let Ok(result) = replay(&fixture, parsed.facts().to_vec(), None, true) {
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
    }
    let changed = fixture.rewrite(
        std::str::from_utf8(ORIGINAL)
            .unwrap()
            .replace("|customer|", "|visitor_|")
            .as_bytes(),
    );
    if let Ok(result) = replay(&changed, parsed.facts().to_vec(), None, false) {
        assert!(
            !result
                .document()
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Read
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
    }
}
