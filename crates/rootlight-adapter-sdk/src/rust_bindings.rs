//! Shared source-backed Rust local environments.
//! Syntax extents are physical coordinates; callers supply decoded names so
//! UTF-16 sources never use physical offsets to slice projected UTF-8 text.

use std::{borrow::Cow, collections::BTreeMap};

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, content_hash};
use rootlight_ir::SourceSpan;

use crate::{
    AdapterError, SinkError, SyntaxFact, SyntaxFactKind, derive_structural_occurrence_identity,
};

/// Canonicalizes one native Rust identifier without changing its source span.
/// The raw-identifier prefix is syntax, and Rust name equality uses NFC. The
/// authored byte length, Unicode XID properties and wildcard are checked first.
#[must_use]
pub fn canonical_rust_identifier(text: &str, maximum_bytes: usize) -> Option<Cow<'_, str>> {
    if text.len() > maximum_bytes {
        return None;
    }
    let name = text.strip_prefix("r#").unwrap_or(text);
    if text.starts_with("r#") && matches!(name, "crate" | "self" | "Self" | "super") {
        return None;
    }
    let mut characters = name.chars();
    let first = characters.next()?;
    if name == "_"
        || !(first == '_' || rootlight_unicode::is_xid_start(first))
        || !characters.all(rootlight_unicode::is_xid_continue)
    {
        return None;
    }
    if name.is_ascii() {
        return Some(Cow::Borrowed(name));
    }
    let normalized = rootlight_unicode::normalize_nfc(name);
    if normalized.len() > maximum_bytes {
        return None;
    }
    if normalized == name {
        Some(Cow::Borrowed(name))
    } else {
        Some(Cow::Owned(normalized))
    }
}

/// The result of consulting the written local value environment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RustBindingLookup {
    /// A source-proven binding's definition fact identifier.
    Definition(u64),
    /// An unavailable inner binding prevents guessing an outer target.
    Blocked,
    /// The occurrence has no source-proven local value binding.
    NotLocal,
}

/// A unique written free-function definition, before resolver adoption.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RustDirectDefinition {
    /// Native declaration fact that owns the function entity.
    pub declaration: u64,
    /// Exact function-name definition witness.
    pub definition: u64,
    /// Rust name identity, independent of either authored spelling.
    pub canonical_name: ContentHash,
}

#[derive(Debug)]
struct Scope {
    parent: Option<u64>,
    span: SourceSpan,
    label: String,
    identity: [u8; 32],
}

#[derive(Debug)]
struct Binding {
    definition: Option<u64>,
    activation: u64,
}

#[derive(Debug, Default)]
struct EnvironmentIndex {
    spans: Vec<SourceSpan>,
    ends: Vec<u64>,
}

impl EnvironmentIndex {
    fn contains(&self, position: u64) -> bool {
        let count = self
            .spans
            .partition_point(|span| span.start_byte() <= position);
        count
            .checked_sub(1)
            .and_then(|index| self.ends.get(index))
            .is_some_and(|end| position < *end)
    }
}

/// A bounded, parser-independent index of Rust lexical locals and owners.
#[derive(Debug, Default)]
pub struct RustBindings {
    scopes: BTreeMap<u64, Scope>,
    fact_scopes: BTreeMap<u64, Option<u64>>,
    positions: BTreeMap<u64, u64>,
    owners: BTreeMap<u64, Option<u64>>,
    definitions: BTreeMap<u64, u64>,
    bindings: BTreeMap<u64, BTreeMap<String, Vec<Binding>>>,
    unavailable_names: BTreeMap<u64, Vec<u64>>,
    blocked_bindings: BTreeMap<u64, BTreeMap<String, Vec<u64>>>,
    unavailable_environments: BTreeMap<u64, BTreeMap<Option<String>, EnvironmentIndex>>,
    identities: BTreeMap<u64, [u8; 32]>,
    direct_definitions: BTreeMap<u64, BTreeMap<String, Vec<RustDirectDefinition>>>,
    direct_barriers: std::collections::BTreeSet<u64>,
    direct_context_complete: bool,
}

