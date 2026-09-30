//! Authored conditional inventories through the production VBA parser.
//! Exact source ranges and canonical envelopes pin down producer decisions;
//! parser facts remain unchanged even when semantic promotion is blocked.

use std::{
    fs,
    path::Path,
    time::{Duration, Instant},
};

use rootlight_adapter_sdk::{
    AnalysisLimits, BatchThresholds, EncodingId, GenerationBoundSnapshot, LanguageId,
    MemoryAdmissionPolicy, ParseOutput, ParseRequest, StreamLimits, execute_parse,
};
use rootlight_cancel::CancellationReason;
use rootlight_ids::{GenerationId, RepositoryId, content_hash, derive_fact};
use rootlight_ir::{
    FactRef, IrLimits, SourceRef, SourceSpan, VbaCompilationBranch,
    decode_vba_compilation_envelope, new_vba_compilation_envelope,
};
use rootlight_vfs::{RelativePath, RepositoryRoot};

use super::*;
use crate::{ParserSettings, RuntimeConfig, TreeSitterProvider};

const SOURCE_BYTES: usize = 256 * 1024;
const SYNTAX_NODES: usize = 50_000;

struct Fixture {
    text: String,
    source: SourceRef,
    output: ParseOutput,
}

impl Fixture {
    fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let directory = tempfile::tempdir().unwrap();
        let relative = RelativePath::parse(Path::new("Conditions.bas")).unwrap();
        fs::write(directory.path().join("Conditions.bas"), &text).unwrap();
        let repository = RepositoryId::from_bytes([1; 16]);
        let root = RepositoryRoot::open(repository, directory.path()).unwrap();
        let snapshot = root
            .snapshot(&relative, u64::try_from(SOURCE_BYTES).unwrap())
            .unwrap();
        let source = SourceRef::new(
            repository,
            GenerationId::from_bytes([2; 20]),
            SourceSpan::new(snapshot.file(), 0, u64::try_from(text.len()).unwrap()).unwrap(),
            snapshot.content_hash(),
            None,
        );
        assert_eq!(source.content_hash(), content_hash(text.as_bytes()));
        let stream = StreamLimits::new(
            1_024,
            100_000,
            16 * 1024 * 1024,
            128,
            128 * 1024,
            4 * 1024 * 1024,
            BatchThresholds::new(128, 256 * 1024, 16, 16 * 1024).unwrap(),
        )
        .unwrap();
        let limits = AnalysisLimits::new(
            SOURCE_BYTES,
            SYNTAX_NODES,
            128,
            16,
            32 * 1024 * 1024,
            stream.clone(),
            stream,
            IrLimits::default(),
        )
        .unwrap();
        let provider = TreeSitterProvider::new(
            RuntimeConfig::new(
                SOURCE_BYTES,
                SYNTAX_NODES,
                128,
                16,
                64,
                1,
                1024 * 1024,
                ParserSettings::new(1024).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let request = ParseRequest::new(
            GenerationBoundSnapshot::new(&snapshot, &source).unwrap(),
            LanguageId::new("vba").unwrap(),
            EncodingId::utf8(),
            Vec::new(),
            &limits,
        )
        .unwrap();
        let output = execute_parse(
            &provider,
            &request,
            MemoryAdmissionPolicy::AllowUnavailableEnforcementFallback,
            &Cancellation::with_deadline(
                Instant::now().checked_add(Duration::from_secs(60)).unwrap(),
            ),
        )
        .unwrap();
        for fact in output.facts() {
            assert_eq!(fact.span().file(), source.span().file());
            assert!(fact.span().end_byte() <= source.span().end_byte());
        }
        Self {
            text,
            source,
            output,
        }
    }

    fn bindings(&self) -> VbaBindings<'_> {
        let original = self.output.facts().to_vec();
        let symbols = self
            .output
            .facts()
            .iter()
            .filter(|fact| {
                fact.kind() == SyntaxFactKind::Occurrence
                    && fact.syntax_kind().as_str().ends_with(".definition")
            })
            .map(|fact| {
                let mut identity = [0; 20];
                identity[..8].copy_from_slice(&fact.local_id().to_be_bytes());
                (fact.local_id(), SymbolId::from_bytes(identity))
            })
            .collect();
        let bindings = VbaBindings::new(
            self.output.facts(),
            self.text.as_bytes(),
            &symbols,
            1_024,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(self.output.facts(), original);
        bindings
    }

    fn span(&self, needle: &str) -> SourceSpan {
        self.span_at(self.text.find(needle).unwrap(), needle.len())
    }

    fn span_at(&self, start: usize, length: usize) -> SourceSpan {
        SourceSpan::new(
            self.source.span().file(),
            u64::try_from(start).unwrap(),
            u64::try_from(start + length).unwrap(),
        )
        .unwrap()
    }

    fn text_at(&self, fact: &SyntaxFact) -> &str {
        &self.text[usize::try_from(fact.span().start_byte()).unwrap()
            ..usize::try_from(fact.span().end_byte()).unwrap()]
    }

    fn model(&self, bindings: &VbaBindings<'_>) -> VbaCompilation {
        let models: Vec<_> = bindings.compilations().collect();
        assert_eq!(models.len(), 1);
        let model = models[0].clone();
        assert_eq!(model.scope, self.source.span());
        let envelope = new_vba_compilation_envelope(
            self.source.repository(),
            self.source.generation(),
            derive_fact("rootlight.test.vba-compilation", b"parser").id(),
            self.source.clone(),
            model.clone(),
        )
        .unwrap();
        assert_eq!(decode_vba_compilation_envelope(&envelope).unwrap(), model);
        assert_eq!(envelope.evidence.source.as_ref(), Some(&self.source));
        assert_eq!(
            envelope.evidence.derivation,
            [FactRef::File(model.scope.file())]
        );
        model
    }
}

fn three_arms(first: &str, second: &str) -> String {
    format!(
        "#If {first} Then\nSub First()\nEnd Sub\n#ElseIf {second} Then\nSub Second()\nEnd Sub\n#Else\nSub Last()\nEnd Sub\n#End If\n"
    )
}

#[test]
fn closed_and_unknown_groups_keep_exact_authored_branches_and_directives() {
    use VbaCompilationActivity::{Active, Inactive, Unproven};

    for (first, second, selected) in [
        ("True", "False", Some(0)),
        ("False", "True", Some(1)),
        ("False", "False", Some(2)),
        ("HostChoice", "True", None),
    ] {
        let fixture = Fixture::new(three_arms(first, second));
        assert_eq!(
            fixture.output.report().coverage().status(),
            rootlight_ir::CoverageStatus::Complete
        );
        let bindings = fixture.bindings();
        let model = fixture.model(&bindings);
        assert!(model.complete);
        let group = fixture.span_at(0, fixture.text.find("#End If").unwrap() + 7);
        let expected: Vec<_> =
            [
                "Sub First()\nEnd Sub\n",
                "Sub Second()\nEnd Sub\n",
                "Sub Last()\nEnd Sub\n",
            ]
            .into_iter()
            .enumerate()
            .map(|(index, body)| VbaCompilationBranch {
                group,
                region: fixture.span(body),
                activity: selected.map_or(Unproven, |selected| {
                    if selected == index { Active } else { Inactive }
                }),
            })
            .collect();
        assert_eq!(model.branches, expected);
        for (index, name) in ["First", "Second", "Last"].into_iter().enumerate() {
            assert_eq!(
                model.activity_at(fixture.span(name)),
                expected[index].activity
            );
        }
        for directive in ["#If", "#ElseIf", "#Else\n", "#End If"] {
            assert_eq!(model.activity_at(fixture.span(directive)), Unproven);
        }
        let group_fact = fixture
            .output
            .facts()
            .iter()
            .find(|fact| {
                fact.syntax_kind().as_str() == "vba.preprocessor.scope" && fact.span() == group
            })
            .unwrap();
        assert_eq!(
            bindings
                .compilation_evaluation_proven(group_fact, &Cancellation::new())
                .unwrap(),
            selected.is_some()
        );
    }
}

#[test]
fn nested_local_choices_obey_enclosing_activity_without_losing_ranges() {
    use VbaCompilationActivity::{Active, Inactive, Unproven};

    for (outer, effective) in [
        ("True", Active),
        ("False", Inactive),
        ("HostChoice", Unproven),
    ] {
        let fixture = Fixture::new(format!(
            "#If {outer} Then\nSub Before()\nEnd Sub\n#If True Then\nSub Inner()\nEnd Sub\n#Else\nSub Excluded()\nEnd Sub\n#End If\n#Else\nSub Alternative()\nEnd Sub\n#End If\n"
        ));
        let bindings = fixture.bindings();
        let model = fixture.model(&bindings);
        assert!(model.complete);
        assert_eq!(model.branches.len(), 4);
        let outer_group = fixture.span_at(0, fixture.text.rfind("#End If").unwrap() + 7);
        let inner_group = fixture
            .span("#If True Then\nSub Inner()\nEnd Sub\n#Else\nSub Excluded()\nEnd Sub\n#End If");
        let outer_choice = match effective {
            Active => [Active, Inactive],
            Inactive => [Inactive, Active],
            Unproven => [Unproven, Unproven],
        };
        assert_eq!(model.branches, [
            VbaCompilationBranch { group: outer_group, region: fixture.span("Sub Before()\nEnd Sub\n#If True Then\nSub Inner()\nEnd Sub\n#Else\nSub Excluded()\nEnd Sub\n#End If\n"), activity: outer_choice[0] },
            VbaCompilationBranch { group: outer_group, region: fixture.span("Sub Alternative()\nEnd Sub\n"), activity: outer_choice[1] },
            VbaCompilationBranch { group: inner_group, region: fixture.span("Sub Inner()\nEnd Sub\n"), activity: Active },
            VbaCompilationBranch { group: inner_group, region: fixture.span("Sub Excluded()\nEnd Sub\n"), activity: Inactive },
        ]);
        assert_eq!(model.activity_at(fixture.span("Inner")), effective);
        assert_eq!(model.activity_at(fixture.span("Excluded")), Inactive);
        assert_eq!(
            model.activity_at(fixture.span("#If True")),
            if effective == Inactive {
                Inactive
            } else {
                Unproven
            }
        );
    }
}

#[test]
fn empty_alternatives_have_exact_nonselectable_body_points() {
    use VbaCompilationActivity::{Active, Inactive, Unproven};

    for (text, boundaries, activities) in [
        ("#If True Then\n#End If\n", vec!["#End If"], vec![Active]),
        (
            "#If False Then\n#ElseIf True Then\n#Else\n#End If\n",
            vec!["#ElseIf", "#Else\n", "#End If"],
            vec![Inactive, Active, Inactive],
        ),
        (
            "#If True _\n Or False Then\n#End If\n",
            vec!["#End If"],
            vec![Active],
        ),
        (
            "#If True Then _\n\n#End If\n",
            vec!["#End If"],
            vec![Active],
        ),
        (
            "#If True Then _\r\n\r\n#End If\r\n",
            vec!["#End If"],
            vec![Active],
        ),
        (
            "#If False Then\n#Else _\n\n#End If\n",
            vec!["#Else", "#End If"],
            vec![Inactive, Active],
        ),
        (
            "#If True Then ' continuation marker _\n#End If\n",
            vec!["#End If"],
            vec![Active],
        ),
        (
            "#If False Then\n#Else ' continuation marker _\n#End If\n",
            vec!["#Else", "#End If"],
            vec![Inactive, Active],
        ),
    ] {
        let fixture = Fixture::new(text);
        assert_eq!(
            fixture.output.report().coverage().status(),
            rootlight_ir::CoverageStatus::Complete
        );
        let bindings = fixture.bindings();
        let model = fixture.model(&bindings);
        assert!(model.complete);
        let group = fixture.span_at(0, fixture.text.find("#End If").unwrap() + 7);
        let expected: Vec<_> = boundaries
            .iter()
            .zip(activities)
            .map(|(boundary, activity)| VbaCompilationBranch {
                group,
                region: fixture.span_at(fixture.text.find(boundary).unwrap(), 0),
                activity,
            })
            .collect();
        assert_eq!(model.branches, expected);
        for boundary in boundaries {
            assert_eq!(model.activity_at(fixture.span(boundary)), Unproven);
        }
    }
}

#[test]
fn zero_branch_inventory_proves_only_nonempty_unguarded_source() {
    let fixture = Fixture::new("Option Explicit\n");
    let bindings = fixture.bindings();
    let model = fixture.model(&bindings);
    assert!(model.complete);
    assert!(model.branches.is_empty());
    assert_eq!(
        model.activity_at(fixture.span("Option Explicit")),
        VbaCompilationActivity::Active
    );

    let fixture = Fixture::new("");
    assert!(fixture.bindings().compilations().next().is_none());
}

#[test]
fn unchanged_branch_cap_keeps_complete_inventory_or_fails_closed() {
    for count in [
        MAX_VBA_COMPILATION_BRANCHES,
        MAX_VBA_COMPILATION_BRANCHES + 1,
    ] {
        let text: String = (0..count)
            .map(|index| format!("#If True Then\nSub Branch{index}()\nEnd Sub\n#End If\n"))
            .collect();
        let fixture = Fixture::new(text);
        assert_eq!(
            fixture.output.report().coverage().status(),
            rootlight_ir::CoverageStatus::Complete
        );
        let bindings = fixture.bindings();
        let model = fixture.model(&bindings);
        if count == MAX_VBA_COMPILATION_BRANCHES {
            assert!(model.complete);
            assert_eq!(model.branches.len(), count);
            assert!(
                model
                    .branches
                    .iter()
                    .all(|branch| branch.activity == VbaCompilationActivity::Active)
            );
        } else {
            assert!(!model.complete);
            assert!(model.branches.is_empty());
            assert_eq!(
                model.activity_at(fixture.span("Branch0")),
                VbaCompilationActivity::Unproven
            );
            for group in fixture
                .output
                .facts()
                .iter()
                .filter(|fact| fact.syntax_kind().as_str() == "vba.preprocessor.scope")
            {
                assert!(
                    !bindings
                        .compilation_evaluation_proven(group, &Cancellation::new())
                        .unwrap()
                );
            }
        }
    }
}

#[test]
fn reserved_hosts_and_duplicate_constants_do_not_select_any_arm() {
    for host in ["Win16", "Win32", "Win64", "Mac", "Vba6", "Vba7"] {
        for text in [
            three_arms(host, "False"),
            format!("#Const {host} = True\n{}", three_arms("True", "False")),
        ] {
            assert_module_barrier(&Fixture::new(text));
        }
    }
    for prefix in [
        "#Const Enabled = True\n#Const ENABLED = False\n",
        "#Const Enabled = True\n#If False Then\n#Const ENABLED = False\n#End If\n",
    ] {
        assert_module_barrier(&Fixture::new(format!(
            "{prefix}{}",
            three_arms("Enabled", "False")
        )));
    }
}

fn assert_module_barrier(fixture: &Fixture) {
    let bindings = fixture.bindings();
    let model = fixture.model(&bindings);
    assert!(model.complete);
    assert!(!model.branches.is_empty());
    assert!(
        model
            .branches
            .iter()
            .all(|branch| branch.activity == VbaCompilationActivity::Unproven)
    );
    assert_eq!(
        model.activity_at(fixture.span("First")),
        VbaCompilationActivity::Unproven
    );
    for fact in fixture.output.facts().iter().filter(|fact| {
        matches!(
            fact.syntax_kind().as_str(),
            "vba.preprocessor.scope" | "vba.compilation_constant.declaration"
        )
    }) {
        assert!(
            !bindings
                .compilation_evaluation_proven(fact, &Cancellation::new())
                .unwrap()
        );
    }
}

#[test]
fn a_later_unknown_or_invalid_elseif_cannot_promote_an_earlier_true_arm() {
    for later in ["HostChoice", "True And", "1 + 1"] {
        let fixture = Fixture::new(three_arms("True", later));
        let bindings = fixture.bindings();
        let model = fixture.model(&bindings);
        assert_eq!(
            model.activity_at(fixture.span("First")),
            VbaCompilationActivity::Unproven
        );
        assert!(
            model
                .branches
                .iter()
                .all(|branch| branch.activity == VbaCompilationActivity::Unproven)
        );
        for group in fixture
            .output
            .facts()
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "vba.preprocessor.scope")
        {
            assert!(
                !bindings
                    .compilation_evaluation_proven(group, &Cancellation::new())
                    .unwrap()
            );
        }
    }
}

