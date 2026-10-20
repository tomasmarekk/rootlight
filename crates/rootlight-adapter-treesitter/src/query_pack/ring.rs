//! Ring written top-level functions are independent of class-method binding proof.
//! Only the closed class census supplies first-argument formal Read certificates.

mod top_level;

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole, query_failure};

struct Declaration {
    extent: Range<usize>,
    name: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
}

/// A parser-run-local census cannot be reused for another source or partial parse.
pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    eligible: bool,
    initialized: bool,
    remaining: usize,
    exhausted: bool,
    declarations: HashMap<usize, Declaration>,
    reads: HashMap<usize, Range<usize>>,
}

impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            eligible: complete,
            initialized: false,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(32),
            exhausted: false,
            declarations: HashMap::new(),
            reads: HashMap::new(),
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

    fn bare(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'s str>, AdapterError> {
        if node.kind() != "identifier" || !clean(node, self.source) {
            return Ok(None);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in raw.chunks(64) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
        }
        let Ok(text) = std::str::from_utf8(raw) else {
            return Ok(None);
        };
        Ok(
            rootlight_adapter_sdk::structural_captured_name_for_language("ring", text, text.len())
                .filter(|name| name.as_ref() == text)
                .map(|_| text),
        )
    }

    fn equal(
        &mut self,
        left: &str,
        right: &str,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        for _ in left.as_bytes().chunks(64) {
            if !self.visit(cancel)? {
                return Ok(false);
            }
        }
        Ok(left.eq_ignore_ascii_case(right))
    }

    fn insert(
        &mut self,
        node: Node<'_>,
        name: Node<'_>,
        header_end: usize,
        syntax: &'static str,
    ) -> Result<(), AdapterError> {
        self.declarations
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.declarations.insert(
            node.id(),
            Declaration {
                extent: node.byte_range(),
                name: name.byte_range(),
                header: node.start_byte()..header_end,
                syntax,
            },
        );
        Ok(())
    }

    fn literal_assignment(
        &mut self,
        node: Node<'_>,
        parameters: &[(Node<'_>, &'s str)],
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if node.kind() != "expression_statement" || node.named_child_count() != 1 {
            return Ok(false);
        }
        let Some(assignment) = node
            .named_child(0)
            .filter(|node| node.kind() == "assignment_expression")
        else {
            return Ok(false);
        };
        if assignment.child_count() != 3 || assignment.named_child_count() != 2 {
            return Ok(false);
        }
        let Some(left) = assignment.child(0) else {
            return Ok(false);
        };
        let Some(operator) = assignment.child(1) else {
            return Ok(false);
        };
        let Some(right) = assignment.child(2) else {
            return Ok(false);
        };
        let Some(name) = self.bare(left, cancel)? else {
            return Ok(false);
        };
        if operator.kind() != "="
            || self.source.get(operator.byte_range()) != Some(b"=")
            || !matches!(right.kind(), "number" | "string")
            || right.named_child_count() != 0
        {
            return Ok(false);
        }
        if right.kind() == "string" {
            let Some(raw) = self.source.get(right.byte_range()) else {
                return Ok(false);
            };
            // Quoted text can interpolate effects even when its CST is opaque.
            for (index, pair) in raw.windows(2).enumerate() {
                if index.is_multiple_of(64) && !self.visit(cancel)? {
                    return Ok(false);
                }
                if pair == b"#{" {
                    return Ok(false);
                }
            }
        }
        for (_, parameter) in parameters {
            if self.equal(name, parameter, cancel)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn first_argument<'tree>(
        &mut self,
        mut node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        loop {
            if !self.visit(cancel)? || !clean(node, self.source) {
                return Ok(None);
            }
            node = match node.kind() {
                "expression_statement" | "return_statement" | "parenthesized_expression"
                    if node.named_child_count() == 1 =>
                {
                    let Some(child) = node.named_child(0) else {
                        return Ok(None);
                    };
                    child
                }
                "if_statement" => {
                    let Some(condition) = node.child_by_field_name("condition") else {
                        return Ok(None);
                    };
                    condition
                }
                "binary_expression" => {
                    let Some(left) = node.named_child(0) else {
                        return Ok(None);
                    };
                    left
                }
                "call_expression" => {
                    if node.named_child_count() != 2 {
                        return Ok(None);
                    }
                    let Some(head) = node.named_child(0) else {
                        return Ok(None);
                    };
                    if self.bare(head, cancel)?.is_none() {
                        return Ok(None);
                    }
                    let Some(arguments) = node
                        .named_child(1)
                        .filter(|node| node.kind() == "arguments" && node.named_child_count() == 1)
                    else {
                        return Ok(None);
                    };
                    return Ok(arguments
                        .named_child(0)
                        .filter(|node| node.kind() == "identifier"));
                }
                _ => return Ok(None),
            };
        }
    }

    fn method(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        if !clean(node, self.source) || node.kind() != "function_definition" {
            return Ok(());
        }
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        if self.bare(name_node, cancel)?.is_none() {
            return Ok(());
        }
        let Some(keyword) = node.child(0) else {
            return Ok(());
        };
        if !matches!(keyword.kind(), "func" | "function" | "def")
            || !self
                .source
                .get(keyword.byte_range())
                .is_some_and(|raw| raw.eq_ignore_ascii_case(keyword.kind().as_bytes()))
        {
            return Ok(());
        }
        let mut opening = None;
        let mut closing = None;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(());
            }
            if child.kind() == "{" {
                opening = Some(child)
            }
            if child.kind() == "}" {
                closing = Some(child)
            }
        }
        let (Some(opening), Some(closing)) = (opening, closing) else {
            return Ok(());
        };
        if closing.end_byte() != node.end_byte() || opening.start_byte() < name_node.end_byte() {
            return Ok(());
        }
        self.insert(node, name_node, opening.start_byte(), "ring.method")?;
        let mut parameters = Vec::new();
        let mut proved_parameters = true;
        if let Some(list) = node.child_by_field_name("parameters") {
            if list.kind() != "param_list"
                || !clean(list, self.source)
                || list.end_byte() > opening.start_byte()
            {
                return Ok(());
            }
            let mut cursor = list.walk();
            for parameter in list.named_children(&mut cursor) {
                if !self.visit(cancel)? {
                    return Ok(());
                }
                let Some(name) = self.bare(parameter, cancel)? else {
                    proved_parameters = false;
                    continue;
                };
                self.insert(parameter, parameter, parameter.end_byte(), "ring.parameter")?;
                parameters
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                parameters.push((parameter, name));
            }
        }
        if !proved_parameters {
            return Ok(());
        }
        // Ring can evaluate code in a caller scope. The finite certificate stops at
        // the first unknown effect; later statements cannot retroactively certify it.
        let mut cursor = node.walk();
        for statement in node.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(());
            }
            if statement.start_byte() <= opening.start_byte() || statement.kind() == "comment" {
                continue;
            }
            if self.literal_assignment(statement, &parameters, cancel)? {
                continue;
            }
            if let Some(read) = self.first_argument(statement, cancel)?
                && let Some(name) = self.bare(read, cancel)?
            {
                let mut target = None;
                let mut count = 0_usize;
                for (parameter, spelling) in &parameters {
                    if self.equal(name, spelling, cancel)? {
                        count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                        target = Some(*parameter);
                    }
                }
                if count == 1
                    && let Some(target) = target
                {
                    self.reads
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.reads
                        .insert(read.id(), target.start_byte()..read.end_byte());
                }
            }
            break;
        }
        Ok(())
    }

    fn initialize(
        &mut self,
        mut root: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        while let Some(parent) = root.parent() {
            if !self.visit(cancel)? {
                return Ok(());
            }
            root = parent;
        }
        if !self.eligible
            || root.kind() != "source_file"
            || !clean(root, self.source)
            || root.end_byte() != self.source.len()
            || !(root.start_byte() == 0
                || (root.start_byte() == 3 && self.source.starts_with(b"\xef\xbb\xbf")))
        {
            self.eligible = false;
            return Ok(());
        }
        self.collect_top_level_functions(root, cancel)?;
        if self.exhausted {
            return Ok(());
        }
        let mut class = None;
        let mut count = 0_usize;
        let mut cursor = root.walk();
        loop {
            if !self.visit(cancel)? {
                return Ok(());
            }
            let current = cursor.node();
            if current.kind() == "class_definition" {
                count = count.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                class = Some(current);
            }
            if matches!(current.kind(), "scanner_command" | "package_definition") {
                self.eligible = false;
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
        if count != 1 || !self.eligible {
            self.eligible = false;
            return Ok(());
        }
        let Some(class) = class.filter(|class| {
            class.parent() == Some(root) && class.child_by_field_name("parent").is_none()
        }) else {
            self.eligible = false;
            return Ok(());
        };
        let mut cursor = root.walk();
        for child in root.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(());
            }
            if child != class && child.kind() != "comment" {
                self.eligible = false;
                return Ok(());
            }
        }
        let Some(name) = class.child_by_field_name("name") else {
            self.eligible = false;
            return Ok(());
        };
        let Some(keyword) = class.child(0) else {
            self.eligible = false;
            return Ok(());
        };
        if keyword.kind() != "class"
            || !self
                .source
                .get(keyword.byte_range())
                .is_some_and(|raw| raw.eq_ignore_ascii_case(b"class"))
            || self.bare(name, cancel)?.is_none()
        {
            self.eligible = false;
            return Ok(());
        }
        self.insert(class, name, name.end_byte(), "ring.class")?;
        let mut cursor = class.walk();
        for child in class.named_children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(());
            }
            if child.kind() == "function_definition" {
                self.method(child, cancel)?
            }
        }
        Ok(())
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.has_error()
        && !node.is_missing()
        && !node.is_error()
        && source.get(node.byte_range()).is_some()
}

