//! Default discovery and Structural indexing retain written ThinkScript ownership.
//! These controls require exact source declarations and value reads, not study evaluation.

use super::*;
use rootlight_ir::{RelationEndpoint, RelationPredicate};
use rootlight_source::{SourceEncoding, SourceFindBudget, SourceFindStop};

const ORIGINAL: &[u8] =
    include_bytes!("../../../rootlight-adapters/tests/fixtures/thinkscript/VIX_FGMR.txt");

fn definition(
    document: &rootlight_ir::NormalizedIrDocument,
    file: FileId,
    start: u64,
    end: u64,
    name: &str,
    kind: EntityKind,
) -> SymbolId {
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span() == SourceSpan::new(file, start, end).unwrap()
                && site.role == OccurrenceRole::Definition
        })
        .collect();
    assert_eq!(sites.len(), 1, "one exact authored definition {name}");
    let OccurrenceTarget::Resolved { symbol } = sites[0].target else {
        panic!("written definition resolves")
    };
    let entity = document
        .entities
        .iter()
        .find(|entity| entity.id == symbol)
        .unwrap();
    assert_eq!(entity.kind, kind);
    assert_eq!(entity.canonical_name, name);
    assert_eq!(entity.language, "thinkscript");
    assert_eq!(entity.provenance, sites[0].provenance);
    symbol
}

#[test]
fn complete_original_default_txt_route_preserves_script_inputs_read_and_exact_source() {
    assert_eq!(ORIGINAL.len(), 2565);
    assert_eq!(
        ORIGINAL
            .windows(2)
            .filter(|bytes| *bytes == b"\r\n")
            .count(),
        91
    );
    assert!(!ORIGINAL.windows(3).any(|bytes| bytes == b"\r\r\n"));
    let directory = TempDir::new().unwrap();
    fs::write(directory.path().join("written.txt"), ORIGINAL).unwrap();
    fs::write(
        directory.path().join("lib.rs"),
        b"pub fn retained_sibling() {}\n",
    )
    .unwrap();
    let mut service = FirstSliceService::new(3).unwrap();
    let receipt = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "written.txt")
        .unwrap();
    assert_eq!(file.language, "thinkscript");
    let source = file.evidence.source.as_ref().unwrap();
    assert_eq!(
        source.span(),
        SourceSpan::new(file.id, 0, ORIGINAL.len() as u64).unwrap()
    );
    assert_eq!(source.content_hash(), content_hash(ORIGINAL));
    assert_eq!(source.repository(), receipt.repository);
    assert_eq!(source.generation(), receipt.generation);
    let owner = definition(
        document,
        file.id,
        316,
        339,
        "GetBollingerBandPercent",
        EntityKind::Function,
    );
    let upper = definition(document, file.id, 380, 385, "upper", EntityKind::Parameter);
    let lower = definition(document, file.id, 402, 407, "lower", EntityKind::Parameter);
    for (child, start, end) in [(upper, 380, 385), (lower, 402, 407)] {
        let edges: Vec<_> = document
            .relations
            .iter()
            .filter(|edge| {
                edge.predicate == RelationPredicate::Contains
                    && edge.subject == RelationEndpoint::Entity(owner)
                    && edge.object == RelationEndpoint::Entity(child)
                    && edge.evidence.source.as_ref().is_some_and(|source| {
                        source.span().file() == file.id
                            && source.span().start_byte() <= start
                            && source.span().end_byte() >= end
                    })
            })
            .collect();
        assert_eq!(edges.len(), 1);
    }
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Contains
            && edge.subject == RelationEndpoint::Entity(upper)
            && edge.object == RelationEndpoint::Entity(owner)
    }));
    let sites: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.source.span() == SourceSpan::new(file.id, 578, 583).unwrap()
                && site.role == OccurrenceRole::Read
        })
        .collect();
    assert_eq!(sites.len(), 1);
    assert_eq!(
        sites[0].target,
        OccurrenceTarget::Resolved { symbol: upper }
    );
    assert_eq!(sites[0].enclosing, Some(owner));
    assert_eq!(sites[0].source.content_hash(), content_hash(ORIGINAL));
    assert!(!document.occurrences.iter().any(|site| {
        site.source.span() == sites[0].source.span() && site.role == OccurrenceRole::Definition
    }));
    assert!(
        document
            .entities
            .iter()
            .any(|entity| entity.language == "rust" && entity.canonical_name == "retained_sibling")
    );
    assert!(document.skipped_regions.iter().any(|gap| {
        gap.source.span().file() == file.id
            && gap.detail == "thinkscript-evaluation-builtins-and-omitted-syntax-unqualified"
    }));
    assert!(!document.relations.iter().any(|edge| {
        edge.predicate == RelationPredicate::Calls
            && edge
                .evidence
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == file.id)
    }));
    let exact = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![source.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(exact.data.chunks.len(), 1);
    assert_eq!(exact.data.chunks[0].reference, *source);
    assert_eq!(exact.data.chunks[0].bytes, ORIGINAL);
    for (query, count) in [
        ("GetBollingerBandPercent", 5),
        ("__absent_written_input__", 0),
    ] {
        let page = service
            .find_literal(
                receipt.generation,
                file.id,
                query,
                None,
                SourceFindBudget::new(),
                &deadline(),
            )
            .unwrap();
        assert_eq!(page.stop, SourceFindStop::EndOfSource);
        assert!(page.continuation.is_none());
        assert_eq!(page.matches.len(), count);
    }
    drop(snapshot);
    let noop = service
        .index_repository(directory.path(), &deadline())
        .unwrap();
    assert_eq!(noop.generation, receipt.generation);
}

