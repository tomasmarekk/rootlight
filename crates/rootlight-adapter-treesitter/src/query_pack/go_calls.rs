//! Certify written bare calls to unique ordinary functions in the same Go file.
//! Import namespaces cannot supply callable bare operands; dot imports and every
//! visible binder/write of the selected name fence this bounded static dependency.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole};

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    initialized: bool,
    allowed: bool,
    remaining: usize,
    exhausted: bool,
    functions: HashMap<&'source str, Option<usize>>,
    globals: HashSet<&'source str>,
    locals: HashSet<(usize, &'source str)>,
    refused_owners: HashSet<usize>,
}

impl<'source> CaptureCache<'source> {
    pub(super) fn new(source: &'source [u8], remaining: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            initialized: false,
            allowed: false,
            remaining,
            exhausted: false,
            functions: HashMap::new(),
            globals: HashSet::new(),
            locals: HashSet::new(),
            refused_owners: HashSet::new(),
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

    fn name(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        if !self.visit(cancel)? {
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
        let Some((&first, rest)) = raw.split_first() else {
            return Ok(None);
        };
        Ok((raw != b"_"
            && (first.is_ascii_alphabetic() || first == b'_')
            && rest.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'_'))
        .then(|| std::str::from_utf8(raw).ok())
        .flatten())
    }

    fn owner<'tree>(
        &mut self,
        mut node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        while let Some(parent) = node.parent() {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if matches!(
                parent.kind(),
                "function_declaration" | "method_declaration" | "func_literal"
            ) {
                return Ok(Some(parent));
            }
            node = parent;
        }
        Ok(None)
    }

    fn refuse_owner(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(owner) = self.owner(node, cancel)? else {
            return Ok(false);
        };
        self.refused_owners
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.refused_owners.insert(owner.id());
        Ok(true)
    }

