//! ECMAScript identifier spelling must not change binding identity.
//! Native source spans retain authored escapes while local shadowing and module
//! boundaries use decoded names without Unicode normalization.

use super::*;

fn assert_bounded_call_relation(
    output: &rootlight_adapter_sdk::ProjectAnalysisOutput,
    call: &rootlight_ir::OccurrenceRecord,
    target: rootlight_ids::SymbolId,
) {
    assert!(
        output
            .document()
            .relations
            .iter()
            .any(|relation| relation.subject
                == rootlight_ir::RelationEndpoint::Occurrence(call.id)
                && relation.predicate == RelationPredicate::Calls
                && relation.object == rootlight_ir::RelationEndpoint::Entity(target))
    );
    assert!(
        output
            .document()
            .occurrences
            .iter()
            .filter(|occurrence| occurrence.role == OccurrenceRole::CallSite)
            .count()
            <= 256
    );
}

#[test]
fn bounded_analysis_preserves_imported_calls_independently_of_identifier_spelling() {
    for (import, call) in [
        ("import {Run} from './provider';", "Run()"),
        ("import {Run} from './provider';", r"\u0052un()"),
        (r"import {Run as \u004cocal} from './provider';", "Local()"),
        (
            "import * as Space from './provider';",
            r"\u0053pace.\u0052un()",
        ),
        (
            r"import * as \u0053pace from './provider';",
            "Space /* receiver */ . Run()",
        ),
    ] {
        for language in [
            SemanticProjectLanguage::JavaScript,
            SemanticProjectLanguage::TypeScript,
        ] {
            let mut source = format!("{import}\nfunction check() {{\n");
            for index in 0..320 {
                source.push_str(&format!("unknown_{index}();\n"));
            }
            source.push_str(&format!("{call};\n}}"));
            let fixture = ProjectFixture::new(
                ["main.ts", "provider.ts"],
                [source.as_str(), "export function Run() {}"],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            assert!(
                output
                    .document()
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "project-syntax-fact-limit"
                        && diagnostic.coverage_effect == CoverageStatus::Bounded)
            );
            let target = output
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Run" && entity.kind == EntityKind::Function
                })
                .unwrap();
            let reference = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.role == OccurrenceRole::CallSite
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: target.id }
                })
                .unwrap_or_else(|| panic!("lost imported call: {language:?}: {import} / {call}"));
            assert_bounded_call_relation(&output, reference, target.id);
            assert_eq!(
                reference.source.content_hash(),
                content_hash(source.as_bytes())
            );
        }
    }
}

#[test]
fn bounded_analysis_preserves_nonrecursive_local_calls_with_escaped_names() {
    for (definition, call) in [
        (r"\u0052un", "Run"),
        ("Run", r"\u0052un"),
        (r"R\u0075n", r"\u0052un"),
    ] {
        for language in [
            SemanticProjectLanguage::JavaScript,
            SemanticProjectLanguage::TypeScript,
        ] {
            let mut source =
                format!("function {definition}() {{ {call}(); }}\nfunction check() {{\n");
            for index in 0..320 {
                source.push_str(&format!("unknown_{index}();\n"));
            }
            let call_start = u64::try_from(source.len()).unwrap();
            source.push_str(&format!("{call}();\n}}"));
            let fixture = ProjectFixture::new(["main.ts"], [source.as_str()], language);
            let output = analyze_with_real_parser(&fixture);
            assert!(
                output
                    .document()
                    .diagnostics
                    .iter()
                    .any(|diagnostic| diagnostic.code == "project-syntax-fact-limit")
            );
            let target = output
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Run" && entity.kind == EntityKind::Function
                })
                .unwrap();
            let reference = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::CallSite
                        && occurrence.source.span().start_byte() == call_start
                })
                .unwrap_or_else(|| {
                    panic!("lost nonrecursive call: {language:?}: {definition} / {call}")
                });
            assert_bounded_call_relation(&output, reference, target.id);
            assert_eq!(
                reference.target,
                OccurrenceTarget::Resolved { symbol: target.id }
            );
            assert_eq!(
                reference.source.content_hash(),
                content_hash(source.as_bytes())
            );
        }
    }
}

