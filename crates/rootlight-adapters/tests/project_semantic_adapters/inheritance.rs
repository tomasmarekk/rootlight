//! Inheritance edges must identify declared bases, not generic constraints or
//! same-spelled types from unrelated modules.

use super::*;

#[test]
fn typescript_generic_constraints_do_not_become_inheritance() {
    let source = concat!(
        "class Constraint {}\n",
        "class Parent {}\n",
        "interface Contract {}\n",
        "function operate<T extends Constraint>(value: T) {}\n",
        "type Alias<T extends Constraint> = T;\n",
        "class Child<T extends Constraint> extends Parent implements Contract {}\n",
    );
    let fixture = ProjectFixture::new(["main.ts"], [source], SemanticProjectLanguage::TypeScript);
    let output = analyze_with_real_parser(&fixture);
    let entities = &output.document().entities;
    let child = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Child")
        .expect("child class is indexed");
    let parent = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Parent")
        .expect("parent class is indexed");
    let contract = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Interface && entity.canonical_name == "Contract")
        .expect("interface is indexed");
    let inheritance = output
        .document()
        .relations
        .iter()
        .filter(|relation| {
            matches!(
                relation.predicate,
                RelationPredicate::Extends | RelationPredicate::Implements
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(inheritance.len(), 2, "generic bounds are not inheritance");
    assert!(inheritance.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(child.id)
            && relation.predicate == RelationPredicate::Extends
            && relation.object == rootlight_ir::RelationEndpoint::Entity(parent.id)
    }));
    assert!(inheritance.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(child.id)
            && relation.predicate == RelationPredicate::Implements
            && relation.object == rootlight_ir::RelationEndpoint::Entity(contract.id)
    }));
}

#[test]
fn typescript_inheritance_requires_a_source_backed_import() {
    for (source, imported) in [
        ("class Child extends Base {}", false),
        (
            "import {Base as Alias} from './provider'; class Child extends Alias {}",
            true,
        ),
        (
            "import * as Namespace from './provider'; class Child extends Namespace.Base {}",
            true,
        ),
    ] {
        let fixture = ProjectFixture::new(
            ["main.ts", "provider.ts"],
            [source, "export class Base {}"],
            SemanticProjectLanguage::TypeScript,
        );
        let output = analyze_with_real_parser(&fixture);
        let child = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Child")
            .expect("child class is indexed");
        let provider = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Base")
            .expect("provider class is indexed");
        let bound = output.document().relations.iter().any(|relation| {
            relation.subject == rootlight_ir::RelationEndpoint::Entity(child.id)
                && relation.predicate == RelationPredicate::Extends
                && relation.object == rootlight_ir::RelationEndpoint::Entity(provider.id)
        });
        assert_eq!(bound, imported, "{source}");
    }
}

#[test]
fn python_function_parameters_do_not_become_base_classes() {
    let source = "class Parent:\n    pass\nclass Child(Parent):\n    pass\ndef helper(Parent):\n    return Parent\n";
    let fixture = ProjectFixture::new(["main.py"], [source], SemanticProjectLanguage::Python);
    let output = analyze_with_real_parser(&fixture);
    let entities = &output.document().entities;
    let child = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Child")
        .expect("child class is indexed");
    let parent = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Parent")
        .expect("parent class is indexed");
    let inheritance = output
        .document()
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Extends)
        .collect::<Vec<_>>();
    assert_eq!(inheritance.len(), 1);
    assert_eq!(
        inheritance[0].subject,
        rootlight_ir::RelationEndpoint::Entity(child.id)
    );
    assert_eq!(
        inheritance[0].object,
        rootlight_ir::RelationEndpoint::Entity(parent.id)
    );
}

#[test]
fn java_generic_bounds_do_not_replace_the_declared_base() {
    let source = concat!(
        "class Constraint {}\n",
        "class Parent {}\n",
        "class Child<T extends Constraint> extends Parent {}\n",
    );
    let fixture = ProjectFixture::new(["Main.java"], [source], SemanticProjectLanguage::Java);
    let output = analyze_with_real_parser(&fixture);
    let entities = &output.document().entities;
    let child = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Child")
        .expect("child class is indexed");
    let parent = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Parent")
        .expect("parent class is indexed");
    let inheritance = output
        .document()
        .relations
        .iter()
        .filter(|relation| relation.predicate == RelationPredicate::Extends)
        .collect::<Vec<_>>();
    assert_eq!(inheritance.len(), 1);
    assert_eq!(
        inheritance[0].subject,
        rootlight_ir::RelationEndpoint::Entity(child.id)
    );
    assert_eq!(
        inheritance[0].object,
        rootlight_ir::RelationEndpoint::Entity(parent.id)
    );
}

#[test]
fn object_shaped_generic_bounds_do_not_hide_a_later_base_class() {
    let source = "class Parent {}\nclass Child<T extends { value: number }> extends Parent {}\n";
    let fixture = ProjectFixture::new(["main.ts"], [source], SemanticProjectLanguage::TypeScript);
    let output = analyze_with_real_parser(&fixture);
    let entities = &output.document().entities;
    let child = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Child")
        .expect("child class is indexed");
    let parent = entities
        .iter()
        .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Parent")
        .expect("parent class is indexed");
    assert!(output.document().relations.iter().any(|relation| {
        relation.subject == rootlight_ir::RelationEndpoint::Entity(child.id)
            && relation.predicate == RelationPredicate::Extends
            && relation.object == rootlight_ir::RelationEndpoint::Entity(parent.id)
    }));
}

#[test]
fn javascript_inheritance_requires_an_exported_base() {
    for (provider_source, exported) in [("class Base {}", false), ("export class Base {}", true)] {
        let fixture = ProjectFixture::new(
            ["main.js", "provider.js"],
            [
                "import {Base} from './provider'; class Child extends Base {}",
                provider_source,
            ],
            SemanticProjectLanguage::JavaScript,
        );
        let output = analyze_with_real_parser(&fixture);
        let child = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Child")
            .expect("child class is indexed");
        let provider = output
            .document()
            .entities
            .iter()
            .find(|entity| entity.kind == EntityKind::Class && entity.canonical_name == "Base")
            .expect("provider class is indexed");
        let bound = output.document().relations.iter().any(|relation| {
            relation.subject == rootlight_ir::RelationEndpoint::Entity(child.id)
                && relation.predicate == RelationPredicate::Extends
                && relation.object == rootlight_ir::RelationEndpoint::Entity(provider.id)
        });
        assert_eq!(bound, exported, "{provider_source}");
    }
}
