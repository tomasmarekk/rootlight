//! Source-owned F# module links require conserved native and project proofs.
//! These fixtures exercise public resolver application without executing a build
//! or treating matching declarations in unrelated projects as candidates.

use super::*;
use rootlight_ir::{
    FSharpBinding, FSharpBindingEvidence, decode_fsharp_binding_envelope,
    new_fsharp_binding_envelope,
};
use std::collections::BTreeSet;

fn fsharp_fixture() -> (Fixture, FileId, FileId, SymbolId) {
    let mut fixture = Fixture::new();
    fixture.document.files[0].path = "Consumer.fs".to_owned();
    fixture.document.files[0].language = "fsharp".to_owned();
    fixture.document.provenance[0].language = "fsharp".to_owned();
    let library = fixture.add_file(9, "Library.fs");
    fixture.document.files[1].language = "fsharp".to_owned();
    let project = fixture.add_file(10, "App.fsproj");
    fixture.document.files[2].language = "xml".to_owned();
    let target = fixture.add_entity(11, "run", library, EntityKind::Function, None);
    fixture.document.entities[0].language = "fsharp".to_owned();
    fixture.document.entities[0].visibility = EntityVisibility::Unknown;
    fixture.add_occurrence(
        20,
        "Library.run",
        fixture.primary_file,
        OccurrenceRole::Reference,
        None,
    );
    fixture.document.occurrences[0].syntax_kind = "fsharp.qualified_body_read.reference".to_owned();
    fixture.add_occurrence(
        21,
        "Library.run",
        fixture.primary_file,
        OccurrenceRole::CallSite,
        None,
    );
    let call = &mut fixture.document.occurrences[1];
    call.syntax_kind = "fsharp.application.call".to_owned();
    call.source = source_ref(
        fixture.document.repository,
        fixture.document.generation,
        fixture.primary_file,
        fixture.content_hash,
        32,
        48,
    );
    call.evidence.source = Some(call.source.clone());
    for file in [fixture.primary_file, library] {
        claim(&mut fixture, file, 0, SOURCE_BYTES, FSharpBinding::Context);
    }
    claim(
        &mut fixture,
        library,
        0,
        8,
        FSharpBinding::Export {
            path: path(),
            symbol: target,
            slots: Some(1),
        },
    );
    let caller = fixture.primary_file;
    claim(
        &mut fixture,
        caller,
        16,
        24,
        FSharpBinding::Reference { path: path() },
    );
    claim(
        &mut fixture,
        caller,
        32,
        48,
        FSharpBinding::Call {
            path: path(),
            slots: 1,
        },
    );
    claim(
        &mut fixture,
        project,
        0,
        SOURCE_BYTES,
        FSharpBinding::Project {
            members: vec![library, caller],
        },
    );
    fixture.validate();
    (fixture, library, project, target)
}

fn path() -> Vec<ContentHash> {
    vec![content_hash(b"Library"), content_hash(b"run")]
}

fn claim(fixture: &mut Fixture, file: FileId, start: u64, end: u64, binding: FSharpBinding) {
    let provenance = fixture
        .document
        .files
        .iter()
        .find(|record| record.id == file)
        .unwrap()
        .provenance;
    fixture.document.extensions.push(
        new_fsharp_binding_envelope(
            fixture.document.repository,
            fixture.document.generation,
            provenance,
            source_ref(
                fixture.document.repository,
                fixture.document.generation,
                file,
                fixture.content_hash,
                start,
                end,
            ),
            FSharpBindingEvidence::new(SourceSpan::new(file, 0, SOURCE_BYTES).unwrap(), binding),
        )
        .unwrap(),
    );
}

fn applied(document: NormalizedIrDocument) -> NormalizedIrDocument {
    ResolutionEngine::default()
        .apply_document(
            document,
            ResolverFactContext::new(content_hash(b"qualified-fsharp-proof")),
            &Cancellation::new(),
        )
        .unwrap()
}

fn fixture_with_terminal_name(written: &str, canonical: &str) -> (Fixture, SymbolId) {
    let (mut fixture, _, _, target) = fsharp_fixture();
    let entity = &mut fixture.document.entities[0];
    entity.canonical_name = written.to_owned();
    entity.display_name = written.to_owned();
    entity.qualified_name = format!("Library.{written}");
    let spelling_hash = content_hash(format!("Library.{written}").as_bytes());
    for site in &mut fixture.document.occurrences {
        site.syntactic_text_hash = spelling_hash;
        site.target = OccurrenceTarget::Unresolved {
            text_hash: spelling_hash,
        };
    }
    for envelope in &mut fixture.document.extensions {
        let evidence = decode_fsharp_binding_envelope(envelope).unwrap();
        let mut binding = evidence.binding().clone();
        match &mut binding {
            FSharpBinding::Export { path, .. }
            | FSharpBinding::SignatureValue { path, .. }
            | FSharpBinding::SignatureType { path, .. }
            | FSharpBinding::SignatureAlias { path, .. }
            | FSharpBinding::Reference { path }
            | FSharpBinding::Call { path, .. } => {
                *path.last_mut().unwrap() = content_hash(canonical.as_bytes());
            }
            FSharpBinding::ImportedReference { path }
            | FSharpBinding::ImportedCall { path, .. } => {
                *path.last_mut().unwrap() = content_hash(canonical.as_bytes());
            }
            FSharpBinding::OpenedReference { name } | FSharpBinding::OpenedCall { name, .. } => {
                *name = content_hash(canonical.as_bytes());
            }
            FSharpBinding::Context
            | FSharpBinding::Project { .. }
            | FSharpBinding::Container { .. }
            | FSharpBinding::Open { .. }
            | FSharpBinding::ImportContext { .. } => continue,
        }
        *envelope = new_fsharp_binding_envelope(
            envelope.repository,
            envelope.generation,
            envelope.provenance,
            envelope.evidence.source.clone().unwrap(),
            FSharpBindingEvidence::new(evidence.scope(), binding),
        )
        .unwrap();
    }
    fixture.validate();
    (fixture, target)
}

fn binding_position(
    fixture: &Fixture,
    matches: impl Fn(&FSharpBinding, &SourceRef) -> bool,
) -> usize {
    fixture
        .document
        .extensions
        .iter()
        .position(|envelope| {
            let evidence = decode_fsharp_binding_envelope(envelope).unwrap();
            matches(
                evidence.binding(),
                envelope.evidence.source.as_ref().unwrap(),
            )
        })
        .unwrap()
}

fn replace_binding(fixture: &mut Fixture, position: usize, binding: FSharpBinding) {
    let envelope = &fixture.document.extensions[position];
    let scope = decode_fsharp_binding_envelope(envelope).unwrap().scope();
    fixture.document.extensions[position] = new_fsharp_binding_envelope(
        envelope.repository,
        envelope.generation,
        envelope.provenance,
        envelope.evidence.source.clone().unwrap(),
        FSharpBindingEvidence::new(scope, binding),
    )
    .unwrap();
}

fn import_manifest(
    fixture: &mut Fixture,
    file: FileId,
    mut containers: Vec<SourceSpan>,
    mut opens: Vec<SourceSpan>,
) {
    containers.sort_unstable();
    opens.sort_unstable();
    let position = binding_position(fixture, |binding, source| {
        source.span().file() == file && matches!(binding, FSharpBinding::ImportContext { .. })
    });
    replace_binding(
        fixture,
        position,
        FSharpBinding::ImportContext { containers, opens },
    );
}

fn opened_value_fixture() -> (Fixture, FileId, FileId, SymbolId) {
    let (mut fixture, library, project, target) = owned_open_fixture();
    singleton_uses(&mut fixture);
    fixture.validate();
    (fixture, library, project, target)
}

fn singleton_uses(fixture: &mut Fixture) {
    for call in [false, true] {
        let position = binding_position(fixture, |binding, _| {
            if call {
                matches!(
                    binding,
                    FSharpBinding::Call { .. } | FSharpBinding::ImportedCall { .. }
                )
            } else {
                matches!(
                    binding,
                    FSharpBinding::Reference { .. } | FSharpBinding::ImportedReference { .. }
                )
            }
        });
        replace_binding(
            fixture,
            position,
            if call {
                FSharpBinding::OpenedCall {
                    name: content_hash(b"run"),
                    slots: 1,
                }
            } else {
                FSharpBinding::OpenedReference {
                    name: content_hash(b"run"),
                }
            },
        );
    }
    for occurrence in &mut fixture.document.occurrences {
        if occurrence.role == OccurrenceRole::Reference {
            occurrence.syntax_kind = "fsharp.simple_function_body_read.reference".to_owned();
        }
        occurrence.syntactic_text_hash = content_hash(b"run");
        occurrence.target = OccurrenceTarget::Unresolved {
            text_hash: content_hash(b"run"),
        };
    }
}

#[test]
fn fsharp_opened_values_preserve_exact_targets_sources_and_all_import_inputs() {
    let (fixture, _, _, target) = opened_value_fixture();
    let inputs = fixture
        .document
        .extensions
        .iter()
        .filter(|envelope| {
            !matches!(
                decode_fsharp_binding_envelope(envelope).unwrap().binding(),
                FSharpBinding::OpenedReference { .. } | FSharpBinding::OpenedCall { .. }
            )
        })
        .map(|envelope| envelope.id)
        .collect::<BTreeSet<_>>();
    let uses = fixture
        .document
        .extensions
        .iter()
        .filter_map(|envelope| {
            let role = match decode_fsharp_binding_envelope(envelope).unwrap().binding() {
                FSharpBinding::OpenedReference { .. } => OccurrenceRole::Reference,
                FSharpBinding::OpenedCall { .. } => OccurrenceRole::CallSite,
                _ => return None,
            };
            Some((envelope.evidence.source.clone().unwrap(), role, envelope.id))
        })
        .collect::<Vec<_>>();
    let sources = fixture
        .document
        .occurrences
        .iter()
        .map(|site| site.source.clone())
        .collect::<Vec<_>>();
    let document = applied(fixture.document);
    for site in &document.occurrences {
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
        assert!(sources.contains(&site.source));
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == site.provenance)
            .unwrap();
        assert_eq!(
            proof.rule.as_deref(),
            Some("fsharp-opened-value-v6.owned_open_source_declared_project")
        );
        for input in &inputs {
            assert!(proof.derivation_parents.contains(&FactRef::Fact(*input)));
        }
        let (_, _, input) = uses
            .iter()
            .find(|(source, role, _)| source == &site.source && *role == site.role)
            .unwrap();
        assert!(proof.derivation_parents.contains(&FactRef::Fact(*input)));
        assert!(proof.evidence_sources.contains(&site.source));
    }
    for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == predicate
                    && edge.object == RelationEndpoint::Entity(target))
                .count(),
            1
        );
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DispatchCandidate)
    );
}

