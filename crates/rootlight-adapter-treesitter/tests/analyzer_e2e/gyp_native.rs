//! Native GYP data retains written targets and literal source mentions.
//! The pinned Python AST is a parser input only; these checks require GYP
//! identities and preserve unsupported build configuration instead of execution.

use super::*;

pub(super) const GYP: LanguageCase = LanguageCase {
    name: "gyp",
    path: "native/binding.gyp",
    frontend: "tree-sitter-python-0.25.0-gyp-literal-1",
    source: include_str!("../fixtures/gyp/binding-deno.gyp"),
    generated: false,
    body_before: "src/binding.cc",
    body_after: "src/changed.cc",
};

fn output(raw: &[u8]) -> AnalysisOutput {
    let native = Arc::new(provider());
    let analyzer = analyzer(&native, GYP);
    let fixture = Fixture::new(GYP, raw);
    analyze(
        &analyzer,
        &request(&fixture.snapshot, &fixture.source, GYP, &limits()),
        &ExtensionSupport::default(),
    )
}

#[test]
fn original_deno_and_electron_inputs_keep_exact_native_gyp_targets_and_source_paths() {
    struct Original {
        raw: &'static [u8],
        targets: &'static [(&'static str, u64, u64)],
        // The final flag records authored conditional ancestry, not selection.
        paths: &'static [(&'static str, u64, u64, &'static str, bool)],
    }
    let deno = || Original {
        raw: include_bytes!("../fixtures/gyp/binding-deno.gyp"),
        targets: &[("node_addon", 45, 55)],
        paths: &[("src/binding.cc", 78, 92, "node_addon", false)],
    };
    let originals = [
        deno(),
        deno(),
        Original {
            raw: include_bytes!("../fixtures/gyp/dialog-helper.gyp"),
            targets: &[("dialog_helper", 211, 224)],
            paths: &[
                ("src/main.cc", 309, 320, "dialog_helper", true),
                ("src/dialog_helper_mac.mm", 336, 360, "dialog_helper", true),
                ("src/main.cc", 642, 653, "dialog_helper", true),
                ("src/dialog_helper_win.cc", 669, 693, "dialog_helper", true),
            ],
        },
        Original {
            raw: include_bytes!("../fixtures/gyp/echo.gyp"),
            targets: &[("echo", 45, 49)],
            paths: &[("binding.cc", 80, 90, "echo", false)],
        },
        Original {
            raw: include_bytes!("../fixtures/gyp/is-valid-window.gyp"),
            targets: &[("is_valid_window", 316, 331)],
            paths: &[
                ("src/impl.h", 362, 372, "is_valid_window", false),
                ("src/main.cc", 384, 395, "is_valid_window", false),
                ("src/impl_win.cc", 489, 504, "is_valid_window", true),
                ("src/impl_darwin.mm", 592, 610, "is_valid_window", true),
                ("src/impl_posix.cc", 820, 837, "is_valid_window", true),
            ],
        },
        Original {
            raw: include_bytes!("../fixtures/gyp/mouse-input.gyp"),
            targets: &[("mouse_input", 316, 327)],
            paths: &[
                ("src/main_win.cc", 412, 427, "mouse_input", true),
                ("src/impl.h", 607, 617, "mouse_input", true),
                ("src/impl_mac.cc", 633, 648, "mouse_input", true),
                ("src/main_posix.cc", 664, 681, "mouse_input", true),
                ("src/post_to_window_mac.mm", 697, 722, "mouse_input", true),
                ("src/impl.h", 1329, 1339, "mouse_input", true),
                ("src/impl_linux.cc", 1355, 1372, "mouse_input", true),
                ("src/main_posix.cc", 1388, 1405, "mouse_input", true),
            ],
        },
        Original {
            raw: include_bytes!("../fixtures/gyp/object-wrap.gyp"),
            targets: &[("object_wrap", 45, 56)],
            paths: &[("binding.cc", 87, 97, "object_wrap", false)],
        },
        Original {
            raw: include_bytes!("../fixtures/gyp/uv-dlopen.gyp"),
            targets: &[("test_module", 45, 56), ("libfoo", 127, 133)],
            paths: &[
                ("main.cpp", 79, 87, "test_module", false),
                ("foo.cpp", 188, 195, "libfoo", false),
            ],
        },
        Original {
            raw: include_bytes!("../fixtures/gyp/virtual-display.gyp"),
            targets: &[("virtual_display", 38, 53)],
            paths: &[
                ("src/addon.mm", 130, 142, "virtual_display", true),
                (
                    "src/VirtualDisplayBridge.m",
                    156,
                    182,
                    "virtual_display",
                    true,
                ),
            ],
        },
    ];
    let mut totals = (0, 0, 0);
    for original in originals {
        let raw = original.raw;
        let output = output(raw);
        let doc = output.document();
        assert_eq!(doc.files[0].language, "gyp");
        assert_eq!(doc.files[0].content_hash, content_hash(raw));
        assert_eq!(
            doc.entities
                .iter()
                .filter(|e| e.kind == EntityKind::Module)
                .count(),
            1
        );
        let targets: Vec<_> = doc
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::BuildTarget)
            .collect();
        assert_eq!(
            targets.len(),
            original.targets.len(),
            "{}: {:?}",
            raw.len(),
            doc.skipped_regions
        );
        for &(name, start, end) in original.targets {
            let target = targets.iter().find(|e| e.canonical_name == name).unwrap();
            let definitions: Vec<_> = doc
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: target.id }
                })
                .collect();
            assert_eq!(definitions.len(), 1);
            let span = definitions[0].source.span();
            assert_eq!((span.start_byte(), span.end_byte()), (start, end));
            assert_eq!(
                raw.get(usize::try_from(start).unwrap()..usize::try_from(end).unwrap()),
                Some(name.as_bytes())
            );
            assert_eq!(definitions[0].source.content_hash(), content_hash(raw));
        }
        let references: Vec<_> = doc
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "gyp.source_path.reference")
            .collect();
        assert_eq!(
            references.len(),
            original.paths.len(),
            "{}: {:?}",
            raw.len(),
            doc.skipped_regions
        );
        for &(name, start, end, owner, conditional) in original.paths {
            let reference = references
                .iter()
                .find(|site| {
                    site.source.span().start_byte() == start && site.source.span().end_byte() == end
                })
                .unwrap();
            assert_eq!(reference.role, OccurrenceRole::Reference);
            assert!(matches!(
                reference.target,
                OccurrenceTarget::Unresolved { .. }
            ));
            let target = targets
                .iter()
                .find(|target| target.canonical_name == owner)
                .unwrap();
            assert_eq!(reference.enclosing, Some(target.id));
            assert_eq!(
                raw.get(usize::try_from(start).unwrap()..usize::try_from(end).unwrap()),
                Some(name.as_bytes())
            );
            assert_eq!(reference.syntactic_text_hash, content_hash(name.as_bytes()));
            assert_eq!(reference.source.content_hash(), content_hash(raw));
            let scoped_condition_gap = doc.skipped_regions.iter().any(|gap| {
                gap.detail == "gyp-conditional-configuration-not-evaluated"
                    && gap.source.span().start_byte() <= start
                    && gap.source.span().end_byte() >= end
            });
            assert_eq!(
                scoped_condition_gap, conditional,
                "{owner}: {name} at {start}"
            );
            totals.2 += usize::from(conditional);
        }
        totals.0 += targets.len();
        totals.1 += references.len();
        assert!(doc.entities.iter().all(|e| e.language == "gyp"
            && matches!(e.kind, EntityKind::Module | EntityKind::BuildTarget)));
        assert!(
            !doc.occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::CallSite)
        );
        assert!(
            !doc.relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::DependsOn)
        );
    }
    assert_eq!(totals, (10, 25, 17));
}

