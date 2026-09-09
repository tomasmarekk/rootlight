//! Source-backed type-parameter identity and lexical visibility.
//! Generic binders occupy only the type namespace and cannot leak into sibling
//! declarations or acquire the identity of a same-named module import.

use super::*;

#[test]
fn typescript_type_bindings_do_not_shadow_namespace_qualifiers() {
    for body in [
        "function run<Space>(value: Space.Public) {}",
        "function run() { type Space = string; let value: Space.Public; }",
        "function run() { class Space {} let value: Space.Public; }",
        "function run() { interface Space {} let value: Space.Public; }",
    ] {
        let source = format!("import * as Space from './provider'; {body}");
        let fixture = ProjectFixture::new(
            ["main.ts", "provider.ts"],
            [source.as_str(), "export class Public {}"],
            SemanticProjectLanguage::TypeScript,
        );
        let output = analyze_with_real_parser(&fixture);
        let start = u64::try_from(source.rfind("Public").unwrap()).unwrap();
        let reference = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.source.span().start_byte() == start
                    && occurrence.role == OccurrenceRole::TypeUse
            })
            .unwrap();
        let target = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Public")
            .unwrap();
        assert_eq!(
            reference.target,
            OccurrenceTarget::Resolved { symbol: target.id },
            "{body}"
        );
    }
}

#[test]
fn typescript_type_parameters_do_not_leak_or_shadow_runtime_values() {
    for (body, role, expected_local) in [
        (
            "function other<Local>() {} function run< /*bind*/ Local>(value: /*use*/ Local) {}",
            OccurrenceRole::TypeUse,
            true,
        ),
        (
            "function run< /*bind*/ Local>() { return /*use*/ Local; }",
            OccurrenceRole::Reference,
            false,
        ),
        (
            "function run< /*bind*/ Local>() { type Value = typeof /*use*/ Local; }",
            OccurrenceRole::Reference,
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
        let start =
            u64::try_from(source.find("/*use*/ Local").unwrap() + "/*use*/ ".len()).unwrap();
        let reference = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.role == role
                    && occurrence.source.span().start_byte() == start
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = reference.target else {
            panic!("missing exact target: {body}: {:?}", reference.target)
        };
        let entity = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(
            entity.kind,
            if expected_local {
                EntityKind::TypeParameter
            } else {
                EntityKind::Class
            },
            "{body}"
        );
        assert_eq!(
            entity.evidence.source.as_ref().unwrap().span().file(),
            fixture.snapshots[usize::from(!expected_local)].file()
        );
    }
}

#[test]
fn typescript_type_parameters_without_imports_stay_with_their_own_declaration() {
    let source = "function first<Local>(value: Local) {} function second<Local>(value: Local) {}";
    let fixture = ProjectFixture::new(["main.ts"], [source], SemanticProjectLanguage::TypeScript);
    let output = analyze_with_real_parser(&fixture);
    for (name, tail) in [("first", "function first"), ("second", "function second")] {
        let base = source.find(tail).unwrap();
        let start =
            u64::try_from(base + source[base..].find("value: Local").unwrap() + "value: ".len())
                .unwrap();
        let reference = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.role == OccurrenceRole::TypeUse
                    && occurrence.source.span().start_byte() == start
            })
            .unwrap();
        let OccurrenceTarget::Resolved { symbol } = reference.target else {
            panic!("missing local target: {name}")
        };
        let parameter = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.id == symbol)
            .unwrap();
        assert_eq!(parameter.kind, EntityKind::TypeParameter);
        let owner = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.canonical_name == name)
            .unwrap();
        assert_eq!(
            parameter.container,
            Some(rootlight_ir::ContainerRef::Entity(owner.id))
        );
    }
}

#[test]
fn typescript_type_parameters_bind_exactly_within_their_written_scope() {
    for (body, local) in [
        (
            "function run< /*bind*/ Local>(value: /*use*/ Local) {}",
            true,
        ),
        (
            "function* run< /*bind*/ Local>(value: /*use*/ Local) {}",
            true,
        ),
        (
            "const run = < /*bind*/ Local>(value: /*use*/ Local) => value;",
            true,
        ),
        (
            "class Box< /*bind*/ Local> { value!: /*use*/ Local; }",
            true,
        ),
        (
            "interface Box< /*bind*/ Local> { value: /*use*/ Local; }",
            true,
        ),
        ("type Box< /*bind*/ Local> = /*use*/ Local;", true),
        (
            "type Box = < /*bind*/ Local>(value: /*use*/ Local) => void;",
            true,
        ),
        (
            "type Box = new < /*bind*/ Local>(value: /*use*/ Local) => object;",
            true,
        ),
        (
            "interface Box { < /*bind*/ Local>(value: /*use*/ Local): void; }",
            true,
        ),
        (
            "class Box { run< /*bind*/ Local>(value: /*use*/ Local) {} }",
            true,
        ),
        (
            "function run< /*bind*/ Local, Other extends /*use*/ Local>() {}",
            true,
        ),
        (
            "type Box< /*bind*/ Local, Other = /*use*/ Local> = Other;",
            true,
        ),
        (
            "class Box< /*bind*/ Local> { run(value: /*use*/ Local) {} }",
            true,
        ),
        (
            "function run< /*bind*/ Local>() { function nested(value: /*use*/ Local) {} }",
            true,
        ),
        (
            "function other<Local>() {} let value: /*use*/ Local;",
            false,
        ),
        ("type Box<Local> = Local; let value: /*use*/ Local;", false),
        ("class Box<Local> {} let value: /*use*/ Local;", false),
        ("interface Box<Local> {} let value: /*use*/ Local;", false),
    ] {
        let source = format!("import {{Public as Local}} from './provider'; {body}");
        let fixture = ProjectFixture::new(
            ["main.ts", "provider.ts"],
            [source.as_str(), "export class Public {}"],
            SemanticProjectLanguage::TypeScript,
        );
        let output = analyze_with_real_parser(&fixture);
        let start =
            u64::try_from(source.find("/*use*/ Local").unwrap() + "/*use*/ ".len()).unwrap();
        let reference = output
            .document()
            .occurrences
            .iter()
            .find(|occurrence| {
                occurrence.file == fixture.snapshots[0].file()
                    && occurrence.source.span().start_byte() == start
                    && occurrence.role == OccurrenceRole::TypeUse
            })
            .unwrap();
        let target = if local {
            let definition_start =
                u64::try_from(source.find("/*bind*/ Local").unwrap() + "/*bind*/ ".len()).unwrap();
            let definition = output
                .document()
                .occurrences
                .iter()
                .find(|occurrence| {
                    occurrence.file == fixture.snapshots[0].file()
                        && occurrence.source.span().start_byte() == definition_start
                        && occurrence.role == OccurrenceRole::Definition
                })
                .unwrap_or_else(|| panic!("missing type parameter definition: {body}"));
            let OccurrenceTarget::Resolved { symbol } = definition.target else {
                panic!("unresolved binder: {body}")
            };
            assert!(
                output
                    .document()
                    .entities
                    .iter()
                    .any(|entity| entity.id == symbol && entity.kind == EntityKind::TypeParameter)
            );
            definition.target.clone()
        } else {
            let entity = output
                .document()
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
    }
}
