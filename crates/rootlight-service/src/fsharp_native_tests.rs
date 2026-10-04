//! Durable F# profile selection, authored declarations and exact source identity.
//! Structural declarations do not establish cross-file binding or execution.

use super::*;
use rootlight_ir::{ContainerRef, EntityKind, OccurrenceRole, OccurrenceTarget, RelationEndpoint};
use std::fs;

const IMPLEMENTATION: &str = "module Sample.Root\nopen System\nmodule Nested =\n    let value = 3\n    let ``value with spaces`` = value\n    let add x y = x + y\n    let rec first n = n + 1\n    and second n = n + 2\n    let repeat limit =\n        for _ = 1 to limit do\n            ()\ntype Record = { Name: string; mutable Count: int }\ntype Choice = | Empty | Named of name: string\ntype Sample =\n      First = 0\n    | Next = 1\ntype Alias = System.String\ntype Constructed<'T> = Container<'T>.Builder\ntype Container(seed: int) =\n    member this.Read amount = seed + amount\n    member this.Value = seed\n    new () = Container(0)\n";
const SIGNATURE: &str = "module Sample.Root\nopen System\nmodule Nested =\n    val value: int\n    val ``value with spaces``: int\n    val add: int -> int -> int\n    val repeat: int -> unit\ntype Record = { Name: string; mutable Count: int }\ntype Choice = | Empty | Named of name: string\ntype Sample =\n      First = 0\n    | Next = 1\ntype Alias = System.String\ntype Constructed<'T> = Container<'T>.Builder\ntype Container =\n    new: seed:int -> Container\n    member Read: amount:int -> int\n    member Value: int with get\n";

fn deadline() -> Cancellation {
    Cancellation::with_deadline(Instant::now().checked_add(Duration::from_secs(30)).unwrap())
}

fn policy() -> FirstSliceStoragePolicy {
    FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nfs='fsharp'\n"),
        4,
    )
    .unwrap()
}

fn index(
    service: &mut FirstSliceService,
    repository: &Path,
    options: FirstSliceIndexOptions,
) -> FirstSliceIndexCommit {
    let cancellation = deadline();
    let prepared = service
        .prepare_repository_with_options(repository, options, &cancellation)
        .unwrap();
    service
        .publish_prepared_with_metrics(prepared, &cancellation)
        .unwrap()
}

fn exact_source(
    service: &FirstSliceService,
    reference: &SourceRef,
    language: &str,
    expected: &[u8],
) {
    let read = service
        .source_read_with_options_and_budget(
            reference.generation(),
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.generation, reference.generation());
    assert_eq!(read.data.chunks.len(), 1);
    let chunk = &read.data.chunks[0];
    assert_eq!(&chunk.reference, reference);
    assert_eq!(chunk.content_hash, reference.content_hash());
    assert_eq!(chunk.start_byte, reference.span().start_byte());
    assert_eq!(chunk.end_byte, reference.span().end_byte());
    assert_eq!(chunk.language, language);
    assert_eq!(chunk.bytes, expected);
}

