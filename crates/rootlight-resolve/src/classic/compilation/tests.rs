use super::*;
use rootlight_ids::{GenerationId, RepositoryId};
use rootlight_ir::{
    AnalysisTier, BuildContextIdentity, FactEvidence, FileRecord, ProducerIdentity,
    ProvenanceRecord, VbaCompilationBranch, new_vba_compilation_envelope,
};

const SOURCE: &str = concat!(
    "Attribute VB_Name = \"Module\"\n",
    "Public Sub Always()\n",
    "value = Uncertain() ' active unknown target\n",
    "#If True Then\n",
    "value = Selected() ' active use\n",
    "#Else\n",
    "value = Excluded() ' inactive use\n",
    "#End If\n",
    "End Sub\n",
    "#If HostChoice Then\n",
    "Public Sub Uncertain()\n",
    "value = Selected() ' unknown use\n",
    "End Sub\n",
    "#End If\n",
    "Public Sub Selected()\nEnd Sub\n",
    "#If False Then\n",
    "Public Sub Excluded()\nEnd Sub\n",
    "#End If\n",
);

struct Fixture {
    files: Vec<FileRecord>,
    entities: Vec<EntityRecord>,
    provenance: Vec<ProvenanceRecord>,
    occurrences: Vec<OccurrenceRecord>,
    envelope: ExtensionEnvelope,
}

impl Fixture {
    fn new() -> Self {
        let repository = RepositoryId::from_bytes([1; 16]);
        let generation = GenerationId::from_bytes([2; 20]);
        let file = FileId::from_bytes([3; 20]);
        let parser = FactId::from_bytes([4; 20]);
        let full_source = source(repository, generation, file, 0, SOURCE.len());
        let evidence = FactEvidence {
            source: Some(full_source.clone()),
            derivation: Vec::new(),
        };
        let mut fixture = Self {
            files: vec![FileRecord {
                id: file,
                repository,
                generation,
                path: "Module.bas".to_owned(),
                path_locator: None,
                content_hash: content_hash(SOURCE.as_bytes()),
                byte_length: u64::try_from(SOURCE.len()).unwrap(),
                language: "vba".to_owned(),
                encoding: "utf-8".to_owned(),
                generated: false,
                provenance: parser,
                evidence: evidence.clone(),
            }],
            entities: Vec::new(),
            provenance: vec![ProvenanceRecord {
                id: parser,
                repository,
                generation,
                producer_kind: ProducerKind::Parser,
                producer: ProducerIdentity::new("test-parser", "1", content_hash(b"config"))
                    .unwrap(),
                binary_digest: content_hash(b"parser"),
                frontend_version: None,
                language: "vba".to_owned(),
                tier: AnalysisTier::TierB,
                build_context: BuildContextIdentity::new(content_hash(b"context")),
                input_sources: vec![full_source.clone()],
                evidence_sources: Vec::new(),
                derivation_parents: Vec::new(),
                rule: None,
            }],
            occurrences: Vec::new(),
            envelope: new_vba_compilation_envelope(
                repository,
                generation,
                parser,
                full_source.clone(),
                VbaCompilation {
                    scope: full_source.span(),
                    complete: true,
                    branches: branches(file),
                },
            )
            .unwrap(),
        };
        for (index, name) in ["Module", "Always", "Selected", "Excluded", "Uncertain"]
            .into_iter()
            .enumerate()
        {
            let id = SymbolId::from_bytes([u8::try_from(10 + index).unwrap(); 20]);
            let range = if index == 0 {
                0..SOURCE.len()
            } else {
                let start = SOURCE.find(&format!("Public Sub {name}()")).unwrap();
                start..start + SOURCE[start..].find("End Sub").unwrap() + "End Sub".len()
            };
            let entity_source = source(repository, generation, file, range.start, range.end);
            fixture.entities.push(EntityRecord {
                id,
                repository,
                generation,
                kind: if index == 0 {
                    EntityKind::Module
                } else {
                    EntityKind::Function
                },
                language: "vba".to_owned(),
                tier: AnalysisTier::TierB,
                canonical_name: name.to_owned(),
                display_name: name.to_owned(),
                qualified_name: format!("Module.{name}"),
                container: Some(if index == 0 {
                    ContainerRef::File(file)
                } else {
                    ContainerRef::Entity(SymbolId::from_bytes([10; 20]))
                }),
                visibility: EntityVisibility::Public,
                flags: Vec::new(),
                provenance: parser,
                evidence: FactEvidence {
                    source: Some(entity_source),
                    derivation: Vec::new(),
                },
            });
            if index != 0 {
                fixture.occurrence(
                    name,
                    &format!("Public Sub {name}()"),
                    OccurrenceRole::Definition,
                    None,
                    OccurrenceTarget::Resolved { symbol: id },
                );
            }
        }
        for (name, phrase, owner) in [
            ("Selected", "value = Selected() ' active use", 11),
            ("Excluded", "value = Excluded() ' inactive use", 11),
            ("Selected", "value = Selected() ' unknown use", 14),
            (
                "Uncertain",
                "value = Uncertain() ' active unknown target",
                11,
            ),
        ] {
            fixture.occurrence(
                name,
                phrase,
                OccurrenceRole::Reference,
                Some(SymbolId::from_bytes([owner; 20])),
                OccurrenceTarget::Unresolved {
                    text_hash: content_hash(name.as_bytes()),
                },
            );
        }
        fixture
    }