#[test]
fn fsharp_opened_values_require_complete_unique_native_graph_and_use_modes() {
    let (fixture, _, _, _) = opened_value_fixture();
    for position in 0..fixture.document.extensions.len() {
        let mut missing = fixture.document.clone();
        let binding = decode_fsharp_binding_envelope(&missing.extensions[position])
            .unwrap()
            .binding()
            .clone();
        missing.extensions.remove(position);
        let document = applied(missing);
        let affected = match binding {
            FSharpBinding::OpenedReference { .. } => Some(OccurrenceRole::Reference),
            FSharpBinding::OpenedCall { .. } => Some(OccurrenceRole::CallSite),
            _ => None,
        };
        assert!(
            document
                .occurrences
                .iter()
                .filter(|site| affected.is_none_or(|role| site.role == role))
                .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "missing {position}"
        );
    }
    let mut wrong_label = opened_value_fixture().0;
    wrong_label
        .document
        .occurrences
        .iter_mut()
        .find(|site| site.role == OccurrenceRole::Reference)
        .unwrap()
        .syntax_kind = "fsharp.qualified_body_read.reference".to_owned();
    let document = applied(wrong_label.document);
    assert!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    let mut legacy = owned_open_fixture().0;
    legacy
        .document
        .occurrences
        .iter_mut()
        .find(|site| site.role == OccurrenceRole::Reference)
        .unwrap()
        .syntax_kind = "fsharp.simple_function_body_read.reference".to_owned();
    let document = applied(legacy.document);
    assert!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    let mut duplicate = opened_value_fixture().0;
    let caller = duplicate.primary_file;
    claim(
        &mut duplicate,
        caller,
        16,
        24,
        FSharpBinding::OpenedReference {
            name: content_hash(b"other"),
        },
    );
    let document = applied(duplicate.document);
    assert!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_opened_root_ambiguity_precedes_kind_and_slot_filtering() {
    for competing_kind in [None, Some(EntityKind::Variable), Some(EntityKind::Function)] {
        let (mut fixture, library, _, _) = opened_value_fixture();
        let caller = fixture.primary_file;
        let peer = fixture.add_file(43, "Peer.fs");
        fixture.document.files.last_mut().unwrap().language = "fsharp".to_owned();
        let scope = SourceSpan::new(peer, 0, SOURCE_BYTES).unwrap();
        claim(&mut fixture, peer, 0, SOURCE_BYTES, FSharpBinding::Context);
        claim(
            &mut fixture,
            peer,
            0,
            SOURCE_BYTES,
            FSharpBinding::Container {
                path: vec![content_hash(b"Peer")],
                roots: vec![content_hash(b"run")],
            },
        );
        claim(
            &mut fixture,
            peer,
            0,
            SOURCE_BYTES,
            FSharpBinding::ImportContext {
                containers: vec![scope],
                opens: vec![],
            },
        );
        if let Some(kind) = competing_kind {
            let target = fixture.add_entity(44, "run", peer, kind, None);
            let entity = fixture.document.entities.last_mut().unwrap();
            entity.language = "fsharp".to_owned();
            entity.visibility = EntityVisibility::Unknown;
            claim(
                &mut fixture,
                peer,
                0,
                8,
                FSharpBinding::Export {
                    path: vec![content_hash(b"Peer"), content_hash(b"run")],
                    symbol: target,
                    slots: (kind == EntityKind::Function).then_some(2),
                },
            );
        }
        claim(
            &mut fixture,
            caller,
            14,
            15,
            FSharpBinding::Open {
                path: vec![content_hash(b"Peer")],
                owner: SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap(),
            },
        );
        import_manifest(
            &mut fixture,
            caller,
            vec![SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap()],
            vec![
                SourceSpan::new(caller, 8, 14).unwrap(),
                SourceSpan::new(caller, 14, 15).unwrap(),
            ],
        );
        let project = binding_position(&fixture, |binding, _| {
            matches!(binding, FSharpBinding::Project { .. })
        });
        replace_binding(
            &mut fixture,
            project,
            FSharpBinding::Project {
                members: vec![library, peer, caller],
            },
        );
        assert_signature_closed(fixture.document);
    }
    let (mut duplicate_open, _, _, target) = opened_value_fixture();
    let caller = duplicate_open.primary_file;
    claim(
        &mut duplicate_open,
        caller,
        6,
        7,
        FSharpBinding::Open {
            path: vec![content_hash(b"Library")],
            owner: SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap(),
        },
    );
    import_manifest(
        &mut duplicate_open,
        caller,
        vec![SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap()],
        vec![
            SourceSpan::new(caller, 6, 7).unwrap(),
            SourceSpan::new(caller, 8, 14).unwrap(),
        ],
    );
    assert!(
        applied(duplicate_open.document)
            .occurrences
            .iter()
            .all(|site| site.target == (OccurrenceTarget::Resolved { symbol: target }))
    );
}

#[test]
fn fsharp_opened_values_keep_signature_surface_and_exact_saturated_calls() {
    for fixture in [
        signature_fixture(true).0,
        signature_record_fixture(true, "Shape").0,
        signature_alias_fixture(true, "Alias").0,
    ] {
        let mut fixture = fixture;
        singleton_uses(&mut fixture);
        let document = applied(fixture.document);
        assert!(
            document
                .occurrences
                .iter()
                .all(|site| matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
        assert!(
            document
                .provenance
                .iter()
                .filter(|proof| proof.rule.as_deref()
                    == Some("fsharp-opened-value-v6.owned_open_source_declared_project"))
                .all(|proof| proof.evidence_sources.iter().any(|source| document
                    .files
                    .iter()
                    .any(|file| file.id == source.span().file()
                        && file.language == "fsharp-signature")))
        );
    }
    for slots in [2, 3] {
        let (mut fixture, _, _, _) = opened_value_fixture();
        let position = binding_position(&fixture, |binding, _| {
            matches!(binding, FSharpBinding::OpenedCall { .. })
        });
        replace_binding(
            &mut fixture,
            position,
            FSharpBinding::OpenedCall {
                name: content_hash(b"run"),
                slots,
            },
        );
        let source = fixture
            .document
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::CallSite)
            .unwrap()
            .source
            .clone();
        let mut gap = rootlight_ir::SkippedRegion {
            id: FactId::from_bytes([0; 20]),
            repository: fixture.document.repository,
            generation: fixture.document.generation,
            source: source.clone(),
            domain: rootlight_ir::FactDomain::Relations,
            reason: rootlight_ir::SkippedRegionReason::UnsupportedConstruct,
            detail: "fsharp-direct-call-target-unavailable".to_owned(),
            provenance: fixture.provenance,
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        gap.id = rootlight_ir::derive_skipped_region_id(&gap).unwrap();
        let gap_id = gap.id;
        fixture.document.skipped_regions.push(gap);
        let document = applied(fixture.document);
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::RefersTo)
                .count(),
            1
        );
        assert!(!document.relations.iter().any(|edge| matches!(
            edge.predicate,
            RelationPredicate::Calls | RelationPredicate::DispatchCandidate
        )));
        assert!(document.skipped_regions.iter().any(|gap| gap.id == gap_id
            && gap.detail == "fsharp-direct-call-target-unavailable"
            && gap.reason == rootlight_ir::SkippedRegionReason::UnsupportedConstruct
            && gap.provenance == fixture.provenance));
    }
    let (mut hidden, _, signature, _, _) = signature_fixture(true);
    singleton_uses(&mut hidden);
    let position = binding_position(&hidden, |binding, _| {
        matches!(binding, FSharpBinding::SignatureValue { .. })
    });
    hidden.document.extensions.remove(position);
    let container = binding_position(&hidden, |binding, source| {
        source.span().file() == signature && matches!(binding, FSharpBinding::Container { .. })
    });
    replace_binding(
        &mut hidden,
        container,
        FSharpBinding::Container {
            path: vec![content_hash(b"Library")],
            roots: vec![],
        },
    );
    assert_signature_closed(hidden.document);
}

#[test]
fn fsharp_opened_variables_are_exact_reads_not_executable_targets() {
    let (mut fixture, _, _, target) = opened_value_fixture();
    fixture.document.entities[0].kind = EntityKind::Variable;
    let position = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Export { .. })
    });
    replace_binding(
        &mut fixture,
        position,
        FSharpBinding::Export {
            path: path(),
            symbol: target,
            slots: None,
        },
    );
    let document = applied(fixture.document);
    assert!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::Reference)
            .all(|site| site.target == (OccurrenceTarget::Resolved { symbol: target }))
    );
    assert!(
        document
            .occurrences
            .iter()
            .filter(|site| site.role == OccurrenceRole::CallSite)
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|edge| edge.predicate == RelationPredicate::RefersTo
                && edge.object == RelationEndpoint::Entity(target))
            .count(),
        1
    );
    assert!(!document.relations.iter().any(|edge| matches!(
        edge.predicate,
        RelationPredicate::Calls | RelationPredicate::DispatchCandidate
    )));
}

#[test]
fn fsharp_opened_claims_cannot_rebake_a_different_authored_name() {
    let (mut fixture, library, _, _) = opened_value_fixture();
    let other = fixture.add_entity(45, "other", library, EntityKind::Function, None);
    let entity = fixture.document.entities.last_mut().unwrap();
    entity.language = "fsharp".to_owned();
    entity.visibility = EntityVisibility::Unknown;
    entity.qualified_name = "Library.other".to_owned();
    claim(
        &mut fixture,
        library,
        0,
        8,
        FSharpBinding::Export {
            path: vec![content_hash(b"Library"), content_hash(b"other")],
            symbol: other,
            slots: Some(1),
        },
    );
    let position = binding_position(&fixture, |binding, source| {
        source.span().file() == library && matches!(binding, FSharpBinding::Container { .. })
    });
    let mut roots = vec![content_hash(b"run"), content_hash(b"other")];
    roots.sort_unstable();
    replace_binding(
        &mut fixture,
        position,
        FSharpBinding::Container {
            path: vec![content_hash(b"Library")],
            roots,
        },
    );
    for call in [false, true] {
        let position = binding_position(&fixture, |binding, _| {
            if call {
                matches!(binding, FSharpBinding::OpenedCall { .. })
            } else {
                matches!(binding, FSharpBinding::OpenedReference { .. })
            }
        });
        replace_binding(
            &mut fixture,
            position,
            if call {
                FSharpBinding::OpenedCall {
                    name: content_hash(b"other"),
                    slots: 1,
                }
            } else {
                FSharpBinding::OpenedReference {
                    name: content_hash(b"other"),
                }
            },
        );
    }
    assert_signature_closed(fixture.document);
}

#[test]
fn fsharp_opened_authored_names_preserve_unicode_and_complete_quotes() {
    for (declaration, canonical, written) in [
        ("run", "run", "``run``"),
        ("``run``", "run", "run"),
        ("δ", "δ", "δ"),
        ("``函 数``", "函 数", "``函 数``"),
    ] {
        let (mut fixture, library, _, target) = opened_value_fixture();
        let entity = &mut fixture.document.entities[0];
        entity.canonical_name = declaration.to_owned();
        entity.display_name = declaration.to_owned();
        entity.qualified_name = format!("Library.{declaration}");
        for site in &mut fixture.document.occurrences {
            site.syntactic_text_hash = content_hash(written.as_bytes());
            site.target = OccurrenceTarget::Unresolved {
                text_hash: site.syntactic_text_hash,
            };
        }
        for position in 0..fixture.document.extensions.len() {
            let evidence =
                decode_fsharp_binding_envelope(&fixture.document.extensions[position]).unwrap();
            let binding = match evidence.binding() {
                FSharpBinding::Export { slots, .. } => Some(FSharpBinding::Export {
                    path: vec![content_hash(b"Library"), content_hash(canonical.as_bytes())],
                    symbol: target,
                    slots: *slots,
                }),
                FSharpBinding::OpenedReference { .. } => Some(FSharpBinding::OpenedReference {
                    name: content_hash(canonical.as_bytes()),
                }),
                FSharpBinding::OpenedCall { slots, .. } => Some(FSharpBinding::OpenedCall {
                    name: content_hash(canonical.as_bytes()),
                    slots: *slots,
                }),
                FSharpBinding::Container { .. } if evidence.scope().file() == library => {
                    Some(FSharpBinding::Container {
                        path: vec![content_hash(b"Library")],
                        roots: vec![content_hash(canonical.as_bytes())],
                    })
                }
                _ => None,
            };
            if let Some(binding) = binding {
                replace_binding(&mut fixture, position, binding);
            }
        }
        assert!(
            applied(fixture.document)
                .occurrences
                .iter()
                .all(|site| site.target == (OccurrenceTarget::Resolved { symbol: target })),
            "{declaration} / {written}"
        );
    }
}

