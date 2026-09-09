//! Native module-chain binding regressions using only synthetic source fixtures.
//! Resolution must reach the authored definition, preserve source evidence and
//! terminate through cycles without guessing missing or ambiguous exports.

use super::*;

#[test]
fn namespace_export_members_resolve_through_authored_module_chains() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        for (facade, source, target_name) in [
            (
                "export * as Space from './bridge';",
                "import {Space as Local} from './facade'; function run() { Local.Public(); }",
                "Actual",
            ),
            (
                "export * as Space from './bridge';",
                "import * as Facade from './facade'; function run() { Facade.Space.Public(); }",
                "Actual",
            ),
            (
                "export * as 'public-space' from './bridge';",
                "import {'public-space' as Local} from './facade'; function run() { Local.Public(); }",
                "Actual",
            ),
            (
                "export * as default from './bridge';",
                "import Local from './facade'; function run() { Local.Public(); }",
                "Actual",
            ),
            (
                "export * as Space from './provider';",
                "import {Space as Local} from './facade'; function run() { Local.default(); }",
                "Hidden",
            ),
            (
                "import * as Inner from './bridge'; export {Inner as Space};",
                "import {Space as Local} from './facade'; function run() { Local.Public(); }",
                "Actual",
            ),
            (
                "import * as Inner from './bridge'; export {Inner as default};",
                "import Local from './facade'; function run() { Local.Public(); }",
                "Actual",
            ),
        ] {
            let fixture = ProjectFixture::new(
                [
                    "src/main.ts",
                    "src/facade.ts",
                    "src/bridge.ts",
                    "src/provider.ts",
                ],
                [
                    source,
                    facade,
                    "export {Actual as Public} from './provider';",
                    "export function Actual() {} export default function Hidden() {} function Public() {}",
                ],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            let target = output
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function
                        && entity.canonical_name == target_name
                        && entity.evidence.source.as_ref().unwrap().span().file()
                            == fixture.snapshots[3].file()
                })
                .unwrap();
            let call = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.role == OccurrenceRole::CallSite
                })
                .unwrap();
            assert_eq!(
                call.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{language:?}: {facade}; {source}"
            );
            assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
            assert!(
                output
                    .document()
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::Calls
                        && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id))
            );
        }
    }
}

#[test]
fn namespace_export_failures_cannot_bind_private_missing_or_type_only_members() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        for (facade, source) in [
            (
                "export * as Space from './provider';",
                "import {Space as Local} from './facade'; function run() { Local.Private(); }",
            ),
            (
                "export * as Space from './absent';",
                "import {Space as Local} from './facade'; function run() { Local.Actual(); }",
            ),
            (
                "export * as Space from './provider';",
                "import {Space as Local} from './facade'; function run() { Local(); }",
            ),
            (
                "export * as Space from './provider';",
                "import * as Local from './facade'; function run() { Local.Space(); }",
            ),
            (
                "export * as Space from './provider';",
                "import {Space as Local} from './facade'; function run() { Local.Actual.Missing(); }",
            ),
            (
                "export type * as Space from './provider';",
                "import {Space as Local} from './facade'; function run() { Local.Actual(); }",
            ),
            (
                "export * as Space from './provider';",
                "import type {Space as Local} from './facade'; function run() { Local.Actual(); }",
            ),
            (
                "export type * as Space from './provider';",
                "import * as Local from './facade'; function run() { Local.Space.Actual(); }",
            ),
        ] {
            if language == SemanticProjectLanguage::JavaScript
                && (facade.contains("type ") || source.contains("type "))
            {
                continue;
            }
            let fixture = ProjectFixture::new(
                ["src/main.ts", "src/facade.ts", "src/provider.ts"],
                [
                    source,
                    facade,
                    "export function Actual() {} function Private() {} class Decoy { Missing() {} }",
                ],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            let call = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.role == OccurrenceRole::CallSite
                })
                .unwrap();
            assert!(
                matches!(call.target, OccurrenceTarget::Unresolved { .. }),
                "{language:?}: {facade}; {source}"
            );
            assert!(
                output
                    .document()
                    .skipped_regions
                    .iter()
                    .any(|gap| gap.source.span() == call.source.span()
                        && gap.source.content_hash() == content_hash(source.as_bytes())),
                "missing source-scoped gap: {facade}; {source}"
            );
        }
    }
}

