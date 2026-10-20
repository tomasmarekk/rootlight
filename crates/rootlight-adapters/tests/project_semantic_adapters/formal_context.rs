//! Public native/project checks for formal-context identity and bounded evidence.
//!
//! Context body captures must neither replace written formals nor lose their source gap.

use super::*;

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
