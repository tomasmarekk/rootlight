//! Bind direct WESL formals only after current native syntax and retained owners agree.
//! Unsupported binders, qualified names and custom preprocessing refuse this proof.

use super::{MaterializedEntity, provider_failure, source_for_span};
use crate::source_view::NativeSourceView;
use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, FileId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, FactDomain, OccurrenceRecord, OccurrenceTarget, SourceRef, SourceSpan,
};
use std::{
    collections::{HashMap, HashSet},
    ops::ControlFlow,
};
use tree_sitter::{Node, ParseOptions};

pub(super) fn gaps(fact: &SyntaxFact) -> impl Iterator<Item = (FactDomain, &'static str)> {
    let rows: &[(FactDomain, &'static str)] = if fact.kind() == SyntaxFactKind::Module
        && fact.syntax_kind().as_str() == "wesl.file.module"
    {
        &[
            (
                FactDomain::Entities,
                "wesl-declarations-beyond-named-functions-and-parameters-unavailable",
            ),
            (
                FactDomain::Occurrences,
                "wesl-general-shader-global-type-resource-and-context-occurrences-unavailable",
            ),
        ]
    } else {
        &[]
    };
    rows.iter().copied()
}

fn step(work: &mut usize, cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    let Some(next) = work.checked_sub(1) else {
        return Ok(false);
    };
    *work = next;
    Ok(true)
}

fn visit(
    mut node: Node<'_>,
    work: &mut usize,
    cancel: &Cancellation,
    mut accept: impl FnMut(Node<'_>, &mut usize) -> Result<bool, AdapterError>,
) -> Result<bool, AdapterError> {
    let mut cursor = node.walk();
    loop {
        if !step(work, cancel)? || !accept(node, work)? {
            return Ok(false);
        }
        if cursor.goto_first_child() {
            node = cursor.node();
            continue;
        }
        loop {
            if cursor.goto_next_sibling() {
                break;
            }
            if !cursor.goto_parent() {
                return Ok(true);
            }
        }
        node = cursor.node();
    }
}

fn span(file: FileId, node: Node<'_>) -> Result<SourceSpan, AdapterError> {
    SourceSpan::new(
        file,
        u64::try_from(node.start_byte()).map_err(|_| SinkError::AccountingOverflow)?,
        u64::try_from(node.end_byte()).map_err(|_| SinkError::AccountingOverflow)?,
    )
    .map_err(|_| provider_failure("wesl-formal-span"))
}

fn child(fact: &SyntaxFact, parent: &SyntaxFact) -> bool {
    fact.parent() == Some(parent.local_id()) && fact.depth() == parent.depth().saturating_add(1)
}

struct Facts<'a> {
    exact: HashMap<(SyntaxFactKind, &'a str, SourceSpan), &'a SyntaxFact>,
}
impl<'a> Facts<'a> {
    fn get(
        &self,
        kind: SyntaxFactKind,
        label: &str,
        node: Node<'_>,
        file: FileId,
    ) -> Result<Option<&'a SyntaxFact>, AdapterError> {
        Ok(self.at(kind, label, span(file, node)?))
    }

    fn at(&self, kind: SyntaxFactKind, label: &str, span: SourceSpan) -> Option<&'a SyntaxFact> {
        self.exact.get(&(kind, label, span)).copied()
    }
    fn scope_chain(
        &self,
        node: Node<'_>,
        body: Node<'_>,
        function: &SyntaxFact,
        file: FileId,
        work: &mut usize,
        cancel: &Cancellation,
    ) -> Result<Option<&'a SyntaxFact>, AdapterError> {
        let mut native = node.parent();
        let mut nearest = None;
        let mut previous: Option<&SyntaxFact> = None;
        while let Some(owner) = native {
            if !step(work, cancel)? {
                return Ok(None);
            }
            if owner.kind() == "compound_statement" {
                let Some(fact) =
                    self.get(SyntaxFactKind::Scope, "wesl.block_scope.scope", owner, file)?
                else {
                    return Ok(None);
                };
                if previous.is_some_and(|inner| !child(inner, fact)) {
                    return Ok(None);
                }
                nearest = nearest.or(Some(fact));
                previous = Some(fact);
                if owner == body {
                    return Ok(child(fact, function).then_some(nearest).flatten());
                }
            }
            native = owner.parent();
        }
        Ok(None)
    }
}

