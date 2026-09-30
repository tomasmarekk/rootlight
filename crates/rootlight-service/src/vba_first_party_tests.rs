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
    let expression = "Call Notify()";
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
    let application_proven = parameters.is_empty();
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
