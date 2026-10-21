//! Preserve clean written CoffeeScript owners without inferring runtime targets.
//! A local assignment additionally requires a complete source-owned binder census.
use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole, query_failure};
use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};
use tree_sitter::Node;

struct Declaration {
    name: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
}
pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    root: Option<usize>,
    remaining: usize,
    exhausted: bool,
    declarations: HashMap<usize, Declaration>,
    definitions: HashSet<(usize, usize)>,
}
fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_error()
        && !node.is_missing()
        && !node.has_error()
        && source.get(node.byte_range()).is_some()
}
impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            root: None,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(32),
            exhausted: false,
            declarations: HashMap::new(),
            definitions: HashSet::new(),
        }
    }
    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }
    fn visit(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        let Some(next) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = next;
        Ok(true)
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
            if !self.visit(cancel)? {
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
        if node.kind() != "identifier" || !clean(node, self.source) {
            return Ok(None);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in bytes.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
        }
        let Some((first, tail)) = bytes.split_first() else {
            return Ok(None);
        };
        Ok(
            ((*first == b'_' || *first == b'$' || first.is_ascii_alphabetic())
                && tail
                    .iter()
                    .all(|b| *b == b'_' || *b == b'$' || b.is_ascii_alphanumeric()))
            .then_some(bytes),
        )
    }
    fn insert(
        &mut self,
        node: Node<'_>,
        name: Node<'_>,
        header: Range<usize>,
        syntax: &'static str,
    ) -> Result<(), AdapterError> {
        self.declarations
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.definitions
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.definitions
            .insert((name.start_byte(), name.end_byte()));
        self.declarations.insert(
            node.id(),
            Declaration {
                name: name.byte_range(),
                header,
                syntax,
            },
        );
        Ok(())
    }
    fn assignment_name<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if node.kind() != "assignment_statement" || !clean(node, self.source) {
            return Ok(None);
        }
        let direct = self.children(node, cancel)?;
        let [pattern, operator, _value] = direct.as_slice() else {
            return Ok(None);
        };
        if pattern.kind() != "pattern"
            || operator.kind() != "="
            || self.source.get(operator.byte_range()) != Some(b"=")
        {
            return Ok(None);
        }
        let names = self.children(*pattern, cancel)?;
        let [name] = names.as_slice() else {
            return Ok(None);
        };
        Ok(self.word(*name, cancel)?.map(|_| *name))
    }
    fn direct_body_owner<'tree>(
        &mut self,
        node: Node<'tree>,
        owner: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = node;
        for kind in ["statement", "statement_sequence", "indented_block", "block"] {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            let Some(parent) = current
                .parent()
                .filter(|n| n.kind() == kind && clean(*n, self.source))
            else {
                return Ok(false);
            };
            if kind == "statement" {
                let children = self.children(parent, cancel)?;
                if children.len() != 1 || children[0].id() != current.id() {
                    return Ok(false);
                }
            }
            current = parent;
        }
        Ok(current
            .parent()
            .is_some_and(|parent| parent.id() == owner.id()))
    }
    fn variable_is_local(
        &mut self,
        assignment: Node<'_>,
        name: Node<'_>,
        owner: Node<'_>,
        root: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if !self.complete
            || !clean(root, self.source)
            || !self.direct_body_owner(assignment, owner, cancel)?
        {
            return Ok(false);
        }
        let Some(written) = self.word(name, cancel)? else {
            return Ok(false);
        };
        // CoffeeScript's function scope already contains the implicit arguments binding.
        if written == b"arguments" {
            return Ok(false);
        }
        let mut pending = Vec::new();
        pending
            .try_reserve_exact(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        pending.push(root);
        while let Some(node) = pending.pop() {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if matches!(node.kind(), "embedded_js" | "embedded_html") {
                return Ok(false);
            }
            let inside =
                owner.start_byte() <= node.start_byte() && node.end_byte() <= owner.end_byte();
            if inside
                && node.id() != owner.id()
                && matches!(
                    node.kind(),
                    "function_definition"
                        | "function_expression"
                        | "method_definition"
                        | "class_property_method"
                        | "class_definition"
                        | "for_statement"
                        | "comprehension"
                        | "try_statement"
                        | "var_statement"
                        | "import_statement"
                )
            {
                return Ok(false);
            }
            if node.kind() == "identifier" && self.word(node, cancel)? == Some(written) {
                if !inside {
                    return Ok(false);
                }
                let mut ancestor = node.parent();
                while let Some(parent) = ancestor {
                    if !self.visit(cancel)? {
                        return Ok(false);
                    }
                    if parent.id() == owner.id() {
                        break;
                    }
                    if parent.kind() == "parameter" {
                        return Ok(false);
                    }
                    ancestor = parent.parent();
                }
            }
            if inside
                && node.kind() == "assignment_statement"
                && node.id() != assignment.id()
                && let Some(other) = self.assignment_name(node, cancel)?
                && self.word(other, cancel)? == Some(written)
            {
                return Ok(false);
            }
            let children = self.children(node, cancel)?;
            pending
                .try_reserve(children.len())
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.extend(children);
        }
        Ok(!self.exhausted)
    }
    fn class<'tree>(
        &mut self,
        class: Node<'tree>,
        root: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        if !clean(class, self.source) {
            return Ok(());
        }
        let header = self.children(class, cancel)?;
        let [keyword, name, tail @ ..] = header.as_slice() else {
            return Ok(());
        };
        if keyword.kind() != "class" || self.word(*name, cancel)?.is_none() {
            return Ok(());
        }
        let body = match tail {
            [body] if body.kind() == "class_body" => *body,
            [extends, base, body]
                if extends.kind() == "extends"
                    && body.kind() == "class_body"
                    && self.word(*base, cancel)?.is_some() =>
            {
                *body
            }
            _ => return Ok(()),
        };
        self.insert(
            class,
            *name,
            class.start_byte()..body.start_byte(),
            "coffeescript.class",
        )?;
        let bodies = self.children(body, cancel)?;
        let Some(members) = bodies
            .iter()
            .find(|n| matches!(n.kind(), "class_indented_body" | "class_inline_body"))
        else {
            return Ok(());
        };
        for member in self.children(*members, cancel)? {
            if member.kind() != "class_member" {
                continue;
            }
            let entries = self.children(member, cancel)?;
            let [method] = entries.as_slice() else {
                continue;
            };
            if method.kind() != "method_definition" || !clean(*method, self.source) {
                continue;
            }
            let pieces = self.children(*method, cancel)?;
            let Some(method_name) = pieces.first().copied().filter(|n| n.kind() == "identifier")
            else {
                continue;
            };
            if self.word(method_name, cancel)?.is_none()
                || self.source.get(method_name.byte_range()) == Some(b"constructor")
                || pieces.get(1).is_none_or(|n| n.kind() != ":")
            {
                continue;
            }
            let Some(arrow) = pieces.iter().find(|n| matches!(n.kind(), "->" | "=>")) else {
                continue;
            };
            self.insert(
                *method,
                method_name,
                method.start_byte()..arrow.end_byte(),
                "coffeescript.method",
            )?;
            let mut pending = self.children(*method, cancel)?;
            while let Some(node) = pending.pop() {
                if !self.visit(cancel)? {
                    return Ok(());
                }
                if let Some(local) = self.assignment_name(node, cancel)?
                    && self.variable_is_local(node, local, *method, root, cancel)?
                {
                    self.insert(node, local, local.byte_range(), "coffeescript.variable")?;
                }
                let children = self.children(node, cancel)?;
                pending
                    .try_reserve(children.len())
                    .map_err(|_| SinkError::AllocationFailed)?;
                pending.extend(children);
            }
        }
        Ok(())
    }
    fn initialize(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        let mut root = node;
        while let Some(parent) = root.parent() {
            if !self.visit(cancel)? {
                return Ok(());
            }
            root = parent;
        }
        if let Some(id) = self.root {
            if id != root.id() {
                return Err(query_failure("coffeescript-cache-tree-mismatch"));
            }
            return Ok(());
        }
        self.root = Some(root.id());
        if !self.complete || root.kind() != "source_file" {
            return Ok(());
        }
        let prefix = self.source.get(..root.start_byte()).unwrap_or(&[]);
        if root.end_byte() != self.source.len() || (!prefix.is_empty() && prefix != b"\xef\xbb\xbf")
        {
            return Ok(());
        }
        for sequence in self.children(root, cancel)? {
            if sequence.kind() != "statement_sequence" {
                continue;
            }
            for statement in self.children(sequence, cancel)? {
                if statement.kind() != "statement" {
                    continue;
                }
                let content = self.children(statement, cancel)?;
                if let [class] = content.as_slice()
                    && class.kind() == "class_definition"
                {
                    self.class(*class, root, cancel)?;
                }
            }
        }
        if self.exhausted {
            self.declarations.clear();
            self.definitions.clear();
        }
        Ok(())
    }
}
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    if !std::ptr::eq(source, cache.source) {
        return Err(query_failure("coffeescript-cache-source-mismatch"));
    }
    cache.initialize(node, cancel)?;
    if !cache.visit(cancel)? || cache.exhausted {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let syntax = if node.parent().is_none()
        && matches!(
            role,
            StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
        ) {
        range = 0..source.len();
        if role == StructuralRole::Root {
            "coffeescript.source_file"
        } else {
            "coffeescript.file"
        }
    } else if let Some(decl) = cache.declarations.get(&node.id()) {
        match role {
            StructuralRole::Scope if decl.syntax != "coffeescript.variable" => decl.syntax,
            StructuralRole::Declaration => decl.syntax,
            StructuralRole::Definition => {
                range = decl.name.clone();
                "coffeescript.name"
            }
            StructuralRole::Signature if decl.syntax != "coffeescript.variable" => {
                range = decl.header.clone();
                "coffeescript.header"
            }
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Declaration {
        "coffeescript.unsupported"
    } else if role == StructuralRole::Reference && cache.word(node, cancel)?.is_some() {
        if cache.definitions.contains(&(range.start, range.end)) {
            return Ok(None);
        }
        "coffeescript.unresolved_name"
    } else if role == StructuralRole::Expression {
        "coffeescript.construct_unavailable"
    } else if role == StructuralRole::Comment {
        "coffeescript.comment"
    } else if role == StructuralRole::StringLiteral {
        "coffeescript.string"
    } else {
        return Ok(None);
    };
    let mut depth = 0_usize;
    let mut parent = node.parent();
    while let Some(ancestor) = parent {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("coffeescript-native-depth"))?;
        parent = ancestor.parent();
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;
    struct Probe(Cancellation);
    impl tree_sitter_coffeescript::CancellationProbe for Probe {
        fn is_cancelled(&self) -> bool {
            self.0.check().is_err()
        }
    }
    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let scope =
            tree_sitter_coffeescript::ScannerScope::begin(Probe(Cancellation::new()), 1_000_000)
                .unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_coffeescript::LANGUAGE.into())
            .unwrap();
        let parsed = parser.parse(source, None).unwrap();
        drop(parser);
        scope.finish().unwrap();
        parsed
    }
    #[test]
    fn original_native_classes_methods_and_local_assignment_keep_exact_names() {
        let source = include_bytes!("../../tests/fixtures/coffeescript/line.coffee");
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let mut cache = CaptureCache::new(source, 16_384, true);
        cache
            .initialize(parsed.root_node(), &Cancellation::new())
            .unwrap();
        for (syntax, span) in [
            ("coffeescript.class", 70..74),
            ("coffeescript.method", 89..105),
            ("coffeescript.method", 155..160),
            ("coffeescript.variable", 168..175),
        ] {
            assert_eq!(
                cache
                    .declarations
                    .values()
                    .filter(|decl| decl.syntax == syntax && decl.name == span)
                    .count(),
                1
            );
        }
        assert!(!cache.definitions.contains(&(462, 466)));
        assert!(!cache.exhausted());
    }
    #[test]
    fn ordinary_outer_parameter_duplicate_and_reader_binders_withhold_only_local_identity() {
        for source in [
            b"value = 0\nclass Written\n  owned: ->\n    value = 1\n".as_slice(),
            b"class Written\n  owned: (value) ->\n    value = 1\n".as_slice(),
            b"class Written\n  owned: ->\n    value = 1\n    value = 2\n".as_slice(),
            b"class Written\n  owned: ->\n    arguments = 1\n".as_slice(),
        ] {
            let parsed = tree(source);
            assert!(!parsed.root_node().has_error(), "{source:?}");
            let mut cache = CaptureCache::new(source, 16_384, true);
            cache
                .initialize(parsed.root_node(), &Cancellation::new())
                .unwrap();
            assert!(
                cache
                    .declarations
                    .values()
                    .any(|decl| decl.syntax == "coffeescript.class")
            );
            assert!(
                cache
                    .declarations
                    .values()
                    .any(|decl| decl.syntax == "coffeescript.method")
            );
            assert!(
                !cache
                    .declarations
                    .values()
                    .any(|decl| decl.syntax == "coffeescript.variable")
            );
        }
    }
    #[test]
    fn source_and_native_role_mismatches_cannot_donate_a_definition() {
        let source = include_bytes!("../../tests/fixtures/coffeescript/line.coffee");
        let parsed = tree(source);
        let mut cache = CaptureCache::new(source, 16_384, true);
        assert!(
            candidate(
                parsed.root_node(),
                StructuralRole::Definition,
                source,
                &Cancellation::new(),
                &mut cache
            )
            .unwrap()
            .is_none()
        );
        let foreign = source.to_vec();
        assert!(
            matches!(candidate(parsed.root_node(),StructuralRole::Root,&foreign,&Cancellation::new(),&mut cache),Err(AdapterError::ProviderFailed{code}) if code.as_str()=="coffeescript-cache-source-mismatch")
        );
    }
    #[test]
    fn partial_native_ranges_work_and_cancel_cannot_keep_partial_declarations() {
        let source = include_bytes!("../../tests/fixtures/coffeescript/line.coffee");
        let parsed = tree(source);
        let mut partial = CaptureCache::new(source, 16_384, false);
        partial
            .initialize(parsed.root_node(), &Cancellation::new())
            .unwrap();
        assert!(partial.declarations.is_empty());
        let mut tiny = CaptureCache::new(source, 16_384, true);
        tiny.remaining = 1;
        tiny.initialize(parsed.root_node(), &Cancellation::new())
            .unwrap();
        assert!(tiny.exhausted());
        assert!(tiny.declarations.is_empty());
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        let mut stopped = CaptureCache::new(source, 16_384, true);
        assert!(matches!(
            stopped.initialize(parsed.root_node(), &cancel),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert!(stopped.declarations.is_empty());
    }
}
