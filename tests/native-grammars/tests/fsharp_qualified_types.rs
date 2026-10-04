//! Neutral qualified atomic types retain constructed bases and written arguments.
//! Queries distinguish measured abbreviation aliases from canonical type routes.
//! These controls prove syntax ownership, not name resolution or type checking.

use std::{
    ops::{ControlFlow, Range},
    time::{Duration, Instant},
};
use tree_sitter::{
    InputEdit, Language, Node, ParseOptions, Parser, Point, Query, QueryCursor, QueryCursorOptions,
    StreamingIterator, Tree,
};

enum Shape {
    Generic,
    Simple(&'static str),
    Paren(&'static Shape),
    Qualified {
        base: &'static Shape,
        name: &'static str,
        argument: Option<&'static str>,
    },
    Flexible(&'static Shape),
    Nullable(&'static Shape),
    Function(&'static Shape, &'static Shape),
    Compound(&'static Shape, &'static Shape),
}

impl Shape {
    fn text(&self) -> String {
        match self {
            Self::Generic => "Container<'T>".to_owned(),
            Self::Simple(name) => (*name).to_owned(),
            Self::Paren(inner) => format!("({})", inner.text()),
            Self::Qualified {
                base,
                name,
                argument,
            } => {
                let suffix = argument.map_or_else(String::new, |value| format!("<{value}>"));
                format!("{}.{name}{suffix}", base.text())
            }
            Self::Flexible(inner) => format!("#{}", inner.text()),
            Self::Nullable(inner) => format!("{} | null", inner.text()),
            Self::Function(left, right) => format!("{} -> {}", left.text(), right.text()),
            Self::Compound(left, right) => format!("{} * {}", left.text(), right.text()),
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Generic => "generic_type",
            Self::Simple(_) => "simple_type",
            Self::Paren(_) => "paren_type",
            Self::Qualified { .. } => "qualified_type",
            Self::Flexible(_) => "flexible_type",
            Self::Nullable(_) => "nullable_type",
            Self::Function(_, _) => "function_type",
            Self::Compound(_, _) => "compound_type",
        }
    }
}

const GENERIC: Shape = Shape::Generic;
const BASE: Shape = Shape::Qualified {
    base: &GENERIC,
    name: "Builder",
    argument: None,
};
const PATH: Shape = Shape::Qualified {
    base: &GENERIC,
    name: "Middle.Builder",
    argument: None,
};
const TRAILING: Shape = Shape::Qualified {
    base: &GENERIC,
    name: "Builder",
    argument: Some("'U"),
};
const TRAILING_INT: Shape = Shape::Qualified {
    base: &GENERIC,
    name: "Builder",
    argument: Some("int"),
};
const MIDDLE: Shape = Shape::Qualified {
    base: &GENERIC,
    name: "Middle",
    argument: Some("'U"),
};
const CHAIN: Shape = Shape::Qualified {
    base: &MIDDLE,
    name: "Builder",
    argument: Some("'V"),
};
const EDITED_CHAIN: Shape = Shape::Qualified {
    base: &MIDDLE,
    name: "LongerBuilder",
    argument: Some("'V"),
};
const PAREN_BASE: Shape = Shape::Paren(&GENERIC);
const PAREN: Shape = Shape::Qualified {
    base: &PAREN_BASE,
    name: "Builder",
    argument: None,
};
const FLEXIBLE: Shape = Shape::Flexible(&BASE);
const INT: Shape = Shape::Simple("int");
const NULLABLE: Shape = Shape::Nullable(&BASE);
const FUNCTION: Shape = Shape::Function(&BASE, &INT);
const COMPOUND: Shape = Shape::Compound(&BASE, &INT);

struct Family {
    implementation: &'static str,
    signature: &'static str,
    head: &'static str,
    parameters: &'static [&'static str],
    shape: &'static Shape,
    canonical_root: bool,
}

const FAMILIES: [Family; 6] = [
    Family {
        implementation: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T> = Container<'T>.Builder
let following = 30
"####,
        signature: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T> = Container<'T>.Builder
val following: int
"####,
        head: "Nested<'T>",
        parameters: &["'T"],
        shape: &BASE,
        canonical_root: false,
    },
    Family {
        implementation: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T> = Container<'T>.Middle.Builder
let following = 30
"####,
        signature: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T> = Container<'T>.Middle.Builder
val following: int
"####,
        head: "Nested<'T>",
        parameters: &["'T"],
        shape: &PATH,
        canonical_root: false,
    },
    Family {
        implementation: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T, 'U> = Container<'T>.Builder<'U>
let following = 30
"####,
        signature: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T, 'U> = Container<'T>.Builder<'U>
val following: int
"####,
        head: "Nested<'T, 'U>",
        parameters: &["'T", "'U"],
        shape: &TRAILING,
        canonical_root: false,
    },
    Family {
        implementation: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T, 'U, 'V> = Container<'T>.Middle<'U>.Builder<'V>
let following = 30
"####,
        signature: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T, 'U, 'V> = Container<'T>.Middle<'U>.Builder<'V>
val following: int
"####,
        head: "Nested<'T, 'U, 'V>",
        parameters: &["'T", "'U", "'V"],
        shape: &CHAIN,
        canonical_root: false,
    },
    Family {
        implementation: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T> = (Container<'T>).Builder
let following = 30
"####,
        signature: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T> = (Container<'T>).Builder
val following: int
"####,
        head: "Nested<'T>",
        parameters: &["'T"],
        shape: &PAREN,
        canonical_root: true,
    },
    Family {
        implementation: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T> = #Container<'T>.Builder
let following = 30
"####,
        signature: r####"// Parse-only atomic-type control; names need not resolve.
// The following declaration is an independent ownership boundary.
module TypeExample
type Nested<'T> = #Container<'T>.Builder
val following: int
"####,
        head: "Nested<'T>",
        parameters: &["'T"],
        shape: &FLEXIBLE,
        canonical_root: true,
    },
];

fn language(signature: bool) -> Language {
    if signature {
        tree_sitter_fsharp::LANGUAGE_SIGNATURE
    } else {
        tree_sitter_fsharp::LANGUAGE_FSHARP
    }
    .into()
}

fn parser(signature: bool) -> Parser {
    let mut parser = Parser::new();
    parser
        .set_language(&language(signature))
        .expect("pinned grammar is incompatible with Tree-sitter");
    parser
}

fn parse(parser: &mut Parser, source: &str, previous: Option<&Tree>) -> Tree {
    assert!(source.len() < 4_096, "authored source exceeds its bound");
    let started = Instant::now();
    let mut checks = 0_usize;
    let mut progress = |_: &tree_sitter::ParseState| {
        checks += 1;
        if checks > 2_000 || started.elapsed() > Duration::from_secs(10) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    parser
        .parse_with_options(
            &mut |offset, _| source.as_bytes().get(offset..).unwrap_or_default(),
            previous,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        )
        .expect("authored qualified type exceeded its parse watchdog")
}

fn point(source: &str, offset: usize) -> Point {
    let prefix = source
        .as_bytes()
        .get(..offset)
        .expect("offset escapes source");
    Point::new(
        prefix.iter().filter(|byte| **byte == b'\n').count(),
        prefix
            .iter()
            .rposition(|byte| *byte == b'\n')
            .map_or(prefix.len(), |last| prefix.len() - last - 1),
    )
}

fn nodes(tree: &Tree) -> Vec<Node<'_>> {
    let mut pending = vec![tree.root_node()];
    let mut result = Vec::new();
    while let Some(node) = pending.pop() {
        assert!(result.len() < 10_000, "node inventory exceeds its bound");
        result.push(node);
        for index in (0..node.child_count()).rev() {
            pending.push(node.child(u32::try_from(index).unwrap()).unwrap());
        }
    }
    result
}

fn children(node: Node<'_>) -> Vec<Node<'_>> {
    (0..node.named_child_count())
        .map(|index| node.named_child(u32::try_from(index).unwrap()).unwrap())
        .filter(|child| !child.is_extra())
        .collect()
}

fn written(node: Node<'_>, source: &str, kind: &str, text: &str) {
    assert_eq!(node.kind(), kind);
    assert_eq!(source.get(node.byte_range()), Some(text));
    assert_eq!(node.start_position(), point(source, node.start_byte()));
    assert_eq!(node.end_position(), point(source, node.end_byte()));
}

fn exact<'tree>(tree: &'tree Tree, source: &str, kind: &str, text: &str) -> Node<'tree> {
    let found: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == kind && source.get(node.byte_range()) == Some(text))
        .collect();
    assert_eq!(
        found.len(),
        1,
        "{kind} {text:?}: {}",
        tree.root_node().to_sexp()
    );
    written(found[0], source, kind, text);
    found[0]
}

fn field<'tree>(owner: Node<'tree>, name: &str) -> Node<'tree> {
    let child = owner
        .child_by_field_name(name)
        .expect("authored field is absent");
    assert_eq!(child.parent(), Some(owner));
    child
}

fn direct(owner: Node<'_>, expected: &[Node<'_>]) {
    assert_eq!(children(owner), expected);
    for child in expected {
        assert_eq!(child.parent(), Some(owner));
    }
}

