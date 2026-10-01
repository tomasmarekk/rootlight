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
    let native = "Attribute VB_Name = \"Authored\"\nOption Explicit\nSub Run()\nDim value As Long\nvalue = &H1\nEnd Sub\n";
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
            body: "value = &H1\n",
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

#[derive(Debug, Clone, Copy)]
enum ModuleVariableShadow {
    None,
    Parameter,
    Local,
}

#[derive(Debug, Clone, Copy)]
struct ModuleVariableState {
    declarations_source: &'static str,
    declarations: WrittenParameters<'static>,
    runtime_name: &'static str,
    call: OptionalCallState,
    notify_body: &'static str,
    shadow: ModuleVariableShadow,
    arithmetic_body: bool,
}

fn assert_module_variable_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: ModuleVariableState,
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
    let run = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Run")
        .unwrap();
    let parser = document
        .provenance
        .iter()
        .find(|proof| proof.id == notify.provenance)
        .unwrap();
    let variables = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Variable
                && entity.container == notify.container
                && entity.evidence.source.is_some()
        })
        .collect::<Vec<_>>();
    let names = state
        .declarations
        .iter()
        .map(|(_, name)| *name)
        .collect::<BTreeSet<_>>();
    assert_eq!(variables.len(), names.len());
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && variables.iter().any(|variable| {
                        site.target
                            == OccurrenceTarget::Resolved {
                                symbol: variable.id,
                            }
                    })
            })
            .count(),
        state.declarations.len()
    );
    for &(written, name) in state.declarations {
        let variable = variables
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(variable.provenance, parser.id);
        assert_eq!(variable.language, language);
        let start = source.find(written).unwrap();
        let end = start + written.len();
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
            variable.evidence.source.as_ref().unwrap(),
            representative.0..representative.1,
        );
        let container = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(variable.id)
                    && relation.evidence.source.as_ref().is_some_and(|reference| {
                        reference.span().start_byte() == u64::try_from(start).unwrap()
                            && reference.span().end_byte() == u64::try_from(end).unwrap()
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(container.len(), 1);
        let owner = match variable.container.unwrap() {
            rootlight_ir::ContainerRef::Repository(repository) => {
                RelationEndpoint::Repository(repository)
            }
            rootlight_ir::ContainerRef::File(file) => RelationEndpoint::File(file),
            rootlight_ir::ContainerRef::Entity(entity) => RelationEndpoint::Entity(entity),
        };
        assert_eq!(container[0].subject, owner);
        assert_eq!(container[0].provenance, parser.id);
        let clause = container[0].evidence.source.as_ref().unwrap();
        assert_exact_parameter_source(service, receipt, source, clause, start..end);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: variable.id,
                        }
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + name.len()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].provenance, parser.id);
        assert_eq!(definitions[0].evidence.source.as_ref(), Some(clause));
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &definitions[0].source,
            start..start + name.len(),
        );
        assert_eq!(
            definitions[0].enclosing,
            match variable.container.unwrap() {
                rootlight_ir::ContainerRef::Entity(entity) => Some(entity),
                rootlight_ir::ContainerRef::Repository(_) | rootlight_ir::ContainerRef::File(_) =>
                    None,
            }
        );
        let visibility = if state.declarations_source.starts_with("Public ") {
            rootlight_ir::EntityVisibility::Public
        } else if state.declarations_source.starts_with("Private ") {
            rootlight_ir::EntityVisibility::Private
        } else {
            rootlight_ir::EntityVisibility::Unknown
        };
        assert_eq!(variable.visibility, visibility);
        if let Some((prefix, authored_type)) = written.split_once(" As ") {
            let type_start = start + prefix.len() + " As ".len();
            let reference = SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    clause.span().file(),
                    u64::try_from(type_start).unwrap(),
                    u64::try_from(end).unwrap(),
                )
                .unwrap(),
                content_hash(source.as_bytes()),
                None,
            );
            assert_eq!(&source[type_start..end], authored_type);
            assert_exact_parameter_source(service, receipt, source, &reference, type_start..end);
        } else {
            let within = |reference: &SourceRef| {
                reference.repository() == clause.repository()
                    && reference.generation() == clause.generation()
                    && reference.content_hash() == clause.content_hash()
                    && reference.span().file() == clause.span().file()
                    && reference.span().start_byte() >= clause.span().start_byte()
                    && reference.span().end_byte() <= clause.span().end_byte()
            };
            assert!(
                !document
                    .occurrences
                    .iter()
                    .any(|site| within(&site.source) && site.role == OccurrenceRole::TypeUse)
            );
            assert!(
                !document
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::UsesType
                        && (relation.subject == RelationEndpoint::Entity(variable.id)
                            || relation.evidence.source.as_ref().is_some_and(within)))
            );
            assert!(!document.entities.iter().any(|entity| {
                entity.canonical_name.eq_ignore_ascii_case("Variant")
                    && entity.evidence.source.as_ref().is_some_and(within)
            }));
        }
    }
    let defaults = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "vba.default_constant_identifier.reference")
        .collect::<Vec<_>>();
    assert_eq!(defaults.len(), 1);
    let default = defaults[0];
    assert!(matches!(
        default.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(default.provenance, parser.id);
    assert!(!document.relations.iter().any(|relation| relation.subject
        == RelationEndpoint::Occurrence(default.id)
        && relation.predicate == RelationPredicate::RefersTo));
    let gaps = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.source == default.source && gap.detail == "vba-default-constant-target-unavailable"
        })
        .collect::<Vec<_>>();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].provenance, parser.id);
    assert_eq!(gaps[0].evidence.source.as_ref(), Some(&default.source));
    let unique_module_target = if state
        .declarations
        .iter()
        .filter(|(_, name)| *name == state.runtime_name)
        .count()
        == 1
    {
        Some(
            variables
                .iter()
                .find(|entity| entity.canonical_name == state.runtime_name)
                .unwrap()
                .id,
        )
    } else {
        None
    };
    let read_start = source
        .find(&format!("sink = {}", state.runtime_name))
        .unwrap()
        + "sink = ".len();
    let call = format!("Call {}()", state.runtime_name);
    let call_start = source.find(&call).unwrap();
    for (syntax, start, expected_role) in [
        (
            "vba.value_identifier.reference",
            read_start,
            OccurrenceRole::Reference,
        ),
        (
            "vba.invoked_identifier.reference",
            call_start + "Call ".len(),
            OccurrenceRole::Reference,
        ),
    ] {
        let uses = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == syntax
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(start + state.runtime_name.len()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(uses.len(), 1);
        let site = uses[0];
        assert_eq!(site.role, expected_role);
        assert_eq!(site.enclosing, Some(run.id));
        assert_eq!(site.provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &site.source,
            start..start + state.runtime_name.len(),
        );
        if let Some(symbol) = unique_module_target {
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol });
        } else {
            assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        }
        let refers = document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Occurrence(site.id)
                    && relation.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        assert_eq!(refers.len(), usize::from(unique_module_target.is_some()));
        if let Some(symbol) = unique_module_target {
            assert_eq!(refers[0].object, RelationEndpoint::Entity(symbol));
            assert_eq!(refers[0].provenance, parser.id);
            assert_eq!(refers[0].evidence.source.as_ref(), Some(&site.source));
        }
        assert!(!document.relations.iter().any(|relation| relation.subject
            == RelationEndpoint::Occurrence(site.id)
            && relation.predicate == RelationPredicate::Calls));
    }
    // A value's callee spelling can retain identity without admitting it as a
    // callable. Written Optional syntax also does not turn that value into Const.
    let application = document
        .occurrences
        .iter()
        .find(|site| {
            site.syntax_kind == "vba.application.reference"
                && site.source.span().start_byte() == u64::try_from(call_start).unwrap()
                && site.source.span().end_byte() == u64::try_from(call_start + call.len()).unwrap()
        })
        .unwrap();
    assert!(matches!(
        application.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(application.provenance, parser.id);
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        &application.source,
        call_start..call_start + call.len(),
    );
    assert!(!document.relations.iter().any(|relation| relation.subject
        == RelationEndpoint::Occurrence(application.id)
        && matches!(
            relation.predicate,
            RelationPredicate::RefersTo | RelationPredicate::Calls
        )));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.source == application.source
                && gap.detail == "vba-call-or-index-target-unavailable")
    );
    let local_variables = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Variable
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(notify.id))
        })
        .collect::<Vec<_>>();
    let shadow = match state.shadow {
        ModuleVariableShadow::None => {
            assert!(local_variables.is_empty());
            None
        }
        ModuleVariableShadow::Parameter => {
            assert!(local_variables.is_empty());
            let target = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.canonical_name == state.runtime_name
                        && entity.container == Some(rootlight_ir::ContainerRef::Entity(notify.id))
                })
                .unwrap();
            let marker = format!("{} = 1", state.runtime_name);
            Some((
                target,
                "vba.let_identifier.reference",
                source.find(&marker).unwrap(),
            ))
        }
        ModuleVariableShadow::Local => {
            assert_eq!(local_variables.len(), 1);
            let target = local_variables[0];
            assert_eq!(target.canonical_name, state.runtime_name);
            assert_eq!(target.provenance, parser.id);
            let clause = format!("{} As Long", state.runtime_name);
            let start = source.find(&format!("Dim {clause}")).unwrap() + "Dim ".len();
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                target.evidence.source.as_ref().unwrap(),
                start..start + clause.len(),
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
            assert_eq!(definition.provenance, parser.id);
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                &definition.source,
                start..start + state.runtime_name.len(),
            );
            let marker = format!("value = {}", state.runtime_name);
            Some((
                target,
                "vba.value_identifier.reference",
                source.find(&marker).unwrap() + "value = ".len(),
            ))
        }
    };
    if let Some((target, syntax, start)) = shadow {
        let site = document
            .occurrences
            .iter()
            .find(|site| {
                site.syntax_kind == syntax
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(start + state.runtime_name.len()).unwrap()
            })
            .unwrap();
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(site.enclosing, Some(notify.id));
        assert_eq!(site.provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &site.source,
            start..start + state.runtime_name.len(),
        );
        assert_ne!(Some(target.id), unique_module_target);
        let refers = document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Occurrence(site.id)
                    && relation.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        assert_eq!(refers.len(), 1);
        assert_eq!(refers[0].object, RelationEndpoint::Entity(target.id));
        assert_eq!(refers[0].provenance, parser.id);
        assert_eq!(refers[0].evidence.source.as_ref(), Some(&site.source));
        assert!(!document.relations.iter().any(|relation| relation.subject
            == RelationEndpoint::Occurrence(site.id)
            && relation.predicate == RelationPredicate::Calls));
    }
    for name in ["Storage", "Buffer"] {
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                20,
                0,
                &deadline(),
            )
            .unwrap();
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
        for (ordinal, hit) in located
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
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .collect::<Vec<_>>();
            let assessment = located
                .data
                .conditional_activity
                .iter()
                .find(|assessment| assessment.result_index == u32::try_from(ordinal).unwrap())
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
                assert_eq!(assessment.reason, None);
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
fn module_variable_edits_preserve_value_binding_without_constant_or_call_claims() {
    let initial = ModuleVariableState {
        declarations_source: "Private Storage As Long\n",
        declarations: &[("Storage As Long", "Storage")],
        runtime_name: "Storage",
        call: OptionalCallState {
            parameters: &[
                ("ByVal value As Long", "value"),
                ("Optional extra As Long = Storage", "extra"),
            ],
            arguments: "1",
            application_proven: true,
        },
        notify_body: "",
        shadow: ModuleVariableShadow::None,
        arithmetic_body: false,
    };
    let untyped = ModuleVariableState {
        declarations_source: "Private Storage\n",
        declarations: &[("Storage", "Storage")],
        ..initial
    };
    let variant = ModuleVariableState {
        declarations_source: "Private Storage As Variant\n",
        declarations: &[("Storage As Variant", "Storage")],
        ..initial
    };
    let renamed = ModuleVariableState {
        declarations_source: "Dim Buffer As Variant\n",
        declarations: &[("Buffer As Variant", "Buffer")],
        ..initial
    };
    let new_default = ModuleVariableState {
        call: OptionalCallState {
            parameters: &[
                ("ByVal value As Long", "value"),
                ("Optional extra As Long = Buffer", "extra"),
            ],
            ..initial.call
        },
        ..renamed
    };
    let renamed_uses = ModuleVariableState {
        runtime_name: "Buffer",
        ..new_default
    };
    let states = [
        initial,
        untyped,
        variant,
        ModuleVariableState {
            declarations_source: "Public Storage As Variant\n",
            ..variant
        },
        ModuleVariableState {
            declarations_source: "Dim Storage As Variant\n",
            ..variant
        },
        renamed,
        new_default,
        renamed_uses,
        ModuleVariableState {
            call: OptionalCallState {
                parameters: &[
                    ("ByVal Buffer As Long", "Buffer"),
                    ("Optional extra As Long = Buffer", "extra"),
                ],
                ..initial.call
            },
            notify_body: "Buffer = 1\n",
            shadow: ModuleVariableShadow::Parameter,
            ..renamed_uses
        },
        ModuleVariableState {
            notify_body: "Dim Buffer As Long\nvalue = Buffer\n",
            shadow: ModuleVariableShadow::Local,
            ..renamed_uses
        },
        ModuleVariableState {
            declarations_source: "Dim Buffer As Variant\nDim Buffer As Long\n",
            declarations: &[
                ("Buffer As Variant", "Buffer"),
                ("Buffer As Long", "Buffer"),
            ],
            ..renamed_uses
        },
        ModuleVariableState {
            arithmetic_body: true,
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
        let configuration = (language == "vba").then_some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n");
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, ModuleVariableState)> = None;
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
            let arithmetic = if state.arithmetic_body { " + &H1" } else { "" };
            let source = format!(
                "Attribute VB_Name = \"Authored\"\nOption Explicit\n{}Sub Notify({header})\n{}End Sub\nSub Run()\nDim sink As Long\nsink = {}{arithmetic}\nCall Notify({})\nCall {}()\nEnd Sub\n",
                state.declarations_source,
                state.notify_body,
                state.runtime_name,
                state.call.arguments,
                state.runtime_name
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
            assert_module_variable_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                let isolated = match position {
                    1 => Some(old_source.replace("Private Storage As Long\n", "Private Storage\n")),
                    2 => Some(
                        old_source.replace("Private Storage\n", "Private Storage As Variant\n"),
                    ),
                    3 => Some(old_source.replace(
                        "Private Storage As Variant\n",
                        "Public Storage As Variant\n",
                    )),
                    4 => Some(
                        old_source
                            .replace("Public Storage As Variant\n", "Dim Storage As Variant\n"),
                    ),
                    5 => Some(
                        old_source.replace("Dim Storage As Variant\n", "Dim Buffer As Variant\n"),
                    ),
                    6 => Some(old_source.replace("= Storage)", "= Buffer)")),
                    _ => None,
                };
                if let Some(expected) = isolated {
                    assert_eq!(source, expected);
                    assert!(receipt.logical_snapshot.is_some());
                    assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                }
                assert_module_variable_contract(
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
            assert_module_variable_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_module_variable_contract(
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
            assert_module_variable_contract(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if position + 1 == states.len() {
                assert_eq!(initial_source.as_ref(), Some(&source));
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            // Keep the latest published predecessor, and check it before the
            // next rebuild can evict it under the unchanged two-generation policy.
            previous = Some((rebuilt, source, state));
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ModuleArrayState {
    declarations_source: &'static str,
    declarations: WrittenParameters<'static>,
    runtime_name: &'static str,
    indexed_body: bool,
}

fn module_array_call(runtime_name: &'static str) -> OptionalCallState {
    let parameters: WrittenParameters<'static> = match runtime_name {
        "Storage" => &[
            ("ByVal value As Long", "value"),
            ("Optional extra As Long = Storage", "extra"),
        ],
        "Buffer" => &[
            ("ByVal value As Long", "value"),
            ("Optional extra As Long = Buffer", "extra"),
        ],
        _ => panic!("an authored module-array name is required"),
    };
    OptionalCallState {
        parameters,
        arguments: "1",
        application_proven: true,
    }
}

fn module_array_declaration_ranges(
    source: &str,
    state: ModuleArrayState,
) -> Vec<std::ops::Range<usize>> {
    let block_start = source.find(state.declarations_source).unwrap();
    let mut cursor = 0;
    state
        .declarations
        .iter()
        .map(|(written, _)| {
            let offset = cursor + state.declarations_source[cursor..].find(written).unwrap();
            cursor = offset + written.len();
            block_start + offset..block_start + cursor
        })
        .collect()
}

fn assert_module_array_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: ModuleArrayState,
) {
    assert_authored_parameter_types(
        service,
        receipt,
        source,
        language,
        module_array_call(state.runtime_name),
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
    let parser = document
        .provenance
        .iter()
        .find(|proof| proof.id == notify.provenance)
        .unwrap();
    let variables = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Variable
                && entity.container == notify.container
                && entity.evidence.source.is_some()
        })
        .collect::<Vec<_>>();
    let names = state
        .declarations
        .iter()
        .map(|(_, name)| *name)
        .collect::<BTreeSet<_>>();
    assert_eq!(variables.len(), names.len());
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && variables.iter().any(|variable| {
                        site.target
                            == OccurrenceTarget::Resolved {
                                symbol: variable.id,
                            }
                    })
            })
            .count(),
        state.declarations.len()
    );
    let declaration_ranges = module_array_declaration_ranges(source, state);
    for (&(written, name), range) in state.declarations.iter().zip(&declaration_ranges) {
        let variable = variables
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(variable.provenance, parser.id);
        assert_eq!(variable.language, language);
        let start = range.start;
        let end = range.end;
        assert_eq!(&source[range.clone()], written);
        let name_length = if written.starts_with('[') {
            written.find(']').unwrap() + 1
        } else {
            written
                .bytes()
                .take_while(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                .count()
        };
        let authored_name = &written[..name_length];
        assert!(
            authored_name
                .trim_start_matches('[')
                .trim_end_matches(']')
                .eq_ignore_ascii_case(name)
        );
        let representative = state
            .declarations
            .iter()
            .zip(&declaration_ranges)
            .filter(|((_, candidate), _)| *candidate == name)
            .map(|(_, range)| range.clone())
            .min_by_key(|range| (range.start, range.end))
            .unwrap();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            variable.evidence.source.as_ref().unwrap(),
            representative,
        );
        let contains = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(variable.id)
                    && relation.evidence.source.as_ref().is_some_and(|reference| {
                        reference.span().start_byte() == u64::try_from(start).unwrap()
                            && reference.span().end_byte() == u64::try_from(end).unwrap()
                    })
            })
            .collect::<Vec<_>>();
        assert_eq!(contains.len(), 1, "written array clause: {written}");
        let owner = match variable.container.unwrap() {
            rootlight_ir::ContainerRef::Repository(repository) => {
                RelationEndpoint::Repository(repository)
            }
            rootlight_ir::ContainerRef::File(file) => RelationEndpoint::File(file),
            rootlight_ir::ContainerRef::Entity(entity) => RelationEndpoint::Entity(entity),
        };
        assert_eq!(contains[0].subject, owner);
        assert_eq!(contains[0].provenance, parser.id);
        let clause = contains[0].evidence.source.as_ref().unwrap();
        assert_exact_parameter_source(service, receipt, source, clause, start..end);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target
                        == OccurrenceTarget::Resolved {
                            symbol: variable.id,
                        }
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + name_length).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        assert_eq!(definitions[0].provenance, parser.id);
        assert_eq!(definitions[0].evidence.source.as_ref(), Some(clause));
        assert_eq!(
            definitions[0].enclosing,
            match variable.container.unwrap() {
                rootlight_ir::ContainerRef::Entity(entity) => Some(entity),
                rootlight_ir::ContainerRef::Repository(_) | rootlight_ir::ContainerRef::File(_) =>
                    None,
            }
        );
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &definitions[0].source,
            start..start + name_length,
        );
        let line_start = source[..start].rfind('\n').map_or(0, |offset| offset + 1);
        let prefix = ["Private ", "Public ", "Dim "]
            .into_iter()
            .find(|prefix| source[line_start..].starts_with(*prefix))
            .unwrap();
        let prefix_end = line_start + prefix.len();
        assert!(prefix_end <= start);
        let visibility = match prefix {
            "Private " => rootlight_ir::EntityVisibility::Private,
            "Public " => rootlight_ir::EntityVisibility::Public,
            "Dim " => rootlight_ir::EntityVisibility::Unknown,
            _ => panic!("an authored module variable prefix is required"),
        };
        assert_eq!(variable.visibility, visibility);
        let reference = |range: std::ops::Range<usize>| {
            SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    clause.span().file(),
                    u64::try_from(range.start).unwrap(),
                    u64::try_from(range.end).unwrap(),
                )
                .unwrap(),
                content_hash(source.as_bytes()),
                None,
            )
        };
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &reference(line_start..prefix_end),
            line_start..prefix_end,
        );
        if let Some(bounds_start) = written.find('(') {
            let bounds_end = written.find(')').unwrap() + 1;
            let range = start + bounds_start..start + bounds_end;
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                &reference(range.clone()),
                range,
            );
        }
        if let Some((prefix, authored_type)) = written.split_once(" As ") {
            let range = start + prefix.len() + " As ".len()..end;
            assert_eq!(&source[range.clone()], authored_type);
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                &reference(range.clone()),
                range,
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
                    && (relation.subject == RelationEndpoint::Entity(variable.id)
                        || relation.evidence.source.as_ref().is_some_and(within_clause))
            }));
            assert!(!document.entities.iter().any(|entity| {
                entity.canonical_name.eq_ignore_ascii_case("Variant")
                    && entity.evidence.source.as_ref().is_some_and(within_clause)
            }));
        }
    }
    let defaults = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "vba.default_constant_identifier.reference")
        .collect::<Vec<_>>();
    assert_eq!(defaults.len(), 1);
    let default = defaults[0];
    assert!(matches!(
        default.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    assert_eq!(default.provenance, parser.id);
    assert!(!document.relations.iter().any(|relation| {
        relation.subject == RelationEndpoint::Occurrence(default.id)
            && matches!(
                relation.predicate,
                RelationPredicate::RefersTo | RelationPredicate::Calls
            )
    }));
    let default_gaps = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.source == default.source && gap.detail == "vba-default-constant-target-unavailable"
        })
        .collect::<Vec<_>>();
    assert_eq!(default_gaps.len(), 1);
    assert_eq!(default_gaps[0].provenance, parser.id);
    assert_eq!(
        default_gaps[0].evidence.source.as_ref(),
        Some(&default.source)
    );
    let unique = state
        .declarations
        .iter()
        .filter(|(_, name)| *name == state.runtime_name)
        .count()
        == 1;
    let variable = variables
        .iter()
        .find(|entity| entity.canonical_name == state.runtime_name)
        .unwrap();
    let read_start = source
        .find(&format!("sink = {}\n", state.runtime_name))
        .unwrap()
        + "sink = ".len();
    let reads = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "vba.value_identifier.reference"
                && site.source.span().start_byte() == u64::try_from(read_start).unwrap()
                && site.source.span().end_byte()
                    == u64::try_from(read_start + state.runtime_name.len()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), 1);
    let read = reads[0];
    assert_eq!(read.role, OccurrenceRole::Reference);
    assert_eq!(read.enclosing, Some(run.id));
    assert_eq!(read.provenance, parser.id);
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        &read.source,
        read_start..read_start + state.runtime_name.len(),
    );
    if unique {
        assert_eq!(
            read.target,
            OccurrenceTarget::Resolved {
                symbol: variable.id
            }
        );
    } else {
        assert!(matches!(read.target, OccurrenceTarget::Unresolved { .. }));
    }
    let refers = document
        .relations
        .iter()
        .filter(|relation| {
            relation.subject == RelationEndpoint::Occurrence(read.id)
                && relation.predicate == RelationPredicate::RefersTo
        })
        .collect::<Vec<_>>();
    assert_eq!(refers.len(), usize::from(unique));
    if unique {
        assert_eq!(refers[0].object, RelationEndpoint::Entity(variable.id));
        assert_eq!(refers[0].provenance, parser.id);
        assert_eq!(refers[0].evidence.source.as_ref(), Some(&read.source));
    }
    assert!(!document.relations.iter().any(|relation| {
        relation.subject == RelationEndpoint::Occurrence(read.id)
            && relation.predicate == RelationPredicate::Calls
    }));
    for name in ["Storage", "Buffer", "Grid"] {
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                20,
                0,
                &deadline(),
            )
            .unwrap();
        let mut actual = located
            .data
            .hits
            .iter()
            .filter_map(|hit| hit.symbol)
            .collect::<Vec<_>>();
        let mut expected = variables
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .map(|entity| entity.id)
            .collect::<Vec<_>>();
        actual.sort_unstable();
        expected.sort_unstable();
        assert_eq!(actual, expected);
        for (ordinal, hit) in located
            .data
            .hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| hit.symbol.is_some())
        {
            let entity = variables
                .iter()
                .find(|entity| Some(entity.id) == hit.symbol)
                .unwrap();
            assert_eq!(hit.source.as_ref(), entity.evidence.source.as_ref());
            let definitions = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .collect::<Vec<_>>();
            let assessment = located
                .data
                .conditional_activity
                .iter()
                .find(|assessment| assessment.result_index == u32::try_from(ordinal).unwrap())
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
                assert_eq!(assessment.reason, None);
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
    if state.indexed_body {
        // The separately authored hexadecimal statement retains native fallback.
        // Declared rank proves indexing shape, not runtime bounds or element types.
        for (expression, name, proven) in [
            ("Buffer()", "Buffer", true),
            ("Buffer(0)", "Buffer", true),
            ("Buffer(0, 1)", "Buffer", false),
            ("Grid()", "Grid", true),
            ("Grid(0)", "Grid", false),
            ("Grid(0, 1)", "Grid", true),
        ] {
            let start = source.find(&format!("sink = {expression}\n")).unwrap() + "sink = ".len();
            let applications = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.syntax_kind == "vba.application.reference"
                        && site.source.span().start_byte() == u64::try_from(start).unwrap()
                        && site.source.span().end_byte()
                            == u64::try_from(start + expression.len()).unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(applications.len(), 1, "array application: {expression}");
            let application = applications[0];
            let array = variables
                .iter()
                .find(|entity| entity.canonical_name == name)
                .unwrap();
            assert_eq!(application.role, OccurrenceRole::Reference);
            assert_eq!(application.enclosing, Some(run.id));
            assert_eq!(application.provenance, parser.id);
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                &application.source,
                start..start + expression.len(),
            );
            if proven {
                assert_eq!(
                    application.target,
                    OccurrenceTarget::Resolved { symbol: array.id }
                );
            } else {
                assert!(matches!(
                    application.target,
                    OccurrenceTarget::Unresolved { .. }
                ));
            }
            let refers = document
                .relations
                .iter()
                .filter(|relation| {
                    relation.subject == RelationEndpoint::Occurrence(application.id)
                        && relation.predicate == RelationPredicate::RefersTo
                })
                .collect::<Vec<_>>();
            assert_eq!(refers.len(), usize::from(proven));
            if proven {
                assert_eq!(refers[0].object, RelationEndpoint::Entity(array.id));
                assert_eq!(refers[0].provenance, parser.id);
                assert_eq!(
                    refers[0].evidence.source.as_ref(),
                    Some(&application.source)
                );
            }
            assert!(!document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::Calls
                    && relation.evidence.source.as_ref().is_some_and(|reference| {
                        reference.span().start_byte() >= u64::try_from(start).unwrap()
                            && reference.span().end_byte()
                                <= u64::try_from(start + expression.len()).unwrap()
                    })
            }));
            assert_eq!(
                document.skipped_regions.iter().any(|gap| {
                    gap.source == application.source
                        && gap.detail == "vba-call-or-index-target-unavailable"
                }),
                !proven
            );
            assert_eq!(
                document
                    .provenance
                    .iter()
                    .find(|proof| proof.id == application.provenance)
                    .unwrap()
                    .rule,
                None
            );
        }
    } else {
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| { site.syntax_kind == "vba.application.reference" })
                .count(),
            1
        );
    }
}

