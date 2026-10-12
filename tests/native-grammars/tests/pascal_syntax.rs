//! Pascal source-bound witnesses for published inputs and local spelling repairs.
//! Direct written owners, headers, names, and original byte spans are independent
//! of product indexing, compiler semantics, overload binding, and preprocessing.

use tree_sitter::{Node, Parser, Tree};

fn parse(source: &str) -> Tree {
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_pascal::LANGUAGE.into())
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

fn identifiers<'tree>(node: Node<'tree>, name: &str) -> Vec<Node<'tree>> {
    let mut cursor = node.walk();
    node.children_by_field_name(name, &mut cursor)
        .filter(|child| child.kind() == "identifier")
        .collect()
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

fn named<'tree>(root: Node<'tree>, kind: &str, source: &str, name: &str) -> Node<'tree> {
    let matches = nodes(root, kind)
        .into_iter()
        .filter(|node| {
            node.child_by_field_name("name")
                .is_some_and(|written| text(written, source) == name)
        })
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "expected one {kind} named {name}");
    matches[0]
}

fn assert_span(node: Node<'_>, source: &str, expected: &str) {
    let witnesses = source.match_indices(expected).collect::<Vec<_>>();
    assert_eq!(witnesses.len(), 1, "written witness must be unique");
    assert_eq!(
        node.byte_range(),
        witnesses[0].0..witnesses[0].0 + expected.len()
    );
    assert_eq!(text(node, source), expected);
    assert!(!node.is_missing());
}

fn assert_clean(node: Node<'_>) {
    assert!(!node.has_error(), "{}", node.to_sexp());
    assert!(!node.is_missing());
}

#[test]
fn unit_forward_class_interface_fields_and_indexed_property_keep_written_owners() {
    let source = "unit ShapeStore;\n\
        interface\n\
        type\n\
        TFuture = class;\n\
        IValue = interface\n\
          function Read(const Key: Integer): Integer;\n\
        end;\n\
        TBox = class\n\
        private\n\
          Left, Right: Integer;\n\
        public\n\
          constructor Create(Start: Integer);\n\
          property Items[Index: Integer]: Integer read Left write Right;\n\
        end;\n\
        implementation\n\
        end.";
    let tree = parse(source);
    let root = tree.root_node();
    assert_clean(root);
    let units = nodes(root, "unit");
    assert_eq!(units.len(), 1);
    assert_span(units[0], source, source);
    let module_names = nodes(units[0], "moduleName");
    assert_eq!(module_names.len(), 1);
    assert_eq!(module_names[0].parent(), Some(units[0]));
    assert_span(module_names[0], source, "ShapeStore");
    let future = named(root, "declType", source, "TFuture");
    let forward_class = field(future, "type");
    assert_eq!(forward_class.kind(), "declClass");
    assert_eq!(forward_class.parent(), Some(future));
    assert_span(future, source, "TFuture = class;");
    assert!(nodes(forward_class, "kEnd").is_empty());

    let interface_type = named(root, "declType", source, "IValue");
    let interface = field(interface_type, "type");
    assert_eq!(interface.kind(), "declIntf");
    assert_eq!(interface.parent(), Some(interface_type));
    let read = named(interface, "declProc", source, "Read");
    assert_span(read, source, "function Read(const Key: Integer): Integer;");
    assert_eq!(owner(read, "declIntf"), Some(interface));
    let read_args = field(read, "args");
    let read_arg = nodes(read_args, "declArg");
    assert_eq!(read_arg.len(), 1);
    assert_eq!(identifiers(read_arg[0], "name").len(), 1);
    assert_eq!(text(identifiers(read_arg[0], "name")[0], source), "Key");
    assert_eq!(read_arg[0].parent(), Some(read_args));

    let class_type = named(root, "declType", source, "TBox");
    let class = field(class_type, "type");
    assert_eq!(class.kind(), "declClass");
    let fields = nodes(class, "declField");
    assert_eq!(fields.len(), 1);
    assert_span(fields[0], source, "Left, Right: Integer;");
    assert_eq!(owner(fields[0], "declClass"), Some(class));
    assert_eq!(text(field(fields[0], "type"), source), "Integer");
    let names = identifiers(fields[0], "name");
    assert_eq!(names.len(), 2);
    assert_eq!(text(names[0], source), "Left");
    assert_eq!(text(names[1], source), "Right");
    assert_eq!(names[0].parent(), Some(fields[0]));
    assert_eq!(names[1].parent(), Some(fields[0]));
    assert!(names[0].end_byte() < names[1].start_byte());

    let constructor = named(class, "declProc", source, "Create");
    assert_span(constructor, source, "constructor Create(Start: Integer);");
    assert_eq!(owner(constructor, "declClass"), Some(class));
    assert_eq!(nodes(constructor, "kConstructor").len(), 1);
    let property = named(class, "declProp", source, "Items");
    assert_span(
        property,
        source,
        "property Items[Index: Integer]: Integer read Left write Right;",
    );
    assert_eq!(owner(property, "declClass"), Some(class));
    assert_eq!(text(field(property, "getter"), source), "Left");
    assert_eq!(text(field(property, "setter"), source), "Right");
    let property_args = field(property, "args");
    assert_span(property_args, source, "[Index: Integer]");
    let property_arg = nodes(property_args, "declArg");
    assert_eq!(property_arg.len(), 1);
    assert_eq!(text(field(property_arg[0], "name"), source), "Index");
    assert_eq!(property_arg[0].parent(), Some(property_args));
    assert_eq!(owner(property_arg[0], "declProp"), Some(property));
}

