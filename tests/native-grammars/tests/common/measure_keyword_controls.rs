// Extends the unchanged reciprocal harness with lexical measure-name controls.
// Keyword fencing must preserve quoted identifiers and complete identifier prefixes.
// These are parse/source/query checks, not F# typing or name-resolution claims.

fn measure_keyword_type_name(tree: &Tree, source: &str, name: &str, signature: bool) {
    let declaration = format!("{name} = int");
    let alias = exact(
        tree,
        "type_abbrev_defn",
        occurrence(source, &declaration, 0),
    );
    written(alias, source, "type_abbrev_defn", &declaration);
    let name_range = occurrence(source, name, 0);
    let header = exact(tree, "type_name", name_range.clone());
    written(header, source, "type_name", name);
    assert_eq!(header.parent(), Some(alias));
    let identifier = exact(tree, "identifier", name_range.clone());
    written(identifier, source, "identifier", name);
    assert_eq!(
        header
            .child_by_field_name("type_name")
            .unwrap()
            .byte_range(),
        name_range
    );
    let block = alias.child_by_field_name("block").unwrap();
    written(block, source, "simple_type", "int");
    assert_eq!(block.parent(), Some(alias));
    assert!(query_captures(tree, source, signature, "type_name").contains(&header));
}

fn measure_keyword_member<'tree>(
    tree: &'tree Tree,
    source: &str,
    container: Node<'tree>,
    text: &str,
    keywords: &[&str],
    signature: bool,
) -> Node<'tree> {
    let owner = exact(tree, "member_defn", occurrence(source, text, 0));
    written(owner, source, "member_defn", text);
    let carrier = exact(tree, "type_extension_elements", owner.byte_range());
    written(carrier, source, "type_extension_elements", text);
    assert_eq!(owner.parent(), Some(carrier));
    assert_eq!(carrier.parent(), Some(container));
    assert_eq!(children(carrier), [owner]);
    for keyword in keywords {
        let offset = owner.start_byte() + text.find(keyword).unwrap();
        let token = owner
            .child(
                (0..owner.child_count())
                    .find(|index| {
                        let node = owner.child(u32::try_from(*index).unwrap()).unwrap();
                        node.kind() == *keyword
                            && node.byte_range() == (offset..offset + keyword.len())
                    })
                    .map(|index| u32::try_from(index).unwrap())
                    .unwrap(),
            )
            .unwrap();
        written(token, source, keyword, keyword);
        assert!(!token.is_named());
        assert_eq!(token.parent(), Some(owner));
    }
    assert!(query_captures(tree, source, signature, "member_defn").contains(&owner));
    owner
}

fn measure_keyword_valid_members(signature: bool, ending: &str) {
    let fields = "Gauge = class\n    val mutable storage: int\n    static val cached: int\nend";
    let source = fixture(signature, &format!("type {fields}"), ending, true);
    let tree = parse(&mut parser(signature), &source, None);
    clean(&tree, &source);
    let container = exact(
        &tree,
        "anon_type_defn",
        occurrence(&source, &fields.replace('\n', ending), 0),
    );
    for (text, name, keywords) in [
        (
            "val mutable storage: int",
            "storage",
            &["val", "mutable"][..],
        ),
        ("static val cached: int", "cached", &["static", "val"][..]),
    ] {
        let member = measure_keyword_member(&tree, &source, container, text, keywords, signature);
        let name_start = member.start_byte() + text.find(name).unwrap();
        let identifier = exact(&tree, "identifier", name_start..name_start + name.len());
        written(identifier, &source, "identifier", name);
        assert_eq!(identifier.parent(), Some(member));
        let type_start = member.start_byte() + text.find("int").unwrap();
        let field_type = exact(&tree, "simple_type", type_start..type_start + 3);
        written(field_type, &source, "simple_type", "int");
        assert_eq!(field_type.parent(), Some(member));
        if signature {
            assert_eq!(member.child_by_field_name("name"), Some(identifier));
            assert_eq!(member.child_by_field_name("type"), Some(field_type));
        } else {
            assert!(member.child_by_field_name("name").is_none());
            assert!(member.child_by_field_name("type").is_none());
        }
    }
    assert_eq!(
        query_captures(&tree, &source, signature, "member_defn").len(),
        2
    );
    sentinel(&tree, &source, signature);
    queries(&tree, &source, signature);

    // Signatures declare auto-property interfaces, not implementation initializers.
    let properties = if signature {
        "Gauge = class\n    member Current: int with get, set\n    static member Shared: int with get\nend"
    } else {
        "Gauge = class\n    member val Current = 0 with get, set\n    static member val Shared = 0 with get\nend"
    };
    let source = fixture(signature, &format!("type {properties}"), ending, true);
    let tree = parse(&mut parser(signature), &source, None);
    clean(&tree, &source);
    let container = exact(
        &tree,
        "anon_type_defn",
        occurrence(&source, &properties.replace('\n', ending), 0),
    );
    let rows = if signature {
        [
            (
                "member Current: int with get, set",
                "Current",
                &["member"][..],
            ),
            (
                "static member Shared: int with get",
                "Shared",
                &["static", "member"][..],
            ),
        ]
    } else {
        [
            (
                "member val Current = 0 with get, set",
                "Current",
                &["member", "val"][..],
            ),
            (
                "static member val Shared = 0 with get",
                "Shared",
                &["static", "member", "val"][..],
            ),
        ]
    };
    for (text, name, keywords) in rows {
        let member = measure_keyword_member(&tree, &source, container, text, keywords, signature);
        let name_start = member.start_byte() + text.find(name).unwrap();
        let identifier = exact(&tree, "identifier", name_start..name_start + name.len());
        written(identifier, &source, "identifier", name);
        if signature {
            let interface_text = text
                .strip_prefix("static ")
                .unwrap_or(text)
                .strip_prefix("member ")
                .unwrap();
            let interface = exact(
                &tree,
                "member_signature",
                occurrence(&source, interface_text, 0),
            );
            written(interface, &source, "member_signature", interface_text);
            assert_eq!(interface.parent(), Some(member));
            assert_eq!(identifier.parent(), Some(interface));
            assert_eq!(interface.child_by_field_name("name"), Some(identifier));
            written(
                interface.child_by_field_name("signature").unwrap(),
                &source,
                "curried_spec",
                "int",
            );
        } else {
            let property = exact(
                &tree,
                "property_or_ident",
                name_start..name_start + name.len(),
            );
            written(property, &source, "property_or_ident", name);
            assert_eq!(identifier.parent(), Some(property));
            assert_eq!(property.parent(), Some(member));
            let value_start = member.start_byte() + text.find('0').unwrap();
            let value = exact(&tree, "const", value_start..value_start + 1);
            written(value, &source, "const", "0");
            assert_eq!(value.parent(), Some(member));
        }
    }
    assert_eq!(
        query_captures(&tree, &source, signature, "member_defn").len(),
        2
    );
    sentinel(&tree, &source, signature);
    queries(&tree, &source, signature);
}

