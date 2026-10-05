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
fn fsharp_parenthesized_enum_sources_survive_durable_reopen() {
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
        let sources = [
            ("Values.fs", "fsharp", "let following = 30"),
            ("Values.fsi", "fsharp-signature", "val following: int"),
        ]
        .map(|(path, language, following)| {
            let source = format!(
                "module EnumValues\ntype Flags =\n    | First = (1u)\n    | Next = (1u <<< 0)\n{following}\n"
            )
            .replace('\n', newline);
            fs::write(repository.path().join(path), &source).unwrap();
            (path, language, source)
        });
        let mut original = None;
        for lifetime in 0..2 {
            let mut service = FirstSliceService::new_durable_with_policy(
                policy(),
                paths.state_dir(),
                &deadline(),
            )
            .unwrap();
            let commit = index(
                &mut service,
                repository.path(),
                FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural),
            );
            let receipt = commit.receipt();
            assert_eq!(receipt.discovered_inputs, 2);
            assert_eq!(receipt.indexed_files, 2);
            assert!(receipt.discovery_complete);
            if lifetime == 0 {
                assert_eq!(commit.evidence().rebuilt_files, 2);
                original = Some(receipt.clone());
            } else {
                assert_eq!(Some(receipt), original.as_ref());
                assert_eq!(commit.evidence().rebuilt_files, 0);
                assert_eq!(commit.evidence().rebuilt_facts, 0);
            }
            let snapshot = service
                .loaded_generation_snapshot(receipt.generation)
                .unwrap();
            let document = snapshot.document();
            assert_eq!(document.files.len(), 2);
            assert!(!document.skipped_regions.iter().any(|gap| {
                matches!(
                    gap.reason,
                    SkippedRegionReason::ParseError | SkippedRegionReason::AdapterFailure
                )
            }));
            for (path, language, source) in &sources {
                let file = document
                    .files
                    .iter()
                    .find(|file| file.path == *path)
                    .unwrap();
                assert_eq!(file.language, *language);
                let full = file.evidence.source.as_ref().unwrap();
                assert_eq!(full.repository(), receipt.repository);
                assert_eq!(full.generation(), receipt.generation);
                assert_eq!(full.content_hash(), content_hash(source.as_bytes()));
                exact_source(&service, full, language, source.as_bytes());
                let coverage = service
                    .source_file_coverage_until(receipt.generation, file.id, &deadline())
                    .unwrap();
                assert_eq!(coverage.language, *language);
                // Complete written declarations remain structural tier D;
                // this fixture does not prove enum evaluation or name resolution.
                assert_eq!(coverage.tier, AnalysisTier::TierD);
                assert_eq!(coverage.status, CoverageStatus::Complete);
                assert_eq!(coverage.reason, None);
                let unique = |name: &str, kind| {
                    let entities: Vec<_> = document
                        .entities
                        .iter()
                        .filter(|entity| {
                            entity.language == *language
                                && entity.canonical_name == name
                                && entity.kind == kind
                                && entity.evidence.source.as_ref().unwrap().span().file() == file.id
                        })
                        .collect();
                    assert_eq!(entities.len(), 1, "{language}: {name}");
                    entities[0]
                };
                let enumeration = unique("Flags", EntityKind::Enum);
                for (name, written) in [("First", "First = (1u)"), ("Next", "Next = (1u <<< 0)")] {
                    let case = unique(name, EntityKind::Constant);
                    assert_eq!(case.container, Some(ContainerRef::Entity(enumeration.id)));
                    assert_eq!(
                        case.evidence.source.as_ref().unwrap().generation(),
                        receipt.generation
                    );
                    exact_source(
                        &service,
                        case.evidence.source.as_ref().unwrap(),
                        language,
                        written.as_bytes(),
                    );
                    let definitions: Vec<_> = document
                        .occurrences
                        .iter()
                        .filter(|site| {
                            site.file == file.id
                                && site.role == OccurrenceRole::Definition
                                && site.target == (OccurrenceTarget::Resolved { symbol: case.id })
                        })
                        .collect();
                    assert_eq!(definitions.len(), 1);
                    assert_eq!(definitions[0].enclosing, Some(enumeration.id));
                    exact_source(&service, &definitions[0].source, language, name.as_bytes());
                    assert!(document.relations.iter().any(|relation| {
                        relation.predicate == RelationPredicate::Contains
                            && relation.subject == RelationEndpoint::Entity(enumeration.id)
                            && relation.object == RelationEndpoint::Entity(case.id)
                    }));
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
                            .any(|hit| hit.symbol == Some(case.id) && hit.file == file.id)
                    );
                }
                unique("following", EntityKind::Variable);
            }
            assert!(document.relations.iter().all(|relation| matches!(
                relation.predicate,
                RelationPredicate::Contains
                    | RelationPredicate::Declares
                    | RelationPredicate::DefinesAt
            )));
        }
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
            "MultilineValue.fs",
            "module Example.Core\nlet multilineValue =\n    1",
            "fsharp",
        ),
        (
            "MultilineFunction.fs",
            "module Example.Core\nlet identity input =\n    input\n",
            "fsharp",
        ),
        (
            "MultilineRecord.fs",
            "namespace Example.Core\r\ntype Entry =\r\n    { Count: int }",
            "fsharp",
        ),
        (
            "MultilineUnion.fs",
            "namespace Example.Core\ntype Choice =\n    | Empty\n    | Item of int\n",
            "fsharp",
        ),
        (
            "MultilineNested.fs",
            "module Example.Core\nmodule Inner =\n    let nestedValue =\n        2\n",
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
        let expected_declarations: &[(&str, EntityKind)] = match path {
            "Full.fs" => &[
                ("value", EntityKind::Variable),
                ("Record", EntityKind::Struct),
                ("Read", EntityKind::Method),
            ],
            "MultilineValue.fs" => &[("multilineValue", EntityKind::Variable)],
            "MultilineFunction.fs" => &[("identity", EntityKind::Function)],
            "MultilineRecord.fs" => &[("Entry", EntityKind::Struct)],
            "MultilineUnion.fs" => &[("Choice", EntityKind::Union)],
            "MultilineNested.fs" => &[
                ("Inner", EntityKind::Module),
                ("nestedValue", EntityKind::Variable),
            ],
            _ => &[],
        };
        for (name, kind) in expected_declarations.iter().copied() {
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
            assert_eq!(site.source.generation(), receipt.generation);
            let located = service
                .code_locate_with_filters_and_budget(
                    receipt.generation,
                    name.to_owned(),
                    LocateMode::Exact,
                    Vec::new(),
                    vec![path.to_owned()],
                    8,
                    0,
                    FirstSliceBudget::default(),
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
        project_language_witnesses: Vec::new(),
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

fn literal_project_input() -> ManifestInput {
    let source = b"let answer = 1\n";
    ManifestInput {
        file: FileId::from_bytes([1; 20]),
        path: "Member.fs".to_owned(),
        content_hash: content_hash(source),
        bytes: u64::try_from(source.len()).unwrap(),
        encoding: "utf-8".to_owned(),
        class: InputClass::Source,
        language_signals: vec![rootlight_discovery::LanguageSignal {
            language: "fsharp".to_owned(),
            evidence: LanguageEvidence::Project,
        }],
        project_language_witnesses: vec![rootlight_discovery::ProjectLanguageWitness {
            language: "fsharp".to_owned(),
            project_file: FileId::from_bytes([2; 20]),
            project_path: "Example.fsproj".to_owned(),
            project_content_hash: content_hash(
                b"<Project><ItemGroup><Compile Include=\"Member.fs\" /></ItemGroup></Project>\n",
            ),
        }],
        decisive_rule: None,
    }
}

#[test]
fn fsharp_project_selection_requires_source_bound_unanimous_witnesses() {
    let service = FirstSliceService::new(2).unwrap();
    let input = literal_project_input();
    assert_eq!(fsharp_content_language(&input), None);
    assert_eq!(detected_source_language(&input), Some("fsharp"));
    assert_eq!(
        supported_source_language(&input, &service.analyzers),
        Some("fsharp")
    );
    let signal = |language: &str, evidence| rootlight_discovery::LanguageSignal {
        language: language.to_owned(),
        evidence,
    };
    let mut invalid = input.clone();
    invalid.project_language_witnesses.clear();
    assert_eq!(detected_source_language(&invalid), None);
    invalid = input.clone();
    invalid.language_signals.clear();
    assert_eq!(detected_source_language(&invalid), None);
    for evidence in [LanguageEvidence::Manifest, LanguageEvidence::Context] {
        invalid.language_signals = vec![signal("fsharp", evidence)];
        assert_eq!(
            supported_source_language(&invalid, &service.analyzers),
            None
        );
    }
    for evidence in [
        LanguageEvidence::Project,
        LanguageEvidence::Content,
        LanguageEvidence::Extension,
        LanguageEvidence::Filename,
        LanguageEvidence::Shebang,
    ] {
        invalid = input.clone();
        invalid.language_signals.push(signal("rust", evidence));
        assert_eq!(detected_source_language(&invalid), None, "{evidence:?}");
        assert_eq!(
            supported_source_language(&invalid, &service.analyzers),
            None
        );
    }
    for path in [
        "../Example.fsproj",
        "/Example.fsproj",
        "dir/./Example.fsproj",
        "Example.fsproj.bak",
        ".fsproj",
    ] {
        invalid = input.clone();
        invalid.project_language_witnesses[0].project_path = path.to_owned();
        assert_eq!(detected_source_language(&invalid), None, "{path}");
    }
    invalid = input.clone();
    invalid.project_language_witnesses[0].project_file = input.file;
    assert_eq!(detected_source_language(&invalid), None);
    invalid = input.clone();
    invalid.project_language_witnesses[0].language = "rust".to_owned();
    assert_eq!(detected_source_language(&invalid), None);
    invalid
        .language_signals
        .push(signal("fsharp", LanguageEvidence::Content));
    assert_eq!(detected_source_language(&invalid), Some("fsharp"));
    invalid
        .language_signals
        .push(signal("rust", LanguageEvidence::Configuration));
    assert_eq!(
        supported_source_language(&invalid, &service.analyzers),
        Some("rust")
    );
    invalid = input;
    invalid.encoding = "utf-16-le".to_owned();
    assert_eq!(detected_source_language(&invalid), Some("fsharp"));
    assert_eq!(
        supported_source_language(&invalid, &service.analyzers),
        None
    );
}

#[test]
fn fsharp_project_routes_bind_member_witnesses_without_global_project_dependencies() {
    let input = literal_project_input();
    let original = file_parser_route_hash(&input, Some("fsharp"))
        .unwrap()
        .unwrap();
    for component in 0..4 {
        let mut changed = input.clone();
        let witness = &mut changed.project_language_witnesses[0];
        match component {
            0 => witness.language = "rust".to_owned(),
            1 => witness.project_file = FileId::from_bytes([3; 20]),
            2 => witness.project_path = "Renamed.fsproj".to_owned(),
            _ => witness.project_content_hash = content_hash(b"changed project document"),
        }
        assert_ne!(
            original,
            file_parser_route_hash(&changed, Some("fsharp"))
                .unwrap()
                .unwrap()
        );
    }
    let mut unknown = input.clone();
    unknown.language_signals.clear();
    unknown.project_language_witnesses.clear();
    let unproven = file_parser_route_hash(&unknown, None).unwrap().unwrap();
    assert_ne!(unproven, original);
    unknown.project_language_witnesses = input.project_language_witnesses.clone();
    assert_ne!(
        unproven,
        file_parser_route_hash(&unknown, None).unwrap().unwrap()
    );
    let mut independent = input.clone();
    for evidence in [LanguageEvidence::Content, LanguageEvidence::Configuration] {
        independent
            .language_signals
            .push(rootlight_discovery::LanguageSignal {
                language: "fsharp".to_owned(),
                evidence,
            });
        let before = file_parser_route_hash(&independent, Some("fsharp")).unwrap();
        independent.project_language_witnesses[0].project_content_hash =
            content_hash(b"unconsumed edit");
        assert_eq!(
            before,
            file_parser_route_hash(&independent, Some("fsharp")).unwrap()
        );
        independent = input.clone();
    }
    let mut objective_c = input;
    objective_c.path = "Example.m".to_owned();
    assert_eq!(
        file_parser_route_hash(&objective_c, Some("matlab")).unwrap(),
        Some(hash_static_components(&[b"rootlight.file-parser-route/1", b"matlab"]).unwrap())
    );
    assert_eq!(file_parser_route_hash(&objective_c, None).unwrap(), None);
}

fn assert_project_member(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    source: &[u8],
    language: &str,
    definition: Option<&str>,
) -> FileId {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .chain(
            snapshot
                .source_files()
                .entries()
                .iter()
                .map(|entry| entry.file()),
        )
        .find(|file| file.path == path)
        .unwrap();
    assert_eq!(file.language, language, "{path}");
    assert_eq!(file.byte_length, u64::try_from(source.len()).unwrap());
    assert_eq!(file.content_hash, content_hash(source));
    let reference = file.evidence.source.as_ref().unwrap();
    assert_eq!(reference.generation(), receipt.generation);
    assert_eq!(reference.span().file(), file.id);
    assert_eq!(reference.span().start_byte(), 0);
    assert_eq!(reference.span().end_byte(), file.byte_length);
    let read = service
        .source_read_with_options_and_budget(
            receipt.generation,
            vec![reference.clone()],
            SourceReadOptions::new()
                .with_context_lines_before(0)
                .with_context_lines_after(0)
                .with_encoding(rootlight_source::SourceEncoding::Bytes),
            FirstSliceBudget::default(),
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks.len(), 1);
    assert_eq!(read.data.chunks[0].reference, *reference);
    assert_eq!(read.data.chunks[0].content_hash, file.content_hash);
    assert_eq!(read.data.chunks[0].bytes, source);
    let coverage = service
        .source_file_coverage_until(receipt.generation, file.id, &deadline())
        .unwrap();
    assert_eq!(coverage.language, language);
    assert_eq!(coverage.tier, AnalysisTier::TierD);
    if let Some(name) = definition {
        assert_eq!(coverage.status, CoverageStatus::Complete);
        assert_eq!(coverage.reason, None);
        let entity = document
            .entities
            .iter()
            .find(|entity| {
                entity.canonical_name == name
                    && entity.language == language
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
        assert_eq!(site.source.generation(), receipt.generation);
        exact_source(service, &site.source, language, name.as_bytes());
        let located = service
            .code_locate_with_filters_and_budget(
                receipt.generation,
                name.to_owned(),
                LocateMode::Exact,
                Vec::new(),
                vec![path.to_owned()],
                8,
                0,
                FirstSliceBudget::default(),
                &deadline(),
            )
            .unwrap();
        assert!(
            located
                .data
                .hits
                .iter()
                .any(|hit| hit.file == file.id && hit.symbol == Some(entity.id))
        );
    } else {
        assert_eq!(coverage.status, CoverageStatus::Unknown);
        let reason = if document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
        {
            FirstSliceSourceCoverageReason::ResourceBounded
        } else if file.encoding != "utf-8" {
            FirstSliceSourceCoverageReason::UnsupportedEncoding
        } else {
            FirstSliceSourceCoverageReason::UnsupportedLanguage
        };
        assert_eq!(coverage.reason, Some(reason));
        assert!(!document.entities.iter().any(|entity| {
            entity
                .evidence
                .source
                .as_ref()
                .is_some_and(|reference| reference.span().file() == file.id)
        }));
    }
    file.id
}

fn retained_parser_route(
    service: &FirstSliceService,
    generation: GenerationId,
    file: FileId,
) -> ContentHash {
    service
        .incremental_inputs
        .get(&generation)
        .unwrap()
        .value(InputKey::FileParserRoute(file))
        .unwrap()
}

#[test]
fn fsharp_literal_project_routes_track_add_remove_change_and_durable_reopen() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let member = b"let answer = 1\n";
    let peer = b"let peer = 2\n";
    let strong = b"module Strong\nlet stable = 3\n";
    for (path, source) in [
        ("Member.fs", member.as_slice()),
        ("Peer.fs", peer.as_slice()),
        ("Strong.fs", strong.as_slice()),
    ] {
        fs::write(repository.path().join(path), source).unwrap();
    }
    let project_path = repository.path().join("Example.fsproj");
    let project = "<Project><ItemGroup><Compile Include=\"Member.fs\" /><Compile Include=\"Strong.fs\" /></ItemGroup></Project>\n";
    let peer_path = repository.path().join("Peer.fsproj");
    let peer_project =
        "<Project><ItemGroup><Compile Include=\"Peer.fs\" /></ItemGroup></Project>\n";
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n"),
        4,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let initial = index(&mut service, repository.path(), options);
    let member_id = assert_project_member(
        &service,
        initial.receipt(),
        "Member.fs",
        member,
        "unknown",
        None,
    );
    let unknown_route = retained_parser_route(&service, initial.receipt().generation, member_id);
    fs::write(&project_path, project).unwrap();
    let added = index(&mut service, repository.path(), options);
    assert_eq!(
        assert_project_member(
            &service,
            added.receipt(),
            "Member.fs",
            member,
            "fsharp",
            Some("answer")
        ),
        member_id
    );
    assert_eq!(added.evidence().rebuilt_files, 1);
    let added_route = retained_parser_route(&service, added.receipt().generation, member_id);
    assert_ne!(unknown_route, added_route);
    fs::write(&peer_path, peer_project).unwrap();
    let paired = index(&mut service, repository.path(), options);
    let peer_id = assert_project_member(
        &service,
        paired.receipt(),
        "Peer.fs",
        peer,
        "fsharp",
        Some("peer"),
    );
    let strong_id = assert_project_member(
        &service,
        paired.receipt(),
        "Strong.fs",
        strong,
        "fsharp",
        Some("stable"),
    );
    let strong_route = retained_parser_route(&service, paired.receipt().generation, strong_id);
    let peer_route = retained_parser_route(&service, paired.receipt().generation, peer_id);
    assert_eq!(
        retained_parser_route(&service, paired.receipt().generation, member_id),
        added_route
    );
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt().generation, paired.receipt().generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    fs::write(
        &project_path,
        project.replace("</Project>", "<!-- authored documentation --></Project>"),
    )
    .unwrap();
    let changed = index(&mut service, repository.path(), options);
    assert_project_member(
        &service,
        changed.receipt(),
        "Member.fs",
        member,
        "fsharp",
        Some("answer"),
    );
    let changed_route = retained_parser_route(&service, changed.receipt().generation, member_id);
    assert_ne!(changed_route, added_route);
    assert_eq!(
        retained_parser_route(&service, changed.receipt().generation, peer_id),
        peer_route
    );
    assert_eq!(
        retained_parser_route(&service, changed.receipt().generation, strong_id),
        strong_route
    );
    assert_eq!(changed.evidence().rebuilt_files, 1);
    assert_eq!(changed.evidence().reused_files, 2);
    fs::remove_file(&project_path).unwrap();
    let removed = index(&mut service, repository.path(), options);
    assert_project_member(
        &service,
        removed.receipt(),
        "Member.fs",
        member,
        "unknown",
        None,
    );
    assert_eq!(
        retained_parser_route(&service, removed.receipt().generation, member_id),
        unknown_route
    );
    assert_eq!(removed.evidence().rebuilt_files, 0);
    fs::write(&project_path, project).unwrap();
    let restored = index(&mut service, repository.path(), options);
    assert_project_member(
        &service,
        restored.receipt(),
        "Member.fs",
        member,
        "fsharp",
        Some("answer"),
    );
    assert_eq!(
        retained_parser_route(&service, restored.receipt().generation, member_id),
        added_route
    );
    assert_eq!(restored.evidence().rebuilt_files, 1);
    fs::write(
        &peer_path,
        peer_project.replace("</Project>", "<!-- peer-only documentation --></Project>"),
    )
    .unwrap();
    let unrelated = index(&mut service, repository.path(), options);
    assert_project_member(
        &service,
        unrelated.receipt(),
        "Member.fs",
        member,
        "fsharp",
        Some("answer"),
    );
    assert_eq!(
        retained_parser_route(&service, unrelated.receipt().generation, member_id),
        added_route
    );
    assert_ne!(
        retained_parser_route(&service, unrelated.receipt().generation, peer_id),
        peer_route
    );
    assert_eq!(unrelated.evidence().rebuilt_files, 1);
    assert_eq!(unrelated.evidence().reused_files, 2);
    let last = unrelated.receipt().clone();
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_project_member(
        &reopened,
        &last,
        "Member.fs",
        member,
        "fsharp",
        Some("answer"),
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, last.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_project_declarations_do_not_expand_admission_or_accept_unevaluated_expressions() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let source = b"let answer = 1\n";
    let cases = [
        (
            "malformed",
            "Example.fsproj",
            "<Project><ItemGroup><Compile Include=\"Library.fs\" /></ItemGroup></Project><broken",
        ),
        (
            "conditional",
            "Example.fsproj",
            "<Project><ItemGroup Condition=\"true\"><Compile Include=\"Library.fs\" /></ItemGroup></Project>",
        ),
        (
            "dynamic",
            "Example.fsproj",
            "<Project><ItemGroup><Compile Include=\"$(Source)\" /></ItemGroup></Project>",
        ),
        (
            "suffix",
            "Example.fsproj.bak",
            "<Project><ItemGroup><Compile Include=\"Library.fs\" /></ItemGroup></Project>",
        ),
        (
            "missing",
            "Example.fsproj",
            "<Project><ItemGroup><Compile Include=\"Missing.fs\" /></ItemGroup></Project>",
        ),
        (
            "wrong-member",
            "Example.fsproj",
            "<Project><ItemGroup><Compile Include=\"Library.fs.bak\" /></ItemGroup></Project>",
        ),
    ];
    for (directory, project_name, project) in cases {
        let root = repository.path().join(directory);
        fs::create_dir(&root).unwrap();
        fs::write(root.join("Library.fs"), source).unwrap();
        fs::write(root.join(project_name), project).unwrap();
    }
    fs::create_dir(repository.path().join("target")).unwrap();
    fs::write(repository.path().join("target/Hidden.fs"), source).unwrap();
    fs::write(
        repository.path().join("IgnoredMember.fsproj"),
        "<Project><ItemGroup><Compile Include=\"target/Hidden.fs\" /></ItemGroup></Project>",
    )
    .unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    for (directory, _, _) in cases {
        assert_project_member(
            &service,
            &receipt,
            &format!("{directory}/Library.fs"),
            source,
            "unknown",
            None,
        );
    }
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert!(
        !snapshot
            .document()
            .files
            .iter()
            .chain(
                snapshot
                    .source_files()
                    .entries()
                    .iter()
                    .map(|entry| entry.file())
            )
            .any(|file| file.path == "target/Hidden.fs")
    );
}

#[test]
fn fsharp_project_routes_preserve_configuration_and_utf16_parser_boundary() {
    let repository = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let plain = b"let answer = 1\n";
    let signature = b"val declared: int\n";
    let encoded = [
        vec![0xff, 0xfe],
        "let encoded = 2\n"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect(),
    ]
    .concat();
    fs::write(repository.path().join("Blocked.fs"), plain).unwrap();
    fs::write(repository.path().join("Signature.fs"), signature).unwrap();
    fs::write(repository.path().join("Encoded.fs"), &encoded).unwrap();
    fs::write(repository.path().join("Example.fsproj"), "<Project><ItemGroup><Compile Include=\"Blocked.fs\" /><Compile Include=\"Signature.fs\" /><Compile Include=\"Encoded.fs\" /></ItemGroup></Project>").unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(Some("version='1.3'\n[extensions.source-languages]\nversion='1.0'\ncritical=true\n[extensions.source-languages.data.paths]\n'Blocked.fs'='unsupported-language'\n'Signature.fs'='fsharp-signature'\n"), 2).unwrap();
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
    assert_project_member(
        &service,
        &receipt,
        "Blocked.fs",
        plain,
        "unsupported-language",
        None,
    );
    assert_project_member(
        &service,
        &receipt,
        "Signature.fs",
        signature,
        "fsharp-signature",
        Some("declared"),
    );
    let file_id = assert_project_member(&service, &receipt, "Encoded.fs", &encoded, "fsharp", None);
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let file = snapshot
        .document()
        .files
        .iter()
        .find(|file| file.id == file_id)
        .unwrap();
    assert_eq!(file.encoding, "utf-16le");
    let coverage = service
        .source_file_coverage_until(receipt.generation, file_id, &deadline())
        .unwrap();
    assert_eq!(coverage.status, CoverageStatus::Unknown);
    assert_eq!(
        coverage.reason,
        Some(FirstSliceSourceCoverageReason::UnsupportedEncoding)
    );
}

#[test]
fn fsharp_project_route_survives_zero_supported_transitions() {
    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let source = b"let standalone = 1\n";
    fs::write(repository.path().join("Member.fs"), source).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n"),
        4,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let initial = index(&mut service, repository.path(), options);
    assert_eq!(initial.evidence().rebuilt_files, 0);
    let file = assert_project_member(
        &service,
        initial.receipt(),
        "Member.fs",
        source,
        "unknown",
        None,
    );
    let unknown_route = retained_parser_route(&service, initial.receipt().generation, file);
    let project_path = repository.path().join("Example.fsproj");
    fs::write(
        &project_path,
        "<Project><ItemGroup><Compile Include=\"Member.fs\" /></ItemGroup></Project>\n",
    )
    .unwrap();
    let added = index(&mut service, repository.path(), options);
    assert_eq!(added.evidence().rebuilt_files, 1);
    assert_eq!(
        assert_project_member(
            &service,
            added.receipt(),
            "Member.fs",
            source,
            "fsharp",
            Some("standalone")
        ),
        file
    );
    assert_ne!(
        retained_parser_route(&service, added.receipt().generation, file),
        unknown_route
    );
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt().generation, added.receipt().generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    fs::remove_file(&project_path).unwrap();
    let removed = index(&mut service, repository.path(), options);
    assert_eq!(removed.evidence().rebuilt_files, 0);
    assert_eq!(
        assert_project_member(
            &service,
            removed.receipt(),
            "Member.fs",
            source,
            "unknown",
            None
        ),
        file
    );
    assert_eq!(
        retained_parser_route(&service, removed.receipt().generation, file),
        unknown_route
    );
    let last = removed.receipt().clone();
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_project_member(&reopened, &last, "Member.fs", source, "unknown", None);
    assert_eq!(
        retained_parser_route(&reopened, last.generation, file),
        unknown_route
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, last.generation);
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}

#[test]
fn fsharp_project_routes_survive_resource_fallback() {
    fn assert_fallback_member(
        service: &FirstSliceService,
        receipt: &FirstSliceIndexReceipt,
        source: &[u8],
        language: &str,
    ) -> FileId {
        assert!(
            receipt
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE)
        );
        assert!(receipt.discovery_complete);
        assert_eq!(receipt.entities, 0);
        assert_eq!(receipt.extracted_files, 0);
        let file = assert_project_member(service, receipt, "Member.fs", source, language, None);
        for query in ["Member.fs", "standalone"] {
            let located = service
                .code_locate(
                    receipt.generation,
                    query.to_owned(),
                    LocateMode::Exact,
                    8,
                    0,
                    &deadline(),
                )
                .unwrap();
            let hit = located
                .data
                .hits
                .iter()
                .find(|hit| hit.file == file)
                .unwrap();
            assert_eq!(hit.kind, "file");
            assert_eq!(hit.symbol, None);
            let reference = hit.source.as_ref().unwrap();
            assert_eq!(reference.generation(), receipt.generation);
            assert_eq!(reference.span().file(), file);
            assert_eq!(reference.content_hash(), content_hash(source));
        }
        file
    }

    let current = std::env::current_dir().unwrap();
    let repository = tempfile::tempdir_in(&current).unwrap();
    let storage = tempfile::tempdir_in(&current).unwrap();
    let paths = rootlight_runtime::RuntimePaths::new(
        storage.path().join("state"),
        storage.path().join("runtime"),
    )
    .unwrap();
    paths.prepare_owner().unwrap();
    let source = b"let standalone = 1\n";
    fs::write(repository.path().join("Member.fs"), source).unwrap();
    fs::write(repository.path().join("Peer.fs"), b"let peer = 2\n").unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(
        Some("version='1.3'\n[storage]\nretained_generations=4\n"),
        4,
    )
    .unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let current = &service.analysis_limits;
    let mut ir = current.ir().clone();
    // Two admitted files force the real resource fallback, not merely an empty
    // parser selection. Every other analysis ceiling remains unchanged.
    ir.max_files = 1;
    let mut limits = AnalysisLimits::new(
        current.max_source_bytes(),
        current.max_syntax_nodes(),
        current.max_syntax_depth(),
        current.max_embedded_ranges(),
        current.max_reported_memory_bytes(),
        current.syntax_stream().clone(),
        current.ir_stream().clone(),
        ir,
    )
    .unwrap();
    if let Some(project) = current.project() {
        limits = limits.with_project_limits(project);
    }
    service.analysis_limits = limits.clone();
    let options = FirstSliceIndexOptions::incremental(FirstSliceIndexMode::Structural);
    let initial = index(&mut service, repository.path(), options);
    let file = assert_fallback_member(&service, initial.receipt(), source, "unknown");
    let unknown_route = retained_parser_route(&service, initial.receipt().generation, file);
    let project_path = repository.path().join("Example.fsproj");
    let project = "<Project><ItemGroup><Compile Include=\"Member.fs\" /></ItemGroup></Project>\n";
    fs::write(&project_path, project).unwrap();
    let added = index(&mut service, repository.path(), options);
    assert_eq!(
        assert_fallback_member(&service, added.receipt(), source, "fsharp"),
        file
    );
    let project_route = retained_parser_route(&service, added.receipt().generation, file);
    assert_ne!(project_route, unknown_route);
    let noop = index(&mut service, repository.path(), options);
    assert_eq!(noop.receipt().generation, added.receipt().generation);
    assert_fallback_member(&service, noop.receipt(), source, "fsharp");
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
    fs::write(
        &project_path,
        format!("{project}<!-- retained provenance -->\n"),
    )
    .unwrap();
    let changed = index(&mut service, repository.path(), options);
    assert_eq!(
        assert_fallback_member(&service, changed.receipt(), source, "fsharp"),
        file
    );
    assert_ne!(
        retained_parser_route(&service, changed.receipt().generation, file),
        project_route
    );
    fs::remove_file(&project_path).unwrap();
    let removed = index(&mut service, repository.path(), options);
    assert_eq!(
        assert_fallback_member(&service, removed.receipt(), source, "unknown"),
        file
    );
    assert_eq!(
        retained_parser_route(&service, removed.receipt().generation, file),
        unknown_route
    );
    let last = removed.receipt().clone();
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    reopened.analysis_limits = limits;
    assert_eq!(
        assert_fallback_member(&reopened, &last, source, "unknown"),
        file
    );
    assert_eq!(
        retained_parser_route(&reopened, last.generation, file),
        unknown_route
    );
    let noop = index(&mut reopened, repository.path(), options);
    assert_eq!(noop.receipt().generation, last.generation);
    assert_fallback_member(&reopened, noop.receipt(), source, "unknown");
    assert_eq!(noop.evidence().rebuilt_files, 0);
    assert_eq!(noop.evidence().rebuilt_facts, 0);
}
