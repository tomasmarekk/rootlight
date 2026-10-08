//! Complete public value and finite type masks from native F# signature captures.
//! Signature declarations remain navigable symbols, never executable exports.
//! Exact owned-container tiling prevents omitted declarations from granting visibility.

use super::{
    DEFINITION, QualifiedClaim, QualifiedClaims, QualifiedModule, UniqueFact, allocation_failed,
    canonical_identifier, component_hashes, contains, invalid_capture, lexical,
    qualified_container_path, qualified_gap, qualified_header_bytes, qualified_owner_path,
    qualified_plain_type_head, source_bytes, source_name, trivia_between, trivia_end,
    unqualified_written_name, validate_ancestry, written_components,
};
use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, SymbolId, content_hash};
use rootlight_ir::FSharpBinding;

const CONTEXT: &str = "fsharp.signature_context.expression";
const VALUE: &str = "fsharp.signature_val.declaration";
const VALUE_HEADER: &str = "fsharp.signature_val.signature";
const RECORD: &str = "fsharp.record.declaration";
const RECORD_MARKER: &str = "fsharp.signature_record_type.expression";
const ALIAS: &str = "fsharp.type_alias.declaration";
const ALIAS_MARKER: &str = "fsharp.signature_alias_type.expression";

struct Value<'a> {
    declaration: &'a SyntaxFact,
    header: UniqueFact<'a>,
    definition: UniqueFact<'a>,
}

struct Type<'a> {
    declaration: &'a SyntaxFact,
    scope: UniqueFact<'a>,
    header: UniqueFact<'a>,
    definition: UniqueFact<'a>,
}

