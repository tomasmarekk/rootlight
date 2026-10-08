//! Exact local-let, lambda and match-arm environments over native witnesses.
//! Publication follows native source order; incomplete ownership stays closed
//! instead of exposing a same-named binding from an outer environment.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FileId, SymbolId};

use super::{
    DEFINITION, FSharpBindings, FUNCTION, HEADER, PARAMETER, PARAMETER_SIGNATURE, PARAMETERS, READ,
    SCOPE, ScopedSignatureFact, UNIT_PARAMETER, UNIT_PARAMETERS, UniqueFact, allocation_failed,
    contains, invalid_capture, source_bytes, source_name, unit_marker_bytes,
    unqualified_written_name, whitespace_between,
};

const LOCAL_SCOPE: &str = "fsharp.local_binding.scope";
const WRAPPER: &str = "fsharp.local_binding.expression";
const GROUP: &str = "fsharp.local_group.expression";
const CONTINUATION: &str = "fsharp.local_continuation.expression";
const VARIABLE: &str = "fsharp.variable.declaration";
const VARIABLE_SCOPE: &str = "fsharp.variable.scope";
const VARIABLE_HEADER: &str = "fsharp.variable.signature";
const LAMBDA_SCOPE: &str = "fsharp.lambda.scope";
const LAMBDA_HEADER: &str = "fsharp.lambda_header.expression";
const LAMBDA_BODY: &str = "fsharp.lambda_body.expression";

struct LambdaFrame<'a> {
    scope: &'a SyntaxFact,
    header: UniqueFact<'a>,
    parameters: UniqueFact<'a>,
    unit_parameters: UniqueFact<'a>,
    units: Vec<&'a SyntaxFact>,
    body: UniqueFact<'a>,
    formals: Vec<LambdaParameter<'a>>,
    invalid: bool,
}

struct LambdaParameter<'a> {
    declaration: &'a SyntaxFact,
    definition: UniqueFact<'a>,
    header: UniqueFact<'a>,
}

struct LambdaFormal<'fact, 'source> {
    declaration: &'fact SyntaxFact,
    definition: &'fact SyntaxFact,
    header: &'fact SyntaxFact,
    name: &'source str,
}

struct LambdaProof<'fact, 'source> {
    body: &'fact SyntaxFact,
    formals: Vec<LambdaFormal<'fact, 'source>>,
}

struct LambdaEnvironment<'fact, 'source> {
    scope: &'fact SyntaxFact,
    proof: Option<LambdaProof<'fact, 'source>>,
}

struct Frame<'a> {
    scope: &'a SyntaxFact,
    wrapper: UniqueFact<'a>,
    group: UniqueFact<'a>,
    continuation: UniqueFact<'a>,
    declaration: UniqueFact<'a>,
    duplicate: bool,
}

struct Owner<'a> {
    declaration: &'a SyntaxFact,
    scope: UniqueFact<'a>,
    header: UniqueFact<'a>,
    definition: UniqueFact<'a>,
}

impl Owner<'_> {
    fn valid(&self) -> bool {
        let (Some(scope), Some(header)) = (self.scope.one(), self.header.one()) else {
            return false;
        };
        scope.kind() == SyntaxFactKind::Scope
            && scope.syntax_kind().as_str()
                == if self.declaration.syntax_kind().as_str() == FUNCTION {
                    SCOPE
                } else {
                    VARIABLE_SCOPE
                }
            && scope.parent() == Some(self.declaration.local_id())
            && scope.span() == self.declaration.span()
            && header.kind() == SyntaxFactKind::Signature
            && header.syntax_kind().as_str()
                == if self.declaration.syntax_kind().as_str() == FUNCTION {
                    HEADER
                } else {
                    VARIABLE_HEADER
                }
            && (header.parent() == Some(self.declaration.local_id())
                || header.parent() == Some(scope.local_id()))
            && header.span().start_byte() == scope.span().start_byte()
            && header.span().start_byte() < header.span().end_byte()
            && header.span().end_byte() < scope.span().end_byte()
            && contains(scope, header)
    }
}

type Names<'a> = Vec<(&'a str, Option<SymbolId>)>;

struct Interval<'a> {
    scope: u64,
    file: FileId,
    start: u64,
    end: u64,
    order: u8,
    parent_scope: Option<u64>,
    container: bool,
    authorized: bool,
    local: bool,
    closed: bool,
    reviewed_parent: bool,
    base: Option<u64>,
    names: Names<'a>,
}

#[derive(Clone, Copy)]
struct Active {
    interval: usize,
    closed: bool,
    base: Option<u64>,
}

#[derive(Clone, Copy)]
struct NamedTarget {
    depth: usize,
    symbol: Option<SymbolId>,
}

/// Resolve each retained read once while sweeping exact lexical intervals.
///
/// A new local name is published only in the native continuation. Lambda
/// formals and match binders are published only in their complete native body. Function
/// bodies therefore capture the environment preceding their own declaration;
/// later continuation bindings cannot change those targets. Closed entries
/// and missing optional proofs never expose an older same-spelled binding.
#[derive(Default)]
pub(super) struct LocalBindings {
    targets: HashMap<u64, Option<SymbolId>>,
    qualified_roots: HashMap<u64, super::QualifiedRoot>,
    arities: HashMap<SymbolId, Option<usize>>,
}

