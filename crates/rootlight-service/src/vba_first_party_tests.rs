//! Durable service contracts for switching procedural VBA parser routes.
//! First-party and native fallback facts share the existing generation-bound
//! query/source contract without retaining a stale parser artifact.

use super::*;
use rootlight_ir::OccurrenceTarget;
use rootlight_query::{
    ConditionalActivityBasis, ConditionalActivityReason, ConditionalActivityState,
};
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
        let bracketed = format!("[{name}]");
        let name_offset = written.find(name).unwrap();
        let bracketed_start = name_offset.checked_sub(1).filter(|offset| {
            written.get(*offset..*offset + bracketed.len()) == Some(bracketed.as_str())
        });
        let (definition_offset, definition_length) = if let Some(offset) = bracketed_start {
            (offset, bracketed.len())
        } else {
            (name_offset, name.len())
        };
        let definition_start = start + definition_offset;
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
            definition_start..definition_start + definition_length,
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
            } else if literal.eq_ignore_ascii_case("True")
                || literal.eq_ignore_ascii_case("False")
                || literal.bytes().all(|byte| byte.is_ascii_digit())
            {
                assert!(literal_sites.is_empty());
            } else {
                assert_eq!(literal_sites.len(), 1);
                assert_eq!(
                    literal_sites[0].syntax_kind,
                    "vba.default_constant_identifier.reference"
                );
                assert_eq!(literal_sites[0].role, OccurrenceRole::Reference);
                assert_eq!(literal_sites[0].enclosing, Some(parameter.id));
                assert_eq!(literal_sites[0].provenance, parser.id);
                assert_exact_parameter_source(
                    service,
                    receipt,
                    source,
                    &literal_sites[0].source,
                    literal_start..literal_end,
                );
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

fn assert_authored_parameter_types(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: OptionalCallState,
) {
    assert_optional_call_contract(service, receipt, source, language, state);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let notify = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Notify")
        .unwrap();
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
        let clause = parameter.evidence.source.as_ref().unwrap();
        if let Some((prefix, tail)) = written.split_once(" As ") {
            let authored_type = tail.split_once(" = ").map_or(tail, |(value, _)| value);
            let start = source.find(written).unwrap() + prefix.len() + " As ".len();
            let reference = SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    clause.span().file(),
                    u64::try_from(start).unwrap(),
                    u64::try_from(start + authored_type.len()).unwrap(),
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
                start..start + authored_type.len(),
            );
            continue;
        }
        // Omitted syntax is not an authored Variant reference. This assertion
        // concerns source facts only, not VBA's runtime default type semantics.
        let within_clause = |reference: &SourceRef| {
            reference.repository() == clause.repository()
                && reference.generation() == clause.generation()
                && reference.content_hash() == clause.content_hash()
                && reference.span().file() == clause.span().file()
                && reference.span().start_byte() >= clause.span().start_byte()
                && reference.span().end_byte() <= clause.span().end_byte()
        };
        assert!(!document.occurrences.iter().any(|site| {
            within_clause(&site.source)
                && (site.role == OccurrenceRole::TypeUse
                    || matches!(
                        site.syntax_kind.as_str(),
                        "vba.direct_type_identifier.reference"
                            | "vba.type_identifier.reference"
                            | "vba.automatic_type_identifier.reference"
                            | "vba.constructed_type_identifier.reference"
                            | "vba.tested_type_identifier.reference"
                    ))
        }));
        assert!(!document.relations.iter().any(|relation| {
            matches!(
                relation.predicate,
                RelationPredicate::UsesType | RelationPredicate::ParameterType
            ) && (relation.subject == RelationEndpoint::Entity(parameter.id)
                || relation.evidence.source.as_ref().is_some_and(within_clause))
        }));
        assert!(!document.entities.iter().any(|entity| {
            entity.canonical_name.eq_ignore_ascii_case("Variant")
                && entity.evidence.source.as_ref().is_some_and(within_clause)
        }));
    }
}