fn path(node: Node<'_>, source: &str, text: &str) {
    written(node, source, "long_identifier", text);
    let names = children(node);
    let parts: Vec<_> = text.split('.').collect();
    assert_eq!(names.len(), parts.len());
    let mut offset = node.start_byte();
    for (name, part) in names.iter().zip(parts) {
        written(*name, source, "identifier", part);
        assert_eq!(name.parent(), Some(node));
        assert_eq!(name.start_byte(), offset);
        offset = name.end_byte() + 1;
    }
}

fn type_argument(node: Node<'_>, source: &str, text: &str) {
    written(node, source, "type_argument", text);
    let names = children(node);
    assert_eq!(names.len(), 1);
    written(
        names[0],
        source,
        "identifier",
        text.strip_prefix('\'').unwrap(),
    );
    assert_eq!(names[0].parent(), Some(node));
    assert_eq!(names[0].start_byte(), node.start_byte() + 1);
    assert_eq!(names[0].end_byte(), node.end_byte());
}

fn attributes<'tree>(owner: Node<'tree>, source: &str, text: &str) -> Node<'tree> {
    written(owner, source, "type_attributes", text);
    let attributes = children(owner);
    assert_eq!(attributes.len(), 1);
    written(attributes[0], source, "type_attribute", text);
    let arguments = children(attributes[0]);
    assert_eq!(arguments.len(), 1);
    if text.starts_with('\'') {
        type_argument(arguments[0], source, text);
    } else {
        written(arguments[0], source, "simple_type", text);
        let names = children(arguments[0]);
        assert_eq!(names.len(), 1);
        path(names[0], source, text);
        direct(arguments[0], &names);
    }
    direct(owner, &attributes);
    direct(attributes[0], &arguments);
    arguments[0]
}

