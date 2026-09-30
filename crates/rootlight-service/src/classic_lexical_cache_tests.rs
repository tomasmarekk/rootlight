//! Identical native module bytes must retain independent container identities.
//! Lexical range reuse cannot reuse a resolved target or a generation-bound
//! source witness, including after one copy changes and durable state reopens.

use super::*;

const CLIENT: &[u8] = b"Attribute VB_Name = \"Client\"\r\nPublic Sub Run()\r\nDim cached As New Worker\r\nDim plain As Worker\r\nSet plain = cached\r\nIf cached Is Nothing Then\r\nEnd If\r\nEnd Sub\r\n";
const WORKER: &[u8] = b"Attribute VB_Name = \"Worker\"\r\nPrivate Sub Class_Initialize()\r\nEnd Sub\r\nPublic Sub Touch()\r\nEnd Sub\r\n";

#[test]
fn repeated_native_lexical_bytes_keep_local_targets_after_one_copy_changes() {
    let root = tempdir();
    let storage = tempdir();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let ordinary_client = String::from_utf8(CLIENT.to_vec())
        .unwrap()
        .replace("As New Worker", "As Worker");
    let original = fixture::typed_book(&[("Client", CLIENT, false), ("Worker", WORKER, true)]);
    let mutated = fixture::typed_book(&[
        ("Client", ordinary_client.as_bytes(), false),
        ("Worker", WORKER, true),
    ]);
    let mut first_identity = None;
    for changed in [false, true, false] {
        fs::write(root.path().join("first.xlsm"), &original).unwrap();
        fs::write(
            root.path().join("second.xlsm"),
            if changed { &mutated } else { &original },
        )
        .unwrap();
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        let receipt = service.index_repository(root.path(), &deadline()).unwrap();
        assert_eq!(receipt.discovered_inputs, 2);
        assert_eq!(receipt.extracted_files, 4);
        assert_eq!(receipt.indexed_files, 6);
        assert_eq!(
            receipt,
            service.index_repository(root.path(), &deadline()).unwrap()
        );
        let identity = assert_local_initializers(&service, &receipt, changed);
        if let Some(prior) = first_identity {
            assert_eq!(
                identity, prior,
                "the untouched native container retains its own identities"
            );
        } else {
            first_identity = Some(identity);
        }
        drop(service);
        let mut service =
            FirstSliceService::new_durable(4, paths.state_dir(), &deadline()).unwrap();
        assert_eq!(
            assert_local_initializers(&service, &receipt, changed),
            identity
        );
        assert_eq!(
            receipt,
            service.index_repository(root.path(), &deadline()).unwrap()
        );
        let prepared = service
            .prepare_repository_with_options(
                root.path(),
                FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
                &deadline(),
            )
            .unwrap();
        let rebuilt = service.publish_prepared(prepared, &deadline()).unwrap();
        assert!(receipt.logical_snapshot.is_some());
        assert_eq!(receipt.logical_snapshot, rebuilt.logical_snapshot);
        assert_eq!(
            assert_local_initializers(&service, &rebuilt, changed),
            identity
        );
    }
}

