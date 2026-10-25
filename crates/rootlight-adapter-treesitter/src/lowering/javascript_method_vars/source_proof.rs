//! Reparse current logical bytes before trusting a native method-var label.
//! This census owns no producer certificate: it reconstructs method, declaration,
//! name and exact operand geometry from the current complete source.

use std::{collections::HashMap, ops::ControlFlow};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::{Node, ParseOptions};

#[derive(Clone, Copy)]
pub(super) struct Proof {
    pub(super) declaration: (usize, usize),
    pub(super) definition: (usize, usize),
    pub(super) method: (usize, usize),
    pub(super) method_name: (usize, usize),
    pub(super) parameters: (usize, usize),
    pub(super) body: (usize, usize),
    pub(super) class: (usize, usize),
    pub(super) class_name: (usize, usize),
    pub(super) context: (usize, usize),
    pub(super) initializer: bool,
}

fn extent(node: Node<'_>) -> (usize, usize) {
    (node.start_byte(), node.end_byte())
}

fn word<'a>(
    node: Node<'_>,
    source: &'a [u8],
    cancel: &Cancellation,
) -> Result<Option<&'a str>, AdapterError> {
    let Some(raw) = source.get(node.byte_range()) else {
        return Ok(None);
    };
    if !raw
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'$'))
    {
        return Ok(None);
    }
    for chunk in raw.chunks(64) {
        cancel.check()?;
        if !chunk
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
        {
            return Ok(None);
        }
    }
    Ok(std::str::from_utf8(raw).ok())
}

fn contains(parent: Node<'_>, child: Node<'_>) -> bool {
    parent.start_byte() <= child.start_byte() && parent.end_byte() >= child.end_byte()
}

fn binder(node: Node<'_>, body: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
    if node.kind() == "shorthand_property_identifier_pattern" {
        return Ok(true);
    }
    let mut child = node;
    while let Some(parent) = child.parent() {
        cancel.check()?;
        if parent == body {
            break;
        }
        if parent.kind() == "variable_declarator" {
            return Ok(parent
                .child_by_field_name("name")
                .is_some_and(|name| contains(name, node)));
        }
        if matches!(
            parent.kind(),
            "array_pattern" | "object_pattern" | "rest_pattern"
        ) {
            return Ok(true);
        }
        if parent.kind() == "catch_clause" {
            return Ok(parent
                .child_by_field_name("parameter")
                .is_some_and(|name| contains(name, node)));
        }
        if parent.kind() == "for_in_statement" {
            return Ok(parent
                .child_by_field_name("left")
                .is_some_and(|name| contains(name, node)));
        }
        child = parent;
    }
    Ok(false)
}

fn declaration<'tree>(name: Node<'tree>, body: Node<'tree>) -> Option<Node<'tree>> {
    let declaration = name.parent()?;
    let statement = declaration.parent()?;
    if name.kind() != "identifier"
        || declaration.kind() != "variable_declarator"
        || declaration.child_by_field_name("name") != Some(name)
        || declaration.child_by_field_name("value").is_none()
        || statement.kind() != "variable_declaration"
        || statement.parent() != Some(body)
        || statement.child(0)?.kind() != "var"
    {
        return None;
    }
    let mut cursor = statement.walk();
    (statement
        .named_children(&mut cursor)
        .filter(|node| !node.is_extra())
        .count()
        == 1)
        .then_some(declaration)
}

fn context(node: Node<'_>, body: Node<'_>) -> Option<((usize, usize), bool)> {
    let binary = node.parent()?;
    if binary.kind() != "binary_expression"
        || (binary.child_by_field_name("left") != Some(node)
            && binary.child_by_field_name("right") != Some(node))
    {
        return None;
    }
    let parent = binary.parent()?;
    if parent.kind() == "variable_declarator" && parent.child_by_field_name("value") == Some(binary)
    {
        declaration(parent.child_by_field_name("name")?, body)?;
        return Some((extent(parent), true));
    }
    if parent.kind() == "return_statement" && parent.parent() == Some(body) {
        let mut cursor = parent.walk();
        let mut children = parent
            .named_children(&mut cursor)
            .filter(|child| !child.is_extra());
        if children.next() == Some(binary) && children.next().is_none() {
            return Some((extent(body), false));
        }
    }
    None
}

struct Work {
    remaining: usize,
    exhausted: bool,
}

impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        match self.remaining.checked_sub(1) {
            Some(next) => {
                self.remaining = next;
                Ok(true)
            }
            None => {
                self.exhausted = true;
                Ok(false)
            }
        }
    }
}

