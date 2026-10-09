//! PL/SQL captures retain direct written names and native header ownership.
//! Body recovery does not erase independently valid headers, but recovered
//! owners and parenthesized applications never establish target bindings.

use std::{collections::HashMap, ops::Range};

use rootlight_adapter_sdk::{AdapterError, SinkError, structural_captured_name_for_language};
use rootlight_cancel::Cancellation;
use tree_sitter::Node;

use super::{QueryCandidate, StructuralRole, native_depth, query_failure};

type OwnerRanges = (Range<usize>, Range<usize>);

/// Proofs are scoped to one immutable native tree and original-source query scan.
pub(super) struct CaptureCache {
    owners: HashMap<usize, Option<OwnerRanges>>,
    maximum_entries: usize,
    exhausted: bool,
}

impl CaptureCache {
    pub(super) fn new(maximum_entries: usize) -> Self {
        Self {
            owners: HashMap::new(),
            maximum_entries,
            exhausted: false,
        }
    }

    pub(super) const fn exhausted(&self) -> bool {
        self.exhausted
    }

    fn owner_ranges(
        &mut self,
        node: Node<'_>,
        source: &[u8],
        cancellation: &Cancellation,
    ) -> Result<Option<OwnerRanges>, AdapterError> {
        cancellation.check()?;
        if let Some(ranges) = self.owners.get(&node.id()) {
            return Ok(ranges.clone());
        }
        if self.owners.len() >= self.maximum_entries {
            self.exhausted = true;
            return Ok(None);
        }
        let ranges = owner_ranges(node, source, cancellation)?;
        self.owners
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        self.owners.insert(node.id(), ranges.clone());
        Ok(ranges)
    }
}

fn child<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for candidate in node.named_children(&mut cursor) {
        cancellation.check()?;
        if candidate.kind() == kind && !candidate.is_extra() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn first_child<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    for candidate in node.named_children(&mut cursor) {
        cancellation.check()?;
        if !candidate.is_extra() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn declaration_syntax(node: Node<'_>) -> Option<&'static str> {
    Some(match node.kind() {
        "create_package" => "plsql.package",
        "create_package_body" => "plsql.package_body",
        "create_procedure" | "procedure_definition" | "procedure_declaration" => "plsql.procedure",
        "create_function" | "function_definition" | "function_declaration" => "plsql.function",
        "create_trigger" => "plsql.trigger",
        "type_definition_record" => "plsql.record_type",
        "parameter_declaration_element" => "plsql.parameter",
        "item_declaration" => "plsql.variable",
        "field_definition" => "plsql.field",
        _ => return None,
    })
}

fn owner_kind(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "create_package"
            | "create_package_body"
            | "create_procedure"
            | "create_function"
            | "procedure_definition"
            | "procedure_declaration"
            | "function_definition"
            | "function_declaration"
            | "create_trigger"
            | "type_definition_record"
    )
}

fn conditional_kind(node: Node<'_>) -> bool {
    matches!(
        node.kind(),
        "conditional_compilation_statement"
            | "conditional_compilation_declaration"
            | "conditional_compilation_declaration_section"
            | "conditional_compilation_function_properties"
            | "conditional_compilation_cache_dependency"
    )
}

fn external_call_spec<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    if !matches!(node.kind(), "create_procedure" | "create_function") {
        return Ok(None);
    }
    let Some(specification) = child(node, "call_spec_ext", cancellation)? else {
        return Ok(None);
    };
    Ok(first_child(specification, cancellation)?
        .filter(|part| matches!(part.kind(), "java_declaration" | "c_declaration"))
        .map(|_| specification))
}

fn name_node<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let field = match node.kind() {
        "create_package" | "create_package_body" => "package_name",
        "create_procedure" | "procedure_definition" | "procedure_declaration" => "prc_name",
        "create_function" | "function_definition" | "function_declaration" => "fnc_name",
        "create_trigger" => "trigger_name",
        "type_definition_record" => "type_rec_name",
        "parameter_declaration_element" | "item_declaration" | "field_definition" => {
            return Ok(first_child(node, cancellation)?.filter(|name| name.kind() == "identifier"));
        }
        _ => return Ok(None),
    };
    Ok(node
        .child_by_field_name(field)
        .filter(|name| name.parent() == Some(node)))
}

