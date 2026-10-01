//! Durable service contracts for switching procedural VBA parser routes.
//! First-party and native fallback facts share the existing generation-bound
//! query/source contract without retaining a stale parser artifact.

use super::*;
use rootlight_ir::OccurrenceTarget;
use std::fs;

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(60)).unwrap())
}

fn assert_source_and_identity(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
) {
    assert_source_and_identity_for_language(service, receipt, source, "visual-basic-classic");
}

fn assert_source_and_identity_for_language(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    let file = &document.files[0];
    assert_eq!(file.language, language);
    let source_ref = file.evidence.source.clone().unwrap();
    assert_eq!(source_ref.content_hash(), content_hash(source.as_bytes()));
    assert_eq!(source_ref.generation(), receipt.generation);
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![source_ref],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, source.as_bytes());
    let parser = document
        .provenance
        .iter()
        .find(|record| {
            record.producer_kind == ProducerKind::Parser && record.language == file.language
        })
        .unwrap();
    assert_eq!(
        parser.frontend_version.as_deref(),
        Some("rootlight-vba-procedural-1+tree-sitter-vba-0.14.1")
    );
    let registry = GrammarRegistry::audited().unwrap();
    let family = match language {
        "vba" => GrammarFamily::Vba,
        "visual-basic-classic" => GrammarFamily::VisualBasicClassic,
        _ => panic!("an authored VBA language route is required"),
    };
    let descriptor = registry
        .descriptors()
        .iter()
        .find(|descriptor| descriptor.family() == family)
        .unwrap();
    assert_eq!(
        parser.binary_digest,
        grammar_binary_digest(descriptor).unwrap()
    );
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.canonical_name == "Run")
    );
    let locate = service
        .code_locate(
            receipt.generation,
            "Run".to_owned(),
            LocateMode::Exact,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(locate.data.hits.iter().any(|hit| hit.symbol.is_some()));
    for hit in locate.data.hits.iter().filter(|hit| hit.symbol.is_some()) {
        let evidence = hit.source.as_ref().unwrap();
        assert_eq!(evidence.generation(), receipt.generation);
        assert_eq!(evidence.content_hash(), content_hash(source.as_bytes()));
    }
}

#[test]
fn procedural_route_changes_survive_noop_reopen_and_match_clean_rebuild() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let empty = "Attribute VB_Name = \"Authored\"\nOption Explicit\nSub Run()\nEnd Sub\n";
    let populated = "Attribute VB_Name = \"Authored\"\nOption Explicit\nSub Run()\nDim value As Long\nConst Limit As Long = 7\nvalue = Limit\nLet value = 1\nEnd Sub\n";
    let native = "Attribute VB_Name = \"Authored\"\nOption Explicit\nSub Run()\nDim value As Long\nvalue = 1 + 2\nEnd Sub\n";
    let mut first_empty = None;
    let mut previous_generation = None;
    for source in [empty, populated, native, populated, empty] {
        fs::write(repository.path().join("module.bas"), source).unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service
            .index_repository(repository.path(), &deadline())
            .unwrap();
        assert_source_and_identity(&service, &receipt, source);
        if source == populated {
            for name in ["value", "Limit"] {
                let locate = service
                    .code_locate(
                        receipt.generation,
                        name.to_owned(),
                        LocateMode::Exact,
                        10,
                        0,
                        &deadline(),
                    )
                    .unwrap();
                assert!(locate.data.hits.iter().any(|hit| hit.symbol.is_some()));
                for hit in locate.data.hits.iter().filter(|hit| hit.symbol.is_some()) {
                    let evidence = hit.source.as_ref().unwrap();
                    assert_eq!(evidence.generation(), receipt.generation);
                    assert_eq!(evidence.content_hash(), content_hash(source.as_bytes()));
                }
            }
        }
        assert_eq!(
            receipt,
            service
                .index_repository(repository.path(), &deadline())
                .unwrap()
        );
        if let Some(previous) = previous_generation {
            assert_ne!(receipt.generation, previous);
        }
        previous_generation = Some(receipt.generation);
        if source == empty {
            if let Some(logical) = &first_empty {
                assert_eq!(&receipt.logical_snapshot, logical);
            } else {
                first_empty = Some(receipt.logical_snapshot.clone());
            }
        }
        drop(service);
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            receipt,
            service
                .index_repository(repository.path(), &deadline())
                .unwrap()
        );
        assert_source_and_identity(&service, &receipt, source);
        let rebuilt = service
            .index_repository_with_options(
                repository.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        assert!(receipt.logical_snapshot.is_some());
        assert_eq!(receipt.logical_snapshot, rebuilt.logical_snapshot);
        assert_source_and_identity(&service, &rebuilt, source);
    }
}

