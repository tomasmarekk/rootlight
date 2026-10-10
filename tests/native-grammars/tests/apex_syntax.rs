//! Ordinary Apex native syntax witnesses for the pinned static language table.
//! Byte spans and direct written owners are checked independently of product
//! indexing, Salesforce compilation, overload dispatch, or query bindings.

use tree_sitter::{Node, Parser, Tree};

fn parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_sfapex::apex::LANGUAGE.into())
        .unwrap();
    parser.parse(source, None).unwrap()
}

fn nodes<'tree>(root: Node<'tree>, kind: &str) -> Vec<Node<'tree>> {
    let mut pending = vec![root];
    let mut found = Vec::new();
    while let Some(node) = pending.pop() {
        if node.kind() == kind {
            found.push(node);
        }
        let mut cursor = node.walk();
        pending.extend(node.named_children(&mut cursor));
    }
    found.sort_by_key(Node::start_byte);
    found
}

fn text<'source>(node: Node<'_>, source: &'source str) -> &'source str {
    node.utf8_text(source.as_bytes()).unwrap()
}

fn field<'tree>(node: Node<'tree>, name: &str) -> Node<'tree> {
    node.child_by_field_name(name)
        .unwrap_or_else(|| panic!("missing {name} field: {}", node.to_sexp()))
}

fn owner<'tree>(mut node: Node<'tree>, kind: &str) -> Option<Node<'tree>> {
    while let Some(parent) = node.parent() {
        if parent.kind() == kind {
            return Some(parent);
        }
        node = parent;
    }
    None
}

fn assert_exact_span(node: Node<'_>, source: &str, expected: &str) {
    let matches = source.match_indices(expected).collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "fixture witness must be unique");
    assert_eq!(node.start_byte(), matches[0].0);
    assert_eq!(node.end_byte(), matches[0].0 + expected.len());
    assert_eq!(text(node, source), expected);
}

fn assert_name(node: Node<'_>, source: &str, expected: &str) {
    let name = field(node, "name");
    assert_eq!(name.kind(), "identifier");
    assert!(!name.is_missing());
    assert_eq!(text(name, source), expected);
    assert_eq!(name.end_byte() - name.start_byte(), expected.len());
    assert!(node.start_byte() <= name.start_byte());
    assert!(name.end_byte() <= node.end_byte());
}

fn assert_header(node: Node<'_>, source: &str, expected: &str) {
    let body = field(node, "body");
    assert!(!body.is_missing());
    assert_eq!(
        source.get(node.start_byte()..body.start_byte()),
        Some(expected)
    );
    let matches = source.match_indices(expected).collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "fixture header must be unique");
    assert_eq!(node.start_byte(), matches[0].0);
    assert_eq!(body.start_byte(), matches[0].0 + expected.len());
}

#[test]
fn class_enum_constructor_method_and_formals_keep_physical_owners() {
    let source = "public with sharing class CounterBox {\r\n\
        public enum Mode { Ready, Waiting }\r\n\
        private Integer total;\r\n\
        public CounterBox(Integer Start) { total = Start; }\r\n\
        public Integer Add(Integer Amount) { return total + Amount; }\r\n\
        }";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let classes = nodes(root, "class_declaration");
    assert_eq!(classes.len(), 1);
    let class = classes[0];
    assert_name(class, source, "CounterBox");
    assert_exact_span(class, source, source);
    assert_header(class, source, "public with sharing class CounterBox ");
    let enums = nodes(root, "enum_declaration");
    assert_eq!(enums.len(), 1);
    let enumeration = enums[0];
    assert_name(enumeration, source, "Mode");
    assert_header(enumeration, source, "public enum Mode ");
    assert_exact_span(enumeration, source, "public enum Mode { Ready, Waiting }");
    assert_eq!(owner(enumeration, "class_declaration"), Some(class));
    let constants = nodes(enumeration, "enum_constant");
    assert_eq!(constants.len(), 2);
    for (constant, name) in constants.iter().zip(["Ready", "Waiting"]) {
        assert_name(*constant, source, name);
        assert_exact_span(*constant, source, name);
        assert_eq!(owner(*constant, "enum_declaration"), Some(enumeration));
    }
    let constructors = nodes(root, "constructor_declaration");
    assert_eq!(constructors.len(), 1);
    let constructor = constructors[0];
    assert_name(constructor, source, "CounterBox");
    assert_header(constructor, source, "public CounterBox(Integer Start) ");
    assert_exact_span(
        constructor,
        source,
        "public CounterBox(Integer Start) { total = Start; }",
    );
    assert_eq!(owner(constructor, "class_declaration"), Some(class));
    let methods = nodes(root, "method_declaration");
    assert_eq!(methods.len(), 1);
    let method = methods[0];
    assert_name(method, source, "Add");
    assert_header(method, source, "public Integer Add(Integer Amount) ");
    assert_eq!(text(field(method, "type"), source), "Integer");
    assert_exact_span(
        method,
        source,
        "public Integer Add(Integer Amount) { return total + Amount; }",
    );
    assert_eq!(owner(method, "class_declaration"), Some(class));
    for (declaration, name, owner_kind) in [
        (constructor, "Start", "constructor_declaration"),
        (method, "Amount", "method_declaration"),
    ] {
        let parameters = field(declaration, "parameters");
        let formals = nodes(parameters, "formal_parameter");
        assert_eq!(formals.len(), 1);
        assert_name(formals[0], source, name);
        assert_eq!(text(field(formals[0], "type"), source), "Integer");
        assert_exact_span(formals[0], source, &format!("Integer {name}"));
        assert_eq!(formals[0].parent(), Some(parameters));
        assert_eq!(owner(formals[0], owner_kind), Some(declaration));
    }
    let fields = nodes(root, "field_declaration");
    assert_eq!(fields.len(), 1);
    assert_exact_span(fields[0], source, "private Integer total;");
    assert_eq!(owner(fields[0], "class_declaration"), Some(class));
    let declarator = field(fields[0], "declarator");
    assert_name(declarator, source, "total");
    assert_eq!(declarator.parent(), Some(fields[0]));
}

