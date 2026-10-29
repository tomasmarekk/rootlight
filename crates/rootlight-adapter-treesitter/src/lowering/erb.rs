//! Prove ERB block-parameter reads from one current native Ruby program.
//! Native host ranges and lexical scopes must join the retained syntax and entities.

use super::{MaterializedEntity, provider_failure, source_for_span};
use crate::source_view::NativeSourceView;
use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FactId, SymbolId, content_hash};
use rootlight_ir::{
    ContainerRef, EntityKind, OccurrenceRecord, OccurrenceTarget, SourceRef, SourceSpan,
};
use std::{collections::HashMap, ops::ControlFlow};
use tree_sitter::{Node, ParseOptions, Tree};

type Extent = (usize, usize);

fn extent(node: Node<'_>) -> Extent {
    (node.start_byte(), node.end_byte())
}

fn inside_code(node: Node<'_>, ranges: &[tree_sitter::Range]) -> bool {
    ranges
        .partition_point(|range| range.start_byte <= node.start_byte())
        .checked_sub(1)
        .and_then(|index| ranges.get(index))
        .is_some_and(|range| node.end_byte() <= range.end_byte)
}

fn step(work: &mut usize, cancel: &Cancellation) -> Result<bool, AdapterError> {
    cancel.check()?;
    let Some(next) = work.checked_sub(1) else {
        return Ok(false);
    };
    *work = next;
    Ok(true)
}

fn span(file: rootlight_ids::FileId, range: Extent) -> Result<SourceSpan, AdapterError> {
    SourceSpan::new(
        file,
        u64::try_from(range.0).map_err(|_| SinkError::AccountingOverflow)?,
        u64::try_from(range.1).map_err(|_| SinkError::AccountingOverflow)?,
    )
    .map_err(|_| provider_failure("erb-lexical-span"))
}

fn parse(
    parser: &mut tree_sitter::Parser,
    bytes: &[u8],
    work: &mut usize,
    cancel: &Cancellation,
) -> Result<Option<Tree>, AdapterError> {
    let mut exhausted = false;
    let mut progress = |_: &tree_sitter::ParseState| {
        if cancel.check().is_err() {
            return ControlFlow::Break(());
        }
        match work.checked_sub(1) {
            Some(next) => {
                *work = next;
                ControlFlow::Continue(())
            }
            None => {
                exhausted = true;
                ControlFlow::Break(())
            }
        }
    };
    let mut input = |offset: usize, _: tree_sitter::Point| bytes.get(offset..).unwrap_or_default();
    let tree = parser.parse_with_options(
        &mut input,
        None,
        Some(ParseOptions::new().progress_callback(&mut progress)),
    );
    cancel.check()?;
    Ok(tree.filter(|tree| !exhausted && !tree.root_node().has_error()))
}

pub(super) struct CurrentSource<'a> {
    pub(super) reference: &'a SourceRef,
    pub(super) original: &'a [u8],
    pub(super) view: &'a NativeSourceView<'a>,
    pub(super) path: &'a str,
    pub(super) complete: bool,
    pub(super) maximum: usize,
    pub(super) max_ranges: usize,
}

struct Binding {
    logical: SourceSpan,
    physical: SourceSpan,
    target: SymbolId,
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

struct Facts<'a> {
    exact: HashMap<(SyntaxFactKind, &'a str, SourceSpan), &'a SyntaxFact>,
    ids: HashMap<u64, &'a SyntaxFact>,
}

impl<'a> Facts<'a> {
    fn get(&self, kind: SyntaxFactKind, label: &str, source: SourceSpan) -> Option<&'a SyntaxFact> {
        self.exact.get(&(kind, label, source)).copied()
    }