fn assert_generation(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    implementation: &str,
    signature: &str,
) -> (BTreeSet<SymbolId>, SourceRef) {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 2);
    assert!(!document.diagnostics.iter().any(|diagnostic| {
        matches!(
            diagnostic.code.as_str(),
            "unsupported-language" | "unsupported-encoding"
        )
    }));
    assert!(!document.skipped_regions.iter().any(|gap| {
        matches!(
            gap.reason,
            SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
        )
    }));
    let mut symbols = BTreeSet::new();
    let mut implementation_reference = None;
    for (path, language, source) in [
        ("Library.fs", "fsharp", implementation),
        ("Library.fsi", "fsharp-signature", signature),
    ] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, language);
        assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
        assert_eq!(file.content_hash, content_hash(source.as_bytes()));
        let full = file.evidence.source.as_ref().unwrap();
        assert_eq!(full.repository(), receipt.repository);
        assert_eq!(full.generation(), receipt.generation);
        assert_eq!(full.span().file(), file.id);
        assert_eq!(full.span().start_byte(), 0);
        assert_eq!(full.span().end_byte(), file.byte_length);
        exact_source(service, full, language, source.as_bytes());
        if language == "fsharp" {
            implementation_reference = Some(full.clone());
        }
        let provenance = document
            .provenance
            .iter()
            .find(|record| record.id == file.provenance)
            .unwrap();
        assert_eq!(provenance.producer_kind, ProducerKind::Parser);
        assert_eq!(provenance.language, language);
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.language, language);
        assert_eq!(coverage.tier, AnalysisTier::TierD);
        assert_ne!(coverage.status, CoverageStatus::Complete);

        let expected = [
            ("value", EntityKind::Variable),
            ("``value with spaces``", EntityKind::Variable),
            ("Record", EntityKind::Struct),
            ("Choice", EntityKind::Union),
            ("Sample", EntityKind::Enum),
            ("First", EntityKind::Constant),
            ("Next", EntityKind::Constant),
            ("Alias", EntityKind::TypeAlias),
            ("Constructed", EntityKind::TypeAlias),
            ("Name", EntityKind::Field),
            ("Count", EntityKind::Field),
            ("Empty", EntityKind::Constructor),
            ("Named", EntityKind::Constructor),
            ("Container", EntityKind::Class),
            ("Read", EntityKind::Method),
            ("Value", EntityKind::Property),
            ("new", EntityKind::Constructor),
        ];
        for (name, kind) in expected.into_iter().chain(["add", "repeat"].map(|name| {
            (
                name,
                if language == "fsharp" {
                    EntityKind::Function
                } else {
                    EntityKind::Variable
                },
            )
        })) {
            let matches: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.language == language
                        && entity.canonical_name == name
                        && entity.kind == kind
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == file.id)
                })
                .collect();
            assert_eq!(
                matches.len(),
                1,
                "{language}: exact authored declaration {name}: {:?}",
                document
                    .entities
                    .iter()
                    .map(|entity| (&entity.language, &entity.canonical_name, entity.kind))
                    .collect::<Vec<_>>()
            );
            let entity = matches[0];
            assert!(symbols.insert(entity.id));
            let definitions: Vec<_> = document
                .occurrences
                .iter()
                .filter(|site| {
                    site.file == file.id
                        && site.role == OccurrenceRole::Definition
                        && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                })
                .collect();
            assert_eq!(definitions.len(), 1, "{language}: definition {name}");
            exact_source(service, &definitions[0].source, language, name.as_bytes());
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
            assert!(
                located
                    .data
                    .hits
                    .iter()
                    .any(|hit| hit.symbol == Some(entity.id) && hit.file == file.id)
            );
        }
        let enumeration = document
            .entities
            .iter()
            .find(|entity| entity.language == language && entity.canonical_name == "Sample")
            .unwrap();
        for name in ["First", "Next"] {
            let case = document
                .entities
                .iter()
                .find(|entity| entity.language == language && entity.canonical_name == name)
                .unwrap();
            assert_eq!(case.container, Some(ContainerRef::Entity(enumeration.id)));
            assert!(document.relations.iter().any(|relation| {
                relation.predicate == RelationPredicate::Contains
                    && relation.subject == RelationEndpoint::Entity(enumeration.id)
                    && relation.object == RelationEndpoint::Entity(case.id)
            }));
        }
        for site in document
            .occurrences
            .iter()
            .filter(|site| site.file == file.id)
        {
            assert_eq!(site.source.generation(), receipt.generation);
            assert_eq!(site.source.content_hash(), full.content_hash());
            let span = site.source.span();
            let written = source
                .as_bytes()
                .get(
                    usize::try_from(span.start_byte()).unwrap()
                        ..usize::try_from(span.end_byte()).unwrap(),
                )
                .unwrap();
            exact_source(service, &site.source, language, written);
            if !matches!(
                site.role,
                OccurrenceRole::Definition | OccurrenceRole::Declaration
            ) {
                assert!(matches!(site.target, OccurrenceTarget::Unresolved { .. }));
                assert!(document.relations.iter().all(|relation| relation.subject
                    != rootlight_ir::RelationEndpoint::Occurrence(site.id)));
            }
        }
    }
    assert!(document.relations.iter().all(|relation| matches!(
        relation.predicate,
        RelationPredicate::Contains | RelationPredicate::Declares | RelationPredicate::DefinesAt
    )));
    (symbols, implementation_reference.unwrap())
}

