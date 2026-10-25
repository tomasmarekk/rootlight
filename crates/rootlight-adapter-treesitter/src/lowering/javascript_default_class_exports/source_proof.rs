//! Reconstruct a module Class/default-export join from fresh current TSX bytes.
//! No producer cache or raw certificate supplies this native structural census.

use std::ops::ControlFlow;

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::{Node, ParseOptions};

#[derive(Clone, Copy)]
pub(super) struct Proof {
    pub(super) class: (usize, usize),
    pub(super) name: (usize, usize),
    pub(super) site: (usize, usize),
}

fn extent(node: Node<'_>) -> (usize, usize) {
    (node.start_byte(), node.end_byte())
}

// Class bindings in a module use strict binding names; property names do not.
fn reserved(raw: &[u8]) -> bool {
    matches!(
        raw,
        b"await"
            | b"break"
            | b"case"
            | b"catch"
            | b"class"
            | b"const"
            | b"continue"
            | b"debugger"
            | b"default"
            | b"delete"
            | b"do"
            | b"else"
            | b"enum"
            | b"export"
            | b"extends"
            | b"false"
            | b"finally"
            | b"for"
            | b"function"
            | b"if"
            | b"import"
            | b"in"
            | b"instanceof"
            | b"let"
            | b"new"
            | b"null"
            | b"return"
            | b"super"
            | b"switch"
            | b"this"
            | b"throw"
            | b"true"
            | b"try"
            | b"typeof"
            | b"var"
            | b"void"
            | b"while"
            | b"with"
            | b"yield"
            | b"implements"
            | b"interface"
            | b"package"
            | b"private"
            | b"protected"
            | b"public"
            | b"static"
            | b"eval"
            | b"arguments"
    )
}

struct Work {
    remaining: usize,
    exhausted: bool,
}

impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn word<'a>(
        &mut self,
        node: Node<'_>,
        source: &'a [u8],
        cancel: &Cancellation,
    ) -> Result<Option<&'a [u8]>, AdapterError> {
        let Some(bytes) = source.get(node.byte_range()) else {
            return Ok(None);
        };
        if reserved(bytes) {
            return Ok(None);
        }
        if !bytes
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'$'))
        {
            return Ok(None);
        }
        for chunk in bytes.chunks(64) {
            if !self.step(cancel)?
                || !chunk
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
            {
                return Ok(None);
            }
        }
        Ok(Some(bytes))
    }

    fn heritage(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut count = 0_usize;
        let mut cursor = node.walk();
        for clause in node.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if clause.is_extra() {
                continue;
            }
            count += 1;
            if count != 1 || clause.kind() != "extends_clause" {
                return Ok(false);
            }
            let Some(base) = clause
                .child_by_field_name("value")
                .filter(|node| node.kind() == "identifier")
            else {
                return Ok(false);
            };
            if self.word(base, source, cancel)?.is_none() {
                return Ok(false);
            }
            let mut fields = clause.walk();
            for field in clause.named_children(&mut fields) {
                if !self.step(cancel)? || (field != base && !field.is_extra()) {
                    return Ok(false);
                }
            }
        }
        Ok(count == 1)
    }
}

