//! Exact F# formal, local-let, lambda, match-arm and module reads over native captures.
//! Native selectors prove own-head/body pairing and reject intermediate binders.
//! Written names, source order and application witnesses bound every target.

mod branch;
mod lexical;
mod local;
mod recursive;
mod signature;

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{
    AdapterError, DiagnosticCode, SinkError, SyntaxFact, SyntaxFactKind,
    structural_captured_name_for_fact,
};
use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, SymbolId, content_hash};
use rootlight_ir::{FSharpBinding, SourceSpan};

const FUNCTION: &str = "fsharp.function.declaration";
const SCOPE: &str = "fsharp.function.scope";
const HEADER: &str = "fsharp.function.signature";
const PARAMETERS: &str = "fsharp.simple_parameters.signature";
const UNIT_PARAMETERS: &str = "fsharp.unit_parameters.expression";
const UNIT_PARAMETER: &str = "fsharp.unit_parameter.expression";
const COMPOUND_PARAMETERS: &str = "fsharp.compound_parameters.expression";
const TUPLE_PARAMETER: &str = "fsharp.tuple_parameter.expression";
const PARAMETER: &str = "fsharp.parameter.declaration";
pub(super) const PARAMETER_SIGNATURE: &str = "fsharp.parameter_header.expression";
const DEFINITION: &str = "fsharp.name.definition";
const READ: &str = "fsharp.simple_function_body_read.reference";
pub(super) const QUALIFIED_READ: &str = "fsharp.qualified_body_read.reference";
const QUALIFIED_CONTEXT: &str = "fsharp.qualified_context.expression";
const QUALIFIED_RECORD: &str = "fsharp.qualified_record_type.expression";
const QUALIFIED_ALIAS: &str = "fsharp.qualified_alias_type.expression";
const QUALIFIED_ENUM: &str = "fsharp.qualified_enum_type.expression";
const QUALIFIED_UNION: &str = "fsharp.qualified_union_type.expression";
const QUALIFIED_CLASS: &str = "fsharp.qualified_class_type.expression";
const RECORD: &str = "fsharp.record.declaration";
const ALIAS: &str = "fsharp.type_alias.declaration";
const ENUM: &str = "fsharp.enum.declaration";
const UNION: &str = "fsharp.union.declaration";
const CLASS: &str = "fsharp.class.declaration";
const UNION_CASE: &str = "fsharp.union_case.declaration";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QualifiedRoot {
    ProvenAbsent,
    ImportsPending,
    Bound,
    Closed,
}

pub(super) struct QualifiedClaim {
    pub(super) source: SourceSpan,
    pub(super) binding: FSharpBinding,
}

pub(super) struct QualifiedClaims {
    pub(super) scope: SourceSpan,
    pub(super) claims: Vec<QualifiedClaim>,
}

/// Retains a complete source-backed public-value mask without executable exports.
///
/// # Errors
/// Returns typed cancellation, allocation and invalid capture errors.
pub(super) fn signature_claims(
    facts: &[SyntaxFact],
    source: &[u8],
    symbols: &HashMap<u64, SymbolId>,
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Option<QualifiedClaims>, AdapterError> {
    signature::retain(facts, source, symbols, maximum_name_bytes, cancellation)
}

struct QualifiedModule<'a> {
    declaration: &'a SyntaxFact,
    scope: UniqueFact<'a>,
    header: UniqueFact<'a>,
    definition: UniqueFact<'a>,
}

struct QualifiedType<'a> {
    declaration: &'a SyntaxFact,
    scope: UniqueFact<'a>,
    header: UniqueFact<'a>,
    definition: UniqueFact<'a>,
    marker: UniqueFact<'a>,
    name: Option<&'a str>,
    case_roots: Vec<&'a str>,
}

struct QualifiedUnionCase<'a> {
    declaration: &'a SyntaxFact,
    header: UniqueFact<'a>,
    definition: UniqueFact<'a>,
}

struct QualifiedOpen<'a> {
    fact: &'a SyntaxFact,
    path: Vec<ContentHash>,
    owner: SourceSpan,
}

#[derive(Default)]
struct UniqueFact<'a> {
    count: usize,
    fact: Option<&'a SyntaxFact>,
}

impl<'a> UniqueFact<'a> {
    fn retain(&mut self, fact: &'a SyntaxFact) -> Result<(), AdapterError> {
        self.count = self.count.checked_add(1).ok_or_else(invalid_capture)?;
        self.fact = (self.count == 1).then_some(fact);
        Ok(())
    }

    fn one(&self) -> Option<&'a SyntaxFact> {
        self.fact.filter(|_| self.count == 1)
    }
}

struct Function<'a> {
    declaration: &'a SyntaxFact,
    scope: UniqueFact<'a>,
    header: UniqueFact<'a>,
    parameters: UniqueFact<'a>,
    unit_parameters: UniqueFact<'a>,
    compound_parameters: UniqueFact<'a>,
    formal_slots: Option<FormalSlots>,
    invalid: bool,
}

impl Function<'_> {
    fn parameter_marker(&self) -> Option<&SyntaxFact> {
        match (
            self.parameters.count,
            self.unit_parameters.count,
            self.compound_parameters.count,
        ) {
            (1, 0, 0) => self.parameters.one(),
            (0, 1, 0) => self.unit_parameters.one(),
            (0, 0, 1) => self.compound_parameters.one(),
            _ => None,
        }
    }

    fn valid(&self) -> bool {
        self.valid_shape()
            && (self.unit_parameters.count == 0 && self.compound_parameters.count == 0
                || self.formal_slots.is_some())
    }

    fn callable_arity(&self, named: Option<Option<usize>>) -> Option<usize> {
        let named = named.unwrap_or(Some(0))?;
        if let Some(formals) = self.formal_slots {
            (named == formals.named).then_some(formals.slots)
        } else {
            (named > 0).then_some(named)
        }
    }

    fn valid_shape(&self) -> bool {
        let (Some(header), Some(parameters)) = (self.header.one(), self.parameter_marker()) else {
            return false;
        };
        self.valid_header()
            && parameters.kind() == SyntaxFactKind::Signature
            && parameters.span().start_byte() < parameters.span().end_byte()
            && header.span().start_byte() < parameters.span().start_byte()
            && contains(header, parameters)
    }

    fn valid_header(&self) -> bool {
        let (Some(scope), Some(header)) = (self.scope.one(), self.header.one()) else {
            return false;
        };
        !self.invalid
            && scope.kind() == SyntaxFactKind::Scope
            && scope.parent() == Some(self.declaration.local_id())
            && scope.span() == self.declaration.span()
            && header.kind() == SyntaxFactKind::Signature
            && header.span().start_byte() == scope.span().start_byte()
            && header.span().start_byte() < header.span().end_byte()
            && header.span().end_byte() < scope.span().end_byte()
            && contains(scope, header)
    }
}

#[derive(Clone, Copy)]
struct FormalSlots {
    slots: usize,
    named: usize,
}

struct Parameter<'a> {
    scope: u64,
    name: Option<&'a str>,
    span: SourceSpan,
    definitions: usize,
    symbol: Option<SymbolId>,
}

struct SignatureParameter<'a> {
    declaration: &'a SyntaxFact,
    scope: u64,
    definition: UniqueFact<'a>,
}

#[derive(Clone, Copy)]
struct ScopedSignatureFact<'a> {
    scope: u64,
    fact: &'a SyntaxFact,
}

impl ScopedSignatureFact<'_> {
    fn key(self) -> (u64, rootlight_ids::FileId, u64, u64) {
        let span = self.fact.span();
        (self.scope, span.file(), span.start_byte(), span.end_byte())
    }

    fn group(self) -> (u64, rootlight_ids::FileId) {
        (self.scope, self.fact.span().file())
    }
}

/// Exact optional authored binder headers, independent of materialized symbols.
///
/// The optional expression label proves one admitted simple formal pattern.
/// This plan corroborates its complete owner/header/all-formals marker and unique
/// leaf definition. It does not infer types, resolve names, or widen leaf spans.
pub(super) struct FSharpParameterSignatures<'a> {
    signatures: HashMap<u64, &'a SyntaxFact>,
}

impl<'a> FSharpParameterSignatures<'a> {
    /// Builds declaration-local ownership without deriving or changing symbol identity.
    ///
    /// Conflicting or incomplete evidence remains unavailable. The caller uses
    /// the shared projected UTF-8 coordinate view and maps retained signature
    /// sources to original physical coordinates for lexical envelope identities.
    /// Parameter symbol discriminators stay independent of optional admission.
    ///
    /// # Errors
    /// Returns typed cancellation, allocation, invalid source-coordinate,
    /// UTF-8, duplicate-ID, or cyclic-ancestry errors.
    pub(super) fn new(
        facts: &'a [SyntaxFact],
        source: &[u8],
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut has_signature = false;
        for fact in facts {
            cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Signature
                && fact.syntax_kind().as_str() == PARAMETER_SIGNATURE
            {
                has_signature = true;
                break;
            }
        }
        if !has_signature {
            return Ok(Self {
                signatures: HashMap::new(),
            });
        }
        let mut by_id = HashMap::new();
        by_id.try_reserve(facts.len()).map_err(allocation_failed)?;
        let mut functions = HashMap::new();
        for fact in facts {
            cancellation.check()?;
            if by_id.insert(fact.local_id(), fact).is_some() {
                return Err(invalid_capture());
            }
            if matches!(
                fact.syntax_kind().as_str(),
                FUNCTION
                    | SCOPE
                    | HEADER
                    | PARAMETERS
                    | COMPOUND_PARAMETERS
                    | TUPLE_PARAMETER
                    | PARAMETER
                    | DEFINITION
                    | PARAMETER_SIGNATURE
            ) {
                source_bytes(fact, source)?;
            }
            if fact.kind() == SyntaxFactKind::Declaration && fact.syntax_kind().as_str() == FUNCTION
            {
                functions.try_reserve(1).map_err(allocation_failed)?;
                functions.insert(
                    fact.local_id(),
                    Function {
                        declaration: fact,
                        scope: UniqueFact::default(),
                        header: UniqueFact::default(),
                        parameters: UniqueFact::default(),
                        unit_parameters: UniqueFact::default(),
                        compound_parameters: UniqueFact::default(),
                        formal_slots: None,
                        invalid: false,
                    },
                );
            }
        }
        validate_ancestry(&by_id, cancellation)?;
        for fact in facts {
            cancellation.check()?;
            let label = fact.syntax_kind().as_str();
            if !matches!(
                label,
                SCOPE | HEADER | PARAMETERS | UNIT_PARAMETERS | COMPOUND_PARAMETERS
            ) {
                continue;
            }
            let Some(owner) = function_owner(fact, &by_id) else {
                continue;
            };
            let Some(function) = functions.get_mut(&owner) else {
                continue;
            };
            match label {
                SCOPE => function.scope.retain(fact)?,
                HEADER => function.header.retain(fact)?,
                PARAMETERS => function.parameters.retain(fact)?,
                UNIT_PARAMETERS => function.unit_parameters.retain(fact)?,
                COMPOUND_PARAMETERS => function.compound_parameters.retain(fact)?,
                _ => unreachable!(),
            }
        }
        retain_formal_slots(
            &mut functions,
            &by_id,
            source,
            maximum_name_bytes,
            cancellation,
        )?;
        let mut parameters = HashMap::new();
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() != PARAMETER {
                continue;
            }
            let Some(scope) = fact.parent().and_then(|id| by_id.get(&id).copied()) else {
                continue;
            };
            let Some(function) = scope.parent().and_then(|id| functions.get_mut(&id)) else {
                continue;
            };
            if !function.valid() || function.scope.one() != Some(scope) {
                continue;
            }
            if fact.kind() != SyntaxFactKind::Declaration
                || !contains(scope, fact)
                || !function
                    .header
                    .one()
                    .is_some_and(|header| contains(header, fact))
                || !function
                    .parameter_marker()
                    .is_some_and(|marker| contains(marker, fact))
            {
                function.invalid = true;
                continue;
            }
            parameters.try_reserve(1).map_err(allocation_failed)?;
            parameters.insert(
                fact.local_id(),
                SignatureParameter {
                    declaration: fact,
                    scope: scope.local_id(),
                    definition: UniqueFact::default(),
                },
            );
        }
        for fact in facts {
            cancellation.check()?;
            if !fact.syntax_kind().as_str().ends_with(".definition") {
                continue;
            }
            if let Some(parameter) = fact.parent().and_then(|id| parameters.get_mut(&id)) {
                parameter.definition.retain(fact)?;
            }
        }
        let mut leaves = Vec::new();
        leaves
            .try_reserve(parameters.len())
            .map_err(allocation_failed)?;
        for parameter in parameters.values() {
            cancellation.check()?;
            let Some(scope) = by_id.get(&parameter.scope).copied() else {
                continue;
            };
            let Some(function) = scope.parent().and_then(|id| functions.get(&id)) else {
                continue;
            };
            if function.valid() {
                // Count every admitted leaf before checking its definition;
                // an unavailable second leaf must still make a broad pattern
                // ambiguous rather than disappear from the ownership proof.
                leaves.push(ScopedSignatureFact {
                    scope: parameter.scope,
                    fact: parameter.declaration,
                });
            }
        }
        let mut patterns = Vec::new();
        for fact in facts {
            cancellation.check()?;
            if fact.kind() != SyntaxFactKind::Signature
                || fact.syntax_kind().as_str() != PARAMETER_SIGNATURE
                || fact.span().start_byte() >= fact.span().end_byte()
            {
                continue;
            }
            let Some(scope) = parameter_signature_scope(fact, &by_id) else {
                continue;
            };
            let Some(function) = scope.parent().and_then(|id| functions.get(&id)) else {
                continue;
            };
            if !function.valid()
                || function.scope.one() != Some(scope)
                || !function
                    .header
                    .one()
                    .is_some_and(|header| contains(header, fact))
                || !function
                    .parameter_marker()
                    .is_some_and(|marker| contains(marker, fact))
            {
                continue;
            }
            patterns.try_reserve(1).map_err(allocation_failed)?;
            patterns.push(ScopedSignatureFact {
                scope: scope.local_id(),
                fact,
            });
        }
        crate::runtime::sort_cancellable_by(&mut leaves, cancellation, |left, right| {
            left.key().cmp(&right.key())
        })?;
        crate::runtime::sort_cancellable_by(&mut patterns, cancellation, |left, right| {
            left.key().cmp(&right.key())
        })?;
        let mut ambiguous = HashSet::new();
        // Admitted native formals are disjoint. Overlap or duplicate spans must
        // not turn a broad or conflicting capture into another leaf's header.
        for entries in [&leaves, &patterns] {
            for pair in entries.windows(2) {
                cancellation.check()?;
                if pair[0].group() == pair[1].group()
                    && pair[0].fact.span().end_byte() > pair[1].fact.span().start_byte()
                {
                    ambiguous.try_reserve(1).map_err(allocation_failed)?;
                    ambiguous.insert(pair[0].group());
                }
            }
        }
        let mut signatures =
            local::lambda_parameter_signatures(&by_id, source, maximum_name_bytes, cancellation)?;
        signatures
            .try_reserve(patterns.len())
            .map_err(allocation_failed)?;
        for pattern in patterns {
            cancellation.check()?;
            if ambiguous.contains(&pattern.group()) {
                continue;
            }
            let span = pattern.fact.span();
            let start_key = (pattern.scope, span.file(), span.start_byte());
            let end_key = (pattern.scope, span.file(), span.end_byte());
            let first = leaves.partition_point(|leaf| {
                let leaf_span = leaf.fact.span();
                (leaf.scope, leaf_span.file(), leaf_span.start_byte()) < start_key
            });
            let after = leaves.partition_point(|leaf| {
                let leaf_span = leaf.fact.span();
                (leaf.scope, leaf_span.file(), leaf_span.start_byte()) < end_key
            });
            if first > 0 {
                let predecessor = leaves[first - 1];
                // A leaf starting before the pattern may still intersect it;
                // start-only partitions cannot prove unique full ownership.
                if predecessor.group() == pattern.group()
                    && predecessor.fact.span().end_byte() > span.start_byte()
                {
                    continue;
                }
            }
            if first.checked_add(1) != Some(after) {
                continue;
            }
            let leaf = leaves[first];
            if !contains(pattern.fact, leaf.fact) {
                continue;
            }
            if let Some(parent) = pattern.fact.parent().and_then(|id| by_id.get(&id).copied())
                && parent.syntax_kind().as_str() == PARAMETER
                && parent.local_id() != leaf.fact.local_id()
            {
                continue;
            }
            let Some(parameter) = parameters.get(&leaf.fact.local_id()) else {
                continue;
            };
            let (Some(definition), Some(name)) = (
                parameter.definition.one(),
                source_name(leaf.fact, source, maximum_name_bytes)?,
            ) else {
                continue;
            };
            if definition.kind() != SyntaxFactKind::Occurrence
                || definition.syntax_kind().as_str() != DEFINITION
                || definition.span() != leaf.fact.span()
                || !unqualified_written_name(definition, name, maximum_name_bytes)
            {
                continue;
            }
            signatures.insert(leaf.fact.local_id(), pattern.fact);
        }
        cancellation.check()?;
        Ok(Self { signatures })
    }

    /// Returns one exact authored signature for the accepted declaration leaf.
    pub(super) fn signature_for_declaration(&self, declaration: u64) -> Option<&'a SyntaxFact> {
        self.signatures.get(&declaration).copied()
    }
}

fn parameter_signature_scope<'a>(
    signature: &SyntaxFact,
    facts: &HashMap<u64, &'a SyntaxFact>,
) -> Option<&'a SyntaxFact> {
    let parent = signature.parent().and_then(|id| facts.get(&id).copied())?;
    let scope = if parent.kind() == SyntaxFactKind::Declaration
        && parent.syntax_kind().as_str() == PARAMETER
    {
        parent.parent().and_then(|id| facts.get(&id).copied())?
    } else {
        parent
    };
    (scope.kind() == SyntaxFactKind::Scope && scope.syntax_kind().as_str() == SCOPE)
        .then_some(scope)
}

