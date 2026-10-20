//! Admit written Haxe owners and bare argument reads from bounded native proof.
//! Macro quotations, conditional branches and unproved binders cannot supply it.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{HARD_MAX_QUERY_CAPTURES, QueryCandidate, StructuralRole};

#[derive(Clone)]
struct Declaration {
    range: Range<usize>,
    name: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
    owner: Option<usize>,
}

#[derive(Default)]
struct Function<'s> {
    names: HashMap<&'s str, Option<usize>>,
    barrier: bool,
}

pub(super) struct CaptureCache<'s> {
    source: &'s [u8],
    complete: bool,
    initialized: bool,
    remaining: usize,
    exhausted: bool,
    declarations: HashMap<usize, Declaration>,
    functions: HashMap<usize, Function<'s>>,
    reads: HashMap<usize, Range<usize>>,
}

impl<'s> CaptureCache<'s> {
    pub(super) fn new(source: &'s [u8], maximum: usize, complete: bool) -> Self {
        Self {
            source,
            complete,
            initialized: false,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES) * 32,
            exhausted: false,
            declarations: HashMap::new(),
            functions: HashMap::new(),
            reads: HashMap::new(),
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

    fn name(
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
        let Ok(name) = std::str::from_utf8(raw) else {
            return Ok(None);
        };
        Ok(
            rootlight_adapter_sdk::structural_captured_name_for_language("haxe", name, name.len())
                .filter(|parsed| parsed.as_ref() == name)
                .map(|_| name),
        )
    }

    fn nearest_function<'tree>(
        &mut self,
        node: Node<'tree>,
        cancel: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        let mut current = node.parent();
        while let Some(parent) = current {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if matches!(
                parent.kind(),
                "function_declaration" | "function_expression"
            ) {
                return Ok(Some(parent));
            }
            current = parent.parent();
        }
        Ok(None)
    }