#[test]
fn measure_keywords_cannot_fall_back_to_unquoted_operands_or_type_names() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for declaration in [
                "[<Measure>]\ntype rate = /let",
                "[<Measure>]\ntype rate = /val",
                "type Reading = float</let>",
                "type Reading = float</val>",
                "type let = int",
                "type val = int",
            ] {
                let source = fixture(signature, declaration, ending, true);
                let tree = parse(&mut parser(signature), &source, None);
                assert!(
                    tree.root_node().has_error(),
                    "{signature:?} {source:?}: {}",
                    tree.root_node().to_sexp()
                );
                assert!(
                    nodes(&tree)
                        .iter()
                        .any(|node| node.is_error() || node.is_missing())
                );
                snapshot(&tree, &source);
                queries(&tree, &source, signature);
            }
            measure_keyword_valid_members(signature, ending);
        }
    }
}

#[test]
fn quoted_measure_keywords_keep_exact_denominator_and_type_name_identity() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for (value, name) in [("/``let``", "``let``"), ("/``val``", "``val``")] {
                let source = fixture(
                    signature,
                    &format!("[<Measure>]\ntype rate = {value}"),
                    ending,
                    true,
                );
                let tree = parse(&mut parser(signature), &source, None);
                clean(&tree, &source);
                let owner = quotient(&tree, &source, value, 0, &[("measure_atom", name)], true);
                alias_owner(&tree, &source, value, owner);
                let identifier = exact(&tree, "identifier", occurrence(&source, name, 0));
                written(identifier, &source, "identifier", name);
                sentinel(&tree, &source, signature);
                queries(&tree, &source, signature);

                let source = fixture(signature, &format!("type {name} = int"), ending, true);
                let tree = parse(&mut parser(signature), &source, None);
                clean(&tree, &source);
                measure_keyword_type_name(&tree, &source, name, signature);
                sentinel(&tree, &source, signature);
                queries(&tree, &source, signature);
            }
        }
    }
}

#[test]
fn complete_keyword_prefix_identifiers_keep_written_measure_and_name_boundaries() {
    for signature in [false, true] {
        for ending in ["\n", "\r\n"] {
            for (value, name) in [
                ("/letter", "letter"),
                ("/valuable", "valuable"),
                ("/letΩ", "letΩ"),
                ("/valΩ", "valΩ"),
            ] {
                let source = fixture(
                    signature,
                    &format!("[<Measure>]\ntype rate = {value}"),
                    ending,
                    true,
                );
                let tree = parse(&mut parser(signature), &source, None);
                clean(&tree, &source);
                let owner = quotient(&tree, &source, value, 0, &[("measure_atom", name)], true);
                alias_owner(&tree, &source, value, owner);
                let identifier = exact(&tree, "identifier", occurrence(&source, name, 0));
                written(identifier, &source, "identifier", name);
                sentinel(&tree, &source, signature);
                queries(&tree, &source, signature);

                let source = fixture(signature, &format!("type {name} = int"), ending, true);
                let tree = parse(&mut parser(signature), &source, None);
                clean(&tree, &source);
                measure_keyword_type_name(&tree, &source, name, signature);
                sentinel(&tree, &source, signature);
                queries(&tree, &source, signature);
            }
        }
    }
}