fn safe_function(
    node: Node<'_>,
    bytes: &[u8],
    work: &mut usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    visit(node, work, cancel, |part, _| {
        if matches!(
            part.kind(),
            "variable_or_value_statement"
                | "variable_decl"
                | "assignment_statement"
                | "increment_statement"
                | "decrement_statement"
                | "ident_path"
                | "const_assert_statement"
                | "preproc_custom"
                | "compound_global_decl"
        ) {
            return Ok(false);
        }
        if part.kind() == "attribute" {
            let name = part
                .child_by_field_name("name")
                .and_then(|name| bytes.get(name.byte_range()));
            // Conditional and unknown extensions require their own context proof.
            return Ok(matches!(
                name,
                Some(
                    b"vertex"
                        | b"fragment"
                        | b"compute"
                        | b"location"
                        | b"builtin"
                        | b"interpolate"
                        | b"invariant"
                        | b"workgroup_size"
                        | b"diagnostic"
                        | b"must_use"
                )
            ));
        }
        Ok(true)
    })
}

fn value_read(
    node: Node<'_>,
    body: Node<'_>,
    work: &mut usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut branch = node;
    while let Some(parent) = branch.parent() {
        if !step(work, cancel)? {
            return Ok(false);
        }
        match parent.kind() {
            "attribute" | "type_specifier" | "template_list" | "ident_path" | "function_header"
            | "param" => return Ok(false),
            "named_component_expression"
                if parent.child_by_field_name("component") == Some(branch) =>
            {
                return Ok(false);
            }
            "call_expression" | "func_call_statement" if branch.kind() != "argument_list" => {
                return Ok(false);
            }
            "unary_expression"
                if parent
                    .child_by_field_name("operator")
                    .is_some_and(|op| matches!(op.kind(), "&" | "*")) =>
            {
                return Ok(false);
            }
            _ => {}
        }
        if parent == body {
            return Ok(true);
        }
        branch = parent;
    }
    Ok(false)
}

pub(super) struct CurrentSource<'a> {
    pub(super) reference: &'a SourceRef,
    pub(super) original: &'a [u8],
    pub(super) view: &'a NativeSourceView<'a>,
    pub(super) path: &'a str,
    pub(super) complete: bool,
    pub(super) maximum: usize,
}
struct Binding {
    fact: SyntaxFact,
    physical: SourceSpan,
    target: SymbolId,
    owner: SymbolId,
    provenance: FactId,
    hash: ContentHash,
}
pub(super) struct Targets {
    source: SourceRef,
    bindings: HashMap<u64, Binding>,
}

fn entity_matches(
    entity: &MaterializedEntity,
    kind: EntityKind,
    fact: &SyntaxFact,
    definition: Option<u64>,
    parent: ContainerRef,
    current: &CurrentSource<'_>,
) -> Result<bool, AdapterError> {
    Ok(entity.record.kind == kind
        && entity.record.language == "wesl"
        && entity.definition_local_id == definition
        && entity.record.repository == current.reference.repository()
        && entity.record.generation == current.reference.generation()
        && entity.direct_parent == parent
        && entity.record.container == Some(parent)
        && entity.record.evidence.source.as_ref()
            == Some(&source_for_span(
                current.reference,
                current.view.physical_span(fact.span())?,
            )))
}

