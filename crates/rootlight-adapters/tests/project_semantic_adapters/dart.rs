//! Native Dart namespace tests through the public project-analysis boundary.
//! Deliberate name collisions prove import identity instead of repository-wide
//! same-name fallback; source spans remain tied to the admitted snapshots.

use super::*;

#[test]
fn bounded_project_retains_late_prefixed_import_calls() {
    let noise = (0..400)
        .map(|index| format!(" value.noise{index}();\n"))
        .collect::<String>();
    let source = format!(
        "import 'library.dart' as api;\nvoid start(dynamic value) {{\n{noise} api.selected();\n}}\n"
    );
    let fixture = ProjectFixture::new(
        ["entry.dart", "library.dart"],
        [&source, "void selected() {}\n"],
        SemanticProjectLanguage::Dart,
    );
    let output = analyze_with_real_parser(&fixture);
    let target = output
        .document()
        .entities
        .iter()
        .find(|entity| entity.display_name == "selected")
        .unwrap()
        .id;
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::CallSite
                && occurrence.target == OccurrenceTarget::Resolved { symbol: target }),
        "bounded optional syntax must preserve the distinct imported target"
    );
    assert!(
        output
            .document()
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == PROJECT_SYNTAX_FACT_LIMIT_DIAGNOSTIC)
    );
}

#[test]
fn declarations_keep_structural_identity_in_project_analysis() {
    let fixture = ProjectFixture::new(
        ["library.dart"],
        [
            "class Store { final int value; Store(this.value); Store /* owner */ . named(this.value); int read(int input) => input; int get size => value; set size(int next) {} }\nvoid start() {}\n",
        ],
        SemanticProjectLanguage::Dart,
    );
    assert_real_parser_symbol_identity(
        &fixture,
        "dart",
        &[
            (EntityKind::Class, "Store"),
            (EntityKind::Constructor, "Store.named"),
            (EntityKind::Method, "read"),
            (EntityKind::Function, "start"),
        ],
    );
}

#[test]
fn direct_imports_bind_only_visible_names_in_the_exact_library() {
    let fixture = ProjectFixture::new(
        [
            "lib/entry.dart",
            "lib/imported/api.dart",
            "lib/other/api.dart",
        ],
        [
            "import 'imported/api.dart' show selected;\nimport './imported/api.dart' as api hide hidden;\nvoid start() { selected(); blocked(); api.selected(); api.hidden(); api._secret(); api.member(); }\n",
            "void selected() {}\nvoid blocked() {}\nvoid hidden() {}\nvoid _secret() {}\nclass Other { void member() {} }\n",
            "void selected() {}\nvoid blocked() {}\nvoid hidden() {}\nvoid _secret() {}\nvoid member() {}\n",
        ],
        SemanticProjectLanguage::Dart,
    );
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let target = document
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "selected"
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == fixture.snapshots[1].file())
        })
        .expect("selected definition belongs to the imported library")
        .id;
    let calls = document
        .occurrences
        .iter()
        .filter(|occurrence| {
            occurrence.file == fixture.snapshots[0].file()
                && occurrence.role == OccurrenceRole::CallSite
        })
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 6);
    for call in calls {
        let span = call.source.span();
        let source = std::str::from_utf8(fixture.snapshots[0].content()).unwrap();
        let text = &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()];
        if matches!(text, "selected()" | "api.selected()") {
            assert_eq!(
                call.target,
                OccurrenceTarget::Resolved { symbol: target },
                "{text}"
            );
        } else {
            assert!(
                matches!(call.target, OccurrenceTarget::Unresolved { .. }),
                "{text}: {:?}",
                call.target
            );
        }
        assert_eq!(
            call.source.content_hash(),
            fixture.snapshots[0].content_hash()
        );
        assert_eq!(call.source.generation(), fixture.sources[0].generation());
    }
    assert!(
        document.skipped_regions.iter().any(
            |region| region.detail == "dart-project-inheritance-extension-dispatch-unavailable"
        )
    );
}