#[test]
fn overloaded_qualified_implementations_and_nested_locals_have_distinct_headers() {
    let source = "unit RoutineScopes;\n\
        interface\n\
        type TCounter = class\n\
          function Map(Value: Integer): Integer; overload;\n\
          function Map(Value: String): String; overload;\n\
        end;\n\
        implementation\n\
        function TCounter.Map(Value: Integer): Integer;\n\
        var Left, Right: Integer;\n\
          function Local(const Input, Scale: Integer): Integer;\n\
          begin Local := Input + Scale; end;\n\
        begin Map := Local(Value, 1); end;\n\
        function TCounter.Map(Value: String): String;\n\
        begin Map := Value; end;\n\
        end.";
    let tree = parse(source);
    let root = tree.root_node();
    assert_clean(root);
    let class_type = named(root, "declType", source, "TCounter");
    let class = field(class_type, "type");
    let prototypes = nodes(class, "declProc");
    assert_eq!(prototypes.len(), 2);
    assert_span(
        prototypes[0],
        source,
        "function Map(Value: Integer): Integer; overload;",
    );
    assert_span(
        prototypes[1],
        source,
        "function Map(Value: String): String; overload;",
    );
    assert_ne!(prototypes[0].id(), prototypes[1].id());
    for declaration in &prototypes {
        assert_eq!(text(field(*declaration, "name"), source), "Map");
        assert_eq!(owner(*declaration, "declClass"), Some(class));
    }
    let definitions = nodes(root, "defProc");
    assert_eq!(definitions.len(), 3);
    let outer = definitions[0];
    let nested = definitions[1];
    let second = definitions[2];
    let first_header = field(outer, "header");
    let second_header = field(second, "header");
    assert_span(
        first_header,
        source,
        "function TCounter.Map(Value: Integer): Integer;",
    );
    assert_span(
        second_header,
        source,
        "function TCounter.Map(Value: String): String;",
    );
    for (definition, header) in [(outer, first_header), (second, second_header)] {
        assert_eq!(header.parent(), Some(definition));
        assert_eq!(field(header, "name").kind(), "genericDot");
        assert_eq!(text(field(header, "name"), source), "TCounter.Map");
        assert!(owner(definition, "defProc").is_none());
        let body = field(definition, "body");
        assert_eq!(body.kind(), "block");
        assert_eq!(body.parent(), Some(definition));
        assert!(header.end_byte() <= body.start_byte());
        assert!(body.end_byte() <= definition.end_byte());
    }
    assert_ne!(first_header.id(), second_header.id());
    let local_header = field(nested, "header");
    assert_span(
        local_header,
        source,
        "function Local(const Input, Scale: Integer): Integer;",
    );
    assert_eq!(owner(nested, "defProc"), Some(outer));
    let local_args = field(local_header, "args");
    let group = nodes(local_args, "declArg");
    assert_eq!(group.len(), 1);
    let parameters = identifiers(group[0], "name");
    assert_eq!(parameters.len(), 2);
    for (parameter, expected) in parameters.iter().zip(["Input", "Scale"]) {
        assert_eq!(text(*parameter, source), expected);
        assert_eq!(parameter.parent(), Some(group[0]));
        assert_eq!(owner(*parameter, "defProc"), Some(nested));
    }
    let locals = nodes(outer, "declVar");
    assert_eq!(locals.len(), 1);
    assert_span(locals[0], source, "Left, Right: Integer;");
    assert_eq!(owner(locals[0], "defProc"), Some(outer));
    assert_eq!(identifiers(locals[0], "name").len(), 2);
}