#[test]
fn procedural_frontend_identity_includes_both_local_and_retained_parser_inputs() {
    let registry = GrammarRegistry::audited().unwrap();
    for family in [GrammarFamily::Vba, GrammarFamily::VisualBasicClassic] {
        let descriptor = registry
            .descriptors()
            .iter()
            .find(|descriptor| descriptor.family() == family)
            .unwrap();
        let legacy = hash_static_components(&[
            ANALYZER_BINARY_SEED,
            descriptor.language().as_str().as_bytes(),
            descriptor.grammar_version().as_bytes(),
            descriptor.grammar_source_sha256().as_bytes(),
            descriptor.parser_sha256().as_bytes(),
            descriptor
                .scanner_sha256()
                .unwrap_or("no-scanner")
                .as_bytes(),
        ])
        .unwrap();
        assert_ne!(grammar_binary_digest(descriptor).unwrap(), legacy);
        assert_eq!(
            grammar_binary_digest(descriptor).unwrap(),
            hash_static_components(&[
                legacy.as_bytes(),
                VBA_PROCEDURAL_FRONTEND_VERSION.as_bytes(),
                vba_procedural_frontend_digest().as_bytes(),
            ])
            .unwrap()
        );
    }
}

fn assert_explicit_call(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    resolved: bool,
) {
    assert_source_and_identity(service, receipt, source);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let calls = document
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), usize::from(resolved), "{source}");
    let run = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Run")
        .unwrap();
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([run.id]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            10,
            0,
            &deadline(),
        )
        .unwrap();
    let items = relationships
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .collect::<Vec<_>>();
    assert_eq!(items.len(), usize::from(resolved));
    assert!(!relationships.data.truncated);
    if resolved {
        let notify = document
            .entities
            .iter()
            .find(|entity| entity.canonical_name == "Notify")
            .unwrap();
        assert_eq!(calls[0].object, RelationEndpoint::Entity(notify.id));
        assert_eq!(items[0].symbol, notify.id);
        assert_eq!(items[0].source_refs.len(), 1);
        let reference = &items[0].source_refs[0];
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        let start = source.find("Call Notify").unwrap() + "Call ".len();
        assert_eq!(reference.span().start_byte(), u64::try_from(start).unwrap());
        assert_eq!(
            reference.span().end_byte(),
            u64::try_from(start + "Notify".len()).unwrap()
        );
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].bytes, b"Notify");
    }
}

#[test]
fn explicit_call_targets_survive_reopen_and_retire_without_fabricated_edges() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let target = "Sub Notify()\nEnd Sub\n";
    let mut previous: Option<(FirstSliceIndexReceipt, String, bool)> = None;
    for (declarations, body, resolved) in [
        (target.to_owned(), "Call Notify", true),
        (target.to_owned(), "Call Notify()", true),
        (String::new(), "Call Notify()", false),
        (
            target.to_owned(),
            "Dim Notify As Long\nCall Notify()",
            false,
        ),
        (target.repeat(2), "Call Notify()", false),
        (target.to_owned(), "Call Notify", true),
        (target.to_owned(), "", false),
    ] {
        let source = format!(
            "Attribute VB_Name = \"Authored\"\nOption Explicit\n{declarations}Sub Run()\n{body}\nEnd Sub\n"
        );
        fs::write(repository.path().join("module.bas"), &source).unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service
            .index_repository(repository.path(), &deadline())
            .unwrap();
        assert_explicit_call(&service, &receipt, &source, resolved);
        assert_eq!(
            receipt,
            service
                .index_repository(repository.path(), &deadline())
                .unwrap()
        );
        if let Some((old_receipt, old_source, old_resolved)) = &previous {
            assert_explicit_call(&service, old_receipt, old_source, *old_resolved);
        }
        drop(service);
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            receipt,
            service
                .index_repository(repository.path(), &deadline())
                .unwrap()
        );
        assert_explicit_call(&service, &receipt, &source, resolved);
        if let Some((old_receipt, old_source, old_resolved)) = &previous {
            assert_explicit_call(&service, old_receipt, old_source, *old_resolved);
        }
        let rebuilt = service
            .index_repository_with_options(
                repository.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        assert!(receipt.logical_snapshot.is_some());
        assert_eq!(receipt.logical_snapshot, rebuilt.logical_snapshot);
        assert_explicit_call(&service, &rebuilt, &source, resolved);
        previous = Some((receipt, source, resolved));
    }
}