    fn occurrence(
        &mut self,
        name: &str,
        phrase: &str,
        role: OccurrenceRole,
        enclosing: Option<SymbolId>,
        target: OccurrenceTarget,
    ) {
        let file = &self.files[0];
        let start = SOURCE.find(phrase).unwrap() + phrase.find(name).unwrap();
        let source = source(
            file.repository,
            file.generation,
            file.id,
            start,
            start + name.len(),
        );
        self.occurrences.push(OccurrenceRecord {
            id: FactId::from_bytes([u8::try_from(30 + self.occurrences.len()).unwrap(); 20]),
            repository: file.repository,
            generation: file.generation,
            file: file.id,
            source: source.clone(),
            role,
            enclosing,
            target,
            syntactic_text_hash: content_hash(name.as_bytes()),
            syntax_kind: if role == OccurrenceRole::Definition {
                "vba.name.definition"
            } else {
                "vba.invoked_identifier.reference"
            }
            .to_owned(),
            provenance: file.provenance,
            confidence: Confidence::new(1000).unwrap(),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        });
    }

    fn index(&self) -> CandidateIndex<'_> {
        CandidateIndex::build(
            &self.files,
            &self.entities,
            &self.provenance,
            &[],
            &[],
            &self.occurrences,
            &Cancellation::new(),
        )
        .unwrap()
    }

    fn import(&self, envelopes: &[ExtensionEnvelope]) -> CompilationIndex {
        CompilationIndex::build(
            &self.index(),
            envelopes,
            &self.occurrences,
            &BTreeMap::from([(self.files[0].id, SOURCE.as_bytes())]),
            &Cancellation::new(),
            &mut ResolutionWorkBudget::new(100_000),
        )
        .unwrap()
    }
}

fn source(
    repository: RepositoryId,
    generation: GenerationId,
    file: FileId,
    start: usize,
    end: usize,
) -> SourceRef {
    SourceRef::new(
        repository,
        generation,
        span(file, start, end),
        content_hash(SOURCE.as_bytes()),
        None,
    )
}

fn span(file: FileId, start: usize, end: usize) -> SourceSpan {
    SourceSpan::new(
        file,
        u64::try_from(start).unwrap(),
        u64::try_from(end).unwrap(),
    )
    .unwrap()
}

