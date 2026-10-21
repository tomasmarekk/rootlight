//! Native compound-declaration evidence for bounded shared-suffix routing.
//! The caller owns foreign-language conflict checks; syntax witnesses retain
//! original snapshot coordinates without publishing parser facts or identities.

use super::*;

const MAX_ADMISSION_SAMPLE_BYTES: usize = 8 * 1024;

/// Characteristic source syntax proved by the bounded F# parser view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FSharpAdmissionKind {
    /// A named binding with authored argument patterns and an owned body.
    NamedFunction,
    /// A value binding whose owned body is an authored lambda.
    BoundLambda,
    /// A complete record declaration with authored field types.
    RecordType,
    /// A complete discriminated union with characteristic case syntax.
    UnionType,
    /// A complete explicit interface declaration.
    InterfaceType,
    /// A complete object declaration with constructor or member syntax.
    ObjectType,
}

/// Checked compound syntax evidence, not universal language identification.
///
/// Both references retain the full original snapshot hash and generation.
/// Construction is private so parser-only coordinates cannot escape as evidence.
/// A recursive declaration reference covers its complete binding group; the kind
/// identifies the first characteristic member after every member is checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FSharpAdmissionWitness {
    kind: FSharpAdmissionKind,
    declaration: SourceRef,
    sample: SourceRef,
}

impl FSharpAdmissionWitness {
    /// Returns the characteristic declaration category without a semantic claim.
    #[must_use]
    pub const fn kind(&self) -> FSharpAdmissionKind {
        self.kind
    }

    /// Returns the complete declaration's exact original-byte source reference.
    #[must_use]
    pub const fn declaration(&self) -> &SourceRef {
        &self.declaration
    }

    /// Returns the exact original-byte prefix inspected by the native parser.
    #[must_use]
    pub const fn sample(&self) -> &SourceRef {
        &self.sample
    }
}

impl TreeSitterProvider {
    /// Proves bounded compound F# syntax without extracting structural facts.
    ///
    /// The request must bind the complete immutable snapshot, request `fsharp`,
    /// and have no included ranges. Original and projected parser input are each
    /// capped at 8192 bytes. A truncated final line cannot establish a witness;
    /// true EOF does not acquire a synthetic delimiter. Atomic bindings and bare
    /// type abbreviations alone return `None`. Witnesses may belong to direct
    /// root module/namespace containers or complete recursive binding groups;
    /// nested modules and unowned expression prefixes do not supply evidence.
    ///
    /// Shared-suffix callers must first apply their source-authored Forth and
    /// other conflicting-language guards. A clean grammar parse is not universal
    /// language identity, and this witness supplies no entity or semantic tier.
    ///
    /// # Errors
    /// Returns typed request, encoding, memory, pool, cancellation or native-work
    /// errors. Node/depth exhaustion is a provider limit, not negative evidence.
    #[allow(clippy::too_many_lines)]
    pub fn probe_fsharp_admission(
        &self,
        request: &ParseRequest<'_>,
        cancellation: &Cancellation,
    ) -> Result<Option<FSharpAdmissionWitness>, AdapterError> {
        cancellation.check()?;
        if request.language().as_str() != "fsharp" {
            return Err(RequestError::UnsupportedLanguage.into());
        }
        if !request.included_ranges().is_empty() {
            return Err(RequestError::EmbeddedRangesUnsupported.into());
        }
        let settings = self.config.default_settings();
        self.admit_parse_input(request, None, &[], settings, cancellation)?;
        let maximum = MAX_ADMISSION_SAMPLE_BYTES
            .min(self.config.max_source_bytes())
            .min(request.limits().max_source_bytes());
        let view = NativeSourceView::bounded_prefix(
            request.source().bytes(),
            request.encoding().as_str(),
            maximum,
            request.limits().max_reported_memory_bytes(),
            cancellation,
        )?;
        let tree = self.parse_native_tree(
            request,
            &language_for(GrammarFamily::FSharp),
            None,
            settings,
            &view,
            None,
            cancellation,
        )?;
        let traversal = traverse_syntax(
            &tree,
            request.limits().max_syntax_nodes(),
            request.limits().max_syntax_depth(),
            None,
            None,
            cancellation,
        )?;
        if traversal.limited_nodes {
            return Err(admission_limit(
                ResourceKind::SyntaxNodes,
                request.limits().max_syntax_nodes(),
            ));
        }
        if traversal.limited_depth {
            return Err(admission_limit(
                ResourceKind::SyntaxDepth,
                request.limits().max_syntax_depth(),
            ));
        }
        let accounted = traversal
            .processed_nodes
            .checked_mul(LOGICAL_TREE_NODE_BYTES)
            .and_then(|bytes| bytes.checked_add(view.owned_bytes()))
            .ok_or_else(|| provider_failure("fsharp-admission-memory-accounting"))?;
        require_provider_limit(
            ResourceKind::ReportedMemoryBytes,
            accounted,
            request.limits().max_reported_memory_bytes(),
        )?;
        let root = tree.root_node();
        if traversal.error_span.is_some() || !valid(root) || root.kind() != "file" {
            return Ok(None);
        }
        let complete_end = if view.physical_bytes() < request.source().bytes().len() {
            view.as_str().rfind('\n').map_or(0, |end| end + 1)
        } else {
            view.as_bytes().len()
        };
        let DeclarationEvidence::Witness(declaration, kind) =
            declaration_sequence(root, 0, complete_end, cancellation)?
        else {
            cancellation.check()?;
            return Ok(None);
        };
        let start = view.to_physical_offset(declaration.start_byte())?;
        let end = view.to_physical_offset(declaration.end_byte())?;
        let declaration = source_ref_for_span(request.source().source_ref(), start, end)
            .ok_or_else(|| provider_failure("fsharp-admission-source-span"))?;
        let sample = source_ref_for_span(request.source().source_ref(), 0, view.physical_bytes())
            .ok_or_else(|| provider_failure("fsharp-admission-source-span"))?;
        cancellation.check()?;
        Ok(Some(FSharpAdmissionWitness {
            kind,
            declaration,
            sample,
        }))
    }
}