#[test]
fn quoted_namespace_aliases_retain_public_binding_source_occurrences() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        for facade in [
            "import * as Inner from './provider'; export {Inner as 'public-space'};",
            "export {'inner-space' as 'public-space'} from './bridge';",
            "export * as 'public-space' from './provider';",
        ] {
            let source = "import {'public-space' as Local} from './facade'; const value = Local;";
            let fixture = ProjectFixture::new(
                [
                    "src/main.ts",
                    "src/facade.ts",
                    "src/bridge.ts",
                    "src/provider.ts",
                ],
                [
                    source,
                    facade,
                    "export * as 'inner-space' from './provider';",
                    "export const Item = 1;",
                ],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            let module = output
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Module
                        && entity.evidence.source.as_ref().unwrap().span().file()
                            == fixture.snapshots[3].file()
                })
                .unwrap();
            let start = u64::try_from(facade.find("'public-space'").unwrap()).unwrap();
            let reference = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[1].file()
                        && occurrence.source.span().start_byte() == start
                        && occurrence.role == OccurrenceRole::Reference
                })
                .expect("public namespace name reference");
            assert_eq!(
                reference.target,
                OccurrenceTarget::Resolved { symbol: module.id },
                "{facade}"
            );
            assert_eq!(reference.source.span().end_byte(), start + 14);
            assert_eq!(
                reference.source.content_hash(),
                content_hash(facade.as_bytes())
            );
        }
    }
}

#[test]
fn namespace_member_paths_cross_cycles_without_changing_identity() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        for expression in ["Local.Public()", "Local.Back.Space.Back.Space.Public()"] {
            let source = format!(
                "import {{Space as Local}} from './facade'; function run() {{ {expression}; }}"
            );
            let fixture = ProjectFixture::new(
                [
                    "src/main.ts",
                    "src/facade.ts",
                    "src/bridge.ts",
                    "src/provider.ts",
                ],
                [
                    source.as_str(),
                    "export * as Space from './bridge';",
                    "export * as Back from './facade'; export {Actual as Public} from './provider';",
                    "export function Actual() {}",
                ],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            let target = output
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function
                        && entity.canonical_name == "Actual"
                        && entity.evidence.source.as_ref().unwrap().span().file()
                            == fixture.snapshots[3].file()
                })
                .unwrap();
            let call = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.role == OccurrenceRole::CallSite
                })
                .unwrap();
            assert_eq!(
                call.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{source}"
            );
            assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
        }
    }
}

#[test]
fn conflicting_namespace_origins_remain_unresolved() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        let source = "import {Space as Local} from './facade'; const value = Local;";
        let fixture = ProjectFixture::new(
            [
                "src/main.ts",
                "src/facade.ts",
                "src/provider.ts",
                "src/alternative.ts",
            ],
            [
                source,
                "export * as Space from './provider'; export * as Space from './alternative';",
                "export const Item = 1;",
                "export const Item = 2;",
            ],
            language,
        );
        let output = analyze_with_real_parser(&fixture);
        let start = u64::try_from(source.rfind("Local;").unwrap()).unwrap();
        let reference = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.source.span().start_byte() == start
            })
            .unwrap();
        assert!(matches!(
            reference.target,
            OccurrenceTarget::Unresolved { .. }
        ));
        assert!(
            output
                .document()
                .skipped_regions
                .iter()
                .any(|gap| gap.detail == "ecmascript-reexport-ambiguous")
        );
    }
}

