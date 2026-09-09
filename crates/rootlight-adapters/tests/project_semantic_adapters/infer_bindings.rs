//! Source-backed infer declarations and conditional branch visibility.
//! The owning extends operand may contain nested signatures or conditionals;
//! the true branch and a binder's own constraint expose the inferred binding.

use super::*;

#[test]
fn typescript_infer_binders_resolve_without_imports() {
    let source = "type Box = (string extends infer Local ? Local : never) | (number extends infer Local ? Local : never);";
    let fixture = ProjectFixture::new(["main.ts"], [source], SemanticProjectLanguage::TypeScript);
    let output = analyze_with_real_parser(&fixture);
    let mut identities = BTreeSet::new();
    for (offset, _) in source.match_indices("infer Local") {
        let definition_start = u64::try_from(offset + 6).unwrap();
        let reference_start =
            u64::try_from(offset + source[offset..].find("? Local").unwrap() + 2).unwrap();
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
            panic!("missing infer binder")
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
fn typescript_infer_bindings_follow_the_matching_conditional_branch() {
    for (body, local) in [
        (
            "type Box<T> = T extends infer /*bind*/ Local extends { value: /*use*/ Local } ? Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends [infer Other extends /*use*/ Local, infer Local] ? Other : never;",
            false,
        ),
        (
            "type Box<T> = T extends infer /*bind*/ Local extends Date ? /*use*/ Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends (value: infer Container extends (T extends infer /*bind*/ Local ? /*use*/ Local : never)) => unknown ? Container : never;",
            true,
        ),
        (
            "/*circular*/ type Box<T> = T extends (value: infer /*bind*/ Local extends (T extends infer Other ? /*use*/ Local : never)) => unknown ? Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends [infer /*bind*/ Local, infer /*bind*/ Local] ? /*use*/ Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends { method(value: infer /*bind*/ Local): infer /*bind*/ Local } ? /*use*/ Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends [infer /*bind*/ Local extends string, infer /*bind*/ Local extends string] ? /*use*/ Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends [infer /*bind*/ Local, () => infer /*bind*/ Local] ? /*use*/ Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends infer /*bind*/ Local ? /*use*/ Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends (infer /*bind*/ Local)[] ? /*use*/ Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends (...args: never[]) => infer /*bind*/ Local ? /*use*/ Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends infer /*bind*/ Local extends string ? /*use*/ Local : never;",
            true,
        ),
        (
            "type Box<T> = T extends infer Local ? Local : /*use*/ Local;",
            false,
        ),
        (
            "type Box<T> = /*use*/ Local extends infer Item ? Item : T;",
            false,
        ),
        (
            "type Box<T> = T extends [infer Local, /*use*/ Local] ? Local : never;",
            false,
        ),
        (
            "type Box<T> = T extends infer Local ? typeof /*use*/ Local : never;",
            false,
        ),
        (
            "type Box<T> = T extends infer /*bind*/ Local ? (T extends infer Other ? /*use*/ Local : never) : never;",
            true,
        ),
        (
            "type Box<T> = T extends infer Local ? (T extends infer /*bind*/ Local ? /*use*/ Local : never) : never;",
            true,
        ),
        (
            "type Box<T> = T extends infer /*bind*/ Local ? (T extends infer Local ? Local : /*use*/ Local) : never;",
            true,
        ),
        (
            "type Box<T> = T extends (T extends infer Local ? Local : never) ? /*use*/ Local : never;",
            false,
        ),
        (
            "type Box<T> = T extends (Array<infer /*bind*/ Local> extends infer Other ? Other : never) ? /*use*/ Local : never;",
            true,
        ),
        (
            "type Box<T> = (T extends infer Local ? Local : never) | (T extends infer /*bind*/ Local ? /*use*/ Local : never);",
            true,
        ),
        (
            "type Box<T> = T extends infer Local ? Local : never; let value: /*use*/ Local;",
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
        if body.matches("/*bind*/").count() > 1 {
            assert_eq!(
                definitions.len(),
                body.matches("/*bind*/").count(),
                "all repeated declarations: {body}"
            );
        }
        assert_eq!(
            identities.len(),
            if body.matches("/*bind*/").count() > 1 {
                1
            } else {
                definitions.len()
            },
            "distinct binders: {body}"
        );
    }
}
