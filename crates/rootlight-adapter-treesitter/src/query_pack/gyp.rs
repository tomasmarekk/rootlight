//! Admit GYP data only after a complete native literal-AST census.
//! Configuration branches retain written target/source evidence; their selection,
//! includes, variable expansion and commands are never evaluated.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth};

pub(super) struct Captures<'source> {
    source: &'source [u8],
    checked: bool,
    valid: bool,
    failure: &'static str,
    remaining: usize,
    exhausted: bool,
    targets: HashSet<usize>,
    definitions: HashMap<usize, (usize, usize)>,
    paths: HashMap<usize, (usize, usize)>,
    gaps: HashMap<usize, &'static str>,
}

impl<'source> Captures<'source> {
    pub(super) fn new(source: &'source [u8], remaining: usize) -> Self {
        Self {
            source,
            checked: false,
            valid: false,
            failure: "gyp.invalid_file",
            remaining,
            exhausted: false,
            targets: HashSet::new(),
            definitions: HashMap::new(),
            paths: HashMap::new(),
            gaps: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(left) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = left;
        Ok(true)
    }

    fn plain(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<(usize, usize)>, AdapterError> {
        if !self.step(cancel)? || node.kind() != "string" || node.has_error() || node.is_missing() {
            return Ok(None);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(None);
            }
        }
        let Some((&quote, rest)) = raw.split_first() else {
            return Ok(None);
        };
        if !matches!(quote, b'\'' | b'"') || rest.last() != Some(&quote) || rest.is_empty() {
            return Ok(None);
        }
        let body = &rest[..rest.len() - 1];
        if body.contains(&b'\\')
            || body.iter().any(|byte| byte.is_ascii_control())
            || body.first() == Some(&quote)
            || std::str::from_utf8(body).is_err()
        {
            return Ok(None);
        }
        Ok(Some((node.start_byte() + 1, node.end_byte() - 1)))
    }

    fn key(
        &mut self,
        pair: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        let Some(key) = pair.child_by_field_name("key") else {
            return Ok(None);
        };
        let Some((start, end)) = self.plain(key, cancel)? else {
            return Ok(None);
        };
        Ok(self
            .source
            .get(start..end)
            .and_then(|bytes| std::str::from_utf8(bytes).ok()))
    }

    fn gap(&mut self, node: Node<'_>, label: &'static str) -> Result<(), AdapterError> {
        self.gaps
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.gaps.insert(node.id(), label);
        Ok(())
    }

    fn children<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Vec<Node<'tree>>, AdapterError> {
        let mut children = Vec::new();
        let mut cursor = node.walk();
        for child in node.named_children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(Vec::new());
            }
            if child.kind() != "comment" {
                children
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                children.push(child);
            }
        }
        Ok(children)
    }

    fn validate(
        &mut self,
        root: Node<'_>,
        complete: bool,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !complete
            || root.kind() != "module"
            || root.has_error()
            || root.is_missing()
            || !(root.start_byte() == 0
                || (root.start_byte() == 3 && self.source.starts_with(b"\xef\xbb\xbf")))
            || root.end_byte() != self.source.len()
        {
            return Ok(false);
        }
        let statements = self.children(root, cancel)?;
        if statements.len() != 1 || statements[0].kind() != "expression_statement" {
            return Ok(false);
        }
        let values = self.children(statements[0], cancel)?;
        if values.len() != 1 || values[0].kind() != "dictionary" {
            return Ok(false);
        }
        let mut cursor = root.walk();
        loop {
            let node = cursor.node();
            if !self.step(cancel)? || node.is_error() || node.is_missing() {
                return Ok(false);
            }
            if node.is_named()
                && !matches!(
                    node.kind(),
                    "module"
                        | "expression_statement"
                        | "dictionary"
                        | "pair"
                        | "list"
                        | "string"
                        | "string_start"
                        | "string_content"
                        | "string_end"
                        | "escape_sequence"
                        | "integer"
                        | "float"
                        | "true"
                        | "false"
                        | "none"
                        | "comment"
                )
            {
                return Ok(false);
            }
            if node.kind() == "string" {
                let Some(raw) = self.source.get(node.byte_range()) else {
                    return Ok(false);
                };
                if !raw.first().is_some_and(|q| matches!(q, b'\'' | b'"'))
                    || raw.get(..3).is_some_and(|q| q == b"'''" || q == b"\"\"\"")
                {
                    return Ok(false);
                }
            }
            if node.kind() == "dictionary" {
                let mut keys = HashSet::new();
                for pair in self.children(node, cancel)? {
                    if pair.kind() != "pair" {
                        return Ok(false);
                    }
                    let Some(key) = self.key(pair, cancel)? else {
                        return Ok(false);
                    };
                    keys.try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    if !keys.insert(key) {
                        self.failure = "gyp.duplicate_key";
                        return Ok(false);
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
                    return Ok(true);
                }
            }
        }
    }

    fn inspect(
        &mut self,
        root: Node<'_>,
        complete: bool,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        if self.checked {
            return Ok(());
        }
        self.checked = true;
        if !self.validate(root, complete, cancel)? {
            return Ok(());
        }
        let statements = self.children(root, cancel)?;
        let Some(statement) = statements.first() else {
            return Ok(());
        };
        let values = self.children(*statement, cancel)?;
        let Some(&document) = values.first() else {
            return Ok(());
        };
        let mut pending = vec![document];
        let mut names = HashMap::<&str, usize>::new();
        while let Some(config) = pending.pop() {
            if !self.step(cancel)? {
                return Ok(());
            }
            for pair in self.children(config, cancel)? {
                let Some(key) = self.key(pair, cancel)? else {
                    return Ok(());
                };
                let Some(value) = pair.child_by_field_name("value") else {
                    return Ok(());
                };
                match key {
                    "targets" if config == document && value.kind() == "list" => {
                        let targets = self.children(value, cancel)?;
                        if targets.iter().any(|target| target.kind() != "dictionary") {
                            self.gap(pair, "gyp.targets_unavailable")?;
                            continue;
                        }
                        for target in targets {
                            let mut named = None;
                            for member in self.children(target, cancel)? {
                                if self.key(member, cancel)? == Some("target_name") {
                                    named = member.child_by_field_name("value");
                                }
                            }
                            if let Some(name) = named
                                && let Some(span) = self.plain(name, cancel)?
                            {
                                let text = std::str::from_utf8(&self.source[span.0..span.1])
                                    .map_err(|_| SinkError::InvalidIr)?;
                                if !text.is_empty()
                                    && text.bytes().all(|b| {
                                        b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.')
                                    })
                                {
                                    self.targets
                                        .try_reserve(1)
                                        .map_err(|_| SinkError::AllocationFailed)?;
                                    self.definitions
                                        .try_reserve(1)
                                        .map_err(|_| SinkError::AllocationFailed)?;
                                    self.targets.insert(target.id());
                                    self.definitions.insert(name.id(), span);
                                    names
                                        .try_reserve(1)
                                        .map_err(|_| SinkError::AllocationFailed)?;
                                    if let Some(previous) = names.insert(text, target.id()) {
                                        self.gaps
                                            .try_reserve(2)
                                            .map_err(|_| SinkError::AllocationFailed)?;
                                        self.gaps.insert(previous, "gyp.duplicate_target");
                                        self.gaps.insert(target.id(), "gyp.duplicate_target");
                                    }
                                } else {
                                    self.gap(target, "gyp.target_name_unavailable")?;
                                }
                            } else {
                                self.gap(target, "gyp.target_name_unavailable")?;
                            }
                            pending
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            pending.push(target);
                        }
                    }
                    "target_defaults" if config == document && value.kind() == "dictionary" => {
                        pending
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        pending.push(value);
                    }
                    "conditions" | "target_conditions" => {
                        self.gap(pair, "gyp.conditions")?;
                        if value.kind() != "list" {
                            continue;
                        }
                        for entry in self.children(value, cancel)? {
                            if entry.kind() != "list" {
                                continue;
                            }
                            let parts = self.children(entry, cancel)?;
                            if !matches!(parts.len(), 2 | 3)
                                || parts[0].kind() != "string"
                                || parts[1..].iter().any(|part| part.kind() != "dictionary")
                            {
                                continue;
                            }
                            pending
                                .try_reserve(parts.len() - 1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            pending.extend_from_slice(&parts[1..]);
                        }
                    }
                    "sources" if value.kind() == "list" => {
                        for path in self.children(value, cancel)? {
                            if let Some(span) = self.plain(path, cancel)? {
                                let raw = &self.source[span.0..span.1];
                                if !raw.is_empty()
                                    && !raw.contains(&b'<')
                                    && !raw.contains(&b'>')
                                    && !raw.contains(&b'$')
                                {
                                    self.paths
                                        .try_reserve(1)
                                        .map_err(|_| SinkError::AllocationFailed)?;
                                    self.paths.insert(path.id(), span);
                                } else {
                                    self.gap(path, "gyp.expansion")?;
                                }
                            } else {
                                self.gap(path, "gyp.expansion")?;
                            }
                        }
                    }
                    "target_name" if self.targets.contains(&config.id()) => {}
                    "includes" | "include_dirs" => self.gap(pair, "gyp.includes")?,
                    "dependencies" => self.gap(pair, "gyp.dependencies")?,
                    _ => self.gap(pair, "gyp.settings")?,
                }
            }
        }
        self.valid = !self.exhausted;
        Ok(())
    }

    pub(super) fn candidate(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        root: Node<'_>,
        complete: bool,
        cancel: &Cancellation,
    ) -> Result<Option<QueryCandidate>, AdapterError> {
        self.inspect(root, complete, cancel)?;
        if !self.step(cancel)? {
            return Ok(None);
        }
        let (syntax, role, span) = match role {
            StructuralRole::Root => ("gyp.file", role, (node.start_byte(), node.end_byte())),
            StructuralRole::Module if self.valid => {
                ("gyp.file", role, (node.start_byte(), node.end_byte()))
            }
            StructuralRole::Signature if node.kind() == "module" && !self.valid => {
                (self.failure, role, (node.start_byte(), node.end_byte()))
            }
            StructuralRole::Declaration if self.valid && self.targets.contains(&node.id()) => {
                ("gyp.target", role, (node.start_byte(), node.end_byte()))
            }
            StructuralRole::Definition
                if self.valid && self.definitions.contains_key(&node.id()) =>
            {
                ("gyp.target_name", role, self.definitions[&node.id()])
            }
            StructuralRole::Signature if self.valid && self.paths.contains_key(&node.id()) => (
                "gyp.source_path",
                StructuralRole::Reference,
                self.paths[&node.id()],
            ),
            StructuralRole::Signature if self.valid && self.gaps.contains_key(&node.id()) => (
                self.gaps[&node.id()],
                role,
                (node.start_byte(), node.end_byte()),
            ),
            StructuralRole::Comment if self.valid => {
                ("gyp.comment", role, (node.start_byte(), node.end_byte()))
            }
            _ => return Ok(None),
        };
        Ok(Some(QueryCandidate {
            start: span.0,
            end: span.1,
            role,
            syntax,
            required: role != StructuralRole::Comment,
            native_depth: native_depth(node, cancel)?,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &[u8] = b"{'targets': [{'target_name': 'safe', 'sources': ['x.cc'] }]}\n";

    fn parsed() -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        parser.parse(SOURCE, None).unwrap()
    }

    fn target(tree: &tree_sitter::Tree) -> Node<'_> {
        let mut cursor = tree.root_node().walk();
        loop {
            let node = cursor.node();
            if node.kind() == "dictionary" && node.parent().is_some_and(|p| p.kind() == "list") {
                return node;
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                assert!(cursor.goto_parent());
            }
        }
    }

    #[test]
    fn full_gyp_census_and_every_capture_require_unexhausted_work() {
        let tree = parsed();
        let root = tree.root_node();
        let target = target(&tree);
        let mut admitted = Captures::new(SOURCE, 4096);
        assert!(
            admitted
                .candidate(
                    target,
                    StructuralRole::Declaration,
                    root,
                    true,
                    &Cancellation::new()
                )
                .unwrap()
                .is_some()
        );
        let required = 4096 - admitted.remaining;
        assert!(required > 1);
        for budget in [0, 1, required / 2, required - 1] {
            let mut cache = Captures::new(SOURCE, budget);
            assert!(
                cache
                    .candidate(
                        target,
                        StructuralRole::Declaration,
                        root,
                        true,
                        &Cancellation::new()
                    )
                    .unwrap()
                    .is_none()
            );
            assert!(cache.exhausted());
        }
    }

    #[test]
    fn partial_or_cancelled_gyp_census_cannot_reuse_admitted_targets() {
        let tree = parsed();
        let root = tree.root_node();
        let target = target(&tree);
        let mut partial = Captures::new(SOURCE, 4096);
        assert!(
            partial
                .candidate(
                    target,
                    StructuralRole::Declaration,
                    root,
                    false,
                    &Cancellation::new()
                )
                .unwrap()
                .is_none()
        );
        let mut cache = Captures::new(SOURCE, 4096);
        assert!(
            cache
                .candidate(
                    target,
                    StructuralRole::Declaration,
                    root,
                    true,
                    &Cancellation::new()
                )
                .unwrap()
                .is_some()
        );
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            cache.candidate(target, StructuralRole::Declaration, root, true, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