#[test]
fn fsharp_opened_gap_reconciliation_requires_exact_source_domain_and_role() {
    let (mut fixture, _, _, _) = opened_value_fixture();
    let read = fixture
        .document
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::Reference)
        .unwrap()
        .source
        .clone();
    let call = fixture
        .document
        .occurrences
        .iter()
        .find(|site| site.role == OccurrenceRole::CallSite)
        .unwrap()
        .source
        .clone();
    let shifted = source_ref(
        fixture.document.repository,
        fixture.document.generation,
        fixture.primary_file,
        fixture.content_hash,
        17,
        24,
    );
    let cases = [
        (
            read.clone(),
            rootlight_ir::FactDomain::Relations,
            "fsharp-formal-binding-target-unavailable",
            true,
        ),
        (
            call.clone(),
            rootlight_ir::FactDomain::Relations,
            "fsharp-direct-call-target-unavailable",
            true,
        ),
        (
            read.clone(),
            rootlight_ir::FactDomain::Occurrences,
            "fsharp-formal-binding-target-unavailable",
            false,
        ),
        (
            read.clone(),
            rootlight_ir::FactDomain::Relations,
            "fsharp-direct-call-target-unavailable",
            false,
        ),
        (
            call,
            rootlight_ir::FactDomain::Relations,
            "fsharp-formal-binding-target-unavailable",
            false,
        ),
        (
            shifted,
            rootlight_ir::FactDomain::Relations,
            "fsharp-formal-binding-target-unavailable",
            false,
        ),
        (
            read.clone(),
            rootlight_ir::FactDomain::Relations,
            "fsharp-body-binding-and-call-analysis-unavailable",
            false,
        ),
        (
            read,
            rootlight_ir::FactDomain::Relations,
            "fsharp-qualified-target-unavailable",
            false,
        ),
    ];
    let mut preserved = BTreeSet::new();
    for (source, domain, detail, reconciled) in cases {
        let mut gap = rootlight_ir::SkippedRegion {
            id: FactId::from_bytes([0; 20]),
            repository: fixture.document.repository,
            generation: fixture.document.generation,
            source: source.clone(),
            domain,
            reason: rootlight_ir::SkippedRegionReason::UnsupportedConstruct,
            detail: detail.to_owned(),
            provenance: fixture.provenance,
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        gap.id = rootlight_ir::derive_skipped_region_id(&gap).unwrap();
        if !reconciled {
            preserved.insert(gap.id);
        }
        fixture.document.skipped_regions.push(gap);
    }
    let document = applied(fixture.document);
    assert_eq!(document.skipped_regions.len(), 8);
    assert_eq!(
        document
            .skipped_regions
            .iter()
            .filter(
                |gap| gap.detail == "fsharp-project-build-closure-unverified"
                    && gap.domain == rootlight_ir::FactDomain::Relations
                    && gap.reason == rootlight_ir::SkippedRegionReason::MissingBuildContext
                    && document
                        .provenance
                        .iter()
                        .any(|proof| proof.id == gap.provenance
                            && proof.rule.as_deref()
                                == Some(
                                    "fsharp-opened-value-v6.owned_open_source_declared_project"
                                ))
            )
            .count(),
        2
    );
    for id in preserved {
        let gap = document
            .skipped_regions
            .iter()
            .find(|gap| gap.id == id)
            .unwrap();
        assert_eq!(
            gap.reason,
            rootlight_ir::SkippedRegionReason::UnsupportedConstruct
        );
        assert_eq!(gap.provenance, fixture.provenance);
    }
}

#[test]
fn fsharp_opened_values_preserve_order_unknown_opens_and_work_fallback() {
    for mutation in 0..4 {
        let (mut fixture, library, _, _) = opened_value_fixture();
        let caller = fixture.primary_file;
        match mutation {
            0 => {
                let position = binding_position(&fixture, |binding, _| {
                    matches!(binding, FSharpBinding::Project { .. })
                });
                replace_binding(
                    &mut fixture,
                    position,
                    FSharpBinding::Project {
                        members: vec![caller, library],
                    },
                );
            }
            1 => {
                let position = binding_position(&fixture, |binding, _| {
                    matches!(binding, FSharpBinding::Open { .. })
                });
                replace_binding(
                    &mut fixture,
                    position,
                    FSharpBinding::Open {
                        path: vec![content_hash(b"Unknown")],
                        owner: SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap(),
                    },
                );
            }
            2 => {
                let position = binding_position(&fixture, |binding, _| {
                    matches!(binding, FSharpBinding::Open { .. })
                });
                fixture.document.extensions.remove(position);
                claim(
                    &mut fixture,
                    caller,
                    49,
                    50,
                    FSharpBinding::Open {
                        path: vec![content_hash(b"Library")],
                        owner: SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap(),
                    },
                );
                import_manifest(
                    &mut fixture,
                    caller,
                    vec![SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap()],
                    vec![SourceSpan::new(caller, 49, 50).unwrap()],
                );
            }
            3 => {
                let position = binding_position(&fixture, |binding, _| {
                    matches!(binding, FSharpBinding::Export { .. })
                });
                fixture.document.extensions.remove(position);
                fixture.document.entities[0].visibility = EntityVisibility::Private;
            }
            _ => unreachable!(),
        }
        assert_signature_closed(fixture.document);
    }
    let (fixture, _, _, _) = opened_value_fixture();
    let engine = ResolutionEngine::new(ResolutionLimits::with_work_limit(1, 1).unwrap());
    let (document, estimate) = engine
        .apply_document_bounded(
            fixture.document,
            ResolverFactContext::new(content_hash(b"bounded-opened-value-proof")),
            &Cancellation::new(),
        )
        .unwrap();
    assert!(!estimate.fits());
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    let cancelled = Cancellation::new();
    cancelled.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    assert!(
        ResolutionEngine::default()
            .estimate_work(&opened_value_fixture().0.document, &cancelled)
            .is_err()
    );
}

fn owned_open_fixture() -> (Fixture, FileId, FileId, SymbolId) {
    let (mut fixture, library, project, target) = fsharp_fixture();
    let reference = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Reference { .. })
    });
    replace_binding(
        &mut fixture,
        reference,
        FSharpBinding::ImportedReference { path: path() },
    );
    let call = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Call { .. })
    });
    replace_binding(
        &mut fixture,
        call,
        FSharpBinding::ImportedCall {
            path: path(),
            slots: 1,
        },
    );
    let caller = fixture.primary_file;
    let library_scope = SourceSpan::new(library, 0, SOURCE_BYTES).unwrap();
    let caller_scope = SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap();
    claim(
        &mut fixture,
        library,
        0,
        SOURCE_BYTES,
        FSharpBinding::Container {
            path: vec![content_hash(b"Library")],
            roots: vec![content_hash(b"run")],
        },
    );
    claim(
        &mut fixture,
        caller,
        0,
        SOURCE_BYTES,
        FSharpBinding::Container {
            path: vec![content_hash(b"Consumer")],
            roots: vec![],
        },
    );
    claim(
        &mut fixture,
        caller,
        8,
        14,
        FSharpBinding::Open {
            path: vec![content_hash(b"Library")],
            owner: caller_scope,
        },
    );
    claim(
        &mut fixture,
        library,
        0,
        SOURCE_BYTES,
        FSharpBinding::ImportContext {
            containers: vec![library_scope],
            opens: vec![],
        },
    );
    claim(
        &mut fixture,
        caller,
        0,
        SOURCE_BYTES,
        FSharpBinding::ImportContext {
            containers: vec![caller_scope],
            opens: vec![SourceSpan::new(caller, 8, 14).unwrap()],
        },
    );
    fixture.validate();
    (fixture, library, project, target)
}

#[test]
fn fsharp_owned_opens_preserve_targets_and_complete_graph_provenance() {
    let (fixture, _, _, target) = owned_open_fixture();
    let inputs = fixture
        .document
        .extensions
        .iter()
        .map(|envelope| envelope.id)
        .collect::<BTreeSet<_>>();
    let expected_sources = fixture
        .document
        .occurrences
        .iter()
        .map(|site| site.source.clone())
        .collect::<Vec<_>>();
    let document = applied(fixture.document);
    for site in &document.occurrences {
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
        assert!(expected_sources.contains(&site.source));
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == site.provenance)
            .unwrap();
        assert_eq!(
            proof.rule.as_deref(),
            Some("fsharp-qualified-module-v2.owned_open_source_declared_project")
        );
        assert_eq!(
            proof
                .derivation_parents
                .iter()
                .filter(|parent| matches!(parent, FactRef::Fact(id) if inputs.contains(id)))
                .count(),
            10
        );
        assert!(proof.evidence_sources.contains(&site.source));
    }
    for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == predicate
                    && relation.object == RelationEndpoint::Entity(target))
                .count(),
            1
        );
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::DispatchCandidate)
    );
}

