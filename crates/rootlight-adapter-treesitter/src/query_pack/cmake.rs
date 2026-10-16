//! Physical CMake owners and command sites remain distinct from evaluated state.
//! The grammar has no field names; direct-child proofs share the query work cap.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

#[derive(Clone)]
struct OwnerProof {
    name: Option<Range<usize>>,
    header: Range<usize>,
    syntax: &'static str,
    declaration: bool,
}

#[derive(Clone)]
struct ArgumentProof {
    position: usize,
    name: Option<Range<usize>>,
}

type Arguments = (usize, Option<Range<usize>>, bool);

/// Bounded request-local proofs avoid repeated argument-list scans.
pub(super) struct CaptureCache {
    owners: HashMap<usize, Option<OwnerProof>>,
    arguments: HashMap<usize, ArgumentProof>,
    headers: HashMap<usize, Arguments>,
    maximum_entries: usize,
    remaining_work: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            owners: HashMap::new(),
            arguments: HashMap::new(),
            headers: HashMap::new(),
            maximum_entries,
            remaining_work: maximum_entries,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn visit(&mut self, cancellation: &Cancellation) -> Result<bool, AdapterError> {
        cancellation.check()?;
        let Some(remaining) = self.remaining_work.checked_sub(1) else {
            self.exhausted = true;
            return Ok(false);
        };
        self.remaining_work = remaining;
        Ok(true)
    }

    fn admit(&mut self) -> bool {
        if self
            .owners
            .len()
            .saturating_add(self.arguments.len())
            .saturating_add(self.headers.len())
            >= self.maximum_entries
        {
            self.exhausted = true;
            false
        } else {
            true
        }
    }

    fn owner(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<OwnerProof>, AdapterError> {
        if let Some(proof) = self.owners.get(&node.id()) {
            return Ok(proof.clone());
        }
        if !self.admit() {
            return Ok(None);
        }
        let proof = owner_proof(node, source, cancellation, self)?;
        if !self.admit() {
            return Ok(None);
        }
        self.owners
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.owners.insert(node.id(), proof.clone());
        Ok(proof)
    }
}

fn clean(node: Node<'_>, source: &[u8]) -> bool {
    !node.is_missing() && !node.has_error() && source.get(node.byte_range()).is_some()
}

fn text<'a>(node: Node<'_>, source: &'a [u8]) -> Option<&'a str> {
    std::str::from_utf8(source.get(node.byte_range())?).ok()
}

fn direct<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut result = None;
    let mut cursor = node.walk();
    for child in node.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if child.kind() == kind && result.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(result)
}

