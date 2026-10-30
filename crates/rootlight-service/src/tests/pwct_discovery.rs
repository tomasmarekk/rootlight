//! Normal PWCT discovery and durable publication retain native visual ownership.
//! Source bytes, public symbol lookup and lifecycle reuse are independently checked.

use super::*;
use rootlight_ids::SymbolId;
use rootlight_ir::{
    ContainerRef, OccurrenceRole, OccurrenceTarget, RelationEndpoint, RelationPredicate,
};

fn synthetic() -> Vec<u8> {
    b"cVisualSourceFileVersion='1.0.0'\ncVisualLanguageName='RingPWCT'\ncNaturalLanguageName='english'\nnStepsID=3\nnInteractionsID=1\naStepsTable=[[1,0,[[\"name\",\"Start\"],[\"active\",1],[\"code\",\"\"],[\"interactionid\",1],[\"visible\",1],[\"stepnumber\",1],[\"steptype\",1]]],[2,1,[[\"name\",\"Functions\"],[\"active\",1],[\"code\",\"see 1\"],[\"interactionid\",1],[\"visible\",1],[\"stepnumber\",1],[\"steptype\",2]]]]\naInteractionsTable=[[1,1,\"print\",\"01/01/2023\",\"12:00:00\",\"\"]]\n".to_vec()
}

fn assert_source(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    raw: &[u8],
    expected: usize,
) -> BTreeMap<String, SymbolId> {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    let file = doc.files.iter().find(|file| file.path == path).unwrap();
    assert_eq!(file.language, "pwct");
    assert_eq!(file.content_hash, content_hash(raw));
    let source = file.evidence.source.as_ref().unwrap();
    assert_eq!(source.generation(), receipt.generation);
    assert_eq!(
        (source.span().start_byte(), source.span().end_byte()),
        (0, u64::try_from(raw.len()).unwrap())
    );
    let read = service
        .source_read(receipt.generation, vec![source.clone()], &deadline())
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, raw);
    assert_eq!(read.data.chunks[0].language, "pwct");
    let provenance = doc
        .provenance
        .iter()
        .find(|entry| entry.id == file.provenance)
        .unwrap();
    assert_eq!(provenance.language, "pwct");
    assert_eq!(
        provenance.frontend_version.as_deref(),
        Some("pwct-visual-source-1")
    );
    assert_eq!(provenance.binary_digest, PwctParseProvider::binary_digest());
    let properties: Vec<_> = doc
        .entities
        .iter()
        .filter(|entity| {
            entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == file.id)
        })
        .collect();
    assert_eq!(properties.len(), expected);
    assert!(
        properties
            .iter()
            .all(|entity| entity.kind == EntityKind::Property && entity.language == "pwct")
    );
    let definitions: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().file() == file.id && site.role == OccurrenceRole::Definition
        })
        .collect();
    assert_eq!(definitions.len(), expected);
    let mut symbols = BTreeMap::new();
    for property in &properties {
        let id: u64 = property.canonical_name.parse().unwrap();
        assert!(id > 0);
        assert!(
            symbols
                .insert(format!("{path}:{id}"), property.id)
                .is_none()
        );
        let definition = definitions
            .iter()
            .find(|site| {
                site.target
                    == OccurrenceTarget::Resolved {
                        symbol: property.id,
                    }
            })
            .unwrap();
        assert_eq!(
            &raw[usize::try_from(definition.source.span().start_byte()).unwrap()
                ..usize::try_from(definition.source.span().end_byte()).unwrap()],
            property.canonical_name.as_bytes()
        );
        let parent = match property.container.unwrap() {
            ContainerRef::File(owner) => RelationEndpoint::File(owner),
            ContainerRef::Entity(owner) => RelationEndpoint::Entity(owner),
            ContainerRef::Repository(_) => panic!("visual rows have file or visual-node ownership"),
        };
        assert_eq!(
            doc.relations
                .iter()
                .filter(|edge| edge.predicate == RelationPredicate::Contains
                    && edge.subject == parent
                    && edge.object == RelationEndpoint::Entity(property.id))
                .count(),
            1
        );
    }
    assert!(
        !doc.occurrences
            .iter()
            .any(|site| site.source.span().file() == file.id
                && site.role == OccurrenceRole::CallSite)
    );
    if expected == 2 {
        assert!(
            !properties
                .iter()
                .any(|property| property.canonical_name == "3")
        );
    }
    let root = properties
        .iter()
        .find(|property| property.canonical_name == "1")
        .unwrap();
    assert_eq!(root.container, Some(ContainerRef::File(file.id)));
    let located = service
        .code_locate(
            receipt.generation,
            "1".to_owned(),
            LocateMode::Exact,
            25,
            0,
            &deadline(),
        )
        .unwrap();
    let hits: Vec<_> = located
        .data
        .hits
        .iter()
        .filter(|hit| hit.path == path && hit.symbol == Some(root.id))
        .collect();
    assert_eq!(
        hits.len(),
        1,
        "file/source-only matches cannot satisfy native exact lookup"
    );
    assert_eq!(
        (hits[0].kind.as_str(), hits[0].language.as_str()),
        ("property", "pwct")
    );
    let explained = service
        .symbol_explain(receipt.generation, root.id, &deadline())
        .unwrap();
    assert_eq!(explained.data.entity.canonical_name, "1");
    assert_eq!(explained.data.entity.kind, EntityKind::Property);
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, "pwct");
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    assert_ne!(coverage.status, CoverageStatus::Complete);
    symbols
}