fn assert_local_initializers(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    changed: bool,
) -> (FileId, SymbolId, SymbolId) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert!(
        !document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "resolution-work-bounded")
    );
    let mappings: BTreeMap<_, _> = document
        .source_mappings
        .iter()
        .filter(|mapping| mapping.kind == SourceMappingKind::ExtractedFromContainer)
        .map(|mapping| (mapping.from.span().file(), mapping.to.span().file()))
        .collect();
    assert_eq!(mappings.len(), 4);
    let containers: BTreeMap<_, _> = document
        .files
        .iter()
        .filter(|file| file.path == "first.xlsm" || file.path == "second.xlsm")
        .map(|file| (file.id, file.path.as_str()))
        .collect();
    assert_eq!(containers.len(), 2);
    let initializers: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "Class_Initialize")
        .collect();
    assert_eq!(initializers.len(), 2);
    assert_ne!(initializers[0].id, initializers[1].id);
    assert_ne!(
        initializers[0]
            .evidence
            .source
            .as_ref()
            .unwrap()
            .span()
            .file(),
        initializers[1]
            .evidence
            .source
            .as_ref()
            .unwrap()
            .span()
            .file()
    );
    assert_eq!(
        initializers[0]
            .evidence
            .source
            .as_ref()
            .unwrap()
            .content_hash(),
        initializers[1]
            .evidence
            .source
            .as_ref()
            .unwrap()
            .content_hash()
    );
    let clients: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| entity.canonical_name == "cached")
        .collect();
    assert_eq!(clients.len(), 2);
    assert_ne!(clients[0].id, clients[1].id);
    assert_ne!(
        clients[0].evidence.source.as_ref().unwrap().span().file(),
        clients[1].evidence.source.as_ref().unwrap().span().file()
    );
    assert_eq!(
        clients[0].evidence.source.as_ref().unwrap().content_hash()
            == clients[1].evidence.source.as_ref().unwrap().content_hash(),
        !changed
    );
    let calls: Vec<_> = document
        .relations
        .iter()
        .filter(|edge| {
            edge.predicate == RelationPredicate::Calls
                && document.provenance.iter().any(|proof| {
                    proof.id == edge.provenance
                        && proof.rule.as_deref()
                            == Some("classic-automatic-initialization-v1.when_nothing")
                })
        })
        .collect();
    assert_eq!(calls.len(), if changed { 2 } else { 4 });
    let mut counts = BTreeMap::new();
    for edge in &calls {
        let RelationEndpoint::Occurrence(id) = edge.subject else {
            panic!("automatic initializer retains its exact use occurrence")
        };
        let site = document
            .occurrences
            .iter()
            .find(|site| site.id == id)
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = site.target else {
            panic!("value read remains bound to its local storage")
        };
        let variable = document
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(variable.canonical_name, "cached");
        let initializer = initializers
            .iter()
            .find(|entity| edge.object == RelationEndpoint::Entity(entity.id))
            .unwrap();
        let source = initializer.evidence.source.as_ref().unwrap();
        let container = mappings[&site.file];
        assert_eq!(mappings[&source.span().file()], container);
        assert_eq!(
            mappings[&variable.evidence.source.as_ref().unwrap().span().file()],
            container
        );
        assert_eq!(site.source.generation(), receipt.generation);
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(edge.evidence.source.as_ref(), Some(&site.source));
        let proof = document
            .provenance
            .iter()
            .find(|proof| proof.id == edge.provenance)
            .unwrap();
        assert!(
            proof
                .derivation_parents
                .contains(&FactRef::Entity(variable.id))
        );
        assert!(
            proof
                .derivation_parents
                .contains(&FactRef::Entity(initializer.id))
        );
        assert!(proof.evidence_sources.contains(&site.source));
        for witness in &proof.evidence_sources {
            assert_eq!(witness.generation(), receipt.generation);
            assert_eq!(
                mappings
                    .get(&witness.span().file())
                    .copied()
                    .unwrap_or(witness.span().file()),
                container
            );
        }
        *counts.entry(containers[&container]).or_insert(0) += 1;
    }
    assert_eq!(counts.get("first.xlsm"), Some(&2));
    assert_eq!(
        counts.get("second.xlsm").copied().unwrap_or(0),
        if changed { 0 } else { 2 }
    );
    let relationships = service
        .symbol_relationships(
            receipt.generation,
            initializers.iter().map(|entity| entity.id).collect(),
            vec![RelationFamily::Calls],
            Some(RelationDirection::Inbound),
            0,
            25,
            0,
            &deadline(),
        )
        .unwrap();
    assert_eq!(
        relationships.data.returned_edges,
        if changed { 2 } else { 4 }
    );
    assert!(!relationships.data.truncated);
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            calls
                .iter()
                .map(|edge| edge.evidence.source.clone().unwrap())
                .collect(),
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), calls.len());
    for chunk in &read.data.chunks {
        assert!(chunk.extracted_from.is_some());
        assert_eq!(chunk.bytes, b"cached");
    }
    let first_client = clients
        .iter()
        .find(|entity| {
            containers[&mappings[&entity.evidence.source.as_ref().unwrap().span().file()]]
                == "first.xlsm"
        })
        .unwrap();
    let first_initializer = initializers
        .iter()
        .find(|entity| {
            containers[&mappings[&entity.evidence.source.as_ref().unwrap().span().file()]]
                == "first.xlsm"
        })
        .unwrap();
    (
        first_client.evidence.source.as_ref().unwrap().span().file(),
        first_client.id,
        first_initializer.id,
    )
}