fn written_name<'source>(node: Node<'_>, source: &'source [u8]) -> Option<&'source str> {
    if node.kind() != "identifier" || node.is_missing() || node.has_error() {
        return None;
    }
    let text = std::str::from_utf8(source.get(node.byte_range())?).ok()?;
    (structural_captured_name_for_language("plsql", text, text.len()).as_deref() == Some(text))
        .then_some(text)
}

fn token(node: Node<'_>, source: &[u8], written: &[u8]) -> bool {
    !node.is_missing()
        && !node.has_error()
        && source
            .get(node.byte_range())
            .is_some_and(|text| text.eq_ignore_ascii_case(written))
}

fn end_matches(
    end: Node<'_>,
    name: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if end.is_missing() || end.has_error() {
        return Ok(false);
    }
    let Some(keyword) = child(end, "kw_end", cancellation)? else {
        return Ok(false);
    };
    if !token(keyword, source, b"end") {
        return Ok(false);
    }
    if let Some(closing) = child(end, "identifier", cancellation)? {
        let (Some(closing), Some(opening)) =
            (written_name(closing, source), written_name(name, source))
        else {
            return Ok(false);
        };
        if opening.starts_with('"') || closing.starts_with('"') {
            if closing != opening {
                return Ok(false);
            }
        } else if !closing.eq_ignore_ascii_case(opening) {
            return Ok(false);
        }
    }
    Ok(source
        .get(end.byte_range())
        .is_some_and(|bytes| bytes.last() == Some(&b';')))
}

fn clean_prefix(
    node: Node<'_>,
    end: usize,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if node.is_missing()
        || end <= node.start_byte()
        || end > node.end_byte()
        || source.get(node.start_byte()..end).is_none()
    {
        return Ok(false);
    }
    let mut cursor = node.walk();
    for part in node.children(&mut cursor) {
        cancellation.check()?;
        if part.start_byte() >= end {
            break;
        }
        if part.is_missing() || part.has_error() || part.end_byte() > end {
            return Ok(false);
        }
    }
    Ok(true)
}

