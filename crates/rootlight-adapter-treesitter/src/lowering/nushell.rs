//! Admit exact Nu names from complete current native syntax.
//! Quoted command names retain their authored interior without unescaping.

use super::{MaterializedEntity, provider_failure, source_for_span};
use crate::source_view::NativeSourceView;
use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceTarget, SourceRef, SourceSpan,
};
use std::{
    collections::{HashMap, HashSet},
    ops::ControlFlow,
};
use tree_sitter::{Node, ParseOptions, Tree};

type Range = (usize, usize);

#[derive(Default)]
pub(super) struct Plan {
    names: HashMap<(Range, Range, EntityKind), ()>,
    tree: Option<Tree>,
    source: Option<SourceRef>,
    maximum: usize,
}

fn range(node: Node<'_>) -> Range {
    (node.start_byte(), node.end_byte())
}

fn written_name(node: Node<'_>) -> Option<Node<'_>> {
    if let Some(name) = node.child_by_field_name("unquoted_name") {
        return (name.kind() == "cmd_identifier").then_some(name);
    }
    let quoted = node.child_by_field_name("quoted_name")?;
    if quoted.kind() != "val_string" || quoted.named_child_count() != 1 {
        return None;
    }
    quoted
        .named_child(0)
        .filter(|name| name.kind() == "string_content" && name.named_child_count() == 0)
}

pub(super) fn formal_parameter(node: Node<'_>) -> bool {
    if node.kind() != "parameter" {
        return false;
    }
    let Some(wrapper) = node.parent() else {
        return false;
    };
    let Some(owner) = wrapper.parent() else {
        return false;
    };
    matches!(wrapper.kind(), "parameter_bracks" | "parameter_parens")
        && owner.kind() == "decl_def"
        && owner.child_by_field_name("parameters") == Some(wrapper)
}

impl Plan {
    pub(super) fn new(
        source: &SourceRef,
        original: &[u8],
        view: &NativeSourceView<'_>,
        complete: bool,
        maximum: usize,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self::default();
        if !complete
            || maximum == 0
            || source.span().start_byte() != 0
            || u64::try_from(original.len()).ok() != Some(source.span().end_byte())
            || view.physical_bytes() != original.len()
        {
            return Ok(result);
        }
        let mut remaining = maximum;
        let mut hash = blake3::Hasher::new();
        for chunk in original.chunks(64) {
            cancel.check()?;
            let Some(next) = remaining.checked_sub(1) else {
                return Ok(result);
            };
            remaining = next;
            hash.update(chunk);
        }
        if ContentHash::from_bytes(*hash.finalize().as_bytes()) != source.content_hash() {
            return Ok(result);
        }
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_nu::LANGUAGE.into())
            .map_err(|_| provider_failure("nu-current-source-grammar"))?;
        let mut exhausted = false;
        let mut progress = |_: &tree_sitter::ParseState| {
            if cancel.check().is_err() {
                return ControlFlow::Break(());
            }
            match remaining.checked_sub(1) {
                Some(next) => {
                    remaining = next;
                    ControlFlow::Continue(())
                }
                None => {
                    exhausted = true;
                    ControlFlow::Break(())
                }
            }
        };
        let mut bytes = |offset: usize, _: tree_sitter::Point| {
            view.as_bytes().get(offset..).unwrap_or_default()
        };
        let tree = parser.parse_with_options(
            &mut bytes,
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        cancel.check()?;
        let Some(tree) = tree.filter(|_| !exhausted) else {
            return Ok(result);
        };
        let mut cursor = tree.walk();
        'walk: loop {
            cancel.check()?;
            let Some(next) = remaining.checked_sub(1) else {
                return Ok(Self::default());
            };
            remaining = next;
            let node = cursor.node();
            if !node.has_error() && !node.is_missing() {
                let candidate = match node.kind() {
                    "decl_def" => written_name(node).map(|name| (name, EntityKind::Function)),
                    "decl_module" => written_name(node).map(|name| (name, EntityKind::Namespace)),
                    "parameter" if formal_parameter(node) => node
                        .child_by_field_name("param_name")
                        .filter(|name| name.kind() == "identifier")
                        .map(|name| (name, EntityKind::Parameter)),
                    "stmt_let" | "stmt_mut" | "stmt_const" => node
                        .child_by_field_name("name")
                        .filter(|name| name.kind() == "identifier")
                        .map(|name| (name, EntityKind::Variable)),
                    _ => None,
                };
                if let Some((name, kind)) = candidate
                    && named_owners_proven(node, &mut remaining, cancel)?
                {
                    result
                        .names
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    result.names.insert((range(node), range(name), kind), ());
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
                    break 'walk;
                }
            }
        }
        drop(cursor);
        result.tree = Some(tree);
        result.source = Some(source.clone());
        result.maximum = remaining;
        Ok(result)
    }

    pub(super) fn name<'a>(
        &self,
        declaration: &SyntaxFact,
        definition: &SyntaxFact,
        kind: EntityKind,
        text: &'a str,
        maximum: usize,
    ) -> Option<std::borrow::Cow<'a, str>> {
        let to_range = |fact: &SyntaxFact| {
            Some((
                usize::try_from(fact.span().start_byte()).ok()?,
                usize::try_from(fact.span().end_byte()).ok()?,
            ))
        };
        (self
            .names
            .contains_key(&(to_range(declaration)?, to_range(definition)?, kind))
            && !text.is_empty()
            && text.len() <= maximum
            && !text.chars().any(char::is_control))
        .then_some(std::borrow::Cow::Borrowed(text))
    }
}