fn shape<'tree>(
    node: Node<'tree>,
    source: &str,
    expected: &Shape,
    canonical: &mut Vec<Node<'tree>>,
) {
    written(node, source, expected.kind(), &expected.text());
    match expected {
        Shape::Simple(name) => {
            let parts = children(node);
            assert_eq!(parts.len(), 1);
            path(parts[0], source, name);
            direct(node, &parts);
        }
        Shape::Generic => {
            let parts = children(node);
            assert_eq!(parts.len(), 2);
            path(parts[0], source, "Container");
            canonical.push(attributes(parts[1], source, "'T"));
            direct(node, &parts);
            assert_eq!(parts[0].end_byte() + 1, parts[1].start_byte());
            assert_eq!(parts[1].end_byte() + 1, node.end_byte());
        }
        Shape::Paren(inner) => {
            let parts = children(node);
            assert_eq!(parts.len(), 1);
            direct(node, &parts);
            assert_eq!(node.start_byte() + 1, parts[0].start_byte());
            assert_eq!(parts[0].end_byte() + 1, node.end_byte());
            canonical.push(parts[0]);
            shape(parts[0], source, inner, canonical);
        }
        Shape::Qualified {
            base,
            name,
            argument,
        } => {
            let base_node = field(node, "base");
            let name_node = field(node, "name");
            path(name_node, source, name);
            assert_eq!(base_node.start_byte(), node.start_byte());
            assert_eq!(base_node.end_byte() + 1, name_node.start_byte());
            assert_eq!(
                source.get(base_node.end_byte()..name_node.start_byte()),
                Some(".")
            );
            let mut parts = vec![base_node, name_node];
            if let Some(argument) = argument {
                let argument_node = field(node, "arguments");
                canonical.push(attributes(argument_node, source, argument));
                assert_eq!(name_node.end_byte() + 1, argument_node.start_byte());
                assert_eq!(argument_node.end_byte() + 1, node.end_byte());
                parts.push(argument_node);
            } else {
                assert!(node.child_by_field_name("arguments").is_none());
                assert_eq!(name_node.end_byte(), node.end_byte());
            }
            direct(node, &parts);
            // A directly admitted atom base has no invented `_type` wrapper.
            shape(base_node, source, base, canonical);
        }
        Shape::Flexible(inner) => {
            let parts = children(node);
            assert_eq!(parts.len(), 1);
            direct(node, &parts);
            assert_eq!(node.start_byte() + 1, parts[0].start_byte());
            assert_eq!(parts[0].end_byte(), node.end_byte());
            assert_eq!(
                source.get(node.start_byte()..parts[0].start_byte()),
                Some("#")
            );
            shape(parts[0], source, inner, canonical);
        }
        Shape::Nullable(inner) => {
            let parts = children(node);
            assert_eq!(parts.len(), 1);
            direct(node, &parts);
            shape(parts[0], source, inner, canonical);
            assert_eq!(parts[0].start_byte(), node.start_byte());
            assert_eq!(
                source.get(parts[0].end_byte()..node.end_byte()),
                Some(" | null")
            );
        }
        Shape::Function(left, right) | Shape::Compound(left, right) => {
            let parts = children(node);
            assert_eq!(parts.len(), 2);
            direct(node, &parts);
            assert_eq!(parts[0].start_byte(), node.start_byte());
            assert_eq!(parts[1].end_byte(), node.end_byte());
            assert_eq!(
                source.get(parts[0].end_byte()..parts[1].start_byte()),
                Some(if matches!(expected, Shape::Function(_, _)) {
                    " -> "
                } else {
                    " * "
                }),
            );
            shape(parts[0], source, left, canonical);
            canonical.push(parts[1]);
            shape(parts[1], source, right, canonical);
        }
    }
}