#[test]
fn module_numeric_array_edits_preserve_source_identity_and_native_rank_contracts() {
    let initial = ModuleArrayState {
        declarations_source: "Private Storage As Long\n",
        declarations: &[("Storage As Long", "Storage")],
        runtime_name: "Storage",
        indexed_body: false,
    };
    let states = [
        initial,
        ModuleArrayState {
            declarations_source: "Private Storage() As Long\n",
            declarations: &[("Storage() As Long", "Storage")],
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Private Storage(2) As Long\n",
            declarations: &[("Storage(2) As Long", "Storage")],
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Private Storage(-2 To +3) As Long\n",
            declarations: &[("Storage(-2 To +3) As Long", "Storage")],
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Private Storage(-2 To +3, 0 To 4) As Long\n",
            declarations: &[("Storage(-2 To +3, 0 To 4) As Long", "Storage")],
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Dim Storage(-2 To +3, 0 To 4)\n",
            declarations: &[("Storage(-2 To +3, 0 To 4)", "Storage")],
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Dim Buffer(-2 To +3, 0 To 4)\n",
            declarations: &[("Buffer(-2 To +3, 0 To 4)", "Buffer")],
            runtime_name: "Buffer",
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Dim Buffer(2)\nDim Buffer(0 To 2, 1 To 3)\n",
            declarations: &[
                ("Buffer(2)", "Buffer"),
                ("Buffer(0 To 2, 1 To 3)", "Buffer"),
            ],
            runtime_name: "Buffer",
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Public Buffer(2) As Long\nPrivate Grid(-2 To +3, 0 To 4) As Long\n",
            declarations: &[
                ("Buffer(2) As Long", "Buffer"),
                ("Grid(-2 To +3, 0 To 4) As Long", "Grid"),
            ],
            runtime_name: "Buffer",
            indexed_body: true,
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
        let configuration = (language == "vba").then_some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n");
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, ModuleArrayState)> = None;
        let mut initial_source = None;
        let mut initial_logical = None;
        for (position, state) in states.into_iter().enumerate() {
            let call = module_array_call(state.runtime_name);
            let header = call
                .parameters
                .iter()
                .map(|(written, _)| *written)
                .collect::<Vec<_>>()
                .join(", ");
            let indexed = if state.indexed_body {
                "sink = Buffer()\nsink = Buffer(0)\nsink = Buffer(0, 1)\nsink = Grid()\nsink = Grid(0)\nsink = Grid(0, 1)\nsink = &H1\n"
            } else {
                ""
            };
            let source = format!(
                "Attribute VB_Name = \"Authored\"\nOption Explicit\n{}Sub Notify({header})\nEnd Sub\nSub Run()\nDim sink As Long\nsink = {}\n{indexed}Call Notify(1)\nEnd Sub\n",
                state.declarations_source, state.runtime_name
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
            assert_module_array_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                let expected =
                    match position {
                        1..=5 => old_source
                            .replace(old_state.declarations_source, state.declarations_source),
                        6 => old_source.replace("Storage", "Buffer"),
                        7 => old_source
                            .replace(old_state.declarations_source, state.declarations_source),
                        8 => old_source
                            .replace(old_state.declarations_source, state.declarations_source)
                            .replace("Call Notify(1)\n", &format!("{indexed}Call Notify(1)\n")),
                        9 => initial_source.clone().unwrap(),
                        _ => panic!("an authored array edit is required"),
                    };
                assert_eq!(source, expected);
                assert!(receipt.logical_snapshot.is_some());
                assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                assert_module_array_contract(
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
            assert_module_array_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_module_array_contract(
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
            assert_module_array_contract(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if position + 1 == states.len() {
                assert_eq!(initial_source.as_ref(), Some(&source));
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            // A rebuild is the latest retained predecessor; check it before
            // another publication legitimately evicts it under retention2.
            previous = Some((rebuilt, source, state));
        }
    }
}

fn grouped_module_variable_source(state: ModuleArrayState) -> String {
    let header = module_array_call(state.runtime_name)
        .parameters
        .iter()
        .map(|(written, _)| *written)
        .collect::<Vec<_>>()
        .join(", ");
    let indexed = if state.indexed_body {
        "sink = Buffer()\nsink = Buffer(0)\nsink = Buffer(0, 1)\nsink = Grid()\nsink = Grid(0)\nsink = Grid(0, 1)\nsink = &H1\n"
    } else {
        ""
    };
    format!(
        "Attribute VB_Name = \"Authored\"\nOption Explicit\n{}Sub Notify({header})\nEnd Sub\nSub Run()\nDim sink As Long\nsink = {}\nsink = Grid\n{indexed}Call Notify(1)\nEnd Sub\n",
        state.declarations_source, state.runtime_name
    )
}

fn assert_grouped_module_variable_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: ModuleArrayState,
) {
    assert_module_array_contract(service, receipt, source, language, state);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let run = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Run")
        .unwrap();
    let grid = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Grid")
        .unwrap();
    assert_eq!(grid.kind, EntityKind::Variable);
    assert!(!document.entities.iter().any(|entity| {
        entity.kind == EntityKind::Constant
            && entity.container == grid.container
            && entity.evidence.source.is_some()
    }));
    let start = source.find("sink = Grid\n").unwrap() + "sink = ".len();
    let sites = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "vba.value_identifier.reference"
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(start + "Grid".len()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(sites.len(), 1);
    let site = sites[0];
    assert_eq!(site.role, OccurrenceRole::Reference);
    assert_eq!(site.enclosing, Some(run.id));
    assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: grid.id });
    assert_eq!(site.provenance, grid.provenance);
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        &site.source,
        start..start + "Grid".len(),
    );
    let relations = document
        .relations
        .iter()
        .filter(|relation| {
            relation.subject == RelationEndpoint::Occurrence(site.id)
                && matches!(
                    relation.predicate,
                    RelationPredicate::RefersTo | RelationPredicate::Calls
                )
        })
        .collect::<Vec<_>>();
    assert_eq!(relations.len(), 1);
    assert_eq!(relations[0].predicate, RelationPredicate::RefersTo);
    assert_eq!(relations[0].object, RelationEndpoint::Entity(grid.id));
    assert_eq!(relations[0].provenance, grid.provenance);
    assert_eq!(relations[0].evidence.source.as_ref(), Some(&site.source));
}

#[test]
fn grouped_module_variable_edits_preserve_independent_declarator_evidence() {
    let initial = ModuleArrayState {
        declarations_source: "Private Storage As Long\nPrivate Grid As String\n",
        declarations: &[("Storage As Long", "Storage"), ("Grid As String", "Grid")],
        runtime_name: "Storage",
        indexed_body: false,
    };
    let states = [
        initial,
        ModuleArrayState {
            declarations_source: "Private Storage As Long, Grid As String\n",
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Public Storage(2) As Long, Grid(-2 To +3, 0 To 4) As Long\n",
            declarations: &[
                ("Storage(2) As Long", "Storage"),
                ("Grid(-2 To +3, 0 To 4) As Long", "Grid"),
            ],
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Public Storage(2), Grid(-2 To +3, 0 To 4) As Long\n",
            declarations: &[
                ("Storage(2)", "Storage"),
                ("Grid(-2 To +3, 0 To 4) As Long", "Grid"),
            ],
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Public Storage(2) As Long, Grid(-2 To +3, 0 To 4)\n",
            declarations: &[
                ("Storage(2) As Long", "Storage"),
                ("Grid(-2 To +3, 0 To 4)", "Grid"),
            ],
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Public Buffer(2) As Long, Grid(-2 To +3, 0 To 4)\n",
            declarations: &[
                ("Buffer(2) As Long", "Buffer"),
                ("Grid(-2 To +3, 0 To 4)", "Grid"),
            ],
            runtime_name: "Buffer",
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Dim Buffer(2) As Long, [Buffer](0 To 2, 1 To 3) As Long, Grid(-2 To +3, 0 To 4)\n",
            declarations: &[
                ("Buffer(2) As Long", "Buffer"),
                ("[Buffer](0 To 2, 1 To 3) As Long", "Buffer"),
                ("Grid(-2 To +3, 0 To 4)", "Grid"),
            ],
            runtime_name: "Buffer",
            ..initial
        },
        ModuleArrayState {
            declarations_source: "Public Buffer(2) As Long, Grid(-2 To +3, 0 To 4) As Long\n",
            declarations: &[
                ("Buffer(2) As Long", "Buffer"),
                ("Grid(-2 To +3, 0 To 4) As Long", "Grid"),
            ],
            runtime_name: "Buffer",
            indexed_body: true,
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
        let configuration = (language == "vba").then_some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n");
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, ModuleArrayState)> = None;
        let mut initial_source = None;
        let mut initial_logical = None;
        for (position, state) in states.into_iter().enumerate() {
            let source = grouped_module_variable_source(state);
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
            assert_grouped_module_variable_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                let expected = match position {
                    1..=4 | 6 => old_source.replace(old_state.declarations_source, state.declarations_source),
                    5 => old_source.replace("Storage", "Buffer"),
                    7 => old_source.replace(old_state.declarations_source, state.declarations_source)
                        .replace("Call Notify(1)\n", "sink = Buffer()\nsink = Buffer(0)\nsink = Buffer(0, 1)\nsink = Grid()\nsink = Grid(0)\nsink = Grid(0, 1)\nsink = &H1\nCall Notify(1)\n"),
                    8 => initial_source.clone().unwrap(),
                    _ => panic!("an authored grouped declaration edit is required"),
                };
                assert_eq!(source, expected);
                assert!(receipt.logical_snapshot.is_some());
                assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                if position == 3 {
                    assert_eq!(
                        source,
                        old_source.replacen("Storage(2) As Long", "Storage(2)", 1)
                    );
                }
                assert_grouped_module_variable_contract(
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
            assert_grouped_module_variable_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_grouped_module_variable_contract(
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
            assert_grouped_module_variable_contract(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if position + 1 == states.len() {
                assert_eq!(initial_source.as_ref(), Some(&source));
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            // Verify the latest rebuilt predecessor before another rebuild can
            // legitimately evict it under the unchanged two-generation policy.
            previous = Some((rebuilt, source, state));
        }
    }
}

#[derive(Clone, Copy)]
struct ModuleVisibilityState {
    declarations_source: &'static str,
    declarations: WrittenParameters<'static>,
    visibilities: &'static [(&'static str, rootlight_ir::EntityVisibility)],
}

fn assert_module_visibility_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: ModuleVisibilityState,
) {
    assert_source_and_identity_for_language(service, receipt, source, language);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = &document.files[0];
    let run = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Run")
        .unwrap();
    let parser = document
        .provenance
        .iter()
        .find(|proof| proof.id == run.provenance)
        .unwrap();
    let modules = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Authored")
        .collect::<Vec<_>>();
    assert_eq!(modules.len(), 1);
    let module = modules[0];
    assert_eq!(
        module.container,
        Some(rootlight_ir::ContainerRef::File(file.id))
    );
    assert_eq!(module.provenance, parser.id);
    assert_eq!(
        run.container,
        Some(rootlight_ir::ContainerRef::Entity(module.id))
    );
    let variables = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Variable
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(module.id))
        })
        .collect::<Vec<_>>();
    assert_eq!(variables.len(), state.visibilities.len());
    let mut cursor = source.find(state.declarations_source).unwrap();
    let declarations_end = cursor + state.declarations_source.len();
    let declarations = state
        .declarations
        .iter()
        .map(|&(written, name)| {
            let start = cursor + source[cursor..].find(written).unwrap();
            let end = start + written.len();
            cursor = end;
            (start..end, written, name)
        })
        .collect::<Vec<_>>();
    assert!(
        declarations
            .iter()
            .all(|(range, _, _)| range.end <= declarations_end)
    );
    let all_definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && variables.iter().any(|variable| {
                    site.target
                        == OccurrenceTarget::Resolved {
                            symbol: variable.id,
                        }
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(all_definitions.len(), declarations.len());
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && variables
                        .iter()
                        .any(|variable| relation.object == RelationEndpoint::Entity(variable.id))
            })
            .count(),
        declarations.len()
    );
    for &(name, visibility) in state.visibilities {
        let matching = variables
            .iter()
            .filter(|entity| entity.canonical_name == name)
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        let variable = matching[0];
        assert_eq!(variable.visibility, visibility, "visibility for {name}");
        assert_eq!(variable.language, language);
        assert_eq!(variable.provenance, parser.id);
        let representative = declarations
            .iter()
            .find(|(_, _, declared_name)| *declared_name == name)
            .unwrap();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            variable.evidence.source.as_ref().unwrap(),
            representative.0.clone(),
        );
        let explained = service
            .symbol_explain(receipt.generation, variable.id, &deadline())
            .unwrap();
        assert_eq!(explained.data.generation, receipt.generation);
        assert_eq!(&explained.data.entity, *variable);
        assert_eq!(explained.data.provenance, *parser);
        assert!(!explained.data.truncated);
        let written_declarations = declarations
            .iter()
            .filter(|(_, _, declared_name)| *declared_name == name)
            .collect::<Vec<_>>();
        assert_eq!(
            all_definitions
                .iter()
                .filter(|site| {
                    site.target
                        == OccurrenceTarget::Resolved {
                            symbol: variable.id,
                        }
                })
                .count(),
            written_declarations.len()
        );
        for (range, written, _) in &written_declarations {
            let token_len = if written.starts_with('[') {
                written.find(']').unwrap() + 1
            } else {
                name.len()
            };
            assert_eq!(written[..token_len].trim_matches(['[', ']']), name);
            let definitions = all_definitions
                .iter()
                .filter(|site| {
                    site.target
                        == OccurrenceTarget::Resolved {
                            symbol: variable.id,
                        }
                        && site.source.span().start_byte() == u64::try_from(range.start).unwrap()
                        && site.source.span().end_byte()
                            == u64::try_from(range.start + token_len).unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(definitions.len(), 1);
            let definition = definitions[0];
            assert_eq!(definition.enclosing, Some(module.id));
            assert_eq!(definition.provenance, parser.id);
            assert_eq!(definition.source.span().file(), file.id);
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                &definition.source,
                range.start..range.start + token_len,
            );
            let clause = definition.evidence.source.as_ref().unwrap();
            assert_eq!(clause.span().file(), file.id);
            assert_exact_parameter_source(service, receipt, source, clause, range.clone());
            let contains = document
                .relations
                .iter()
                .filter(|relation| {
                    relation.predicate == RelationPredicate::Contains
                        && relation.object == RelationEndpoint::Entity(variable.id)
                        && relation.evidence.source.as_ref() == Some(clause)
                })
                .collect::<Vec<_>>();
            assert_eq!(contains.len(), 1);
            assert_eq!(contains[0].subject, RelationEndpoint::Entity(module.id));
            assert_eq!(contains[0].provenance, parser.id);
            assert!(
                explained
                    .data
                    .occurrences
                    .iter()
                    .any(|site| site == *definition)
            );
            assert!(
                explained
                    .data
                    .relations
                    .iter()
                    .any(|relation| relation == contains[0])
            );
            let line_start = source[..range.start]
                .rfind('\n')
                .map_or(0, |newline| newline + 1);
            let prefix = &source[line_start..range.start];
            assert!(matches!(prefix, "Public " | "Private " | "Dim "));
            let reference = SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    file.id,
                    u64::try_from(line_start).unwrap(),
                    u64::try_from(range.start).unwrap(),
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
                line_start..range.start,
            );
        }
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                20,
                0,
                &deadline(),
            )
            .unwrap();
        let hits = located
            .data
            .hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| hit.symbol == Some(variable.id))
            .collect::<Vec<_>>();
        assert_eq!(hits.len(), 1);
        let (ordinal, hit) = hits[0];
        assert_eq!(hit.source.as_ref(), variable.evidence.source.as_ref());
        let assessment = located
            .data
            .conditional_activity
            .iter()
            .find(|assessment| assessment.result_index == u32::try_from(ordinal).unwrap())
            .unwrap();
        assert_eq!(assessment.basis, ConditionalActivityBasis::DefinitionToken);
        if written_declarations.len() > 1 {
            assert_eq!(assessment.activity, ConditionalActivityState::Unproven);
            assert_eq!(
                assessment.reason,
                Some(ConditionalActivityReason::AmbiguousDefinition)
            );
            assert!(assessment.assessed_source_ref.is_none());
            assert!(assessment.inventory.is_none());
        } else {
            let definition = all_definitions
                .iter()
                .find(|site| {
                    site.target
                        == OccurrenceTarget::Resolved {
                            symbol: variable.id,
                        }
                })
                .unwrap();
            assert_eq!(assessment.activity, ConditionalActivityState::Active);
            assert_eq!(assessment.reason, None);
            assert_eq!(
                assessment.assessed_source_ref.as_ref(),
                Some(&definition.source)
            );
            assert_eq!(
                assessment.inventory.as_ref().unwrap().parser_provenance_id,
                parser.id
            );
        }
    }
    let start = source.find("sink = stored\n").unwrap() + "sink = ".len();
    let uses = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "vba.value_identifier.reference"
                && site.source.span().start_byte() == u64::try_from(start).unwrap()
                && site.source.span().end_byte() == u64::try_from(start + "stored".len()).unwrap()
        })
        .collect::<Vec<_>>();
    assert_eq!(uses.len(), 1);
    let site = uses[0];
    assert_eq!(site.role, OccurrenceRole::Reference);
    assert_eq!(site.enclosing, Some(run.id));
    assert_eq!(site.provenance, parser.id);
    assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        &site.source,
        start..start + "stored".len(),
    );
    assert!(!document.relations.iter().any(|relation| {
        relation.subject == RelationEndpoint::Occurrence(site.id)
            && matches!(
                relation.predicate,
                RelationPredicate::RefersTo | RelationPredicate::Calls
            )
    }));
}