#[test]
fn fsharp_profiles_preserve_source_and_identity_across_recovery() {
    for newline in ["\n", "\r\n"] {
        let current = std::env::current_dir().unwrap();
        let repository = tempfile::tempdir_in(&current).unwrap();
        let storage = tempfile::tempdir_in(&current).unwrap();
        let paths = rootlight_runtime::RuntimePaths::new(
            storage.path().join("state"),
            storage.path().join("runtime"),
        )
        .unwrap();
        paths.prepare_owner().unwrap();
        let implementation = IMPLEMENTATION.replace('\n', newline);
        let signature = SIGNATURE.replace('\n', newline);
        fs::write(repository.path().join("Library.fs"), &implementation).unwrap();
        fs::write(repository.path().join("Library.fsi"), &signature).unwrap();
        let mut service =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        let first = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let initial = first.receipt().clone();
        assert_eq!(initial.discovered_inputs, 2);
        assert_eq!(initial.indexed_files, 2);
        assert!(initial.discovery_complete);
        assert_eq!(first.evidence().rebuilt_files, 2);
        let (symbols, original_source) =
            assert_generation(&service, &initial, &implementation, &signature);
        let noop = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_eq!(noop.receipt(), &initial);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);

        assert_eq!(implementation.matches("n + 1").count(), 1);
        let changed = implementation.replace("n + 1", "n + 4");
        fs::write(repository.path().join("Library.fs"), &changed).unwrap();
        let mutation = index(
            &mut service,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        let updated = mutation.receipt().clone();
        assert_ne!(updated.generation, initial.generation);
        assert_eq!(updated.parent, Some(initial.generation));
        assert_eq!(mutation.evidence().changed_files, 1);
        assert_eq!(mutation.evidence().rebuilt_files, 1);
        let (updated_symbols, updated_source) =
            assert_generation(&service, &updated, &changed, &signature);
        assert_eq!(updated_symbols, symbols);
        assert_ne!(
            updated_source.content_hash(),
            original_source.content_hash()
        );
        exact_source(
            &service,
            &original_source,
            "fsharp",
            implementation.as_bytes(),
        );
        assert!(
            service
                .source_read_with_options_and_budget(
                    updated.generation,
                    vec![original_source.clone()],
                    SourceReadOptions::new(),
                    FirstSliceBudget::default(),
                    &deadline()
                )
                .is_err()
        );
        drop(service);

        let mut reopened =
            FirstSliceService::new_durable_with_policy(policy(), paths.state_dir(), &deadline())
                .unwrap();
        assert_eq!(
            assert_generation(&reopened, &initial, &implementation, &signature).0,
            symbols
        );
        assert_eq!(
            assert_generation(&reopened, &updated, &changed, &signature).0,
            symbols
        );
        let noop = index(
            &mut reopened,
            repository.path(),
            FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
        );
        assert_eq!(noop.receipt(), &updated);
        assert_eq!(noop.evidence().rebuilt_files, 0);
        assert_eq!(noop.evidence().rebuilt_facts, 0);
        let clean = index(
            &mut reopened,
            repository.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
        );
        assert!(updated.logical_snapshot.is_some());
        assert_eq!(clean.receipt().logical_snapshot, updated.logical_snapshot);
        assert_eq!(
            assert_generation(&reopened, clean.receipt(), &changed, &signature).0,
            symbols
        );
    }
}

#[test]
fn fsharp_shared_suffix_keeps_unproven_source_and_rejects_binary_input() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(current).unwrap();
    let source = ": square dup * ;\n// module Sample.Root\n// let value = 1\n";
    fs::write(repository.path().join("Library.fs"), source).unwrap();
    fs::write(repository.path().join("binary.fsx"), b"\0binary").unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.indexed_files, 1);
    assert_eq!(receipt.binary_inputs, 1);
    let status = service.repository_status(receipt.repository, None).unwrap();
    assert!(status.coverage.iter().all(|entry| entry.indexed_files == 0));
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert_eq!(document.files.len(), 1);
    assert!(document.entities.is_empty());
    assert!(document.files.iter().all(|file| file.path != "binary.fsx"));
    let file = &document.files[0];
    assert_eq!(file.path, "Library.fs");
    assert!(!matches!(
        file.language.as_str(),
        "fsharp" | "fsharp-signature"
    ));
    assert_eq!(file.encoding, "utf-8");
    let provenance = document
        .provenance
        .iter()
        .find(|record| record.id == file.provenance)
        .unwrap();
    assert_eq!(provenance.producer_kind, ProducerKind::Rule);
    assert_eq!(provenance.language, file.language);
    assert_eq!(provenance.tier, AnalysisTier::TierD);
    assert_eq!(provenance.rule.as_deref(), Some("unsupported-language"));
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, file.language);
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
    );
    assert!(document.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "unsupported-language"
            && diagnostic.provenance == file.provenance
            && diagnostic.source.as_ref() == file.evidence.source.as_ref()
            && diagnostic.coverage_effect == CoverageStatus::Unknown
    }));
    exact_source(
        &service,
        file.evidence.source.as_ref().unwrap(),
        &file.language,
        source.as_bytes(),
    );
}

