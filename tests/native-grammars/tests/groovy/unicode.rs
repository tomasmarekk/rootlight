//! Unicode spelling and source coordinates must survive every identifier context.
//! Fixtures exercise Java identifier categories; they are parsed, never executed.
use tree_sitter::{Node, Parser, Point, Tree};

fn parser() -> Parser {
    let mut p = Parser::new();
    p.set_language(&dekobon_tree_sitter_groovy::LANGUAGE.into())
        .unwrap();
    p
}
fn nodes(root: Node<'_>) -> Vec<Node<'_>> {
    let mut pending = vec![root];
    let mut result = vec![];
    while let Some(n) = pending.pop() {
        let mut c = n.walk();
        pending.extend(n.children(&mut c));
        result.push(n);
    }
    result
}
fn point(source: &str, offset: usize) -> Point {
    let prefix = &source[..offset];
    Point::new(
        prefix.bytes().filter(|b| *b == b'\n').count(),
        prefix
            .rfind('\n')
            .map_or(prefix.len(), |p| prefix.len() - p - 1),
    )
}
fn valid(source: &str) -> Tree {
    let tree = parser().parse(source, None).unwrap();
    assert!(
        !tree.root_node().has_error(),
        "{source:?}\n{}",
        tree.root_node().to_sexp()
    );
    for n in nodes(tree.root_node()) {
        assert!(source.get(n.byte_range()).is_some());
        assert_eq!(n.start_position(), point(source, n.start_byte()));
        assert_eq!(n.end_position(), point(source, n.end_byte()));
    }
    tree
}
fn exact(tree: &Tree, source: &str, kind: &str, text: &str) {
    assert!(
        nodes(tree.root_node())
            .iter()
            .any(|n| n.kind() == kind && n.utf8_text(source.as_bytes()).unwrap() == text),
        "missing {kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
}

#[test]
fn java_identifier_categories_preserve_exact_declaration_names() {
    for name in [
        "λ",
        "Δelta",
        "变量",
        "данные",
        "é",
        "e\u{301}",
        "a\u{903}",
        "a\u{661}",
        "€value",
        "＿value",
        "Ⅰvalue",
        "𐐀value",
        "𐐨value",
        "𠀀value",
        "a$b",
        "$value",
        "_value",
    ] {
        let source = format!("def {name} = 1\nfinish({name})\n");
        let tree = valid(&source);
        exact(
            &tree,
            &source,
            "variable_declarator",
            &format!("{name} = 1"),
        );
        exact(&tree, &source, "identifier", name);
        exact(
            &tree,
            &source,
            "method_invocation",
            &format!("finish({name})"),
        );
    }
}

#[test]
fn unicode_uppercase_types_are_not_command_calls() {
    for name in ["Δelta", "État", "Ⅰvalue", "𐐀value"] {
        let declaration = format!("{name} value");
        let source = format!("def run() {{ {declaration}\nfinish() }}\n");
        let tree = valid(&source);
        exact(&tree, &source, "local_variable_declaration", &declaration);
        exact(&tree, &source, "type_identifier", name);
        exact(&tree, &source, "method_invocation", "finish()");
    }
}

#[test]
fn unicode_lowercase_and_titlecase_receivers_remain_commands() {
    for name in ["λ", "é", "变量", "ǅelta", "𐐨value"] {
        let source = format!("{name} value\n");
        let tree = valid(&source);
        assert!(
            !nodes(tree.root_node())
                .iter()
                .any(|n| n.kind() == "local_variable_declaration")
        );
        exact(&tree, &source, "command_chain", source.trim());
    }
}

#[test]
fn unicode_members_methods_parameters_and_closures_are_named() {
    let source = "class Δelta { def λ(变量) { def κ = { δ -> δ.μέλος() }; return 变量.μέλος } }\n";
    let tree = valid(source);
    for name in ["Δelta", "λ", "变量", "κ", "δ", "μέλος"] {
        exact(&tree, source, "identifier", name);
    }
    exact(&tree, source, "method_invocation", "δ.μέλος()");
}

#[test]
fn unicode_gstring_paths_preserve_full_interpolations() {
    for path in [
        "λ.μέλος",
        "变量.名",
        "e\u{301}.a\u{661}",
        "€value.＿value",
        "𐐀value.𠀀value",
    ] {
        for (open, close) in [("\"", "\""), ("\"\"\"", "\"\"\""), ("/", "/"), ("$/", "/$")] {
            let interpolation = format!("${path}");
            let source = format!("def text = {open}{interpolation}{close}\n");
            let tree = valid(&source);
            exact(
                &tree,
                &source,
                "gstring_dollar_interpolation",
                &interpolation,
            );
        }
    }
}

#[test]
fn dollar_separates_adjacent_gstring_references() {
    let source = "def text = \"$λ$δ $变量.名$€value\"\n";
    let tree = valid(source);
    let interpolations: Vec<_> = nodes(tree.root_node())
        .into_iter()
        .filter(|n| n.kind() == "gstring_dollar_interpolation")
        .collect();
    assert_eq!(interpolations.len(), 4);
    for text in ["$λ", "$δ", "$变量.名", "$€value"] {
        exact(&tree, source, "gstring_dollar_interpolation", text);
    }
}

#[test]
fn invalid_identifier_characters_are_not_silently_consumed() {
    for name in [
        "😀value",
        "\u{301}value",
        "\u{661}value",
        "a\u{200d}b",
        "a\u{200b}b",
        "a\u{ad}b",
        "a\u{0}b",
        "a\u{7f}b",
        "a\u{202e}b",
        "a\u{20dd}b",
    ] {
        let source = format!("def {name} = 1\n");
        assert!(
            parser()
                .parse(&source, None)
                .unwrap()
                .root_node()
                .has_error(),
            "invalid identifier accepted: {source:?}"
        );
    }
}

#[test]
fn keyword_prefixes_with_unicode_suffixes_remain_identifiers() {
    for name in [
        "ifλ",
        "class变量",
        "return𐐀",
        "inδ",
        "as€",
        "instanceof名",
        "true\u{301}",
    ] {
        let source = format!("def {name} = 1\n{name}\n");
        let tree = valid(&source);
        exact(&tree, &source, "identifier", name);
    }
    for name in ["inδ", "as€", "instanceof名"] {
        let source = format!("value\n/* λ */ {name}\n");
        let tree = valid(&source);
        assert_eq!(
            tree.root_node().named_child_count(),
            3,
            "{}",
            tree.root_node().to_sexp()
        );
    }
}

#[test]
fn unicode_spelling_is_not_normalized_or_case_folded() {
    let source = "def é = 1\ndef e\u{301} = 2\ndef É = 3\n";
    let tree = valid(source);
    for name in ["é", "e\u{301}", "É"] {
        exact(&tree, source, "identifier", name);
    }
}

#[test]
fn unicode_identifier_edits_match_fresh_byte_and_point_ranges() {
    let source = "// λ😀\r\ndef value = 1\r\nfinish(value)\r\n";
    for replacement in ["λ", "e\u{301}", "𐐀value", "变量"] {
        let start = source.find("value").unwrap();
        let changed = source.replacen("value", replacement, 1);
        let mut old = valid(source);
        old.edit(&tree_sitter::InputEdit {
            start_byte: start,
            old_end_byte: start + 5,
            new_end_byte: start + replacement.len(),
            start_position: point(source, start),
            old_end_position: point(source, start + 5),
            new_end_position: point(&changed, start + replacement.len()),
        });
        let incremental = parser().parse(&changed, Some(&old)).unwrap();
        let fresh = valid(&changed);
        let signature = |tree: &Tree| {
            nodes(tree.root_node())
                .into_iter()
                .map(|n| {
                    (
                        n.kind().to_owned(),
                        n.byte_range(),
                        n.start_position(),
                        n.end_position(),
                        n.is_missing(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(signature(&incremental), signature(&fresh));
        exact(&fresh, &changed, "identifier", replacement);
    }
}

#[test]
fn unicode_identifiers_keep_included_host_coordinates() {
    let prefix = "host λ😀\r\n";
    let body = "def 𐐀value = 1\r\nfinish(𐐀value)\r\n";
    let source = format!("{prefix}{body}suffix");
    let mut p = parser();
    p.set_included_ranges(&[tree_sitter::Range {
        start_byte: prefix.len(),
        end_byte: prefix.len() + body.len(),
        start_point: point(&source, prefix.len()),
        end_point: point(&source, prefix.len() + body.len()),
    }])
    .unwrap();
    let tree = p.parse(&source, None).unwrap();
    assert!(!tree.root_node().has_error());
    exact(&tree, &source, "identifier", "𐐀value");
    for n in nodes(tree.root_node()) {
        assert!(source.get(n.byte_range()).is_some());
        assert_eq!(n.start_position(), point(&source, n.start_byte()));
        assert_eq!(n.end_position(), point(&source, n.end_byte()));
    }
}

#[test]
fn growing_unicode_identifiers_have_bounded_input_work() {
    for (kind, fragment) in [
        ("bmp", "λ名"),
        ("supplementary", "𐐀𠀀"),
        ("combining", "e\u{301}"),
    ] {
        let mut prior = None;
        for count in [128, 256, 512, 1024] {
            let name = fragment.repeat(count);
            let source = format!("def {name} = 1\nfinish({name})\n");
            let mut reads = 0_usize;
            let tree = parser()
                .parse_with_options(
                    &mut |offset, _| {
                        reads += 1;
                        // The input callback must let the runtime decode a complete
                        // scalar; a permanently one-byte chunk splits UTF-8 forever.
                        let tail = &source[offset..];
                        let length = tail.chars().next().map_or(0, char::len_utf8);
                        &tail.as_bytes()[..length]
                    },
                    None,
                    None,
                )
                .unwrap();
            assert!(!tree.root_node().has_error());
            exact(&tree, &source, "identifier", &name);
            exact(
                &tree,
                &source,
                "method_invocation",
                &format!("finish({name})"),
            );
            assert!(reads <= source.len() * 20);
            if let Some(value) = prior {
                assert!(reads <= value * 3);
            }
            prior = Some(reads);
            println!(
                "unicode-work kind={kind} count={count} bytes={} reads={reads}",
                source.len()
            );
        }
    }
}