#[test]
fn omitted_parameter_types_preserve_authored_sources_and_call_proofs() {
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
                ("ByVal value", "value"),
                ("Optional ByVal extra As Long = 7", "extra"),
            ],
            ..initial
        },
        OptionalCallState {
            parameters: &[
                ("ByVal value", "value"),
                ("Optional ByVal extra = 7", "extra"),
            ],
            ..initial
        },
        OptionalCallState {
            parameters: &[
                ("ByVal value As Variant", "value"),
                ("Optional ByVal extra = 7", "extra"),
            ],
            ..initial
        },
        OptionalCallState {
            parameters: &[
                ("ByVal value As Variant", "value"),
                ("Optional ByVal extra As Variant = 7", "extra"),
            ],
            ..initial
        },
        OptionalCallState {
            parameters: &[
                ("ByRef renamed", "renamed"),
                ("Optional ByVal extra", "extra"),
            ],
            ..initial
        },
        OptionalCallState {
            parameters: &[
                ("ByRef renamed", "renamed"),
                ("Optional ByVal extra = \"text\"", "extra"),
            ],
            arguments: "1, \"text\"",
            application_proven: true,
        },
        OptionalCallState {
            parameters: &[("ByRef renamed", "renamed"), ("ByVal extra", "extra")],
            arguments: "1",
            application_proven: false,
        },
        // Literal atoms establish written cardinality, not inferred scalar
        // types or an evaluation of an omitted formal's default value.
        OptionalCallState {
            parameters: &[("ByRef renamed", "renamed"), ("ByVal extra", "extra")],
            arguments: "False, True",
            application_proven: true,
        },
        OptionalCallState {
            parameters: &[
                ("ByRef renamed As Long", "renamed"),
                ("Optional extra = True", "extra"),
            ],
            arguments: "",
            application_proven: false,
        },
        OptionalCallState {
            parameters: &[
                ("ByRef renamed As Long", "renamed"),
                ("Optional extra = True", "extra"),
            ],
            arguments: "1, True, 7",
            application_proven: false,
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
            assert_authored_parameter_types(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                let isolated_type_edit = match position {
                    1 => Some(("ByVal value As Long", "ByVal value")),
                    2 => Some(("Optional ByVal extra As Long", "Optional ByVal extra")),
                    3 => Some(("ByVal value,", "ByVal value As Variant,")),
                    4 => Some((
                        "Optional ByVal extra =",
                        "Optional ByVal extra As Variant =",
                    )),
                    _ => None,
                };
                if let Some((before, after)) = isolated_type_edit {
                    assert_eq!(source, old_source.replacen(before, after, 1));
                    assert!(receipt.logical_snapshot.is_some());
                    assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                }
                assert_authored_parameter_types(
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
            assert_authored_parameter_types(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_authored_parameter_types(
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
            assert_authored_parameter_types(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if position + 1 == states.len() {
                assert_eq!(initial_source.as_ref(), Some(&source));
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            // This rebuild is adjacent to the next incremental publication.
            // Check it before that iteration's rebuild evicts its generation.
            previous = Some((rebuilt, source, state));
        }
    }
}

#[test]
fn malformed_authored_parameter_types_remain_incomplete() {
    for language in ["visual-basic-classic", "vba"] {
        for written in ["ByVal value As", "Optional value As = 7"] {
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
            let source = format!(
                "Attribute VB_Name = \"Authored\"\nOption Explicit\nSub Notify({written})\nEnd Sub\nSub Run()\nCall Notify(1)\nEnd Sub\n",
            );
            fs::write(repository.path().join("module.bas"), &source).unwrap();
            let mut service =
                FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                    .unwrap();
            let receipt = service
                .index_repository(repository.path(), &deadline())
                .unwrap();
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            assert_eq!(document.files.len(), 1);
            let file = &document.files[0];
            assert_eq!(file.language, language);
            assert_exact_parameter_source(
                &service,
                &receipt,
                &source,
                file.evidence.source.as_ref().unwrap(),
                0..source.len(),
            );
            assert_eq!(
                service
                    .source_file_coverage(receipt.generation, file.id)
                    .unwrap()
                    .status,
                CoverageStatus::Unknown,
                "{language}: {written}",
            );
            assert!(document.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == "syntax-error-recovery"
                    && diagnostic.coverage_effect == CoverageStatus::Unknown
                    && diagnostic.source.as_ref().is_some_and(|reference| {
                        reference.repository() == receipt.repository
                            && reference.generation() == receipt.generation
                            && reference.span().file() == file.id
                            && reference.content_hash() == content_hash(source.as_bytes())
                    })
            }));
            let expression = "Call Notify(1)";
            let start = source.find(expression).unwrap();
            assert!(!document.occurrences.iter().any(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(start + expression.len()).unwrap()
                    && matches!(site.target, OccurrenceTarget::Resolved { .. })
            }));
        }
    }
}

#[test]
fn parameter_header_comments_do_not_become_formals_or_block_application_proofs() {
    let cases = [
        (
            "Rem",
            OptionalCallState {
                parameters: &[],
                arguments: "",
                application_proven: true,
            },
        ),
        (
            "Rem[value], Optional extra = 7",
            OptionalCallState {
                parameters: &[("[value]", "value"), ("Optional extra = 7", "extra")],
                arguments: "1",
                application_proven: true,
            },
        ),
        (
            "\x0cByVal\x0cvalue\x0cAs\x0cLong\x0c",
            OptionalCallState {
                parameters: &[("ByVal\x0cvalue\x0cAs\x0cLong", "value")],
                arguments: "1",
                application_proven: true,
            },
        ),
        (
            "ByVal Rem[value], Optional extra = 7",
            OptionalCallState {
                parameters: &[
                    ("ByVal Rem[value]", "value"),
                    ("Optional extra = 7", "extra"),
                ],
                arguments: "1",
                application_proven: true,
            },
        ),
    ];
    for language in ["visual-basic-classic", "vba"] {
        for (header, state) in cases {
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
            let source = format!(
                "Attribute VB_Name = \"Authored\"\nOption Explicit\nSub Notify({header})\nEnd Sub\nSub Run()\nCall Notify({})\nEnd Sub\n",
                state.arguments,
            );
            fs::write(repository.path().join("module.bas"), &source).unwrap();
            let mut service =
                FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline())
                    .unwrap();
            let prepared = service
                .prepare_repository(repository.path(), &deadline())
                .unwrap();
            let committed = service
                .publish_prepared_with_metrics(prepared, &deadline())
                .unwrap();
            assert_eq!(committed.evidence().rebuilt_files, 1);
            assert_eq!(committed.evidence().reused_files, 0);
            let receipt = committed.receipt().clone();
            assert_optional_call_contract(&service, &receipt, &source, language, state);
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            assert!(!document.entities.iter().any(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name.eq_ignore_ascii_case("Rem")
            }));
            if let Some(start) = source.find("Rem") {
                // A native bare comment token is not a runtime formal. Its
                // bytes remain readable even with an empty formal list.
                let comments = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.role == OccurrenceRole::Documentation
                            && site.source.span().start_byte() == u64::try_from(start).unwrap()
                            && site.source.span().end_byte() == u64::try_from(start + 3).unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(comments.len(), 1);
                assert_exact_parameter_source(
                    &service,
                    &receipt,
                    &source,
                    &comments[0].source,
                    start..start + 3,
                );
                let notify = document
                    .entities
                    .iter()
                    .find(|entity| entity.canonical_name == "Notify")
                    .unwrap();
                assert_eq!(comments[0].provenance, notify.provenance);
            }
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum NamedDefaultShadow {
    None,
    Parameter,
    LocalConstant,
}

#[derive(Debug, Clone, Copy)]
struct NamedDefaultState {
    module_constants: &'static str,
    declarations: WrittenParameters<'static>,
    call: OptionalCallState,
    body: &'static str,
    bound_constant: Option<&'static str>,
    shadow: NamedDefaultShadow,
}

fn assert_named_default_constant_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: NamedDefaultState,
) {
    assert_authored_parameter_types(service, receipt, source, language, state.call);
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
    let module_constants = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Constant
                && entity.container == notify.container
                && entity.evidence.source.is_some()
        })
        .collect::<Vec<_>>();
    let semantic_names = state
        .declarations
        .iter()
        .map(|(_, name)| *name)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(module_constants.len(), semantic_names.len());
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && module_constants.iter().any(|constant| {
                        site.target
                            == OccurrenceTarget::Resolved {
                                symbol: constant.id,
                            }
                    })
            })
            .count(),
        state.declarations.len()
    );
    for &(written, name) in state.declarations {
        let start = source.find(written).unwrap();
        let end = start + written.len();
        let constants = module_constants
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect::<Vec<_>>();
        assert_eq!(constants.len(), 1, "module declaration {written}");
        let constant = constants[0];
        assert_eq!(constant.language, language);
        assert_eq!(constant.provenance, parser.id);
        // Ordinary Const identity coalesces equal name/kind/owner signatures.
        // The entity keeps the earliest declaration, while every written clause
        // still owns an exact Definition and Contains source witness.
        let representative = state
            .declarations
            .iter()
            .filter(|(_, candidate)| *candidate == name)
            .map(|(written, _)| {
                let start = source.find(written).unwrap();
                (start, start + written.len())
            })
            .min()
            .unwrap();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            constant.evidence.source.as_ref().unwrap(),
            representative.0..representative.1,
        );
        let containers = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(constant.id)
                    && relation.evidence.source.as_ref().is_some_and(|reference| {
                        reference.span().start_byte() == u64::try_from(start).unwrap()
                            && reference.span().end_byte() == u64::try_from(end).unwrap()
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(containers.len(), 1, "written declaration {written}");
        let owner = match constant.container.unwrap() {
            rootlight_ir::ContainerRef::Repository(repository) => {
                RelationEndpoint::Repository(repository)
            }
            rootlight_ir::ContainerRef::File(file) => RelationEndpoint::File(file),
            rootlight_ir::ContainerRef::Entity(entity) => RelationEndpoint::Entity(entity),
        };
        assert_eq!(containers[0].subject, owner);
        assert_eq!(containers[0].provenance, parser.id);
        let clause = containers[0].evidence.source.as_ref().unwrap();
        assert_exact_parameter_source(service, receipt, source, clause, start..end);
        let token = if written.starts_with('[') {
            format!("[{name}]")
        } else {
            name.to_owned()
        };
        assert!(written.starts_with(&token));
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: constant.id,
                        }
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + token.len()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(
            definitions[0].enclosing,
            document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: notify.id }
                })
                .unwrap()
                .enclosing
        );
        assert_eq!(definitions[0].provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &definitions[0].source,
            start..start + token.len(),
        );
        let (prefix, initializer) = written.split_once(" = ").unwrap();
        let initializer_start = start + prefix.len() + " = ".len();
        let initializer_ref = SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                clause.span().file(),
                u64::try_from(initializer_start).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap(),
            content_hash(source.as_bytes()),
            None,
        );
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &initializer_ref,
            initializer_start..end,
        );
        let initializer_sites = document
            .occurrences
            .iter()
            .filter(|site| site.source.span() == initializer_ref.span())
            .collect::<Vec<_>>();
        if initializer.starts_with('"') {
            assert_eq!(initializer_sites.len(), 1);
            assert_eq!(initializer_sites[0].role, OccurrenceRole::StringEvidence);
            assert_eq!(initializer_sites[0].provenance, parser.id);
        } else {
            // Numeric and Boolean syntax does not manufacture evaluated-value
            // occurrences or a runtime type identity for the constant.
            assert!(initializer_sites.is_empty());
        }
        if let Some((type_prefix, authored)) = prefix.split_once(" As ") {
            let type_start = start + type_prefix.len() + " As ".len();
            let reference = SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    clause.span().file(),
                    u64::try_from(type_start).unwrap(),
                    u64::try_from(type_start + authored.len()).unwrap(),
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
                type_start..type_start + authored.len(),
            );
        } else {
            let within_clause = |reference: &SourceRef| {
                reference.repository() == clause.repository()
                    && reference.generation() == clause.generation()
                    && reference.content_hash() == clause.content_hash()
                    && reference.span().file() == clause.span().file()
                    && reference.span().start_byte() >= clause.span().start_byte()
                    && reference.span().end_byte() <= clause.span().end_byte()
            };
            assert!(!document.occurrences.iter().any(|site| {
                within_clause(&site.source) && site.role == OccurrenceRole::TypeUse
            }));
            assert!(!document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::UsesType
                    && (relation.subject == RelationEndpoint::Entity(constant.id)
                        || relation.evidence.source.as_ref().is_some_and(within_clause))
            }));
            assert!(!document.entities.iter().any(|entity| {
                entity.canonical_name.eq_ignore_ascii_case("Variant")
                    && entity.evidence.source.as_ref().is_some_and(within_clause)
            }));
        }
    }
    let expected = state.bound_constant.map(|written| {
        let start = u64::try_from(source.find(written).unwrap()).unwrap();
        *module_constants
            .iter()
            .find(|entity| {
                entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|reference| reference.span().start_byte() == start)
            })
            .unwrap()
    });
    let defaults = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "vba.default_constant_identifier.reference")
        .collect::<Vec<_>>();
    assert_eq!(
        defaults.len(),
        state
            .call
            .parameters
            .iter()
            .filter(|(written, _)| written.contains(" = "))
            .count()
    );
    for default in defaults {
        assert_eq!(default.role, OccurrenceRole::Reference);
        let formal = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(notify.id))
                    && entity.evidence.source.as_ref().is_some_and(|clause| {
                        clause.span().file() == default.source.span().file()
                            && clause.span().start_byte() <= default.source.span().start_byte()
                            && clause.span().end_byte() >= default.source.span().end_byte()
                    })
            })
            .unwrap();
        assert_eq!(default.enclosing, Some(formal.id));
        assert_eq!(default.provenance, parser.id);
        let refers = document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Occurrence(default.id)
                    && relation.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        if let Some(constant) = expected {
            assert_eq!(
                default.target,
                OccurrenceTarget::Resolved {
                    symbol: constant.id
                }
            );
            assert_eq!(refers.len(), 1);
            assert_eq!(refers[0].object, RelationEndpoint::Entity(constant.id));
            assert_eq!(refers[0].provenance, parser.id);
            assert_eq!(refers[0].evidence.source.as_ref(), Some(&default.source));
        } else {
            assert!(matches!(
                default.target,
                OccurrenceTarget::Unresolved { .. }
            ));
            assert!(refers.is_empty());
        }
        let gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.source == default.source
                    && gap.detail == "vba-default-constant-target-unavailable"
            })
            .collect::<Vec<_>>();
        assert_eq!(gaps.len(), usize::from(expected.is_none()));
        for gap in gaps {
            assert_eq!(gap.repository, receipt.repository);
            assert_eq!(gap.generation, receipt.generation);
            assert_eq!(gap.provenance, parser.id);
            assert_eq!(gap.evidence.source.as_ref(), Some(&default.source));
        }
        assert!(!document.relations.iter().any(|relation| {
            relation.subject == RelationEndpoint::Occurrence(default.id)
                && relation.predicate == RelationPredicate::Calls
        }));
    }
    let local_constants = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Constant
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(notify.id))
        })
        .collect::<Vec<_>>();
    let runtime_target = match state.shadow {
        NamedDefaultShadow::None => {
            assert!(local_constants.is_empty());
            None
        }
        NamedDefaultShadow::Parameter => {
            assert!(local_constants.is_empty());
            let target = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.canonical_name == "Fallback"
                        && entity.container == Some(rootlight_ir::ContainerRef::Entity(notify.id))
                })
                .unwrap();
            Some((target, "vba.let_identifier.reference", "Fallback = 1", 0))
        }
        NamedDefaultShadow::LocalConstant => {
            assert_eq!(local_constants.len(), 1);
            let target = local_constants[0];
            assert_eq!(target.canonical_name, "Fallback");
            assert_eq!(target.provenance, parser.id);
            let written = "Fallback As Long = 9";
            let start = source.find(written).unwrap();
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                target.evidence.source.as_ref().unwrap(),
                start..start + written.len(),
            );
            let definition = document
                .occurrences
                .iter()
                .find(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: target.id }
                })
                .unwrap();
            assert_eq!(definition.enclosing, Some(notify.id));
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                &definition.source,
                start..start + "Fallback".len(),
            );
            Some((
                target,
                "vba.value_identifier.reference",
                "value = Fallback",
                "value = ".len(),
            ))
        }
    };
    if let Some((target, syntax, statement, offset)) = runtime_target {
        let start = source.find(statement).unwrap() + offset;
        let uses = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == syntax
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(start + "Fallback".len()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(uses.len(), 1);
        assert_eq!(
            uses[0].target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(uses[0].enclosing, Some(notify.id));
        assert_eq!(uses[0].provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &uses[0].source,
            start..start + "Fallback".len(),
        );
        if let Some(constant) = expected {
            assert_ne!(constant.id, target.id);
        }
    }
    for name in ["Fallback", "Chosen"] {
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                20,
                0,
                &deadline(),
            )
            .unwrap_or_else(|error| {
                let lease = service
                    .generation_lease(receipt.generation, &deadline())
                    .unwrap();
                let query = lease.query().unwrap();
                let budget = FirstSliceBudget::default();
                let plan = query
                    .plan_code_locate(
                        name.to_owned(),
                        LocateMode::Exact,
                        20,
                        0,
                        budget.search(),
                        budget.query(),
                    )
                    .unwrap();
                let typed = query.execute_code_locate(&plan, &deadline());
                panic!("named default locate {name}: {error:?}; typed={typed:?}; state={state:?}")
            });
        let mut actual = located
            .data
            .hits
            .iter()
            .filter_map(|hit| hit.symbol)
            .collect::<Vec<_>>();
        let mut expected = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.evidence.source.is_some())
            .map(|entity| entity.id)
            .collect::<Vec<_>>();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
        for (result_index, hit) in located
            .data
            .hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| hit.symbol.is_some())
        {
            let entity = document
                .entities
                .iter()
                .find(|entity| Some(entity.id) == hit.symbol)
                .unwrap();
            assert_eq!(hit.source.as_ref(), entity.evidence.source.as_ref());
            let definitions = document
                .occurrences
                .iter()
                .filter(|occurrence| {
                    occurrence.role == OccurrenceRole::Definition
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .collect::<Vec<_>>();
            let assessment = located
                .data
                .conditional_activity
                .iter()
                .find(|assessment| {
                    usize::try_from(assessment.result_index).unwrap() == result_index
                })
                .unwrap();
            assert_eq!(assessment.basis, ConditionalActivityBasis::DefinitionToken);
            if definitions.len() > 1 {
                assert_eq!(assessment.activity, ConditionalActivityState::Unproven);
                assert_eq!(
                    assessment.reason,
                    Some(ConditionalActivityReason::AmbiguousDefinition)
                );
                assert!(assessment.assessed_source_ref.is_none());
                assert!(assessment.inventory.is_none());
            } else {
                assert_eq!(definitions.len(), 1);
                assert_eq!(assessment.activity, ConditionalActivityState::Active);
                assert!(assessment.reason.is_none());
                assert_eq!(
                    assessment.assessed_source_ref.as_ref(),
                    Some(&definitions[0].source)
                );
                assert_eq!(
                    assessment.inventory.as_ref().unwrap().parser_provenance_id,
                    parser.id
                );
            }
        }
    }
}

