//! Source-authenticated plain Rust Struct member ownership shared by providers.
//! Lexical positions reuse the IR impl scanner; native facts still certify syntax.

use std::collections::{BTreeMap, BTreeSet};

use rootlight_cancel::Cancellation;
use rootlight_ids::content_hash;
use rootlight_ir::{RustSourceToken, SourceRef, SourceSpan, rust_source_tokens};

use crate::{
    AdapterError, DiagnosticCode, SinkError, SyntaxFact, SyntaxFactKind, canonical_rust_identifier,
};

#[cfg(test)]
#[path = "rust_inherent_owners_tests.rs"]
mod tests;

/// A same-frame association between one direct method and its authored Struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RustInherentOwnerProof {
    /// Native method declaration fact identifier.
    pub method: u64,
    /// Its unique written method-name Definition capture.
    pub definition: u64,
    /// Unique same-module plain Struct declaration fact identifier.
    pub owner: u64,
    /// Its unique written Struct-name Definition capture.
    pub owner_definition: u64,
    /// Actual enclosing `rust.module.module` local identifier, or file root.
    pub lexical_module: Option<u64>,
}

/// Direct written named Field and its independently authenticated plain Struct.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RustStructFieldProof {
    /// Native direct named field declaration fact identifier.
    pub field: u64,
    /// Its exact written `rust.field_identifier.definition` capture.
    pub definition: u64,
    /// Plain same-frame Struct declaration fact identifier.
    pub owner: u64,
    /// Its exact written Struct-name Definition capture.
    pub owner_definition: u64,
    /// Actual enclosing module declaration identifier, or file root.
    pub lexical_module: Option<u64>,
}

/// Immutable source-bound member proposals; consumers retain their IR authority.
#[derive(Debug, Clone)]
pub struct RustInherentOwnerPlan {
    /// Exact complete physical source authenticated by this plan.
    pub source: SourceRef,
    /// Associations requiring retained-entity and identity checks by consumers.
    pub proofs: Vec<RustInherentOwnerProof>,
    /// Direct named fields requiring independent retained-container agreement.
    pub fields: Vec<RustStructFieldProof>,
    /// Authored impl methods without an admitted ownership proof.
    pub unproven_methods: Vec<u64>,
}

