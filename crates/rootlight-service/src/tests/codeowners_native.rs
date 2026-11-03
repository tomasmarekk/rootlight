//! Native CODEOWNERS intent through retained, rebound and reopened generations.
//! Source owner tokens stay rule data and never imply GitHub account authority
//! or create software symbols, imports or resolved ownership relations.

use super::*;
use rootlight_adapters::CodeownersMatcher;
use rootlight_ir::{
    CODEOWNERS_PROGRAM_NAMESPACE, CodeownersProgram, decode_codeowners_program_envelope,
};

const ORDERED: &[u8] = b"\xef\xbb\xbf# owners\r\n*.rs @first\r\n/apps/ @org/apps\r\n/apps/clear\r\n!bad @ignored\r\n*.rs @last\r\n";

fn current_program(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    path: &str,
    bytes: &[u8],
) -> CodeownersProgram {
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    let file = document
        .files
        .iter()
        .find(|file| file.path == path)
        .unwrap();
    assert_eq!(file.language, "codeowners");
    assert_eq!(file.content_hash, content_hash(bytes));
    let read = service
        .source_read(
            receipt.generation,
            vec![file.evidence.source.clone().unwrap()],
            &deadline(),
        )
        .unwrap();
    assert_eq!(read.data.chunks[0].bytes, bytes);
    assert_eq!(read.data.chunks[0].language, "codeowners");
    let mut programs = document
        .extensions
        .iter()
        .filter(|extension| {
            extension.namespace == CODEOWNERS_PROGRAM_NAMESPACE
                && extension
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == file.id)
        })
        .map(|extension| decode_codeowners_program_envelope(extension).unwrap());
    let program = programs.next().unwrap();
    assert!(programs.next().is_none());
    assert_eq!(program.text().as_bytes(), bytes);
    assert_eq!(program.source().generation(), receipt.generation);
    assert_eq!(program.source().content_hash(), file.content_hash);
    assert_eq!(program.source().span().file(), file.id);
    assert!(
        !document
            .entities
            .iter()
            .any(|entity| entity.language == "codeowners")
    );
    assert!(!document.occurrences.iter().any(|site| site.file == file.id));
    assert!(!document.relations.iter().any(|relation| {
        relation
            .evidence
            .source
            .as_ref()
            .is_some_and(|source| source.span().file() == file.id)
    }));
    assert!(document.skipped_regions.iter().any(|gap| gap.detail
        == "codeowners-repository-application-and-owner-permissions-unverified"
        && gap.source == *program.source()));
    program
}

fn assert_order(program: CodeownersProgram, changed: bool) {
    assert_eq!(program.rules().len(), 4);
    assert_eq!(program.invalid_lines().len(), 1);
    assert_eq!(
        program
            .rules()
            .iter()
            .map(|rule| rule.line_number)
            .collect::<Vec<_>>(),
        [2, 3, 4, 6]
    );
    let last_owner = program.rules()[3].owners[0].span;
    assert_eq!(
        program
            .text()
            .get(
                usize::try_from(last_owner.start_byte()).unwrap()
                    ..usize::try_from(last_owner.end_byte()).unwrap()
            )
            .unwrap(),
        if changed { "@other" } else { "@last" }
    );
    assert!(program.rules()[2].owners.is_empty());
    let matcher = CodeownersMatcher::from_program(program, &Cancellation::new()).unwrap();
    assert_eq!(
        matcher
            .decision("nested/file.rs", false, &Cancellation::new())
            .unwrap()
            .unwrap()
            .rule_index,
        3
    );
    assert_eq!(
        matcher
            .decision("apps/clear/nested/file.txt", false, &Cancellation::new())
            .unwrap()
            .unwrap()
            .rule_index,
        2
    );
}

#[test]
fn codeowners_ordered_intent_survives_noop_change_restore_and_cold_reopen() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    fs::create_dir(repository.path().join(".github")).unwrap();
    let input = repository.path().join(".github/CODEOWNERS");
    fs::write(&input, ORDERED).unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_order(
        current_program(&service, &initial, ".github/CODEOWNERS", ORDERED),
        false,
    );
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = std::str::from_utf8(ORDERED)
        .unwrap()
        .replace("@last", "@other");
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
    assert_order(
        current_program(&service, &update, ".github/CODEOWNERS", changed.as_bytes()),
        true,
    );
    assert_order(
        current_program(&service, &initial, ".github/CODEOWNERS", ORDERED),
        false,
    );
    drop(service);
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_order(
        current_program(&service, &update, ".github/CODEOWNERS", changed.as_bytes()),
        true,
    );
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        update.generation
    );
    fs::write(&input, ORDERED).unwrap();
    let restored = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_order(
        current_program(&service, &restored, ".github/CODEOWNERS", ORDERED),
        false,
    );
    drop(service);
    let mut service =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_order(
        current_program(&service, &restored, ".github/CODEOWNERS", ORDERED),
        false,
    );
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        restored.generation
    );
}