#[test]
fn normal_pwct_route_publishes_native_symbols_and_exact_source() {
    let repository = TempDir::new().unwrap();
    let raw = synthetic();
    fs::write(repository.path().join("visual.PWCT"), &raw).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.indexed_files, 1);
    assert_source(&service, &receipt, "visual.PWCT", &raw, 2);
    let inventory = service.support_inventory_snapshot().unwrap();
    let capability = inventory
        .languages
        .iter()
        .find(|entry| entry.language == "pwct")
        .unwrap();
    assert_eq!(capability.suffixes, [".pwct"]);
    assert!(capability.aliases.is_empty());
    assert_eq!(capability.analyzers, ["native"]);
    assert_eq!(capability.maximum_tier, "tier_d");
    assert_eq!(capability.detectors, ["extension"]);
}

#[test]
fn normal_pwct_configuration_and_outer_suffix_respect_binary_exclusion() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let raw = synthetic();
    fs::write(repository.path().join("explicit.data"), &raw).unwrap();
    fs::write(
        repository.path().join("overridden.pwct"),
        b"{\"written\":1}\n",
    )
    .unwrap();
    fs::write(
        repository.path().join("outer.pwct.json"),
        b"{\"outer\":1}\n",
    )
    .unwrap();
    fs::write(repository.path().join("backup.pwct.bak"), &raw).unwrap();
    fs::write(repository.path().join("binary.pwct"), b"%PDF-1.7\nopaque\n").unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\ndata='pwct'\n[extensions.source-languages.data.paths]\n'overridden.pwct'='json'\n'binary.pwct'='pwct'\n"), 2).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.indexed_files, 4);
    assert_source(&service, &receipt, "explicit.data", &raw, 2);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let files: BTreeMap<_, _> = snapshot
        .document()
        .files
        .iter()
        .map(|file| (file.path.as_str(), file.language.as_str()))
        .collect();
    assert_eq!(files["overridden.pwct"], "json");
    assert_eq!(files["outer.pwct.json"], "json");
    assert_eq!(files["backup.pwct.bak"], "unknown");
    assert!(!files.contains_key("binary.pwct"));
}

#[test]
fn pwct_discovery_revision_invalidates_the_previous_unknown_generation_identity() {
    assert_eq!(DISCOVERY_MANIFEST_VERSION, "1.88");
    let parser = first_slice_parser_provider_hash().unwrap();
    let previous = hash_static_components(&[
        PROVIDER_SET_SEED,
        parser.as_bytes(),
        b"1.87",
        RESOLVER_PROVIDER_VERSION.as_bytes(),
        RESOLVER_BINARY_SEED,
        SEARCH_REVISION_SEED,
        INCREMENTAL_SCHEMA_VERSION.as_bytes(),
        DERIVED_PLAN_REVISION_SEED,
        LANGUAGE_DISPOSITION_PROVIDER_SEED,
        SOURCE_FILE_FALLBACK_PROVIDER_SEED,
        container_sources::REVISION,
    ])
    .unwrap();
    assert_ne!(first_slice_provider_set_hash().unwrap(), previous);
}

fn original_pair() -> [(String, Vec<u8>, usize); 2] {
    let root = PathBuf::from(
        std::env::var_os("ROOTLIGHT_PWCT_REPOSITORY")
            .expect("set ROOTLIGHT_PWCT_REPOSITORY to the pinned original checkout"),
    );
    let first = fs::read(root.join("applications/FromPWCTDocs/program9.pwct")).unwrap();
    let second = fs::read(root.join("applications/FromPWCTDocs/program3.pwct")).unwrap();
    assert_eq!((first.len(), second.len()), (1292, 3183));
    [
        ("program9.PWCT".to_owned(), first, 2),
        ("program3.pwct".to_owned(), second, 7),
    ]
}