#[test]
fn split_headers_do_not_claim_an_unguarded_module_or_bind_excluded_parameters() {
    let fixture = Fixture::new(
        "#If False Then\nSub Choose(excluded As Long)\n#Else\nSub Choose(included As Long)\n#End If\nCall Sink(excluded)\nEnd Sub\n",
    );
    let bindings = fixture.bindings();
    let model = fixture.model(&bindings);
    assert!(!model.complete);
    assert!(model.branches.is_empty());
    let parameters: Vec<_> = fixture
        .output
        .facts()
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "vba.parameter.declaration")
        .map(|fact| fixture.text_at(fact))
        .collect();
    assert_eq!(parameters, ["excluded As Long", "included As Long"]);
    let source = fixture.span_at(fixture.text.rfind("excluded").unwrap(), "excluded".len());
    let reference = fixture
        .output
        .facts()
        .iter()
        .find(|fact| fact.span() == source && fact.syntax_kind().as_str().ends_with(".reference"))
        .unwrap();
    assert!(
        bindings
            .resolve(reference, "excluded", &Cancellation::new())
            .unwrap()
            .is_none()
    );
    assert_eq!(model.activity_at(source), VbaCompilationActivity::Unproven);
}

#[test]
fn constructor_and_evaluation_proof_propagate_precancelled_reason() {
    let fixture = Fixture::new(three_arms("True", "False"));
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        VbaBindings::new(
            fixture.output.facts(),
            fixture.text.as_bytes(),
            &HashMap::new(),
            1_024,
            &cancellation
        ),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
    let bindings = fixture.bindings();
    let group = fixture
        .output
        .facts()
        .iter()
        .find(|fact| fact.syntax_kind().as_str() == "vba.preprocessor.scope")
        .unwrap();
    assert!(matches!(
        bindings.compilation_evaluation_proven(group, &cancellation),
        Err(AdapterError::Cancelled {
            reason: CancellationReason::ClientRequest
        })
    ));
}