fn method(
    method: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<HashMap<(usize, usize), Proof>, AdapterError> {
    let mut result = HashMap::new();
    let Some(class_body) = method.parent().filter(|node| node.kind() == "class_body") else {
        return Ok(result);
    };
    let Some(class) = class_body
        .parent()
        .filter(|node| node.kind() == "class_declaration")
    else {
        return Ok(result);
    };
    let Some(parent) = class.parent() else {
        return Ok(result);
    };
    if parent.kind() != "program"
        && !(parent.kind() == "export_statement"
            && parent.child_by_field_name("declaration") == Some(class)
            && parent.parent().is_some_and(|node| node.kind() == "program"))
    {
        return Ok(result);
    }
    let Some(class_name) = class.child_by_field_name("name") else {
        return Ok(result);
    };
    let Some(method_name) = method.child_by_field_name("name") else {
        return Ok(result);
    };
    let Some(parameters) = method.child_by_field_name("parameters") else {
        return Ok(result);
    };
    let Some(body) = method.child_by_field_name("body") else {
        return Ok(result);
    };
    if class.child_by_field_name("body") != Some(class_body)
        || !matches!(method_name.kind(), "identifier" | "property_identifier")
        || parameters.kind() != "formal_parameters"
        || body.kind() != "statement_block"
        || word(class_name, source, cancel)?.is_none()
        || word(method_name, source, cancel)?.is_none()
    {
        return Ok(result);
    }
    let mut cursor = method.walk();
    if method
        .children(&mut cursor)
        .filter(|node| !node.is_extra())
        .any(|node| node != method_name && node != parameters && node != body)
    {
        return Ok(result);
    }
    let mut cursor = parameters.walk();
    if parameters
        .named_children(&mut cursor)
        .any(|node| !node.is_extra())
    {
        return Ok(result);
    }
    let mut bindings = HashMap::<&str, (usize, Option<Node<'_>>)>::new();
    let mut sites = Vec::new();
    let mut cursor = body.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(HashMap::new());
        }
        let node = cursor.node();
        if node.is_error()
            || node.is_missing()
            || matches!(
                node.kind(),
                "with_statement"
                    | "function_declaration"
                    | "function_expression"
                    | "generator_function_declaration"
                    | "generator_function"
                    | "arrow_function"
                    | "method_definition"
                    | "class_declaration"
                    | "class"
                    | "type_annotation"
                    | "function_signature"
                    | "abstract_class_declaration"
                    | "module"
                    | "type_alias_declaration"
                    | "enum_declaration"
                    | "interface_declaration"
                    | "import_alias"
                    | "ambient_declaration"
                    | "internal_module"
                    | "namespace_declaration"
            )
        {
            return Ok(HashMap::new());
        }
        if matches!(
            node.kind(),
            "identifier" | "shorthand_property_identifier_pattern"
        ) {
            let Some(name) = word(node, source, cancel)? else {
                return Ok(HashMap::new());
            };
            if name == "eval" {
                return Ok(HashMap::new());
            }
            if binder(node, body, cancel)? {
                bindings
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                let row = bindings.entry(name).or_default();
                row.0 = row.0.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                row.1 = declaration(node, body);
            }
            if let Some((context, initializer)) = context(node, body) {
                sites
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                sites.push((node, name, context, initializer));
            }
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                for (site, name, context, initializer) in sites {
                    cancel.check()?;
                    let Some((1, Some(declaration))) = bindings.get(name) else {
                        continue;
                    };
                    if declaration.end_byte() > site.start_byte() {
                        continue;
                    }
                    let Some(definition) = declaration.child_by_field_name("name") else {
                        continue;
                    };
                    result
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    result.insert(
                        extent(site),
                        Proof {
                            declaration: extent(*declaration),
                            definition: extent(definition),
                            method: extent(method),
                            method_name: extent(method_name),
                            parameters: extent(parameters),
                            body: extent(body),
                            class: extent(class),
                            class_name: extent(class_name),
                            context,
                            initializer,
                        },
                    );
                }
                return Ok(result);
            }
        }
    }
}

pub(super) fn collect(
    source: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<HashMap<(usize, usize), Proof>, AdapterError> {
    cancel.check()?;
    let mut result = HashMap::new();
    if maximum == 0 {
        return Ok(result);
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
        .map_err(|_| super::super::provider_failure("javascript-method-var-consumer-grammar"))?;
    let mut parse_remaining = maximum;
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancel.check().is_err() {
            return ControlFlow::Break(());
        }
        match parse_remaining.checked_sub(1) {
            Some(next) => {
                parse_remaining = next;
                ControlFlow::Continue(())
            }
            None => {
                exhausted = true;
                ControlFlow::Break(())
            }
        }
    };
    let mut input = |at: usize, _: tree_sitter::Point| source.get(at..).unwrap_or_default();
    let tree = parser.parse_with_options(
        &mut input,
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    cancel.check()?;
    if exhausted {
        return Ok(result);
    }
    let Some(tree) = tree else { return Ok(result) };
    let root = tree.root_node();
    if root.kind() != "program"
        || root.has_error()
        || root.is_missing()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(result);
    }
    let mut work = Work {
        remaining: parse_remaining,
        exhausted: false,
    };
    let mut methods = Vec::new();
    let mut cursor = root.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(HashMap::new());
        }
        let node = cursor.node();
        if node.kind() == "method_definition" {
            methods
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            methods.push(node);
        }
        if cursor.goto_first_child() {
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                for owner in methods {
                    let proofs = method(owner, source, &mut work, cancel)?;
                    if work.exhausted {
                        return Ok(HashMap::new());
                    }
                    result
                        .try_reserve(proofs.len())
                        .map_err(|_| SinkError::AllocationFailed)?;
                    result.extend(proofs);
                }
                return Ok(result);
            }
        }
    }
}