impl LocalBindings {
    pub(super) fn new(
        plan: &FSharpBindings<'_>,
        symbols: &HashMap<u64, SymbolId>,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut has_local = false;
        for fact in plan.facts.values() {
            cancellation.check()?;
            if matches!(fact.syntax_kind().as_str(), LOCAL_SCOPE | LAMBDA_SCOPE)
                || fact.syntax_kind().as_str() == super::branch::SCOPE
            {
                has_local = true;
                break;
            }
        }
        if !has_local {
            return Ok(Self::default());
        }
        let mut result = Self::default();
        let mut frames = HashMap::<u64, Frame<'_>>::new();
        let mut owners = HashMap::new();
        let mut positions = HashMap::new();
        for fact in plan.facts.values().copied() {
            cancellation.check()?;
            if matches!(
                fact.syntax_kind().as_str(),
                LOCAL_SCOPE
                    | WRAPPER
                    | GROUP
                    | CONTINUATION
                    | VARIABLE
                    | VARIABLE_SCOPE
                    | VARIABLE_HEADER
            ) {
                source_bytes(fact, plan.source)?;
            }
            if fact.kind() == SyntaxFactKind::Scope && fact.syntax_kind().as_str() == LOCAL_SCOPE {
                let key = (fact.parent(), fact.span());
                frames.try_reserve(1).map_err(allocation_failed)?;
                positions.try_reserve(1).map_err(allocation_failed)?;
                let previous = positions.insert(key, fact.local_id());
                if let Some(previous) = previous
                    && let Some(frame) = frames.get_mut(&previous)
                {
                    frame.duplicate = true;
                }
                frames.insert(
                    fact.local_id(),
                    Frame {
                        scope: fact,
                        wrapper: UniqueFact::default(),
                        group: UniqueFact::default(),
                        continuation: UniqueFact::default(),
                        declaration: UniqueFact::default(),
                        duplicate: previous.is_some(),
                    },
                );
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && matches!(fact.syntax_kind().as_str(), FUNCTION | VARIABLE)
            {
                owners.try_reserve(1).map_err(allocation_failed)?;
                owners.insert(
                    fact.local_id(),
                    Owner {
                        declaration: fact,
                        scope: UniqueFact::default(),
                        header: UniqueFact::default(),
                        definition: UniqueFact::default(),
                    },
                );
            }
        }
        for fact in plan.facts.values().copied() {
            cancellation.check()?;
            if let Some(frame) = fact.parent().and_then(|id| frames.get_mut(&id)) {
                match fact.syntax_kind().as_str() {
                    WRAPPER => frame.wrapper.retain(fact)?,
                    GROUP => frame.group.retain(fact)?,
                    CONTINUATION => frame.continuation.retain(fact)?,
                    _ if fact.kind() == SyntaxFactKind::Declaration => {
                        frame.declaration.retain(fact)?
                    }
                    _ => {}
                }
            }
            let parent = fact.parent().and_then(|id| plan.facts.get(&id).copied());
            let owner = parent.and_then(|parent| {
                if owners.contains_key(&parent.local_id()) {
                    Some(parent.local_id())
                } else if matches!(parent.syntax_kind().as_str(), SCOPE | VARIABLE_SCOPE) {
                    parent.parent()
                } else {
                    None
                }
            });
            if let Some(owner) = owner.and_then(|id| owners.get_mut(&id)) {
                match fact.syntax_kind().as_str() {
                    SCOPE | VARIABLE_SCOPE => owner.scope.retain(fact)?,
                    HEADER | VARIABLE_HEADER => owner.header.retain(fact)?,
                    label if label.ends_with(".definition") => owner.definition.retain(fact)?,
                    _ => {}
                }
            }
        }
        let mut formals = HashMap::<u64, Names<'_>>::new();
        let mut counts = HashMap::<u64, Option<usize>>::new();
        formals
            .try_reserve(plan.bindings.len())
            .map_err(allocation_failed)?;
        counts
            .try_reserve(plan.functions.len())
            .map_err(allocation_failed)?;
        for (&(scope, name), &symbol) in &plan.bindings {
            cancellation.check()?;
            let names = formals.entry(scope).or_default();
            names.try_reserve(1).map_err(allocation_failed)?;
            names.push((
                name,
                symbol.filter(|symbol| !plan.conflicting_symbols.contains(symbol)),
            ));
        }
        for fact in plan.facts.values().copied() {
            cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == PARAMETER
                && let Some(scope) = fact.parent()
            {
                let available = source_name(fact, plan.source, plan.maximum_name_bytes)?
                    .and_then(|name| plan.bindings.get(&(scope, name)))
                    .copied()
                    .flatten()
                    .is_some_and(|symbol| !plan.conflicting_symbols.contains(&symbol));
                let count = counts.entry(scope).or_insert(Some(0));
                *count = if available {
                    count
                        .map(|count| count.checked_add(1).ok_or_else(invalid_capture))
                        .transpose()?
                } else {
                    None
                };
            }
        }
        let mut intervals = Vec::new();
        intervals
            .try_reserve(plan.facts.len())
            .map_err(allocation_failed)?;
        if let Some(lexical) = &plan.lexical {
            for (scope, file, start, end) in lexical.body_ranges() {
                cancellation.check()?;
                intervals.push(Interval {
                    scope,
                    file,
                    start,
                    end,
                    order: 1,
                    parent_scope: None,
                    container: true,
                    authorized: true,
                    local: false,
                    closed: false,
                    reviewed_parent: false,
                    base: Some(scope),
                    names: formals.remove(&scope).unwrap_or_default(),
                });
            }
        }
        for frame in frames.values() {
            cancellation.check()?;
            let scope = frame.scope;
            let owner = frame
                .declaration
                .one()
                .and_then(|fact| owners.get(&fact.local_id()));
            let definition = owner
                .and_then(|owner| owner.definition.one())
                .filter(|definition| {
                    definition.kind() == SyntaxFactKind::Occurrence
                        && definition.syntax_kind().as_str() == DEFINITION
                        && owner.is_some_and(|owner| {
                            (definition.parent() == Some(owner.declaration.local_id())
                                || owner.scope.one().is_some_and(|scope| {
                                    definition.parent() == Some(scope.local_id())
                                }))
                                && owner
                                    .header
                                    .one()
                                    .is_some_and(|header| contains(header, definition))
                        })
                });
            let name = definition
                .map(|fact| source_name(fact, plan.source, plan.maximum_name_bytes))
                .transpose()?
                .flatten()
                .filter(|name| {
                    definition.is_some_and(|fact| {
                        unqualified_written_name(fact, name, plan.maximum_name_bytes)
                    })
                });
            let valid = !frame.duplicate
                && owner.is_some_and(Owner::valid)
                && name.is_some()
                && valid_frame(frame, owner.map(|owner| owner.declaration));
            let symbol = valid
                .then(|| definition.and_then(|fact| symbols.get(&fact.local_id()).copied()))
                .flatten()
                .filter(|symbol| !plan.conflicting_symbols.contains(symbol));
            push_interval(
                &mut intervals,
                Interval {
                    scope: scope.local_id(),
                    file: scope.span().file(),
                    start: scope.span().start_byte(),
                    end: scope.span().end_byte(),
                    order: 2,
                    parent_scope: scope.parent(),
                    container: true,
                    authorized: false,
                    local: true,
                    closed: !valid,
                    reviewed_parent: false,
                    base: None,
                    names: Vec::new(),
                },
            )?;
            if valid && let Some(continuation) = frame.continuation.one() {
                let mut names = Vec::new();
                names.try_reserve_exact(1).map_err(allocation_failed)?;
                if let Some(name) = name {
                    names.push((name, symbol));
                }
                push_interval(
                    &mut intervals,
                    Interval {
                        scope: scope.local_id(),
                        file: scope.span().file(),
                        start: continuation.span().start_byte(),
                        end: continuation.span().end_byte(),
                        order: 0,
                        parent_scope: Some(scope.local_id()),
                        container: false,
                        authorized: true,
                        local: true,
                        closed: false,
                        reviewed_parent: false,
                        base: None,
                        names,
                    },
                )?;
            }
            if let Some(owner) = owner
                && owner.valid()
                && let (Some(body), Some(header)) = (owner.scope.one(), owner.header.one())
            {
                let function = plan.functions.get(&owner.declaration.local_id());
                let body_valid = owner.declaration.syntax_kind().as_str() == VARIABLE
                    || function.is_some_and(|function| function.valid());
                if let Some(symbol) = symbol
                    && owner.declaration.syntax_kind().as_str() == FUNCTION
                {
                    let arity = function
                        .filter(|function| function.valid())
                        .and_then(|function| {
                            function.callable_arity(counts.get(&body.local_id()).copied())
                        });
                    result.arities.try_reserve(1).map_err(allocation_failed)?;
                    result
                        .arities
                        .entry(symbol)
                        .and_modify(|arity| *arity = None)
                        .or_insert(arity);
                }
                push_interval(
                    &mut intervals,
                    Interval {
                        scope: body.local_id(),
                        file: body.span().file(),
                        start: header.span().end_byte(),
                        end: body.span().end_byte(),
                        order: 1,
                        parent_scope: Some(scope.local_id()),
                        container: true,
                        authorized: body_valid,
                        local: true,
                        closed: !body_valid,
                        reviewed_parent: false,
                        base: None,
                        names: if body_valid {
                            formals.remove(&body.local_id()).unwrap_or_default()
                        } else {
                            Vec::new()
                        },
                    },
                )?;
            }
        }
        for environment in lambda_environments(
            &plan.facts,
            plan.source,
            plan.maximum_name_bytes,
            cancellation,
        )? {
            cancellation.check()?;
            let scope = environment.scope;
            let mut names = Vec::new();
            let mut available = environment.proof.is_some();
            if let Some(proof) = &environment.proof {
                names
                    .try_reserve_exact(proof.formals.len())
                    .map_err(allocation_failed)?;
                for formal in &proof.formals {
                    cancellation.check()?;
                    let symbol = symbols
                        .get(&formal.definition.local_id())
                        .copied()
                        .filter(|symbol| !plan.conflicting_symbols.contains(symbol));
                    available &= symbol.is_some();
                    names.push((formal.name, symbol));
                }
            }
            push_interval(
                &mut intervals,
                Interval {
                    scope: scope.local_id(),
                    file: scope.span().file(),
                    start: scope.span().start_byte(),
                    end: scope.span().end_byte(),
                    order: 2,
                    parent_scope: scope.parent(),
                    container: true,
                    authorized: false,
                    local: true,
                    closed: !available,
                    reviewed_parent: true,
                    base: None,
                    names: Vec::new(),
                },
            )?;
            if available && let Some(proof) = environment.proof {
                push_interval(
                    &mut intervals,
                    Interval {
                        scope: scope.local_id(),
                        file: scope.span().file(),
                        start: proof.body.span().start_byte(),
                        end: proof.body.span().end_byte(),
                        order: 0,
                        parent_scope: Some(scope.local_id()),
                        container: false,
                        authorized: true,
                        local: true,
                        closed: false,
                        reviewed_parent: false,
                        base: None,
                        names,
                    },
                )?;
            }
        }
        for environment in super::branch::environments(
            &plan.facts,
            plan.source,
            plan.maximum_name_bytes,
            cancellation,
        )? {
            cancellation.check()?;
            let scope = environment.scope;
            let mut names = Vec::new();
            let mut available = environment.proof.is_some();
            if let Some(proof) = &environment.proof {
                names
                    .try_reserve_exact(proof.bindings.len())
                    .map_err(allocation_failed)?;
                for binding in &proof.bindings {
                    cancellation.check()?;
                    let symbol = symbols
                        .get(&binding.definition.local_id())
                        .copied()
                        .filter(|symbol| !plan.conflicting_symbols.contains(symbol));
                    available &= symbol.is_some();
                    names.push((binding.name, symbol));
                }
            }
            // An incomplete arm remains a barrier, including against an outer
            // homonym. Its binders never enter the scrutinee or a sibling arm.
            push_interval(
                &mut intervals,
                Interval {
                    scope: scope.local_id(),
                    file: scope.span().file(),
                    start: scope.span().start_byte(),
                    end: scope.span().end_byte(),
                    order: 2,
                    parent_scope: scope.parent(),
                    container: true,
                    authorized: false,
                    local: true,
                    closed: !available,
                    reviewed_parent: true,
                    base: None,
                    names: Vec::new(),
                },
            )?;
            if available && let Some(proof) = environment.proof {
                push_interval(
                    &mut intervals,
                    Interval {
                        scope: scope.local_id(),
                        file: scope.span().file(),
                        start: proof.body.span().start_byte(),
                        end: proof.body.span().end_byte(),
                        order: 0,
                        parent_scope: Some(scope.local_id()),
                        container: false,
                        authorized: true,
                        local: true,
                        closed: false,
                        reviewed_parent: false,
                        base: None,
                        names,
                    },
                )?;
            }
        }
        result.sweep(plan, &intervals, cancellation)?;
        Ok(result)
    }

    pub(super) fn resolve(&self, read: &SyntaxFact) -> Option<Option<SymbolId>> {
        self.targets.get(&read.local_id()).copied()
    }

    pub(super) fn qualified_root(&self, read: &SyntaxFact) -> Option<super::QualifiedRoot> {
        self.qualified_roots.get(&read.local_id()).copied()
    }

    pub(super) fn arity(&self, symbol: SymbolId) -> Option<usize> {
        self.arities.get(&symbol).copied().flatten()
    }

    fn sweep(
        &mut self,
        plan: &FSharpBindings<'_>,
        intervals: &[Interval<'_>],
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        let mut order = Vec::new();
        order
            .try_reserve_exact(intervals.len())
            .map_err(allocation_failed)?;
        for index in 0..intervals.len() {
            cancellation.check()?;
            order.push(index);
        }
        crate::runtime::sort_cancellable_by(&mut order, cancellation, |&left, &right| {
            let left = &intervals[left];
            let right = &intervals[right];
            (
                left.file,
                left.start,
                std::cmp::Reverse(left.end),
                left.order,
                left.scope,
            )
                .cmp(&(
                    right.file,
                    right.start,
                    std::cmp::Reverse(right.end),
                    right.order,
                    right.scope,
                ))
        })?;
        let mut reads = Vec::new();
        reads
            .try_reserve(plan.facts.len())
            .map_err(allocation_failed)?;
        for fact in plan.facts.values().copied() {
            cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Occurrence
                && matches!(fact.syntax_kind().as_str(), READ | super::QUALIFIED_READ)
            {
                reads.push(fact);
            }
        }
        crate::runtime::sort_cancellable_by(&mut reads, cancellation, |left, right| {
            (
                left.span().file(),
                left.span().start_byte(),
                left.span().end_byte(),
                left.local_id(),
            )
                .cmp(&(
                    right.span().file(),
                    right.span().start_byte(),
                    right.span().end_byte(),
                    right.local_id(),
                ))
        })?;
        let mut active = Vec::<Active>::new();
        let mut scopes = Vec::<usize>::new();
        let mut barriers = Vec::<usize>::new();
        let mut names = HashMap::<&str, Vec<NamedTarget>>::new();
        active
            .try_reserve(intervals.len())
            .map_err(allocation_failed)?;
        scopes
            .try_reserve(intervals.len())
            .map_err(allocation_failed)?;
        barriers
            .try_reserve(intervals.len())
            .map_err(allocation_failed)?;
        let mut capacity = 0usize;
        for interval in intervals {
            cancellation.check()?;
            capacity = capacity
                .checked_add(interval.names.len())
                .ok_or_else(invalid_capture)?;
        }
        names.try_reserve(capacity).map_err(allocation_failed)?;
        let mut root_names = HashMap::<&str, Vec<NamedTarget>>::new();
        root_names
            .try_reserve(capacity)
            .map_err(allocation_failed)?;
        self.targets
            .try_reserve(reads.len())
            .map_err(allocation_failed)?;
        self.qualified_roots
            .try_reserve(reads.len())
            .map_err(allocation_failed)?;
        let mut next = 0usize;
        let mut local_depth = 0usize;
        for read in reads {
            cancellation.check()?;
            while let Some(&index) = order.get(next)
                && let Some(interval) = intervals.get(index)
                && (interval.file, interval.start) <= (read.span().file(), read.span().start_byte())
            {
                pop_expired(
                    interval.file,
                    interval.start,
                    intervals,
                    &mut active,
                    &mut scopes,
                    &mut barriers,
                    &mut names,
                    &mut root_names,
                    &mut local_depth,
                    cancellation,
                )?;
                let parent = scopes
                    .last()
                    .and_then(|depth| active.get(*depth))
                    .map(|entry| &intervals[entry.interval]);
                let closed = interval.closed
                    || (interval.reviewed_parent
                        && !parent.is_some_and(|parent| {
                            interval.parent_scope == Some(parent.scope)
                                && (parent.authorized
                                    || active.last().is_some_and(|entry| {
                                        let region = &intervals[entry.interval];
                                        !entry.closed
                                            && !region.container
                                            && region.authorized
                                            && region.scope == parent.scope
                                            && interval.end <= region.end
                                    }))
                        }))
                    || interval.parent_scope.is_some_and(|expected| {
                        parent.is_none_or(|parent| parent.scope != expected)
                    })
                    || active.last().is_some_and(|entry| {
                        let outer = &intervals[entry.interval];
                        outer.file != interval.file || interval.end > outer.end
                    });
                let depth = active.len();
                let base = interval
                    .base
                    .or_else(|| active.last().and_then(|entry| entry.base));
                active.push(Active {
                    interval: index,
                    closed,
                    base,
                });
                if interval.container {
                    scopes.push(depth);
                }
                if closed {
                    barriers.push(depth);
                }
                if interval.local {
                    local_depth = local_depth.checked_add(1).ok_or_else(invalid_capture)?;
                }
                for &(name, symbol) in &interval.names {
                    cancellation.check()?;
                    let stack = names.entry(name).or_default();
                    stack.try_reserve(1).map_err(allocation_failed)?;
                    stack.push(NamedTarget { depth, symbol });
                    let stack = root_names
                        .entry(super::canonical_identifier(name))
                        .or_default();
                    stack.try_reserve(1).map_err(allocation_failed)?;
                    stack.push(NamedTarget { depth, symbol });
                }
                next += 1;
            }
            pop_expired(
                read.span().file(),
                read.span().start_byte(),
                intervals,
                &mut active,
                &mut scopes,
                &mut barriers,
                &mut names,
                &mut root_names,
                &mut local_depth,
                cancellation,
            )?;
            if local_depth == 0 {
                continue;
            }
            let parent = scopes
                .last()
                .and_then(|depth| active.get(*depth))
                .map(|entry| &intervals[entry.interval]);
            let authorized = parent.is_some_and(|parent| {
                read.parent() == Some(parent.scope)
                    && read.span().file() == parent.file
                    && read.span().start_byte() >= parent.start
                    && read.span().end_byte() <= parent.end
                    && (parent.authorized
                        || active.last().is_some_and(|entry| {
                            let continuation = &intervals[entry.interval];
                            !entry.closed
                                && continuation.authorized
                                && !continuation.container
                                && continuation.scope == parent.scope
                                && read.span().end_byte() <= continuation.end
                        }))
            });
            let qualified = read.syntax_kind().as_str() == super::QUALIFIED_READ;
            let name = source_name(read, plan.source, plan.maximum_name_bytes)?;
            let root_name = if qualified {
                name.and_then(super::written_components)
                    .and_then(|parts| parts.first().copied())
            } else {
                name.filter(|name| unqualified_written_name(read, name, plan.maximum_name_bytes))
            };
            // A missing local target is not absence: unavailable binders and
            // incomplete arm/lambda witnesses must veto owned-open deferral.
            let root = if !authorized || !barriers.is_empty() {
                super::QualifiedRoot::Closed
            } else if let Some(name) = root_name {
                if root_names.contains_key(super::canonical_identifier(name)) {
                    super::QualifiedRoot::Bound
                } else if !plan.allows_module_lookup() {
                    super::QualifiedRoot::Closed
                } else {
                    active
                        .last()
                        .and_then(|entry| entry.base)
                        .zip(plan.lexical.as_ref())
                        .map_or(super::QualifiedRoot::Closed, |(base, lexical)| {
                            lexical.qualified_root_in(base, read, name)
                        })
                }
            } else {
                super::QualifiedRoot::Closed
            };
            self.qualified_roots.insert(read.local_id(), root);
            if qualified {
                continue;
            }
            let name = root_name;
            let target = if authorized {
                name.and_then(|name| {
                    if let Some(named) = names.get(name).and_then(|stack| stack.last()) {
                        if barriers.last().is_none_or(|barrier| named.depth > *barrier) {
                            return named.symbol;
                        }
                        return None;
                    }
                    if !barriers.is_empty() || !plan.allows_module_lookup() {
                        return None;
                    }
                    let base = active.last()?.base?;
                    plan.lexical.as_ref()?.resolve_in(base, read, name)
                })
            } else {
                None
            };
            self.targets.insert(read.local_id(), target);
        }
        Ok(())
    }
}

pub(super) fn lambda_parameter_signatures<'a>(
    facts: &HashMap<u64, &'a SyntaxFact>,
    source: &[u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<HashMap<u64, &'a SyntaxFact>, AdapterError> {
    let mut signatures = HashMap::new();
    for environment in lambda_environments(facts, source, maximum_name_bytes, cancellation)? {
        cancellation.check()?;
        if let Some(proof) = environment.proof {
            signatures
                .try_reserve(proof.formals.len())
                .map_err(allocation_failed)?;
            for formal in proof.formals {
                cancellation.check()?;
                signatures.insert(formal.declaration.local_id(), formal.header);
            }
        }
    }
    Ok(signatures)
}

fn lambda_environments<'fact, 'source>(
    facts: &HashMap<u64, &'fact SyntaxFact>,
    source: &'source [u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Vec<LambdaEnvironment<'fact, 'source>>, AdapterError> {
    let mut frames = HashMap::<u64, LambdaFrame<'_>>::new();
    let mut scope_positions = HashMap::new();
    for fact in facts.values().copied() {
        cancellation.check()?;
        if matches!(
            fact.syntax_kind().as_str(),
            LAMBDA_SCOPE | LAMBDA_HEADER | LAMBDA_BODY
        ) {
            source_bytes(fact, source)?;
        }
        if fact.kind() != SyntaxFactKind::Scope || fact.syntax_kind().as_str() != LAMBDA_SCOPE {
            continue;
        }
        scope_positions.try_reserve(1).map_err(allocation_failed)?;
        let previous = scope_positions.insert((fact.parent(), fact.span()), fact.local_id());
        if let Some(previous) = previous
            && let Some(frame) = frames.get_mut(&previous)
        {
            frame.invalid = true;
        }
        frames.try_reserve(1).map_err(allocation_failed)?;
        frames.insert(
            fact.local_id(),
            LambdaFrame {
                scope: fact,
                header: UniqueFact::default(),
                parameters: UniqueFact::default(),
                unit_parameters: UniqueFact::default(),
                units: Vec::new(),
                body: UniqueFact::default(),
                formals: Vec::new(),
                invalid: previous.is_some(),
            },
        );
    }
    if frames.is_empty() {
        return Ok(Vec::new());
    }
    let mut parameters = HashMap::<u64, LambdaParameter<'_>>::new();
    let mut leaves = Vec::new();
    for fact in facts.values().copied() {
        cancellation.check()?;
        let Some(frame) = fact.parent().and_then(|id| frames.get_mut(&id)) else {
            continue;
        };
        match fact.syntax_kind().as_str() {
            LAMBDA_HEADER => frame.header.retain(fact)?,
            LAMBDA_BODY => frame.body.retain(fact)?,
            PARAMETERS => frame.parameters.retain(fact)?,
            UNIT_PARAMETERS => frame.unit_parameters.retain(fact)?,
            UNIT_PARAMETER => {
                frame.units.try_reserve(1).map_err(allocation_failed)?;
                frame.units.push(fact);
            }
            _ if fact.kind() == SyntaxFactKind::Declaration => {
                if fact.syntax_kind().as_str() != PARAMETER {
                    frame.invalid = true;
                    continue;
                }
                parameters.try_reserve(1).map_err(allocation_failed)?;
                parameters.insert(
                    fact.local_id(),
                    LambdaParameter {
                        declaration: fact,
                        definition: UniqueFact::default(),
                        header: UniqueFact::default(),
                    },
                );
                // Count every declared leaf before checking definitions. A
                // missing definition cannot make a broad header unambiguous.
                leaves.try_reserve(1).map_err(allocation_failed)?;
                leaves.push(ScopedSignatureFact {
                    scope: frame.scope.local_id(),
                    fact,
                });
            }
            _ => {}
        }
    }
    let mut headers = Vec::new();
    for fact in facts.values().copied() {
        cancellation.check()?;
        if fact.syntax_kind().as_str().ends_with(".definition") {
            if let Some(parameter) = fact.parent().and_then(|id| parameters.get_mut(&id)) {
                parameter.definition.retain(fact)?;
            } else if let Some(frame) = fact.parent().and_then(|id| frames.get_mut(&id)) {
                frame.invalid = true;
            }
        } else if fact.syntax_kind().as_str() == PARAMETER_SIGNATURE {
            let scope = fact.parent().and_then(|parent| {
                parameters
                    .get(&parent)
                    .and_then(|parameter| parameter.declaration.parent())
                    .or_else(|| frames.contains_key(&parent).then_some(parent))
            });
            if let Some(scope) = scope {
                headers.try_reserve(1).map_err(allocation_failed)?;
                headers.push(ScopedSignatureFact { scope, fact });
            }
        }
    }
    crate::runtime::sort_cancellable_by(&mut leaves, cancellation, |left, right| {
        left.key().cmp(&right.key())
    })?;
    crate::runtime::sort_cancellable_by(&mut headers, cancellation, |left, right| {
        left.key().cmp(&right.key())
    })?;
    for entries in [&leaves, &headers] {
        for pair in entries.windows(2) {
            cancellation.check()?;
            if pair[0].group() == pair[1].group()
                && pair[0].fact.span().end_byte() > pair[1].fact.span().start_byte()
                && let Some(frame) = frames.get_mut(&pair[0].scope)
            {
                frame.invalid = true;
            }
        }
    }
    for header in headers {
        cancellation.check()?;
        let span = header.fact.span();
        let first = leaves.partition_point(|leaf| {
            (
                leaf.scope,
                leaf.fact.span().file(),
                leaf.fact.span().start_byte(),
            ) < (header.scope, span.file(), span.start_byte())
        });
        let after = leaves.partition_point(|leaf| {
            (
                leaf.scope,
                leaf.fact.span().file(),
                leaf.fact.span().start_byte(),
            ) < (header.scope, span.file(), span.end_byte())
        });
        let crossed = first
            .checked_sub(1)
            .and_then(|index| leaves.get(index))
            .is_some_and(|leaf| {
                leaf.group() == header.group() && leaf.fact.span().end_byte() > span.start_byte()
            });
        let owner = if !crossed && first.checked_add(1) == Some(after) {
            leaves.get(first).filter(|leaf| contains(header.fact, leaf.fact)
                && matches!(header.fact.parent(), Some(parent) if parent == header.scope || parent == leaf.fact.local_id()))
                .map(|leaf| leaf.fact.local_id())
        } else {
            None
        };
        if let Some(parameter) = owner.and_then(|id| parameters.get_mut(&id)) {
            parameter.header.retain(header.fact)?;
        } else if let Some(frame) = frames.get_mut(&header.scope) {
            frame.invalid = true;
        }
    }
    for parameter in parameters.into_values() {
        cancellation.check()?;
        if let Some(frame) = parameter
            .declaration
            .parent()
            .and_then(|id| frames.get_mut(&id))
        {
            frame.formals.try_reserve(1).map_err(allocation_failed)?;
            frame.formals.push(parameter);
        }
    }
    let mut environments = Vec::new();
    environments
        .try_reserve_exact(frames.len())
        .map_err(allocation_failed)?;
    for frame in frames.into_values() {
        cancellation.check()?;
        let proof = lambda_proof(&frame, source, maximum_name_bytes, cancellation)?;
        environments.push(LambdaEnvironment {
            scope: frame.scope,
            proof,
        });
    }
    Ok(environments)
}

fn lambda_proof<'fact, 'source>(
    frame: &LambdaFrame<'fact>,
    source: &'source [u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Option<LambdaProof<'fact, 'source>>, AdapterError> {
    let parameters = match (frame.parameters.count, frame.unit_parameters.count) {
        (1, 0) if frame.units.is_empty() => frame.parameters.one(),
        (0, 1) if !frame.units.is_empty() => frame.unit_parameters.one(),
        _ => None,
    };
    let (Some(header), Some(parameters), Some(body)) =
        (frame.header.one(), parameters, frame.body.one())
    else {
        return Ok(None);
    };
    let scope = frame.scope;
    if frame.invalid
        || frame.formals.is_empty() && frame.units.is_empty()
        || [header, parameters, body].into_iter().any(|fact| {
            fact.kind() != SyntaxFactKind::Signature
                || fact.parent() != Some(scope.local_id())
                || !contains(scope, fact)
                || fact.span().start_byte() >= fact.span().end_byte()
        })
        || header.span().start_byte() != scope.span().start_byte()
        || header.span().end_byte() > body.span().start_byte()
        || body.span().end_byte() != scope.span().end_byte()
        || !contains(header, parameters)
    {
        return Ok(None);
    }
    let header_bytes = source_bytes(header, source)?;
    if !header_bytes.starts_with(b"fun") || !header_bytes.ends_with(b"->") {
        return Ok(None);
    }
    let after_keyword = header
        .span()
        .start_byte()
        .checked_add(3)
        .ok_or_else(invalid_capture)?;
    let before_arrow = header
        .span()
        .end_byte()
        .checked_sub(2)
        .ok_or_else(invalid_capture)?;
    if after_keyword >= parameters.span().start_byte()
        || parameters.span().end_byte() > before_arrow
        || !whitespace_between(
            source,
            after_keyword,
            parameters.span().start_byte(),
            cancellation,
        )?
        || !whitespace_between(
            source,
            parameters.span().end_byte(),
            before_arrow,
            cancellation,
        )?
        || !whitespace_between(
            source,
            header.span().end_byte(),
            body.span().start_byte(),
            cancellation,
        )?
    {
        return Ok(None);
    }
    let mut formals = Vec::new();
    let mut names = HashSet::new();
    let mut slots = Vec::new();
    formals
        .try_reserve_exact(frame.formals.len())
        .map_err(allocation_failed)?;
    names
        .try_reserve(frame.formals.len())
        .map_err(allocation_failed)?;
    slots
        .try_reserve_exact(
            frame
                .formals
                .len()
                .checked_add(frame.units.len())
                .ok_or_else(invalid_capture)?,
        )
        .map_err(allocation_failed)?;
    for parameter in &frame.formals {
        cancellation.check()?;
        let declaration = parameter.declaration;
        let (Some(definition), Some(parameter_header), Some(name)) = (
            parameter.definition.one(),
            parameter.header.one(),
            source_name(declaration, source, maximum_name_bytes)?,
        ) else {
            return Ok(None);
        };
        if declaration.parent() != Some(scope.local_id())
            || !contains(parameters, declaration)
            || definition.kind() != SyntaxFactKind::Occurrence
            || definition.syntax_kind().as_str() != DEFINITION
            || definition.parent() != Some(declaration.local_id())
            || definition.span() != declaration.span()
            || parameter_header.kind() != SyntaxFactKind::Signature
            || !matches!(parameter_header.parent(), Some(parent) if parent == declaration.local_id() || parent == scope.local_id())
            || !contains(parameter_header, declaration)
            || !unqualified_written_name(definition, name, maximum_name_bytes)
            || !names.insert(name)
        {
            return Ok(None);
        }
        slots.push(parameter_header);
        formals.push(LambdaFormal {
            declaration,
            definition,
            header: parameter_header,
            name,
        });
    }
    for unit in &frame.units {
        cancellation.check()?;
        if unit.kind() != SyntaxFactKind::Signature
            || unit.parent() != Some(scope.local_id())
            || !unit_marker_bytes(source_bytes(unit, source)?, cancellation)?
        {
            return Ok(None);
        }
        slots.push(unit);
    }
    crate::runtime::sort_cancellable_by(&mut slots, cancellation, |left, right| {
        let key = |fact: &&SyntaxFact| {
            (
                fact.span().file(),
                fact.span().start_byte(),
                fact.span().end_byte(),
            )
        };
        key(left).cmp(&key(right))
    })?;
    let mut position = parameters.span().start_byte();
    for slot in slots {
        cancellation.check()?;
        let span = slot.span();
        if !contains(parameters, slot)
            || span.start_byte() >= span.end_byte()
            || span.start_byte() < position
            || !whitespace_between(source, position, span.start_byte(), cancellation)?
        {
            return Ok(None);
        }
        position = span.end_byte();
    }
    // Full authored headers and binder-free units conserve the native list.
    // Losing a slot cannot expose an outer homonym or shorten a unit list.
    if !whitespace_between(source, position, parameters.span().end_byte(), cancellation)? {
        return Ok(None);
    }
    Ok(Some(LambdaProof { body, formals }))
}

fn valid_frame(frame: &Frame<'_>, declaration: Option<&SyntaxFact>) -> bool {
    let (Some(wrapper), Some(group), Some(continuation), Some(declaration)) = (
        frame.wrapper.one(),
        frame.group.one(),
        frame.continuation.one(),
        declaration,
    ) else {
        return false;
    };
    let scope = frame.scope;
    [wrapper, group, continuation].into_iter().all(|fact| {
        fact.kind() == SyntaxFactKind::Signature
            && fact.parent() == Some(scope.local_id())
            && contains(scope, fact)
            && fact.span().start_byte() < fact.span().end_byte()
    }) && wrapper.span() == scope.span()
        && declaration.parent() == Some(scope.local_id())
        && contains(group, declaration)
        && group.span().start_byte() == scope.span().start_byte()
        && group.span().end_byte() == declaration.span().end_byte()
        && group.span().end_byte() <= continuation.span().start_byte()
        && continuation.span().end_byte() == scope.span().end_byte()
}

fn push_interval<'a>(
    intervals: &mut Vec<Interval<'a>>,
    interval: Interval<'a>,
) -> Result<(), AdapterError> {
    intervals.try_reserve(1).map_err(allocation_failed)?;
    intervals.push(interval);
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the sweep retires its parallel bounded stacks together"
)]
fn pop_expired(
    file: FileId,
    start: u64,
    intervals: &[Interval<'_>],
    active: &mut Vec<Active>,
    scopes: &mut Vec<usize>,
    barriers: &mut Vec<usize>,
    names: &mut HashMap<&str, Vec<NamedTarget>>,
    root_names: &mut HashMap<&str, Vec<NamedTarget>>,
    local_depth: &mut usize,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    while active.last().is_some_and(|entry| {
        let interval = &intervals[entry.interval];
        interval.file != file || interval.end <= start
    }) {
        cancellation.check()?;
        let depth = active.len() - 1;
        let entry = active.pop().ok_or_else(invalid_capture)?;
        let interval = &intervals[entry.interval];
        if interval.container && scopes.pop() != Some(depth) {
            return Err(invalid_capture());
        }
        if entry.closed && barriers.pop() != Some(depth) {
            return Err(invalid_capture());
        }
        if interval.local {
            *local_depth = local_depth.checked_sub(1).ok_or_else(invalid_capture)?;
        }
        for &(name, _) in &interval.names {
            cancellation.check()?;
            let stack = names.get_mut(name).ok_or_else(invalid_capture)?;
            if stack.pop().is_none_or(|named| named.depth != depth) {
                return Err(invalid_capture());
            }
            if stack.is_empty() {
                names.remove(name);
            }
            let canonical = super::canonical_identifier(name);
            let stack = root_names.get_mut(canonical).ok_or_else(invalid_capture)?;
            if stack.pop().is_none_or(|named| named.depth != depth) {
                return Err(invalid_capture());
            }
            if stack.is_empty() {
                root_names.remove(canonical);
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ir::SourceSpan;

    const SOURCE: &str = "let outer item = let item = item in let cached extra = item extra in let item = item in cached item";

    fn fact(
        id: u64,
        parent: Option<u64>,
        kind: SyntaxFactKind,
        label: &str,
        start: usize,
        end: usize,
    ) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            kind,
            SourceSpan::new(
                FileId::from_bytes([9; 20]),
                u64::try_from(start).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn fixture() -> (Vec<SyntaxFact>, HashMap<u64, SymbolId>) {
        let first = SOURCE.find("let item").unwrap();
        let cached = SOURCE.find("let cached").unwrap();
        let second = SOURCE.rfind("let item").unwrap();
        let continuation = SOURCE.rfind("cached item").unwrap();
        let first_body = SOURCE[first..].find("= item").unwrap() + first + 2;
        let cached_body = SOURCE[cached..].find("= item").unwrap() + cached + 2;
        let second_body = SOURCE[second..].find("= item").unwrap() + second + 2;
        let outer_header = SOURCE.find('=').unwrap();
        let outer_parameter = SOURCE.find("item").unwrap();
        let mut facts = vec![
            fact(
                1,
                None,
                SyntaxFactKind::Scope,
                "fsharp.file.scope",
                0,
                SOURCE.len(),
            ),
            fact(
                10,
                Some(1),
                SyntaxFactKind::Declaration,
                FUNCTION,
                4,
                SOURCE.len(),
            ),
            fact(11, Some(10), SyntaxFactKind::Scope, SCOPE, 4, SOURCE.len()),
            fact(
                12,
                Some(11),
                SyntaxFactKind::Signature,
                HEADER,
                4,
                outer_header,
            ),
            fact(
                13,
                Some(11),
                SyntaxFactKind::Signature,
                super::super::PARAMETERS,
                outer_parameter,
                outer_parameter + 4,
            ),
            fact(
                14,
                Some(11),
                SyntaxFactKind::Declaration,
                PARAMETER,
                outer_parameter,
                outer_parameter + 4,
            ),
            fact(
                15,
                Some(14),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                outer_parameter,
                outer_parameter + 4,
            ),
            fact(16, Some(11), SyntaxFactKind::Occurrence, DEFINITION, 4, 9),
            fact(
                17,
                Some(10),
                SyntaxFactKind::Signature,
                "fsharp.nonrecursive_binding.expression",
                4,
                SOURCE.len(),
            ),
        ];
        for (scope, parent, start, body_end, next) in [
            (20, 11, first, first_body + 4, cached),
            (40, 20, cached, cached_body + "item extra".len(), second),
            (60, 40, second, second_body + 4, continuation),
        ] {
            facts.extend([
                fact(
                    scope,
                    Some(parent),
                    SyntaxFactKind::Scope,
                    LOCAL_SCOPE,
                    start,
                    SOURCE.len(),
                ),
                fact(
                    scope + 1,
                    Some(scope),
                    SyntaxFactKind::Signature,
                    WRAPPER,
                    start,
                    SOURCE.len(),
                ),
                fact(
                    scope + 2,
                    Some(scope),
                    SyntaxFactKind::Signature,
                    GROUP,
                    start,
                    body_end,
                ),
                fact(
                    scope + 3,
                    Some(scope),
                    SyntaxFactKind::Signature,
                    CONTINUATION,
                    next,
                    SOURCE.len(),
                ),
            ]);
        }
        for (id, parent, start, body) in [
            (30, 20, first + 4, first_body),
            (70, 60, second + 4, second_body),
        ] {
            let header_end = SOURCE[start..].find('=').unwrap() + start;
            facts.extend([
                fact(
                    id,
                    Some(parent),
                    SyntaxFactKind::Declaration,
                    VARIABLE,
                    start,
                    body + 4,
                ),
                fact(
                    id + 1,
                    Some(id),
                    SyntaxFactKind::Scope,
                    VARIABLE_SCOPE,
                    start,
                    body + 4,
                ),
                fact(
                    id + 2,
                    Some(id + 1),
                    SyntaxFactKind::Signature,
                    VARIABLE_HEADER,
                    start,
                    header_end,
                ),
                fact(
                    id + 3,
                    Some(id + 1),
                    SyntaxFactKind::Occurrence,
                    DEFINITION,
                    start,
                    start + 4,
                ),
            ]);
        }
        let cached_head = cached + 4;
        let parameter = cached_head + "cached ".len();
        let header_end = SOURCE[cached_head..].find('=').unwrap() + cached_head;
        facts.extend([
            fact(
                50,
                Some(40),
                SyntaxFactKind::Declaration,
                FUNCTION,
                cached_head,
                cached_body + 10,
            ),
            fact(
                51,
                Some(50),
                SyntaxFactKind::Scope,
                SCOPE,
                cached_head,
                cached_body + 10,
            ),
            fact(
                52,
                Some(51),
                SyntaxFactKind::Signature,
                HEADER,
                cached_head,
                header_end,
            ),
            fact(
                53,
                Some(51),
                SyntaxFactKind::Signature,
                super::super::PARAMETERS,
                parameter,
                parameter + 5,
            ),
            fact(
                54,
                Some(51),
                SyntaxFactKind::Declaration,
                PARAMETER,
                parameter,
                parameter + 5,
            ),
            fact(
                55,
                Some(54),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                parameter,
                parameter + 5,
            ),
            fact(
                56,
                Some(51),
                SyntaxFactKind::Occurrence,
                DEFINITION,
                cached_head,
                cached_head + 6,
            ),
            fact(
                90,
                Some(31),
                SyntaxFactKind::Occurrence,
                READ,
                first_body,
                first_body + 4,
            ),
            fact(
                91,
                Some(51),
                SyntaxFactKind::Occurrence,
                READ,
                cached_body,
                cached_body + 4,
            ),
            fact(
                92,
                Some(51),
                SyntaxFactKind::Occurrence,
                READ,
                cached_body + 5,
                cached_body + 10,
            ),
            fact(
                93,
                Some(71),
                SyntaxFactKind::Occurrence,
                READ,
                second_body,
                second_body + 4,
            ),
            fact(
                94,
                Some(60),
                SyntaxFactKind::Occurrence,
                READ,
                continuation,
                continuation + 6,
            ),
            fact(
                95,
                Some(60),
                SyntaxFactKind::Occurrence,
                READ,
                continuation + 7,
                SOURCE.len(),
            ),
            fact(
                100,
                Some(51),
                SyntaxFactKind::Occurrence,
                super::super::lexical::CALL,
                cached_body,
                cached_body + 10,
            ),
            fact(
                101,
                Some(60),
                SyntaxFactKind::Occurrence,
                super::super::lexical::CALL,
                continuation,
                SOURCE.len(),
            ),
            fact(
                110,
                Some(51),
                SyntaxFactKind::Signature,
                "fsharp.application_argument.expression",
                cached_body + 5,
                cached_body + 10,
            ),
            fact(
                111,
                Some(60),
                SyntaxFactKind::Signature,
                "fsharp.application_argument.expression",
                continuation + 7,
                SOURCE.len(),
            ),
        ]);
        let symbols = [15u64, 16, 33, 55, 56, 73]
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                (
                    id,
                    SymbolId::from_bytes([u8::try_from(index + 1).unwrap(); 20]),
                )
            })
            .collect();
        (facts, symbols)
    }

    fn read(facts: &[SyntaxFact], id: u64) -> &SyntaxFact {
        facts.iter().find(|fact| fact.local_id() == id).unwrap()
    }

    #[test]
    fn local_names_shadow_outer_formals_only_after_initializer_and_freeze_captures() {
        let (facts, symbols) = fixture();
        let cancel = Cancellation::new();
        let plan = FSharpBindings::new(&facts, SOURCE.as_bytes(), &symbols, 128, &cancel).unwrap();
        for (id, target) in [(90, 15), (91, 33), (92, 55), (93, 33), (94, 56), (95, 73)] {
            let fact = read(&facts, id);
            let name = source_name(fact, SOURCE.as_bytes(), 128).unwrap().unwrap();
            assert_eq!(
                plan.resolve(fact, name, &cancel).unwrap(),
                Some(symbols[&target]),
                "read {id}"
            );
        }
        assert_eq!(plan.resolve_call(read(&facts, 100), &cancel).unwrap(), None);
        assert_eq!(
            plan.resolve_call(read(&facts, 101), &cancel).unwrap(),
            Some(symbols[&56])
        );
    }

    #[test]
    fn missing_duplicate_and_foreign_local_proofs_close_captures_not_own_formals() {
        for label in [WRAPPER, GROUP, CONTINUATION] {
            for mode in 0..3 {
                let (mut facts, symbols) = fixture();
                let index = facts
                    .iter()
                    .position(|fact| {
                        fact.parent() == Some(40) && fact.syntax_kind().as_str() == label
                    })
                    .unwrap();
                let original = facts[index].clone();
                match mode {
                    0 => {
                        facts.remove(index);
                    }
                    1 => facts.push(fact(
                        200,
                        original.parent(),
                        original.kind(),
                        label,
                        usize::try_from(original.span().start_byte()).unwrap(),
                        usize::try_from(original.span().end_byte()).unwrap(),
                    )),
                    _ => {
                        facts[index] = fact(
                            original.local_id(),
                            Some(20),
                            original.kind(),
                            label,
                            usize::try_from(original.span().start_byte()).unwrap(),
                            usize::try_from(original.span().end_byte()).unwrap(),
                        )
                    }
                }
                let cancel = Cancellation::new();
                let plan =
                    FSharpBindings::new(&facts, SOURCE.as_bytes(), &symbols, 128, &cancel).unwrap();
                assert_eq!(
                    plan.resolve(read(&facts, 91), "item", &cancel).unwrap(),
                    None,
                    "{label}, {mode}"
                );
                assert_eq!(
                    plan.resolve(read(&facts, 92), "extra", &cancel).unwrap(),
                    Some(symbols[&55]),
                    "{label}, {mode}"
                );
                assert_eq!(
                    plan.resolve_call(read(&facts, 101), &cancel).unwrap(),
                    None,
                    "{label}, {mode}"
                );
            }
        }
    }

    #[test]
    fn unavailable_local_symbol_remains_closed_against_outer_homonym() {
        let (facts, mut symbols) = fixture();
        symbols.remove(&33);
        let cancel = Cancellation::new();
        let plan = FSharpBindings::new(&facts, SOURCE.as_bytes(), &symbols, 128, &cancel).unwrap();
        assert_eq!(
            plan.resolve(read(&facts, 90), "item", &cancel).unwrap(),
            Some(symbols[&15])
        );
        for id in [91, 93] {
            assert_eq!(
                plan.resolve(read(&facts, id), "item", &cancel).unwrap(),
                None
            );
        }
        assert_eq!(
            plan.resolve(read(&facts, 95), "item", &cancel).unwrap(),
            Some(symbols[&73])
        );
    }

    #[test]
    fn duplicate_local_scopes_and_unowned_reads_are_not_transparent() {
        let (mut facts, symbols) = fixture();
        let scope = read(&facts, 40).clone();
        facts.push(fact(
            200,
            scope.parent(),
            scope.kind(),
            LOCAL_SCOPE,
            usize::try_from(scope.span().start_byte()).unwrap(),
            usize::try_from(scope.span().end_byte()).unwrap(),
        ));
        let cancel = Cancellation::new();
        let plan = FSharpBindings::new(&facts, SOURCE.as_bytes(), &symbols, 128, &cancel).unwrap();
        assert_eq!(
            plan.resolve(read(&facts, 91), "item", &cancel).unwrap(),
            None
        );
        assert_eq!(plan.resolve_call(read(&facts, 101), &cancel).unwrap(), None);
        let (mut facts, symbols) = fixture();
        let index = facts.iter().position(|fact| fact.local_id() == 91).unwrap();
        let original = facts[index].clone();
        facts[index] = fact(
            91,
            Some(11),
            original.kind(),
            READ,
            usize::try_from(original.span().start_byte()).unwrap(),
            usize::try_from(original.span().end_byte()).unwrap(),
        );
        let plan = FSharpBindings::new(&facts, SOURCE.as_bytes(), &symbols, 128, &cancel).unwrap();
        assert_eq!(
            plan.resolve(read(&facts, 91), "item", &cancel).unwrap(),
            None
        );
    }

    #[test]
    fn local_plan_is_order_independent_and_returns_typed_coordinate_and_cancel_errors() {
        let (mut facts, symbols) = fixture();
        facts.reverse();
        let cancel = Cancellation::new();
        let plan = FSharpBindings::new(&facts, SOURCE.as_bytes(), &symbols, 128, &cancel).unwrap();
        assert_eq!(
            plan.resolve_call(read(&facts, 101), &cancel).unwrap(),
            Some(symbols[&56])
        );
        let index = facts.iter().position(|fact| fact.local_id() == 43).unwrap();
        let original = facts[index].clone();
        facts[index] = fact(
            43,
            original.parent(),
            original.kind(),
            CONTINUATION,
            usize::try_from(original.span().start_byte()).unwrap(),
            SOURCE.len() + 1,
        );
        assert!(FSharpBindings::new(&facts, SOURCE.as_bytes(), &symbols, 128, &cancel).is_err());
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            FSharpBindings::new(&facts, SOURCE.as_bytes(), &symbols, 128, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    const LAMBDA_SOURCE: &str = "let outer item input = fun item extra -> item + extra + input";

    fn lambda_fixture() -> (Vec<SyntaxFact>, HashMap<u64, SymbolId>) {
        let start = LAMBDA_SOURCE.find("fun").unwrap();
        let arrow_end = LAMBDA_SOURCE.find("->").unwrap() + 2;
        let body = arrow_end + 1;
        let header_end = LAMBDA_SOURCE.find('=').unwrap();
        let outer_item = LAMBDA_SOURCE.find("item").unwrap();
        let outer_input = LAMBDA_SOURCE.find("input").unwrap();
        let item = start + 4;
        let extra = item + 5;
        let end = LAMBDA_SOURCE.len();
        let mut facts = vec![
            fact(1, None, SyntaxFactKind::Scope, "fsharp.file.scope", 0, end),
            fact(10, Some(1), SyntaxFactKind::Declaration, FUNCTION, 4, end),
            fact(11, Some(10), SyntaxFactKind::Scope, SCOPE, 4, end),
            fact(
                12,
                Some(11),
                SyntaxFactKind::Signature,
                HEADER,
                4,
                header_end,
            ),
            fact(
                13,
                Some(11),
                SyntaxFactKind::Signature,
                PARAMETERS,
                outer_item,
                outer_input + 5,
            ),
            fact(16, Some(11), SyntaxFactKind::Occurrence, DEFINITION, 4, 9),
            fact(
                17,
                Some(10),
                SyntaxFactKind::Signature,
                "fsharp.nonrecursive_binding.expression",
                4,
                end,
            ),
            fact(
                20,
                Some(11),
                SyntaxFactKind::Scope,
                LAMBDA_SCOPE,
                start,
                end,
            ),
            fact(
                21,
                Some(20),
                SyntaxFactKind::Signature,
                LAMBDA_HEADER,
                start,
                arrow_end,
            ),
            fact(
                22,
                Some(20),
                SyntaxFactKind::Signature,
                PARAMETERS,
                item,
                extra + 5,
            ),
            fact(
                23,
                Some(20),
                SyntaxFactKind::Signature,
                LAMBDA_BODY,
                body,
                end,
            ),
            fact(
                30,
                Some(20),
                SyntaxFactKind::Occurrence,
                READ,
                body,
                body + 4,
            ),
            fact(
                31,
                Some(20),
                SyntaxFactKind::Occurrence,
                READ,
                body + 7,
                body + 12,
            ),
            fact(32, Some(20), SyntaxFactKind::Occurrence, READ, end - 5, end),
        ];
        for (id, scope, position, length) in [
            (14, 11, outer_item, 4),
            (18, 11, outer_input, 5),
            (24, 20, item, 4),
            (27, 20, extra, 5),
        ] {
            facts.extend([
                fact(
                    id,
                    Some(scope),
                    SyntaxFactKind::Declaration,
                    PARAMETER,
                    position,
                    position + length,
                ),
                fact(
                    id + 1,
                    Some(id),
                    SyntaxFactKind::Occurrence,
                    DEFINITION,
                    position,
                    position + length,
                ),
            ]);
            if scope == 20 {
                facts.push(fact(
                    id + 2,
                    Some(id),
                    SyntaxFactKind::Signature,
                    PARAMETER_SIGNATURE,
                    position,
                    position + length,
                ));
            }
        }
        let symbols = [15u64, 16, 19, 25, 28]
            .into_iter()
            .enumerate()
            .map(|(index, id)| {
                (
                    id,
                    SymbolId::from_bytes([u8::try_from(index + 1).unwrap(); 20]),
                )
            })
            .collect();
        (facts, symbols)
    }

    fn assert_lambda_targets(
        facts: &[SyntaxFact],
        symbols: &HashMap<u64, SymbolId>,
        expected: [Option<u64>; 3],
    ) {
        let cancel = Cancellation::new();
        let plan =
            FSharpBindings::new(facts, LAMBDA_SOURCE.as_bytes(), symbols, 128, &cancel).unwrap();
        for (id, target) in [30, 31, 32].into_iter().zip(expected) {
            let occurrence = read(facts, id);
            let name = source_name(occurrence, LAMBDA_SOURCE.as_bytes(), 128)
                .unwrap()
                .unwrap();
            assert_eq!(
                plan.resolve(occurrence, name, &cancel).unwrap(),
                target.map(|id| symbols[&id]),
                "read {id}"
            );
        }
    }

    #[test]
    fn lambda_only_plan_keeps_exact_formals_outer_captures_and_authored_signatures() {
        let (mut facts, symbols) = lambda_fixture();
        assert!(
            !facts
                .iter()
                .any(|fact| fact.syntax_kind().as_str() == LOCAL_SCOPE)
        );
        for _ in 0..2 {
            assert_lambda_targets(&facts, &symbols, [Some(25), Some(28), Some(19)]);
            let signatures = super::super::FSharpParameterSignatures::new(
                &facts,
                LAMBDA_SOURCE.as_bytes(),
                128,
                &Cancellation::new(),
            )
            .unwrap();
            for (declaration, header) in [(24, 26), (27, 29)] {
                let signature = signatures.signature_for_declaration(declaration).unwrap();
                assert_eq!(signature, read(&facts, header));
                assert_eq!(signature.span(), read(&facts, declaration).span());
            }
            facts.reverse();
        }
    }

    #[test]
    fn atomic_lambda_slot_union_preserves_typed_shadowing_and_rejects_witness_loss() {
        let (original, symbols) = lambda_fixture();
        let source = LAMBDA_SOURCE.replacen("fun item extra", "fun (item: int) extra () ()", 1);
        let parameter_start = source.find("(item: int)").unwrap();
        let extra = source.find("extra").unwrap();
        let arrow = source.find("->").unwrap();
        let unit = extra + "extra ".len();
        let delta = source.len() - LAMBDA_SOURCE.len();
        let old_end = LAMBDA_SOURCE.find("extra").unwrap() + "extra".len();
        let shifted = |position| {
            if position >= old_end {
                position + delta
            } else {
                position
            }
        };
        let mut facts = Vec::new();
        for old in original {
            let (start, end, label) = match old.local_id() {
                22 => (parameter_start, arrow - 1, UNIT_PARAMETERS),
                24 | 25 => (
                    parameter_start + 1,
                    parameter_start + 5,
                    old.syntax_kind().as_str(),
                ),
                26 => (
                    parameter_start,
                    parameter_start + "(item: int)".len(),
                    PARAMETER_SIGNATURE,
                ),
                27..=29 => (extra, extra + "extra".len(), old.syntax_kind().as_str()),
                _ => (
                    shifted(usize::try_from(old.span().start_byte()).unwrap()),
                    shifted(usize::try_from(old.span().end_byte()).unwrap()),
                    old.syntax_kind().as_str(),
                ),
            };
            facts.push(fact(
                old.local_id(),
                old.parent(),
                old.kind(),
                label,
                start,
                end,
            ));
        }
        facts.extend([
            fact(
                40,
                Some(20),
                SyntaxFactKind::Signature,
                UNIT_PARAMETER,
                unit,
                unit + 2,
            ),
            fact(
                41,
                Some(20),
                SyntaxFactKind::Signature,
                UNIT_PARAMETER,
                unit + 3,
                unit + 5,
            ),
        ]);
        let assert_targets = |facts: &[SyntaxFact], expected: [Option<u64>; 3]| {
            let cancellation = Cancellation::new();
            let plan = FSharpBindings::new(facts, source.as_bytes(), &symbols, 128, &cancellation)
                .unwrap();
            for (id, target) in [30, 31, 32].into_iter().zip(expected) {
                let occurrence = read(facts, id);
                let name = source_name(occurrence, source.as_bytes(), 128)
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    plan.resolve(occurrence, name, &cancellation).unwrap(),
                    target.map(|id| symbols[&id]),
                    "read {id}"
                );
            }
        };
        for _ in 0..2 {
            assert_targets(&facts, [Some(25), Some(28), Some(19)]);
            let signatures = super::super::FSharpParameterSignatures::new(
                &facts,
                source.as_bytes(),
                128,
                &Cancellation::new(),
            )
            .unwrap();
            assert_eq!(
                signatures.signature_for_declaration(24),
                Some(read(&facts, 26))
            );
            assert_ne!(read(&facts, 24).span(), read(&facts, 26).span());
            facts.reverse();
        }
        for id in [22, 25, 26, 29, 40, 41] {
            for mode in 0..4 {
                let mut changed = facts.clone();
                let index = changed
                    .iter()
                    .position(|fact| fact.local_id() == id)
                    .unwrap();
                let old = changed[index].clone();
                match mode {
                    0 => {
                        changed.remove(index);
                    }
                    1 => changed.push(fact(
                        200,
                        old.parent(),
                        old.kind(),
                        old.syntax_kind().as_str(),
                        usize::try_from(old.span().start_byte()).unwrap(),
                        usize::try_from(old.span().end_byte()).unwrap(),
                    )),
                    2 => {
                        changed[index] = fact(
                            id,
                            Some(11),
                            old.kind(),
                            old.syntax_kind().as_str(),
                            usize::try_from(old.span().start_byte()).unwrap(),
                            usize::try_from(old.span().end_byte()).unwrap(),
                        )
                    }
                    _ => {
                        changed[index] = fact(
                            id,
                            old.parent(),
                            SyntaxFactKind::Scope,
                            old.syntax_kind().as_str(),
                            usize::try_from(old.span().start_byte()).unwrap(),
                            usize::try_from(old.span().end_byte()).unwrap(),
                        )
                    }
                }
                assert_targets(&changed, [None; 3]);
            }
        }
        let mut missing_formal = facts.clone();
        missing_formal.retain(|fact| ![24, 25, 26].contains(&fact.local_id()));
        assert_targets(&missing_formal, [None; 3]);
        let mut broad = facts.clone();
        let index = broad.iter().position(|fact| fact.local_id() == 26).unwrap();
        broad[index] = fact(
            26,
            Some(20),
            SyntaxFactKind::Signature,
            PARAMETER_SIGNATURE,
            parameter_start,
            extra + "extra".len(),
        );
        broad.retain(|fact| fact.local_id() != 28);
        assert_targets(&broad, [None; 3]);
        let mut conflict = facts.clone();
        conflict.push(fact(
            42,
            Some(20),
            SyntaxFactKind::Signature,
            PARAMETERS,
            parameter_start,
            arrow - 1,
        ));
        assert_targets(&conflict, [None; 3]);
    }

    #[test]
    fn lambda_missing_duplicate_and_foreign_witnesses_close_all_lookup() {
        for id in [21, 22, 23, 26, 29] {
            for mode in 0..3 {
                let (mut facts, symbols) = lambda_fixture();
                let index = facts.iter().position(|fact| fact.local_id() == id).unwrap();
                let original = facts[index].clone();
                let replacement = |id, parent| {
                    fact(
                        id,
                        parent,
                        original.kind(),
                        original.syntax_kind().as_str(),
                        usize::try_from(original.span().start_byte()).unwrap(),
                        usize::try_from(original.span().end_byte()).unwrap(),
                    )
                };
                match mode {
                    0 => {
                        facts.remove(index);
                    }
                    1 => facts.push(replacement(200, original.parent())),
                    _ => facts[index] = replacement(id, Some(11)),
                }
                assert_lambda_targets(&facts, &symbols, [None; 3]);
                let signatures = super::super::FSharpParameterSignatures::new(
                    &facts,
                    LAMBDA_SOURCE.as_bytes(),
                    128,
                    &Cancellation::new(),
                )
                .unwrap();
                assert!(
                    signatures.signature_for_declaration(24).is_none(),
                    "id {id}, mode {mode}"
                );
                assert!(
                    signatures.signature_for_declaration(27).is_none(),
                    "id {id}, mode {mode}"
                );
            }
        }
    }

    #[test]
    fn lambda_complete_list_conservation_rejects_missing_whole_formal_and_clipped_list() {
        for clip_list in [false, true] {
            let (mut facts, symbols) = lambda_fixture();
            facts.retain(|fact| ![24, 25, 26].contains(&fact.local_id()));
            if clip_list {
                let index = facts.iter().position(|fact| fact.local_id() == 22).unwrap();
                let extra = LAMBDA_SOURCE.find("extra").unwrap();
                facts[index] = fact(
                    22,
                    Some(20),
                    SyntaxFactKind::Signature,
                    PARAMETERS,
                    extra,
                    extra + 5,
                );
            }
            assert_lambda_targets(&facts, &symbols, [None; 3]);
        }
        let (facts, mut symbols) = lambda_fixture();
        symbols.remove(&25);
        assert_lambda_targets(&facts, &symbols, [None; 3]);
    }

    #[test]
    fn lambda_duplicate_scopes_overlap_and_unreviewed_parent_close_outer_capture() {
        let (mut facts, symbols) = lambda_fixture();
        let scope = read(&facts, 20).clone();
        facts.push(fact(
            200,
            scope.parent(),
            scope.kind(),
            LAMBDA_SCOPE,
            usize::try_from(scope.span().start_byte()).unwrap(),
            LAMBDA_SOURCE.len(),
        ));
        assert_lambda_targets(&facts, &symbols, [None; 3]);
        let (mut facts, symbols) = lambda_fixture();
        let index = facts.iter().position(|fact| fact.local_id() == 27).unwrap();
        let item = usize::try_from(read(&facts, 24).span().start_byte()).unwrap();
        facts[index] = fact(
            27,
            Some(20),
            SyntaxFactKind::Declaration,
            PARAMETER,
            item + 1,
            item + 4,
        );
        assert_lambda_targets(&facts, &symbols, [None; 3]);
        let (mut facts, symbols) = lambda_fixture();
        facts.retain(|fact| fact.local_id() != 17);
        assert_lambda_targets(&facts, &symbols, [Some(25), Some(28), None]);
        for parent in [None, Some(1), Some(10)] {
            let (mut facts, symbols) = lambda_fixture();
            let index = facts.iter().position(|fact| fact.local_id() == 20).unwrap();
            let start = usize::try_from(facts[index].span().start_byte()).unwrap();
            facts[index] = fact(
                20,
                parent,
                SyntaxFactKind::Scope,
                LAMBDA_SCOPE,
                start,
                LAMBDA_SOURCE.len(),
            );
            assert_lambda_targets(&facts, &symbols, [Some(25), Some(28), None]);
        }
    }

    #[test]
    fn lambda_invalid_coordinates_and_cancellation_return_typed_errors() {
        let (mut facts, symbols) = lambda_fixture();
        let index = facts.iter().position(|fact| fact.local_id() == 23).unwrap();
        let body = usize::try_from(facts[index].span().start_byte()).unwrap();
        facts[index] = fact(
            23,
            Some(20),
            SyntaxFactKind::Signature,
            LAMBDA_BODY,
            body,
            LAMBDA_SOURCE.len() + 1,
        );
        let cancel = Cancellation::new();
        assert!(
            FSharpBindings::new(&facts, LAMBDA_SOURCE.as_bytes(), &symbols, 128, &cancel).is_err()
        );
        cancel.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            FSharpBindings::new(&facts, LAMBDA_SOURCE.as_bytes(), &symbols, 128, &cancel),
            Err(AdapterError::Cancelled { .. })
        ));
        assert!(matches!(
            super::super::FSharpParameterSignatures::new(
                &facts,
                LAMBDA_SOURCE.as_bytes(),
                128,
                &cancel
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