fn assert_exact_parameter_source(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    reference: &SourceRef,
    range: std::ops::Range<usize>,
) {
    assert_eq!(reference.repository(), receipt.repository);
    assert_eq!(reference.generation(), receipt.generation);
    assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
    assert_eq!(
        reference.span().start_byte(),
        u64::try_from(range.start).unwrap()
    );
    assert_eq!(
        reference.span().end_byte(),
        u64::try_from(range.end).unwrap()
    );
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].bytes, &source.as_bytes()[range]);
}

type WrittenParameters<'a> = &'a [(&'a str, &'a str)];

fn assert_required_parameter_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    parameters: WrittenParameters<'_>,
) {
    assert_parameter_call_contract(
        service,
        receipt,
        source,
        language,
        parameters,
        "Call Notify()",
        parameters.is_empty(),
    );
}

fn assert_parameter_call_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    parameters: WrittenParameters<'_>,
    expression: &str,
    application_proven: bool,
) {
    assert_source_and_identity_for_language(service, receipt, source, language);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let notify = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Notify")
        .unwrap();
    let run = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Run")
        .unwrap();
    let owner_start = source.find("Sub Notify(").unwrap();
    let owner_end = source.find("End Sub").unwrap() + "End Sub".len();
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        notify.evidence.source.as_ref().unwrap(),
        owner_start..owner_end,
    );
    let owned = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(notify.id))
        })
        .collect::<Vec<_>>();
    assert_eq!(owned.len(), parameters.len());
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        parameters.len()
    );
    for &(written, name) in parameters {
        let parameter = owned
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(parameter.language, language);
        let start = source.find(written).unwrap();
        let declaration = parameter.evidence.source.as_ref().unwrap();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            declaration,
            start..start + written.len(),
        );
        let definition_start = start + written.find(name).unwrap();
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        }
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].enclosing, Some(notify.id));
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &definitions[0].source,
            definition_start..definition_start + name.len(),
        );
        let explained = service
            .symbol_explain(receipt.generation, parameter.id, &deadline())
            .unwrap();
        assert_eq!(&explained.data.entity, *parameter);
        assert!(!explained.data.truncated);
    }
    for name in ["value", "renamed", "extra"] {
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                10,
                0,
                &deadline(),
            )
            .unwrap();
        let hits = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol.is_some())
            .collect::<Vec<_>>();
        let expected = owned.iter().find(|entity| entity.canonical_name == name);
        assert_eq!(hits.len(), usize::from(expected.is_some()));
        if let Some(parameter) = expected {
            assert_eq!(hits[0].symbol, Some(parameter.id));
            assert_eq!(hits[0].source.as_ref(), parameter.evidence.source.as_ref());
        }
    }
    let start = source.find(expression).unwrap();
    let applications = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(start + expression.len()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(applications.len(), 1);
    let application = applications[0];
    assert_eq!(application.role, OccurrenceRole::Reference);
    assert_eq!(application.enclosing, Some(run.id));
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        &application.source,
        start..start + expression.len(),
    );
    if application_proven {
        assert_eq!(
            application.target,
            OccurrenceTarget::Resolved { symbol: notify.id }
        );
    } else {
        assert!(matches!(
            application.target,
            OccurrenceTarget::Unresolved { .. }
        ));
    }
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Occurrence(application.id)
                    && relation.predicate == RelationPredicate::RefersTo
                    && relation.object == RelationEndpoint::Entity(notify.id)
            })
            .count(),
        usize::from(application_proven)
    );
    assert!(!document.relations.iter().any(|relation| {
        relation.subject == RelationEndpoint::Occurrence(application.id)
            && relation.predicate == RelationPredicate::Calls
    }));
    assert_eq!(
        document.skipped_regions.iter().any(|gap| {
            gap.source == application.source && gap.detail == "vba-call-or-index-target-unavailable"
        }),
        !application_proven
    );
    // Arity failure retires the application proof, not the independently known
    // callee identity or its source-written Calls edge.
    let callee_start = start + "Call ".len();
    let callees = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "vba.invoked_identifier.reference"
                && site.source.span().start_byte() == u64::try_from(callee_start).unwrap()
                && site.source.span().end_byte()
                    == u64::try_from(callee_start + "Notify".len()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(callees.len(), 1);
    assert_eq!(callees[0].enclosing, Some(run.id));
    assert_eq!(callees[0].role, OccurrenceRole::CallSite);
    assert_eq!(
        callees[0].target,
        OccurrenceTarget::Resolved { symbol: notify.id }
    );
    let calls = document
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].subject,
        RelationEndpoint::Occurrence(callees[0].id)
    );
    assert_eq!(calls[0].object, RelationEndpoint::Entity(notify.id));
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        &callees[0].source,
        callee_start..callee_start + "Notify".len(),
    );
}