#[test]
fn duplicate_module_visibility_requires_all_declarations_to_agree() {
    use rootlight_ir::EntityVisibility::{Private, Public, Unknown};
    let initial = ModuleVisibilityState {
        declarations_source: "Public stored As Long\nPublic [stored] As Long\n",
        declarations: &[("stored As Long", "stored"), ("[stored] As Long", "stored")],
        visibilities: &[("stored", Public)],
    };
    let states = [
        initial,
        ModuleVisibilityState {
            declarations_source: "Public stored As Long\nPrivate [stored] As Long\n",
            visibilities: &[("stored", Unknown)],
            ..initial
        },
        initial,
        ModuleVisibilityState {
            declarations_source: "Private stored As Long\nPrivate [stored] As Long\n",
            visibilities: &[("stored", Private)],
            ..initial
        },
        ModuleVisibilityState {
            declarations_source: "Public stored As Long\nDim [stored] As Long\n",
            visibilities: &[("stored", Unknown)],
            ..initial
        },
        ModuleVisibilityState {
            declarations_source: "Dim stored As Long\nPublic [stored] As Long\n",
            visibilities: &[("stored", Unknown)],
            ..initial
        },
        ModuleVisibilityState {
            declarations_source: "Public stored As Long\nPrivate [stored] As Long\nPublic stored As Long\n",
            declarations: &[
                ("stored As Long", "stored"),
                ("[stored] As Long", "stored"),
                ("stored As Long", "stored"),
            ],
            visibilities: &[("stored", Unknown)],
        },
        // Declared identities preserve case even when lookup folds it. Each
        // distinct identity must retain its own visibility, not a folded union.
        ModuleVisibilityState {
            declarations_source: "Public stored As Long\nPrivate [STORED] As Long\n",
            declarations: &[("stored As Long", "stored"), ("[STORED] As Long", "STORED")],
            visibilities: &[("stored", Public), ("STORED", Private)],
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
        let configuration = (language == "vba").then_some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n");
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, ModuleVisibilityState)> = None;
        let mut initial_source = None;
        let mut initial_logical = None;
        for (position, state) in states.into_iter().enumerate() {
            let source = format!(
                "Attribute VB_Name = \"Authored\"\nOption Explicit\n{}Sub Run()\nDim sink As Long\nsink = stored\nEnd Sub\n",
                state.declarations_source
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
            assert_module_visibility_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_eq!(
                    source,
                    old_source.replace(old_state.declarations_source, state.declarations_source)
                );
                assert_ne!(receipt.generation, old_receipt.generation);
                assert!(receipt.logical_snapshot.is_some());
                assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                assert_module_visibility_contract(
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
            assert_module_visibility_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_module_visibility_contract(
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
            assert_module_visibility_contract(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if source == *initial_source.as_ref().unwrap() {
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            // The latest rebuilt predecessor remains queryable before the next
            // rebuild can evict it under the unchanged two-generation policy.
            previous = Some((rebuilt, source, state));
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ExpectedBufferApplication {
    Fixed(usize),
    Unspecified,
    Scalar,
    Ambiguous,
    Missing,
    LocalScalar,
}

#[derive(Debug, Clone, Copy)]
struct ValueApplicationState {
    declarations_source: &'static str,
    declarations: WrittenParameters<'static>,
    buffer: ExpectedBufferApplication,
    arithmetic_fallback: bool,
}

fn value_application_source(state: ValueApplicationState) -> String {
    let shadow = if matches!(state.buffer, ExpectedBufferApplication::LocalScalar) {
        "Dim Buffer As Long\n"
    } else {
        ""
    };
    // Hexadecimal arguments remain outside the first-party expression slice;
    // this separately retained application keeps native rank evidence alive.
    let arithmetic = if state.arithmetic_fallback {
        "sink = Buffer(&H0) + 1\n"
    } else {
        ""
    };
    format!(
        "Attribute VB_Name = \"Authored\"\nOption Explicit\n{}Function Fetch(ByVal value As Long, Optional extra As Long = 7) As Long\nFetch = value\nEnd Function\nSub Run()\nDim sink As Long\nDim index As Long\nDim value As Long\n{shadow}index = 0\nvalue = 1\nsink = Buffer()\nsink = Buffer(0)\nsink = Buffer(0, 1)\nsink = Buffer(index)\nsink = Grid()\nsink = Grid(0)\nsink = Grid(0, 1)\nsink = Grid(index, value)\nsink = Fetch()\nsink = Fetch(value)\nsink = Fetch(value, 7)\nsink = Fetch(value, 7, 8)\n{arithmetic}End Sub\n",
        state.declarations_source
    )
}

fn assert_value_application_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: ValueApplicationState,
) {
    assert_source_and_identity_for_language(service, receipt, source, language);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = &document.files[0];
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Authored")
        .unwrap();
    assert_eq!(
        module.container,
        Some(rootlight_ir::ContainerRef::File(file.id))
    );
    let run = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Run")
        .unwrap();
    let fetch = document
        .entities
        .iter()
        .find(|entity| entity.canonical_name == "Fetch")
        .unwrap();
    assert_eq!(fetch.kind, EntityKind::Function);
    assert_eq!(
        run.container,
        Some(rootlight_ir::ContainerRef::Entity(module.id))
    );
    assert_eq!(fetch.container, run.container);
    let fetch_start = source.find("Function Fetch(").unwrap();
    let fetch_end = source.find("End Function").unwrap() + "End Function".len();
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        fetch.evidence.source.as_ref().unwrap(),
        fetch_start..fetch_end,
    );
    let explained = service
        .symbol_explain(receipt.generation, fetch.id, &deadline())
        .unwrap();
    assert_eq!(&explained.data.entity, fetch);
    assert!(!explained.data.truncated);
    let parser = document
        .provenance
        .iter()
        .find(|proof| proof.id == run.provenance)
        .unwrap();
    assert_eq!(fetch.provenance, parser.id);
    let array_state = ModuleArrayState {
        declarations_source: state.declarations_source,
        declarations: state.declarations,
        runtime_name: "Buffer",
        indexed_body: false,
    };
    let ranges = module_array_declaration_ranges(source, array_state);
    let variables = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable && entity.container == run.container)
        .collect::<Vec<_>>();
    let names = state
        .declarations
        .iter()
        .map(|(_, name)| *name)
        .collect::<BTreeSet<_>>();
    assert_eq!(variables.len(), names.len());
    let definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && variables.iter().any(|variable| {
                    site.target
                        == OccurrenceTarget::Resolved {
                            symbol: variable.id,
                        }
                })
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), state.declarations.len());
    for (&(written, name), range) in state.declarations.iter().zip(&ranges) {
        let variable = variables
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(variable.provenance, parser.id);
        assert_eq!(variable.visibility, rootlight_ir::EntityVisibility::Private);
        let representative = state
            .declarations
            .iter()
            .zip(&ranges)
            .find(|((_, candidate), _)| *candidate == name)
            .unwrap()
            .1;
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            variable.evidence.source.as_ref().unwrap(),
            representative.clone(),
        );
        let token_len = if written.starts_with('[') {
            written.find(']').unwrap() + 1
        } else {
            name.len()
        };
        assert_eq!(written[..token_len].trim_matches(['[', ']']), name);
        let definition = definitions
            .iter()
            .filter(|site| {
                site.target
                    == OccurrenceTarget::Resolved {
                        symbol: variable.id,
                    }
                    && site.source.span().start_byte() == u64::try_from(range.start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(range.start + token_len).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(definition.len(), 1);
        assert_eq!(definition[0].enclosing, Some(module.id));
        assert_eq!(definition[0].provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &definition[0].source,
            range.start..range.start + token_len,
        );
        let clause = definition[0].evidence.source.as_ref().unwrap();
        assert_exact_parameter_source(service, receipt, source, clause, range.clone());
        let contains = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(module.id)
                    && relation.object == RelationEndpoint::Entity(variable.id)
                    && relation.evidence.source.as_ref() == Some(clause)
            })
            .collect::<Vec<_>>();
        assert_eq!(contains.len(), 1);
        assert_eq!(contains[0].provenance, parser.id);
        let explained = service
            .symbol_explain(receipt.generation, variable.id, &deadline())
            .unwrap();
        assert_eq!(&explained.data.entity, *variable);
        assert!(!explained.data.truncated);
        assert!(
            explained
                .data
                .occurrences
                .iter()
                .any(|site| site == *definition[0])
        );
        assert!(
            explained
                .data
                .relations
                .iter()
                .any(|relation| relation == contains[0])
        );
        if !written.contains(" As ") {
            let within = |reference: &SourceRef| {
                reference.span().file() == file.id
                    && reference.span().start_byte() >= u64::try_from(range.start).unwrap()
                    && reference.span().end_byte() <= u64::try_from(range.end).unwrap()
            };
            assert!(
                !document
                    .occurrences
                    .iter()
                    .any(|site| within(&site.source) && site.role == OccurrenceRole::TypeUse)
            );
            assert!(!document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::UsesType
                    && (relation.subject == RelationEndpoint::Entity(variable.id)
                        || relation.evidence.source.as_ref().is_some_and(within))
            }));
            assert!(!document.entities.iter().any(|entity| {
                entity.canonical_name.eq_ignore_ascii_case("Variant")
                    && entity.evidence.source.as_ref().is_some_and(within)
            }));
        }
    }
    for name in ["Buffer", "Grid", "Renamed"] {
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                20,
                0,
                &deadline(),
            )
            .unwrap();
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
        for (ordinal, hit) in located
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
            let anchors = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .collect::<Vec<_>>();
            let assessment = located
                .data
                .conditional_activity
                .iter()
                .find(|assessment| assessment.result_index == u32::try_from(ordinal).unwrap())
                .unwrap();
            assert_eq!(assessment.basis, ConditionalActivityBasis::DefinitionToken);
            if anchors.len() > 1 {
                assert_eq!(assessment.activity, ConditionalActivityState::Unproven);
                assert_eq!(
                    assessment.reason,
                    Some(ConditionalActivityReason::AmbiguousDefinition)
                );
                assert!(assessment.assessed_source_ref.is_none());
                assert!(assessment.inventory.is_none());
            } else {
                assert_eq!(anchors.len(), 1);
                assert_eq!(assessment.activity, ConditionalActivityState::Active);
                assert_eq!(assessment.reason, None);
                assert_eq!(
                    assessment.assessed_source_ref.as_ref(),
                    Some(&anchors[0].source)
                );
                assert_eq!(
                    assessment.inventory.as_ref().unwrap().parser_provenance_id,
                    parser.id
                );
            }
        }
    }
    let formals = [
        ("ByVal value As Long", "value"),
        ("Optional extra As Long = 7", "extra"),
    ]
    .map(|(written, name)| {
        let parameter = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(fetch.id))
            })
            .unwrap();
        let start = source.find(written).unwrap();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            parameter.evidence.source.as_ref().unwrap(),
            start..start + written.len(),
        );
        let name_start = start + written.find(name).unwrap();
        let definition = document
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
        assert_eq!(definition.len(), 1);
        assert_eq!(definition[0].enclosing, Some(fetch.id));
        assert_eq!(definition[0].provenance, parser.id);
        assert_eq!(
            definition[0].evidence.source.as_ref(),
            parameter.evidence.source.as_ref()
        );
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &definition[0].source,
            name_start..name_start + name.len(),
        );
        (parameter, definition[0])
    });
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(fetch.id))
            })
            .count(),
        formals.len()
    );
    let local = |name: &str| {
        document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Variable
                    && entity.canonical_name == name
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(run.id))
            })
            .unwrap()
    };
    let buffer_receiver = match state.buffer {
        ExpectedBufferApplication::Ambiguous | ExpectedBufferApplication::Missing => None,
        ExpectedBufferApplication::LocalScalar => Some(local("Buffer").id),
        ExpectedBufferApplication::Fixed(_)
        | ExpectedBufferApplication::Unspecified
        | ExpectedBufferApplication::Scalar => Some(
            variables
                .iter()
                .find(|entity| entity.canonical_name == "Buffer")
                .unwrap()
                .id,
        ),
    };
    let grid = variables
        .iter()
        .find(|entity| entity.canonical_name == "Grid")
        .unwrap();
    let probes = [
        ("Buffer()", "Buffer", 0, false),
        ("Buffer(0)", "Buffer", 1, false),
        ("Buffer(0, 1)", "Buffer", 2, false),
        ("Buffer(index)", "Buffer", 1, false),
        ("Grid()", "Grid", 0, false),
        ("Grid(0)", "Grid", 1, false),
        ("Grid(0, 1)", "Grid", 2, false),
        ("Grid(index, value)", "Grid", 2, false),
        ("Fetch()", "Fetch", 0, true),
        ("Fetch(value)", "Fetch", 1, true),
        ("Fetch(value, 7)", "Fetch", 2, true),
        ("Fetch(value, 7, 8)", "Fetch", 3, true),
    ];
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| { site.syntax_kind == "vba.application.reference" })
            .count(),
        probes.len() + usize::from(state.arithmetic_fallback)
    );
    for (ordinal, (expression, name, argument_count, is_function)) in probes
        .into_iter()
        .chain(
            state
                .arithmetic_fallback
                .then_some(("Buffer(&H0)", "Buffer", 1, false)),
        )
        .enumerate()
    {
        let statement = if ordinal == probes.len() {
            format!("sink = {expression} + 1\n")
        } else {
            format!("sink = {expression}\n")
        };
        let start = source.find(&statement).unwrap() + "sink = ".len();
        let end = start + expression.len();
        let receiver = match name {
            "Buffer" => buffer_receiver,
            "Grid" => Some(grid.id),
            "Fetch" => Some(fetch.id),
            _ => panic!("an authored application receiver is required"),
        };
        let proven = match name {
            "Buffer" => match state.buffer {
                ExpectedBufferApplication::Fixed(rank) => {
                    argument_count == 0 || argument_count == rank
                }
                ExpectedBufferApplication::Unspecified => true,
                ExpectedBufferApplication::Scalar
                | ExpectedBufferApplication::Ambiguous
                | ExpectedBufferApplication::Missing
                | ExpectedBufferApplication::LocalScalar => false,
            },
            "Grid" => argument_count == 0 || argument_count == 2,
            "Fetch" => matches!(argument_count, 1 | 2),
            _ => panic!("an authored application contract is required"),
        };
        let application = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(end).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(application.len(), 1, "application: {statement}");
        let application = application[0];
        assert_eq!(application.role, OccurrenceRole::Reference);
        assert_eq!(application.enclosing, Some(run.id));
        assert_exact_parameter_source(service, receipt, source, &application.source, start..end);
        if proven {
            assert_eq!(
                application.target,
                OccurrenceTarget::Resolved {
                    symbol: receiver.unwrap()
                }
            );
        } else {
            assert!(matches!(
                application.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
        let refers = document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Occurrence(application.id)
                    && relation.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        assert_eq!(refers.len(), usize::from(proven));
        if proven {
            assert_eq!(
                refers[0].object,
                RelationEndpoint::Entity(receiver.unwrap())
            );
            assert_eq!(refers[0].provenance, application.provenance);
            assert_eq!(
                refers[0].evidence.source.as_ref(),
                Some(&application.source)
            );
        }
        assert!(!document.relations.iter().any(|relation| {
            relation.subject == RelationEndpoint::Occurrence(application.id)
                && relation.predicate == RelationPredicate::Calls
        }));
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .filter(|gap| {
                    gap.source == application.source
                        && gap.detail == "vba-call-or-index-target-unavailable"
                })
                .count(),
            usize::from(!proven)
        );
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == application.provenance)
            .unwrap();
        if is_function && proven {
            let mut parents = vec![FactRef::Fact(parser.id), FactRef::Entity(fetch.id)];
            for (parameter, definition) in formals {
                parents.push(FactRef::Entity(parameter.id));
                let clause = parameter.evidence.source.as_ref().unwrap();
                assert!(proof.input_sources.contains(clause));
                assert!(proof.evidence_sources.contains(clause));
                assert!(proof.evidence_sources.contains(&definition.source));
            }
            parents.sort_unstable();
            parents.dedup();
            assert_eq!(proof.derivation_parents, parents);
            assert_eq!(
                proof.rule.as_deref(),
                Some("vba-procedure-application-v1.source")
            );
            assert_ne!(proof.id, parser.id);
            assert_eq!(proof.producer_kind, parser.producer_kind);
            assert_eq!(proof.binary_digest, parser.binary_digest);
            assert_eq!(proof.frontend_version, parser.frontend_version);
            assert!(
                proof
                    .input_sources
                    .contains(file.evidence.source.as_ref().unwrap())
            );
            assert!(proof.evidence_sources.contains(&application.source));
            assert!(
                proof
                    .evidence_sources
                    .contains(fetch.evidence.source.as_ref().unwrap())
            );
        } else {
            assert_eq!(proof.id, parser.id);
            assert_eq!(proof.rule, None);
            assert!(!formals.iter().any(|(parameter, _)| {
                proof
                    .derivation_parents
                    .contains(&FactRef::Entity(parameter.id))
            }));
        }
        for reference in proof.input_sources.iter().chain(&proof.evidence_sources) {
            assert_eq!(reference.repository(), receipt.repository);
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
            assert_eq!(reference.span().file(), file.id);
            assert!(reference.span().end_byte() <= u64::try_from(source.len()).unwrap());
        }
        let callees = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.invoked_get_identifier.reference"
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + name.len()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(callees.len(), 1, "value receiver: {statement}");
        let callee = callees[0];
        assert_eq!(
            callee.role,
            if is_function {
                OccurrenceRole::CallSite
            } else {
                OccurrenceRole::Reference
            }
        );
        assert_eq!(callee.enclosing, Some(run.id));
        assert_eq!(callee.provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &callee.source,
            start..start + name.len(),
        );
        if let Some(symbol) = receiver {
            assert_eq!(callee.target, OccurrenceTarget::Resolved { symbol });
        } else {
            assert!(matches!(callee.target, OccurrenceTarget::Unresolved { .. }));
        }
        for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
            let relations = document
                .relations
                .iter()
                .filter(|relation| {
                    relation.subject == RelationEndpoint::Occurrence(callee.id)
                        && relation.predicate == predicate
                })
                .collect::<Vec<_>>();
            let present = receiver.is_some()
                && if is_function {
                    predicate == RelationPredicate::Calls
                } else {
                    predicate == RelationPredicate::RefersTo
                };
            assert_eq!(
                relations.len(),
                usize::from(present),
                "{statement}: {predicate:?}"
            );
            if present {
                assert_eq!(
                    relations[0].object,
                    RelationEndpoint::Entity(receiver.unwrap())
                );
                assert_eq!(relations[0].provenance, parser.id);
                assert_eq!(relations[0].evidence.source.as_ref(), Some(&callee.source));
            }
        }
        let arguments_start = expression.find('(').unwrap() + 1;
        let arguments_end = expression.len() - 1;
        let mut cursor = arguments_start;
        let mut name_count = 0;
        for atom in expression[arguments_start..arguments_end]
            .split(',')
            .map(str::trim)
            .filter(|atom| !atom.is_empty())
        {
            let atom_start = start + cursor + expression[cursor..].find(atom).unwrap();
            cursor = atom_start - start + atom.len();
            if atom == "index" || atom == "value" {
                name_count += 1;
                let target = local(atom);
                let declaration = format!("Dim {atom} As Long");
                let declaration_start = source.find(&declaration).unwrap() + "Dim ".len();
                assert_exact_parameter_source(
                    service,
                    receipt,
                    source,
                    target.evidence.source.as_ref().unwrap(),
                    declaration_start..declaration_start + atom.len() + " As Long".len(),
                );
                let reads = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.syntax_kind == "vba.value_identifier.reference"
                            && site.source.span().start_byte() == u64::try_from(atom_start).unwrap()
                            && site.source.span().end_byte()
                                == u64::try_from(atom_start + atom.len()).unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(reads.len(), 1);
                assert_eq!(reads[0].role, OccurrenceRole::Reference);
                assert_eq!(reads[0].enclosing, Some(run.id));
                assert_eq!(reads[0].provenance, parser.id);
                assert_eq!(
                    reads[0].target,
                    OccurrenceTarget::Resolved { symbol: target.id }
                );
                assert_exact_parameter_source(
                    service,
                    receipt,
                    source,
                    &reads[0].source,
                    atom_start..atom_start + atom.len(),
                );
                assert!(
                    !formals
                        .iter()
                        .any(|(parameter, _)| parameter.id == target.id)
                );
                let edges = document
                    .relations
                    .iter()
                    .filter(|relation| {
                        relation.subject == RelationEndpoint::Occurrence(reads[0].id)
                            && relation.predicate == RelationPredicate::RefersTo
                    })
                    .collect::<Vec<_>>();
                assert_eq!(edges.len(), 1);
                assert_eq!(edges[0].object, RelationEndpoint::Entity(target.id));
                assert_eq!(edges[0].evidence.source.as_ref(), Some(&reads[0].source));
                assert!(!document.relations.iter().any(|relation| {
                    relation.subject == RelationEndpoint::Occurrence(reads[0].id)
                        && relation.predicate == RelationPredicate::Calls
                }));
                let explained = service
                    .symbol_explain(receipt.generation, target.id, &deadline())
                    .unwrap();
                assert_eq!(&explained.data.entity, target);
                assert!(!explained.data.truncated);
            } else {
                assert!(
                    atom.bytes().all(|byte| byte.is_ascii_digit())
                        || (ordinal == probes.len() && atom == "&H0")
                );
                assert!(!document.occurrences.iter().any(|site| {
                    site.source.span().start_byte() == u64::try_from(atom_start).unwrap()
                        && site.source.span().end_byte()
                            == u64::try_from(atom_start + atom.len()).unwrap()
                }));
            }
        }
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| {
                    site.syntax_kind == "vba.value_identifier.reference"
                        && site.source.span().start_byte() >= u64::try_from(start).unwrap()
                        && site.source.span().end_byte() <= u64::try_from(end).unwrap()
                })
                .count(),
            name_count
        );
    }
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Calls)
            .count(),
        4
    );
}

