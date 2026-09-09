//! Exact bindings inside anonymous signatures and mapped types.
//! Same-spelled binders in sibling or nested types must retain separate source
//! identities, while references outside their written scope keep outer targets.

use super::*;

#[test]
fn typescript_mapped_binders_resolve_without_imports() {
    let source = "type Box = { [Local in 'a']: Local } | { [Local in 'b']: Local };";
    let fixture = ProjectFixture::new(["main.ts"], [source], SemanticProjectLanguage::TypeScript);
    let output = analyze_with_real_parser(&fixture);
    let mut identities = BTreeSet::new();
    for (offset, _) in source.match_indices("[Local") {
        let definition_start = u64::try_from(offset + 1).unwrap();
        let reference_start =
            u64::try_from(offset + source[offset..].find(": Local").unwrap() + 2).unwrap();
        let definition = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::Definition
                    && occurrence.source.span().start_byte() == definition_start
            })
            .unwrap();
        let reference = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::TypeUse
                    && occurrence.source.span().start_byte() == reference_start
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = definition.target else {
            panic!("missing mapped binder")
        };
        assert!(
            output
                .document()
                .entities
                .iter()
                .any(|entity| entity.id == symbol && entity.kind == EntityKind::TypeParameter)
        );
        assert!(identities.insert(symbol));
        assert_eq!(definition.target, reference.target);
    }
    assert_eq!(identities.len(), 2);
}

#[test]
fn typescript_inner_type_bindings_keep_exact_source_targets() {
    for (body, local) in [
        (
            "type Box = (<Local>(value: Local) => Local) | (< /*bind*/ Local>(value: /*use*/ Local) => Local);",
            true,
        ),
        (
            "type Box = (< /*bind*/ Local>(value: /*use*/ Local) => Local) | (<Local>(value: Local) => Local);",
            true,
        ),
        (
            "type Box = <Local>(value: Local) => < /*bind*/ Local>(value: /*use*/ Local) => Local;",
            true,
        ),
        (
            "interface Box { <Local>(value: Local): Local; < /*bind*/ Local>(value: /*use*/ Local): Local; }",
            true,
        ),
        (
            "type Box = (new <Local>(value: Local) => object) | (new < /*bind*/ Local>(value: /*use*/ Local) => object);",
            true,
        ),
        (
            "type Box<T> = { [ /*bind*/ Local in keyof T]: T[ /*use*/ Local] };",
            true,
        ),
        (
            "type Box<T> = { [ /*bind*/ Local in keyof T as /*use*/ Local]: T[Local] };",
            true,
        ),
        (
            "type Box<T> = { readonly [ /*bind*/ Local in keyof T]?: /*use*/ Local };",
            true,
        ),
        (
            "type Box<T> = { -readonly [ /*bind*/ Local in keyof T]-?: /*use*/ Local };",
            true,
        ),
        (
            "type Box<T> = { [Local in keyof T]: Local } | { [ /*bind*/ Local in keyof T]: /*use*/ Local };",
            true,
        ),
        (
            "type Box<T> = { [ /*bind*/ Local in keyof T]: /*use*/ Local } | { [Local in keyof T]: Local };",
            true,
        ),
        (
            "type Box<T> = { [Local in keyof T]: { [ /*bind*/ Local in keyof T]: /*use*/ Local } };",
            true,
        ),
        (
            "type Box<T> = { [ /*bind*/ Local in keyof T]: { [Other in keyof T]: /*use*/ Local } };",
            true,
        ),
        (
            "type Box<T> = { [Local in keyof T]: Local }; let value: /*use*/ Local;",
            false,
        ),
        (
            "type Box<T> = { [Local in keyof T]: Local } | /*use*/ Local;",
            false,
        ),
        (
            "type Box<T> = { [Local in keyof T]: Local }; const value = /*use*/ Local;",
            false,
        ),
    ] {
        let source = format!("import {{Public as Local}} from './provider'; {body}");
        let fixture = ProjectFixture::new(
            ["main.ts", "provider.ts"],
            [source.as_str(), "export class Public {}"],
            SemanticProjectLanguage::TypeScript,
        );
        let output = analyze_with_real_parser(&fixture);
        let document = output.document();
        let start =
            u64::try_from(source.find("/*use*/ Local").unwrap() + "/*use*/ ".len()).unwrap();
        let reference = document
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.source.span().start_byte() == start
                    && matches!(
                        occurrence.role,
                        OccurrenceRole::TypeUse | OccurrenceRole::Reference
                    )
            })
            .unwrap_or_else(|| panic!("missing reference: {body}"));
        let target = if local {
            let bind =
                u64::try_from(source.find("/*bind*/ Local").unwrap() + "/*bind*/ ".len()).unwrap();
            let definition = document
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.source.span().start_byte() == bind
                        && occurrence.role == OccurrenceRole::Definition
                })
                .unwrap_or_else(|| panic!("missing binder: {body}"));
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("unresolved binder: {body}")
            };
            assert!(
                document
                    .entities
                    .iter()
                    .any(|entity| entity.id == symbol && entity.kind == EntityKind::TypeParameter)
            );
            definition.target.clone()
        } else {
            let entity = document
                .entities
                .iter()
                .find(|entity| {
                    entity.canonical_name == "Public" && entity.kind == EntityKind::Class
                })
                .unwrap();
            OccurrenceTarget::Resolved { symbol: entity.id }
        };
        assert_eq!(reference.target, target, "{body}");
        assert_eq!(reference.source.span().end_byte(), start + 5);
        assert_eq!(
            reference.source.content_hash(),
            content_hash(source.as_bytes())
        );
        let definitions: Vec<_> = document.occurrences.iter().filter(|occurrence| {
            occurrence.role == OccurrenceRole::Definition && matches!(occurrence.target, OccurrenceTarget::Resolved { symbol } if document.entities.iter().any(|entity| entity.id == symbol && entity.kind == EntityKind::TypeParameter && entity.canonical_name == "Local"))
        }).collect();
        let identities: BTreeSet<_> = definitions
            .iter()
            .map(|occurrence| match occurrence.target {
                OccurrenceTarget::Resolved { symbol } => symbol,
                _ => unreachable!(),
            })
            .collect();
        assert_eq!(
            identities.len(),
            definitions.len(),
            "distinct binders: {body}"
        );
    }
}
