//! Prove one direct module assignment without inventing a symbol per write.
//! A whole-source census distinguishes module augmented writes from new binders.
//! Initial assignment identity never certifies values, execution or general reads.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole};

type NameKey = [u8; 32];

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    initialized: bool,
    available: bool,
    ambiguous: bool,
    remaining: usize,
    exhausted: bool,
    bindings: HashMap<NameKey, u8>,
    ordinary_bindings: HashMap<NameKey, u8>,
    first_module_update: HashMap<NameKey, usize>,
    module_names: HashSet<NameKey>,
    initial_assignments: HashMap<NameKey, Option<(usize, usize)>>,
    flat_rhs: HashMap<usize, bool>,
    reads: HashMap<usize, Option<(usize, usize)>>,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            initialized: false,
            available: false,
            ambiguous: false,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(64),
            exhausted: false,
            bindings: HashMap::new(),
            ordinary_bindings: HashMap::new(),
            first_module_update: HashMap::new(),
            module_names: HashSet::new(),
            initial_assignments: HashMap::new(),
            flat_rhs: HashMap::new(),
            reads: HashMap::new(),
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

    fn key(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<NameKey>, AdapterError> {
        if !self.step(cancel)? || node.is_error() || node.is_missing() || node.has_error() {
            return Ok(None);
        }
        let Some(raw) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        let Some(first) = raw.first() else {
            return Ok(None);
        };
        if !(first.is_ascii_alphabetic() || *first == b'_') {
            return Ok(None);
        }
        let mut hash = blake3::Hasher::new_derive_key("rootlight.python-module-binder-spelling/1");
        for chunk in raw.chunks(64) {
            if !self.step(cancel)? {
                return Ok(None);
            }
            if !chunk
                .iter()
                .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
            {
                return Ok(None);
            }
            hash.update(chunk);
        }
        Ok(Some(*hash.finalize().as_bytes()))
    }

    fn record(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        self.record_origin(node, None, cancel)
    }

    fn record_origin(
        &mut self,
        node: Node<'_>,
        module_update: Option<usize>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        let Some(key) = self.key(node, cancel)? else {
            // Unknown spelling can normalize to an ASCII Python identifier.
            self.ambiguous = true;
            return Ok(());
        };
        self.bindings
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        let count = self.bindings.entry(key).or_default();
        // Only uniqueness is stored; this is not a reported operation count.
        *count = count.saturating_add(1).min(2);
        if let Some(start) = module_update {
            self.first_module_update
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.first_module_update
                .entry(key)
                .and_modify(|first| *first = (*first).min(start))
                .or_insert(start);
        } else {
            self.ordinary_bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            let count = self.ordinary_bindings.entry(key).or_default();
            *count = count.saturating_add(1).min(2);
        }
        Ok(())
    }

    fn target(&mut self, target: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        let mut walk = target.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(());
            }
            let node = walk.node();
            let skip = if !node.is_named() {
                false
            } else {
                match node.kind() {
                    "attribute" | "subscript" => true,
                    "identifier" => {
                        self.record(node, cancel)?;
                        true
                    }
                    "as_pattern_target" if node.named_child_count() == 0 => {
                        self.record(node, cancel)?;
                        true
                    }
                    "tuple_pattern"
                    | "list_pattern"
                    | "pattern_list"
                    | "list_splat_pattern"
                    | "dictionary_splat_pattern"
                    | "as_pattern_target" => false,
                    _ => {
                        self.ambiguous = true;
                        true
                    }
                }
            };
            if !skip && walk.goto_first_child() {
                continue;
            }
            loop {
                if walk.goto_next_sibling() {
                    break;
                }
                if !walk.goto_parent() {
                    return Ok(());
                }
            }
        }
    }

    fn inspect(&mut self, root: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        self.available = self.complete
            && root.kind() == "module"
            && root.parent().is_none()
            && (root.start_byte() == 0
                || (root.start_byte() == 3 && self.source.starts_with(b"\xef\xbb\xbf")))
            && root.end_byte() == self.source.len()
            && !root.has_error()
            && !root.is_missing()
            && !root.is_error();
        if !self.available {
            return Ok(());
        }
        let mut walk = root.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(());
            }
            let node = walk.node();
            match node.kind() {
                // Comprehension targets bind their implicit local scope. Keep
                // descending so walrus and dynamic namespace barriers remain.
                "for_in_clause"
                    if node.parent().is_some_and(|parent| {
                        matches!(
                            parent.kind(),
                            "list_comprehension"
                                | "set_comprehension"
                                | "dictionary_comprehension"
                                | "generator_expression"
                        )
                    }) => {}
                "assignment" | "augmented_assignment" | "for_statement" | "for_in_clause" => {
                    if let Some(left) = node.child_by_field_name("left") {
                        if node.kind() == "augmented_assignment"
                            && left.kind() == "identifier"
                            && self.same_module_update(node, cancel)?
                        {
                            self.record_origin(left, Some(node.start_byte()), cancel)?;
                        } else {
                            self.target(left, cancel)?;
                        }
                    } else {
                        self.ambiguous = true;
                    }
                    if node.kind() == "assignment"
                        && Self::direct_module_assignment(node)
                        && node.child_by_field_name("type").is_none()
                        && node
                            .child_by_field_name("right")
                            .is_some_and(|right| right.kind() != "assignment")
                        && let Some(name) = node
                            .child_by_field_name("left")
                            .filter(|name| name.kind() == "identifier")
                        && let Some(key) = self.key(name, cancel)?
                    {
                        self.module_names
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.module_names.insert(key);
                        self.initial_assignments
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.initial_assignments
                            .entry(key)
                            .and_modify(|old| *old = None)
                            .or_insert(Some((name.end_byte(), node.end_byte())));
                    }
                }
                "named_expression" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        self.target(name, cancel)?;
                    } else {
                        self.ambiguous = true;
                    }
                }
                "function_definition" | "class_definition" => {
                    if let Some(name) = node.child_by_field_name("name") {
                        self.record(name, cancel)?;
                    } else {
                        self.ambiguous = true;
                    }
                }
                "as_pattern" => {
                    if let Some(alias) = node.child_by_field_name("alias") {
                        self.target(alias, cancel)?;
                    }
                }
                "import_statement" | "import_from_statement" => {
                    let mut names = node.walk();
                    for name in node.children_by_field_name("name", &mut names) {
                        if !self.step(cancel)? {
                            return Ok(());
                        }
                        let binder = if name.kind() == "aliased_import" {
                            name.child_by_field_name("alias")
                        } else if name.kind() == "dotted_name" {
                            name.named_child(0)
                        } else {
                            None
                        };
                        if let Some(binder) = binder {
                            self.record(binder, cancel)?;
                        } else {
                            self.ambiguous = true;
                        }
                    }
                }
                "global_statement"
                | "nonlocal_statement"
                | "delete_statement"
                | "match_statement"
                | "except_clause"
                | "type_alias_statement"
                | "wildcard_import"
                | "exec_statement" => self.ambiguous = true,
                "identifier"
                    if self.source.get(node.byte_range()).is_some_and(|raw| {
                        [b"exec".as_slice(), b"eval", b"globals", b"locals", b"vars"].contains(&raw)
                    }) =>
                {
                    self.ambiguous = true;
                }
                _ => {}
            }
            // A nested write conservatively withholds a same-name module binder.
            // Scope-sensitive rebinding and dynamic aliases remain unproven.
            if walk.goto_first_child() {
                continue;
            }
            loop {
                if walk.goto_next_sibling() {
                    break;
                }
                if !walk.goto_parent() {
                    return Ok(());
                }
            }
        }
    }

    fn same_module_update(
        &mut self,
        mut node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        loop {
            if !self.step(cancel)? {
                return Ok(false);
            }
            let Some(parent) = node.parent() else {
                return Ok(false);
            };
            match parent.kind() {
                "module" => return Ok(true),
                "expression_statement"
                | "block"
                | "for_statement"
                | "while_statement"
                | "if_statement"
                | "elif_clause"
                | "else_clause" => node = parent,
                _ => return Ok(false),
            }
        }
    }

    fn admitted_assignment(&self, assignment: Node<'_>, key: NameKey) -> bool {
        self.module_names.contains(&key)
            && self.ordinary_bindings.get(&key) == Some(&1)
            && self.bindings.get(&key)
                == Some(&if self.first_module_update.contains_key(&key) {
                    2
                } else {
                    1
                })
            && self.first_module_update.get(&key).is_none_or(|first| {
                // The update is a use/write of the existing lexical slot. Its
                // source order is evidence, never proof that execution reaches it.
                *first >= assignment.end_byte()
            })
    }

    fn flat_rhs(&mut self, owner: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if let Some(valid) = self.flat_rhs.get(&owner.id()).copied() {
            return Ok(valid);
        }
        let Some(rhs) = owner.child_by_field_name("right") else {
            return Ok(false);
        };
        let mut valid = true;
        let mut walk = rhs.walk();
        loop {
            if !self.step(cancel)? {
                return Ok(false);
            }
            let node = walk.node();
            if node.is_missing()
                || node.is_error()
                || node.has_error()
                || (node.is_named()
                    && !matches!(
                        node.kind(),
                        "identifier"
                            | "integer"
                            | "float"
                            | "binary_operator"
                            | "unary_operator"
                            | "parenthesized_expression"
                    ))
                || (!node.is_named()
                    && !matches!(
                        node.kind(),
                        "+" | "-"
                            | "*"
                            | "/"
                            | "//"
                            | "%"
                            | "**"
                            | "~"
                            | "&"
                            | "|"
                            | "^"
                            | "<<"
                            | ">>"
                            | "("
                            | ")"
                    ))
            {
                valid = false;
                break;
            }
            if walk.goto_first_child() {
                continue;
            }
            loop {
                if walk.goto_next_sibling() {
                    break;
                }
                if !walk.goto_parent() {
                    self.flat_rhs
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.flat_rhs.insert(owner.id(), valid);
                    return Ok(valid);
                }
            }
        }
        self.flat_rhs
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.flat_rhs.insert(owner.id(), valid);
        Ok(valid)
    }

    fn read_target(
        &mut self,
        root: Node<'_>,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<(usize, usize)>, AdapterError> {
        self.inspect(root, cancel)?;
        if !self.available || self.ambiguous || node.kind() != "identifier" {
            return Ok(None);
        }
        if let Some(target) = self.reads.get(&node.id()).copied() {
            return Ok(target);
        }
        let mut current = node;
        let owner = loop {
            if !self.step(cancel)? {
                return Ok(None);
            }
            let Some(parent) = current.parent() else {
                return Ok(None);
            };
            if parent.kind() == "assignment" && parent.child_by_field_name("right") == Some(current)
            {
                break parent;
            }
            if !matches!(
                parent.kind(),
                "binary_operator" | "unary_operator" | "parenthesized_expression"
            ) {
                return Ok(None);
            }
            current = parent;
        };
        let selected = if Self::direct_module_assignment(owner)
            && owner.child_by_field_name("type").is_none()
            && let Some(owner_name) = owner
                .child_by_field_name("left")
                .filter(|name| name.kind() == "identifier")
            && let Some(owner_key) = self.key(owner_name, cancel)?
            && self.admitted_assignment(owner, owner_key)
            && self.flat_rhs(owner, cancel)?
            && let Some(key) = self.key(node, cancel)?
            && self.module_names.contains(&key)
            && self.ordinary_bindings.get(&key) == Some(&1)
            && self.bindings.get(&key)
                == Some(&if self.first_module_update.contains_key(&key) {
                    2
                } else {
                    1
                })
            && let Some(target) = self.initial_assignments.get(&key).copied().flatten()
            && target.1 <= owner.start_byte()
            && self
                .first_module_update
                .get(&key)
                .is_none_or(|first| *first >= target.1)
        {
            Some(target)
        } else {
            None
        };
        self.reads
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.reads.insert(node.id(), selected);
        Ok(selected)
    }

    fn direct_module_assignment(node: Node<'_>) -> bool {
        node.parent()
            .filter(|statement| statement.kind() == "expression_statement")
            .and_then(|statement| statement.parent())
            .is_some_and(|root| root.kind() == "module")
    }

    pub(super) fn refine(
        &mut self,
        root: Node<'_>,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if role == StructuralRole::Signature && node.kind() == "module" {
            self.inspect(root, cancel)?;
            if !self.available || self.ambiguous {
                return Ok(false);
            }
            candidate.start = 0;
            candidate.end = self.source.len();
            candidate.syntax = "python.module_binding_context";
            candidate.required = true;
            return Ok(true);
        }
        if role == StructuralRole::Scope && node.kind() == "identifier" {
            if let Some(target) = self.read_target(root, node, cancel)? {
                candidate.role = StructuralRole::Signature;
                candidate.syntax = "python.module_target";
                candidate.start = target.0;
                candidate.required = true;
                return Ok(true);
            }
            return Ok(false);
        }
        if role == StructuralRole::Reference
            && node.kind() == "identifier"
            && self.read_target(root, node, cancel)?.is_some()
        {
            candidate.syntax = "python.module_read";
            candidate.required = true;
            return Ok(true);
        }
        if role == StructuralRole::Signature && node.kind() == "augmented_assignment" {
            candidate.syntax = "python.augmented_assignment_binding_unavailable";
            candidate.required = true;
            return Ok(true);
        }
        if role == StructuralRole::Signature && node.kind() == "assignment" {
            candidate.syntax = "python.assignment_binding_unavailable";
            candidate.required = true;
            return Ok(true);
        }
        if matches!(role, StructuralRole::Reference | StructuralRole::Call)
            && node.kind() == "identifier"
        {
            self.inspect(root, cancel)?;
            if self.available
                && !self.ambiguous
                && let Some(key) = self.key(node, cancel)?
                && self.module_names.contains(&key)
            {
                // Multiwrite names also stay fenced once the initial entity exists.
                // Written assignment identity does not certify any use or call.
                // The existing formal helper may subsequently prove its own read.
                candidate.syntax = if role == StructuralRole::Call {
                    "python.module_call_unavailable"
                } else {
                    "python.module_binding_unavailable"
                };
            }
        }
        let assignment = if role == StructuralRole::Declaration && node.kind() == "assignment" {
            Some(node)
        } else if matches!(role, StructuralRole::Definition | StructuralRole::Reference)
            && node.kind() == "identifier"
        {
            node.parent().filter(|owner| {
                owner.kind() == "assignment" && owner.child_by_field_name("left") == Some(node)
            })
        } else {
            None
        };
        let Some(assignment) = assignment else {
            return Ok(true);
        };
        if !Self::direct_module_assignment(assignment) {
            return Ok(true);
        }
        if role == StructuralRole::Reference {
            return Ok(false);
        }
        self.inspect(root, cancel)?;
        let name = assignment
            .child_by_field_name("left")
            .filter(|name| name.kind() == "identifier");
        let key = if let Some(name) = name {
            self.key(name, cancel)?
        } else {
            None
        };
        let admitted = self.available
            && !self.ambiguous
            && assignment.child_by_field_name("type").is_none()
            && assignment
                .child_by_field_name("right")
                .is_some_and(|right| right.kind() != "assignment")
            && key.is_some_and(|key| self.admitted_assignment(assignment, key));
        if admitted {
            candidate.syntax = if role == StructuralRole::Declaration {
                "python.module_variable"
            } else {
                "python.module_variable_name"
            };
        } else {
            candidate.role = StructuralRole::Signature;
            candidate.syntax = "python.module_assignment_unavailable";
            candidate.start = assignment.start_byte();
            candidate.end = assignment.end_byte();
        }
        candidate.required = true;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn candidate(node: Node<'_>, role: StructuralRole) -> QueryCandidate {
        QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role,
            syntax: "python.identifier",
            required: true,
            native_depth: 0,
        }
    }

    #[test]
    fn original_unique_module_assignment_is_native_and_exact_without_runtime_binding() {
        let source =
            include_bytes!("../../tests/fixtures/written_residuals/python/query_client.py");
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let name = parsed
            .root_node()
            .descendant_for_byte_range(61, 65)
            .unwrap();
        assert_eq!(name.kind(), "identifier");
        let declaration = name.parent().unwrap();
        assert_eq!(declaration.kind(), "assignment");
        let mut cache = CaptureCache::new(source, 16_384, true);
        let cancel = Cancellation::new();
        let mut declared = candidate(declaration, StructuralRole::Declaration);
        assert!(
            cache
                .refine(
                    parsed.root_node(),
                    declaration,
                    StructuralRole::Declaration,
                    &mut declared,
                    &cancel
                )
                .unwrap()
        );
        assert_eq!(declared.syntax, "python.module_variable");
        let mut definition = candidate(name, StructuralRole::Definition);
        cache
            .refine(
                parsed.root_node(),
                name,
                StructuralRole::Definition,
                &mut definition,
                &cancel,
            )
            .unwrap();
        assert_eq!(definition.syntax, "python.module_variable_name");
        assert_eq!(definition.start..definition.end, 61..65);
        assert!(!cache.exhausted());
        let mut reference = candidate(name, StructuralRole::Reference);
        assert!(
            !cache
                .refine(
                    parsed.root_node(),
                    name,
                    StructuralRole::Reference,
                    &mut reference,
                    &cancel
                )
                .unwrap()
        );
    }

    #[test]
    fn repeated_augmented_or_nested_writes_never_create_one_new_symbol_per_write() {
        for source in [
            b"value = 1\nvalue = 2\n".as_slice(),
            b"value = 1\nvalue += 1\n",
            b"value = 1\ndef owned():\n    value = 2\n",
            b"value = 1\nfor value in []:\n    pass\n",
            b"import value\nvalue = 1\n",
        ] {
            let parsed = tree(source);
            assert!(!parsed.root_node().has_error());
            let mut cache = CaptureCache::new(source, 16_384, true);
            cache
                .inspect(parsed.root_node(), &Cancellation::new())
                .unwrap();
            let offset = source.windows(5).position(|part| part == b"value").unwrap();
            let name = parsed
                .root_node()
                .descendant_for_byte_range(offset, offset + 5)
                .unwrap();
            let key = cache.key(name, &Cancellation::new()).unwrap().unwrap();
            assert_eq!(cache.bindings.get(&key), Some(&2));
        }
    }

    #[test]
    fn comments_and_strings_are_not_binders_but_annotations_stay_explicitly_unavailable() {
        let source = b"# sock = 7\ntext = 'sock = 9'\nsock = 1\ntyped: int = 1\n";
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let offset = source
            .windows(8)
            .position(|part| part == b"sock = 1")
            .unwrap();
        let name = parsed
            .root_node()
            .descendant_for_byte_range(offset, offset + 4)
            .unwrap();
        let mut cache = CaptureCache::new(source, 16_384, true);
        let mut definition = candidate(name, StructuralRole::Definition);
        cache
            .refine(
                parsed.root_node(),
                name,
                StructuralRole::Definition,
                &mut definition,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(definition.role, StructuralRole::Definition);
        let offset = source.windows(5).position(|part| part == b"typed").unwrap();
        let typed = parsed
            .root_node()
            .descendant_for_byte_range(offset, offset + 5)
            .unwrap();
        let mut unavailable = candidate(typed, StructuralRole::Definition);
        cache
            .refine(
                parsed.root_node(),
                typed,
                StructuralRole::Definition,
                &mut unavailable,
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(unavailable.role, StructuralRole::Signature);
        assert_eq!(unavailable.syntax, "python.module_assignment_unavailable");
    }

    #[test]
    fn incomplete_work_cancel_and_recovery_cannot_promote_partial_assignment_identity() {
        let source = b"value = 1\n";
        let parsed = tree(source);
        let name = parsed.root_node().descendant_for_byte_range(0, 5).unwrap();
        for complete in [false, true] {
            let mut cache = CaptureCache::new(source, if complete { 0 } else { 16_384 }, complete);
            let mut definition = candidate(name, StructuralRole::Definition);
            cache
                .refine(
                    parsed.root_node(),
                    name,
                    StructuralRole::Definition,
                    &mut definition,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(definition.role, StructuralRole::Signature);
            assert_eq!(cache.exhausted(), complete);
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source, 16_384, true);
        assert!(matches!(
            cache.refine(
                parsed.root_node(),
                name,
                StructuralRole::Definition,
                &mut candidate(name, StructuralRole::Definition),
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        let broken = tree(b"value = (\n");
        assert!(broken.root_node().has_error());
        let mut cache = CaptureCache::new(b"value = (\n", 16_384, true);
        cache
            .inspect(broken.root_node(), &Cancellation::new())
            .unwrap();
        assert!(!cache.available);
    }

    #[test]
    fn comprehension_targets_keep_module_identity_without_admitting_module_or_walrus_writes() {
        for expression in [
            "[value for value in []]",
            "{value for value in []}",
            "{value: item for value, item in []}",
            "(value for value in [])",
            "[value for value in [] for item in []]",
            "[(value, other) for (value, other) in []]",
        ] {
            let source =
                format!("value = 1\nvalue += 2\ncopy = {expression}\nresult = value + 1\n");
            let parsed = tree(source.as_bytes());
            assert!(!parsed.root_node().has_error(), "{source}");
            let name = parsed.root_node().descendant_for_byte_range(0, 5).unwrap();
            let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
            let mut output = candidate(name, StructuralRole::Definition);
            cache
                .refine(
                    parsed.root_node(),
                    name,
                    StructuralRole::Definition,
                    &mut output,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(output.syntax, "python.module_variable_name", "{source}");
        }
        for suffix in [
            "for value in []:\n    pass\n",
            "copy = [(value := item) for item in []]\n",
            "copy = [[(value := item) for item in []] for unused in []]\n",
        ] {
            let source = format!("value = 1\nvalue += 2\n{suffix}result = value + 1\n");
            let parsed = tree(source.as_bytes());
            assert!(!parsed.root_node().has_error(), "{source}");
            let name = parsed.root_node().descendant_for_byte_range(0, 5).unwrap();
            let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
            let mut output = candidate(name, StructuralRole::Definition);
            cache
                .refine(
                    parsed.root_node(),
                    name,
                    StructuralRole::Definition,
                    &mut output,
                    &Cancellation::new(),
                )
                .unwrap();
            assert_eq!(
                output.syntax, "python.module_assignment_unavailable",
                "{source}"
            );
        }
    }
}

#[cfg(test)]
mod use_fence_tests {
    use super::*;

    #[test]
    fn earlier_reads_and_calls_stay_unknown_while_old_formal_proof_is_preserved() {
        let source = b"value()\nvalue = 1\ndef owned(value):\n    return value\n";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        let parsed = parser.parse(source, None).unwrap();
        assert!(!parsed.root_node().has_error());
        let cancel = Cancellation::new();
        let mut cache = CaptureCache::new(source, 16_384, true);
        let mut formals = super::super::python_parameters::CaptureCache::new(source, 16_384, true);
        for (start, role, expected) in [
            (
                0,
                StructuralRole::Reference,
                "python.module_binding_unavailable",
            ),
            (0, StructuralRole::Call, "python.module_call_unavailable"),
        ] {
            let node = parsed
                .root_node()
                .descendant_for_byte_range(start, start + 5)
                .unwrap();
            let mut candidate = QueryCandidate {
                start,
                end: start + 5,
                role,
                syntax: "python.identifier",
                required: false,
                native_depth: 0,
            };
            cache
                .refine(parsed.root_node(), node, role, &mut candidate, &cancel)
                .unwrap();
            assert_eq!(candidate.syntax, expected);
        }
        let start = source
            .windows(5)
            .rposition(|part| part == b"value")
            .unwrap();
        let node = parsed
            .root_node()
            .descendant_for_byte_range(start, start + 5)
            .unwrap();
        let mut candidate = QueryCandidate {
            start,
            end: start + 5,
            role: StructuralRole::Reference,
            syntax: "python.identifier",
            required: false,
            native_depth: 0,
        };
        cache
            .refine(
                parsed.root_node(),
                node,
                StructuralRole::Reference,
                &mut candidate,
                &cancel,
            )
            .unwrap();
        assert_eq!(candidate.syntax, "python.module_binding_unavailable");
        formals
            .refine(node, StructuralRole::Reference, &mut candidate, &cancel)
            .unwrap();
        assert_eq!(candidate.syntax, "python.parameter_body");
    }
}

// Native controls retain the raw census while distinguishing same-module updates.
// They qualify initial identity only; the mandatory module-use consumer is separate.

#[cfg(test)]
mod module_update_tests {
    use super::*;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn first_name<'tree>(
        tree: &'tree tree_sitter::Tree,
        source: &[u8],
        name: &[u8],
    ) -> Node<'tree> {
        let start = source
            .windows(name.len())
            .position(|part| part == name)
            .unwrap();
        let node = tree
            .root_node()
            .descendant_for_byte_range(start, start + name.len())
            .unwrap();
        assert_eq!(node.kind(), "identifier");
        node
    }

    fn refine(
        cache: &mut CaptureCache<'_>,
        root: Node<'_>,
        node: Node<'_>,
        role: StructuralRole,
    ) -> QueryCandidate {
        let mut candidate = QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role,
            syntax: "python.identifier",
            required: false,
            native_depth: 0,
        };
        cache
            .refine(root, node, role, &mut candidate, &Cancellation::new())
            .unwrap();
        candidate
    }

    #[test]
    fn original_total_retains_all_writes_but_only_initial_definition() {
        let source = include_bytes!("../../tests/fixtures/python_module_updates/summarize_eval.py");
        let tree = tree(source);
        assert!(!tree.root_node().has_error());
        let name = tree
            .root_node()
            .descendant_for_byte_range(535, 540)
            .unwrap();
        assert_eq!(name.kind(), "identifier");
        let mut cache = CaptureCache::new(source, 16_384, true);
        let output = refine(
            &mut cache,
            tree.root_node(),
            name,
            StructuralRole::Definition,
        );
        assert_eq!(output.role, StructuralRole::Definition);
        assert_eq!(output.syntax, "python.module_variable_name");
        assert_eq!(output.start..output.end, 535..540);
        let key = cache.key(name, &Cancellation::new()).unwrap().unwrap();
        assert_eq!(cache.bindings.get(&key), Some(&2));
        assert_eq!(cache.ordinary_bindings.get(&key), Some(&1));
        assert!(cache.first_module_update.contains_key(&key));
    }

    #[test]
    fn renamed_direct_loop_and_conditional_updates_keep_one_initial_slot() {
        for source in [
            b"value = 1\nvalue += 2\n".as_slice(),
            b"amount = 7\nfor entry in []:\n    amount += entry\namount *= 3\n",
            b"tally = 9\nif ready:\n    tally -= 1\nelse:\n    tally += 4\n",
            b"score = 2\nwhile ready:\n    score /= 2\n",
        ] {
            let tree = tree(source);
            assert!(!tree.root_node().has_error());
            let end = source.iter().position(|byte| *byte == b' ').unwrap();
            let name = tree.root_node().descendant_for_byte_range(0, end).unwrap();
            let mut cache = CaptureCache::new(source, 16_384, true);
            let output = refine(
                &mut cache,
                tree.root_node(),
                name,
                StructuralRole::Definition,
            );
            assert_eq!(output.syntax, "python.module_variable_name", "{source:?}");
            let key = cache.key(name, &Cancellation::new()).unwrap().unwrap();
            assert_eq!(cache.bindings.get(&key), Some(&2));
            assert_eq!(cache.ordinary_bindings.get(&key), Some(&1));
        }
    }

    #[test]
    fn duplicate_ordinary_rebinding_and_scope_barriers_still_withhold_initial_slot() {
        for source in [
            b"value = 1\nvalue += 2\nvalue = 3\n".as_slice(),
            b"value += 2\nvalue = 1\n",
            b"value = 1\nfor value in []:\n    value += 2\n",
            b"value = 1\ndef owned():\n    value += 2\n",
            b"value = 1\nclass Owned:\n    value += 2\n",
            b"value = 1\nimport other as value\nvalue += 2\n",
            b"value = 1\nif (value := 2):\n    value += 3\n",
            b"value = 1\ndel value\nvalue += 2\n",
            b"value = 1\ndef owned():\n    global value\n    value += 2\n",
            b"value = 1\nglobals()['value'] = 2\nvalue += 3\n",
        ] {
            let tree = tree(source);
            assert!(!tree.root_node().has_error());
            let start = source
                .windows(7)
                .position(|bytes| bytes == b"value =")
                .unwrap();
            let name = tree
                .root_node()
                .descendant_for_byte_range(start, start + 5)
                .unwrap();
            let mut cache = CaptureCache::new(source, 16_384, true);
            let output = refine(
                &mut cache,
                tree.root_node(),
                name,
                StructuralRole::Definition,
            );
            assert_ne!(output.role, StructuralRole::Definition, "{source:?}");
        }
    }

    #[test]
    fn augmented_operand_reads_and_calls_never_escape_unavailable_fence() {
        let source =
            b"value = 1\nvalue += 2\ncopy = value\nvalue()\nshadow = lambda value: value\n";
        let tree = tree(source);
        assert!(!tree.root_node().has_error());
        let mut cache = CaptureCache::new(source, 16_384, true);
        for start in source
            .windows(5)
            .enumerate()
            .filter_map(|(start, bytes)| (bytes == b"value" && start != 0).then_some(start))
        {
            let name = tree
                .root_node()
                .descendant_for_byte_range(start, start + 5)
                .unwrap();
            let output = refine(
                &mut cache,
                tree.root_node(),
                name,
                StructuralRole::Reference,
            );
            let expected = if source.get(start.saturating_sub(7)..start) == Some(b"copy = ") {
                "python.module_read"
            } else {
                "python.module_binding_unavailable"
            };
            assert_eq!(output.syntax, expected);
        }
        let start = source
            .windows(7)
            .position(|bytes| bytes == b"value()")
            .unwrap();
        let call = tree
            .root_node()
            .descendant_for_byte_range(start, start + 5)
            .unwrap();
        let output = refine(&mut cache, tree.root_node(), call, StructuralRole::Call);
        assert_eq!(output.syntax, "python.module_call_unavailable");
    }

    #[test]
    fn augmented_source_metadata_is_required_without_a_new_definition() {
        let source = b"value = 1\nvalue += 2\n";
        let tree = tree(source);
        let name = tree.root_node().descendant_for_byte_range(10, 15).unwrap();
        let update = name.parent().unwrap();
        assert_eq!(update.kind(), "augmented_assignment");
        let mut cache = CaptureCache::new(source, 16_384, true);
        let output = refine(
            &mut cache,
            tree.root_node(),
            update,
            StructuralRole::Signature,
        );
        assert_eq!(
            output.syntax,
            "python.augmented_assignment_binding_unavailable"
        );
        assert_eq!(output.role, StructuralRole::Signature);
        assert!(output.required);
        assert_eq!(output.start..output.end, update.byte_range());
    }

    #[test]
    fn updated_binding_still_refuses_partial_recovery_zero_work_and_cancellation() {
        let source = b"value = 1\nvalue += 2\n";
        let tree = tree(source);
        let name = first_name(&tree, source, b"value");
        for (maximum, complete) in [(0, true), (16_384, false)] {
            let mut cache = CaptureCache::new(source, maximum, complete);
            let output = refine(
                &mut cache,
                tree.root_node(),
                name,
                StructuralRole::Definition,
            );
            assert_eq!(output.role, StructuralRole::Signature);
        }
        let cancel = Cancellation::new();
        cancel.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source, 16_384, true);
        let mut candidate = QueryCandidate {
            start: 0,
            end: 5,
            role: StructuralRole::Definition,
            syntax: "python.identifier",
            required: false,
            native_depth: 0,
        };
        assert!(matches!(
            cache.refine(
                tree.root_node(),
                name,
                StructuralRole::Definition,
                &mut candidate,
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
        let broken = b"value = 1\nvalue += (\n";
        let tree = self::tree(broken);
        assert!(tree.root_node().has_error());
        let name = first_name(&tree, broken, b"value");
        let mut cache = CaptureCache::new(broken, 16_384, true);
        assert_eq!(
            refine(
                &mut cache,
                tree.root_node(),
                name,
                StructuralRole::Definition
            )
            .role,
            StructuralRole::Signature
        );
    }
}

#[cfg(test)]
mod module_read_tests {
    use super::*;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_python::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn candidate(node: Node<'_>, role: StructuralRole) -> QueryCandidate {
        QueryCandidate {
            start: node.start_byte(),
            end: node.end_byte(),
            role,
            syntax: "python.identifier",
            required: false,
            native_depth: 0,
        }
    }

    #[test]
    fn full_original_reference_has_exact_required_native_target_end_window() {
        let source = include_bytes!("../../tests/fixtures/python_module_updates/summarize_eval.py");
        let tree = tree(source);
        assert!(!tree.root_node().has_error());
        let root = tree.root_node();
        let node = root.descendant_for_byte_range(1020, 1025).unwrap();
        assert_eq!(node.kind(), "identifier");
        let mut cache = CaptureCache::new(source, 16_384, true);
        for role in [StructuralRole::Reference, StructuralRole::Scope] {
            let mut output = candidate(node, role);
            assert!(
                cache
                    .refine(root, node, role, &mut output, &Cancellation::new())
                    .unwrap()
            );
            assert!(output.required);
            if role == StructuralRole::Reference {
                assert_eq!(output.syntax, "python.module_read");
                assert_eq!(output.start..output.end, 1020..1025);
            } else {
                assert_eq!(output.role, StructuralRole::Signature);
                assert_eq!(output.syntax, "python.module_target");
                assert_eq!(output.start..output.end, 540..1025);
            }
        }
        let mut context = candidate(root, StructuralRole::Signature);
        assert!(
            cache
                .refine(
                    root,
                    root,
                    StructuralRole::Signature,
                    &mut context,
                    &Cancellation::new()
                )
                .unwrap()
        );
        assert_eq!(context.syntax, "python.module_binding_context");
        assert_eq!(context.start..context.end, 0..source.len());
        assert!(context.required);
    }

    #[test]
    fn only_current_flat_module_assignment_operands_gain_read_certificates() {
        for (source, allowed) in [
            ("value = 1\nvalue += 2\ncopy = value / 2", true),
            (
                "amount = 3\nfor entry in []:\n    amount += entry\ncopy = (amount + 1) / 2\n",
                true,
            ),
            ("value = 1\nvalue += 2\ncopy = lambda value: value\n", false),
            ("value = 1\nvalue += 2\ncopy = value()\n", false),
            ("value = 1\nvalue += 2\ncopy = holder.value\n", false),
            ("value = 1\nvalue += 2\ncopy = value[0]\n", false),
            (
                "value = 1\nvalue += 2\ncopy = [value for value in []]\n",
                false,
            ),
            (
                "value = 1\nvalue += 2\ndef owned(value=value):\n    return value\n",
                false,
            ),
            (
                "value = 1\nvalue += 2\ncopy = value if ready else 0\n",
                false,
            ),
            ("value = 1\nvalue = 2\ncopy = value\n", false),
        ] {
            let source = source.as_bytes();
            let tree = tree(source);
            assert!(!tree.root_node().has_error());
            let name = if source.starts_with(b"amount") {
                b"amount".as_slice()
            } else {
                b"value".as_slice()
            };
            let start = source
                .windows(name.len())
                .rposition(|part| part == name)
                .unwrap();
            let node = tree
                .root_node()
                .descendant_for_byte_range(start, start + name.len())
                .unwrap();
            let mut cache = CaptureCache::new(source, 16_384, true);
            let mut output = candidate(node, StructuralRole::Scope);
            assert_eq!(
                cache
                    .refine(
                        tree.root_node(),
                        node,
                        StructuralRole::Scope,
                        &mut output,
                        &Cancellation::new()
                    )
                    .unwrap(),
                allowed,
                "{source:?}"
            );
        }
    }
}