fn branches(file: FileId) -> Vec<VbaCompilationBranch> {
    let mut result = Vec::new();
    for (header, activity) in [
        ("#If True Then\n", VbaCompilationActivity::Active),
        ("#If HostChoice Then\n", VbaCompilationActivity::Unproven),
        ("#If False Then\n", VbaCompilationActivity::Inactive),
    ] {
        let start = SOURCE.find(header).unwrap();
        let end_header = start + header.len();
        let end_if = end_header + SOURCE[end_header..].find("#End If\n").unwrap();
        let group = span(file, start, end_if + "#End If\n".len());
        let else_start = SOURCE[end_header..end_if]
            .find("#Else\n")
            .map(|offset| end_header + offset);
        result.push(VbaCompilationBranch {
            group,
            region: span(file, end_header, else_start.unwrap_or(end_if)),
            activity,
        });
        if let Some(else_start) = else_start {
            result.push(VbaCompilationBranch {
                group,
                region: span(file, else_start + "#Else\n".len(), end_if),
                activity: VbaCompilationActivity::Inactive,
            });
        }
    }
    result
}

#[test]
fn exact_declarations_and_uses_are_independently_gated() {
    let fixture = Fixture::new();
    let compilation = fixture.import(std::slice::from_ref(&fixture.envelope));
    for index in [0, 1, 2] {
        assert!(compilation.entity_active(fixture.entities[index].id));
    }
    assert!(compilation.entity_inactive(fixture.entities[3].id));
    assert!(!compilation.entity_active(fixture.entities[4].id));
    assert!(!compilation.entity_inactive(fixture.entities[4].id));
    let uses = &fixture.occurrences[4..];
    assert!(compilation.use_active(uses[0].id));
    assert!(!compilation.use_active(uses[1].id));
    assert!(!compilation.use_active(uses[2].id));
    assert!(compilation.use_active(uses[3].id));
    assert_eq!(
        compilation
            .evidence(fixture.files[0].id)
            .collect::<Vec<_>>(),
        vec![FactRef::Fact(fixture.envelope.id)]
    );
    let mut classic = ClassicIndex {
        compilation,
        ..ClassicIndex::default()
    };
    let context = Arc::new(ProjectEvidence {
        manifest: fixture.files[0].id,
        native: true,
        sources: Vec::new(),
        facts: Vec::new(),
    });
    for (site, target, expected) in [
        (&uses[0], &fixture.entities[2], true),
        (&uses[1], &fixture.entities[2], false),
        (&uses[2], &fixture.entities[2], false),
        (&uses[3], &fixture.entities[4], false),
        (&uses[0], &fixture.entities[3], false),
    ] {
        classic.proofs.insert(
            site.id,
            Proof {
                target,
                context: Arc::clone(&context),
                receiver: None,
                rule: ResolutionRule::ClassicProjectMember,
                property_call: false,
            },
        );
        assert_eq!(classic.proof(site).is_some(), expected);
    }
}

#[test]
fn broad_procedure_context_uses_a_written_header_not_its_conditional_body() {
    let mut fixture = Fixture::new();
    fixture.occurrences.retain(|site| {
        !matches!(site.target,
        OccurrenceTarget::Resolved { symbol } if symbol == fixture.entities[1].id)
    });
    let compilation = fixture.import(std::slice::from_ref(&fixture.envelope));
    assert!(compilation.entity_active(fixture.entities[1].id));
    assert!(compilation.support_active(fixture.entities[1].evidence.source.as_ref().unwrap()));
    fixture.entities[1].canonical_name = "NotTheWrittenHeader".to_owned();
    assert!(
        !fixture
            .import(std::slice::from_ref(&fixture.envelope))
            .entity_active(fixture.entities[1].id)
    );
}