#[test]
fn fsharp_content_routes_default_sources_without_promoting_mixed_suffix_peers() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let sources = [
        ("Full.fs", IMPLEMENTATION, "fsharp"),
        (
            "Library.fs",
            "module Example.Core\nlet value = 1\n",
            "fsharp",
        ),
        (
            "Upper.FS",
            "namespace Example.Core\ntype Entry = { Count: int }\n",
            "fsharp",
        ),
        (
            "Bom.fs",
            "\u{feff}module Example.Core\r\nlet bomValue = 1\r\n",
            "fsharp",
        ),
        ("Forth.fs", ": square dup * ;\n", "unknown"),
        (
            "Mixed.fs",
            "module Example.Core\nlet value = 1\n: square dup * ;\n",
            "unknown",
        ),
        (
            "InlineMixed.fs",
            "module Example.Core\nlet value = 1 : square dup * ;\n",
            "unknown",
        ),
        (
            "ConditionalMixed.fs",
            "module Example.Core\nlet value = 1\n0 [if] : square dup * ; [then]\n",
            "unknown",
        ),
        (
            "Annotated.fs",
            "module Example.Core\nlet value : int = 1 ;\n",
            "fsharp",
        ),
        (
            "Record.fs",
            "module Example.Core\ntype Entry = { Pair : int * int ; Name : string }\n",
            "fsharp",
        ),
        (
            "Postfix.fs",
            "module Example.Core\nlet value = [ 1 ] : int list ;\n",
            "fsharp",
        ),
        (
            "TypedRhs.fs",
            "module Example.Core\nlet value = (1 : int) ;\n",
            "fsharp",
        ),
        (
            "MemberContract.fs",
            "module Example.Core\nlet value = 1\ntype Contract =\n    abstract Pair : int * int\n",
            "fsharp",
        ),
        (
            "Escaped.fs",
            "module Example.Core\nlet ``value : square dup * ;`` = 1\n",
            "fsharp",
        ),
        ("Empty.fs", "", "unknown"),
        ("Words.fs", "module and namespace let type\n", "unknown"),
        (
            "Literal.fs",
            "s\" module Example.Core\nlet value = 1\"\n",
            "unknown",
        ),
        (
            "Backup.fs.bak",
            "module Example.Core\nlet value = 1\n",
            "unknown",
        ),
        (
            "Other.txt",
            "module Example.Core\nlet value = 1\n",
            "unknown",
        ),
        ("Script.fsx", "let scriptValue = 1\n", "fsharp"),
        (
            "Contract.fsi",
            "module Example.Core\nval declaredValue: int\n",
            "fsharp-signature",
        ),
        (
            "Broken.fs",
            "module Example.Core\nlet brokenValue = (\n",
            "fsharp",
        ),
    ];
    for (path, source, _) in sources {
        fs::write(repository.path().join(path), source).unwrap();
    }
    fs::write(
        repository.path().join("binary.fs"),
        b"\0module Example\nlet value = 1\n",
    )
    .unwrap();
    fs::write(repository.path().join("Example.fsproj"), "<Project />\n").unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(receipt.binary_inputs, 1);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    assert!(document.files.iter().all(|file| file.path != "binary.fs"));
    let mut configured = FirstSliceService::new_with_storage_policy(
        4,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        None,
        policy(),
    )
    .unwrap();
    let configured_receipt = configured
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let configured_snapshot = configured
        .loaded_generation_snapshot(configured_receipt.generation)
        .unwrap();
    for (path, source, language) in sources {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, language, "{path}");
        assert_eq!(file.content_hash, content_hash(source.as_bytes()));
        assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
        let provenance = document
            .provenance
            .iter()
            .find(|record| record.id == file.provenance)
            .unwrap();
        assert_eq!(provenance.language, language);
        assert_eq!(provenance.tier, AnalysisTier::TierD);
        assert_eq!(
            provenance.producer_kind,
            if language == "unknown" {
                ProducerKind::Rule
            } else {
                ProducerKind::Parser
            }
        );
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.language, language);
        assert_eq!(coverage.tier, AnalysisTier::TierD);
        if language == "unknown" {
            assert_eq!(provenance.rule.as_deref(), Some("unsupported-language"));
            assert_eq!(coverage.status, CoverageStatus::Unknown);
            assert_eq!(
                coverage.reason,
                Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
            );
            assert!(!document.entities.iter().any(|entity| {
                entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|reference| reference.span().file() == file.id)
            }));
        } else {
            let configured_file = configured_snapshot
                .document()
                .files
                .iter()
                .find(|file| file.path == path)
                .unwrap();
            let configured_coverage = configured
                .source_file_coverage_until(
                    configured_receipt.generation,
                    configured_file.id,
                    &deadline(),
                )
                .unwrap();
            assert_eq!(coverage.language, configured_coverage.language, "{path}");
            assert_eq!(coverage.tier, configured_coverage.tier, "{path}");
            assert_eq!(coverage.status, configured_coverage.status, "{path}");
            assert_eq!(coverage.reason, configured_coverage.reason, "{path}");
        }
        if path == "Broken.fs" {
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span().file() == file.id
                        && gap.reason == SkippedRegionReason::ParseError)
            );
        }
        if path == "Full.fs" {
            for (name, kind) in [
                ("value", EntityKind::Variable),
                ("Record", EntityKind::Struct),
                ("Read", EntityKind::Method),
            ] {
                let entity = document
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.language == "fsharp"
                            && entity.canonical_name == name
                            && entity.kind == kind
                            && entity
                                .evidence
                                .source
                                .as_ref()
                                .is_some_and(|reference| reference.span().file() == file.id)
                    })
                    .unwrap();
                let site = document
                    .occurrences
                    .iter()
                    .find(|site| {
                        site.file == file.id
                            && site.role == OccurrenceRole::Definition
                            && site.target == (OccurrenceTarget::Resolved { symbol: entity.id })
                    })
                    .unwrap();
                exact_source(&service, &site.source, "fsharp", name.as_bytes());
            }
        }
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            language,
            source.as_bytes(),
        );
    }
}