fn index_with_work(service: &mut FirstSliceService, repository: &Path) -> FirstSliceIndexCommit {
    let prepared = service.prepare_repository(repository, &deadline()).unwrap();
    service
        .publish_prepared_with_metrics(prepared, &deadline())
        .unwrap()
}

fn assert_pair(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    originals: &[(String, Vec<u8>, usize); 2],
) -> BTreeMap<String, SymbolId> {
    let mut symbols = BTreeMap::new();
    for (path, raw, expected) in originals {
        symbols.extend(assert_source(service, receipt, path, raw, *expected));
    }
    assert_eq!(symbols.len(), 9);
    symbols
}

fn assert_noop(service: &mut FirstSliceService, repository: &Path, generation: GenerationId) {
    let noop = index_with_work(service, repository);
    assert_eq!(noop.receipt().generation, generation);
    assert_eq!(
        noop.evidence().strategy,
        FirstSliceIndexOperationStrategy::RetainedGeneration
    );
    assert_eq!(
        (
            noop.evidence().rebuilt_files,
            noop.evidence().rebuilt_facts,
            noop.evidence().reused_files
        ),
        (0, 0, 2)
    );
    println!("PWCT no-op: rebuilt_files=0 rebuilt_facts=0 reused_files=2");
}

#[test]
#[ignore = "requires the independently pinned original PWCT2 checkout"]
fn original_pwct_sources_survive_noop_edit_restore_and_reopen() {
    let originals = original_pair();
    let repository = durable_test_tempdir();
    for (path, raw, _) in &originals {
        fs::write(repository.path().join(path), raw).unwrap();
    }
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    let initial = index_with_work(&mut service, repository.path());
    assert_eq!(initial.receipt().indexed_files, 2);
    let symbols = assert_pair(&service, initial.receipt(), &originals);
    assert_noop(
        &mut service,
        repository.path(),
        initial.receipt().generation,
    );
    let mut changed = original_pair();
    changed[0].1 = std::str::from_utf8(&changed[0].1)
        .unwrap()
        .replace("see 3+5*4", "see 3+5*5")
        .into_bytes();
    assert_ne!(changed[0].1, originals[0].1);
    fs::write(repository.path().join(&changed[0].0), &changed[0].1).unwrap();
    let updated = index_with_work(&mut service, repository.path());
    assert_ne!(updated.receipt().generation, initial.receipt().generation);
    let work = service
        .incremental_evidence(updated.receipt().generation)
        .unwrap();
    assert_eq!(work.parsed_files(), 1);
    assert_eq!(work.reused_parser_artifacts(), 1);
    assert_eq!(assert_pair(&service, updated.receipt(), &changed), symbols);
    assert_fresh_equivalent(
        &service,
        repository.path(),
        initial.receipt().generation,
        updated.receipt(),
        &deadline(),
    );
    drop(service);
    let mut reopened = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(assert_pair(&reopened, updated.receipt(), &changed), symbols);
    assert_noop(
        &mut reopened,
        repository.path(),
        updated.receipt().generation,
    );
    fs::write(repository.path().join(&originals[0].0), &originals[0].1).unwrap();
    let restored = index_with_work(&mut reopened, repository.path());
    assert_eq!(
        restored.receipt().logical_snapshot,
        initial.receipt().logical_snapshot
    );
    assert_eq!(
        assert_pair(&reopened, restored.receipt(), &originals),
        symbols
    );
    drop(reopened);
    let mut final_open = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        assert_pair(&final_open, restored.receipt(), &originals),
        symbols
    );
    assert_noop(
        &mut final_open,
        repository.path(),
        restored.receipt().generation,
    );
    println!(
        "PWCT original pair: native Properties=9 Definitions=9 Contains=9; source-exact fresh/no-op/incremental/byte-restored/reopened; Ring semantics remain pending"
    );
}

fn payload_hashes(root: &Path) -> BTreeMap<PathBuf, ContentHash> {
    let mut pending = vec![root.to_path_buf()];
    let mut result = BTreeMap::new();
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                pending.push(entry.path());
            } else {
                assert!(kind.is_file(), "private test payloads contain no links");
                result.insert(
                    entry.path().strip_prefix(root).unwrap().to_path_buf(),
                    content_hash(&fs::read(entry.path()).unwrap()),
                );
            }
        }
    }
    result
}

