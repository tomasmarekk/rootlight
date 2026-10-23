//! Certify one literal command in an ordinary, uniquely declared shell wrapper.
//! The complete native census selects an earlier same-module written function;
//! this source dependency does not establish command evaluation or runtime lookup.

use std::collections::HashMap;

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    functions: HashMap<&'source str, Option<(usize, usize)>>,
    earliest_barrier: Option<usize>,
    remaining: usize,
    complete_source: bool,
    initialized: bool,
    allowed: bool,
    exhausted: bool,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], remaining: usize, complete_source: bool) -> Self {
        Self {
            source,
            functions: HashMap::new(),
            earliest_barrier: None,
            remaining,
            complete_source,
            initialized: false,
            allowed: false,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }

    fn text(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in raw.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
        }
        Ok(std::str::from_utf8(raw).ok())
    }

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        let Some(text) = self.text(node, cancel)? else {
            return Ok(None);
        };
        let mut bytes = text.bytes();
        Ok(bytes
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == b'_')
            .then_some(text)
            .filter(|_| bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')))
    }

    fn module_owned(
        &mut self,
        mut node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        while let Some(parent) = node.parent() {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            match parent.kind() {
                "program" => return Ok(true),
                "compound_statement" if self.source.get(parent.start_byte()) == Some(&b'{') => {}
                _ => return Ok(false),
            }
            node = parent;
        }
        Ok(false)
    }

    fn outside_function(
        &mut self,
        mut node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        while let Some(parent) = node.parent() {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            match parent.kind() {
                "function_definition" => return Ok(false),
                "program" => return Ok(true),
                _ => {}
            }
            node = parent;
        }
        Ok(false)
    }

    fn ordinary(
        &mut self,
        function: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(name) = function.child_by_field_name("name") else {
            return Ok(false);
        };
        let Some(body) = function.child_by_field_name("body") else {
            return Ok(false);
        };
        if function.kind() != "function_definition"
            || function.start_byte() != name.start_byte()
            || function.child_by_field_name("redirect").is_some()
            || body.kind() != "compound_statement"
            || self.source.get(body.start_byte()) != Some(&b'{')
            || self.source.get(body.end_byte().saturating_sub(1)) != Some(&b'}')
            || self.name(name, cancel)?.is_none()
            || !self.module_owned(function, cancel)?
        {
            return Ok(false);
        }
        let Some(header) = self.source.get(name.end_byte()..body.start_byte()) else {
            return Ok(false);
        };
        for _ in header.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
        }
        let mut punctuation = header
            .iter()
            .copied()
            .filter(|byte| !byte.is_ascii_whitespace());
        Ok(punctuation.next() == Some(b'(')
            && punctuation.next() == Some(b')')
            && punctuation.next().is_none())
    }

    fn initialize(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        self.initialized = true;
        if !self.complete_source {
            return Ok(());
        }
        let mut root = node;
        while let Some(parent) = root.parent() {
            if !self.visit(cancel)? {
                return Ok(());
            }
            root = parent;
        }
        if root.kind() != "program"
            || root.has_error()
            || root.end_byte() != self.source.len()
            || !matches!(
                self.source.get(..root.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            )
        {
            return Ok(());
        }
        let mut cursor = root.walk();
        loop {
            if !self.visit(cancel)? {
                return Ok(());
            }
            let current = cursor.node();
            if current.kind() == "function_definition" {
                let Some(name_node) = current.child_by_field_name("name") else {
                    return Ok(());
                };
                let Some(name) = self.name(name_node, cancel)? else {
                    return Ok(());
                };
                let ordinary = self.ordinary(current, cancel)?;
                self.functions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.functions
                    .entry(name)
                    .and_modify(|value| *value = None)
                    .or_insert(ordinary.then_some((current.start_byte(), current.end_byte())));
            }
            if matches!(
                current.kind(),
                "command" | "unset_command" | "declaration_command" | "&"
            ) && self.outside_function(current, cancel)?
            {
                // An asynchronous terminator can follow its function or outer brace
                // group. Its later byte offset cannot establish a synchronous module.
                // Uncalled function bodies do not execute their own ampersands.
                let position = if current.kind() == "&" {
                    0
                } else {
                    current.start_byte()
                };
                self.earliest_barrier = Some(
                    self.earliest_barrier
                        .map_or(position, |old| old.min(position)),
                );
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    self.allowed = true;
                    return Ok(());
                }
            }
        }
    }

    fn descriptor(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        let Some(text) = self.text(node, cancel)? else {
            return Ok(false);
        };
        let Some((left, right)) = text.split_once(">&") else {
            return Ok(false);
        };
        Ok(
            (left.is_empty() || left.bytes().all(|byte| byte.is_ascii_digit()))
                && !right.is_empty()
                && right.bytes().all(|byte| byte.is_ascii_digit()),
        )
    }

    fn argument(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        let Some(text) = self.text(node, cancel)? else {
            return Ok(false);
        };
        Ok(match node.kind() {
            "word" => {
                !text.is_empty()
                    && text
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || b"_./+-:=,@%".contains(&byte))
            }
            "raw_string" => {
                text.starts_with('\'') && text.ends_with('\'') && !text.contains(['\n', '\r'])
            }
            "string" => {
                text == "\"$@\""
                    || (text.starts_with('"')
                        && text.ends_with('"')
                        && !text
                            .get(1..text.len().saturating_sub(1))
                            .is_none_or(|inside| {
                                inside.bytes().any(|byte| b"$`\\\"\r\n".contains(&byte))
                            }))
            }
            _ => false,
        })
    }

    fn certified(
        &mut self,
        command: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.initialized {
            self.initialize(command, cancel)?
        }
        if !self.allowed || command.kind() != "command" {
            return Ok(false);
        }
        let Some(name_node) = command.child_by_field_name("name") else {
            return Ok(false);
        };
        if name_node.kind() != "command_name" || name_node.named_child_count() != 1 {
            return Ok(false);
        }
        let Some(word) = name_node
            .named_child(0)
            .filter(|word| word.kind() == "word")
        else {
            return Ok(false);
        };
        let Some(name) = self.name(word, cancel)? else {
            return Ok(false);
        };
        if matches!(
            name,
            "command"
                | "builtin"
                | "eval"
                | "source"
                | "exec"
                | "alias"
                | "unalias"
                | "unset"
                | "enable"
                | "trap"
        ) {
            return Ok(false);
        }
        let mut children = command.walk();
        for child in command.named_children(&mut children) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if child == name_node {
                continue;
            }
            if child.kind() == "file_redirect" {
                if !self.descriptor(child, cancel)? {
                    return Ok(false);
                }
            } else if !self.argument(child, cancel)? {
                return Ok(false);
            }
        }
        let mut statement = command;
        if let Some(parent) = command
            .parent()
            .filter(|parent| parent.kind() == "redirected_statement")
        {
            if parent.child_by_field_name("body") != Some(command) {
                return Ok(false);
            }
            let mut cursor = parent.walk();
            for child in parent.named_children(&mut cursor) {
                if !self.visit(cancel)? {
                    return Ok(false);
                }
                if child != command
                    && (child.kind() != "file_redirect" || !self.descriptor(child, cancel)?)
                {
                    return Ok(false);
                }
            }
            statement = parent;
        }
        let Some(body) = statement
            .parent()
            .filter(|parent| parent.kind() == "compound_statement")
        else {
            return Ok(false);
        };
        let Some(owner) = body.parent().filter(|parent| {
            parent.kind() == "function_definition"
                && parent.child_by_field_name("body") == Some(body)
        }) else {
            return Ok(false);
        };
        if !self.ordinary(owner, cancel)? {
            return Ok(false);
        }
        let mut children = body.walk();
        let mut count = 0usize;
        for child in body.children(&mut children) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if child == statement {
                count += 1;
            } else if child.kind() != "comment" && !matches!(child.kind(), "{" | "}" | ";") {
                return Ok(false);
            }
        }
        if count != 1
            || self
                .earliest_barrier
                .is_some_and(|position| position < owner.start_byte())
        {
            return Ok(false);
        }
        let Some(owner_name) = owner
            .child_by_field_name("name")
            .map(|node| self.name(node, cancel))
            .transpose()?
            .flatten()
        else {
            return Ok(false);
        };
        let owner_unique = self.functions.get(owner_name).copied().flatten()
            == Some((owner.start_byte(), owner.end_byte()));
        Ok(owner_unique
            && self
                .functions
                .get(name)
                .copied()
                .flatten()
                .is_some_and(|(_, end)| end <= owner.start_byte()))
    }

    /// Refine a command only when the complete native census certifies its wrapper.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if role == StructuralRole::Scope && node.kind() == "compound_statement" {
            let mut children = node.walk();
            let mut statement = None;
            let mut competing = false;
            for child in node.named_children(&mut children) {
                if !self.visit(cancel)? {
                    return Ok(false);
                }
                if child.kind() == "comment" {
                    continue;
                }
                competing |= statement.replace(child).is_some();
            }
            let command = statement.filter(|_| !competing).and_then(|statement| {
                if statement.kind() == "command" {
                    Some(statement)
                } else if statement.kind() == "redirected_statement" {
                    statement
                        .child_by_field_name("body")
                        .filter(|body| body.kind() == "command")
                } else {
                    None
                }
            });
            if let Some(command) = command
                && self.certified(command, cancel)?
            {
                candidate.syntax = "bash.direct_wrapper_body";
                candidate.required = true;
            }
            return Ok(true);
        }
        if node.kind() != "command"
            || !matches!(role, StructuralRole::Call | StructuralRole::Signature)
        {
            return Ok(true);
        }
        if !self.certified(node, cancel)? {
            return Ok(role != StructuralRole::Signature);
        }
        candidate.syntax = if role == StructuralRole::Signature {
            "bash.direct_wrapper_context"
        } else {
            "bash.direct_wrapper_call"
        };
        candidate.required = true;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proof(
        source: &[u8],
        capacity: usize,
        complete: bool,
        cancel: &Cancellation,
    ) -> Result<(Vec<(usize, usize)>, bool), AdapterError> {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_bash::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut cache = CaptureCache::new(source, capacity, complete);
        let mut sites = Vec::new();
        let mut cursor = tree.root_node().walk();
        loop {
            let node = cursor.node();
            if node.kind() == "command" {
                for role in [StructuralRole::Call, StructuralRole::Signature] {
                    let mut candidate = QueryCandidate {
                        start: node.start_byte(),
                        end: node.end_byte(),
                        role,
                        syntax: "bash.call",
                        required: false,
                        native_depth: 0,
                    };
                    if cache.refine(node, role, &mut candidate, cancel)?
                        && candidate.syntax == "bash.direct_wrapper_call"
                    {
                        assert!(candidate.required);
                        sites.push((candidate.start, candidate.end));
                    }
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
                    return Ok((sites, cache.exhausted()));
                }
            }
        }
    }

    #[test]
    fn complete_original_and_renamed_wrappers_receive_native_pairs() {
        let source = include_bytes!("../../tests/fixtures/bash_written_calls/nvm.sh");
        let (sites, exhausted) = proof(source, 1_048_576, true, &Cancellation::new()).unwrap();
        assert!(!exhausted);
        assert!(sites.contains(&(732, 749)));
        assert!(sites.contains(&(779, 808)));
        for source in [
            "target(){ :; }\nwrapper(){ >&2 target \"$@\"; }\n",
            "{ target(){ :; }\nwrapper(){ target \"$@\" >&2; }\n}\n",
            "target(){ :; }\nwrapper(){ target literal; }\nother(){ unset -f target; }\n",
        ] {
            assert_eq!(
                proof(source.as_bytes(), 16384, true, &Cancellation::new())
                    .unwrap()
                    .0
                    .len(),
                1,
                "{source}"
            );
        }
    }

    #[test]
    fn dynamic_control_flow_and_competing_definitions_withhold_native_proof() {
        for source in [
            "target(){ :; }\nwrapper(){ command target; }\n",
            "target(){ :; }\nwrapper(){ builtin target; }\n",
            "target(){ :; }\nwrapper(){ eval target; }\n",
            "target(){ :; }\nwrapper(){ source target; }\n",
            "target(){ :; }\nwrapper(){ \"target\"; }\n",
            "target(){ :; }\nwrapper(){ $target; }\n",
            "target(){ :; }\nwrapper(){ X=x target; }\n",
            "target(){ :; }\nwrapper(){ target \"$(other)\"; }\n",
            "target(){ :; }\nwrapper(){ target ${value:=changed}; }\n",
            "target(){ :; }\nwrapper(){ target <(other); }\n",
            "target(){ :; }\nwrapper(){ target >output; }\n",
            "target(){ :; }\nwrapper(){ target >&\"$fd\"; }\n",
            "target(){ :; }\nwrapper(){ if flag; then target; fi; }\n",
            "target(){ :; }\nwrapper(){ target | other; }\n",
            "target(){ :; }\nwrapper(){ target && other; }\n",
            "target(){ :; }\nwrapper(){ target & }\n",
            "target(){ :; }\nwrapper(){ (target); }\n",
            "target(){ :; }\nwrapper(){ target; other; }\n",
            "target(){ :; }\nwrapper(){ target; }\ntarget(){ :; }\n",
            "target(){ :; }\nwrapper(){ target; }\nwrapper(){ :; }\n",
            "wrapper(){ target; }\ntarget(){ :; }\n",
            "target(){ :; }\nunset -f target\nwrapper(){ target; }\n",
            "target(){ :; }\nif flag; then wrapper(){ target; }; fi\n",
            "target(){ :; }\nwrapper(){ target; } >output\n",
            "target(){ :; }\nwrapper(){ target\n",
        ] {
            assert!(
                proof(source.as_bytes(), 16384, true, &Cancellation::new())
                    .unwrap()
                    .0
                    .is_empty(),
                "{source}"
            );
        }
    }

    #[test]
    fn valid_async_function_and_outer_group_terminators_never_certify_module_wrappers() {
        for source in [
            "target(){ :; }\nwrapper(){ target; } &\n",
            "{ target(){ :; }; wrapper(){ target; }; } &\n",
            "target(){ :; } &\nwrapper(){ target; }\n",
            "target(){ :; }\nwrapper(){ target; }\nexternal &\n",
        ] {
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&tree_sitter_bash::LANGUAGE.into())
                .unwrap();
            let tree = parser.parse(source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "valid async syntax premise: {source}"
            );
            assert!(
                proof(source.as_bytes(), 16384, true, &Cancellation::new())
                    .unwrap()
                    .0
                    .is_empty(),
                "{source}"
            );
        }
        let uncalled = "target(){ :; }\nwrapper(){ target; }\nother(){ external & }\n";
        assert_eq!(
            proof(uncalled.as_bytes(), 16384, true, &Cancellation::new())
                .unwrap()
                .0
                .len(),
            1
        );
    }

    #[test]
    fn incomplete_census_capacity_and_typed_cancellation_never_certify_calls() {
        let source = b"target(){ :; }\nwrapper(){ target; }\n";
        assert_eq!(
            proof(source, 16384, true, &Cancellation::new())
                .unwrap()
                .0
                .len(),
            1
        );
        assert!(proof(source, 1, true, &Cancellation::new()).unwrap().1);
        assert!(
            proof(source, 16384, false, &Cancellation::new())
                .unwrap()
                .0
                .is_empty()
        );
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            proof(source, 16384, true, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