#[test]
fn missing_incomplete_foreign_forged_and_overlapping_inventories_fail_closed() {
    let fixture = Fixture::new();
    assert!(!fixture.import(&[]).entity_active(fixture.entities[2].id));
    let mut incomplete = decode_vba_compilation_envelope(&fixture.envelope).unwrap();
    incomplete.complete = false;
    let incomplete = new_vba_compilation_envelope(
        fixture.envelope.repository,
        fixture.envelope.generation,
        fixture.envelope.provenance,
        fixture.envelope.evidence.source.clone().unwrap(),
        incomplete,
    )
    .unwrap();
    assert!(
        !fixture
            .import(&[incomplete])
            .entity_active(fixture.entities[2].id)
    );
    let mut forged = fixture.envelope.clone();
    forged.payload.push(' ');
    assert!(
        !fixture
            .import(&[forged])
            .entity_active(fixture.entities[2].id)
    );
    let mut foreign = fixture.envelope.evidence.source.clone().unwrap();
    foreign = SourceRef::new(
        foreign.repository(),
        GenerationId::from_bytes([99; 20]),
        foreign.span(),
        foreign.content_hash(),
        None,
    );
    let foreign = new_vba_compilation_envelope(
        foreign.repository(),
        foreign.generation(),
        fixture.envelope.provenance,
        foreign,
        decode_vba_compilation_envelope(&fixture.envelope).unwrap(),
    )
    .unwrap();
    assert!(
        !fixture
            .import(&[foreign])
            .entity_active(fixture.entities[2].id)
    );
    assert!(
        !fixture
            .import(&[fixture.envelope.clone(), fixture.envelope.clone()])
            .entity_active(fixture.entities[2].id)
    );
    let mut parser = Fixture::new();
    parser.provenance[0].producer_kind = ProducerKind::Rule;
    assert!(
        !parser
            .import(std::slice::from_ref(&parser.envelope))
            .entity_active(parser.entities[2].id)
    );
    parser.provenance[0].producer_kind = ProducerKind::Parser;
    parser.provenance[0].input_sources.clear();
    assert!(
        !parser
            .import(std::slice::from_ref(&parser.envelope))
            .entity_active(parser.entities[2].id)
    );
}

#[test]
fn importer_preserves_exact_work_and_cancellation_errors() {
    let fixture = Fixture::new();
    let index = fixture.index();
    let sources = BTreeMap::from([(fixture.files[0].id, SOURCE.as_bytes())]);
    let cancellation = Cancellation::new();
    let mut measured = ResolutionWorkBudget::new(100_000);
    let run = |work: &mut ResolutionWorkBudget, cancel: &Cancellation| {
        CompilationIndex::build(
            &index,
            std::slice::from_ref(&fixture.envelope),
            &fixture.occurrences,
            &sources,
            cancel,
            work,
        )
    };
    assert!(run(&mut measured, &cancellation).is_ok());
    let consumed = measured.consumed();
    assert!(run(&mut ResolutionWorkBudget::new(consumed), &cancellation).is_ok());
    assert!(matches!(
        run(&mut ResolutionWorkBudget::new(consumed - 1), &cancellation),
        Err(ResolutionError::WorkLimit { .. })
    ));
    cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    let mut untouched = ResolutionWorkBudget::new(100_000);
    assert!(matches!(
        run(&mut untouched, &cancellation),
        Err(ResolutionError::Cancelled(_))
    ));
    assert_eq!(untouched.consumed(), 0);
}