#[test]
fn assignment_value_applications_preserve_arrays_formals_and_source_lifetimes() {
    let initial = ValueApplicationState {
        declarations_source: "Private Buffer(2) As Long, Grid(0 To 2, 1 To 3) As Long\n",
        declarations: &[
            ("Buffer(2) As Long", "Buffer"),
            ("Grid(0 To 2, 1 To 3) As Long", "Grid"),
        ],
        buffer: ExpectedBufferApplication::Fixed(1),
        arithmetic_fallback: false,
    };
    let states = [
        initial,
        ValueApplicationState {
            declarations_source: "Private Buffer() As Long, Grid(0 To 2, 1 To 3) As Long\n",
            declarations: &[
                ("Buffer() As Long", "Buffer"),
                ("Grid(0 To 2, 1 To 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::Unspecified,
            ..initial
        },
        ValueApplicationState {
            declarations_source: "Private Buffer(0 To 2, 1 To 3), Grid(0 To 2, 1 To 3) As Long\n",
            declarations: &[
                ("Buffer(0 To 2, 1 To 3)", "Buffer"),
                ("Grid(0 To 2, 1 To 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::Fixed(2),
            ..initial
        },
        ValueApplicationState {
            declarations_source: "Private Buffer As Long, Grid(0 To 2, 1 To 3) As Long\n",
            declarations: &[
                ("Buffer As Long", "Buffer"),
                ("Grid(0 To 2, 1 To 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::Scalar,
            ..initial
        },
        ValueApplicationState {
            declarations_source: "Private Buffer(2) As Long, [Buffer](0 To 2, 1 To 3) As Long, Grid(0 To 2, 1 To 3) As Long\n",
            declarations: &[
                ("Buffer(2) As Long", "Buffer"),
                ("[Buffer](0 To 2, 1 To 3) As Long", "Buffer"),
                ("Grid(0 To 2, 1 To 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::Ambiguous,
            ..initial
        },
        ValueApplicationState {
            declarations_source: "Private Renamed(2) As Long, Grid(0 To 2, 1 To 3) As Long\n",
            declarations: &[
                ("Renamed(2) As Long", "Renamed"),
                ("Grid(0 To 2, 1 To 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::Missing,
            ..initial
        },
        ValueApplicationState {
            buffer: ExpectedBufferApplication::LocalScalar,
            ..initial
        },
        ValueApplicationState {
            arithmetic_fallback: true,
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
        let configuration = (language == "vba").then_some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n");
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, ValueApplicationState)> = None;
        let mut initial_source = None;
        let mut initial_logical = None;
        for (position, state) in states.into_iter().enumerate() {
            let source = value_application_source(state);
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
            assert_value_application_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                let expected = old_source
                    .replace(old_state.declarations_source, state.declarations_source)
                    .replace("Dim Buffer As Long\n", "")
                    .replace("sink = Buffer(&H0) + 1\n", "");
                let expected = if matches!(state.buffer, ExpectedBufferApplication::LocalScalar) {
                    expected.replace("index = 0\n", "Dim Buffer As Long\nindex = 0\n")
                } else {
                    expected
                };
                let expected = if state.arithmetic_fallback {
                    expected.replace(
                        "sink = Fetch(value, 7, 8)\n",
                        "sink = Fetch(value, 7, 8)\nsink = Buffer(&H0) + 1\n",
                    )
                } else {
                    expected
                };
                assert_eq!(source, expected);
                assert_ne!(receipt.generation, old_receipt.generation);
                assert!(receipt.logical_snapshot.is_some());
                assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                assert_value_application_contract(
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
            assert_value_application_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_value_application_contract(
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
            assert_value_application_contract(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if position + 1 == states.len() {
                assert_eq!(initial_source.as_ref(), Some(&source));
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            // Keep the latest rebuilt predecessor until the next clean rebuild;
            // older receipts can legitimately expire under retention two.
            previous = Some((rebuilt, source, state));
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum ExpressionFunctionTarget {
    Unique,
    Missing,
    Duplicate,
    LocalScalar,
}

#[derive(Debug, Clone, Copy)]
struct ExpressionSourceState {
    declarations_source: &'static str,
    declarations: WrittenParameters<'static>,
    buffer: ExpectedBufferApplication,
    function: ExpressionFunctionTarget,
    operand: &'static str,
    native_fallback: bool,
}

fn expression_assignment_source(state: ExpressionSourceState) -> String {
    let name = if matches!(state.function, ExpressionFunctionTarget::Missing) {
        "Measure"
    } else {
        "Fetch"
    };
    let function = format!(
        "Function {name}(ByVal value As Long, Optional extra As Long = 7) As Long\n{name} = value\nEnd Function\n"
    );
    let functions = if matches!(state.function, ExpressionFunctionTarget::Duplicate) {
        function.repeat(2)
    } else {
        function
    };
    let shadow = if matches!(state.function, ExpressionFunctionTarget::LocalScalar) {
        "Dim Fetch As Long\n"
    } else {
        ""
    };
    let fallback = if state.native_fallback {
        "sink = &H1\n"
    } else {
        ""
    };
    // The retained grammar recovers these source-valid comparison combinations;
    // the hex control must remain a native-valid whole module independently.
    let comparisons = if state.native_fallback {
        ""
    } else {
        "sink = value = 0 Xor index <> 1\nsink = value < 2 Eqv index >= 0 Imp value <> index\nsink = value = 0 Or index = 1 And value > index\nsink = value < index < 2\n"
    };
    format!(
        "Attribute VB_Name = \"Authored\"\nOption Explicit\n{}{functions}Sub Run()\nDim sink As Long\nDim index As Long\nDim value As Long\n{shadow}index = 0\nvalue = 1\nsink = -(value + {}) * 3 ^ 2 / 4 \\ 2 Mod 2\nsink = Not (value = 0) And True Or False Xor True Eqv False Imp True\n{comparisons}sink = \"left\" & \"right\"\nsink = (Buffer(index) + Grid(0, value)) * Fetch(value)\nsink = Fetch(Buffer(index), Fetch(value + 1)) + Fetch()\nsink = (Fetch(value, 7, 8))\n{fallback}End Sub\n",
        state.declarations_source, state.operand
    )
}

fn assert_expression_assignment_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: ExpressionSourceState,
) {
    assert_source_and_identity_for_language(service, receipt, source, language);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = &document.files[0];
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Authored")
        .unwrap();
    assert_eq!(
        module.container,
        Some(rootlight_ir::ContainerRef::File(file.id))
    );
    let run = document
        .entities
        .iter()
        .find(|entity| {
            entity.canonical_name == "Run"
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(module.id))
        })
        .unwrap();
    let parser = document
        .provenance
        .iter()
        .find(|proof| proof.id == run.provenance)
        .unwrap();
    let source_reference = |range: std::ops::Range<usize>| {
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(range.start).unwrap(),
                u64::try_from(range.end).unwrap(),
            )
            .unwrap(),
            content_hash(source.as_bytes()),
            None,
        )
    };
    let assert_declaration = |entity: &rootlight_ir::EntityRecord,
                              owner,
                              range: std::ops::Range<usize>,
                              name: std::ops::Range<usize>| {
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(owner))
        );
        assert_eq!(entity.provenance, parser.id);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                    && site.source.span().start_byte() == u64::try_from(name.start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(name.end).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1);
        let definition = definitions[0];
        assert_eq!(definition.enclosing, Some(owner));
        assert_eq!(definition.provenance, parser.id);
        assert_exact_parameter_source(service, receipt, source, &definition.source, name);
        let clause = definition.evidence.source.as_ref().unwrap();
        assert_exact_parameter_source(service, receipt, source, clause, range);
        let contains = document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Entity(owner)
                    && relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(entity.id)
                    && relation.evidence.source.as_ref() == Some(clause)
            })
            .collect::<Vec<_>>();
        assert_eq!(contains.len(), 1);
        assert_eq!(contains[0].provenance, parser.id);
        let explained = service
            .symbol_explain(receipt.generation, entity.id, &deadline())
            .unwrap();
        assert_eq!(&explained.data.entity, entity);
        assert!(!explained.data.truncated);
        assert!(
            explained
                .data
                .occurrences
                .iter()
                .any(|site| site == definition)
        );
        assert!(
            explained
                .data
                .relations
                .iter()
                .any(|relation| relation == contains[0])
        );
    };
    let module_variables = document
        .entities
        .iter()
        .filter(|entity| entity.kind == EntityKind::Variable && entity.container == run.container)
        .collect::<Vec<_>>();
    assert_eq!(module_variables.len(), 2);
    let ranges = module_array_declaration_ranges(
        source,
        ModuleArrayState {
            declarations_source: state.declarations_source,
            declarations: state.declarations,
            runtime_name: "Buffer",
            indexed_body: false,
        },
    );
    for (&(written, name), range) in state.declarations.iter().zip(&ranges) {
        let entity = module_variables
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(entity.visibility, rootlight_ir::EntityVisibility::Private);
        let token_length = if written.starts_with('[') {
            written.find(']').unwrap() + 1
        } else {
            name.len()
        };
        assert_declaration(
            entity,
            module.id,
            range.clone(),
            range.start..range.start + token_length,
        );
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && module_variables.iter().any(|entity| {
                        site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                    })
            })
            .count(),
        state.declarations.len()
    );
    let buffer = module_variables
        .iter()
        .find(|entity| entity.canonical_name == "Buffer")
        .unwrap();
    let grid = module_variables
        .iter()
        .find(|entity| entity.canonical_name == "Grid")
        .unwrap();
    let function_name = if matches!(state.function, ExpressionFunctionTarget::Missing) {
        "Measure"
    } else {
        "Fetch"
    };
    let function = document.entities.iter().find(|entity| {
        entity.kind == EntityKind::Function
            && entity.canonical_name == function_name
            && entity.container == run.container
    });
    let function_prefix = format!("Function {function_name}(");
    let headers = source
        .match_indices(&function_prefix)
        .map(|(start, _)| start)
        .collect::<Vec<_>>();
    assert_eq!(
        headers.len(),
        if matches!(state.function, ExpressionFunctionTarget::Duplicate) {
            2
        } else {
            1
        }
    );
    let written_formals = [
        ("ByVal value As Long", "value"),
        ("Optional extra As Long = 7", "extra"),
    ];
    let mut formals = Vec::new();
    if matches!(state.function, ExpressionFunctionTarget::Duplicate) {
        // Equal public identities with different procedure-scope guards quarantine
        // both subtrees. Their exact declarations remain diagnostic source anchors,
        // not invented Definition occurrences or a first-wins callable identity.
        assert!(function.is_none());
        assert!(!document.entities.iter().any(|entity| {
            entity.kind == EntityKind::Parameter
                || (entity.kind == EntityKind::Function && entity.canonical_name == function_name)
        }));
        let mut expected_skipped = BTreeSet::new();
        let mut assert_quarantined =
            |range: std::ops::Range<usize>, name: std::ops::Range<usize>| {
                let reference = source_reference(range.clone());
                let gaps = document
                    .skipped_regions
                    .iter()
                    .filter(|gap| {
                        gap.source.span() == reference.span()
                            && gap.detail == "stable-scope-identity-unavailable"
                    })
                    .collect::<Vec<_>>();
                assert_eq!(gaps.len(), 1, "{state:?}: quarantined {range:?}");
                let gap = gaps[0];
                assert_eq!(gap.repository, receipt.repository);
                assert_eq!(gap.generation, receipt.generation);
                assert_eq!(gap.domain, rootlight_ir::FactDomain::Entities);
                assert_eq!(
                    gap.reason,
                    rootlight_ir::SkippedRegionReason::UnsupportedConstruct
                );
                assert_eq!(gap.provenance, parser.id);
                assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
                assert_exact_parameter_source(service, receipt, source, &gap.source, range.clone());
                assert!(range.start <= name.start && name.end <= range.end);
                let name_source = source_reference(name.clone());
                assert_exact_parameter_source(service, receipt, source, &name_source, name);
                assert!(!document.occurrences.iter().any(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.source.span() == name_source.span()
                }));
                assert!(!document.relations.iter().any(|relation| {
                    relation.predicate == RelationPredicate::Contains
                        && relation
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span() == reference.span())
                }));
                assert!(expected_skipped.insert(reference.span()));
            };
        for &start in &headers {
            let end = start + source[start..].find("End Function").unwrap() + "End Function".len();
            let name_start = start + "Function ".len();
            assert_quarantined(start..end, name_start..name_start + function_name.len());
            for (written, name) in written_formals {
                let clause_start = start + source[start..end].find(written).unwrap();
                let name_start = clause_start + written.find(name).unwrap();
                assert_quarantined(
                    clause_start..clause_start + written.len(),
                    name_start..name_start + name.len(),
                );
            }
        }
        assert_eq!(expected_skipped.len(), 6);
        let skipped = document
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "stable-scope-identity-unavailable")
            .map(|gap| gap.source.span())
            .collect::<Vec<_>>();
        assert_eq!(skipped.len(), expected_skipped.len());
        assert_eq!(
            skipped.into_iter().collect::<BTreeSet<_>>(),
            expected_skipped
        );
    } else {
        let function = function.unwrap_or_else(|| {
            let entities = document
                .entities
                .iter()
                .take(32)
                .map(|entity| (
                    entity.kind,
                    entity.canonical_name.chars().take(64).collect::<String>(),
                    entity.container,
                    entity.evidence.source.as_ref().map(|source| source.span()),
                ))
                .collect::<Vec<_>>();
            let excerpt = source.chars().take(4096).collect::<String>();
            panic!(
                "missing authored function {function_name:?} under {:?}; language={language:?}; state={state:?}; entities(first32)={entities:?}; source(first4096chars)={excerpt:?}",
                run.container
            );
        });
        let first_end =
            headers[0] + source[headers[0]..].find("End Function").unwrap() + "End Function".len();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            function.evidence.source.as_ref().unwrap(),
            headers[0]..first_end,
        );
        for (written, name) in written_formals {
            let entity = document
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Parameter
                        && entity.canonical_name == name
                        && entity.container == Some(rootlight_ir::ContainerRef::Entity(function.id))
                })
                .unwrap();
            let start = headers[0] + source[headers[0]..].find(written).unwrap();
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                entity.evidence.source.as_ref().unwrap(),
                start..start + written.len(),
            );
            formals.push((entity, written, name));
        }
        for &start in &headers {
            let end = start + source[start..].find("End Function").unwrap() + "End Function".len();
            let name_start = start + "Function ".len();
            assert_declaration(
                function,
                module.id,
                start..end,
                name_start..name_start + function_name.len(),
            );
            for &(parameter, written, name) in &formals {
                let clause_start = start + source[start..end].find(written).unwrap();
                let name_start = clause_start + written.find(name).unwrap();
                assert_declaration(
                    parameter,
                    function.id,
                    clause_start..clause_start + written.len(),
                    name_start..name_start + name.len(),
                );
            }
        }
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        formals.len()
    );
    let local = |name: &str| {
        document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Variable
                    && entity.canonical_name == name
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(run.id))
            })
            .unwrap()
    };
    for name in ["sink", "index", "value"]
        .into_iter()
        .chain(matches!(state.function, ExpressionFunctionTarget::LocalScalar).then_some("Fetch"))
    {
        let entity = local(name);
        let written = format!("Dim {name} As Long");
        let start = source.find(&written).unwrap() + "Dim ".len();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            entity.evidence.source.as_ref().unwrap(),
            start..start + name.len() + " As Long".len(),
        );
        assert_declaration(
            entity,
            run.id,
            start..start + name.len() + " As Long".len(),
            start..start + name.len(),
        );
    }
    for name in ["Fetch", "Measure", "Buffer"] {
        let locate = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                20,
                0,
                &deadline(),
            )
            .unwrap();
        let actual = locate
            .data
            .hits
            .iter()
            .filter_map(|hit| hit.symbol)
            .collect::<BTreeSet<_>>();
        let expected = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == name && entity.evidence.source.is_some())
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
        for (ordinal, hit) in locate
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
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                })
                .collect::<Vec<_>>();
            let assessment = locate
                .data
                .conditional_activity
                .iter()
                .find(|assessment| assessment.result_index == u32::try_from(ordinal).unwrap())
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
    let grouped = "sink = (Buffer(index) + Grid(0, value)) * Fetch(value)\n";
    let nested = "sink = Fetch(Buffer(index), Fetch(value + 1)) + Fetch()\n";
    let extra = "sink = (Fetch(value, 7, 8))\n";
    let probes = [
        (grouped, "Buffer(index)", "Buffer", 1),
        (grouped, "Grid(0, value)", "Grid", 2),
        (grouped, "Fetch(value)", "Fetch", 1),
        (nested, "Fetch(Buffer(index), Fetch(value + 1))", "Fetch", 2),
        (nested, "Buffer(index)", "Buffer", 1),
        (nested, "Fetch(value + 1)", "Fetch", 1),
        (nested, "Fetch()", "Fetch", 0),
        (extra, "Fetch(value, 7, 8)", "Fetch", 3),
    ];
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "vba.application.reference")
            .count(),
        probes.len()
    );
    let mut expected_call_sources = BTreeSet::new();
    for (statement, expression, name, count) in probes {
        let statement_start = source.find(statement).unwrap();
        let start = statement_start + statement.find(expression).unwrap();
        let end = start + expression.len();
        let (receiver, callable) = match name {
            "Fetch" => match state.function {
                ExpressionFunctionTarget::Unique => (Some(function.unwrap().id), true),
                ExpressionFunctionTarget::Missing | ExpressionFunctionTarget::Duplicate => {
                    (None, false)
                }
                ExpressionFunctionTarget::LocalScalar => (Some(local("Fetch").id), false),
            },
            "Buffer" => (
                if matches!(state.buffer, ExpectedBufferApplication::Ambiguous) {
                    None
                } else {
                    Some(buffer.id)
                },
                false,
            ),
            "Grid" => (Some(grid.id), false),
            _ => panic!("an authored expression receiver is required"),
        };
        // Written arity is independent of the nested argument's own target or
        // runtime value: unavailable inner indexing must not fabricate typing.
        let proven = match name {
            "Fetch" => callable && matches!(count, 1 | 2),
            "Buffer" => {
                matches!(state.buffer, ExpectedBufferApplication::Fixed(rank) if rank == count)
            }
            "Grid" => count == 2,
            _ => panic!("an authored expression application is required"),
        };
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(end).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "{state:?}: {expression}");
        let application = sites[0];
        assert_eq!(application.role, OccurrenceRole::Reference);
        assert_eq!(application.enclosing, Some(run.id));
        assert_exact_parameter_source(service, receipt, source, &application.source, start..end);
        if proven {
            assert_eq!(
                application.target,
                OccurrenceTarget::Resolved {
                    symbol: receiver.unwrap()
                }
            );
        } else {
            assert!(matches!(
                application.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
        let refers = document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Occurrence(application.id)
                    && relation.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        assert_eq!(refers.len(), usize::from(proven));
        if proven {
            assert_eq!(
                refers[0].object,
                RelationEndpoint::Entity(receiver.unwrap())
            );
            assert_eq!(refers[0].provenance, application.provenance);
            assert_eq!(
                refers[0].evidence.source.as_ref(),
                Some(&application.source)
            );
        }
        assert!(!document.relations.iter().any(|relation| relation.subject
            == RelationEndpoint::Occurrence(application.id)
            && relation.predicate == RelationPredicate::Calls));
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .filter(|gap| gap.source == application.source
                    && gap.detail == "vba-call-or-index-target-unavailable")
                .count(),
            usize::from(!proven)
        );
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == application.provenance)
            .unwrap();
        if name == "Fetch" && proven {
            let function = function.unwrap();
            let mut parents = vec![FactRef::Fact(parser.id), FactRef::Entity(function.id)];
            for &(parameter, _, _) in &formals {
                parents.push(FactRef::Entity(parameter.id));
                let clause = parameter.evidence.source.as_ref().unwrap();
                assert!(proof.input_sources.contains(clause));
                assert!(proof.evidence_sources.contains(clause));
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
                assert!(proof.evidence_sources.contains(&definition.source));
            }
            parents.sort_unstable();
            parents.dedup();
            assert_eq!(proof.derivation_parents, parents);
            assert_eq!(
                proof.rule.as_deref(),
                Some("vba-procedure-application-v1.source")
            );
            assert_ne!(proof.id, parser.id);
            assert!(
                proof
                    .input_sources
                    .contains(file.evidence.source.as_ref().unwrap())
            );
            assert!(proof.evidence_sources.contains(&application.source));
            assert!(
                proof
                    .evidence_sources
                    .contains(function.evidence.source.as_ref().unwrap())
            );
        } else {
            assert_eq!(proof.id, parser.id);
            assert_eq!(proof.rule, None);
            assert!(!formals.iter().any(|(parameter, _, _)| {
                proof
                    .derivation_parents
                    .contains(&FactRef::Entity(parameter.id))
            }));
        }
        assert_eq!(proof.producer_kind, parser.producer_kind);
        assert_eq!(proof.binary_digest, parser.binary_digest);
        assert_eq!(proof.frontend_version, parser.frontend_version);
        for reference in proof.input_sources.iter().chain(&proof.evidence_sources) {
            assert_eq!(reference.repository(), receipt.repository);
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
            assert_eq!(reference.span().file(), file.id);
            assert!(reference.span().end_byte() <= u64::try_from(source.len()).unwrap());
        }
        let callees = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.invoked_get_identifier.reference"
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + name.len()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(callees.len(), 1);
        let callee = callees[0];
        assert_eq!(callee.enclosing, Some(run.id));
        assert_eq!(callee.provenance, parser.id);
        assert_eq!(
            callee.role,
            if callable {
                OccurrenceRole::CallSite
            } else {
                OccurrenceRole::Reference
            }
        );
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &callee.source,
            start..start + name.len(),
        );
        if let Some(symbol) = receiver {
            assert_eq!(callee.target, OccurrenceTarget::Resolved { symbol });
        } else {
            assert!(matches!(callee.target, OccurrenceTarget::Unresolved { .. }));
        }
        for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
            let edges = document
                .relations
                .iter()
                .filter(|relation| {
                    relation.subject == RelationEndpoint::Occurrence(callee.id)
                        && relation.predicate == predicate
                })
                .collect::<Vec<_>>();
            let present = receiver.is_some()
                && if callable {
                    predicate == RelationPredicate::Calls
                } else {
                    predicate == RelationPredicate::RefersTo
                };
            assert_eq!(edges.len(), usize::from(present));
            if present {
                assert_eq!(edges[0].object, RelationEndpoint::Entity(receiver.unwrap()));
                assert_eq!(edges[0].provenance, parser.id);
                assert_eq!(edges[0].evidence.source.as_ref(), Some(&callee.source));
            }
        }
        if callable {
            expected_call_sources.insert(callee.source.clone());
        }
    }
    let mut expected_reads = Vec::new();
    let run_start = source.find("Sub Run()").unwrap();
    for (offset, line) in source[run_start..]
        .split_inclusive('\n')
        .scan(run_start, |position, line| {
            let offset = *position;
            *position += line.len();
            Some((offset, line))
        })
        .filter(|(_, line)| line.starts_with("sink = "))
    {
        let expression_start = offset + "sink = ".len();
        let expression_end = offset + line.trim_end_matches('\n').len();
        let reference = source_reference(expression_start..expression_end);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &reference,
            expression_start..expression_end,
        );
        for name in ["value", "index"] {
            for (relative, _) in line["sink = ".len()..].match_indices(name) {
                let start = expression_start + relative;
                let reads = document
                    .occurrences
                    .iter()
                    .filter(|site| {
                        site.syntax_kind == "vba.value_identifier.reference"
                            && site.source.span().start_byte() == u64::try_from(start).unwrap()
                            && site.source.span().end_byte()
                                == u64::try_from(start + name.len()).unwrap()
                    })
                    .collect::<Vec<_>>();
                assert_eq!(reads.len(), 1);
                let read = reads[0];
                let target = local(name);
                assert_eq!(read.role, OccurrenceRole::Reference);
                assert_eq!(read.enclosing, Some(run.id));
                assert_eq!(read.provenance, parser.id);
                assert_eq!(
                    read.target,
                    OccurrenceTarget::Resolved { symbol: target.id }
                );
                assert!(
                    !formals
                        .iter()
                        .any(|(parameter, _, _)| parameter.id == target.id)
                );
                assert_exact_parameter_source(
                    service,
                    receipt,
                    source,
                    &read.source,
                    start..start + name.len(),
                );
                let edges = document
                    .relations
                    .iter()
                    .filter(|relation| {
                        relation.subject == RelationEndpoint::Occurrence(read.id)
                            && relation.predicate == RelationPredicate::RefersTo
                    })
                    .collect::<Vec<_>>();
                assert_eq!(edges.len(), 1);
                assert_eq!(edges[0].object, RelationEndpoint::Entity(target.id));
                assert_eq!(edges[0].provenance, parser.id);
                assert_eq!(edges[0].evidence.source.as_ref(), Some(&read.source));
                assert!(!document.relations.iter().any(|relation| relation.subject
                    == RelationEndpoint::Occurrence(read.id)
                    && relation.predicate == RelationPredicate::Calls));
                expected_reads.push((read, edges[0]));
            }
        }
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "vba.value_identifier.reference"
                && site.enclosing == Some(run.id))
            .count(),
        expected_reads.len()
    );
    let mut read_targets = BTreeSet::new();
    for name in ["value", "index"] {
        let target = local(name);
        read_targets.insert(target.id);
        let explained = service
            .symbol_explain(receipt.generation, target.id, &deadline())
            .unwrap();
        assert_eq!(&explained.data.entity, target);
        assert!(!explained.data.truncated);
        for &(read, relation) in &expected_reads {
            if read.target == (OccurrenceTarget::Resolved { symbol: target.id }) {
                assert!(explained.data.occurrences.iter().any(|site| site == read));
                assert!(explained.data.relations.iter().any(|edge| edge == relation));
            }
        }
    }
    let expected_references = document
        .relations
        .iter()
        .filter(|relation| {
            relation.predicate == RelationPredicate::RefersTo
                && matches!(relation.object, RelationEndpoint::Entity(target) if read_targets.contains(&target))
                && matches!(relation.subject, RelationEndpoint::Occurrence(id)
                    if document.occurrences.iter().any(|site| site.id == id && site.enclosing == Some(run.id)))
        })
        .collect::<Vec<_>>();
    let expected_reference_sources = expected_references
        .iter()
        .map(|relation| relation.evidence.source.clone().unwrap())
        .collect::<BTreeSet<_>>();
    assert_eq!(expected_reference_sources.len(), expected_references.len());
    assert!(
        expected_reads
            .iter()
            .all(|(read, _)| expected_reference_sources.contains(&read.source))
    );
    assert!(expected_references.len() <= 64);
    let references = service
        .symbol_relationships(
            receipt.generation,
            read_targets,
            vec![RelationFamily::References],
            Some(RelationDirection::Inbound),
            0,
            64,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(!references.data.truncated);
    assert_eq!(
        usize::try_from(references.data.returned_edges).unwrap(),
        expected_references.len()
    );
    let reference_items = references
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .collect::<Vec<_>>();
    assert_eq!(reference_items.len(), expected_references.len());
    assert_eq!(
        reference_items
            .iter()
            .flat_map(|item| item.source_refs.iter().cloned())
            .collect::<BTreeSet<_>>(),
        expected_reference_sources
    );
    for item in reference_items {
        assert_eq!(item.symbol, run.id);
        assert_eq!(item.source_refs.len(), 1);
    }
    for literal in ["\"left\"", "\"right\""] {
        let start = source.find(literal).unwrap();
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(start + literal.len()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1);
        assert_eq!(sites[0].role, OccurrenceRole::StringEvidence);
        assert_eq!(sites[0].provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &sites[0].source,
            start..start + literal.len(),
        );
    }
    let calls = document
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), expected_call_sources.len());
    assert_eq!(
        expected_call_sources.len(),
        if matches!(state.function, ExpressionFunctionTarget::Unique) {
            5
        } else {
            0
        }
    );
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([run.id]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            20,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(!relationships.data.truncated);
    let items = relationships
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .collect::<Vec<_>>();
    assert_eq!(items.len(), expected_call_sources.len());
    assert_eq!(
        items
            .iter()
            .flat_map(|item| item.source_refs.iter().cloned())
            .collect::<BTreeSet<_>>(),
        expected_call_sources
    );
    for item in items {
        assert_eq!(item.symbol, function.unwrap().id);
        assert_eq!(item.source_refs.len(), 1);
        for reference in &item.source_refs {
            let start = usize::try_from(reference.span().start_byte()).unwrap();
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                reference,
                start..start + "Fetch".len(),
            );
        }
    }
}

