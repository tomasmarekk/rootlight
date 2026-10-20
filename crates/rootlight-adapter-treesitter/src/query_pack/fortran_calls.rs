//! Certify ordinary same-module CALL binding only after a complete native census.
//! Unsupported binding scopes withhold proof; source spelling never bypasses a
//! native ownership, shadow, recovery, preprocessing or capacity barrier.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, ResourceKind, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, fortran};

fn folded(name: &str) -> Result<String, AdapterError> {
    let mut result = String::new();
    result
        .try_reserve_exact(name.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    result.push_str(name);
    result.make_ascii_lowercase();
    Ok(result)
}

fn name<'s>(node: Node<'_>, source: &'s [u8]) -> Option<&'s str> {
    fortran::ordinary_name(node, source)
        .then(|| std::str::from_utf8(source.get(node.byte_range())?).ok())
        .flatten()
}

fn procedure_module(node: Node<'_>) -> Option<Node<'_>> {
    node.parent()
        .filter(|parent| parent.kind() == "internal_procedures")?
        .parent()
        .filter(|parent| parent.kind() == "module")
}

fn nearest_procedure<'tree>(
    mut node: Node<'tree>,
    cancel: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    while let Some(parent) = node.parent() {
        cancel.check()?;
        if matches!(parent.kind(), "function" | "subroutine") {
            return Ok(Some(parent));
        }
        if matches!(
            parent.kind(),
            "module" | "submodule" | "program" | "interface"
        ) {
            return Ok(None);
        }
        node = parent;
    }
    Ok(None)
}

#[derive(Default)]
struct Module {
    barrier: bool,
    implicit_none: bool,
    procedures: HashMap<String, Option<usize>>,
}

struct Procedure {
    module: usize,
    subroutine: bool,
    barrier: bool,
    shadows: HashSet<String>,
}

struct Call {
    start: usize,
    end: usize,
    procedure: usize,
    name: String,
}

pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    maximum: usize,
    visited: usize,
    initialized: bool,
    complete: bool,
    exhausted: bool,
    module_names: HashSet<String>,
    modules: HashMap<usize, Module>,
    procedures: HashMap<usize, Procedure>,
    calls: Vec<Call>,
    proven: HashSet<(usize, usize)>,
}

impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], maximum: usize) -> Self {
        Self {
            source,
            maximum: maximum.min(HARD_MAX_QUERY_CAPTURES),
            visited: 0,
            initialized: false,
            complete: false,
            exhausted: false,
            module_names: HashSet::new(),
            modules: HashMap::new(),
            procedures: HashMap::new(),
            calls: Vec::new(),
            proven: HashSet::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    pub(super) fn exhaustion_error(&self) -> AdapterError {
        AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::SyntaxNodes,
            observed: self.visited.saturating_add(1),
            limit: self.maximum,
        })
    }

    fn charge(&mut self, cancel: &Cancellation) -> Result<bool, AdapterError> {
        cancel.check()?;
        if self.visited >= self.maximum {
            self.exhausted = true;
            return Ok(false);
        }
        self.visited += 1;
        Ok(true)
    }

    fn shadow(&mut self, owner: usize, node: Node<'_>) -> Result<(), AdapterError> {
        let Some(written) = name(node, self.source) else {
            self.procedures
                .get_mut(&owner)
                .expect("censused procedure")
                .barrier = true;
            return Ok(());
        };
        let value = folded(written)?;
        let entry = self.procedures.get_mut(&owner).expect("censused procedure");
        entry
            .shadows
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        entry.shadows.insert(value);
        Ok(())
    }

    fn visit(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if node.kind().starts_with("preproc_") {
            return Ok(false);
        }
        if node.kind() == "comment" {
            let text = std::str::from_utf8(self.source.get(node.byte_range()).unwrap_or_default())
                .unwrap_or_default()
                .trim_start();
            if ["!$", "!dir$", "!dec$", "!gcc$", "!ms$", "c$", "*$"]
                .iter()
                .any(|prefix| {
                    text.get(..prefix.len())
                        .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                })
            {
                return Ok(false);
            }
        }
        // The anonymous MODULE keyword shares the named construct's kind string.
        if node.is_named() && node.kind() == "module" {
            if !node
                .parent()
                .is_some_and(|parent| parent.kind() == "translation_unit")
            {
                return Ok(false);
            }
            let Some(written) =
                fortran::definition(node, cancel)?.and_then(|leaf| name(leaf, self.source))
            else {
                return Ok(false);
            };
            self.module_names
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            if !self.module_names.insert(folded(written)?) {
                return Ok(false);
            }
            self.modules
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.modules.insert(node.start_byte(), Module::default());
        }
        if let Some(parent) = node.parent().filter(|parent| parent.kind() == "module") {
            let entry = self
                .modules
                .get_mut(&parent.start_byte())
                .expect("censused module");
            if !matches!(
                node.kind(),
                "module_statement"
                    | "end_module_statement"
                    | "internal_procedures"
                    | "implicit_statement"
                    | "private_statement"
                    | "public_statement"
                    | "comment"
            ) && node.is_named()
            {
                entry.barrier = true;
            }
            if node.kind() == "implicit_statement" {
                let mut cursor = node.walk();
                let mut children = node.named_children(&mut cursor).peekable();
                let valid_none =
                    children.peek().is_some() && children.all(|child| child.kind() == "none");
                entry.implicit_none |= valid_none;
                if !valid_none {
                    entry.barrier = true;
                }
            }
        }
        if node.is_named()
            && let Some(parent) = node
                .parent()
                .filter(|parent| parent.kind() == "internal_procedures")
            && let Some(module) = parent.parent().filter(|parent| parent.kind() == "module")
            && !matches!(
                node.kind(),
                "contains_statement" | "function" | "subroutine" | "comment"
            )
        {
            self.modules
                .get_mut(&module.start_byte())
                .expect("censused module")
                .barrier = true;
        }
        if matches!(node.kind(), "function" | "subroutine")
            && let Some(module) = procedure_module(node)
        {
            let Some(written) =
                fortran::definition(node, cancel)?.and_then(|leaf| name(leaf, self.source))
            else {
                self.modules
                    .get_mut(&module.start_byte())
                    .expect("censused module")
                    .barrier = true;
                return Ok(true);
            };
            let key = folded(written)?;
            let entry = self
                .modules
                .get_mut(&module.start_byte())
                .expect("censused module");
            entry
                .procedures
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            entry
                .procedures
                .entry(key)
                .and_modify(|value| *value = None)
                .or_insert(Some(node.start_byte()));
            self.procedures
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.procedures.insert(
                node.start_byte(),
                Procedure {
                    module: module.start_byte(),
                    subroutine: node.kind() == "subroutine",
                    barrier: false,
                    shadows: HashSet::new(),
                },
            );
            // Self-calls are outside this first host-association slice.
            self.shadow(
                node.start_byte(),
                fortran::definition(node, cancel)?.expect("validated name"),
            )?;
        }
        if matches!(
            node.kind(),
            "end_module_statement" | "end_function_statement" | "end_subroutine_statement"
        ) && let Some(owner) = node.parent()
            && let Some(written) =
                fortran::definition(owner, cancel)?.and_then(|leaf| name(leaf, self.source))
        {
            let mut cursor = node.walk();
            for leaf in node
                .named_children(&mut cursor)
                .filter(|leaf| leaf.kind() == "name")
            {
                if !self.charge(cancel)? {
                    return Ok(false);
                }
                if name(leaf, self.source)
                    .is_none_or(|ending| !ending.eq_ignore_ascii_case(written))
                {
                    return Ok(false);
                }
            }
        }
        let Some(owner) = nearest_procedure(node, cancel)? else {
            return Ok(true);
        };
        let owner = owner.start_byte();
        if !self.procedures.contains_key(&owner) {
            return Ok(true);
        }
        let direct = node.parent().is_some_and(|parent| {
            parent.start_byte() == owner && matches!(parent.kind(), "function" | "subroutine")
        });
        let unsupported_direct = direct
            && node.is_named()
            && !matches!(
                node.kind(),
                "function_statement"
                    | "end_function_statement"
                    | "subroutine_statement"
                    | "end_subroutine_statement"
                    | "variable_declaration"
                    | "implicit_statement"
                    | "comment"
                    | "assignment_statement"
                    | "subroutine_call"
                    | "do_loop"
                    | "if_statement"
                    | "keyword_statement"
                    | "stop_statement"
                    | "print_statement"
                    | "read_statement"
                    | "write_statement"
                    | "format_statement"
                    | "statement_label"
            );
        if unsupported_direct
            || matches!(
                node.kind(),
                "internal_procedures"
                    | "interface"
                    | "use_statement"
                    | "import_statement"
                    | "include_statement"
                    | "procedure_statement"
                    | "variable_modification"
                    | "procedure"
                    | "language_binding"
                    | "procedure_attributes"
                    | "block_construct"
                    | "associate_statement"
                    | "select_rank_statement"
                    | "select_type_statement"
                    | "forall_statement"
                    | "concurrent_statement"
                    | "entry_statement"
                    | "derived_type_definition"
                    | "block_label_start_expression"
                    | "block_label"
            )
        {
            self.procedures
                .get_mut(&owner)
                .expect("censused procedure")
                .barrier = true;
        }
        if node.kind() == "implicit_statement" {
            let mut cursor = node.walk();
            if !node
                .named_children(&mut cursor)
                .all(|child| child.kind() == "none")
            {
                self.procedures
                    .get_mut(&owner)
                    .expect("censused procedure")
                    .barrier = true;
            }
        }
        if node.kind() == "procedure_qualifier" {
            let text = std::str::from_utf8(self.source.get(node.byte_range()).unwrap_or_default())
                .unwrap_or_default();
            if !["pure", "impure", "elemental", "recursive", "non_recursive"]
                .iter()
                .any(|allowed| text.eq_ignore_ascii_case(allowed))
            {
                self.procedures
                    .get_mut(&owner)
                    .expect("censused procedure")
                    .barrier = true;
            }
        }
        if matches!(node.kind(), "parameters" | "function_result") {
            let mut cursor = node.walk();
            for child in node.named_children(&mut cursor) {
                if !self.charge(cancel)? {
                    return Ok(false);
                }
                self.shadow(owner, child)?;
            }
        }
        if node.kind() == "variable_declaration" {
            if !node
                .child_by_field_name("type")
                .is_some_and(|kind| kind.kind() == "intrinsic_type")
            {
                self.procedures
                    .get_mut(&owner)
                    .expect("censused procedure")
                    .barrier = true;
            }
            let mut cursor = node.walk();
            for declarator in node.children_by_field_name("declarator", &mut cursor) {
                if !self.charge(cancel)? {
                    return Ok(false);
                }
                if let Some(leaf) = fortran::definition(declarator, cancel)? {
                    self.shadow(owner, leaf)?;
                } else {
                    self.procedures
                        .get_mut(&owner)
                        .expect("censused procedure")
                        .barrier = true;
                }
            }
            let mut cursor = node.walk();
            for attribute in node.children_by_field_name("attribute", &mut cursor) {
                if !self.charge(cancel)? {
                    return Ok(false);
                }
                let text = std::str::from_utf8(
                    self.source.get(attribute.byte_range()).unwrap_or_default(),
                )
                .unwrap_or_default();
                if ["external", "intrinsic", "pointer"]
                    .iter()
                    .any(|value| text.eq_ignore_ascii_case(value))
                {
                    self.procedures
                        .get_mut(&owner)
                        .expect("censused procedure")
                        .barrier = true;
                }
            }
        }
        if node.kind() == "loop_control_expression"
            && let Some(leaf) = node
                .named_child(0)
                .filter(|leaf| leaf.kind() == "identifier")
        {
            self.shadow(owner, leaf)?;
        }
        if node.kind() == "assignment_statement"
            && let Some(left) = node.child_by_field_name("left")
        {
            if left.kind() == "identifier" {
                self.shadow(owner, left)?;
            } else if left.kind() == "call_expression"
                && let Some(leaf) = left.child_by_field_name("function")
            {
                self.shadow(owner, leaf)?;
            }
        }
        if node.kind() == "subroutine_call"
            && !node
                .named_children(&mut node.walk())
                .any(|child| child.kind() == "cuda_kernel_argument_list")
            && let Some(written) =
                fortran::terminal_call_name(node, cancel)?.and_then(|leaf| name(leaf, self.source))
        {
            let key = folded(written)?;
            self.calls
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.calls.push(Call {
                start: node.start_byte(),
                end: node.end_byte(),
                procedure: owner,
                name: key,
            });
        }
        Ok(true)
    }

    fn initialize(
        &mut self,
        mut root: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        self.initialized = true;
        while let Some(parent) = root.parent() {
            cancel.check()?;
            root = parent;
        }
        if root.kind() != "translation_unit"
            || root.has_error()
            || root.is_missing()
            // The native parser excludes an initial UTF-8 BOM from its root.
            || !(root.start_byte() == 0
                || (root.start_byte() == 3 && self.source.starts_with(b"\xef\xbb\xbf")))
            || root.end_byte() != self.source.len()
            || fortran::template_source(self.source, cancel)?
        {
            return Ok(());
        }
        let mut cursor = root.walk();
        loop {
            if !self.charge(cancel)? || !self.visit(cursor.node(), cancel)? {
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
                    for call in &self.calls {
                        cancel.check()?;
                        let caller = &self.procedures[&call.procedure];
                        let module = &self.modules[&caller.module];
                        if module.barrier
                            || !module.implicit_none
                            || caller.barrier
                            || caller.shadows.contains(&call.name)
                        {
                            continue;
                        }
                        let Some(target) = module
                            .procedures
                            .get(&call.name)
                            .copied()
                            .flatten()
                            .and_then(|start| self.procedures.get(&start))
                        else {
                            continue;
                        };
                        if target.module != caller.module || !target.subroutine || target.barrier {
                            continue;
                        }
                        self.proven
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.proven.insert((call.start, call.end));
                    }
                    self.complete = true;
                    return Ok(());
                }
            }
        }
    }

    pub(super) fn direct_call(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        cancel.check()?;
        if !self.initialized {
            self.initialize(node, cancel)?;
        }
        Ok(self.complete
            && !self.exhausted
            && self.proven.contains(&(node.start_byte(), node.end_byte())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn parsed(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_fortran::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(!tree.root_node().has_error());
        tree
    }

    fn calls(tree: &tree_sitter::Tree) -> Vec<Node<'_>> {
        let mut result = Vec::new();
        let mut cursor = tree.root_node().walk();
        loop {
            if cursor.node().kind() == "subroutine_call" {
                result.push(cursor.node());
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    return result;
                }
            }
        }
    }

    #[test]
    fn native_original_census_proves_both_calls_only_after_complete_scan() {
        let source = include_bytes!("../../tests/fixtures/fortran-direct-call/stdlib_array.f90");
        let tree = parsed(source);
        let calls = calls(&tree);
        assert_eq!(calls.len(), 2);
        let mut proof = CaptureCache::new(source, 4096);
        for call in &calls {
            assert!(proof.direct_call(*call, &Cancellation::new()).unwrap());
        }
        assert!(proof.complete);
        assert!(!proof.exhausted());
        let work = proof.visited;
        let mut short = CaptureCache::new(source, work - 1);
        assert!(!short.direct_call(calls[0], &Cancellation::new()).unwrap());
        assert!(short.exhausted());
        assert!(
            matches!(short.exhaustion_error(), AdapterError::Sink(SinkError::StreamLimit {
            resource: ResourceKind::SyntaxNodes, observed, limit
        }) if observed == work && limit == work - 1)
        );
        assert!(!short.complete);
        assert!(short.proven.is_empty());
        assert!(!short.direct_call(calls[1], &Cancellation::new()).unwrap());
        let mut exact = CaptureCache::new(source, work);
        assert!(exact.direct_call(calls[0], &Cancellation::new()).unwrap());
        assert!(!exact.exhausted());
        assert_eq!(exact.visited, work);
    }

    #[test]
    fn native_bom_prefix_preserves_complete_original_call_proof() {
        let mut source = b"\xef\xbb\xbf".to_vec();
        source.extend_from_slice(include_bytes!(
            "../../tests/fixtures/fortran-direct-call/stdlib_array.f90"
        ));
        let tree = parsed(&source);
        assert_eq!(tree.root_node().start_byte(), 3);
        assert_eq!(tree.root_node().end_byte(), source.len());
        let calls = calls(&tree);
        assert_eq!(calls.len(), 2);
        let mut proof = CaptureCache::new(&source, 4096);
        for call in calls {
            assert!(proof.direct_call(call, &Cancellation::new()).unwrap());
        }
        assert!(proof.complete);
        assert!(!proof.exhausted());
    }

    #[test]
    fn cancellation_and_finite_or_unbounded_caller_ceilings_do_not_publish_partial_proof() {
        let source = b"module Sample\nimplicit none\ncontains\nsubroutine Run()\ncall Work()\nend subroutine Run\nsubroutine Work()\nend subroutine Work\nend module Sample\n";
        let tree = parsed(source);
        let call = calls(&tree)[0];
        for maximum in [0, 1, 8] {
            let mut proof = CaptureCache::new(source, maximum);
            assert!(!proof.direct_call(call, &Cancellation::new()).unwrap());
            assert!(proof.exhausted());
            assert!(proof.visited <= maximum);
            assert!(proof.proven.is_empty());
        }
        let mut proof = CaptureCache::new(source, usize::MAX);
        assert_eq!(proof.maximum, HARD_MAX_QUERY_CAPTURES);
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(proof.direct_call(call, &cancel).is_err());
        assert!(!proof.initialized);
        assert!(proof.proven.is_empty());
        assert!(proof.direct_call(call, &Cancellation::new()).unwrap());
    }
}
