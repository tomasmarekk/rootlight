//! Shared WESL admission preserves native owners and original shader bytes.
//! Unproved shader and import semantics retain the adapter's explicit gaps.

use super::*;
use rootlight_ir::{
    ContainerRef, OccurrenceRole, OccurrenceTarget, RelationEndpoint, RelationPredicate,
};

const MANUAL: &[u8] = include_bytes!(
    "../../../rootlight-adapter-treesitter/tests/fixtures/wesl/manual_material.wesl"
);
const WIREFRAME: &[u8] =
    include_bytes!("../../../rootlight-adapter-treesitter/tests/fixtures/wesl/wireframe2d.wesl");

fn assert_original(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    raw: &[u8],
    parameter: &str,
    expected_reads: usize,
) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let doc = snapshot.document();
    let file = doc.files.iter().find(|file| file.path == path).unwrap();
    assert_eq!(file.language, "wesl");
    assert_eq!(file.content_hash, content_hash(raw));
    let source = file.evidence.source.as_ref().unwrap();
    assert_eq!(source.generation(), receipt.generation);
    assert_eq!(source.span().start_byte(), 0);
    assert_eq!(source.span().end_byte(), u64::try_from(raw.len()).unwrap());
    let read = service
        .source_read(receipt.generation, vec![source.clone()], &deadline())
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, raw);
    assert_eq!(read.data.chunks[0].language, "wesl");
    let provenance = doc
        .provenance
        .iter()
        .find(|entry| entry.id == file.provenance)
        .unwrap();
    assert_eq!(provenance.language, "wesl");
    assert_eq!(
        provenance.frontend_version.as_deref(),
        Some("tree-sitter-wesl-0.1.0")
    );
    assert!(
        doc.diagnostics
            .iter()
            .all(|diagnostic| diagnostic.code != "unsupported-language")
    );
    let module = doc
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Module
                && entity.container == Some(ContainerRef::File(file.id))
        })
        .unwrap();
    let function = doc
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Function
                && entity.container == Some(ContainerRef::Entity(module.id))
        })
        .unwrap();
    let formal = doc
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Parameter
                && entity.canonical_name == parameter
                && entity.container == Some(ContainerRef::Entity(function.id))
        })
        .unwrap();
    for entity in [module, function, formal] {
        assert_eq!(entity.language, "wesl");
    }
    let definitions: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span().file() == file.id && site.role == OccurrenceRole::Definition
        })
        .collect();
    assert_eq!(definitions.len(), 2);
    assert!(definitions.iter().any(|site| site.target
        == OccurrenceTarget::Resolved { symbol: formal.id }
        && site.enclosing == Some(function.id)));
    let reads: Vec<_> = doc
        .occurrences
        .iter()
        .filter(|site| site.source.span().file() == file.id && site.role == OccurrenceRole::Read)
        .collect();
    assert_eq!(reads.len(), expected_reads);
    for site in reads {
        assert_eq!(
            site.target,
            OccurrenceTarget::Resolved { symbol: formal.id }
        );
        assert_eq!(site.enclosing, Some(function.id));
        assert_eq!(
            &raw[usize::try_from(site.source.span().start_byte()).unwrap()
                ..usize::try_from(site.source.span().end_byte()).unwrap()],
            parameter.as_bytes()
        );
        assert!(
            doc.relations
                .iter()
                .any(|edge| edge.predicate == RelationPredicate::RefersTo
                    && edge.subject == RelationEndpoint::Occurrence(site.id)
                    && edge.object == RelationEndpoint::Entity(formal.id))
        );
    }
    assert!(
        doc.occurrences
            .iter()
            .any(|site| site.source.span().file() == file.id
                && site.role == OccurrenceRole::ImportUse
                && matches!(site.target, OccurrenceTarget::Unresolved { .. }))
    );
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, "wesl");
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    assert_ne!(coverage.status, CoverageStatus::Complete);
    let explained = service
        .symbol_explain(receipt.generation, formal.id, &deadline())
        .unwrap();
    assert_eq!(explained.data.entity.language, "wesl");
    assert_eq!(explained.data.entity.canonical_name, parameter);
}

#[test]
fn published_original_uses_distinct_wesl_owners_reads_and_exact_bytes() {
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("manual.WESL"), MANUAL).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.indexed_files, 1);
    assert_original(&service, &receipt, "manual.WESL", MANUAL, "mesh", 1);
    let inventory = service.support_inventory_snapshot().unwrap();
    let capability = inventory
        .languages
        .iter()
        .find(|entry| entry.language == "wesl")
        .unwrap();
    assert_eq!(capability.suffixes, [".wesl"]);
    assert!(capability.aliases.is_empty());
    assert_eq!(capability.analyzers, ["treesitter"]);
    assert_eq!(capability.maximum_tier, "tier_d");
    assert_eq!(capability.detectors, ["extension"]);
}

