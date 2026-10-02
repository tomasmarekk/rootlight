//! Exact Haskell equation-formal reads over complete native clause captures.
//! The separate native match marker leaves clause identity unchanged.
//! Missing or conflicting identities close lookup; no outer search occurs.

use std::collections::{HashMap, HashSet};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SinkError, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;

const CLAUSE: &str = "haskell.function_clause.scope";
const MATCH: &str = "haskell.simple_equation_match.signature";
const READ: &str = "haskell.equation_rhs_read.reference";

struct Parameter<'a> {
    scope: u64,
    name: &'a str,
    definitions: usize,
    symbol: Option<SymbolId>,
    span: rootlight_ir::SourceSpan,
    before_match: bool,
}

/// Exact formal identities from complete, native-marked simple equations.
pub(super) struct HaskellBindings<'a> {
    facts: HashMap<u64, &'a SyntaxFact>,
    clause_counts: HashMap<u64, usize>,
    matches: HashMap<u64, Option<&'a SyntaxFact>>,
    bindings: HashMap<(u64, &'a str), Option<SymbolId>>,
    conflicting_symbols: HashSet<SymbolId>,
}

impl<'a> HaskellBindings<'a> {
    /// Retain same-clause bindings without inferring missing definition identities.
    ///
    /// # Errors
    /// Returns typed cancellation, allocation or invalid native-capture errors.
    pub(super) fn new(
        facts: &'a [SyntaxFact],
        source: &'a [u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut result = Self {
            facts: HashMap::new(),
            clause_counts: HashMap::new(),
            matches: HashMap::new(),
            bindings: HashMap::new(),
            conflicting_symbols: HashSet::new(),
        };
        result
            .facts
            .try_reserve(facts.len())
            .map_err(allocation_failed)?;
        result
            .clause_counts
            .try_reserve(facts.len())
            .map_err(allocation_failed)?;
        result
            .matches
            .try_reserve(facts.len())
            .map_err(allocation_failed)?;
        result
            .bindings
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
            if fact.kind() == SyntaxFactKind::Scope
                && fact.syntax_kind().as_str() == CLAUSE
                && let Some(parent) = fact.parent()
            {
                let count = result.clause_counts.entry(parent).or_default();
                *count = count.checked_add(1).ok_or_else(invalid_capture)?;
            }
        }
        for fact in facts {
            cancellation.check()?;
            if fact.kind() != SyntaxFactKind::Signature || fact.syntax_kind().as_str() != MATCH {
                continue;
            }
            let Some(parent) = fact.parent().and_then(|id| result.facts.get(&id).copied()) else {
                continue;
            };
            let declaration = if parent.kind() == SyntaxFactKind::Scope
                && parent.syntax_kind().as_str() == CLAUSE
            {
                parent
                    .parent()
                    .and_then(|id| result.facts.get(&id).copied())
            } else {
                Some(parent)
            };
            if let Some(declaration) = declaration.filter(|declaration| {
                declaration.kind() == SyntaxFactKind::Declaration
                    && declaration.syntax_kind().as_str() == "haskell.function.declaration"
                    && contains(declaration, fact)
            }) {
                result
                    .matches
                    .entry(declaration.local_id())
                    .and_modify(|matched| *matched = None)
                    .or_insert(Some(fact));
            }
        }
        let mut parameters = HashMap::new();
        parameters
            .try_reserve(facts.len())
            .map_err(allocation_failed)?;
        for fact in facts {
            cancellation.check()?;
            if fact.kind() != SyntaxFactKind::Declaration
                || fact.syntax_kind().as_str() != "haskell.parameter.declaration"
            {
                continue;
            }
            let Some(scope) = fact.parent().and_then(|id| result.facts.get(&id).copied()) else {
                continue;
            };
            if !result.valid_clause(scope) || !contains(scope, fact) {
                continue;
            }
            let Some(name) = source_name(fact, source, maximum_name_bytes)? else {
                continue;
            };
            parameters.insert(
                fact.local_id(),
                Parameter {
                    scope: scope.local_id(),
                    name,
                    definitions: 0,
                    symbol: None,
                    span: fact.span(),
                    before_match: result.matched(scope).is_some_and(|matched| {
                        fact.span().end_byte() <= matched.span().start_byte()
                    }),
                },
            );
        }
        for fact in facts {
            cancellation.check()?;
            if fact.kind() != SyntaxFactKind::Occurrence
                || !fact.syntax_kind().as_str().ends_with(".definition")
            {
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
                && fact.syntax_kind().as_str() == "haskell.binding_name.definition"
                && fact.span() == parameter.span
                && parameter.before_match
            {
                symbols.get(&fact.local_id()).copied()
            } else {
                None
            };
        }
        let mut symbol_owners = HashMap::new();
        symbol_owners
            .try_reserve(parameters.len())
            .map_err(allocation_failed)?;
        for (local_id, parameter) in parameters {
            cancellation.check()?;
            let symbol = (parameter.definitions == 1)
                .then_some(parameter.symbol)
                .flatten();
            if let Some(symbol) = symbol
                && symbol_owners.insert(symbol, local_id).is_some()
            {
                result.conflicting_symbols.insert(symbol);
            }
            // An unavailable or duplicate formal remains a barrier rather than
            // disappearing and exposing a same-spelled binding elsewhere.
            result
                .bindings
                .entry((parameter.scope, parameter.name))
                .and_modify(|symbol| *symbol = None)
                .or_insert(symbol);
        }
        Ok(result)
    }

    /// Resolve an admitted RHS read only to its unique, materialized formal.
    ///
    /// # Errors
    /// Returns typed cancellation or cyclic native-ancestry errors.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if fact.kind() != SyntaxFactKind::Occurrence || fact.syntax_kind().as_str() != READ {
            return Ok(None);
        }
        let mut parent = fact.parent();
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(current) = parent.and_then(|id| self.facts.get(&id).copied()) else {
                return Ok(None);
            };
            if !contains(current, fact) {
                return Ok(None);
            }
            if current.kind() == SyntaxFactKind::Scope {
                if !self.valid_clause(current)
                    || !self
                        .matched(current)
                        .is_some_and(|matched| contains(matched, fact))
                {
                    return Ok(None);
                }
                return Ok(self
                    .bindings
                    .get(&(current.local_id(), name))
                    .copied()
                    .flatten()
                    .filter(|symbol| !self.conflicting_symbols.contains(symbol)));
            }
            if current.kind() == SyntaxFactKind::Declaration {
                return Ok(None);
            }
            if current.kind() != SyntaxFactKind::Occurrence
                || current.syntax_kind().as_str() != "haskell.application.call"
            {
                return Ok(None);
            }
            parent = current.parent();
        }
        Err(invalid_capture())
    }

    fn matched(&self, scope: &SyntaxFact) -> Option<&'a SyntaxFact> {
        scope
            .parent()
            .and_then(|id| self.matches.get(&id).copied().flatten())
    }

    fn valid_clause(&self, scope: &SyntaxFact) -> bool {
        scope.kind() == SyntaxFactKind::Scope
            && scope.syntax_kind().as_str() == CLAUSE
            && scope
                .parent()
                .and_then(|id| self.facts.get(&id).copied())
                .is_some_and(|declaration| {
                    declaration.kind() == SyntaxFactKind::Declaration
                        && declaration.syntax_kind().as_str() == "haskell.function.declaration"
                        && declaration.span() == scope.span()
                        && self.clause_counts.get(&declaration.local_id()) == Some(&1)
                        && self.matched(scope).is_some_and(|matched| {
                            contains(scope, matched)
                                && scope.span().start_byte() < matched.span().start_byte()
                        })
                })
    }
}