#[test]
fn codeowners_unchanged_program_rebinds_when_a_different_file_changes() {
    let repository = durable_test_tempdir();
    fs::write(repository.path().join("CODEOWNERS"), ORDERED).unwrap();
    let other = repository.path().join("note.yaml");
    fs::write(&other, b"value: before\n").unwrap();
    let mut service = FirstSliceService::new(3).unwrap();
    let initial = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let before = current_program(&service, &initial, "CODEOWNERS", ORDERED);
    fs::write(&other, b"value: after\n").unwrap();
    let update = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let after = current_program(&service, &update, "CODEOWNERS", ORDERED);
    assert_ne!(before.source().generation(), after.source().generation());
    assert_eq!(before.rules(), after.rules());
    assert_eq!(before.invalid_lines(), after.invalid_lines());
    assert_eq!(
        service
            .incremental_evidence(update.generation)
            .unwrap()
            .parsed_files(),
        1
    );
}

#[test]
fn codeowners_native_routing_requires_a_real_convention_or_explicit_configuration() {
    let service = FirstSliceService::new(2).unwrap();
    assert!(service.structural_parsers.contains_key("codeowners"));
    let mut absent = service.analyzers.clone();
    assert!(absent.remove("codeowners").is_some());
    for path in ["CODEOWNERS", ".github/CODEOWNERS", "docs/CODEOWNERS"] {
        let input = ManifestInput {
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
        assert_eq!(detected_source_language(&input), Some("codeowners"));
        assert_eq!(
            supported_source_language(&input, &service.analyzers),
            Some("codeowners")
        );
        assert_eq!(source_language_route(&input, &absent), None);
    }
    for path in [
        "codeowners",
        "CODEOWNERS.md",
        "nested/CODEOWNERS",
        "Docs/CODEOWNERS",
    ] {
        assert_ne!(
            source_language_from_path(path),
            Some("codeowners"),
            "{path}"
        );
    }
}

#[test]
fn codeowners_native_invalidates_exact_preceding_parser_and_discovery_contexts() {
    let grammar = first_slice_grammar_revision_hash().unwrap();
    let preceding_parser = hash_static_components(&[
        PARSER_PROVIDER_SET_SEED,
        TREE_SITTER_ADAPTER_VERSION.as_bytes(),
        TREE_SITTER_RUNTIME_VERSION.as_bytes(),
        b"rootlight.first-slice.treesitter-structural/389",
        grammar.as_bytes(),
    ])
    .unwrap();
    let parser = first_slice_parser_provider_hash().unwrap();
    assert_ne!(parser, preceding_parser);
    let preceding_discovery = hash_static_components(&[
        PROVIDER_SET_SEED,
        parser.as_bytes(),
        b"1.107",
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
#[ignore = "requires the pinned external original; verifies one CODEOWNERS file subset only"]
fn codeowners_original_file_subset_retains_pinned_freecodecamp_rules() {
    let original_repository = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_CODEOWNERS_ORIGINAL_REPOSITORY")
            .expect("set ROOTLIGHT_CODEOWNERS_ORIGINAL_REPOSITORY to the pinned original"),
    );
    let original_path = original_repository.join(".github/CODEOWNERS");
    let raw = fs::read(&original_path).unwrap();
    assert_eq!(raw.len(), 1546);
    assert_eq!(std::str::from_utf8(&raw).unwrap().lines().count(), 43);
    eprintln!(
        "original CODEOWNERS file subset: bytes={}, source_hash={}",
        raw.len(),
        content_hash(&raw)
    );
    let repository = durable_test_tempdir();
    fs::create_dir(repository.path().join(".github")).unwrap();
    fs::write(repository.path().join(".github/CODEOWNERS"), &raw).unwrap();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 3).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let receipt = service
        .index_repository(repository.path(), &deadline())
        .unwrap();
    let program = current_program(&service, &receipt, ".github/CODEOWNERS", &raw);
    assert!(program.invalid_lines().is_empty());
    type ExpectedOwner = (u64, u64, &'static str);
    type ExpectedRule = (u64, u64, u64, &'static str, &'static [ExpectedOwner]);
    let expected: &[ExpectedRule] = &[
        (15, 444, 468, "*", &[(446, 468, "@freecodecamp/dev-team")]),
        (
            17,
            470,
            518,
            "/curriculum/challenges/",
            &[(494, 518, "@freecodecamp/curriculum")],
        ),
        (
            18,
            519,
            566,
            "/curriculum/structure/",
            &[(542, 566, "@freecodecamp/curriculum")],
        ),
        (
            19,
            567,
            631,
            "/client/i18n/locales/english/intro.json",
            &[(607, 631, "@freecodecamp/curriculum")],
        ),
        (
            23,
            671,
            718,
            "**/package.json",
            &[(687, 718, "@freecodecamp/package-approvers")],
        ),
        (
            24,
            719,
            768,
            "**/pnpm-lock.yaml",
            &[(737, 768, "@freecodecamp/package-approvers")],
        ),
        (
            25,
            769,
            821,
            "/pnpm-workspace.yaml",
            &[(790, 821, "@freecodecamp/package-approvers")],
        ),
        (
            32,
            991,
            1051,
            "**/motivation.json",
            &[
                (1010, 1032, "@freeCodeCamp/dev-team"),
                (1033, 1051, "@freeCodeCamp/i18n"),
            ],
        ),
        (
            39,
            1208,
            1283,
            "/client/src/redux/prop-types.ts",
            &[
                (1240, 1262, "@freeCodeCamp/dev-team"),
                (1263, 1283, "@freeCodeCamp/mobile"),
            ],
        ),
        (
            40,
            1284,
            1363,
            "/client/tools/external-curriculum/*",
            &[
                (1320, 1342, "@freeCodeCamp/dev-team"),
                (1343, 1363, "@freeCodeCamp/mobile"),
            ],
        ),
        (
            41,
            1364,
            1446,
            "/curriculum/schema/challenge-schema.js",
            &[
                (1403, 1425, "@freeCodeCamp/dev-team"),
                (1426, 1446, "@freeCodeCamp/mobile"),
            ],
        ),
        (
            42,
            1447,
            1493,
            "/renovate.json",
            &[(1462, 1493, "@freecodecamp/package-approvers")],
        ),
        (
            43,
            1494,
            1545,
            "/.github/CODEOWNERS",
            &[(1514, 1545, "@freecodecamp/package-approvers")],
        ),
    ];
    assert_eq!(program.rules().len(), expected.len());
    for (rule, &(line_number, start, end, pattern, owners)) in program.rules().iter().zip(expected)
    {
        assert_eq!(rule.line_number, line_number);
        assert_eq!((rule.line.start_byte(), rule.line.end_byte()), (start, end));
        assert_eq!(
            (rule.pattern.start_byte(), rule.pattern.end_byte()),
            (start, start + u64::try_from(pattern.len()).unwrap())
        );
        assert_eq!(
            program
                .text()
                .get(
                    usize::try_from(rule.pattern.start_byte()).unwrap()
                        ..usize::try_from(rule.pattern.end_byte()).unwrap()
                )
                .unwrap(),
            pattern
        );
        assert_eq!(rule.owners.len(), owners.len());
        for (owner, &(start, end, text)) in rule.owners.iter().zip(owners) {
            assert_eq!(owner.kind, rootlight_ir::CodeownersOwnerKind::Team);
            assert_eq!(
                (owner.span.start_byte(), owner.span.end_byte()),
                (start, end)
            );
            assert_eq!(
                program
                    .text()
                    .get(usize::try_from(start).unwrap()..usize::try_from(end).unwrap())
                    .unwrap(),
                text
            );
        }
    }
    let matcher = CodeownersMatcher::from_program(program.clone(), &Cancellation::new()).unwrap();
    for (path, index) in [
        ("README.md", 0),
        ("curriculum/challenges/file.md", 1),
        ("client/package.json", 4),
        ("client/i18n/motivation.json", 7),
        ("client/tools/external-curriculum/file.ts", 9),
        (".github/CODEOWNERS", 12),
    ] {
        assert_eq!(
            matcher
                .decision(path, false, &Cancellation::new())
                .unwrap()
                .unwrap()
                .rule_index,
            index
        );
    }
    assert_eq!(
        service
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        receipt.generation
    );
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_eq!(
        current_program(&reopened, &receipt, ".github/CODEOWNERS", &raw),
        program
    );
    assert_eq!(
        reopened
            .index_repository(repository.path(), &deadline())
            .unwrap()
            .generation,
        receipt.generation
    );
    assert_eq!(fs::read(original_path).unwrap(), raw);
}
