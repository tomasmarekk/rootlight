//! Authenticated GYP file mentions do not resolve build-target symbols.

use super::*;

const GYP: &[u8] = br#"{'targets': [{'target_name': 'present.cc',
 'sources': ['present.cc', '../shared.cc', 'missing.cc', '../../outside.cc',
             '/absolute.cc', 'C:/outside.cc', '<(generated)', 'escaped\\name.cc'],
 'conditions': [['mode == 1', {'sources': ['conditional.cc']}]]}]}
"#;

#[test]
fn literal_gyp_paths_bind_only_authenticated_files_and_preserve_conditional_gaps() {
    let repository = durable_test_tempdir();
    fs::create_dir(repository.path().join("native")).unwrap();
    fs::write(repository.path().join("native/binding.gyp"), GYP).unwrap();
    for path in [
        "native/present.cc",
        "shared.cc",
        "native/conditional.cc",
        "outside.cc",
        "native/absolute.cc",
    ] {
        fs::write(repository.path().join(path), b"// captured source\n").unwrap();
    }
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let inputs: Vec<_> = [
        ("native/binding.gyp", GYP),
        ("native/present.cc", b"// captured source\n".as_slice()),
        ("shared.cc", b"// captured source\n".as_slice()),
        ("native/conditional.cc", b"// captured source\n".as_slice()),
        ("outside.cc", b"// captured source\n".as_slice()),
        ("native/absolute.cc", b"// captured source\n".as_slice()),
    ]
    .into_iter()
    .map(|(path, bytes)| {
        let path = RelativePath::parse(Path::new(path)).unwrap();
        let file = rootlight_ids::derive_file(rootlight_ids::FileIdentity {
            repository: receipt.repository,
            path_identity: path.identity_bytes(),
        })
        .id();
        RustSourceInput {
            snapshot: SourceSnapshot::from_persisted(
                receipt.repository,
                path,
                file,
                content_hash(bytes),
                bytes.to_vec(),
            )
            .unwrap(),
            encoding: "utf8".to_owned(),
            generated: false,
            origins: Vec::new(),
        }
    })
    .collect();
    let usage = IrDocumentUsage::measure(document, service.analysis_limits.ir()).unwrap();
    let mut exact = service.analysis_limits.ir().clone();
    exact.max_relations = usage.relations();
    exact.max_provenance_records = usage.provenance_records();
    exact.max_total_records = usage.total_records();
    exact.max_total_string_bytes = usage.total_string_bytes();
    let mut replacement = document.clone();
    crate::gyp_source_files::bind(&mut replacement, &inputs, &exact, &deadline(), &mut |_| {
        Ok(())
    })
    .unwrap();
    let replacement =
        rootlight_ir::canonicalize_ir_document(replacement, &exact, &service.extensions).unwrap();
    assert_eq!(&replacement, document);
    let mut refused = document.clone();
    let before = refused.clone();
    assert!(matches!(
        crate::gyp_source_files::bind(
            &mut refused,
            &[],
            service.analysis_limits.ir(),
            &deadline(),
            &mut |_| Err(FirstSliceError::Limits)
        ),
        Err(FirstSliceError::Limits)
    ));
    assert_eq!(refused, before);
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        crate::gyp_source_files::bind(
            &mut refused,
            &[],
            service.analysis_limits.ir(),
            &cancelled,
            &mut |_| Ok(())
        ),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(refused, before);
    let mentions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "gyp.source_path.reference")
        .collect();
    assert_eq!(mentions.len(), 7);
    let mut joined = 0;
    for site in mentions {
        assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
        let source = &site.source;
        let span = source.span();
        let raw = &GYP[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        let expected = match raw {
            b"present.cc" => Some("native/present.cc"),
            b"../shared.cc" => Some("shared.cc"),
            b"conditional.cc" => Some("native/conditional.cc"),
            _ => None,
        };
        let relations: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.predicate == RelationPredicate::RefersTo
            })
            .collect();
        let gaps: Vec<_> = document
            .skipped_regions
            .iter()
            .filter(|gap| {
                gap.detail == "gyp-written-source-path-resolution-unavailable"
                    && gap.source == *source
            })
            .collect();
        if let Some(path) = expected {
            let relative = RelativePath::parse(Path::new(path)).unwrap();
            let file = rootlight_ids::derive_file(rootlight_ids::FileIdentity {
                repository: receipt.repository,
                path_identity: relative.identity_bytes(),
            })
            .id();
            assert!(document.files.iter().any(|record| record.id == file));
            assert_eq!(relations.len(), 1);
            assert_eq!(relations[0].object, RelationEndpoint::File(file));
            assert_eq!(relations[0].evidence.source.as_ref(), Some(source));
            assert!(
                relations[0]
                    .evidence
                    .derivation
                    .contains(&FactRef::File(file))
            );
            assert!(
                relations[0]
                    .evidence
                    .derivation
                    .contains(&FactRef::Fact(site.id))
            );
            assert!(gaps.is_empty());
            joined += 1;
        } else {
            assert!(relations.is_empty());
            assert_eq!(gaps.len(), 1);
        }
    }
    assert_eq!(joined, 3);
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "gyp-conditional-configuration-not-evaluated")
    );
    assert_eq!(
        document
            .skipped_regions
            .iter()
            .filter(|gap| gap.detail == "gyp-plain-literal-source-path-unavailable")
            .count(),
        2
    );
    assert_eq!(
        document
            .entities
            .iter()
            .filter(|entity| entity.kind == EntityKind::BuildTarget
                && entity.canonical_name == "present.cc")
            .count(),
        1
    );
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DependsOn)
    );
}