#[test]
fn fsharp_content_respects_exact_configuration_and_unsupported_overrides() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let implementation = "module Example.Core\nlet value = 1\n";
    let signature = "module Example.Core\nval value: int\n";
    let forth = ": square dup * ;\n";
    for (path, source) in [
        ("Default.fs", implementation),
        ("Blocked.fs", implementation),
        ("Contract.fs", signature),
        ("Explicit.fs", forth),
    ] {
        fs::write(repository.path().join(path), source).unwrap();
    }
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.extensions]\nfs='fsharp'\n[extensions.source-languages.data.paths]\n'Blocked.fs'='unsupported-language'\n'Contract.fs'='fsharp-signature'\n"), 2).unwrap();
    let mut service = FirstSliceService::new_with_storage_policy(
        2,
        MAX_RETAINED_SOURCE_BYTES,
        None,
        None,
        policy,
    )
    .unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    for (path, language, source) in [
        ("Default.fs", "fsharp", implementation),
        ("Blocked.fs", "unsupported-language", implementation),
        ("Contract.fs", "fsharp-signature", signature),
        ("Explicit.fs", "fsharp", forth),
    ] {
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, language, "{path}");
        let provenance = document
            .provenance
            .iter()
            .find(|record| record.id == file.provenance)
            .unwrap();
        assert_eq!(provenance.language, language);
        assert_eq!(provenance.tier, AnalysisTier::TierD);
        assert_eq!(
            provenance.producer_kind,
            if path == "Blocked.fs" {
                ProducerKind::Rule
            } else {
                ProducerKind::Parser
            }
        );
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            language,
            source.as_bytes(),
        );
    }
}

