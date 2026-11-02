//! Original React Native headers must resolve their two physical included calls.
//! A fitting Structural fixture separates semantic candidates from resolver work refusal.

/*
The embedded fixtures are retained from facebook/react-native, revision
85a81818c6b9aa19e3d19453640e6ea58afb0357, at their original relative paths.
The full original CMake witness exports the ReactCommon public include root;
no replacement standard-library declarations or altered resolver budgets are used.

MIT License

Copyright (c) Meta Platforms, Inc. and affiliates.

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

use super::*;
use rootlight_ir::{
    EntityKind, OccurrenceRole, OccurrenceTarget, RelationEndpoint, RelationPredicate,
};

// Original SHA-256: b799890bbb3d9d5161f8cb89bbd9e6a3232d0f61aa9e84de9d4f6b39747d4b1c.
const IEQUALS: &str = concat!(
    "/*\r\n",
    " * Copyright (c) Meta Platforms, Inc. and affiliates.\r\n",
    " *\r\n",
    " * This source code is licensed under the MIT license found in the\r\n",
    " * LICENSE file in the root directory of this source tree.\r\n",
    " */\r\n",
    "\r\n",
    "#pragma once\r\n",
    "\r\n",
    "#include <react/utils/toLower.h>\r\n",
    "#include <string_view>\r\n",
    "\r\n",
    "namespace facebook::react {\r\n",
    "\r\n",
    "/**\r\n",
    " * constexpr check for case insensitive equality of two strings.\r\n",
    " */\r\n",
    "constexpr bool iequals(std::string_view a, std::string_view b)\r\n",
    "{\r\n",
    "  if (a.size() != b.size()) {\r\n",
    "    return false;\r\n",
    "  }\r\n",
    "\r\n",
    "  for (size_t i = 0; i < a.size(); i++) {\r\n",
    "    if (toLower(a[i]) != toLower(b[i])) {\r\n",
    "      return false;\r\n",
    "    }\r\n",
    "  }\r\n",
    "\r\n",
    "  return true;\r\n",
    "}\r\n",
    "\r\n",
    "} // namespace facebook::react\r\n",
);
const IEQUALS_PATH: &str = "packages/react-native/ReactCommon/react/utils/iequals.h";

// Original SHA-256: 82071184f0764a4f08a286312ad3b0278375d22c46ee4e34d034ceb333d52490.
const TO_LOWER: &str = concat!(
    "/*\r\n",
    " * Copyright (c) Meta Platforms, Inc. and affiliates.\r\n",
    " *\r\n",
    " * This source code is licensed under the MIT license found in the\r\n",
    " * LICENSE file in the root directory of this source tree.\r\n",
    " */\r\n",
    "\r\n",
    "#pragma once\r\n",
    "\r\n",
    "namespace facebook::react {\r\n",
    "\r\n",
    "/**\r\n",
    " * constexpr version of tolower\r\n",
    " */\r\n",
    "constexpr char toLower(char c)\r\n",
    "{\r\n",
    "  if (c >= 'A' && c <= 'Z') {\r\n",
    "    return static_cast<char>(c + 32);\r\n",
    "  }\r\n",
    "  return c;\r\n",
    "}\r\n",
    "\r\n",
    "} // namespace facebook::react\r\n",
);
const TO_LOWER_PATH: &str = "packages/react-native/ReactCommon/react/utils/toLower.h";

// Original SHA-256: e91e2acd3363ce61e434ff801eb420aaf003df17e3bef5f53681e64b5728e7e0.
const CMAKE: &str = include_str!(
    "fixtures/cpp_include_resolution/packages/react-native/ReactCommon/react/utils/CMakeLists.txt"
);
const CMAKE_PATH: &str = "packages/react-native/ReactCommon/react/utils/CMakeLists.txt";

// Original SHA-256: 767b46d8f0b504006ccabf466fbacca697be54e64d1815123392a78a0b56f93a.
const SOURCE_BUILD_SETTINGS: &str =
    include_str!("fixtures/cpp_include_resolution/packages/react-native/settings.gradle.kts");
