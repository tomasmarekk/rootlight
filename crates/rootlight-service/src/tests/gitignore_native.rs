//! Native Gitignore routing and exact ordered rules through durable generations.
//! Patterns remain rule data; they never create software symbols or file targets.

use super::*;
use rootlight_ir::{
    GITIGNORE_PROGRAM_NAMESPACE, GitignoreAnchoring, GitignorePolarity, GitignoreProgram,
    decode_gitignore_program_envelope,
};

const RUST_IGNORE: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../rootlight-adapters/tests/fixtures/gitignore/Rust.gitignore"
));
const ORDERED: &[u8] = b"\xef\xbb\xbf# rules\r\n\r\n*.log\r\n!important.log\r\n*.log\r\n";

fn current_program(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    source: &[u8],
) -> GitignoreProgram {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    assert_eq!(file.language, "gitignore");
    assert_eq!(file.content_hash, content_hash(source));
    let read = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, source);
    assert_eq!(read.data.chunks[0].language, "gitignore");
    let programs: Vec<_> = document
        .extensions
        .iter()
        .filter(|extension| {
            extension.namespace == GITIGNORE_PROGRAM_NAMESPACE
                && extension
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file.id)
        })
        .map(|extension| decode_gitignore_program_envelope(extension).unwrap())
        .collect();
    assert_eq!(programs.len(), 1);
    let program = programs.into_iter().next().unwrap();
    assert_eq!(program.text().as_bytes(), source);
    assert_eq!(program.source().generation(), receipt.generation);
    assert_eq!(program.source().content_hash(), content_hash(source));
    assert_eq!(program.source().span().file(), file.id);
    assert!(
        program
            .rules()
            .windows(2)
            .all(|rules| rules[0].line.end_byte() < rules[1].line.start_byte())
    );
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.language == "gitignore")
    );
    assert!(!document.occurrences.iter().any(|site| site.file == file.id));
    assert!(!document.relations.iter().any(|relation| matches!(
        relation.predicate,
        RelationPredicate::RefersTo | RelationPredicate::Imports | RelationPredicate::Calls
    )));
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "gitignore-repository-context-not-analyzed"
        && gap.source.span().file() == file.id
        && gap.source.generation() == receipt.generation
        && gap.source.content_hash() == content_hash(source)));
    program
}

#[test]
fn gitignore_original_rust_template_retains_ordered_source_owned_patterns() {
    let repository = durable_test_tempdir();
    fs::write(repository.path().join("Rust.GITIGNORE"), RUST_IGNORE).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let program = current_program(&service, &receipt, "Rust.GITIGNORE", RUST_IGNORE);
    let patterns: Vec<_> = program
        .rules()
        .iter()
        .map(|rule| {
            program
                .text()
                .get(
                    usize::try_from(rule.pattern.start_byte()).unwrap()
                        ..usize::try_from(rule.pattern.end_byte()).unwrap(),
                )
                .unwrap()
        })
        .collect();
    assert_eq!(
        patterns,
        [
            "debug",
            "target",
            "**/*.rs.bk",
            "*.pdb",
            "**/mutants.out*",
            "rustc-ice-*.txt"
        ]
    );
    assert!(
        program
            .rules()
            .iter()
            .all(|rule| rule.polarity == GitignorePolarity::Exclude)
    );
    assert!(program.rules()[4].directory_only);
    assert_eq!(
        program.rules()[2].anchoring,
        GitignoreAnchoring::RelativePath
    );
    assert_eq!(program.rules()[0].anchoring, GitignoreAnchoring::AnyDepth);
}

fn assert_ordered(program: &GitignoreProgram, changed: bool) {
    assert_eq!(program.rules().len(), 3);
    assert_eq!(
        program
            .rules()
            .iter()
            .map(|rule| rule.line_number)
            .collect::<Vec<_>>(),
        [3, 4, 5]
    );
    assert_eq!(program.rules()[0].polarity, GitignorePolarity::Exclude);
    assert_eq!(program.rules()[1].polarity, GitignorePolarity::Reinclude);
    assert_eq!(
        program.rules()[2].polarity,
        if changed {
            GitignorePolarity::Reinclude
        } else {
            GitignorePolarity::Exclude
        }
    );
    let patterns: Vec<_> = program
        .rules()
        .iter()
        .map(|rule| {
            program
                .text()
                .get(
                    usize::try_from(rule.pattern.start_byte()).unwrap()
                        ..usize::try_from(rule.pattern.end_byte()).unwrap(),
                )
                .unwrap()
        })
        .collect();
    assert_eq!(patterns, ["*.log", "important.log", "*.log"]);
    assert_ne!(program.rules()[0].line, program.rules()[2].line);
}

