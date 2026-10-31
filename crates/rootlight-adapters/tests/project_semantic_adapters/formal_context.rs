//! Public native/project checks for formal-context identity and bounded evidence.
//!
//! Context body captures must neither replace written formals nor lose their source gap.

use super::*;

#[test]
fn python_written_locals_preserve_owners_without_cross_scope_name_fallback() {
    let text = "def owned():\n    value = 1\n    result = value + 2\n    return value\n\nprint(value)\n\ndef other():\n    return value\n";
    let fixture = ProjectFixture::new(["written.py"], [text], SemanticProjectLanguage::Python);
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let local = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Variable && entity.canonical_name == "value")
        .expect("project path keeps the written local");
    let owner = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "owned")
        .unwrap();
    assert_eq!(
        local.container,
        Some(rootlight_ir::ContainerRef::Entity(owner.id))
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == rootlight_ir::RelationEndpoint::Entity(owner.id)
                    && edge.object == rootlight_ir::RelationEndpoint::Entity(local.id)
            })
            .count(),
        1
    );
    let reads: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Reference
                && text.as_bytes().get(
                    usize::try_from(site.source.span().start_byte()).unwrap()
                        ..usize::try_from(site.source.span().end_byte()).unwrap(),
                ) == Some(b"value".as_slice())
        })
        .collect();
    assert_eq!(reads.len(), 4);
    let own_reads = [
        u64::try_from(text.find("value + 2").unwrap()).unwrap(),
        u64::try_from(text.find("return value").unwrap() + 7).unwrap(),
    ];
    for site in reads {
        if own_reads.contains(&site.source.span().start_byte()) {
            assert_eq!(site.syntax_kind, "python.local_read.reference");
            assert_eq!(site.enclosing, Some(owner.id));
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: local.id });
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|edge| {
                        edge.predicate == RelationPredicate::RefersTo
                            && edge.subject == rootlight_ir::RelationEndpoint::Occurrence(site.id)
                            && edge.object == rootlight_ir::RelationEndpoint::Entity(local.id)
                            && edge.evidence.source.as_ref() == Some(&site.source)
                    })
                    .count(),
                1
            );
        } else {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
            assert!(!document.relations.iter().any(|edge| {
                edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == rootlight_ir::RelationEndpoint::Occurrence(site.id)
            }));
        }
    }
}

#[test]
fn refused_python_local_reads_do_not_adopt_same_named_imports() {
    for (header, body) in [
        (
            "owned()",
            "    value = 1\n    value = 2\n    return value\n",
        ),
        (
            "owned()",
            "    value = 1\n    value += 2\n    return value\n",
        ),
        (
            "owned()",
            "    consume(value)\n    value = 1\n    return value\n",
        ),
        ("owned(value)", "    value = 1\n    return value\n"),
        (
            "owned()",
            "    global value\n    value = 1\n    return value\n",
        ),
        (
            "owned()",
            "    nonlocal value\n    value = 1\n    return value\n",
        ),
        ("owned()", "    value, peer = [1, 2]\n    return value\n"),
        (
            "owned()",
            "    if True:\n        value = 1\n    return value\n",
        ),
    ] {
        let text = format!("from peer import value\n\ndef {header}:\n{body}");
        let fixture = ProjectFixture::new(
            ["peer.py", "written.py"],
            ["value = 0\n", text.as_str()],
            SemanticProjectLanguage::Python,
        );
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        assert!(
            document.entities.iter().any(|entity| {
                entity.kind == EntityKind::Variable
                    && entity.canonical_name == "value"
                    && entity.evidence.source.as_ref().is_some_and(|source| {
                        source.span().file() == fixture.sources[0].span().file()
                    })
            }),
            "the imported same-name target must exist: {text}"
        );
        let start = u64::try_from(text.rfind("return value").unwrap() + 7).unwrap();
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference
                    && site.file == fixture.sources[1].span().file()
                    && site.source.span().start_byte() == start
            })
            .unwrap();
        assert!(
            matches!(site.target, OccurrenceTarget::Unresolved { .. }),
            "refused local cannot inherit its import: {text}"
        );
        assert!(!document.relations.iter().any(|edge| {
            edge.predicate == RelationPredicate::RefersTo
                && edge.subject == rootlight_ir::RelationEndpoint::Occurrence(site.id)
        }));
    }
}