fn query_captures<'tree>(
    tree: &'tree Tree,
    source: &str,
    signature: bool,
    pattern: &str,
) -> Vec<Node<'tree>> {
    let query = Query::new(&language(signature), pattern)
        .expect("authored public query must compile in its declared profile");
    let mut cursor = QueryCursor::new();
    cursor.set_match_limit(4_096);
    let started = Instant::now();
    let mut checks = 0_usize;
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::QueryCursorState| {
        checks += 1;
        if checks > 2_000 || started.elapsed() > Duration::from_secs(10) {
            exhausted = true;
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    let mut result = Vec::new();
    {
        let mut captures = cursor.captures_with_options(
            &query,
            tree.root_node(),
            source.as_bytes(),
            QueryCursorOptions::new().progress_callback(&mut progress),
        );
        while let Some((matched, index)) = captures.next() {
            assert!(result.len() < 4_096, "capture inventory exceeds its bound");
            let capture = matched.captures.get(*index).expect("invalid capture index");
            assert_eq!(capture.index, 0);
            result.push(capture.node);
        }
    }
    assert!(!exhausted && !cursor.did_exceed_match_limit());
    result.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind()));
    result
}

fn type_captures<'tree>(tree: &'tree Tree, source: &str) -> Vec<Node<'tree>> {
    query_captures(tree, source, false, "(_type) @type")
}

fn qualified_query(tree: &Tree, source: &str, signature: bool) {
    let mut expected: Vec<_> = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == "qualified_type")
        .collect();
    assert!(
        !expected.is_empty(),
        "authored source must contain a qualified type"
    );
    expected.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind()));
    assert_eq!(
        query_captures(tree, source, signature, "(qualified_type) @qualified_type"),
        expected,
        "explicit qualified-type query must retain every actual qualified descendant",
    );
}

