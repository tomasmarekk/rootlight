//! Named-declaration boundaries in the public project-analysis path.
//!
//! The controlled parser emits valid SDK facts without claiming a native
//! language currently produces the deliberately unsupported declaration label.

use super::*;

#[test]
fn project_declaration_association_stops_at_unknown_named_declarations() {
    let source = "class Outer:\n    inner = 0\n";
    for through_scope in [false, true] {
        let fixture = ProjectFixture::new(["main.py"], [source], SemanticProjectLanguage::Python);
        let output = analyze_boundary(&fixture, true, through_scope);
        assert_outer_definition(&output, &fixture, source);
        let inner_start = u64::try_from(source.find("inner").unwrap()).unwrap();
        assert!(!output.document().occurrences.iter().any(|site| {
            site.role == OccurrenceRole::Definition
                && site.source.span().start_byte() == inner_start
                && site.source.span().end_byte() == inner_start + 5
        }));
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|entity| { entity.canonical_name == "inner" })
        );
    }
}

#[test]
fn project_declaration_association_keeps_lexical_scopes_transparent() {
    let source = "class Outer:\n    pass\n";
    let fixture = ProjectFixture::new(["main.py"], [source], SemanticProjectLanguage::Python);
    let output = analyze_boundary(&fixture, false, true);
    assert_outer_definition(&output, &fixture, source);
}

fn assert_outer_definition(
    output: &rootlight_adapter_sdk::ProjectAnalysisOutput,
    fixture: &ProjectFixture,
    source: &str,
) {
    let classes: Vec<_> = output
        .document()
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Class)
        .collect();
    assert_eq!(classes.len(), 1);
    assert_eq!(classes[0].canonical_name, "Outer");
    let definitions: Vec<_> = output
        .document()
        .occurrences
        .iter()
        .filter(|site| site.role == OccurrenceRole::Definition)
        .collect();
    assert_eq!(definitions.len(), 1);
    let definition = definitions[0];
    assert_eq!(
        definition.source.span(),
        span(fixture.snapshots[0].file(), 6, 11)
    );
    assert_eq!(
        definition.source.repository(),
        fixture.sources[0].repository()
    );
    assert_eq!(
        definition.source.generation(),
        fixture.sources[0].generation()
    );
    assert_eq!(
        definition.source.content_hash(),
        content_hash(source.as_bytes())
    );
    assert_eq!(
        definition.target,
        OccurrenceTarget::Resolved {
            symbol: classes[0].id
        }
    );
}

fn analyze_boundary(
    fixture: &ProjectFixture,
    unknown_declaration: bool,
    through_scope: bool,
) -> rootlight_adapter_sdk::ProjectAnalysisOutput {
    let parser = AssociationParser {
        capabilities: FixtureParser::new().capabilities,
        unknown_declaration,
        through_scope,
    };
    let analyzer = SemanticProjectAnalyzer::new(
        fixture.language,
        Arc::new(parser),
        producer_identity(),
        content_hash(b"declaration-boundary-fixture-parser"),
        fixture.build_context,
    )
    .unwrap();
    execute_project_analysis(
        &analyzer,
        &fixture.request(&limits(), AnalysisTier::TierB),
        ExtensionSupport::default(),
        MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
        &deadline(),
    )
    .expect("valid typed facts commit through the public project SDK")
}

struct AssociationParser {
    capabilities: ParseCapabilities,
    unknown_declaration: bool,
    through_scope: bool,
}

impl ParseProvider for AssociationParser {
    fn capabilities(&self) -> &ParseCapabilities {
        &self.capabilities
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        cancellation.check()?;
        let source = std::str::from_utf8(request.source().bytes()).unwrap();
        let file = request.source().source_ref().span().file();
        let full = span(file, 0, source.len());
        let fact = |id, parent, kind, span, depth, label| {
            SyntaxFact::new(
                id,
                parent,
                kind,
                span,
                depth,
                SyntaxKindLabel::new(label).unwrap(),
            )
        };
        let mut facts = vec![
            fact(1, None, SyntaxFactKind::Root, full, 0, "python.file.root"),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Declaration,
                full,
                1,
                "python.class.declaration",
            ),
        ];
        if !self.unknown_declaration && self.through_scope {
            facts.push(fact(
                3,
                Some(2),
                SyntaxFactKind::Scope,
                span(file, 6, 11),
                2,
                "python.name.scope",
            ));
        }
        facts.push(fact(
            4,
            Some(if !self.unknown_declaration && self.through_scope {
                3
            } else {
                2
            }),
            SyntaxFactKind::Occurrence,
            span(file, 6, 11),
            if !self.unknown_declaration && self.through_scope {
                3
            } else {
                2
            },
            "python.definition",
        ));
        if self.unknown_declaration {
            let start = source.find("inner").unwrap();
            let extent = span(file, start, source.len() - 1);
            let unknown = fact(
                5,
                Some(2),
                SyntaxFactKind::Declaration,
                extent,
                2,
                "python.opaque.declaration",
            );
            assert_eq!(
                rootlight_adapter_sdk::structural_entity_kind(&unknown),
                None
            );
            facts.push(unknown);
            if self.through_scope {
                facts.push(fact(
                    6,
                    Some(5),
                    SyntaxFactKind::Scope,
                    extent,
                    3,
                    "python.opaque.scope",
                ));
            }
            facts.push(fact(
                7,
                Some(if self.through_scope { 6 } else { 5 }),
                SyntaxFactKind::Occurrence,
                span(file, start, start + 5),
                if self.through_scope { 4 } else { 3 },
                "python.definition",
            ));
        }
        facts.sort_by_key(|fact| (fact.span(), fact.local_id()));
        sink.push(rootlight_adapter_sdk::SyntaxFactBatch::new(
            sink.next_sequence(),
            facts.clone(),
            Vec::new(),
        ))?;
        let usage = sink.staged_usage();
        let coverage = CoverageReport::new(
            AnalysisTier::TierD,
            CoverageStatus::Complete,
            source.len(),
            source.len(),
            0,
            Vec::new(),
        )?;
        WorkReport::new(
            coverage,
            ResourceUsage::new(
                source.len(),
                facts.len(),
                facts.len(),
                facts.iter().map(SyntaxFact::depth).max().unwrap_or(0),
                None,
                usage,
            ),
            StreamEnd::new(sink.next_sequence(), usage),
        )
        .map_err(AdapterError::from)
    }
}