impl Targets {
    #[allow(clippy::too_many_lines)]
    pub(super) fn new(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        current: CurrentSource<'_>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            source: current.reference.clone(),
            bindings: HashMap::new(),
        };
        if !current.complete
            || current.maximum == 0
            || facts.len() > current.maximum
            || entities.len() > current.maximum
            || current.reference.span().start_byte() != 0
            || u64::try_from(current.original.len()).ok()
                != Some(current.reference.span().end_byte())
            || current.view.physical_bytes() != current.original.len()
        {
            return Ok(result);
        }
        let mut work = current.maximum;
        let mut hash = blake3::Hasher::new();
        for chunk in current.original.chunks(64) {
            if !step(&mut work, cancel)? {
                return Ok(result);
            }
            hash.update(chunk);
        }
        if ContentHash::from_bytes(*hash.finalize().as_bytes()) != current.reference.content_hash()
        {
            return Ok(result);
        }
        let file = current.reference.span().file();
        let mut retained = Facts {
            exact: HashMap::new(),
        };
        let mut ids = HashSet::new();
        retained
            .exact
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        ids.try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            if !step(&mut work, cancel)?
                || fact.span().file() != file
                || !ids.insert(fact.local_id())
                || retained
                    .exact
                    .insert(
                        (fact.kind(), fact.syntax_kind().as_str(), fact.span()),
                        fact,
                    )
                    .is_some()
            {
                return Ok(result);
            }
        }
        let logical = current.view.as_bytes();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_wesl::LANGUAGE.into())
            .map_err(|_| provider_failure("wesl-formal-grammar"))?;
        let mut exhausted = false;
        let mut progress = |_: &tree_sitter::ParseState| {
            if cancel.check().is_err() {
                return ControlFlow::Break(());
            }
            match work.checked_sub(1) {
                Some(next) => {
                    work = next;
                    ControlFlow::Continue(())
                }
                None => {
                    exhausted = true;
                    ControlFlow::Break(())
                }
            }
        };
        let mut input =
            |offset: usize, _: tree_sitter::Point| logical.get(offset..).unwrap_or_default();
        let tree = parser.parse_with_options(
            &mut input,
            None,
            Some(ParseOptions::new().progress_callback(&mut progress)),
        );
        cancel.check()?;
        let Some(tree) = tree.filter(|tree| !exhausted && !tree.root_node().has_error()) else {
            return Ok(result);
        };
        let native = tree.root_node();
        // Match the extractor's complete-file BOM envelope, never a clipped root.
        if !(native.start_byte() == 0
            || native.start_byte() == 3 && logical.starts_with(b"\xef\xbb\xbf"))
            || native.end_byte() != logical.len()
            || !visit(native, &mut work, cancel, |node, _| {
                Ok(node.kind() != "preproc_custom")
            })?
        {
            return Ok(result);
        }
        let envelope = SourceSpan::new(
            file,
            0,
            u64::try_from(logical.len()).map_err(|_| SinkError::AccountingOverflow)?,
        )
        .map_err(|_| provider_failure("wesl-formal-envelope"))?;
        let Some(root) = retained.at(SyntaxFactKind::Root, "wesl.file.root", envelope) else {
            return Ok(result);
        };
        let Some(module) = retained.at(SyntaxFactKind::Module, "wesl.file.module", envelope) else {
            return Ok(result);
        };
        let Some(file_scope) =
            retained.at(SyntaxFactKind::Scope, "wesl.file_scope.scope", envelope)
        else {
            return Ok(result);
        };
        let Some(module_entity) = entities.get(&module.local_id()) else {
            return Ok(result);
        };
        if root.parent().is_some()
            || root.depth() != 0
            || !child(module, root)
            || !child(file_scope, module)
            || module_entity.record.canonical_name != current.path
            || !entity_matches(
                module_entity,
                EntityKind::Module,
                module,
                None,
                ContainerRef::File(file),
                &current,
            )?
        {
            return Ok(result);
        }
        let mut cursor = native.walk();
        'functions: loop {
            if !step(&mut work, cancel)? {
                result.bindings.clear();
                return Ok(result);
            }
            let owner = cursor.node();
            if owner.kind() == "function_decl"
                && owner.parent().is_some_and(|parent| {
                    parent.kind() == "global_decl" && parent.parent() == Some(native)
                })
                && let Some(global) = owner.parent()
                && safe_function(global, logical, &mut work, cancel)?
                && let Some(header) = owner
                    .named_child(0)
                    .filter(|node| node.kind() == "function_header")
                && let Some(name) = header.child_by_field_name("name")
                && let Some(params) = header.child_by_field_name("parameters")
                && let Some(body) = owner.child_by_field_name("body")
                && let Some(declaration) = retained.get(
                    SyntaxFactKind::Declaration,
                    "wesl.function.declaration",
                    owner,
                    file,
                )?
                && let Some(scope) = retained.get(
                    SyntaxFactKind::Scope,
                    "wesl.function_scope.scope",
                    owner,
                    file,
                )?
                && let Some(definition) = retained.get(
                    SyntaxFactKind::Occurrence,
                    "wesl.name.definition",
                    name,
                    file,
                )?
                && let Some(signature) = retained.get(
                    SyntaxFactKind::Signature,
                    "wesl.header.signature",
                    header,
                    file,
                )?
                && let Some(param_scope) = retained.get(
                    SyntaxFactKind::Scope,
                    "wesl.parameters_scope.scope",
                    params,
                    file,
                )?
                && child(declaration, file_scope)
                && child(scope, declaration)
                && child(definition, scope)
                && child(signature, scope)
                && child(param_scope, scope)
                && let Some(function) = entities.get(&declaration.local_id())
                && function.record.canonical_name.as_bytes()
                    == logical.get(name.byte_range()).unwrap_or_default()
                && function.record.provenance == module_entity.record.provenance
                && entity_matches(
                    function,
                    EntityKind::Function,
                    declaration,
                    Some(definition.local_id()),
                    ContainerRef::Entity(module_entity.record.id),
                    &current,
                )?
            {
                let mut parameters = HashMap::new();
                let mut names = HashSet::new();
                let mut valid = true;
                parameters
                    .try_reserve(params.named_child_count())
                    .map_err(|_| SinkError::AllocationFailed)?;
                names
                    .try_reserve(params.named_child_count())
                    .map_err(|_| SinkError::AllocationFailed)?;
                for index in 0..params.named_child_count() {
                    if !step(&mut work, cancel)? {
                        valid = false;
                        break;
                    }
                    let Some(param) = params.named_child(
                        u32::try_from(index).map_err(|_| SinkError::AccountingOverflow)?,
                    ) else {
                        valid = false;
                        break;
                    };
                    let Some(name) = param.child_by_field_name("name") else {
                        valid = false;
                        break;
                    };
                    let raw = logical.get(name.byte_range()).unwrap_or_default();
                    if param.kind() != "param" || !names.insert(raw) {
                        valid = false;
                        break;
                    }
                    let Some(declaration) = retained.get(
                        SyntaxFactKind::Declaration,
                        "wesl.parameter.declaration",
                        param,
                        file,
                    )?
                    else {
                        valid = false;
                        break;
                    };
                    let Some(definition) = retained.get(
                        SyntaxFactKind::Occurrence,
                        "wesl.name.definition",
                        name,
                        file,
                    )?
                    else {
                        valid = false;
                        break;
                    };
                    let Some(entity) = entities.get(&declaration.local_id()) else {
                        valid = false;
                        break;
                    };
                    if !child(declaration, param_scope)
                        || !child(definition, declaration)
                        || entity.record.canonical_name.as_bytes() != raw
                        || entity.record.provenance != function.record.provenance
                        || !entity_matches(
                            entity,
                            EntityKind::Parameter,
                            declaration,
                            Some(definition.local_id()),
                            ContainerRef::Entity(function.record.id),
                            &current,
                        )?
                    {
                        valid = false;
                        break;
                    }
                    parameters.insert(raw, entity);
                }
                if valid {
                    let complete = visit(body, &mut work, cancel, |node, work| {
                        if node.kind() != "identifier" || !value_read(node, body, work, cancel)? {
                            return Ok(true);
                        }
                        let Some(entity) =
                            parameters.get(logical.get(node.byte_range()).unwrap_or_default())
                        else {
                            return Ok(true);
                        };
                        let Some(reference) = retained.get(
                            SyntaxFactKind::Occurrence,
                            "wesl.identifier.reference",
                            node,
                            file,
                        )?
                        else {
                            return Ok(true);
                        };
                        let Some(parent) =
                            retained.scope_chain(node, body, scope, file, work, cancel)?
                        else {
                            return Ok(true);
                        };
                        if !child(reference, parent) {
                            return Ok(true);
                        }
                        result
                            .bindings
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        result.bindings.insert(
                            reference.local_id(),
                            Binding {
                                fact: reference.clone(),
                                physical: current.view.physical_span(reference.span())?,
                                target: entity.record.id,
                                owner: function.record.id,
                                provenance: entity.record.provenance,
                                hash: content_hash(
                                    logical.get(node.byte_range()).unwrap_or_default(),
                                ),
                            },
                        );
                        Ok(true)
                    })?;
                    if !complete || work == 0 {
                        result.bindings.clear();
                        return Ok(result);
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
                    break 'functions;
                }
            }
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        physical: SourceSpan,
        occurrence: &OccurrenceRecord,
        cancel: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancel.check()?;
        if fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "wesl.identifier.reference"
            || occurrence.syntax_kind != fact.syntax_kind().as_str()
            || !matches!(occurrence.target, OccurrenceTarget::Unresolved { .. })
            || occurrence.repository != self.source.repository()
            || occurrence.source.repository() != self.source.repository()
            || occurrence.generation != self.source.generation()
            || occurrence.source.generation() != self.source.generation()
            || occurrence.source.content_hash() != self.source.content_hash()
            || occurrence.file != self.source.span().file()
            || occurrence.source.span().file() != self.source.span().file()
        {
            return Ok(None);
        }
        Ok(self
            .bindings
            .get(&fact.local_id())
            .filter(|binding| {
                binding.fact.span() == fact.span()
                    && binding.fact.parent() == fact.parent()
                    && binding.fact.depth() == fact.depth()
                    && binding.physical == physical
                    && physical == occurrence.source.span()
                    && binding.hash == occurrence.syntactic_text_hash
                    && binding.provenance == occurrence.provenance
                    && occurrence.enclosing == Some(binding.owner)
            })
            .map(|binding| binding.target))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn visitor_and_callbacks_share_one_exhaustion_counter() {
        let cancel = Cancellation::new();
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_wesl::LANGUAGE.into())
            .unwrap();
        let tree = parser
            .parse("fn entry(value: f32) -> f32 { return value; }", None)
            .unwrap();
        let mut remaining = 4;
        let mut callbacks = 0;
        let complete = visit(tree.root_node(), &mut remaining, &cancel, |_, work| {
            callbacks += 1;
            step(work, &cancel)
        })
        .unwrap();
        assert!(!complete);
        assert_eq!(remaining, 0);
        assert_eq!(callbacks, 2);
    }
}