fn owner_ranges(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<OwnerRanges>, AdapterError> {
    if node.kind() == "plsql_block" {
        let Some(begin) = child(node, "kw_begin", cancellation)? else {
            return Ok(None);
        };
        let Some(end) = child(node, "end_obj", cancellation)? else {
            return Ok(None);
        };
        let Some(keyword) = child(end, "kw_end", cancellation)? else {
            return Ok(None);
        };
        return Ok((!end.is_missing()
            && !end.has_error()
            && token(begin, source, b"begin")
            && token(keyword, source, b"end")
            && begin.end_byte() < end.start_byte()
            && clean_prefix(node, begin.end_byte(), source, cancellation)?)
        .then_some((node.byte_range(), node.start_byte()..begin.end_byte())));
    }
    let Some(name) = name_node(node, cancellation)? else {
        return Ok(None);
    };
    if written_name(name, source).is_none() {
        return Ok(None);
    }
    if matches!(
        node.kind(),
        "procedure_declaration" | "function_declaration"
    ) {
        return Ok((!node.has_error()
            && clean_prefix(node, node.end_byte(), source, cancellation)?)
        .then_some((node.byte_range(), node.byte_range())));
    }
    if node.kind() == "type_definition_record" {
        let Some(first) = child(node, "field_definition", cancellation)? else {
            return Ok(None);
        };
        let end = first.start_byte();
        return Ok((clean_prefix(node, end, source, cancellation)?
            && source
                .get(node.byte_range())
                .is_some_and(|text| text.ends_with(b");")))
        .then_some((node.byte_range(), node.start_byte()..end)));
    }
    if node.kind() == "create_trigger" {
        let Some(body) = child(node, "plsql_block", cancellation)? else {
            return Ok(None);
        };
        let Some(end) = child(body, "end_obj", cancellation)? else {
            return Ok(None);
        };
        let Some(keyword) = child(end, "kw_end", cancellation)? else {
            return Ok(None);
        };
        return Ok(
            (clean_prefix(node, body.start_byte(), source, cancellation)?
                && token(keyword, source, b"end"))
            .then_some((node.byte_range(), node.start_byte()..body.start_byte())),
        );
    }
    let mut delimiter = None;
    let mut cursor = node.walk();
    for part in node.named_children(&mut cursor) {
        cancellation.check()?;
        if matches!(part.kind(), "kw_is" | "kw_as") {
            delimiter = Some(part);
            break;
        }
    }
    let Some(delimiter) = delimiter else {
        return Ok(None);
    };
    if name.end_byte() > delimiter.start_byte()
        || !token(
            delimiter,
            source,
            if delimiter.kind() == "kw_is" {
                b"is"
            } else {
                b"as"
            },
        )
        || !clean_prefix(node, delimiter.end_byte(), source, cancellation)?
    {
        return Ok(None);
    }
    if let Some(call_spec) = external_call_spec(node, cancellation)? {
        if node.has_error()
            || call_spec.is_missing()
            || call_spec.has_error()
            || call_spec.start_byte() < delimiter.end_byte()
        {
            return Ok(None);
        }
        let mut cursor = node.walk();
        for part in node.children(&mut cursor) {
            cancellation.check()?;
            if part.start_byte() < call_spec.end_byte() || part.is_extra() {
                continue;
            }
            return Ok((part.kind() == ";"
                && token(part, source, b";")
                && clean_prefix(node, part.end_byte(), source, cancellation)?)
            .then_some((
                node.start_byte()..part.end_byte(),
                node.start_byte()..delimiter.end_byte(),
            )));
        }
        return Ok(None);
    }
    let mut closing = child(node, "end_obj_named", cancellation)?;
    if closing.is_none_or(|end| end.is_missing() || end.has_error()) {
        // Routine END tokens belong to their direct body, not the CREATE node.
        closing = match child(node, "body", cancellation)? {
            Some(body) => child(body, "end_obj_named", cancellation)?,
            None => None,
        };
    }
    let Some(closing) = closing else {
        return Ok(None);
    };
    Ok((end_matches(closing, name, source, cancellation)?
        && delimiter.end_byte() < closing.start_byte())
    .then_some((
        node.start_byte()..closing.end_byte(),
        node.start_byte()..delimiter.end_byte(),
    )))
}

fn valid_owner_chain(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let target = if owner_kind(node) {
        let Some((range, _)) = cache.owner_ranges(node, source, cancellation)? else {
            return Ok(false);
        };
        range
    } else {
        node.byte_range()
    };
    let mut current = Some(node);
    while let Some(owner) = current {
        cancellation.check()?;
        if owner.kind() == "ERROR" || owner.is_missing() {
            return Ok(false);
        }
        if conditional_kind(owner) && owner.has_error() {
            return Ok(false);
        }
        if owner_kind(owner) {
            let Some((range, _)) = cache.owner_ranges(owner, source, cancellation)? else {
                return Ok(false);
            };
            if target.start < range.start || target.end > range.end {
                return Ok(false);
            }
        }
        if owner.kind() == "plsql_block" {
            let Some((range, _)) = cache.owner_ranges(owner, source, cancellation)? else {
                return Ok(false);
            };
            if target.start < range.start || target.end > range.end {
                return Ok(false);
            }
        }
        if matches!(owner.kind(), "create_type" | "create_type_body") {
            return Ok(false);
        }
        current = owner.parent();
    }
    Ok(true)
}

fn valid_definition(
    node: Node<'_>,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    let Some(name) = name_node(node, cancellation)? else {
        return Ok(false);
    };
    if written_name(name, source).is_none()
        || !valid_owner_chain(name, source, cancellation, cache)?
    {
        return Ok(false);
    }
    Ok(if owner_kind(node) {
        valid_owner_chain(node, source, cancellation, cache)?
    } else {
        !node.has_error()
    })
}