fn check(tree: &Tree, source: &str, signature: bool, family: &Family, expected: &Shape) {
    let root = tree.root_node();
    assert_eq!(root.kind(), "file");
    assert_eq!(root.start_byte(), 0);
    assert_eq!(root.end_byte(), source.len());
    assert!(!root.has_error(), "{source:?}: {}", root.to_sexp());
    for node in nodes(tree) {
        assert!(!node.is_error() && !node.is_missing());
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
        if let Some(parent) = node.parent() {
            assert!(
                parent.start_byte() <= node.start_byte() && node.end_byte() <= parent.end_byte()
            );
        }
    }
    let module = nodes(tree)
        .into_iter()
        .filter(|node| node.kind() == "named_module")
        .collect::<Vec<_>>();
    assert_eq!(module.len(), 1);
    let module = module[0];
    assert_eq!(module.parent(), Some(root));
    let module_name = field(module, "name");
    path(module_name, source, "TypeExample");
    let alias_text = format!("{} = {}", family.head, expected.text());
    let alias = exact(tree, source, "type_abbrev_defn", &alias_text);
    let definition = alias.parent().unwrap();
    assert_eq!(definition.kind(), "type_definition");
    assert_eq!(definition.parent(), Some(module));
    assert_eq!(definition.start_byte(), source.find("type Nested").unwrap());
    direct(definition, &[alias]);
    let head = exact(tree, source, "type_name", family.head);
    let name = field(head, "type_name");
    path(name, source, "Nested");
    let head_parts = children(head);
    assert_eq!(head_parts.len(), 2);
    assert_eq!(head_parts[0], name);
    written(
        head_parts[1],
        source,
        "type_arguments",
        &format!("<{}>", family.parameters.join(", ")),
    );
    let parameters = children(head_parts[1]);
    assert_eq!(parameters.len(), family.parameters.len());
    for (parameter, text) in parameters.iter().zip(family.parameters) {
        written(*parameter, source, "type_argument_defn", text);
        let argument = children(*parameter);
        assert_eq!(argument.len(), 1);
        type_argument(argument[0], source, text);
        direct(*parameter, &argument);
    }
    direct(head_parts[1], &parameters);
    direct(head, &head_parts);
    let block = field(alias, "block");
    direct(alias, &[head, block]);
    assert_eq!(block.start_byte(), source.find(&expected.text()).unwrap());
    assert_eq!(alias.end_byte(), block.end_byte());
    // Published generic abbreviations bypass `_type` at their aliased root.
    // Parenthesized/flexible entry and all real canonical descendants retain it.
    let mut canonical = if family.canonical_root {
        vec![block]
    } else {
        Vec::new()
    };
    shape(block, source, expected, &mut canonical);
    let following = if signature {
        let value = exact(tree, source, "value_definition", "val following: int");
        let left = exact(tree, source, "value_declaration_left", "following");
        let spec = exact(tree, source, "curried_spec", "int");
        direct(value, &[left, spec]);
        assert!(value.child_by_field_name("body").is_none());
        let terminal = children(spec);
        assert_eq!(terminal.len(), 1);
        written(terminal[0], source, "simple_type", "int");
        direct(spec, &terminal);
        value
    } else {
        let wrapper = exact(tree, source, "declaration_expression", "let following = 30");
        let value = exact(tree, source, "function_or_value_defn", "let following = 30");
        let left = exact(tree, source, "value_declaration_left", "following");
        let body = field(value, "body");
        written(body, source, "const", "30");
        let number = children(body);
        assert_eq!(number.len(), 1);
        written(number[0], source, "int", "30");
        direct(body, &number);
        direct(value, &[left, body]);
        direct(wrapper, &[value]);
        wrapper
    };
    assert_eq!(following.parent(), Some(module));
    assert!(
        block.end_byte() <= definition.end_byte() && definition.end_byte() < following.start_byte()
    );
    assert!(
        source
            .get(block.end_byte()..definition.end_byte())
            .unwrap()
            .chars()
            .all(char::is_whitespace)
    );
    direct(module, &[module_name, definition, following]);
    assert_eq!(
        nodes(tree)
            .iter()
            .filter(|node| node.kind() == "type_definition")
            .count(),
        1
    );
    assert_eq!(
        nodes(tree)
            .iter()
            .filter(|node| node.kind() == "type_abbrev_defn")
            .count(),
        1
    );
    if !signature {
        canonical.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind()));
        assert_eq!(
            type_captures(tree, source),
            canonical,
            "canonical type ancestry differs"
        );
    }
    qualified_query(tree, source, signature);
}