#[test]
fn nested_class_members_and_shared_field_type_keep_distinct_names() {
    let source = "public class OuterCase {\n\
        private Integer First = 1, Second = 2;\n\
        public class InnerCase {\n\
        public InnerCase() {}\n\
        public Integer Echo(Integer Raw) { return Raw; }\n\
        }\n}";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let classes = nodes(root, "class_declaration");
    assert_eq!(classes.len(), 2);
    let outer = classes[0];
    let inner = classes[1];
    assert_name(outer, source, "OuterCase");
    assert_name(inner, source, "InnerCase");
    assert_eq!(owner(inner, "class_declaration"), Some(outer));
    assert_header(inner, source, "public class InnerCase ");
    let declarations = nodes(root, "field_declaration");
    assert_eq!(declarations.len(), 1);
    let declaration = declarations[0];
    assert_exact_span(
        declaration,
        source,
        "private Integer First = 1, Second = 2;",
    );
    assert_eq!(text(field(declaration, "type"), source), "Integer");
    let variables = nodes(declaration, "variable_declarator");
    assert_eq!(variables.len(), 2);
    for (variable, name, witness) in [
        (variables[0], "First", "First = 1"),
        (variables[1], "Second", "Second = 2"),
    ] {
        assert_name(variable, source, name);
        assert_exact_span(variable, source, witness);
        assert_eq!(variable.parent(), Some(declaration));
        assert_eq!(owner(variable, "class_declaration"), Some(outer));
    }
    let constructor = nodes(inner, "constructor_declaration");
    assert_eq!(constructor.len(), 1);
    assert_name(constructor[0], source, "InnerCase");
    assert_eq!(owner(constructor[0], "class_declaration"), Some(inner));
    assert_header(constructor[0], source, "public InnerCase() ");
    let method = nodes(inner, "method_declaration");
    assert_eq!(method.len(), 1);
    assert_name(method[0], source, "Echo");
    assert_eq!(owner(method[0], "class_declaration"), Some(inner));
    let formals = nodes(field(method[0], "parameters"), "formal_parameter");
    assert_eq!(formals.len(), 1);
    assert_name(formals[0], source, "Raw");
    assert_eq!(owner(formals[0], "method_declaration"), Some(method[0]));
}

#[test]
fn interface_prototype_keeps_semicolon_extent_and_written_parameter_type() {
    let source = "public interface ValueSource { Integer Read(String Key); }";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let interfaces = nodes(root, "interface_declaration");
    assert_eq!(interfaces.len(), 1);
    let interface = interfaces[0];
    assert_name(interface, source, "ValueSource");
    assert_header(interface, source, "public interface ValueSource ");
    let methods = nodes(interface, "method_declaration");
    assert_eq!(methods.len(), 1);
    let method = methods[0];
    assert_name(method, source, "Read");
    assert!(method.child_by_field_name("body").is_none());
    assert_exact_span(method, source, "Integer Read(String Key);");
    assert_eq!(owner(method, "interface_declaration"), Some(interface));
    let parameters = field(method, "parameters");
    assert_exact_span(parameters, source, "(String Key)");
    let formals = nodes(parameters, "formal_parameter");
    assert_eq!(formals.len(), 1);
    assert_name(formals[0], source, "Key");
    assert_eq!(text(field(formals[0], "type"), source), "String");
    assert_eq!(owner(formals[0], "method_declaration"), Some(method));
}