#[test]
fn required_parameter_edits_preserve_ownership_and_distinguish_application_arity() {
    let states: [(&str, WrittenParameters<'_>); 4] = [
        ("", &[]),
        ("ByVal value As Long", &[("ByVal value As Long", "value")]),
        (
            "ByRef renamed As Long, ByVal extra As String",
            &[
                ("ByRef renamed As Long", "renamed"),
                ("ByVal extra As String", "extra"),
            ],
        ),
        ("", &[]),
    ];
    for language in ["visual-basic-classic", "vba"] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let configuration = (language == "vba").then_some(
            "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n",
        );
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, WrittenParameters<'_>)> = None;
        let mut first_zero = None;
        for (header, parameters) in states {
            let source = format!(
                "Attribute VB_Name = \"Authored\"\nOption Explicit\nSub Notify({header})\nEnd Sub\nSub Run()\nCall Notify()\nEnd Sub\n"
            );
            fs::write(repository.path().join("module.bas"), &source).unwrap();
            let mut service = FirstSliceService::new_durable_with_policy(
                policy.clone(),
                paths.state_dir(),
                &deadline(),
            )
            .unwrap();
            let receipt = service
                .index_repository(repository.path(), &deadline())
                .unwrap();
            assert_required_parameter_contract(&service, &receipt, &source, language, parameters);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_parameters)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                assert_required_parameter_contract(
                    &service,
                    old_receipt,
                    old_source,
                    language,
                    old_parameters,
                );
            }
            drop(service);
            let mut service = FirstSliceService::new_durable_with_policy(
                policy.clone(),
                paths.state_dir(),
                &deadline(),
            )
            .unwrap();
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            assert_required_parameter_contract(&service, &receipt, &source, language, parameters);
            if let Some((old_receipt, old_source, old_parameters)) = &previous {
                assert_required_parameter_contract(
                    &service,
                    old_receipt,
                    old_source,
                    language,
                    old_parameters,
                );
            }
            let rebuilt = service
                .index_repository_with_options(
                    repository.path(),
                    FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                    &deadline(),
                )
                .unwrap();
            assert_ne!(receipt.generation, rebuilt.generation);
            assert!(receipt.logical_snapshot.is_some());
            assert_eq!(receipt.logical_snapshot, rebuilt.logical_snapshot);
            assert_required_parameter_contract(&service, &rebuilt, &source, language, parameters);
            if parameters.is_empty() {
                if let Some(logical) = &first_zero {
                    assert_eq!(&receipt.logical_snapshot, logical);
                } else {
                    first_zero = Some(receipt.logical_snapshot.clone());
                }
            }
            // Current configuration retains two generations. The clean rebuild
            // is the latest published receipt, not an extra historical lifetime.
            previous = Some((rebuilt, source, parameters));
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ScalarCallState {
    local: &'static str,
    arguments: &'static str,
    name_argument: bool,
    application_proven: bool,
}

fn assert_scalar_call_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: ScalarCallState,
) {
    let parameters: WrittenParameters<'_> = &[
        ("ByVal payload As Long", "payload"),
        ("ByVal second As String", "second"),
    ];
    let expression = format!("Call Notify({})", state.arguments);
    assert_parameter_call_contract(
        service,
        receipt,
        source,
        language,
        parameters,
        &expression,
        state.application_proven,
    );
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let notify = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Notify")
        .unwrap();
    let run = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Run")
        .unwrap();
    let locals = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Variable
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(run.id))
        })
        .collect::<Vec<_>>();
    assert_eq!(locals.len(), 1);
    let local = locals[0];
    assert_eq!(local.canonical_name, state.local);
    assert_eq!(local.language, language);
    let local_declaration = format!("{} As Long", state.local);
    let local_start = source.find(&format!("Dim {local_declaration}")).unwrap() + "Dim ".len();
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        local.evidence.source.as_ref().unwrap(),
        local_start..local_start + local_declaration.len(),
    );
    let local_definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && site.target == OccurrenceTarget::Resolved { symbol: local.id }
        })
        .collect::<Vec<_>>();
    assert_eq!(local_definitions.len(), 1);
    assert_eq!(local_definitions[0].enclosing, Some(run.id));
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        &local_definitions[0].source,
        local_start..local_start + state.local.len(),
    );
    for name in ["payload", "second", "updated"] {
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                10,
                0,
                &deadline(),
            )
            .unwrap();
        let hits = located
            .data
            .hits
            .iter()
            .filter(|hit| hit.symbol.is_some())
            .collect::<Vec<_>>();
        let expected = document
            .entities
            .iter()
            .filter(|entity| {
                matches!(entity.kind, EntityKind::Variable | EntityKind::Parameter)
                    && entity.canonical_name == name
            })
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), expected.len());
        for entity in expected {
            let hit = hits
                .iter()
                .find(|hit| hit.symbol == Some(entity.id))
                .unwrap();
            assert_eq!(hit.source.as_ref(), entity.evidence.source.as_ref());
        }
    }
    let start = source.find(&expression).unwrap();
    let end = start + expression.len();
    let application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(end).unwrap()
        })
        .unwrap();
    let proof = document
        .provenance
        .iter()
        .find(|proof| proof.id == application.provenance)
        .unwrap();
    assert_eq!(proof.repository, receipt.repository);
    assert_eq!(proof.generation, receipt.generation);
    for parameter in document.entities.iter().filter(|entity| {
        entity.kind == EntityKind::Parameter
            && entity.container == Some(rootlight_ir::ContainerRef::Entity(notify.id))
    }) {
        assert_eq!(
            proof
                .derivation_parents
                .contains(&FactRef::Entity(parameter.id)),
            state.application_proven,
            "formal parameter proof for {state:?}: rule={:?}, parents={:?}",
            proof.rule,
            proof.derivation_parents
        );
        if state.application_proven {
            let clause = parameter.evidence.source.as_ref().unwrap();
            assert!(proof.input_sources.contains(clause));
            assert!(proof.evidence_sources.contains(clause));
        }
    }
    let reads = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "vba.value_identifier.reference"
                && site.source.span().start_byte() >= u64::try_from(start).unwrap()
                && site.source.span().end_byte() <= u64::try_from(end).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), usize::from(state.name_argument));
    if state.name_argument {
        let read = reads[0];
        assert_eq!(read.role, OccurrenceRole::Reference);
        assert_eq!(read.enclosing, Some(run.id));
        assert_eq!(read.target, OccurrenceTarget::Resolved { symbol: local.id });
        let name_start = start + expression.find(state.local).unwrap();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &read.source,
            name_start..name_start + state.local.len(),
        );
        let refers_to = document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Occurrence(read.id)
                    && relation.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        assert_eq!(refers_to.len(), 1);
        assert_eq!(refers_to[0].object, RelationEndpoint::Entity(local.id));
        assert_eq!(refers_to[0].evidence.source.as_ref(), Some(&read.source));
        assert!(!document.relations.iter().any(|relation| {
            relation.subject == RelationEndpoint::Occurrence(read.id)
                && relation.predicate == RelationPredicate::Calls
        }));
        let formal = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == "payload"
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(notify.id))
            })
            .unwrap();
        assert_ne!(local.id, formal.id);
        assert_ne!(
            read.target,
            OccurrenceTarget::Resolved { symbol: formal.id }
        );
    }
    let strings = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::StringEvidence
                && site.source.span().start_byte() >= u64::try_from(start).unwrap()
                && site.source.span().end_byte() <= u64::try_from(end).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        strings.len(),
        usize::from(state.arguments.contains("\"text\""))
    );
    if let Some(string) = strings.first() {
        let string_start = start + expression.find("\"text\"").unwrap();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &string.source,
            string_start..string_start + "\"text\"".len(),
        );
    }
    for literal in ["1", "True", "False"] {
        if let Some(offset) = expression.find(literal) {
            let literal_start = start + offset;
            assert!(!document.occurrences.iter().any(|site| {
                site.source.span().start_byte() == u64::try_from(literal_start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(literal_start + literal.len()).unwrap()
            }));
        }
    }
}

