//! Local runtime receivers must remain distinct from module namespace bindings.
//! Method-name candidates retain uncertainty; import shadows never become exact calls.

use super::*;

#[test]
fn local_receivers_preserve_zero_one_and_many_uncertain_method_candidates() {
    for (language, parameter) in [
        (SemanticProjectLanguage::JavaScript, "receiver"),
        (
            SemanticProjectLanguage::TypeScript,
            "receiver: Left | Right",
        ),
    ] {
        let source = format!(
            "import {{Left, Right}} from './library'; function run({parameter}) {{ receiver.absent(); receiver.single(); receiver.shared(); }}"
        );
        let library = "export class Left { single() {} shared() {} } export class Right { shared() {} } export function shared() {}";
        let fixture = ProjectFixture::new(
            ["src/main.ts", "src/library.ts"],
            [source.as_str(), library],
            language,
        );
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        for (name, expected) in [("absent", 0), ("single", 1), ("shared", 2)] {
            let call = document
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.role == OccurrenceRole::CallSite
                        && occurrence.syntactic_text_hash == content_hash(name.as_bytes())
                })
                .unwrap();
            assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
            assert!(!document.relations.iter().any(|relation| {
                relation.subject == rootlight_ir::RelationEndpoint::Occurrence(call.id)
                    && relation.predicate == RelationPredicate::Calls
            }));
            if expected == 0 {
                assert!(matches!(call.target, OccurrenceTarget::Unresolved { .. }));
                continue;
            }
            let OccurrenceTarget::Candidates {
                symbols,
                total_count,
                completeness,
            } = &call.target
            else {
                panic!(
                    "{language:?} {name}: method possibilities must remain uncertain: {:?}",
                    call.target
                );
            };
            assert_eq!(symbols.len(), expected);
            assert_eq!(*total_count, u64::try_from(expected).unwrap());
            assert_eq!(*completeness, CoverageStatus::Unknown);
            for symbol in symbols {
                let target = document
                    .entities
                    .iter()
                    .find(|entity| entity.id == *symbol)
                    .unwrap();
                assert_eq!(target.kind, EntityKind::Method);
                assert_eq!(
                    target.evidence.source.as_ref().unwrap().span().file(),
                    fixture.snapshots[1].file()
                );
                assert_eq!(
                    target.evidence.source.as_ref().unwrap().content_hash(),
                    content_hash(library.as_bytes())
                );
                assert!(document.relations.iter().any(|relation| {
                    relation.subject == rootlight_ir::RelationEndpoint::Occurrence(call.id)
                        && relation.object == rootlight_ir::RelationEndpoint::Entity(*symbol)
                        && relation.predicate == RelationPredicate::DispatchCandidate
                        && relation.evidence.source.as_ref() == Some(&call.source)
                }));
            }
            assert!(
                document
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span() == call.source.span())
            );
        }
    }
}

#[test]
fn shadowed_module_roots_never_rebind_to_imported_functions() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        for source in [
            "import * as receiver from './library'; function run(receiver) { receiver.shared(); }",
            "import {Left as receiver} from './library'; function run(receiver) { receiver.shared(); }",
            "import {Left} from './library'; function run(receiver) { return receiver.shared; }",
        ] {
            let fixture = ProjectFixture::new(
                ["src/main.ts", "src/library.ts"],
                [
                    source,
                    "export function shared() {} export class Left { shared() {} }",
                ],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            let start = u64::try_from(source.rfind("shared").unwrap()).unwrap();
            let occurrence = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.source.span().start_byte() == start
                })
                .unwrap();
            if occurrence.role == OccurrenceRole::CallSite {
                let OccurrenceTarget::Candidates {
                    symbols,
                    total_count: 1,
                    completeness: CoverageStatus::Unknown,
                } = &occurrence.target
                else {
                    panic!("a shadowed module root remains a runtime receiver: {occurrence:?}");
                };
                assert_eq!(symbols.len(), 1);
                assert!(
                    output
                        .document()
                        .entities
                        .iter()
                        .any(|entity| entity.id == symbols[0] && entity.kind == EntityKind::Method)
                );
                assert!(
                    !output
                        .document()
                        .relations
                        .iter()
                        .any(|relation| relation.subject
                            == rootlight_ir::RelationEndpoint::Occurrence(occurrence.id)
                            && relation.predicate == RelationPredicate::Calls)
                );
            } else {
                assert!(
                    matches!(occurrence.target, OccurrenceTarget::Unresolved { .. }),
                    "{source}: {occurrence:?}"
                );
            }
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span() == occurrence.source.span())
            );
        }
    }
}