#[test]
fn gitignore_order_duplicates_bom_and_crlf_survive_durable_transitions() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let input = repository.path().join(".gitignore");
    fs::write(&input, ORDERED).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ordered(
        &current_program(&service, &initial, ".gitignore", ORDERED),
        false,
    );
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let original = std::str::from_utf8(ORDERED).unwrap();
    let last = original.rfind("*.log\r\n").unwrap();
    let mut changed = original.to_owned();
    changed.insert(last, '!');
    fs::write(&input, changed.as_bytes()).unwrap();
    let update = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_ne!(update.generation, initial.generation);
    assert_eq!(
        service
            .incremental_evidence(update.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    assert_ordered(
        &current_program(&service, &update, ".gitignore", changed.as_bytes()),
        true,
    );
    assert_ordered(
        &current_program(&service, &initial, ".gitignore", ORDERED),
        false,
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_ordered(
        &current_program(&reopened, &update, ".gitignore", changed.as_bytes()),
        true,
    );
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        update.generation
    );
    fs::write(&input, ORDERED).unwrap();
    let restored = reopened
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_ordered(
        &current_program(&reopened, &restored, ".gitignore", ORDERED),
        false,
    );
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_ordered(
        &current_program(&final_open, &restored, ".gitignore", ORDERED),
        false,
    );
    assert_eq!(
        final_open
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn gitignore_native_routes_authentic_suffixes_and_configuration_alias_only() {
    let service = FirstSliceService::new(2).unwrap();
    assert!(service.structural_parsers.contains_key("gitignore"));
    let mut absent = service.analyzers.clone();
    assert!(absent.remove("gitignore").is_some());
    for path in [
        ".gitignore",
        ".GITIGNORE",
        "Rust.gitignore",
        "Global/Editor.GITIGNORE",
    ] {
        let mut input = ManifestInput {
            file: FileId::from_bytes([3; 20]),
            path: path.to_owned(),
            content_hash: content_hash(ORDERED),
            bytes: ORDERED.len().try_into().unwrap(),
            encoding: "utf-8".to_owned(),
            class: InputClass::Source,
            language_signals: vec![],
            project_language_witnesses: vec![],
            decisive_rule: None,
        };
        assert_eq!(detected_source_language(&input), Some("gitignore"));
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("gitignore")
        );
        assert_eq!(source_language_route(&input, &absent), None);
        input
            .language_signals
            .push(rootlight_discovery::LanguageSignal {
                language: "git-ignore".to_owned(),
                evidence: LanguageEvidence::Configuration,
            });
        assert_eq!(
            source_language_route(&input, &service.analyzers),
            Some("gitignore")
        );
        input.language_signals[0].language = "unsupported-language".to_owned();
        assert_eq!(source_language_route(&input, &service.analyzers), None);
    }
    for path in [
        ".dockerignore",
        ".npmignore",
        ".hgignore",
        ".ignore",
        ".git/info/exclude",
        "gitignore",
        "Rust.gitignore.bak",
    ] {
        assert_ne!(source_language_from_path(path), Some("gitignore"), "{path}");
    }
}

#[test]
fn gitignore_native_invalidates_preceding_analyzer_and_discovery_independently() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let preceding_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/388",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_ne!(parser, preceding_parser);
    let preceding_discovery = hash_static_components(&[
        PROVIDER_SET_SEED,
        parser.as_bytes(),
        b"1.106",
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
    assert_ne!(
        first_slice_provider_set_hash().unwrap(),
        preceding_discovery
    );
    assert_eq!(
        ANALYZER_BINARY_SEED,
        b"rootlight.first-slice.treesitter-structural/390"
    );
    assert_eq!(DISCOVERY_MANIFEST_VERSION, "1.108");
    assert_eq!(RESOLVER_BINARY_SEED, b"rootlight.first-slice.resolve/164");
}

#[test]
fn gitignore_invalid_program_keeps_exact_source_with_a_scoped_parse_gap() {
    let repository = durable_test_tempdir();
    let raw = b"valid/\n[z-a]\n!keep\n";
    fs::write(repository.path().join("Broken.gitignore"), raw).unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == "Broken.gitignore")
        .unwrap();
    assert_eq!(file.language, "gitignore");
    let read = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, raw);
    assert!(
        !document
            .extensions
            .iter()
            .any(|extension| extension.namespace == GITIGNORE_PROGRAM_NAMESPACE)
    );
    assert!(document.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == "gitignore-pattern-parse-error"
            && diagnostic
                .source
                .as_ref()
                .is_some_and(|source| source.span().file() == file.id)
    }));
    assert!(
        document
            .skipped_regions
            .iter()
            .any(|gap| gap.detail == "gitignore-pattern-parse-error"
                && gap.source.span().file() == file.id)
    );
    assert_eq!(
        service
            .source_file_coverage(receipt.generation, file.id)
            .unwrap()
            .status,
        CoverageStatus::Unknown
    );
}