#[test]
fn final_retention_charges_each_parameter_and_support_source() {
    let fixture = Fixture::new();
    let site = &fixture.occurrences[4];
    let other_source = &fixture.occurrences[7].source;
    let context = Arc::new(ProjectEvidence {
        manifest: fixture.files[0].id,
        native: true,
        sources: Vec::new(),
        facts: Vec::new(),
    });
    let build = || {
        let mut classic = ClassicIndex {
            compilation: fixture.import(std::slice::from_ref(&fixture.envelope)),
            ..ClassicIndex::default()
        };
        classic.proofs.insert(
            site.id,
            Proof {
                target: &fixture.entities[2],
                context: Arc::clone(&context),
                receiver: Some((&fixture.entities[1], site.source.clone())),
                rule: ResolutionRule::ClassicProjectMember,
                property_call: false,
            },
        );
        classic
            .application_parameters
            .insert(site.id, Arc::new(fixture.entities[..3].iter().collect()));
        classic.receiver_sources.insert(
            site.id,
            Arc::new(vec![site.source.clone(), other_source.clone()]),
        );
        classic
    };
    let mut classic = build();
    let expected = 1
        + 3
        + 2 * classic.compilation.source_cost(&site.source)
        + classic.compilation.source_cost(other_source);
    let mut exact = ResolutionWorkBudget::new(expected);
    classic
        .retain_eligible(&Cancellation::new(), &mut exact)
        .unwrap();
    assert_eq!(exact.consumed(), expected);
    assert!(classic.proof(site).is_some());
    assert_eq!(classic.application_parameters(site).len(), 3);
    assert_eq!(classic.receiver_sources(site).len(), 2);
    assert!(matches!(
        build().retain_eligible(
            &Cancellation::new(),
            &mut ResolutionWorkBudget::new(expected - 1)
        ),
        Err(ResolutionError::WorkLimit { maximum }) if maximum == expected - 1
    ));

    let cancellation = Cancellation::new();
    cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    let mut untouched = ResolutionWorkBudget::new(expected);
    assert!(matches!(
        build().retain_eligible(&cancellation, &mut untouched),
        Err(ResolutionError::Cancelled(_))
    ));
    assert_eq!(untouched.consumed(), 0);

    let mut rejected = build();
    rejected
        .application_parameters
        .insert(site.id, Arc::new(vec![&fixture.entities[4]]));
    rejected
        .retain_eligible(
            &Cancellation::new(),
            &mut ResolutionWorkBudget::new(100_000),
        )
        .unwrap();
    assert!(rejected.proof(site).is_none());
    assert!(rejected.application_parameters(site).is_empty());
    assert!(rejected.receiver_sources(site).is_empty());
}

#[test]
fn indexed_activity_matches_every_nonempty_span_in_admitted_nested_models() {
    use VbaCompilationActivity::{Active, Inactive, Unproven};

    let fixture = Fixture::new();
    let file = fixture.files[0].id;
    let choices = [
        (Active, Inactive),
        (Inactive, Active),
        (Inactive, Inactive),
        (Unproven, Unproven),
        (Unproven, Inactive),
    ];
    for (outer, other) in choices {
        for (inner, sibling) in choices {
            for complete in [true, false] {
                let model = VbaCompilation {
                    scope: span(file, 0, 100),
                    complete,
                    branches: vec![
                        VbaCompilationBranch {
                            group: span(file, 10, 80),
                            region: span(file, 15, 45),
                            activity: outer,
                        },
                        VbaCompilationBranch {
                            group: span(file, 10, 80),
                            region: span(file, 45, 75),
                            activity: other,
                        },
                        VbaCompilationBranch {
                            group: span(file, 20, 40),
                            region: span(file, 25, 30),
                            activity: inner,
                        },
                        VbaCompilationBranch {
                            group: span(file, 20, 40),
                            region: span(file, 30, 35),
                            activity: sibling,
                        },
                        VbaCompilationBranch {
                            group: span(file, 85, 99),
                            region: span(file, 90, 90),
                            activity: Active,
                        },
                    ],
                };
                let envelope = new_vba_compilation_envelope(
                    fixture.files[0].repository,
                    fixture.files[0].generation,
                    fixture.files[0].provenance,
                    source(
                        fixture.files[0].repository,
                        fixture.files[0].generation,
                        file,
                        0,
                        100,
                    ),
                    model,
                )
                .unwrap();
                let model = decode_vba_compilation_envelope(&envelope).unwrap();
                let compiled = ActivityIndex::build(
                    &model,
                    &Cancellation::new(),
                    &mut ResolutionWorkBudget::new(100_000),
                )
                .unwrap();
                for start in 0..=101 {
                    for end in start..=102 {
                        let written = span(file, start, end);
                        let mut actual_work = 0;
                        let actual = compiled
                            .activity(&model, written, &mut || {
                                actual_work += 1;
                                Ok::<(), ()>(())
                            })
                            .unwrap();
                        assert_eq!(actual, model.activity_at(written), "{model:?}: {written:?}");
                        assert!(actual_work <= compiled.maximum_work());
                    }
                }
                let foreign = span(FileId::from_bytes([99; 20]), 25, 26);
                assert_eq!(
                    compiled
                        .activity(&model, foreign, &mut || Ok::<(), ()>(()))
                        .unwrap(),
                    Unproven
                );
            }
        }
    }
}