#[test]
fn executable_python_recovery_and_unparsed_tails_cannot_donate_gyp_targets() {
    for source in [
        "targets = [{'target_name': 'bad', 'sources': ['x.cc']}]",
        "{'targets': [factory()]}",
        "{'targets': [{'target_name': name}]}",
        "{'targets': [{'target_name': f'bad{value}'}]}",
        "{'targets': [item for item in inputs]}",
        "{'targets': [{'target_name': 'bad'}]}\nimport os\n",
        "'documentation'\n{'targets': [{'target_name': 'bad'}]}",
        "{'targets': [{'target_name': 'bad'}]} !",
        "{'targets': [{'target_name': 'bad'}]",
        "{'targets': [{'target_name': 'bad', 'target_name': 'other'}]}",
    ] {
        let output = output(source.as_bytes());
        assert!(
            !output
                .document()
                .entities
                .iter()
                .any(|e| e.kind == EntityKind::BuildTarget),
            "{source}"
        );
        assert!(
            !output
                .document()
                .occurrences
                .iter()
                .any(|site| site.syntax_kind == "gyp.source_path.reference"
                    || site.role == OccurrenceRole::CallSite),
            "{source}"
        );
        assert!(!output.document().skipped_regions.is_empty(), "{source}");
    }
}

#[test]
fn repeated_written_targets_remain_distinct_without_runtime_uniqueness() {
    let output = output(b"{'targets': [{'target_name': 'same', 'sources': ['a.cc']}, {'target_name': 'same', 'sources': ['b.cc']}]}");
    let targets: Vec<_> = output
        .document()
        .entities
        .iter()
        .filter(|e| e.kind == EntityKind::BuildTarget)
        .collect();
    assert_eq!(targets.len(), 2);
    assert_ne!(targets[0].id, targets[1].id);
    assert!(targets.iter().all(|target| target.canonical_name == "same"));
    assert!(output.document().skipped_regions.iter().any(|gap| gap.detail == "gyp-duplicate-written-target-names-remain-distinct-without-evaluated-uniqueness"));
}