fn literal(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<Range<usize>>, AdapterError> {
    if node.kind() != "argument" || !clean(node, source) {
        return Ok(None);
    }
    let Some(value) = node
        .named_child(0)
        .filter(|_| node.named_child_count() == 1)
    else {
        return Ok(None);
    };
    let range = match value.kind() {
        "unquoted_argument" => value.byte_range(),
        "quoted_argument" => {
            let Some(raw) = source.get(value.byte_range()) else {
                return Ok(None);
            };
            if !raw.starts_with(b"\"") || !raw.ends_with(b"\"") || raw.len() < 2 {
                return Ok(None);
            }
            value.start_byte() + 1..value.end_byte() - 1
        }
        "bracket_argument" => {
            let Some(content) = direct(value, "bracket_argument_content", cancellation, cache)?
            else {
                return Ok(None);
            };
            let Some(open) = direct(value, "bracket_argument_open", cancellation, cache)? else {
                return Ok(None);
            };
            let Some(close) = direct(value, "bracket_argument_close", cancellation, cache)? else {
                return Ok(None);
            };
            if open.end_byte() != content.start_byte() || content.end_byte() != close.start_byte() {
                return Ok(None);
            }
            let mut range = content.byte_range();
            let raw = source.get(range.clone()).unwrap_or_default();
            // CMake ignores only the immediate first newline of a bracket value.
            range.start += if raw.starts_with(b"\r\n") {
                2
            } else {
                usize::from(raw.starts_with(b"\n"))
            };
            range
        }
        _ => return Ok(None),
    };
    let Some(raw) = source.get(range.clone()) else {
        return Ok(None);
    };
    // Bracket arguments do not expand their contents; other forms must be literal.
    for chunk in raw.chunks(32) {
        if !cache.visit(cancellation)? {
            return Ok(None);
        }
        if value.kind() != "bracket_argument"
            && chunk
                .iter()
                .any(|byte| matches!(*byte, b'$' | b'\\' | b'@'))
        {
            return Ok(None);
        }
        // Unquoted list expansion can change both argument count and spelling.
        if value.kind() == "unquoted_argument" && chunk.contains(&b';') {
            return Ok(None);
        }
    }
    let Some(name) = std::str::from_utf8(raw).ok() else {
        return Ok(None);
    };
    Ok(
        (structural_captured_name_for_language("cmake", name, name.len()).as_deref() == Some(name))
            .then_some(range),
    )
}

fn arguments(
    header: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Arguments, AdapterError> {
    if let Some(arguments) = cache.headers.get(&header.id()) {
        return Ok(arguments.clone());
    }
    let Some(list) = direct(header, "argument_list", cancellation, cache)? else {
        return Ok((0, None, false));
    };
    let mut position = 0_usize;
    let mut first = None;
    let mut cache_keyword = None;
    let mut cursor = list.walk();
    for child in list.named_children(&mut cursor) {
        if !cache.visit(cancellation)? {
            break;
        }
        if child.kind() != "argument" {
            continue;
        }
        let name = literal(child, source, cancellation, cache)?;
        if position == 0 {
            first = name.clone();
        }
        if position > 0
            && name
                .as_ref()
                .and_then(|range| source.get(range.clone()))
                .is_some_and(|raw| raw == b"CACHE")
        {
            cache_keyword.get_or_insert(position);
        }
        if !cache.admit() {
            break;
        }
        cache
            .arguments
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        cache
            .arguments
            .insert(child.id(), ArgumentProof { position, name });
        position = position
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
    }
    let result = (
        position,
        first,
        cache_keyword.is_some_and(|keyword| position.saturating_sub(keyword) >= 3),
    );
    if cache.admit() {
        cache
            .headers
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        cache.headers.insert(header.id(), result.clone());
    }
    Ok(result)
}

fn owner_proof(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<OwnerProof>, AdapterError> {
    if !clean(node, source) {
        return Ok(None);
    }
    if matches!(node.kind(), "function_def" | "macro_def") {
        let (opening, ending, syntax) = if node.kind() == "function_def" {
            ("function_command", "endfunction_command", "cmake.function")
        } else {
            ("macro_command", "endmacro_command", "cmake.macro")
        };
        let Some(header) = direct(node, opening, cancellation, cache)? else {
            return Ok(None);
        };
        let Some(end) = direct(node, ending, cancellation, cache)? else {
            return Ok(None);
        };
        if !clean(header, source)
            || !clean(end, source)
            || header.start_byte() != node.start_byte()
            || end.end_byte() != node.end_byte()
        {
            return Ok(None);
        }
        let (_, name, _) = arguments(header, source, cancellation, cache)?;
        let name = name.filter(|range| {
            source
                .get(range.clone())
                .and_then(|raw| std::str::from_utf8(raw).ok())
                .is_some_and(|name| {
                    let mut bytes = name.bytes();
                    bytes
                        .next()
                        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
                        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
                })
        });
        return Ok(Some(OwnerProof {
            declaration: name.is_some(),
            name,
            header: header.byte_range(),
            syntax,
        }));
    }
    if node.kind() != "normal_command" {
        return Ok(None);
    }
    let Some(command) = direct(node, "identifier", cancellation, cache)? else {
        return Ok(None);
    };
    let Some(command) = text(command, source) else {
        return Ok(None);
    };
    // Ordinary command arguments cannot establish declaration ownership. Their
    // imports, calls and lexical captures have independent query paths.
    if ![
        "set",
        "option",
        "add_library",
        "add_executable",
        "unset",
        "cmake_language",
        "configure_file",
    ]
    .iter()
    .any(|known| command.eq_ignore_ascii_case(known))
    {
        return Ok(None);
    }
    let (count, name, cache_assignment) = arguments(node, source, cancellation, cache)?;
    let (syntax, declaration) = if command.eq_ignore_ascii_case("set") && count > 1 {
        let raw = name
            .as_ref()
            .and_then(|range| source.get(range.clone()))
            .unwrap_or_default();
        let syntax = if raw.starts_with(b"ENV{") && raw.ends_with(b"}") {
            "cmake.environment_assignment"
        } else if cache_assignment || raw.starts_with(b"CACHE{") && raw.ends_with(b"}") {
            "cmake.cache_assignment"
        } else {
            "cmake.normal_assignment"
        };
        (syntax, name.is_some())
    } else if command.eq_ignore_ascii_case("option") {
        if matches!(count, 2 | 3) {
            ("cmake.option", name.is_some())
        } else {
            ("cmake.unsupported_option_arguments", false)
        }
    } else if command.eq_ignore_ascii_case("add_library")
        || command.eq_ignore_ascii_case("add_executable")
    {
        ("cmake.target", name.is_some())
    } else if command.eq_ignore_ascii_case("unset") || command.eq_ignore_ascii_case("set") {
        ("cmake.unset_binding", false)
    } else if command.eq_ignore_ascii_case("cmake_language")
        || command.eq_ignore_ascii_case("configure_file")
    {
        ("cmake.dynamic_evaluation", false)
    } else {
        return Ok(None);
    };
    Ok(Some(OwnerProof {
        name,
        header: node.byte_range(),
        syntax,
        declaration,
    }))
}

fn physical_call_boundary(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    let start = node.start_byte();
    let after_bom = start == 3 && source.starts_with(b"\xef\xbb\xbf");
    let after_comment = node
        .parent()
        .filter(|parent| parent.kind() == "normal_command")
        .and_then(|parent| parent.prev_named_sibling())
        .is_some_and(|previous| {
            previous.kind() == "bracket_comment"
                && clean(previous, source)
                && previous.end_byte() == start
        });
    if start != 0
        && !after_bom
        && !after_comment
        && !start
            .checked_sub(1)
            .and_then(|index| source.get(index))
            .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b')')
    {
        return Ok(false);
    }
    let mut index = node.end_byte();
    while source.get(index).is_some_and(u8::is_ascii_whitespace) {
        if index.is_multiple_of(256) && !cache.visit(cancellation)? {
            return Ok(false);
        }
        index += 1;
    }
    cancellation.check()?;
    Ok(source.get(index) == Some(&b'('))
}

/// Projects only proven physical owners; unsupported written constructs retain gaps.
pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<Option<QueryCandidate>, AdapterError> {
    cancellation.check()?;
    let mut range = node.byte_range();
    if source.get(range.clone()).is_none() {
        return Err(query_failure("query-cmake-source-range"));
    }
    let syntax = if node.kind() == "source_file" {
        if !matches!(role, StructuralRole::Root | StructuralRole::Module) {
            return Ok(None);
        }
        range = 0..source.len();
        "cmake.file"
    } else if matches!(node.kind(), "function_def" | "macro_def" | "normal_command")
        && matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
        )
    {
        let proof = cache.owner(node, source, cancellation)?;
        match proof {
            Some(proof) if proof.declaration => match role {
                StructuralRole::Declaration => proof.syntax,
                StructuralRole::Definition => {
                    range = proof
                        .name
                        .ok_or_else(|| query_failure("query-cmake-name-proof"))?;
                    "cmake.name"
                }
                StructuralRole::Signature => {
                    range = proof.header;
                    "cmake.written_header"
                }
                _ => return Ok(None),
            },
            Some(proof) if role == StructuralRole::Signature => {
                if proof.name.is_none()
                    && matches!(
                        proof.syntax,
                        "cmake.function"
                            | "cmake.macro"
                            | "cmake.normal_assignment"
                            | "cmake.cache_assignment"
                            | "cmake.environment_assignment"
                            | "cmake.option"
                            | "cmake.target"
                    )
                {
                    "cmake.unproven_name"
                } else {
                    proof.syntax
                }
            }
            None if role == StructuralRole::Signature && node.kind() != "normal_command" => {
                "cmake.unsupported_owner"
            }
            _ => return Ok(None),
        }
    } else if node.kind() == "argument"
        && matches!(
            role,
            StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
        )
    {
        let Some(header) = node
            .parent()
            .filter(|parent| parent.kind() == "argument_list")
            .and_then(|list| list.parent())
            .filter(|header| matches!(header.kind(), "function_command" | "macro_command"))
        else {
            return Ok(None);
        };
        let Some(owner) = header.parent() else {
            return Ok(None);
        };
        if !cache
            .owner(owner, source, cancellation)?
            .is_some_and(|proof| proof.declaration)
        {
            return Ok(None);
        }
        let Some(proof) = cache
            .arguments
            .get(&node.id())
            .cloned()
            .filter(|proof| proof.position > 0)
        else {
            return Ok(None);
        };
        let Some(name) = proof.name else {
            return Ok(
                (role == StructuralRole::Signature).then_some(QueryCandidate {
                    start: range.start,
                    end: range.end,
                    role,
                    syntax: "cmake.unproven_parameter",
                    required: true,
                    native_depth: native_depth(node, cancellation)?,
                }),
            );
        };
        range = name;
        match role {
            StructuralRole::Declaration if header.kind() == "function_command" => "cmake.parameter",
            StructuralRole::Declaration => "cmake.macro_placeholder",
            StructuralRole::Definition => "cmake.name",
            StructuralRole::Signature => "cmake.written_parameter",
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Call
        && node.kind() == "identifier"
        && clean(node, source)
        && physical_call_boundary(node, source, cancellation, cache)?
        && node
            .parent()
            .is_some_and(|parent| parent.kind() == "normal_command" && !parent.is_missing())
    {
        // Recovery can retain a written call only at a physical token boundary;
        // a clean native identifier may be the suffix of an invalid template.
        if text(node, source).is_some_and(|name| name.contains('@')) {
            "cmake.template_command"
        } else {
            "cmake.command"
        }
    } else if role == StructuralRole::Reference
        && node.kind() == "variable_ref"
        && clean(node, source)
    {
        let Some(variable) = node
            .named_child(0)
            .filter(|_| node.named_child_count() == 1)
        else {
            return Ok(None);
        };
        let Some(name) = direct(variable, "variable", cancellation, cache)? else {
            return Ok(None);
        };
        if name.named_child_count() != 0 || !clean(name, source) {
            return Ok(None);
        }
        range = name.byte_range();
        match variable.kind() {
            "normal_var" => "cmake.normal_variable",
            "cache_var" => "cmake.cache_variable",
            "env_var" => "cmake.environment_variable",
            _ => return Ok(None),
        }
    } else if role == StructuralRole::Import
        && node.kind() == "normal_command"
        && clean(node, source)
    {
        let Some(command) = direct(node, "identifier", cancellation, cache)?
            .and_then(|command| text(command, source))
        else {
            return Ok(None);
        };
        if !["include", "add_subdirectory", "find_package"]
            .iter()
            .any(|known| command.eq_ignore_ascii_case(known))
        {
            return Ok(None);
        }
        let (_, name, _) = arguments(node, source, cancellation, cache)?;
        let Some(name) = name else {
            return Ok(None);
        };
        range = name;
        "cmake.written_import"
    } else if role == StructuralRole::Signature && node.kind() == "configure_placeholder" {
        "cmake.configure_placeholder"
    } else if role == StructuralRole::Signature
        && matches!(
            node.kind(),
            "if_condition" | "foreach_loop" | "while_loop" | "block_def"
        )
    {
        "cmake.control_flow"
    } else if role == StructuralRole::Comment
        && matches!(node.kind(), "line_comment" | "bracket_comment")
        && clean(node, source)
    {
        "cmake.comment"
    } else if role == StructuralRole::StringLiteral
        && matches!(node.kind(), "quoted_argument" | "bracket_argument")
        && clean(node, source)
    {
        "cmake.string"
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
        ),
        native_depth: native_depth(node, cancellation)?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GrammarFamily, query_pack::QueryPack};

    #[test]
    fn cmake_non_owner_arguments_cannot_spend_the_identity_proof_allowance() {
        let literal = format!("[=[{}]=]", "x".repeat(20_000));
        let source = format!(
            "# long-literal-marker\nmessage({literal})\nfunction(Written arg)\nendfunction()\n"
        );
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_cmake::LANGUAGE_CMAKE.into())
            .unwrap();
        let tree = parser.parse(&source, None).unwrap();
        assert!(!tree.root_node().has_error());
        let pack = QueryPack::compile(
            GrammarFamily::CMake,
            include_str!("../../queries/cmake.scm"),
        )
        .unwrap();
        // Native parsing and the query proof budget are independent here. This
        // small allowance exposes scans irrelevant to declaration ownership.
        let extraction = pack
            .extract(
                GrammarFamily::CMake,
                &tree,
                source.as_bytes(),
                128,
                128,
                &Cancellation::new(),
            )
            .unwrap();
        assert!(extraction.limit.is_none());
        for (role, selected) in [
            (StructuralRole::Definition, "Written"),
            (StructuralRole::Definition, "arg"),
            (StructuralRole::Call, "message"),
            (StructuralRole::Comment, "# long-literal-marker"),
            (StructuralRole::StringLiteral, literal.as_str()),
        ] {
            assert!(
                extraction
                    .candidates
                    .iter()
                    .any(|candidate| candidate.role == role
                        && source.get(candidate.start..candidate.end) == Some(selected)),
                "{role:?}"
            );
        }
    }
}