/// Plans written Struct ownership without changing any symbol identity.
///
/// Only complete, non-BOM UTF-8 frames within declared source, token, modeled
/// work and memory admission are eligible. Native root/context/name/type/body/module
/// facts and independent
/// lexical positions must agree. Plain Structs, plain inherent targets and an
/// adjacent ordinary `#[derive(Default)]` wrapper are the initial profile.
/// Complex imports, attributes, namespace ambiguity and recovered/incomplete
/// source produce no associations. Consumers independently retain ownership gaps.
/// No typechecking or macro evaluation occurs. Named fields require guarded native
/// captures plus written name/colon/type extent and unique same-body spelling.
/// Consumers must validate retained records and atomically change semantic
/// containers while preserving their existing identity discriminators.
///
/// # Errors
/// Returns cancellation, malformed frame/graph, checked accounting or bounded
/// allocation errors. The optional proof's source/token/work/memory refusal returns
/// no associations; it never raises a parser or repository capacity limit.
#[expect(
    clippy::too_many_arguments,
    reason = "both consumers supply the same explicit source and admission bounds"
)]
pub fn rust_inherent_owner_plan(
    facts: &[SyntaxFact],
    source: &SourceRef,
    bytes: &[u8],
    complete: bool,
    maximum_facts: usize,
    maximum_source_bytes: usize,
    maximum_tokens: usize,
    maximum_work_units: usize,
    maximum_name_bytes: usize,
    maximum_memory_bytes: usize,
    cancellation: &Cancellation,
) -> Result<RustInherentOwnerPlan, AdapterError> {
    cancellation.check()?;
    if source.span().start_byte() != 0
        || usize::try_from(source.span().end_byte()).ok() != Some(bytes.len())
        || source.content_hash() != content_hash(bytes)
        || facts.len() > maximum_facts
    {
        return Err(invalid_frame());
    }
    let memory = bytes
        .len()
        .checked_mul(128)
        .and_then(|value| {
            facts
                .len()
                .checked_mul(512)
                .and_then(|facts| value.checked_add(facts))
        })
        .ok_or(SinkError::AccountingOverflow)?;
    // This conservative model bounds repeated fact/token scans. It is an
    // admission estimate, not measured work or a parser resource guarantee.
    let scan_work = bytes
        .len()
        .checked_mul(bytes.len())
        .ok_or(SinkError::AccountingOverflow)?;
    let mut plan = RustInherentOwnerPlan {
        source: source.clone(),
        proofs: Vec::new(),
        fields: Vec::new(),
        unproven_methods: Vec::new(),
    };
    if !complete
        || facts
            .iter()
            .any(|fact| fact.kind() == SyntaxFactKind::ErrorRecovery)
        || bytes.starts_with(&[0xef, 0xbb, 0xbf])
        || bytes.len() > maximum_source_bytes
        || scan_work > maximum_work_units
        || memory > maximum_memory_bytes
    {
        return Ok(plan);
    }
    // Fact/depth work must be admitted before even a partial ancestry walk.
    // Same-span forged scopes can be deep while the physical source is tiny.
    let minimum_work = modeled_work(bytes.len(), 0, facts)?;
    cancellation.check()?;
    if minimum_work > maximum_work_units {
        return Ok(plan);
    }
    let graph = Graph::new(facts, source, cancellation)?;
    plan.unproven_methods
        .try_reserve_exact(facts.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for fact in facts {
        cancellation.check()?;
        if fact.syntax_kind().as_str() == "rust.function.declaration"
            && graph.impl_scope(fact)?.is_some()
        {
            plan.unproven_methods.push(fact.local_id());
        }
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid_frame())?;
    if !graph.complete_root(source)? {
        return Ok(plan);
    }
    let tokens = match rust_source_tokens(text, maximum_source_bytes, maximum_tokens) {
        Ok(Some(tokens)) => tokens,
        Ok(None) | Err(rootlight_ir::RustScopeIdentityError::ResourceLimit) => return Ok(plan),
        Err(rootlight_ir::RustScopeIdentityError::AccountingOverflow) => {
            return Err(SinkError::AccountingOverflow.into());
        }
    };
    cancellation.check()?;
    if modeled_work(bytes.len(), tokens.len(), facts)? > maximum_work_units {
        return Ok(plan);
    }
    let Some(code) = Code::new(text, &tokens, cancellation)? else {
        return Ok(plan);
    };
    let mut modules = BTreeMap::from([(None, None)]);
    let mut module_parents = BTreeMap::new();
    let mut ordered: Vec<_> = facts.iter().collect();
    ordered.sort_by_key(|fact| (fact.depth(), fact.local_id()));
    let mut recognized = BTreeSet::new();
    let mut bindings = BTreeMap::<(Option<u64>, String), usize>::new();
    for fact in &ordered {
        cancellation.check()?;
        if fact.syntax_kind().as_str() != "rust.module.module" {
            continue;
        }
        let parent = graph.module(fact)?;
        let Some(expected) = modules.get(&parent).copied() else {
            continue;
        };
        let Some(name) = graph.definition(fact)? else {
            continue;
        };
        let Some((keyword, body)) = code.named_item(fact, name, "mod", expected, true) else {
            continue;
        };
        let Some(body) = body else { continue };
        let Some(canonical) = graph.name(name, text, maximum_name_bytes) else {
            continue;
        };
        modules.insert(Some(fact.local_id()), Some(body));
        module_parents.insert(fact.local_id(), parent);
        recognized.insert(keyword);
        *bindings.entry((parent, canonical)).or_default() += 1;
    }
    let mut structs = BTreeMap::<(Option<u64>, String), Vec<(&SyntaxFact, &SyntaxFact)>>::new();
    let mut allowed_attributes = BTreeSet::new();
    for fact in facts {
        cancellation.check()?;
        if !matches!(
            fact.syntax_kind().as_str(),
            "rust.struct.declaration"
                | "rust.enum.declaration"
                | "rust.trait.declaration"
                | "rust.type.declaration"
        ) {
            continue;
        }
        let module = graph.module(fact)?;
        if !graph.direct_item(fact)? {
            continue;
        }
        let Some(expected) = modules.get(&module).copied() else {
            continue;
        };
        let Some(name) = graph.definition(fact)? else {
            continue;
        };
        if name.syntax_kind().as_str() != "rust.type_identifier.definition" {
            continue;
        }
        let Some(canonical) = graph.name(name, text, maximum_name_bytes) else {
            continue;
        };
        let keyword = match fact.syntax_kind().as_str() {
            "rust.struct.declaration" => "struct",
            "rust.enum.declaration" => "enum",
            "rust.trait.declaration" => "trait",
            _ => "type",
        };
        let Some(start) = code.item_keyword(fact, name, keyword, expected) else {
            continue;
        };
        recognized.insert(start);
        *bindings.entry((module, canonical.clone())).or_default() += 1;
        if keyword == "struct"
            && code
                .named_item(fact, name, keyword, expected, false)
                .is_some()
        {
            structs
                .entry((module, canonical))
                .or_default()
                .push((fact, name));
        }
    }
    let mut opaque = BTreeSet::<Option<u64>>::new();
    for fact in facts {
        cancellation.check()?;
        let module = graph.module(fact)?;
        if !graph.direct_item(fact)? {
            continue;
        }
        let Some(parent) = modules.get(&module).copied() else {
            continue;
        };
        if fact.kind() == SyntaxFactKind::Import {
            let Some((keyword, name)) = code.simple_import(fact, parent, maximum_name_bytes) else {
                opaque.insert(module);
                continue;
            };
            recognized.insert(keyword);
            *bindings.entry((module, name)).or_default() += 1;
        }
        if fact.syntax_kind().as_str() == "rust.attribute.signature" {
            let part = code.in_span(fact.span());
            let spellings: Vec<_> = part.iter().map(|index| code.text(*index)).collect();
            let ordinary = spellings == ["#", "[", "derive", "(", "Default", ")", "]"]
                && part.iter().all(|index| !tokens[*index].literal);
            let attached = structs.iter().any(|((owner_module, _), owners)| {
                *owner_module == module
                    && owners.iter().any(|(owner, _)| {
                        fact.span().end_byte() <= owner.span().start_byte()
                            && code
                                .between(fact.span().end_byte(), owner.span().start_byte())
                                .is_empty()
                    })
            });
            if ordinary && attached {
                if let Some(first) = part.first() {
                    allowed_attributes.insert(*first);
                }
            } else {
                opaque.insert(module);
            }
        }
    }
    for ((module, _), count) in &bindings {
        if *count > 1 {
            opaque.insert(*module);
        }
    }
    for (index, token) in tokens.iter().enumerate() {
        cancellation.check()?;
        if token.literal {
            continue;
        }
        let Some((&module, _)) = modules
            .iter()
            .find(|(_, body)| **body == code.parents[index])
        else {
            continue;
        };
        match code.text(index) {
            "struct" | "enum" | "trait" | "type" | "mod" | "use"
                if !recognized.contains(&index) =>
            {
                opaque.insert(module);
            }
            "union" | "extern" | "macro_rules" | "!" => {
                opaque.insert(module);
            }
            "#" if !allowed_attributes.contains(&index) => {
                opaque.insert(module);
            }
            _ => {}
        }
    }
    for field in facts.iter().filter(|fact| {
        fact.kind() == SyntaxFactKind::Declaration
            && fact.syntax_kind().as_str() == "rust.field.declaration"
    }) {
        cancellation.check()?;
        let Some(owner) = graph.enclosing_declaration(field)? else {
            continue;
        };
        if owner.syntax_kind().as_str() != "rust.struct.declaration" {
            continue;
        }
        let module = graph.module(owner)?;
        if inherited_opaque(module, &opaque, &module_parents) {
            continue;
        }
        let (Some(expected), Some(owner_definition), Some(definition)) = (
            modules.get(&module).copied(),
            graph.definition(owner)?,
            graph.definition(field)?,
        ) else {
            continue;
        };
        if owner_definition.syntax_kind().as_str() != "rust.type_identifier.definition"
            || definition.syntax_kind().as_str() != "rust.field_identifier.definition"
        {
            continue;
        }
        let (Some(owner_name), Some(field_name)) = (
            graph.name(owner_definition, text, maximum_name_bytes),
            graph.name(definition, text, maximum_name_bytes),
        ) else {
            continue;
        };
        if bindings.get(&(module, owner_name.clone())) != Some(&1) {
            continue;
        }
        let Some(candidates) = structs.get(&(module, owner_name)) else {
            continue;
        };
        if candidates.len() != 1 || candidates[0].0.local_id() != owner.local_id() {
            continue;
        }
        let Some((_, Some(body))) =
            code.named_item(owner, owner_definition, "struct", expected, false)
        else {
            continue;
        };
        if !code.field(field, definition, body, &field_name, maximum_name_bytes) {
            continue;
        }
        plan.fields
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        plan.fields.push(RustStructFieldProof {
            field: field.local_id(),
            definition: definition.local_id(),
            owner: owner.local_id(),
            owner_definition: owner_definition.local_id(),
            lexical_module: module,
        });
    }
    let mut proposals = Vec::new();
    proposals
        .try_reserve_exact(plan.unproven_methods.len())
        .map_err(|_| SinkError::AllocationFailed)?;
    for method_id in &plan.unproven_methods {
        cancellation.check()?;
        let method = graph.facts[method_id];
        let Some(scope) = graph.impl_scope(method)? else {
            continue;
        };
        let module = graph.module(scope)?;
        if inherited_opaque(module, &opaque, &module_parents) {
            continue;
        }
        let Some(parent) = modules.get(&module).copied() else {
            continue;
        };
        let captures: Vec<_> = facts
            .iter()
            .filter(|fact| {
                fact.parent() == Some(scope.local_id())
                    && fact.syntax_kind().as_str() == "rust.impl_type.scope_type"
            })
            .collect();
        let [target] = captures.as_slice() else {
            continue;
        };
        if facts.iter().any(|fact| {
            fact.parent() == Some(scope.local_id())
                && fact.syntax_kind().as_str() == "rust.impl_trait.scope_trait"
        }) {
            continue;
        }
        let Some(name) = graph.name(target, text, maximum_name_bytes) else {
            continue;
        };
        let Some((_, Some(body))) = code.named_item(scope, target, "impl", parent, true) else {
            continue;
        };
        if tokens.iter().enumerate().any(|(index, token)| {
            !token.literal
                && code.parents[index] == Some(body)
                && matches!(code.text(index), "#" | "!")
        }) {
            continue;
        }
        if bindings.get(&(module, name.clone())) != Some(&1) {
            continue;
        }
        let Some(owners) = structs.get(&(module, name)) else {
            continue;
        };
        let [(owner, owner_definition)] = owners.as_slice() else {
            continue;
        };
        let Some(definition) = graph.definition(method)? else {
            continue;
        };
        if definition.syntax_kind().as_str() != "rust.identifier.definition"
            || graph.name(definition, text, maximum_name_bytes).is_none()
            || !code.method(method, definition, body, &graph)?
        {
            continue;
        }
        proposals.push(RustInherentOwnerProof {
            method: *method_id,
            definition: definition.local_id(),
            owner: owner.local_id(),
            owner_definition: owner_definition.local_id(),
            lexical_module: module,
        });
    }
    let proven: BTreeSet<_> = proposals.iter().map(|proof| proof.method).collect();
    plan.unproven_methods
        .retain(|method| !proven.contains(method));
    plan.proofs = proposals;
    cancellation.check()?;
    Ok(plan)
}

fn invalid_frame() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("rust-inherent-owner-source-frame")
            .expect("static diagnostic code is valid"),
    }
}