    fn scope(
        &self,
        node: Node<'_>,
        file: rootlight_ids::FileId,
    ) -> Result<Option<&'a SyntaxFact>, AdapterError> {
        let label = match node.kind() {
            "program" => "ruby.file.scope",
            "do_block" | "block" | "body_statement" => "ruby.block.scope",
            _ => return Ok(None),
        };
        Ok(self.get(SyntaxFactKind::Scope, label, span(file, extent(node))?))
    }

    fn native_parent(
        &self,
        node: Node<'_>,
        file: rootlight_ids::FileId,
        work: &mut usize,
        cancel: &Cancellation,
    ) -> Result<Option<&'a SyntaxFact>, AdapterError> {
        let mut parent = node.parent();
        while let Some(node) = parent {
            if !step(work, cancel)? {
                return Ok(None);
            }
            if matches!(
                node.kind(),
                "method" | "singleton_method" | "class" | "module" | "singleton_class" | "lambda"
            ) {
                return Ok(None);
            }
            if let Some(scope) = self.scope(node, file)? {
                return Ok(Some(scope));
            }
            parent = node.parent();
        }
        Ok(None)
    }

    fn scope_chain(
        &self,
        node: Node<'_>,
        module: &SyntaxFact,
        work: &mut usize,
        cancel: &Cancellation,
    ) -> Result<bool, AdapterError> {
        let file = module.span().file();
        let Some(fact) = self.scope(node, file)? else {
            return Ok(false);
        };
        let mut current = fact;
        let mut native = node;
        loop {
            if !step(work, cancel)? {
                return Ok(false);
            }
            if native.kind() == "program" {
                return Ok(current.parent() == Some(module.local_id())
                    && current.depth() == module.depth().saturating_add(1));
            }
            let Some(parent) = self.native_parent(native, file, work, cancel)? else {
                return Ok(false);
            };
            if current.parent() != Some(parent.local_id())
                || current.depth() != parent.depth().saturating_add(1)
            {
                return Ok(false);
            }
            let mut ancestor = native.parent();
            loop {
                let Some(next) = ancestor else {
                    return Ok(false);
                };
                if !step(work, cancel)? {
                    return Ok(false);
                }
                if self
                    .scope(next, file)?
                    .is_some_and(|scope| scope.local_id() == parent.local_id())
                {
                    native = next;
                    break;
                }
                ancestor = next.parent();
            }
            current = parent;
        }
    }
}

fn reference_scope_chain(
    facts: &Facts<'_>,
    node: Node<'_>,
    module: &SyntaxFact,
    work: &mut usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut parent = node.parent();
    while let Some(node) = parent {
        if !step(work, cancel)? {
            return Ok(false);
        }
        if matches!(
            node.kind(),
            "method" | "singleton_method" | "class" | "module" | "singleton_class" | "lambda"
        ) {
            return Ok(false);
        }
        if facts.scope(node, module.span().file())?.is_some() {
            return facts.scope_chain(node, module, work, cancel);
        }
        parent = node.parent();
    }
    Ok(false)
}

fn host_context_matches(
    facts: &Facts<'_>,
    tree: &Tree,
    host: &SyntaxFact,
    module: &SyntaxFact,
    work: &mut usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut parent = module;
    let mut cursor = tree.walk();
    loop {
        if !step(work, cancel)? {
            return Ok(false);
        }
        let node = cursor.node();
        if u64::try_from(node.start_byte())
            .ok()
            .is_some_and(|start| start <= host.span().start_byte())
            && u64::try_from(node.end_byte())
                .ok()
                .is_some_and(|end| host.span().end_byte() <= end)
            && matches!(
                node.kind(),
                "program" | "do_block" | "block" | "body_statement"
            )
        {
            let Some(scope) = facts.scope(node, module.span().file())? else {
                return Ok(false);
            };
            if !facts.scope_chain(node, module, work, cancel)? {
                return Ok(false);
            }
            if scope.depth() > parent.depth() {
                parent = scope;
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
                return Ok(host.parent() == Some(parent.local_id())
                    && host.depth() == parent.depth().saturating_add(1));
            }
        }
    }
}

fn block_owner<'a>(
    node: Node<'a>,
    work: &mut usize,
    cancel: &Cancellation,
) -> Result<Option<Node<'a>>, AdapterError> {
    let mut parent = node.parent();
    while let Some(node) = parent {
        if !step(work, cancel)? {
            return Ok(None);
        }
        match node.kind() {
            "do_block" | "block" => return Ok(Some(node)),
            "method" | "singleton_method" | "class" | "module" | "singleton_class" | "lambda" => {
                return Ok(None);
            }
            _ => parent = node.parent(),
        }
    }
    Ok(None)
}

fn plain_parameters<'a>(
    owner: Node<'a>,
    work: &mut usize,
    cancel: &Cancellation,
) -> Result<Option<Vec<Node<'a>>>, AdapterError> {
    let Some(parameters) = owner.child_by_field_name("parameters") else {
        return Ok(None);
    };
    let mut result = Vec::new();
    for index in 0..parameters.child_count() {
        if !step(work, cancel)? {
            return Ok(None);
        }
        let Some(node) =
            parameters.child(u32::try_from(index).map_err(|_| SinkError::AccountingOverflow)?)
        else {
            return Ok(None);
        };
        if !node.is_named() {
            if !matches!(node.kind(), "|" | ",") {
                return Ok(None);
            }
        } else {
            if node.kind() != "identifier" {
                return Ok(None);
            }
            result
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            result.push(node);
        }
    }
    Ok(Some(result))
}