fn variants(fixture: &str, mut run: impl FnMut(&str)) {
    assert!(!fixture.contains('\r'), "canonical source must use LF");
    let eof = fixture
        .strip_suffix('\n')
        .expect("canonical source must end in LF");
    for newline in ["\n", "\r\n"] {
        for source in [eof, fixture] {
            run(&source.replace('\n', newline));
        }
    }
}

fn family(index: usize) {
    let family = &FAMILIES[index];
    for signature in [false, true] {
        variants(
            if signature {
                family.signature
            } else {
                family.implementation
            },
            |source| {
                let tree = parse(&mut parser(signature), source, None);
                check(&tree, source, signature, family, family.shape);
            },
        );
    }
}

fn complete(tree: &Tree, source: &str, clean: bool) {
    assert_eq!(tree.root_node().kind(), "file");
    assert_eq!(tree.root_node().start_byte(), 0);
    assert_eq!(tree.root_node().end_byte(), source.len());
    assert_eq!(
        tree.root_node().has_error(),
        !clean,
        "{source:?}: {}",
        tree.root_node().to_sexp()
    );
    for node in nodes(tree) {
        if clean {
            assert!(!node.is_error() && !node.is_missing());
        }
        assert!(node.start_byte() <= node.end_byte() && node.end_byte() <= source.len());
        assert!(source.get(node.byte_range()).is_some());
        assert_eq!(node.start_position(), point(source, node.start_byte()));
        assert_eq!(node.end_position(), point(source, node.end_byte()));
        if let Some(parent) = node.parent() {
            assert!(
                parent.start_byte() <= node.start_byte() && node.end_byte() <= parent.end_byte()
            );
        }
    }
}

fn following<'tree>(tree: &'tree Tree, source: &str, signature: bool) -> Node<'tree> {
    if signature {
        let value = exact(tree, source, "value_definition", "val following: int");
        let left = exact(tree, source, "value_declaration_left", "following");
        let spec = exact(tree, source, "curried_spec", "int");
        direct(value, &[left, spec]);
        assert!(value.child_by_field_name("body").is_none());
        let terminal = children(spec);
        assert_eq!(terminal.len(), 1);
        written(terminal[0], source, "simple_type", "int");
        direct(spec, &terminal);
        value
    } else {
        let wrapper = exact(tree, source, "declaration_expression", "let following = 30");
        let value = exact(tree, source, "function_or_value_defn", "let following = 30");
        let left = exact(tree, source, "value_declaration_left", "following");
        let body = field(value, "body");
        written(body, source, "const", "30");
        let number = children(body);
        assert_eq!(number.len(), 1);
        written(number[0], source, "int", "30");
        direct(body, &number);
        direct(value, &[left, body]);
        direct(wrapper, &[value]);
        wrapper
    }
}

fn composition(expected: &'static Shape) {
    let implementation = format!(
        "module TypeExample\ntype Nested<'T> = {}\nlet following = 30\n",
        expected.text()
    );
    let signature = format!(
        "module TypeExample\ntype Nested<'T> = {}\nval following: int\n",
        expected.text()
    );
    let family = Family {
        implementation: "",
        signature: "",
        head: "Nested<'T>",
        parameters: &["'T"],
        shape: expected,
        canonical_root: false,
    };
    for signature_profile in [false, true] {
        variants(
            if signature_profile {
                &signature
            } else {
                &implementation
            },
            |source| {
                let tree = parse(&mut parser(signature_profile), source, None);
                check(&tree, source, signature_profile, &family, expected);
            },
        );
    }
}

#[test]
fn qualified_nullable_alias_keeps_the_whole_head_before_nullness() {
    composition(&NULLABLE);
}

#[test]
fn qualified_function_alias_keeps_the_head_and_canonical_return_type() {
    composition(&FUNCTION);
}

#[test]
fn qualified_compound_alias_keeps_the_head_and_canonical_later_operand() {
    composition(&COMPOUND);
}