fn modeled_work(bytes: usize, tokens: usize, facts: &[SyntaxFact]) -> Result<usize, AdapterError> {
    let items = tokens
        .checked_add(facts.len())
        .ok_or(SinkError::AccountingOverflow)?;
    let depth = facts
        .iter()
        .map(SyntaxFact::depth)
        .max()
        .unwrap_or(0)
        .checked_add(1)
        .ok_or(SinkError::AccountingOverflow)?;
    bytes
        .checked_mul(bytes)
        .and_then(|scan| {
            items
                .checked_mul(items)
                .and_then(|pairs| pairs.checked_mul(depth))
                .and_then(|walk| walk.checked_mul(16))
                .and_then(|walk| scan.checked_add(walk))
        })
        .ok_or_else(|| SinkError::AccountingOverflow.into())
}

fn inherited_opaque(
    mut module: Option<u64>,
    opaque: &BTreeSet<Option<u64>>,
    parents: &BTreeMap<u64, Option<u64>>,
) -> bool {
    loop {
        if opaque.contains(&module) {
            return true;
        }
        let Some(id) = module else { return false };
        module = parents.get(&id).copied().flatten();
    }
}

struct Graph<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    cancellation: Cancellation,
}

struct Ancestors<'a, 'g> {
    facts: &'g BTreeMap<u64, &'a SyntaxFact>,
    next: Option<u64>,
    cancellation: &'g Cancellation,
}