fn bare_argument(node: Node<'_>) -> bool {
    node.kind() == "identifier"
        && node.parent().is_some_and(|parent| {
            parent.kind() == "arguments"
                && parent
                    .parent()
                    .is_some_and(|call| call.kind() == "call_expression")
        })
}

/// Convert exact native nodes, keeping proof endpoints in mandatory syntax closure.
/// Cache exhaustion and cancellation must be handled by the shared query runner.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancel.check()?;
    if !std::ptr::eq(source, cache.source) {
        return Err(query_failure("ring-cache-source-mismatch"));
    }
    cache.initialize(node, cancel)?;
    if !cache.visit(cancel)? {
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
            "ring.source_file"
        } else {
            "ring.file"
        }
    } else if let Some(declaration) = cache.declarations.get(&node.id()) {
        match role {
            StructuralRole::Declaration | StructuralRole::Scope => {
                range = declaration.extent.clone();
                declaration.syntax
            }
            StructuralRole::Definition => {
                range = declaration.name.clone();
                "ring.name"
            }
            StructuralRole::Signature if declaration.syntax != "ring.parameter" => {
                range = declaration.header.clone();
                "ring.header"
            }
            _ => return Ok(None),
        }
    } else if matches!(role, StructuralRole::Declaration) {
        "ring.unsupported"
    } else if matches!(role, StructuralRole::Reference | StructuralRole::Signature)
        && bare_argument(node)
        && clean(node, source)
        && cache.bare(node, cancel)?.is_some()
    {
        if let Some(marker) = cache.reads.get(&node.id()) {
            if role == StructuralRole::Signature {
                range = marker.clone();
                "ring.parameter_read_target"
            } else {
                "ring.parameter_read"
            }
        } else if role == StructuralRole::Reference {
            "ring.unresolved_read"
        } else {
            return Ok(None);
        }
    } else if role == StructuralRole::Expression && cache.declarations.contains_key(&node.id()) {
        return Ok(None);
    } else if role == StructuralRole::Expression {
        "ring.construct_unavailable"
    } else if role == StructuralRole::Comment && cache.eligible {
        "ring.comment"
    } else if role == StructuralRole::StringLiteral && cache.eligible {
        "ring.string"
    } else {
        return Ok(None);
    };
    let mut depth = 0_usize;
    let mut parent = node.parent();
    while let Some(current) = parent {
        if !cache.visit(cancel)? {
            return Ok(None);
        }
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("ring-native-depth"))?;
        parent = current.parent();
    }
    if syntax == "ring.parameter_read_target" {
        depth = 0
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure() || syntax == "ring.parameter_read",
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn census(
        source: &[u8],
        maximum: usize,
        complete: bool,
        check: impl FnOnce(&CaptureCache<'_>),
    ) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_ring::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let mut cache = CaptureCache::new(source, maximum, complete);
        cache
            .initialize(tree.root_node(), &Cancellation::new())
            .unwrap();
        check(&cache);
    }

    #[test]
    fn original_native_census_certifies_only_the_required_first_argument_endpoint() {
        let original = include_bytes!("../../tests/fixtures/ring/htmlfunctions.ring");
        census(original, 16384, true, |cache| {
            assert!(cache.eligible && !cache.exhausted);
            assert!(
                cache
                    .declarations
                    .values()
                    .any(|declaration| declaration.syntax == "ring.class"
                        && declaration.name == (209..222))
            );
            assert!(
                cache
                    .declarations
                    .values()
                    .any(|declaration| declaration.syntax == "ring.method"
                        && declaration.name == (383..399))
            );
            assert!(
                cache
                    .declarations
                    .values()
                    .any(|declaration| declaration.syntax == "ring.parameter"
                        && declaration.name == (400..404))
            );
            assert!(cache.reads.values().any(|range| *range == (400..443)));
            assert!(!cache.reads.values().any(|range| range.end == 457));
        });
    }

    #[test]
    fn native_first_effect_duplicate_formal_and_class_census_are_fail_closed() {
        for source in [
            "class Written\nfunc measure value,VALUE { consume(value) }\n",
            "class Written\nfunc measure value { prior() consume(value) }\n",
            "class Written\nfunc measure value { text=\"#{prior()}\" consume(value) }\n",
            "class Written\nfunc measure value { text='#{prior()}' consume(value) }\n",
            "class Written\nfunc measure value { text=`#{prior()}` consume(value) }\n",
            "class Written\nfunc measure value { value=1 consume(value) }\n",
            "class First\nfunc first value { consume(value) }\nclass Second\nfunc second value { consume(value) }\n",
            "ChangeRingKeyword func task\nclass Written\ntask measure value { consume(value) }\n",
        ] {
            census(source.as_bytes(), 4096, true, |cache| {
                assert!(cache.reads.is_empty(), "{source}")
            });
        }
        for source in [
            "class Written\nfunc measure value { consume(VALUE) }\n",
            "class Written\nfunc measure(value) { text=\"inert\" if consume(value) { return 1 } }\n",
        ] {
            census(source.as_bytes(), 4096, true, |cache| {
                assert_eq!(cache.reads.len(), 1, "{source}")
            });
        }
    }

    #[test]
    fn native_cache_bound_partial_context_and_cancellation_do_not_emit_certificate() {
        let source = b"class Written\nfunc measure value { consume(value) }\n";
        census(source, 1, true, |cache| {
            assert!(cache.exhausted);
            assert!(cache.reads.is_empty());
        });
        census(source, 4096, false, |cache| {
            assert!(!cache.eligible);
            assert!(cache.reads.is_empty());
        });
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_ring::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            CaptureCache::new(source, 4096, true).initialize(tree.root_node(), &cancel),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
    }

    #[test]
    fn native_query_keeps_each_owner_role_and_body_independent_headers() {
        use tree_sitter::{Query, QueryCursor, StreamingIterator};

        let original = "class Written\nfunc measure value { consume(value) return 1 }\n";
        let changed = original.replace("return 1", "return 123456");
        let language: tree_sitter::Language = tree_sitter_ring::LANGUAGE.into();
        let query = Query::new(&language, include_str!("../../queries/ring.scm")).unwrap();
        let mut previous_headers = None;
        for source in [original, changed.as_str()] {
            let mut parser = tree_sitter::Parser::new();
            parser.set_language(&language).unwrap();
            let tree = parser.parse(source, None).unwrap();
            assert!(!tree.root_node().has_error());
            let mut cache = CaptureCache::new(source.as_bytes(), 4096, true);
            let mut cursor = QueryCursor::new();
            let mut matches = cursor.matches(&query, tree.root_node(), source.as_bytes());
            let mut roles = std::collections::BTreeMap::<(String, usize), Vec<String>>::new();
            let mut headers = Vec::new();
            while let Some(matched) = matches.next() {
                for capture in matched.captures {
                    if !matches!(
                        capture.node.kind(),
                        "class_definition" | "function_definition"
                    ) {
                        continue;
                    }
                    let name = query.capture_names()[usize::try_from(capture.index).unwrap()];
                    if !["scope", "declaration", "definition", "signature"].contains(&name) {
                        continue;
                    }
                    roles
                        .entry((capture.node.kind().to_owned(), capture.node.start_byte()))
                        .or_default()
                        .push(name.to_owned());
                    if name == "signature" {
                        let header = candidate(
                            capture.node,
                            StructuralRole::Signature,
                            source.as_bytes(),
                            &Cancellation::new(),
                            &mut cache,
                        )
                        .unwrap()
                        .unwrap();
                        assert_eq!(header.syntax, "ring.header");
                        assert!(header.required);
                        headers.push((
                            header.start,
                            header.end,
                            source[header.start..header.end].to_owned(),
                        ));
                    }
                }
            }
            assert_eq!(roles.len(), 2);
            for role_names in roles.values_mut() {
                role_names.sort();
                assert_eq!(
                    role_names.iter().map(String::as_str).collect::<Vec<_>>(),
                    ["declaration", "definition", "scope", "signature"]
                );
            }
            headers.sort();
            let method = source.find("func measure").unwrap();
            let body = source[method..].find('{').unwrap() + method;
            assert_eq!(
                headers,
                [
                    (0, "class Written".len(), "class Written".to_owned()),
                    (method, body, "func measure value ".to_owned()),
                ]
            );
            if let Some(previous) = &previous_headers {
                assert_eq!(&headers, previous);
            }
            previous_headers = Some(headers);
            assert!(!cache.exhausted());
        }
    }
}
