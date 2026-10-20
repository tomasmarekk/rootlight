//! Admit Fish functions only from static native names and reviewed header fields.
//! Named options never turn descriptions or expanded words into parameters.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone, Copy)]
struct Header {
    name_start: usize,
    name_end: usize,
    end: usize,
}

pub(super) struct CaptureCache {
    headers: HashMap<usize, Option<Header>>,
    parameters: HashSet<usize>,
    remaining: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(maximum: usize) -> Self {
        Self {
            headers: HashMap::new(),
            parameters: HashSet::new(),
            remaining: maximum,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }

    fn header(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Header>, AdapterError> {
        cancellation.check()?;
        if let Some(header) = self.headers.get(&node.id()) {
            return Ok(*header);
        }
        if !self.visit(cancellation)? {
            return Ok(None);
        }
        let header = self.prove_header(node, source, cancellation)?;
        self.headers
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.headers.insert(node.id(), header);
        Ok(header)
    }

    fn prove_header(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Header>, AdapterError> {
        if node.kind() != "function_definition"
            || node.has_error()
            || node.is_missing()
            || !node
                .parent()
                .is_some_and(|parent| parent.kind() == "program")
        {
            return Ok(None);
        }
        let Some(name) = node.child_by_field_name("name").filter(|name| {
            static_word(*name, source) && text(*name, source).is_some_and(|raw| !reserved(raw))
        }) else {
            return Ok(None);
        };
        let mut named_arguments = false;
        let mut argument_option_seen = false;
        let mut argument_count = 0usize;
        let mut value_pending = false;
        let mut parameters = Vec::new();
        let mut names = HashSet::new();
        let mut header_end = None;
        let mut cursor = node.walk();
        for (index, child) in node.children(&mut cursor).enumerate() {
            if !self.visit(cancellation)? {
                return Ok(None);
            }
            let field = node.field_name_for_child(
                u32::try_from(index).map_err(|_| query_failure("query-fish-child-index"))?,
            );
            if matches!(child.kind(), ";" | "&" | "\n" | "\r" | "\r\n") {
                header_end = Some(child.start_byte());
                break;
            }
            if field != Some("option") {
                continue;
            }
            let Some(raw) = text(child, source) else {
                return Ok(None);
            };
            if value_pending {
                if !literal_option_value(child, source) {
                    return Ok(None);
                }
                value_pending = false;
                continue;
            }
            if raw.starts_with('-') {
                if named_arguments && argument_count == 0 {
                    return Ok(None);
                }
                named_arguments = false;
                match raw {
                    "-a" | "--argument-names" => {
                        if argument_option_seen {
                            return Ok(None);
                        }
                        argument_option_seen = true;
                        named_arguments = true;
                        argument_count = 0;
                    }
                    "-d" | "--description" | "-w" | "--wraps" | "-e" | "--on-event" | "-v"
                    | "--on-variable" | "-j" | "--on-job-exit" | "-p" | "--on-process-exit"
                    | "-s" | "--on-signal" | "-V" | "--inherit-variable" => value_pending = true,
                    "-S" | "--no-scope-shadowing" => {}
                    _ => return Ok(None),
                }
            } else if named_arguments
                && child.kind() == "word"
                && raw
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                && !raw.is_empty()
            {
                names
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                if !names.insert(raw) {
                    return Ok(None);
                }
                parameters
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                parameters.push(child.id());
                argument_count += 1;
            } else {
                return Ok(None);
            }
        }
        let Some(end) = header_end else {
            return Ok(None);
        };
        if value_pending || named_arguments && argument_count == 0 {
            return Ok(None);
        }
        self.parameters
            .try_reserve(parameters.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        self.parameters.extend(parameters);
        Ok(Some(Header {
            name_start: name.start_byte(),
            name_end: name.end_byte(),
            end,
        }))
    }
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn static_word(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "word"
        && !node.is_missing()
        && !node.has_error()
        && text(node, source).is_some_and(|raw| {
            structural_captured_name_for_language("fish", raw, raw.len()).is_some()
        })
}

fn reserved(raw: &str) -> bool {
    matches!(
        raw,
        "function"
            | "end"
            | "if"
            | "else"
            | "while"
            | "for"
            | "switch"
            | "case"
            | "begin"
            | "break"
            | "continue"
            | "return"
            | "and"
            | "or"
            | "not"
            | "command"
            | "builtin"
            | "time"
            | "exec"
    )
}

fn literal_option_value(node: Node<'_>, source: &[u8]) -> bool {
    if node.is_missing() || node.has_error() {
        return false;
    }
    if static_word(node, source) || node.kind() == "integer" {
        return true;
    }
    if !matches!(node.kind(), "single_quote_string" | "double_quote_string") {
        return false;
    }
    let mut cursor = node.walk();
    node.named_children(&mut cursor).next().is_none()
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    let syntax = if node.kind() == "program" {
        range = 0..source.len();
        "fish.file"
    } else if role == StructuralRole::Comment && node.kind() == "comment" {
        "fish.comment"
    } else if role == StructuralRole::StringLiteral {
        "fish.string"
    } else if node.kind() == "function_definition" {
        let header = cache.header(node, source, cancellation)?;
        if role == StructuralRole::Expression {
            if header.is_some() {
                return Ok(None);
            }
            if let Some(name) = node.child_by_field_name("name") {
                range.end = name.end_byte();
            }
            "fish.unsupported"
        } else {
            let Some(header) = header else {
                return Ok(None);
            };
            match role {
                StructuralRole::Definition => {
                    range = header.name_start..header.name_end;
                    "fish.name"
                }
                StructuralRole::Signature => {
                    range.end = header.end;
                    "fish.header"
                }
                _ => "fish.function",
            }
        }
    } else if let Some(parent) = node
        .parent()
        .filter(|parent| parent.kind() == "function_definition")
    {
        if cache.header(parent, source, cancellation)?.is_none()
            || !cache.parameters.contains(&node.id())
        {
            return Ok(None);
        }
        if role == StructuralRole::Definition {
            "fish.name"
        } else {
            "fish.parameter"
        }
    } else {
        return Ok(None);
    };
    let mut depth = native_depth(node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() == "function_definition" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-fish-depth"))?;
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        syntax,
        role,
        required: role.belongs_to_identity_closure(),
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_proofs_obey_capacity_and_cancellation() {
        let source = b"function written --argument-names value\n echo $value\nend\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_fish::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let function = tree.root_node().named_child(0).unwrap();
        let cancellation = Cancellation::new();
        let mut bounded = CaptureCache::new(0);
        assert!(
            candidate(
                function,
                StructuralRole::Declaration,
                source,
                &cancellation,
                &mut bounded
            )
            .unwrap()
            .is_none()
        );
        assert!(bounded.exhausted());
        let mut admitted = CaptureCache::new(128);
        assert!(
            candidate(
                function,
                StructuralRole::Declaration,
                source,
                &cancellation,
                &mut admitted
            )
            .unwrap()
            .is_some()
        );
        assert!(cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest));
        assert!(matches!(
            candidate(
                function,
                StructuralRole::Definition,
                source,
                &cancellation,
                &mut admitted
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