    fn barrier(
        &mut self,
        binder: Node<'_>,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        if self.source.get(node.byte_range()) == Some(b"_") {
            return Ok(true);
        }
        let Some(name) = self.name(node, cancel)? else {
            return self.refuse_owner(binder, cancel);
        };
        if let Some(owner) = self.owner(binder, cancel)? {
            self.locals
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.locals.insert((owner.id(), name));
        } else {
            self.globals
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            self.globals.insert(name);
        }
        Ok(true)
    }

    fn initialize(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        self.initialized = true;
        let mut root = node;
        while let Some(parent) = root.parent() {
            if !self.visit(cancel)? {
                return Ok(());
            }
            root = parent;
        }
        if !self.complete
            || root.kind() != "source_file"
            || root.has_error()
            || root.end_byte() != self.source.len()
            || !matches!(
                self.source.get(..root.start_byte()),
                Some(b"" | b"\xef\xbb\xbf")
            )
        {
            return Ok(());
        }
        let mut packages = 0usize;
        let mut cursor = root.walk();
        loop {
            if !self.visit(cancel)? {
                return Ok(());
            }
            let current = cursor.node();
            if current.is_missing() || current.is_error() {
                return Ok(());
            }
            match current.kind() {
                "package_clause" => {
                    if current.parent() != Some(root) {
                        return Ok(());
                    }
                    packages = packages.saturating_add(1);
                }
                "function_declaration" => {
                    let Some(name_node) = current.child_by_field_name("name") else {
                        return Ok(());
                    };
                    let Some(name) = self.name(name_node, cancel)? else {
                        return Ok(());
                    };
                    self.functions
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    self.functions
                        .entry(name)
                        .and_modify(|entry| *entry = None)
                        .or_insert(Some(current.id()));
                    if name == "init"
                        || current.parent() != Some(root)
                        || current.child_by_field_name("type_parameters").is_some()
                        || current.child_by_field_name("body").is_none()
                        || current.child_by_field_name("parameters").is_none()
                    {
                        self.globals
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        self.globals.insert(name);
                    }
                }
                "import_spec" => {
                    // Omitted import names denote namespaces, never callable
                    // operands. Their package-clause spelling is not inferred.
                    if let Some(alias) = current.child_by_field_name("name") {
                        if alias.kind() == "dot" {
                            return Ok(());
                        }
                        if !self.barrier(current, alias, cancel)? {
                            return Ok(());
                        }
                    }
                }
                "parameter_declaration"
                | "variadic_parameter_declaration"
                | "type_parameter_declaration"
                | "var_spec"
                | "const_spec"
                | "type_spec"
                | "type_alias" => {
                    let mut fields = current.walk();
                    for name in current.children_by_field_name("name", &mut fields) {
                        if !self.visit(cancel)? {
                            return Ok(());
                        }
                        if name.kind() == "," {
                            continue;
                        }
                        if !self.barrier(current, name, cancel)? {
                            return Ok(());
                        }
                    }
                }
                "short_var_declaration"
                | "assignment_statement"
                | "range_clause"
                | "receive_statement"
                | "type_switch_statement" => {
                    let field = if current.kind() == "type_switch_statement" {
                        "alias"
                    } else {
                        "left"
                    };
                    if let Some(left) = current.child_by_field_name(field) {
                        let mut fields = left.walk();
                        for name in left.named_children(&mut fields) {
                            if !self.visit(cancel)? {
                                return Ok(());
                            }
                            if name.kind() == "identifier" {
                                if !self.barrier(current, name, cancel)? {
                                    return Ok(());
                                }
                            } else if !self.refuse_owner(current, cancel)? {
                                return Ok(());
                            }
                        }
                    }
                }
                "func_literal" if !self.refuse_owner(current, cancel)? => {
                    return Ok(());
                }
                _ => {}
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    self.allowed = packages == 1 && !self.exhausted;
                    return Ok(());
                }
            }
        }
    }

    fn certified(&mut self, callee: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        if !self.initialized {
            self.initialize(callee, cancel)?;
        }
        if !self.allowed || self.exhausted {
            return Ok(false);
        }
        let Some(name) = self.name(callee, cancel)? else {
            return Ok(false);
        };
        let Some(owner) = self.owner(callee, cancel)? else {
            return Ok(false);
        };
        let Some(owner_name_node) = owner.child_by_field_name("name") else {
            return Ok(false);
        };
        let Some(owner_name) = self.name(owner_name_node, cancel)? else {
            return Ok(false);
        };
        let Some(body) = owner.child_by_field_name("body") else {
            return Ok(false);
        };
        Ok(!self.exhausted
            && owner.kind() == "function_declaration"
            && owner
                .parent()
                .is_some_and(|parent| parent.kind() == "source_file")
            && owner.child_by_field_name("type_parameters").is_none()
            && callee.start_byte() >= body.start_byte()
            && callee.end_byte() <= body.end_byte()
            && self.functions.get(owner_name) == Some(&Some(owner.id()))
            && self.functions.get(name).is_some_and(Option::is_some)
            && !self.globals.contains(name)
            && !self.globals.contains(owner_name)
            && !self.locals.contains(&(owner.id(), name))
            && !self.refused_owners.contains(&owner.id()))
    }

    /// Refine only bare callee carriers; all other Go captures keep their owner.
    /// Typed cancellation/allocation errors propagate and the caller handles exhaustion.
    pub(super) fn refine(
        &mut self,
        node: Node<'_>,
        role: StructuralRole,
        candidate: &mut QueryCandidate,
        cancel: &Cancellation,
    ) -> Result<Option<bool>, AdapterError> {
        cancel.check()?;
        if role == StructuralRole::Call
            && node.kind() == "call_expression"
            && node
                .child_by_field_name("function")
                .is_some_and(|callee| callee.kind() == "identifier")
        {
            candidate.syntax = "go.unproven_direct_function";
            return Ok(Some(true));
        }
        if node.kind() != "identifier"
            || !node.parent().is_some_and(|parent| {
                parent.kind() == "call_expression"
                    && parent.child_by_field_name("function") == Some(node)
            })
        {
            return Ok(None);
        }
        if role == StructuralRole::CallName {
            candidate.syntax = "go.unproven_direct_function";
            return Ok(Some(true));
        }
        if !matches!(role, StructuralRole::Reference | StructuralRole::Signature) {
            return Ok(None);
        }
        if self.certified(node, cancel)? {
            candidate.role = StructuralRole::Reference;
            candidate.syntax = "go.direct_file_function";
            candidate.required = true;
            return Ok(Some(true));
        }
        if role == StructuralRole::Signature {
            return Ok(Some(false));
        }
        candidate.syntax = "go.unproven_direct_function";
        Ok(Some(true))
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    fn parse(source: &[u8]) -> tree_sitter::Tree {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_go::LANGUAGE.into())
            .unwrap();
        parser.parse(source, None).unwrap()
    }

    fn proof(
        source: &[u8],
        maximum: usize,
        complete: bool,
        cancel: &Cancellation,
    ) -> Result<(Vec<(usize, usize)>, bool), AdapterError> {
        let tree = parse(source);
        let mut cache = CaptureCache::new(source, maximum, complete);
        let mut found = Vec::new();
        let mut cursor = tree.root_node().walk();
        loop {
            let node = cursor.node();
            if node.kind() == "identifier" {
                for role in [StructuralRole::Reference, StructuralRole::Signature] {
                    let mut candidate = QueryCandidate {
                        start: node.start_byte(),
                        end: node.end_byte(),
                        role,
                        syntax: "go.identifier",
                        required: false,
                        native_depth: 0,
                    };
                    cache.refine(node, role, &mut candidate, cancel)?;
                    if candidate.syntax == "go.direct_file_function" {
                        assert!(candidate.required);
                        assert_eq!(candidate.role, StructuralRole::Reference);
                        found.push((node.start_byte(), node.end_byte()));
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
                    found.sort_unstable();
                    found.dedup();
                    return Ok((found, cache.exhausted()));
                }
            }
        }
    }

    #[test]
    fn original_all_four_sites_use_the_same_complete_bounded_census() {
        let source = include_bytes!("../../tests/fixtures/go_written_calls/time.go");
        let tree = parse(source);
        assert!(!tree.root_node().has_error());
        let (sites, exhausted) = proof(source, 16384, true, &Cancellation::new()).unwrap();
        assert!(!exhausted);
        for site in [(1051, 1060), (1162, 1171), (1398, 1411)] {
            assert!(sites.contains(&site));
        }
        assert_eq!(sites.len(), 4);
    }

    #[test]
    fn native_shadow_and_namespace_shapes_do_not_certify_another_target() {
        for source in [
            "package sample\nfunc target(){}; func caller(target func()){target()}\n",
            "package sample\nfunc target(){}; func caller(){target := external; target()}\n",
            "package sample\nfunc target(){}; func caller(){for target := range values {target()}}\n",
            "package sample\nfunc target(){}; func caller(){switch target := value.(type) {default: target()}}\n",
            "package sample\nimport . \"external\"\nfunc target(){}; func caller(){target()}\n",
            "package sample\nimport target \"external\"\nfunc target(){}; func caller(){target()}\n",
            "package sample\nfunc target(){}; func target(){}; func caller(){target()}\n",
            "package sample\nfunc target(){}; func caller(){func(){target()}()}\n",
            "package sample\nfunc target(){}; func (r Item) caller(){target()}\n",
            "package sample\nfunc caller(){value()}; var value func()\n",
            "package sample\nfunc init(){}; func caller(){init()}\n",
            "package sample\nfunc target(){}; func caller(){receiver.target()}\n",
        ] {
            assert!(
                !parse(source.as_bytes()).root_node().has_error(),
                "native premise: {source}"
            );
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
    fn partial_recovered_exhausted_and_cancelled_inputs_keep_proof_unavailable() {
        let source = b"package sample\nfunc target(){}; func caller(){target()}\n";
        assert!(
            !proof(source, 16384, true, &Cancellation::new())
                .unwrap()
                .0
                .is_empty()
        );
        assert!(proof(source, 0, true, &Cancellation::new()).unwrap().1);
        assert!(
            proof(source, 16384, false, &Cancellation::new())
                .unwrap()
                .0
                .is_empty()
        );
        let invalid = b"package sample\nfunc target(){}; func caller(){target(}\n";
        assert!(parse(invalid).root_node().has_error());
        assert!(
            proof(invalid, 16384, true, &Cancellation::new())
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