pub(super) const VALUE: &str = "nu.value.reference";
pub(super) const COMPLETER: &str = "nu.completer.reference";

pub(super) fn reference(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && matches!(fact.syntax_kind().as_str(), VALUE | COMPLETER)
}

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let detail = match (fact.kind(), fact.syntax_kind().as_str()) {
        (SyntaxFactKind::Root, "nu.file.root") => Some(
            "nu-written-declarations-formal-reads-and-static-local-completers-only-runtime-and-import-semantics-unavailable",
        ),
        (SyntaxFactKind::Signature, "nu.runtime_semantics_unavailable.signature")
        | (SyntaxFactKind::Signature, "nu.runtime_semantics_unavailable.expression") => {
            Some("nu-command-dispatch-runtime-values-and-unsupported-binding-context-unavailable")
        }
        _ => None,
    };
    [
        FactDomain::Entities,
        FactDomain::Occurrences,
        FactDomain::Relations,
    ]
    .into_iter()
    .filter_map(move |domain| detail.map(|detail| (domain, detail)))
}

fn step(remaining: &mut usize, cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    let Some(next) = remaining.checked_sub(1) else {
        return Ok(false);
    };
    *remaining = next;
    Ok(true)
}

fn named_owners_proven(
    node: Node<'_>,
    remaining: &mut usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut parent = node.parent();
    while let Some(owner) = parent {
        if !step(remaining, cancel)? {
            return Ok(false);
        }
        if matches!(owner.kind(), "decl_def" | "decl_module")
            && (owner.has_error() || owner.is_missing() || written_name(owner).is_none())
        {
            return Ok(false);
        }
        parent = owner.parent();
    }
    Ok(true)
}

fn ancestor<'a>(
    node: Node<'a>,
    kinds: &[&str],
    remaining: &mut usize,
    cancel: &Cancellation,
) -> Result<Option<Node<'a>>, AdapterError> {
    let mut parent = node.parent();
    while let Some(node) = parent {
        if !step(remaining, cancel)? {
            return Ok(None);
        }
        if kinds.contains(&node.kind()) {
            return Ok(Some(node));
        }
        parent = node.parent();
    }
    Ok(None)
}

fn unique<K: Eq + std::hash::Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|previous| *previous = None)
        .or_insert(Some(value));
}