#[test]
fn bounded_namespace_calls_retain_each_member_of_the_same_import() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        let mut source = "import * as Space from './provider'; function check() {\n".to_owned();
        for index in 0..320 {
            source.push_str(&format!("unknown_{index}();\n"));
        }
        source.push_str(r"\u0053pace.Run(); Space.Other(); Space.\u0052un(); }");
        let fixture = ProjectFixture::new(
            ["main.ts", "provider.ts"],
            [
                source.as_str(),
                "export function Run() {} export function Other() {}",
            ],
            language,
        );
        let output = analyze_with_real_parser(&fixture);
        assert!(
            output
                .document()
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "project-syntax-fact-limit")
        );
        for name in ["Run", "Other"] {
            let target = output
                .document()
                .entities
                .iter()
                .find(|entity| entity.canonical_name == name && entity.kind == EntityKind::Function)
                .unwrap();
            let reference = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.role == OccurrenceRole::CallSite
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: target.id }
                })
                .unwrap_or_else(|| panic!("lost member {name} in {language:?}"));
            assert_bounded_call_relation(&output, reference, target.id);
            assert_eq!(
                reference.source.content_hash(),
                content_hash(source.as_bytes())
            );
        }
    }
}

#[test]
fn escaped_local_declarations_and_references_keep_the_exact_binding() {
    for body in [
        r"function run( /*bind*/ \u004cocal) { return /*use*/ Local; }",
        r"function run( /*bind*/ Local) { return /*use*/ \u004cocal; }",
        r"function run() { const /*bind*/ L\u{6f}cal = 1; return /*use*/ Local; }",
        r"function run() { let /*bind*/ Local = 1; return /*use*/ L\u{0000006f}cal; }",
        r"function run() { function /*bind*/ \u004cocal() {} return /*use*/ Local(); }",
        r"function run() { class /*bind*/ \u004cocal {} return /*use*/ Local; }",
        r"function run() { try {} catch ( /*bind*/ \u004cocal) { return /*use*/ Local; } }",
        r"function run({value: /*bind*/ \u004cocal}) { return /*use*/ Local; }",
    ] {
        for language in [
            SemanticProjectLanguage::JavaScript,
            SemanticProjectLanguage::TypeScript,
        ] {
            let source = format!("import {{Public as Local}} from './provider'; {body}");
            let fixture = ProjectFixture::new(
                ["main.ts", "provider.ts"],
                [source.as_str(), "export class Public {}"],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            let bind =
                u64::try_from(source.find("/*bind*/ ").unwrap() + "/*bind*/ ".len()).unwrap();
            let start = u64::try_from(source.find("/*use*/ ").unwrap() + "/*use*/ ".len()).unwrap();
            let definition = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.role == OccurrenceRole::Definition
                        && occurrence.source.span().start_byte() == bind
                })
                .unwrap_or_else(|| panic!("missing definition: {body}"));
            let reference = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.source.span().start_byte() == start
                        && matches!(
                            occurrence.role,
                            OccurrenceRole::Reference | OccurrenceRole::CallSite
                        )
                })
                .unwrap_or_else(|| panic!("missing reference: {body}"));
            assert_eq!(reference.target, definition.target, "{language:?}: {body}");
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("unresolved binder")
            };
            assert!(
                output
                    .document()
                    .entities
                    .iter()
                    .any(|entity| entity.id == symbol && entity.canonical_name == "Local")
            );
            assert_eq!(
                reference.source.content_hash(),
                content_hash(source.as_bytes())
            );
        }
    }
}