const SOURCE_BUILD_SETTINGS_PATH: &str = "packages/react-native/settings.gradle.kts";

// Original SHA-256: 967c76e431935fbb75adcc934c9aaa7ff00eedf988fdfb1bdaca32b3d6ff31fc.
const ANDROID_BUILD: &str = include_str!(
    "fixtures/cpp_include_resolution/packages/react-native/ReactAndroid/build.gradle.kts"
);
const ANDROID_BUILD_PATH: &str = "packages/react-native/ReactAndroid/build.gradle.kts";

// Original SHA-256: 9a7bf06ada1681efe38ccdee581946216a2d8950368f1f5a9db8facc58074495.
const JNI_CMAKE: &str = include_str!(
    "fixtures/cpp_include_resolution/packages/react-native/ReactAndroid/src/main/jni/CMakeLists.txt"
);
const JNI_CMAKE_PATH: &str = "packages/react-native/ReactAndroid/src/main/jni/CMakeLists.txt";

fn original_files() -> [(&'static str, &'static str); 6] {
    [
        (IEQUALS_PATH, IEQUALS),
        (TO_LOWER_PATH, TO_LOWER),
        (CMAKE_PATH, CMAKE),
        (SOURCE_BUILD_SETTINGS_PATH, SOURCE_BUILD_SETTINGS),
        (ANDROID_BUILD_PATH, ANDROID_BUILD),
        (JNI_CMAKE_PATH, JNI_CMAKE),
    ]
}

#[test]
fn cpp_original_included_function_calls_resolve_to_physical_definitions() {
    assert_eq!(IEQUALS.len(), 679);
    assert_eq!(TO_LOWER.len(), 448);
    assert_eq!(CMAKE.len(), 1084);
    assert_eq!(SOURCE_BUILD_SETTINGS.len(), 1695);
    assert_eq!(ANDROID_BUILD.len(), 32911);
    assert_eq!(JNI_CMAKE.len(), 19272);
    assert!(CMAKE.contains(
        "target_include_directories(react_utils\r\n        PUBLIC\r\n          ${REACT_COMMON_DIR}"
    ));
    assert_eq!(&IEQUALS.as_bytes()[236..243], b"toLower");
    assert_eq!(&IEQUALS.as_bytes()[556..563], b"toLower");
    assert_eq!(&IEQUALS.as_bytes()[573..580], b"toLower");
    assert_eq!(&TO_LOWER.as_bytes()[303..310], b"toLower");
    let fixture = TempDir::new().expect("original C++ fixture root exists");
    write_language_fixture(fixture.path(), &original_files());
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).expect("structural service initializes");
    let receipt = service
        .index_repository_with_mode(
            fixture.path(),
            FirstSliceIndexMode::Structural,
            &cancellation,
        )
        .expect("original C++ files index structurally");
    assert_original_calls(&service, &receipt, IEQUALS);
}

