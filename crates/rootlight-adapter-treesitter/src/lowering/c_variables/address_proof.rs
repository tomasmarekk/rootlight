//! Reconstruct address binding geometry from the current decoded C source.
//! Producer labels and target windows are checked against this separate census.

use std::{collections::HashMap, ops::ControlFlow};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::{Node, ParseOptions};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Address {
    pub(super) declaration: (usize, usize),
    pub(super) name: (usize, usize),
    pub(super) owner: (usize, usize),
    pub(super) owner_name: (usize, usize),
}

struct Owner {
    declaration: (usize, usize),
    name: (usize, usize),
}

struct Work(usize);

impl Work {
    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.0.checked_sub(1) else {
            return Ok(false);
        };
        self.0 = next;
        Ok(true)
    }

    fn name<'source>(
        &mut self,
        node: Node<'_>,
        source: &'source [u8],
        cancel: &Cancellation,
    ) -> Result<Option<&'source [u8]>, AdapterError> {
        if node.kind() != "identifier" || !self.step(cancel)? {
            return Ok(None);
        }
        let Some(raw) = source.get(node.byte_range()) else {
            return Ok(None);
        };
        if !raw
            .first()
            .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        {
            return Ok(None);
        }
        for chunk in raw.chunks(64) {
            if !self.step(cancel)?
                || !chunk
                    .iter()
                    .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
            {
                return Ok(None);
            }
        }
        Ok(Some(raw))
    }

    fn owner(
        &mut self,
        mut node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<Owner>, AdapterError> {
        loop {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if node.kind() == "function_definition" {
                if !self.step(cancel)? {
                    return Ok(None);
                }
                let Some(declarator) = node.child_by_field_name("declarator") else {
                    return Ok(None);
                };
                let Some(name) = declarator.child_by_field_name("declarator") else {
                    return Ok(None);
                };
                if declarator.kind() != "function_declarator" || name.kind() != "identifier" {
                    return Ok(None);
                }
                return Ok(Some(Owner {
                    declaration: (node.start_byte(), node.end_byte()),
                    name: (name.start_byte(), name.end_byte()),
                }));
            }
            let Some(parent) = node.parent() else {
                return Ok(None);
            };
            node = parent;
        }
    }
}

fn bind<'source>(
    bindings: &mut HashMap<(usize, &'source [u8]), Option<Address>>,
    scope: usize,
    name: &'source [u8],
    address: Option<Address>,
) -> Result<(), AdapterError> {
    bindings
        .try_reserve(1)
        .map_err(|_| SinkError::AllocationFailed)?;
    bindings
        .entry((scope, name))
        .and_modify(|old| *old = None)
        .or_insert(address);
    Ok(())
}

fn operand(
    node: Node<'_>,
    source: &[u8],
    work: &mut Work,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(pointer) = node.parent() else {
        return Ok(false);
    };
    let Some(arguments) = pointer.parent() else {
        return Ok(false);
    };
    let Some(call) = arguments.parent() else {
        return Ok(false);
    };
    let Some(statement) = call.parent() else {
        return Ok(false);
    };
    if !work.step(cancel)?
        || pointer.kind() != "pointer_expression"
        || pointer.child_by_field_name("argument") != Some(node)
        || pointer
            .child_by_field_name("operator")
            .and_then(|n| source.get(n.byte_range()))
            != Some(b"&".as_slice())
        || arguments.kind() != "argument_list"
        || call.kind() != "call_expression"
        || call.child_by_field_name("arguments") != Some(arguments)
        || statement.kind() != "expression_statement"
    {
        return Ok(false);
    }
    let mut child = statement;
    while let Some(parent) = child.parent() {
        if !work.step(cancel)? {
            return Ok(false);
        }
        match parent.kind() {
            "compound_statement" | "if_statement" | "while_statement" | "do_statement"
            | "switch_statement" | "case_statement" | "labeled_statement" => {}
            "for_statement" if parent.child_by_field_name("body") == Some(child) => {}
            "function_definition" => return Ok(parent.child_by_field_name("body") == Some(child)),
            _ => return Ok(false),
        }
        child = parent;
    }
    Ok(false)
}