#[test]
fn changed_gyp_path_and_removed_target_replace_file_relations_without_reparsing_gyp() {
    const ORIGINAL: &str = "{'targets': [{'target_name': 'first.cc', 'sources': ['first.cc'] }]}\n";
    let repository = durable_test_tempdir();
    fs::write(repository.path().join("binding.gyp"), ORIGINAL).unwrap();
    fs::write(repository.path().join("first.cc"), b"// first\n").unwrap();
    fs::write(repository.path().join("other.cc"), b"// other\n").unwrap();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let first = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_file_binding(&service, &first, Some("first.cc"));
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        first.generation
    );
    let changed = ORIGINAL.replace("'sources': ['first.cc']", "'sources': ['other.cc']");
    fs::write(repository.path().join("binding.gyp"), &changed).unwrap();
    let next = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(
        service
            .incremental_evidence(next.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    assert_file_binding(&service, &next, Some("other.cc"));
    fs::remove_file(repository.path().join("other.cc")).unwrap();
    let removed = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(
        service
            .incremental_evidence(removed.generation)
            .unwrap()
            .parsed_files(),
        0
    );
    assert_file_binding(&service, &removed, None);
    drop(service);
    let reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_file_binding(&reopened, &removed, None);
}

fn assert_file_binding(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    expected: Option<&str>,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let mentions: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| site.syntax_kind == "gyp.source_path.reference")
        .collect();
    assert_eq!(mentions.len(), 1);
    let mention = mentions[0];
    assert!(matches!(
        mention.target,
        OccurrenceTarget::Unresolved { .. }
    ));
    let relations: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.subject == RelationEndpoint::Occurrence(mention.id)
                && edge.predicate == RelationPredicate::RefersTo
        })
        .collect();
    let gaps: Vec<_> = document
        .skipped_regions
        .iter()
        .filter(|gap| {
            gap.detail == "gyp-written-source-path-resolution-unavailable"
                && gap.source == mention.source
        })
        .collect();
    if let Some(path) = expected {
        let relative = RelativePath::parse(Path::new(path)).unwrap();
        let expected = rootlight_ids::derive_file(rootlight_ids::FileIdentity {
            repository: receipt.repository,
            path_identity: relative.identity_bytes(),
        })
        .id();
        assert_eq!(relations.len(), 1);
        assert_eq!(relations[0].object, RelationEndpoint::File(expected));
        assert!(gaps.is_empty());
    } else {
        assert!(relations.is_empty());
        assert_eq!(gaps.len(), 1);
        assert!(!document.files.iter().any(|file| file.path == "other.cc"));
    }
    assert!(
        !document
            .relations
            .iter()
            .any(|edge| edge.predicate == RelationPredicate::DependsOn)
    );
}