#[test]
fn java_and_go_body_edits_keep_native_project_formal_identity_and_header_edits_change_it() {
    for (language, path, kind, sources, expected) in [
        (
            SemanticProjectLanguage::Go,
            "written.go",
            EntityKind::Function,
            [
                "package written\ntype Holder struct { data []byte }\nfunc owned(value []byte) *Holder { return &Holder{data: value} }\n",
                "package written\ntype Holder struct { data []byte }\nfunc owned(value []byte) *Holder { return &Holder{data: nil} }\n",
                "package written\ntype Holder struct { data []byte }\nfunc owned(value []byte, other []byte) *Holder { return &Holder{data: value} }\n",
            ],
            [
                "(value[]byte)",
                "(value[]byte)",
                "(value[]byte,other[]byte)",
            ],
        ),
        (
            SemanticProjectLanguage::Go,
            "argument.go",
            EntityKind::Function,
            [
                "package written\nfunc owned(value int) { external(value) }\n",
                "package written\nfunc owned(value int) { other(value) }\n",
                "package written\nfunc owned(value int, other int) { external(value) }\n",
            ],
            ["(valueint)", "(valueint)", "(valueint,otherint)"],
        ),
        (
            SemanticProjectLanguage::Java,
            "Written.java",
            EntityKind::Method,
            [
                "class Written { Object field; void owned(Object value) { this.field = value; } }\n",
                "class Written { Object field; void owned(Object value) { this.field = null; } }\n",
                "class Written { Object field; void owned(Object value, Object other) { this.field = value; } }\n",
            ],
            [
                "(Objectvalue)",
                "(Objectvalue)",
                "(Objectvalue,Objectother)",
            ],
        ),
    ] {
        let mut identities = Vec::new();
        for (text, signature) in sources.into_iter().zip(expected) {
            let fixture = ProjectFixture::new([path], [text], language);
            assert_real_parser_symbol_identity(
                &fixture,
                language.as_str(),
                &[(kind, "owned"), (EntityKind::Parameter, "value")],
            );
            let project = analyze_with_real_parser(&fixture);
            let entity = project
                .document()
                .entities
                .iter()
                .find(|entity| entity.kind == kind && entity.display_name == "owned")
                .unwrap();
            let claim = project
                .document()
                .extensions
                .iter()
                .filter_map(|envelope| decode_symbol_identity_claim_envelope(envelope).ok())
                .find(|claim| claim.symbol == entity.id)
                .unwrap();
            assert_eq!(claim.signature_discriminator, signature.as_bytes());
            identities.push(entity.id);
        }
        assert_eq!(
            identities[0], identities[1],
            "body edits preserve {language:?} callable identity"
        );
        assert_ne!(
            identities[0], identities[2],
            "written formal changes distinguish {language:?} callable identity"
        );
    }
}

#[test]
fn java_and_go_native_context_limits_reach_project_exact_source_relation_coverage() {
    for (language, path, text, reason) in [
        (
            SemanticProjectLanguage::Go,
            "written.go",
            "package written\ntype Holder struct { data []byte }\nfunc owned(value []byte) *Holder { return &Holder{data: value} }\n",
            "go-formal-proof-is-limited-to-ascii-free-functions-and-direct-single-return-literal-values",
        ),
        (
            SemanticProjectLanguage::Go,
            "argument.go",
            "package written\nfunc owned(value int) { external(value) }\n",
            "go-call-argument-proof-requires-clean-free-function-and-no-formal-rebinding-or-inner-function",
        ),
        (
            SemanticProjectLanguage::Java,
            "Written.java",
            "class Written { Object field; void owned(Object value) { this.field = value; } }\n",
            "java-formal-binding-proof-is-limited-to-single-assignment-setters",
        ),
    ] {
        let fixture = ProjectFixture::new([path], [text], language);
        let limits = real_parser_limits();
        let native_analyzer = TreeSitterAnalyzer::new(
            Arc::new(real_parser()),
            producer_identity(),
            LanguageId::new(language.as_str()).unwrap(),
            language.as_str(),
            content_hash(b"real-parser-binary"),
        )
        .unwrap();
        let request = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshots[0], &fixture.sources[0]).unwrap(),
            LanguageId::new(language.as_str()).unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            fixture.build_context,
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let native = execute_analysis(
            &native_analyzer,
            &request,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
        let native_gaps: Vec<_> = native
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.domain == FactDomain::Relations && gap.detail == reason)
            .collect();
        assert_eq!(
            native_gaps.len(),
            1,
            "actual native context is a required prerequisite"
        );
        let project = analyze_with_real_parser(&fixture);
        let gaps: Vec<_> = project
            .document()
            .skipped_regions
            .iter()
            .filter(|gap| gap.domain == FactDomain::Relations && gap.detail == reason)
            .collect();
        assert_eq!(gaps.len(), 1);
        assert_eq!(gaps[0].source, native_gaps[0].source);
        assert_eq!(gaps[0].reason, native_gaps[0].reason);
        assert_eq!(gaps[0].source.repository(), fixture.sources[0].repository());
        assert_eq!(gaps[0].source.generation(), fixture.sources[0].generation());
        assert_eq!(gaps[0].source.content_hash(), content_hash(text.as_bytes()));
        let start = text
            .find(if language == SemanticProjectLanguage::Go {
                "func owned"
            } else {
                "void owned"
            })
            .unwrap();
        let end = if language == SemanticProjectLanguage::Go {
            text.trim_end_matches('\n').len()
        } else {
            text.rfind(" }").unwrap()
        };
        assert_eq!(gaps[0].source.span().start_byte(), start as u64);
        assert_eq!(gaps[0].source.span().end_byte(), end as u64);
        let coverage: Vec<_> = project
            .document()
            .coverage_records
            .iter()
            .filter(|record| {
                record.domain == FactDomain::Relations
                    && record.scope
                        == rootlight_ir::CoverageScope::File(fixture.sources[0].span().file())
            })
            .collect();
        assert_eq!(coverage.len(), 1);
        assert_ne!(coverage[0].status, CoverageStatus::Complete);
        assert!(coverage[0].skipped > 0);
        assert!(!project.document().skipped_regions.iter().any(|gap| gap.detail == if language == SemanticProjectLanguage::Go { "java-formal-binding-proof-is-limited-to-single-assignment-setters" } else { "go-formal-proof-is-limited-to-ascii-free-functions-and-direct-single-return-literal-values" }));
    }
}