fn admission_limit(resource: ResourceKind, limit: usize) -> AdapterError {
    RequestError::ProviderLimit {
        resource,
        observed: limit.saturating_add(1),
        limit,
    }
    .into()
}

fn valid(node: Node<'_>) -> bool {
    !node.is_error()
        && !node.is_missing()
        && !node.has_error()
        && node.start_byte() < node.end_byte()
}

fn direct_kind<'tree>(
    node: Node<'tree>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<Node<'tree>>, AdapterError> {
    let mut cursor = node.walk();
    let mut selected = None;
    for child in node.children(&mut cursor) {
        cancellation.check()?;
        if !child.is_extra() && child.kind() == kind && selected.replace(child).is_some() {
            return Ok(None);
        }
    }
    Ok(selected.filter(|node| valid(*node)))
}

enum DeclarationEvidence<'tree> {
    Unowned,
    NoWitness,
    Witness(Node<'tree>, FSharpAdmissionKind),
}

fn declaration_sequence<'tree>(
    owner: Node<'tree>,
    header_end: usize,
    complete_end: usize,
    cancellation: &Cancellation,
) -> Result<DeclarationEvidence<'tree>, AdapterError> {
    let mut cursor = owner.walk();
    for declaration in owner.named_children(&mut cursor) {
        cancellation.check()?;
        if declaration.is_extra() || declaration.end_byte() <= header_end {
            continue;
        }
        if !valid(declaration) {
            return Ok(DeclarationEvidence::Unowned);
        }
        let evidence = match declaration.kind() {
            "named_module" | "namespace" if owner.kind() == "file" => {
                let Some(header_end) = root_container_header(declaration, cancellation)? else {
                    return Ok(DeclarationEvidence::Unowned);
                };
                if header_end > complete_end {
                    return Ok(DeclarationEvidence::Unowned);
                }
                // A container can end on the sampled partial line while an
                // earlier direct declaration is complete. Never descend through
                // nested modules or skip an unowned prefix in another namespace.
                declaration_sequence(declaration, header_end, complete_end, cancellation)?
            }
            _ if declaration.end_byte() > complete_end => DeclarationEvidence::Unowned,
            // These complete declarations own their contents, but only a later
            // characteristic declaration can establish language evidence.
            "import_decl" | "exception_definition" => DeclarationEvidence::NoWitness,
            "declaration_expression" => {
                let Some(group) = direct_kind(declaration, "function_or_value_defn", cancellation)?
                else {
                    return Ok(DeclarationEvidence::Unowned);
                };
                if declaration.child_by_field_name("in").is_some() {
                    return Ok(DeclarationEvidence::Unowned);
                }
                match binding_kind(group, cancellation)? {
                    DeclarationEvidence::Witness(_, kind) => {
                        DeclarationEvidence::Witness(declaration, kind)
                    }
                    evidence => evidence,
                }
            }
            "type_definition" => type_kind(declaration, cancellation)?
                .map_or(DeclarationEvidence::NoWitness, |kind| {
                    DeclarationEvidence::Witness(declaration, kind)
                }),
            _ => DeclarationEvidence::Unowned,
        };
        match evidence {
            DeclarationEvidence::NoWitness => {}
            evidence => return Ok(evidence),
        }
    }
    Ok(DeclarationEvidence::NoWitness)
}