#[test]
fn named_optional_defaults_preserve_module_constant_namespace_and_source_lifetimes() {
    let initial = NamedDefaultState {
        module_constants: "Private Const Fallback As Long = 7\n",
        declarations: &[("Fallback As Long = 7", "Fallback")],
        call: OptionalCallState {
            parameters: &[
                ("ByVal value As Long", "value"),
                ("Optional extra As Long = Fallback", "extra"),
            ],
            arguments: "1",
            application_proven: true,
        },
        body: "",
        bound_constant: Some("Fallback As Long = 7"),
        shadow: NamedDefaultShadow::None,
    };
    let untyped = NamedDefaultState {
        module_constants: "Private Const Fallback = 8\n",
        declarations: &[("Fallback = 8", "Fallback")],
        bound_constant: Some("Fallback = 8"),
        ..initial
    };
    let variant = NamedDefaultState {
        module_constants: "Private Const Fallback As Variant = 8\n",
        declarations: &[("Fallback As Variant = 8", "Fallback")],
        bound_constant: Some("Fallback As Variant = 8"),
        ..initial
    };
    let renamed = NamedDefaultState {
        module_constants: "Public Const Chosen As Variant = 8\n",
        declarations: &[("Chosen As Variant = 8", "Chosen")],
        bound_constant: None,
        ..initial
    };
    let local_body = "Const Fallback As Long = 9\nvalue = Fallback\n";
    let states = [
        initial,
        NamedDefaultState {
            module_constants: "Private Const Fallback As Long = 8\n",
            declarations: &[("Fallback As Long = 8", "Fallback")],
            bound_constant: Some("Fallback As Long = 8"),
            ..initial
        },
        untyped,
        variant,
        NamedDefaultState {
            module_constants: "Public Const Fallback As Variant = 8\n",
            ..variant
        },
        renamed,
        NamedDefaultState {
            call: OptionalCallState {
                parameters: &[
                    ("ByVal value As Long", "value"),
                    ("Optional extra As Long = [Chosen]", "extra"),
                ],
                ..initial.call
            },
            bound_constant: Some("Chosen As Variant = 8"),
            ..renamed
        },
        NamedDefaultState {
            module_constants: "Const Fallback = True\n",
            declarations: &[("Fallback = True", "Fallback")],
            call: OptionalCallState {
                parameters: &[
                    ("ByVal value As Long", "value"),
                    ("Optional extra = Fallback", "extra"),
                ],
                arguments: "1, False",
                application_proven: true,
            },
            bound_constant: Some("Fallback = True"),
            ..initial
        },
        NamedDefaultState {
            module_constants: "Public Const Fallback As String = \"text\"\n",
            declarations: &[("Fallback As String = \"text\"", "Fallback")],
            call: OptionalCallState {
                parameters: &[
                    ("ByVal value As Long", "value"),
                    ("Optional extra As String = Fallback", "extra"),
                    ("Optional renamed = [FALLBACK]", "renamed"),
                ],
                ..initial.call
            },
            bound_constant: Some("Fallback As String = \"text\""),
            ..initial
        },
        NamedDefaultState {
            call: OptionalCallState {
                parameters: &[("Optional ByVal Fallback As Long = [FALLBACK]", "Fallback")],
                arguments: "",
                application_proven: true,
            },
            body: "Fallback = 1\n",
            shadow: NamedDefaultShadow::Parameter,
            ..initial
        },
        NamedDefaultState {
            module_constants: "",
            declarations: &[],
            bound_constant: None,
            call: OptionalCallState {
                parameters: &[("Optional ByVal Fallback As Long = Fallback", "Fallback")],
                arguments: "",
                application_proven: true,
            },
            body: "Fallback = 1\n",
            shadow: NamedDefaultShadow::Parameter,
        },
        NamedDefaultState {
            body: local_body,
            shadow: NamedDefaultShadow::LocalConstant,
            ..initial
        },
        NamedDefaultState {
            module_constants: "",
            declarations: &[],
            body: local_body,
            bound_constant: None,
            shadow: NamedDefaultShadow::LocalConstant,
            ..initial
        },
        NamedDefaultState {
            module_constants: "Private Const Fallback As Long = 7\nPrivate Const Fallback As Long = 8\n",
            declarations: &[
                ("Fallback As Long = 7", "Fallback"),
                ("Fallback As Long = 8", "Fallback"),
            ],
            bound_constant: None,
            ..initial
        },
        NamedDefaultState {
            call: OptionalCallState {
                arguments: "",
                application_proven: false,
                ..initial.call
            },
            ..initial
        },
        NamedDefaultState {
            call: OptionalCallState {
                arguments: "1, 2, 3",
                application_proven: false,
                ..initial.call
            },
            ..initial
        },
        // An unsupported own-body expression selects the native fallback. The
        // composite parser identity alone is not evidence of which route ran.
        NamedDefaultState {
            body: "value = 1 + 2\n",
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
        let mut previous: Option<(FirstSliceIndexReceipt, String, NamedDefaultState)> = None;
        let mut initial_source = None;
        let mut initial_logical = None;
        for (position, state) in states.into_iter().enumerate() {
            let header = state
                .call
                .parameters
                .iter()
                .map(|(written, _)| *written)
                .collect::<Vec<_>>()
                .join(", ");
            let source = format!(
                "Attribute VB_Name = \"Authored\"\nOption Explicit\n{}Sub Notify({header})\n{}End Sub\nSub Run()\nCall Notify({})\nEnd Sub\n",
                state.module_constants, state.body, state.call.arguments,
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
            assert_named_default_constant_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                let isolated_edit = match position {
                    1 => Some(
                        old_source
                            .replace("Const Fallback As Long = 7", "Const Fallback As Long = 8"),
                    ),
                    2 => {
                        Some(old_source.replace("Const Fallback As Long = 8", "Const Fallback = 8"))
                    }
                    3 => Some(
                        old_source.replace("Const Fallback = 8", "Const Fallback As Variant = 8"),
                    ),
                    4 => Some(old_source.replace("Private Const", "Public Const")),
                    5 => Some(
                        old_source.replace("Const Fallback As Variant", "Const Chosen As Variant"),
                    ),
                    6 => Some(old_source.replace("= Fallback)", "= [Chosen])")),
                    _ => None,
                };
                if let Some(expected) = isolated_edit {
                    assert_eq!(source, expected);
                    assert!(receipt.logical_snapshot.is_some());
                    assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                }
                assert_named_default_constant_contract(
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
            assert_named_default_constant_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_named_default_constant_contract(
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
            assert_named_default_constant_contract(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if position + 1 == states.len() {
                assert_eq!(initial_source.as_ref(), Some(&source));
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            // The latest rebuilt predecessor is checked before a new rebuild
            // can legitimately evict it under the unchanged retention2 policy.
            previous = Some((rebuilt, source, state));
        }
    }
}