#[test]
fn scalar_call_argument_edits_preserve_sources_ownership_and_written_arity() {
    let states = [
        ScalarCallState {
            local: "payload",
            arguments: "",
            name_argument: false,
            application_proven: false,
        },
        ScalarCallState {
            local: "payload",
            arguments: "payload",
            name_argument: true,
            application_proven: false,
        },
        ScalarCallState {
            local: "payload",
            arguments: "payload, \"text\"",
            name_argument: true,
            application_proven: true,
        },
        ScalarCallState {
            local: "updated",
            arguments: "1, updated, True",
            name_argument: true,
            application_proven: false,
        },
        // This proves written cardinality and passing syntax, not runtime type
        // compatibility between a Boolean literal and an intrinsic parameter.
        ScalarCallState {
            local: "updated",
            arguments: "False, \"text\"",
            name_argument: false,
            application_proven: true,
        },
        ScalarCallState {
            local: "payload",
            arguments: "",
            name_argument: false,
            application_proven: false,
        },
    ];
    for language in ["visual-basic-classic", "vba"] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let configuration = (language == "vba").then_some(
            "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n",
        );
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, ScalarCallState)> = None;
        let mut first_zero = None;
        for state in states {
            let source = format!(
                "Attribute VB_Name = \"Authored\"\nOption Explicit\nSub Notify(ByVal payload As Long, ByVal second As String)\nEnd Sub\nSub Run()\nDim {} As Long\nCall Notify({})\nEnd Sub\n",
                state.local, state.arguments,
            );
            fs::write(repository.path().join("module.bas"), &source).unwrap();
            let mut service = FirstSliceService::new_durable_with_policy(
                policy.clone(),
                paths.state_dir(),
                &deadline(),
            )
            .unwrap();
            let receipt = service
                .index_repository(repository.path(), &deadline())
                .unwrap();
            assert_scalar_call_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                assert_scalar_call_contract(
                    &service,
                    old_receipt,
                    old_source,
                    language,
                    *old_state,
                );
            }
            drop(service);
            let mut service = FirstSliceService::new_durable_with_policy(
                policy.clone(),
                paths.state_dir(),
                &deadline(),
            )
            .unwrap();
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            assert_scalar_call_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_scalar_call_contract(
                    &service,
                    old_receipt,
                    old_source,
                    language,
                    *old_state,
                );
            }
            let rebuilt = service
                .index_repository_with_options(
                    repository.path(),
                    FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                    &deadline(),
                )
                .unwrap();
            assert_ne!(receipt.generation, rebuilt.generation);
            assert!(receipt.logical_snapshot.is_some());
            assert_eq!(receipt.logical_snapshot, rebuilt.logical_snapshot);
            assert_scalar_call_contract(&service, &rebuilt, &source, language, state);
            if state.arguments.is_empty() {
                if let Some(logical) = &first_zero {
                    assert_eq!(&receipt.logical_snapshot, logical);
                } else {
                    first_zero = Some(receipt.logical_snapshot.clone());
                }
            }
            // Check the prior receipt before this rebuild: modern policy retains
            // two generations, and the rebuilt receipt is now the latest one.
            previous = Some((rebuilt, source, state));
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct OptionalCallState {
    parameters: WrittenParameters<'static>,
    arguments: &'static str,
    application_proven: bool,
}