fn assert_original_calls(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    caller_source: &str,
) {
    let cancellation = deadline();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let document = snapshot.document();
    for (path, original) in original_files() {
        let original = if path == IEQUALS_PATH {
            caller_source
        } else {
            original
        };
        let file = document
            .files
            .iter()
            .find(|file| file.path == path)
            .expect("complete original source is indexed");
        assert_eq!(file.content_hash, content_hash(original.as_bytes()));
        let read = service
            .source_read(
                receipt.generation,
                vec![file.evidence.source.clone().unwrap()],
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks[0].bytes, original.as_bytes());
    }
    let caller_file = document
        .files
        .iter()
        .find(|file| file.path == IEQUALS_PATH)
        .unwrap();
    let target_file = document
        .files
        .iter()
        .find(|file| file.path == TO_LOWER_PATH)
        .unwrap();
    assert_eq!(caller_file.language, "cpp");
    assert_eq!(target_file.language, "cpp");
    let caller = document
        .entities
        .iter()
        .find(|entity| {
            entity.kind == EntityKind::Function
                && entity.canonical_name == "iequals"
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == caller_file.id)
        })
        .expect("original iequals definition has its physical owner");
    let targets: Vec<_> = document
        .entities
        .iter()
        .filter(|entity| {
            entity.kind == EntityKind::Function
                && entity.canonical_name == "toLower"
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == target_file.id)
        })
        .collect();
    assert_eq!(
        targets.len(),
        1,
        "the original included definition is unique"
    );
    let target = targets[0];
    let target_span = target.evidence.source.as_ref().unwrap().span();
    assert!(target_span.start_byte() <= 303 && target_span.end_byte() >= 310);
    let mut calls: Vec<_> = document
        .occurrences
        .iter()
        .filter(|site| {
            site.file == caller_file.id
                && site.role == OccurrenceRole::CallSite
                && site.syntactic_text_hash == content_hash(b"toLower")
        })
        .collect();
    calls.sort_by_key(|site| site.source.span().start_byte());
    assert_eq!(calls.len(), 2);
    let spans: Vec<_> = calls
        .iter()
        .map(|site| {
            (
                site.source.span().start_byte(),
                site.source.span().end_byte(),
            )
        })
        .collect();
    assert_eq!(spans, [(556, 569), (573, 586)]);
    for (call, expression) in calls.iter().zip(["toLower(a[i])", "toLower(b[i])"]) {
        assert_eq!(call.syntax_kind, "cpp.call.call");
        assert_eq!(call.enclosing, Some(caller.id));
        assert_eq!(call.source.span().file(), caller_file.id);
        assert_eq!(call.source.generation(), receipt.generation);
        assert_eq!(
            call.source.content_hash(),
            content_hash(caller_source.as_bytes())
        );
        let span = call.source.span();
        assert_eq!(
            &caller_source.as_bytes()[usize::try_from(span.start_byte()).unwrap()
                ..usize::try_from(span.end_byte()).unwrap()],
            expression.as_bytes()
        );
    }
    let work_omissions: Vec<_> = document
        .diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.code == "resolution-work-bounded")
        .map(|diagnostic| diagnostic.message.as_str())
        .collect();
    assert!(
        work_omissions.is_empty(),
        "this fitting original fixture must not be refused for resolver work: {work_omissions:?}"
    );
    assert!(
        !receipt
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "resolution-work-bounded")
    );
    let observed: Vec<_> = calls
        .iter()
        .map(|call| {
            let dispatch_to_target = document
                .relations
                .iter()
                .filter(|relation| {
                    relation.subject == RelationEndpoint::Occurrence(call.id)
                        && relation.predicate == RelationPredicate::DispatchCandidate
                        && relation.object == RelationEndpoint::Entity(target.id)
                })
                .count();
            format!(
                "{}..{} target={:?}, dispatch_to_original_definition={dispatch_to_target}",
                call.source.span().start_byte(),
                call.source.span().end_byte(),
                call.target
            )
        })
        .collect();
    let mut resolved_calls = Vec::new();
    for call in &calls {
        for relation in document.relations.iter().filter(|relation| {
            relation.subject == RelationEndpoint::Occurrence(call.id)
                && relation.predicate == RelationPredicate::Calls
        }) {
            assert_eq!(relation.object, RelationEndpoint::Entity(target.id));
            resolved_calls.push((
                call.source.span().start_byte(),
                call.source.span().end_byte(),
            ));
        }
    }
    assert_eq!(
        resolved_calls,
        [(556, 569), (573, 586)],
        "both original physical calls require Calls to the included definition; semantic candidates/DispatchCandidate are distinct from work-bound omission: {observed:?}; work_omissions={work_omissions:?}"
    );
    for call in calls {
        assert_eq!(
            call.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        let provenance = document
            .provenance
            .iter()
            .find(|record| record.id == call.provenance)
            .unwrap();
        assert!(provenance.rule.as_deref().unwrap().contains(
            "partial-indexed-namespace-no-compiler-overload-adl-preprocessor-sdk-closure"
        ));
        for (path, _) in original_files() {
            let file = document
                .files
                .iter()
                .find(|file| file.path == path)
                .unwrap();
            assert!(
                provenance
                    .evidence_sources
                    .iter()
                    .any(|source| source == file.evidence.source.as_ref().unwrap()),
                "written binding retains complete source authority: {path}"
            );
        }
    }
}