#[test]
fn shared_routes_preserve_configuration_and_refuse_inner_suffixes() {
    let service = FirstSliceService::new(2).unwrap();
    let mut absent = service.analyzers.clone();
    assert!(absent.remove("wesl").is_some());
    assert!(absent.contains_key("wgsl"));
    for path in ["shader.wesl", "SHADER.WESL", "shader.wgsl.wesl"] {
        let mut input = ManifestInput {
            file: FileId::from_bytes([3; 20]),
            path: path.to_owned(),
            content_hash: content_hash(MANUAL),
            bytes: u64::try_from(MANUAL.len()).unwrap(),
            encoding: "utf-8".to_owned(),
            class: InputClass::Source,
            language_signals: vec![rootlight_discovery::LanguageSignal {
                language: "hlsl".to_owned(),
                evidence: LanguageEvidence::Content,
            }],
            project_language_witnesses: Vec::new(),
            decisive_rule: None,
        };
        assert_eq!(source_language_from_path(path), Some("wesl"));
        assert_eq!(detected_source_language(&input), Some("wesl"));
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("wesl")
        );
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("wesl")
        );
        assert_eq!(source_language_route(&input, &absent), None);
        input
            .language_signals
            .push(rootlight_discovery::LanguageSignal {
                language: "wgsl".to_owned(),
                evidence: LanguageEvidence::Configuration,
            });
        assert_eq!(detected_source_language(&input), Some("wgsl"));
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("wgsl")
        );
        input.language_signals.last_mut().unwrap().language = "unsupported-language".to_owned();
        assert_eq!(source_language_route(&input, &service.analyzers), None);
    }
    for path in [
        "shader.wesl.bak",
        "shader.weslx",
        "shader.wesl/source",
        "shader.wesl.tt",
    ] {
        assert_eq!(source_language_from_path(path), None);
    }
    assert_eq!(source_language_from_path("shader.wesl.wgsl"), Some("wgsl"));
    assert_eq!(source_language_from_path("shader.wesl.json"), Some("json"));
}

#[test]
fn original_wesl_sources_survive_noop_edit_byte_restore_and_reopen() {
    let fixture = durable_test_tempdir();
    fs::write(fixture.path().join("manual.WESL"), MANUAL).unwrap();
    fs::write(fixture.path().join("wireframe.wesl"), WIREFRAME).unwrap();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let mut service = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    let initial = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_eq!(initial.indexed_files, 2);
    assert_original(&service, &initial, "manual.WESL", MANUAL, "mesh", 1);
    assert_original(&service, &initial, "wireframe.wesl", WIREFRAME, "in", 0);
    assert_eq!(
        service
            .index_repository(fixture.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let mut changed = MANUAL.to_vec();
    changed.extend_from_slice(b"\n// retained source edit\n");
    fs::write(fixture.path().join("manual.WESL"), &changed).unwrap();
    let updated = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_ne!(updated.generation, initial.generation);
    let evidence = service.incremental_evidence(updated.generation).unwrap();
    assert_eq!(evidence.parsed_files(), 1);
    assert_eq!(evidence.reused_parser_artifacts(), 1);
    assert_fresh_equivalent(
        &service,
        fixture.path(),
        initial.generation,
        &updated,
        &deadline(),
    );
    drop(service);
    let mut restored = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    let mut previous = None;
    for (receipt, raw) in [(&initial, MANUAL), (&updated, changed.as_slice())] {
        assert_original(&restored, receipt, "manual.WESL", raw, "mesh", 1);
        assert_original(&restored, receipt, "wireframe.wesl", WIREFRAME, "in", 0);
        let snapshot = restored
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let symbols: BTreeSet<_> = snapshot
            .document()
            .entities
            .iter()
            .map(|entity| entity.id)
            .collect();
        if let Some(prior) = &previous {
            assert_eq!(&symbols, prior);
        }
        previous = Some(symbols);
        assert_eq!(
            snapshot.metadata().provider_set_hash(),
            restored
                .provider_set_hash(FirstSliceIndexMode::Structural)
                .unwrap()
        );
    }
    assert_eq!(
        restored
            .index_repository(fixture.path(), &deadline())
            .unwrap()
            .generation,
        updated.generation
    );
    fs::write(fixture.path().join("manual.WESL"), MANUAL).unwrap();
    let reverted = restored
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    assert_ne!(reverted.generation, updated.generation);
    let evidence = restored.incremental_evidence(reverted.generation).unwrap();
    // Durable generations preserve facts and bytes; native trees remain process-local.
    assert_eq!(evidence.parsed_files(), 2);
    assert_eq!(evidence.reused_parser_artifacts(), 0);
    assert!(initial.logical_snapshot.is_some());
    assert_eq!(reverted.logical_snapshot, initial.logical_snapshot);
    assert_original(&restored, &reverted, "manual.WESL", MANUAL, "mesh", 1);
    assert_original(&restored, &reverted, "wireframe.wesl", WIREFRAME, "in", 0);
    drop(restored);
    let mut reopened = FirstSliceService::new_durable(3, paths.state_dir(), &deadline()).unwrap();
    assert_original(&reopened, &reverted, "manual.WESL", MANUAL, "mesh", 1);
    assert_eq!(
        reopened
            .index_repository(fixture.path(), &deadline())
            .unwrap()
            .generation,
        reverted.generation
    );
}