fn assert_optional_call_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: OptionalCallState,
) {
    let expression = format!("Call Notify({})", state.arguments);
    assert_parameter_call_contract(
        service,
        receipt,
        source,
        language,
        state.parameters,
        &expression,
        state.application_proven,
    );
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let notify = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Notify")
        .unwrap();
    let parser = document
        .provenance
        .iter()
        .find(|proof| proof.id == notify.provenance)
        .unwrap();
    assert_eq!(parser.producer_kind, ProducerKind::Parser);
    assert_eq!(parser.repository, receipt.repository);
    assert_eq!(parser.generation, receipt.generation);
    assert_eq!(parser.language, language);
    let start = source.find(&expression).unwrap();
    let end = start + expression.len();
    let application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(end).unwrap()
        })
        .unwrap();
    let proof = document
        .provenance
        .iter()
        .find(|proof| proof.id == application.provenance)
        .unwrap();
    let mut expected_parents = vec![FactRef::Fact(parser.id), FactRef::Entity(notify.id)];
    for &(written, name) in state.parameters {
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(notify.id))
            })
            .unwrap();
        assert_eq!(parameter.provenance, parser.id);
        let clause = parameter.evidence.source.as_ref().unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: parameter.id,
                        }
            })
            .unwrap();
        assert_eq!(definition.provenance, parser.id);
        assert_eq!(
            proof
                .derivation_parents
                .contains(&FactRef::Entity(parameter.id)),
            state.application_proven,
            "formal source closure for {state:?}: rule={:?}",
            proof.rule,
        );
        if state.application_proven {
            expected_parents.push(FactRef::Entity(parameter.id));
            assert!(proof.input_sources.contains(clause));
            assert!(proof.evidence_sources.contains(clause));
            assert!(proof.evidence_sources.contains(&definition.source));
        }
        if let Some((prefix, literal)) = written.split_once(" = ") {
            let literal_start = source.find(written).unwrap() + prefix.len() + " = ".len();
            let literal_end = literal_start + literal.len();
            let reference = SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    clause.span().file(),
                    u64::try_from(literal_start).unwrap(),
                    u64::try_from(literal_end).unwrap(),
                )
                .unwrap(),
                content_hash(source.as_bytes()),
                None,
            );
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                &reference,
                literal_start..literal_end,
            );
            let literal_sites = document
                .occurrences
                .iter()
                .filter(|site| site.source.span() == reference.span())
                .collect::<Vec<_>>();
            if literal.starts_with('"') {
                assert_eq!(literal_sites.len(), 1);
                assert_eq!(literal_sites[0].role, OccurrenceRole::StringEvidence);
                assert_eq!(literal_sites[0].provenance, parser.id);
                assert_exact_parameter_source(
                    service,
                    receipt,
                    source,
                    &literal_sites[0].source,
                    literal_start..literal_end,
                );
            } else {
                assert!(literal_sites.is_empty());
            }
        }
    }
    if state.application_proven {
        expected_parents.sort_unstable();
        expected_parents.dedup();
        assert_ne!(proof.id, parser.id);
        assert_eq!(proof.derivation_parents, expected_parents);
        assert_eq!(
            proof.rule.as_deref(),
            Some("vba-procedure-application-v1.source")
        );
        assert_eq!(proof.producer_kind, parser.producer_kind);
        assert_eq!(proof.binary_digest, parser.binary_digest);
        assert_eq!(proof.frontend_version, parser.frontend_version);
        assert!(
            proof
                .input_sources
                .contains(document.files[0].evidence.source.as_ref().unwrap())
        );
        assert!(proof.evidence_sources.contains(&application.source));
        assert!(
            proof
                .evidence_sources
                .contains(notify.evidence.source.as_ref().unwrap())
        );
        let relation = document
            .relations
            .iter()
            .find(|relation| {
                relation.subject == RelationEndpoint::Occurrence(application.id)
                    && relation.predicate == RelationPredicate::RefersTo
                    && relation.object == RelationEndpoint::Entity(notify.id)
            })
            .unwrap();
        assert_eq!(relation.provenance, proof.id);
        assert_eq!(relation.evidence.source.as_ref(), Some(&application.source));
    } else {
        assert_eq!(proof.id, parser.id);
    }
    for reference in proof.input_sources.iter().chain(&proof.evidence_sources) {
        assert_eq!(reference.repository(), receipt.repository);
        assert_eq!(reference.generation(), receipt.generation);
        assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
        assert_eq!(reference.span().file(), document.files[0].id);
        assert!(reference.span().end_byte() <= u64::try_from(source.len()).unwrap());
    }
    assert!(!document.occurrences.iter().any(|site| {
        site.syntax_kind == "vba.value_identifier.reference"
            && site.source.span().start_byte() >= u64::try_from(start).unwrap()
            && site.source.span().end_byte() <= u64::try_from(end).unwrap()
    }));
    for argument in state
        .arguments
        .split(',')
        .filter(|argument| !argument.trim().is_empty())
    {
        let literal = argument.trim();
        let offset = expression.find(literal).unwrap();
        let literal_start = start + offset;
        let literal_end = literal_start + literal.len();
        let literal_sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == u64::try_from(literal_start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(literal_end).unwrap()
            })
            .collect::<Vec<_>>();
        if literal.starts_with('"') {
            assert_eq!(literal_sites.len(), 1);
            assert_eq!(literal_sites[0].role, OccurrenceRole::StringEvidence);
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                &literal_sites[0].source,
                literal_start..literal_end,
            );
        } else {
            assert!(literal_sites.is_empty());
        }
    }
}