#[test]
fn fsharp_owned_opens_close_missing_duplicate_and_unowned_graph_claims() {
    let (fixture, _, _, _) = owned_open_fixture();
    for position in 0..fixture.document.extensions.len() {
        let binding =
            decode_fsharp_binding_envelope(&fixture.document.extensions[position]).unwrap();
        if !matches!(
            binding.binding(),
            FSharpBinding::Container { .. }
                | FSharpBinding::Open { .. }
                | FSharpBinding::ImportContext { .. }
        ) {
            continue;
        }
        let mut missing = fixture.document.clone();
        missing.extensions.remove(position);
        assert!(
            applied(missing)
                .occurrences
                .iter()
                .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "missing graph claim {position}"
        );
        let mut duplicate = fixture.document.clone();
        let original = &fixture.document.extensions[position];
        let changed = match binding.binding() {
            FSharpBinding::Container { path, .. } => FSharpBinding::Container {
                path: path.clone(),
                roots: vec![content_hash(b"conflict")],
            },
            FSharpBinding::Open { owner, .. } => FSharpBinding::Open {
                path: vec![content_hash(b"External")],
                owner: *owner,
            },
            FSharpBinding::ImportContext { containers, opens } => {
                let first = containers.first().unwrap();
                FSharpBinding::ImportContext {
                    containers: vec![SourceSpan::new(first.file(), 1, first.end_byte()).unwrap()],
                    opens: opens.clone(),
                }
            }
            _ => unreachable!(),
        };
        duplicate.extensions.push(
            new_fsharp_binding_envelope(
                original.repository,
                original.generation,
                original.provenance,
                original.evidence.source.clone().unwrap(),
                FSharpBindingEvidence::new(binding.scope(), changed),
            )
            .unwrap(),
        );
        assert!(
            applied(duplicate)
                .occurrences
                .iter()
                .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. })),
            "duplicate graph claim {position}"
        );
    }
    let (mut fixture, _, _, _) = owned_open_fixture();
    let caller = fixture.primary_file;
    let open = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Open { .. })
    });
    replace_binding(
        &mut fixture,
        open,
        FSharpBinding::Open {
            path: vec![content_hash(b"Library")],
            owner: SourceSpan::new(caller, 0, SOURCE_BYTES - 1).unwrap(),
        },
    );
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_owned_opens_conserve_every_preceding_import_and_project_graph() {
    let (mut fixture, _, _, _) = owned_open_fixture();
    let caller = fixture.primary_file;
    let caller_scope = SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap();
    claim(
        &mut fixture,
        caller,
        14,
        15,
        FSharpBinding::Open {
            path: vec![content_hash(b"Library")],
            owner: caller_scope,
        },
    );
    import_manifest(
        &mut fixture,
        caller,
        vec![caller_scope],
        vec![
            SourceSpan::new(caller, 8, 14).unwrap(),
            SourceSpan::new(caller, 14, 15).unwrap(),
        ],
    );
    assert!(
        applied(fixture.document.clone())
            .occurrences
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    let first = binding_position(&fixture, |binding, source| {
        matches!(binding, FSharpBinding::Open { .. }) && source.span().start_byte() == 8
    });
    fixture.document.extensions.remove(first);
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );

    let (mut fixture, library, project, _) = owned_open_fixture();
    let peer = fixture.add_file(40, "Peer.fs");
    fixture.document.files.last_mut().unwrap().language = "fsharp".to_owned();
    claim(&mut fixture, peer, 0, SOURCE_BYTES, FSharpBinding::Context);
    let project_claim = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Project { .. })
    });
    replace_binding(
        &mut fixture,
        project_claim,
        FSharpBinding::Project {
            members: vec![peer, library, caller],
        },
    );
    assert_eq!(
        fixture
            .document
            .files
            .iter()
            .find(|file| file.id == project)
            .unwrap()
            .path,
        "App.fsproj"
    );
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_owned_opens_reject_unknown_shadowed_same_file_and_suffix_roots() {
    for name in ["External", "Consumer"] {
        let (mut fixture, _, _, _) = owned_open_fixture();
        let caller = fixture.primary_file;
        let open = binding_position(&fixture, |binding, _| {
            matches!(binding, FSharpBinding::Open { .. })
        });
        replace_binding(
            &mut fixture,
            open,
            FSharpBinding::Open {
                path: vec![content_hash(name.as_bytes())],
                owner: SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap(),
            },
        );
        assert!(
            applied(fixture.document)
                .occurrences
                .iter()
                .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
    }
    let (mut fixture, library, _, _) = owned_open_fixture();
    let container = binding_position(&fixture, |binding, source| {
        matches!(binding, FSharpBinding::Container { .. }) && source.span().file() == library
    });
    replace_binding(
        &mut fixture,
        container,
        FSharpBinding::Container {
            path: vec![content_hash(b"Library")],
            roots: vec![content_hash(b"Library")],
        },
    );
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );

    let (mut fixture, library, _, _) = owned_open_fixture();
    let caller = fixture.primary_file;
    let other = fixture.add_file(41, "Nested.fs");
    fixture.document.files.last_mut().unwrap().language = "fsharp".to_owned();
    claim(&mut fixture, other, 0, SOURCE_BYTES, FSharpBinding::Context);
    claim(
        &mut fixture,
        other,
        0,
        SOURCE_BYTES,
        FSharpBinding::Container {
            path: vec![content_hash(b"Area"), content_hash(b"Library")],
            roots: vec![],
        },
    );
    claim(
        &mut fixture,
        other,
        0,
        SOURCE_BYTES,
        FSharpBinding::ImportContext {
            containers: vec![SourceSpan::new(other, 0, SOURCE_BYTES).unwrap()],
            opens: vec![],
        },
    );
    let project = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Project { .. })
    });
    replace_binding(
        &mut fixture,
        project,
        FSharpBinding::Project {
            members: vec![other, library, caller],
        },
    );
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_owned_opens_respect_clause_order_scopes_and_legacy_barriers() {
    let (mut fixture, _, _, _) = owned_open_fixture();
    let caller = fixture.primary_file;
    let open = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Open { .. })
    });
    fixture.document.extensions.remove(open);
    let scope = SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap();
    claim(
        &mut fixture,
        caller,
        24,
        30,
        FSharpBinding::Open {
            path: vec![content_hash(b"Library")],
            owner: scope,
        },
    );
    import_manifest(
        &mut fixture,
        caller,
        vec![scope],
        vec![SourceSpan::new(caller, 24, 30).unwrap()],
    );
    let reference = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::ImportedReference { .. })
    });
    replace_binding(
        &mut fixture,
        reference,
        FSharpBinding::Reference { path: path() },
    );
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );

    let (mut fixture, _, _, _) = owned_open_fixture();
    let reference = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::ImportedReference { .. })
    });
    replace_binding(
        &mut fixture,
        reference,
        FSharpBinding::Reference { path: path() },
    );
    let call = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::ImportedCall { .. })
    });
    replace_binding(
        &mut fixture,
        call,
        FSharpBinding::Call {
            path: path(),
            slots: 1,
        },
    );
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );

    let (mut fixture, _, _, _) = owned_open_fixture();
    let caller = fixture.primary_file;
    let nested = SourceSpan::new(caller, 0, 30).unwrap();
    claim(
        &mut fixture,
        caller,
        0,
        30,
        FSharpBinding::Container {
            path: vec![content_hash(b"Consumer"), content_hash(b"Nested")],
            roots: vec![],
        },
    );
    let open = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Open { .. })
    });
    replace_binding(
        &mut fixture,
        open,
        FSharpBinding::Open {
            path: vec![content_hash(b"Library")],
            owner: nested,
        },
    );
    import_manifest(
        &mut fixture,
        caller,
        vec![nested, SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap()],
        vec![SourceSpan::new(caller, 8, 14).unwrap()],
    );
    let document = applied(fixture.document);
    assert!(
        document
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::Reference)
            .is_some_and(|site| matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert!(
        document
            .occurrences
            .iter()
            .find(|site| site.role == OccurrenceRole::CallSite)
            .is_some_and(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_owned_opens_keep_call_slots_and_bounded_fallback_exact() {
    let (mut fixture, _, _, _) = owned_open_fixture();
    let call = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::ImportedCall { .. })
    });
    replace_binding(
        &mut fixture,
        call,
        FSharpBinding::ImportedCall {
            path: path(),
            slots: 2,
        },
    );
    let document = applied(fixture.document);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
            .count(),
        1
    );
    assert!(!document.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls | RelationPredicate::DispatchCandidate
    )));

    let (fixture, _, _, _) = owned_open_fixture();
    let engine = ResolutionEngine::new(ResolutionLimits::with_work_limit(1, 1).unwrap());
    let (document, estimate) = engine
        .apply_document_bounded(
            fixture.document,
            ResolverFactContext::new(content_hash(b"bounded-owned-open-proof")),
            &Cancellation::new(),
        )
        .unwrap();
    assert!(!estimate.fits());
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

fn signature_fixture(imported: bool) -> (Fixture, FileId, FileId, SymbolId, SymbolId) {
    let (mut fixture, library, _, target) = owned_open_fixture();
    let caller = fixture.primary_file;
    if !imported {
        let open = binding_position(&fixture, |binding, _| {
            matches!(binding, FSharpBinding::Open { .. })
        });
        fixture.document.extensions.remove(open);
        import_manifest(
            &mut fixture,
            caller,
            vec![SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap()],
            vec![],
        );
        for call in [false, true] {
            let position = binding_position(&fixture, |binding, _| {
                if call {
                    matches!(binding, FSharpBinding::ImportedCall { .. })
                } else {
                    matches!(binding, FSharpBinding::ImportedReference { .. })
                }
            });
            replace_binding(
                &mut fixture,
                position,
                if call {
                    FSharpBinding::Call {
                        path: path(),
                        slots: 1,
                    }
                } else {
                    FSharpBinding::Reference { path: path() }
                },
            );
        }
    }
    let signature = fixture.add_file(71, "Library.fsi");
    let signature_provenance = FactId::from_bytes([70; 20]);
    let mut provenance = fixture.document.provenance[0].clone();
    provenance.id = signature_provenance;
    provenance.language = "fsharp-signature".to_owned();
    let source = source_ref(
        fixture.document.repository,
        fixture.document.generation,
        signature,
        fixture.content_hash,
        0,
        SOURCE_BYTES,
    );
    provenance.input_sources = vec![source.clone()];
    provenance.evidence_sources = vec![source];
    fixture.document.provenance.push(provenance);
    let file = fixture.document.files.last_mut().unwrap();
    file.language = "fsharp-signature".to_owned();
    file.provenance = signature_provenance;
    let signature_symbol = fixture.add_entity(72, "run", signature, EntityKind::Variable, None);
    let entity = fixture.document.entities.last_mut().unwrap();
    entity.language = "fsharp-signature".to_owned();
    entity.provenance = signature_provenance;
    entity.visibility = EntityVisibility::Unknown;
    entity.qualified_name = "Library.run".to_owned();
    let scope = SourceSpan::new(signature, 0, SOURCE_BYTES).unwrap();
    for binding in [
        FSharpBinding::Context,
        FSharpBinding::Container {
            path: vec![content_hash(b"Library")],
            roots: vec![content_hash(b"run")],
        },
        FSharpBinding::ImportContext {
            containers: vec![scope],
            opens: vec![],
        },
    ] {
        claim(&mut fixture, signature, 0, SOURCE_BYTES, binding);
    }
    claim(
        &mut fixture,
        signature,
        0,
        8,
        FSharpBinding::SignatureValue {
            path: path(),
            symbol: signature_symbol,
        },
    );
    let project = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Project { .. })
    });
    replace_binding(
        &mut fixture,
        project,
        FSharpBinding::Project {
            members: vec![signature, library, caller],
        },
    );
    fixture.validate();
    (fixture, library, signature, target, signature_symbol)
}

fn assert_signature_closed(document: NormalizedIrDocument) {
    let document = applied(document);
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    assert!(!document.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::RefersTo
            | RelationPredicate::Calls
            | RelationPredicate::DispatchCandidate
    )));
}

fn signature_record_fixture(
    imported: bool,
    name: &str,
) -> (Fixture, FileId, FileId, SymbolId, SymbolId, SymbolId) {
    let (mut fixture, library, signature, target, _) = signature_fixture(imported);
    let mut records = Vec::new();
    for (file, module_id, record_id) in [(library, 80, 82), (signature, 81, 83)] {
        let position = binding_position(&fixture, |binding, source| {
            source.span().file() == file && matches!(binding, FSharpBinding::Container { .. })
        });
        let mut roots = vec![content_hash(b"run"), content_hash(name.as_bytes())];
        roots.sort_unstable();
        replace_binding(
            &mut fixture,
            position,
            FSharpBinding::Container {
                path: vec![content_hash(b"Library")],
                roots,
            },
        );
        let file_record = fixture
            .document
            .files
            .iter()
            .find(|record| record.id == file)
            .unwrap()
            .clone();
        let module = fixture.add_entity(module_id, "Library", file, EntityKind::Module, None);
        let entity = fixture.document.entities.last_mut().unwrap();
        entity.language = file_record.language.clone();
        entity.provenance = file_record.provenance;
        entity.visibility = EntityVisibility::Unknown;
        entity.evidence.source = Some(source_ref(
            fixture.document.repository,
            fixture.document.generation,
            file,
            fixture.content_hash,
            0,
            SOURCE_BYTES,
        ));
        let record = fixture.add_entity(record_id, name, file, EntityKind::Struct, Some(module));
        let entity = fixture.document.entities.last_mut().unwrap();
        entity.language = file_record.language;
        entity.provenance = file_record.provenance;
        entity.visibility = EntityVisibility::Unknown;
        entity.evidence.source = Some(source_ref(
            fixture.document.repository,
            fixture.document.generation,
            file,
            fixture.content_hash,
            17,
            24,
        ));
        if file == signature {
            claim(
                &mut fixture,
                file,
                16,
                24,
                FSharpBinding::SignatureType {
                    path: vec![content_hash(b"Library"), content_hash(name.as_bytes())],
                    symbol: record,
                },
            );
        }
        records.push(record);
    }
    fixture.validate();
    (fixture, library, signature, target, records[0], records[1])
}