#[test]
fn namespace_exports_do_not_bind_to_same_named_star_functions() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        for facade in [
            "export * as Actual from './provider'; export * from './alternative';",
            "export * as 'Actual' from './provider'; export * from './alternative';",
        ] {
            let source = "import {Actual as Local} from './facade'; const value = Local;";
            let fixture = ProjectFixture::new(
                [
                    "src/main.ts",
                    "src/facade.ts",
                    "src/provider.ts",
                    "src/alternative.ts",
                ],
                [
                    source,
                    facade,
                    "export const Item = 1;",
                    "export function Actual() {}",
                ],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            let start = u64::try_from(source.rfind("Local;").unwrap()).unwrap();
            let reference = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.source.span().start_byte() == start
                })
                .unwrap();
            let module = output
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Module
                        && entity.evidence.source.as_ref().unwrap().span().file()
                            == fixture.snapshots[2].file()
                })
                .unwrap();
            assert_eq!(
                reference.target,
                OccurrenceTarget::Resolved { symbol: module.id }
            );
            assert!(module.flags.contains(&rootlight_ir::EntityFlag::Synthetic));
            assert!(
                output
                    .document()
                    .occurrences
                    .iter()
                    .any(|occurrence| occurrence.file == fixture.snapshots[1].file()
                        && occurrence.role == OccurrenceRole::Reference
                        && occurrence.target == OccurrenceTarget::Resolved { symbol: module.id }),
                "{facade}: {:?}",
                output
                    .document()
                    .occurrences
                    .iter()
                    .filter(|occurrence| occurrence.file == fixture.snapshots[1].file())
                    .collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn namespace_reexports_decode_foreign_names_and_module_literals() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        let source = "import * as Space from './facade'; function run() { Space.Public(); }";
        let fixture = ProjectFixture::new(
            ["src/main.ts", "src/facade.ts", "src/provider.ts"],
            [
                source,
                "export {'public-name' as Public} from './pro\\u0076ider';",
                "function Actual() {} export {Actual as 'public-name'};",
            ],
            language,
        );
        let output = analyze_with_real_parser(&fixture);
        let target = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Function && entity.canonical_name == "Actual")
            .unwrap();
        let call = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.role == OccurrenceRole::CallSite
            })
            .unwrap();
        assert_eq!(
            call.target,
            OccurrenceTarget::Resolved { symbol: target.id }
        );
        assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
    }
}

#[test]
fn native_reexport_cycles_diamonds_and_overrides_preserve_binding_identity() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        for (bridge, facade, name, resolved) in [
            (
                "export * from './facade'; export * from './provider';",
                "export * from './bridge';",
                "Actual",
                true,
            ),
            (
                "export * from './facade';",
                "export * from './bridge';",
                "Actual",
                false,
            ),
            (
                "export * from './provider';",
                "export * from './bridge'; export * from './provider';",
                "Actual",
                true,
            ),
            (
                "export * from './provider';",
                "export * from './bridge'; export * from './alternative';",
                "Actual",
                false,
            ),
            (
                "export * from './alternative';",
                "export * from './bridge'; export {Actual} from './provider';",
                "Actual",
                true,
            ),
            (
                "export * from './provider';",
                "export * from './bridge';",
                "default",
                false,
            ),
            (
                "export * from './provider';",
                "export * from './bridge'; export * from './absent';",
                "Actual",
                false,
            ),
            (
                "export * from './provider';",
                "export {Missing as Actual} from './provider'; export * from './alternative';",
                "Actual",
                false,
            ),
        ] {
            let source = format!(
                "import {{{name} as Local}} from './facade'; function run() {{ Local(); }}"
            );
            let fixture = ProjectFixture::new(
                [
                    "src/main.ts",
                    "src/facade.ts",
                    "src/bridge.ts",
                    "src/provider.ts",
                    "src/alternative.ts",
                ],
                [
                    source.as_str(),
                    facade,
                    bridge,
                    "export function Actual() {} export default function Hidden() {}",
                    "export function Actual() {}",
                ],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            let call = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.role == OccurrenceRole::CallSite
                })
                .unwrap();
            if resolved {
                let target = output
                    .document()
                    .entities
                    .iter()
                    .find(|entity| {
                        entity.kind == EntityKind::Function
                            && entity.canonical_name == "Actual"
                            && entity.evidence.source.as_ref().unwrap().span().file()
                                == fixture.snapshots[3].file()
                    })
                    .unwrap();
                assert_eq!(
                    call.target,
                    OccurrenceTarget::Resolved { symbol: target.id },
                    "{language:?}: {facade}"
                );
            } else {
                assert!(
                    matches!(call.target, OccurrenceTarget::Unresolved { .. }),
                    "{language:?}: {facade}"
                );
                assert!(
                    output
                        .document()
                        .skipped_regions
                        .iter()
                        .any(|gap| gap.detail.starts_with("ecmascript-reexport-")
                            && gap.source.content_hash() == content_hash(source.as_bytes()))
                );
            }
        }
    }
}

