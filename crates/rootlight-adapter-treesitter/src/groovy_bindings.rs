//! Resolves authored Groovy locals and parameters from audited lexical captures.
//! Visibility excludes initializers and noncapturing method/type boundaries;
//! runtime properties and generated closure parameters never acquire invented identities.

use std::collections::{BTreeMap, HashMap};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;

struct Binding {
    visible_from: u64,
    symbol: Option<SymbolId>,
}

pub(super) struct GroovyBindings<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    iterable_ends: BTreeMap<u64, u64>,
    bindings: BTreeMap<(u64, &'a str), Binding>,
}

impl<'a> GroovyBindings<'a> {
    /// Builds a file-bounded visibility plan; absent entity identities still shadow.
    ///
    /// # Errors
    /// Returns cancellation or invalid capture/source relationships.
    pub(super) fn new(
        facts: &'a [SyntaxFact],
        source: &'a [u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        let mut result = Self {
            facts: BTreeMap::new(),
            iterable_ends: BTreeMap::new(),
            bindings: BTreeMap::new(),
        };
        cancellation.check()?;
        for fact in facts {
            cancellation.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(invalid_capture());
            }
        }
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() == "groovy.iterable.expression" {
                let mut parent = fact.parent();
                for _ in 0..facts.len() {
                    cancellation.check()?;
                    let Some(id) = parent else {
                        break;
                    };
                    let owner = result.fact(id)?;
                    if owner.syntax_kind().as_str() == "groovy.enhanced_for.scope" {
                        result.iterable_ends.insert(id, fact.span().end_byte());
                        break;
                    }
                    parent = owner.parent();
                }
            }
        }
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() != "groovy.identifier.definition" {
                continue;
            }
            let Some((scope, visible_from)) = result.visibility(fact, cancellation)? else {
                continue;
            };
            let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
            let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
            let name = source.get(start..end).ok_or_else(invalid_capture)?;
            if name.len() > maximum_name_bytes {
                continue;
            }
            let name = std::str::from_utf8(name).map_err(|_| invalid_capture())?;
            let binding = Binding {
                visible_from,
                symbol: symbols.get(&fact.local_id()).copied(),
            };
            // Duplicate local declarations are not valid Groovy shadowing.
            // Keep the name unavailable instead of selecting by traversal order.
            result
                .bindings
                .entry((scope, name))
                .and_modify(|existing| {
                    existing.symbol = None;
                    existing.visible_from = 0;
                })
                .or_insert(binding);
        }
        Ok(result)
    }

    /// Returns only an exact authored lexical target, never a runtime dispatch guess.
    ///
    /// # Errors
    /// Returns cancellation or malformed cyclic/missing capture ancestry.
    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if fact.syntax_kind().as_str() != "groovy.identifier.reference" {
            return Ok(None);
        }
        let mut scope = self.nearest_scope(fact.parent(), cancellation)?;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(current) = scope else {
                return Ok(None);
            };
            if let Some(binding) = self.bindings.get(&(current, name))
                && fact.span().start_byte() >= binding.visible_from
            {
                return Ok(binding.symbol);
            }
            let owner = self.fact(current)?;
            if matches!(
                owner.syntax_kind().as_str(),
                "groovy.function.scope"
                    | "groovy.constructor.scope"
                    | "groovy.type_body.scope"
                    | "groovy.file.scope"
            ) || (name == "it"
                && owner.syntax_kind().as_str() == "groovy.implicit_closure.scope")
            {
                return Ok(None);
            }
            scope = self.nearest_scope(owner.parent(), cancellation)?;
        }
        Err(invalid_capture())
    }

    fn visibility(
        &self,
        definition: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<(u64, u64)>, AdapterError> {
        let mut parent = definition.parent();
        let mut declaration = None;
        let mut destructuring_end = None;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            let fact = self.fact(id)?;
            if fact.kind() == SyntaxFactKind::Declaration && declaration.is_none() {
                if !matches!(
                    fact.syntax_kind().as_str(),
                    "groovy.variable.declaration" | "groovy.parameter.declaration"
                ) {
                    return Ok(None);
                }
                declaration = Some(fact);
            }
            if fact.syntax_kind().as_str() == "groovy.destructuring.scope" {
                destructuring_end = Some(fact.span().end_byte());
            }
            if is_scope(fact) {
                let Some(declaration) = declaration else {
                    return Ok(None);
                };
                if fact.syntax_kind().as_str() == "groovy.type_body.scope" {
                    return Ok(None);
                }
                let visible_from = if fact.syntax_kind().as_str() == "groovy.enhanced_for.scope" {
                    let iterable_end = self
                        .iterable_ends
                        .get(&id)
                        .copied()
                        .unwrap_or(fact.span().end_byte());
                    // An unbraced loop body can itself declare a local. Only the
                    // header binding becomes visible after evaluating the iterable.
                    if declaration.span().end_byte() <= iterable_end {
                        iterable_end
                    } else {
                        destructuring_end.unwrap_or(declaration.span().end_byte())
                    }
                } else {
                    destructuring_end.unwrap_or(declaration.span().end_byte())
                };
                return Ok(Some((id, visible_from)));
            }
            parent = fact.parent();
        }
        Err(invalid_capture())
    }

    fn nearest_scope(
        &self,
        mut parent: Option<u64>,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            let fact = self.fact(id)?;
            if is_scope(fact) {
                return Ok(Some(id));
            }
            parent = fact.parent();
        }
        Err(invalid_capture())
    }

    fn fact(&self, id: u64) -> Result<&'a SyntaxFact, AdapterError> {
        self.facts.get(&id).copied().ok_or_else(invalid_capture)
    }
}