pub(super) fn collect(
    source: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<Option<Proof>, AdapterError> {
    cancel.check()?;
    if maximum == 0 {
        return Ok(None);
    }
    let mut work = Work {
        remaining: maximum,
        exhausted: false,
    };
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
        .map_err(|_| super::super::provider_failure("javascript-default-class-consumer-grammar"))?;
    let mut progress = |_: &tree_sitter::ParseState| match work.step(cancel) {
        Ok(true) => ControlFlow::Continue(()),
        _ => ControlFlow::Break(()),
    };
    let mut input = |at: usize, _: tree_sitter::Point| source.get(at..).unwrap_or_default();
    let tree = parser.parse_with_options(
        &mut input,
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    cancel.check()?;
    let Some(tree) = tree.filter(|_| !work.exhausted) else {
        return Ok(None);
    };
    let root = tree.root_node();
    if root.kind() != "program"
        || root.has_error()
        || root.is_missing()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(None);
    }
    let mut reference = None;
    let mut cursor = root.walk();
    for statement in root.named_children(&mut cursor) {
        if !work.step(cancel)? {
            return Ok(None);
        }
        match statement.kind() {
            "comment" | "empty_statement" | "class_declaration" | "import_statement" => {}
            "export_statement" => {
                if reference.is_some() || statement.child_by_field_name("declaration").is_some() {
                    return Ok(None);
                }
                let Some(value) = statement
                    .child_by_field_name("value")
                    .filter(|node| node.kind() == "identifier")
                else {
                    return Ok(None);
                };
                let mut children = statement.walk();
                let mut seen_export = false;
                let mut seen_default = false;
                for token in statement.children(&mut children) {
                    if !work.step(cancel)? {
                        return Ok(None);
                    }
                    match token.kind() {
                        "export" if !seen_export => seen_export = true,
                        "default" if seen_export && !seen_default => seen_default = true,
                        "identifier" if seen_default && token == value => {}
                        ";" | "comment" => {}
                        _ => return Ok(None),
                    }
                }
                if !seen_export || !seen_default {
                    return Ok(None);
                }
                reference = Some(value);
            }
            _ => return Ok(None),
        }
    }
    let Some(reference) = reference else {
        return Ok(None);
    };
    let Some(word) = work.word(reference, source, cancel)? else {
        return Ok(None);
    };
    let mut matched = None;
    let mut duplicate = false;
    let mut cursor = root.walk();
    loop {
        if !work.step(cancel)? {
            return Ok(None);
        }
        let node = cursor.node();
        if node.is_missing() || node.is_error() {
            return Ok(None);
        }
        if node.kind() == "import_statement" && node.parent() == Some(root) {
            let mut imported = node.walk();
            loop {
                if !work.step(cancel)? {
                    return Ok(None);
                }
                let part = imported.node();
                if matches!(part.kind(), "identifier" | "type_identifier") {
                    let Some(imported) = work.word(part, source, cancel)? else {
                        return Ok(None);
                    };
                    if imported == word {
                        return Ok(None);
                    }
                }
                if imported.goto_first_child() {
                    continue;
                }
                loop {
                    if imported.goto_next_sibling() {
                        break;
                    }
                    if !imported.goto_parent() {
                        break;
                    }
                }
                if imported.node() == node {
                    break;
                }
            }
        }
        if node.kind() == "class_declaration" && node.parent() == Some(root) {
            let (Some(name), Some(body)) = (
                node.child_by_field_name("name"),
                node.child_by_field_name("body"),
            ) else {
                return Ok(None);
            };
            if name.kind() != "type_identifier" || body.kind() != "class_body" {
                return Ok(None);
            }
            let mut fields = node.walk();
            for part in node.named_children(&mut fields) {
                if !work.step(cancel)? {
                    return Ok(None);
                }
                if part.kind() == "class_heritage" && !work.heritage(part, source, cancel)? {
                    return Ok(None);
                }
                if part != name
                    && part != body
                    && part.kind() != "class_heritage"
                    && !part.is_extra()
                {
                    return Ok(None);
                }
            }
            let Some(class_name) = work.word(name, source, cancel)? else {
                return Ok(None);
            };
            if class_name == word {
                if matched.is_some() {
                    duplicate = true
                }
                matched = Some(Proof {
                    class: extent(node),
                    name: extent(name),
                    site: extent(reference),
                });
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
                return Ok(if duplicate { None } else { matched });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_original_census_proves_exact_class_name_and_export_geometry() {
        let source = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/javascript_method_var_reads/curve.babel.js"
        ));
        let proof = collect(source, 16_384, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert_eq!(proof.class, (31, 1456));
        assert_eq!(proof.name, (37, 42));
        assert_eq!(proof.site, (1473, 1478));
    }

    #[test]
    fn independent_consumer_refuses_root_competitors_and_nonbare_exports() {
        assert!(
            collect(
                b"class Name{} export default Name;",
                1024,
                &Cancellation::new()
            )
            .unwrap()
            .is_some()
        );
        for source in [
            "class Name{} var Name; export default Name;",
            "class Name{} import {other as Name} from 'other'; export default Name;",
            "class Name{} {function Name(){}} export default Name;",
            "function f(){class Name{}} export default Name;",
            "class Name{} export default Name.member;",
            "class Name{} enum Name{X} export default Name;",
        ] {
            assert!(
                collect(source.as_bytes(), 16_384, &Cancellation::new())
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
    }

    #[test]
    fn independent_census_cannot_reuse_partial_work_or_cancelled_source() {
        let source = b"class Name{} export default Name;";
        assert!(collect(source, 0, &Cancellation::new()).unwrap().is_none());
        assert!(
            collect(
                b"class Name{ export default Name;",
                1024,
                &Cancellation::new()
            )
            .unwrap()
            .is_none()
        );
        let cancel = Cancellation::new();
        assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            collect(source, 1024, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn fresh_consumer_applies_strict_binding_names_only_to_the_class_reference() {
        for name in [
            "if",
            "await",
            "yield",
            "implements",
            "package",
            "static",
            "eval",
            "arguments",
        ] {
            assert!(reserved(name.as_bytes()));
            let source = format!("class {name}{{}} export default {name};");
            assert!(
                collect(source.as_bytes(), 4096, &Cancellation::new())
                    .unwrap()
                    .is_none(),
                "{source}"
            );
        }
        for name in ["from", "of", "Changed"] {
            let source = format!("class {name}{{ return(){{}} }} export default {name};");
            assert!(
                collect(source.as_bytes(), 4096, &Cancellation::new())
                    .unwrap()
                    .is_some(),
                "{source}"
            );
        }
    }
}
