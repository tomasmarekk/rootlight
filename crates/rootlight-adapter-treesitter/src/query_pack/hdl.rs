//! HDL capture proofs preserve physical owners and declaration-token boundaries.
//! Preprocessor branches remain authored syntax; no configuration or elaboration runs.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use crate::registry::GrammarFamily;

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
        if let Some(proof) = self.declarations.get(&node.id()) {
            return Ok(proof.clone());
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
        let mut parent = node.parent();
        let mut pending = Vec::new();
        let mut visible = true;
        while let Some(node) = parent {
            if let Some(found) = self.visibility.get(&node.id()) {
                visible = *found;
                break;
            }
            if !self.visit(cancellation)? {
                return Ok(false);
            }
            pending
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            pending.push(node.id());
            if matches!(
                node.kind(),
                "ERROR" | "macro_text" | "string_literal" | "one_line_comment" | "block_comment"
            ) || (owner(node.kind()).is_some()
                && self.declaration(node, source, cancellation)?.is_none())
            {
                visible = false;
                break;
            }
            parent = node.parent();
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
    if !clean(node, source) {
        return false;
    }
    let Some(text) = source.get(node.byte_range()) else {
        return false;
    };
    match node.kind() {
        "simple_identifier" => {
            text.first()
                .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
                && text
                    .iter()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
        }
        "escaped_identifier" => {
            text.first() == Some(&b'\\')
                && text.len() > 1
                && text[1..].iter().all(u8::is_ascii_graphic)
                && source
                    .get(node.end_byte())
                    .is_some_and(u8::is_ascii_whitespace)
        }
        _ => false,
    }
}

fn direct<'tree>(
    node: Node<'tree>,
    kinds: &[&str],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if kinds.contains(&child.kind()) {
            return Ok(Some(child));
        }
    }
    Ok(None)
}

fn first_identifier<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    direct(
        node,
        &["simple_identifier", "escaped_identifier"],
        cancellation,
        cache,
    )
}

fn owner(kind: &str) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        "module_declaration" => ("hdl.module", "endmodule"),
        "interface_declaration" => ("hdl.interface", "endinterface"),
        "program_declaration" => ("hdl.program", "endprogram"),
        "package_declaration" => ("hdl.package", "endpackage"),
        "class_declaration" | "interface_class_declaration" => ("hdl.class", "endclass"),
        "function_declaration" => ("hdl.function", "endfunction"),
        "task_declaration" => ("hdl.task", "endtask"),
        "udp_declaration" => ("hdl.primitive", "endprimitive"),
        "checker_declaration" => ("hdl.checker", "endchecker"),
        "property_declaration" => ("hdl.property", "endproperty"),
        "sequence_declaration" => ("hdl.sequence", "endsequence"),
        "clocking_declaration" => ("hdl.clocking", "endclocking"),
        _ => return None,
    })
}

fn owner_declaration(
    node: Node<'_>,
    source: &[u8],
    syntax: &'static str,
    closing: &str,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Declaration>, AdapterError> {
    let nested_header = direct(
        node,
        &[
            "module_ansi_header",
            "module_nonansi_header",
            "interface_ansi_header",
            "interface_nonansi_header",
            "program_ansi_header",
            "program_nonansi_header",
            "udp_ansi_declaration",
            "udp_nonansi_declaration",
            "function_body_declaration",
            "task_body_declaration",
        ],
        cancellation,
        cache,
    )?;
    let header = nested_header.unwrap_or(node);
    let name =
        header
            .child_by_field_name("name")
            .or(first_identifier(header, cancellation, cache)?);
    let Some(name) = name.filter(|name| identifier(*name, source)) else {
        return Ok(None);
    };
    let mut header_end = None;
    let mut cursor = header.walk();
    for child in header.children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if !clean(child, source) {
            return Ok(None);
        }
        if child.kind() == ";" {
            header_end = Some(child.end_byte());
            break;
        }
    }
    let Some(header_end) = header_end else {
        return Ok(None);
    };
    let closer_owner = if matches!(
        header.kind(),
        "function_body_declaration" | "task_body_declaration"
    ) {
        header
    } else {
        node
    };
    let mut closed = false;
    let mut cursor = closer_owner.walk();
    for child in closer_owner.children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if child.kind() == closing && clean(child, source) {
            closed = true;
        }
    }
    let extern_header =
        node.child(0).is_some_and(|child| child.kind() == "extern") && clean(node, source);
    if !closed && !extern_header {
        return Ok(None);
    }
    Ok(Some(Declaration {
        range: node.byte_range(),
        name: name.byte_range(),
        header: node.start_byte()..header_end,
        syntax,
    }))
}