/// Return no geometry when the complete source census cannot be established.
pub(super) fn source_addresses(
    source: &[u8],
    maximum: usize,
    cancel: &Cancellation,
) -> Result<HashMap<(usize, usize), Address>, AdapterError> {
    cancel.check()?;
    let mut result = HashMap::new();
    if maximum == 0 {
        return Ok(result);
    }
    let mut parser = tree_sitter::Parser::new();
    parser
        .set_language(&tree_sitter_c::LANGUAGE.into())
        .map_err(|_| super::super::provider_failure("c-address-consumer-grammar"))?;
    let mut parse_work = maximum;
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancel.check().is_err() {
            return ControlFlow::Break(());
        }
        match parse_work.checked_sub(1) {
            Some(next) => {
                parse_work = next;
                ControlFlow::Continue(())
            }
            None => {
                exhausted = true;
                ControlFlow::Break(())
            }
        }
    };
    let mut input = |offset: usize, _: tree_sitter::Point| source.get(offset..).unwrap_or_default();
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
    if root.kind() != "translation_unit"
        || root.has_error()
        || root.is_missing()
        || root.end_byte() != source.len()
        || !matches!(source.get(..root.start_byte()), Some(b"" | b"\xef\xbb\xbf"))
    {
        return Ok(result);
    }
    let mut work = Work(maximum);
    let mut bindings = HashMap::new();
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if !work.step(cancel)?
            || node.is_error()
            || node.is_missing()
            || (node.kind().starts_with("preproc_")
                && node.kind() != "preproc_include"
                && node.parent().is_none_or(|p| p.kind() != "preproc_include"))
            || matches!(
                node.kind(),
                "type_definition" | "struct_specifier" | "union_specifier" | "enum_specifier"
            )
        {
            return Ok(result);
        }
        if node.kind() == "function_definition" {
            let Some(declarator) = node.child_by_field_name("declarator") else {
                return Ok(result);
            };
            let Some(name) = declarator.child_by_field_name("declarator") else {
                return Ok(result);
            };
            if node.parent() != Some(root)
                || declarator.kind() != "function_declarator"
                || work.name(name, source, cancel)?.is_none()
            {
                return Ok(result);
            }
        }
        if node.kind() == "parameter_declaration"
            && let Some(name) = node.child_by_field_name("declarator")
        {
            let Some(name) = work.name(name, source, cancel)? else {
                return Ok(result);
            };
            let mut parent = node.parent();
            let body = loop {
                let Some(owner) = parent else {
                    return Ok(result);
                };
                if !work.step(cancel)? {
                    return Ok(result);
                }
                if owner.kind() == "function_definition" {
                    break owner.child_by_field_name("body");
                }
                parent = owner.parent();
            };
            let Some(body) = body else { return Ok(result) };
            bind(&mut bindings, body.id(), name, None)?;
        }
        if node.kind() == "declaration" {
            let Some(parent) = node.parent() else {
                return Ok(result);
            };
            if !matches!(
                parent.kind(),
                "translation_unit" | "compound_statement" | "for_statement"
            ) || (parent.kind() == "for_statement"
                && parent.child_by_field_name("initializer") != Some(node))
            {
                return Ok(result);
            }
            let mut declarator = None;
            let mut children = node.walk();
            for child in node.named_children(&mut children) {
                if !work.step(cancel)? {
                    return Ok(result);
                }
                match child.kind() {
                    "primitive_type" | "type_identifier" | "type_qualifier" | "comment" => {}
                    "init_declarator" if declarator.is_none() => declarator = Some(child),
                    "identifier"
                        if declarator.is_none() && parent.kind() == "compound_statement" =>
                    {
                        declarator = Some(child)
                    }
                    _ => return Ok(result),
                }
            }
            let Some(declarator) = declarator else {
                return Ok(result);
            };
            let initialized = declarator.kind() == "init_declarator";
            let name_node = if initialized {
                if declarator.child_by_field_name("value").is_none() {
                    return Ok(result);
                }
                let Some(name) = declarator.child_by_field_name("declarator") else {
                    return Ok(result);
                };
                name
            } else {
                declarator
            };
            let Some(name) = work.name(name_node, source, cancel)? else {
                return Ok(result);
            };
            let address = work.owner(node, cancel)?.map(|owner| Address {
                declaration: if initialized {
                    (declarator.start_byte(), declarator.end_byte())
                } else {
                    (node.start_byte(), node.end_byte())
                },
                name: (name_node.start_byte(), name_node.end_byte()),
                owner: owner.declaration,
                owner_name: owner.name,
            });
            if work.0 == 0 {
                return Ok(result);
            }
            bind(&mut bindings, parent.id(), name, address)?;
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
    let mut cursor = root.walk();
    loop {
        let node = cursor.node();
        if !work.step(cancel)? {
            return Ok(HashMap::new());
        }
        if node.kind() == "identifier" && operand(node, source, &mut work, cancel)? {
            let Some(name) = work.name(node, source, cancel)? else {
                return Ok(HashMap::new());
            };
            let mut ancestor = node.parent();
            while let Some(parent) = ancestor {
                if !work.step(cancel)? {
                    return Ok(HashMap::new());
                }
                if parent.kind() == "function_definition" {
                    break;
                }
                if matches!(parent.kind(), "compound_statement" | "for_statement")
                    && let Some(address) = bindings.get(&(parent.id(), name))
                {
                    if let Some(address) = address.filter(|a| a.declaration.1 <= node.start_byte())
                    {
                        result
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        result.insert((node.start_byte(), node.end_byte()), address);
                    }
                    break;
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
                return Ok(result);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_complete_original_census_requires_all_declarations_before_address_geometry() {
        let source =
            include_bytes!("../../../tests/fixtures/c_local_references/test_outlier_detector.c");
        let actual = source_addresses(source, 16384, &Cancellation::new()).unwrap();
        assert_eq!(actual.len(), 1);
        assert_eq!(
            actual.get(&(320, 328)),
            Some(&Address {
                declaration: (274, 299),
                name: (290, 298),
                owner: (252, 1776),
                owner_name: (256, 260),
            })
        );
    }

    #[test]
    fn independent_source_geometry_selects_nearest_local_and_preserves_value_mutation() {
        let source = b"int owned(void){ int local; local=7; use(&local); { int local=2; use(&local); } use(&local); }\n";
        let actual = source_addresses(source, 16384, &Cancellation::new()).unwrap();
        let at: Vec<_> = source
            .windows(6)
            .enumerate()
            .filter_map(|(at, w)| (w == b"&local").then_some(at + 1))
            .collect();
        assert_eq!(at.len(), 3);
        assert_eq!(actual.len(), 3);
        let first = actual.get(&(at[0], at[0] + 5)).unwrap();
        let inner = actual.get(&(at[1], at[1] + 5)).unwrap();
        let last = actual.get(&(at[2], at[2] + 5)).unwrap();
        assert_eq!(first, last);
        assert_ne!(first.declaration, inner.declaration);
        assert_eq!(first.owner_name, (4, 9));
        assert_eq!(inner.owner_name, first.owner_name);
    }

    #[test]
    fn independent_grammar_and_context_refusals_do_not_trust_producer_labels() {
        for source in [
            "int owned(void){ int local; use(local); }\n",
            "int owned(void){ int local; return sizeof(use(&local)); }\n",
            "int owned(void){ int local; int value=use(&local); }\n",
            "int owned(void){ int local; int local; use(&local); }\n",
            "int owned(int local){ int local; use(&local); }\n",
            "int owned(int *argument){ int local; use(&local); }\n",
            "int local=1; int owned(void){ use(&local); }\n",
            "int owned(void){ int local[2]; use(&local); }\n",
            "int owned(void){ int local; use(&local); int later[2]; }\n",
            "#define alias local\nint owned(void){ int local; use(&local); }\n",
            "typedef int External; int owned(void){ External local; use(&local); }\n",
            "int owned(void){ int local; use(&local); } broken @\n",
        ] {
            assert!(
                source_addresses(source.as_bytes(), 16384, &Cancellation::new())
                    .unwrap()
                    .is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn independent_work_and_cancel_return_no_partial_address_set() {
        let source =
            include_bytes!("../../../tests/fixtures/c_local_references/test_outlier_detector.c");
        for maximum in [0, 1, 32] {
            assert!(
                source_addresses(source, maximum, &Cancellation::new())
                    .unwrap()
                    .is_empty()
            );
        }
        assert!(
            !source_addresses(source, 16384, &Cancellation::new())
                .unwrap()
                .is_empty()
        );
        let stopped = Cancellation::new();
        stopped.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            source_addresses(source, 16384, &stopped),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