impl<'a> Iterator for Ancestors<'a, '_> {
    type Item = Result<&'a SyntaxFact, AdapterError>;

    fn next(&mut self) -> Option<Self::Item> {
        let id = self.next?;
        if let Err(error) = self.cancellation.check() {
            self.next = None;
            return Some(Err(error.into()));
        }
        let parent = self.facts[&id];
        self.next = parent.parent();
        Some(Ok(parent))
    }
}

impl<'a> Graph<'a> {
    fn new(
        facts: &'a [SyntaxFact],
        source: &SourceRef,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut graph = Self {
            facts: BTreeMap::new(),
            cancellation: cancellation.clone(),
        };
        for fact in facts {
            cancellation.check()?;
            if fact.span().file() != source.span().file()
                || fact.span().start_byte() < source.span().start_byte()
                || fact.span().end_byte() > source.span().end_byte()
                || graph.facts.insert(fact.local_id(), fact).is_some()
            {
                return Err(invalid_frame());
            }
        }
        for fact in facts {
            cancellation.check()?;
            if let Some(parent) = fact.parent() {
                let Some(parent) = graph.facts.get(&parent) else {
                    return Err(invalid_frame());
                };
                if parent.depth().checked_add(1) != Some(fact.depth())
                    || parent.span().start_byte() > fact.span().start_byte()
                    || parent.span().end_byte() < fact.span().end_byte()
                {
                    return Err(invalid_frame());
                }
            }
        }
        Ok(graph)
    }
    fn ancestors<'g>(&'g self, fact: &SyntaxFact) -> Ancestors<'a, 'g> {
        Ancestors {
            facts: &self.facts,
            next: fact.parent(),
            cancellation: &self.cancellation,
        }
    }
    fn module(&self, fact: &SyntaxFact) -> Result<Option<u64>, AdapterError> {
        for parent in self.ancestors(fact) {
            let parent = parent?;
            if parent.syntax_kind().as_str() == "rust.module.module" {
                return Ok(Some(parent.local_id()));
            }
        }
        Ok(None)
    }
    fn direct_item(&self, fact: &SyntaxFact) -> Result<bool, AdapterError> {
        for parent in self.ancestors(fact) {
            let parent = parent?;
            if parent.syntax_kind().as_str() == "rust.module.module"
                || parent.kind() == SyntaxFactKind::Root
            {
                return Ok(true);
            }
            if matches!(
                parent.kind(),
                SyntaxFactKind::Declaration | SyntaxFactKind::Scope | SyntaxFactKind::Module
            ) {
                return Ok(false);
            }
        }
        Ok(false)
    }
    fn impl_scope(&self, fact: &SyntaxFact) -> Result<Option<&'a SyntaxFact>, AdapterError> {
        for parent in self.ancestors(fact) {
            let parent = parent?;
            if parent.syntax_kind().as_str() == "rust.impl.scope" {
                return Ok(self.direct_item(parent)?.then_some(parent));
            }
            if matches!(
                parent.kind(),
                SyntaxFactKind::Declaration | SyntaxFactKind::Module
            ) {
                return Ok(None);
            }
        }
        Ok(None)
    }
    fn enclosing_declaration(
        &self,
        fact: &SyntaxFact,
    ) -> Result<Option<&'a SyntaxFact>, AdapterError> {
        for parent in self.ancestors(fact) {
            let parent = parent?;
            if matches!(
                parent.kind(),
                SyntaxFactKind::Declaration | SyntaxFactKind::Module
            ) {
                return Ok(Some(parent));
            }
        }
        Ok(None)
    }
    fn definition(&self, declaration: &SyntaxFact) -> Result<Option<&'a SyntaxFact>, AdapterError> {
        let mut name = None;
        for fact in self.facts.values().copied() {
            self.cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str().ends_with(".definition")
                && self.enclosing_declaration(fact)?.map(SyntaxFact::local_id)
                    == Some(declaration.local_id())
            {
                if name.is_some() {
                    return Ok(None);
                }
                name = Some(fact);
            }
        }
        Ok(name)
    }
    fn name(&self, fact: &SyntaxFact, text: &str, maximum: usize) -> Option<String> {
        let start = usize::try_from(fact.span().start_byte()).ok()?;
        let end = usize::try_from(fact.span().end_byte()).ok()?;
        canonical_rust_identifier(text.get(start..end)?, maximum).map(|name| name.into_owned())
    }
    fn complete_root(&self, source: &SourceRef) -> Result<bool, AdapterError> {
        let mut root = None;
        for fact in self.facts.values().copied() {
            self.cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Root
                && fact.syntax_kind().as_str() == "rust.file.root"
            {
                if root.is_some() {
                    return Ok(false);
                }
                root = Some(fact);
            }
        }
        let Some(root) = root else { return Ok(false) };
        if root.parent().is_some() || root.depth() != 0 || root.span() != source.span() {
            return Ok(false);
        }
        let mut contexts = 0usize;
        for fact in self.facts.values().copied() {
            self.cancellation.check()?;
            if fact.syntax_kind().as_str() != "rust.proof_context.signature"
                || fact.span() != source.span()
            {
                continue;
            }
            let mut reaches_root = false;
            for parent in self.ancestors(fact) {
                let parent = parent?;
                if parent.local_id() == root.local_id() {
                    reaches_root = true;
                    break;
                }
                if parent.span() != source.span() {
                    break;
                }
            }
            if reaches_root {
                contexts = contexts
                    .checked_add(1)
                    .ok_or(SinkError::AccountingOverflow)?;
            }
        }
        Ok(contexts == 1)
    }
}