#[test]
fn escaped_module_bindings_resolve_through_exports_and_reexports() {
    for (source, bridge, provider) in [
        (
            r"import {P\u0075blic as \u004cocal} from './bridge'; /*use*/ Local();",
            "export {Public} from './provider';",
            "export function Public() {}",
        ),
        (
            r"import {Public as Local} from './bridge'; /*use*/ \u004cocal();",
            r"export {\u0050ublic} from './provider';",
            r"export function \u0050ublic() {}",
        ),
        (
            r"import {\u0041lias as Local} from './bridge'; /*use*/ Local();",
            r"export {P\u0075blic as \u0041lias} from './provider';",
            "export function Public() {}",
        ),
        (
            r"import \u004cocal from './bridge'; /*use*/ Local();",
            "export {default} from './provider';",
            "export default function Public() {}",
        ),
        (
            r"import * as \u0053pace from './bridge'; Space./*use*/ \u0050ublic();",
            "export * from './provider';",
            "export function Public() {}",
        ),
        (
            r"import * as Space from './bridge'; \u0053pace./*use*/ Public;",
            "export * from './provider';",
            "export function Public() {}",
        ),
        (
            r"import {Alias as Local} from './bridge'; /*use*/ Local();",
            r"import {Public as \u004cocal} from './provider'; export {\u004cocal as \u0041lias};",
            "export function Public() {}",
        ),
        (
            r"import {\u0041lias as Local} from './bridge'; /*use*/ Local();",
            "export * from './provider';",
            r"function \u0050ublic() {} export {P\u0075blic as \u0041lias};",
        ),
    ] {
        for language in [
            SemanticProjectLanguage::JavaScript,
            SemanticProjectLanguage::TypeScript,
        ] {
            let fixture = ProjectFixture::new(
                ["main.ts", "bridge.ts", "provider.ts"],
                [source, bridge, provider],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            let target = output
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Public"
                        && entity.evidence.source.as_ref().is_some_and(|source| {
                            source.span().file() == fixture.snapshots[2].file()
                        })
                })
                .unwrap();
            let start = u64::try_from(source.find("/*use*/ ").unwrap() + "/*use*/ ".len()).unwrap();
            let reference = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.source.span().start_byte() == start
                        && matches!(
                            occurrence.role,
                            OccurrenceRole::Reference | OccurrenceRole::CallSite
                        )
                })
                .unwrap_or_else(|| panic!("missing reference: {source}"));
            assert_eq!(
                reference.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{language:?}: {source} / {bridge} / {provider}"
            );
            assert_eq!(
                reference.source.content_hash(),
                content_hash(source.as_bytes())
            );
        }
    }
}

#[test]
fn unicode_spelling_keeps_distinct_local_identities() {
    for (left, right, reference, expected) in [
        (r"\u00e9", r"e\u0301", "é", "é"),
        (r"\u00e9", r"e\u0301", r"e\u{301}", "e\u{301}"),
        ("a", r"a\u200c", r"a\u{200c}", "a\u{200c}"),
        ("a", r"a\u200d", "a", "a"),
        ("A", r"\u{1d49c}", "𝒜", "𝒜"),
    ] {
        for language in [
            SemanticProjectLanguage::JavaScript,
            SemanticProjectLanguage::TypeScript,
        ] {
            let source = format!(
                "function run() {{ const {left} = 1; const {right} = 2; return /*use*/ {reference}; }}"
            );
            let fixture = ProjectFixture::new(["main.ts"], [source.as_str()], language);
            let output = analyze_with_real_parser(&fixture);
            let locals: Vec<_> = output
                .document()
                .entities
                .iter()
                .filter(|entity| entity.kind == EntityKind::Variable)
                .collect();
            assert_eq!(locals.len(), 2, "{source}");
            assert_ne!(locals[0].id, locals[1].id);
            let target = locals
                .iter()
                .find(|entity| entity.canonical_name == expected)
                .unwrap();
            let start = u64::try_from(source.find("/*use*/ ").unwrap() + "/*use*/ ".len()).unwrap();
            let reference = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.source.span().start_byte() == start
                        && occurrence.role == OccurrenceRole::Reference
                })
                .unwrap();
            assert_eq!(
                reference.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{source}"
            );
            assert_eq!(
                reference.source.content_hash(),
                content_hash(source.as_bytes())
            );
        }
    }
}

#[test]
fn escaped_typescript_binders_shadow_only_the_matching_type_scope() {
    for body in [
        r"type Box< /*bind*/ \u004cocal> = /*use*/ Local;",
        r"type Box = { [ /*bind*/ \u004cocal in 'a']: /*use*/ Local };",
        r"type Box<T> = T extends infer /*bind*/ \u004cocal ? /*use*/ Local : never;",
    ] {
        let source = format!("import {{Public as Local}} from './provider'; {body}");
        let fixture = ProjectFixture::new(
            ["main.ts", "provider.ts"],
            [source.as_str(), "export class Public {}"],
            SemanticProjectLanguage::TypeScript,
        );
        let output = analyze_with_real_parser(&fixture);
        let bind = u64::try_from(source.find("/*bind*/ ").unwrap() + "/*bind*/ ".len()).unwrap();
        let start = u64::try_from(source.find("/*use*/ ").unwrap() + "/*use*/ ".len()).unwrap();
        let definition = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == bind
            })
            .unwrap();
        let reference = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.role == OccurrenceRole::TypeUse
                    && occurrence.source.span().start_byte() == start
            })
            .unwrap();
        assert_eq!(reference.target, definition.target, "{body}");
        assert!(matches!(
            definition.target,
            OccurrenceTarget::Resolved { .. }
        ));
    }
}