#[test]
fn ordinary_typed_argument_and_return_keep_canonical_qualified_roots() {
    for expected in [&BASE, &PATH, &TRAILING_INT] {
        let written_type = expected.text();
        for signature in [false, true] {
            let fixture = if signature {
                format!(
                    "module TypeExample\nval create: {written_type} -> {written_type}\nval following: int\n"
                )
            } else {
                format!(
                    "module TypeExample\nlet create (input: {written_type}) : {written_type} = input\nlet following = 30\n"
                )
            };
            variants(&fixture, |source| {
                let tree = parse(&mut parser(signature), source, None);
                complete(&tree, source, true);
                let module = nodes(&tree)
                    .into_iter()
                    .filter(|node| node.kind() == "named_module")
                    .collect::<Vec<_>>();
                assert_eq!(module.len(), 1);
                let module = module[0];
                assert_eq!(module.parent(), Some(tree.root_node()));
                let module_name = field(module, "name");
                path(module_name, source, "TypeExample");
                let qualified = nodes(&tree)
                    .into_iter()
                    .filter(|node| node.kind() == "qualified_type")
                    .collect::<Vec<_>>();
                assert_eq!(qualified.len(), 2);
                let offsets: Vec<_> = source
                    .match_indices(written_type.as_str())
                    .map(|(offset, _)| offset)
                    .collect();
                assert_eq!(offsets.len(), 2);
                let mut canonical = vec![qualified[0], qualified[1]];
                for (node, offset) in qualified.iter().zip(offsets) {
                    assert_eq!(node.start_byte(), offset);
                    shape(*node, source, expected, &mut canonical);
                }
                let first = if signature {
                    let value = exact(
                        &tree,
                        source,
                        "value_definition",
                        &format!("val create: {written_type} -> {written_type}"),
                    );
                    let left = exact(&tree, source, "value_declaration_left", "create");
                    let spec = exact(
                        &tree,
                        source,
                        "curried_spec",
                        &format!("{written_type} -> {written_type}"),
                    );
                    let arguments = exact(&tree, source, "arguments_spec", &written_type);
                    let argument = exact(&tree, source, "argument_spec", &written_type);
                    direct(argument, &[qualified[0]]);
                    direct(arguments, &[argument]);
                    direct(spec, &[arguments, qualified[1]]);
                    direct(value, &[left, spec]);
                    assert!(value.child_by_field_name("body").is_none());
                    value
                } else {
                    let text =
                        format!("let create (input: {written_type}) : {written_type} = input");
                    let wrapper = exact(&tree, source, "declaration_expression", &text);
                    let value = exact(&tree, source, "function_or_value_defn", &text);
                    let left = exact(
                        &tree,
                        source,
                        "function_declaration_left",
                        &format!("create (input: {written_type})"),
                    );
                    let args = exact(
                        &tree,
                        source,
                        "argument_patterns",
                        &format!("(input: {written_type})"),
                    );
                    let typed = exact(
                        &tree,
                        source,
                        "typed_pattern",
                        &format!("input: {written_type}"),
                    );
                    let pattern = exact(&tree, source, "identifier_pattern", "input");
                    let name = exact(&tree, source, "identifier", "create");
                    direct(typed, &[pattern, qualified[0]]);
                    direct(args, &[typed]);
                    direct(left, &[name, args]);
                    let body = field(value, "body");
                    written(body, source, "long_identifier_or_op", "input");
                    direct(value, &[left, qualified[1], body]);
                    direct(wrapper, &[value]);
                    canonical.sort_by_key(|node| (node.start_byte(), node.end_byte(), node.kind()));
                    assert_eq!(type_captures(&tree, source), canonical);
                    wrapper
                };
                let after = following(&tree, source, signature);
                assert_eq!(first.parent(), Some(module));
                assert_eq!(after.parent(), Some(module));
                assert!(first.end_byte() < after.start_byte());
                direct(module, &[module_name, first, after]);
                qualified_query(&tree, source, signature);
            });
        }
    }
}