fn declaration(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Declaration>, AdapterError> {
    if let Some((syntax, closing)) = owner(node.kind()) {
        return owner_declaration(node, source, syntax, closing, cancellation, cache);
    }
    if !clean(node, source) {
        return Ok(None);
    }
    let mut range = node.byte_range();
    let mut header = range.clone();
    let (name, syntax) = match node.kind() {
        "text_macro_definition" => {
            let Some(head) = direct(node, &["text_macro_name"], cancellation, cache)? else {
                return Ok(None);
            };
            header.end = head.end_byte();
            (first_identifier(head, cancellation, cache)?, "hdl.macro")
        }
        "generate_block" => {
            let Some(name) = node.child_by_field_name("name") else {
                return Ok(None);
            };
            // An optional end label alone does not declare a named generate block.
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                if !cache.visit(cancellation)? {
                    return Ok(None);
                }
                if child.id() == name.id() {
                    break;
                }
                if child.kind() != "begin" && child.kind() != ":" {
                    return Ok(None);
                }
            }
            header.end = name.end_byte();
            (Some(name), "hdl.generate")
        }
        "modport_item" => (first_identifier(node, cancellation, cache)?, "hdl.modport"),
        "function_prototype" => (node.child_by_field_name("name"), "hdl.function"),
        "task_prototype" => (node.child_by_field_name("name"), "hdl.task"),
        "let_declaration" => (node.child_by_field_name("name"), "hdl.let"),
        "simple_identifier" | "escaped_identifier" => {
            let Some(parent) = node.parent().filter(|parent| clean(*parent, source)) else {
                return Ok(None);
            };
            let first = || {
                parent
                    .named_child(0)
                    .is_some_and(|first| first.id() == node.id())
            };
            let field = |name| {
                parent
                    .child_by_field_name(name)
                    .is_some_and(|found| found.id() == node.id())
            };
            let syntax = match parent.kind() {
                "ansi_port_declaration" if field("port_name") => "hdl.port",
                "tf_port_item" if field("name") => "hdl.argument",
                "type_declaration" if field("type_name") => "hdl.type",
                "variable_decl_assignment" if field("name") => "hdl.variable",
                "type_assignment" if field("name") => "hdl.type_parameter",
                "name_of_instance" if field("instance_name") => "hdl.instance",
                "param_assignment" | "specparam_assignment" if first() => "hdl.parameter",
                "net_decl_assignment" if first() => "hdl.net",
                "enum_name_declaration" if first() => "hdl.enumerator",
                "list_of_genvar_identifiers" => "hdl.genvar",
                "list_of_port_identifiers"
                | "list_of_variable_identifiers"
                | "list_of_variable_port_identifiers"
                | "list_of_interface_identifiers"
                | "list_of_udp_port_identifiers" => "hdl.port",
                "list_of_tf_variable_identifiers" | "formal_argument" => "hdl.argument",
                "udp_output_declaration" | "udp_reg_declaration" if first() => "hdl.port",
                "for_variable_declaration" => "hdl.variable",
                "genvar_initialization"
                    if first()
                        && parent
                            .child(0)
                            .is_some_and(|child| child.kind() == "genvar") =>
                {
                    "hdl.genvar"
                }
                _ => return Ok(None),
            };
            if syntax == "hdl.port" {
                let mut ancestor = parent.parent();
                let mut written_owner = false;
                while let Some(outer) = ancestor {
                    if !cache.visit(cancellation)? {
                        return Ok(None);
                    }
                    if matches!(
                        outer.kind(),
                        "module_declaration"
                            | "interface_declaration"
                            | "program_declaration"
                            | "udp_declaration"
                    ) {
                        written_owner = cache.declaration(outer, source, cancellation)?.is_some();
                        break;
                    }
                    ancestor = outer.parent();
                }
                if !written_owner {
                    return Ok(None);
                }
            }
            if !parent.kind().starts_with("list_") && parent.kind() != "for_variable_declaration" {
                range = parent.byte_range();
                header = range.clone();
            }
            (Some(node), syntax)
        }
        _ => return Ok(None),
    };
    Ok(name
        .filter(|name| identifier(*name, source))
        .map(|name| Declaration {
            range,
            header,
            name: name.byte_range(),
            syntax,
        }))
}