fn is_scope(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Scope
        && matches!(
            fact.syntax_kind().as_str(),
            "groovy.file.scope"
                | "groovy.function.scope"
                | "groovy.constructor.scope"
                | "groovy.block.scope"
                | "groovy.type_body.scope"
                | "groovy.for.scope"
                | "groovy.enhanced_for.scope"
                | "groovy.catch.scope"
                | "groovy.try.scope"
                | "groovy.closure.scope"
                | "groovy.implicit_closure.scope"
        )
}

fn invalid_capture() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("groovy-binding-capture")
            .expect("built-in Groovy capture diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

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
        vec![
            fact(1, None, SyntaxFactKind::Scope, "groovy.file.scope", 0, 8),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Declaration,
                "groovy.variable.declaration",
                0,
                3,
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                0,
                1,
            ),
            fact(
                4,
                Some(1),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.reference",
                4,
                5,
            ),
        ]
    }

    #[test]
    fn capture_order_does_not_change_targets_and_missing_symbols_remain_unresolved() {
        let mut facts = facts();
        let symbol = SymbolId::from_bytes([9; 20]);
        let symbols = HashMap::from([(3, symbol)]);
        let cancellation = Cancellation::new();
        for _ in 0..2 {
            let plan =
                GroovyBindings::new(&facts, b"x   x   ", &symbols, 64, &cancellation).unwrap();
            let read = facts.iter().find(|fact| fact.local_id() == 4).unwrap();
            assert_eq!(
                plan.resolve(read, "x", &cancellation).unwrap(),
                Some(symbol)
            );
            let plan = GroovyBindings::new(&facts, b"x   x   ", &HashMap::new(), 64, &cancellation)
                .unwrap();
            assert_eq!(plan.resolve(read, "x", &cancellation).unwrap(), None);
            facts.reverse();
        }
    }

    #[test]
    fn duplicate_bindings_do_not_select_an_arbitrary_target() {
        let mut facts = facts();
        facts.push(fact(
            5,
            Some(1),
            SyntaxFactKind::Declaration,
            "groovy.variable.declaration",
            6,
            7,
        ));
        facts.push(fact(
            6,
            Some(5),
            SyntaxFactKind::Occurrence,
            "groovy.identifier.definition",
            6,
            7,
        ));
        let symbol = SymbolId::from_bytes([9; 20]);
        let cancellation = Cancellation::new();
        let plan = GroovyBindings::new(
            &facts,
            b"x   x x ",
            &HashMap::from([(3, symbol)]),
            64,
            &cancellation,
        )
        .unwrap();
        assert_eq!(plan.resolve(&facts[3], "x", &cancellation).unwrap(), None);
    }

    #[test]
    fn malformed_ancestry_and_source_ranges_return_typed_errors() {
        let cancellation = Cancellation::new();
        for bad in [
            fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                0,
                1,
            ),
            fact(
                5,
                Some(99),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                0,
                1,
            ),
            fact(
                5,
                Some(5),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                0,
                1,
            ),
            fact(
                5,
                Some(2),
                SyntaxFactKind::Occurrence,
                "groovy.identifier.definition",
                20,
                21,
            ),
        ] {
            let mut facts = facts();
            facts.push(bad);
            assert!(
                GroovyBindings::new(&facts, b"x   x   ", &HashMap::new(), 64, &cancellation)
                    .is_err()
            );
        }
        let mut facts = facts();
        facts.push(fact(
            5,
            Some(5),
            SyntaxFactKind::Occurrence,
            "groovy.identifier.reference",
            6,
            7,
        ));
        let plan =
            GroovyBindings::new(&facts, b"x   x x ", &HashMap::new(), 64, &cancellation).unwrap();
        assert!(plan.resolve(&facts[4], "x", &cancellation).is_err());
    }

    #[test]
    fn both_plan_construction_and_resolution_honor_cancellation() {
        let facts = facts();
        let cancellation = Cancellation::new();
        let plan =
            GroovyBindings::new(&facts, b"x   x   ", &HashMap::new(), 64, &cancellation).unwrap();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(plan.resolve(&facts[3], "x", &cancellation).is_err());
        assert!(
            GroovyBindings::new(&facts, b"x   x   ", &HashMap::new(), 64, &cancellation).is_err()
        );
    }
}