#[test]
fn fsharp_signature_record_provenance_folds_entity_and_parser_quality() {
    for imported in [false, true] {
        for (entity_tier, parser_tier, expected_tier) in [
            (
                AnalysisTier::TierC,
                AnalysisTier::TierB,
                AnalysisTier::TierC,
            ),
            (
                AnalysisTier::TierB,
                AnalysisTier::TierC,
                AnalysisTier::TierC,
            ),
            (
                AnalysisTier::TierC,
                AnalysisTier::TierD,
                AnalysisTier::TierD,
            ),
        ] {
            let (mut fixture, _, signature, target, _, signature_type) =
                signature_record_fixture(imported, "Shape");
            let original_context = fixture.document.provenance[0].build_context.digest();
            let signature_provenance = fixture
                .document
                .files
                .iter()
                .find(|file| file.id == signature)
                .unwrap()
                .provenance;
            fixture
                .document
                .entities
                .iter_mut()
                .find(|entity| entity.id == signature_type)
                .unwrap()
                .tier = entity_tier;
            let signature_parser = fixture
                .document
                .provenance
                .iter_mut()
                .find(|parser| parser.id == signature_provenance)
                .unwrap();
            signature_parser.tier = parser_tier;
            signature_parser.build_context =
                BuildContextIdentity::new(content_hash(b"signature-record-parser-context"));
            let signature_context = signature_parser.build_context.digest();
            assert_ne!(original_context, signature_context);
            let mut contexts = [original_context, signature_context];
            contexts.sort_unstable();
            let mut context_bytes = Vec::new();
            for context in contexts {
                context_bytes.extend_from_slice(context.as_bytes());
            }
            let expected_context = BuildContextIdentity::new(content_hash(&context_bytes));
            fixture.validate();
            let document = applied(fixture.document);
            for occurrence in &document.occurrences {
                assert_eq!(
                    occurrence.target,
                    OccurrenceTarget::Resolved { symbol: target }
                );
                let proof = document
                    .provenance
                    .iter()
                    .find(|proof| proof.id == occurrence.provenance)
                    .unwrap();
                assert_eq!(proof.tier, expected_tier);
                assert_eq!(proof.build_context, expected_context);
                assert!(
                    proof
                        .derivation_parents
                        .contains(&FactRef::Entity(signature_type))
                );
                assert!(
                    proof
                        .derivation_parents
                        .contains(&FactRef::Fact(signature_provenance))
                );
            }
        }
    }
}

#[test]
fn fsharp_signature_record_masks_preserve_value_links_and_native_type_provenance() {
    for imported in [false, true] {
        let (fixture, _, _, target, implementation_type, signature_type) =
            signature_record_fixture(imported, "Shape");
        let type_input = fixture
            .document
            .extensions
            .iter()
            .find(|envelope| {
                matches!(
                    decode_fsharp_binding_envelope(envelope).unwrap().binding(),
                    FSharpBinding::SignatureType { .. }
                )
            })
            .unwrap()
            .id;
        let records = fixture
            .document
            .entities
            .iter()
            .filter(|entity| [implementation_type, signature_type].contains(&entity.id))
            .cloned()
            .collect::<Vec<_>>();
        let document = applied(fixture.document);
        for occurrence in &document.occurrences {
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol: target }
            );
            let proof = document
                .provenance
                .iter()
                .find(|proof| proof.id == occurrence.provenance)
                .unwrap();
            assert_eq!(
                proof.rule.as_deref(),
                Some("fsharp-qualified-module-v4.record_signature_mask_source_declared_project")
            );
            assert!(
                proof
                    .derivation_parents
                    .contains(&FactRef::Fact(type_input))
            );
            for record in &records {
                assert!(
                    proof
                        .derivation_parents
                        .contains(&FactRef::Entity(record.id))
                );
                assert!(
                    proof
                        .derivation_parents
                        .contains(&FactRef::Fact(record.provenance))
                );
                assert!(
                    proof
                        .evidence_sources
                        .contains(record.evidence.source.as_ref().unwrap())
                );
                assert!(proof.derivation_parents.contains(&FactRef::Entity(
                    match record.container.unwrap() {
                        ContainerRef::Entity(module) => module,
                        _ => panic!("record has an authored module owner"),
                    }
                )));
            }
        }
        for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|relation| relation.predicate == predicate
                        && relation.object == RelationEndpoint::Entity(target))
                    .count(),
                1
            );
        }
        assert!(!document.relations.iter().any(|relation| matches!(relation.object, RelationEndpoint::Entity(symbol) if symbol == implementation_type || symbol == signature_type)));
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::DispatchCandidate)
        );
    }
}

#[test]
fn fsharp_signature_record_masks_close_missing_duplicate_and_wrong_owners() {
    for mode in 0..6 {
        let (mut fixture, library, signature, _, implementation_type, signature_type) =
            signature_record_fixture(false, "Shape");
        match mode {
            0 => {
                let position = binding_position(&fixture, |binding, _| {
                    matches!(binding, FSharpBinding::SignatureType { .. })
                });
                fixture.document.extensions.remove(position);
            }
            1 => fixture
                .document
                .entities
                .retain(|entity| entity.id != implementation_type),
            2 => claim(
                &mut fixture,
                signature,
                15,
                24,
                FSharpBinding::SignatureType {
                    path: vec![content_hash(b"Library"), content_hash(b"Shape")],
                    symbol: signature_type,
                },
            ),
            3 => {
                fixture
                    .document
                    .entities
                    .iter_mut()
                    .find(|entity| entity.id == implementation_type)
                    .unwrap()
                    .container = Some(ContainerRef::File(library))
            }
            4 => {
                let record = fixture
                    .document
                    .entities
                    .iter()
                    .find(|entity| entity.id == implementation_type)
                    .unwrap()
                    .clone();
                let mut duplicate = record;
                duplicate.id = SymbolId::from_bytes([84; 20]);
                duplicate.evidence.source = Some(source_ref(
                    fixture.document.repository,
                    fixture.document.generation,
                    library,
                    fixture.content_hash,
                    32,
                    40,
                ));
                fixture.document.entities.push(duplicate);
            }
            _ => {
                let owner = match fixture
                    .document
                    .entities
                    .iter()
                    .find(|entity| entity.id == implementation_type)
                    .unwrap()
                    .container
                    .unwrap()
                {
                    ContainerRef::Entity(owner) => owner,
                    _ => unreachable!(),
                };
                let mut duplicate = fixture
                    .document
                    .entities
                    .iter()
                    .find(|entity| entity.id == owner)
                    .unwrap()
                    .clone();
                duplicate.id = SymbolId::from_bytes([85; 20]);
                fixture.document.entities.push(duplicate);
            }
        }
        assert_signature_closed(fixture.document);
    }
}

#[test]
fn fsharp_signature_record_masks_reject_value_alias_and_union_substitutions() {
    for kind in [
        EntityKind::Variable,
        EntityKind::TypeAlias,
        EntityKind::Enum,
    ] {
        let (mut fixture, _, _, _, implementation_type, _) =
            signature_record_fixture(false, "Shape");
        fixture
            .document
            .entities
            .iter_mut()
            .find(|entity| entity.id == implementation_type)
            .unwrap()
            .kind = kind;
        assert_signature_closed(fixture.document);
    }
    let (mut fixture, _, _, _, _, signature_type) = signature_record_fixture(false, "Shape");
    let position = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::SignatureType { .. })
    });
    replace_binding(
        &mut fixture,
        position,
        FSharpBinding::SignatureValue {
            path: vec![content_hash(b"Library"), content_hash(b"Shape")],
            symbol: signature_type,
        },
    );
    assert!(matches!(
        ResolutionEngine::default().resolve(&fixture.document, &Cancellation::new()),
        Err(ResolutionError::UnsupportedIdentityRemap)
    ));
}

#[test]
fn fsharp_signature_record_roots_mask_hidden_and_exposed_import_bindings() {
    let (mut fixture, _, signature, target, _, _) = signature_record_fixture(true, "Library");
    assert_signature_closed(fixture.document.clone());
    let position = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::SignatureType { .. })
    });
    fixture.document.extensions.remove(position);
    let position = binding_position(&fixture, |binding, source| {
        source.span().file() == signature && matches!(binding, FSharpBinding::Container { .. })
    });
    replace_binding(
        &mut fixture,
        position,
        FSharpBinding::Container {
            path: vec![content_hash(b"Library")],
            roots: vec![content_hash(b"run")],
        },
    );
    let document = applied(fixture.document);
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.target == OccurrenceTarget::Resolved { symbol: target })
    );
    assert!(
        document
            .provenance
            .iter()
            .filter(|proof| proof.producer_kind == ProducerKind::Rule)
            .all(|proof| proof.rule.as_deref()
                != Some(
                    "fsharp-qualified-module-v4.record_signature_mask_source_declared_project"
                ))
    );
}

#[test]
fn fsharp_signature_record_masks_keep_profile_override_and_work_fallback() {
    let (mut fixture, _, signature, _, _, _) = signature_record_fixture(false, "Shape");
    fixture
        .document
        .files
        .iter_mut()
        .find(|file| file.id == signature)
        .unwrap()
        .path = "Contract.data".to_owned();
    assert_signature_closed(fixture.document);
    let fixture = signature_record_fixture(false, "Shape").0;
    let engine = ResolutionEngine::new(ResolutionLimits::with_work_limit(1, 1).unwrap());
    let (document, estimate) = engine
        .apply_document_bounded(
            fixture.document,
            ResolverFactContext::new(content_hash(b"bounded-record-mask")),
            &Cancellation::new(),
        )
        .unwrap();
    assert!(!estimate.fits());
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

fn signature_alias_fixture(
    imported: bool,
    name: &str,
) -> (Fixture, FileId, FileId, SymbolId, SymbolId, SymbolId) {
    let (mut fixture, library, signature, target, implementation_alias, signature_alias) =
        signature_record_fixture(imported, name);
    for entity in &mut fixture.document.entities {
        if [implementation_alias, signature_alias].contains(&entity.id) {
            entity.kind = EntityKind::TypeAlias;
        }
    }
    let position = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::SignatureType { .. })
    });
    replace_binding(
        &mut fixture,
        position,
        FSharpBinding::SignatureAlias {
            path: vec![content_hash(b"Library"), content_hash(name.as_bytes())],
            symbol: signature_alias,
        },
    );
    fixture.validate();
    (
        fixture,
        library,
        signature,
        target,
        implementation_alias,
        signature_alias,
    )
}

#[test]
fn fsharp_signature_alias_masks_preserve_value_links_and_native_provenance() {
    for imported in [false, true] {
        let (fixture, _, _, target, implementation_alias, signature_alias) =
            signature_alias_fixture(imported, "Alias");
        let alias_input = fixture
            .document
            .extensions
            .iter()
            .find(|envelope| {
                matches!(
                    decode_fsharp_binding_envelope(envelope).unwrap().binding(),
                    FSharpBinding::SignatureAlias { .. }
                )
            })
            .unwrap()
            .id;
        let aliases = fixture
            .document
            .entities
            .iter()
            .filter(|entity| [implementation_alias, signature_alias].contains(&entity.id))
            .cloned()
            .collect::<Vec<_>>();
        let document = applied(fixture.document);
        for occurrence in &document.occurrences {
            assert_eq!(
                occurrence.target,
                OccurrenceTarget::Resolved { symbol: target }
            );
            let proof = document
                .provenance
                .iter()
                .find(|proof| proof.id == occurrence.provenance)
                .unwrap();
            assert_eq!(
                proof.rule.as_deref(),
                Some("fsharp-qualified-module-v5.alias_signature_mask_source_declared_project")
            );
            assert!(
                proof
                    .derivation_parents
                    .contains(&FactRef::Fact(alias_input))
            );
            for alias in &aliases {
                assert!(
                    proof
                        .derivation_parents
                        .contains(&FactRef::Entity(alias.id))
                );
                assert!(
                    proof
                        .derivation_parents
                        .contains(&FactRef::Fact(alias.provenance))
                );
                assert!(
                    proof
                        .evidence_sources
                        .contains(alias.evidence.source.as_ref().unwrap())
                );
                let ContainerRef::Entity(module) = alias.container.unwrap() else {
                    panic!("alias has an authored module owner");
                };
                assert!(proof.derivation_parents.contains(&FactRef::Entity(module)));
            }
        }
        for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|relation| {
                        relation.predicate == predicate
                            && relation.object == RelationEndpoint::Entity(target)
                    })
                    .count(),
                1
            );
        }
        assert!(!document.relations.iter().any(|relation| {
            matches!(relation.object, RelationEndpoint::Entity(symbol) if [implementation_alias, signature_alias].contains(&symbol))
                || relation.predicate == RelationPredicate::DispatchCandidate
        }));
    }
}