    fn blocked(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<bool, AdapterError> {
        let mut current = Some(node);
        while let Some(parent) = current {
            if !self.visit(cancel)? {
                return Ok(true);
            }
            if parent.is_error()
                || parent.is_missing()
                || matches!(
                    parent.kind(),
                    "macro_expression"
                        | "reification_expression"
                        | "preprocessor_statement"
                        | "metadata"
                )
            {
                return Ok(true);
            }
            current = parent.parent();
        }
        Ok(false)
    }

    fn bind(
        &mut self,
        function: usize,
        name: Option<&'s str>,
        target: Option<usize>,
    ) -> Result<(), AdapterError> {
        let Some(entry) = self.functions.get_mut(&function) else {
            return Ok(());
        };
        let Some(name) = name else {
            entry.barrier = true;
            return Ok(());
        };
        entry
            .names
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        entry
            .names
            .entry(name)
            .and_modify(|value| *value = None)
            .or_insert(target);
        Ok(())
    }

    fn local_variable_static(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<bool>, AdapterError> {
        let Some(name) = node.child_by_field_name("name") else {
            return Ok(None);
        };
        let (mut keyword, mut static_local, mut named) = (false, false, false);
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if child.kind() == "comment" {
                continue;
            }
            if child == name {
                if !keyword {
                    return Ok(None);
                }
                named = true;
                continue;
            }
            if !named {
                match child.kind() {
                    "metadata" if !keyword => {}
                    "static" if !keyword && !static_local => static_local = true,
                    "var" | "final" if !keyword => keyword = true,
                    _ => return Ok(None),
                }
            } else if child.kind() == "access_identifiers" {
                return Ok(None);
            } else if child.kind() == "operator" {
                // The native rule also accepts field forms and general operators.
                // Only this initializer delimiter is part of a local header.
                return Ok(
                    (self.source.get(child.byte_range()) == Some(b"=")).then_some(static_local)
                );
            }
        }
        Ok((named && keyword).then_some(static_local))
    }

    fn declaration(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        if !clean(node, self.source) || self.blocked(node, cancel)? {
            return Ok(());
        }
        let Some(name_node) = node.child_by_field_name("name") else {
            return Ok(());
        };
        if self.name(name_node, cancel)?.is_none() {
            return Ok(());
        }
        let (syntax, owner, end) = match node.kind() {
            "class_declaration"
                if node
                    .parent()
                    .is_some_and(|parent| parent.kind() == "module") =>
            {
                let Some(body) = node
                    .child_by_field_name("body")
                    .filter(|body| body.kind() == "block")
                else {
                    return Ok(());
                };
                ("haxe.class", None, body.start_byte())
            }
            "function_declaration" => {
                let Some(block) = node.parent().filter(|parent| parent.kind() == "block") else {
                    return Ok(());
                };
                let Some(class) = block.parent().filter(|parent| {
                    parent.kind() == "class_declaration"
                        && parent.child_by_field_name("body") == Some(block)
                }) else {
                    return Ok(());
                };
                if !self.declarations.contains_key(&class.start_byte()) {
                    return Ok(());
                }
                let Some(body) = node
                    .child_by_field_name("body")
                    .filter(|body| body.kind() == "block")
                else {
                    return Ok(());
                };
                self.functions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                self.functions
                    .insert(node.start_byte(), Function::default());
                ("haxe.method", Some(class.start_byte()), body.start_byte())
            }
            "variable_declaration" => {
                let Some(block) = node.parent().filter(|parent| parent.kind() == "block") else {
                    return Ok(());
                };
                let Some(function) = block.parent().filter(|parent| {
                    parent.kind() == "function_declaration"
                        && parent.child_by_field_name("body") == Some(block)
                }) else {
                    return Ok(());
                };
                if !self.functions.contains_key(&function.start_byte())
                    || self.local_variable_static(node, cancel)?.is_none()
                {
                    return Ok(());
                }
                (
                    "haxe.variable",
                    Some(function.start_byte()),
                    name_node.end_byte(),
                )
            }
            _ => return Ok(()),
        };
        self.declarations
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.declarations.insert(
            node.start_byte(),
            Declaration {
                range: node.byte_range(),
                name: name_node.byte_range(),
                header: node.start_byte()..end,
                syntax,
                owner,
            },
        );
        Ok(())
    }

    fn census_binder(&mut self, node: Node<'_>, cancel: &Cancellation) -> Result<(), AdapterError> {
        let Some(function) = self.nearest_function(node, cancel)? else {
            return Ok(());
        };
        if !self.functions.contains_key(&function.start_byte()) {
            return Ok(());
        }
        let field = if matches!(node.kind(), "for_statement" | "comprehension_for") {
            "binding"
        } else {
            "name"
        };
        let name = match node.child_by_field_name(field) {
            Some(name) => self.name(name, cancel)?,
            None => None,
        };
        let target = self
            .declarations
            .get(&node.start_byte())
            .filter(|declaration| {
                declaration.syntax == "haxe.variable"
                    && declaration.owner == Some(function.start_byte())
            })
            .map(|_| node.start_byte());
        self.bind(function.start_byte(), name, target)
    }

    fn bare_argument(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let Some(call) = node
            .parent()
            .filter(|parent| parent.kind() == "call_expression" && clean(*parent, self.source))
        else {
            return Ok(false);
        };
        let mut cursor = call.walk();
        let (mut count, mut selected) = (0_usize, false);
        for (index, child) in call.children(&mut cursor).enumerate() {
            if !self.visit(cancel)? {
                return Ok(false);
            }
            if call.field_name_for_child(
                u32::try_from(index).map_err(|_| super::query_failure("haxe-child-index"))?,
            ) != Some("arguments_list")
            {
                continue;
            }
            if matches!(child.kind(), "(" | "," | ")") {
                if selected {
                    return Ok(count == 1);
                }
                count = 0;
            } else if child.kind() != "comment" {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| super::query_failure("haxe-argument-count"))?;
                selected |= child == node;
            }
        }
        Ok(false)
    }