#[test]
fn indexed_comparisons_preserve_exact_callback_errors() {
    let fixture = Fixture::new();
    let model = decode_vba_compilation_envelope(&fixture.envelope).unwrap();
    let compiled = ActivityIndex::build(
        &model,
        &Cancellation::new(),
        &mut ResolutionWorkBudget::new(100_000),
    )
    .unwrap();
    let query = fixture.entities[2].evidence.source.as_ref().unwrap().span();
    let mut comparisons = 0;
    assert_eq!(
        compiled
            .activity(&model, query, &mut || {
                comparisons += 1;
                Ok::<(), ()>(())
            })
            .unwrap(),
        VbaCompilationActivity::Active
    );
    assert!(comparisons > 1);
    for fail_at in 1..=comparisons {
        let mut calls = 0;
        let result = compiled.activity(&model, query, &mut || {
            calls += 1;
            if calls == fail_at {
                Err(("cancelled comparison", fail_at))
            } else {
                Ok(())
            }
        });
        assert_eq!(result, Err(("cancelled comparison", fail_at)));
        assert_eq!(calls, fail_at);
    }
}

#[test]
fn many_groups_and_uses_fit_bounded_indexed_work_without_losing_activity() {
    let mut fixture = Fixture::new();
    let file = fixture.files[0].id;
    let repository = fixture.files[0].repository;
    let generation = fixture.files[0].generation;
    let mut text = String::new();
    let mut branches = Vec::new();
    for index in 0..128 {
        let start = text.len();
        text.push_str("#If HostChoice Then\n");
        let first = text.len();
        text.push_str(&format!("Public Const First{index} = True\n"));
        let first_end = text.len();
        text.push_str("#Else\n");
        let second = text.len();
        text.push_str(&format!("Public Const Second{index} = False\n"));
        let second_end = text.len();
        text.push_str("#End If\n");
        let group = span(file, start, text.len());
        for region in [span(file, first, first_end), span(file, second, second_end)] {
            branches.push(VbaCompilationBranch {
                group,
                region,
                activity: VbaCompilationActivity::Unproven,
            });
        }
    }
    fixture.occurrences.clear();
    fixture.entities.clear();
    text.push_str("Public Sub Run()\n");
    let mut uses = Vec::new();
    for index in 0_u64..2048 {
        text.push_str("value = ");
        let start = text.len();
        text.push_str("Probe");
        uses.push((index, start..text.len()));
        text.push_str("()\n");
    }
    text.push_str("End Sub\n");
    let hash = content_hash(text.as_bytes());
    let full = SourceRef::new(
        repository,
        generation,
        span(file, 0, text.len()),
        hash,
        None,
    );
    fixture.files[0].byte_length = u64::try_from(text.len()).unwrap();
    fixture.files[0].content_hash = hash;
    fixture.files[0].evidence.source = Some(full.clone());
    fixture.provenance[0].input_sources = vec![full.clone()];
    for (index, range) in uses {
        let mut id = [0; 20];
        id[..8].copy_from_slice(&index.to_le_bytes());
        let source = SourceRef::new(
            repository,
            generation,
            span(file, range.start, range.end),
            hash,
            None,
        );
        fixture.occurrences.push(OccurrenceRecord {
            id: FactId::from_bytes(id),
            repository,
            generation,
            file,
            source: source.clone(),
            role: OccurrenceRole::Reference,
            enclosing: None,
            target: OccurrenceTarget::Unresolved {
                text_hash: content_hash(b"Probe"),
            },
            syntactic_text_hash: content_hash(b"Probe"),
            syntax_kind: "vba.invoked_identifier.reference".to_owned(),
            provenance: fixture.files[0].provenance,
            confidence: Confidence::new(1000).unwrap(),
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        });
    }
    let model = VbaCompilation {
        scope: full.span(),
        complete: true,
        branches,
    };
    let previous_scan_lower_bound = model.branches.len() * fixture.occurrences.len();
    let maximum = 100_000;
    assert!(previous_scan_lower_bound > maximum);
    fixture.envelope = new_vba_compilation_envelope(
        repository,
        generation,
        fixture.files[0].provenance,
        full,
        model,
    )
    .unwrap();
    let index = fixture.index();
    let inputs = BTreeMap::from([(file, text.as_bytes())]);
    let run = |work: &mut ResolutionWorkBudget, cancellation: &Cancellation| {
        CompilationIndex::build(
            &index,
            std::slice::from_ref(&fixture.envelope),
            &fixture.occurrences,
            &inputs,
            cancellation,
            work,
        )
    };
    let mut measured = ResolutionWorkBudget::new(maximum);
    let compilation = run(&mut measured, &Cancellation::new()).unwrap();
    assert!(measured.consumed() < previous_scan_lower_bound);
    for site in &fixture.occurrences {
        assert!(compilation.use_active(site.id));
        assert_eq!(
            compilation.activity(&site.source),
            VbaCompilationActivity::Active
        );
    }
    let exact = measured.consumed();
    assert!(run(&mut ResolutionWorkBudget::new(exact), &Cancellation::new()).is_ok());
    assert!(matches!(
        run(&mut ResolutionWorkBudget::new(exact - 1), &Cancellation::new()),
        Err(ResolutionError::WorkLimit { maximum }) if maximum == exact - 1
    ));
    let cancellation = Cancellation::new();
    cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    let mut untouched = ResolutionWorkBudget::new(maximum);
    assert!(matches!(
        run(&mut untouched, &cancellation),
        Err(ResolutionError::Cancelled(_))
    ));
    assert_eq!(untouched.consumed(), 0);
}