#[test]
fn bom_crlf_and_unicode_literal_controls_preserve_original_byte_spans() {
    let source = "\u{feff}program RawCase;\r\n\
        { procedure Hidden; begin end; }\r\n\
        (* type FakeClass = class end; *)\r\n\
        // function Shadow: Integer; begin end;\r\n\
        const Caption = 'λ procedure Quoted; begin end;';\r\n\
        var MixedCase: Integer;\r\n\
        procedure Written(const &type: Integer);\r\n\
        begin MixedCase := &type; end;\r\n\
        begin Written(1); end.";
    let tree = parse(source);
    let root = tree.root_node();
    assert_clean(root);
    let programs = nodes(root, "program");
    assert_eq!(programs.len(), 1);
    assert_eq!(programs[0].start_byte(), '\u{feff}'.len_utf8());
    assert_span(programs[0], source, source.trim_start_matches('\u{feff}'));
    let declarations = nodes(root, "declProc");
    assert_eq!(declarations.len(), 1);
    assert_span(
        declarations[0],
        source,
        "procedure Written(const &type: Integer);",
    );
    assert_eq!(nodes(root, "defProc").len(), 1);
    assert!(nodes(root, "declClass").is_empty());
    let variable = named(root, "declVar", source, "MixedCase");
    assert_span(variable, source, "MixedCase: Integer;");
    let strings = nodes(root, "literalString");
    assert_eq!(strings.len(), 1);
    assert_span(strings[0], source, "'λ procedure Quoted; begin end;'");
    assert_eq!(nodes(root, "comment").len(), 3);
    let args = field(declarations[0], "args");
    let parameters = nodes(args, "declArg");
    assert_eq!(parameters.len(), 1);
    let parameter = field(parameters[0], "name");
    assert_eq!(text(parameter, source), "&type");
    assert!(source.is_char_boundary(parameter.start_byte()));
    assert!(source.is_char_boundary(parameter.end_byte()));
    assert_eq!(
        owner(parameter, "defProc"),
        nodes(root, "defProc").first().copied()
    );
}

#[test]
fn malformed_routine_header_cannot_borrow_a_complete_following_owner() {
    let source = "procedure Damaged(Value: );\n\
        begin end;\n\
        procedure Stable(Argument: Integer);\n\
        begin end;";
    let tree = parse(source);
    let root = tree.root_node();
    assert!(root.has_error(), "malformed source must retain uncertainty");
    let definitions = nodes(root, "defProc");
    let stable = definitions
        .iter()
        .find(|definition| {
            definition
                .child_by_field_name("header")
                .is_some_and(|header| {
                    header
                        .child_by_field_name("name")
                        .is_some_and(|name| text(name, source) == "Stable")
                })
        })
        .copied()
        .expect("the complete following routine must retain its own native owner");
    assert_clean(stable);
    assert_span(
        stable,
        source,
        "procedure Stable(Argument: Integer);\nbegin end;",
    );
    assert_span(
        field(stable, "header"),
        source,
        "procedure Stable(Argument: Integer);",
    );
    assert!(owner(stable, "defProc").is_none());
    for definition in definitions {
        if definition == stable {
            continue;
        }
        assert!(definition.has_error(), "{}", definition.to_sexp());
        assert!(definition.end_byte() <= stable.start_byte());
        assert_ne!(owner(definition, "defProc"), Some(stable));
    }
    for declaration in nodes(root, "declProc") {
        if declaration == field(stable, "header") {
            continue;
        }
        assert!(declaration.has_error(), "{}", declaration.to_sexp());
        assert!(declaration.end_byte() <= stable.start_byte());
        assert_ne!(owner(declaration, "defProc"), Some(stable));
    }
}

#[test]
fn correctly_spelled_sealed_class_retains_modifier_and_extent() {
    // The published preferred grammar spells this token "seled". This physical
    // ordinary spelling must expose that gap rather than adapting the fixture.
    let source =
        "unit ModifierCase; interface type TClosed = class sealed end; implementation end.";
    let tree = parse(source);
    let root = tree.root_node();
    assert_clean(root);
    let declaration = named(root, "declType", source, "TClosed");
    let class = field(declaration, "type");
    assert_eq!(class.kind(), "declClass");
    assert_span(class, source, "class sealed end");
    let modifiers = nodes(class, "kSealed");
    assert_eq!(modifiers.len(), 1);
    assert_span(modifiers[0], source, "sealed");
}

#[test]
fn correctly_spelled_safecall_routine_retains_attribute_and_header() {
    // The published preferred grammar spells this token "safecal". The proper
    // spelling remains an independent native regression witness.
    let source = "unit CallingCase; interface procedure Invoke(Value: Integer); safecall; implementation end.";
    let tree = parse(source);
    let root = tree.root_node();
    assert_clean(root);
    let declaration = named(root, "declProc", source, "Invoke");
    assert_span(
        declaration,
        source,
        "procedure Invoke(Value: Integer); safecall;",
    );
    let modifiers = nodes(declaration, "kSafecall");
    assert_eq!(modifiers.len(), 1);
    assert_span(modifiers[0], source, "safecall");
    assert!(nodes(root, "defProc").is_empty());
}