#[test]
fn fsharp_signature_alias_masks_fold_entity_and_parser_quality() {
    let (mut fixture, _, signature, target, _, signature_alias) =
        signature_alias_fixture(false, "Alias");
    let original_context = fixture.document.provenance[0].build_context.digest();
    let parser_id = fixture
        .document
        .files
        .iter()
        .find(|file| file.id == signature)
        .unwrap()
        .provenance;
    fixture
        .document
        .entities
        .iter_mut()
        .find(|entity| entity.id == signature_alias)
        .unwrap()
        .tier = AnalysisTier::TierC;
    let parser = fixture
        .document
        .provenance
        .iter_mut()
        .find(|parser| parser.id == parser_id)
        .unwrap();
    parser.tier = AnalysisTier::TierB;
    parser.build_context =
        BuildContextIdentity::new(content_hash(b"signature-alias-parser-context"));
    let mut contexts = [original_context, parser.build_context.digest()];
    contexts.sort_unstable();
    let mut bytes = Vec::new();
    for context in contexts {
        bytes.extend_from_slice(context.as_bytes());
    }
    let expected_context = BuildContextIdentity::new(content_hash(&bytes));
    fixture.validate();
    let document = applied(fixture.document);
    for occurrence in &document.occurrences {
        assert_eq!(
            occurrence.target,
            OccurrenceTarget::Resolved { symbol: target }
        );
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == occurrence.provenance)
            .unwrap();
        assert_eq!(proof.tier, AnalysisTier::TierC);
        assert_eq!(proof.build_context, expected_context);
        assert!(
            proof
                .derivation_parents
                .contains(&FactRef::Entity(signature_alias))
        );
        assert!(proof.derivation_parents.contains(&FactRef::Fact(parser_id)));
    }
}

#[test]
fn fsharp_signature_alias_and_record_masks_conserve_both_native_type_inputs() {
    let (mut fixture, library, signature, target, implementation_alias, signature_alias) =
        signature_alias_fixture(false, "Alias");
    let mut record_ids = Vec::new();
    for (file, alias_id, record_id) in [
        (library, implementation_alias, 86),
        (signature, signature_alias, 87),
    ] {
        let mut entity = fixture
            .document
            .entities
            .iter()
            .find(|entity| entity.id == alias_id)
            .unwrap()
            .clone();
        entity.id = SymbolId::from_bytes([record_id; 20]);
        entity.kind = EntityKind::Struct;
        entity.canonical_name = "Shape".to_owned();
        entity.display_name = "Shape".to_owned();
        entity.qualified_name = "Library.Shape".to_owned();
        entity.evidence.source = Some(source_ref(
            fixture.document.repository,
            fixture.document.generation,
            file,
            fixture.content_hash,
            32,
            40,
        ));
        record_ids.push(entity.id);
        fixture.document.entities.push(entity);
        let position = binding_position(&fixture, |binding, source| {
            source.span().file() == file && matches!(binding, FSharpBinding::Container { .. })
        });
        let mut roots = vec![
            content_hash(b"run"),
            content_hash(b"Alias"),
            content_hash(b"Shape"),
        ];
        roots.sort_unstable();
        replace_binding(
            &mut fixture,
            position,
            FSharpBinding::Container {
                path: vec![content_hash(b"Library")],
                roots,
            },
        );
        if file == signature {
            claim(
                &mut fixture,
                file,
                31,
                40,
                FSharpBinding::SignatureType {
                    path: vec![content_hash(b"Library"), content_hash(b"Shape")],
                    symbol: SymbolId::from_bytes([record_id; 20]),
                },
            );
        }
    }
    fixture.validate();
    let type_inputs = fixture
        .document
        .extensions
        .iter()
        .filter(|envelope| {
            matches!(
                decode_fsharp_binding_envelope(envelope).unwrap().binding(),
                FSharpBinding::SignatureAlias { .. } | FSharpBinding::SignatureType { .. }
            )
        })
        .map(|envelope| envelope.id)
        .collect::<Vec<_>>();
    assert_eq!(type_inputs.len(), 2);
    let document = applied(fixture.document);
    for occurrence in &document.occurrences {
        assert_eq!(
            occurrence.target,
            OccurrenceTarget::Resolved { symbol: target }
        );
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == occurrence.provenance)
            .unwrap();
        assert_eq!(
            proof.rule.as_deref(),
            Some("fsharp-qualified-module-v5.alias_signature_mask_source_declared_project")
        );
        for input in &type_inputs {
            assert!(proof.derivation_parents.contains(&FactRef::Fact(*input)));
        }
        for entity in record_ids
            .iter()
            .copied()
            .chain([implementation_alias, signature_alias])
        {
            assert!(proof.derivation_parents.contains(&FactRef::Entity(entity)));
        }
    }
}

#[test]
fn fsharp_signature_alias_masks_reconcile_only_resolved_target_gaps() {
    let (mut fixture, _, _, _, _, _) = signature_alias_fixture(false, "Alias");
    let source = fixture.document.occurrences[0].source.clone();
    for detail in [
        "fsharp-qualified-target-unavailable",
        "fsharp-direct-call-target-unavailable",
        "fsharp-body-binding-and-call-analysis-unavailable",
    ] {
        let mut gap = rootlight_ir::SkippedRegion {
            id: FactId::from_bytes([0; 20]),
            repository: fixture.document.repository,
            generation: fixture.document.generation,
            source: source.clone(),
            domain: if detail == "fsharp-qualified-target-unavailable" {
                rootlight_ir::FactDomain::Occurrences
            } else {
                rootlight_ir::FactDomain::Relations
            },
            reason: rootlight_ir::SkippedRegionReason::UnsupportedConstruct,
            detail: detail.to_owned(),
            provenance: fixture.provenance,
            evidence: FactEvidence {
                source: Some(source.clone()),
                derivation: Vec::new(),
            },
        };
        gap.id = rootlight_ir::derive_skipped_region_id(&gap).unwrap();
        fixture.document.skipped_regions.push(gap);
    }
    let document = applied(fixture.document);
    assert_eq!(document.skipped_regions.len(), 3);
    assert_eq!(document.skipped_regions.iter().filter(|gap| {
        gap.detail == "fsharp-project-build-closure-unverified"
            && gap.reason == rootlight_ir::SkippedRegionReason::MissingBuildContext
            && gap.source == source
            && document.provenance.iter().any(|proof| proof.id == gap.provenance
                && proof.rule.as_deref() == Some("fsharp-qualified-module-v5.alias_signature_mask_source_declared_project"))
    }).count(), 2);
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.detail == "fsharp-body-binding-and-call-analysis-unavailable"
            && gap.reason == rootlight_ir::SkippedRegionReason::UnsupportedConstruct
            && gap.provenance == fixture.provenance
    }));
}

#[test]
fn fsharp_signature_alias_masks_close_missing_duplicate_and_wrong_owners() {
    for mode in 0..8 {
        let (mut fixture, library, signature, _, implementation_alias, signature_alias) =
            signature_alias_fixture(false, "Alias");
        match mode {
            0 => {
                let position = binding_position(&fixture, |binding, _| {
                    matches!(binding, FSharpBinding::SignatureAlias { .. })
                });
                fixture.document.extensions.remove(position);
            }
            1 => fixture
                .document
                .entities
                .retain(|entity| entity.id != implementation_alias),
            2 => claim(
                &mut fixture,
                signature,
                15,
                24,
                FSharpBinding::SignatureAlias {
                    path: vec![content_hash(b"Library"), content_hash(b"Alias")],
                    symbol: signature_alias,
                },
            ),
            3 => {
                fixture
                    .document
                    .entities
                    .iter_mut()
                    .find(|entity| entity.id == implementation_alias)
                    .unwrap()
                    .container = Some(ContainerRef::File(library))
            }
            4 => {
                let mut duplicate = fixture
                    .document
                    .entities
                    .iter()
                    .find(|entity| entity.id == implementation_alias)
                    .unwrap()
                    .clone();
                duplicate.id = SymbolId::from_bytes([84; 20]);
                duplicate.evidence.source = Some(source_ref(
                    fixture.document.repository,
                    fixture.document.generation,
                    library,
                    fixture.content_hash,
                    32,
                    40,
                ));
                fixture.document.entities.push(duplicate);
            }
            5 => {
                let ContainerRef::Entity(owner) = fixture
                    .document
                    .entities
                    .iter()
                    .find(|entity| entity.id == implementation_alias)
                    .unwrap()
                    .container
                    .unwrap()
                else {
                    panic!("alias has an authored module owner");
                };
                let mut duplicate = fixture
                    .document
                    .entities
                    .iter()
                    .find(|entity| entity.id == owner)
                    .unwrap()
                    .clone();
                duplicate.id = SymbolId::from_bytes([85; 20]);
                fixture.document.entities.push(duplicate);
            }
            6 => {
                fixture
                    .document
                    .entities
                    .iter_mut()
                    .find(|entity| entity.id == implementation_alias)
                    .unwrap()
                    .kind = EntityKind::Struct
            }
            _ => {
                fixture
                    .document
                    .entities
                    .iter_mut()
                    .find(|entity| entity.id == implementation_alias)
                    .unwrap()
                    .visibility = EntityVisibility::Private
            }
        }
        assert_signature_closed(fixture.document);
    }
}

#[test]
fn fsharp_signature_alias_claims_reject_foreign_kinds_and_source_owners() {
    for kind in [EntityKind::Struct, EntityKind::Variable, EntityKind::Union] {
        let (mut fixture, _, _, _, _, signature_alias) = signature_alias_fixture(false, "Alias");
        fixture
            .document
            .entities
            .iter_mut()
            .find(|entity| entity.id == signature_alias)
            .unwrap()
            .kind = kind;
        assert!(matches!(
            ResolutionEngine::default().resolve(&fixture.document, &Cancellation::new()),
            Err(ResolutionError::UnsupportedIdentityRemap)
        ));
    }
    let (mut fixture, _, signature, _, _, signature_alias) =
        signature_alias_fixture(false, "Alias");
    let position = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::SignatureAlias { .. })
    });
    fixture.document.extensions.remove(position);
    claim(
        &mut fixture,
        signature,
        18,
        24,
        FSharpBinding::SignatureAlias {
            path: vec![content_hash(b"Library"), content_hash(b"Alias")],
            symbol: signature_alias,
        },
    );
    assert!(matches!(
        ResolutionEngine::default().resolve(&fixture.document, &Cancellation::new()),
        Err(ResolutionError::UnsupportedIdentityRemap)
    ));
}

#[test]
fn fsharp_signature_alias_roots_conserve_exposed_and_hidden_import_bindings() {
    let (mut fixture, _, signature, target, _, _) = signature_alias_fixture(true, "Library");
    assert_signature_closed(fixture.document.clone());
    let position = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::SignatureAlias { .. })
    });
    fixture.document.extensions.remove(position);
    let position = binding_position(&fixture, |binding, source| {
        source.span().file() == signature && matches!(binding, FSharpBinding::Container { .. })
    });
    replace_binding(
        &mut fixture,
        position,
        FSharpBinding::Container {
            path: vec![content_hash(b"Library")],
            roots: vec![content_hash(b"run")],
        },
    );
    let document = applied(fixture.document);
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.target == OccurrenceTarget::Resolved { symbol: target })
    );
    assert!(
        document
            .provenance
            .iter()
            .filter(|proof| proof.producer_kind == ProducerKind::Rule)
            .all(|proof| proof.rule.as_deref()
                != Some("fsharp-qualified-module-v5.alias_signature_mask_source_declared_project"))
    );
}