impl RustBindings {
    /// Builds local environments from a complete, already bounded fact stream.
    ///
    /// Names are decoded, source-backed spellings keyed by syntax-local fact ID.
    /// Missing extents, names or definition witnesses never authorize a target.
    /// Closure lookups cross into their creation environment; item boundaries
    /// stop local capture. Identities omit source bytes, hashes and offsets.
    ///
    /// # Errors
    /// Returns cancellation, allocation or string-limit errors without publishing
    /// a partial environment.
    pub fn new(
        facts: &[SyntaxFact],
        names: &BTreeMap<u64, String>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let by_id: BTreeMap<_, _> = facts.iter().map(|fact| (fact.local_id(), fact)).collect();
        let mut ordered: Vec<_> = facts.iter().collect();
        ordered.sort_by(|left, right| crate::structural_syntax_fact_order(left, right));
        let mut result = Self::default();
        let mut positions = BTreeMap::<(Option<u64>, String), u64>::new();
        for fact in &ordered {
            cancellation.check()?;
            if !is_scope(fact) {
                continue;
            }
            let parent = ancestor(fact.parent(), &by_id, facts.len(), cancellation, is_scope)?;
            let label = fact.syntax_kind().as_str();
            let next = positions.entry((parent, label.to_owned())).or_default();
            let ordinal = if fact.kind() == SyntaxFactKind::Declaration && !is_closure(fact) {
                0
            } else {
                *next
            };
            *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
            let parent_identity = parent
                .and_then(|id| result.scopes.get(&id))
                .map(|scope| scope.identity);
            let identity = derive_structural_occurrence_identity(
                "rootlight.rust-lexical-scope/1",
                parent_identity,
                label,
                ordinal,
            );
            result.scopes.insert(
                fact.local_id(),
                Scope {
                    parent,
                    span: fact.span(),
                    label: label.to_owned(),
                    identity,
                },
            );
        }
        for fact in &ordered {
            cancellation.check()?;
            let scope = ancestor(fact.parent(), &by_id, facts.len(), cancellation, is_scope)?;
            result.fact_scopes.insert(fact.local_id(), scope);
            result
                .positions
                .insert(fact.local_id(), fact.span().start_byte());
            if is_local(fact) || is_closure(fact) {
                let owner = ancestor(
                    fact.parent(),
                    &by_id,
                    facts.len(),
                    cancellation,
                    |ancestor| {
                        matches!(
                            ancestor.syntax_kind().as_str(),
                            "rust.function.declaration" | "rust.anonymous_function.declaration"
                        )
                    },
                )?;
                result.owners.insert(fact.local_id(), owner);
            }
            if fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str().ends_with(".definition")
            {
                let declaration =
                    ancestor(fact.parent(), &by_id, facts.len(), cancellation, is_local)?;
                if let Some(declaration) = declaration {
                    result.definitions.insert(fact.local_id(), declaration);
                }
            }
        }
        let mut definition_witnesses = BTreeMap::<u64, Vec<u64>>::new();
        for (definition, declaration) in &result.definitions {
            cancellation.check()?;
            definition_witnesses
                .entry(*declaration)
                .or_default()
                .push(*definition);
        }
        let mut active_extents = Vec::<&SyntaxFact>::new();
        let mut activations = BTreeMap::new();
        let mut bodies = BTreeMap::<u64, Vec<SourceSpan>>::new();
        let mut pattern_names = BTreeMap::<u64, Option<std::collections::BTreeSet<String>>>::new();
        for fact in &ordered {
            cancellation.check()?;
            if matches!(
                fact.syntax_kind().as_str(),
                "rust.pattern_binding_unavailable.reference"
                    | "rust.pattern_macro_bindings_unavailable.signature"
            ) && let Some(owner) = ancestor(
                fact.parent(),
                &by_id,
                facts.len(),
                cancellation,
                |ancestor| ancestor.syntax_kind().as_str() == "rust.pattern_environment.scope",
            )? {
                let entry = pattern_names
                    .entry(owner)
                    .or_insert_with(|| Some(Default::default()));
                if fact.syntax_kind().as_str()
                    == "rust.pattern_macro_bindings_unavailable.signature"
                {
                    *entry = None;
                } else if let Some(collected) = entry.as_mut() {
                    if let Some(name) = names
                        .get(&fact.local_id())
                        .and_then(|name| canonical_rust_identifier(name, maximum_name_bytes))
                    {
                        collected.insert(name.into_owned());
                    } else {
                        *entry = None;
                    }
                }
            }
        }
        for fact in &ordered {
            cancellation.check()?;
            if fact.syntax_kind().as_str() == "rust.unavailable_pattern_environment.signature" {
                let owner = ancestor(
                    fact.parent(),
                    &by_id,
                    facts.len(),
                    cancellation,
                    |ancestor| ancestor.syntax_kind().as_str() == "rust.pattern_environment.scope",
                )?;
                if let Some(names) = owner
                    .and_then(|id| pattern_names.get(&id))
                    .and_then(Option::as_ref)
                {
                    for name in names {
                        result
                            .unavailable_environments
                            .entry(owner.unwrap_or(0))
                            .or_default()
                            .entry(Some(name.clone()))
                            .or_default()
                            .spans
                            .push(fact.span());
                    }
                } else {
                    result
                        .unavailable_environments
                        .entry(owner.unwrap_or(0))
                        .or_default()
                        .entry(None)
                        .or_default()
                        .spans
                        .push(fact.span());
                }
            }
        }

        for fact in &ordered {
            cancellation.check()?;
            if fact.syntax_kind().as_str() == "rust.body.signature"
                && let Some(owner) = ancestor(
                    fact.parent(),
                    &by_id,
                    facts.len(),
                    cancellation,
                    |ancestor| {
                        matches!(
                            ancestor.syntax_kind().as_str(),
                            "rust.function.declaration" | "rust.anonymous_function.declaration"
                        )
                    },
                )?
            {
                bodies.entry(owner).or_default().push(fact.span());
            }
        }
        for fact in &ordered {
            cancellation.check()?;
            while active_extents
                .last()
                .is_some_and(|extent| extent.span().end_byte() <= fact.span().start_byte())
            {
                active_extents.pop();
            }
            if matches!(
                fact.syntax_kind().as_str(),
                "rust.let.signature" | "rust.parameters.signature"
            ) {
                active_extents.push(fact);
            } else if is_local(fact)
                || matches!(
                    fact.syntax_kind().as_str(),
                    "rust.pattern_binding_unavailable.reference"
                        | "rust.pattern_macro_bindings_unavailable.signature"
                )
            {
                let label = if fact.syntax_kind().as_str() == "rust.parameter.declaration" {
                    "rust.parameters.signature"
                } else {
                    "rust.let.signature"
                };
                activations.insert(
                    fact.local_id(),
                    active_extents
                        .iter()
                        .rev()
                        .find(|extent| {
                            (matches!(
                                fact.syntax_kind().as_str(),
                                "rust.pattern_binding_unavailable.reference"
                                    | "rust.pattern_macro_bindings_unavailable.signature"
                            ) || extent.syntax_kind().as_str() == label)
                                && extent.span().start_byte() <= fact.span().start_byte()
                                && extent.span().end_byte() >= fact.span().end_byte()
                        })
                        .map(|extent| extent.span().end_byte()),
                );
            }
        }
        let mut ordinals = BTreeMap::<(u64, String, String), u64>::new();
        for fact in &ordered {
            cancellation.check()?;
            if matches!(
                fact.syntax_kind().as_str(),
                "rust.pattern_binding_unavailable.reference"
                    | "rust.pattern_macro_bindings_unavailable.signature"
            ) {
                let activation = activations.get(&fact.local_id()).copied().flatten();
                if let Some(activation) = activation
                    && let Some(scope) = result.fact_scopes.get(&fact.local_id()).copied().flatten()
                {
                    if let Some(name) = names
                        .get(&fact.local_id())
                        .and_then(|name| canonical_rust_identifier(name, maximum_name_bytes))
                    {
                        result
                            .blocked_bindings
                            .entry(scope)
                            .or_default()
                            .entry(name.into_owned())
                            .or_default()
                            .push(activation);
                    } else {
                        result
                            .unavailable_names
                            .entry(scope)
                            .or_default()
                            .push(activation);
                    }
                }
                continue;
            }

            if is_closure(fact) {
                if let Some(scope) = result.scopes.get(&fact.local_id()) {
                    result.identities.insert(fact.local_id(), scope.identity);
                }
                continue;
            }
            if !is_local(fact) {
                continue;
            }
            let Some(scope) = result.fact_scopes.get(&fact.local_id()).copied().flatten() else {
                continue;
            };
            let activation = if fact.syntax_kind().as_str() == "rust.parameter.declaration" {
                result
                    .owners
                    .get(&fact.local_id())
                    .copied()
                    .flatten()
                    .and_then(|owner| bodies.get(&owner))
                    .and_then(|bodies| match bodies.as_slice() {
                        [body] => Some(body.start_byte()),
                        _ => None,
                    })
            } else {
                activations.get(&fact.local_id()).copied().flatten()
            };
            let Some(written_name) = names.get(&fact.local_id()).or_else(|| {
                definition_witnesses
                    .get(&fact.local_id())
                    .and_then(|definitions| definitions.first())
                    .and_then(|definition| names.get(definition))
            }) else {
                result
                    .unavailable_names
                    .entry(scope)
                    .or_default()
                    .push(activation.unwrap_or(0));
                continue;
            };
            if written_name.len() > maximum_name_bytes {
                return Err(AdapterError::Sink(SinkError::StreamLimit {
                    resource: crate::ResourceKind::StringBytes,
                    observed: written_name.len(),
                    limit: maximum_name_bytes,
                }));
            }
            let Some(name) = canonical_rust_identifier(written_name, maximum_name_bytes) else {
                result
                    .unavailable_names
                    .entry(scope)
                    .or_default()
                    .push(activation.unwrap_or(0));
                continue;
            };
            let definition = match definition_witnesses
                .get(&fact.local_id())
                .map(Vec::as_slice)
                .unwrap_or(&[])
            {
                [definition] => Some(*definition),
                _ => None,
            };
            let next = ordinals
                .entry((
                    scope,
                    fact.syntax_kind().as_str().to_owned(),
                    name.to_string(),
                ))
                .or_default();
            let ordinal = *next;
            *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
            let parent_identity = result.scopes.get(&scope).map(|scope| scope.identity);
            result.identities.insert(
                fact.local_id(),
                derive_structural_occurrence_identity(
                    "rootlight.rust-local-declaration/1",
                    parent_identity,
                    fact.syntax_kind().as_str(),
                    ordinal,
                ),
            );
            let bindings = result
                .bindings
                .entry(scope)
                .or_default()
                .entry(name.to_string())
                .or_default();
            bindings
                .try_reserve(1)
                .map_err(|_| SinkError::AllocationFailed)?;
            bindings.push(Binding {
                definition: definition.filter(|_| activation.is_some()),
                activation: activation.unwrap_or(0),
            });
        }
        for environment in result
            .unavailable_environments
            .values_mut()
            .flat_map(|environments| environments.values_mut())
        {
            environment
                .spans
                .sort_by_key(|span| (span.start_byte(), span.end_byte()));
            let mut maximum_end = 0;
            for span in &environment.spans {
                cancellation.check()?;
                maximum_end = maximum_end.max(span.end_byte());
                environment.ends.push(maximum_end);
            }
        }
        let roots: Vec<_> = facts
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "rust.file.root")
            .collect();
        result.direct_context_complete = match roots.as_slice() {
            [root] if root.kind() == SyntaxFactKind::Root && root.parent().is_none() => {
                facts
                    .iter()
                    .filter(|fact| {
                        fact.kind() == SyntaxFactKind::Signature
                            && fact.syntax_kind().as_str() == "rust.proof_context.signature"
                            && fact.span() == root.span()
                            && fact.parent() == Some(root.local_id())
                    })
                    .count()
                    == 1
            }
            _ => false,
        };
        let mut function_definitions = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() == "rust.identifier.definition"
                && let Some(parent) = fact.parent()
            {
                function_definitions.entry(parent).or_default().push(fact);
            }
            if matches!(
                fact.syntax_kind().as_str(),
                "rust.attribute.signature"
                    | "rust.namespace_barrier.signature"
                    | "rust.macro_bindings_unavailable.signature"
            ) {
                let owner = ancestor(fact.parent(), &by_id, facts.len(), cancellation, |scope| {
                    is_scope(scope)
                        && !(matches!(
                            scope.kind(),
                            SyntaxFactKind::Declaration | SyntaxFactKind::Module
                        ) && scope.span() == fact.span())
                })?;
                if let Some(owner) = owner {
                    result.direct_barriers.insert(owner);
                } else {
                    result.direct_context_complete = false;
                }
            }
        }
        for declaration in facts
            .iter()
            .filter(|fact| fact.syntax_kind().as_str() == "rust.function.declaration")
        {
            cancellation.check()?;
            let Some(module) = result
                .fact_scopes
                .get(&declaration.local_id())
                .copied()
                .flatten()
                .filter(|module| {
                    result.scopes.get(module).is_some_and(|scope| {
                        matches!(
                            scope.label.as_str(),
                            "rust.file.root" | "rust.module.module"
                        )
                    })
                })
            else {
                continue;
            };
            let definitions = function_definitions
                .get(&declaration.local_id())
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            let [definition] = definitions else {
                result.direct_barriers.insert(module);
                continue;
            };
            let Some(name) = names
                .get(&definition.local_id())
                .and_then(|name| canonical_rust_identifier(name, maximum_name_bytes))
            else {
                result.direct_barriers.insert(module);
                continue;
            };
            result
                .direct_definitions
                .entry(module)
                .or_default()
                .entry(name.to_string())
                .or_default()
                .push(RustDirectDefinition {
                    declaration: declaration.local_id(),
                    definition: definition.local_id(),
                    canonical_name: content_hash(name.as_bytes()),
                });
        }
        Ok(result)
    }

    /// Returns the nearest written callable owner of a local or closure declaration.
    #[must_use]
    pub fn declaration_owner(&self, declaration: u64) -> Option<u64> {
        self.owners.get(&declaration).copied().flatten()
    }

    /// Returns a source-occurrence discriminator shared by both lowering paths.
    /// Named items preserve their supplied scope identity; local and closure
    /// identities additionally bind lexical scope and same-name occurrence order.
    #[must_use]
    pub fn identity_scope(&self, declaration: u64, parent: Option<[u8; 32]>) -> Option<[u8; 32]> {
        let Some(identity) = self.identities.get(&declaration) else {
            return parent;
        };
        let mut hash = blake3::Hasher::new_derive_key("rootlight.rust-source-declaration/1");
        match parent {
            Some(parent) => {
                hash.update(&[1]);
                hash.update(&parent);
            }
            None => {
                hash.update(&[0]);
            }
        }
        hash.update(identity);
        Some(*hash.finalize().as_bytes())
    }

    /// Maps a proven definition witness to its leaf declaration.
    #[must_use]
    pub fn definition_declaration(&self, definition: u64) -> Option<u64> {
        self.definitions.get(&definition).copied()
    }

    /// Selects a same-module free function only after local binding and context proof.
    /// Legacy, receiver, qualified and incomplete-context calls remain unproven.
    ///
    /// # Errors
    /// Returns cancellation errors; every ancestor walk is bounded by retained scopes.
    pub fn direct_call(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<RustDirectDefinition>, AdapterError> {
        cancellation.check()?;
        if !self.direct_context_complete
            || fact.kind() != SyntaxFactKind::Occurrence
            || fact.syntax_kind().as_str() != "rust.direct_call.call"
            || self.lookup(fact, name, cancellation)? != RustBindingLookup::NotLocal
        {
            return Ok(None);
        }
        let Some(name) = canonical_rust_identifier(name, usize::MAX) else {
            return Ok(None);
        };
        let mut scope = self.fact_scopes.get(&fact.local_id()).copied().flatten();
        let mut module = None;
        for _ in 0..self.scopes.len() {
            cancellation.check()?;
            let Some(id) = scope else {
                break;
            };
            let Some(current) = self.scopes.get(&id) else {
                return Ok(None);
            };
            if self.direct_barriers.contains(&id)
                || current.label == "rust.impl.scope"
                || current.label == "rust.trait.declaration"
            {
                return Ok(None);
            }
            if module.is_none()
                && matches!(
                    current.label.as_str(),
                    "rust.file.root" | "rust.module.module"
                )
            {
                module = Some(id);
            }
            scope = current.parent;
        }
        if scope.is_some() {
            return Ok(None);
        }
        Ok(module
            .and_then(|module| self.direct_definitions.get(&module))
            .and_then(|names| names.get(name.as_ref()))
            .and_then(|definitions| match definitions.as_slice() {
                [definition] => Some(*definition),
                _ => None,
            }))
    }

    /// Resolves only value reads in their source-proven local environment.
    /// Calls, types, constructors and macro tokens never acquire value targets.
    ///
    /// # Errors
    /// Returns cancellation errors; scope traversal is bounded by retained scopes.
    pub fn lookup(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<RustBindingLookup, AdapterError> {
        cancellation.check()?;
        let call = fact.syntax_kind().as_str().ends_with(".call");
        if fact.syntax_kind().as_str() != "rust.value.reference" && !call {
            return Ok(RustBindingLookup::NotLocal);
        }
        let Some(name) = canonical_rust_identifier(name, usize::MAX) else {
            return Ok(RustBindingLookup::NotLocal);
        };
        let mut scope = self.fact_scopes.get(&fact.local_id()).copied().flatten();
        let Some(mut position) = self.positions.get(&fact.local_id()).copied() else {
            return Ok(RustBindingLookup::Blocked);
        };
        for _ in 0..self.scopes.len() {
            cancellation.check()?;
            let Some(id) = scope else {
                break;
            };
            let Some(current) = self.scopes.get(&id) else {
                return Ok(RustBindingLookup::Blocked);
            };
            if self.unavailable_names.get(&id).is_some_and(|activations| {
                activations.iter().any(|activation| *activation <= position)
            }) {
                return Ok(RustBindingLookup::Blocked);
            }
            let blocked = self
                .blocked_bindings
                .get(&id)
                .and_then(|bindings| bindings.get(name.as_ref()))
                .and_then(|activations| {
                    activations
                        .iter()
                        .copied()
                        .filter(|activation| *activation <= position)
                        .max()
                });
            let binding = self
                .bindings
                .get(&id)
                .and_then(|bindings| bindings.get(name.as_ref()))
                .and_then(|bindings| {
                    bindings
                        .iter()
                        .rev()
                        .find(|binding| binding.activation <= position)
                });
            if blocked
                .is_some_and(|blocked| binding.is_none_or(|binding| blocked >= binding.activation))
            {
                return Ok(RustBindingLookup::Blocked);
            }
            if let Some(binding) = binding {
                return Ok(if call {
                    RustBindingLookup::Blocked
                } else {
                    binding
                        .definition
                        .map_or(RustBindingLookup::Blocked, RustBindingLookup::Definition)
                });
            }
            if self
                .unavailable_environments
                .get(&id)
                .is_some_and(|environments| {
                    environments
                        .get(&None)
                        .is_some_and(|environment| environment.contains(position))
                        || environments
                            .get(&Some(name.to_string()))
                            .is_some_and(|environment| environment.contains(position))
                })
                || self
                    .unavailable_environments
                    .get(&0)
                    .is_some_and(|environments| {
                        environments
                            .get(&None)
                            .is_some_and(|environment| environment.contains(position))
                    })
            {
                return Ok(RustBindingLookup::Blocked);
            }
            if current.label == "rust.anonymous_function.declaration" {
                position = current.span.start_byte();
            } else if current.label != "rust.block.scope"
                && current.label != "rust.pattern_environment.scope"
                && current.label != "rust.file.root"
            {
                break;
            }
            scope = current.parent;
        }
        Ok(RustBindingLookup::NotLocal)
    }
}

fn is_local(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Declaration
        && matches!(
            fact.syntax_kind().as_str(),
            "rust.local_variable.declaration" | "rust.parameter.declaration"
        )
}

fn is_closure(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Declaration
        && fact.syntax_kind().as_str() == "rust.anonymous_function.declaration"
}

fn is_scope(fact: &SyntaxFact) -> bool {
    fact.syntax_kind().as_str() == "rust.block.scope"
        || fact.syntax_kind().as_str() == "rust.pattern_environment.scope"
        || fact.syntax_kind().as_str() == "rust.file.root"
        || fact.kind() == SyntaxFactKind::Declaration
            && fact.syntax_kind().as_str().starts_with("rust.")
            && !is_local(fact)
        || fact.kind() == SyntaxFactKind::Module && fact.syntax_kind().as_str().starts_with("rust.")
}

fn ancestor(
    mut parent: Option<u64>,
    facts: &BTreeMap<u64, &SyntaxFact>,
    maximum: usize,
    cancellation: &Cancellation,
    predicate: impl Fn(&SyntaxFact) -> bool,
) -> Result<Option<u64>, AdapterError> {
    for _ in 0..maximum {
        cancellation.check()?;
        let Some(fact) = parent.and_then(|id| facts.get(&id)) else {
            break;
        };
        if predicate(fact) {
            return Ok(Some(fact.local_id()));
        }
        parent = fact.parent();
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        start: u64,
        end: u64,
        label: &str,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(FileId::from_bytes([3; 20]), start, end).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn written() -> Vec<SyntaxFact> {
        use SyntaxFactKind::{Declaration, Occurrence, Root, Scope, Signature};
        vec![
            fact(1, None, Root, 0, 200, "rust.file.root"),
            fact(2, Some(1), Declaration, 0, 200, "rust.function.declaration"),
            fact(3, Some(2), Scope, 20, 190, "rust.block.scope"),
            fact(4, Some(3), Signature, 25, 50, "rust.let.signature"),
            fact(
                5,
                Some(3),
                Declaration,
                30,
                31,
                "rust.local_variable.declaration",
            ),
            fact(
                6,
                Some(5),
                Occurrence,
                30,
                31,
                "rust.local_variable.definition",
            ),
            fact(7, Some(3), Occurrence, 40, 41, "rust.value.reference"),
            fact(8, Some(3), Occurrence, 60, 61, "rust.value.reference"),
        ]
    }

    fn bindings(facts: &[SyntaxFact]) -> RustBindings {
        RustBindings::new(
            facts,
            &BTreeMap::from([(5, "value".into())]),
            64,
            &Cancellation::new(),
        )
        .unwrap()
    }

    #[test]
    fn binding_activates_after_complete_extent_and_preserves_physical_coordinates() {
        let facts = written();
        let bindings = bindings(&facts);
        let cancellation = Cancellation::new();
        assert_eq!(
            bindings.lookup(&facts[6], "value", &cancellation).unwrap(),
            RustBindingLookup::NotLocal
        );
        assert_eq!(
            bindings.lookup(&facts[7], "value", &cancellation).unwrap(),
            RustBindingLookup::Definition(6)
        );
        let projected = fact(
            8,
            Some(3),
            SyntaxFactKind::Occurrence,
            600,
            601,
            "rust.value.reference",
        );
        assert_eq!(
            bindings.lookup(&projected, "value", &cancellation).unwrap(),
            RustBindingLookup::Definition(6)
        );
        assert_eq!(bindings.declaration_owner(5), Some(2));
        assert_eq!(bindings.definition_declaration(6), Some(5));
    }

    #[test]
    fn incomplete_definition_or_activation_blocks_guessing() {
        for omitted in [4, 6] {
            let mut facts = written();
            facts.retain(|fact| fact.local_id() != omitted);
            let reference = facts.iter().find(|fact| fact.local_id() == 8).unwrap();
            assert_eq!(
                bindings(&facts)
                    .lookup(reference, "value", &Cancellation::new())
                    .unwrap(),
                RustBindingLookup::Blocked
            );
        }
        let facts = written();
        let no_names =
            RustBindings::new(&facts, &BTreeMap::new(), 64, &Cancellation::new()).unwrap();
        assert_eq!(
            no_names
                .lookup(&facts[7], "value", &Cancellation::new())
                .unwrap(),
            RustBindingLookup::Blocked
        );
    }

    #[test]
    fn closure_capture_and_nested_item_have_distinct_environments() {
        let mut facts = written();
        facts.extend([
            fact(
                9,
                Some(3),
                SyntaxFactKind::Declaration,
                65,
                85,
                "rust.anonymous_function.declaration",
            ),
            fact(
                10,
                Some(9),
                SyntaxFactKind::Occurrence,
                75,
                76,
                "rust.value.reference",
            ),
            fact(
                11,
                Some(3),
                SyntaxFactKind::Declaration,
                90,
                115,
                "rust.function.declaration",
            ),
            fact(
                12,
                Some(11),
                SyntaxFactKind::Occurrence,
                105,
                106,
                "rust.value.reference",
            ),
        ]);
        let bindings = bindings(&facts);
        assert_eq!(
            bindings
                .lookup(&facts[9], "value", &Cancellation::new())
                .unwrap(),
            RustBindingLookup::Definition(6)
        );
        assert_eq!(
            bindings
                .lookup(&facts[11], "value", &Cancellation::new())
                .unwrap(),
            RustBindingLookup::NotLocal
        );
        assert_eq!(bindings.declaration_owner(9), Some(2));
    }

    #[test]
    fn local_call_and_type_tokens_never_acquire_value_targets() {
        let facts = written();
        let bindings = bindings(&facts);
        for (label, expected) in [
            ("rust.call.call", RustBindingLookup::Blocked),
            ("rust.scoped_call.scoped_call", RustBindingLookup::NotLocal),
            (
                "rust.type_identifier.reference",
                RustBindingLookup::NotLocal,
            ),
        ] {
            let token = fact(8, Some(3), SyntaxFactKind::Occurrence, 60, 61, label);
            assert_eq!(
                bindings
                    .lookup(&token, "value", &Cancellation::new())
                    .unwrap(),
                expected
            );
        }
    }

    #[test]
    fn cancellation_cycles_and_name_budgets_remain_bounded() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(RustBindings::new(&[], &BTreeMap::new(), 64, &cancellation).is_err());
        let cyclic = [fact(
            1,
            Some(1),
            SyntaxFactKind::Declaration,
            0,
            1,
            "rust.local_variable.declaration",
        )];
        assert!(bindings(&cyclic).identity_scope(1, None).is_none());
        assert!(
            RustBindings::new(
                &written(),
                &BTreeMap::from([(5, "large".into())]),
                2,
                &Cancellation::new()
            )
            .is_err()
        );
    }

    #[test]
    fn raw_and_unicode_names_use_rust_identifier_equality() {
        assert_eq!(
            canonical_rust_identifier("r#type", 6).as_deref(),
            Some("type")
        );
        assert_eq!(canonical_rust_identifier("λ", 2).as_deref(), Some("λ"));
        assert_eq!(
            canonical_rust_identifier("e\u{301}", 3).as_deref(),
            Some("é")
        );
        for invalid in [
            "_", "r#_", "r#self", "r#super", "r#Self", "r#crate", "a b", "0abc", "x.y", "",
        ] {
            assert!(
                canonical_rust_identifier(invalid, 64).is_none(),
                "{invalid}"
            );
        }
        assert!(canonical_rust_identifier("r#type", 5).is_none());
        assert!(canonical_rust_identifier("\u{958}", 3).is_none());
        assert_eq!(
            canonical_rust_identifier("\u{958}", 6).as_deref(),
            Some("\u{915}\u{93c}")
        );
    }

    #[test]
    fn direct_function_selection_requires_positive_context_and_unique_written_definition() {
        let mut facts = vec![
            fact(1, None, SyntaxFactKind::Root, 0, 100, "rust.file.root"),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Declaration,
                0,
                20,
                "rust.function.declaration",
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                3,
                9,
                "rust.identifier.definition",
            ),
            fact(
                4,
                Some(1),
                SyntaxFactKind::Declaration,
                20,
                100,
                "rust.function.declaration",
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Scope,
                40,
                100,
                "rust.block.scope",
            ),
            fact(
                6,
                Some(4),
                SyntaxFactKind::Occurrence,
                23,
                28,
                "rust.identifier.definition",
            ),
            fact(
                7,
                Some(5),
                SyntaxFactKind::Occurrence,
                50,
                56,
                "rust.direct_call.call",
            ),
        ];
        let names = BTreeMap::from([(3, "r#target".to_owned()), (6, "owner".to_owned())]);
        let cancellation = Cancellation::new();
        let missing = RustBindings::new(&facts, &names, 64, &cancellation).unwrap();
        assert_eq!(
            missing
                .direct_call(&facts[6], "target", &cancellation)
                .unwrap(),
            None
        );
        facts.push(fact(
            8,
            Some(1),
            SyntaxFactKind::Signature,
            0,
            100,
            "rust.proof_context.signature",
        ));
        let complete = RustBindings::new(&facts, &names, 64, &cancellation).unwrap();
        assert_eq!(
            complete
                .direct_call(&facts[6], "target", &cancellation)
                .unwrap(),
            Some(RustDirectDefinition {
                declaration: 2,
                definition: 3,
                canonical_name: content_hash(b"target")
            })
        );
        facts.push(fact(
            9,
            Some(1),
            SyntaxFactKind::Signature,
            10,
            19,
            "rust.attribute.signature",
        ));
        let opaque = RustBindings::new(&facts, &names, 64, &cancellation).unwrap();
        assert_eq!(
            opaque
                .direct_call(&facts[6], "target", &cancellation)
                .unwrap(),
            None
        );
        facts.pop();
        facts.push(fact(
            10,
            Some(2),
            SyntaxFactKind::Occurrence,
            10,
            16,
            "rust.identifier.definition",
        ));
        let ambiguous = RustBindings::new(&facts, &names, 64, &cancellation).unwrap();
        assert_eq!(
            ambiguous
                .direct_call(&facts[6], "target", &cancellation)
                .unwrap(),
            None
        );
    }

    #[test]
    fn parameters_are_visible_only_from_the_native_callable_body() {
        let mut facts = written();
        facts.extend([
            fact(
                9,
                Some(2),
                SyntaxFactKind::Signature,
                5,
                15,
                "rust.parameters.signature",
            ),
            fact(
                10,
                Some(2),
                SyntaxFactKind::Signature,
                70,
                190,
                "rust.body.signature",
            ),
            fact(
                11,
                Some(2),
                SyntaxFactKind::Declaration,
                8,
                9,
                "rust.parameter.declaration",
            ),
            fact(
                12,
                Some(11),
                SyntaxFactKind::Occurrence,
                8,
                9,
                "rust.parameter.definition",
            ),
            fact(
                13,
                Some(2),
                SyntaxFactKind::Occurrence,
                30,
                31,
                "rust.value.reference",
            ),
            fact(
                14,
                Some(3),
                SyntaxFactKind::Occurrence,
                80,
                81,
                "rust.value.reference",
            ),
        ]);
        let bindings = RustBindings::new(
            &facts,
            &BTreeMap::from([(5, "value".into()), (11, "parameter".into())]),
            64,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(
            bindings
                .lookup(&facts[12], "parameter", &Cancellation::new())
                .unwrap(),
            RustBindingLookup::NotLocal
        );
        assert_eq!(
            bindings
                .lookup(&facts[13], "parameter", &Cancellation::new())
                .unwrap(),
            RustBindingLookup::Definition(12)
        );
    }
}