#[test]
fn annotations_sharing_modifiers_and_apex_switch_preserve_original_tokens() {
    let source = "@IsTest private inherited sharing class ChoiceBox {\n\
        @AuraEnabled public static Integer Choose(Integer Value) {\n\
        switch on Value { when 1, 2 { return 3; } when else { return 0; } }\n\
        }\n}";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let classes = nodes(root, "class_declaration");
    assert_eq!(classes.len(), 1);
    let class = classes[0];
    assert_name(class, source, "ChoiceBox");
    assert_header(
        class,
        source,
        "@IsTest private inherited sharing class ChoiceBox ",
    );
    let methods = nodes(class, "method_declaration");
    assert_eq!(methods.len(), 1);
    let method = methods[0];
    assert_name(method, source, "Choose");
    assert_header(
        method,
        source,
        "@AuraEnabled public static Integer Choose(Integer Value) ",
    );
    let annotations = nodes(root, "annotation");
    assert_eq!(annotations.len(), 2);
    assert_name(annotations[0], source, "IsTest");
    assert_name(annotations[1], source, "AuraEnabled");
    assert_exact_span(annotations[0], source, "@IsTest");
    assert_exact_span(annotations[1], source, "@AuraEnabled");
    assert_eq!(owner(annotations[0], "class_declaration"), Some(class));
    assert_eq!(owner(annotations[1], "method_declaration"), Some(method));
    let modifiers = nodes(root, "inherited_sharing");
    assert_eq!(modifiers.len(), 1);
    assert_exact_span(modifiers[0], source, "inherited sharing");
    let switches = nodes(method, "switch_expression");
    assert_eq!(switches.len(), 1);
    let switch = switches[0];
    assert_eq!(text(field(switch, "condition"), source), "Value");
    assert_exact_span(
        switch,
        source,
        "switch on Value { when 1, 2 { return 3; } when else { return 0; } }",
    );
    assert_eq!(owner(switch, "method_declaration"), Some(method));
    let rules = nodes(field(switch, "body"), "switch_rule");
    assert_eq!(rules.len(), 2);
    let labels = nodes(switch, "switch_label");
    assert_eq!(labels.len(), 2);
    assert_exact_span(labels[0], source, "when 1, 2");
    assert_exact_span(labels[1], source, "when else");
}

#[test]
fn comments_and_quoted_declaration_text_are_not_native_declarations() {
    let source = "// public class Phantom {}\n\
        public class NoteBox {\n\
        /* public Integer Bogus() { return 1; } */\n\
        private String Note = 'class Hidden { public void Fake() {} }';\n\
        public String Read() { return Note; }\n}";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(!root.has_error(), "{}", root.to_sexp());
    let classes = nodes(root, "class_declaration");
    assert_eq!(classes.len(), 1);
    assert_name(classes[0], source, "NoteBox");
    let methods = nodes(root, "method_declaration");
    assert_eq!(methods.len(), 1);
    assert_name(methods[0], source, "Read");
    assert_eq!(nodes(root, "constructor_declaration").len(), 0);
    let strings = nodes(root, "string_literal");
    assert_eq!(strings.len(), 1);
    assert_exact_span(
        strings[0],
        source,
        "'class Hidden { public void Fake() {} }'",
    );
    assert_eq!(nodes(root, "line_comment").len(), 1);
    assert_eq!(nodes(root, "block_comment").len(), 1);
    let declarations = nodes(root, "field_declaration");
    assert_eq!(declarations.len(), 1);
    assert_name(field(declarations[0], "declarator"), source, "Note");
    assert_eq!(
        owner(declarations[0], "class_declaration"),
        Some(classes[0])
    );
}

#[test]
fn malformed_header_cannot_gain_a_clean_owner_from_a_following_class() {
    let source = "public class Cracked {\n\
        public Integer Broken( { Integer Leaked; }\n\
        }\n\
        public class StableBox { public Integer Good(Integer Value) { return Value; } }";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(
        root.has_error(),
        "malformed fixture must retain uncertainty"
    );
    let classes = nodes(root, "class_declaration");
    let stable = classes
        .iter()
        .find(|class| {
            class
                .child_by_field_name("name")
                .is_some_and(|name| text(name, source) == "StableBox")
        })
        .copied()
        .expect("the complete following class must remain its own native owner");
    assert!(!stable.has_error(), "{}", stable.to_sexp());
    assert_exact_span(
        stable,
        source,
        "public class StableBox { public Integer Good(Integer Value) { return Value; } }",
    );
    assert!(owner(stable, "class_declaration").is_none());
    for class in classes {
        if class == stable {
            continue;
        }
        assert!(
            class.end_byte() <= stable.start_byte(),
            "an earlier malformed owner borrowed its sibling"
        );
        assert!(class.has_error(), "{}", class.to_sexp());
    }
    let good = nodes(stable, "method_declaration");
    assert_eq!(good.len(), 1);
    assert_name(good[0], source, "Good");
    assert_eq!(owner(good[0], "class_declaration"), Some(stable));
    assert_header(good[0], source, "public Integer Good(Integer Value) ");
    for method in nodes(root, "method_declaration") {
        if method == good[0] {
            continue;
        }
        assert!(
            method.end_byte() <= stable.start_byte(),
            "a malformed method borrowed a sibling's bytes"
        );
        assert!(
            method.has_error(),
            "a malformed header became clean: {}",
            method.to_sexp()
        );
        assert_ne!(owner(method, "class_declaration"), Some(stable));
    }
}