fn application_name<'tree>(
    node: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    Ok(first_child(node, cancellation)?
        .filter(|head| head.kind() == "referenced_element")
        .and_then(|head| head.child_by_field_name("ref_name")))
}

fn definition_identifier(
    node: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let Some(parent) = node.parent() else {
        return Ok(false);
    };
    if matches!(parent.kind(), "end_obj_named" | "parameter_name") {
        return Ok(true);
    }
    if name_node(parent, cancellation)? == Some(node) {
        return Ok(true);
    }
    if parent.kind() == "referenced_element"
        && let Some(application) = parent.parent().filter(|owner| owner.kind() == "ref_call")
    {
        return Ok(application_name(application, cancellation)? == Some(node));
    }
    Ok(false)
}

pub(super) fn retain_capture(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    if node.kind() == "source_file" {
        return Ok(true);
    }
    if role == StructuralRole::Expression {
        return Ok(conditional_kind(node)
            || !valid_definition(node, source, cancellation, cache)?
            || external_call_spec(node, cancellation)?.is_some()
            || matches!(
                node.kind(),
                "create_package"
                    | "create_package_body"
                    | "procedure_declaration"
                    | "function_declaration"
                    | "ref_call"
            ));
    }
    if matches!(
        role,
        StructuralRole::Declaration | StructuralRole::Definition | StructuralRole::Signature
    ) {
        return valid_definition(node, source, cancellation, cache);
    }
    if !valid_owner_chain(node, source, cancellation, cache)? {
        return Ok(false);
    }
    if role == StructuralRole::Scope {
        return Ok(!owner_kind(node) || cache.owner_ranges(node, source, cancellation)?.is_some());
    }
    if role == StructuralRole::Reference && node.kind() == "identifier" {
        return Ok(
            written_name(node, source).is_some() && !definition_identifier(node, cancellation)?
        );
    }
    if role == StructuralRole::Reference && node.kind() == "ref_call" {
        return Ok(application_name(node, cancellation)?
            .is_some_and(|head| written_name(head, source).is_some()));
    }
    Ok(!node.is_missing())
}

pub(super) fn candidate(
    node: Node<'_>,
    role: StructuralRole,
    source: &[u8],
    cancellation: &Cancellation,
    cache: &mut CaptureCache,
) -> Result<QueryCandidate, AdapterError> {
    if matches!(
        role,
        StructuralRole::Call | StructuralRole::CallName | StructuralRole::ScopedCall
    ) {
        return Err(query_failure("query-plsql-unproven-call"));
    }
    let syntax = match role {
        _ if node.kind() == "source_file" => "plsql.file",
        StructuralRole::Expression if conditional_kind(node) => "plsql.conditional_selection",
        StructuralRole::Expression
            if external_call_spec(node, cancellation)?.is_some()
                && valid_definition(node, source, cancellation, cache)? =>
        {
            "plsql.external_implementation"
        }
        StructuralRole::Expression if node.kind() == "ref_call" => "plsql.application_shape",
        StructuralRole::Expression if valid_definition(node, source, cancellation, cache)? => {
            "plsql.declaration_linkage"
        }
        StructuralRole::Expression => "plsql.unsupported_owner",
        StructuralRole::Reference if node.kind() == "ref_call" => "plsql.application",
        StructuralRole::Reference => "plsql.identifier",
        StructuralRole::Comment => "plsql.comment",
        StructuralRole::StringLiteral => "plsql.string",
        StructuralRole::Scope if node.kind() == "plsql_block" => "plsql.block",
        _ => declaration_syntax(node).ok_or_else(|| query_failure("query-plsql-declaration"))?,
    };
    let range = match role {
        StructuralRole::Expression if syntax == "plsql.external_implementation" => {
            cache
                .owner_ranges(node, source, cancellation)?
                .ok_or_else(|| query_failure("query-plsql-call-spec"))?
                .0
        }
        StructuralRole::Definition => name_node(node, cancellation)?
            .ok_or_else(|| query_failure("query-plsql-definition"))?
            .byte_range(),
        StructuralRole::Signature => {
            cache
                .owner_ranges(node, source, cancellation)?
                .ok_or_else(|| query_failure("query-plsql-header"))?
                .1
        }
        StructuralRole::Declaration | StructuralRole::Scope if owner_kind(node) => {
            cache
                .owner_ranges(node, source, cancellation)?
                .ok_or_else(|| query_failure("query-plsql-owner"))?
                .0
        }
        StructuralRole::Reference if node.kind() == "ref_call" => {
            application_name(node, cancellation)?
                .ok_or_else(|| query_failure("query-plsql-application"))?
                .byte_range()
        }
        _ => node.byte_range(),
    };
    let mut depth = native_depth(node, cancellation)?;
    if role == StructuralRole::Scope && node.kind() != "source_file" {
        depth = depth
            .checked_add(1)
            .ok_or_else(|| query_failure("query-plsql-depth"))?;
    }
    Ok(QueryCandidate {
        start: range.start,
        end: range.end,
        role,
        syntax,
        required: role.belongs_to_identity_closure(),
        native_depth: depth,
    })
}