fn header_identifier(node: Node<'_>) -> bool {
    node.parent().is_some_and(|parent| {
        owner(parent.kind()).is_some()
            || matches!(
                parent.kind(),
                "module_ansi_header"
                    | "module_nonansi_header"
                    | "interface_ansi_header"
                    | "interface_nonansi_header"
                    | "program_ansi_header"
                    | "program_nonansi_header"
                    | "udp_ansi_declaration"
                    | "udp_nonansi_declaration"
                    | "function_body_declaration"
                    | "task_body_declaration"
                    | "text_macro_name"
                    | "generate_block"
                    | "modport_item"
                    | "function_prototype"
                    | "task_prototype"
            )
    })
}

fn gap_syntax(kind: &str) -> Option<&'static str> {
    if matches!(
        kind,
        "text_macro_definition"
            | "text_macro_name"
            | "macro_text"
            | "text_macro_usage"
            | "conditional_compilation_directive"
    ) {
        Some("hdl.preprocessor")
    } else if matches!(
        kind,
        "loop_generate_construct" | "conditional_generate_construct"
    ) {
        Some("hdl.generate_semantics")
    } else if matches!(
        kind,
        "include_compiler_directive"
            | "include_statement"
            | "package_import_declaration"
            | "package_export_declaration"
            | "config_declaration"
            | "library_declaration"
            | "bind_directive"
    ) {
        Some("hdl.import_semantics")
    } else if matches!(
        kind,
        "covergroup_declaration"
            | "constraint_declaration"
            | "extern_constraint_declaration"
            | "class_constructor_declaration"
            | "nettype_declaration"
    ) {
        Some("hdl.unsupported_declaration")
    } else {
        None
    }
}

fn call_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if node.kind() == "method_call_body" {
        return Ok(node.child_by_field_name("name"));
    }
    if node.kind() != "tf_call" {
        return Ok(None);
    }
    let head = direct(
        node,
        &[
            "hierarchical_identifier",
            "simple_identifier",
            "escaped_identifier",
        ],
        cancellation,
        cache,
    )?;
    let Some(head) = head else {
        return Ok(None);
    };
    if head.kind() != "hierarchical_identifier" {
        return Ok(Some(head));
    }
    let mut last = None;
    let mut cursor = head.walk();
    for child in head.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if matches!(child.kind(), "simple_identifier" | "escaped_identifier") {
            last = Some(child);
        }
    }
    Ok(last)
}

