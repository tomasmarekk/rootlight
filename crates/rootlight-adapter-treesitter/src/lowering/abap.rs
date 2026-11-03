//! Native declarations and proved same-file implementation locations.
//! Generic statement bindings remain explicit gaps even inside a matched body.

use std::collections::HashMap;
use std::hash::Hash;

use rootlight_adapter_sdk::{AdapterError, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;
use rootlight_ir::{ContainerRef, EntityKind, FactDomain, SourceRef, SourceSpan};

use super::{MaterializedEntity, source_for_span, span_contains};

pub(super) const IMPLEMENTATION_UNAVAILABLE: &str =
    "abap-method-implementation-identity-unavailable";

pub(super) fn implementation_name(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Occurrence
        && fact.syntax_kind().as_str() == "abap.method_implementation_name.reference"
}

#[derive(Clone, Copy)]
pub(super) struct Implementation {
    pub(super) symbol: SymbolId,
    pub(super) owner: SymbolId,
    pub(super) body: SourceSpan,
}

#[derive(Default)]
pub(super) struct Implementations {
    classes: HashMap<u64, u64>,
    methods: HashMap<u64, Implementation>,
}

fn unique<K: Eq + Hash, V>(map: &mut HashMap<K, Option<V>>, key: K, value: V) {
    map.entry(key)
        .and_modify(|entry| *entry = None)
        .or_insert(Some(value));
}

fn name(fact: &SyntaxFact, source: &str) -> Option<String> {
    let start = usize::try_from(fact.span().start_byte()).ok()?;
    let end = usize::try_from(fact.span().end_byte()).ok()?;
    let text = source.get(start..end).filter(|text| text.len() <= 256)?;
    (text
        .as_bytes()
        .first()
        .is_some_and(|b| b.is_ascii_alphabetic() || *b == b'_')
        && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_'))
    .then(|| text.to_ascii_lowercase())
}

impl Implementations {
    pub(super) fn new(
        facts: &[SyntaxFact],
        entities: &HashMap<u64, MaterializedEntity>,
        source: &SourceRef,
        text: &str,
        mut physical: impl FnMut(SourceSpan) -> Result<SourceSpan, AdapterError>,
        cancel: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut output = Self::default();
        let mut by_id = HashMap::new();
        let mut names = HashMap::new();
        let mut signatures = HashMap::new();
        let mut classes = HashMap::new();
        let mut class_bodies = HashMap::new();
        let mut methods = HashMap::new();
        let mut method_bodies = HashMap::new();
        let mut symbols = HashMap::<SymbolId, Option<u64>>::new();
        by_id
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for map in [&mut names, &mut signatures] {
            map.try_reserve(facts.len())
                .map_err(|_| SinkError::AllocationFailed)?;
        }
        classes
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        class_bodies
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        methods
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        method_bodies
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        symbols
            .try_reserve(entities.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        output
            .classes
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        output
            .methods
            .try_reserve(facts.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        for fact in facts {
            cancel.check()?;
            if by_id.insert(fact.local_id(), fact).is_some() {
                return Ok(Self::default());
            }
            if fact.kind() == SyntaxFactKind::ErrorRecovery {
                return Ok(Self::default());
            }
            if fact.syntax_kind().as_str() == "abap.name.definition"
                || fact.syntax_kind().as_str() == "abap.class_implementation_name.expression"
                || implementation_name(fact)
            {
                unique(&mut names, fact.parent(), fact);
            }
            if matches!(
                fact.syntax_kind().as_str(),
                "abap.class_implementation.signature"
                    | "abap.method_implementation.signature"
                    | "abap.method_implementation_header.signature"
            ) {
                unique(&mut signatures, fact.parent(), fact);
            }
        }
        let roots: Vec<_> = facts
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Root)
            .collect();
        let modules: Vec<_> = facts
            .iter()
            .filter(|fact| fact.kind() == SyntaxFactKind::Module)
            .collect();
        let ([root], [module]) = (roots.as_slice(), modules.as_slice()) else {
            return Ok(output);
        };
        if root.syntax_kind().as_str() != "abap.file.root"
            || root.parent().is_some()
            || module.syntax_kind().as_str() != "abap.file.module"
            || module.parent() != Some(root.local_id())
            || physical(root.span())? != source.span()
            || physical(module.span())? != source.span()
        {
            return Ok(output);
        }
        for (local, entity) in entities {
            cancel.check()?;
            unique(&mut symbols, entity.record.id, *local);
        }
        for fact in facts {
            cancel.check()?;
            let Some(written) = names.get(&Some(fact.local_id())).copied().flatten() else {
                continue;
            };
            let Some(name) = name(written, text) else {
                continue;
            };
            match fact.syntax_kind().as_str() {
                "abap.class.declaration" => unique(&mut classes, name, fact.local_id()),
                "abap.class_implementation.scope" => {
                    unique(&mut class_bodies, name, fact.local_id())
                }
                "abap.method.declaration" | "abap.static_method.declaration" => {
                    unique(&mut methods, (fact.parent(), name), fact.local_id());
                }
                "abap.method_implementation.scope" => {
                    unique(&mut method_bodies, (fact.parent(), name), fact.local_id());
                }
                _ => {}
            }
        }
        for (class_name, body) in &class_bodies {
            cancel.check()?;
            let Some(body) = body.and_then(|local| by_id.get(&local).copied()) else {
                continue;
            };
            let Some(class) = classes
                .get(class_name)
                .copied()
                .flatten()
                .and_then(|local| by_id.get(&local).copied())
            else {
                continue;
            };
            let Some(scope) = class.parent().and_then(|local| by_id.get(&local).copied()) else {
                continue;
            };
            let Some(owner) = entities.get(&class.local_id()) else {
                continue;
            };
            let Some(definition) = owner
                .definition_local_id
                .and_then(|local| by_id.get(&local).copied())
            else {
                continue;
            };
            let frame = source_for_span(source, physical(class.span())?);
            if class.kind() != SyntaxFactKind::Declaration
                || scope.kind() != SyntaxFactKind::Scope
                || scope.syntax_kind().as_str() != "abap.class.scope"
                || scope.span() != class.span()
                || scope.parent() != Some(module.local_id())
                || body.parent() != Some(module.local_id())
                || class.span().end_byte() > body.span().start_byte()
                || definition.parent() != Some(class.local_id())
                || !span_contains(class.span(), definition.span())
                || names
                    .get(&Some(class.local_id()))
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(definition.local_id())
                || owner.record.kind != EntityKind::Class
                || owner.record.language != "abap"
                || owner.record.canonical_name != *class_name
                || owner.record.repository != source.repository()
                || owner.record.generation != source.generation()
                || owner.record.evidence.source.as_ref() != Some(&frame)
                || symbols.get(&owner.record.id) != Some(&Some(class.local_id()))
                || !signatures
                    .get(&Some(body.local_id()))
                    .copied()
                    .flatten()
                    .is_some_and(|signature| {
                        signature.syntax_kind().as_str() == "abap.class_implementation.signature"
                            && signature.span() == body.span()
                    })
            {
                continue;
            }
            output.classes.insert(body.local_id(), class.local_id());
        }
        for ((parent, method_name), method_body) in &method_bodies {
            cancel.check()?;
            let Some(class) = parent
                .and_then(|scope| output.classes.get(&scope))
                .and_then(|local| by_id.get(local).copied())
            else {
                continue;
            };
            let Some(body) = parent.and_then(|scope| by_id.get(&scope).copied()) else {
                continue;
            };
            let Some(owner) = entities.get(&class.local_id()) else {
                continue;
            };
            let Some(method_body) = method_body.and_then(|local| by_id.get(&local).copied()) else {
                continue;
            };
            let Some(method) = methods
                .get(&(Some(class.local_id()), method_name.clone()))
                .copied()
                .flatten()
                .and_then(|local| by_id.get(&local).copied())
            else {
                continue;
            };
            let Some(entity) = entities.get(&method.local_id()) else {
                continue;
            };
            let Some(definition) = entity
                .definition_local_id
                .and_then(|local| by_id.get(&local).copied())
            else {
                continue;
            };
            let frame = source_for_span(source, physical(method.span())?);
            if method.kind() != SyntaxFactKind::Declaration
                || method.span().end_byte() > method_body.span().start_byte()
                || !span_contains(class.span(), method.span())
                || !span_contains(body.span(), method_body.span())
                || definition.parent() != Some(method.local_id())
                || !span_contains(method.span(), definition.span())
                || names
                    .get(&Some(method.local_id()))
                    .copied()
                    .flatten()
                    .map(SyntaxFact::local_id)
                    != Some(definition.local_id())
                || entity.record.kind != EntityKind::Method
                || entity.record.language != "abap"
                || entity.record.canonical_name != *method_name
                || entity.record.repository != source.repository()
                || entity.record.generation != source.generation()
                || entity.record.evidence.source.as_ref() != Some(&frame)
                || entity.record.provenance != owner.record.provenance
                || entity.direct_parent != ContainerRef::Entity(owner.record.id)
                || entity.record.container != Some(entity.direct_parent)
                || symbols.get(&entity.record.id) != Some(&Some(method.local_id()))
                || !signatures
                    .get(&Some(method_body.local_id()))
                    .copied()
                    .flatten()
                    .is_some_and(|signature| {
                        signature.syntax_kind().as_str() == "abap.method_implementation.signature"
                            && signature.span() == method_body.span()
                    })
            {
                continue;
            }
            output.methods.insert(
                method_body.local_id(),
                Implementation {
                    symbol: entity.record.id,
                    owner: owner.record.id,
                    body: method_body.span(),
                },
            );
        }
        Ok(output)
    }

    pub(super) fn target(&self, fact: &SyntaxFact) -> Option<Implementation> {
        implementation_name(fact)
            .then(|| self.methods.get(&fact.parent()?).copied())
            .flatten()
            .filter(|binding| span_contains(binding.body, fact.span()))
    }

    pub(super) fn covers(&self, fact: &SyntaxFact) -> bool {
        fact.kind() == SyntaxFactKind::Signature
            && fact
                .parent()
                .is_some_and(|parent| match fact.syntax_kind().as_str() {
                    "abap.class_implementation.signature" => self.classes.contains_key(&parent),
                    "abap.method_implementation.signature" => self.methods.contains_key(&parent),
                    _ => false,
                })
    }
}

pub(super) fn written(kind: EntityKind, label: &str) -> bool {
    matches!(
        (kind, label),
        (EntityKind::Interface, "abap.interface.declaration")
            | (EntityKind::Class, "abap.class.declaration")
            | (EntityKind::Method, "abap.method.declaration")
            | (EntityKind::Method, "abap.static_method.declaration")
    )
}

pub(super) fn gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    if fact.kind() != SyntaxFactKind::Signature {
        return None;
    }
    Some(match fact.syntax_kind().as_str() {
        "abap.invalid_file.signature" => (
            FactDomain::Entities,
            "abap-complete-native-structure-unavailable",
        ),
        "abap.method_contract.signature" => (
            FactDomain::Relations,
            "abap-method-parameter-and-contract-bindings-unavailable",
        ),
        "abap.class_header.signature" => (
            FactDomain::Relations,
            "abap-class-header-bindings-unavailable",
        ),
        "abap.static_dispatch.signature" => (
            FactDomain::Relations,
            "abap-static-method-dispatch-bindings-unavailable",
        ),
        "abap.class_implementation.signature" => (
            FactDomain::Occurrences,
            "abap-class-implementation-identity-unavailable",
        ),
        "abap.method_implementation.signature" => {
            (FactDomain::Occurrences, IMPLEMENTATION_UNAVAILABLE)
        }
        "abap.method_implementation_header.signature" => (
            FactDomain::Relations,
            "abap-method-implementation-header-bindings-unavailable",
        ),
        "abap.unmodeled_statement.signature" => (
            FactDomain::Entities,
            "abap-statement-structure-and-bindings-unavailable",
        ),
        "abap.unmodeled_structure.signature" => {
            (FactDomain::Entities, "abap-native-construct-not-lowered")
        }
        _ => return None,
    })
}

pub(super) fn additional_gap(fact: &SyntaxFact) -> Option<(FactDomain, &'static str)> {
    (fact.kind() == SyntaxFactKind::Signature
        && fact.syntax_kind().as_str() == "abap.method_implementation_header.signature")
        .then_some((FactDomain::Occurrences, IMPLEMENTATION_UNAVAILABLE))
}
