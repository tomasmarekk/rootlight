//! Ordinary top-level Ring functions have written identity without a binding proof.
//! Exact native headers are staged independently of the stricter class-method census.

use super::*;

struct Header<'tree, 'source> {
    node: Node<'tree>,
    name: Node<'tree>,
    spelling: &'source str,
    end: usize,
    parameters: Vec<(Node<'tree>, &'source str)>,
}

impl<'source> CaptureCache<'source> {
    fn function_header<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Header<'tree, 'source>>, AdapterError> {
        if node.kind() != "function_definition" || !clean(node, self.source) {
            return Ok(None);
        }
        let Some(name) = node.child_by_field_name("name") else {
            return Ok(None);
        };
        let Some(spelling) = self.bare(name, cancel)? else {
            return Ok(None);
        };
        let Some(keyword) = node.child(0) else {
            return Ok(None);
        };
        if !matches!(keyword.kind(), "func" | "function" | "def")
            || !self
                .source
                .get(keyword.byte_range())
                .is_some_and(|raw| raw.eq_ignore_ascii_case(keyword.kind().as_bytes()))
            || keyword.end_byte() > name.start_byte()
        {
            return Ok(None);
        }
        let mut end = name.end_byte();
        let mut parameters: Vec<(Node<'tree>, &'source str)> = Vec::new();
        let list = node.child_by_field_name("parameters");
        if let Some(list) = list {
            if list.kind() != "param_list"
                || list.parent() != Some(node)
                || !clean(list, self.source)
                || list.start_byte() < name.end_byte()
            {
                return Ok(None);
            }
            let mut cursor = list.walk();
            for parameter in list.named_children(&mut cursor) {
                if !self.visit(cancel)? {
                    return Ok(None);
                }
                let Some(text) = self.bare(parameter, cancel)? else {
                    return Ok(None);
                };
                for (_, previous) in &parameters {
                    if self.equal(text, previous, cancel)? {
                        return Ok(None);
                    }
                }
                parameters
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                parameters.push((parameter, text));
            }
            end = list.end_byte();
        }
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if child == keyword || child == name || Some(child) == list || child.kind() == "comment"
            {
                continue;
            }
            if child.kind() == ")" && child.start_byte() >= end {
                end = child.end_byte();
            } else if child.kind() == "{" || child.is_named() {
                break;
            }
        }
        Ok(Some(Header {
            node,
            name,
            spelling,
            end,
            parameters,
        }))
    }

    pub(super) fn collect_top_level_functions(
        &mut self,
        root: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        let mut has_function = false;
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(());
            }
            if child.kind() == "function_definition" {
                has_function = true;
                break;
            }
        }
        if !has_function {
            return Ok(());
        }
        let mut cursor = root.walk();
        loop {
            if !self.visit(cancel)? {
                return Ok(());
            }
            if matches!(
                cursor.node().kind(),
                "scanner_command" | "package_definition"
            ) {
                return Ok(());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    break;
                }
            }
            if cursor.node() == root {
                break;
            }
        }
        let mut headers = Vec::new();
        let mut cursor = root.walk();
        for node in root.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(());
            }
            if node.kind() != "function_definition" {
                continue;
            }
            let Some(header) = self.function_header(node, cancel)? else {
                continue;
            };
            let mut matches = 0_usize;
            let mut siblings = root.walk();
            for sibling in root.named_children(&mut siblings) {
                if !self.visit(cancel)? {
                    return Ok(());
                }
                if sibling.kind() == "function_definition"
                    && let Some(name) = sibling.child_by_field_name("name")
                    && let Some(text) = self.bare(name, cancel)?
                    && self.equal(header.spelling, text, cancel)?
                {
                    matches = matches
                        .checked_add(1)
                        .ok_or(SinkError::AccountingOverflow)?;
                }
            }
            if matches == 1 {
                headers
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                headers.push(header);
            }
        }
        if self.exhausted {
            return Ok(());
        }
        // Loading or executing unrelated code does not erase a written header.
        // No declaration in this branch supplies a caller-scope Read certificate.
        for header in headers {
            if !self.visit(cancel)? {
                return Ok(());
            }
            self.insert(header.node, header.name, header.end, "ring.function")?;
            for (parameter, _) in header.parameters {
                if !self.visit(cancel)? {
                    return Ok(());
                }
                self.insert(parameter, parameter, parameter.end_byte(), "ring.parameter")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_ring::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        tree
    }

    #[test]
    fn original_native_function_headers_keep_all_four_independent_capture_roles() {
        use tree_sitter::{Query, QueryCursor, StreamingIterator};

        let language: tree_sitter::Language = tree_sitter_ring::LANGUAGE.into();
        let query = Query::new(&language, include_str!("../../../queries/ring.scm")).unwrap();
        for (source, expected) in [
            (
                include_bytes!("../../../tests/fixtures/ring/top-level/test16.ring").as_slice(),
                vec![(5..10, 0..10)],
            ),
            (
                include_bytes!("../../../tests/fixtures/ring/top-level/test4.ring").as_slice(),
                vec![(5..9, 0..9), (64..67, 59..67)],
            ),
        ] {
            let tree = tree(source);
            let mut cache = CaptureCache::new(source, 4096, true);
            let mut cursor = QueryCursor::new();
            let mut matched = cursor.matches(&query, tree.root_node(), source);
            let mut roles = std::collections::BTreeMap::<usize, Vec<String>>::new();
            let mut headers = Vec::new();
            while let Some(found) = matched.next() {
                for capture in found.captures {
                    if capture.node.kind() != "function_definition" {
                        continue;
                    }
                    let role_name = query.capture_names()[usize::try_from(capture.index).unwrap()];
                    let role = match role_name {
                        "scope" => StructuralRole::Scope,
                        "declaration" => StructuralRole::Declaration,
                        "definition" => StructuralRole::Definition,
                        "signature" => StructuralRole::Signature,
                        _ => continue,
                    };
                    let proof =
                        candidate(capture.node, role, source, &Cancellation::new(), &mut cache)
                            .unwrap()
                            .unwrap();
                    assert!(proof.required);
                    if role == StructuralRole::Definition {
                        assert!(
                            expected
                                .iter()
                                .any(|(name, _)| *name == (proof.start..proof.end))
                        );
                    }
                    if role == StructuralRole::Signature {
                        assert_eq!(proof.syntax, "ring.header");
                        headers.push(proof.start..proof.end);
                    }
                    roles
                        .entry(capture.node.start_byte())
                        .or_default()
                        .push(role_name.to_owned());
                }
            }
            for found in roles.values_mut() {
                found.sort();
                assert_eq!(
                    found.iter().map(String::as_str).collect::<Vec<_>>(),
                    ["declaration", "definition", "scope", "signature"]
                );
            }
            assert_eq!(roles.len(), expected.len());
            headers.sort_by_key(|range| range.start);
            assert_eq!(
                headers,
                expected
                    .into_iter()
                    .map(|(_, header)| header)
                    .collect::<Vec<_>>()
            );
            assert!(cache.reads.is_empty());
            assert!(!cache.exhausted());
        }
    }

    #[test]
    fn native_function_headers_ignore_body_changes_and_never_donate_binding_certificates() {
        let source = "func measure(value, other) { consume(value) return 1 }\n";
        let changed = source.replace("return 1", "return 123456");
        let mut previous = None;
        for source in [source, changed.as_str()] {
            let tree = tree(source.as_bytes());
            let mut cache = CaptureCache::new(source.as_bytes(), 4096, true);
            cache
                .initialize(tree.root_node(), &Cancellation::new())
                .unwrap();
            let function = cache
                .declarations
                .values()
                .find(|declaration| declaration.syntax == "ring.function")
                .unwrap();
            assert_eq!(
                &source[function.header.clone()],
                "func measure(value, other)"
            );
            assert_eq!(
                cache
                    .declarations
                    .values()
                    .filter(|declaration| declaration.syntax == "ring.parameter")
                    .count(),
                2
            );
            if let Some(previous) = &previous {
                assert_eq!(&function.header, previous);
            }
            previous = Some(function.header.clone());
            assert!(cache.reads.is_empty());
        }
    }

    #[test]
    fn whole_native_census_work_partial_source_and_cancel_cannot_publish_function_headers() {
        let source = format!("func measure value {{ {} }}\n", "consume(1)\n".repeat(96));
        let tree = tree(source.as_bytes());
        let mut limited = CaptureCache::new(source.as_bytes(), 1, true);
        limited
            .initialize(tree.root_node(), &Cancellation::new())
            .unwrap();
        assert!(limited.exhausted());
        assert!(limited.declarations.is_empty() && limited.reads.is_empty());
        let mut partial = CaptureCache::new(source.as_bytes(), 4096, false);
        partial
            .initialize(tree.root_node(), &Cancellation::new())
            .unwrap();
        assert!(partial.declarations.is_empty() && partial.reads.is_empty());
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            CaptureCache::new(source.as_bytes(), 4096, true).initialize(tree.root_node(), &cancel),
            Err(AdapterError::Cancelled {
                reason: rootlight_cancel::CancellationReason::ClientRequest
            })
        ));
    }
}