#[test]
fn fsharp_signature_alias_masks_keep_profile_override_and_work_fallback() {
    let (mut fixture, _, signature, _, _, _) = signature_alias_fixture(false, "Alias");
    fixture
        .document
        .files
        .iter_mut()
        .find(|file| file.id == signature)
        .unwrap()
        .path = "Contract.data".to_owned();
    assert_signature_closed(fixture.document);
    let fixture = signature_alias_fixture(false, "Alias").0;
    let engine = ResolutionEngine::new(ResolutionLimits::with_work_limit(1, 1).unwrap());
    let (document, estimate) = engine
        .apply_document_bounded(
            fixture.document,
            ResolverFactContext::new(content_hash(b"bounded-alias-mask")),
            &Cancellation::new(),
        )
        .unwrap();
    assert!(!estimate.fits());
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_signature_profile_claims_preserve_overrides_without_project_pairing() {
    for project_retained in [false, true] {
        for path in ["Contract.fs", "Library.data"] {
            let (mut fixture, _, signature, _, signature_symbol) = signature_fixture(false);
            let signature_entity = fixture
                .document
                .entities
                .iter()
                .find(|entity| entity.id == signature_symbol)
                .unwrap()
                .clone();
            fixture
                .document
                .files
                .iter_mut()
                .find(|file| file.id == signature)
                .unwrap()
                .path = path.to_owned();
            if !project_retained {
                let project = binding_position(&fixture, |binding, _| {
                    matches!(binding, FSharpBinding::Project { .. })
                });
                fixture.document.extensions.remove(project);
            }
            let document = applied(fixture.document);
            assert_eq!(
                document
                    .entities
                    .iter()
                    .find(|entity| entity.id == signature_symbol),
                Some(&signature_entity)
            );
            assert!(
                document
                    .occurrences
                    .iter()
                    .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
            );
            assert!(!document.relations.iter().any(|relation| matches!(
                relation.predicate,
                RelationPredicate::RefersTo
                    | RelationPredicate::Calls
                    | RelationPredicate::DispatchCandidate
            )));
        }
    }
}

#[test]
fn fsharp_signature_masks_keep_implementation_targets_and_source_proofs() {
    for imported in [false, true] {
        let (fixture, _, signature, target, signature_symbol) = signature_fixture(imported);
        let signature_inputs = fixture
            .document
            .extensions
            .iter()
            .filter(|envelope| {
                envelope.evidence.source.as_ref().unwrap().span().file() == signature
            })
            .map(|envelope| envelope.id)
            .collect::<BTreeSet<_>>();
        let sources = fixture
            .document
            .occurrences
            .iter()
            .map(|site| site.source.clone())
            .collect::<Vec<_>>();
        let document = applied(fixture.document);
        for site in &document.occurrences {
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
            assert_ne!(
                site.target,
                OccurrenceTarget::Resolved {
                    symbol: signature_symbol
                }
            );
            assert!(sources.contains(&site.source));
            let proof = document
                .provenance
                .iter()
                .find(|proof| proof.id == site.provenance)
                .unwrap();
            assert_eq!(
                proof.rule.as_deref(),
                Some("fsharp-qualified-module-v3.signature_mask_source_declared_project")
            );
            assert!(
                proof
                    .evidence_sources
                    .iter()
                    .any(|source| source.span().file() == signature
                        && source.span().start_byte() == 0
                        && source.span().end_byte() == 8)
            );
            for input in &signature_inputs {
                assert!(proof.derivation_parents.contains(&FactRef::Fact(*input)));
            }
        }
        for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|relation| relation.predicate == predicate
                        && relation.object == RelationEndpoint::Entity(target))
                    .count(),
                1
            );
        }
        assert!(
            !document
                .relations
                .iter()
                .any(|relation| relation.predicate == RelationPredicate::DispatchCandidate)
        );
    }
}

#[test]
fn fsharp_signature_masks_close_incomplete_duplicate_and_foreign_claims() {
    let (fixture, library, signature, _, signature_symbol) = signature_fixture(false);
    for removed in fixture
        .document
        .extensions
        .iter()
        .enumerate()
        .filter_map(|(index, envelope)| {
            let evidence = decode_fsharp_binding_envelope(envelope).unwrap();
            (evidence.scope().file() == signature
                || (evidence.scope().file() == library
                    && matches!(
                        evidence.binding(),
                        FSharpBinding::ImportContext { .. }
                            | FSharpBinding::Container { .. }
                            | FSharpBinding::Export { .. }
                    )))
            .then_some(index)
        })
    {
        let mut document = fixture.document.clone();
        document.extensions.remove(removed);
        assert_signature_closed(document);
    }
    let mut duplicate = signature_fixture(false).0;
    claim(
        &mut duplicate,
        signature,
        1,
        7,
        FSharpBinding::SignatureValue {
            path: path(),
            symbol: signature_symbol,
        },
    );
    assert_signature_closed(duplicate.document);
    let mut duplicate_export = signature_fixture(false).0;
    let target = duplicate_export.document.entities[0].id;
    claim(
        &mut duplicate_export,
        library,
        1,
        7,
        FSharpBinding::Export {
            path: path(),
            symbol: target,
            slots: Some(1),
        },
    );
    assert_signature_closed(duplicate_export.document);
    let mut foreign = signature_fixture(false).0;
    let position = binding_position(&foreign, |binding, _| {
        matches!(binding, FSharpBinding::SignatureValue { .. })
    });
    let target = foreign.document.entities[0].id;
    replace_binding(
        &mut foreign,
        position,
        FSharpBinding::SignatureValue {
            path: path(),
            symbol: target,
        },
    );
    assert!(matches!(
        ResolutionEngine::default().resolve(&foreign.document, &Cancellation::new()),
        Err(ResolutionError::UnsupportedIdentityRemap)
    ));
}

#[test]
fn fsharp_signature_masks_require_order_stem_container_and_unique_membership() {
    for mode in 0..5 {
        let (mut fixture, library, signature, _, _) = signature_fixture(false);
        let caller = fixture.primary_file;
        match mode {
            0 => {
                let position = binding_position(&fixture, |binding, _| {
                    matches!(binding, FSharpBinding::Project { .. })
                });
                replace_binding(
                    &mut fixture,
                    position,
                    FSharpBinding::Project {
                        members: vec![library, signature, caller],
                    },
                );
            }
            1 => {
                fixture
                    .document
                    .files
                    .iter_mut()
                    .find(|file| file.id == signature)
                    .unwrap()
                    .path = "other/Library.fsi".to_owned()
            }
            2 => {
                let position = binding_position(&fixture, |binding, source| {
                    source.span().file() == signature
                        && matches!(binding, FSharpBinding::Container { .. })
                });
                replace_binding(
                    &mut fixture,
                    position,
                    FSharpBinding::Container {
                        path: vec![content_hash(b"Other")],
                        roots: vec![content_hash(b"run")],
                    },
                );
            }
            3 => {
                let project = fixture.add_file(73, "Other.fsproj");
                claim(
                    &mut fixture,
                    project,
                    0,
                    SOURCE_BYTES,
                    FSharpBinding::Project {
                        members: vec![signature, library],
                    },
                );
            }
            _ => {
                let position = binding_position(&fixture, |binding, _| {
                    matches!(binding, FSharpBinding::Project { .. })
                });
                replace_binding(
                    &mut fixture,
                    position,
                    FSharpBinding::Project {
                        members: vec![signature, caller],
                    },
                );
            }
        }
        assert_signature_closed(fixture.document);
    }
}

#[test]
fn fsharp_signature_masks_hide_values_modules_and_imported_roots() {
    let (mut fixture, library, _, target, _) = signature_fixture(true);
    let position = binding_position(&fixture, |binding, source| {
        source.span().file() == library && matches!(binding, FSharpBinding::Container { .. })
    });
    let mut roots = vec![
        content_hash(b"run"),
        content_hash(b"Library"),
        content_hash(b"Shadow"),
    ];
    roots.sort_unstable();
    replace_binding(
        &mut fixture,
        position,
        FSharpBinding::Container {
            path: vec![content_hash(b"Library")],
            roots,
        },
    );
    claim(
        &mut fixture,
        library,
        8,
        32,
        FSharpBinding::Container {
            path: vec![content_hash(b"Library"), content_hash(b"Shadow")],
            roots: vec![content_hash(b"Library")],
        },
    );
    claim(
        &mut fixture,
        library,
        16,
        24,
        FSharpBinding::Container {
            path: vec![
                content_hash(b"Library"),
                content_hash(b"Shadow"),
                content_hash(b"Library"),
            ],
            roots: vec![],
        },
    );
    import_manifest(
        &mut fixture,
        library,
        vec![
            SourceSpan::new(library, 0, SOURCE_BYTES).unwrap(),
            SourceSpan::new(library, 8, 32).unwrap(),
            SourceSpan::new(library, 16, 24).unwrap(),
        ],
        vec![],
    );
    let document = applied(fixture.document.clone());
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.target == OccurrenceTarget::Resolved { symbol: target })
    );
    let open = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Open { .. })
    });
    let caller = fixture.primary_file;
    replace_binding(
        &mut fixture,
        open,
        FSharpBinding::Open {
            path: vec![content_hash(b"Library"), content_hash(b"Shadow")],
            owner: SourceSpan::new(caller, 0, SOURCE_BYTES).unwrap(),
        },
    );
    assert_signature_closed(fixture.document);

    let (mut fixture, library, _, _, _) = signature_fixture(false);
    let hidden = fixture.add_entity(74, "hidden", library, EntityKind::Variable, None);
    let entity = fixture.document.entities.last_mut().unwrap();
    entity.language = "fsharp".to_owned();
    entity.visibility = EntityVisibility::Unknown;
    let hidden_path = vec![content_hash(b"Library"), content_hash(b"hidden")];
    claim(
        &mut fixture,
        library,
        0,
        8,
        FSharpBinding::Export {
            path: hidden_path.clone(),
            symbol: hidden,
            slots: None,
        },
    );
    for call in [false, true] {
        let position = binding_position(&fixture, |binding, _| {
            if call {
                matches!(binding, FSharpBinding::Call { .. })
            } else {
                matches!(binding, FSharpBinding::Reference { .. })
            }
        });
        replace_binding(
            &mut fixture,
            position,
            if call {
                FSharpBinding::Call {
                    path: hidden_path.clone(),
                    slots: 1,
                }
            } else {
                FSharpBinding::Reference {
                    path: hidden_path.clone(),
                }
            },
        );
    }
    assert_signature_closed(fixture.document);
}

#[test]
fn fsharp_signature_masks_conserve_declared_nested_surface() {
    let (mut fixture, library, signature, target, _) = signature_fixture(false);
    let nested_path = vec![content_hash(b"Library"), content_hash(b"Nested")];
    let value_path = vec![
        content_hash(b"Library"),
        content_hash(b"Nested"),
        content_hash(b"carry"),
    ];
    let mut root_names = vec![content_hash(b"run"), content_hash(b"Nested")];
    root_names.sort_unstable();
    for file in [library, signature] {
        let position = binding_position(&fixture, |binding, source| {
            source.span().file() == file && matches!(binding, FSharpBinding::Container { .. })
        });
        replace_binding(
            &mut fixture,
            position,
            FSharpBinding::Container {
                path: vec![content_hash(b"Library")],
                roots: root_names.clone(),
            },
        );
        claim(
            &mut fixture,
            file,
            8,
            32,
            FSharpBinding::Container {
                path: nested_path.clone(),
                roots: vec![content_hash(b"carry")],
            },
        );
        import_manifest(
            &mut fixture,
            file,
            vec![
                SourceSpan::new(file, 0, SOURCE_BYTES).unwrap(),
                SourceSpan::new(file, 8, 32).unwrap(),
            ],
            vec![],
        );
        let provenance = fixture
            .document
            .files
            .iter()
            .find(|record| record.id == file)
            .unwrap()
            .provenance;
        let symbol = fixture.add_entity(
            if file == signature { 75 } else { 76 },
            "carry",
            file,
            if file == signature {
                EntityKind::Variable
            } else {
                EntityKind::Function
            },
            None,
        );
        let entity = fixture.document.entities.last_mut().unwrap();
        entity.language = if file == signature {
            "fsharp-signature"
        } else {
            "fsharp"
        }
        .to_owned();
        entity.visibility = EntityVisibility::Unknown;
        entity.provenance = provenance;
        entity.evidence.source = Some(source_ref(
            fixture.document.repository,
            fixture.document.generation,
            file,
            fixture.content_hash,
            16,
            24,
        ));
        claim(
            &mut fixture,
            file,
            16,
            24,
            if file == signature {
                FSharpBinding::SignatureValue {
                    path: value_path.clone(),
                    symbol,
                }
            } else {
                FSharpBinding::Export {
                    path: value_path.clone(),
                    symbol,
                    slots: Some(1),
                }
            },
        );
    }
    let document = applied(fixture.document.clone());
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| site.target == OccurrenceTarget::Resolved { symbol: target })
    );
    let position = binding_position(
        &fixture,
        |binding, _| matches!(binding, FSharpBinding::SignatureValue { path, .. } if path == &value_path),
    );
    fixture.document.extensions.remove(position);
    assert_signature_closed(fixture.document);
}