#[test]
#[ignore = "requires the explicitly prepared actual retained predecessor product state"]
fn actual_predecessor_unknown_pwct_state_requalifies_before_noop_and_reopen() {
    let repository = PathBuf::from(
        std::env::var_os("ROOTLIGHT_PWCT_PREDECESSOR_REPOSITORY")
            .expect("actual predecessor repository is required"),
    );
    let outer = PathBuf::from(
        std::env::var_os("ROOTLIGHT_PWCT_PREDECESSOR_STATE")
            .expect("actual predecessor state is required"),
    );
    let report_path = PathBuf::from(
        std::env::var_os("ROOTLIGHT_PWCT_PREDECESSOR_REPORT")
            .expect("actual predecessor report is required"),
    );
    let originals = original_pair();
    let report_bytes = fs::read(&report_path).unwrap();
    let report: serde_json::Value = serde_json::from_slice(&report_bytes).unwrap();
    assert_eq!(
        report["schema"],
        "rootlight-index-source-accounting-diagnostic/1"
    );
    assert_eq!(report["no_op_same_generation"], true);
    assert_eq!(
        report["source_accounting"]["source_read_verified_file_records"],
        2
    );
    assert_eq!(report["source_accounting"]["retained_entity_records"], 0);
    assert_eq!(report["source_accounting"]["retained_relation_records"], 0);
    let prior: FirstSliceIndexReceipt = serde_json::from_value(report["cold"].clone()).unwrap();
    assert_eq!(prior.indexed_files, 2);
    assert_eq!(prior.entities, 0);
    for (path, raw, _) in &originals {
        assert_eq!(fs::read(repository.join(path)).unwrap(), *raw);
        let file = report["files"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["file"]["path"] == *path)
            .unwrap();
        assert_eq!(file["file"]["language"], "unknown");
        assert_eq!(file["file"]["content_hash"], content_hash(raw).to_string());
        assert_eq!(file["source_exact"], true);
        assert_eq!(file["source_read_exact"], true);
        assert_eq!(file["occurrence_record_count"], 0);
    }
    let state = outer.join("state");
    let repository_state = state
        .join("first-slice/repositories")
        .join(prior.repository.to_string());
    let old_generation = repository_state.join(prior.generation.to_string());
    let old_payloads = payload_hashes(&old_generation);
    let blobs = repository_state.join("source-blobs");
    let old_blobs = payload_hashes(&blobs);
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 2).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, &state, &deadline()).unwrap();
    assert_eq!(
        service
            .registered_repository_for_root(&repository, &deadline())
            .unwrap(),
        Some(prior.repository)
    );
    let old = service
        .loaded_generation_snapshot(prior.generation)
        .unwrap();
    assert!(old.document().entities.is_empty());
    assert!(old.document().occurrences.is_empty());
    assert!(old.document().relations.is_empty());
    let old_provider = old.metadata().provider_set_hash();
    let old_configuration = old.metadata().configuration_hash();
    for (path, raw, _) in &originals {
        let file = old
            .document()
            .files
            .iter()
            .find(|file| file.path == *path)
            .unwrap();
        assert_eq!(file.language, "unknown");
        let read = service
            .source_read(
                prior.generation,
                vec![file.evidence.source.clone().unwrap()],
                &deadline(),
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, *raw);
        assert_eq!(read.data.chunks[0].language, "unknown");
    }
    drop(old);
    let upgraded = index_with_work(&mut service, &repository);
    assert_eq!(upgraded.receipt().repository, prior.repository);
    assert_eq!(upgraded.receipt().parent, Some(prior.generation));
    assert_ne!(upgraded.receipt().generation, prior.generation);
    assert_eq!(
        (
            upgraded.evidence().rebuilt_files,
            upgraded.evidence().reused_files
        ),
        (2, 0)
    );
    let current = service
        .loaded_generation_snapshot(upgraded.receipt().generation)
        .unwrap();
    assert_eq!(current.metadata().configuration_hash(), old_configuration);
    assert_ne!(current.metadata().provider_set_hash(), old_provider);
    drop(current);
    let symbols = assert_pair(&service, upgraded.receipt(), &originals);
    assert_noop(&mut service, &repository, upgraded.receipt().generation);
    drop(service);
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 2).unwrap();
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, &state, &deadline()).unwrap();
    assert_eq!(
        assert_pair(&reopened, upgraded.receipt(), &originals),
        symbols
    );
    assert_noop(&mut reopened, &repository, upgraded.receipt().generation);
    assert_eq!(payload_hashes(&old_generation), old_payloads);
    let retained_blobs = payload_hashes(&blobs);
    for (path, hash) in old_blobs {
        assert_eq!(retained_blobs.get(&path), Some(&hash));
    }
    assert_eq!(fs::read(report_path).unwrap(), report_bytes);
    for (path, raw, _) in &originals {
        assert_eq!(fs::read(repository.join(path)).unwrap(), *raw);
    }
    println!(
        "actual predecessor PWCT upgrade: unknown_files=2 graphfacts=0 -> native_files=2 Properties=9 Definitions=9 Contains=9; same repository, real parent generation, no-op/reopen stable, prior payloads/report/source blobs unchanged"
    );
}