fn discovered_thinkscript_route_input(service: &FirstSliceService, path: &str) -> ManifestInput {
    let directory = TempDir::new().unwrap();
    let raw = b"script Renamed { input width=2; plot result=width; }\r\n";
    fs::write(directory.path().join(path), raw).unwrap();
    let root = RepositoryRoot::open(
        derive_repository(b"thinkscript-content-route").id(),
        directory.path(),
    )
    .unwrap();
    let manifest = rootlight_discovery::discover(
        &root,
        &service.config,
        &DiscoveryPolicy::build(Vec::new(), false).unwrap(),
        rootlight_discovery::DiscoveryLimits::from_config(&service.config),
        &deadline(),
    )
    .unwrap();
    assert_eq!(manifest.inputs.len(), 1);
    let input = manifest.inputs.into_iter().next().unwrap();
    assert_eq!(input.path, path);
    assert_eq!(input.content_hash, content_hash(raw));
    assert_eq!(input.bytes, raw.len() as u64);
    assert_eq!(input.encoding, "utf-8");
    assert_eq!(input.class, InputClass::Source);
    assert_eq!(input.language_signals.len(), 1);
    assert_eq!(input.language_signals[0].language, "thinkscript");
    assert_eq!(
        input.language_signals[0].evidence,
        LanguageEvidence::Content
    );
    input
}

#[test]
fn authentic_shared_text_content_routes_require_the_registered_provider() {
    let service = FirstSliceService::new(2).unwrap();
    for path in ["ordinary.txt", "RENAMED.TXT"] {
        let input = discovered_thinkscript_route_input(&service, path);
        assert_eq!(source_language_from_path(path), None);
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("thinkscript")
        );
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("thinkscript")
        );
        let mut absent = service.analyzers.clone();
        assert!(absent.remove("thinkscript").is_some());
        assert_eq!(source_language_route(&input, &absent), None);
        assert_eq!(supported_source_language(&input, &absent), None);
    }
}

#[test]
fn configured_and_known_source_routes_keep_precedence_over_shared_text_content() {
    let service = FirstSliceService::new(2).unwrap();
    let mut input = discovered_thinkscript_route_input(&service, "ordinary.txt");
    input
        .language_signals
        .push(rootlight_discovery::LanguageSignal {
            language: "python".to_owned(),
            evidence: LanguageEvidence::Configuration,
        });
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("python")
    );
    input
        .language_signals
        .push(rootlight_discovery::LanguageSignal {
            language: "thinkscript".to_owned(),
            evidence: LanguageEvidence::Configuration,
        });
    assert_eq!(source_language_route(&input, &service.analyzers), None);
    input.language_signals = vec![rootlight_discovery::LanguageSignal {
        language: "thinkscript".to_owned(),
        evidence: LanguageEvidence::Configuration,
    }];
    input.path = "configured.data".to_owned();
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("thinkscript")
    );
    let mut input = discovered_thinkscript_route_input(&service, "ordinary.txt");
    input.path = "written.rs".to_owned();
    assert_eq!(source_language_from_path(&input.path), Some("rust"));
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("rust")
    );
}

#[test]
fn conflicting_or_unavailable_shared_text_evidence_never_invents_a_route() {
    let service = FirstSliceService::new(2).unwrap();
    let original = discovered_thinkscript_route_input(&service, "ordinary.txt");
    for evidence in [
        LanguageEvidence::Filename,
        LanguageEvidence::Extension,
        LanguageEvidence::Content,
        LanguageEvidence::Shebang,
        LanguageEvidence::Context,
        LanguageEvidence::Manifest,
        LanguageEvidence::Project,
    ] {
        let mut input = original.clone();
        input
            .language_signals
            .push(rootlight_discovery::LanguageSignal {
                language: "python".to_owned(),
                evidence,
            });
        assert_eq!(source_language_route(&input, &service.analyzers), None);
    }
    for evidence in [
        LanguageEvidence::Filename,
        LanguageEvidence::Extension,
        LanguageEvidence::Shebang,
    ] {
        let mut input = original.clone();
        input.language_signals[0].evidence = evidence;
        assert_eq!(source_language_route(&input, &service.analyzers), None);
    }
    for path in ["ordinary.txt.bak", "ordinary.unmapped"] {
        let mut input = original.clone();
        input.path = path.to_owned();
        assert_eq!(source_language_route(&input, &service.analyzers), None);
    }
    let mut input = original.clone();
    input.language_signals[0].language = "ruby".to_owned();
    assert_eq!(source_language_route(&input, &service.analyzers), None);
    let mut input = original;
    input.encoding = "bytes".to_owned();
    assert_eq!(
        source_language_route(&input, &service.analyzers),
        Some("thinkscript")
    );
    assert_eq!(supported_source_language(&input, &service.analyzers), None);
}