fn source_name<'a>(
    fact: &SyntaxFact,
    source: &'a [u8],
    maximum_name_bytes: usize,
) -> Result<Option<&'a str>, AdapterError> {
    let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
    let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
    let bytes = source.get(start..end).ok_or_else(invalid_capture)?;
    if bytes.is_empty() || bytes.len() > maximum_name_bytes {
        return Ok(None);
    }
    std::str::from_utf8(bytes)
        .map(Some)
        .map_err(|_| invalid_capture())
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
        code: DiagnosticCode::new("haskell-equation-binding-capture")
            .expect("built-in Haskell binding capture diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    const SOURCE: &[u8] = b"f x = x\ng x = x\n";

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

    fn facts() -> Vec<SyntaxFact> {
        let mut facts = Vec::new();
        for (base, start) in [(1, 0), (6, 8)] {
            facts.extend([
                fact(
                    base,
                    None,
                    SyntaxFactKind::Declaration,
                    "haskell.function.declaration",
                    start,
                    start + 7,
                ),
                fact(
                    base + 1,
                    Some(base),
                    SyntaxFactKind::Scope,
                    CLAUSE,
                    start,
                    start + 7,
                ),
                fact(
                    base + 2,
                    Some(base + 1),
                    SyntaxFactKind::Declaration,
                    "haskell.parameter.declaration",
                    start + 2,
                    start + 3,
                ),
                fact(
                    base + 3,
                    Some(base + 2),
                    SyntaxFactKind::Occurrence,
                    "haskell.binding_name.definition",
                    start + 2,
                    start + 3,
                ),
                fact(
                    base + 4,
                    Some(base + 1),
                    SyntaxFactKind::Occurrence,
                    READ,
                    start + 6,
                    start + 7,
                ),
                fact(
                    base + 19,
                    Some(base),
                    SyntaxFactKind::Signature,
                    MATCH,
                    start + 4,
                    start + 7,
                ),
            ]);
        }
        facts
    }

    fn symbols() -> HashMap<u64, SymbolId> {
        HashMap::from([
            (4, SymbolId::from_bytes([1; 20])),
            (9, SymbolId::from_bytes([2; 20])),
        ])
    }

    #[test]
    fn same_spelling_is_bound_only_within_its_exact_clause_and_order_is_irrelevant() {
        let mut facts = facts();
        let symbols = symbols();
        for _ in 0..2 {
            let plan =
                HaskellBindings::new(&facts, SOURCE, &symbols, 64, &Cancellation::new()).unwrap();
            for (read, definition) in [(5, 4), (10, 9)] {
                let reference = facts.iter().find(|fact| fact.local_id() == read).unwrap();
                assert_eq!(
                    plan.resolve(reference, "x", &Cancellation::new()).unwrap(),
                    symbols.get(&definition).copied()
                );
                assert_eq!(
                    plan.resolve(reference, "other", &Cancellation::new())
                        .unwrap(),
                    None
                );
            }
            facts.reverse();
        }
    }

    #[test]
    fn missing_duplicate_conflicting_and_resource_unavailable_formals_close_lookup() {
        let original = facts();
        let symbols = symbols();
        for case in 0..8 {
            let mut facts = original.clone();
            let mut symbols = symbols.clone();
            let maximum_name_bytes = if case == 5 { 0 } else { 64 };
            match case {
                0 => {
                    symbols.remove(&4);
                }
                1 => {
                    facts.retain(|fact| fact.local_id() != 4);
                }
                2 => facts.extend([
                    fact(
                        11,
                        Some(2),
                        SyntaxFactKind::Declaration,
                        "haskell.parameter.declaration",
                        2,
                        3,
                    ),
                    fact(
                        12,
                        Some(11),
                        SyntaxFactKind::Occurrence,
                        "haskell.binding_name.definition",
                        2,
                        3,
                    ),
                ]),
                3 => {
                    symbols.insert(9, symbols[&4]);
                }
                4 => facts.push(fact(
                    11,
                    Some(1),
                    SyntaxFactKind::Scope,
                    "haskell.function_clause.scope",
                    0,
                    7,
                )),
                5 => {}
                6 => {
                    facts.retain(|fact| fact.local_id() != 20);
                }
                7 => facts.push(fact(11, Some(1), SyntaxFactKind::Signature, MATCH, 4, 7)),
                _ => unreachable!(),
            }
            let plan = HaskellBindings::new(
                &facts,
                SOURCE,
                &symbols,
                maximum_name_bytes,
                &Cancellation::new(),
            )
            .unwrap();
            let reference = facts.iter().find(|fact| fact.local_id() == 5).unwrap();
            assert_eq!(
                plan.resolve(reference, "x", &Cancellation::new()).unwrap(),
                None,
                "case {case}"
            );
        }
    }

    #[test]
    fn wrong_clause_nested_scope_missing_parent_and_non_native_read_labels_never_fall_outward() {
        let original = facts();
        let symbols = symbols();
        for case in 0..4 {
            let mut facts = original.clone();
            facts.retain(|fact| fact.local_id() != 5);
            let (parent, label) = match case {
                0 => (7, READ),
                1 => {
                    facts.push(fact(
                        11,
                        Some(2),
                        SyntaxFactKind::Scope,
                        "haskell.lambda.scope",
                        0,
                        7,
                    ));
                    (11, READ)
                }
                2 => (99, READ),
                3 => (2, "haskell.identifier.reference"),
                _ => unreachable!(),
            };
            facts.push(fact(
                5,
                Some(parent),
                SyntaxFactKind::Occurrence,
                label,
                6,
                7,
            ));
            let plan =
                HaskellBindings::new(&facts, SOURCE, &symbols, 64, &Cancellation::new()).unwrap();
            let reference = facts.iter().find(|fact| fact.local_id() == 5).unwrap();
            assert_eq!(
                plan.resolve(reference, "x", &Cancellation::new()).unwrap(),
                None
            );
        }
    }

    #[test]
    fn duplicate_ids_cycles_and_invalid_source_coordinates_fail_closed() {
        let mut duplicate = facts();
        duplicate.push(duplicate[0].clone());
        assert!(
            HaskellBindings::new(&duplicate, SOURCE, &symbols(), 64, &Cancellation::new()).is_err()
        );
        assert!(
            HaskellBindings::new(&facts(), b"f", &symbols(), 64, &Cancellation::new()).is_err()
        );
        let mut cycle = facts();
        cycle.retain(|fact| fact.local_id() != 5);
        cycle.extend([
            fact(
                11,
                Some(11),
                SyntaxFactKind::Occurrence,
                "haskell.application.call",
                0,
                7,
            ),
            fact(5, Some(11), SyntaxFactKind::Occurrence, READ, 6, 7),
        ]);
        let plan =
            HaskellBindings::new(&cycle, SOURCE, &symbols(), 64, &Cancellation::new()).unwrap();
        assert!(
            plan.resolve(cycle.last().unwrap(), "x", &Cancellation::new())
                .is_err()
        );
    }

    #[test]
    fn cancelled_construction_and_resolution_return_the_typed_cancellation() {
        let facts = facts();
        let symbols = symbols();
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            HaskellBindings::new(&facts, SOURCE, &symbols, 64, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        let plan =
            HaskellBindings::new(&facts, SOURCE, &symbols, 64, &Cancellation::new()).unwrap();
        assert!(matches!(
            plan.resolve(&facts[4], "x", &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