#[test]
fn local_declaration_shadows_imported_function() {
    let fixture = ProjectFixture::new(
        ["entry.dart", "library.dart"],
        [
            "import 'library.dart';\nvoid selected() {}\nvoid start() { selected(); }\n",
            "void selected() {}\n",
        ],
        SemanticProjectLanguage::Dart,
    );
    let output = analyze_with_real_parser(&fixture);
    let document = output.document();
    let target = document
        .entities
        .iter()
        .find(|entity| {
            entity.display_name == "selected"
                && entity
                    .evidence
                    .source
                    .as_ref()
                    .is_some_and(|source| source.span().file() == fixture.snapshots[0].file())
        })
        .unwrap()
        .id;
    let call = document
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .unwrap();
    assert_eq!(call.target, OccurrenceTarget::Resolved { symbol: target });
}

#[test]
fn conflicting_imports_remain_ambiguous_without_a_global_name_guess() {
    let fixture = ProjectFixture::new(
        ["entry.dart", "first.dart", "second.dart"],
        [
            "import 'first.dart';\nimport 'second.dart';\nvoid start() { selected(); }\n",
            "void selected() {}\n",
            "void selected() {}\n",
        ],
        SemanticProjectLanguage::Dart,
    );
    let output = analyze_with_real_parser(&fixture);
    let call = output
        .document()
        .occurrences
        .iter()
        .find(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .unwrap();
    assert!(
        matches!(&call.target, OccurrenceTarget::Candidates { symbols, .. } if symbols.len() == 2),
        "{:?}",
        call.target
    );
}

#[test]
fn prefixed_call_trivia_keeps_the_exact_written_source() {
    let source = "import /* outer /* inner */ end */ 'library.dart' as api show selected;\nvoid start() { api /* keep */ . selected(); }\n";
    let fixture = ProjectFixture::new(
        ["entry.dart", "library.dart"],
        [source, "void selected() {}\n"],
        SemanticProjectLanguage::Dart,
    );
    let output = analyze_with_real_parser(&fixture);
    let calls = output
        .document()
        .occurrences
        .iter()
        .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
        .collect::<Vec<_>>();
    assert_eq!(calls.len(), 1);
    assert!(matches!(calls[0].target, OccurrenceTarget::Resolved { .. }));
    let span = calls[0].source.span();
    assert_eq!(
        &source[usize::try_from(span.start_byte()).unwrap()
            ..usize::try_from(span.end_byte()).unwrap()],
        "api /* keep */ . selected()"
    );
}

#[test]
fn unsupported_directives_and_absent_libraries_preserve_unresolved_calls() {
    for directive in [
        "export 'library.dart';",
        "import 'missing.dart';",
        "import 'package:unknown/library.dart';",
        "import 'library.dart' if (dart.library.io) 'alternate.dart';",
    ] {
        let source = format!("{directive}\nvoid start() {{ selected(); }}\n");
        let fixture = ProjectFixture::new(
            ["entry.dart", "library.dart"],
            [&source, "void selected() {}\n"],
            SemanticProjectLanguage::Dart,
        );
        let output = analyze_with_real_parser(&fixture);
        let calls = output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .collect::<Vec<_>>();
        assert_eq!(calls.len(), 1, "{directive}");
        assert!(
            matches!(calls[0].target, OccurrenceTarget::Unresolved { .. }),
            "{directive}"
        );
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|region| matches!(
                    region.detail.as_str(),
                    "dart-library-directive-resolution-unavailable"
                        | "dart-library-uri-target-unavailable"
                )),
            "{directive}"
        );
    }
}

#[test]
fn local_parameters_do_not_bind_to_library_prefixes_or_functions() {
    for body in [
        "void start(dynamic api) { api.selected(); }",
        "void start(void Function() selected) { selected(); }",
    ] {
        let source = format!("import 'library.dart';\nimport 'library.dart' as api;\n{body}\n");
        let fixture = ProjectFixture::new(
            ["entry.dart", "library.dart"],
            [&source, "void selected() {}\n"],
            SemanticProjectLanguage::Dart,
        );
        let output = analyze_with_real_parser(&fixture);
        let call = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .unwrap();
        assert!(
            matches!(call.target, OccurrenceTarget::Unresolved { .. }),
            "{body}: {:?}",
            call.target
        );
    }
}