#[test]
fn expression_assignments_preserve_nested_proofs_and_generation_lifetimes() {
    let initial = ExpressionSourceState {
        declarations_source: "Private Buffer(2) As Long, Grid(0 To 2, 1 To 3) As Long\n",
        declarations: &[
            ("Buffer(2) As Long", "Buffer"),
            ("Grid(0 To 2, 1 To 3) As Long", "Grid"),
        ],
        buffer: ExpectedBufferApplication::Fixed(1),
        function: ExpressionFunctionTarget::Unique,
        operand: "2",
        native_fallback: false,
    };
    let states = [
        initial,
        ExpressionSourceState {
            operand: "3",
            ..initial
        },
        ExpressionSourceState {
            function: ExpressionFunctionTarget::Missing,
            ..initial
        },
        ExpressionSourceState {
            function: ExpressionFunctionTarget::Duplicate,
            ..initial
        },
        ExpressionSourceState {
            function: ExpressionFunctionTarget::LocalScalar,
            ..initial
        },
        ExpressionSourceState {
            declarations_source: "Private Buffer(2) As Long, [Buffer](0 To 2) As Long, Grid(0 To 2, 1 To 3) As Long\n",
            declarations: &[
                ("Buffer(2) As Long", "Buffer"),
                ("[Buffer](0 To 2) As Long", "Buffer"),
                ("Grid(0 To 2, 1 To 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::Ambiguous,
            ..initial
        },
        ExpressionSourceState {
            declarations_source: "Private Buffer As Long, Grid(0 To 2, 1 To 3) As Long\n",
            declarations: &[
                ("Buffer As Long", "Buffer"),
                ("Grid(0 To 2, 1 To 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::Scalar,
            ..initial
        },
        ExpressionSourceState {
            native_fallback: true,
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
        let configuration = (language == "vba").then_some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n");
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, ExpressionSourceState)> = None;
        let mut initial_source = None;
        let mut initial_logical = None;
        for (position, state) in states.into_iter().enumerate() {
            let source = expression_assignment_source(state);
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
            assert_expression_assignment_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                assert!(receipt.logical_snapshot.is_some());
                assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                if position == 1 {
                    assert_eq!(source, old_source.replace("-(value + 2)", "-(value + 3)"));
                }
                assert_expression_assignment_contract(
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
            assert_expression_assignment_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_expression_assignment_contract(
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
            assert_expression_assignment_contract(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if position + 1 == states.len() {
                assert_eq!(initial_source.as_ref(), Some(&source));
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            // Only the latest rebuilt predecessor is needed before the next
            // clean rebuild can legitimately evict it under retention two.
            previous = Some((rebuilt, source, state));
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ExpressionCallState {
    target: ExpressionFunctionTarget,
    arguments: &'static str,
    nested: &'static [&'static str],
    argument_count: usize,
}

fn expression_call_source(state: ExpressionCallState) -> String {
    let name = if matches!(state.target, ExpressionFunctionTarget::Missing) {
        "OtherTarget"
    } else {
        "Target"
    };
    let declaration = format!("Sub {name}(ByVal payload As Long, ByVal second As Long)\nEnd Sub\n");
    let target = if matches!(state.target, ExpressionFunctionTarget::Duplicate) {
        declaration.repeat(2)
    } else {
        declaration
    };
    let shadow = if matches!(state.target, ExpressionFunctionTarget::LocalScalar) {
        "Dim Target As Long\n"
    } else {
        ""
    };
    format!(
        "Attribute VB_Name = \"Authored\"\nOption Explicit\n{target}Function Inner(ByVal value As Long) As Long\nInner = value\nEnd Function\nSub Run()\nDim local As Long\n{shadow}local = 1\nCall Target({})\nEnd Sub\n",
        state.arguments
    )
}

fn assert_expression_call_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: ExpressionCallState,
) {
    assert_source_and_identity_for_language(service, receipt, source, language);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = &document.files[0];
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Authored")
        .unwrap();
    assert_eq!(
        module.container,
        Some(rootlight_ir::ContainerRef::File(file.id))
    );
    let run = document
        .entities
        .iter()
        .find(|entity| {
            entity.canonical_name == "Run"
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(module.id))
        })
        .unwrap();
    let parser = document
        .provenance
        .iter()
        .find(|proof| proof.id == run.provenance)
        .unwrap();
    let reference = |range: std::ops::Range<usize>| {
        SourceRef::new(
            receipt.repository,
            receipt.generation,
            SourceSpan::new(
                file.id,
                u64::try_from(range.start).unwrap(),
                u64::try_from(range.end).unwrap(),
            )
            .unwrap(),
            content_hash(source.as_bytes()),
            None,
        )
    };
    let assert_declaration = |entity: &rootlight_ir::EntityRecord,
                              owner,
                              range: std::ops::Range<usize>,
                              name: std::ops::Range<usize>| {
        assert_eq!(
            entity.container,
            Some(rootlight_ir::ContainerRef::Entity(owner))
        );
        assert_eq!(entity.provenance, parser.id);
        let definitions = document
            .occurrences
            .iter()
            .filter(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: entity.id }
                    && site.source.span() == reference(name.clone()).span()
            })
            .collect::<Vec<_>>();
        assert_eq!(definitions.len(), 1, "{state:?}: definition {name:?}");
        let definition = definitions[0];
        assert_eq!(definition.enclosing, Some(owner));
        assert_eq!(definition.provenance, parser.id);
        assert_exact_parameter_source(service, receipt, source, &definition.source, name);
        let clause = definition.evidence.source.as_ref().unwrap();
        assert_exact_parameter_source(service, receipt, source, clause, range);
        let contains = document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Entity(owner)
                    && relation.predicate == RelationPredicate::Contains
                    && relation.object == RelationEndpoint::Entity(entity.id)
                    && relation.evidence.source.as_ref() == Some(clause)
            })
            .collect::<Vec<_>>();
        assert_eq!(contains.len(), 1);
        assert_eq!(contains[0].provenance, parser.id);
        let explained = service
            .symbol_explain(receipt.generation, entity.id, &deadline())
            .unwrap();
        assert_eq!(&explained.data.entity, entity);
        assert!(!explained.data.truncated);
        assert!(
            explained
                .data
                .occurrences
                .iter()
                .any(|site| site == definition)
        );
        assert!(
            explained
                .data
                .relations
                .iter()
                .any(|edge| edge == contains[0])
        );
    };
    let target_name = if matches!(state.target, ExpressionFunctionTarget::Missing) {
        "OtherTarget"
    } else {
        "Target"
    };
    let target = document.entities.iter().find(|entity| {
        entity.kind == EntityKind::Function
            && entity.canonical_name == target_name
            && entity.container == run.container
    });
    let inner = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Function
                && entity.canonical_name == "Inner"
                && entity.container == run.container
        })
        .unwrap();
    let mut formals = BTreeMap::new();
    for (name, header, terminator, written, entity) in [
        (
            target_name,
            format!("Sub {target_name}("),
            "End Sub",
            &[
                ("ByVal payload As Long", "payload"),
                ("ByVal second As Long", "second"),
            ][..],
            target,
        ),
        (
            "Inner",
            "Function Inner(".to_owned(),
            "End Function",
            &[("ByVal value As Long", "value")][..],
            Some(inner),
        ),
    ] {
        let headers = source
            .match_indices(&header)
            .map(|(start, _)| start)
            .collect::<Vec<_>>();
        let quarantined =
            name == "Target" && matches!(state.target, ExpressionFunctionTarget::Duplicate);
        assert_eq!(headers.len(), if quarantined { 2 } else { 1 });
        let mut expected_gaps = BTreeSet::new();
        if quarantined {
            assert!(entity.is_none());
            assert!(!document.entities.iter().any(|entity| {
                entity.kind == EntityKind::Parameter
                    && matches!(entity.canonical_name.as_str(), "payload" | "second")
            }));
        }
        for &start in &headers {
            let end = start + source[start..].find(terminator).unwrap() + terminator.len();
            let name_start = start + header.find(name).unwrap();
            let mut declarations = vec![(start..end, name_start..name_start + name.len())];
            for &(clause, parameter_name) in written {
                let clause_start = start + source[start..end].find(clause).unwrap();
                let parameter_start = clause_start + clause.find(parameter_name).unwrap();
                declarations.push((
                    clause_start..clause_start + clause.len(),
                    parameter_start..parameter_start + parameter_name.len(),
                ));
            }
            if quarantined {
                // Ambiguous procedure scopes retain declarations as diagnostic
                // anchors, not first-wins callable or parameter identities.
                for (range, token) in declarations {
                    let expected = reference(range.clone());
                    let gaps = document
                        .skipped_regions
                        .iter()
                        .filter(|gap| {
                            gap.source.span() == expected.span()
                                && gap.detail == "stable-scope-identity-unavailable"
                        })
                        .collect::<Vec<_>>();
                    assert_eq!(gaps.len(), 1, "{state:?}: quarantine {range:?}");
                    let gap = gaps[0];
                    assert_eq!(gap.domain, rootlight_ir::FactDomain::Entities);
                    assert_eq!(
                        gap.reason,
                        rootlight_ir::SkippedRegionReason::UnsupportedConstruct
                    );
                    assert_eq!(gap.repository, receipt.repository);
                    assert_eq!(gap.generation, receipt.generation);
                    assert_eq!(gap.provenance, parser.id);
                    assert_eq!(gap.evidence.source.as_ref(), Some(&gap.source));
                    assert_exact_parameter_source(
                        service,
                        receipt,
                        source,
                        &gap.source,
                        range.clone(),
                    );
                    assert!(range.start <= token.start && token.end <= range.end);
                    let token_source = reference(token.clone());
                    assert_exact_parameter_source(service, receipt, source, &token_source, token);
                    assert!(!document.occurrences.iter().any(|site| {
                        site.role == OccurrenceRole::Definition
                            && site.source.span() == token_source.span()
                    }));
                    assert!(!document.relations.iter().any(|edge| {
                        edge.predicate == RelationPredicate::Contains
                            && edge
                                .evidence
                                .source
                                .as_ref()
                                .is_some_and(|source| source.span() == expected.span())
                    }));
                    assert!(expected_gaps.insert(expected.span()));
                }
            } else {
                let entity = entity.unwrap();
                assert_exact_parameter_source(
                    service,
                    receipt,
                    source,
                    entity.evidence.source.as_ref().unwrap(),
                    start..end,
                );
                assert_declaration(
                    entity,
                    module.id,
                    declarations[0].0.clone(),
                    declarations[0].1.clone(),
                );
                let parameters = document
                    .entities
                    .iter()
                    .filter(|parameter| {
                        parameter.kind == EntityKind::Parameter
                            && parameter.container
                                == Some(rootlight_ir::ContainerRef::Entity(entity.id))
                    })
                    .collect::<Vec<_>>();
                assert_eq!(parameters.len(), written.len());
                for ((_, parameter_name), (range, token)) in written.iter().zip(&declarations[1..])
                {
                    let parameter = parameters
                        .iter()
                        .find(|parameter| parameter.canonical_name == *parameter_name)
                        .unwrap();
                    assert_exact_parameter_source(
                        service,
                        receipt,
                        source,
                        parameter.evidence.source.as_ref().unwrap(),
                        range.clone(),
                    );
                    assert_declaration(parameter, entity.id, range.clone(), token.clone());
                }
                assert!(formals.insert(entity.id, parameters).is_none());
            }
        }
        if quarantined {
            assert_eq!(expected_gaps.len(), 6);
            let actual = document
                .skipped_regions
                .iter()
                .filter(|gap| gap.detail == "stable-scope-identity-unavailable")
                .map(|gap| gap.source.span())
                .collect::<Vec<_>>();
            assert_eq!(actual.len(), expected_gaps.len());
            assert_eq!(actual.into_iter().collect::<BTreeSet<_>>(), expected_gaps);
        }
    }
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::Parameter)
            .count(),
        if target.is_some() { 3 } else { 1 }
    );
    let local = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Variable
                && entity.canonical_name == "local"
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(run.id))
        })
        .unwrap();
    let local_start = source.find("Dim local As Long").unwrap() + "Dim ".len();
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        local.evidence.source.as_ref().unwrap(),
        local_start..local_start + "local As Long".len(),
    );
    assert_declaration(
        local,
        run.id,
        local_start..local_start + "local As Long".len(),
        local_start..local_start + "local".len(),
    );
    let shadow = document.entities.iter().find(|entity| {
        entity.kind == EntityKind::Variable
            && entity.canonical_name == "Target"
            && entity.container == Some(rootlight_ir::ContainerRef::Entity(run.id))
    });
    assert_eq!(
        shadow.is_some(),
        matches!(state.target, ExpressionFunctionTarget::LocalScalar)
    );
    if let Some(shadow) = shadow {
        let start = source.find("Dim Target As Long").unwrap() + "Dim ".len();
        assert_declaration(
            shadow,
            run.id,
            start..start + "Target As Long".len(),
            start..start + "Target".len(),
        );
    }
    for name in [
        "Target",
        "OtherTarget",
        "Inner",
        "payload",
        "second",
        "value",
        "local",
    ] {
        let located = service
            .code_locate(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                20,
                0,
                &deadline(),
            )
            .unwrap();
        assert_eq!(
            located
                .data
                .hits
                .iter()
                .filter_map(|hit| hit.symbol)
                .collect::<BTreeSet<_>>(),
            document
                .entities
                .iter()
                .filter(|entity| entity.canonical_name == name)
                .map(|entity| entity.id)
                .collect::<BTreeSet<_>>()
        );
        for hit in located.data.hits.iter().filter(|hit| hit.symbol.is_some()) {
            let entity = document
                .entities
                .iter()
                .find(|entity| Some(entity.id) == hit.symbol)
                .unwrap();
            assert_eq!(hit.source.as_ref(), entity.evidence.source.as_ref());
        }
    }
    let statement = format!("Call Target({})", state.arguments);
    let statement_start = source.find(&statement).unwrap();
    let statement_end = statement_start + statement.len();
    let outer_callable = matches!(state.target, ExpressionFunctionTarget::Unique);
    let outer_receiver = if outer_callable {
        Some(target.unwrap().id)
    } else {
        shadow.map(|entity| entity.id)
    };
    let mut probes = vec![(
        statement_start..statement_end,
        statement_start + "Call ".len(),
        "Target",
        true,
        outer_receiver,
        outer_callable,
        outer_callable && state.argument_count == 2,
    )];
    for &application in state.nested {
        let start = statement_start + statement.find(application).unwrap();
        probes.push((
            start..start + application.len(),
            start,
            "Inner",
            false,
            Some(inner.id),
            true,
            true,
        ));
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "vba.application.reference")
            .count(),
        probes.len()
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "vba.invoked_identifier.reference")
            .count(),
        1
    );
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| site.syntax_kind == "vba.invoked_get_identifier.reference")
            .count(),
        state.nested.len()
    );
    let mut expected_calls = BTreeMap::new();
    for (range, callee_start, name, outer, receiver, callable, proven) in probes {
        let sites = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span() == reference(range.clone()).span()
            })
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1, "{state:?}: application {range:?}");
        let application = sites[0];
        assert_eq!(application.role, OccurrenceRole::Reference);
        assert_eq!(application.enclosing, Some(run.id));
        assert_exact_parameter_source(service, receipt, source, &application.source, range);
        if proven {
            assert_eq!(
                application.target,
                OccurrenceTarget::Resolved {
                    symbol: receiver.unwrap()
                }
            );
        } else {
            assert!(matches!(
                application.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
        let refers = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(application.id)
                    && edge.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        assert_eq!(refers.len(), usize::from(proven));
        if proven {
            assert_eq!(
                refers[0].object,
                RelationEndpoint::Entity(receiver.unwrap())
            );
            assert_eq!(refers[0].provenance, application.provenance);
            assert_eq!(
                refers[0].evidence.source.as_ref(),
                Some(&application.source)
            );
            let explained = service
                .symbol_explain(receipt.generation, receiver.unwrap(), &deadline())
                .unwrap();
            assert!(!explained.data.truncated);
            assert!(
                explained
                    .data
                    .occurrences
                    .iter()
                    .any(|site| site == application)
            );
            assert!(
                explained
                    .data
                    .relations
                    .iter()
                    .any(|edge| edge == refers[0])
            );
        }
        assert!(!document.relations.iter().any(|edge| edge.subject
            == RelationEndpoint::Occurrence(application.id)
            && edge.predicate == RelationPredicate::Calls));
        let gaps = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.source == application.source
                    && gap.detail == "vba-call-or-index-target-unavailable"
            })
            .collect::<Vec<_>>();
        assert_eq!(gaps.len(), usize::from(!proven));
        for gap in gaps {
            assert_eq!(gap.provenance, parser.id);
            assert_eq!(gap.evidence.source.as_ref(), Some(&application.source));
        }
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == application.provenance)
            .unwrap();
        if proven {
            let target_id = receiver.unwrap();
            let entity = document
                .entities
                .iter()
                .find(|entity| entity.id == target_id)
                .unwrap();
            let mut parents = vec![FactRef::Fact(parser.id), FactRef::Entity(target_id)];
            for &parameter in &formals[&target_id] {
                parents.push(FactRef::Entity(parameter.id));
                let clause = parameter.evidence.source.as_ref().unwrap();
                assert!(proof.input_sources.contains(clause));
                assert!(proof.evidence_sources.contains(clause));
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
                assert!(proof.evidence_sources.contains(&definition.source));
            }
            parents.sort_unstable();
            parents.dedup();
            assert_eq!(proof.derivation_parents, parents);
            assert_eq!(
                proof.rule.as_deref(),
                Some("vba-procedure-application-v1.source")
            );
            assert!(
                proof
                    .input_sources
                    .contains(file.evidence.source.as_ref().unwrap())
            );
            assert!(
                proof
                    .evidence_sources
                    .contains(entity.evidence.source.as_ref().unwrap())
            );
            assert!(proof.evidence_sources.contains(&application.source));
        } else {
            assert_eq!(proof.id, parser.id);
            assert_eq!(proof.rule, None);
        }
        assert_eq!(proof.producer_kind, parser.producer_kind);
        assert_eq!(proof.binary_digest, parser.binary_digest);
        assert_eq!(proof.frontend_version, parser.frontend_version);
        for source_ref in proof.input_sources.iter().chain(&proof.evidence_sources) {
            assert_eq!(source_ref.repository(), receipt.repository);
            assert_eq!(source_ref.generation(), receipt.generation);
            assert_eq!(source_ref.content_hash(), content_hash(source.as_bytes()));
            assert_eq!(source_ref.span().file(), file.id);
            assert!(source_ref.span().end_byte() <= u64::try_from(source.len()).unwrap());
        }
        let label = if outer {
            "vba.invoked_identifier.reference"
        } else {
            "vba.invoked_get_identifier.reference"
        };
        let callees = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == label
                    && site.source.span()
                        == reference(callee_start..callee_start + name.len()).span()
            })
            .collect::<Vec<_>>();
        assert_eq!(callees.len(), 1);
        let callee = callees[0];
        assert_eq!(callee.enclosing, Some(run.id));
        assert_eq!(callee.provenance, parser.id);
        assert_eq!(
            callee.role,
            if callable {
                OccurrenceRole::CallSite
            } else {
                OccurrenceRole::Reference
            }
        );
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &callee.source,
            callee_start..callee_start + name.len(),
        );
        if let Some(symbol) = receiver {
            assert_eq!(callee.target, OccurrenceTarget::Resolved { symbol });
        } else {
            assert!(matches!(callee.target, OccurrenceTarget::Unresolved { .. }));
        }
        let explained_callee = receiver.map(|symbol| {
            let explained = service
                .symbol_explain(receipt.generation, symbol, &deadline())
                .unwrap();
            assert!(!explained.data.truncated);
            assert!(explained.data.occurrences.iter().any(|site| site == callee));
            explained
        });
        for predicate in [RelationPredicate::Calls, RelationPredicate::RefersTo] {
            let present = receiver.is_some()
                && if callable {
                    predicate == RelationPredicate::Calls
                } else {
                    predicate == RelationPredicate::RefersTo
                };
            let edges = document
                .relations
                .iter()
                .filter(|edge| {
                    edge.subject == RelationEndpoint::Occurrence(callee.id)
                        && edge.predicate == predicate
                })
                .collect::<Vec<_>>();
            assert_eq!(edges.len(), usize::from(present));
            if present {
                assert_eq!(edges[0].object, RelationEndpoint::Entity(receiver.unwrap()));
                assert_eq!(edges[0].provenance, parser.id);
                assert_eq!(edges[0].evidence.source.as_ref(), Some(&callee.source));
                assert!(
                    explained_callee
                        .as_ref()
                        .unwrap()
                        .data
                        .relations
                        .iter()
                        .any(|edge| edge == edges[0])
                );
            }
        }
        if callable {
            assert!(
                expected_calls
                    .insert(callee.source.clone(), receiver.unwrap())
                    .is_none()
            );
        }
    }
    let reads = document
        .occurrences
        .iter()
        .filter(|site| {
            site.syntax_kind == "vba.value_identifier.reference" && site.enclosing == Some(run.id)
        })
        .collect::<Vec<_>>();
    let written_reads = statement
        .match_indices("local")
        .map(|(offset, _)| statement_start + offset)
        .collect::<Vec<_>>();
    assert_eq!(reads.len(), written_reads.len());
    let explained = service
        .symbol_explain(receipt.generation, local.id, &deadline())
        .unwrap();
    assert_eq!(&explained.data.entity, local);
    assert!(!explained.data.truncated);
    for start in written_reads {
        let sites = reads
            .iter()
            .filter(|site| site.source.span() == reference(start..start + "local".len()).span())
            .collect::<Vec<_>>();
        assert_eq!(sites.len(), 1);
        let site = sites[0];
        assert_eq!(site.role, OccurrenceRole::Reference);
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: local.id });
        assert_eq!(site.provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &site.source,
            start..start + "local".len(),
        );
        assert!(
            explained
                .data
                .occurrences
                .iter()
                .any(|candidate| candidate == *site)
        );
        let edges = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].object, RelationEndpoint::Entity(local.id));
        assert_eq!(edges[0].provenance, parser.id);
        assert_eq!(edges[0].evidence.source.as_ref(), Some(&site.source));
        assert!(explained.data.relations.iter().any(|edge| edge == edges[0]));
        assert!(
            !document
                .relations
                .iter()
                .any(|edge| edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == RelationPredicate::Calls)
        );
    }
    let call_edges = document
        .relations
        .iter()
        .filter(|edge| edge.predicate == RelationPredicate::Calls)
        .collect::<Vec<_>>();
    assert_eq!(call_edges.len(), expected_calls.len());
    assert_eq!(
        expected_calls.len(),
        state.nested.len() + usize::from(outer_callable)
    );
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([run.id]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            20,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(!relationships.data.truncated);
    let items = relationships
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .collect::<Vec<_>>();
    assert_eq!(items.len(), expected_calls.len());
    assert_eq!(
        items
            .iter()
            .flat_map(|item| item.source_refs.iter().cloned())
            .collect::<BTreeSet<_>>(),
        expected_calls.keys().cloned().collect::<BTreeSet<_>>()
    );
    for item in items {
        assert_eq!(item.source_refs.len(), 1);
        assert_eq!(Some(&item.symbol), expected_calls.get(&item.source_refs[0]));
        let span = item.source_refs[0].span();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &item.source_refs[0],
            usize::try_from(span.start_byte()).unwrap()..usize::try_from(span.end_byte()).unwrap(),
        );
    }
}