pub(super) fn retain(
    facts: &[SyntaxFact],
    source: &[u8],
    symbols: &HashMap<u64, SymbolId>,
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Option<QualifiedClaims>, AdapterError> {
    cancellation.check()?;
    let mut by_id = HashMap::new();
    let mut modules = HashMap::<u64, QualifiedModule<'_>>::new();
    let mut values = HashMap::<u64, Value<'_>>::new();
    let mut types = HashMap::<u64, Type<'_>>::new();
    let mut markers = HashMap::<(Option<u64>, rootlight_ids::FileId, u64), UniqueFact<'_>>::new();
    let mut context = UniqueFact::default();
    let mut file_scope = UniqueFact::default();
    by_id.try_reserve(facts.len()).map_err(allocation_failed)?;
    modules
        .try_reserve(facts.len())
        .map_err(allocation_failed)?;
    values.try_reserve(facts.len()).map_err(allocation_failed)?;
    types.try_reserve(facts.len()).map_err(allocation_failed)?;
    markers
        .try_reserve(facts.len())
        .map_err(allocation_failed)?;
    for fact in facts {
        cancellation.check()?;
        source_bytes(fact, source)?;
        if by_id.insert(fact.local_id(), fact).is_some() {
            return Err(invalid_capture());
        }
        if fact.syntax_kind().as_str() == CONTEXT {
            context.retain(fact)?;
        }
        if matches!(fact.syntax_kind().as_str(), RECORD_MARKER | ALIAS_MARKER) {
            markers
                .entry((fact.parent(), fact.span().file(), fact.span().end_byte()))
                .or_default()
                .retain(fact)?;
        }
        if fact.kind() == SyntaxFactKind::Scope
            && fact.syntax_kind().as_str() == "fsharp.file.scope"
        {
            file_scope.retain(fact)?;
        }
        if fact.kind() == SyntaxFactKind::Declaration {
            match fact.syntax_kind().as_str() {
                "fsharp.module.declaration"
                | "fsharp.namespace.declaration"
                | "fsharp.nested_module.declaration" => {
                    modules.insert(
                        fact.local_id(),
                        QualifiedModule {
                            declaration: fact,
                            scope: UniqueFact::default(),
                            header: UniqueFact::default(),
                            definition: UniqueFact::default(),
                        },
                    );
                }
                VALUE => {
                    values.insert(
                        fact.local_id(),
                        Value {
                            declaration: fact,
                            header: UniqueFact::default(),
                            definition: UniqueFact::default(),
                        },
                    );
                }
                RECORD | ALIAS => {
                    types.insert(
                        fact.local_id(),
                        Type {
                            declaration: fact,
                            scope: UniqueFact::default(),
                            header: UniqueFact::default(),
                            definition: UniqueFact::default(),
                        },
                    );
                }
                _ => {}
            }
        }
    }
    validate_ancestry(&by_id, cancellation)?;
    let (Some(context), Some(file_scope)) = (context.one(), file_scope.one()) else {
        return Ok(None);
    };
    let end = u64::try_from(source.len()).map_err(|_| invalid_capture())?;
    if context.kind() != SyntaxFactKind::Signature
        || context.span().start_byte() != 0
        || context.span().end_byte() != end
        || context.span().file() != file_scope.span().file()
        || modules.is_empty()
        || !file_owner(file_scope, &by_id, source, cancellation)?
    {
        return Ok(None);
    }
    for fact in facts {
        cancellation.check()?;
        if fact.span().file() != context.span().file() {
            return Ok(None);
        }
        let parent = fact.parent().and_then(|id| by_id.get(&id).copied());
        let owner = parent.and_then(|parent| {
            if modules.contains_key(&parent.local_id())
                || values.contains_key(&parent.local_id())
                || types.contains_key(&parent.local_id())
            {
                Some(parent.local_id())
            } else if parent.kind() == SyntaxFactKind::Scope {
                parent.parent()
            } else {
                None
            }
        });
        if let Some(module) = owner.and_then(|id| modules.get_mut(&id)) {
            match fact.kind() {
                SyntaxFactKind::Scope if lexical::module_scope(fact) => {
                    module.scope.retain(fact)?
                }
                SyntaxFactKind::Signature
                    if fact.syntax_kind().as_str().ends_with(".signature") =>
                {
                    module.header.retain(fact)?;
                }
                SyntaxFactKind::Occurrence if fact.syntax_kind().as_str() == DEFINITION => {
                    module.definition.retain(fact)?;
                }
                _ => {}
            }
        }
        if let Some(value) = owner.and_then(|id| values.get_mut(&id)) {
            match fact.syntax_kind().as_str() {
                VALUE_HEADER => value.header.retain(fact)?,
                DEFINITION => value.definition.retain(fact)?,
                _ => {}
            }
        }
        if let Some(ty) = owner.and_then(|id| types.get_mut(&id)) {
            match fact.syntax_kind().as_str() {
                "fsharp.record.scope" | "fsharp.type_alias.scope" => ty.scope.retain(fact)?,
                "fsharp.record.signature" | "fsharp.type_alias.signature" => {
                    ty.header.retain(fact)?
                }
                DEFINITION => ty.definition.retain(fact)?,
                _ => {}
            }
        }
    }
    let mut owners = HashMap::new();
    let mut children = HashMap::<u64, Vec<&SyntaxFact>>::new();
    let mut roots = HashMap::<u64, Vec<ContentHash>>::new();
    owners
        .try_reserve(modules.len())
        .map_err(allocation_failed)?;
    children
        .try_reserve(modules.len().saturating_add(1))
        .map_err(allocation_failed)?;
    roots
        .try_reserve(modules.len())
        .map_err(allocation_failed)?;
    children.insert(file_scope.local_id(), Vec::new());
    for (&id, module) in &modules {
        cancellation.check()?;
        let (Some(scope), Some(header), Some(definition)) = (
            module.scope.one(),
            module.header.one(),
            module.definition.one(),
        ) else {
            return Ok(None);
        };
        if scope.parent() != Some(id)
            || scope.span() != module.declaration.span()
            || !contains(scope, header)
            || !contains(header, definition)
            || header.span().start_byte() != scope.span().start_byte()
            || !qualified_header_bytes(module, source, cancellation)?
            || owners.insert(scope.local_id(), id).is_some()
        {
            return Ok(None);
        }
        children.entry(scope.local_id()).or_default();
        roots.entry(scope.local_id()).or_default();
    }
    for module in modules.values() {
        cancellation.check()?;
        let Some(parent) = module
            .declaration
            .parent()
            .filter(|parent| *parent == file_scope.local_id() || owners.contains_key(parent))
        else {
            return Ok(None);
        };
        if !by_id.get(&parent).is_some_and(|owner| {
            lexical::module_scope(owner) && contains(owner, module.declaration)
        }) {
            return Ok(None);
        }
        let entries = children.get_mut(&parent).ok_or_else(invalid_capture)?;
        entries.try_reserve(1).map_err(allocation_failed)?;
        entries.push(module.declaration);
        if let Some(parent_roots) = roots.get_mut(&parent) {
            let Some(parts) = module
                .definition
                .one()
                .map(|definition| source_name(definition, source, maximum_name_bytes))
                .transpose()?
                .flatten()
                .and_then(written_components)
            else {
                return Ok(None);
            };
            let Some(name) = parts.first() else {
                return Ok(None);
            };
            parent_roots.try_reserve(1).map_err(allocation_failed)?;
            parent_roots.push(content_hash(canonical_identifier(name).as_bytes()));
        }
    }
    let mut claims = Vec::new();
    claims.try_reserve(facts.len()).map_err(allocation_failed)?;
    for value in values.values() {
        cancellation.check()?;
        let (Some(header), Some(definition), Some(parent)) = (
            value.header.one(),
            value.definition.one(),
            value
                .declaration
                .parent()
                .filter(|parent| owners.contains_key(parent)),
        ) else {
            return Ok(None);
        };
        if header.kind() != SyntaxFactKind::Signature
            || header.parent() != Some(value.declaration.local_id())
            || header.span() != value.declaration.span()
            || definition.kind() != SyntaxFactKind::Occurrence
            || definition.parent() != Some(value.declaration.local_id())
            || !contains(header, definition)
            || !by_id
                .get(&parent)
                .is_some_and(|owner| contains(owner, value.declaration))
        {
            return Ok(None);
        }
        let Some(symbol) = symbols.get(&definition.local_id()).copied() else {
            return Ok(None);
        };
        let Some(parts) = source_name(definition, source, maximum_name_bytes)?
            .and_then(written_components)
            .filter(|parts| parts.len() == 1)
        else {
            return Ok(None);
        };
        let Some(mut path) = qualified_owner_path(
            parent,
            &modules,
            &owners,
            file_scope.local_id(),
            source,
            maximum_name_bytes,
            cancellation,
        )?
        else {
            return Ok(None);
        };
        if path.len() >= rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS {
            return Ok(None);
        }
        let Some(name) = parts.first() else {
            return Ok(None);
        };
        let name = content_hash(canonical_identifier(name).as_bytes());
        path.try_reserve(1).map_err(allocation_failed)?;
        path.push(name);
        let names = roots.get_mut(&parent).ok_or_else(invalid_capture)?;
        names.try_reserve(1).map_err(allocation_failed)?;
        names.push(name);
        let entries = children.get_mut(&parent).ok_or_else(invalid_capture)?;
        entries.try_reserve(1).map_err(allocation_failed)?;
        entries.push(value.declaration);
        claims.push(QualifiedClaim {
            source: value.declaration.span(),
            binding: FSharpBinding::SignatureValue { path, symbol },
        });
    }
    for ty in types.values() {
        cancellation.check()?;
        let declaration = ty.declaration;
        let alias = declaration.syntax_kind().as_str() == ALIAS;
        let marker = markers
            .remove(&(
                declaration.parent(),
                declaration.span().file(),
                declaration.span().end_byte(),
            ))
            .unwrap_or_default();
        let (Some(header), Some(definition), Some(marker), Some(parent)) = (
            ty.header.one(),
            ty.definition.one(),
            marker.one(),
            declaration
                .parent()
                .filter(|parent| owners.contains_key(parent)),
        ) else {
            return Ok(None);
        };
        let ownership = if alias {
            ty.scope.count == 0
                && header.parent() == Some(declaration.local_id())
                && header.span() == declaration.span()
                && definition.parent() == Some(declaration.local_id())
        } else {
            ty.scope.one().is_some_and(|scope| {
                scope.kind() == SyntaxFactKind::Scope
                    && scope.parent() == Some(declaration.local_id())
                    && scope.span() == declaration.span()
                    && header.parent() == Some(scope.local_id())
                    && header.span().start_byte() == declaration.span().start_byte()
                    && header.span().end_byte() < declaration.span().end_byte()
                    && contains(scope, header)
                    && matches!(definition.parent(), Some(id) if id == scope.local_id() || id == declaration.local_id())
            })
        };
        let (expected_header, expected_marker) = if alias {
            ("fsharp.type_alias.signature", ALIAS_MARKER)
        } else {
            ("fsharp.record.signature", RECORD_MARKER)
        };
        if !ownership
            || header.kind() != SyntaxFactKind::Signature
            || header.syntax_kind().as_str() != expected_header
            || definition.kind() != SyntaxFactKind::Occurrence
            || !contains(header, definition)
            || marker.kind() != SyntaxFactKind::Signature
            || marker.syntax_kind().as_str() != expected_marker
            || marker.parent() != Some(parent)
            || marker.span().start_byte() >= declaration.span().start_byte()
            || marker.span().end_byte() != declaration.span().end_byte()
            || !contains(marker, declaration)
            || !by_id
                .get(&parent)
                .is_some_and(|owner| contains(owner, marker))
            || !qualified_plain_type_head(declaration, definition, source, cancellation)?
            || !alias
                && !trivia_between(
                    source,
                    definition.span().end_byte(),
                    header.span().end_byte(),
                    cancellation,
                )?
        {
            return Ok(None);
        }
        let header_end =
            usize::try_from(header.span().end_byte()).map_err(|_| invalid_capture())?;
        if !alias && source.get(header_end) != Some(&b'=') {
            return Ok(None);
        }
        let marker_start =
            usize::try_from(marker.span().start_byte()).map_err(|_| invalid_capture())?;
        let declaration_start =
            usize::try_from(declaration.span().start_byte()).map_err(|_| invalid_capture())?;
        let Some(prefix) = source
            .get(marker_start..declaration_start)
            .and_then(|bytes| bytes.strip_prefix(b"type"))
        else {
            return Ok(None);
        };
        if prefix.is_empty() || trivia_end(prefix, 0, cancellation)? != Some(prefix.len()) {
            return Ok(None);
        }
        let Some(name) = source_name(definition, source, maximum_name_bytes)?
            .filter(|name| unqualified_written_name(definition, name, maximum_name_bytes))
        else {
            return Ok(None);
        };
        let Some(symbol) = symbols.get(&definition.local_id()).copied() else {
            return Ok(None);
        };
        let Some(mut path) = qualified_owner_path(
            parent,
            &modules,
            &owners,
            file_scope.local_id(),
            source,
            maximum_name_bytes,
            cancellation,
        )?
        else {
            return Ok(None);
        };
        if path.len() >= rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS {
            return Ok(None);
        }
        let name = content_hash(canonical_identifier(name).as_bytes());
        path.try_reserve(1).map_err(allocation_failed)?;
        path.push(name);
        let names = roots.get_mut(&parent).ok_or_else(invalid_capture)?;
        names.try_reserve(1).map_err(allocation_failed)?;
        names.push(name);
        let entries = children.get_mut(&parent).ok_or_else(invalid_capture)?;
        entries.try_reserve(1).map_err(allocation_failed)?;
        // The structural type starts at its head. Tiling the whole wrapper
        // also conserves the authored `type` keyword and prevents omitted roots.
        entries.push(marker);
        claims.push(QualifiedClaim {
            source: marker.span(),
            binding: if alias {
                FSharpBinding::SignatureAlias { path, symbol }
            } else {
                FSharpBinding::SignatureType { path, symbol }
            },
        });
    }
    if !markers.is_empty() {
        return Ok(None);
    }
    let mut open_spans = Vec::new();
    let mut seen_opens = HashSet::new();
    open_spans
        .try_reserve(facts.len())
        .map_err(allocation_failed)?;
    seen_opens
        .try_reserve(facts.len())
        .map_err(allocation_failed)?;
    for fact in facts
        .iter()
        .filter(|fact| fact.kind() == SyntaxFactKind::Import)
    {
        cancellation.check()?;
        let Some(parent) = fact.parent().filter(|parent| owners.contains_key(parent)) else {
            return Ok(None);
        };
        let Some(owner) = owners.get(&parent).and_then(|id| modules.get(id)) else {
            return Ok(None);
        };
        if fact.syntax_kind().as_str() != "fsharp.open.import"
            || !seen_opens.insert(fact.span())
            || !contains(owner.declaration, fact)
        {
            return Ok(None);
        }
        let bytes = source_bytes(fact, source)?;
        if !bytes.starts_with(b"open") {
            return Ok(None);
        }
        let Some(position) = trivia_end(bytes, 4, cancellation)?.filter(|position| *position > 4)
        else {
            return Ok(None);
        };
        let text = std::str::from_utf8(bytes.get(position..).ok_or_else(invalid_capture)?)
            .map_err(|_| invalid_capture())?
            .trim();
        let Some(parts) = written_components(text) else {
            return Ok(None);
        };
        let entries = children.get_mut(&parent).ok_or_else(invalid_capture)?;
        entries.try_reserve(1).map_err(allocation_failed)?;
        entries.push(fact);
        claims.push(QualifiedClaim {
            source: fact.span(),
            binding: FSharpBinding::Open {
                path: component_hashes(&parts)?,
                owner: owner.declaration.span(),
            },
        });
        open_spans.push(fact.span());
    }
    for (&scope, entries) in &mut children {
        cancellation.check()?;
        crate::runtime::sort_cancellable_by(entries, cancellation, |left, right| {
            (left.span().start_byte(), left.span().end_byte())
                .cmp(&(right.span().start_byte(), right.span().end_byte()))
        })?;
        let (mut position, container_end, nested) = if scope == file_scope.local_id() {
            (0, end, false)
        } else {
            let Some(module) = owners.get(&scope).and_then(|id| modules.get(id)) else {
                return Ok(None);
            };
            let Some(header) = module.header.one() else {
                return Ok(None);
            };
            (
                header.span().end_byte(),
                module.declaration.span().end_byte(),
                module.declaration.syntax_kind().as_str() == "fsharp.nested_module.declaration",
            )
        };
        for (index, child) in entries.iter().enumerate() {
            cancellation.check()?;
            if child.span().start_byte() < position
                || child.span().end_byte() > container_end
                || !qualified_gap(
                    source,
                    position,
                    child.span().start_byte(),
                    false,
                    false,
                    nested && index == 0,
                    cancellation,
                )?
            {
                return Ok(None);
            }
            position = child.span().end_byte();
        }
        if !trivia_between(source, position, container_end, cancellation)? {
            return Ok(None);
        }
    }
    let mut container_spans = Vec::new();
    container_spans
        .try_reserve_exact(modules.len())
        .map_err(allocation_failed)?;
    for (&scope, names) in &mut roots {
        cancellation.check()?;
        let Some(module) = owners.get(&scope).and_then(|id| modules.get(id)) else {
            return Ok(None);
        };
        let Some(path) = qualified_container_path(
            scope,
            &modules,
            &owners,
            file_scope.local_id(),
            source,
            maximum_name_bytes,
            cancellation,
        )?
        else {
            return Ok(None);
        };
        crate::runtime::sort_cancellable_by(names, cancellation, Ord::cmp)?;
        // Repeated public names cannot become a superficially complete set.
        if names.windows(2).any(|pair| pair[0] == pair[1]) {
            return Ok(None);
        }
        claims.push(QualifiedClaim {
            source: module.declaration.span(),
            binding: FSharpBinding::Container {
                path,
                roots: std::mem::take(names),
            },
        });
        container_spans.push(module.declaration.span());
    }
    crate::runtime::sort_cancellable_by(&mut container_spans, cancellation, Ord::cmp)?;
    crate::runtime::sort_cancellable_by(&mut open_spans, cancellation, Ord::cmp)?;
    if container_spans.windows(2).any(|pair| pair[0] == pair[1]) {
        return Ok(None);
    }
    claims.push(QualifiedClaim {
        source: context.span(),
        binding: FSharpBinding::ImportContext {
            containers: container_spans,
            opens: open_spans,
        },
    });
    let mut order = Vec::new();
    order
        .try_reserve_exact(claims.len())
        .map_err(allocation_failed)?;
    for index in 0..claims.len() {
        cancellation.check()?;
        order.push(index);
    }
    crate::runtime::sort_cancellable_by(&mut order, cancellation, |&left, &right| {
        let left = &claims[left];
        let right = &claims[right];
        (
            left.source.start_byte(),
            left.source.end_byte(),
            super::qualified_claim_order(&left.binding),
        )
            .cmp(&(
                right.source.start_byte(),
                right.source.end_byte(),
                super::qualified_claim_order(&right.binding),
            ))
    })?;
    let mut destination = Vec::new();
    destination
        .try_reserve_exact(claims.len())
        .map_err(allocation_failed)?;
    destination.resize(claims.len(), 0);
    for (sorted, original) in order.into_iter().enumerate() {
        cancellation.check()?;
        destination[original] = sorted;
    }
    // Reorder owned paths through a bounded permutation instead of cloning them.
    for index in 0..claims.len() {
        while destination[index] != index {
            cancellation.check()?;
            let next = destination[index];
            claims.swap(index, next);
            destination.swap(index, next);
        }
    }
    Ok(Some(QualifiedClaims {
        scope: context.span(),
        claims,
    }))
}

fn file_owner(
    scope: &SyntaxFact,
    facts: &HashMap<u64, &SyntaxFact>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let end = u64::try_from(source.len()).map_err(|_| invalid_capture())?;
    if scope.span().end_byte() > end
        || !trivia_between(source, 0, scope.span().start_byte(), cancellation)?
        || !trivia_between(source, scope.span().end_byte(), end, cancellation)?
    {
        return Ok(false);
    }
    let Some(mut parent) = scope.parent().and_then(|id| facts.get(&id).copied()) else {
        return Ok(false);
    };
    if parent.kind() == SyntaxFactKind::Module
        && parent.syntax_kind().as_str() == "fsharp.file.module"
        && parent.span() == scope.span()
    {
        let Some(root) = parent.parent().and_then(|id| facts.get(&id).copied()) else {
            return Ok(false);
        };
        parent = root;
    }
    Ok(parent.kind() == SyntaxFactKind::Root
        && parent.syntax_kind().as_str() == "fsharp.file.root"
        && parent.span() == scope.span()
        && parent.parent().is_none())
}