#[test]
fn typescript_reexport_type_only_edges_cannot_be_widened_downstream() {
    for (bridge, imported) in [
        (
            "export type {Actual as Middle} from './provider';",
            "Middle",
        ),
        (
            "export {type Actual as Middle} from './provider';",
            "Middle",
        ),
        (
            "import type {Actual as Local} from './provider'; export {Local as Middle};",
            "Middle",
        ),
        ("export type * from './provider';", "Actual"),
        ("export\r\ntype\r\n* from './provider';", "Actual"),
    ] {
        let facade = format!("export {{{imported} as Public}} from './bridge';");
        let source =
            "import {Public as Local} from './facade'; let item: Local; const invalid = Local;";
        let fixture = ProjectFixture::new(
            [
                "src/main.ts",
                "src/facade.ts",
                "src/bridge.ts",
                "src/provider.ts",
            ],
            [source, facade.as_str(), bridge, "export class Actual {}"],
            SemanticProjectLanguage::TypeScript,
        );
        let output = analyze_with_real_parser(&fixture);
        let target = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Actual")
            .unwrap();
        for (needle, resolved) in [(": Local;", true), ("= Local;", false)] {
            let start = u64::try_from(source.find(needle).unwrap() + 2).unwrap();
            let reference = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.source.span().start_byte() == start
                })
                .unwrap();
            if resolved {
                assert_eq!(
                    reference.target,
                    OccurrenceTarget::Resolved { symbol: target.id },
                    "{bridge}"
                );
            } else {
                assert!(
                    matches!(reference.target, OccurrenceTarget::Unresolved { .. }),
                    "{bridge}"
                );
            }
        }
    }
}

#[test]
fn native_reexport_chains_reach_the_original_declaration() {
    for language in [
        SemanticProjectLanguage::JavaScript,
        SemanticProjectLanguage::TypeScript,
    ] {
        for (bridge, facade, name) in [
            (
                "export {Actual as Middle} from './provider';",
                "export {Middle as Public} from './bridge';",
                "Public",
            ),
            (
                "import {Actual as Local} from './provider'; export {Local as Middle};",
                "export {Middle as Public} from './bridge';",
                "Public",
            ),
            (
                "export * from './provider';",
                "export * from './bridge';",
                "Actual",
            ),
            (
                "export {Actual as default} from './provider';",
                "export {default as Public} from './bridge';",
                "Public",
            ),
            (
                "export {Actual as default} from './provider';",
                "import Local from './bridge'; export {Local as Public};",
                "Public",
            ),
            (
                "export {Actual as default} from './provider';",
                "import Local from './bridge'; export {Local as default};",
                "default",
            ),
        ] {
            let source = format!(
                "import {{{name} as Local}} from './facade'; function run() {{ Local(); }}"
            );
            let fixture = ProjectFixture::new(
                [
                    "src/main.ts",
                    "src/facade.ts",
                    "src/bridge.ts",
                    "src/provider.ts",
                ],
                [
                    source.as_str(),
                    facade,
                    bridge,
                    "export function Actual() {}",
                ],
                language,
            );
            let output = analyze_with_real_parser(&fixture);
            let target = output
                .document()
                .entities
                .iter()
                .find(|entity| {
                    entity.kind == EntityKind::Function && entity.canonical_name == "Actual"
                })
                .unwrap();
            let call = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.role == OccurrenceRole::CallSite
                })
                .unwrap();
            assert_eq!(
                call.target,
                OccurrenceTarget::Resolved { symbol: target.id },
                "{language:?}: {bridge}; {facade}"
            );
            assert_eq!(call.source.content_hash(), content_hash(source.as_bytes()));
            assert!(
                output
                    .document()
                    .relations
                    .iter()
                    .any(|relation| relation.predicate == RelationPredicate::Calls
                        && relation.object == rootlight_ir::RelationEndpoint::Entity(target.id))
            );
        }
    }
}