#[test]
fn cpp_written_includes_refuse_missing_conflicting_or_unsupported_authority() {
    let earlier = "packages/react-native/ReactCommon/react/utils/earlier/react/utils/toLower.h";
    let conditional = CMAKE.replace("target_include_directories(", "if(UNKNOWN_PROFILE)\r\n  file(TO_CMAKE_PATH \"${CMAKE_CURRENT_SOURCE_DIR}/other\" REACT_COMMON_DIR)\r\nendif()\r\ntarget_include_directories(");
    let earlier_root = CMAKE.replace(
        "          ${REACT_COMMON_DIR}",
        "          ${CMAKE_CURRENT_SOURCE_DIR}/earlier\r\n          ${REACT_COMMON_DIR}",
    );
    let cases = [
        (
            "absent-project-mapping",
            vec![(SOURCE_BUILD_SETTINGS_PATH, None)],
        ),
        ("conditional-root", vec![(CMAKE_PATH, Some(conditional))]),
        (
            "earlier-physical-c-header",
            vec![
                (CMAKE_PATH, Some(earlier_root)),
                (earlier, Some("_Atomic(int) earlier_header;\n".to_owned())),
            ],
        ),
        (
            "competing-overload",
            vec![(
                TO_LOWER_PATH,
                Some(format!(
                    "{TO_LOWER}\r\nnamespace facebook::react {{ constexpr int toLower(int c) {{ return c; }} }}\r\n"
                )),
            )],
        ),
        (
            "different-namespace",
            vec![(
                TO_LOWER_PATH,
                Some(TO_LOWER.replace("namespace facebook::react", "namespace unrelated::react")),
            )],
        ),
        (
            "macro-context",
            vec![(
                IEQUALS_PATH,
                Some(IEQUALS.replace("#pragma once", "#pragma once\r\n#define toLower(x) (x)")),
            )],
        ),
    ];
    for (name, patches) in cases {
        let fixture = TempDir::new().unwrap();
        let mut files: BTreeMap<_, _> = original_files()
            .into_iter()
            .map(|(path, bytes)| (path, bytes.to_owned()))
            .collect();
        for (path, bytes) in patches {
            if let Some(bytes) = bytes {
                files.insert(path, bytes);
            } else {
                files.remove(path);
            }
        }
        let files: Vec<_> = files
            .iter()
            .map(|(path, bytes)| (*path, bytes.as_str()))
            .collect();
        write_language_fixture(fixture.path(), &files);
        let mut service = FirstSliceService::new(2).unwrap();
        let receipt = service
            .index_repository_with_mode(
                fixture.path(),
                FirstSliceIndexMode::Structural,
                &deadline(),
            )
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        let document = snapshot.document();
        let caller = document
            .files
            .iter()
            .find(|file| file.path == IEQUALS_PATH)
            .unwrap();
        if name == "competing-overload" {
            let target_file = document
                .files
                .iter()
                .find(|file| file.path == TO_LOWER_PATH)
                .unwrap();
            let definitions: Vec<_> = document
                .entities
                .iter()
                .filter(|entity| {
                    entity.kind == EntityKind::Function
                        && entity.canonical_name == "toLower"
                        && entity
                            .evidence
                            .source
                            .as_ref()
                            .is_some_and(|source| source.span().file() == target_file.id)
                })
                .collect();
            assert_eq!(
                definitions.len(),
                2,
                "both physical overloads must remain indexed: {definitions:?}"
            );
            assert_ne!(definitions[0].id, definitions[1].id);
        }
        if name == "earlier-physical-c-header" {
            assert_eq!(
                document
                    .files
                    .iter()
                    .find(|file| file.path == earlier)
                    .unwrap()
                    .language,
                "c"
            );
        }
        let calls: Vec<_> = document
            .occurrences
            .iter()
            .filter(|site| {
                site.file == caller.id
                    && site.role == OccurrenceRole::CallSite
                    && site.syntactic_text_hash == content_hash(b"toLower")
            })
            .collect();
        assert_eq!(calls.len(), 2, "physical calls remain visible: {name}");
        for call in calls {
            assert!(
                !matches!(call.target, OccurrenceTarget::Resolved { .. }),
                "unsupported authority stays unproven: {name}"
            );
            assert!(
                !document.relations.iter().any(|relation| relation.subject
                    == RelationEndpoint::Occurrence(call.id)
                    && relation.predicate == RelationPredicate::Calls),
                "unsupported authority grants no Calls: {name}"
            );
        }
        assert!(
            !document
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "resolution-work-bounded"),
            "refusal is semantic, not budget exhaustion: {name}"
        );
    }
}

