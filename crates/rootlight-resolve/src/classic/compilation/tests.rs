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

// The pre-optimization traversal is an independent oracle for both lookup
// results and the exact comparison removed by full-file ownership admission.
fn original_activity_checked<E>(
    index: &CompilationIndex,
    source: &SourceRef,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<VbaCompilationActivity, E> {
    check()?;
    let Some(file) = index.files.get(&source.span().file()) else {
        return Ok(VbaCompilationActivity::Unproven);
    };
    if file.blocked
        || !same_identity(&file.source, source)
        || !contains(file.source.span(), source.span())
    {
        return Ok(VbaCompilationActivity::Unproven);
    }
    for inventory in &file.inventories {
        check()?;
        if contains(inventory.model.scope, source.span()) {
            return inventory
                .activity
                .activity(&inventory.model, source.span(), check);
        }
    }
    if file.conditional {
        return Ok(VbaCompilationActivity::Unproven);
    }
    for inventory in &file.inventories {
        check()?;
        if overlaps(inventory.model.scope, source.span()) {
            return Ok(VbaCompilationActivity::Unproven);
        }
    }
    Ok(VbaCompilationActivity::Active)
}

fn original_occurrence_active_with_work(
    index: &CompilationIndex,
    occurrence: &OccurrenceRecord,
    cancellation: &Cancellation,
    work: &mut ResolutionWorkBudget,
) -> Result<bool, ResolutionError> {
    cancellation.check()?;
    Ok(
        original_activity_checked(index, &occurrence.source, &mut || {
            cancellation.check()?;
            work.consume()
        })? == VbaCompilationActivity::Active
            && occurrence
                .enclosing
                .is_none_or(|id| index.entity_active(id))
            && match occurrence.target {
                OccurrenceTarget::Resolved { symbol } => index.entity_active(symbol),
                _ => true,
            },
    )
}

fn compilation_envelope(fixture: &Fixture, model: VbaCompilation) -> ExtensionEnvelope {
    let file = &fixture.files[0];
    new_vba_compilation_envelope(
        file.repository,
        file.generation,
        file.provenance,
        SourceRef::new(
            file.repository,
            file.generation,
            model.scope,
            file.content_hash,
            None,
        ),
        model,
    )
    .unwrap()
}

#[test]
fn sole_full_file_lookup_preserves_original_ranges_and_identity_guards() {
    let fixture = Fixture::new();
    let file = &fixture.files[0];
    for complete in [true, false] {
        let mut model = decode_vba_compilation_envelope(&fixture.envelope).unwrap();
        model.complete = complete;
        let envelope = compilation_envelope(&fixture, model);
        let compilation = fixture.import(&[envelope]);
        for start in 0..=SOURCE.len() + 1 {
            for end in start..=SOURCE.len() + 1 {
                let query = source(file.repository, file.generation, file.id, start, end);
                let expected = original_activity_checked(&compilation, &query, &mut || {
                    Ok::<(), std::convert::Infallible>(())
                })
                .unwrap();
                assert_eq!(
                    compilation.activity(&query),
                    expected,
                    "{complete}: {start}..{end}"
                );
            }
        }
        let base = &fixture.occurrences[4].source;
        let hinted = SourceRef::new(
            base.repository(),
            base.generation(),
            base.span(),
            base.content_hash(),
            Some(rootlight_ir::LineRange::new(900, 901).unwrap()),
        );
        assert_eq!(compilation.activity(&hinted), compilation.activity(base));
        let foreign = [
            SourceRef::new(
                RepositoryId::from_bytes([99; 16]),
                base.generation(),
                base.span(),
                base.content_hash(),
                hinted.line_hint(),
            ),
            SourceRef::new(
                base.repository(),
                GenerationId::from_bytes([99; 20]),
                base.span(),
                base.content_hash(),
                None,
            ),
            SourceRef::new(
                base.repository(),
                base.generation(),
                base.span(),
                content_hash(b"different source"),
                None,
            ),
            SourceRef::new(
                base.repository(),
                base.generation(),
                span(FileId::from_bytes([99; 20]), 0, 1),
                base.content_hash(),
                None,
            ),
        ];
        for query in foreign {
            let mut work = ResolutionWorkBudget::new(1);
            assert_eq!(
                compilation
                    .activity_with_work(&query, &Cancellation::new(), &mut work)
                    .unwrap(),
                VbaCompilationActivity::Unproven,
            );
            assert_eq!(work.consumed(), 1);
        }
    }
    let blocked = fixture.import(&[fixture.envelope.clone(), fixture.envelope.clone()]);
    let query = &fixture.occurrences[4].source;
    let mut work = ResolutionWorkBudget::new(1);
    assert_eq!(
        blocked
            .activity_with_work(query, &Cancellation::new(), &mut work)
            .unwrap(),
        VbaCompilationActivity::Unproven,
    );
    assert_eq!(work.consumed(), 1);
}

#[test]
fn sole_full_file_lookup_charges_exact_work_and_preserves_interruption() {
    let fixture = Fixture::new();
    for complete in [true, false] {
        let mut model = decode_vba_compilation_envelope(&fixture.envelope).unwrap();
        model.complete = complete;
        let compilation = fixture.import(&[compilation_envelope(&fixture, model)]);
        let file = &compilation.files[&fixture.files[0].id];
        for site in &fixture.occurrences {
            let mut old_work = ResolutionWorkBudget::new(100);
            let expected =
                original_activity_checked(&compilation, &site.source, &mut || old_work.consume())
                    .unwrap();
            let mut measured = ResolutionWorkBudget::new(100);
            assert_eq!(
                compilation
                    .activity_with_work(&site.source, &Cancellation::new(), &mut measured)
                    .unwrap(),
                expected,
            );
            let exact = measured.consumed();
            assert_eq!(old_work.consumed(), exact + 1);
            assert_eq!(
                compilation.source_cost(&site.source),
                file.maximum_activity_work + 1
            );
            assert!(exact <= compilation.source_cost(&site.source));
            let mut exact_work = ResolutionWorkBudget::new(exact);
            assert_eq!(
                compilation
                    .activity_with_work(&site.source, &Cancellation::new(), &mut exact_work)
                    .unwrap(),
                expected,
            );
            assert_eq!(exact_work.consumed(), exact);
            for maximum in [0, exact - 1] {
                let mut short = ResolutionWorkBudget::new(maximum);
                assert!(matches!(
                    compilation.activity_with_work(&site.source, &Cancellation::new(), &mut short),
                    Err(ResolutionError::WorkLimit { maximum: actual }) if actual == maximum
                ));
                assert_eq!(short.consumed(), maximum);
            }
            for stop_at in 1..=exact {
                let mut calls = 0;
                assert_eq!(
                    compilation.activity_checked(&site.source, &mut || {
                        calls += 1;
                        if calls == stop_at {
                            Err(stop_at)
                        } else {
                            Ok(())
                        }
                    }),
                    Err(stop_at),
                );
                assert_eq!(calls, stop_at);
            }
            let cancellation = Cancellation::new();
            cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
            let mut untouched = ResolutionWorkBudget::new(exact);
            assert!(matches!(
                compilation.activity_with_work(&site.source, &cancellation, &mut untouched),
                Err(ResolutionError::Cancelled(_))
            ));
            assert_eq!(untouched.consumed(), 0);
        }
    }
}

#[test]
fn partial_and_multiple_inventories_keep_original_lookup_work() {
    let fixture = Fixture::new();
    let file = &fixture.files[0];
    let full_model = decode_vba_compilation_envelope(&fixture.envelope).unwrap();
    let split = SOURCE.find("#If HostChoice Then").unwrap();
    let models =
        [span(file.id, 0, split), span(file.id, split, SOURCE.len())].map(|scope| VbaCompilation {
            scope,
            complete: true,
            branches: full_model
                .branches
                .iter()
                .filter(|branch| contains(scope, branch.group))
                .cloned()
                .collect(),
        });
    let envelopes = models.map(|model| compilation_envelope(&fixture, model));
    for inputs in [&envelopes[..1], envelopes.as_slice()] {
        let compilation = fixture.import(inputs);
        let inventory = &compilation.files[&file.id];
        assert!(!inventory.blocked);
        assert_eq!(inventory.inventories.len(), inputs.len());
        for (start, end) in [
            (0, 1),
            (split - 1, split),
            (split, split + 1),
            (split - 1, split + 1),
            (0, SOURCE.len()),
            (SOURCE.len(), SOURCE.len()),
        ] {
            let query = source(file.repository, file.generation, file.id, start, end);
            let mut original = ResolutionWorkBudget::new(100);
            let expected =
                original_activity_checked(&compilation, &query, &mut || original.consume())
                    .unwrap();
            let mut measured = ResolutionWorkBudget::new(100);
            assert_eq!(
                compilation
                    .activity_with_work(&query, &Cancellation::new(), &mut measured)
                    .unwrap(),
                expected,
            );
            assert_eq!(measured.consumed(), original.consumed());
            assert_eq!(
                compilation.source_cost(&query),
                inventory.maximum_activity_work + 2 * inputs.len() + 1
            );
            assert!(measured.consumed() <= compilation.source_cost(&query));
        }
    }
    let missing = fixture.import(&[]);
    let query = &fixture.occurrences[4].source;
    let mut work = ResolutionWorkBudget::new(1);
    assert_eq!(
        missing
            .activity_with_work(query, &Cancellation::new(), &mut work)
            .unwrap(),
        VbaCompilationActivity::Unproven
    );
    assert_eq!(work.consumed(), 1);
    assert_eq!(missing.source_cost(query), 1);
}

#[test]
fn immutable_owner_and_target_prechecks_preserve_all_use_choices() {
    let fixture = Fixture::new();
    let compilation = fixture.import(std::slice::from_ref(&fixture.envelope));
    let active = fixture.entities[2].id;
    let inactive = fixture.entities[3].id;
    let unknown = fixture.entities[4].id;
    let missing = SymbolId::from_bytes([99; 20]);
    let targets = [
        OccurrenceTarget::Unresolved {
            text_hash: content_hash(b"Selected"),
        },
        OccurrenceTarget::Candidates {
            symbols: vec![inactive, unknown, missing],
            total_count: 3,
            completeness: rootlight_ir::CoverageStatus::Complete,
        },
        OccurrenceTarget::Resolved { symbol: active },
        OccurrenceTarget::Resolved { symbol: inactive },
        OccurrenceTarget::Resolved { symbol: unknown },
        OccurrenceTarget::Resolved { symbol: missing },
    ];
    for enclosing in [
        None,
        Some(active),
        Some(inactive),
        Some(unknown),
        Some(missing),
    ] {
        for target in &targets {
            for source_site in &fixture.occurrences[4..] {
                let mut site = source_site.clone();
                site.enclosing = enclosing;
                site.target = target.clone();
                let mut original = ResolutionWorkBudget::new(100);
                let expected = original_occurrence_active_with_work(
                    &compilation,
                    &site,
                    &Cancellation::new(),
                    &mut original,
                )
                .unwrap();
                let rejected = enclosing.is_some_and(|id| !compilation.entity_active(id))
                    || matches!(target, OccurrenceTarget::Resolved { symbol } if !compilation.entity_active(*symbol));
                let mut measured = ResolutionWorkBudget::new(100);
                assert_eq!(
                    compilation
                        .occurrence_active_with_work(&site, &Cancellation::new(), &mut measured)
                        .unwrap(),
                    expected
                );
                let exact = measured.consumed();
                if rejected {
                    assert!(!expected);
                    assert_eq!(exact, 1);
                    assert!(original.consumed() > exact);
                } else {
                    let mut source_work = ResolutionWorkBudget::new(100);
                    compilation
                        .activity_with_work(&site.source, &Cancellation::new(), &mut source_work)
                        .unwrap();
                    assert_eq!(exact, source_work.consumed());
                }
                let mut exact_work = ResolutionWorkBudget::new(exact);
                assert_eq!(
                    compilation
                        .occurrence_active_with_work(&site, &Cancellation::new(), &mut exact_work)
                        .unwrap(),
                    expected
                );
                assert_eq!(exact_work.consumed(), exact);
                for maximum in [0, exact - 1] {
                    assert!(
                        matches!(compilation.occurrence_active_with_work(&site, &Cancellation::new(), &mut ResolutionWorkBudget::new(maximum)), Err(ResolutionError::WorkLimit { maximum: actual }) if actual == maximum)
                    );
                }
                let cancelled = Cancellation::new();
                cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
                let mut untouched = ResolutionWorkBudget::new(exact);
                assert!(matches!(
                    compilation.occurrence_active_with_work(&site, &cancelled, &mut untouched),
                    Err(ResolutionError::Cancelled(_))
                ));
                assert_eq!(untouched.consumed(), 0);
            }
        }
    }
    let mut candidates = fixture.occurrences[4].clone();
    candidates.enclosing = None;
    candidates.target = targets[1].clone();
    assert!(
        compilation
            .occurrence_active_with_work(
                &candidates,
                &Cancellation::new(),
                &mut ResolutionWorkBudget::new(100)
            )
            .unwrap()
    );
}

#[test]
fn prechecked_use_import_preserves_definitions_and_exact_active_multiset() {
    let mut fixture = Fixture::new();
    let active = fixture.entities[2].id;
    let inactive = fixture.entities[3].id;
    let unknown = fixture.entities[4].id;
    for enclosing in [None, Some(active), Some(inactive), Some(unknown)] {
        for target in [
            OccurrenceTarget::Unresolved {
                text_hash: content_hash(b"Selected"),
            },
            OccurrenceTarget::Candidates {
                symbols: vec![inactive, unknown],
                total_count: 2,
                completeness: rootlight_ir::CoverageStatus::Complete,
            },
            OccurrenceTarget::Resolved { symbol: active },
            OccurrenceTarget::Resolved { symbol: inactive },
            OccurrenceTarget::Resolved { symbol: unknown },
        ] {
            fixture.occurrence(
                "Selected",
                "value = Selected() ' active use",
                OccurrenceRole::Reference,
                enclosing,
                target,
            );
        }
    }
    let index = fixture.index();
    let sources = BTreeMap::from([(fixture.files[0].id, SOURCE.as_bytes())]);
    let run = |work: &mut ResolutionWorkBudget| {
        CompilationIndex::build(
            &index,
            std::slice::from_ref(&fixture.envelope),
            &fixture.occurrences,
            &sources,
            &Cancellation::new(),
            work,
        )
    };
    let mut measured = ResolutionWorkBudget::new(100_000);
    let compilation = run(&mut measured).unwrap();
    let mut expected = BTreeSet::new();
    for site in &fixture.occurrences {
        if original_occurrence_active_with_work(
            &compilation,
            site,
            &Cancellation::new(),
            &mut ResolutionWorkBudget::new(100),
        )
        .unwrap()
        {
            expected.insert(site.id);
        }
    }
    assert_eq!(compilation.uses, expected);
    assert_eq!(compilation.declarations.len(), fixture.entities.len());
    assert!(compilation.entity_active(active));
    assert!(compilation.entity_inactive(inactive));
    assert!(!compilation.entity_active(unknown));
    assert!(!compilation.entity_inactive(unknown));
    assert_eq!(
        fixture
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Definition)
            .count(),
        4
    );
    let exact = measured.consumed();
    assert_eq!(
        run(&mut ResolutionWorkBudget::new(exact)).unwrap().uses,
        expected
    );
    assert!(
        matches!(run(&mut ResolutionWorkBudget::new(exact - 1)), Err(ResolutionError::WorkLimit { maximum }) if maximum == exact - 1)
    );
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
fn declared_support_retention_charges_only_verified_definition_or_header_lookup() {
    for header_only in [false, true] {
        let mut fixture = Fixture::new();
        if header_only {
            fixture.occurrences.retain(|site| {
                !(site.role == OccurrenceRole::Definition
                    && site.target
                        == (OccurrenceTarget::Resolved {
                            symbol: fixture.entities[1].id,
                        }))
            });
        }
        let compilation = fixture.import(std::slice::from_ref(&fixture.envelope));
        let declaration = fixture.entities[1].evidence.source.as_ref().unwrap();
        let module = fixture.entities[0].evidence.source.as_ref().unwrap();
        assert_eq!(
            compilation.activity(declaration),
            VbaCompilationActivity::Unproven
        );
        assert!(compilation.entity_active(fixture.entities[1].id));
        assert!(compilation.support_active(declaration));
        assert!(compilation.support_active(module));
        assert_eq!(compilation.support_cost(declaration), 1);
        assert_eq!(compilation.support_cost(module), 1);
        let raw_reserved = compilation.source_cost(declaration);
        let mut raw_work = ResolutionWorkBudget::new(raw_reserved);
        assert_eq!(
            compilation
                .activity_with_work(declaration, &Cancellation::new(), &mut raw_work)
                .unwrap(),
            VbaCompilationActivity::Unproven,
        );
        assert!(raw_work.consumed() > compilation.support_cost(declaration));
        assert!(raw_work.consumed() <= raw_reserved);
        let site = fixture
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Reference
                    && site.syntactic_text_hash == content_hash(b"Selected")
                    && compilation.use_active(site.id)
            })
            .unwrap();
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
                    receiver: Some((&fixture.entities[1], declaration.clone())),
                    rule: ResolutionRule::ClassicProjectMember,
                    property_call: false,
                },
            );
            classic
                .receiver_sources
                .insert(site.id, Arc::new(vec![module.clone(), declaration.clone()]));
            classic
        };
        let mut classic = build();
        let mut exact = ResolutionWorkBudget::new(4);
        classic
            .retain_eligible(&Cancellation::new(), &mut exact)
            .unwrap();
        assert_eq!(exact.consumed(), 4);
        assert!(classic.proof(site).is_some());
        assert_eq!(classic.receiver_sources(site).len(), 2);
        for maximum in [0, 3] {
            let mut short = build();
            assert!(matches!(
                short.retain_eligible(
                    &Cancellation::new(),
                    &mut ResolutionWorkBudget::new(maximum),
                ),
                Err(ResolutionError::WorkLimit { maximum: actual }) if actual == maximum
            ));
            assert!(short.proof(site).is_some());
            assert_eq!(short.receiver_sources(site).len(), 2);
        }
        let cancelled = Cancellation::new();
        cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut untouched = ResolutionWorkBudget::new(4);
        assert!(matches!(
            build().retain_eligible(&cancelled, &mut untouched),
            Err(ResolutionError::Cancelled(_))
        ));
        assert_eq!(untouched.consumed(), 0);
    }
}

