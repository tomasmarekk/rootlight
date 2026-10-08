//! Source-ordered bindings, owned-root absence and native application witnesses.

use super::{
    DEFINITION, FSharpBindings, FUNCTION, HEADER, READ, SCOPE, UniqueFact, allocation_failed,
    contains, invalid_capture, source_bytes, source_name, unqualified_written_name,
};
use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::{FileId, SymbolId};

const VARIABLE: &str = "fsharp.variable.declaration";
const VARIABLE_SCOPE: &str = "fsharp.variable.scope";
const VARIABLE_HEADER: &str = "fsharp.variable.signature";
const NONRECURSIVE: &str = "fsharp.nonrecursive_binding.expression";
pub(super) const CALL: &str = "fsharp.application.call";
const ARGUMENT: &str = "fsharp.application_argument.expression";

struct Owner<'a> {
    declaration: &'a SyntaxFact,
    scope: UniqueFact<'a>,
    header: UniqueFact<'a>,
    marker: UniqueFact<'a>,
    definition: UniqueFact<'a>,
}

impl Owner<'_> {
    fn valid(&self) -> bool {
        let Some(marker) = self.marker.one() else {
            return false;
        };
        self.valid_shape()
            && marker.kind() == SyntaxFactKind::Signature
            && marker.parent() == Some(self.declaration.local_id())
            && marker.span() == self.declaration.span()
    }

    fn valid_shape(&self) -> bool {
        let (Some(scope), Some(header)) = (self.scope.one(), self.header.one()) else {
            return false;
        };
        scope.kind() == SyntaxFactKind::Scope
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
struct Binding {
    start: u64,
    end: u64,
    symbol: Option<SymbolId>,
}

#[derive(Clone, Copy)]
struct Body {
    module: u64,
    declaration_start: u64,
    header_end: u64,
    end: u64,
    file: FileId,
    recursive_group: Option<u64>,
}

#[derive(Clone, Copy)]
struct Application {
    read: u64,
    arguments: usize,
    argument_end: Option<u64>,
    valid: bool,
}

/// Positive native witnesses do not license cross-module or speculative lookup.
pub(super) struct LexicalBindings<'a> {
    bodies: HashMap<u64, Body>,
    bindings: HashMap<(u64, &'a str), Vec<Binding>>,
    root_bindings: HashMap<(u64, &'a str), Vec<Binding>>,
    recursive_roots: HashSet<(u64, &'a str)>,
    barriers: HashMap<u64, Vec<u64>>,
    qualified_barriers: HashMap<u64, Vec<u64>>,
    module_parents: HashMap<u64, Option<u64>>,
    imported_bodies: HashSet<u64>,
    arities: HashMap<SymbolId, Option<usize>>,
    exports: HashMap<u64, (SymbolId, Option<usize>, &'a SyntaxFact)>,
    applications: HashMap<u64, Application>,
    recursive: super::recursive::RecursiveBindings<'a>,
}

impl<'a> LexicalBindings<'a> {
    pub(super) fn new(
        plan: &FSharpBindings<'a>,
        symbols: &HashMap<u64, SymbolId>,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            bodies: HashMap::new(),
            bindings: HashMap::new(),
            root_bindings: HashMap::new(),
            recursive_roots: HashSet::new(),
            barriers: HashMap::new(),
            qualified_barriers: HashMap::new(),
            module_parents: HashMap::new(),
            imported_bodies: HashSet::new(),
            arities: HashMap::new(),
            exports: HashMap::new(),
            applications: HashMap::new(),
            recursive: super::recursive::RecursiveBindings::default(),
        };
        let mut open_intervals = Vec::new();
        open_intervals
            .try_reserve_exact(plan.qualified_opens.len())
            .map_err(allocation_failed)?;
        for open in &plan.qualified_opens {
            cancellation.check()?;
            open_intervals.push((
                open.fact.span().file(),
                open.fact.span().end_byte(),
                open.owner.end_byte(),
            ));
        }
        let mut owners = HashMap::new();
        owners
            .try_reserve(plan.facts.len())
            .map_err(allocation_failed)?;
        for fact in plan.facts.values().copied() {
            cancellation.check()?;
            if module_scope(fact) {
                if fact.syntax_kind().as_str() == "fsharp.file.scope"
                    && qualified_file_scope(fact, plan, cancellation)?
                {
                    result
                        .module_parents
                        .try_reserve(1)
                        .map_err(allocation_failed)?;
                    result.module_parents.insert(fact.local_id(), None);
                } else if let Some(declaration) =
                    fact.parent().and_then(|id| plan.facts.get(&id).copied())
                    && declaration.kind() == SyntaxFactKind::Declaration
                    && matches!(
                        declaration.syntax_kind().as_str(),
                        "fsharp.module.declaration"
                            | "fsharp.namespace.declaration"
                            | "fsharp.nested_module.declaration"
                    )
                    && declaration.span() == fact.span()
                    && let Some(parent) = declaration
                        .parent()
                        .and_then(|id| plan.facts.get(&id).copied())
                    && module_scope(parent)
                    && contains(parent, fact)
                {
                    result
                        .module_parents
                        .try_reserve(1)
                        .map_err(allocation_failed)?;
                    result
                        .module_parents
                        .insert(fact.local_id(), Some(parent.local_id()));
                }
            }
            if fact.kind() == SyntaxFactKind::Declaration
                && matches!(fact.syntax_kind().as_str(), FUNCTION | VARIABLE)
            {
                owners.insert(
                    fact.local_id(),
                    Owner {
                        declaration: fact,
                        scope: UniqueFact::default(),
                        header: UniqueFact::default(),
                        marker: UniqueFact::default(),
                        definition: UniqueFact::default(),
                    },
                );
            }
            if matches!(fact.syntax_kind().as_str(), NONRECURSIVE | CALL | ARGUMENT) {
                source_bytes(fact, plan.source)?;
            }
        }
        for fact in plan.facts.values().copied() {
            cancellation.check()?;
            let Some(parent) = fact.parent().and_then(|id| plan.facts.get(&id).copied()) else {
                continue;
            };
            let owner = if owners.contains_key(&parent.local_id()) {
                Some(parent.local_id())
            } else if matches!(parent.syntax_kind().as_str(), SCOPE | VARIABLE_SCOPE) {
                parent.parent()
            } else {
                None
            };
            let Some(owner) = owner.and_then(|id| owners.get_mut(&id)) else {
                continue;
            };
            let function = owner.declaration.syntax_kind().as_str() == FUNCTION;
            match fact.syntax_kind().as_str() {
                SCOPE if function => owner.scope.retain(fact)?,
                VARIABLE_SCOPE if !function => owner.scope.retain(fact)?,
                HEADER if function => owner.header.retain(fact)?,
                VARIABLE_HEADER if !function => owner.header.retain(fact)?,
                NONRECURSIVE => owner.marker.retain(fact)?,
                label if label.ends_with(".definition") => owner.definition.retain(fact)?,
                _ => {}
            }
        }
        let mut formal_counts = HashMap::<u64, Option<usize>>::new();
        formal_counts
            .try_reserve(plan.functions.len())
            .map_err(allocation_failed)?;
        for fact in plan.facts.values().copied() {
            cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Declaration
                && fact.syntax_kind().as_str() == super::PARAMETER
                && let Some(scope) = fact.parent()
            {
                let name = source_name(fact, plan.source, plan.maximum_name_bytes)?;
                let available = name
                    .and_then(|name| plan.bindings.get(&(scope, name)))
                    .copied()
                    .flatten()
                    .is_some_and(|symbol| !plan.conflicting_symbols.contains(&symbol));
                let count = formal_counts.entry(scope).or_insert(Some(0));
                *count = if available {
                    count
                        .map(|count| count.checked_add(1).ok_or_else(invalid_capture))
                        .transpose()?
                } else {
                    None
                };
            }
        }
        result.recursive = super::recursive::RecursiveBindings::new(plan, symbols, cancellation)?;
        for fact in plan.facts.values().copied() {
            cancellation.check()?;
            if !matches!(
                fact.kind(),
                SyntaxFactKind::Declaration | SyntaxFactKind::Import
            ) && !(fact.kind() == SyntaxFactKind::Scope
                && matches!(
                    fact.syntax_kind().as_str(),
                    "fsharp.preprocessor.scope" | "fsharp.preprocessor_branch.scope"
                ))
            {
                continue;
            }
            let Some(module) = fact
                .parent()
                .and_then(|id| plan.facts.get(&id).copied())
                .filter(|parent| module_scope(parent) && contains(parent, fact))
            else {
                continue;
            };
            let owner = owners.get(&fact.local_id());
            let definition = owner
                .and_then(|owner| owner.definition.one())
                .filter(|definition| {
                    definition.kind() == SyntaxFactKind::Occurrence
                        && definition.syntax_kind().as_str() == DEFINITION
                        && (definition.parent() == Some(fact.local_id())
                            || owner
                                .and_then(|owner| owner.scope.one())
                                .is_some_and(|scope| definition.parent() == Some(scope.local_id())))
                        && owner
                            .and_then(|owner| owner.header.one())
                            .is_some_and(|header| contains(header, definition))
                });
            let name = definition
                .map(|definition| source_name(definition, plan.source, plan.maximum_name_bytes))
                .transpose()?
                .flatten()
                .filter(|name| {
                    definition.is_some_and(|definition| {
                        unqualified_written_name(definition, name, plan.maximum_name_bytes)
                    })
                });
            // Unknown patterns, imports and conditional arms may introduce
            // unenumerated names.
            // They close older lookup instead of exposing an earlier homonym.
            let Some(name) = name else {
                result.barriers.try_reserve(1).map_err(allocation_failed)?;
                let barriers = result.barriers.entry(module.local_id()).or_default();
                barriers.try_reserve(1).map_err(allocation_failed)?;
                barriers.push(fact.span().end_byte());
                // Reviewed types conserve every outer type and constructor root.
                // Ordinary lookup retains its barrier; only owned-root absence
                // can use the complete root proof retained by the caller.
                let reviewed_open = fact.kind() == SyntaxFactKind::Import
                    && plan.qualified_opens_complete
                    && fact.syntax_kind().as_str() == "fsharp.open.import";
                // Container names are checked by the complete qualified claim
                // plan; their children do not introduce parent module values.
                let reviewed_container = fact.kind() == SyntaxFactKind::Declaration
                    && matches!(
                        fact.syntax_kind().as_str(),
                        "fsharp.module.declaration"
                            | "fsharp.namespace.declaration"
                            | "fsharp.nested_module.declaration"
                    );
                if !reviewed_open
                    && !reviewed_container
                    && !plan
                        .qualified_types
                        .get(&fact.local_id())
                        .is_some_and(|record| record.name.is_some())
                {
                    result
                        .qualified_barriers
                        .try_reserve(1)
                        .map_err(allocation_failed)?;
                    let barriers = result
                        .qualified_barriers
                        .entry(module.local_id())
                        .or_default();
                    barriers.try_reserve(1).map_err(allocation_failed)?;
                    barriers.push(fact.span().end_byte());
                }
                continue;
            };
            let recursive_member = result.recursive.member(fact.local_id());
            if let Some(member) = recursive_member {
                result
                    .recursive_roots
                    .try_reserve(1)
                    .map_err(allocation_failed)?;
                result
                    .recursive_roots
                    .insert((member.group, super::canonical_identifier(name)));
            }
            let ordinary_valid = owner.is_some_and(Owner::valid)
                && recursive_member.is_none()
                && (fact.syntax_kind().as_str() == VARIABLE
                    || plan
                        .functions
                        .get(&fact.local_id())
                        .is_some_and(|function| {
                            function.valid()
                                && function.scope.one().is_some_and(|scope| {
                                    function
                                        .callable_arity(
                                            formal_counts.get(&scope.local_id()).copied(),
                                        )
                                        .is_some()
                                })
                        }));
            let recursive_valid = recursive_member.is_some()
                && owner.is_some_and(|owner| owner.marker.count == 0 && owner.valid_shape());
            let valid = ordinary_valid || recursive_valid;
            if !valid && let Some(member) = recursive_member {
                result.recursive.close(member.group, name);
            }
            let symbol = valid
                .then(|| {
                    definition.and_then(|definition| symbols.get(&definition.local_id()).copied())
                })
                .flatten();
            result.bindings.try_reserve(1).map_err(allocation_failed)?;
            let bindings = result
                .bindings
                .entry((module.local_id(), name))
                .or_default();
            bindings.try_reserve(1).map_err(allocation_failed)?;
            bindings.push(Binding {
                start: recursive_member.map_or(fact.span().start_byte(), |member| member.start),
                end: recursive_member.map_or(fact.span().end_byte(), |member| member.end),
                symbol,
            });
            result
                .root_bindings
                .try_reserve(1)
                .map_err(allocation_failed)?;
            let roots = result
                .root_bindings
                .entry((module.local_id(), super::canonical_identifier(name)))
                .or_default();
            roots.try_reserve(1).map_err(allocation_failed)?;
            roots.push(Binding {
                start: recursive_member.map_or(fact.span().start_byte(), |member| member.start),
                end: recursive_member.map_or(fact.span().end_byte(), |member| member.end),
                symbol,
            });
            // A proved recursive head can be visible even when its formal
            // pattern is not supported. Its body and Calls remain unavailable.
            if !valid
                || (recursive_member.is_some()
                    && !plan
                        .functions
                        .get(&fact.local_id())
                        .is_some_and(|function| function.valid()))
            {
                continue;
            }
            let Some(owner) = owner else {
                continue;
            };
            let Some(scope) = owner.scope.one() else {
                continue;
            };
            let Some(header) = owner.header.one() else {
                continue;
            };
            result.bodies.try_reserve(1).map_err(allocation_failed)?;
            result.bodies.insert(
                scope.local_id(),
                Body {
                    module: module.local_id(),
                    declaration_start: recursive_member
                        .map_or(fact.span().start_byte(), |member| member.start),
                    header_end: header.span().end_byte(),
                    end: fact.span().end_byte(),
                    file: fact.span().file(),
                    recursive_group: recursive_member.map(|member| member.group),
                },
            );
            if fact.syntax_kind().as_str() == FUNCTION
                && let Some(symbol) = symbol
            {
                let arity = plan.functions.get(&fact.local_id()).and_then(|function| {
                    function.callable_arity(formal_counts.get(&scope.local_id()).copied())
                });
                result.arities.try_reserve(1).map_err(allocation_failed)?;
                result
                    .arities
                    .entry(symbol)
                    .and_modify(|arity| *arity = None)
                    .or_insert(arity);
            }
            if let Some(symbol) = symbol.filter(|symbol| !plan.conflicting_symbols.contains(symbol))
            {
                let arity = (fact.syntax_kind().as_str() == FUNCTION)
                    .then(|| {
                        plan.functions.get(&fact.local_id()).and_then(|function| {
                            function.callable_arity(formal_counts.get(&scope.local_id()).copied())
                        })
                    })
                    .flatten();
                result.exports.try_reserve(1).map_err(allocation_failed)?;
                if let Some(definition) = definition {
                    result
                        .exports
                        .insert(fact.local_id(), (symbol, arity, definition));
                }
            }
        }
        for bindings in result.bindings.values_mut() {
            crate::runtime::sort_cancellable_by(bindings, cancellation, |left, right| {
                (left.end, left.start).cmp(&(right.end, right.start))
            })?;
        }
        for barriers in result.barriers.values_mut() {
            crate::runtime::sort_cancellable_by(barriers, cancellation, u64::cmp)?;
        }
        for barriers in result.qualified_barriers.values_mut() {
            crate::runtime::sort_cancellable_by(barriers, cancellation, u64::cmp)?;
        }
        for bindings in result.root_bindings.values_mut() {
            crate::runtime::sort_cancellable_by(bindings, cancellation, |left, right| {
                (left.end, left.start).cmp(&(right.end, right.start))
            })?;
        }
        let mut body_intervals = Vec::new();
        body_intervals
            .try_reserve_exact(result.bodies.len())
            .map_err(allocation_failed)?;
        for (&scope, body) in &result.bodies {
            cancellation.check()?;
            body_intervals.push((body.file, body.declaration_start, body.end, scope));
        }
        crate::runtime::sort_cancellable_by(&mut open_intervals, cancellation, Ord::cmp)?;
        crate::runtime::sort_cancellable_by(&mut body_intervals, cancellation, Ord::cmp)?;
        let mut next = 0usize;
        let mut file = None;
        let mut furthest_end = 0u64;
        result
            .imported_bodies
            .try_reserve(result.bodies.len())
            .map_err(allocation_failed)?;
        // An open begins after its clause and ends with its owning container.
        // The interval sweep respects ancestors without importing sibling scope
        // or rescanning every directive for each read.
        for (body_file, start, end, scope) in body_intervals {
            cancellation.check()?;
            if file != Some(body_file) {
                file = Some(body_file);
                furthest_end = 0;
            }
            while let Some(&(open_file, begin, finish)) = open_intervals.get(next) {
                cancellation.check()?;
                if open_file > body_file || open_file == body_file && begin > start {
                    break;
                }
                if open_file == body_file {
                    furthest_end = furthest_end.max(finish);
                }
                next = next.checked_add(1).ok_or_else(invalid_capture)?;
            }
            if furthest_end >= end {
                result.imported_bodies.insert(scope);
            }
        }
        result.collect_applications(plan, cancellation)?;
        Ok(result)
    }

    pub(super) fn resolve(&self, read: &SyntaxFact, name: &str) -> Option<SymbolId> {
        self.resolve_in(read.parent()?, read, name)
    }

    pub(super) fn resolve_in(&self, scope: u64, read: &SyntaxFact, name: &str) -> Option<SymbolId> {
        let body = self.bodies.get(&scope)?;
        if read.span().file() != body.file
            || read.span().start_byte() < body.header_end
            || read.span().end_byte() > body.end
        {
            return None;
        }
        if let Some(group) = body.recursive_group
            && let Some(symbol) = self.recursive.resolve(group, name)
        {
            return symbol;
        }
        let bindings = self.bindings.get(&(body.module, name))?;
        let index = bindings.partition_point(|binding| binding.end <= body.declaration_start);
        let binding = bindings.get(index.checked_sub(1)?)?;
        if index
            .checked_sub(2)
            .and_then(|index| bindings.get(index))
            .is_some_and(|previous| previous.end == binding.end)
            || self.barriers.get(&body.module).is_some_and(|barriers| {
                let index = barriers.partition_point(|end| *end <= body.declaration_start);
                index > 0 && barriers[index - 1] > binding.start
            })
        {
            return None;
        }
        binding.symbol
    }

    pub(super) fn body_ranges(&self) -> impl Iterator<Item = (u64, FileId, u64, u64)> + '_ {
        self.bodies
            .iter()
            .map(|(&scope, body)| (scope, body.file, body.header_end, body.end))
    }

    pub(super) fn callee(&self, call: &SyntaxFact) -> Option<u64> {
        self.applications
            .get(&call.local_id())
            .filter(|application| application.valid)
            .map(|application| application.read)
    }

    pub(super) fn export(
        &self,
        declaration: u64,
    ) -> Option<(SymbolId, Option<usize>, &'a SyntaxFact)> {
        self.exports.get(&declaration).copied()
    }

    pub(super) fn recursive_member(&self, declaration: u64) -> Option<super::recursive::Member> {
        self.recursive.member(declaration)
    }

    pub(super) fn qualified_root_in(
        &self,
        scope: u64,
        read: &SyntaxFact,
        name: &str,
    ) -> super::QualifiedRoot {
        let Some(body) = self.bodies.get(&scope) else {
            return super::QualifiedRoot::Closed;
        };
        if read.span().file() != body.file
            || read.span().start_byte() < body.header_end
            || read.span().end_byte() > body.end
        {
            return super::QualifiedRoot::Closed;
        }
        if let Some(group) = body.recursive_group
            && self
                .recursive_roots
                .contains(&(group, super::canonical_identifier(name)))
        {
            return super::QualifiedRoot::Bound;
        }
        // Enclosing module values remain visible inside nested modules. Import
        // deferral cannot turn a nearer authored value into a module target.
        let mut module = body.module;
        let mut complete = false;
        for _ in 0..=rootlight_ir::MAX_FSHARP_BINDING_COMPONENTS {
            if self
                .qualified_barriers
                .get(&module)
                .is_some_and(|barriers| {
                    barriers.partition_point(|end| *end <= body.declaration_start) > 0
                })
            {
                return super::QualifiedRoot::Closed;
            }
            if self
                .root_bindings
                .get(&(module, super::canonical_identifier(name)))
                .is_some_and(|bindings| {
                    bindings.partition_point(|binding| binding.end <= body.declaration_start) > 0
                })
            {
                return super::QualifiedRoot::Bound;
            }
            match self.module_parents.get(&module) {
                Some(Some(parent)) => module = *parent,
                Some(None) => {
                    complete = true;
                    break;
                }
                None => return super::QualifiedRoot::Closed,
            }
        }
        if !complete {
            return super::QualifiedRoot::Closed;
        }
        if self.imported_bodies.contains(&scope) {
            super::QualifiedRoot::ImportsPending
        } else {
            super::QualifiedRoot::ProvenAbsent
        }
    }

    pub(super) fn argument_slots(&self, call: &SyntaxFact) -> Option<usize> {
        self.applications
            .get(&call.local_id())
            .filter(|application| application.valid)
            .map(|application| application.arguments)
    }

    pub(super) fn saturated(&self, call: &SyntaxFact, symbol: SymbolId) -> bool {
        self.arities
            .get(&symbol)
            .copied()
            .flatten()
            .is_some_and(|arity| self.saturated_with_arity(call, arity))
    }

    pub(super) fn saturated_with_arity(&self, call: &SyntaxFact, arity: usize) -> bool {
        self.applications
            .get(&call.local_id())
            .is_some_and(|application| application.valid && arity == application.arguments)
    }

    fn collect_applications(
        &mut self,
        plan: &FSharpBindings<'a>,
        cancellation: &Cancellation,
    ) -> Result<(), AdapterError> {
        let mut reads = HashMap::<(Option<u64>, FileId, u64), Option<u64>>::new();
        reads
            .try_reserve(plan.facts.len())
            .map_err(allocation_failed)?;
        let mut events = Vec::new();
        events
            .try_reserve(plan.facts.len())
            .map_err(allocation_failed)?;
        for fact in plan.facts.values().copied() {
            cancellation.check()?;
            if fact.kind() == SyntaxFactKind::Occurrence
                && matches!(fact.syntax_kind().as_str(), READ | super::QUALIFIED_READ)
            {
                reads
                    .entry((fact.parent(), fact.span().file(), fact.span().start_byte()))
                    .and_modify(|read| *read = None)
                    .or_insert(Some(fact.local_id()));
            } else if (fact.kind() == SyntaxFactKind::Occurrence
                && fact.syntax_kind().as_str() == CALL)
                || (fact.kind() == SyntaxFactKind::Signature
                    && fact.syntax_kind().as_str() == ARGUMENT)
            {
                events.push(fact);
            }
        }
        crate::runtime::sort_cancellable_by(&mut events, cancellation, |left, right| {
            (
                left.span().file(),
                left.parent(),
                left.span().start_byte(),
                std::cmp::Reverse(left.span().end_byte()),
                left.syntax_kind().as_str() == CALL,
            )
                .cmp(&(
                    right.span().file(),
                    right.parent(),
                    right.span().start_byte(),
                    std::cmp::Reverse(right.span().end_byte()),
                    right.syntax_kind().as_str() == CALL,
                ))
        })?;
        let mut stack: Vec<&SyntaxFact> = Vec::new();
        stack.try_reserve(events.len()).map_err(allocation_failed)?;
        for fact in events {
            cancellation.check()?;
            while stack
                .last()
                .is_some_and(|parent| parent.parent() != fact.parent() || !contains(parent, fact))
            {
                stack.pop();
            }
            if fact.syntax_kind().as_str() == CALL {
                let read = reads
                    .get(&(fact.parent(), fact.span().file(), fact.span().start_byte()))
                    .copied()
                    .flatten()
                    .and_then(|id| plan.facts.get(&id).copied())
                    .filter(|read| {
                        contains(fact, read) && read.span().end_byte() < fact.span().end_byte()
                    });
                if let Some(read) = read {
                    let duplicate = stack.last().is_some_and(|call| call.span() == fact.span());
                    if duplicate
                        && let Some(call) = stack.last()
                        && let Some(application) = self.applications.get_mut(&call.local_id())
                    {
                        application.valid = false;
                    }
                    self.applications
                        .try_reserve(1)
                        .map_err(allocation_failed)?;
                    self.applications.insert(
                        fact.local_id(),
                        Application {
                            read: read.local_id(),
                            arguments: 0,
                            argument_end: None,
                            valid: !duplicate,
                        },
                    );
                }
                stack.push(fact);
            } else if let Some(call) = stack.last()
                && let Some(application) = self.applications.get_mut(&call.local_id())
            {
                let read = plan
                    .facts
                    .get(&application.read)
                    .copied()
                    .ok_or_else(invalid_capture)?;
                if fact.span().start_byte()
                    < application.argument_end.unwrap_or(read.span().end_byte())
                {
                    application.valid = false;
                }
                application.arguments = application
                    .arguments
                    .checked_add(1)
                    .ok_or_else(invalid_capture)?;
                application.argument_end = Some(fact.span().end_byte());
            }
        }
        for (id, application) in &mut self.applications {
            cancellation.check()?;
            let call = plan.facts.get(id).copied().ok_or_else(invalid_capture)?;
            application.valid &= application.arguments > 0
                && application.argument_end == Some(call.span().end_byte());
        }
        Ok(())
    }
}

pub(super) fn module_scope(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Scope
        && matches!(
            fact.syntax_kind().as_str(),
            "fsharp.file.scope"
                | "fsharp.namespace.scope"
                | "fsharp.module.scope"
                | "fsharp.nested_module.scope"
        )
}

fn qualified_file_scope(
    fact: &SyntaxFact,
    plan: &FSharpBindings<'_>,
    cancellation: &Cancellation,
) -> Result<bool, AdapterError> {
    let source_end = u64::try_from(plan.source.len()).map_err(|_| invalid_capture())?;
    if fact.span().end_byte() > source_end
        || !super::trivia_between(plan.source, 0, fact.span().start_byte(), cancellation)?
        || !super::trivia_between(
            plan.source,
            fact.span().end_byte(),
            source_end,
            cancellation,
        )?
    {
        return Ok(false);
    }
    // Native parent assignment retains the same-file Root/Module containers
    // above its lexical Scope; they are not additional language-level owners.
    let Some(parent_id) = fact.parent() else {
        return Ok(true);
    };
    let Some(mut parent) = plan.facts.get(&parent_id).copied() else {
        return Ok(false);
    };
    if parent.kind() == SyntaxFactKind::Module
        && parent.syntax_kind().as_str() == "fsharp.file.module"
        && parent.span() == fact.span()
    {
        let Some(root) = parent.parent().and_then(|id| plan.facts.get(&id).copied()) else {
            return Ok(false);
        };
        parent = root;
    }
    Ok(parent.kind() == SyntaxFactKind::Root
        && parent.syntax_kind().as_str() == "fsharp.file.root"
        && parent.span() == fact.span()
        && parent.parent().is_none())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_ir::SourceSpan;

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
                FileId::from_bytes([3; 20]),
                u64::try_from(start).unwrap(),
                u64::try_from(end).unwrap(),
            )
            .unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    fn value_facts(source: &str, id: u64, name: &str) -> Vec<SyntaxFact> {
        let start = source.find(&format!("{name} =")).unwrap();
        let tail = &source[start..];
        let end = start + tail.find('\n').unwrap_or(tail.len());
        let header_end = start + tail.find('=').unwrap();
        vec![
            fact(
                id,
                Some(1),
                SyntaxFactKind::Declaration,
                VARIABLE,
                start,
                end,
            ),
            fact(
                id + 1,
                Some(id),
                SyntaxFactKind::Scope,
                VARIABLE_SCOPE,
                start,
                end,
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
                start + name.len(),
            ),
            fact(
                id + 4,
                Some(id),
                SyntaxFactKind::Signature,
                NONRECURSIVE,
                start,
                end,
            ),
        ]
    }

    #[test]
    fn conditional_scopes_close_older_module_targets_but_allow_later_bindings() {
        let source = "let seed = 1\n#if FEATURE\n#else\n#endif\nlet alias = seed\nlet renewed = 2\nlet result = renewed\n";
        let mut facts = vec![fact(
            1,
            None,
            SyntaxFactKind::Scope,
            "fsharp.file.scope",
            0,
            source.len(),
        )];
        for (id, name) in [(10, "seed"), (20, "alias"), (30, "renewed"), (40, "result")] {
            facts.extend(value_facts(source, id, name));
        }
        let seed_read = source.find("= seed").unwrap() + 2;
        let renewed_read = source.find("= renewed").unwrap() + 2;
        let before = fact(
            60,
            Some(21),
            SyntaxFactKind::Occurrence,
            READ,
            seed_read,
            seed_read + 4,
        );
        let after = fact(
            61,
            Some(41),
            SyntaxFactKind::Occurrence,
            READ,
            renewed_read,
            renewed_read + 7,
        );
        facts.extend([before.clone(), after.clone()]);
        let symbols = HashMap::from([
            (13, SymbolId::from_bytes([1; 20])),
            (23, SymbolId::from_bytes([2; 20])),
            (33, SymbolId::from_bytes([3; 20])),
            (43, SymbolId::from_bytes([4; 20])),
        ]);
        let cancellation = Cancellation::new();
        let ordinary =
            FSharpBindings::new(&facts, source.as_bytes(), &symbols, 128, &cancellation).unwrap();
        assert_eq!(
            ordinary.resolve(&before, "seed", &cancellation).unwrap(),
            symbols.get(&13).copied()
        );
        for label in [
            "fsharp.preprocessor.scope",
            "fsharp.preprocessor_branch.scope",
        ] {
            let start = source.find("#if").unwrap();
            let end = source.find("#endif").unwrap() + "#endif".len();
            let mut conditional = facts.clone();
            let scope = fact(70, Some(1), SyntaxFactKind::Scope, label, start, end);
            assert!(!module_scope(&scope));
            conditional.push(scope);
            let plan = FSharpBindings::new(
                &conditional,
                source.as_bytes(),
                &symbols,
                128,
                &cancellation,
            )
            .unwrap();
            assert_eq!(plan.resolve(&before, "seed", &cancellation).unwrap(), None);
            assert_eq!(
                plan.resolve(&after, "renewed", &cancellation).unwrap(),
                symbols.get(&33).copied()
            );
            let mut wrong_kind = facts.clone();
            wrong_kind.push(fact(
                70,
                Some(1),
                SyntaxFactKind::Signature,
                label,
                start,
                end,
            ));
            let plan =
                FSharpBindings::new(&wrong_kind, source.as_bytes(), &symbols, 128, &cancellation)
                    .unwrap();
            assert_eq!(
                plan.resolve(&before, "seed", &cancellation).unwrap(),
                symbols.get(&13).copied()
            );
        }
    }
}