fn retain_formal_slots<'a>(
    functions: &mut HashMap<u64, Function<'a>>,
    facts: &HashMap<u64, &'a SyntaxFact>,
    source: &[u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut has_slots = false;
    for function in functions.values() {
        cancellation.check()?;
        has_slots |= function.unit_parameters.count > 0 || function.compound_parameters.count > 0;
    }
    if !has_slots {
        return Ok(());
    }
    let mut leaves = Vec::new();
    let mut slots = Vec::new();
    let mut definitions = HashMap::<u64, UniqueFact<'a>>::new();
    for fact in facts.values().copied() {
        cancellation.check()?;
        if fact.kind() == SyntaxFactKind::Declaration && fact.syntax_kind().as_str() == PARAMETER {
            if let Some(scope) = fact.parent().and_then(|id| facts.get(&id).copied())
                && scope.syntax_kind().as_str() == SCOPE
                && scope
                    .parent()
                    .and_then(|id| functions.get(&id))
                    .is_some_and(|function| {
                        function.unit_parameters.count > 0 || function.compound_parameters.count > 0
                    })
            {
                source_bytes(fact, source)?;
                leaves.try_reserve(1).map_err(allocation_failed)?;
                leaves.push(ScopedSignatureFact {
                    scope: scope.local_id(),
                    fact,
                });
            }
        } else if matches!(
            fact.syntax_kind().as_str(),
            PARAMETER_SIGNATURE | UNIT_PARAMETER | TUPLE_PARAMETER
        ) {
            if let Some(scope) = parameter_signature_scope(fact, facts)
                && scope
                    .parent()
                    .and_then(|id| functions.get(&id))
                    .is_some_and(|function| {
                        function.unit_parameters.count > 0 || function.compound_parameters.count > 0
                    })
            {
                source_bytes(fact, source)?;
                slots.try_reserve(1).map_err(allocation_failed)?;
                slots.push(ScopedSignatureFact {
                    scope: scope.local_id(),
                    fact,
                });
            }
        } else if fact.syntax_kind().as_str().ends_with(".definition")
            && let Some(parent) = fact.parent().and_then(|id| facts.get(&id).copied())
            && parent.syntax_kind().as_str() == PARAMETER
        {
            definitions.try_reserve(1).map_err(allocation_failed)?;
            definitions
                .entry(parent.local_id())
                .or_default()
                .retain(fact)?;
        }
    }
    crate::runtime::sort_cancellable_by(&mut leaves, cancellation, |left, right| {
        left.key().cmp(&right.key())
    })?;
    crate::runtime::sort_cancellable_by(&mut slots, cancellation, |left, right| {
        left.key().cmp(&right.key())
    })?;
    for function in functions.values_mut() {
        cancellation.check()?;
        if function.unit_parameters.count == 0 && function.compound_parameters.count == 0
            || !function.valid_shape()
        {
            continue;
        }
        let scope = function.scope.one().ok_or_else(invalid_capture)?;
        let marker = function.parameter_marker().ok_or_else(invalid_capture)?;
        let group = (scope.local_id(), marker.span().file());
        let first = slots.partition_point(|slot| slot.group() < group);
        let after = slots.partition_point(|slot| slot.group() <= group);
        let mut position = marker.span().start_byte();
        let mut named = 0usize;
        let mut units = 0usize;
        let mut tuples = 0usize;
        let mut argument_slots = 0usize;
        let mut valid = true;
        let mut index = first;
        while index < after {
            cancellation.check()?;
            let slot = slots[index];
            let span = slot.fact.span();
            if slot.fact.kind() != SyntaxFactKind::Signature
                || !contains(marker, slot.fact)
                || span.start_byte() < position
                || span.start_byte() >= span.end_byte()
                || !whitespace_between(source, position, span.start_byte(), cancellation)?
            {
                valid = false;
                break;
            }
            if slot.fact.syntax_kind().as_str() == UNIT_PARAMETER {
                if slot.fact.parent() != Some(scope.local_id())
                    || !unit_marker_bytes(source_bytes(slot.fact, source)?, cancellation)?
                {
                    valid = false;
                    break;
                }
                units = units.checked_add(1).ok_or_else(invalid_capture)?;
            } else if slot.fact.syntax_kind().as_str() == TUPLE_PARAMETER {
                if slot.fact.parent() != Some(scope.local_id()) {
                    valid = false;
                    break;
                }
                let first_header = index.checked_add(1).ok_or_else(invalid_capture)?;
                let mut after_header = first_header;
                while after_header < after && contains(slot.fact, slots[after_header].fact) {
                    cancellation.check()?;
                    let header = slots[after_header];
                    if header.fact.syntax_kind().as_str() != PARAMETER_SIGNATURE
                        || !valid_formal_header(
                            header,
                            scope,
                            &leaves,
                            &definitions,
                            facts,
                            source,
                            maximum_name_bytes,
                            cancellation,
                        )?
                    {
                        valid = false;
                        break;
                    }
                    after_header = after_header.checked_add(1).ok_or_else(invalid_capture)?;
                }
                if !valid
                    || !tuple_slot_bytes(
                        slot.fact,
                        &slots[first_header..after_header],
                        source,
                        cancellation,
                    )?
                {
                    valid = false;
                    break;
                }
                named = named
                    .checked_add(after_header - first_header)
                    .ok_or_else(invalid_capture)?;
                tuples = tuples.checked_add(1).ok_or_else(invalid_capture)?;
                index = after_header;
            } else {
                if !valid_formal_header(
                    slot,
                    scope,
                    &leaves,
                    &definitions,
                    facts,
                    source,
                    maximum_name_bytes,
                    cancellation,
                )? {
                    valid = false;
                    break;
                }
                named = named.checked_add(1).ok_or_else(invalid_capture)?;
            }
            position = span.end_byte();
            argument_slots = argument_slots.checked_add(1).ok_or_else(invalid_capture)?;
            if slot.fact.syntax_kind().as_str() != TUPLE_PARAMETER {
                index = index.checked_add(1).ok_or_else(invalid_capture)?;
            }
        }
        let first_leaf = leaves.partition_point(|leaf| leaf.group() < group);
        let after_leaf = leaves.partition_point(|leaf| leaf.group() <= group);
        // Optional slot loss must not turn a longer authored list into a
        // smaller arity. Only the complete native union conserves every byte.
        if valid
            && (function.compound_parameters.count == 1 && tuples > 0
                || function.unit_parameters.count == 1 && units > 0 && tuples == 0)
            && named == after_leaf - first_leaf
            && whitespace_between(source, position, marker.span().end_byte(), cancellation)?
        {
            function.formal_slots = Some(FormalSlots {
                slots: argument_slots,
                named,
            });
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "one exact formal header is corroborated against its owner, leaves and definitions"
)]
fn valid_formal_header(
    header: ScopedSignatureFact<'_>,
    scope: &SyntaxFact,
    leaves: &[ScopedSignatureFact<'_>],
    definitions: &HashMap<u64, UniqueFact<'_>>,
    facts: &HashMap<u64, &SyntaxFact>,
    source: &[u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    let span = header.fact.span();
    if header.fact.kind() != SyntaxFactKind::Signature || span.start_byte() >= span.end_byte() {
        return Ok(false);
    }
    let start_key = (header.scope, span.file(), span.start_byte());
    let end_key = (header.scope, span.file(), span.end_byte());
    let first = leaves.partition_point(|leaf| {
        (
            leaf.scope,
            leaf.fact.span().file(),
            leaf.fact.span().start_byte(),
        ) < start_key
    });
    let after = leaves.partition_point(|leaf| {
        (
            leaf.scope,
            leaf.fact.span().file(),
            leaf.fact.span().start_byte(),
        ) < end_key
    });
    if first.checked_add(1) != Some(after)
        || first
            .checked_sub(1)
            .and_then(|index| leaves.get(index))
            .is_some_and(|leaf| {
                leaf.group() == header.group() && leaf.fact.span().end_byte() > span.start_byte()
            })
    {
        return Ok(false);
    }
    let leaf = leaves[first].fact;
    let definition = definitions.get(&leaf.local_id()).and_then(UniqueFact::one);
    let name = source_name(leaf, source, maximum_name_bytes)?;
    Ok(contains(header.fact, leaf)
        && contains(scope, leaf)
        && header
            .fact
            .parent()
            .and_then(|id| facts.get(&id).copied())
            .is_some_and(|parent| {
                parent.local_id() == scope.local_id() || parent.local_id() == leaf.local_id()
            })
        && definition.is_some_and(|definition| {
            definition.kind() == SyntaxFactKind::Occurrence
                && definition.syntax_kind().as_str() == DEFINITION
                && definition.span() == leaf.span()
                && name.is_some_and(|name| {
                    unqualified_written_name(definition, name, maximum_name_bytes)
                })
        }))
}

fn tuple_slot_bytes(
    slot: &SyntaxFact,
    headers: &[ScopedSignatureFact<'_>],
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    if headers.len() < 2 {
        return Ok(false);
    }
    let start = usize::try_from(slot.span().start_byte()).map_err(|_| invalid_capture())?;
    let end = usize::try_from(slot.span().end_byte()).map_err(|_| invalid_capture())?;
    source.get(start..end).ok_or_else(invalid_capture)?;
    let mut position = start;
    let mut next_header = 0usize;
    let mut depth = 0usize;
    let mut root_commas = 0usize;
    let mut expects_value = true;
    let mut complete = false;
    // Headers consume every named leaf atomically. Only balanced tuple syntax
    // may remain between them; losing an optional leaf/header cannot shrink
    // the authored list into a smaller callable arity.
    while position < end {
        cancellation.check()?;
        if let Some(header) = headers.get(next_header) {
            let header_start =
                usize::try_from(header.fact.span().start_byte()).map_err(|_| invalid_capture())?;
            let header_end =
                usize::try_from(header.fact.span().end_byte()).map_err(|_| invalid_capture())?;
            if header_start < position {
                return Ok(false);
            }
            if header_start == position {
                if depth == 0 || !expects_value || header_end <= position || header_end > end {
                    return Ok(false);
                }
                position = header_end;
                next_header = next_header.checked_add(1).ok_or_else(invalid_capture)?;
                expects_value = false;
                continue;
            }
        }
        let byte = *source.get(position).ok_or_else(invalid_capture)?;
        if byte.is_ascii_whitespace() {
            position = position.checked_add(1).ok_or_else(invalid_capture)?;
            continue;
        }
        if complete {
            return Ok(false);
        }
        match byte {
            b'(' if expects_value => {
                depth = depth.checked_add(1).ok_or_else(invalid_capture)?;
            }
            b',' if depth > 0 && !expects_value => {
                if depth == 1 {
                    root_commas = root_commas.checked_add(1).ok_or_else(invalid_capture)?;
                }
                expects_value = true;
            }
            b')' if depth > 0 && !expects_value => {
                depth -= 1;
                if depth == 0 {
                    complete = root_commas > 0;
                    if !complete {
                        return Ok(false);
                    }
                }
            }
            _ => return Ok(false),
        }
        position = position.checked_add(1).ok_or_else(invalid_capture)?;
    }
    Ok(complete && depth == 0 && next_header == headers.len())
}

fn whitespace_between(
    source: &[u8],
    start: u64,
    end: u64,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let start = usize::try_from(start).map_err(|_| invalid_capture())?;
    let end = usize::try_from(end).map_err(|_| invalid_capture())?;
    let bytes = source.get(start..end).ok_or_else(invalid_capture)?;
    for byte in bytes {
        cancellation.check()?;
        if !byte.is_ascii_whitespace() {
            return Ok(false);
        }
    }
    Ok(true)
}

fn unit_marker_bytes(bytes: &[u8], cancellation: &Cancellation) -> Result<bool, AdapterError> {
    cancellation.check()?;
    let Some(inside) = bytes
        .strip_prefix(b"(")
        .and_then(|bytes| bytes.strip_suffix(b")"))
    else {
        return Ok(false);
    };
    for byte in inside {
        cancellation.check()?;
        if !matches!(byte, b' ' | b'\t') {
            return Ok(false);
        }
    }
    Ok(true)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum BindingContext {
    CompleteFile,
    RecoveredOwners,
}

/// Source-bound identities from unclipped native F# parameter and binding captures.
pub(super) struct FSharpBindings<'a> {
    context: BindingContext,
    facts: HashMap<u64, &'a SyntaxFact>,
    functions: HashMap<u64, Function<'a>>,
    bindings: HashMap<(u64, &'a str), Option<SymbolId>>,
    root_formals: HashSet<(u64, &'a str)>,
    conflicting_symbols: HashSet<SymbolId>,
    lexical: Option<lexical::LexicalBindings<'a>>,
    local: Option<local::LocalBindings>,
    qualified: Option<QualifiedClaims>,
    qualified_types: HashMap<u64, QualifiedType<'a>>,
    qualified_types_complete: bool,
    qualified_opens: Vec<QualifiedOpen<'a>>,
    qualified_opens_complete: bool,
    source: &'a [u8],
    maximum_name_bytes: usize,
}

impl<'a> FSharpBindings<'a> {
    /// Retain exact lexical bindings without inferring missing identities.
    ///
    /// The caller supplies complete native captures and symbols keyed by the
    /// `fsharp.name.definition` occurrence local ID, not the declaration ID.
    /// A positive marker proves every native argument is a simple binder, or
    /// an exact complete union additionally accounts for tuple and unit slots.
    /// Missing or conflicting capture evidence disables the affected target.
    /// Optional application arguments require complete native extraction, not
    /// an arbitrary subset supplied by an incomplete syntax provider.
    ///
    /// # Errors
    /// Returns typed cancellation, allocation, or invalid capture-coordinate,
    /// UTF-8, duplicate-ID, and cyclic-ancestry errors.
    pub(super) fn new(
        facts: &'a [SyntaxFact],
        source: &'a [u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        Self::new_with_context(
            facts,
            source,
            symbols,
            maximum_name_bytes,
            BindingContext::CompleteFile,
            cancellation,
        )
    }

    /// Retains clean owned lexical targets from an unclipped recovery partition.
    ///
    /// The caller must exclude traversal, capture and context limits. Native
    /// owner witnesses certify clean formal/local environments independently
    /// of the first reported recovery span; module targets and import claims
    /// remain unavailable because recovery does not conserve their root inventory.
    ///
    /// # Errors
    /// Returns the same typed cancellation, allocation and capture errors as [`Self::new`].
    pub(super) fn new_recovered_owners(
        facts: &'a [SyntaxFact],
        source: &'a [u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        Self::new_with_context(
            facts,
            source,
            symbols,
            maximum_name_bytes,
            BindingContext::RecoveredOwners,
            cancellation,
        )
    }

    fn new_with_context(
        facts: &'a [SyntaxFact],
        source: &'a [u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        context: BindingContext,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut result = Self {
            context,
            facts: HashMap::new(),
            functions: HashMap::new(),
            bindings: HashMap::new(),
            root_formals: HashSet::new(),
            conflicting_symbols: HashSet::new(),
            lexical: None,
            local: None,
            qualified: None,
            qualified_types: HashMap::new(),
            qualified_types_complete: false,
            qualified_opens: Vec::new(),
            qualified_opens_complete: false,
            source,
            maximum_name_bytes,
        };
        result
            .facts
            .try_reserve(facts.len())
            .map_err(allocation_failed)?;
        result
            .functions
            .try_reserve(facts.len())
            .map_err(allocation_failed)?;
        result
            .bindings
            .try_reserve(facts.len())
            .map_err(allocation_failed)?;
        result
            .root_formals
            .try_reserve(facts.len())
            .map_err(allocation_failed)?;
        result
            .conflicting_symbols
            .try_reserve(facts.len())
            .map_err(allocation_failed)?;
        for fact in facts {
            cancellation.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(invalid_capture());
            }
            if matches!(
                fact.syntax_kind().as_str(),
                FUNCTION
                    | SCOPE
                    | HEADER
                    | PARAMETERS
                    | UNIT_PARAMETERS
                    | UNIT_PARAMETER
                    | COMPOUND_PARAMETERS
                    | TUPLE_PARAMETER
                    | PARAMETER
                    | PARAMETER_SIGNATURE
                    | DEFINITION
                    | READ
                    | QUALIFIED_READ
                    | QUALIFIED_CONTEXT
                    | QUALIFIED_RECORD
                    | QUALIFIED_ALIAS
                    | QUALIFIED_ENUM
            ) {
                source_bytes(fact, source)?;
            }
            if fact.kind() == SyntaxFactKind::Declaration && fact.syntax_kind().as_str() == FUNCTION
            {
                result.functions.insert(
                    fact.local_id(),
                    Function {
                        declaration: fact,
                        scope: UniqueFact::default(),
                        header: UniqueFact::default(),
                        parameters: UniqueFact::default(),
                        unit_parameters: UniqueFact::default(),
                        compound_parameters: UniqueFact::default(),
                        formal_slots: None,
                        invalid: false,
                    },
                );
            }
        }
        validate_ancestry(&result.facts, cancellation)?;
        for fact in facts {
            cancellation.check()?;
            let label = fact.syntax_kind().as_str();
            if !matches!(
                label,
                SCOPE | HEADER | PARAMETERS | UNIT_PARAMETERS | COMPOUND_PARAMETERS
            ) {
                continue;
            }
            let Some(owner) = function_owner(fact, &result.facts) else {
                continue;
            };
            let Some(function) = result.functions.get_mut(&owner) else {
                continue;
            };
            match label {
                SCOPE => function.scope.retain(fact)?,
                HEADER => function.header.retain(fact)?,
                PARAMETERS => function.parameters.retain(fact)?,
                UNIT_PARAMETERS => function.unit_parameters.retain(fact)?,
                COMPOUND_PARAMETERS => function.compound_parameters.retain(fact)?,
                _ => unreachable!(),
            }
        }
        retain_formal_slots(
            &mut result.functions,
            &result.facts,
            source,
            maximum_name_bytes,
            cancellation,
        )?;
        let mut parameters = HashMap::new();
        parameters
            .try_reserve(facts.len())
            .map_err(allocation_failed)?;
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() != PARAMETER {
                continue;
            }
            let Some(scope) = fact.parent().and_then(|id| result.facts.get(&id).copied()) else {
                continue;
            };
            let Some(function) = scope.parent().and_then(|id| result.functions.get_mut(&id)) else {
                continue;
            };
            if !function.valid() || function.scope.one() != Some(scope) {
                continue;
            }
            if fact.kind() != SyntaxFactKind::Declaration
                || !contains(scope, fact)
                || !function
                    .header
                    .one()
                    .is_some_and(|header| contains(header, fact))
                || !function
                    .parameter_marker()
                    .is_some_and(|marker| contains(marker, fact))
            {
                function.invalid = true;
                continue;
            }
            parameters.insert(
                fact.local_id(),
                Parameter {
                    scope: scope.local_id(),
                    name: source_name(fact, source, maximum_name_bytes)?,
                    span: fact.span(),
                    definitions: 0,
                    symbol: None,
                },
            );
        }
        for fact in facts {
            cancellation.check()?;
            if !fact.syntax_kind().as_str().ends_with(".definition") {
                continue;
            }
            let Some(parameter) = fact.parent().and_then(|id| parameters.get_mut(&id)) else {
                continue;
            };
            parameter.definitions = parameter
                .definitions
                .checked_add(1)
                .ok_or_else(invalid_capture)?;
            parameter.symbol = if parameter.definitions == 1
                && fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == DEFINITION
                && fact.span() == parameter.span
                && parameter
                    .name
                    .is_some_and(|name| unqualified_written_name(fact, name, maximum_name_bytes))
            {
                symbols.get(&fact.local_id()).copied()
            } else {
                None
            };
        }
        let mut symbol_owners = HashMap::new();
        symbol_owners
            .try_reserve(symbols.len())
            .map_err(allocation_failed)?;
        for (&definition, &symbol) in symbols {
            cancellation.check()?;
            if symbol_owners.insert(symbol, definition).is_some() {
                result.conflicting_symbols.insert(symbol);
            }
        }
        for parameter in parameters.into_values() {
            cancellation.check()?;
            let symbol = (parameter.definitions == 1)
                .then_some(parameter.symbol)
                .flatten();
            if let Some(name) = parameter.name {
                result
                    .root_formals
                    .insert((parameter.scope, canonical_identifier(name)));
                // Unavailable and duplicate formals remain closed entries, not
                // an invitation to resolve a same-spelled outer declaration.
                result
                    .bindings
                    .entry((parameter.scope, name))
                    .and_modify(|symbol| *symbol = None)
                    .or_insert(symbol);
            }
        }
        if result.allows_module_lookup() {
            let (types, complete) = result.retain_qualified_types(cancellation)?;
            result.qualified_types = types;
            result.qualified_types_complete = complete;
            let (opens, complete) = result.retain_qualified_opens(cancellation)?;
            result.qualified_opens = opens;
            result.qualified_opens_complete = complete;
        }
        result.lexical = Some(lexical::LexicalBindings::new(
            &result,
            symbols,
            cancellation,
        )?);
        result.local = Some(local::LocalBindings::new(&result, symbols, cancellation)?);
        if result.allows_module_lookup() {
            result.qualified = result.retain_qualified_claims(cancellation)?;
        }
        Ok(result)
    }

    fn allows_module_lookup(&self) -> bool {
        self.context == BindingContext::CompleteFile
    }

    /// Resolve an admitted body read in its positively proved lexical context.
    ///
    /// Only the exact retained fact and exact written source name are accepted.
    /// Native capture parents must directly expose the associated function
    /// scope or an exact local-let, lambda or match-arm environment. Native
    /// body publication and nested binders determine shadowing without walking
    /// unknown ancestors. Same-module lookup is unavailable after recovery;
    /// otherwise it requires a positive ordinary binding or complete recursive-group
    /// witness. Unavailable formals remain closed entries.
    ///
    /// # Errors
    /// Returns typed cancellation or invalid read-coordinate and UTF-8 errors.
    pub(super) fn resolve(
        &self,
        read: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if read.kind() != SyntaxFactKind::Occurrence
            || read.syntax_kind().as_str() != READ
            || self.facts.get(&read.local_id()).copied() != Some(read)
            || source_name(read, self.source, self.maximum_name_bytes)? != Some(name)
            || !unqualified_written_name(read, name, self.maximum_name_bytes)
        {
            return Ok(None);
        }
        if let Some(target) = self.local.as_ref().and_then(|local| local.resolve(read)) {
            return Ok(target.filter(|symbol| !self.conflicting_symbols.contains(symbol)));
        }
        let Some(scope) = read.parent().and_then(|id| self.facts.get(&id).copied()) else {
            return Ok(None);
        };
        if let Some(function) = scope.parent().and_then(|id| self.functions.get(&id)) {
            if !function.valid()
                || function.scope.one() != Some(scope)
                || !contains(scope, read)
                || function
                    .header
                    .one()
                    .is_none_or(|header| header.span().end_byte() > read.span().start_byte())
            {
                return Ok(None);
            }
            if let Some(symbol) = self.bindings.get(&(scope.local_id(), name)) {
                return Ok(symbol.filter(|symbol| !self.conflicting_symbols.contains(symbol)));
            }
        }
        if !self.allows_module_lookup() {
            return Ok(None);
        }
        Ok(self
            .lexical
            .as_ref()
            .and_then(|lexical| lexical.resolve(read, name))
            .filter(|symbol| !self.conflicting_symbols.contains(symbol)))
    }

    /// Retain the native callee's exact written name, never whole-call text.
    ///
    /// # Errors
    /// Returns typed cancellation or invalid source-coordinate and UTF-8 errors.
    pub(super) fn call_name(
        &self,
        call: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<&'a str>, AdapterError> {
        cancellation.check()?;
        let Some(read) = self.callee_read(call) else {
            return Ok(None);
        };
        let name = source_name(read, self.source, self.maximum_name_bytes)?;
        if read.syntax_kind().as_str() == QUALIFIED_READ {
            return Ok(
                name.filter(|name| written_components(name).is_some_and(|parts| parts.len() > 1))
            );
        }
        Ok(name.filter(|name| unqualified_written_name(read, name, self.maximum_name_bytes)))
    }

    /// Prove direct dispatch only to an ordinary function at exact saturation.
    ///
    /// Formals, variables, unknown names and partial/over-applied functions do
    /// not acquire Calls merely because their callee read has an exact target.
    ///
    /// # Errors
    /// Returns typed cancellation or invalid read-coordinate and UTF-8 errors.
    pub(super) fn resolve_call(
        &self,
        call: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        let (Some(read), Some(name)) =
            (self.callee_read(call), self.call_name(call, cancellation)?)
        else {
            return Ok(None);
        };
        Ok(self.resolve(read, name, cancellation)?.filter(|symbol| {
            self.lexical.as_ref().is_some_and(|lexical| {
                (self.allows_module_lookup() && lexical.saturated(call, *symbol))
                    || self.local.as_ref().is_some_and(|local| {
                        local
                            .arity(*symbol)
                            .is_some_and(|arity| lexical.saturated_with_arity(call, arity))
                    })
            })
        }))
    }

    fn callee_read(&self, call: &SyntaxFact) -> Option<&'a SyntaxFact> {
        if call.kind() != SyntaxFactKind::Occurrence
            || call.syntax_kind().as_str() != lexical::CALL
            || self.facts.get(&call.local_id()).copied() != Some(call)
        {
            return None;
        }
        self.lexical
            .as_ref()?
            .callee(call)
            .and_then(|id| self.facts.get(&id).copied())
    }

    fn qualified_root(&self, read: &SyntaxFact, name: &str) -> QualifiedRoot {
        if !self.allows_module_lookup() {
            return QualifiedRoot::Closed;
        }
        if let Some(root) = self
            .local
            .as_ref()
            .and_then(|local| local.qualified_root(read))
        {
            return root;
        }
        let Some(scope) = read.parent().and_then(|id| self.facts.get(&id).copied()) else {
            return QualifiedRoot::Closed;
        };
        if let Some(function) = scope.parent().and_then(|id| self.functions.get(&id)) {
            if !function.valid()
                || function.scope.one() != Some(scope)
                || !contains(scope, read)
                || function
                    .header
                    .one()
                    .is_none_or(|header| header.span().end_byte() > read.span().start_byte())
            {
                return QualifiedRoot::Closed;
            }
            if self
                .root_formals
                .contains(&(scope.local_id(), canonical_identifier(name)))
            {
                return QualifiedRoot::Bound;
            }
        }
        self.lexical
            .as_ref()
            .map_or(QualifiedRoot::Closed, |lexical| {
                lexical.qualified_root_in(scope.local_id(), read, name)
            })
    }

    fn opened_name(
        &self,
        read: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<ContentHash>, AdapterError> {
        cancellation.check()?;
        if read.kind() != SyntaxFactKind::Occurrence
            || read.syntax_kind().as_str() != READ
            || self.facts.get(&read.local_id()).copied() != Some(read)
        {
            return Ok(None);
        }
        let Some(name) = source_name(read, self.source, self.maximum_name_bytes)?
            .filter(|name| unqualified_written_name(read, name, self.maximum_name_bytes))
        else {
            return Ok(None);
        };
        // Unavailable or shadowed local targets are not import candidates.
        Ok(
            (self.qualified_root(read, name) == QualifiedRoot::ImportsPending)
                .then(|| content_hash(canonical_identifier(name).as_bytes())),
        )
    }

    pub(super) fn repository_claims(&self) -> Option<&QualifiedClaims> {
        self.qualified.as_ref()
    }

    pub(super) fn qualified_read_name(
        &self,
        read: &SyntaxFact,
    ) -> Result<Option<&'a str>, AdapterError> {
        if read.syntax_kind().as_str() != QUALIFIED_READ
            || self.facts.get(&read.local_id()).copied() != Some(read)
        {
            return Ok(None);
        }
        Ok(source_name(read, self.source, self.maximum_name_bytes)?
            .filter(|name| written_components(name).is_some_and(|parts| parts.len() > 1)))
    }

    fn retain_qualified_opens(
        &self,
        cancellation: &Cancellation,
    ) -> Result<(Vec<QualifiedOpen<'a>>, bool), AdapterError> {
        let mut opens = Vec::new();
        let mut seen = HashSet::new();
        opens
            .try_reserve(self.facts.len())
            .map_err(allocation_failed)?;
        seen.try_reserve(self.facts.len())
            .map_err(allocation_failed)?;
        let mut complete = true;
        for fact in self.facts.values().copied() {
            cancellation.check()?;
            if fact.kind() != SyntaxFactKind::Import {
                continue;
            }
            if fact.syntax_kind().as_str() != "fsharp.open.import" || !seen.insert(fact.span()) {
                complete = false;
                continue;
            }
            let Some(scope) = fact
                .parent()
                .and_then(|id| self.facts.get(&id).copied())
                .filter(|scope| lexical::module_scope(scope))
            else {
                complete = false;
                continue;
            };
            let Some(owner) = scope
                .parent()
                .and_then(|id| self.facts.get(&id).copied())
                .filter(|owner| {
                    owner.kind() == SyntaxFactKind::Declaration
                        && matches!(
                            owner.syntax_kind().as_str(),
                            "fsharp.module.declaration"
                                | "fsharp.namespace.declaration"
                                | "fsharp.nested_module.declaration"
                        )
                        && owner.span() == scope.span()
                        && contains(owner, fact)
                })
            else {
                complete = false;
                continue;
            };
            let bytes = source_bytes(fact, self.source)?;
            if !bytes.starts_with(b"open") {
                complete = false;
                continue;
            }
            let Some(position) =
                trivia_end(bytes, 4, cancellation)?.filter(|position| *position > 4)
            else {
                complete = false;
                continue;
            };
            let text = std::str::from_utf8(bytes.get(position..).ok_or_else(invalid_capture)?)
                .map_err(|_| invalid_capture())?
                .trim();
            let Some(parts) = written_components(text).filter(|_| !text.starts_with("type "))
            else {
                complete = false;
                continue;
            };
            opens.push(QualifiedOpen {
                fact,
                path: component_hashes(&parts)?,
                owner: owner.span(),
            });
        }
        Ok((opens, complete))
    }

    fn retain_qualified_types(
        &self,
        cancellation: &Cancellation,
    ) -> Result<(HashMap<u64, QualifiedType<'a>>, bool), AdapterError> {
        let mut records = HashMap::new();
        let mut cases = HashMap::<u64, QualifiedUnionCase<'a>>::new();
        let mut markers =
            HashMap::<(Option<u64>, rootlight_ids::FileId, u64), UniqueFact<'a>>::new();
        records
            .try_reserve(self.facts.len())
            .map_err(allocation_failed)?;
        markers
            .try_reserve(self.facts.len())
            .map_err(allocation_failed)?;
        for fact in self.facts.values().copied() {
            cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Declaration
                && matches!(
                    fact.syntax_kind().as_str(),
                    RECORD | ALIAS | ENUM | UNION | CLASS
                )
            {
                records.insert(
                    fact.local_id(),
                    QualifiedType {
                        declaration: fact,
                        scope: UniqueFact::default(),
                        header: UniqueFact::default(),
                        definition: UniqueFact::default(),
                        marker: UniqueFact::default(),
                        name: None,
                        case_roots: Vec::new(),
                    },
                );
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == UNION_CASE
            {
                cases.try_reserve(1).map_err(allocation_failed)?;
                cases.insert(
                    fact.local_id(),
                    QualifiedUnionCase {
                        declaration: fact,
                        header: UniqueFact::default(),
                        definition: UniqueFact::default(),
                    },
                );
            }
            if matches!(
                fact.syntax_kind().as_str(),
                QUALIFIED_RECORD
                    | QUALIFIED_ALIAS
                    | QUALIFIED_ENUM
                    | QUALIFIED_UNION
                    | QUALIFIED_CLASS
            ) {
                markers
                    .entry((fact.parent(), fact.span().file(), fact.span().end_byte()))
                    .or_default()
                    .retain(fact)?;
            }
        }
        for fact in self.facts.values().copied() {
            cancellation.check()?;
            if let Some(case) = fact.parent().and_then(|parent| cases.get_mut(&parent)) {
                match fact.syntax_kind().as_str() {
                    "fsharp.union_case.signature" => case.header.retain(fact)?,
                    DEFINITION => case.definition.retain(fact)?,
                    _ => {}
                }
            }
            let owner = fact
                .parent()
                .and_then(|id| self.facts.get(&id).copied())
                .and_then(|parent| {
                    if records.contains_key(&parent.local_id()) {
                        Some(parent.local_id())
                    } else if parent.kind() == SyntaxFactKind::Scope {
                        parent.parent()
                    } else {
                        None
                    }
                });
            let Some(record) = owner.and_then(|id| records.get_mut(&id)) else {
                continue;
            };
            match fact.syntax_kind().as_str() {
                "fsharp.record.scope"
                | "fsharp.enum.scope"
                | "fsharp.union.scope"
                | "fsharp.class.scope" => record.scope.retain(fact)?,
                "fsharp.record.signature"
                | "fsharp.enum.signature"
                | "fsharp.union.signature"
                | "fsharp.class.signature"
                | "fsharp.type_alias.signature" => record.header.retain(fact)?,
                DEFINITION => record.definition.retain(fact)?,
                _ => {}
            }
        }
        let mut cases_by_scope = HashMap::<u64, Vec<&QualifiedUnionCase<'a>>>::new();
        cases_by_scope
            .try_reserve(cases.len())
            .map_err(allocation_failed)?;
        for case in cases.values() {
            cancellation.check()?;
            if let Some(parent) = case.declaration.parent() {
                let entries = cases_by_scope.entry(parent).or_default();
                entries.try_reserve(1).map_err(allocation_failed)?;
                entries.push(case);
            }
        }
        let mut complete = true;
        for record in records.values_mut() {
            cancellation.check()?;
            let declaration = record.declaration;
            record.marker = markers
                .remove(&(
                    declaration.parent(),
                    declaration.span().file(),
                    declaration.span().end_byte(),
                ))
                .unwrap_or_default();
            record.name = qualified_type_name(
                record,
                &self.facts,
                self.source,
                self.maximum_name_bytes,
                cancellation,
            )?;
            if declaration.syntax_kind().as_str() == UNION && record.name.is_some() {
                let entries = record
                    .scope
                    .one()
                    .and_then(|scope| cases_by_scope.remove(&scope.local_id()))
                    .unwrap_or_default();
                if let Some(names) = qualified_union_case_roots(
                    record,
                    entries,
                    self.source,
                    self.maximum_name_bytes,
                    cancellation,
                )? {
                    record.case_roots = names;
                } else {
                    record.name = None;
                }
            }
            complete &= record.name.is_some();
        }
        Ok((records, complete && markers.is_empty()))
    }

    fn retain_qualified_claims(
        &self,
        cancellation: &Cancellation,
    ) -> Result<Option<QualifiedClaims>, AdapterError> {
        let mut context = UniqueFact::default();
        let mut file_scope = UniqueFact::default();
        let mut modules = HashMap::<u64, QualifiedModule<'a>>::new();
        modules
            .try_reserve(self.facts.len())
            .map_err(allocation_failed)?;
        for fact in self.facts.values().copied() {
            cancellation.check()?;
            if fact.syntax_kind().as_str() == QUALIFIED_CONTEXT {
                context.retain(fact)?;
            }
            if fact.kind() == SyntaxFactKind::Scope
                && fact.syntax_kind().as_str() == "fsharp.file.scope"
            {
                file_scope.retain(fact)?;
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && matches!(
                    fact.syntax_kind().as_str(),
                    "fsharp.module.declaration"
                        | "fsharp.namespace.declaration"
                        | "fsharp.nested_module.declaration"
                )
            {
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
        }
        let (Some(context), Some(file_scope), Some(lexical)) =
            (context.one(), file_scope.one(), self.lexical.as_ref())
        else {
            return Ok(None);
        };
        if context.kind() != SyntaxFactKind::Signature
            || context.span().start_byte() != 0
            || context.span().end_byte()
                != u64::try_from(self.source.len()).map_err(|_| invalid_capture())?
            || context.span().file() != file_scope.span().file()
            || modules.is_empty()
            || !self.qualified_types_complete
            || !self.qualified_opens_complete
        {
            return Ok(None);
        }
        for fact in self.facts.values().copied() {
            cancellation.check()?;
            let parent = fact.parent().and_then(|id| self.facts.get(&id).copied());
            let owner = parent.and_then(|parent| {
                if modules.contains_key(&parent.local_id()) {
                    Some(parent.local_id())
                } else if parent.kind() == SyntaxFactKind::Scope {
                    parent.parent()
                } else {
                    None
                }
            });
            let Some(module) = owner.and_then(|owner| modules.get_mut(&owner)) else {
                continue;
            };
            match fact.kind() {
                SyntaxFactKind::Scope if lexical::module_scope(fact) => {
                    module.scope.retain(fact)?
                }
                SyntaxFactKind::Signature
                    if fact.syntax_kind().as_str().ends_with(".signature") =>
                {
                    module.header.retain(fact)?
                }
                SyntaxFactKind::Occurrence if fact.syntax_kind().as_str() == DEFINITION => {
                    module.definition.retain(fact)?
                }
                _ => {}
            }
        }
        let mut owners = HashMap::<u64, u64>::new();
        let mut children = HashMap::<u64, Vec<&SyntaxFact>>::new();
        let mut nearer_modules = HashSet::<ContentHash>::new();
        let mut roots = HashMap::<u64, Vec<ContentHash>>::new();
        owners
            .try_reserve(modules.len())
            .map_err(allocation_failed)?;
        children
            .try_reserve(modules.len().saturating_add(1))
            .map_err(allocation_failed)?;
        nearer_modules
            .try_reserve(modules.len())
            .map_err(allocation_failed)?;
        roots
            .try_reserve(modules.len())
            .map_err(allocation_failed)?;
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
                || !qualified_header_bytes(module, self.source, cancellation)?
            {
                return Ok(None);
            }
            let Some(name) = source_name(definition, self.source, self.maximum_name_bytes)?
                .and_then(written_components)
            else {
                return Ok(None);
            };
            if module.declaration.syntax_kind().as_str() != "fsharp.namespace.declaration" {
                let Some(last) = name.last() else {
                    return Ok(None);
                };
                nearer_modules.insert(content_hash(canonical_identifier(last).as_bytes()));
            }
            owners.insert(scope.local_id(), id);
            children.entry(scope.local_id()).or_default();
            roots.entry(scope.local_id()).or_default();
            let Some(parent) = module.declaration.parent() else {
                return Ok(None);
            };
            let entries = children.entry(parent).or_default();
            entries.try_reserve(1).map_err(allocation_failed)?;
            entries.push(module.declaration);
        }
        for module in modules.values() {
            cancellation.check()?;
            let Some(parent_roots) = module
                .declaration
                .parent()
                .and_then(|parent| roots.get_mut(&parent))
            else {
                continue;
            };
            let Some(definition) = module.definition.one() else {
                return Ok(None);
            };
            let Some(parts) = source_name(definition, self.source, self.maximum_name_bytes)?
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
        // Types and ordinary DU constructors remain closed roots, not value
        // exports or a claim about compiler precedence between homonyms.
        for record in self.qualified_types.values() {
            cancellation.check()?;
            let (Some(marker), Some(name), Some(parent)) = (
                record.marker.one(),
                record.name,
                record
                    .declaration
                    .parent()
                    .filter(|id| owners.contains_key(id)),
            ) else {
                return Ok(None);
            };
            nearer_modules.try_reserve(1).map_err(allocation_failed)?;
            nearer_modules.insert(content_hash(canonical_identifier(name).as_bytes()));
            let entries = children.entry(parent).or_default();
            entries.try_reserve(1).map_err(allocation_failed)?;
            entries.push(marker);
            let Some(record_roots) = roots.get_mut(&parent) else {
                return Ok(None);
            };
            record_roots.try_reserve(1).map_err(allocation_failed)?;
            record_roots.push(content_hash(canonical_identifier(name).as_bytes()));
            nearer_modules
                .try_reserve(record.case_roots.len())
                .map_err(allocation_failed)?;
            record_roots
                .try_reserve(record.case_roots.len())
                .map_err(allocation_failed)?;
            for name in &record.case_roots {
                cancellation.check()?;
                let root = content_hash(canonical_identifier(name).as_bytes());
                nearer_modules.insert(root);
                record_roots.push(root);
            }
        }
        let mut claims = Vec::new();
        claims
            .try_reserve(self.facts.len())
            .map_err(allocation_failed)?;
        for declaration in self.facts.values().copied() {
            cancellation.check()?;
            if declaration.kind() != SyntaxFactKind::Declaration
                || !matches!(
                    declaration.syntax_kind().as_str(),
                    FUNCTION | "fsharp.variable.declaration"
                )
            {
                continue;
            }
            let Some(module_scope) = declaration.parent().filter(|id| owners.contains_key(id))
            else {
                continue;
            };
            let Some((symbol, slots, definition)) = lexical.export(declaration.local_id()) else {
                return Ok(None);
            };
            let mut path = qualified_owner_path(
                module_scope,
                &modules,
                &owners,
                file_scope.local_id(),
                self.source,
                self.maximum_name_bytes,
                cancellation,
            )?;
            let Some(mut path) = path.take() else {
                return Ok(None);
            };
            let Some(name) = source_name(definition, self.source, self.maximum_name_bytes)?
                .and_then(written_components)
                .filter(|parts| parts.len() == 1)
            else {
                return Ok(None);
            };
            if path.len() >= rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS {
                return Ok(None);
            }
            path.try_reserve(1).map_err(allocation_failed)?;
            let Some(name) = name.first() else {
                return Ok(None);
            };
            path.push(content_hash(canonical_identifier(name).as_bytes()));
            let Some(module_roots) = roots.get_mut(&module_scope) else {
                return Ok(None);
            };
            module_roots.try_reserve(1).map_err(allocation_failed)?;
            module_roots.push(content_hash(canonical_identifier(name).as_bytes()));
            let slots = slots
                .map(u32::try_from)
                .transpose()
                .map_err(|_| invalid_capture())?;
            claims.push(QualifiedClaim {
                source: declaration.span(),
                binding: FSharpBinding::Export {
                    path,
                    symbol,
                    slots,
                },
            });
            let entries = children.entry(module_scope).or_default();
            entries.try_reserve(1).map_err(allocation_failed)?;
            entries.push(declaration);
        }
        let mut open_spans = Vec::new();
        open_spans
            .try_reserve_exact(self.qualified_opens.len())
            .map_err(allocation_failed)?;
        for open in &self.qualified_opens {
            cancellation.check()?;
            let Some(parent) = open
                .fact
                .parent()
                .filter(|parent| owners.contains_key(parent))
            else {
                return Ok(None);
            };
            let entries = children.entry(parent).or_default();
            entries.try_reserve(1).map_err(allocation_failed)?;
            entries.push(open.fact);
            let mut path = Vec::new();
            path.try_reserve_exact(open.path.len())
                .map_err(allocation_failed)?;
            path.extend_from_slice(&open.path);
            claims.push(QualifiedClaim {
                source: open.fact.span(),
                binding: FSharpBinding::Open {
                    path,
                    owner: open.owner,
                },
            });
            open_spans.push(open.fact.span());
        }
        // Every direct declaration must tile its owned container. This catches
        // dropped exports even when the native file-context marker survived.
        for (&scope, entries) in &mut children {
            cancellation.check()?;
            crate::runtime::sort_cancellable_by(entries, cancellation, |left, right| {
                (left.span().start_byte(), left.span().end_byte())
                    .cmp(&(right.span().start_byte(), right.span().end_byte()))
            })?;
            let (mut end, container_end, nested) = if scope == file_scope.local_id() {
                (0, context.span().end_byte(), false)
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
            for (index, declaration) in entries.iter().enumerate() {
                if declaration.span().start_byte() < end
                    || declaration.span().end_byte() > container_end
                {
                    return Ok(None);
                }
                let recursive = lexical.recursive_member(declaration.local_id()).is_some();
                let binding = matches!(
                    declaration.syntax_kind().as_str(),
                    FUNCTION | "fsharp.variable.declaration"
                );
                if !qualified_gap(
                    self.source,
                    end,
                    declaration.span().start_byte(),
                    binding,
                    recursive,
                    nested && index == 0,
                    cancellation,
                )? {
                    return Ok(None);
                }
                end = declaration.span().end_byte();
            }
            if !trivia_between(self.source, end, container_end, cancellation)? {
                return Ok(None);
            }
        }
        if !children.contains_key(&file_scope.local_id())
            || owners.keys().any(|scope| !children.contains_key(scope))
        {
            return Ok(None);
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
                self.source,
                self.maximum_name_bytes,
                cancellation,
            )?
            else {
                return Ok(None);
            };
            crate::runtime::sort_cancellable_by(names, cancellation, Ord::cmp)?;
            let mut unique = Vec::new();
            unique
                .try_reserve_exact(names.len())
                .map_err(allocation_failed)?;
            for &name in names.iter() {
                cancellation.check()?;
                if unique.last() != Some(&name) {
                    unique.push(name);
                }
            }
            claims.push(QualifiedClaim {
                source: module.declaration.span(),
                binding: FSharpBinding::Container {
                    path,
                    roots: unique,
                },
            });
            container_spans.push(module.declaration.span());
        }
        crate::runtime::sort_cancellable_by(&mut container_spans, cancellation, Ord::cmp)?;
        crate::runtime::sort_cancellable_by(&mut open_spans, cancellation, Ord::cmp)?;
        if container_spans.windows(2).any(|spans| spans[0] == spans[1])
            || open_spans.windows(2).any(|spans| spans[0] == spans[1])
        {
            return Ok(None);
        }
        claims.push(QualifiedClaim {
            source: context.span(),
            binding: FSharpBinding::ImportContext {
                containers: container_spans,
                opens: open_spans,
            },
        });
        let mut reads = HashSet::new();
        reads
            .try_reserve(self.facts.len())
            .map_err(allocation_failed)?;
        for read in self.facts.values().copied().filter(|fact| {
            fact.syntax_kind().as_str() == QUALIFIED_READ
                || !self.qualified_opens.is_empty() && fact.syntax_kind().as_str() == READ
        }) {
            cancellation.check()?;
            if read.kind() != SyntaxFactKind::Occurrence || !reads.insert(read.span()) {
                return Ok(None);
            }
            if read.syntax_kind().as_str() == READ {
                if let Some(name) = self.opened_name(read, cancellation)?
                    && !nearer_modules.contains(&name)
                {
                    claims.push(QualifiedClaim {
                        source: read.span(),
                        binding: FSharpBinding::OpenedReference { name },
                    });
                }
                continue;
            }
            let Some(parts) = source_name(read, self.source, self.maximum_name_bytes)?
                .and_then(written_components)
                .filter(|parts| parts.len() > 1)
            else {
                return Ok(None);
            };
            let path = component_hashes(&parts)?;
            let (Some(first), Some(root)) = (path.first(), parts.first()) else {
                return Ok(None);
            };
            let selection = self.qualified_root(read, root);
            if nearer_modules.contains(first)
                || !matches!(
                    selection,
                    QualifiedRoot::ProvenAbsent | QualifiedRoot::ImportsPending
                )
            {
                continue;
            }
            claims.push(QualifiedClaim {
                source: read.span(),
                binding: if selection == QualifiedRoot::ImportsPending {
                    FSharpBinding::ImportedReference { path }
                } else {
                    FSharpBinding::Reference { path }
                },
            });
        }
        for call in self
            .facts
            .values()
            .copied()
            .filter(|fact| fact.syntax_kind().as_str() == lexical::CALL)
        {
            cancellation.check()?;
            let Some(read) = self.callee_read(call) else {
                continue;
            };
            if read.syntax_kind().as_str() == READ {
                if self.qualified_opens.is_empty() {
                    continue;
                }
                if let Some(name) = self.opened_name(read, cancellation)?
                    && !nearer_modules.contains(&name)
                {
                    let Some(slots) = lexical.argument_slots(call) else {
                        return Ok(None);
                    };
                    claims.push(QualifiedClaim {
                        source: call.span(),
                        binding: FSharpBinding::OpenedCall {
                            name,
                            slots: u32::try_from(slots).map_err(|_| invalid_capture())?,
                        },
                    });
                }
                continue;
            }
            let Some(parts) = source_name(read, self.source, self.maximum_name_bytes)?
                .and_then(written_components)
                .filter(|parts| parts.len() > 1)
            else {
                return Ok(None);
            };
            let path = component_hashes(&parts)?;
            let (Some(first), Some(root)) = (path.first(), parts.first()) else {
                return Ok(None);
            };
            let selection = self.qualified_root(read, root);
            if nearer_modules.contains(first)
                || !matches!(
                    selection,
                    QualifiedRoot::ProvenAbsent | QualifiedRoot::ImportsPending
                )
            {
                continue;
            }
            let Some(slots) = lexical.argument_slots(call) else {
                return Ok(None);
            };
            claims.push(QualifiedClaim {
                source: call.span(),
                binding: if selection == QualifiedRoot::ImportsPending {
                    FSharpBinding::ImportedCall {
                        path,
                        slots: u32::try_from(slots).map_err(|_| invalid_capture())?,
                    }
                } else {
                    FSharpBinding::Call {
                        path,
                        slots: u32::try_from(slots).map_err(|_| invalid_capture())?,
                    }
                },
            });
        }
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
                qualified_claim_order(&left.binding),
            )
                .cmp(&(
                    right.source.start_byte(),
                    right.source.end_byte(),
                    qualified_claim_order(&right.binding),
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
        // Apply the permutation without cloning owned path vectors. Each swap
        // fixes one destination, and cancellation remains observable throughout.
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
}

fn qualified_type_name<'a>(
    record: &QualifiedType<'a>,
    facts: &HashMap<u64, &'a SyntaxFact>,
    source: &'a [u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Option<&'a str>, AdapterError> {
    cancellation.check()?;
    let declaration = record.declaration;
    let (Some(header), Some(definition), Some(marker), Some(module)) = (
        record.header.one(),
        record.definition.one(),
        record.marker.one(),
        declaration
            .parent()
            .and_then(|id| facts.get(&id).copied())
            .filter(|parent| lexical::module_scope(parent)),
    ) else {
        return Ok(None);
    };
    let (expected_header, expected_marker) = match declaration.syntax_kind().as_str() {
        RECORD => ("fsharp.record.signature", QUALIFIED_RECORD),
        ALIAS => ("fsharp.type_alias.signature", QUALIFIED_ALIAS),
        ENUM => ("fsharp.enum.signature", QUALIFIED_ENUM),
        UNION => ("fsharp.union.signature", QUALIFIED_UNION),
        CLASS => ("fsharp.class.signature", QUALIFIED_CLASS),
        _ => return Ok(None),
    };
    let alias = declaration.syntax_kind().as_str() == ALIAS;
    let scope = record.scope.one();
    let ownership = if alias {
        record.scope.count == 0
            && header.parent() == Some(declaration.local_id())
            && header.span() == declaration.span()
            && definition.parent() == Some(declaration.local_id())
    } else {
        scope.is_some_and(|scope| scope.kind() == SyntaxFactKind::Scope
            && scope.parent() == Some(declaration.local_id())
            && scope.span() == declaration.span()
            && header.parent() == Some(scope.local_id())
            && header.span().end_byte() < declaration.span().end_byte()
            && contains(scope, header)
            && matches!(definition.parent(), Some(id) if id == scope.local_id() || id == declaration.local_id()))
    };
    if !ownership
        || header.kind() != SyntaxFactKind::Signature
        || header.syntax_kind().as_str() != expected_header
        || header.span().start_byte() != declaration.span().start_byte()
        || definition.kind() != SyntaxFactKind::Occurrence
        || !contains(header, definition)
        || marker.kind() != SyntaxFactKind::Signature
        || marker.syntax_kind().as_str() != expected_marker
        || marker.parent() != Some(module.local_id())
        || marker.span().start_byte() >= declaration.span().start_byte()
        || marker.span().end_byte() != declaration.span().end_byte()
        || !contains(marker, declaration)
        || !contains(module, marker)
    {
        return Ok(None);
    }
    // Alias signatures cover the RHS as well as the head. Containment alone
    // could therefore replace the authored typename with an RHS identifier.
    let plain_head = if declaration.syntax_kind().as_str() == CLASS {
        qualified_class_head(declaration, header, definition, source, cancellation)?
    } else {
        declaration.syntax_kind().as_str() == RECORD
            || qualified_plain_type_head(declaration, definition, source, cancellation)?
    };
    if !plain_head {
        return Ok(None);
    }
    let start = usize::try_from(marker.span().start_byte()).map_err(|_| invalid_capture())?;
    let end = usize::try_from(declaration.span().start_byte()).map_err(|_| invalid_capture())?;
    let Some(prefix) = source
        .get(start..end)
        .and_then(|bytes| bytes.strip_prefix(b"type"))
    else {
        return Ok(None);
    };
    if trivia_end(prefix, 0, cancellation)? != Some(prefix.len()) {
        return Ok(None);
    }
    Ok(source_name(definition, source, maximum_name_bytes)?
        .filter(|name| unqualified_written_name(definition, name, maximum_name_bytes)))
}

fn qualified_union_case_roots<'a>(
    union: &QualifiedType<'a>,
    mut cases: Vec<&QualifiedUnionCase<'a>>,
    source: &'a [u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Option<Vec<&'a str>>, AdapterError> {
    let (Some(scope), Some(header)) = (union.scope.one(), union.header.one()) else {
        return Ok(None);
    };
    if cases.is_empty() {
        return Ok(None);
    }
    crate::runtime::sort_cancellable_by(&mut cases, cancellation, |left, right| {
        left.declaration.span().cmp(&right.declaration.span())
    })?;
    let mut names = Vec::new();
    let mut seen = HashSet::new();
    names
        .try_reserve_exact(cases.len())
        .map_err(allocation_failed)?;
    seen.try_reserve(cases.len()).map_err(allocation_failed)?;
    let mut position = header.span().end_byte();
    for (index, case) in cases.iter().enumerate() {
        cancellation.check()?;
        let declaration = case.declaration;
        let (Some(case_header), Some(definition)) = (case.header.one(), case.definition.one())
        else {
            return Ok(None);
        };
        if declaration.parent() != Some(scope.local_id())
            || !contains(scope, declaration)
            || case_header.kind() != SyntaxFactKind::Signature
            || case_header.parent() != Some(declaration.local_id())
            || case_header.span() != declaration.span()
            || definition.kind() != SyntaxFactKind::Occurrence
            || definition.parent() != Some(declaration.local_id())
            || !contains(declaration, definition)
            || definition.span().start_byte() != declaration.span().start_byte()
            || declaration.span().start_byte() < position
        {
            return Ok(None);
        }
        let start = usize::try_from(position).map_err(|_| invalid_capture())?;
        let end =
            usize::try_from(declaration.span().start_byte()).map_err(|_| invalid_capture())?;
        let gap = source.get(start..end).ok_or_else(invalid_capture)?;
        let Some(mut cursor) = trivia_end(gap, 0, cancellation)? else {
            return Ok(None);
        };
        if index == 0 {
            if gap.get(cursor) != Some(&b'=') {
                return Ok(None);
            }
            let Some(next) = trivia_end(
                gap,
                cursor.checked_add(1).ok_or_else(invalid_capture)?,
                cancellation,
            )?
            else {
                return Ok(None);
            };
            cursor = next;
            if gap
                .get(cursor..)
                .is_some_and(|rest| rest.starts_with(b"public"))
            {
                let end = cursor.checked_add(6).ok_or_else(invalid_capture)?;
                let Some(next) = trivia_end(gap, end, cancellation)?.filter(|next| *next > end)
                else {
                    return Ok(None);
                };
                cursor = next;
            }
        }
        if gap.get(cursor) == Some(&b'|') {
            let Some(next) = trivia_end(
                gap,
                cursor.checked_add(1).ok_or_else(invalid_capture)?,
                cancellation,
            )?
            else {
                return Ok(None);
            };
            cursor = next;
        } else if index != 0 {
            return Ok(None);
        }
        if cursor != gap.len() {
            return Ok(None);
        }
        let Some(name) = source_name(definition, source, maximum_name_bytes)?
            .filter(|name| unqualified_written_name(definition, name, maximum_name_bytes))
        else {
            return Ok(None);
        };
        if !seen.insert(content_hash(canonical_identifier(name).as_bytes())) {
            return Ok(None);
        }
        let suffix_start =
            usize::try_from(definition.span().end_byte()).map_err(|_| invalid_capture())?;
        let suffix_end =
            usize::try_from(declaration.span().end_byte()).map_err(|_| invalid_capture())?;
        let suffix = source
            .get(suffix_start..suffix_end)
            .ok_or_else(invalid_capture)?;
        let Some(cursor) = trivia_end(suffix, 0, cancellation)? else {
            return Ok(None);
        };
        if cursor != suffix.len() {
            if cursor == 0
                || !suffix
                    .get(cursor..)
                    .is_some_and(|rest| rest.starts_with(b"of"))
            {
                return Ok(None);
            }
            let end = cursor.checked_add(2).ok_or_else(invalid_capture)?;
            if trivia_end(suffix, end, cancellation)?
                .is_none_or(|next| next == end || next == suffix.len())
            {
                return Ok(None);
            }
            // A widened payload capture must not swallow a later authored case.
            if !qualified_type_fragment(
                suffix.get(end..).ok_or_else(invalid_capture)?,
                false,
                cancellation,
            )? {
                return Ok(None);
            }
        }
        names.push(name);
        position = declaration.span().end_byte();
    }
    if !trivia_between(
        source,
        position,
        union.declaration.span().end_byte(),
        cancellation,
    )? {
        return Ok(None);
    }
    Ok(Some(names))
}

fn qualified_class_head(
    declaration: &SyntaxFact,
    header: &SyntaxFact,
    definition: &SyntaxFact,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let start = usize::try_from(declaration.span().start_byte()).map_err(|_| invalid_capture())?;
    let name_start =
        usize::try_from(definition.span().start_byte()).map_err(|_| invalid_capture())?;
    let name_end = usize::try_from(definition.span().end_byte()).map_err(|_| invalid_capture())?;
    let end = usize::try_from(header.span().end_byte()).map_err(|_| invalid_capture())?;
    if source.get(end) != Some(&b'=') {
        return Ok(false);
    }
    let prefix = source.get(start..name_start).ok_or_else(invalid_capture)?;
    let Some(mut position) = trivia_end(prefix, 0, cancellation)? else {
        return Ok(false);
    };
    if position != prefix.len() {
        if !prefix
            .get(position..)
            .is_some_and(|rest| rest.starts_with(b"public"))
        {
            return Ok(false);
        }
        let end = position.checked_add(6).ok_or_else(invalid_capture)?;
        let Some(next) = trivia_end(prefix, end, cancellation)?.filter(|next| *next > end) else {
            return Ok(false);
        };
        position = next;
    }
    if position != prefix.len() {
        return Ok(false);
    }
    let suffix = source.get(name_end..end).ok_or_else(invalid_capture)?;
    let Some(position) = trivia_end(suffix, 0, cancellation)? else {
        return Ok(false);
    };
    // The native whole-wrapper witness owns constructor syntax. Only its exact
    // head boundary is needed here; constructor parameters are not outer roots.
    Ok(
        (position == suffix.len() || suffix.get(position) == Some(&b'('))
            && qualified_type_fragment(suffix, true, cancellation)?,
    )
}

fn qualified_type_fragment(
    bytes: &[u8],
    constructor: bool,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut position = 0usize;
    let mut parentheses = 0usize;
    while position < bytes.len() {
        cancellation.check()?;
        let Some(next) = trivia_end(bytes, position, cancellation)? else {
            return Ok(false);
        };
        if next > position {
            position = next;
            continue;
        }
        if bytes
            .get(position..)
            .is_some_and(|rest| rest.starts_with(b"``"))
        {
            position = position.checked_add(2).ok_or_else(invalid_capture)?;
            loop {
                cancellation.check()?;
                if bytes
                    .get(position..)
                    .is_some_and(|rest| rest.starts_with(b"``"))
                {
                    position = position.checked_add(2).ok_or_else(invalid_capture)?;
                    break;
                }
                if position >= bytes.len() {
                    return Ok(false);
                }
                position = position.checked_add(1).ok_or_else(invalid_capture)?;
            }
            continue;
        }
        match bytes.get(position) {
            Some(b'(') => parentheses = parentheses.checked_add(1).ok_or_else(invalid_capture)?,
            Some(b')') => {
                let Some(depth) = parentheses.checked_sub(1) else {
                    return Ok(false);
                };
                parentheses = depth;
            }
            Some(b'=') if constructor => return Ok(false),
            Some(b'|') if !constructor && parentheses == 0 => return Ok(false),
            _ => {}
        }
        position = position.checked_add(1).ok_or_else(invalid_capture)?;
    }
    Ok(parentheses == 0)
}

fn qualified_plain_type_head(
    declaration: &SyntaxFact,
    definition: &SyntaxFact,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let start = usize::try_from(declaration.span().start_byte()).map_err(|_| invalid_capture())?;
    let name_start =
        usize::try_from(definition.span().start_byte()).map_err(|_| invalid_capture())?;
    let name_end = usize::try_from(definition.span().end_byte()).map_err(|_| invalid_capture())?;
    let end = usize::try_from(declaration.span().end_byte()).map_err(|_| invalid_capture())?;
    let prefix = source.get(start..name_start).ok_or_else(invalid_capture)?;
    let Some(mut position) = trivia_end(prefix, 0, cancellation)? else {
        return Ok(false);
    };
    if position != prefix.len() {
        if !prefix
            .get(position..)
            .is_some_and(|tail| tail.starts_with(b"public"))
        {
            return Ok(false);
        }
        let modifier_end = position.checked_add(6).ok_or_else(invalid_capture)?;
        let Some(next) =
            trivia_end(prefix, modifier_end, cancellation)?.filter(|next| *next > modifier_end)
        else {
            return Ok(false);
        };
        position = next;
    }
    if position != prefix.len() {
        return Ok(false);
    }
    let suffix = source.get(name_end..end).ok_or_else(invalid_capture)?;
    let Some(position) = trivia_end(suffix, 0, cancellation)? else {
        return Ok(false);
    };
    Ok(suffix.get(position) == Some(&b'='))
}

fn qualified_header_bytes(
    module: &QualifiedModule<'_>,
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    cancellation.check()?;
    let (Some(header), Some(definition)) = (module.header.one(), module.definition.one()) else {
        return Ok(false);
    };
    let start = usize::try_from(header.span().start_byte()).map_err(|_| invalid_capture())?;
    let name_start =
        usize::try_from(definition.span().start_byte()).map_err(|_| invalid_capture())?;
    let prefix = std::str::from_utf8(source.get(start..name_start).ok_or_else(invalid_capture)?)
        .map_err(|_| invalid_capture())?;
    let keyword = if module.declaration.syntax_kind().as_str() == "fsharp.namespace.declaration" {
        "namespace"
    } else {
        "module"
    };
    let Some(rest) = prefix.strip_prefix(keyword) else {
        return Ok(false);
    };
    let rest = rest.trim();
    Ok((rest.is_empty() || rest == "public")
        && trivia_between(
            source,
            definition.span().end_byte(),
            header.span().end_byte(),
            cancellation,
        )?)
}

fn qualified_owner_path(
    scope: u64,
    modules: &HashMap<u64, QualifiedModule<'_>>,
    owners: &HashMap<u64, u64>,
    file_scope: u64,
    source: &[u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Option<Vec<ContentHash>>, AdapterError> {
    if owners
        .get(&scope)
        .and_then(|id| modules.get(id))
        .is_some_and(|module| {
            module.declaration.syntax_kind().as_str() == "fsharp.namespace.declaration"
        })
    {
        return Ok(None);
    }
    qualified_container_path(
        scope,
        modules,
        owners,
        file_scope,
        source,
        maximum_name_bytes,
        cancellation,
    )
}

fn qualified_container_path(
    mut scope: u64,
    modules: &HashMap<u64, QualifiedModule<'_>>,
    owners: &HashMap<u64, u64>,
    file_scope: u64,
    source: &[u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Option<Vec<ContentHash>>, AdapterError> {
    let mut chain = Vec::new();
    while scope != file_scope {
        cancellation.check()?;
        if chain.len() >= rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS {
            return Ok(None);
        }
        let Some(module) = owners.get(&scope).and_then(|id| modules.get(id)) else {
            return Ok(None);
        };
        chain.try_reserve(1).map_err(allocation_failed)?;
        chain.push(module);
        let Some(parent) = module.declaration.parent() else {
            return Ok(None);
        };
        scope = parent;
    }
    let mut path = Vec::new();
    for module in chain.into_iter().rev() {
        cancellation.check()?;
        let Some(definition) = module.definition.one() else {
            return Ok(None);
        };
        let Some(parts) =
            source_name(definition, source, maximum_name_bytes)?.and_then(written_components)
        else {
            return Ok(None);
        };
        if path.len().saturating_add(parts.len()) > rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS {
            return Ok(None);
        }
        path.try_reserve(parts.len()).map_err(allocation_failed)?;
        path.extend(
            parts
                .into_iter()
                .map(|part| content_hash(canonical_identifier(part).as_bytes())),
        );
    }
    Ok((!path.is_empty()).then_some(path))
}

fn trivia_end(
    bytes: &[u8],
    mut position: usize,
    cancellation: &Cancellation,
) -> Result<Option<usize>, AdapterError> {
    if position == 0 && bytes.starts_with(b"\xef\xbb\xbf") {
        position = 3;
    }
    loop {
        cancellation.check()?;
        let Some(tail) = bytes.get(position..) else {
            return Ok(None);
        };
        if tail.first().is_some_and(u8::is_ascii_whitespace) {
            position = position.checked_add(1).ok_or_else(invalid_capture)?;
        } else if tail.starts_with(b"//") {
            while bytes.get(position).is_some_and(|byte| *byte != b'\n') {
                cancellation.check()?;
                position = position.checked_add(1).ok_or_else(invalid_capture)?;
            }
        } else if tail.starts_with(b"(*") {
            let mut depth = 1usize;
            position = position.checked_add(2).ok_or_else(invalid_capture)?;
            while depth > 0 {
                cancellation.check()?;
                let Some(tail) = bytes.get(position..) else {
                    return Ok(None);
                };
                if tail.starts_with(b"(*") {
                    depth = depth.checked_add(1).ok_or_else(invalid_capture)?;
                    position = position.checked_add(2).ok_or_else(invalid_capture)?;
                } else if tail.starts_with(b"*)") {
                    depth -= 1;
                    position = position.checked_add(2).ok_or_else(invalid_capture)?;
                } else if tail.is_empty() {
                    return Ok(None);
                } else {
                    position = position.checked_add(1).ok_or_else(invalid_capture)?;
                }
            }
        } else {
            return Ok(Some(position));
        }
    }
}

fn trivia_between(
    source: &[u8],
    start: u64,
    end: u64,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let start = usize::try_from(start).map_err(|_| invalid_capture())?;
    let end = usize::try_from(end).map_err(|_| invalid_capture())?;
    let bytes = source.get(start..end).ok_or_else(invalid_capture)?;
    Ok(trivia_end(bytes, 0, cancellation)? == Some(bytes.len()))
}

fn qualified_gap(
    source: &[u8],
    start: u64,
    end: u64,
    binding: bool,
    recursive: bool,
    nested_first: bool,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let start = usize::try_from(start).map_err(|_| invalid_capture())?;
    let end = usize::try_from(end).map_err(|_| invalid_capture())?;
    let bytes = source.get(start..end).ok_or_else(invalid_capture)?;
    let Some(mut position) = trivia_end(bytes, 0, cancellation)? else {
        return Ok(false);
    };
    if nested_first {
        if bytes.get(position) != Some(&b'=') {
            return Ok(false);
        }
        let Some(next) = trivia_end(
            bytes,
            position.checked_add(1).ok_or_else(invalid_capture)?,
            cancellation,
        )?
        else {
            return Ok(false);
        };
        position = next;
    }
    if binding {
        let Some(tail) = bytes.get(position..) else {
            return Ok(false);
        };
        let length = if tail.starts_with(b"let") || (recursive && tail.starts_with(b"and")) {
            3
        } else {
            return Ok(false);
        };
        let Some(next) = trivia_end(
            bytes,
            position.checked_add(length).ok_or_else(invalid_capture)?,
            cancellation,
        )?
        else {
            return Ok(false);
        };
        position = next;
        if recursive
            && bytes
                .get(position..)
                .is_some_and(|tail| tail.starts_with(b"rec"))
        {
            let Some(next) = trivia_end(
                bytes,
                position.checked_add(3).ok_or_else(invalid_capture)?,
                cancellation,
            )?
            else {
                return Ok(false);
            };
            position = next;
        }
    }
    Ok(position == bytes.len())
}

fn canonical_identifier(name: &str) -> &str {
    name.strip_prefix("``")
        .and_then(|name| name.strip_suffix("``"))
        .unwrap_or(name)
}

fn qualified_claim_order(binding: &FSharpBinding) -> u8 {
    match binding {
        FSharpBinding::Context => 0,
        FSharpBinding::Export { .. } => 1,
        FSharpBinding::Reference { .. } => 2,
        FSharpBinding::Call { .. } => 3,
        FSharpBinding::Project { .. } => 4,
        FSharpBinding::Container { .. } => 5,
        FSharpBinding::Open { .. } => 6,
        FSharpBinding::ImportedReference { .. } => 7,
        FSharpBinding::ImportedCall { .. } => 8,
        FSharpBinding::ImportContext { .. } => 9,
        FSharpBinding::SignatureValue { .. } => 10,
        FSharpBinding::SignatureType { .. } => 11,
        FSharpBinding::SignatureAlias { .. } => 12,
        FSharpBinding::OpenedReference { .. } => 13,
        FSharpBinding::OpenedCall { .. } => 14,
    }
}

fn written_components(mut text: &str) -> Option<Vec<&str>> {
    let mut parts = Vec::new();
    while !text.is_empty() {
        if parts.len() >= rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS {
            return None;
        }
        let end = if let Some(quoted) = text.strip_prefix("``") {
            quoted.find("``")?.checked_add(4)?
        } else {
            text.find(|ch: char| ch == '.' || ch.is_whitespace())
                .unwrap_or(text.len())
        };
        let part = text.get(..end)?;
        let canonical = canonical_identifier(part);
        let mut chars = canonical.chars();
        let first = chars.next()?;
        let bare_character = |ch: char| {
            ch == '_'
                || ch == '\''
                || ch.is_alphanumeric()
                || !ch.is_ascii() && !ch.is_control() && !ch.is_whitespace()
        };
        if canonical.is_empty()
            || canonical.chars().any(char::is_control)
            || !part.starts_with("``")
                && (!(first == '_'
                    || first.is_alphabetic()
                    || !first.is_ascii() && bare_character(first))
                    || !chars.all(bare_character))
        {
            return None;
        }
        parts.try_reserve(1).ok()?;
        parts.push(part);
        text = text.get(end..)?.trim_start();
        if text.is_empty() {
            break;
        }
        text = text.strip_prefix('.')?.trim_start();
        if text.is_empty() {
            return None;
        }
    }
    (!parts.is_empty()).then_some(parts)
}

fn component_hashes(parts: &[&str]) -> Result<Vec<ContentHash>, AdapterError> {
    let mut path = Vec::new();
    path.try_reserve_exact(parts.len())
        .map_err(allocation_failed)?;
    path.extend(
        parts
            .iter()
            .map(|part| content_hash(canonical_identifier(part).as_bytes())),
    );
    Ok(path)
}

fn function_owner(fact: &SyntaxFact, facts: &HashMap<u64, &SyntaxFact>) -> Option<u64> {
    let parent = fact.parent().and_then(|id| facts.get(&id).copied())?;
    if parent.kind() == SyntaxFactKind::Declaration && parent.syntax_kind().as_str() == FUNCTION {
        return Some(parent.local_id());
    }
    if parent.kind() == SyntaxFactKind::Scope && parent.syntax_kind().as_str() == SCOPE {
        let declaration = parent.parent().and_then(|id| facts.get(&id).copied())?;
        if declaration.kind() == SyntaxFactKind::Declaration
            && declaration.syntax_kind().as_str() == FUNCTION
        {
            return Some(declaration.local_id());
        }
    }
    None
}

fn validate_ancestry(
    facts: &HashMap<u64, &SyntaxFact>,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut completed = HashSet::new();
    let mut active = HashSet::new();
    let mut path = Vec::new();
    completed
        .try_reserve(facts.len())
        .map_err(allocation_failed)?;
    active.try_reserve(facts.len()).map_err(allocation_failed)?;
    path.try_reserve(facts.len()).map_err(allocation_failed)?;
    for fact in facts.values() {
        cancellation.check()?;
        let mut current = Some(fact.local_id());
        while let Some(id) = current {
            cancellation.check()?;
            if completed.contains(&id) {
                break;
            }
            let Some(fact) = facts.get(&id).copied() else {
                break;
            };
            if !active.insert(id) || path.len() >= facts.len() {
                return Err(invalid_capture());
            }
            path.push(id);
            current = fact.parent();
        }
        for id in path.drain(..) {
            cancellation.check()?;
            active.remove(&id);
            completed.insert(id);
        }
    }
    Ok(())
}

fn source_bytes<'a>(fact: &SyntaxFact, source: &'a [u8]) -> Result<&'a [u8], AdapterError> {
    let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
    let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
    source.get(start..end).ok_or_else(invalid_capture)
}

fn source_name<'a>(
    fact: &SyntaxFact,
    source: &'a [u8],
    maximum_name_bytes: usize,
) -> Result<Option<&'a str>, AdapterError> {
    let bytes = source_bytes(fact, source)?;
    if bytes.is_empty() || bytes.len() > maximum_name_bytes {
        return Ok(None);
    }
    std::str::from_utf8(bytes)
        .map(Some)
        .map_err(|_| invalid_capture())
}

fn unqualified_written_name(fact: &SyntaxFact, name: &str, maximum_name_bytes: usize) -> bool {
    let unqualified = if let Some(quoted) = name.strip_prefix("``") {
        quoted
            .strip_suffix("``")
            .is_some_and(|quoted| !quoted.contains("``"))
    } else {
        !name.contains('.')
    };
    unqualified
        && structural_captured_name_for_fact("fsharp", fact, name, maximum_name_bytes)
            .is_some_and(|captured| captured.as_ref() == name)
}

fn contains(outer: &SyntaxFact, inner: &SyntaxFact) -> bool {
    outer.span().file() == inner.span().file()
        && outer.span().start_byte() <= inner.span().start_byte()
        && inner.span().end_byte() <= outer.span().end_byte()
}

fn allocation_failed(_: std::collections::TryReserveError) -> AdapterError {
    AdapterError::Sink(SinkError::AllocationFailed)
}

fn invalid_capture() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("fsharp-formal-binding-capture")
            .expect("built-in F# formal binding capture diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;

    const SOURCE: &[u8] = b"f x = x\ng x = x\n";

    #[test]
    fn type_fragments_preserve_case_and_constructor_boundaries() {
        for (text, constructor, complete) in [
            (" value: int", false, true),
            (" ``field|part``: string (* separator | *)", false, true),
            (" (string | null)", false, true),
            (" int | Library of int", false, false),
            (" (int", false, false),
            (" int)", false, false),
            (" ``unterminated", false, false),
            ("(seed: int) ", true, true),
            ("(``seed=part``: int) (* = *)", true, true),
            ("(seed: int) =\n    member this.Read() ", true, false),
        ] {
            assert_eq!(
                qualified_type_fragment(text.as_bytes(), constructor, &Cancellation::new())
                    .unwrap(),
                complete,
                "{text}"
            );
        }
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            qualified_type_fragment(b"(seed: int)", true, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        label: &str,
        start: u64,
        end: u64,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(FileId::from_bytes([7; 20]), start, end).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn function_facts(base: u64, start: u64) -> [SyntaxFact; 7] {
        [
            fact(
                base,
                None,
                SyntaxFactKind::Declaration,
                FUNCTION,
                start,
                start + 7,
            ),
            fact(
                base + 1,
                Some(base),
                SyntaxFactKind::Scope,
                SCOPE,
                start,
                start + 7,
            ),
            fact(
                base + 2,
                Some(base + 1),
                SyntaxFactKind::Signature,
                HEADER,
                start,
                start + 4,
            ),
            fact(
                base + 3,
                Some(base + 1),
                SyntaxFactKind::Signature,
                PARAMETERS,
                start + 2,
                start + 3,
            ),
            fact(
                base + 4,
                Some(base + 1),
                SyntaxFactKind::Declaration,
                PARAMETER,
                start + 2,
                start + 3,
            ),
            fact(
                base + 5,
                Some(base + 4),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                start + 2,
                start + 3,
            ),
            fact(
                base + 6,
                Some(base + 1),
                SyntaxFactKind::Occurrence,
                READ,
                start + 6,
                start + 7,
            ),
        ]
    }

    fn qualified_fixture(
        source: &str,
        parameter: &str,
    ) -> (Vec<SyntaxFact>, HashMap<u64, SymbolId>) {
        body_fixture(source, parameter, "Library.carry", QUALIFIED_READ)
    }

    fn body_fixture(
        source: &str,
        parameter: &str,
        written: &str,
        read_label: &str,
    ) -> (Vec<SyntaxFact>, HashMap<u64, SymbolId>) {
        let end = u64::try_from(source.len()).unwrap();
        let module_name = u64::try_from(source.find("Consumer").unwrap()).unwrap();
        let module_end = module_name + 8;
        let start = u64::try_from(source.find("f ").unwrap()).unwrap();
        let parameter_start = start + 2;
        let parameter_end = parameter_start + u64::try_from(parameter.len()).unwrap();
        let header_end = start
            + u64::try_from(
                source
                    .get(usize::try_from(start).unwrap()..)
                    .unwrap()
                    .find('=')
                    .unwrap(),
            )
            .unwrap();
        let read_start = u64::try_from(if read_label == QUALIFIED_READ {
            source.find(written).unwrap()
        } else {
            source.rfind(written).unwrap()
        })
        .unwrap();
        let body_tail = &source[usize::try_from(read_start).unwrap()..];
        let body_end =
            read_start + u64::try_from(body_tail.find('\n').unwrap_or(body_tail.len())).unwrap();
        let body_end = if source
            .as_bytes()
            .get(usize::try_from(body_end).unwrap().saturating_sub(1))
            == Some(&b'\r')
        {
            body_end - 1
        } else {
            body_end
        };
        let read_end = read_start + u64::try_from(written.len()).unwrap();
        let argument_start = body_end - 1;
        let facts = vec![
            fact(1, None, SyntaxFactKind::Scope, "fsharp.file.scope", 0, end),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Signature,
                QUALIFIED_CONTEXT,
                0,
                end,
            ),
            fact(
                3,
                Some(1),
                SyntaxFactKind::Declaration,
                "fsharp.module.declaration",
                0,
                end,
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Scope,
                "fsharp.module.scope",
                0,
                end,
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Signature,
                "fsharp.module.signature",
                0,
                module_end,
            ),
            fact(
                6,
                Some(4),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                module_name,
                module_end,
            ),
            fact(
                10,
                Some(4),
                SyntaxFactKind::Declaration,
                FUNCTION,
                start,
                body_end,
            ),
            fact(11, Some(10), SyntaxFactKind::Scope, SCOPE, start, body_end),
            fact(
                12,
                Some(11),
                SyntaxFactKind::Signature,
                HEADER,
                start,
                header_end,
            ),
            fact(
                13,
                Some(11),
                SyntaxFactKind::Signature,
                PARAMETERS,
                parameter_start,
                parameter_end,
            ),
            fact(
                14,
                Some(11),
                SyntaxFactKind::Declaration,
                PARAMETER,
                parameter_start,
                parameter_end,
            ),
            fact(
                15,
                Some(14),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                parameter_start,
                parameter_end,
            ),
            fact(
                16,
                Some(11),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                start,
                start + 1,
            ),
            fact(
                17,
                Some(10),
                SyntaxFactKind::Signature,
                "fsharp.nonrecursive_binding.expression",
                start,
                body_end,
            ),
            fact(
                18,
                Some(11),
                SyntaxFactKind::Occurrence,
                read_label,
                read_start,
                read_end,
            ),
            fact(
                20,
                Some(11),
                SyntaxFactKind::Occurrence,
                lexical::CALL,
                read_start,
                body_end,
            ),
            fact(
                21,
                Some(11),
                SyntaxFactKind::Signature,
                "fsharp.application_argument.expression",
                argument_start,
                body_end,
            ),
        ];
        (
            facts,
            HashMap::from([
                (15, SymbolId::from_bytes([15; 20])),
                (16, SymbolId::from_bytes([16; 20])),
            ]),
        )
    }

    fn qualified_record_facts(source: &str, name: &str) -> Vec<SyntaxFact> {
        let marker_start = u64::try_from(source.find("type ").unwrap()).unwrap();
        let start = u64::try_from(source.find(name).unwrap()).unwrap();
        let end = u64::try_from(source.find('}').unwrap() + 1).unwrap();
        let header_end = start
            + u64::try_from(
                source
                    .get(usize::try_from(start).unwrap()..)
                    .unwrap()
                    .find('=')
                    .unwrap(),
            )
            .unwrap();
        let name_end = start + u64::try_from(name.len()).unwrap();
        vec![
            fact(30, Some(4), SyntaxFactKind::Declaration, RECORD, start, end),
            fact(
                31,
                Some(30),
                SyntaxFactKind::Scope,
                "fsharp.record.scope",
                start,
                end,
            ),
            fact(
                32,
                Some(31),
                SyntaxFactKind::Signature,
                "fsharp.record.signature",
                start,
                header_end,
            ),
            fact(
                33,
                Some(31),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                start,
                name_end,
            ),
            fact(
                34,
                Some(4),
                SyntaxFactKind::Signature,
                QUALIFIED_RECORD,
                marker_start,
                end,
            ),
        ]
    }

    fn qualified_open_fact(source: &str, id: u64, parent: u64) -> SyntaxFact {
        let start = source.find("open ").unwrap();
        let end = start + source[start..].find('\n').unwrap_or(source.len() - start);
        let end = if source.as_bytes().get(end.saturating_sub(1)) == Some(&b'\r') {
            end - 1
        } else {
            end
        };
        fact(
            id,
            Some(parent),
            SyntaxFactKind::Import,
            "fsharp.open.import",
            u64::try_from(start).unwrap(),
            u64::try_from(end).unwrap(),
        )
    }

    fn opened_fixture(
        source: &str,
        parameter: &str,
        written: &str,
    ) -> (Vec<SyntaxFact>, HashMap<u64, SymbolId>) {
        let (mut facts, symbols) = body_fixture(source, parameter, written, READ);
        facts.push(qualified_open_fact(source, 40, 4));
        (facts, symbols)
    }

    #[test]
    fn plain_opened_claims_keep_exact_names_sources_and_preceding_open_order() {
        for newline in ["\n", "\r\n"] {
            for (source, written, pending) in [
                (
                    "module Consumer\nopen Library\nlet f x = carry x\n",
                    "carry",
                    true,
                ),
                (
                    "module Consumer\nlet f x = carry x\nopen Library\n",
                    "carry",
                    false,
                ),
                (
                    "module Consumer\nopen Library\nlet f x = ``carry`` x\n",
                    "``carry``",
                    true,
                ),
            ] {
                let source = source.replace('\n', newline);
                let (mut facts, symbols) = opened_fixture(&source, "x", written);
                for _ in 0..2 {
                    let cancellation = Cancellation::new();
                    let plan = FSharpBindings::new(
                        &facts,
                        source.as_bytes(),
                        &symbols,
                        128,
                        &cancellation,
                    )
                    .unwrap();
                    let claims = &plan.repository_claims().unwrap().claims;
                    let read = plan.facts[&18];
                    assert_eq!(plan.resolve(read, written, &cancellation).unwrap(), None);
                    assert_eq!(
                        plan.qualified_root(read, written),
                        if pending {
                            QualifiedRoot::ImportsPending
                        } else {
                            QualifiedRoot::ProvenAbsent
                        }
                    );
                    assert_eq!(
                        claims
                            .iter()
                            .filter(|claim| matches!(
                                claim.binding,
                                FSharpBinding::OpenedReference { .. }
                            ))
                            .count(),
                        usize::from(pending)
                    );
                    assert_eq!(
                        claims
                            .iter()
                            .filter(|claim| matches!(
                                claim.binding,
                                FSharpBinding::OpenedCall { .. }
                            ))
                            .count(),
                        usize::from(pending)
                    );
                    if pending {
                        assert!(claims.iter().any(|claim| claim.source == read.span() && matches!(claim.binding, FSharpBinding::OpenedReference { name } if name == content_hash(b"carry"))));
                        assert!(claims.iter().any(|claim| claim.source == plan.facts[&20].span() && matches!(claim.binding, FSharpBinding::OpenedCall { name, slots: 1 } if name == content_hash(b"carry"))));
                    }
                    assert!(!claims.iter().any(|claim| matches!(
                        claim.binding,
                        FSharpBinding::Reference { .. }
                            | FSharpBinding::Call { .. }
                            | FSharpBinding::ImportedReference { .. }
                            | FSharpBinding::ImportedCall { .. }
                    )));
                    facts.reverse();
                }
            }
        }
    }

    #[test]
    fn plain_opened_absence_never_follows_unavailable_or_shadowed_formal_targets() {
        let source = "module Consumer\nopen Library\nlet f carry = carry x\n";
        let (facts, symbols) = opened_fixture(source, "carry", "carry");
        for available in [true, false] {
            let mut symbols = symbols.clone();
            if !available {
                symbols.remove(&15);
            }
            let cancellation = Cancellation::new();
            let plan = FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation)
                .unwrap();
            assert_eq!(
                plan.qualified_root(plan.facts[&18], "carry"),
                QualifiedRoot::Bound
            );
            assert_eq!(
                plan.resolve(plan.facts[&18], "carry", &cancellation)
                    .unwrap(),
                available.then(|| symbols[&15])
            );
            assert_eq!(
                plan.opened_name(plan.facts[&18], &cancellation).unwrap(),
                None
            );
            assert!(
                plan.repository_claims()
                    .is_none_or(|claims| !claims.claims.iter().any(|claim| matches!(
                        claim.binding,
                        FSharpBinding::OpenedReference { .. } | FSharpBinding::OpenedCall { .. }
                    )))
            );
        }
    }

    #[test]
    fn plain_opened_claims_close_lost_duplicated_and_foreign_proofs_but_keep_read_only_use() {
        let source = "module Consumer\nopen Library\nlet f x = carry x\n";
        let (facts, symbols) = opened_fixture(source, "x", "carry");
        for missing in [2, 5, 6, 12, 13, 17, 40] {
            let mut changed = facts.clone();
            changed.retain(|fact| fact.local_id() != missing);
            let plan = FSharpBindings::new(
                &changed,
                source.as_bytes(),
                &symbols,
                128,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(plan.repository_claims().is_none(), "missing {missing}");
        }
        for duplicate in [5, 12, 13, 17, 18, 40] {
            let mut changed = facts.clone();
            let original = facts
                .iter()
                .find(|fact| fact.local_id() == duplicate)
                .unwrap();
            changed.push(fact(
                100,
                original.parent(),
                original.kind(),
                original.syntax_kind().as_str(),
                original.span().start_byte(),
                original.span().end_byte(),
            ));
            let plan = FSharpBindings::new(
                &changed,
                source.as_bytes(),
                &symbols,
                128,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(plan.repository_claims().is_none(), "duplicate {duplicate}");
        }
        let mut changed = facts.clone();
        let original = changed
            .iter_mut()
            .find(|fact| fact.local_id() == 18)
            .unwrap();
        *original = fact(
            18,
            Some(40),
            original.kind(),
            READ,
            original.span().start_byte(),
            original.span().end_byte(),
        );
        let plan = FSharpBindings::new(
            &changed,
            source.as_bytes(),
            &symbols,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(
            plan.opened_name(plan.facts[&18], &Cancellation::new())
                .unwrap(),
            None
        );
        changed = facts.clone();
        changed.retain(|fact| fact.local_id() != 21);
        let plan = FSharpBindings::new(
            &changed,
            source.as_bytes(),
            &symbols,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        let claims = &plan.repository_claims().unwrap().claims;
        assert!(
            claims
                .iter()
                .any(|claim| matches!(claim.binding, FSharpBinding::OpenedReference { .. }))
        );
        assert!(
            !claims
                .iter()
                .any(|claim| matches!(claim.binding, FSharpBinding::OpenedCall { .. }))
        );
    }

    fn nested_opened_fixture(
        kind: &str,
        binder: &str,
    ) -> (String, Vec<SyntaxFact>, HashMap<u64, SymbolId>) {
        let body = match kind {
            "local" => format!("let {binder} = x in offset"),
            "lambda" => format!("fun {binder} -> offset"),
            "match" => format!("match x with | {binder} -> offset"),
            _ => unreachable!(),
        };
        let source = format!("module Consumer\nopen Library\nlet f x = {body}\n");
        let (mut facts, mut symbols) = opened_fixture(&source, "x", "offset");
        facts.retain(|fact| !matches!(fact.local_id(), 20 | 21));
        let read_start = u64::try_from(source.rfind("offset").unwrap()).unwrap();
        let end = u64::try_from(source.len() - 1).unwrap();
        let read = facts.iter_mut().find(|fact| fact.local_id() == 18).unwrap();
        *read = fact(
            18,
            Some(50),
            SyntaxFactKind::Occurrence,
            READ,
            read_start,
            end,
        );
        if kind == "local" {
            let start =
                u64::try_from(source.find(&format!("let {binder} = x in")).unwrap()).unwrap();
            let name = start + 4;
            let header_end = name + u64::try_from(binder.len()).unwrap() + 1;
            let group_end = header_end + 3;
            facts.extend([
                fact(
                    50,
                    Some(11),
                    SyntaxFactKind::Scope,
                    "fsharp.local_binding.scope",
                    start,
                    end,
                ),
                fact(
                    51,
                    Some(50),
                    SyntaxFactKind::Signature,
                    "fsharp.local_binding.expression",
                    start,
                    end,
                ),
                fact(
                    52,
                    Some(50),
                    SyntaxFactKind::Signature,
                    "fsharp.local_group.expression",
                    start,
                    group_end,
                ),
                fact(
                    53,
                    Some(50),
                    SyntaxFactKind::Signature,
                    "fsharp.local_continuation.expression",
                    read_start,
                    end,
                ),
                fact(
                    60,
                    Some(50),
                    SyntaxFactKind::Declaration,
                    "fsharp.variable.declaration",
                    name,
                    group_end,
                ),
                fact(
                    61,
                    Some(60),
                    SyntaxFactKind::Scope,
                    "fsharp.variable.scope",
                    name,
                    group_end,
                ),
                fact(
                    62,
                    Some(61),
                    SyntaxFactKind::Signature,
                    "fsharp.variable.signature",
                    name,
                    header_end,
                ),
                fact(
                    63,
                    Some(61),
                    SyntaxFactKind::Occurrence,
                    DEFINITION,
                    name,
                    name + u64::try_from(binder.len()).unwrap(),
                ),
            ]);
            symbols.insert(63, SymbolId::from_bytes([63; 20]));
        } else {
            let start = u64::try_from(if kind == "lambda" {
                source.find("fun ").unwrap()
            } else {
                source.find("with | ").unwrap() + 7
            })
            .unwrap();
            let name = if kind == "lambda" { start + 4 } else { start };
            let name_end = name + u64::try_from(binder.len()).unwrap();
            let arrow_end = u64::try_from(source.find("->").unwrap() + 2).unwrap();
            let (scope_label, header_label, pattern_label, body_label, binder_label) =
                if kind == "lambda" {
                    (
                        "fsharp.lambda.scope",
                        "fsharp.lambda_header.expression",
                        PARAMETERS,
                        "fsharp.lambda_body.expression",
                        PARAMETER,
                    )
                } else {
                    (
                        "fsharp.match_branch.scope",
                        "fsharp.match_branch_header.expression",
                        "fsharp.match_branch_pattern.expression",
                        "fsharp.match_branch_body.expression",
                        "fsharp.variable.declaration",
                    )
                };
            facts.extend([
                fact(50, Some(11), SyntaxFactKind::Scope, scope_label, start, end),
                fact(
                    51,
                    Some(50),
                    SyntaxFactKind::Signature,
                    header_label,
                    start,
                    arrow_end,
                ),
                fact(
                    52,
                    Some(50),
                    SyntaxFactKind::Signature,
                    pattern_label,
                    name,
                    name_end,
                ),
                fact(
                    53,
                    Some(50),
                    SyntaxFactKind::Signature,
                    body_label,
                    read_start,
                    end,
                ),
                fact(
                    60,
                    Some(50),
                    SyntaxFactKind::Declaration,
                    binder_label,
                    name,
                    name_end,
                ),
                fact(
                    61,
                    Some(60),
                    SyntaxFactKind::Occurrence,
                    DEFINITION,
                    name,
                    name_end,
                ),
            ]);
            if kind == "lambda" {
                facts.push(fact(
                    62,
                    Some(60),
                    SyntaxFactKind::Signature,
                    PARAMETER_SIGNATURE,
                    name,
                    name_end,
                ));
            }
            symbols.insert(61, SymbolId::from_bytes([61; 20]));
        }
        (source, facts, symbols)
    }

    #[test]
    fn nested_plain_opened_reads_require_absence_not_a_missing_local_target() {
        for kind in ["local", "lambda", "match"] {
            for binder in ["cached", "offset"] {
                let (source, mut facts, symbols) = nested_opened_fixture(kind, binder);
                for _ in 0..2 {
                    let cancellation = Cancellation::new();
                    let plan = FSharpBindings::new(
                        &facts,
                        source.as_bytes(),
                        &symbols,
                        128,
                        &cancellation,
                    )
                    .unwrap();
                    let read = plan.facts[&18];
                    let expected = if binder == "offset" {
                        QualifiedRoot::Bound
                    } else {
                        QualifiedRoot::ImportsPending
                    };
                    assert_eq!(
                        plan.qualified_root(read, "offset"),
                        expected,
                        "{kind} {binder}"
                    );
                    assert_eq!(
                        plan.opened_name(read, &cancellation).unwrap(),
                        (binder != "offset").then(|| content_hash(b"offset")),
                        "{kind} {binder}"
                    );
                    assert_eq!(
                        plan.repository_claims()
                            .unwrap()
                            .claims
                            .iter()
                            .filter(|claim| matches!(
                                claim.binding,
                                FSharpBinding::OpenedReference { .. }
                            ))
                            .count(),
                        usize::from(binder != "offset")
                    );
                    facts.reverse();
                }
                for duplicate in [false, true] {
                    let mut changed = facts.clone();
                    let proof = changed
                        .iter()
                        .find(|fact| fact.local_id() == 51)
                        .unwrap()
                        .clone();
                    if duplicate {
                        changed.push(fact(
                            100,
                            proof.parent(),
                            proof.kind(),
                            proof.syntax_kind().as_str(),
                            proof.span().start_byte(),
                            proof.span().end_byte(),
                        ));
                    } else {
                        changed.retain(|fact| fact.local_id() != 51);
                    }
                    let plan = FSharpBindings::new(
                        &changed,
                        source.as_bytes(),
                        &symbols,
                        128,
                        &Cancellation::new(),
                    )
                    .unwrap();
                    assert_eq!(
                        plan.qualified_root(plan.facts[&18], "offset"),
                        QualifiedRoot::Closed,
                        "{kind} {binder} duplicate {duplicate}"
                    );
                    assert_eq!(
                        plan.resolve(plan.facts[&18], "offset", &Cancellation::new())
                            .unwrap(),
                        None
                    );
                    assert!(!plan.repository_claims().unwrap().claims.iter().any(
                        |claim| matches!(
                            claim.binding,
                            FSharpBinding::OpenedReference { .. }
                                | FSharpBinding::OpenedCall { .. }
                        )
                    ));
                }
                let mut clipped = facts.clone();
                let proof = clipped
                    .iter_mut()
                    .find(|fact| fact.local_id() == 51)
                    .unwrap();
                *proof = fact(
                    51,
                    proof.parent(),
                    proof.kind(),
                    proof.syntax_kind().as_str(),
                    proof.span().start_byte(),
                    proof.span().end_byte() - 1,
                );
                let plan = FSharpBindings::new(
                    &clipped,
                    source.as_bytes(),
                    &symbols,
                    128,
                    &Cancellation::new(),
                )
                .unwrap();
                assert_eq!(
                    plan.qualified_root(plan.facts[&18], "offset"),
                    QualifiedRoot::Closed,
                    "clipped {kind} {binder}"
                );
                if binder == "offset" {
                    let mut unavailable = symbols.clone();
                    unavailable.remove(&if kind == "local" { 63 } else { 61 });
                    let plan = FSharpBindings::new(
                        &facts,
                        source.as_bytes(),
                        &unavailable,
                        128,
                        &Cancellation::new(),
                    )
                    .unwrap();
                    assert!(
                        matches!(
                            plan.qualified_root(plan.facts[&18], "offset"),
                            QualifiedRoot::Bound | QualifiedRoot::Closed
                        ),
                        "unavailable {kind}"
                    );
                    assert_eq!(
                        plan.opened_name(plan.facts[&18], &Cancellation::new())
                            .unwrap(),
                        None
                    );
                    assert_eq!(
                        plan.resolve(plan.facts[&18], "offset", &Cancellation::new())
                            .unwrap(),
                        None
                    );
                }
                let cancellation = Cancellation::new();
                let plan =
                    FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation)
                        .unwrap();
                cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
                assert!(matches!(
                    plan.opened_name(plan.facts[&18], &cancellation),
                    Err(AdapterError::Cancelled { .. })
                ));
            }
        }
    }

    fn qualified_nested_facts(source: &str, id: u64, name: &str, end: u64) -> Vec<SyntaxFact> {
        let prefix = format!("module {name}");
        let start = u64::try_from(source.find(&prefix).unwrap()).unwrap();
        let name_start = start + 7;
        let header_end = name_start + u64::try_from(name.len()).unwrap();
        vec![
            fact(
                id,
                Some(4),
                SyntaxFactKind::Declaration,
                "fsharp.nested_module.declaration",
                start,
                end,
            ),
            fact(
                id + 1,
                Some(id),
                SyntaxFactKind::Scope,
                "fsharp.nested_module.scope",
                start,
                end,
            ),
            fact(
                id + 2,
                Some(id + 1),
                SyntaxFactKind::Signature,
                "fsharp.nested_module.signature",
                start,
                header_end,
            ),
            fact(
                id + 3,
                Some(id + 1),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                name_start,
                header_end,
            ),
        ]
    }

    #[test]
    fn qualified_roots_follow_native_file_root_module_scope_ownership() {
        let source = "module Consumer\nlet f x = Library.carry x\n";
        let (mut facts, symbols) = qualified_fixture(source, "x");
        let end = u64::try_from(source.len()).unwrap();
        facts.extend([
            fact(90, None, SyntaxFactKind::Root, "fsharp.file.root", 0, end),
            fact(
                91,
                Some(90),
                SyntaxFactKind::Module,
                "fsharp.file.module",
                0,
                end,
            ),
        ]);
        for parent in [90, 91] {
            let scope = facts.iter_mut().find(|fact| fact.local_id() == 1).unwrap();
            *scope = fact(
                1,
                Some(parent),
                SyntaxFactKind::Scope,
                "fsharp.file.scope",
                0,
                end,
            );
            let plan = FSharpBindings::new(
                &facts,
                source.as_bytes(),
                &symbols,
                128,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(
                plan.qualified_root(plan.facts[&18], "Library"),
                QualifiedRoot::ProvenAbsent
            );
            assert!(
                plan.repository_claims()
                    .unwrap()
                    .claims
                    .iter()
                    .any(|claim| matches!(claim.binding, FSharpBinding::Reference { .. }))
            );
            assert!(
                plan.repository_claims()
                    .unwrap()
                    .claims
                    .iter()
                    .any(|claim| matches!(claim.binding, FSharpBinding::Call { slots: 1, .. }))
            );
        }
        facts.retain(|fact| fact.local_id() != 90);
        let plan = FSharpBindings::new(
            &facts,
            source.as_bytes(),
            &symbols,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(
            plan.qualified_root(plan.facts[&18], "Library"),
            QualifiedRoot::Closed
        );
        assert!(
            !plan
                .repository_claims()
                .unwrap()
                .claims
                .iter()
                .any(|claim| matches!(
                    claim.binding,
                    FSharpBinding::Reference { .. } | FSharpBinding::Call { .. }
                ))
        );
    }

    #[test]
    fn qualified_file_roots_conserve_bom_and_trivia_margins() {
        let source =
            "\u{feff}// \u{1f680}\nmodule Consumer\nopen Imports\nlet f x = Library.carry x\n";
        let (original, symbols) = qualified_fixture(source, "x");
        let end = u64::try_from(source.len()).unwrap();
        let module_start = u64::try_from(source.find("module Consumer").unwrap()).unwrap();
        for file_start in [3, module_start] {
            let mut facts = original.clone();
            for id in [1, 3, 4, 5] {
                let item = facts.iter_mut().find(|fact| fact.local_id() == id).unwrap();
                let start = if id == 1 { file_start } else { module_start };
                let finish = if id == 5 {
                    item.span().end_byte()
                } else {
                    end - 1
                };
                *item = fact(
                    id,
                    if id == 1 { Some(91) } else { item.parent() },
                    item.kind(),
                    item.syntax_kind().as_str(),
                    start,
                    finish,
                );
            }
            facts.extend([
                fact(
                    90,
                    None,
                    SyntaxFactKind::Root,
                    "fsharp.file.root",
                    file_start,
                    end - 1,
                ),
                fact(
                    91,
                    Some(90),
                    SyntaxFactKind::Module,
                    "fsharp.file.module",
                    file_start,
                    end - 1,
                ),
                qualified_open_fact(source, 40, 4),
            ]);
            let plan = FSharpBindings::new(
                &facts,
                source.as_bytes(),
                &symbols,
                128,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(
                plan.qualified_root(plan.facts[&18], "Library"),
                QualifiedRoot::ImportsPending
            );
            assert!(
                plan.repository_claims()
                    .unwrap()
                    .claims
                    .iter()
                    .any(|claim| matches!(claim.binding, FSharpBinding::ImportedReference { .. }))
            );
            let item = facts.iter_mut().find(|fact| fact.local_id() == 1).unwrap();
            *item = fact(
                1,
                None,
                SyntaxFactKind::Scope,
                "fsharp.file.scope",
                module_start + 1,
                end - 1,
            );
            let plan = FSharpBindings::new(
                &facts,
                source.as_bytes(),
                &symbols,
                128,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(
                plan.qualified_root(plan.facts[&18], "Library"),
                QualifiedRoot::Closed
            );
        }
    }

    #[test]
    fn qualified_open_claims_conserve_exact_sources_and_source_order() {
        for newline in ["\n", "\r\n"] {
            for (source, pending) in [
                (
                    "module Consumer\nopen Imports\nlet f x = Library.carry x\n",
                    true,
                ),
                (
                    "module Consumer\nlet f x = Library.carry x\nopen Imports\n",
                    false,
                ),
            ] {
                let source = source.replace('\n', newline);
                let (mut facts, symbols) = qualified_fixture(&source, "x");
                let open = qualified_open_fact(&source, 40, 4);
                facts.push(open.clone());
                let cancellation = Cancellation::new();
                let plan =
                    FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation)
                        .unwrap();
                let claims = plan.repository_claims().unwrap();
                assert_eq!(claims.claims.len(), 6);
                assert_eq!(
                    plan.qualified_root(plan.facts[&18], "Library"),
                    if pending {
                        QualifiedRoot::ImportsPending
                    } else {
                        QualifiedRoot::ProvenAbsent
                    }
                );
                assert!(claims.claims.iter().any(|claim| claim.source == open.span() && matches!(&claim.binding, FSharpBinding::Open { path, owner } if path == &vec![content_hash(b"Imports")] && *owner == plan.facts[&3].span())));
                assert!(claims.claims.iter().any(|claim| matches!(&claim.binding, FSharpBinding::Container { path, roots } if path == &vec![content_hash(b"Consumer")] && roots == &vec![content_hash(b"f")])));
                assert!(claims.claims.iter().any(|claim| matches!(&claim.binding, FSharpBinding::ImportContext { containers, opens } if containers == &vec![plan.facts[&3].span()] && opens == &vec![open.span()])));
                assert!(claims.claims.iter().any(|claim| if pending {
                    matches!(claim.binding, FSharpBinding::ImportedReference { .. })
                } else {
                    matches!(claim.binding, FSharpBinding::Reference { .. })
                }));
                assert!(claims.claims.iter().any(|claim| if pending {
                    matches!(claim.binding, FSharpBinding::ImportedCall { slots: 1, .. })
                } else {
                    matches!(claim.binding, FSharpBinding::Call { slots: 1, .. })
                }));
                for duplicate in [false, true] {
                    let mut changed = facts.clone();
                    if duplicate {
                        changed.push(fact(
                            100,
                            open.parent(),
                            open.kind(),
                            open.syntax_kind().as_str(),
                            open.span().start_byte(),
                            open.span().end_byte(),
                        ));
                    } else {
                        changed.retain(|fact| fact.local_id() != 40);
                    }
                    assert!(
                        FSharpBindings::new(
                            &changed,
                            source.as_bytes(),
                            &symbols,
                            128,
                            &cancellation
                        )
                        .unwrap()
                        .repository_claims()
                        .is_none()
                    );
                }
                let mut changed = facts.clone();
                changed.retain(|fact| fact.local_id() != 40);
                changed.push(fact(
                    40,
                    Some(11),
                    open.kind(),
                    open.syntax_kind().as_str(),
                    open.span().start_byte(),
                    open.span().end_byte(),
                ));
                assert!(
                    FSharpBindings::new(&changed, source.as_bytes(), &symbols, 128, &cancellation)
                        .unwrap()
                        .repository_claims()
                        .is_none()
                );
            }
        }
    }

    #[test]
    fn qualified_opens_apply_to_ancestors_not_siblings_and_keep_parent_shadowing() {
        for (source, inherited) in [
            (
                "module Consumer\nopen Imports\nmodule Uses =\n    let f x = Library.carry x\n",
                true,
            ),
            (
                "module Consumer\nmodule Other =\n    open Imports\nmodule Uses =\n    let f x = Library.carry x\n",
                false,
            ),
        ] {
            let (mut facts, symbols) = qualified_fixture(source, "x");
            let end = u64::try_from(source.len()).unwrap();
            let uses_start = u64::try_from(source.find("module Uses").unwrap()).unwrap();
            facts.extend(qualified_nested_facts(source, 40, "Uses", end));
            if !inherited {
                facts.extend(qualified_nested_facts(source, 50, "Other", uses_start));
            }
            let declaration = facts.iter_mut().find(|fact| fact.local_id() == 10).unwrap();
            *declaration = fact(
                10,
                Some(41),
                declaration.kind(),
                declaration.syntax_kind().as_str(),
                declaration.span().start_byte(),
                declaration.span().end_byte(),
            );
            facts.push(qualified_open_fact(
                source,
                60,
                if inherited { 4 } else { 51 },
            ));
            let cancellation = Cancellation::new();
            let plan = FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation)
                .unwrap();
            assert!(plan.repository_claims().is_some());
            assert_eq!(
                plan.qualified_root(plan.facts[&18], "Library"),
                if inherited {
                    QualifiedRoot::ImportsPending
                } else {
                    QualifiedRoot::ProvenAbsent
                }
            );
        }

        let source = "module Consumer\nopen Imports\nlet ``Library`` = 1\nmodule Uses =\n    let f x = Library.carry x\n";
        let (mut facts, mut symbols) = qualified_fixture(source, "x");
        let end = u64::try_from(source.len()).unwrap();
        facts.extend(qualified_nested_facts(source, 40, "Uses", end));
        let declaration = facts.iter_mut().find(|fact| fact.local_id() == 10).unwrap();
        *declaration = fact(
            10,
            Some(41),
            declaration.kind(),
            declaration.syntax_kind().as_str(),
            declaration.span().start_byte(),
            declaration.span().end_byte(),
        );
        facts.push(qualified_open_fact(source, 60, 4));
        let start = u64::try_from(source.find("``Library``").unwrap()).unwrap();
        let finish = start + 15;
        facts.extend([
            fact(
                70,
                Some(4),
                SyntaxFactKind::Declaration,
                "fsharp.variable.declaration",
                start,
                finish,
            ),
            fact(
                71,
                Some(70),
                SyntaxFactKind::Scope,
                "fsharp.variable.scope",
                start,
                finish,
            ),
            fact(
                72,
                Some(71),
                SyntaxFactKind::Signature,
                "fsharp.variable.signature",
                start,
                start + 12,
            ),
            fact(
                73,
                Some(71),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                start,
                start + 11,
            ),
            fact(
                74,
                Some(70),
                SyntaxFactKind::Signature,
                "fsharp.nonrecursive_binding.expression",
                start,
                finish,
            ),
        ]);
        symbols.insert(73, SymbolId::from_bytes([73; 20]));
        let plan = FSharpBindings::new(
            &facts,
            source.as_bytes(),
            &symbols,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(
            plan.qualified_root(plan.facts[&18], "Library"),
            QualifiedRoot::Bound
        );
        assert!(plan.repository_claims().is_some());
        assert!(
            !plan
                .repository_claims()
                .unwrap()
                .claims
                .iter()
                .any(|claim| matches!(
                    claim.binding,
                    FSharpBinding::Reference { .. }
                        | FSharpBinding::Call { .. }
                        | FSharpBinding::ImportedReference { .. }
                        | FSharpBinding::ImportedCall { .. }
                ))
        );
        facts.retain(|fact| fact.local_id() != 73);
        let plan = FSharpBindings::new(
            &facts,
            source.as_bytes(),
            &symbols,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(
            plan.qualified_root(plan.facts[&18], "Library"),
            QualifiedRoot::Closed
        );
        assert!(plan.repository_claims().is_none());
    }

    #[test]
    fn qualified_records_conserve_owned_names_without_exporting_fields() {
        let source = "module Consumer\ntype Receiver = { Library: int; carry: int }\nlet f x = Library.carry x\n";
        let (mut facts, symbols) = qualified_fixture(source, "x");
        facts.extend(qualified_record_facts(source, "Receiver"));
        let cancellation = Cancellation::new();
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 64, &cancellation).unwrap();
        let claims = plan.repository_claims().unwrap();
        assert_eq!(claims.claims.len(), 5);
        assert!(claims.claims.iter().any(|claim| matches!(&claim.binding, FSharpBinding::Reference { path } if path == &vec![content_hash(b"Library"), content_hash(b"carry")])));
        let lexical = plan.lexical.as_ref().unwrap();
        assert_eq!(
            plan.qualified_root(plan.facts[&18], "Library"),
            QualifiedRoot::ProvenAbsent
        );
        assert_eq!(
            lexical.qualified_root_in(11, plan.facts[&18], "Library"),
            QualifiedRoot::ProvenAbsent
        );
        for id in [30, 31, 32, 33, 34] {
            let missing = facts
                .iter()
                .filter(|fact| fact.local_id() != id)
                .cloned()
                .collect::<Vec<_>>();
            assert!(
                FSharpBindings::new(&missing, source.as_bytes(), &symbols, 64, &cancellation)
                    .unwrap()
                    .repository_claims()
                    .is_none(),
                "missing record proof {id}"
            );
            let mut duplicate = facts.clone();
            let proof = facts.iter().find(|fact| fact.local_id() == id).unwrap();
            duplicate.push(fact(
                100,
                proof.parent(),
                proof.kind(),
                proof.syntax_kind().as_str(),
                proof.span().start_byte(),
                proof.span().end_byte(),
            ));
            assert!(
                FSharpBindings::new(&duplicate, source.as_bytes(), &symbols, 64, &cancellation)
                    .unwrap()
                    .repository_claims()
                    .is_none(),
                "duplicate record proof {id}"
            );
        }
        let mut absent = facts.clone();
        absent.retain(|fact| !(30..=34).contains(&fact.local_id()));
        assert!(
            FSharpBindings::new(&absent, source.as_bytes(), &symbols, 64, &cancellation)
                .unwrap()
                .repository_claims()
                .is_none()
        );

        let source = "module Consumer\ntype Library = { carry: int }\nlet f x = Library.carry x\n";
        let (mut facts, symbols) = qualified_fixture(source, "x");
        facts.extend(qualified_record_facts(source, "Library"));
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 64, &cancellation).unwrap();
        assert!(
            plan.repository_claims()
                .unwrap()
                .claims
                .iter()
                .all(|claim| matches!(
                    &claim.binding,
                    FSharpBinding::Export { .. }
                        | FSharpBinding::Container { .. }
                        | FSharpBinding::ImportContext { .. }
                ))
        );

        let source = "module Consumer\ntype Receiver = { carry: int -> int }\nlet f ``Library`` = Library.carry 1\n";
        let (mut facts, symbols) = qualified_fixture(source, "``Library``");
        facts.extend(qualified_record_facts(source, "Receiver"));
        let plan =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 64, &cancellation).unwrap();
        assert!(plan.repository_claims().is_some());
        assert_eq!(
            plan.qualified_root(plan.facts[&18], "Library"),
            QualifiedRoot::Bound
        );
        assert!(
            plan.repository_claims()
                .unwrap()
                .claims
                .iter()
                .all(|claim| matches!(
                    &claim.binding,
                    FSharpBinding::Export { .. }
                        | FSharpBinding::Container { .. }
                        | FSharpBinding::ImportContext { .. }
                ))
        );
    }

    #[test]
    fn qualified_record_only_modules_retain_context_without_exports() {
        let source = "module Consumer\ntype Receiver = { Value: int }\n";
        let end = u64::try_from(source.len()).unwrap();
        let mut facts = vec![
            fact(1, None, SyntaxFactKind::Scope, "fsharp.file.scope", 0, end),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Signature,
                QUALIFIED_CONTEXT,
                0,
                end,
            ),
            fact(
                3,
                Some(1),
                SyntaxFactKind::Declaration,
                "fsharp.module.declaration",
                0,
                end,
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Scope,
                "fsharp.module.scope",
                0,
                end,
            ),
            fact(
                5,
                Some(4),
                SyntaxFactKind::Signature,
                "fsharp.module.signature",
                0,
                15,
            ),
            fact(6, Some(4), SyntaxFactKind::Occurrence, DEFINITION, 7, 15),
        ];
        facts.extend(qualified_record_facts(source, "Receiver"));
        let plan = FSharpBindings::new(
            &facts,
            source.as_bytes(),
            &HashMap::new(),
            64,
            &Cancellation::new(),
        )
        .unwrap();
        let claims = &plan.repository_claims().unwrap().claims;
        assert_eq!(claims.len(), 2);
        assert!(claims.iter().any(|claim| matches!(&claim.binding, FSharpBinding::Container { roots, .. } if roots == &vec![content_hash(b"Receiver")])));
        assert!(claims.iter().any(|claim| matches!(&claim.binding, FSharpBinding::ImportContext { containers, opens } if containers.len() == 1 && opens.is_empty())));
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            FSharpBindings::new(
                &facts,
                source.as_bytes(),
                &HashMap::new(),
                64,
                &cancellation
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn qualified_claims_conserve_module_exports_paths_and_application_slots() {
        let source = "module Consumer\nlet f x = Library.carry x\n";
        let (facts, symbols) = qualified_fixture(source, "x");
        let plan = FSharpBindings::new(
            &facts,
            source.as_bytes(),
            &symbols,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        let claims = plan.repository_claims().unwrap();
        assert_eq!(
            claims.scope.end_byte(),
            u64::try_from(source.len()).unwrap()
        );
        assert_eq!(claims.claims.len(), 5);
        assert!(claims.claims.iter().any(|claim| matches!(&claim.binding,
            FSharpBinding::Export { path, slots: Some(1), .. } if path == &vec![content_hash(b"Consumer"), content_hash(b"f")])));
        assert!(claims.claims.iter().any(|claim| matches!(&claim.binding,
            FSharpBinding::Reference { path } if path == &vec![content_hash(b"Library"), content_hash(b"carry")])));
        assert!(
            claims
                .claims
                .iter()
                .any(|claim| matches!(claim.binding, FSharpBinding::Call { slots: 1, .. }))
        );
        for missing in [2, 3, 4, 5, 6, 10, 11, 12, 13, 14, 15, 16, 17] {
            let mut changed = facts.clone();
            changed.retain(|fact| fact.local_id() != missing);
            let plan = FSharpBindings::new(
                &changed,
                source.as_bytes(),
                &symbols,
                128,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(plan.repository_claims().is_none(), "missing {missing}");
        }
        for duplicate in [2, 5, 6, 12, 13, 16, 17, 18] {
            let original = facts
                .iter()
                .find(|fact| fact.local_id() == duplicate)
                .unwrap();
            let mut changed = facts.clone();
            changed.push(SyntaxFact::new(
                100,
                original.parent(),
                original.kind(),
                original.span(),
                original.depth(),
                original.syntax_kind().clone(),
            ));
            let plan = FSharpBindings::new(
                &changed,
                source.as_bytes(),
                &symbols,
                128,
                &Cancellation::new(),
            )
            .unwrap();
            assert!(plan.repository_claims().is_none(), "duplicate {duplicate}");
        }
        let mut changed = facts.clone();
        changed.retain(|fact| fact.local_id() != 21);
        let plan = FSharpBindings::new(
            &changed,
            source.as_bytes(),
            &symbols,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(
            !plan
                .repository_claims()
                .unwrap()
                .claims
                .iter()
                .any(|claim| matches!(claim.binding, FSharpBinding::Call { .. }))
        );
    }

    #[test]
    fn qualified_roots_keep_formal_shadowing_and_unavailable_scopes_closed() {
        let source = "module Consumer\nlet f ``Library`` = Library.carry 1\n";
        let (facts, symbols) = qualified_fixture(source, "``Library``");
        let plan = FSharpBindings::new(
            &facts,
            source.as_bytes(),
            &symbols,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        let read = facts.iter().find(|fact| fact.local_id() == 18).unwrap();
        assert_eq!(plan.qualified_root(read, "Library"), QualifiedRoot::Bound);
        assert!(
            !plan
                .repository_claims()
                .unwrap()
                .claims
                .iter()
                .any(|claim| matches!(
                    claim.binding,
                    FSharpBinding::Reference { .. } | FSharpBinding::Call { .. }
                ))
        );
        let source = "module Consumer\nlet f x = Library.carry x\n";
        let (mut facts, symbols) = qualified_fixture(source, "x");
        let read = facts.iter_mut().find(|fact| fact.local_id() == 18).unwrap();
        *read = SyntaxFact::new(
            read.local_id(),
            Some(30),
            read.kind(),
            read.span(),
            read.depth(),
            read.syntax_kind().clone(),
        );
        let start = u64::try_from(source.find("Library.carry").unwrap()).unwrap();
        let end = u64::try_from(source.trim_end().len()).unwrap();
        facts.push(fact(
            30,
            Some(11),
            SyntaxFactKind::Scope,
            "fsharp.local_binding.scope",
            start,
            end,
        ));
        let plan = FSharpBindings::new(
            &facts,
            source.as_bytes(),
            &symbols,
            128,
            &Cancellation::new(),
        )
        .unwrap();
        let read = facts.iter().find(|fact| fact.local_id() == 18).unwrap();
        assert_eq!(plan.qualified_root(read, "Library"), QualifiedRoot::Closed);
        assert!(
            !plan
                .repository_claims()
                .unwrap()
                .claims
                .iter()
                .any(|claim| matches!(claim.binding, FSharpBinding::Reference { .. }))
        );
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn qualified_components_unquote_whole_identifiers_without_splitting_their_dots() {
        let parts = written_components("``Library.Part`` . ``carry.part``").unwrap();
        assert_eq!(
            component_hashes(&parts).unwrap(),
            vec![content_hash(b"Library.Part"), content_hash(b"carry.part")]
        );
        for source in [
            "Library.",
            ".carry",
            "Library..carry",
            "``Library.carry",
            "Library.````",
        ] {
            assert!(written_components(source).is_none(), "{source}");
        }
        assert!(
            written_components(
                &vec!["Part"; rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS + 1].join(".")
            )
            .is_none()
        );
    }

    fn facts() -> Vec<SyntaxFact> {
        function_facts(1, 0)
            .into_iter()
            .chain(function_facts(11, 8))
            .collect()
    }

    fn symbols() -> HashMap<u64, SymbolId> {
        HashMap::from([
            (6, SymbolId::from_bytes([1; 20])),
            (16, SymbolId::from_bytes([2; 20])),
        ])
    }

    fn read(facts: &[SyntaxFact], id: u64) -> &SyntaxFact {
        facts.iter().find(|fact| fact.local_id() == id).unwrap()
    }

    fn parameter_signature_facts() -> Vec<SyntaxFact> {
        vec![
            fact(1, None, SyntaxFactKind::Declaration, FUNCTION, 0, 19),
            fact(2, Some(1), SyntaxFactKind::Scope, SCOPE, 0, 19),
            fact(3, Some(2), SyntaxFactKind::Signature, HEADER, 0, 15),
            fact(4, Some(2), SyntaxFactKind::Signature, PARAMETERS, 2, 14),
            fact(5, Some(2), SyntaxFactKind::Declaration, PARAMETER, 3, 5),
            fact(6, Some(5), SyntaxFactKind::Occurrence, DEFINITION, 3, 5),
            fact(7, Some(2), SyntaxFactKind::Declaration, PARAMETER, 12, 14),
            fact(8, Some(7), SyntaxFactKind::Occurrence, DEFINITION, 12, 14),
        ]
    }

    #[test]
    fn parameter_signatures_keep_exact_typed_and_bare_headers_without_widening_leaves() {
        let source = b"f (aa: int) bb = aa\n";
        for parent in [2, 5] {
            let mut facts = parameter_signature_facts();
            facts.extend([
                fact(
                    30,
                    Some(parent),
                    SyntaxFactKind::Signature,
                    PARAMETER_SIGNATURE,
                    2,
                    11,
                ),
                fact(
                    31,
                    Some(2),
                    SyntaxFactKind::Signature,
                    PARAMETER_SIGNATURE,
                    12,
                    14,
                ),
            ]);
            for _ in 0..2 {
                let plan = FSharpParameterSignatures::new(&facts, source, 64, &Cancellation::new())
                    .unwrap();
                let typed = plan.signature_for_declaration(5).unwrap();
                let bare = plan.signature_for_declaration(7).unwrap();
                assert_eq!(typed.local_id(), 30);
                assert_eq!(bare.local_id(), 31);
                assert_eq!(source_bytes(typed, source).unwrap(), b"(aa: int)");
                assert_eq!(source_bytes(bare, source).unwrap(), b"bb");
                assert_eq!(read(&facts, 5).span().start_byte(), 3);
                assert_eq!(read(&facts, 5).span().end_byte(), 5);
                assert_eq!(read(&facts, 6).span(), read(&facts, 5).span());
                assert_eq!(plan.signature_for_declaration(1), None);
                facts.reverse();
            }
        }
    }

    #[test]
    fn parameter_signatures_keep_sibling_ownership_with_identical_written_names() {
        let mut facts = facts();
        facts.extend([
            fact(
                30,
                Some(2),
                SyntaxFactKind::Signature,
                PARAMETER_SIGNATURE,
                2,
                3,
            ),
            fact(
                31,
                Some(12),
                SyntaxFactKind::Signature,
                PARAMETER_SIGNATURE,
                10,
                11,
            ),
        ]);
        for _ in 0..2 {
            let plan =
                FSharpParameterSignatures::new(&facts, SOURCE, 64, &Cancellation::new()).unwrap();
            assert_eq!(plan.signature_for_declaration(5), Some(read(&facts, 30)));
            assert_eq!(plan.signature_for_declaration(15), Some(read(&facts, 31)));
            facts.reverse();
        }
    }

    #[test]
    fn parameter_signatures_reject_broad_and_partial_leaf_intersections() {
        let source = b"f aa bb = aa\n";
        for (start, end) in [(2, 7), (3, 7), (2, 6)] {
            let facts = [
                fact(1, None, SyntaxFactKind::Declaration, FUNCTION, 0, 12),
                fact(2, Some(1), SyntaxFactKind::Scope, SCOPE, 0, 12),
                fact(3, Some(2), SyntaxFactKind::Signature, HEADER, 0, 8),
                fact(4, Some(2), SyntaxFactKind::Signature, PARAMETERS, 2, 7),
                fact(5, Some(2), SyntaxFactKind::Declaration, PARAMETER, 2, 4),
                fact(6, Some(5), SyntaxFactKind::Occurrence, DEFINITION, 2, 4),
                fact(7, Some(2), SyntaxFactKind::Declaration, PARAMETER, 5, 7),
                fact(8, Some(7), SyntaxFactKind::Occurrence, DEFINITION, 5, 7),
                fact(
                    30,
                    Some(2),
                    SyntaxFactKind::Signature,
                    PARAMETER_SIGNATURE,
                    start,
                    end,
                ),
            ];
            let plan =
                FSharpParameterSignatures::new(&facts, source, 64, &Cancellation::new()).unwrap();
            assert_eq!(plan.signature_for_declaration(5), None, "{start}..{end}");
            assert_eq!(plan.signature_for_declaration(7), None, "{start}..{end}");
        }
    }

    #[test]
    fn parameter_signatures_require_unique_exact_leaf_definitions() {
        let source = b"f (aa: int) bb = aa\n";
        for case in 0..8 {
            let mut facts = parameter_signature_facts();
            facts.push(fact(
                30,
                Some(2),
                SyntaxFactKind::Signature,
                PARAMETER_SIGNATURE,
                2,
                11,
            ));
            match case {
                0 => facts.retain(|fact| fact.local_id() != 6),
                1 => facts.push(fact(
                    29,
                    Some(5),
                    SyntaxFactKind::Occurrence,
                    DEFINITION,
                    3,
                    5,
                )),
                2 => {
                    facts.retain(|fact| fact.local_id() != 6);
                    facts.push(fact(
                        6,
                        Some(5),
                        SyntaxFactKind::Occurrence,
                        DEFINITION,
                        12,
                        14,
                    ));
                }
                3 => {
                    facts.retain(|fact| fact.local_id() != 6);
                    facts.push(fact(
                        6,
                        Some(5),
                        SyntaxFactKind::Declaration,
                        DEFINITION,
                        3,
                        5,
                    ));
                }
                4 => {
                    facts.retain(|fact| fact.local_id() != 6);
                    facts.push(fact(
                        6,
                        Some(7),
                        SyntaxFactKind::Occurrence,
                        DEFINITION,
                        3,
                        5,
                    ));
                }
                5 => {
                    facts.retain(|fact| fact.local_id() != 6);
                    facts.push(fact(
                        6,
                        Some(5),
                        SyntaxFactKind::Occurrence,
                        "other.definition",
                        3,
                        5,
                    ));
                }
                6 => facts.extend([
                    fact(29, Some(2), SyntaxFactKind::Declaration, PARAMETER, 3, 5),
                    fact(32, Some(29), SyntaxFactKind::Occurrence, DEFINITION, 3, 5),
                ]),
                7 => facts.push(fact(
                    29,
                    Some(2),
                    SyntaxFactKind::Signature,
                    PARAMETER,
                    3,
                    5,
                )),
                _ => unreachable!(),
            }
            let plan =
                FSharpParameterSignatures::new(&facts, source, 64, &Cancellation::new()).unwrap();
            assert_eq!(plan.signature_for_declaration(5), None, "case {case}");
        }
    }

    #[test]
    fn parameter_signatures_require_unique_complete_owner_proofs() {
        let source = b"f (aa: int) bb = aa\n";
        for case in 0..8 {
            let mut facts = parameter_signature_facts();
            let (parent, start, end) = match case {
                0 => (7, 2, 11),
                1 => (99, 2, 11),
                2 => (2, 2, 18),
                3 => (2, 2, 2),
                _ => (2, 2, 11),
            };
            facts.push(fact(
                30,
                Some(parent),
                SyntaxFactKind::Signature,
                PARAMETER_SIGNATURE,
                start,
                end,
            ));
            match case {
                4 => facts.retain(|fact| fact.local_id() != 4),
                5 => facts.push(fact(
                    29,
                    Some(2),
                    SyntaxFactKind::Signature,
                    PARAMETERS,
                    2,
                    14,
                )),
                6 => facts.push(fact(29, Some(2), SyntaxFactKind::Signature, HEADER, 0, 15)),
                7 => facts.push(fact(
                    29,
                    Some(2),
                    SyntaxFactKind::Signature,
                    PARAMETER_SIGNATURE,
                    2,
                    11,
                )),
                _ => {}
            }
            let plan =
                FSharpParameterSignatures::new(&facts, source, 64, &Cancellation::new()).unwrap();
            assert_eq!(plan.signature_for_declaration(5), None, "case {case}");
        }
    }

    #[test]
    fn parameter_signatures_do_not_hide_a_second_leaf_missing_its_definition() {
        let source = b"f aa bb = aa\n";
        let facts = [
            fact(1, None, SyntaxFactKind::Declaration, FUNCTION, 0, 12),
            fact(2, Some(1), SyntaxFactKind::Scope, SCOPE, 0, 12),
            fact(3, Some(2), SyntaxFactKind::Signature, HEADER, 0, 8),
            fact(4, Some(2), SyntaxFactKind::Signature, PARAMETERS, 2, 7),
            fact(5, Some(2), SyntaxFactKind::Declaration, PARAMETER, 2, 4),
            fact(6, Some(5), SyntaxFactKind::Occurrence, DEFINITION, 2, 4),
            fact(7, Some(2), SyntaxFactKind::Declaration, PARAMETER, 5, 7),
            fact(
                30,
                Some(2),
                SyntaxFactKind::Signature,
                PARAMETER_SIGNATURE,
                2,
                7,
            ),
        ];
        let plan =
            FSharpParameterSignatures::new(&facts, source, 64, &Cancellation::new()).unwrap();
        assert_eq!(plan.signature_for_declaration(5), None);
        assert_eq!(plan.signature_for_declaration(7), None);
    }

    #[test]
    fn parameter_signatures_without_witnesses_skip_the_optional_ownership_graph() {
        let facts = [
            function_facts(1, 0)[0].clone(),
            function_facts(1, 0)[0].clone(),
        ];
        let plan =
            FSharpParameterSignatures::new(&facts, SOURCE, 64, &Cancellation::new()).unwrap();
        assert!(plan.signatures.is_empty());
        let cancellation = Cancellation::new();
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            FSharpParameterSignatures::new(&facts, SOURCE, 64, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn parameter_signatures_reject_invalid_coordinates_duplicate_ids_and_cycles() {
        let source = b"f (aa: int) bb = aa\n";
        for case in 0..3 {
            let mut facts = parameter_signature_facts();
            facts.push(fact(
                30,
                Some(2),
                SyntaxFactKind::Signature,
                PARAMETER_SIGNATURE,
                2,
                11,
            ));
            match case {
                0 => facts.push(fact(
                    31,
                    Some(2),
                    SyntaxFactKind::Signature,
                    PARAMETER_SIGNATURE,
                    90,
                    100,
                )),
                1 => facts.push(facts[0].clone()),
                2 => {
                    facts.retain(|fact| fact.local_id() != 2);
                    facts.push(fact(2, Some(2), SyntaxFactKind::Scope, SCOPE, 0, 19));
                }
                _ => unreachable!(),
            }
            assert!(
                FSharpParameterSignatures::new(&facts, source, 64, &Cancellation::new()).is_err(),
                "case {case}"
            );
        }
    }

    #[test]
    fn unit_formals_require_unique_complete_native_slot_conservation() {
        let source = "f value () = value\n";
        let original = vec![
            fact(1, None, SyntaxFactKind::Declaration, FUNCTION, 0, 18),
            fact(2, Some(1), SyntaxFactKind::Scope, SCOPE, 0, 18),
            fact(3, Some(2), SyntaxFactKind::Signature, HEADER, 0, 11),
            fact(
                4,
                Some(2),
                SyntaxFactKind::Signature,
                UNIT_PARAMETERS,
                2,
                10,
            ),
            fact(5, Some(2), SyntaxFactKind::Declaration, PARAMETER, 2, 7),
            fact(6, Some(5), SyntaxFactKind::Occurrence, DEFINITION, 2, 7),
            fact(7, Some(2), SyntaxFactKind::Occurrence, READ, 13, 18),
            fact(
                8,
                Some(5),
                SyntaxFactKind::Signature,
                PARAMETER_SIGNATURE,
                2,
                7,
            ),
            fact(9, Some(2), SyntaxFactKind::Signature, UNIT_PARAMETER, 8, 10),
            fact(10, Some(2), SyntaxFactKind::Occurrence, DEFINITION, 0, 1),
        ];
        let symbols = HashMap::from([
            (6, SymbolId::from_bytes([1; 20])),
            (10, SymbolId::from_bytes([2; 20])),
        ]);
        let cancellation = Cancellation::new();
        let plan = FSharpBindings::new(&original, source.as_bytes(), &symbols, 128, &cancellation)
            .unwrap();
        assert!(plan.functions[&1].valid());
        assert_eq!(plan.functions[&1].callable_arity(Some(Some(1))), Some(2));
        assert_eq!(plan.functions[&1].callable_arity(Some(None)), None);
        assert_eq!(plan.functions[&1].callable_arity(Some(Some(0))), None);
        assert_eq!(
            plan.resolve(read(&original, 7), "value", &cancellation)
                .unwrap(),
            Some(symbols[&6])
        );
        let signatures =
            FSharpParameterSignatures::new(&original, source.as_bytes(), 128, &cancellation)
                .unwrap();
        assert_eq!(
            signatures.signature_for_declaration(5),
            Some(read(&original, 8))
        );
        for corruption in 0..13 {
            let mut facts = original.clone();
            match corruption {
                0 => facts.retain(|fact| fact.local_id() != 4),
                1 => facts.retain(|fact| fact.local_id() != 9),
                2 => facts.retain(|fact| fact.local_id() != 8),
                3 => facts.push(fact(
                    14,
                    Some(2),
                    SyntaxFactKind::Signature,
                    UNIT_PARAMETERS,
                    2,
                    10,
                )),
                4 => facts.push(fact(
                    19,
                    Some(2),
                    SyntaxFactKind::Signature,
                    UNIT_PARAMETER,
                    8,
                    10,
                )),
                5 => {
                    facts.retain(|fact| fact.local_id() != 9);
                    facts.push(fact(
                        9,
                        Some(2),
                        SyntaxFactKind::Signature,
                        UNIT_PARAMETER,
                        8,
                        9,
                    ));
                }
                6 => {
                    facts.retain(|fact| fact.local_id() != 9);
                    facts.push(fact(
                        9,
                        Some(5),
                        SyntaxFactKind::Signature,
                        UNIT_PARAMETER,
                        8,
                        10,
                    ));
                }
                7 => facts.push(fact(
                    14,
                    Some(2),
                    SyntaxFactKind::Signature,
                    PARAMETERS,
                    2,
                    10,
                )),
                8 => {
                    facts.retain(|fact| fact.local_id() != 9);
                    facts.push(fact(
                        9,
                        Some(2),
                        SyntaxFactKind::Signature,
                        UNIT_PARAMETER,
                        7,
                        10,
                    ));
                }
                9 => {
                    facts.retain(|fact| fact.local_id() != 9);
                    facts.push(fact(
                        9,
                        Some(2),
                        SyntaxFactKind::Signature,
                        UNIT_PARAMETER,
                        2,
                        7,
                    ));
                }
                10 => facts.push(fact(
                    19,
                    Some(2),
                    SyntaxFactKind::Signature,
                    UNIT_PARAMETER,
                    13,
                    15,
                )),
                11 => {
                    facts.retain(|fact| fact.local_id() != 8);
                    facts.push(fact(
                        8,
                        Some(1),
                        SyntaxFactKind::Signature,
                        PARAMETER_SIGNATURE,
                        2,
                        7,
                    ));
                }
                12 => facts.push(fact(
                    18,
                    Some(5),
                    SyntaxFactKind::Signature,
                    PARAMETER_SIGNATURE,
                    2,
                    7,
                )),
                _ => unreachable!(),
            }
            let core = |facts: &[SyntaxFact]| {
                facts
                    .iter()
                    .filter(|fact| matches!(fact.local_id(), 1 | 2 | 3 | 5 | 6 | 10))
                    .cloned()
                    .collect::<Vec<_>>()
            };
            assert_eq!(core(&facts), core(&original), "corruption {corruption}");
            let plan = FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation)
                .unwrap();
            assert!(!plan.functions[&1].valid(), "corruption {corruption}");
            assert_eq!(
                plan.resolve(read(&facts, 7), "value", &cancellation)
                    .unwrap(),
                None,
                "corruption {corruption}"
            );
        }
    }

    #[test]
    fn unit_formals_preserve_binder_free_tokens_and_typed_cancellation() {
        let cancellation = Cancellation::new();
        for bytes in [b"()".as_slice(), b"( )", b"(\t \t)"] {
            assert!(unit_marker_bytes(bytes, &cancellation).unwrap());
        }
        for bytes in [b"(\n)".as_slice(), b"(value)", b"((), ())", b"() ()"] {
            assert!(!unit_marker_bytes(bytes, &cancellation).unwrap());
        }
        let source = format!("({})", " \t".repeat(8192));
        assert!(unit_marker_bytes(source.as_bytes(), &cancellation).unwrap());
        assert!(cancellation.cancel(CancellationReason::ClientRequest));
        assert!(matches!(
            unit_marker_bytes(source.as_bytes(), &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            whitespace_between(
                source.as_bytes(),
                1,
                u64::try_from(source.len() - 1).unwrap(),
                &cancellation
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn tuple_formals_conserve_each_leaf_and_one_slot_under_capture_loss() {
        let source = b"f (left, right) () = left\n";
        let original = vec![
            fact(1, None, SyntaxFactKind::Declaration, FUNCTION, 0, 25),
            fact(2, Some(1), SyntaxFactKind::Scope, SCOPE, 0, 25),
            fact(3, Some(2), SyntaxFactKind::Signature, HEADER, 0, 19),
            fact(
                4,
                Some(2),
                SyntaxFactKind::Signature,
                COMPOUND_PARAMETERS,
                2,
                18,
            ),
            fact(5, Some(2), SyntaxFactKind::Declaration, PARAMETER, 3, 7),
            fact(6, Some(5), SyntaxFactKind::Occurrence, DEFINITION, 3, 7),
            fact(7, Some(2), SyntaxFactKind::Declaration, PARAMETER, 9, 14),
            fact(8, Some(7), SyntaxFactKind::Occurrence, DEFINITION, 9, 14),
            fact(
                9,
                Some(5),
                SyntaxFactKind::Signature,
                PARAMETER_SIGNATURE,
                3,
                7,
            ),
            fact(
                10,
                Some(7),
                SyntaxFactKind::Signature,
                PARAMETER_SIGNATURE,
                9,
                14,
            ),
            fact(
                11,
                Some(2),
                SyntaxFactKind::Signature,
                TUPLE_PARAMETER,
                2,
                15,
            ),
            fact(
                12,
                Some(2),
                SyntaxFactKind::Signature,
                UNIT_PARAMETER,
                16,
                18,
            ),
            fact(13, Some(2), SyntaxFactKind::Occurrence, DEFINITION, 0, 1),
            fact(14, Some(2), SyntaxFactKind::Occurrence, READ, 21, 25),
        ];
        let symbols = HashMap::from([
            (6, SymbolId::from_bytes([1; 20])),
            (8, SymbolId::from_bytes([2; 20])),
            (13, SymbolId::from_bytes([3; 20])),
        ]);
        let cancellation = Cancellation::new();
        for facts in [original.clone(), original.iter().rev().cloned().collect()] {
            let plan = FSharpBindings::new(&facts, source, &symbols, 128, &cancellation).unwrap();
            assert!(plan.functions[&1].valid());
            assert_eq!(plan.functions[&1].callable_arity(Some(Some(2))), Some(2));
            assert_eq!(plan.functions[&1].callable_arity(Some(Some(1))), None);
            assert_eq!(
                plan.resolve(read(&facts, 14), "left", &cancellation)
                    .unwrap(),
                Some(symbols[&6])
            );
            let signatures =
                FSharpParameterSignatures::new(&facts, source, 128, &cancellation).unwrap();
            assert_eq!(
                signatures.signature_for_declaration(5),
                Some(read(&facts, 9))
            );
            assert_eq!(
                signatures.signature_for_declaration(7),
                Some(read(&facts, 10))
            );
        }
        for corruption in 0..13 {
            let mut facts = original.clone();
            match corruption {
                0 => facts.retain(|fact| fact.local_id() != 4),
                1 => facts.retain(|fact| fact.local_id() != 11),
                2 => facts.retain(|fact| fact.local_id() != 10),
                3 => facts.retain(|fact| !matches!(fact.local_id(), 7 | 8 | 10)),
                4 => facts.push(fact(
                    21,
                    Some(2),
                    SyntaxFactKind::Signature,
                    TUPLE_PARAMETER,
                    2,
                    15,
                )),
                5 => facts.push(fact(
                    21,
                    Some(7),
                    SyntaxFactKind::Signature,
                    PARAMETER_SIGNATURE,
                    9,
                    14,
                )),
                6 => facts.push(fact(
                    21,
                    Some(2),
                    SyntaxFactKind::Signature,
                    COMPOUND_PARAMETERS,
                    2,
                    18,
                )),
                7 => facts.push(fact(
                    21,
                    Some(2),
                    SyntaxFactKind::Signature,
                    PARAMETERS,
                    2,
                    18,
                )),
                8 => {
                    facts.retain(|fact| fact.local_id() != 11);
                    facts.push(fact(
                        11,
                        Some(5),
                        SyntaxFactKind::Signature,
                        TUPLE_PARAMETER,
                        2,
                        15,
                    ));
                }
                9 => {
                    facts.retain(|fact| fact.local_id() != 11);
                    facts.push(fact(
                        11,
                        Some(2),
                        SyntaxFactKind::Signature,
                        TUPLE_PARAMETER,
                        3,
                        14,
                    ));
                }
                10 => facts.retain(|fact| fact.local_id() != 12),
                11 => {
                    facts.retain(|fact| fact.local_id() != 9);
                    facts.push(fact(
                        9,
                        Some(5),
                        SyntaxFactKind::Signature,
                        PARAMETER_SIGNATURE,
                        2,
                        15,
                    ));
                }
                12 => facts.push(fact(
                    21,
                    Some(7),
                    SyntaxFactKind::Occurrence,
                    DEFINITION,
                    9,
                    14,
                )),
                _ => unreachable!(),
            }
            let plan = FSharpBindings::new(&facts, source, &symbols, 128, &cancellation).unwrap();
            assert!(!plan.functions[&1].valid(), "corruption {corruption}");
            assert_eq!(
                plan.resolve(read(&facts, 14), "left", &cancellation)
                    .unwrap(),
                None,
                "corruption {corruption}"
            );
            let signatures =
                FSharpParameterSignatures::new(&facts, source, 128, &cancellation).unwrap();
            assert!(
                signatures.signature_for_declaration(5).is_none(),
                "corruption {corruption}"
            );
        }
    }

    #[test]
    fn exact_formals_resolve_only_in_their_own_function_independently_of_fact_order() {
        let mut facts = facts();
        let symbols = symbols();
        for _ in 0..2 {
            let plan =
                FSharpBindings::new(&facts, SOURCE, &symbols, 64, &Cancellation::new()).unwrap();
            for (reference, definition) in [(7, 6), (17, 16)] {
                assert_eq!(
                    plan.resolve(read(&facts, reference), "x", &Cancellation::new())
                        .unwrap(),
                    symbols.get(&definition).copied()
                );
                assert_eq!(
                    plan.resolve(read(&facts, reference), "other", &Cancellation::new())
                        .unwrap(),
                    None
                );
            }
            facts.reverse();
        }
    }

    #[test]
    fn header_and_complete_marker_can_be_owned_by_the_own_declaration_or_scope() {
        let mut facts = facts();
        facts.retain(|fact| !matches!(fact.local_id(), 3 | 4));
        facts.extend([
            fact(3, Some(1), SyntaxFactKind::Signature, HEADER, 0, 4),
            fact(4, Some(1), SyntaxFactKind::Signature, PARAMETERS, 2, 3),
        ]);
        let symbols = symbols();
        let plan = FSharpBindings::new(&facts, SOURCE, &symbols, 64, &Cancellation::new()).unwrap();
        assert_eq!(
            plan.resolve(read(&facts, 7), "x", &Cancellation::new())
                .unwrap(),
            Some(symbols[&6])
        );
    }

    #[test]
    fn missing_duplicate_and_conflicting_formals_close_targets_without_outer_lookup() {
        for case in 0..9 {
            let mut facts = facts();
            let mut symbols = symbols();
            match case {
                0 => {
                    symbols.remove(&6);
                }
                1 => facts.retain(|fact| fact.local_id() != 6),
                2 => facts.extend([
                    fact(30, Some(2), SyntaxFactKind::Declaration, PARAMETER, 2, 3),
                    fact(31, Some(30), SyntaxFactKind::Occurrence, DEFINITION, 2, 3),
                ]),
                3 => facts.push(fact(
                    30,
                    Some(5),
                    SyntaxFactKind::Occurrence,
                    DEFINITION,
                    2,
                    3,
                )),
                4 => {
                    symbols.insert(16, symbols[&6]);
                }
                5 => facts.retain(|fact| fact.local_id() != 4),
                6 => facts.push(fact(
                    30,
                    Some(1),
                    SyntaxFactKind::Signature,
                    PARAMETERS,
                    2,
                    3,
                )),
                7 => facts.retain(|fact| fact.local_id() != 3),
                8 => {
                    let symbol = symbols.remove(&6).unwrap();
                    symbols.insert(5, symbol);
                }
                _ => unreachable!(),
            }
            let plan =
                FSharpBindings::new(&facts, SOURCE, &symbols, 64, &Cancellation::new()).unwrap();
            assert_eq!(
                plan.resolve(read(&facts, 7), "x", &Cancellation::new())
                    .unwrap(),
                None,
                "case {case}"
            );
            assert_eq!(
                plan.resolve(read(&facts, 17), "x", &Cancellation::new())
                    .unwrap(),
                (case != 4).then_some(symbols[&16]),
                "case {case}"
            );
        }
    }

    #[test]
    fn invalid_scopes_headers_and_parameter_markers_disable_the_own_function() {
        for case in 0..8 {
            let mut facts = facts();
            match case {
                0 => facts.push(fact(30, Some(1), SyntaxFactKind::Scope, SCOPE, 0, 7)),
                1 => {
                    facts.retain(|fact| fact.local_id() != 2);
                    facts.push(fact(2, Some(1), SyntaxFactKind::Scope, SCOPE, 0, 6));
                }
                2 => {
                    facts.retain(|fact| fact.local_id() != 2);
                    facts.push(fact(2, Some(99), SyntaxFactKind::Scope, SCOPE, 0, 7));
                }
                3 => {
                    facts.retain(|fact| fact.local_id() != 3);
                    facts.push(fact(3, Some(2), SyntaxFactKind::Signature, HEADER, 1, 4));
                }
                4 => {
                    facts.retain(|fact| fact.local_id() != 4);
                    facts.push(fact(
                        4,
                        Some(2),
                        SyntaxFactKind::Signature,
                        PARAMETERS,
                        6,
                        7,
                    ));
                }
                5 => {
                    facts.retain(|fact| fact.local_id() != 4);
                    facts.push(fact(
                        4,
                        Some(12),
                        SyntaxFactKind::Signature,
                        PARAMETERS,
                        2,
                        3,
                    ));
                }
                6 => {
                    facts.retain(|fact| fact.local_id() != 5);
                    facts.push(fact(
                        5,
                        Some(2),
                        SyntaxFactKind::Declaration,
                        PARAMETER,
                        6,
                        7,
                    ));
                }
                7 => facts.push(fact(30, Some(2), SyntaxFactKind::Signature, HEADER, 0, 4)),
                _ => unreachable!(),
            }
            let plan =
                FSharpBindings::new(&facts, SOURCE, &symbols(), 64, &Cancellation::new()).unwrap();
            assert_eq!(
                plan.resolve(read(&facts, 7), "x", &Cancellation::new())
                    .unwrap(),
                None,
                "case {case}"
            );
        }
    }

    #[test]
    fn exact_definition_spans_files_and_roles_are_required() {
        for case in 0..3 {
            let mut facts = facts();
            facts.retain(|fact| fact.local_id() != 6);
            let definition = match case {
                0 => fact(6, Some(5), SyntaxFactKind::Occurrence, DEFINITION, 2, 4),
                1 => SyntaxFact::new(
                    6,
                    Some(5),
                    SyntaxFactKind::Occurrence,
                    SourceSpan::new(FileId::from_bytes([8; 20]), 2, 3).unwrap(),
                    0,
                    SyntaxKindLabel::new(DEFINITION).unwrap(),
                ),
                2 => fact(
                    6,
                    Some(5),
                    SyntaxFactKind::Occurrence,
                    "fsharp.other.definition",
                    2,
                    3,
                ),
                _ => unreachable!(),
            };
            facts.push(definition);
            let plan =
                FSharpBindings::new(&facts, SOURCE, &symbols(), 64, &Cancellation::new()).unwrap();
            assert_eq!(
                plan.resolve(read(&facts, 7), "x", &Cancellation::new())
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn sibling_parents_header_reads_missing_parents_and_non_native_labels_stay_unresolved() {
        for case in 0..5 {
            let mut facts = facts();
            facts.retain(|fact| fact.local_id() != 7);
            let reference = match case {
                0 => fact(7, Some(12), SyntaxFactKind::Occurrence, READ, 6, 7),
                1 => fact(7, Some(2), SyntaxFactKind::Occurrence, READ, 2, 3),
                2 => fact(7, Some(99), SyntaxFactKind::Occurrence, READ, 6, 7),
                3 => fact(
                    7,
                    Some(2),
                    SyntaxFactKind::Occurrence,
                    "fsharp.name.reference",
                    6,
                    7,
                ),
                4 => fact(
                    7,
                    Some(2),
                    SyntaxFactKind::Occurrence,
                    "fsharp.application.call",
                    6,
                    7,
                ),
                _ => unreachable!(),
            };
            facts.push(reference);
            let plan =
                FSharpBindings::new(&facts, SOURCE, &symbols(), 64, &Cancellation::new()).unwrap();
            assert_eq!(
                plan.resolve(read(&facts, 7), "x", &Cancellation::new())
                    .unwrap(),
                None
            );
        }
    }

    #[test]
    fn intervening_binders_and_unreviewed_capture_parents_never_fall_outward() {
        for (kind, label) in [
            (SyntaxFactKind::Scope, "fsharp.lambda.scope"),
            (SyntaxFactKind::Scope, "fsharp.rule.scope"),
            (SyntaxFactKind::Scope, "fsharp.for.scope"),
            (SyntaxFactKind::Scope, "fsharp.member.scope"),
            (SyntaxFactKind::Scope, "fsharp.computation.scope"),
            (SyntaxFactKind::Declaration, "fsharp.variable.declaration"),
            (SyntaxFactKind::Occurrence, "fsharp.unreviewed.reference"),
        ] {
            let mut facts = facts();
            facts.retain(|fact| fact.local_id() != 7);
            facts.extend([
                fact(30, Some(2), kind, label, 6, 7),
                fact(7, Some(30), SyntaxFactKind::Occurrence, READ, 6, 7),
            ]);
            let plan =
                FSharpBindings::new(&facts, SOURCE, &symbols(), 64, &Cancellation::new()).unwrap();
            assert_eq!(
                plan.resolve(read(&facts, 7), "x", &Cancellation::new())
                    .unwrap(),
                None,
                "{label}"
            );
        }
    }

    #[test]
    fn quoted_names_remain_exact_and_all_written_bytes_count_toward_the_limit() {
        const QUOTED: &[u8] = b"f ``x`` = ``x``\n";
        let facts = [
            fact(1, None, SyntaxFactKind::Declaration, FUNCTION, 0, 15),
            fact(2, Some(1), SyntaxFactKind::Scope, SCOPE, 0, 15),
            fact(3, Some(2), SyntaxFactKind::Signature, HEADER, 0, 8),
            fact(4, Some(2), SyntaxFactKind::Signature, PARAMETERS, 2, 7),
            fact(5, Some(2), SyntaxFactKind::Declaration, PARAMETER, 2, 7),
            fact(6, Some(5), SyntaxFactKind::Occurrence, DEFINITION, 2, 7),
            fact(7, Some(2), SyntaxFactKind::Occurrence, READ, 10, 15),
        ];
        let symbols = symbols();
        for limit in [0, 4, 5] {
            let plan =
                FSharpBindings::new(&facts, QUOTED, &symbols, limit, &Cancellation::new()).unwrap();
            assert_eq!(
                plan.resolve(&facts[6], "``x``", &Cancellation::new())
                    .unwrap(),
                (limit == 5).then_some(symbols[&6])
            );
            assert_eq!(
                plan.resolve(&facts[6], "x", &Cancellation::new()).unwrap(),
                None
            );
        }
    }

    #[test]
    fn each_simple_formal_has_its_own_exact_definition_and_materialization() {
        const TWO_FORMALS: &[u8] = b"f x y = x + y\n";
        let facts = [
            fact(1, None, SyntaxFactKind::Declaration, FUNCTION, 0, 13),
            fact(2, Some(1), SyntaxFactKind::Scope, SCOPE, 0, 13),
            fact(3, Some(2), SyntaxFactKind::Signature, HEADER, 0, 6),
            fact(4, Some(2), SyntaxFactKind::Signature, PARAMETERS, 2, 5),
            fact(5, Some(2), SyntaxFactKind::Declaration, PARAMETER, 2, 3),
            fact(6, Some(5), SyntaxFactKind::Occurrence, DEFINITION, 2, 3),
            fact(7, Some(2), SyntaxFactKind::Occurrence, READ, 8, 9),
            fact(8, Some(2), SyntaxFactKind::Declaration, PARAMETER, 4, 5),
            fact(9, Some(8), SyntaxFactKind::Occurrence, DEFINITION, 4, 5),
            fact(10, Some(2), SyntaxFactKind::Occurrence, READ, 12, 13),
        ];
        let x = SymbolId::from_bytes([1; 20]);
        let y = SymbolId::from_bytes([2; 20]);
        for include_y in [false, true] {
            let mut symbols = HashMap::from([(6, x)]);
            if include_y {
                symbols.insert(9, y);
            }
            let plan = FSharpBindings::new(&facts, TWO_FORMALS, &symbols, 64, &Cancellation::new())
                .unwrap();
            assert_eq!(
                plan.resolve(&facts[6], "x", &Cancellation::new()).unwrap(),
                Some(x)
            );
            assert_eq!(
                plan.resolve(&facts[9], "y", &Cancellation::new()).unwrap(),
                include_y.then_some(y)
            );
        }
    }

    #[test]
    fn qualified_names_cannot_forge_the_simple_native_parameter_contract() {
        const QUALIFIED: &[u8] = b"f a.b = a.b\n";
        let facts = [
            fact(1, None, SyntaxFactKind::Declaration, FUNCTION, 0, 11),
            fact(2, Some(1), SyntaxFactKind::Scope, SCOPE, 0, 11),
            fact(3, Some(2), SyntaxFactKind::Signature, HEADER, 0, 6),
            fact(4, Some(2), SyntaxFactKind::Signature, PARAMETERS, 2, 5),
            fact(5, Some(2), SyntaxFactKind::Declaration, PARAMETER, 2, 5),
            fact(6, Some(5), SyntaxFactKind::Occurrence, DEFINITION, 2, 5),
            fact(7, Some(2), SyntaxFactKind::Occurrence, READ, 8, 11),
        ];
        let plan =
            FSharpBindings::new(&facts, QUALIFIED, &symbols(), 64, &Cancellation::new()).unwrap();
        assert_eq!(
            plan.resolve(&facts[6], "a.b", &Cancellation::new())
                .unwrap(),
            None
        );
    }

    #[test]
    fn forged_read_identity_is_not_accepted_by_spelling_or_local_id() {
        let facts = facts();
        let plan =
            FSharpBindings::new(&facts, SOURCE, &symbols(), 64, &Cancellation::new()).unwrap();
        for forged in [
            fact(7, Some(2), SyntaxFactKind::Occurrence, READ, 2, 3),
            fact(7, Some(12), SyntaxFactKind::Occurrence, READ, 6, 7),
            fact(99, Some(2), SyntaxFactKind::Occurrence, READ, 6, 7),
        ] {
            assert_eq!(
                plan.resolve(&forged, "x", &Cancellation::new()).unwrap(),
                None
            );
        }
    }

    #[test]
    fn duplicate_ids_cycles_and_invalid_name_bytes_return_typed_capture_errors() {
        let mut duplicate = facts();
        duplicate.push(duplicate[0].clone());
        let mut cyclic = facts();
        cyclic.retain(|fact| fact.local_id() != 7);
        cyclic.push(fact(7, Some(7), SyntaxFactKind::Occurrence, READ, 6, 7));
        for malformed in [&duplicate, &cyclic] {
            assert!(matches!(
                FSharpBindings::new(malformed, SOURCE, &symbols(), 64, &Cancellation::new()),
                Err(AdapterError::ProviderFailed { .. })
            ));
        }
        assert!(matches!(
            FSharpBindings::new(&facts(), b"f", &symbols(), 64, &Cancellation::new()),
            Err(AdapterError::ProviderFailed { .. })
        ));
        let mut invalid_utf8 = SOURCE.to_vec();
        invalid_utf8[2] = 0xff;
        assert!(matches!(
            FSharpBindings::new(
                &facts(),
                &invalid_utf8,
                &symbols(),
                64,
                &Cancellation::new()
            ),
            Err(AdapterError::ProviderFailed { .. })
        ));
    }

    #[test]
    fn invalid_utf8_in_the_exact_read_returns_the_typed_capture_error() {
        let facts = facts();
        let mut source = SOURCE.to_vec();
        source[6] = 0xff;
        let plan =
            FSharpBindings::new(&facts, &source, &symbols(), 64, &Cancellation::new()).unwrap();
        assert!(matches!(
            plan.resolve(read(&facts, 7), "x", &Cancellation::new()),
            Err(AdapterError::ProviderFailed { .. })
        ));
    }

    #[test]
    fn construction_and_lookup_propagate_typed_cancellation() {
        let facts = facts();
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            FSharpBindings::new(&facts, SOURCE, &symbols(), 64, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        let plan =
            FSharpBindings::new(&facts, SOURCE, &symbols(), 64, &Cancellation::new()).unwrap();
        assert!(matches!(
            plan.resolve(read(&facts, 7), "x", &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    fn module_value_facts(source: &str, id: u64, name: &str) -> Vec<SyntaxFact> {
        let start = source.find(&format!("{name} =")).unwrap();
        module_value_facts_at(source, id, name, start)
    }

    fn module_value_facts_at(source: &str, id: u64, name: &str, start: usize) -> Vec<SyntaxFact> {
        let tail = &source[start..];
        let end = start + tail.find('\n').unwrap_or(tail.len());
        let header_end = start + tail.find('=').unwrap();
        [
            fact(
                id,
                Some(1),
                SyntaxFactKind::Declaration,
                "fsharp.variable.declaration",
                start as u64,
                end as u64,
            ),
            fact(
                id + 1,
                Some(id),
                SyntaxFactKind::Scope,
                "fsharp.variable.scope",
                start as u64,
                end as u64,
            ),
            fact(
                id + 2,
                Some(id + 1),
                SyntaxFactKind::Signature,
                "fsharp.variable.signature",
                start as u64,
                header_end as u64,
            ),
            fact(
                id + 3,
                Some(id + 1),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                start as u64,
                (start + name.len()) as u64,
            ),
            fact(
                id + 4,
                Some(id),
                SyntaxFactKind::Signature,
                "fsharp.nonrecursive_binding.expression",
                start as u64,
                end as u64,
            ),
        ]
        .into_iter()
        .collect()
    }

    #[test]
    fn ordinary_module_values_require_exact_native_eligibility_and_source_order() {
        let source = "seed = 1\nalias = seed\n";
        let mut facts = vec![fact(
            1,
            None,
            SyntaxFactKind::Scope,
            "fsharp.file.scope",
            0,
            source.len() as u64,
        )];
        facts.extend(module_value_facts(source, 10, "seed"));
        facts.extend(module_value_facts(source, 20, "alias"));
        let start = source.rfind("seed").unwrap() as u64;
        facts.push(fact(
            26,
            Some(21),
            SyntaxFactKind::Occurrence,
            READ,
            start,
            start + 4,
        ));
        let target = SymbolId::from_bytes([3; 20]);
        let symbols = HashMap::from([(13, target)]);
        for case in 0..9 {
            let mut facts = facts.clone();
            let mut symbols = symbols.clone();
            match case {
                0 => {}
                1 => facts.retain(|fact| fact.local_id() != 14),
                2 => facts.retain(|fact| fact.local_id() != 24),
                3 => facts.push(fact(
                    30,
                    Some(10),
                    SyntaxFactKind::Signature,
                    "fsharp.nonrecursive_binding.expression",
                    0,
                    8,
                )),
                4 => {
                    symbols.clear();
                }
                5 => facts.push(fact(
                    30,
                    Some(1),
                    SyntaxFactKind::Import,
                    "fsharp.open.import",
                    8,
                    9,
                )),
                6 => {
                    facts.retain(|fact| fact.local_id() != 14);
                    facts.push(fact(
                        14,
                        Some(11),
                        SyntaxFactKind::Signature,
                        "fsharp.nonrecursive_binding.expression",
                        0,
                        8,
                    ));
                }
                7 => {
                    facts.retain(|fact| fact.local_id() != 14);
                    facts.push(fact(
                        14,
                        Some(10),
                        SyntaxFactKind::Signature,
                        "fsharp.nonrecursive_binding.expression",
                        0,
                        7,
                    ));
                }
                8 => facts.retain(|fact| fact.local_id() != 13),
                _ => unreachable!(),
            }
            for _ in 0..2 {
                let plan = FSharpBindings::new(
                    &facts,
                    source.as_bytes(),
                    &symbols,
                    64,
                    &Cancellation::new(),
                )
                .unwrap();
                assert_eq!(
                    plan.resolve(read(&facts, 26), "seed", &Cancellation::new())
                        .unwrap(),
                    (case == 0).then_some(target),
                    "case {case}"
                );
                facts.reverse();
            }
        }
    }

    #[test]
    fn unavailable_nearer_module_bindings_do_not_reveal_older_homonyms() {
        let source = "seed = 1\nseed = 2\nalias = seed\n";
        let mut facts = vec![fact(
            1,
            None,
            SyntaxFactKind::Scope,
            "fsharp.file.scope",
            0,
            source.len() as u64,
        )];
        facts.extend(module_value_facts(source, 10, "seed"));
        facts.extend(module_value_facts_at(
            source,
            20,
            "seed",
            source.find("seed = 2").unwrap(),
        ));
        facts.extend(module_value_facts(source, 30, "alias"));
        let start = source.rfind("seed").unwrap() as u64;
        facts.push(fact(
            36,
            Some(31),
            SyntaxFactKind::Occurrence,
            READ,
            start,
            start + 4,
        ));
        let symbols = HashMap::from([(13, SymbolId::from_bytes([3; 20]))]);
        for _ in 0..2 {
            let plan = FSharpBindings::new(
                &facts,
                source.as_bytes(),
                &symbols,
                64,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(
                plan.resolve(read(&facts, 36), "seed", &Cancellation::new())
                    .unwrap(),
                None
            );
            facts.reverse();
        }
    }

    #[test]
    fn unavailable_own_formals_still_shadow_materialized_module_values() {
        let source = "x = 1\nf x = x\n";
        let mut facts = vec![fact(
            1,
            None,
            SyntaxFactKind::Scope,
            "fsharp.file.scope",
            0,
            source.len() as u64,
        )];
        facts.extend(module_value_facts(source, 10, "x"));
        let mut function = function_facts(20, 6).to_vec();
        function[0] = fact(20, Some(1), SyntaxFactKind::Declaration, FUNCTION, 6, 13);
        facts.extend(function);
        facts.push(fact(
            40,
            Some(20),
            SyntaxFactKind::Signature,
            "fsharp.nonrecursive_binding.expression",
            6,
            13,
        ));
        facts.push(fact(
            41,
            Some(21),
            SyntaxFactKind::Occurrence,
            DEFINITION,
            6,
            7,
        ));
        let outer = SymbolId::from_bytes([3; 20]);
        let own = SymbolId::from_bytes([4; 20]);
        for available in [true, false] {
            let mut symbols = HashMap::from([(13, outer)]);
            if available {
                symbols.insert(25, own);
            }
            for _ in 0..2 {
                let plan = FSharpBindings::new(
                    &facts,
                    source.as_bytes(),
                    &symbols,
                    64,
                    &Cancellation::new(),
                )
                .unwrap();
                assert_eq!(
                    plan.resolve(read(&facts, 26), "x", &Cancellation::new())
                        .unwrap(),
                    available.then_some(own)
                );
                facts.reverse();
            }
        }
    }
}