#[test]
fn declared_support_cost_preserves_exact_identity_and_unknown_barriers() {
    let fixture = Fixture::new();
    let compilation = fixture.import(std::slice::from_ref(&fixture.envelope));
    let declaration = fixture.entities[1].evidence.source.as_ref().unwrap();
    let altered_span = SourceSpan::new(
        declaration.span().file(),
        declaration.span().start_byte() + 1,
        declaration.span().end_byte(),
    )
    .unwrap();
    let nonhits = [
        SourceRef::new(
            declaration.repository(),
            GenerationId::from_bytes([99; 20]),
            declaration.span(),
            declaration.content_hash(),
            None,
        ),
        SourceRef::new(
            declaration.repository(),
            declaration.generation(),
            declaration.span(),
            content_hash(b"different retained source"),
            None,
        ),
        SourceRef::new(
            declaration.repository(),
            declaration.generation(),
            altered_span,
            declaration.content_hash(),
            None,
        ),
        fixture.entities[3].evidence.source.clone().unwrap(),
        fixture.entities[4].evidence.source.clone().unwrap(),
    ];
    for source in nonhits {
        assert!(!compilation.declaration_sources.contains(&source));
        assert!(!compilation.support_active(&source));
        let reserved = compilation.support_cost(&source);
        assert!(reserved > 1);
        let mut measured = ResolutionWorkBudget::new(reserved);
        let activity = compilation
            .activity_with_work(&source, &Cancellation::new(), &mut measured)
            .unwrap();
        assert_ne!(activity, VbaCompilationActivity::Active);
        assert!(measured.consumed() <= reserved);
    }
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
    for closed in [fixture.import(&[]), fixture.import(&[incomplete])] {
        assert!(!closed.declaration_sources.contains(declaration));
        assert!(!closed.entity_active(fixture.entities[1].id));
        assert!(!closed.support_active(declaration));
    }
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
        + 2 * classic.compilation.support_cost(&site.source)
        + classic.compilation.support_cost(other_source);
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
fn collapsed_groups_preserve_every_admitted_nested_and_empty_range() {
    use VbaCompilationActivity::{Active, Inactive, Unproven};

    let fixture = Fixture::new();
    let file = fixture.files[0].id;
    let choices = [
        (Active, Inactive),
        (Inactive, Active),
        (Inactive, Inactive),
        (Unproven, Unproven),
        (Unproven, Inactive),
        (Inactive, Unproven),
    ];
    for (outer, other) in choices {
        for (inner, sibling) in choices {
            for empty in [Active, Inactive, Unproven] {
                for complete in [true, false] {
                    let model = VbaCompilation {
                        scope: span(file, 0, 64),
                        complete,
                        branches: vec![
                            VbaCompilationBranch {
                                group: span(file, 8, 48),
                                region: span(file, 12, 28),
                                activity: outer,
                            },
                            VbaCompilationBranch {
                                group: span(file, 8, 48),
                                region: span(file, 28, 44),
                                activity: other,
                            },
                            VbaCompilationBranch {
                                group: span(file, 16, 24),
                                region: span(file, 18, 18),
                                activity: inner,
                            },
                            VbaCompilationBranch {
                                group: span(file, 16, 24),
                                region: span(file, 18, 22),
                                activity: sibling,
                            },
                            VbaCompilationBranch {
                                group: span(file, 52, 62),
                                region: span(file, 56, 56),
                                activity: empty,
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
                            64,
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
                    for start in 0..=65 {
                        for end in start..=66 {
                            let written = span(file, start, end);
                            let mut calls = 0;
                            let actual = compiled
                                .activity(&model, written, &mut || {
                                    calls += 1;
                                    Ok::<(), ()>(())
                                })
                                .unwrap();
                            assert_eq!(
                                actual,
                                model.activity_at(written),
                                "{model:?}: {written:?}"
                            );
                            assert!(calls <= compiled.maximum_work());
                        }
                    }
                    let foreign = span(FileId::from_bytes([99; 20]), 18, 19);
                    assert_eq!(
                        compiled.activity(&model, foreign, &mut || Err("must not look up")),
                        Ok(Unproven),
                    );
                }
            }
        }
    }
    for complete in [true, false] {
        let model = VbaCompilation {
            scope: span(file, 0, 64),
            complete,
            branches: Vec::new(),
        };
        let compiled = ActivityIndex::build(
            &model,
            &Cancellation::new(),
            &mut ResolutionWorkBudget::new(0),
        )
        .unwrap();
        assert_eq!(compiled.maximum_work(), 0);
        for start in 0..=65 {
            for end in start..=66 {
                let written = span(file, start, end);
                assert_eq!(
                    compiled.activity(&model, written, &mut || Err("must not look up")),
                    Ok(model.activity_at(written)),
                );
            }
        }
    }
}

#[test]
fn collapsed_unknown_groups_fit_exact_indexed_work_and_preserve_errors() {
    let fixture = Fixture::new();
    let file = fixture.files[0].id;
    let count = 128;
    let mut text = String::from("Public Sub Run()\n");
    let mut branches = Vec::new();
    let mut queries = Vec::new();
    for _ in 0..count {
        let start = text.len();
        text.push_str("#If HostChoice Then\n");
        let first = text.len();
        text.push_str("First()\n");
        let first_end = text.len();
        text.push_str("#Else\n");
        let second = text.len();
        text.push_str("Second()\n");
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
        queries.push(span(file, first, first + "First".len()));
    }
    text.push_str("End Sub\n");
    let full = SourceRef::new(
        fixture.files[0].repository,
        fixture.files[0].generation,
        span(file, 0, text.len()),
        content_hash(text.as_bytes()),
        None,
    );
    let envelope = new_vba_compilation_envelope(
        full.repository(),
        full.generation(),
        fixture.files[0].provenance,
        full.clone(),
        VbaCompilation {
            scope: full.span(),
            complete: true,
            branches,
        },
    )
    .unwrap();
    let model = decode_vba_compilation_envelope(&envelope).unwrap();
    let mut measured = ResolutionWorkBudget::new(100_000);
    let compiled = ActivityIndex::build(&model, &Cancellation::new(), &mut measured).unwrap();
    assert_eq!(measured.consumed(), count * 5);
    assert_eq!(compiled.maximum_work(), 9);
    assert!(
        ActivityIndex::build(
            &model,
            &Cancellation::new(),
            &mut ResolutionWorkBudget::new(count * 5),
        )
        .is_ok()
    );
    assert!(matches!(
        ActivityIndex::build(
            &model,
            &Cancellation::new(),
            &mut ResolutionWorkBudget::new(count * 5 - 1),
        ),
        Err(ResolutionError::WorkLimit { maximum }) if maximum == count * 5 - 1
    ));
    for &query in &queries {
        let mut exact = ResolutionWorkBudget::new(9);
        let actual = compiled
            .activity(&model, query, &mut || exact.consume())
            .unwrap();
        assert_eq!(actual, VbaCompilationActivity::Unproven);
        assert_eq!(actual, model.activity_at(query));
        assert!(exact.consumed() <= 9);
    }
    for maximum in [0, 8] {
        let mut short = ResolutionWorkBudget::new(maximum);
        assert!(matches!(
            compiled.activity(&model, queries[0], &mut || short.consume()),
            Err(ResolutionError::WorkLimit { maximum: actual }) if actual == maximum
        ));
    }
    for stop_at in 1..=9 {
        let mut calls = 0;
        assert_eq!(
            compiled.activity(&model, queries[0], &mut || {
                calls += 1;
                if calls == stop_at {
                    Err(("cancelled comparison", stop_at))
                } else {
                    Ok(())
                }
            }),
            Err(("cancelled comparison", stop_at)),
        );
        assert_eq!(calls, stop_at);
    }
    let cancelled = Cancellation::new();
    cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    let mut untouched = ResolutionWorkBudget::new(count * 5);
    assert!(matches!(
        ActivityIndex::build(&model, &cancelled, &mut untouched),
        Err(ResolutionError::Cancelled(_))
    ));
    assert_eq!(untouched.consumed(), 0);
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