#[test]
fn incomplete_or_malformed_qualified_suffixes_require_recovery() {
    for rhs in [
        "Container<'T>.",
        "Container<'T>..Builder",
        "Container<'T>.Builder<'U",
    ] {
        for signature in [false, true] {
            let after = if signature {
                "val following: int"
            } else {
                "let following = 30"
            };
            let fixture = format!("module TypeExample\ntype Nested<'T, 'U> = {rhs}\n{after}\n");
            variants(&fixture, |source| {
                let tree = parse(&mut parser(signature), source, None);
                complete(&tree, source, false);
                assert!(
                    nodes(&tree)
                        .iter()
                        .any(|node| node.is_error() || node.is_missing())
                );
                // This control requires visible recovery, not later-declaration retention.
            });
        }
    }
}

#[test]
fn constructed_generic_base_is_not_flattened_into_a_dotted_name() {
    family(0);
}

#[test]
fn qualified_path_keeps_every_suffix_identifier_under_one_name_field() {
    family(1);
}

#[test]
fn suffix_arguments_belong_to_the_qualified_segment_not_its_base() {
    family(2);
}

#[test]
fn repeated_constructed_segments_keep_recursive_base_ownership() {
    family(3);
}

#[test]
fn parenthesized_constructed_base_keeps_its_canonical_inner_type() {
    family(4);
}

#[test]
fn flexible_constraint_wraps_the_complete_qualified_type() {
    family(5);
}

#[derive(Debug, PartialEq, Eq)]
struct Witness {
    kind: String,
    span: Range<usize>,
    start: Point,
    end: Point,
    depth: usize,
    field: Option<String>,
    named: bool,
    extra: bool,
    missing: bool,
    error: bool,
}

fn inventory(tree: &Tree) -> Vec<Witness> {
    let mut pending = vec![(tree.root_node(), 0_usize, None)];
    let mut result = Vec::new();
    while let Some((node, depth, field)) = pending.pop() {
        assert!(result.len() < 10_000, "inventory exceeds its bound");
        result.push(Witness {
            kind: node.kind().to_owned(),
            span: node.byte_range(),
            start: node.start_position(),
            end: node.end_position(),
            depth,
            field,
            named: node.is_named(),
            extra: node.is_extra(),
            missing: node.is_missing(),
            error: node.is_error(),
        });
        for index in (0..node.child_count()).rev() {
            let index = u32::try_from(index).unwrap();
            pending.push((
                node.child(index).unwrap(),
                depth + 1,
                node.field_name_for_child(index).map(str::to_owned),
            ));
        }
    }
    result
}

#[test]
fn length_changing_suffix_edit_and_restore_match_fresh_source_ownership() {
    let family = &FAMILIES[3];
    for signature in [false, true] {
        variants(
            if signature {
                family.signature
            } else {
                family.implementation
            },
            |source| {
                let mut reused = parser(signature);
                let original = parse(&mut reused, source, None);
                check(&original, source, signature, family, family.shape);
                let offset = source.find("Builder").unwrap();
                let changed = source.replacen("Builder", "LongerBuilder", 1);
                let mut edited = original.clone();
                edited.edit(&InputEdit {
                    start_byte: offset,
                    old_end_byte: offset + "Builder".len(),
                    new_end_byte: offset + "LongerBuilder".len(),
                    start_position: point(source, offset),
                    old_end_position: point(source, offset + "Builder".len()),
                    new_end_position: point(&changed, offset + "LongerBuilder".len()),
                });
                let mut incremental = parse(&mut reused, &changed, Some(&edited));
                let fresh = parse(&mut parser(signature), &changed, None);
                check(&incremental, &changed, signature, family, &EDITED_CHAIN);
                check(&fresh, &changed, signature, family, &EDITED_CHAIN);
                assert_eq!(inventory(&incremental), inventory(&fresh));
                incremental.edit(&InputEdit {
                    start_byte: offset,
                    old_end_byte: offset + "LongerBuilder".len(),
                    new_end_byte: offset + "Builder".len(),
                    start_position: point(&changed, offset),
                    old_end_position: point(&changed, offset + "LongerBuilder".len()),
                    new_end_position: point(source, offset + "Builder".len()),
                });
                let restored = parse(&mut reused, source, Some(&incremental));
                let fresh = parse(&mut parser(signature), source, None);
                check(&restored, source, signature, family, family.shape);
                assert_eq!(inventory(&restored), inventory(&fresh));
                assert_eq!(inventory(&restored), inventory(&original));
            },
        );
    }
}