#[test]
fn fsharp_content_admission_rejects_conflicting_file_evidence_but_not_project_hints() {
    let service = FirstSliceService::new(2).unwrap();
    for path in [".fs", "dir/.fs", "Library.fs.bak"] {
        assert!(!is_shared_fsharp_source(path), "{path}");
    }
    for path in ["Library.fs", "dir/LIBRARY.FS"] {
        assert!(is_shared_fsharp_source(path), "{path}");
    }
    let signal = |language: &str, evidence| rootlight_discovery::LanguageSignal {
        language: language.to_owned(),
        evidence,
    };
    let mut input = ManifestInput {
        file: FileId::from_bytes([0; 20]),
        path: "Library.fs".to_owned(),
        content_hash: content_hash(b""),
        bytes: 0,
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: vec![signal("fsharp", LanguageEvidence::Content)],
        decisive_rule: None,
    };
    assert_eq!(
        supported_source_language(&input, &service.analyzers),
        Some("fsharp")
    );
    assert_eq!(detected_source_language(&input), Some("fsharp"));
    for evidence in [
        LanguageEvidence::Extension,
        LanguageEvidence::Filename,
        LanguageEvidence::Shebang,
        LanguageEvidence::Content,
    ] {
        input.language_signals.push(signal("rust", evidence));
        assert_eq!(supported_source_language(&input, &service.analyzers), None);
        assert_eq!(detected_source_language(&input), None);
        input.language_signals.pop();
    }
    for evidence in [LanguageEvidence::Manifest, LanguageEvidence::Context] {
        input.language_signals.push(signal("rust", evidence));
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("fsharp")
        );
        assert_eq!(detected_source_language(&input), Some("fsharp"));
        input.language_signals.remove(0);
        assert_eq!(supported_source_language(&input, &service.analyzers), None);
        assert_eq!(detected_source_language(&input), None);
        input.language_signals = vec![signal("fsharp", LanguageEvidence::Content)];
    }
    input.language_signals.push(signal(
        "unsupported-language",
        LanguageEvidence::Configuration,
    ));
    assert_eq!(supported_source_language(&input, &service.analyzers), None);
    assert_eq!(
        detected_source_language(&input),
        Some("unsupported-language")
    );
    input.language_signals.pop();
    input.encoding = "utf-16-le".to_owned();
    assert_eq!(supported_source_language(&input, &service.analyzers), None);
}

#[test]
fn fsharp_content_truncation_distinguishes_prior_evidence_from_late_declarations() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bound = rootlight_discovery::MAX_CLASSIFICATION_BYTES;
    let mut incomplete_utf8 = "module Example\nlet value = 1\n// ".to_owned();
    incomplete_utf8.extend(std::iter::repeat_n(' ', bound - 1 - incomplete_utf8.len()));
    incomplete_utf8.push('λ');
    let mut late_declaration = "module Example\n".to_owned();
    late_declaration.extend(std::iter::repeat_n(' ', bound - late_declaration.len()));
    late_declaration.push_str("let value = 1\n");
    for (name, source) in [("Cut.fs", &incomplete_utf8), ("Late.fs", &late_declaration)] {
        fs::write(repository.path().join(name), source).unwrap();
    }
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    for (path, language, source) in [
        ("Cut.fs", "fsharp", incomplete_utf8),
        ("Late.fs", "unknown", late_declaration),
    ] {
        let file = snapshot
            .document()
            .files
            .iter()
            .find(|file| file.path == path)
            .unwrap();
        assert_eq!(file.language, language);
        assert_eq!(file.content_hash, content_hash(source.as_bytes()));
        let coverage = service
            .source_file_coverage_until(receipt.generation, file.id, &deadline())
            .unwrap();
        assert_eq!(coverage.tier, AnalysisTier::TierD);
        let entities = snapshot
            .document()
            .entities
            .iter()
            .filter(|entity| {
                entity.evidence.source.as_ref().is_some_and(|reference| {
                    reference.span().file() == file.id
                        && reference.generation() == receipt.generation
                })
            })
            .collect::<Vec<_>>();
        if language == "unknown" {
            assert!(entities.is_empty());
            assert_eq!(coverage.status, CoverageStatus::Unknown);
            assert_eq!(
                coverage.reason,
                Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
            );
        } else {
            assert!(entities.iter().any(|entity| {
                entity.display_name == "value"
                    && entity.kind == EntityKind::Variable
                    && entity.language == "fsharp"
            }));
            assert_ne!(
                coverage.reason,
                Some(FirstSliceSourceCoverageReason::UnsupportedLanguage)
            );
            assert!(!snapshot.document().skipped_regions.iter().any(|gap| {
                gap.source.span().file() == file.id && gap.reason == SkippedRegionReason::ParseError
            }));
        }
        exact_source(
            &service,
            file.evidence.source.as_ref().unwrap(),
            language,
            source.as_bytes(),
        );
    }
}