#[test]
fn cpp_written_calls_survive_ordinary_restore_noop_incremental_and_cold_rebind() {
    let repository = durable_test_tempdir();
    let storage = durable_test_tempdir();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    write_language_fixture(repository.path(), &original_files());
    let policy = FirstSliceStoragePolicy::resolve_user_config(None, 2).unwrap();
    let mut service =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    let (initial, undo_bytes) =
        oracle_restore_preservation::with_resolution_undo_observation(|| {
            service
                .index_repository_with_mode(
                    repository.path(),
                    FirstSliceIndexMode::Structural,
                    &deadline(),
                )
                .unwrap()
        });
    assert!(
        undo_bytes > 0,
        "ordinary durable resolution actually captures its restore undo"
    );
    assert_original_calls(&service, &initial, IEQUALS);
    assert_eq!(
        service
            .index_repository_with_mode(
                repository.path(),
                FirstSliceIndexMode::Structural,
                &deadline()
            )
            .unwrap()
            .generation,
        initial.generation
    );
    let changed = format!("{IEQUALS}\r\n// retained source revision\r\n");
    fs::write(repository.path().join(IEQUALS_PATH), &changed).unwrap();
    let updated = service
        .index_repository_with_mode(
            repository.path(),
            FirstSliceIndexMode::Structural,
            &deadline(),
        )
        .unwrap();
    assert_ne!(updated.generation, initial.generation);
    assert_eq!(
        service
            .incremental_evidence(updated.generation)
            .unwrap()
            .parsed_files(),
        1
    );
    assert_original_calls(&service, &updated, &changed);
    drop(service);
    let mut reopened =
        FirstSliceService::new_durable_with_policy(policy.clone(), paths.state_dir(), &deadline())
            .unwrap();
    assert_original_calls(&reopened, &updated, &changed);
    fs::write(repository.path().join(IEQUALS_PATH), IEQUALS).unwrap();
    let restored = reopened
        .index_repository_with_mode(
            repository.path(),
            FirstSliceIndexMode::Structural,
            &deadline(),
        )
        .unwrap();
    assert_eq!(restored.logical_snapshot, initial.logical_snapshot);
    assert_original_calls(&reopened, &restored, IEQUALS);
    drop(reopened);
    let mut final_open =
        FirstSliceService::new_durable_with_policy(policy, paths.state_dir(), &deadline()).unwrap();
    assert_original_calls(&final_open, &restored, IEQUALS);
    assert_eq!(
        final_open
            .index_repository_with_mode(
                repository.path(),
                FirstSliceIndexMode::Structural,
                &deadline()
            )
            .unwrap()
            .generation,
        restored.generation
    );
}