fn root_container_header(
    container: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    if container
        .parent()
        .is_none_or(|parent| parent.kind() != "file")
    {
        return Ok(None);
    }
    let keyword = if container.kind() == "named_module" {
        "module"
    } else {
        "namespace"
    };
    let Some(keyword) = direct_kind(container, keyword, cancellation)? else {
        return Ok(None);
    };
    let mut cursor = container.walk();
    let mut name = None;
    for child in container.children_by_field_name("name", &mut cursor) {
        cancellation.check()?;
        if child.is_named()
            && (!valid(child) || child.kind() != "long_identifier" || name.replace(child).is_some())
        {
            return Ok(None);
        }
    }
    let global = if container.kind() == "namespace" {
        direct_kind(container, "global", cancellation)?
    } else {
        None
    };
    let name = match (name, global) {
        (Some(name), None) | (None, Some(name)) => name,
        _ => return Ok(None),
    };
    Ok((keyword.end_byte() <= name.start_byte()).then_some(name.end_byte()))
}

fn binding_kind<'tree>(
    group: Node<'tree>,
    cancellation: &Cancellation,
) -> Result<DeclarationEvidence<'tree>, AdapterError> {
    let mut cursor = group.walk();
    if !cursor.goto_first_child() {
        return Ok(DeclarationEvidence::Unowned);
    }
    let mut let_token: Option<Node<'tree>> = None;
    let mut rec_token: Option<Node<'tree>> = None;
    let mut connector: Option<Node<'tree>> = None;
    let mut head: Option<Node<'tree>> = None;
    let mut equals: Option<Node<'tree>> = None;
    let mut body: Option<Node<'tree>> = None;
    let mut kind = None;
    let mut previous_end = group.start_byte();
    // Native `body` fields pair with their preceding head and '='. A strong
    // first member cannot hide a missing or unowned later recursive member.
    loop {
        cancellation.check()?;
        let child = cursor.node();
        if !child.is_extra() {
            if !valid(child) || previous_end > child.start_byte() {
                return Ok(DeclarationEvidence::Unowned);
            }
            if cursor.field_name() == Some("body") {
                let Some((head, equals)) = head.zip(equals) else {
                    return Ok(DeclarationEvidence::Unowned);
                };
                if body.replace(child).is_some()
                    || head.end_byte() > equals.start_byte()
                    || equals.end_byte() > child.start_byte()
                {
                    return Ok(DeclarationEvidence::Unowned);
                }
                kind = kind.or(member_kind(head, child, cancellation)?);
            } else {
                match child.kind() {
                    "let" if child.start_byte() == group.start_byte() => {
                        if let_token.replace(child).is_some() {
                            return Ok(DeclarationEvidence::Unowned);
                        }
                    }
                    "rec" if let_token.is_some() && head.is_none() && connector.is_none() => {
                        if rec_token.replace(child).is_some() {
                            return Ok(DeclarationEvidence::Unowned);
                        }
                    }
                    "and" if rec_token.is_some() && body.is_some() => {
                        connector = Some(child);
                        head = None;
                        equals = None;
                        body = None;
                    }
                    "function_declaration_left" | "value_declaration_left" => {
                        let Some(prefix) = connector.take().or(rec_token).or(let_token) else {
                            return Ok(DeclarationEvidence::Unowned);
                        };
                        if head.replace(child).is_some()
                            || equals.is_some()
                            || body.is_some()
                            || prefix.end_byte() > child.start_byte()
                        {
                            return Ok(DeclarationEvidence::Unowned);
                        }
                    }
                    "=" if head.is_some() && body.is_none() => {
                        if equals.replace(child).is_some() {
                            return Ok(DeclarationEvidence::Unowned);
                        }
                    }
                    _ if head.is_some()
                        && equals.is_none()
                        && body.is_none()
                        && (child.is_named() || child.kind() == ":") => {}
                    _ => return Ok(DeclarationEvidence::Unowned),
                }
            }
            previous_end = child.end_byte();
        }
        if !cursor.goto_next_sibling() {
            break;
        }
    }
    if let_token.is_none()
        || head.is_none()
        || equals.is_none()
        || body.is_none()
        || connector.is_some()
    {
        return Ok(DeclarationEvidence::Unowned);
    }
    Ok(kind.map_or(DeclarationEvidence::NoWitness, |kind| {
        DeclarationEvidence::Witness(group, kind)
    }))
}

