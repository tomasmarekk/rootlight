//! Certify written GDScript file owners and direct callable declarations.
//! A unique clean class_name supplies ownership; absent owners remain explicit gaps.

use std::{
    collections::{HashMap, HashSet},
    ops::Range,
};

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

pub(super) struct CaptureCache<'source> {
    source: &'source [u8],
    complete: bool,
    initialized: bool,
    root_id: Option<usize>,
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
            initialized: false,
            root_id: None,
            remaining: maximum.min(HARD_MAX_QUERY_CAPTURES).saturating_mul(32),
            exhausted: false,
            declarations: HashMap::new(),
            definitions: HashSet::new(),
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
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
        cancellation: &Cancellation,
    ) -> Result<Vec<Node<'tree>>, AdapterError> {
        if node.child_count() > self.remaining {
            self.exhausted = true;
            return Ok(Vec::new());
        }
        let mut children = Vec::new();
        children
            .try_reserve_exact(node.child_count())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if !self.visit(cancellation)? {
                return Ok(Vec::new());
            }
            children.push(child);
        }
        Ok(children)
    }

    fn name(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<Option<&'source str>, AdapterError> {
        if !matches!(node.kind(), "name" | "_init") || !clean(node, self.source) {
            return Ok(None);
        }
        let Some(bytes) = self.source.get(node.byte_range()) else {
            return Ok(None);
        };
        for _ in bytes.chunks(64) {
            if !self.visit(cancellation)? {
                return Ok(None);
            }
        }
        let Ok(text) = std::str::from_utf8(bytes) else {
            return Ok(None);
        };
        Ok(
            rootlight_adapter_sdk::structural_captured_name_for_language(
                "gdscript",
                text,
                text.len(),
            )
            .filter(|name| name.as_ref() == text)
            .map(|_| text),
        )
    }

    fn callable_name<'tree>(
        &mut self,
        node: Node<'tree>,
        cancellation: &Cancellation,
    ) -> Result<Option<Node<'tree>>, AdapterError> {
        if !clean(node, self.source) {
            return Ok(None);
        }
        let children = self.children(node, cancellation)?;
        let mut name = None;
        let mut count = 0_usize;
        for child in children {
            if !self.visit(cancellation)? {
                return Ok(None);
            }
            if (node.kind() == "constructor_definition"
                && child.kind() == "_init"
                && !child.is_named())
                || (node.kind() == "function_definition"
                    && child.kind() == "name"
                    && node
                        .child_by_field_name("name")
                        .is_some_and(|name| name.id() == child.id()))
            {
                name = Some(child);
                count = count.saturating_add(1);
            }
        }
        let Some(name) = name.filter(|_| count == 1) else {
            return Ok(None);
        };
        let Some(parameters) = node.child_by_field_name("parameters") else {
            return Ok(None);
        };
        let Some(body) = node.child_by_field_name("body") else {
            return Ok(None);
        };
        if parameters.kind() != "parameters"
            || body.kind() != "body"
            || parameters
                .parent()
                .is_none_or(|parent| parent.id() != node.id())
            || body.parent().is_none_or(|parent| parent.id() != node.id())
            || name.end_byte() > parameters.start_byte()
            || parameters.end_byte() > body.start_byte()
            || body.end_byte() != node.end_byte()
            || self.name(name, cancellation)?.is_none()
        {
            return Ok(None);
        }
        Ok(Some(name))
    }

    fn insert(
        &mut self,
        node: Node<'_>,
        extent: Range<usize>,
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
                extent,
                name: name.byte_range(),
                header,
                syntax,
            },
        );
        Ok(())
    }

    fn initialize(
        &mut self,
        node: Node<'_>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        let mut root = node;
        while let Some(parent) = root.parent() {
            if !self.visit(cancellation)? {
                return Ok(());
            }
            root = parent;
        }
        if self.initialized {
            if self.root_id != Some(root.id()) {
                return Err(query_failure("gdscript-cache-tree-mismatch"));
            }
            return Ok(());
        }
        self.initialized = true;
        self.root_id = Some(root.id());
        if root.kind() != "source" {
            return Ok(());
        }
        let children = self.children(root, cancellation)?;
        let mut class_statement = None;
        let mut class_count = 0_usize;
        for child in &children {
            if !self.visit(cancellation)? {
                return Ok(());
            }
            if child.kind() == "class_name_statement" {
                class_count = class_count.saturating_add(1);
                class_statement = Some(*child);
            }
        }
        let prefix = self.source.get(..root.start_byte()).unwrap_or(&[]);
        let whole_clean = self.complete
            && root.end_byte() == self.source.len()
            && (prefix.is_empty() || prefix == b"\xef\xbb\xbf")
            && clean(root, self.source);
        let mut class_proved = false;
        if whole_clean
            && class_count == 1
            && let Some(statement) = class_statement
            && let Some(name) = statement.child_by_field_name("name")
            && name.kind() == "name"
            && name
                .parent()
                .is_some_and(|parent| parent.id() == statement.id())
            && self.name(name, cancellation)?.is_some()
        {
            let written = self.children(statement, cancellation)?;
            if written
                .iter()
                .filter(|child| child.kind() == "name")
                .count()
                == 1
            {
                self.insert(
                    statement,
                    0..self.source.len(),
                    name,
                    statement.byte_range(),
                    "gdscript.class",
                )?;
                class_proved = true;
            }
        }
        let mut callables = Vec::new();
        callables
            .try_reserve_exact(children.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut name_counts = HashMap::<&str, usize>::new();
        name_counts
            .try_reserve(children.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for child in children {
            if !self.visit(cancellation)? {
                return Ok(());
            }
            if !matches!(
                child.kind(),
                "constructor_definition" | "function_definition"
            ) {
                continue;
            }
            if let Some(name) = self.callable_name(child, cancellation)?
                && let Some(text) = self.name(name, cancellation)?
            {
                let count = name_counts.entry(text).or_insert(0);
                *count = count.saturating_add(1);
                callables.push((child, name, text));
            }
        }
        for (callable, name, text) in callables {
            if !self.visit(cancellation)? {
                return Ok(());
            }
            if name_counts.get(text) != Some(&1) {
                continue;
            }
            let syntax = if !class_proved {
                "gdscript.unowned_function"
            } else if callable.kind() == "constructor_definition" {
                "gdscript.constructor"
            } else {
                "gdscript.method"
            };
            let body = callable
                .child_by_field_name("body")
                .ok_or_else(|| query_failure("gdscript-callable-body-invariant"))?;
            self.insert(
                callable,
                callable.byte_range(),
                name,
                callable.start_byte()..body.start_byte(),
                syntax,
            )?;
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
    cancellation: &Cancellation,
    cache: &mut CaptureCache<'_>,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    if !std::ptr::eq(source, cache.source) {
        return Err(query_failure("gdscript-cache-source-mismatch"));
    }
    cache.initialize(node, cancellation)?;
    if !cache.visit(cancellation)? {
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
            "gdscript.source_file"
        } else {
            "gdscript.file"
        }
    } else if let Some(declaration) = cache.declarations.get(&node.id()) {
        match role {
            StructuralRole::Scope | StructuralRole::Declaration => {
                range = declaration.extent.clone();
                declaration.syntax
            }
            StructuralRole::Definition => {
                range = declaration.name.clone();
                "gdscript.name"
            }
            StructuralRole::Signature => {
                range = declaration.header.clone();
                "gdscript.header"
            }
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Declaration {
        "gdscript.unsupported"
    } else if role == StructuralRole::Reference
        && matches!(node.kind(), "identifier" | "name")
        && clean(node, source)
    {
        if cache
            .definitions
            .contains(&(node.start_byte(), node.end_byte()))
        {
            return Ok(None);
        }
        "gdscript.unresolved_name"
    } else if role == StructuralRole::Expression {
        "gdscript.construct_unavailable"
    } else if role == StructuralRole::Comment {
        "gdscript.comment"
    } else if role == StructuralRole::StringLiteral {
        "gdscript.string"
    } else {
        return Ok(None);
    };
    let mut native_depth = 0_usize;
    let mut parent = node.parent();
    while let Some(current) = parent {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        native_depth = native_depth
            .checked_add(1)
            .ok_or_else(|| query_failure("gdscript-native-depth"))?;
        parent = current.parent();
    }
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_cancel::CancellationReason;

    fn tree(source: &[u8]) -> tree_sitter::Tree {
        let scope = tree_sitter_gdscript::ScannerScope::begin().unwrap();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_gdscript::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        drop(parser);
        scope.finish().unwrap();
        tree
    }

    #[test]
    fn original_clean_file_class_and_native_constructor_keep_all_four_exact_names() {
        let source = include_bytes!("../../tests/fixtures/gdscript/GroupCel.gd");
        let parsed = tree(source);
        assert!(!parsed.root_node().has_error());
        let mut cache = CaptureCache::new(source, 16_384, true);
        cache
            .initialize(parsed.root_node(), &Cancellation::new())
            .unwrap();
        for (syntax, name, extent, header) in [
            ("gdscript.class", 11..19, 0..433, 0..19),
            ("gdscript.constructor", 181..186, 176..270, 176..212),
            ("gdscript.method", 281..290, 276..373, 276..302),
            ("gdscript.method", 384..398, 379..431, 379..411),
        ] {
            assert_eq!(
                cache
                    .declarations
                    .values()
                    .filter(|decl| decl.syntax == syntax
                        && decl.name == name
                        && decl.extent == extent
                        && decl.header == header)
                    .count(),
                1
            );
        }
        assert_eq!(cache.declarations.len(), 4);
        assert!(!cache.exhausted());
    }

    #[test]
    fn absent_or_duplicate_class_names_preserve_functions_without_method_owner_claims() {
        for source in [
            b"extends Node\nfunc available(value):\n\treturn value\n".as_slice(),
            b"class_name First\nclass_name Second\nfunc available(value):\n\treturn value\n"
                .as_slice(),
        ] {
            let parsed = tree(source);
            let mut cache = CaptureCache::new(source, 16_384, true);
            cache
                .initialize(parsed.root_node(), &Cancellation::new())
                .unwrap();
            assert!(!cache.declarations.values().any(|decl| matches!(
                decl.syntax,
                "gdscript.class" | "gdscript.method" | "gdscript.constructor"
            )));
            assert!(
                cache
                    .declarations
                    .values()
                    .any(|decl| decl.syntax == "gdscript.unowned_function")
            );
        }
    }

    #[test]
    fn recovered_duplicate_callable_and_wrong_native_roles_do_not_forge_definitions() {
        for source in [
            b"class_name Written\nfunc same():\n\tpass\nfunc same():\n\tpass\n".as_slice(),
            b"class_name Written\nfunc broken(\n".as_slice(),
        ] {
            let parsed = tree(source);
            let mut cache = CaptureCache::new(source, 16_384, true);
            cache
                .initialize(parsed.root_node(), &Cancellation::new())
                .unwrap();
            assert!(
                !cache
                    .declarations
                    .values()
                    .any(|decl| matches!(decl.syntax, "gdscript.constructor" | "gdscript.method"))
            );
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
        }
    }

    #[test]
    fn original_literal_is_not_a_name_and_uncertified_identifiers_remain_optional() {
        let source = include_bytes!("../../tests/fixtures/gdscript/GroupCel.gd");
        let parsed = tree(source);
        let mut cache = CaptureCache::new(source, 16_384, true);
        let identifier = parsed
            .root_node()
            .descendant_for_byte_range(29, 36)
            .unwrap();
        assert_eq!(identifier.kind(), "identifier");
        let reference = candidate(
            identifier,
            StructuralRole::Reference,
            source,
            &Cancellation::new(),
            &mut cache,
        )
        .unwrap()
        .unwrap();
        assert_eq!(reference.syntax, "gdscript.unresolved_name");
        assert!(!reference.required);
        let literal = parsed
            .root_node()
            .descendant_for_byte_range(422, 430)
            .unwrap();
        assert_eq!(literal.kind(), "string");
        assert_eq!(literal.byte_range(), 421..431);
        assert!(
            candidate(
                literal,
                StructuralRole::Definition,
                source,
                &Cancellation::new(),
                &mut cache
            )
            .unwrap()
            .is_none()
        );
        assert!(
            candidate(
                literal,
                StructuralRole::Reference,
                source,
                &Cancellation::new(),
                &mut cache
            )
            .unwrap()
            .is_none()
        );
    }

    #[test]
    fn included_range_work_exhaustion_and_typed_cancellation_withhold_owner_authority() {
        let source = include_bytes!("../../tests/fixtures/gdscript/GroupCel.gd");
        let parsed = tree(source);
        let mut partial = CaptureCache::new(source, 16_384, false);
        partial
            .initialize(parsed.root_node(), &Cancellation::new())
            .unwrap();
        assert!(
            !partial
                .declarations
                .values()
                .any(|decl| decl.syntax == "gdscript.class")
        );
        assert!(
            partial
                .declarations
                .values()
                .any(|decl| decl.syntax == "gdscript.unowned_function")
        );
        let mut tiny = CaptureCache::new(source, 1, true);
        tiny.remaining = 1;
        tiny.initialize(parsed.root_node(), &Cancellation::new())
            .unwrap();
        assert!(tiny.exhausted());
        let stopped = Cancellation::new();
        assert!(stopped.cancel(CancellationReason::ClientRequest));
        let mut cancelled = CaptureCache::new(source, usize::MAX, true);
        assert_eq!(cancelled.remaining, HARD_MAX_QUERY_CAPTURES * 32);
        assert!(matches!(
            cancelled.initialize(parsed.root_node(), &stopped),
            Err(AdapterError::Cancelled {
                reason: CancellationReason::ClientRequest
            })
        ));
        assert!(cancelled.declarations.is_empty());
    }
}