fn span(file: rootlight_ids::FileId, extent: Range) -> Result<SourceSpan, AdapterError> {
    SourceSpan::new(
        file,
        u64::try_from(extent.0).map_err(|_| provider_failure("nu-source-span"))?,
        u64::try_from(extent.1).map_err(|_| provider_failure("nu-source-span"))?,
    )
    .map_err(|_| provider_failure("nu-source-span"))
}

struct NativeDefinition {
    name: Range,
    kind: EntityKind,
    owner: Option<Range>,
}

struct Binding {
    logical: SourceSpan,
    physical: SourceSpan,
    target: SymbolId,
    subject: Option<SymbolId>,
    enclosing: SymbolId,
    provenance: FactId,
    hash: ContentHash,
    parent: Option<u64>,
    depth: usize,
}

pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

pub(super) struct CurrentSource<'a> {
    pub(super) reference: &'a SourceRef,
    pub(super) logical: &'a [u8],
}

impl Targets {
    pub(super) fn new<'text>(
        plan: Plan,
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        current: CurrentSource<'_>,
        mut text: impl FnMut(SourceSpan) -> Result<&'text str, AdapterError>,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let CurrentSource {
            reference: source,
            logical,
        } = current;
        let mut result = Self {
            source: source.clone(),
            bindings: HashMap::new(),
        };
        let Some(tree) = plan
            .tree
            .as_ref()
            .filter(|tree| !tree.root_node().has_error())
        else {
            return Ok(result);
        };
        if plan.source.as_ref() != Some(source)
            || facts.len() > plan.maximum
            || entities.len() > plan.maximum
        {
            return Ok(result);
        }
        let mut remaining = plan.maximum;
        let file = source.span().file();
        let mut exact = HashMap::new();
        let mut scopes = HashMap::new();
        let mut locals = HashSet::new();
        exact
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        locals
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        scopes
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            if !step(&mut remaining, cancel)? {
                return Ok(result);
            }
            if fact.span().file() != file || !locals.insert(fact.local_id()) {
                return Ok(result);
            }
            if fact.kind() == SyntaxFactKind::Scope
                && fact.syntax_kind().as_str() == "nu.scope.scope"
            {
                unique(&mut scopes, (fact.span(), fact.parent()), fact);
            } else {
                unique(
                    &mut exact,
                    (fact.kind(), fact.syntax_kind().as_str(), fact.span()),
                    fact,
                );
            }
        }
        if exact.values().any(Option::is_none) || scopes.values().any(Option::is_none) {
            return Ok(result);
        }
        let lookup = |kind, label, extent| exact.get(&(kind, label, extent)).copied().flatten();
        let Some(root) = lookup(
            SyntaxFactKind::Root,
            "nu.file.root",
            span(file, range(tree.root_node()))?,
        ) else {
            return Ok(result);
        };
        if root.parent().is_some() || root.depth() != 0 {
            return Ok(result);
        }
        let Some(module) = lookup(SyntaxFactKind::Module, "nu.file.module", root.span()) else {
            return Ok(result);
        };
        if module.parent() != Some(root.local_id()) || module.depth() != 1 {
            return Ok(result);
        }
        let scope = |extent, parent| scopes.get(&(extent, Some(parent))).copied().flatten();
        let Some(file_scope) = scope(root.span(), module.local_id()) else {
            return Ok(result);
        };
        if file_scope.depth() != 2 {
            return Ok(result);
        }
        let mut definitions = HashMap::new();
        let mut parameters = HashMap::new();
        let mut commands = HashMap::new();
        let mut unsafe_functions = HashSet::new();
        let mut unsafe_command_scopes = HashSet::new();
        let mut cursor = tree.walk();
        'walk: loop {
            if !step(&mut remaining, cancel)? {
                return Ok(result);
            }
            let node = cursor.node();
            let owner = ancestor(node, &["decl_def"], &mut remaining, cancel)?.map(range);
            let changes_scope = node.kind() == "command"
                && node.child_by_field_name("head").is_some_and(|head| {
                    node.start_byte() == head.start_byte()
                        && logical.get(head.byte_range()).is_some_and(|head| {
                            matches!(
                                head,
                                b"source" | b"source-env" | b"overlay" | b"hide" | b"plugin"
                            )
                        })
                });
            if (matches!(
                node.kind(),
                "stmt_let" | "stmt_mut" | "stmt_const" | "ctrl_for" | "ctrl_match"
            ) || changes_scope
                || (formal_parameter(node)
                    && node
                        .child_by_field_name("param_name")
                        .is_none_or(|name| name.kind() != "identifier")))
                && let Some(owner) = owner
            {
                unsafe_functions
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                unsafe_functions.insert(owner);
            }
            if matches!(
                node.kind(),
                "command" | "decl_use" | "decl_alias" | "decl_extern" | "decl_module"
            ) && let Some(scope) =
                ancestor(node, &["nu_script", "block"], &mut remaining, cancel)?
            {
                unsafe_command_scopes
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                unsafe_command_scopes.insert(range(scope));
            }
            let candidate = match node.kind() {
                "decl_def" => written_name(node).map(|name| (name, EntityKind::Function)),
                "parameter" if formal_parameter(node) => node
                    .child_by_field_name("param_name")
                    .filter(|name| name.kind() == "identifier")
                    .map(|name| (name, EntityKind::Parameter)),
                _ => None,
            };
            if let Some((name, kind)) = candidate {
                let declaration = range(node);
                if plan.names.contains_key(&(declaration, range(name), kind)) {
                    definitions
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    definitions.insert(
                        declaration,
                        NativeDefinition {
                            name: range(name),
                            kind,
                            owner,
                        },
                    );
                    if let Some(bytes) = logical.get(name.byte_range()) {
                        if kind == EntityKind::Parameter
                            && let Some(owner) = owner
                        {
                            parameters
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            unique(&mut parameters, (owner, bytes), declaration);
                        } else if kind == EntityKind::Function
                            && let Some(scope) =
                                ancestor(node, &["nu_script", "block"], &mut remaining, cancel)?
                        {
                            commands
                                .try_reserve(1)
                                .map_err(|_| SinkError::AllocationFailed)?;
                            unique(&mut commands, (range(scope), bytes), declaration);
                        }
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
                    break 'walk;
                }
            }
        }
        let mut joined = HashMap::new();
        joined
            .try_reserve(definitions.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (extent, proof) in &definitions {
            if !step(&mut remaining, cancel)? {
                return Ok(result);
            }
            let label = if proof.kind == EntityKind::Function {
                "nu.function.declaration"
            } else {
                "nu.parameter.declaration"
            };
            let Some(declaration) =
                lookup(SyntaxFactKind::Declaration, label, span(file, *extent)?)
            else {
                continue;
            };
            let name_span = span(file, proof.name)?;
            let Some(definition) = lookup(
                SyntaxFactKind::Occurrence,
                "nu.identifier.definition",
                name_span,
            ) else {
                continue;
            };
            let Some(entity) = entities.get(&declaration.local_id()) else {
                continue;
            };
            let lexical_parent = if let Some(owner) = proof.owner {
                lookup(
                    SyntaxFactKind::Declaration,
                    "nu.function.declaration",
                    span(file, owner)?,
                )
                .and_then(|owner| scope(owner.span(), owner.local_id()))
            } else {
                Some(file_scope)
            };
            if !lexical_parent.is_some_and(|parent| {
                declaration.parent() == Some(parent.local_id())
                    && declaration.depth() == parent.depth().saturating_add(1)
            }) {
                continue;
            }
            let parent_valid = if proof.kind == EntityKind::Function {
                scope(declaration.span(), declaration.local_id()).is_some_and(|scope| {
                    scope.parent() == Some(declaration.local_id())
                        && definition.parent() == Some(scope.local_id())
                        && scope.depth() == declaration.depth().saturating_add(1)
                        && definition.depth() == scope.depth().saturating_add(1)
                })
            } else {
                definition.parent() == Some(declaration.local_id())
                    && definition.depth() == declaration.depth().saturating_add(1)
            };
            if !parent_valid
                || entity.definition_local_id != Some(definition.local_id())
                || entity.record.kind != proof.kind
                || entity.record.language != "nu"
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
                || entity.record.canonical_name != text(name_span)?
                || text(name_span)?.as_bytes()
                    != logical.get(proof.name.0..proof.name.1).unwrap_or_default()
                || entity.record.evidence.source.as_ref()
                    != Some(&source_for_span(source, physical(declaration.span())?))
            {
                continue;
            }
            joined.insert(*extent, entity);
        }
        let mut valid_functions = HashSet::new();
        valid_functions
            .try_reserve(joined.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (extent, proof) in &definitions {
            if proof.kind != EntityKind::Function {
                continue;
            }
            let mut current = *extent;
            let mut valid = true;
            loop {
                if !step(&mut remaining, cancel)? {
                    return Ok(result);
                }
                let Some(proof) = definitions.get(&current) else {
                    valid = false;
                    break;
                };
                let Some(entity) = joined.get(&current) else {
                    valid = false;
                    break;
                };
                let parent = if let Some(owner) = proof.owner {
                    let Some(owner_entity) = joined.get(&owner) else {
                        valid = false;
                        break;
                    };
                    ContainerRef::Entity(owner_entity.record.id)
                } else {
                    ContainerRef::File(file)
                };
                if entity.direct_parent != parent || entity.record.container != Some(parent) {
                    valid = false;
                    break;
                }
                let Some(owner) = proof.owner else { break };
                current = owner;
            }
            if valid {
                valid_functions.insert(*extent);
            }
        }
        joined.retain(|extent, _| {
            definitions.get(extent).is_some_and(|proof| {
                if proof.kind == EntityKind::Function {
                    valid_functions.contains(extent)
                } else {
                    proof
                        .owner
                        .is_some_and(|owner| valid_functions.contains(&owner))
                }
            })
        });
        let mut cursor = tree.walk();
        'walk: loop {
            if !step(&mut remaining, cancel)? {
                return Ok(result);
            }
            let node = cursor.node();
            let mut proposed = None;
            if node.kind() == "val_variable"
                && let Some(name) = node.child_by_field_name("name")
                && name.kind() == "identifier"
                && let Some(owner) = ancestor(node, &["decl_def"], &mut remaining, cancel)?
                && !unsafe_functions.contains(&range(owner))
                && let Some(body) = owner.child_by_field_name("body")
                && body.start_byte() <= node.start_byte()
                && node.end_byte() <= body.end_byte()
                && let Some(bytes) = logical.get(name.byte_range())
                && let Some(Some(target)) = parameters.get(&(range(owner), bytes))
            {
                let mut current = node.parent();
                let mut safe = true;
                while let Some(parent) = current {
                    if !step(&mut remaining, cancel)? {
                        return Ok(result);
                    }
                    if parent == owner {
                        break;
                    }
                    if parent.kind() == "decl_module" {
                        safe = false;
                        break;
                    }
                    if parent.kind() == "assignment"
                        && parent.child_by_field_name("lhs").is_some_and(|lhs| {
                            lhs.start_byte() <= node.start_byte()
                                && node.end_byte() <= lhs.end_byte()
                        })
                    {
                        safe = false;
                        break;
                    }
                    if parent.kind() == "val_closure"
                        && let Some(params) = parent.child_by_field_name("parameters")
                    {
                        let mut params_cursor = params.walk();
                        for param in params.named_children(&mut params_cursor) {
                            if !step(&mut remaining, cancel)? {
                                return Ok(result);
                            }
                            if param.kind() != "parameter" {
                                continue;
                            }
                            let local = param.child_by_field_name("param_name");
                            if local.is_none_or(|local| {
                                local.kind() != "identifier"
                                    || logical.get(local.byte_range()) == Some(bytes)
                            }) {
                                safe = false;
                                break;
                            }
                        }
                    }
                    current = parent.parent();
                }
                if safe {
                    proposed = Some((range(name), *target, None, range(owner), VALUE));
                }
            } else if node.kind() == "param_completer"
                && let Some(parameter) =
                    ancestor(node, &["parameter", "decl_def"], &mut remaining, cancel)?
                && formal_parameter(parameter)
                && node.parent().is_some_and(|annotation| {
                    annotation.kind() == "param_type"
                        && annotation.child_by_field_name("completion") == Some(node)
                        && annotation.parent() == Some(parameter)
                })
                && let Some(owner) = ancestor(parameter, &["decl_def"], &mut remaining, cancel)?
                && let Some(scope) =
                    ancestor(owner, &["nu_script", "block"], &mut remaining, cancel)?
                && !unsafe_command_scopes.contains(&range(scope))
                && let Some(command) = node.child_by_field_name("command")
            {
                let name = if command.kind() == "cmd_identifier" {
                    Some(command)
                } else if command.kind() == "val_string" && command.named_child_count() == 1 {
                    command.named_child(0).filter(|name| {
                        name.kind() == "string_content" && name.named_child_count() == 0
                    })
                } else {
                    None
                };
                if let Some(name) = name
                    && let Some(bytes) = logical.get(name.byte_range())
                    && let Some(Some(target)) = commands.get(&(range(scope), bytes))
                {
                    proposed = Some((
                        range(name),
                        *target,
                        Some(range(parameter)),
                        range(owner),
                        COMPLETER,
                    ));
                }
            }
            if let Some((extent, target, subject, owner, label)) = proposed {
                let endpoint = joined.get(&target).copied();
                let enclosing = joined.get(&owner).copied();
                let subject_entity = subject.and_then(|subject| joined.get(&subject).copied());
                let target_proof = definitions.get(&target);
                if let (Some(endpoint), Some(enclosing), Some(target_proof)) =
                    (endpoint, enclosing, target_proof)
                {
                    let ownership_valid = if subject.is_some() {
                        subject_entity.is_some_and(|subject| {
                            subject.direct_parent == ContainerRef::Entity(enclosing.record.id)
                        })
                    } else {
                        target_proof.owner == Some(owner)
                            && endpoint.direct_parent == ContainerRef::Entity(enclosing.record.id)
                    };
                    let logical_span = span(file, extent)?;
                    if ownership_valid
                        && endpoint.record.provenance == enclosing.record.provenance
                        && let Some(fact) = lookup(SyntaxFactKind::Occurrence, label, logical_span)
                    {
                        result
                            .bindings
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        result.bindings.insert(
                            fact.local_id(),
                            Binding {
                                logical: logical_span,
                                physical: physical(logical_span)?,
                                target: endpoint.record.id,
                                subject: subject_entity.map(|entity| entity.record.id),
                                enclosing: subject_entity.unwrap_or(enclosing).record.id,
                                provenance: endpoint.record.provenance,
                                hash: content_hash(text(logical_span)?.as_bytes()),
                                parent: fact.parent(),
                                depth: fact.depth(),
                            },
                        );
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
                    break 'walk;
                }
            }
        }
        if remaining == 0 {
            result.bindings.clear();
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<(SymbolId, Option<SymbolId>)>, AdapterError> {
        cancel.check()?;
        if !reference(fact)
            || occurrence.syntax_kind != fact.syntax_kind().as_str()
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.source.repository() != self.source.repository()
            || occurrence.repository != self.source.repository()
            || occurrence.source.generation() != self.source.generation()
            || occurrence.generation != self.source.generation()
            || occurrence.source.content_hash() != self.source.content_hash()
            || occurrence.source.span().file() != self.source.span().file()
            || occurrence.file != self.source.span().file()
        {
            return Ok(None);
        }
        Ok(self
            .bindings
            .get(&fact.local_id())
            .filter(|binding| {
                binding.logical == fact.span()
                    && binding.physical == physical
                    && physical == occurrence.source.span()
                    && binding.hash == occurrence.syntactic_text_hash
                    && binding.provenance == occurrence.provenance
                    && binding.parent == fact.parent()
                    && binding.depth == fact.depth()
                    && occurrence.enclosing == Some(binding.enclosing)
            })
            .map(|binding| (binding.target, binding.subject)))
    }
}