#[test]
fn explicit_call_expressions_preserve_nested_targets_and_generation_lifetimes() {
    let initial = ExpressionCallState {
        target: ExpressionFunctionTarget::Unique,
        arguments: "Inner(local + 2), Inner(Inner(local))",
        nested: &["Inner(local + 2)", "Inner(Inner(local))", "Inner(local)"],
        argument_count: 2,
    };
    let states = [
        initial,
        ExpressionCallState {
            arguments: "Inner(local + 3), Inner(Inner(local))",
            nested: &["Inner(local + 3)", "Inner(Inner(local))", "Inner(local)"],
            ..initial
        },
        ExpressionCallState {
            target: ExpressionFunctionTarget::Missing,
            ..initial
        },
        ExpressionCallState {
            target: ExpressionFunctionTarget::Duplicate,
            ..initial
        },
        ExpressionCallState {
            target: ExpressionFunctionTarget::LocalScalar,
            ..initial
        },
        ExpressionCallState {
            arguments: "Inner(local + 2)",
            nested: &["Inner(local + 2)"],
            argument_count: 1,
            ..initial
        },
        ExpressionCallState {
            arguments: "Inner(local + 2), Inner(Inner(local)), 9",
            argument_count: 3,
            ..initial
        },
        ExpressionCallState {
            arguments: "Inner(local + &H2), Inner(Inner(local))",
            nested: &["Inner(local + &H2)", "Inner(Inner(local))", "Inner(local)"],
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
        let configuration = (language == "vba").then_some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n");
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, ExpressionCallState)> = None;
        let mut initial_source = None;
        let mut initial_logical = None;
        for (position, state) in states.into_iter().enumerate() {
            let source = expression_call_source(state);
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
            assert_expression_call_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                assert!(receipt.logical_snapshot.is_some());
                assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                if position == 1 {
                    assert_eq!(source, old_source.replace("local + 2", "local + 3"));
                }
                assert_expression_call_contract(
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
            assert_expression_call_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_expression_call_contract(
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
            assert_expression_call_contract(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if position + 1 == states.len() {
                assert_eq!(initial_source.as_ref(), Some(&source));
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            previous = Some((rebuilt, source, state));
        }
    }
}

#[derive(Clone, Copy)]
struct LocalDimState {
    declarations_source: &'static str,
    declarations: WrittenParameters<'static>,
    buffer: ExpectedBufferApplication,
    native_fallback: bool,
}

fn local_dim_source(state: LocalDimState) -> String {
    format!(
        "Attribute VB_Name = \"Authored\"\nOption Explicit\nPrivate Buffer(2) As Long\nFunction Fetch(ByVal value As Long, Optional extra As Long = 7) As Long\nFetch = value\nEnd Function\nSub Run()\n{}index = 0\nsink = spare\nsink = Buffer()\nsink = Buffer(index)\nsink = Buffer(index, 1)\nsink = dynamic(index)\nsink = dynamic(index, 1)\nsink = Grid(0)\nsink = Grid(0, 1)\nsink = Fetch()\nsink = Fetch(index)\nsink = Fetch(index, 7, 8)\n{}End Sub\n",
        state.declarations_source,
        if state.native_fallback {
            "sink = &H1\n"
        } else {
            ""
        },
    )
}

fn assert_local_dim_contract(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    source: &str,
    language: &str,
    state: LocalDimState,
) {
    assert_source_and_identity_for_language(service, receipt, source, language);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = &document.files[0];
    let module = document
        .entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Module && entity.canonical_name == "Authored")
        .unwrap();
    assert_eq!(
        module.container,
        Some(rootlight_ir::ContainerRef::File(file.id))
    );
    let run = document
        .entities
        .iter()
        .find(|entity| {
            entity.canonical_name == "Run"
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(module.id))
        })
        .unwrap();
    let fetch = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Function
                && entity.canonical_name == "Fetch"
                && entity.container == run.container
        })
        .unwrap();
    let parser = document
        .provenance
        .iter()
        .find(|proof| proof.id == run.provenance)
        .unwrap();
    assert_eq!(parser.producer_kind, ProducerKind::Parser);
    assert_eq!(fetch.provenance, parser.id);
    let module_buffer = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Variable
                && entity.canonical_name == "Buffer"
                && entity.container == run.container
        })
        .unwrap();
    let module_start = source.find("Private Buffer(2) As Long").unwrap() + "Private ".len();
    assert_exact_parameter_source(
        service,
        receipt,
        source,
        module_buffer.evidence.source.as_ref().unwrap(),
        module_start..module_start + "Buffer(2) As Long".len(),
    );

    let locals = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Variable
                && entity.container == Some(rootlight_ir::ContainerRef::Entity(run.id))
        })
        .collect::<Vec<_>>();
    let names = state
        .declarations
        .iter()
        .map(|(_, name)| *name)
        .collect::<BTreeSet<_>>();
    assert_eq!(
        locals.len(),
        names.len(),
        "local identity inventory: {}",
        state.declarations_source
    );
    let local = |name: &str| {
        *locals
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap()
    };
    let body_start = source.find("Sub Run()\n").unwrap() + "Sub Run()\n".len();
    let mut cursor = body_start;
    let ranges = state
        .declarations
        .iter()
        .map(|(written, _)| {
            let start = cursor + source[cursor..].find(written).unwrap();
            cursor = start + written.len();
            start..cursor
        })
        .collect::<Vec<_>>();
    let definitions = document
        .occurrences
        .iter()
        .filter(|site| {
            site.role == OccurrenceRole::Definition
                && locals
                    .iter()
                    .any(|entity| site.target == OccurrenceTarget::Resolved { symbol: entity.id })
        })
        .collect::<Vec<_>>();
    assert_eq!(definitions.len(), state.declarations.len());
    for (&(written, name), range) in state.declarations.iter().zip(&ranges) {
        let variable = local(name);
        assert_eq!(variable.provenance, parser.id);
        assert_eq!(variable.visibility, rootlight_ir::EntityVisibility::Unknown);
        let representative = state
            .declarations
            .iter()
            .zip(&ranges)
            .find(|((_, candidate), _)| *candidate == name)
            .unwrap()
            .1;
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            variable.evidence.source.as_ref().unwrap(),
            representative.clone(),
        );
        let token_len = if written.starts_with('[') {
            written.find(']').unwrap() + 1
        } else {
            name.len()
        };
        assert_eq!(written[..token_len].trim_matches(['[', ']']), name);
        let matching = definitions
            .iter()
            .filter(|site| {
                site.target
                    == OccurrenceTarget::Resolved {
                        symbol: variable.id,
                    }
                    && site.source.span().start_byte() == u64::try_from(range.start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(range.start + token_len).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1, "local Definition: {written}");
        let definition = matching[0];
        assert_eq!(definition.syntax_kind, "vba.identifier.definition");
        assert_eq!(definition.enclosing, Some(run.id));
        assert_eq!(definition.provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &definition.source,
            range.start..range.start + token_len,
        );
        let clause = definition.evidence.source.as_ref().unwrap();
        assert_exact_parameter_source(service, receipt, source, clause, range.clone());
        let contains = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(run.id)
                    && relation.object == RelationEndpoint::Entity(variable.id)
                    && relation.evidence.source.as_ref() == Some(clause)
            })
            .collect::<Vec<_>>();
        assert_eq!(contains.len(), 1, "local Contains: {written}");
        assert_eq!(contains[0].provenance, parser.id);
        let explained = service
            .symbol_explain(receipt.generation, variable.id, &deadline())
            .unwrap();
        assert_eq!(&explained.data.entity, variable);
        assert!(!explained.data.truncated);
        assert!(
            explained
                .data
                .occurrences
                .iter()
                .any(|site| site == *definition)
        );
        assert!(
            explained
                .data
                .relations
                .iter()
                .any(|relation| relation == contains[0])
        );
        if let Some(start) = written.find('(') {
            let end = written.find(')').unwrap() + 1;
            let bounds = SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    file.id,
                    u64::try_from(range.start + start).unwrap(),
                    u64::try_from(range.start + end).unwrap(),
                )
                .unwrap(),
                content_hash(source.as_bytes()),
                None,
            );
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                &bounds,
                range.start + start..range.start + end,
            );
        }
        let within = |reference: &SourceRef| {
            reference.span().file() == file.id
                && reference.span().start_byte() >= u64::try_from(range.start).unwrap()
                && reference.span().end_byte() <= u64::try_from(range.end).unwrap()
        };
        if let Some(offset) = written.find(" As ") {
            let type_start = range.start + offset + " As ".len();
            let reference = SourceRef::new(
                receipt.repository,
                receipt.generation,
                SourceSpan::new(
                    file.id,
                    u64::try_from(type_start).unwrap(),
                    u64::try_from(range.end).unwrap(),
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
                type_start..range.end,
            );
        } else {
            assert!(
                !document
                    .occurrences
                    .iter()
                    .any(|site| within(&site.source) && site.role == OccurrenceRole::TypeUse)
            );
            assert!(!document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::UsesType
                    && (relation.subject == RelationEndpoint::Entity(variable.id)
                        || relation.evidence.source.as_ref().is_some_and(within))
            }));
            assert!(!document.entities.iter().any(|entity| {
                entity.canonical_name.eq_ignore_ascii_case("Variant")
                    && entity.evidence.source.as_ref().is_some_and(within)
            }));
        }
    }
    for name in &names {
        let located = service
            .code_locate(
                receipt.generation,
                (*name).to_owned(),
                LocateMode::Exact,
                10,
                0,
                &deadline(),
            )
            .unwrap();
        assert!(!located.data.truncated);
        let expected = document
            .entities
            .iter()
            .filter(|entity| entity.canonical_name == *name)
            .map(|entity| entity.id)
            .collect::<BTreeSet<_>>();
        assert_eq!(
            located
                .data
                .hits
                .iter()
                .filter_map(|hit| hit.symbol)
                .collect::<BTreeSet<_>>(),
            expected
        );
        for (ordinal, hit) in located
            .data
            .hits
            .iter()
            .enumerate()
            .filter(|(_, hit)| hit.symbol.is_some())
        {
            let symbol = hit.symbol.unwrap();
            let matching = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.role == OccurrenceRole::Definition
                        && site.target == OccurrenceTarget::Resolved { symbol }
                })
                .collect::<Vec<_>>();
            let assessment = located
                .data
                .conditional_activity
                .iter()
                .find(|assessment| assessment.result_index == u32::try_from(ordinal).unwrap())
                .unwrap();
            assert_eq!(assessment.basis, ConditionalActivityBasis::DefinitionToken);
            if matching.len() > 1 {
                assert_eq!(assessment.activity, ConditionalActivityState::Unproven);
                assert_eq!(
                    assessment.reason,
                    Some(ConditionalActivityReason::AmbiguousDefinition)
                );
                assert!(assessment.assessed_source_ref.is_none());
                assert!(assessment.inventory.is_none());
            } else {
                assert_eq!(matching.len(), 1);
                assert_eq!(assessment.activity, ConditionalActivityState::Active);
                assert_eq!(assessment.reason, None);
                assert_eq!(
                    assessment.assessed_source_ref.as_ref(),
                    Some(&matching[0].source)
                );
                assert_eq!(
                    assessment.inventory.as_ref().unwrap().parser_provenance_id,
                    parser.id
                );
            }
        }
    }

    let formals = [
        ("ByVal value As Long", "value"),
        ("Optional extra As Long = 7", "extra"),
    ]
    .into_iter()
    .map(|(written, name)| {
        let formal = document
            .entities
            .iter()
            .find(|entity| {
                entity.kind == EntityKind::Parameter
                    && entity.canonical_name == name
                    && entity.container == Some(rootlight_ir::ContainerRef::Entity(fetch.id))
            })
            .unwrap();
        assert_eq!(formal.provenance, parser.id);
        let start = source.find(written).unwrap();
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            formal.evidence.source.as_ref().unwrap(),
            start..start + written.len(),
        );
        let name_start = start + written.find(name).unwrap();
        let definition = document
            .occurrences
            .iter()
            .find(|site| {
                site.role == OccurrenceRole::Definition
                    && site.target == OccurrenceTarget::Resolved { symbol: formal.id }
                    && site.source.span().start_byte() == u64::try_from(name_start).unwrap()
                    && site.source.span().end_byte()
                        == u64::try_from(name_start + name.len()).unwrap()
            })
            .unwrap();
        assert_eq!(definition.enclosing, Some(fetch.id));
        assert_eq!(definition.provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &definition.source,
            name_start..name_start + name.len(),
        );
        let contains = document
            .relations
            .iter()
            .filter(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(fetch.id)
                    && relation.object == RelationEndpoint::Entity(formal.id)
                    && relation.evidence.source.as_ref() == formal.evidence.source.as_ref()
            })
            .collect::<Vec<_>>();
        assert_eq!(contains.len(), 1);
        assert_eq!(contains[0].provenance, parser.id);
        (formal, definition)
    })
    .collect::<Vec<_>>();
    let buffer = match state.buffer {
        ExpectedBufferApplication::Missing => Some(module_buffer.id),
        ExpectedBufferApplication::Ambiguous => None,
        _ => Some(local("Buffer").id),
    };
    let mut expected_calls = BTreeMap::new();
    for (expression, name, count) in [
        ("Buffer()", "Buffer", 0),
        ("Buffer(index)", "Buffer", 1),
        ("Buffer(index, 1)", "Buffer", 2),
        ("dynamic(index)", "dynamic", 1),
        ("dynamic(index, 1)", "dynamic", 2),
        ("Grid(0)", "Grid", 1),
        ("Grid(0, 1)", "Grid", 2),
        ("Fetch()", "Fetch", 0),
        ("Fetch(index)", "Fetch", 1),
        ("Fetch(index, 7, 8)", "Fetch", 3),
    ] {
        let is_function = name == "Fetch";
        let receiver = match name {
            "Fetch" => Some(fetch.id),
            "Buffer" => buffer,
            _ => Some(local(name).id),
        };
        // This proves written rank/cardinality only, not runtime element bounds or types.
        let proven = match name {
            "Fetch" => count == 1,
            "dynamic" => true,
            "Grid" => count == 2,
            "Buffer" => match state.buffer {
                ExpectedBufferApplication::Fixed(rank) => count == 0 || count == rank,
                ExpectedBufferApplication::Unspecified => true,
                ExpectedBufferApplication::Missing => count == 0 || count == 1,
                ExpectedBufferApplication::Scalar
                | ExpectedBufferApplication::LocalScalar
                | ExpectedBufferApplication::Ambiguous => false,
            },
            _ => unreachable!(),
        };
        let start = source.find(&format!("sink = {expression}\n")).unwrap() + "sink = ".len();
        let end = start + expression.len();
        let applications = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.application.reference"
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(end).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            applications.len(),
            1,
            "local array/value application: {expression}"
        );
        let application = applications[0];
        assert_eq!(application.role, OccurrenceRole::Reference);
        assert_eq!(application.enclosing, Some(run.id));
        assert_exact_parameter_source(service, receipt, source, &application.source, start..end);
        if proven {
            assert_eq!(
                application.target,
                OccurrenceTarget::Resolved {
                    symbol: receiver.unwrap()
                }
            );
        } else {
            assert!(matches!(
                application.target,
                OccurrenceTarget::Unresolved { .. }
            ));
        }
        let refers = document
            .relations
            .iter()
            .filter(|relation| {
                relation.subject == RelationEndpoint::Occurrence(application.id)
                    && relation.predicate == RelationPredicate::RefersTo
            })
            .collect::<Vec<_>>();
        assert_eq!(
            refers.len(),
            usize::from(proven),
            "application RefersTo: {expression}"
        );
        if proven {
            assert_eq!(
                refers[0].object,
                RelationEndpoint::Entity(receiver.unwrap())
            );
            assert_eq!(refers[0].provenance, application.provenance);
            assert_eq!(
                refers[0].evidence.source.as_ref(),
                Some(&application.source)
            );
        }
        assert!(!document.relations.iter().any(|relation| {
            relation.subject == RelationEndpoint::Occurrence(application.id)
                && relation.predicate == RelationPredicate::Calls
        }));
        assert_eq!(
            document
                .skipped_regions
                .iter()
                .filter(|gap| {
                    gap.source == application.source
                        && gap.detail == "vba-call-or-index-target-unavailable"
                })
                .count(),
            usize::from(!proven)
        );
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == application.provenance)
            .unwrap();
        if is_function && proven {
            let mut parents = vec![FactRef::Fact(parser.id), FactRef::Entity(fetch.id)];
            for (formal, definition) in &formals {
                parents.push(FactRef::Entity(formal.id));
                let clause = formal.evidence.source.as_ref().unwrap();
                assert!(proof.input_sources.contains(clause));
                assert!(proof.evidence_sources.contains(clause));
                assert!(proof.evidence_sources.contains(&definition.source));
            }
            parents.sort_unstable();
            parents.dedup();
            assert_eq!(proof.derivation_parents, parents);
            assert_eq!(
                proof.rule.as_deref(),
                Some("vba-procedure-application-v1.source")
            );
            assert!(
                proof
                    .input_sources
                    .contains(file.evidence.source.as_ref().unwrap())
            );
            assert!(proof.evidence_sources.contains(&application.source));
            assert!(
                proof
                    .evidence_sources
                    .contains(fetch.evidence.source.as_ref().unwrap())
            );
        } else {
            assert_eq!(proof.id, parser.id);
            assert_eq!(proof.rule, None);
        }
        for reference in proof.input_sources.iter().chain(&proof.evidence_sources) {
            assert_eq!(reference.repository(), receipt.repository);
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.content_hash(), content_hash(source.as_bytes()));
            assert_eq!(reference.span().file(), file.id);
            assert!(reference.span().end_byte() <= u64::try_from(source.len()).unwrap());
        }
        let callees = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.invoked_get_identifier.reference"
                    && site.source.span().start_byte() == u64::try_from(start).unwrap()
                    && site.source.span().end_byte() == u64::try_from(start + name.len()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(callees.len(), 1, "value callee: {expression}");
        let callee = callees[0];
        assert_eq!(
            callee.role,
            if is_function {
                OccurrenceRole::CallSite
            } else {
                OccurrenceRole::Reference
            }
        );
        assert_eq!(callee.enclosing, Some(run.id));
        assert_eq!(callee.provenance, parser.id);
        assert_exact_parameter_source(
            service,
            receipt,
            source,
            &callee.source,
            start..start + name.len(),
        );
        if let Some(symbol) = receiver {
            assert_eq!(callee.target, OccurrenceTarget::Resolved { symbol });
        } else {
            assert!(matches!(callee.target, OccurrenceTarget::Unresolved { .. }));
        }
        for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
            let relations = document
                .relations
                .iter()
                .filter(|relation| {
                    relation.subject == RelationEndpoint::Occurrence(callee.id)
                        && relation.predicate == predicate
                })
                .collect::<Vec<_>>();
            let present = receiver.is_some()
                && if is_function {
                    predicate == RelationPredicate::Calls
                } else {
                    predicate == RelationPredicate::RefersTo
                };
            assert_eq!(
                relations.len(),
                usize::from(present),
                "callee {expression}: {predicate:?}"
            );
            if present {
                assert_eq!(
                    relations[0].object,
                    RelationEndpoint::Entity(receiver.unwrap())
                );
                assert_eq!(relations[0].provenance, parser.id);
                assert_eq!(relations[0].evidence.source.as_ref(), Some(&callee.source));
            }
        }
        if is_function {
            expected_calls.insert(callee.source.clone(), fetch.id);
        }
    }
    assert_eq!(
        document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.application.reference" && site.enclosing == Some(run.id)
            })
            .count(),
        10
    );
    let mut expected_reads = BTreeSet::new();
    for name in ["index", "spare"] {
        let target = local(name);
        let reads = document
            .occurrences
            .iter()
            .filter(|site| {
                site.syntax_kind == "vba.value_identifier.reference"
                    && site.enclosing == Some(run.id)
                    && &source[usize::try_from(site.source.span().start_byte()).unwrap()
                        ..usize::try_from(site.source.span().end_byte()).unwrap()]
                        == name
            })
            .collect::<Vec<_>>();
        assert_eq!(reads.len(), if name == "index" { 6 } else { 1 });
        for read in reads {
            assert_eq!(read.role, OccurrenceRole::Reference);
            assert_eq!(
                read.target,
                OccurrenceTarget::Resolved { symbol: target.id }
            );
            assert_eq!(read.provenance, parser.id);
            assert!(!formals.iter().any(|(formal, _)| formal.id == target.id));
            let span = read.source.span();
            assert_exact_parameter_source(
                service,
                receipt,
                source,
                &read.source,
                usize::try_from(span.start_byte()).unwrap()
                    ..usize::try_from(span.end_byte()).unwrap(),
            );
            let relations = document
                .relations
                .iter()
                .filter(|relation| {
                    relation.subject == RelationEndpoint::Occurrence(read.id)
                        && relation.predicate == RelationPredicate::RefersTo
                })
                .collect::<Vec<_>>();
            assert_eq!(relations.len(), 1);
            assert_eq!(relations[0].object, RelationEndpoint::Entity(target.id));
            assert_eq!(relations[0].provenance, parser.id);
            assert_eq!(relations[0].evidence.source.as_ref(), Some(&read.source));
            assert!(!document.relations.iter().any(|relation| {
                relation.subject == RelationEndpoint::Occurrence(read.id)
                    && relation.predicate == RelationPredicate::Calls
            }));
            expected_reads.insert(read.source.clone());
        }
        let explained = service
            .symbol_explain(receipt.generation, target.id, &deadline())
            .unwrap();
        assert!(!explained.data.truncated);
        assert_eq!(&explained.data.entity, target);
        assert_eq!(
            explained
                .data
                .occurrences
                .iter()
                .filter(|site| { site.syntax_kind == "vba.value_identifier.reference" })
                .map(|site| site.source.clone())
                .collect::<BTreeSet<_>>(),
            expected_reads
                .iter()
                .filter(|reference| {
                    &source[usize::try_from(reference.span().start_byte()).unwrap()
                        ..usize::try_from(reference.span().end_byte()).unwrap()]
                        == name
                })
                .cloned()
                .collect::<BTreeSet<_>>()
        );
    }
    let calls = service
        .symbol_relationships(
            receipt.generation,
            BTreeSet::from([run.id]),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Outbound),
            0,
            20,
            0,
            &deadline(),
        )
        .unwrap();
    assert!(!calls.data.truncated);
    let items = calls
        .data
        .groups
        .iter()
        .flat_map(|group| &group.items)
        .collect::<Vec<_>>();
    assert_eq!(items.len(), expected_calls.len());
    assert_eq!(
        items
            .iter()
            .flat_map(|item| item.source_refs.iter().cloned())
            .collect::<BTreeSet<_>>(),
        expected_calls.keys().cloned().collect::<BTreeSet<_>>()
    );
    for item in items {
        assert_eq!(item.source_refs.len(), 1);
        assert_eq!(Some(&item.symbol), expected_calls.get(&item.source_refs[0]));
    }
}