#[test]
fn gyp_required_partition_and_output_quotas_preserve_native_targets() {
    let native = Arc::new(provider());
    let analyzer = analyzer(&native, GYP);
    let fixture = Fixture::new(GYP, GYP.source.as_bytes());
    let full = limits();
    let input = request(&fixture.snapshot, &fixture.source, GYP, &full);
    let required = native
        .required_syntax_fact_count(&input.to_parse_request(), &deadline())
        .unwrap();
    let exact = limits_with_syntax_records(required);
    let exact_input = request(&fixture.snapshot, &fixture.source, GYP, &exact);
    let admitted = analyze(&analyzer, &exact_input, &ExtensionSupport::default());
    assert_eq!(
        admitted
            .document()
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::BuildTarget)
            .count(),
        1
    );
    assert_eq!(
        admitted
            .document()
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "gyp.source_path.reference")
            .count(),
        1
    );
    let short = limits_with_syntax_records(required - 1);
    assert!(matches!(
        analyzer.analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, GYP, &short),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline()
        ),
        Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::RequiredSyntaxFacts,
            ..
        }))
    ));
    let mut ir = IrLimits::default();
    ir.max_total_records = 1;
    let bounded = AnalysisLimits::new(
        full.max_source_bytes(),
        full.max_syntax_nodes(),
        full.max_syntax_depth(),
        full.max_embedded_ranges(),
        full.max_reported_memory_bytes(),
        full.syntax_stream().clone(),
        full.ir_stream().clone(),
        ir,
    )
    .unwrap();
    assert!(matches!(
        analyzer.analyze_and_capture(
            &request(&fixture.snapshot, &fixture.source, GYP, &bounded),
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline()
        ),
        Err(AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::Records,
            ..
        }))
    ));
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        analyzer.analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &cancel
        ),
        Err(AdapterError::Cancelled { .. })
    ));
}