#[test]
fn optional_parameter_default_edits_preserve_sources_and_omission_proofs() {
    let initial = OptionalCallState {
        parameters: &[
            ("ByVal value As Long", "value"),
            ("Optional ByVal extra As Long = 7", "extra"),
        ],
        arguments: "1",
        application_proven: true,
    };
    let states = [
        initial,
        OptionalCallState {
            parameters: &[
                ("ByVal value As Long", "value"),
                ("Optional ByVal extra As Long = 8", "extra"),
            ],
            ..initial
        },
        OptionalCallState {
            parameters: &[
                ("ByVal value As Long", "value"),
                ("Optional ByRef renamed As String = \"text\"", "renamed"),
            ],
            arguments: "1, \"text\"",
            application_proven: true,
        },
        OptionalCallState {
            parameters: &[
                ("ByVal value As Long", "value"),
                ("Optional ByVal renamed As Boolean = False", "renamed"),
            ],
            arguments: "1, True",
            application_proven: true,
        },
        // Omission uses the source-written Optional marker, not a fabricated
        // default value or a claim about compiler legality of this typed form.
        OptionalCallState {
            parameters: &[
                ("ByVal value As Long", "value"),
                ("Optional ByVal renamed As Boolean", "renamed"),
            ],
            ..initial
        },
        OptionalCallState {
            parameters: &[("ByVal value As Long", "value")],
            arguments: "1, True",
            application_proven: false,
        },
        OptionalCallState {
            arguments: "1, 2, True",
            application_proven: false,
            ..initial
        },
        OptionalCallState {
            parameters: &[
                ("ByVal value As Long", "value"),
                ("ByVal extra As Long", "extra"),
            ],
            arguments: "1",
            application_proven: false,
        },
        // The application matcher proves written arguments and formal omission;
        // Boolean atoms do not prove or disprove runtime scalar type coercion.
        OptionalCallState {
            parameters: &[
                ("ByVal value As Long", "value"),
                ("ByVal extra As Long", "extra"),
            ],
            arguments: "False, True",
            application_proven: true,
        },
        OptionalCallState {
            arguments: "",
            application_proven: false,
            ..initial
        },
        initial,
    ];
    for language in ["visual-basic-classic", "vba"] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let configuration = (language == "vba").then_some(
            "version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n",
        );
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, OptionalCallState)> = None;
        let mut initial_source = None;
        let mut initial_logical = None;
        for (position, state) in states.into_iter().enumerate() {
            let header = state
                .parameters
                .iter()
                .map(|(written, _)| *written)
                .collect::<Vec<_>>()
                .join(", ");
            let source = format!(
                "Attribute VB_Name = \"Authored\"\nOption Explicit\nSub Notify({header})\nEnd Sub\nSub Run()\nCall Notify({})\nEnd Sub\n",
                state.arguments,
            );
            fs::write(repository.path().join("module.bas"), &source).unwrap();
            let mut service = FirstSliceService::new_durable_with_policy(
                policy.clone(),
                paths.state_dir(),
                &deadline(),
            )
            .unwrap();
            let prepared = service
                .prepare_repository(repository.path(), &deadline())
                .unwrap();
            let committed = service
                .publish_prepared_with_metrics(prepared, &deadline())
                .unwrap();
            assert_eq!(committed.evidence().rebuilt_files, 1);
            assert_eq!(committed.evidence().reused_files, 0);
            if position > 0 {
                assert_eq!(committed.evidence().changed_files, 1);
            }
            let receipt = committed.receipt().clone();
            assert_optional_call_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                if position == 1 {
                    assert_eq!(source, old_source.replace(" = 7", " = 8"));
                    assert!(receipt.logical_snapshot.is_some());
                    assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                }
                assert_optional_call_contract(
                    &service,
                    old_receipt,
                    old_source,
                    language,
                    *old_state,
                );
            }
            drop(service);
            let mut service = FirstSliceService::new_durable_with_policy(
                policy.clone(),
                paths.state_dir(),
                &deadline(),
            )
            .unwrap();
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            assert_optional_call_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_optional_call_contract(
                    &service,
                    old_receipt,
                    old_source,
                    language,
                    *old_state,
                );
            }
            let prepared = service
                .prepare_repository_with_options(
                    repository.path(),
                    FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                    &deadline(),
                )
                .unwrap();
            let committed = service
                .publish_prepared_with_metrics(prepared, &deadline())
                .unwrap();
            assert_eq!(committed.evidence().rebuilt_files, 1);
            assert_eq!(committed.evidence().reused_files, 0);
            let rebuilt = committed.receipt().clone();
            assert_ne!(receipt.generation, rebuilt.generation);
            assert!(receipt.logical_snapshot.is_some());
            assert_eq!(receipt.logical_snapshot, rebuilt.logical_snapshot);
            assert_optional_call_contract(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if position + 1 == states.len() {
                assert_eq!(initial_source.as_ref(), Some(&source));
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            // The retained predecessor is the latest published rebuild. Check
            // it before another rebuild legitimately evicts it under retention2.
            previous = Some((rebuilt, source, state));
        }
    }
}