#[test]
fn local_dim_lists_and_arrays_preserve_scope_and_generation_lifetimes() {
    let initial = LocalDimState {
        declarations_source: "Dim sink As Long\nDim index As Long\nDim spare As Long\nDim Buffer(2) As Long\nDim dynamic() As Long\nDim Grid(0 To 2, 3) As Long\n",
        declarations: &[
            ("sink As Long", "sink"),
            ("index As Long", "index"),
            ("spare As Long", "spare"),
            ("Buffer(2) As Long", "Buffer"),
            ("dynamic() As Long", "dynamic"),
            ("Grid(0 To 2, 3) As Long", "Grid"),
        ],
        buffer: ExpectedBufferApplication::Fixed(1),
        native_fallback: false,
    };
    let grouped = LocalDimState {
        declarations_source: "Dim sink As Long, index As Long, spare As Long\nDim Buffer(2) As Long, dynamic() As Long, Grid(0 To 2, 3) As Long\n",
        ..initial
    };
    let states = [
        initial,
        grouped,
        LocalDimState {
            declarations_source: "Dim sink As Long, index As Long, spare\nDim Buffer(2) As Long, dynamic(), Grid(0 To 2, 3) As Long\n",
            declarations: &[
                ("sink As Long", "sink"),
                ("index As Long", "index"),
                ("spare", "spare"),
                ("Buffer(2) As Long", "Buffer"),
                ("dynamic()", "dynamic"),
                ("Grid(0 To 2, 3) As Long", "Grid"),
            ],
            ..initial
        },
        LocalDimState {
            declarations_source: "Dim sink As Long, index As Long, spare As Long\nDim Buffer(-1 To +2, 3) As Long, dynamic() As Long, Grid(0 To 2, 3) As Long\n",
            declarations: &[
                ("sink As Long", "sink"),
                ("index As Long", "index"),
                ("spare As Long", "spare"),
                ("Buffer(-1 To +2, 3) As Long", "Buffer"),
                ("dynamic() As Long", "dynamic"),
                ("Grid(0 To 2, 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::Fixed(2),
            ..initial
        },
        LocalDimState {
            declarations_source: "Dim sink As Long, index As Long, spare As Long\nDim Buffer As Long, dynamic() As Long, Grid(0 To 2, 3) As Long\n",
            declarations: &[
                ("sink As Long", "sink"),
                ("index As Long", "index"),
                ("spare As Long", "spare"),
                ("Buffer As Long", "Buffer"),
                ("dynamic() As Long", "dynamic"),
                ("Grid(0 To 2, 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::LocalScalar,
            ..initial
        },
        LocalDimState {
            declarations_source: "Dim sink As Long, index As Long, spare As Long\nDim dynamic() As Long, Grid(0 To 2, 3) As Long\n",
            declarations: &[
                ("sink As Long", "sink"),
                ("index As Long", "index"),
                ("spare As Long", "spare"),
                ("dynamic() As Long", "dynamic"),
                ("Grid(0 To 2, 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::Missing,
            ..initial
        },
        LocalDimState {
            declarations_source: "Dim sink As Long, index As Long, spare As Long\nDim Buffer(2) As Long, [Buffer](-1 To 3) As Long, dynamic() As Long, Grid(0 To 2, 3) As Long\n",
            declarations: &[
                ("sink As Long", "sink"),
                ("index As Long", "index"),
                ("spare As Long", "spare"),
                ("Buffer(2) As Long", "Buffer"),
                ("[Buffer](-1 To 3) As Long", "Buffer"),
                ("dynamic() As Long", "dynamic"),
                ("Grid(0 To 2, 3) As Long", "Grid"),
            ],
            buffer: ExpectedBufferApplication::Ambiguous,
            ..initial
        },
        // A hexadecimal atom remains outside the first-party body subset.
        LocalDimState {
            native_fallback: true,
            ..grouped
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
        let configuration = (language == "vba").then_some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nbas='vba'\n");
        let policy = FirstSliceStoragePolicy::resolve_user_config(configuration, 4).unwrap();
        assert_eq!(policy.retained_generations(), 2);
        let mut previous: Option<(FirstSliceIndexReceipt, String, LocalDimState)> = None;
        let mut initial_source = None;
        let mut initial_logical = None;
        for (position, state) in states.into_iter().enumerate() {
            let source = local_dim_source(state);
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
            assert_local_dim_contract(&service, &receipt, &source, language, state);
            assert_eq!(
                receipt,
                service
                    .index_repository(repository.path(), &deadline())
                    .unwrap()
            );
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_ne!(receipt.generation, old_receipt.generation);
                assert!(receipt.logical_snapshot.is_some());
                assert_ne!(receipt.logical_snapshot, old_receipt.logical_snapshot);
                let expected =
                    old_source.replace(old_state.declarations_source, state.declarations_source);
                let expected = if state.native_fallback {
                    expected.replace("End Sub\n", "sink = &H1\nEnd Sub\n")
                } else if old_state.native_fallback {
                    expected.replace("sink = &H1\n", "")
                } else {
                    expected
                };
                assert_eq!(source, expected);
                assert_local_dim_contract(&service, old_receipt, old_source, language, *old_state);
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
            assert_local_dim_contract(&service, &receipt, &source, language, state);
            if let Some((old_receipt, old_source, old_state)) = &previous {
                assert_local_dim_contract(&service, old_receipt, old_source, language, *old_state);
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
            assert_local_dim_contract(&service, &rebuilt, &source, language, state);
            if position == 0 {
                initial_source = Some(source.clone());
                initial_logical = receipt.logical_snapshot.clone();
            } else if position + 1 == states.len() {
                assert_eq!(initial_source.as_ref(), Some(&source));
                assert_eq!(receipt.logical_snapshot, initial_logical);
            }
            previous = Some((rebuilt, source, state));
        }
    }
}