#[cfg(test)]
mod tests {
    use rootlight_cancel::CancellationReason;

    use super::*;

    #[test]
    fn anonymous_block_proofs_share_the_scan_cache_and_entry_bound() {
        let source = b"begin null; end;\nbegin null; end;\n";
        let language = tree_sitter_plsql::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let cancellation = Cancellation::new();
        let mut cursor = tree.root_node().walk();
        let blocks: Vec<_> = tree
            .root_node()
            .named_children(&mut cursor)
            .map(|statement| {
                child(statement, "plsql_block", &cancellation)
                    .unwrap()
                    .unwrap()
            })
            .collect();
        assert_eq!(blocks.len(), 2);
        let mut cache = CaptureCache::new(1);
        let first = child(blocks[0], "kw_begin", &cancellation)
            .unwrap()
            .unwrap();
        for _ in 0..100 {
            assert!(valid_owner_chain(first, source, &cancellation, &mut cache).unwrap());
            assert_eq!(cache.owners.len(), 1);
            assert!(!cache.exhausted());
        }
        assert!(
            cache
                .owner_ranges(blocks[1], source, &cancellation)
                .unwrap()
                .is_none()
        );
        assert!(cache.exhausted());
        assert_eq!(cache.owners.len(), 1);
    }

    #[test]
    fn owner_proofs_reuse_within_one_scan_and_exhaust_the_existing_entry_bound() {
        let source = b"create or replace package FirstPackage as\nprocedure Adjust(Value in number);\nend FirstPackage;\n/\ncreate or replace package SecondPackage as\nprocedure Adjust(Value in number);\nend SecondPackage;\n/\n";
        let language = tree_sitter_plsql::LANGUAGE.into();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&language).unwrap();
        let tree = parser.parse(source, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );
        let cancellation = Cancellation::new();
        let mut cursor = tree.root_node().walk();
        let packages: Vec<_> = tree
            .root_node()
            .named_children(&mut cursor)
            .filter(|node| node.kind() == "create_package")
            .collect();
        assert_eq!(packages.len(), 2);
        let mut cache = CaptureCache::new(1);
        let first = cache
            .owner_ranges(packages[0], source, &cancellation)
            .unwrap();
        assert!(first.is_some());
        for _ in 0..100 {
            assert_eq!(
                cache
                    .owner_ranges(packages[0], source, &cancellation)
                    .unwrap(),
                first
            );
            assert_eq!(cache.owners.len(), 1);
            assert!(!cache.exhausted());
        }
        assert!(
            cache
                .owner_ranges(packages[1], source, &cancellation)
                .unwrap()
                .is_none()
        );
        assert!(cache.exhausted());
        assert_eq!(cache.owners.len(), 1);
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(
            cache
                .owner_ranges(packages[0], source, &cancellation)
                .is_err()
        );
    }
}