#[test]
fn gyp_artifacts_rebind_current_sources_without_crossing_the_python_profile() {
    let native = Arc::new(provider());
    let gyp = analyzer(&native, GYP);
    let python_case = LanguageCase {
        name: "python",
        frontend: "tree-sitter-python-0.25.0",
        ..GYP
    };
    let python = analyzer(&native, python_case);
    let fixture = Fixture::new(GYP, GYP.source.as_bytes());
    let limits = limits();
    let input = request(&fixture.snapshot, &fixture.source, GYP, &limits);
    let (_, artifact) = gyp
        .analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let successor = fixture.next_generation();
    let current = request(&successor.snapshot, &successor.source, GYP, &limits);
    let rebound = gyp
        .analyze_from_artifact(
            &current,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    let clean = analyze(&gyp, &current, &ExtensionSupport::default());
    assert_eq!(rebound.document(), clean.document());
    assert_eq!(
        artifact.required_syntax_fact_count(&deadline()).unwrap(),
        native
            .required_syntax_fact_count(&current.to_parse_request(), &deadline())
            .unwrap()
    );
    let foreign = request(&fixture.snapshot, &fixture.source, python_case, &limits);
    assert!(
        python
            .analyze_from_artifact(
                &foreign,
                &artifact,
                ExtensionSupport::default(),
                MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
                &deadline()
            )
            .is_err()
    );
    let (_, python_artifact) = python
        .analyze_and_capture(
            &foreign,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        )
        .unwrap();
    assert!(
        gyp.analyze_from_artifact(
            &input,
            &python_artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline()
        )
        .is_err()
    );
    let changed = fixture.rewrite(
        &GYP.source
            .replace("src/binding.cc", "src/changed.cc")
            .into_bytes(),
    );
    let changed_input = request(&changed.snapshot, &changed.source, GYP, &limits);
    assert!(
        gyp.analyze_from_artifact(
            &changed_input,
            &artifact,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline()
        )
        .is_err()
    );
    assert_eq!(
        analyze(&gyp, &changed_input, &ExtensionSupport::default())
            .document()
            .entities
            .iter()
            .filter(|e| e.kind == EntityKind::BuildTarget)
            .count(),
        1
    );
}

#[test]
fn gyp_partial_ranges_and_native_node_depth_byte_limits_never_certify_targets() {
    let native = Arc::new(provider());
    let analyzer = analyzer(&native, GYP);
    let source = format!("{}\nimport foreign\n", GYP.source);
    let fixture = Fixture::new(GYP, source.as_bytes());
    let full = limits();
    let input = AnalysisRequest::new_with_parse_context(
        GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
        language(GYP),
        EncodingId::utf8(),
        vec![IncludedRange::new(
            SourceSpan::new(
                fixture.snapshot.file(),
                0,
                u64::try_from(GYP.source.len()).unwrap(),
            )
            .unwrap(),
            language(GYP),
        )],
        AnalysisTier::TierD,
        build_context(),
        &full,
    )
    .unwrap()
    .with_generated_status(false);
    let partial = analyze(&analyzer, &input, &ExtensionSupport::default());
    assert!(
        !partial
            .document()
            .entities
            .iter()
            .any(|e| e.kind == EntityKind::BuildTarget)
    );
    assert!(
        !partial
            .document()
            .occurrences
            .iter()
            .any(|site| site.syntax_kind == "gyp.source_path.reference")
    );
    for (bytes, nodes, depth) in [
        (GYP.source.len() - 1, MAX_SYNTAX_NODES, MAX_SYNTAX_DEPTH),
        (MAX_SOURCE_BYTES, 1, MAX_SYNTAX_DEPTH),
        (MAX_SOURCE_BYTES, MAX_SYNTAX_NODES, 1),
    ] {
        let bounded = AnalysisLimits::new(
            bytes,
            nodes,
            depth,
            full.max_embedded_ranges(),
            full.max_reported_memory_bytes(),
            full.syntax_stream().clone(),
            full.ir_stream().clone(),
            IrLimits::default(),
        )
        .unwrap();
        let fixture = Fixture::new(GYP, GYP.source.as_bytes());
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(GYP),
            EncodingId::utf8(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &bounded,
        );
        let input = match input {
            Ok(input) => input.with_generated_status(false),
            Err(rootlight_adapter_sdk::RequestError::SourceTooLarge { .. }) => {
                assert!(bytes < GYP.source.len());
                continue;
            }
            other => panic!("unexpected GYP request admission: {other:?}"),
        };
        let result = analyzer.analyze_and_capture(
            &input,
            ExtensionSupport::default(),
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &deadline(),
        );
        match result {
            Ok((output, _)) => assert!(
                !output
                    .document()
                    .entities
                    .iter()
                    .any(|e| e.kind == EntityKind::BuildTarget)
            ),
            Err(AdapterError::Sink(SinkError::StreamLimit { .. })) => {}
            other => panic!("unexpected GYP resource refusal: {other:?}"),
        }
    }
}

#[test]
fn gyp_bom_crlf_and_utf16_keep_exact_physical_target_and_path_spans() {
    let text = GYP.source.replace('\n', "\r\n");
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let raw = if encoding == "utf-8" {
            [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat()
        } else {
            let little = encoding == "utf-16le";
            let mut raw = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in text.encode_utf16() {
                raw.extend(if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            raw
        };
        let native = Arc::new(provider());
        let fixture = Fixture::new(GYP, &raw);
        let limits = limits();
        let input = AnalysisRequest::new_with_parse_context(
            GenerationBoundSnapshot::new(&fixture.snapshot, &fixture.source).unwrap(),
            language(GYP),
            EncodingId::new(encoding).unwrap(),
            Vec::new(),
            AnalysisTier::TierD,
            build_context(),
            &limits,
        )
        .unwrap()
        .with_generated_status(false);
        let output = analyze(
            &analyzer(&native, GYP),
            &input,
            &ExtensionSupport::default(),
        );
        for (name, role) in [
            ("node_addon", OccurrenceRole::Definition),
            ("src/binding.cc", OccurrenceRole::Reference),
        ] {
            let sites: Vec<_> = output
                .document()
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == role && site.syntactic_text_hash == content_hash(name.as_bytes())
                })
                .collect();
            assert_eq!(sites.len(), 1, "{encoding}: {name}");
            let physical = if encoding == "utf-8" {
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
            let span = sites[0].source.span();
            assert_eq!(
                raw.get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap()
                ),
                Some(physical.as_slice())
            );
            assert_eq!(sites[0].source.content_hash(), content_hash(&raw));
        }
    }
}
