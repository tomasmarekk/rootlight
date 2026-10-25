//! Certify a bare default-export identifier bound to a written module Class.
//! The closed module profile excludes other lexical and hoisted binding forms;
//! it proves a source reference, not the runtime value of an export.

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

pub(super) struct CaptureCache<'source, 'tree> {
    source: &'source [u8],
    root: Node<'tree>,
    remaining: usize,
    complete: bool,
    initialized: bool,
    exhausted: bool,
    site: Option<(usize, usize)>,
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

impl<'source, 'tree> CaptureCache<'source, 'tree> {
    pub(super) fn new(
        source: &'source [u8],
        root: Node<'tree>,
        maximum: usize,
        complete: bool,
    ) -> Self {
        Self {
            source,
            root,
            remaining: maximum,
            complete,
            initialized: false,
            exhausted: false,
            site: None,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            self.site = None;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn word(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source [u8]>, AdapterError> {
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        if reserved(raw) {
            return Ok(None);
        }
        if !raw
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || matches!(b, b'_' | b'$'))
        {
            return Ok(None);
        }
        for chunk in raw.chunks(64) {
            if !self.step(cancel)?
                || !chunk
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'$'))
            {
                return Ok(None);
            }
        }
        Ok(Some(raw))
    }

    fn initialize(&mut self, cancel: &Cancellation) -> Result<(), AdapterError> {
        self.initialized = true;
        if !self.complete
            || self.root.kind() != "program"
            || self.root.has_error()
            || self.root.is_missing()
            || self.root.end_byte() != self.source.len()
            || !matches!(
                self.source.get(..self.root.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            )
        {
            return Ok(());
        }
        let mut value = None;
        let mut cursor = self.root.walk();
        for statement in self.root.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(());
            }
            match statement.kind() {
                "comment" | "empty_statement" | "import_statement" | "class_declaration" => {}
                "export_statement" => {
                    let Some(identifier) = statement
                        .child_by_field_name("value")
                        .filter(|node| node.kind() == "identifier")
                    else {
                        return Ok(());
                    };
                    if statement.child_by_field_name("declaration").is_some()
                        || value.replace(identifier).is_some()
                    {
                        return Ok(());
                    }
                    let mut children = statement.walk();
                    let mut defaults = 0_usize;
                    for child in statement.children(&mut children) {
                        if !self.step(cancel)? {
                            return Ok(());
                        }
                        match child.kind() {
                            "export" | ";" | "comment" => {}
                            "default" => defaults += 1,
                            "identifier" if child == identifier => {}
                            _ => return Ok(()),
                        }
                    }
                    if defaults != 1 {
                        return Ok(());
                    }
                }
                // A root block may hoist var/function bindings. TSX declarations
                // and other statements are not admitted by this lexical proof.
                _ => return Ok(()),
            }
        }
        let Some(value) = value else { return Ok(()) };
        let Some(spelling) = self.word(value, cancel)? else {
            return Ok(());
        };
        let mut matching_classes = 0_usize;
        let mut cursor = self.root.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(());
            }
            let node = cursor.node();
            if node.is_error() || node.is_missing() {
                return Ok(());
            }
            if node.kind() == "class_declaration" && node.parent() == Some(self.root) {
                let (Some(name), Some(body)) = (
                    node.child_by_field_name("name"),
                    node.child_by_field_name("body"),
                ) else {
                    return Ok(());
                };
                if name.kind() != "type_identifier" || body.kind() != "class_body" {
                    return Ok(());
                }
                let mut children = node.walk();
                for child in node.named_children(&mut children) {
                    if !self.step(cancel)? {
                        return Ok(());
                    }
                    if child.kind() == "class_heritage" && !self.heritage(child, cancel)? {
                        return Ok(());
                    }
                    if child != name
                        && child != body
                        && child.kind() != "class_heritage"
                        && !child.is_extra()
                    {
                        return Ok(());
                    }
                }
                let Some(class_name) = self.word(name, cancel)? else {
                    return Ok(());
                };
                if class_name == spelling {
                    matching_classes += 1
                }
            }
            if matches!(node.kind(), "identifier" | "type_identifier") {
                let mut ancestor = node.parent();
                while let Some(parent) = ancestor {
                    if !self.step(cancel)? {
                        return Ok(());
                    }
                    if parent == self.root {
                        break;
                    }
                    if parent.kind() == "import_statement" {
                        let Some(imported) = self.word(node, cancel)? else {
                            return Ok(());
                        };
                        if imported == spelling {
                            return Ok(());
                        }
                    }
                    ancestor = parent.parent();
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
                    if matching_classes == 1 {
                        self.site = Some((value.start_byte(), value.end_byte()))
                    }
                    return Ok(());
                }
            }
        }
    }

    pub(super) fn certified(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if !self.initialized {
            self.initialize(cancel)?
        }
        Ok(!self.exhausted && self.site == Some((node.start_byte(), node.end_byte())))
    }

    fn heritage(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        let mut cursor = node.walk();
        let mut clauses = node
            .named_children(&mut cursor)
            .filter(|node| !node.is_extra());
        let Some(clause) = clauses
            .next()
            .filter(|node| node.kind() == "extends_clause")
        else {
            return Ok(false);
        };
        if clauses.next().is_some() || !self.step(cancel)? {
            return Ok(false);
        }
        let Some(value) = clause
            .child_by_field_name("value")
            .filter(|node| node.kind() == "identifier")
        else {
            return Ok(false);
        };
        let mut cursor = clause.walk();
        for child in clause.named_children(&mut cursor) {
            if !self.step(cancel)? || (child != value && !child.is_extra()) {
                return Ok(false);
            }
        }
        Ok(self.word(value, cancel)?.is_some())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn selected(source: &[u8], maximum: usize, complete: bool) -> (bool, bool) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let start = source
            .windows(b"export default ".len())
            .position(|part| part == b"export default ")
            .unwrap()
            + 15;
        let node = tree
            .root_node()
            .descendant_for_byte_range(start, start + 1)
            .unwrap();
        let mut cache = CaptureCache::new(source, tree.root_node(), maximum, complete);
        let result = cache.certified(node, &Cancellation::new()).unwrap();
        (result, cache.exhausted())
    }

    #[test]
    fn whole_original_export_and_renamed_class_are_native_certificates() {
        let source = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/javascript_method_var_reads/curve.babel.js"
        ));
        assert_eq!(selected(source, 16_384, true), (true, false));
        assert_eq!(
            selected(b"class Changed{} export default Changed;", 1024, true),
            (true, false)
        );
    }

    #[test]
    fn lexical_import_hoist_and_tsx_declaration_competitors_are_not_class_authority() {
        for extra in [
            "var Name;",
            "let other;",
            "function other(){}",
            "{var Name;}",
            "import Name from 'other';",
            "enum Name{X}",
            "namespace Name{}",
            "interface Name{}",
            "type Name=number;",
            "declare const Name:number;",
            "import Name=other.member;",
            "abstract class Other{}",
            "function Name():void;",
        ] {
            let source = format!("class Name{{}} {extra} export default Name;");
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
                .unwrap();
            let tree = parser.parse(source.as_bytes(), None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "native prerequisite: {source}"
            );
            assert_eq!(
                selected(source.as_bytes(), 16_384, true),
                (false, false),
                "{source}"
            );
        }
    }

    #[test]
    fn partial_source_work_exhaustion_dirty_and_expression_exports_fail_closed() {
        let source = b"class Name{} export default Name;";
        assert_eq!(selected(source, 1024, false), (false, false));
        assert_eq!(selected(source, 0, true), (false, true));
        for source in [
            b"class Name{} export default (Name);".as_slice(),
            b"class Name{ export default Name;".as_slice(),
        ] {
            assert!(!selected(source, 1024, true).0);
        }
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_typescript::LANGUAGE_TSX.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let cancel = Cancellation::new();
        assert!(cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        let mut cache = CaptureCache::new(source, tree.root_node(), 1024, true);
        assert!(matches!(
            cache.certified(tree.root_node(), &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn strict_module_class_bindings_do_not_borrow_property_name_permissions() {
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
            assert!(!selected(source.as_bytes(), 4096, true).0, "{source}");
        }
        for name in ["from", "of", "Changed"] {
            assert!(!reserved(name.as_bytes()));
            let source = format!("class {name}{{ return(){{}} }} export default {name};");
            assert_eq!(
                selected(source.as_bytes(), 4096, true),
                (true, false),
                "{source}"
            );
        }
    }
}