fn dialect_syntax(family: GrammarFamily, label: &str) -> Result<&'static str, AdapterError> {
    let systemverilog = family == GrammarFamily::SystemVerilog;
    match label {
        "hdl.argument" => Ok(if systemverilog {
            "systemverilog.argument"
        } else {
            "verilog.argument"
        }),
        "hdl.checker" => Ok(if systemverilog {
            "systemverilog.checker"
        } else {
            "verilog.checker"
        }),
        "hdl.class" => Ok(if systemverilog {
            "systemverilog.class"
        } else {
            "verilog.class"
        }),
        "hdl.clocking" => Ok(if systemverilog {
            "systemverilog.clocking"
        } else {
            "verilog.clocking"
        }),
        "hdl.comment" => Ok(if systemverilog {
            "systemverilog.comment"
        } else {
            "verilog.comment"
        }),
        "hdl.enumerator" => Ok(if systemverilog {
            "systemverilog.enumerator"
        } else {
            "verilog.enumerator"
        }),
        "hdl.file" => Ok(if systemverilog {
            "systemverilog.file"
        } else {
            "verilog.file"
        }),
        "hdl.function" => Ok(if systemverilog {
            "systemverilog.function"
        } else {
            "verilog.function"
        }),
        "hdl.generate" => Ok(if systemverilog {
            "systemverilog.generate"
        } else {
            "verilog.generate"
        }),
        "hdl.generate_semantics" => Ok(if systemverilog {
            "systemverilog.generate_semantics"
        } else {
            "verilog.generate_semantics"
        }),
        "hdl.genvar" => Ok(if systemverilog {
            "systemverilog.genvar"
        } else {
            "verilog.genvar"
        }),
        "hdl.identifier" => Ok(if systemverilog {
            "systemverilog.identifier"
        } else {
            "verilog.identifier"
        }),
        "hdl.import_semantics" => Ok(if systemverilog {
            "systemverilog.import_semantics"
        } else {
            "verilog.import_semantics"
        }),
        "hdl.instance" => Ok(if systemverilog {
            "systemverilog.instance"
        } else {
            "verilog.instance"
        }),
        "hdl.interface" => Ok(if systemverilog {
            "systemverilog.interface"
        } else {
            "verilog.interface"
        }),
        "hdl.let" => Ok(if systemverilog {
            "systemverilog.let"
        } else {
            "verilog.let"
        }),
        "hdl.macro" => Ok(if systemverilog {
            "systemverilog.macro"
        } else {
            "verilog.macro"
        }),
        "hdl.modport" => Ok(if systemverilog {
            "systemverilog.modport"
        } else {
            "verilog.modport"
        }),
        "hdl.module" => Ok(if systemverilog {
            "systemverilog.module"
        } else {
            "verilog.module"
        }),
        "hdl.name" => Ok(if systemverilog {
            "systemverilog.name"
        } else {
            "verilog.name"
        }),
        "hdl.net" => Ok(if systemverilog {
            "systemverilog.net"
        } else {
            "verilog.net"
        }),
        "hdl.package" => Ok(if systemverilog {
            "systemverilog.package"
        } else {
            "verilog.package"
        }),
        "hdl.parameter" => Ok(if systemverilog {
            "systemverilog.parameter"
        } else {
            "verilog.parameter"
        }),
        "hdl.port" => Ok(if systemverilog {
            "systemverilog.port"
        } else {
            "verilog.port"
        }),
        "hdl.preprocessor" => Ok(if systemverilog {
            "systemverilog.preprocessor"
        } else {
            "verilog.preprocessor"
        }),
        "hdl.primitive" => Ok(if systemverilog {
            "systemverilog.primitive"
        } else {
            "verilog.primitive"
        }),
        "hdl.program" => Ok(if systemverilog {
            "systemverilog.program"
        } else {
            "verilog.program"
        }),
        "hdl.property" => Ok(if systemverilog {
            "systemverilog.property"
        } else {
            "verilog.property"
        }),
        "hdl.sequence" => Ok(if systemverilog {
            "systemverilog.sequence"
        } else {
            "verilog.sequence"
        }),
        "hdl.string" => Ok(if systemverilog {
            "systemverilog.string"
        } else {
            "verilog.string"
        }),
        "hdl.system_call" => Ok(if systemverilog {
            "systemverilog.system_call"
        } else {
            "verilog.system_call"
        }),
        "hdl.task" => Ok(if systemverilog {
            "systemverilog.task"
        } else {
            "verilog.task"
        }),
        "hdl.type" => Ok(if systemverilog {
            "systemverilog.type"
        } else {
            "verilog.type"
        }),
        "hdl.type_parameter" => Ok(if systemverilog {
            "systemverilog.type_parameter"
        } else {
            "verilog.type_parameter"
        }),
        "hdl.unsupported_declaration" => Ok(if systemverilog {
            "systemverilog.unsupported_declaration"
        } else {
            "verilog.unsupported_declaration"
        }),
        "hdl.unsupported_owner" => Ok(if systemverilog {
            "systemverilog.unsupported_owner"
        } else {
            "verilog.unsupported_owner"
        }),
        "hdl.variable" => Ok(if systemverilog {
            "systemverilog.variable"
        } else {
            "verilog.variable"
        }),
        "hdl.written_call" => Ok(if systemverilog {
            "systemverilog.written_call"
        } else {
            "verilog.written_call"
        }),
        "hdl.written_header" => Ok(if systemverilog {
            "systemverilog.written_header"
        } else {
            "verilog.written_header"
        }),
        _ => Err(query_failure("query-hdl-unknown-label")),
    }
}