fn safe_read(
    node: Node<'_>,
    owner: Node<'_>,
    work: &mut usize,
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    if node.parent().is_some_and(|parent| {
        parent.kind() == "call" && parent.child_by_field_name("method") == Some(node)
    }) {
        return Ok(false);
    }
    let mut ancestor = node.parent();
    while let Some(parent) = ancestor {
        if !step(work, cancel)? {
            return Ok(false);
        }
        if parent == owner {
            break;
        }
        if parent
            .child_by_field_name("operator")
            .is_some_and(|operator| operator.kind() == "defined?")
        {
            return Ok(false);
        }
        ancestor = parent.parent();
    }
    let mut cursor = owner.walk();
    loop {
        if !step(work, cancel)? {
            return Ok(false);
        }
        let node = cursor.node();
        if matches!(
            node.kind(),
            "assignment"
                | "operator_assignment"
                | "for"
                | "rescue"
                | "in_clause"
                | "match_pattern"
                | "match_pattern_p"
                | "right_assignment_list"
        ) {
            return Ok(false);
        }
        if cursor.goto_first_child() {
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
    }
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
            ids: HashMap::new(),
        };
        retained
            .exact
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        retained
            .ids
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            if !step(&mut work, cancel)?
                || fact.span().file() != file
                || retained.ids.insert(fact.local_id(), fact).is_some()
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
            .set_language(&tree_sitter_embedded_template::LANGUAGE.into())
            .map_err(|_| provider_failure("erb-lexical-host-grammar"))?;
        let Some(host) = parse(&mut parser, logical, &mut work, cancel)? else {
            return Ok(result);
        };
        let Some(root) = retained.get(
            SyntaxFactKind::Root,
            "erb.template.root",
            span(file, extent(host.root_node()))?,
        ) else {
            return Ok(result);
        };
        let Some(module) = retained.get(SyntaxFactKind::Module, "erb.file.module", root.span())
        else {
            return Ok(result);
        };
        let Some(module_entity) = entities.get(&module.local_id()) else {
            return Ok(result);
        };
        if root.parent().is_some()
            || root.depth() != 0
            || module.parent() != Some(root.local_id())
            || module.depth() != 1
            || module_entity.definition_local_id.is_some()
            || module_entity.record.kind != EntityKind::Module
            || module_entity.record.language != "erb"
            || module_entity.record.canonical_name != current.path
            || module_entity.record.repository != current.reference.repository()
            || module_entity.record.generation != current.reference.generation()
            || module_entity.direct_parent != ContainerRef::File(file)
            || module_entity.record.container != Some(ContainerRef::File(file))
            || module_entity.record.evidence.source.as_ref()
                != Some(&source_for_span(
                    current.reference,
                    current.view.physical_span(module.span())?,
                ))
        {
            return Ok(result);
        }
        let mut ranges = Vec::new();
        let mut signatures = Vec::new();
        let mut cursor = host.walk();
        'host: loop {
            if !step(&mut work, cancel)? {
                return Ok(result);
            }
            let node = cursor.node();
            if node.kind() == "code" && node.start_byte() < node.end_byte() {
                let Some(parent) = node.parent() else {
                    return Ok(result);
                };
                let label = match parent.kind() {
                    "directive" => "erb.statement_ruby_parsed.expression",
                    "output_directive" => "erb.output_ruby_parsed.expression",
                    _ => return Ok(result),
                };
                let signature = retained.get(
                    SyntaxFactKind::Signature,
                    label,
                    span(file, extent(parent))?,
                );
                let Some(signature) = signature else {
                    return Ok(result);
                };
                if ranges.len() >= current.max_ranges {
                    return Ok(result);
                }
                ranges
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                ranges.push(node.range());
                signatures
                    .try_reserve(1)
                    .map_err(|_| SinkError::AllocationFailed)?;
                signatures.push(signature);
            }
            if cursor.goto_first_child() {
                continue;
            }
            loop {
                if cursor.goto_next_sibling() {
                    break;
                }
                if !cursor.goto_parent() {
                    break 'host;
                }
            }
        }
        // Tree-sitter interprets zero included ranges as the full template.
        if ranges.is_empty() {
            return Ok(result);
        }
        parser
            .set_language(&tree_sitter_ruby::LANGUAGE.into())
            .map_err(|_| provider_failure("erb-lexical-ruby-grammar"))?;
        parser
            .set_included_ranges(&ranges)
            .map_err(|_| provider_failure("erb-lexical-ranges"))?;
        let Some(tree) = parse(&mut parser, logical, &mut work, cancel)? else {
            return Ok(result);
        };
        for signature in signatures {
            if !host_context_matches(&retained, &tree, signature, module, &mut work, cancel)? {
                return Ok(result);
            }
        }
        let mut cursor = tree.walk();
        'ruby: loop {
            if !step(&mut work, cancel)? {
                return Ok(Self {
                    source: result.source,
                    bindings: HashMap::new(),
                });
            }
            let node = cursor.node();
            if node.kind() == "identifier"
                && inside_code(node, &ranges)
                && let Some(owner) = block_owner(node, &mut work, cancel)?
                && let Some(body) = owner.child_by_field_name("body")
                && body.start_byte() <= node.start_byte()
                && node.end_byte() <= body.end_byte()
                && safe_read(node, owner, &mut work, cancel)?
                && retained.scope_chain(owner, module, &mut work, cancel)?
                && let Some(parameters) = plain_parameters(owner, &mut work, cancel)?
            {
                let mut matching = parameters.into_iter().filter(|parameter| {
                    logical.get(parameter.byte_range()) == logical.get(node.byte_range())
                });
                if let Some(parameter) = matching.next()
                    && matching.next().is_none()
                    && inside_code(parameter, &ranges)
                {
                    let reference_span = span(file, extent(node))?;
                    let parameter_span = span(file, extent(parameter))?;
                    let reference = retained.get(
                        SyntaxFactKind::Occurrence,
                        "ruby.identifier.reference",
                        reference_span,
                    );
                    let declaration = retained.get(
                        SyntaxFactKind::Declaration,
                        "ruby.parameter.declaration",
                        parameter_span,
                    );
                    let definition = retained.get(
                        SyntaxFactKind::Occurrence,
                        "ruby.identifier.definition",
                        parameter_span,
                    );
                    if let (
                        Some(reference),
                        Some(declaration),
                        Some(definition),
                        Some(scope),
                        Some(parent),
                    ) = (
                        reference,
                        declaration,
                        definition,
                        retained.scope(owner, file)?,
                        retained.native_parent(node, file, &mut work, cancel)?,
                    ) && reference_scope_chain(&retained, node, module, &mut work, cancel)?
                        && reference.parent() == Some(parent.local_id())
                        && reference.depth() == parent.depth().saturating_add(1)
                        && declaration.parent() == Some(scope.local_id())
                        && declaration.depth() == scope.depth().saturating_add(1)
                        && definition.parent() == Some(declaration.local_id())
                        && definition.depth() == declaration.depth().saturating_add(1)
                        && let Some(entity) = entities.get(&declaration.local_id())
                        && entity.definition_local_id == Some(definition.local_id())
                        && entity.record.kind == EntityKind::Parameter
                        && entity.record.language == "ruby"
                        && entity.record.repository == current.reference.repository()
                        && entity.record.generation == current.reference.generation()
                        && entity.record.canonical_name.as_bytes()
                            == logical.get(parameter.byte_range()).unwrap_or_default()
                        && entity.record.provenance == module_entity.record.provenance
                        && entity.direct_parent == ContainerRef::Entity(module_entity.record.id)
                        && entity.record.container == Some(entity.direct_parent)
                        && entity.record.evidence.source.as_ref()
                            == Some(&source_for_span(
                                current.reference,
                                current.view.physical_span(declaration.span())?,
                            ))
                    {
                        result
                            .bindings
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        result.bindings.insert(
                            reference.local_id(),
                            Binding {
                                logical: reference_span,
                                physical: current.view.physical_span(reference_span)?,
                                target: entity.record.id,
                                enclosing: module_entity.record.id,
                                provenance: entity.record.provenance,
                                hash: content_hash(
                                    logical.get(node.byte_range()).unwrap_or_default(),
                                ),
                                parent: reference.parent(),
                                depth: reference.depth(),
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
                    break 'ruby;
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
            || fact.syntax_kind().as_str() != "ruby.identifier.reference"
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
                binding.logical == fact.span()
                    && binding.physical == physical
                    && physical == occurrence.source.span()
                    && binding.hash == occurrence.syntactic_text_hash
                    && binding.provenance == occurrence.provenance
                    && binding.parent == fact.parent()
                    && binding.depth == fact.depth()
                    && occurrence.enclosing == Some(binding.enclosing)
            })
            .map(|binding| binding.target))
    }
}