#[cfg(test)]
mod qualification_tests {
    use super::*;

    const ORIGINAL: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/javascript_method_var_reads/curve.babel.js"
    ));

    #[test]
    fn complete_original_current_census_keeps_method_name_target_and_initializer_geometry() {
        let proofs = collect(ORIGINAL, 16_384, &Cancellation::new()).unwrap();
        let proof = proofs
            .get(&(511, 518))
            .expect("complete original lexical operand");
        assert_eq!(proof.definition, (374, 381));
        assert_eq!(proof.declaration, (374, 426));
        assert_eq!(proof.method_name, (311, 316));
        assert_eq!(proof.method, (311, 1118));
        assert_eq!(proof.body, (319, 1118));
        assert_eq!(proof.class_name, (37, 42));
        assert_eq!(proof.context, (499, 540));
        assert!(proof.initializer);
        assert_ne!(
            proof.context, proof.method,
            "initializer ownership cannot become callable ownership"
        );
    }

    #[test]
    fn current_census_refuses_competing_bindings_formals_dynamic_and_nonordinary_owners() {
        for source in [
            "class A { run(){ var local=1; var local=2; return local === other; } }",
            "class A { run(){ let local=1; return local === other; } }",
            "class A { run(local){ var local=1; return local === other; } }",
            "class A { run(){ var local=1; eval(code); return local === other; } }",
            "class A { run(){ var local=1; with(other){} return local === other; } }",
            "class A { run(){ var local=1; function nested(){} return local === other; } }",
            "class A { run(){ var local=1; { let local=2; } return local === other; } }",
            "class A { static run(){ var local=1; return local === other; } }",
            "const A = class { run(){ var local=1; return local === other; } };",
            "class A { run(){ var local=1; return local === ; } }",
        ] {
            assert!(
                collect(source.as_bytes(), 16_384, &Cancellation::new())
                    .unwrap()
                    .is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn current_census_work_exhaustion_and_cancel_cannot_publish_partial_proofs() {
        assert!(
            collect(ORIGINAL, 0, &Cancellation::new())
                .unwrap()
                .is_empty()
        );
        assert!(
            collect(ORIGINAL, 1, &Cancellation::new())
                .unwrap()
                .is_empty()
        );
        let cancel = Cancellation::new();
        assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(collect(ORIGINAL, 16_384, &cancel).is_err());
    }
}

#[cfg(test)]
mod tsx_declaration_refusal_tests {
    use super::*;

    #[test]
    fn fresh_method_var_census_refuses_complete_tsx_declaration_family() {
        let ordinary = b"class A { run(){ var local=1; return local === other; } }";
        let start = std::str::from_utf8(ordinary)
            .unwrap()
            .rfind("return local")
            .unwrap()
            + 7;
        assert!(
            collect(ordinary, 16_384, &Cancellation::new())
                .unwrap()
                .contains_key(&(start, start + 5))
        );
        for (kind, source) in [
            (
                "function_signature",
                "class A { run(){ var local=1; function local(); return local === other; } }",
            ),
            (
                "abstract_class_declaration",
                "class A { run(){ var local=1; abstract class local {} return local === other; } }",
            ),
            (
                "module",
                "class A { run(){ var local=1; module local {} return local === other; } }",
            ),
            (
                "internal_module",
                "class A { run(){ var local=1; namespace local {} return local === other; } }",
            ),
            (
                "type_alias_declaration",
                "class A { run(){ var local=1; type local = number; return local === other; } }",
            ),
            (
                "enum_declaration",
                "class A { run(){ var local=1; enum local { X } return local === other; } }",
            ),
            (
                "interface_declaration",
                "class A { run(){ var local=1; interface local {} return local === other; } }",
            ),
            (
                "import_alias",
                "class A { run(){ var local=1; import local = other; return local === other; } }",
            ),
            (
                "ambient_declaration",
                "class A { run(){ var local=1; declare global {} return local === other; } }",
            ),
            (
                "enum_declaration",
                "class A { run(){ var local=1; const enum local { X } return local === other; } }",
            ),
            (
                "module",
                "class A { run(){ var local=1; module local.nested {} return local === other; } }",
            ),
            (
                "module",
                "class A { run(){ var local=1; module \"local\" {} return local === other; } }",
            ),
            (
                "import_alias",
                "class A { run(){ var local=1; import local = other.member; return local === other; } }",
            ),
        ] {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
                .unwrap();
            let tree = parser.parse(source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "{source}: {}",
                tree.root_node().to_sexp()
            );
            assert!(
                tree.root_node()
                    .to_sexp()
                    .contains(format!("({kind} ").as_str()),
                "expected {kind}: {source}"
            );
            assert!(
                collect(source.as_bytes(), 16_384, &Cancellation::new())
                    .unwrap()
                    .is_empty(),
                "{kind}: {source}"
            );
        }
    }
}