fn member_kind(
    head: Node<'_>,
    body: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<FSharpAdmissionKind>, AdapterError> {
    if head.kind() == "function_declaration_left"
        && let Some(arguments) = direct_kind(head, "argument_patterns", cancellation)?
        && arguments.named_child_count() > 0
    {
        return Ok(Some(FSharpAdmissionKind::NamedFunction));
    }
    if head.kind() == "value_declaration_left"
        && body.kind() == "fun_expression"
        && direct_kind(body, "fun", cancellation)?.is_some()
        && direct_kind(body, "argument_patterns", cancellation)?.is_some()
        && direct_kind(body, "->", cancellation)?.is_some()
    {
        return Ok(Some(FSharpAdmissionKind::BoundLambda));
    }
    Ok(None)
}

fn type_kind(
    declaration: Node<'_>,
    cancellation: &Cancellation,
) -> Result<Option<FSharpAdmissionKind>, AdapterError> {
    let mut cursor = declaration.walk();
    for body in declaration.named_children(&mut cursor) {
        cancellation.check()?;
        if body.is_extra() || !valid(body) {
            continue;
        }
        let kind = match body.kind() {
            "record_type_defn" => FSharpAdmissionKind::RecordType,
            "union_type_defn" => FSharpAdmissionKind::UnionType,
            "interface_type_defn" => FSharpAdmissionKind::InterfaceType,
            "anon_type_defn"
                if direct_kind(body, "primary_constr_args", cancellation)?.is_some()
                    || has_owned_type_member(body, cancellation)?
                    || direct_kind(body, "end", cancellation)?.is_some() =>
            {
                FSharpAdmissionKind::ObjectType
            }
            _ => continue,
        };
        return Ok(Some(kind));
    }
    Ok(None)
}

fn has_owned_type_member(
    body: Node<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut cursor = body.walk();
    for child in body.named_children(&mut cursor) {
        cancellation.check()?;
        if child.is_extra() || !valid(child) {
            continue;
        }
        if child.kind() == "member_defn" {
            return Ok(true);
        }
        // Implicit type bodies own members through this direct wrapper. A
        // descendant search could borrow members from nested expressions.
        if child.kind() == "type_extension_elements" {
            let mut member_cursor = child.walk();
            for member in child.named_children(&mut member_cursor) {
                cancellation.check()?;
                if !member.is_extra() && member.kind() == "member_defn" && valid(member) {
                    return Ok(true);
                }
            }
        }
    }
    Ok(false)
}
