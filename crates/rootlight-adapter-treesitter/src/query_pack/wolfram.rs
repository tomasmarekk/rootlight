//! Written delayed heads and String-pattern formals from complete native input.
//! Only an owned Module body can certify a direct bare formal argument; runtime
//! evaluation, computed scopes, multiple clauses and imported targets stay gaps.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole, query_failure};

#[derive(Clone)]
struct Declaration {
    name: Range<usize>,
    header: Range<usize>,
    label: &'static str,
}

struct Formal<'tree> {
    pattern: Node<'tree>,
    name: Node<'tree>,
}

struct Function<'tree> {
    node: Node<'tree>,
    name: Node<'tree>,
    body: Node<'tree>,
    formals: Vec<Formal<'tree>>,
}

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    initialized: bool,
    remaining: usize,
    exhausted: bool,
    declarations: HashMap<usize, Declaration>,
    definitions: HashMap<usize, Range<usize>>,
    reads: HashMap<usize, Range<usize>>,
    markers: HashMap<usize, Range<usize>>,
    contexts: HashMap<usize, Range<usize>>,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            initialized: false,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(64),
            exhausted: false,
            declarations: HashMap::new(),
            definitions: HashMap::new(),
            reads: HashMap::new(),
            markers: HashMap::new(),
            contexts: HashMap::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn step(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
    }

    fn clean(&self, node: Node<'_>) -> bool {
        !node.has_error()
            && !node.is_error()
            && !node.is_missing()
            && node.start_byte() < node.end_byte()
            && node.end_byte() <= self.source.len()
    }

    fn children<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Vec<Node<'tree>>, AdapterError> {
        if node.child_count() > self.remaining {
            self.exhausted = true;
            return Ok(Vec::new());
        }
        let mut result = Vec::new();
        result
            .try_reserve_exact(node.child_count())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.step(cancel)? {
                return Ok(Vec::new());
            }
            if child.kind() != "comment" {
                result.push(child);
            }
        }
        Ok(result)
    }

    fn word(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source [u8]>, AdapterError> {
        if node.kind() != "symbol" || !self.clean(node) || node.named_child_count() != 0 {
            return Ok(None);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for (index, chunk) in bytes.chunks(64).enumerate() {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if !chunk.iter().enumerate().all(|(offset, byte)| {
                if index == 0 && offset == 0 {
                    *byte == b'$' || byte.is_ascii_alphabetic()
                } else {
                    *byte == b'$' || byte.is_ascii_alphanumeric()
                }
            }) {
                return Ok(None);
            }
        }
        Ok(Some(bytes))
    }

    fn comma_items<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Vec<Node<'tree>>>, AdapterError> {
        let mut pending = Vec::new();
        pending
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        pending.push(node);
        let mut result = Vec::new();
        while let Some(item) = pending.pop() {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if item.kind() == "infix" {
                let children = self.children(item, cancel)?;
                let [left, separator, right] = children.as_slice() else {
                    return Ok(None);
                };
                if separator.kind() != "," {
                    return Ok(None);
                }
                pending
                    .try_reserve(2)
                    .map_err(|_| SinkError::AllocationFailed)?;
                pending.push(*right);
                pending.push(*left);
            } else {
                result
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                result.push(item);
            }
        }
        Ok(Some(result))
    }

    fn delayed<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<(Node<'tree>, Node<'tree>, Node<'tree>)>, AdapterError> {
        if node.kind() != "binary" || !self.clean(node) {
            return Ok(None);
        }
        let children = self.children(node, cancel)?;
        let [left, operator, right] = children.as_slice() else {
            return Ok(None);
        };
        if operator.kind() != ":=" || left.kind() != "call" {
            return Ok(None);
        }
        let Some(head) = left.child_by_field_name("head") else {
            return Ok(None);
        };
        Ok(self.word(head, cancel)?.map(|_| (head, *left, *right)))
    }

    fn function<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Function<'tree>>, AdapterError> {
        let Some((name, call, body)) = self.delayed(node, cancel)? else {
            return Ok(None);
        };
        let mut ancestor = node.parent();
        while let Some(parent) = ancestor {
            if !self.step(cancel)? || self.delayed(parent, cancel)?.is_some() {
                return Ok(None);
            }
            ancestor = parent.parent();
        }
        let Some(arguments) = call.child_by_field_name("arguments") else {
            return Ok(None);
        };
        let Some(items) = self.comma_items(arguments, cancel)? else {
            return Ok(None);
        };
        let mut names = HashMap::new();
        names
            .try_reserve(items.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut formals = Vec::new();
        formals
            .try_reserve_exact(items.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for pattern in items {
            if !self.step(cancel)? || pattern.kind() != "pattern" || !self.clean(pattern) {
                return Ok(None);
            }
            let children = self.children(pattern, cancel)?;
            let [formal, constraint] = children.as_slice() else {
                return Ok(None);
            };
            if pattern.child_by_field_name("name") != Some(*formal)
                || pattern.child_by_field_name("constraint") != Some(*constraint)
                || constraint.kind() != "blank"
                || self.source.get(constraint.byte_range()) != Some(b"_String")
            {
                return Ok(None);
            }
            let Some(spelling) = self.word(*formal, cancel)? else {
                return Ok(None);
            };
            if names.insert(spelling, ()).is_some() {
                return Ok(None);
            }
            formals.push(Formal {
                pattern,
                name: *formal,
            });
        }
        if formals.is_empty() {
            return Ok(None);
        }
        Ok(Some(Function {
            node,
            name,
            body,
            formals,
        }))
    }

    fn module<'tree>(
        &mut self,
        function: &Function<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if function.body.kind() != "call" {
            return Ok(None);
        }
        let Some(head) = function.body.child_by_field_name("head") else {
            return Ok(None);
        };
        if self.word(head, cancel)? != Some(b"Module") {
            return Ok(None);
        }
        let Some(arguments) = function.body.child_by_field_name("arguments") else {
            return Ok(None);
        };
        let children = self.children(arguments, cancel)?;
        let [locals, comma, body] = children.as_slice() else {
            return Ok(None);
        };
        if arguments.kind() != "infix" || comma.kind() != "," || locals.kind() != "group" {
            return Ok(None);
        }
        let local_children = self.children(*locals, cancel)?;
        let list = match local_children.as_slice() {
            [open, close] if open.kind() == "{" && close.kind() == "}" => Vec::new(),
            [open, list, close] if open.kind() == "{" && close.kind() == "}" => {
                let Some(items) = self.comma_items(*list, cancel)? else {
                    return Ok(None);
                };
                items
            }
            _ => return Ok(None),
        };
        let mut seen = HashMap::new();
        seen.try_reserve(list.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for local in list {
            let Some(name) = self.word(local, cancel)? else {
                return Ok(None);
            };
            if seen.insert(name, ()).is_some() {
                return Ok(None);
            }
        }
        for formal in &function.formals {
            let Some(name) = self.word(formal.name, cancel)? else {
                return Ok(None);
            };
            if seen.contains_key(name) {
                return Ok(None);
            }
        }
        Ok(Some(*body))
    }

    fn read_path(
        &mut self,
        node: Node<'_>,
        body: Node<'_>,
        owner: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = node;
        while current.id() != body.id() {
            if !self.step(cancel)? {
                return Ok(false);
            }
            let Some(parent) = current.parent() else {
                return Ok(false);
            };
            if parent.id() == owner.id() {
                return Ok(false);
            }
            match parent.kind() {
                "call" => {
                    let Some(head) = parent.child_by_field_name("head") else {
                        return Ok(false);
                    };
                    let Some(name) = self.word(head, cancel)? else {
                        return Ok(false);
                    };
                    if current.id() == head.id()
                        || matches!(
                            name,
                            b"Module"
                                | b"Block"
                                | b"With"
                                | b"Function"
                                | b"Table"
                                | b"Do"
                                | b"Sum"
                                | b"Product"
                                | b"Integrate"
                                | b"SetDelayed"
                                | b"RuleDelayed"
                                | b"ToExpression"
                                | b"Symbol"
                                | b"Apply"
                                | b"Map"
                                | b"Scan"
                        )
                    {
                        return Ok(false);
                    }
                }
                "binary" => {
                    let children = self.children(parent, cancel)?;
                    let [_, operator, right] = children.as_slice() else {
                        return Ok(false);
                    };
                    if operator.kind() != "=" || right.id() != current.id() {
                        return Ok(false);
                    }
                }
                "infix" => {
                    let children = self.children(parent, cancel)?;
                    if children
                        .iter()
                        .filter(|child| !child.is_named())
                        .any(|child| !matches!(child.kind(), ";" | ","))
                    {
                        return Ok(false);
                    }
                }
                "group" => {}
                _ => return Ok(false),
            }
            current = parent;
        }
        Ok(true)
    }

    fn nodes<'tree>(
        &mut self,
        root: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Vec<Node<'tree>>, AdapterError> {
        let mut result = Vec::new();
        let mut cursor = root.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(Vec::new());
            }
            result
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.push(cursor.node());
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

    fn body_safe(
        &mut self,
        nodes: &[Node<'_>],
        formals: &HashMap<&[u8], usize>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        for node in nodes {
            if !self.step(cancel)? {
                return Ok(false);
            }
            if node.kind() == "pattern" {
                return Ok(false);
            }
            if node.kind() == "call" {
                let Some(head) = node.child_by_field_name("head") else {
                    return Ok(false);
                };
                let Some(name) = self.word(head, cancel)? else {
                    return Ok(false);
                };
                if matches!(
                    name,
                    b"ToExpression"
                        | b"Symbol"
                        | b"Set"
                        | b"SetDelayed"
                        | b"TagSet"
                        | b"TagSetDelayed"
                        | b"UpSet"
                        | b"UpSetDelayed"
                        | b"Clear"
                        | b"ClearAll"
                        | b"Remove"
                        | b"Unprotect"
                        | b"Unset"
                        | b"AppendTo"
                        | b"PrependTo"
                        | b"AssociateTo"
                        | b"AddTo"
                        | b"SubtractFrom"
                        | b"TimesBy"
                        | b"DivideBy"
                        | b"Increment"
                        | b"Decrement"
                ) {
                    return Ok(false);
                }
            }
            if matches!(node.kind(), "prefix" | "postfix") {
                let children = self.children(*node, cancel)?;
                if children
                    .iter()
                    .any(|child| matches!(child.kind(), "++" | "--" | "=."))
                {
                    for candidate in self.nodes(*node, cancel)? {
                        if let Some(name) = self.word(candidate, cancel)?
                            && formals.contains_key(name)
                        {
                            return Ok(false);
                        }
                    }
                }
            }
            if node.kind() == "binary" {
                let children = self.children(*node, cancel)?;
                let [left, operator, _] = children.as_slice() else {
                    return Ok(false);
                };
                if matches!(operator.kind(), ":=" | "^:=" | "/:" | ":") {
                    return Ok(false);
                }
                if matches!(operator.kind(), "=" | "+=" | "-=" | "*=" | "/=" | "^=") {
                    for candidate in self.nodes(*left, cancel)? {
                        if let Some(name) = self.word(candidate, cancel)?
                            && formals.contains_key(name)
                        {
                            return Ok(false);
                        }
                    }
                }
            }
        }
        Ok(!self.exhausted)
    }

    fn initialize(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        let mut root = node;
        while let Some(parent) = root.parent() {
            if !self.step(cancel)? {
                return Ok(());
            }
            root = parent;
        }
        self.complete &= root.kind() == "source_file"
            && self.clean(root)
            && root.end_byte() == self.source.len()
            && (root.start_byte() == 0
                || (root.start_byte() == 3 && self.source.starts_with(b"\xef\xbb\xbf")));
        if !self.complete {
            return Ok(());
        }
        let all = self.nodes(root, cancel)?;
        let mut counts = HashMap::new();
        counts
            .try_reserve(all.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for item in &all {
            if let Some((name, _, _)) = self.delayed(*item, cancel)? {
                let Some(spelling) = self.word(name, cancel)? else {
                    continue;
                };
                counts
                    .entry(spelling)
                    .and_modify(|n| *n = 2_usize)
                    .or_insert(1);
            }
            if item.kind() == "binary" {
                let children = self.children(*item, cancel)?;
                if let [left, operator, _] = children.as_slice()
                    && matches!(operator.kind(), "=" | "+=" | "-=" | "*=" | "/=" | "^=")
                    && let Some(name) = self.word(*left, cancel)?
                {
                    counts.entry(name).and_modify(|n| *n = 2_usize).or_insert(2);
                }
            }
        }
        let module_rebound = counts.contains_key(b"Module".as_slice());
        for item in &all {
            let Some(function) = self.function(*item, cancel)? else {
                continue;
            };
            let Some(name) = self.word(function.name, cancel)? else {
                continue;
            };
            if counts.get(name) != Some(&1) {
                continue;
            }
            self.declarations
                .try_reserve(1 + function.formals.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            self.definitions
                .try_reserve(1 + function.formals.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            self.declarations.insert(
                function.node.id(),
                Declaration {
                    name: function.name.byte_range(),
                    header: function.node.start_byte()..function.body.start_byte(),
                    label: "wolfram.function",
                },
            );
            self.definitions
                .insert(function.name.id(), function.name.byte_range());
            for formal in &function.formals {
                self.declarations.insert(
                    formal.pattern.id(),
                    Declaration {
                        name: formal.name.byte_range(),
                        header: formal.pattern.byte_range(),
                        label: "wolfram.parameter",
                    },
                );
                self.definitions
                    .insert(formal.name.id(), formal.name.byte_range());
            }
            if module_rebound {
                continue;
            }
            let Some(body) = self.module(&function, cancel)? else {
                continue;
            };
            let mut formals = HashMap::new();
            formals
                .try_reserve(function.formals.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            for formal in &function.formals {
                let Some(name) = self.word(formal.name, cancel)? else {
                    continue;
                };
                formals.insert(name, formal.pattern.start_byte());
            }
            let body_nodes = self.nodes(body, cancel)?;
            if !self.body_safe(&body_nodes, &formals, cancel)? {
                continue;
            }
            for site in &body_nodes {
                if !self.step(cancel)? {
                    return Ok(());
                }
                if site.start_byte() < body.start_byte() || site.end_byte() > body.end_byte() {
                    continue;
                }
                let Some(spelling) = self.word(*site, cancel)? else {
                    continue;
                };
                let Some(target_start) = formals.get(spelling).copied() else {
                    continue;
                };
                let Some(call) = site.parent().filter(|p| p.kind() == "call") else {
                    continue;
                };
                if call.child_by_field_name("arguments") != Some(*site)
                    || !self.read_path(*site, body, function.node, cancel)?
                {
                    continue;
                }
                self.reads
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.markers
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.reads.insert(site.id(), site.byte_range());
                self.markers
                    .insert(call.id(), target_start..site.end_byte());
            }
            self.contexts
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.contexts
                .insert(function.body.id(), function.node.byte_range());
        }
        Ok(())
    }
}

pub(super) fn candidate(
    node: Node<'_>,
    mut role: StructuralRole,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cache.initialize(node, cancel)?;
    if !cache.step(cancel)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let mut required = role.belongs_to_identity_closure();
    let syntax = if node.parent().is_none() && node.kind() == "source_file" {
        match role {
            StructuralRole::Root => {
                if cache.complete {
                    range = 0..source.len();
                }
                "wolfram.file"
            }
            StructuralRole::Module | StructuralRole::Scope if cache.complete => {
                range = 0..source.len();
                "wolfram.file_context"
            }
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Signature && cache.contexts.contains_key(&node.id()) {
        range = cache.contexts[&node.id()].clone();
        "wolfram.formal_binding_context"
    } else if role == StructuralRole::Signature && cache.markers.contains_key(&node.id()) {
        range = cache.markers[&node.id()].clone();
        "wolfram.formal_target_context"
    } else if matches!(role, StructuralRole::Reference | StructuralRole::Signature)
        && node.kind() == "symbol"
    {
        if cache.definitions.contains_key(&node.id()) || cache.word(node, cancel)?.is_none() {
            return Ok(None);
        }
        if let Some(read) = cache.reads.get(&node.id()) {
            range = read.clone();
            role = StructuralRole::Reference;
            required = true;
            "wolfram.formal_argument"
        } else if role == StructuralRole::Reference {
            "wolfram.unproved_name"
        } else {
            return Ok(None);
        }
    } else if let Some(declaration) = cache.declarations.get(&node.id()) {
        match role {
            StructuralRole::Declaration | StructuralRole::Scope => declaration.label,
            StructuralRole::Definition => {
                range = declaration.name.clone();
                "wolfram.name"
            }
            StructuralRole::Signature => {
                range = declaration.header.clone();
                "wolfram.header"
            }
            _ => return Ok(None),
        }
    } else {
        match role {
            StructuralRole::Signature if node.kind() == "binary" => "wolfram.unproved_construct",
            StructuralRole::Comment => "wolfram.comment",
            StructuralRole::StringLiteral => "wolfram.string",
            _ => return Ok(None),
        }
    };
    let mut depth = 0_usize;
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        if !cache.step(cancel)? {
            return Ok(None);
        }
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("wolfram-native-depth"))?;
        parent = ancestor.parent();
    }
    if cache.exhausted {
        return Ok(None);
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required,
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    struct NeverCancelled;
    impl tree_sitter_wolfram::CancellationProbe for NeverCancelled {
        fn is_cancelled(&self) -> bool {
            false
        }
    }

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let scope = tree_sitter_wolfram::ScannerScope::begin(NeverCancelled, 1_000_000).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_wolfram::LANGUAGE.into())
            .unwrap();
        let parsed = parser.parse(source, None).unwrap();
        drop(parser);
        scope.finish().unwrap();
        parsed
    }

    fn find<'tree>(root: Node<'tree>, kind: &str, start: usize) -> Node<'tree> {
        let mut cursor = root.walk();
        loop {
            let node = cursor.node();
            if node.kind() == kind && node.start_byte() == start {
                return node;
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                assert!(cursor.goto_parent(), "native {kind}@{start} exists");
            }
        }
    }

    #[test]
    fn original_native_binary_patterns_module_context_and_target_are_physical() {
        let source = include_bytes!("../../tests/fixtures/wolfram/SocketUtilities.wl");
        assert_eq!(source.len(), 3944);
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let mut cache = CaptureCache::new(source, 16384, true);
        let cancel = Cancellation::new();
        for (kind, at, role, span, syntax) in [
            (
                "binary",
                2679,
                StructuralRole::Definition,
                2679..2683,
                "wolfram.name",
            ),
            (
                "pattern",
                2684,
                StructuralRole::Definition,
                2684..2687,
                "wolfram.name",
            ),
            (
                "pattern",
                2696,
                StructuralRole::Definition,
                2696..2703,
                "wolfram.name",
            ),
            (
                "call",
                2718,
                StructuralRole::Signature,
                2679..3692,
                "wolfram.formal_binding_context",
            ),
            (
                "call",
                3000,
                StructuralRole::Signature,
                2684..3021,
                "wolfram.formal_target_context",
            ),
            (
                "symbol",
                3018,
                StructuralRole::Signature,
                3018..3021,
                "wolfram.formal_argument",
            ),
        ] {
            let selected = candidate(
                find(parsed.root_node(), kind, at),
                role,
                source,
                &cancel,
                &mut cache,
            )
            .unwrap()
            .unwrap();
            assert_eq!(
                (selected.start, selected.end, selected.syntax),
                (span.start, span.end, syntax)
            );
            assert!(selected.required);
        }
        assert!(!cache.exhausted());
    }

    #[test]
    fn full_bom_is_canonical_but_partial_context_work_exhaustion_and_cancel_cannot_issue_proofs() {
        let source = b"\xef\xbb\xbfowned[value_String] := Module[{}, external[value]];\n";
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let mut cache = CaptureCache::new(source, 16384, true);
        let root = candidate(
            parsed.root_node(),
            StructuralRole::Root,
            source,
            &Cancellation::new(),
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!((root.start, root.end), (0, source.len()));
        let owner = find(parsed.root_node(), "binary", 3);
        let mut partial = CaptureCache::new(source, 16384, false);
        assert!(
            candidate(
                owner,
                StructuralRole::Declaration,
                source,
                &Cancellation::new(),
                &mut partial
            )
            .unwrap()
            .is_none()
        );
        let mut small = CaptureCache::new(source, 1, true);
        small.remaining = 1;
        assert!(
            candidate(
                owner,
                StructuralRole::Declaration,
                source,
                &Cancellation::new(),
                &mut small
            )
            .unwrap()
            .is_none()
        );
        assert!(small.exhausted());
        let scope = tree_sitter_wolfram::ScannerScope::begin(NeverCancelled, 1_000_000).unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_wolfram::LANGUAGE.into())
            .unwrap();
        parser
            .set_included_ranges(&[tree_sitter::Range {
                start_byte: 3,
                end_byte: source.len(),
                start_point: tree_sitter::Point::new(0, 3),
                end_point: tree_sitter::Point::new(1, 0),
            }])
            .unwrap();
        let partial_tree = parser.parse(source, None).unwrap();
        drop(parser);
        scope.finish().unwrap();
        assert!(!partial_tree.root_node().has_error());
        let pack = super::super::QueryPack::compile(
            crate::GrammarFamily::Wolfram,
            include_str!("../../queries/wolfram.scm"),
        )
        .unwrap();
        let candidates = pack
            .extract_identity(
                crate::GrammarFamily::Wolfram,
                &partial_tree,
                source,
                16_384,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(
            candidates
                .iter()
                .filter(|fact| fact.role == StructuralRole::Root)
                .count(),
            1
        );
        assert!(!candidates.iter().any(|fact| matches!(
            fact.role,
            StructuralRole::Module | StructuralRole::Declaration | StructuralRole::Definition
        )));
        let stop = Cancellation::new();
        assert!(stop.cancel(CancellationReason::ClientRequest));
        let mut full = CaptureCache::new(source, 16384, true);
        assert!(matches!(
            candidate(owner, StructuralRole::Declaration, source, &stop, &mut full),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn native_module_local_shadow_and_formal_writes_leave_only_uncertified_references() {
        for source in [
            b"owned[value_String] := Module[{value}, external[value]];\n".as_slice(),
            b"owned[value_String] := Module[{}, value=other; external[value]];\n",
            b"owned[value_String] := Module[{}, value++; external[value]];\n",
            b"owned[value_String] := Module[{}, Block[{value}, external[value]]];\n",
        ] {
            let parsed = tree(source);
            assert!(!parsed.root_node().has_error());
            let at = std::str::from_utf8(source).unwrap().rfind("value").unwrap();
            let site = find(parsed.root_node(), "symbol", at);
            let mut cache = CaptureCache::new(source, 16384, true);
            let selected = candidate(
                site,
                StructuralRole::Reference,
                source,
                &Cancellation::new(),
                &mut cache,
            )
            .unwrap()
            .unwrap();
            assert_eq!(selected.syntax, "wolfram.unproved_name");
            assert!(!selected.required);
        }
    }
}