#[test]
fn verified_legacy_source_without_conditional_markers_remains_ordinary() {
    let mut fixture = Fixture::new();
    let bytes = b"Sub Plain(): End Sub\n' #If ignored in a comment\n";
    let file = &mut fixture.files[0];
    file.byte_length = u64::try_from(bytes.len()).unwrap();
    file.content_hash = content_hash(bytes);
    let full = SourceRef::new(
        file.repository,
        file.generation,
        span(file.id, 0, bytes.len()),
        file.content_hash,
        None,
    );
    file.evidence.source = Some(full.clone());
    fixture.provenance[0].input_sources = vec![full.clone()];
    fixture.entities.clear();
    fixture.occurrences.clear();
    let compilation = CompilationIndex::build(
        &fixture.index(),
        &[],
        &[],
        &BTreeMap::from([(fixture.files[0].id, bytes.as_slice())]),
        &Cancellation::new(),
        &mut ResolutionWorkBudget::new(1024),
    )
    .unwrap();
    let name = SourceRef::new(
        full.repository(),
        full.generation(),
        span(full.span().file(), 4, 9),
        full.content_hash(),
        None,
    );
    assert_eq!(compilation.activity(&name), VbaCompilationActivity::Active);
    let stale = SourceRef::new(
        name.repository(),
        name.generation(),
        name.span(),
        content_hash(b"stale"),
        None,
    );
    assert_eq!(
        compilation.activity(&stale),
        VbaCompilationActivity::Unproven
    );
}