#[test]
fn fsharp_signature_masks_preserve_slot_checks_and_bounded_fallback() {
    let (mut fixture, _, _, target, _) = signature_fixture(false);
    let position = binding_position(&fixture, |binding, _| {
        matches!(binding, FSharpBinding::Call { .. })
    });
    replace_binding(
        &mut fixture,
        position,
        FSharpBinding::Call {
            path: path(),
            slots: 2,
        },
    );
    let document = applied(fixture.document);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::RefersTo
                && relation.object == RelationEndpoint::Entity(target))
            .count(),
        1
    );
    assert!(!document.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls | RelationPredicate::DispatchCandidate
    )));
    let fixture = signature_fixture(true).0;
    let engine = ResolutionEngine::new(ResolutionLimits::with_work_limit(1, 1).unwrap());
    let (document, estimate) = engine
        .apply_document_bounded(
            fixture.document,
            ResolverFactContext::new(content_hash(b"bounded-signature-proof")),
            &Cancellation::new(),
        )
        .unwrap();
    assert!(!estimate.fits());
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_qualified_quoted_terminals_preserve_written_identity_and_exact_targets() {
    for (written, canonical) in [("``run``", "run"), ("``r. x``", "r. x")] {
        let (fixture, target) = fixture_with_terminal_name(written, canonical);
        let expected_sources = fixture
            .document
            .occurrences
            .iter()
            .map(|site| site.source.clone())
            .collect::<Vec<_>>();
        let document = applied(fixture.document);
        assert_eq!(document.entities[0].canonical_name, written);
        assert_eq!(document.entities[0].display_name, written);
        for site in &document.occurrences {
            assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
            assert!(expected_sources.contains(&site.source));
        }
        for predicate in [RelationPredicate::RefersTo, RelationPredicate::Calls] {
            assert_eq!(
                document
                    .relations
                    .iter()
                    .filter(|relation| relation.predicate == predicate
                        && relation.object == RelationEndpoint::Entity(target))
                    .count(),
                1
            );
        }
    }
}

#[test]
fn fsharp_qualified_malformed_quoted_terminals_fail_closed() {
    for written in ["``run", "run``", "````", "``r``x", "``r``x``", "`run`"] {
        let (fixture, _) = fixture_with_terminal_name(written, "run");
        assert!(
            matches!(
                ResolutionEngine::default().resolve(&fixture.document, &Cancellation::new()),
                Err(ResolutionError::UnsupportedIdentityRemap)
            ),
            "malformed terminal {written:?}"
        );
    }
}

#[test]
fn fsharp_qualified_links_preserve_source_and_all_five_proof_inputs() {
    let (fixture, _, _, target) = fsharp_fixture();
    let expected_sources = fixture
        .document
        .occurrences
        .iter()
        .map(|site| site.source.clone())
        .collect::<Vec<_>>();
    let expected_inputs = fixture
        .document
        .extensions
        .iter()
        .map(|claim| claim.id)
        .collect::<BTreeSet<_>>();
    let document = applied(fixture.document);
    assert_eq!(document.occurrences.len(), 2);
    for site in &document.occurrences {
        assert_eq!(site.target, OccurrenceTarget::Resolved { symbol: target });
        assert!(expected_sources.contains(&site.source));
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == site.provenance)
            .unwrap();
        assert_eq!(
            proof.rule.as_deref(),
            Some("fsharp-qualified-module-v1.source_declared_project")
        );
        assert!(proof.evidence_sources.contains(&site.source));
        assert_eq!(
            proof
                .derivation_parents
                .iter()
                .filter(
                    |parent| matches!(parent, FactRef::Fact(id) if expected_inputs.contains(id))
                )
                .count(),
            5
        );
        assert!(document.relations.iter().any(|relation| relation.subject
            == RelationEndpoint::Occurrence(site.id)
            && relation.object == RelationEndpoint::Entity(target)
            && relation.predicate
                == if site.role == OccurrenceRole::CallSite {
                    RelationPredicate::Calls
                } else {
                    RelationPredicate::RefersTo
                }));
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::DispatchCandidate)
    );
}

#[test]
fn fsharp_qualified_links_close_on_missing_or_duplicate_native_claims() {
    let (fixture, library, _, target) = fsharp_fixture();
    for removed in 0..fixture.document.extensions.len() {
        let mut document = fixture.document.clone();
        document.extensions.remove(removed);
        let document = applied(document);
        let expected = match removed {
            3 => 1,
            4 => 1,
            _ => 0,
        };
        assert_eq!(
            document
                .occurrences
                .iter()
                .filter(|site| matches!(site.target, OccurrenceTarget::Resolved { .. }))
                .count(),
            expected,
            "removed claim {removed}"
        );
    }
    let mut duplicated = fixture;
    claim(
        &mut duplicated,
        library,
        1,
        7,
        FSharpBinding::Export {
            path: path(),
            symbol: target,
            slots: Some(1),
        },
    );
    let duplicate = applied(duplicated.document);
    assert!(
        duplicate
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_qualified_calls_require_exact_slots_and_source_order() {
    for slots in [1, 2, 3] {
        let (mut fixture, _, _, _) = fsharp_fixture();
        fixture.document.extensions.remove(4);
        let caller = fixture.primary_file;
        claim(
            &mut fixture,
            caller,
            32,
            48,
            FSharpBinding::Call {
                path: path(),
                slots,
            },
        );
        let document = applied(fixture.document);
        assert!(
            document
                .occurrences
                .iter()
                .any(|site| site.role == OccurrenceRole::Reference
                    && matches!(site.target, OccurrenceTarget::Resolved { .. }))
        );
        assert_eq!(
            document
                .relations
                .iter()
                .filter(|relation| relation.predicate == RelationPredicate::Calls)
                .count(),
            usize::from(slots == 1)
        );
    }
    let (mut fixture, library, project, _) = fsharp_fixture();
    fixture.document.extensions.pop();
    let caller = fixture.primary_file;
    claim(
        &mut fixture,
        project,
        0,
        SOURCE_BYTES,
        FSharpBinding::Project {
            members: vec![caller, library],
        },
    );
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_qualified_projects_do_not_cross_or_hide_signature_constraints() {
    let (mut fixture, library, project, _) = fsharp_fixture();
    fixture.document.extensions.pop();
    let caller = fixture.primary_file;
    claim(
        &mut fixture,
        project,
        0,
        SOURCE_BYTES,
        FSharpBinding::Project {
            members: vec![library],
        },
    );
    let other = fixture.add_file(30, "Other.fsproj");
    claim(
        &mut fixture,
        other,
        0,
        SOURCE_BYTES,
        FSharpBinding::Project {
            members: vec![caller],
        },
    );
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
    let (mut fixture, library, project, _) = fsharp_fixture();
    fixture.document.extensions.pop();
    let signature = fixture.add_file(31, "Library.fsi");
    fixture.document.files.last_mut().unwrap().language = "fsharp".to_owned();
    claim(
        &mut fixture,
        signature,
        0,
        SOURCE_BYTES,
        FSharpBinding::Context,
    );
    let caller = fixture.primary_file;
    claim(
        &mut fixture,
        project,
        0,
        SOURCE_BYTES,
        FSharpBinding::Project {
            members: vec![signature, library, caller],
        },
    );
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_qualified_resolution_charges_preparation_and_preserves_bounded_fallback() {
    let (fixture, _, _, _) = fsharp_fixture();
    let engine = ResolutionEngine::new(ResolutionLimits::with_work_limit(1, 1).unwrap());
    assert!(
        !engine
            .estimate_work(&fixture.document, &Cancellation::new())
            .unwrap()
            .fits()
    );
    assert!(matches!(
        engine.resolve(&fixture.document, &Cancellation::new()),
        Err(ResolutionError::WorkLimit { maximum: 1 })
    ));
    let (document, estimate) = engine
        .apply_document_bounded(
            fixture.document,
            ResolverFactContext::new(content_hash(b"bounded-fsharp-proof")),
            &Cancellation::new(),
        )
        .unwrap();
    assert!(!estimate.fits());
    assert!(
        document
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_qualified_values_and_conflicting_membership_never_invent_calls() {
    let (mut fixture, library, _, target) = fsharp_fixture();
    fixture.document.entities[0].kind = EntityKind::Variable;
    fixture.document.extensions.remove(2);
    claim(
        &mut fixture,
        library,
        0,
        8,
        FSharpBinding::Export {
            path: path(),
            symbol: target,
            slots: None,
        },
    );
    let document = applied(fixture.document);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::RefersTo)
            .count(),
        1
    );
    assert!(!document.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::Calls | RelationPredicate::DispatchCandidate
    )));
    let (mut fixture, library, _, _) = fsharp_fixture();
    let other = fixture.add_file(32, "Other.fsproj");
    claim(
        &mut fixture,
        other,
        0,
        SOURCE_BYTES,
        FSharpBinding::Project {
            members: vec![library],
        },
    );
    assert!(
        applied(fixture.document)
            .occurrences
            .iter()
            .all(|site| !matches!(site.target, OccurrenceTarget::Resolved { .. }))
    );
}

#[test]
fn fsharp_qualified_duplicate_uses_and_unrelated_gaps_stay_closed() {
    let (mut fixture, _, _, _) = fsharp_fixture();
    let caller = fixture.primary_file;
    claim(
        &mut fixture,
        caller,
        16,
        24,
        FSharpBinding::Reference {
            path: vec![content_hash(b"Other"), content_hash(b"run")],
        },
    );
    let document = applied(fixture.document);
    assert_eq!(
        document
            .relations
            .iter()
            .filter(|relation| relation.predicate == RelationPredicate::Calls)
            .count(),
        1
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|relation| relation.predicate == RelationPredicate::RefersTo)
    );
    let (mut fixture, _, _, _) = fsharp_fixture();
    for (index, detail) in [
        "fsharp-qualified-target-unavailable",
        "fsharp-body-binding-and-call-analysis-unavailable",
    ]
    .into_iter()
    .enumerate()
    {
        let source = fixture.document.occurrences[0].source.clone();
        let mut gap = rootlight_ir::SkippedRegion {
            id: FactId::from_bytes([40 + u8::try_from(index).unwrap(); 20]),
            repository: fixture.document.repository,
            generation: fixture.document.generation,
            source: source.clone(),
            domain: rootlight_ir::FactDomain::Relations,
            reason: rootlight_ir::SkippedRegionReason::UnsupportedConstruct,
            detail: detail.to_owned(),
            provenance: fixture.provenance,
            evidence: FactEvidence {
                source: Some(source),
                derivation: Vec::new(),
            },
        };
        gap.id = rootlight_ir::derive_skipped_region_id(&gap).unwrap();
        fixture.document.skipped_regions.push(gap);
    }
    let document = applied(fixture.document);
    assert_eq!(document.skipped_regions.len(), 2);
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "fsharp-project-build-closure-unverified"
        && gap.reason == rootlight_ir::SkippedRegionReason::MissingBuildContext));
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "fsharp-body-binding-and-call-analysis-unavailable"
        && gap.reason == rootlight_ir::SkippedRegionReason::UnsupportedConstruct));
}