pub(super) fn candidate(
    family: GrammarFamily,
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
        return Err(query_failure("query-hdl-source-range"));
    }
    let syntax = if node.kind() == "source_file"
        && matches!(role, StructuralRole::Root | StructuralRole::Module)
    {
        range = 0..source.len();
        "hdl.file"
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
                    "hdl.name"
                }
                StructuralRole::Signature => {
                    range = proof.header;
                    "hdl.written_header"
                }
                _ => return Ok(None),
            }
        } else if role == StructuralRole::Signature && owner(node.kind()).is_some() {
            "hdl.unsupported_owner"
        } else if role == StructuralRole::Signature {
            let Some(syntax) = gap_syntax(node.kind()) else {
                return Ok(None);
            };
            syntax
        } else {
            return Ok(None);
        }
    } else if role == StructuralRole::Reference && identifier(node, source) {
        if cache.declaration(node, source, cancellation)?.is_some() || header_identifier(node) {
            return Ok(None);
        }
        if node
            .parent()
            .is_some_and(|parent| parent.kind() == "text_macro_usage")
        {
            "hdl.macro"
        } else if node.parent().is_some_and(|parent| {
            parent
                .child_by_field_name("instance_type")
                .is_some_and(|name| name.id() == node.id())
        }) {
            "hdl.type"
        } else {
            "hdl.identifier"
        }
    } else if role == StructuralRole::Call && clean(node, source) {
        if node.kind() == "system_tf_identifier"
            && node
                .parent()
                .is_some_and(|parent| parent.kind() == "system_tf_call")
        {
            "hdl.system_call"
        } else {
            let name = call_name(node, cancellation, cache)?;
            let Some(name) = name.filter(|name| identifier(*name, source)) else {
                return Ok(None);
            };
            range = name.byte_range();
            "hdl.written_call"
        }
    } else if role == StructuralRole::Comment
        && clean(node, source)
        && matches!(node.kind(), "one_line_comment" | "block_comment")
    {
        "hdl.comment"
    } else if role == StructuralRole::StringLiteral
        && clean(node, source)
        && node.kind() == "string_literal"
    {
        "hdl.string"
    } else {
        return Ok(None);
    };
    Ok(Some(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax: dialect_syntax(family, syntax)?,
        required: matches!(
            role,
            StructuralRole::Root
                | StructuralRole::Module
                | StructuralRole::Declaration
                | StructuralRole::Definition
                | StructuralRole::Signature
        ),
        native_depth: native_depth(node, cancellation)?,
    }))
}
