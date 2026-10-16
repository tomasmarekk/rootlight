//! V capture proofs retain written declaration tokens and physical owners.
//! Compiler, receiver-type and compile-time binding are separate obligations.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone)]
struct Declaration {
    range: Range<usize>,
    name: Range<usize>,
    header: Range<usize>,
    syntax: &'static str,
}

pub(super) struct CaptureCache {
    declarations: HashMap<usize, Option<Declaration>>,
    visibility: HashMap<usize, bool>,
    remaining: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(remaining: usize) -> Self {
        Self {
            declarations: HashMap::new(),
            visibility: HashMap::new(),
            remaining,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining = remaining;
        Ok(true)
    }

    fn declaration(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<Declaration>, AdapterError> {
        if let Some(found) = self.declarations.get(&node.id()) {
            return Ok(found.clone());
        }
        let proof = declaration(node, source, cancellation, self)?;
        self.declarations
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.declarations.insert(node.id(), proof.clone());
        Ok(proof)
    }

    fn visible(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let mut current = node.parent();
        let mut pending = Vec::new();
        let mut visible = true;
        while let Some(parent) = current {
            if let Some(found) = self.visibility.get(&parent.id()) {
                visible = *found;
                break;
            }
            if !self.visit(cancellation)? {
                return Ok(false);
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(parent.id());
            if matches!(
                parent.kind(),
                "ERROR"
                    | "line_comment"
                    | "block_comment"
                    | "raw_string_literal"
                    | "c_string_literal"
                    | "function_literal"
                    | "anon_struct_type"
            ) || (owner(parent.kind())
                && self.declaration(parent, source, cancellation)?.is_none())
            {
                visible = false;
                break;
            }
            current = parent.parent();
        }
        self.visibility
            .try_reserve(pending.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for id in pending {
            self.visibility.insert(id, visible);
        }
        Ok(visible)
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn identifier(node: Node<'_>, source: &[u8]) -> bool {
    node.kind() == "identifier"
        && clean(node, source)
        && std::str::from_utf8(source.get(node.byte_range()).unwrap_or_default())
            .ok()
            .and_then(|text| structural_captured_name_for_language("v", text, text.len()))
            .is_some()
}

fn named<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if child.kind() == kind {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn owner(kind: &str) -> bool {
    matches!(
        kind,
        "struct_declaration"
            | "interface_declaration"
            | "enum_declaration"
            | "function_declaration"
            | "static_method_declaration"
            | "interface_method_definition"
    )
}

fn owner_header(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Range<usize>>, AdapterError> {
    let body = node.child_by_field_name("body");
    let mut header_end = node.end_byte();
    let mut closed = false;
    let mut opened = false;
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if Some(child.id()) == body.map(|body| body.id()) {
            header_end = child.start_byte();
            let first = child.child(0);
            let last = child
                .child(u32::try_from(child.child_count().saturating_sub(1)).unwrap_or(u32::MAX));
            if !first.is_some_and(|first| first.kind() == "{" && clean(first, source))
                || !last.is_some_and(|last| last.kind() == "}" && clean(last, source))
            {
                return Ok(None);
            }
            break;
        }
        if child.kind() == "{" {
            if !clean(child, source) {
                return Ok(None);
            }
            header_end = child.end_byte();
            opened = true;
        } else if child.kind() == "}" && opened {
            closed = clean(child, source);
        } else if !opened && !clean(child, source) {
            return Ok(None);
        }
    }
    if matches!(
        node.kind(),
        "struct_declaration" | "interface_declaration" | "enum_declaration"
    ) && !closed
    {
        return Ok(None);
    }
    Ok(Some(node.start_byte()..header_end))
}

fn binding_identifier(node: Node<'_>, source: &[u8]) -> bool {
    if !identifier(node, source) {
        return false;
    }
    let mut current = node;
    // A short declaration's left side is syntactically an expression list.
    // Only complete identifier wrappers establish a new written binding.
    for _ in 0..4 {
        let Some(parent) = current.parent() else {
            return false;
        };
        if parent.kind() == "var_declaration" {
            return parent
                .child_by_field_name("var_list")
                .is_some_and(|left| left.id() == current.id())
                && clean(parent, source);
        }
        if !matches!(
            parent.kind(),
            "reference_expression"
                | "mutable_expression"
                | "mutable_identifier"
                | "expression_list"
                | "identifier_list"
        ) {
            return false;
        }
        current = parent;
    }
    false
}

fn declaration(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Declaration>, AdapterError> {
    let (name, syntax) = match node.kind() {
        "module_clause" => (named(node, "identifier", cancellation, cache)?, "v.module"),
        "import_spec" => (import_name(node, cancellation, cache)?, "v.import"),
        "struct_declaration" => {
            let mut syntax = "v.struct";
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !cache.visit(cancellation)? {
                    return Ok(None);
                }
                if child.kind() == "union" && clean(child, source) {
                    syntax = "v.union";
                    break;
                }
            }
            (node.child_by_field_name("name"), syntax)
        }
        "interface_declaration" => (node.child_by_field_name("name"), "v.interface"),
        "enum_declaration" => (node.child_by_field_name("name"), "v.enum"),
        "type_declaration" => (node.child_by_field_name("name"), "v.type"),
        "function_declaration" => (
            node.child_by_field_name("name"),
            if node.child_by_field_name("receiver").is_some() {
                "v.method"
            } else {
                "v.function"
            },
        ),
        "static_method_declaration" | "interface_method_definition" => {
            (node.child_by_field_name("name"), "v.method")
        }
        "struct_field_declaration" => (node.child_by_field_name("name"), "v.field"),
        "enum_field_definition" => (node.child_by_field_name("name"), "v.enumerator"),
        "const_definition" => (node.child_by_field_name("name"), "v.constant"),
        "global_var_definition" | "var_definition" => {
            (node.child_by_field_name("name"), "v.variable")
        }
        "parameter_declaration" => (node.child_by_field_name("name"), "v.parameter"),
        "receiver" => (node.child_by_field_name("name"), "v.receiver"),
        "generic_parameter" => (
            named(node, "identifier", cancellation, cache)?,
            "v.type_parameter",
        ),
        "identifier" if binding_identifier(node, source) => (Some(node), "v.variable"),
        _ => return Ok(None),
    };
    let Some(name) = name else {
        return Ok(None);
    };
    if syntax == "v.variable" && source.get(name.byte_range()) == Some(b"_") {
        return Ok(None);
    }
    let operator = matches!(syntax, "v.function" | "v.method")
        && name.kind() == "overridable_operator"
        && clean(name, source);
    if !operator && !identifier(name, source) {
        return Ok(None);
    }
    let header = if owner(node.kind()) {
        let Some(header) = owner_header(node, source, cancellation, cache)? else {
            return Ok(None);
        };
        header
    } else {
        if !clean(node, source) {
            return Ok(None);
        }
        node.byte_range()
    };
    Ok(Some(Declaration {
        range: node.byte_range(),
        name: name.byte_range(),
        header,
        syntax,
    }))
}

fn reference_name(node: Node<'_>) -> Option<Node<'_>> {
    let child = node.named_child(0)?;
    (node.named_child_count() == 1 && child.kind() == "identifier").then_some(child)
}

fn import_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let path = named(node, "import_alias", cancellation, cache)?.or(named(
        node,
        "import_path",
        cancellation,
        cache,
    )?);
    let Some(path) = path else {
        return Ok(None);
    };
    let mut last = None;
    let mut cursor = path.walk();
    for child in path.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if child.kind() == "import_name" {
            last = child.named_child(0);
        }
    }
    Ok(last)
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<QueryCandidate>, AdapterError> {
    if !cache.visit(cancellation)? || !cache.visible(node, source, cancellation)? {
        return Ok(None);
    }
    let mut range = node.byte_range();
    if source.get(range.clone()).is_none() {
        return Err(query_failure("query-v-source-range"));
    }
    let syntax = if node.kind() == "source_file"
        && matches!(role, StructuralRole::Root | StructuralRole::Module)
    {
        range = 0..source.len();
        "v.file"
    } else if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        if let Some(proof) = cache.declaration(node, source, cancellation)? {
            match role {
                StructuralRole::Declaration => {
                    range = proof.range;
                    proof.syntax
                }
                StructuralRole::Definition => {
                    range = proof.name;
                    "v.name"
                }
                StructuralRole::Signature => {
                    range = proof.header;
                    "v.written_header"
                }
                _ => return Ok(None),
            }
        } else if role == StructuralRole::Signature && node.kind() != "identifier" {
            if matches!(node.kind(), "var_definition" | "global_var_definition")
                && clean(node, source)
                && node
                    .child_by_field_name("name")
                    .is_some_and(|name| source.get(name.byte_range()) == Some(b"_"))
            {
                return Ok(None);
            }
            if owner(node.kind()) {
                "v.unsupported_owner"
            } else if matches!(
                node.kind(),
                "compile_time_if_expression" | "compile_time_for_statement"
            ) {
                "v.compile_time"
            } else if node.kind() == "import_declaration" {
                "v.import_semantics"
            } else {
                "v.unsupported_construct"
            }
        } else {
            return Ok(None);
        }
    } else if role == StructuralRole::Reference && clean(node, source) {
        let Some(name) = reference_name(node).filter(|name| identifier(*name, source)) else {
            return Ok(None);
        };
        if binding_identifier(name, source) {
            return Ok(None);
        }
        range = name.byte_range();
        if node.kind() == "type_reference_expression" {
            "v.type"
        } else {
            "v.identifier"
        }
    } else if role == StructuralRole::Call && clean(node, source) {
        let Some(mut head) = node.child_by_field_name("name") else {
            return Ok(None);
        };
        if head.kind() == "selector_expression" {
            let Some(field) = head.child_by_field_name("field") else {
                return Ok(None);
            };
            head = field;
        }
        let Some(name) = reference_name(head).filter(|name| identifier(*name, source)) else {
            return Ok(None);
        };
        range = name.byte_range();
        "v.written_call"
    } else if role == StructuralRole::Import && clean(node, source) {
        let Some(name) =
            import_name(node, cancellation, cache)?.filter(|name| identifier(*name, source))
        else {
            return Ok(None);
        };
        range = name.byte_range();
        "v.written_import"
    } else if role == StructuralRole::Comment && clean(node, source) {
        "v.comment"
    } else if role == StructuralRole::StringLiteral && clean(node, source) {
        "v.string"
    } else {
        return Ok(None);
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: matches!(
            role,
            StructuralRole::Root
                | StructuralRole::Module
                | StructuralRole::Declaration
                | StructuralRole::Definition
                | StructuralRole::Signature
                | StructuralRole::Import
        ),
        native_depth: native_depth(node, cancellation)?,
    }))
}