struct Brace {
    open: usize,
    close: usize,
    parent: Option<usize>,
}
struct Code<'a> {
    text: &'a str,
    tokens: &'a [RustSourceToken],
    parents: Vec<Option<usize>>,
    braces: Vec<Brace>,
}

impl<'a> Code<'a> {
    fn new(
        text: &'a str,
        tokens: &'a [RustSourceToken],
        cancellation: &Cancellation,
    ) -> Result<Option<Self>, AdapterError> {
        let mut code = Self {
            text,
            tokens,
            parents: Vec::new(),
            braces: Vec::new(),
        };
        code.parents
            .try_reserve_exact(tokens.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        code.braces
            .try_reserve_exact(tokens.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut stack = Vec::new();
        stack
            .try_reserve_exact(tokens.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for (index, token) in tokens.iter().enumerate() {
            cancellation.check()?;
            code.parents.push(stack.last().copied());
            if token.literal {
                continue;
            }
            match code.text(index) {
                "{" => {
                    let id = code.braces.len();
                    code.braces.push(Brace {
                        open: index,
                        close: index,
                        parent: stack.last().copied(),
                    });
                    stack.push(id);
                }
                "}" => {
                    let Some(id) = stack.pop() else {
                        return Ok(None);
                    };
                    code.braces[id].close = index;
                }
                _ => {}
            }
        }
        Ok(stack.is_empty().then_some(code))
    }
    fn text(&self, index: usize) -> &'a str {
        let token = self.tokens[index];
        &self.text[token.start_byte..token.end_byte]
    }
    fn between(&self, start: u64, end: u64) -> Vec<usize> {
        self.tokens
            .iter()
            .enumerate()
            .filter(|(_, token)| {
                u64::try_from(token.start_byte)
                    .ok()
                    .is_some_and(|position| start <= position && position < end)
            })
            .map(|(index, _)| index)
            .collect()
    }
    fn in_span(&self, span: SourceSpan) -> Vec<usize> {
        self.between(span.start_byte(), span.end_byte())
    }
    fn name_token(&self, name: &SyntaxFact) -> Option<usize> {
        self.tokens.iter().position(|token| {
            !token.literal
                && u64::try_from(token.start_byte).ok() == Some(name.span().start_byte())
                && u64::try_from(token.end_byte).ok() == Some(name.span().end_byte())
        })
    }
    fn item_keyword(
        &self,
        item: &SyntaxFact,
        name: &SyntaxFact,
        keyword: &str,
        parent: Option<usize>,
    ) -> Option<usize> {
        let name = self.name_token(name)?;
        if self.parents[name] != parent {
            return None;
        }
        let before = self.between(
            item.span().start_byte(),
            u64::try_from(self.tokens[name].start_byte).ok()?,
        );
        let words: Vec<_> = before.iter().map(|index| self.text(*index)).collect();
        if words != [keyword] && (keyword == "impl" || words != ["pub", keyword]) {
            return None;
        }
        before.last().copied()
    }
    fn named_item(
        &self,
        item: &SyntaxFact,
        name: &SyntaxFact,
        keyword: &str,
        parent: Option<usize>,
        require_body: bool,
    ) -> Option<(usize, Option<usize>)> {
        let start = self.item_keyword(item, name, keyword, parent)?;
        let after = self.between(name.span().end_byte(), item.span().end_byte());
        let first = *after.first()?;
        if !require_body
            && self.text(first) == ";"
            && u64::try_from(self.tokens[first].end_byte).ok() == Some(item.span().end_byte())
        {
            return Some((start, None));
        }
        let body = self.braces.iter().position(|brace| {
            brace.open == first
                && brace.parent == parent
                && u64::try_from(self.tokens[brace.close].end_byte).ok()
                    == Some(item.span().end_byte())
        })?;
        Some((start, Some(body)))
    }
    fn simple_import(
        &self,
        fact: &SyntaxFact,
        parent: Option<usize>,
        maximum: usize,
    ) -> Option<(usize, String)> {
        let part = self.in_span(fact.span());
        if part.len() < 3
            || self.text(part[0]) != "use"
            || self.text(*part.last()?) != ";"
            || part
                .iter()
                .any(|index| self.tokens[*index].literal || self.parents[*index] != parent)
        {
            return None;
        }
        let names = &part[1..part.len() - 1];
        let mut index = 0;
        let mut last = None;
        while index < names.len() {
            last = canonical_rust_identifier(self.text(names[index]), maximum)
                .map(|name| name.into_owned());
            last.as_ref()?;
            index += 1;
            if index < names.len() {
                if index + 1 >= names.len()
                    || self.text(names[index]) != ":"
                    || self.text(names[index + 1]) != ":"
                {
                    return None;
                }
                index += 2;
                if index == names.len() {
                    return None;
                }
            }
        }
        Some((part[0], last?))
    }
    fn method(
        &self,
        method: &SyntaxFact,
        name: &SyntaxFact,
        body: usize,
        graph: &Graph<'_>,
    ) -> Result<bool, AdapterError> {
        let Some(name_token) = self.name_token(name) else {
            return Ok(false);
        };
        if self.parents[name_token] != Some(body) {
            return Ok(false);
        }
        let prefix = self.between(method.span().start_byte(), name.span().start_byte());
        if prefix.last().is_none_or(|index| self.text(*index) != "fn") {
            return Ok(false);
        }
        let mut signature = None;
        for fact in graph.facts.values().copied() {
            graph.cancellation.check()?;
            if fact.syntax_kind().as_str() == "rust.body.signature"
                && graph.enclosing_declaration(fact)?.map(SyntaxFact::local_id)
                    == Some(method.local_id())
            {
                if signature.is_some() {
                    return Ok(false);
                }
                signature = Some(fact);
            }
        }
        let Some(signature) = signature else {
            return Ok(false);
        };
        Ok(self.braces.iter().any(|brace| {
            brace.parent == Some(body)
                && u64::try_from(self.tokens[brace.open].start_byte).ok()
                    == Some(signature.span().start_byte())
                && u64::try_from(self.tokens[brace.close].end_byte).ok()
                    == Some(signature.span().end_byte())
                && signature.span().end_byte() == method.span().end_byte()
        }))
    }
    fn field(
        &self,
        field: &SyntaxFact,
        definition: &SyntaxFact,
        body: usize,
        name: &str,
        maximum: usize,
    ) -> bool {
        let Some(named) = self.name_token(definition) else {
            return false;
        };
        let Some(fields) = self.direct_fields(body, maximum) else {
            return false;
        };
        let matching = fields
            .iter()
            .filter(|candidate| candidate.2 == name)
            .collect::<Vec<_>>();
        let [candidate] = matching.as_slice() else {
            return false;
        };
        candidate.0 == named
            && u64::try_from(self.tokens[candidate.1].start_byte).ok()
                == Some(field.span().start_byte())
            && u64::try_from(self.tokens[candidate.3].end_byte).ok()
                == Some(field.span().end_byte())
    }

    fn direct_fields(
        &self,
        body: usize,
        maximum: usize,
    ) -> Option<Vec<(usize, usize, String, usize)>> {
        let brace = self.braces.get(body)?;
        let mut fields = Vec::new();
        let mut start = brace.open.checked_add(1)?;
        while start < brace.close {
            let field_start = start;
            if self.text(start) == "pub" {
                start = start.checked_add(1)?;
            }
            if start >= brace.close || self.tokens[start].literal {
                return None;
            }
            let name = canonical_rust_identifier(self.text(start), maximum)?.into_owned();
            let named = start;
            let colon = start.checked_add(1)?;
            let type_start = start.checked_add(2)?;
            if type_start >= brace.close || self.text(colon) != ":" || self.text(type_start) == ":"
            {
                return None;
            }
            let mut delimiters = Vec::new();
            let mut end = type_start;
            while end < brace.close {
                let token = self.tokens[end];
                if !token.literal {
                    let written = self.text(end);
                    if written == "," && delimiters.is_empty() {
                        break;
                    }
                    match written {
                        "(" => delimiters.push(")"),
                        "[" => delimiters.push("]"),
                        "{" => delimiters.push("}"),
                        "<" => delimiters.push(">"),
                        // A function pointer return arrow is not a generic closer.
                        ">" if end > type_start && self.text(end - 1) == "-" => {}
                        ")" | "]" | "}" | ">" => {
                            if delimiters.pop() != Some(written) {
                                return None;
                            }
                        }
                        "#" | ";" | "=" if delimiters.is_empty() => return None,
                        _ => {}
                    }
                }
                end = end.checked_add(1)?;
            }
            if !delimiters.is_empty() || end == type_start {
                return None;
            }
            fields.push((named, field_start, name, end.checked_sub(1)?));
            start = end.checked_add(1)?;
        }
        Some(fields)
    }
}