    fn direct_read_owner(
        &mut self,
        node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<Option<usize>, AdapterError> {
        let Some(function) = self.nearest_function(node, cancel)? else {
            return Ok(None);
        };
        if !self.functions.contains_key(&function.start_byte()) {
            return Ok(None);
        }
        let Some(body) = function.child_by_field_name("body") else {
            return Ok(None);
        };
        let mut parent = node.parent();
        while let Some(current) = parent {
            if !self.visit(cancel)? {
                return Ok(None);
            }
            if current == body {
                return Ok(Some(function.start_byte()));
            }
            // A static initializer cannot close over ordinary method locals.
            if current.kind() == "variable_declaration"
                && self.local_variable_static(current, cancel)? != Some(false)
            {
                return Ok(None);
            }
            if !matches!(current.kind(), "call_expression" | "variable_declaration") {
                return Ok(None);
            }
            parent = current.parent();
        }
        Ok(None)
    }

    fn initialize(
        &mut self,
        mut node: Node<'_>,
        cancel: &Cancellation,
    ) -> Result<(), AdapterError> {
        if self.initialized {
            return Ok(());
        }
        self.initialized = true;
        while let Some(parent) = node.parent() {
            if !self.visit(cancel)? {
                return Ok(());
            }
            node = parent;
        }
        if node.kind() != "module" {
            return Ok(());
        }
        let root = node;
        let mut identifiers = Vec::new();
        let mut cursor = root.walk();
        loop {
            if !self.visit(cancel)? {
                return Ok(());
            }
            let node = cursor.node();
            let blocked = self.blocked(node, cancel)?;
            if !blocked {
                if matches!(
                    node.kind(),
                    "class_declaration" | "function_declaration" | "variable_declaration"
                ) {
                    self.declaration(node, cancel)?;
                }
                if matches!(
                    node.kind(),
                    "variable_declaration"
                        | "function_arg"
                        | "catch_clause"
                        | "for_statement"
                        | "comprehension_for"
                ) {
                    self.census_binder(node, cancel)?;
                }
                if node.kind() == "function_declaration"
                    && !self.functions.contains_key(&node.start_byte())
                {
                    self.census_binder(node, cancel)?;
                }
                if matches!(
                    node.kind(),
                    "function_expression" | "case_statement" | "preprocessor_statement"
                ) && let Some(function) = self.nearest_function(node, cancel)?
                    && let Some(entry) = self.functions.get_mut(&function.start_byte())
                {
                    entry.barrier = true;
                }
                if node.kind() == "identifier"
                    && self.name(node, cancel)?.is_some()
                    && self.bare_argument(node, cancel)?
                    && let Some(owner) = self.direct_read_owner(node, cancel)?
                {
                    identifiers
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    identifiers.push((node.byte_range(), owner));
                }
            } else if node.kind() == "preprocessor_statement"
                && let Some(function) = self.nearest_function(node, cancel)?
                && let Some(entry) = self.functions.get_mut(&function.start_byte())
            {
                entry.barrier = true;
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    if self.complete && !self.exhausted {
                        for (range, owner) in identifiers {
                            if !self.visit(cancel)? {
                                return Ok(());
                            }
                            let Some(name) = self
                                .source
                                .get(range.clone())
                                .and_then(|raw| std::str::from_utf8(raw).ok())
                            else {
                                continue;
                            };
                            let Some(function) = self
                                .functions
                                .get(&owner)
                                .filter(|function| !function.barrier)
                            else {
                                continue;
                            };
                            let Some(target) = function.names.get(name).copied().flatten() else {
                                continue;
                            };
                            let Some(declaration) = self.declarations.get(&target) else {
                                continue;
                            };
                            if declaration.range.end > range.start {
                                continue;
                            }
                            self.reads
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            self.reads
                                .insert(range.start, declaration.range.start..range.end);
                        }
                    }
                    return Ok(());
                }
            }
        }
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.has_error()
        && !node.is_missing()
        && !node.is_error()
        && source.get(node.byte_range()).is_some()
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    _source: &[u8],
    cancel: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cache.initialize(node, cancel)?;
    if !cache.visit(cancel)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let syntax = if node.kind() == "module"
        || (node.is_error()
            && node.parent().is_none()
            && matches!(
                role,
                StructuralRole::Root | StructuralRole::Module | StructuralRole::Scope
            )) {
        match role {
            StructuralRole::Root if node.is_error() => "haxe.recovered_file",
            StructuralRole::Root => "haxe.source_file",
            StructuralRole::Module => "haxe.file",
            StructuralRole::Scope => "haxe.file",
            _ => return Ok(None),
        }
    } else if matches!(
        role,
        StructuralRole::Declaration
            | StructuralRole::Definition
            | StructuralRole::Signature
            | StructuralRole::Scope
    ) && matches!(
        node.kind(),
        "class_declaration" | "function_declaration" | "variable_declaration"
    ) {
        if let Some(declaration) = cache.declarations.get(&node.start_byte()) {
            match role {
                StructuralRole::Definition => {
                    range = declaration.name.clone();
                    "haxe.name"
                }
                StructuralRole::Signature => {
                    range = declaration.header.clone();
                    "haxe.header"
                }
                StructuralRole::Scope => declaration.syntax,
                _ => declaration.syntax,
            }
        } else if role == StructuralRole::Declaration && !cache.blocked(node, cancel)? {
            "haxe.unsupported"
        } else {
            return Ok(None);
        }
    } else if node.kind() == "identifier"
        && matches!(role, StructuralRole::Reference | StructuralRole::Signature)
    {
        if cache.blocked(node, cancel)? || cache.name(node, cancel)?.is_none() {
            return Ok(None);
        }
        if let Some(marker) = cache.reads.get(&node.start_byte()) {
            if role == StructuralRole::Signature {
                range = marker.clone();
                "haxe.local_read_target"
            } else {
                "haxe.local_read"
            }
        } else if role == StructuralRole::Reference && cache.bare_argument(node, cancel)? {
            "haxe.unresolved_read"
        } else {
            return Ok(None);
        }
    } else if role == StructuralRole::Expression {
        match node.kind() {
            "macro_expression" | "reification_expression" => "haxe.macro_unavailable",
            "preprocessor_statement" => "haxe.conditional_unavailable",
            _ => "haxe.construct_unavailable",
        }
    } else if node.kind() == "comment" && role == StructuralRole::Comment {
        "haxe.comment"
    } else if node.kind() == "string" && role == StructuralRole::StringLiteral {
        "haxe.string"
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
            .ok_or_else(|| super::query_failure("haxe-native-depth"))?;
        parent = current.parent();
    }
    if role == StructuralRole::Signature && syntax == "haxe.local_read_target" {
        depth = 0;
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: matches!(
            role,
            StructuralRole::Root
                | StructuralRole::Module
                | StructuralRole::Scope
                | StructuralRole::Declaration
                | StructuralRole::Definition
                | StructuralRole::Signature
        ) || syntax == "haxe.local_read",
        native_depth: depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GrammarFamily, registry::language_for};
    use rootlight_cancel::CancellationReason;

    fn inspect(source: &str, complete: bool, check: impl FnOnce(&CaptureCache<'_>)) {
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Haxe))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}: {}",
            source,
            tree.root_node().to_sexp()
        );
        let mut cache = CaptureCache::new(source.as_bytes(), 16_384, complete);
        cache
            .initialize(tree.root_node(), &Cancellation::new())
            .unwrap();
        assert!(!cache.exhausted());
        check(&cache);
    }

    #[test]
    fn original_host_ancestry_and_target_marker_are_exact_native_witnesses() {
        let source = include_str!("../../tests/fixtures/haxe/MacroTools.hx");
        inspect(source, true, |cache| {
            for (start, name, kind, owner) in [
                (0, 6..16, "haxe.class", None),
                (435, 464..487, "haxe.method", Some(0)),
                (522, 526..529, "haxe.variable", Some(435)),
            ] {
                let declaration = cache.declarations.get(&start).unwrap();
                assert_eq!(declaration.name, name);
                assert_eq!(declaration.syntax, kind);
                assert_eq!(declaration.owner, owner);
            }
            assert_eq!(cache.reads.get(&609), Some(&(522..612)));
        });
        inspect(source, false, |cache| assert!(cache.reads.is_empty()));
    }

    #[test]
    fn ordinary_and_macro_host_modifiers_do_not_quote_the_written_function_body() {
        for modifier in ["", "static ", "public static macro "] {
            let source = format!(
                "class Written {{ {modifier}function measure() {{ var value = 1; consume(value); }} }}"
            );
            inspect(&source, true, |cache| {
                assert_eq!(
                    cache
                        .declarations
                        .values()
                        .filter(|decl| decl.syntax == "haxe.class")
                        .count(),
                    1
                );
                assert_eq!(
                    cache
                        .declarations
                        .values()
                        .filter(|decl| decl.syntax == "haxe.method")
                        .count(),
                    1
                );
                assert_eq!(
                    cache
                        .declarations
                        .values()
                        .filter(|decl| decl.syntax == "haxe.variable")
                        .count(),
                    1
                );
                assert_eq!(cache.reads.len(), 1);
            });
        }
    }

    #[test]
    fn repeated_shadow_formal_loop_catch_and_other_function_names_never_rescue_a_read() {
        for source in [
            "class Written { function measure() { consume(value); var value = 1; } }",
            "class Written { function measure() { var value = 1; var value = 2; consume(value); } }",
            "class Written { function measure(value:Int) { var value = 1; consume(value); } }",
            "class Written { function measure() { var value = 1; { var value = 2; } consume(value); } }",
            "class Written { function measure() { var value = 1; function value() {} consume(value); } }",
            "class Written { function first() { var value = 1; } function second() { consume(value); } }",
            "class Written { var value = 1; function measure() { consume(value); } }",
            "class Written { function measure() { var value = 1; for (value in values) {} consume(value); } }",
            "class Written { function measure() { var value = 1; try consume(0) catch(value:Dynamic) {} consume(value); } }",
        ] {
            inspect(source, true, |cache| {
                assert!(cache.reads.is_empty(), "{source}")
            });
        }
    }

    #[test]
    fn quoted_spliced_conditional_and_nested_callable_sources_do_not_become_host_binders() {
        for quoted in [
            "macro { var phantom = 1; consume(value); }",
            "macro value",
            "macro $e{value}",
        ] {
            let source = format!(
                "class Written {{ static macro function measure() {{ var value = 1; return {quoted}; }} }}"
            );
            inspect(&source, true, |cache| {
                assert!(cache.reads.is_empty());
                assert!(
                    !cache
                        .declarations
                        .values()
                        .any(|decl| &source[decl.name.clone()] == "phantom")
                );
            });
        }
        let source = "class Written { function measure() { var value = 1;\n#if enabled\nvar other = 2;\n#end\nconsume(value); } }";
        inspect(source, true, |cache| assert!(cache.reads.is_empty()));
        let source = "class Written { function measure() { var value = 1; var closure = function() { consume(value); }; } }";
        inspect(source, true, |cache| assert!(cache.reads.is_empty()));
    }

    #[test]
    fn argument_delimiters_exclude_writes_and_members_but_not_unrelated_later_assignments() {
        for argument in [
            "value = 0",
            "value += 1",
            "value++",
            "holder.value",
            "value + 1",
        ] {
            let source = format!(
                "class Written {{ function measure() {{ var value = 1; consume({argument}); }} }}"
            );
            inspect(&source, true, |cache| {
                assert!(cache.reads.is_empty(), "{source}")
            });
        }
        let source = "class Written { function measure() { var value = 1; consume(value); var other = 1; other = 2; } }";
        inspect(source, true, |cache| assert_eq!(cache.reads.len(), 1));
        let source = "class Written { function first() { var value = 1; consume(value); } function second() { var value = 2; consume(value); } }";
        inspect(source, true, |cache| assert_eq!(cache.reads.len(), 2));
    }

    #[test]
    fn native_work_and_cancellation_are_checked_before_any_certificate_publication() {
        let source = "class Written { function measure() { var value = 1; consume(value); } }";
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&language_for(GrammarFamily::Haxe))
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        for maximum in [0, 1] {
            let mut cache = CaptureCache::new(source.as_bytes(), maximum, true);
            cache
                .initialize(tree.root_node(), &Cancellation::new())
                .unwrap();
            assert!(cache.exhausted());
            assert!(cache.reads.is_empty());
        }
        let mut cache = CaptureCache::new(source.as_bytes(), usize::MAX, true);
        assert_eq!(cache.remaining, HARD_MAX_QUERY_CAPTURES * 32);
        cache
            .initialize(tree.root_node(), &Cancellation::new())
            .unwrap();
        assert!(!cache.exhausted());
        assert_eq!(cache.reads.len(), 1);
        let cancel = Cancellation::new();
        cancel.cancel(CancellationReason::ClientRequest);
        let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
        assert!(matches!(
            cache.initialize(tree.root_node(), &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(cache.reads.is_empty());
    }

    #[test]
    fn local_header_native_shapes_withhold_invalid_operators_and_class_field_syntax() {
        for declaration in [
            "var value + 1;",
            "var value - 1;",
            "var value == 1;",
            "var value += 1;",
            "var value && true;",
            "var value(get,set):Int;",
            "public var value = 1;",
            "private var value = 1;",
            "override var value = 1;",
        ] {
            let source = format!(
                "class Written {{ function measure() {{ {declaration} consume(value); }} }}"
            );
            let mut parser = tree_sitter::Parser::new();
            parser
                .set_language(&language_for(GrammarFamily::Haxe))
                .unwrap();
            let tree = parser.parse(&source, None).unwrap();
            assert!(
                !tree.root_node().has_error(),
                "native premise: {source}: {}",
                tree.root_node().to_sexp()
            );
            let start = source.find("value").unwrap();
            let name = tree
                .root_node()
                .named_descendant_for_byte_range(start, start + 5)
                .unwrap();
            assert_eq!(name.kind(), "identifier", "{source}");
            let variable = name.parent().unwrap();
            assert_eq!(variable.kind(), "variable_declaration", "{source}");
            assert_eq!(variable.child_by_field_name("name"), Some(name));
            let mut cursor = variable.walk();
            let direct: Vec<_> = variable
                .children(&mut cursor)
                .map(|child| {
                    (
                        child.kind().to_owned(),
                        source[child.byte_range()].to_owned(),
                    )
                })
                .collect();
            if declaration.contains("(get,set)") {
                assert!(
                    direct.iter().any(|(kind, _)| kind == "access_identifiers"),
                    "{direct:?}"
                );
            } else if declaration.starts_with("var ") {
                let operator = direct.iter().find(|(kind, _)| kind == "operator").unwrap();
                assert_ne!(operator.1, "=", "{direct:?}");
            } else {
                assert!(
                    direct.iter().any(|(kind, _)| matches!(
                        kind.as_str(),
                        "public" | "private" | "override"
                    )),
                    "{direct:?}"
                );
            }
            let mut cache = CaptureCache::new(source.as_bytes(), 16_384, true);
            cache
                .initialize(tree.root_node(), &Cancellation::new())
                .unwrap();
            assert!(!cache.exhausted());
            assert!(
                !cache
                    .declarations
                    .values()
                    .any(|decl| decl.syntax == "haxe.variable"),
                "{source}: {direct:?}"
            );
            assert!(cache.reads.is_empty(), "{source}");
        }
    }

    #[test]
    fn valid_local_headers_keep_metadata_static_and_nested_initializer_operators() {
        for declaration in [
            "var value = 1;",
            "var value:Int = 1 + 2;",
            "var value:Int; value = 1;",
            "final value = 1;",
            "@:keep var value:Int = 1;",
            "static var value:Int = 1;",
            "static final value = 1;",
        ] {
            let source = format!(
                "class Written {{ function measure() {{ {declaration} consume(value); }} }}"
            );
            inspect(&source, true, |cache| {
                assert_eq!(
                    cache
                        .declarations
                        .values()
                        .filter(|decl| decl.syntax == "haxe.variable")
                        .count(),
                    1,
                    "{source}"
                );
                assert_eq!(cache.reads.len(), 1, "{source}");
            });
        }
    }

    #[test]
    fn static_initializer_cannot_certify_capture_of_an_ordinary_method_local() {
        let source = "class Written { function measure() { var value = 1; static var cached = consume(value); consume(cached); } }";
        inspect(source, true, |cache| {
            assert_eq!(
                cache
                    .declarations
                    .values()
                    .filter(|decl| decl.syntax == "haxe.variable")
                    .count(),
                2
            );
            let forbidden = source.rfind("value").unwrap();
            assert!(!cache.reads.contains_key(&forbidden));
            let later = source.rfind("cached").unwrap();
            assert!(cache.reads.contains_key(&later));
        });
    }

    #[test]
    fn native_method_header_keeps_modifier_and_all_trivia_before_body() {
        let source =
            "class Written { static function measure() { var value = 1; consume(value); } }";
        inspect(source, true, |cache| {
            let method = cache.declarations.get(&16).unwrap();
            assert_eq!(method.syntax, "haxe.method");
            assert_eq!(method.name, 32..39);
            assert_eq!(method.range, 16..76);
            assert_eq!(method.header, 16..42);
            assert_eq!(&source[method.header.clone()], "static function measure() ");
        });
    }

    #[test]
    fn native_static_initializer_withholds_outer_local_but_keeps_later_static_read() {
        let source = "class Written { function measure() { var value = 1; static var cached = consume(value); consume(cached); } }";
        inspect(source, true, |cache| {
            assert_eq!(cache.declarations.get(&37).unwrap().name, 41..46);
            assert_eq!(cache.declarations.get(&52).unwrap().name, 63..69);
            assert!(!cache.reads.contains_key(&80));
            assert_eq!(cache.reads.get(&96), Some(&(52..102)));
        });
    }
}
