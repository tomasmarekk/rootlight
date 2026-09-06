//! Lexical Lua binding visibility over audited, parser-independent captures.
//!
//! This resolves variable identities, not runtime function values or module
//! loading. Missing declaration identities still shadow enclosing bindings.

use std::collections::{BTreeMap, HashMap};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;

#[derive(Clone, Copy)]
struct Binding {
    visible_from: u64,
    symbol: Option<SymbolId>,
}

pub(super) struct LuaBindings<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    block_starts: BTreeMap<u64, u64>,
    bindings: BTreeMap<(u64, &'a str), Vec<Binding>>,
}

impl<'a> LuaBindings<'a> {
    pub(super) fn new(
        facts: &'a [SyntaxFact],
        source: &'a [u8],
        symbols: &HashMap<u64, SymbolId>,
        maximum_name_bytes: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut result = Self {
            facts: BTreeMap::new(),
            block_starts: BTreeMap::new(),
            bindings: BTreeMap::new(),
        };
        for fact in facts {
            cancellation.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(invalid_capture());
            }
            if fact.syntax_kind().as_str() == "lua.block.scope"
                && let Some(parent) = fact.parent()
            {
                result
                    .block_starts
                    .entry(parent)
                    .and_modify(|start| *start = (*start).min(fact.span().start_byte()))
                    .or_insert(fact.span().start_byte());
            }
        }
        for fact in facts {
            cancellation.check()?;
            if fact.syntax_kind().as_str() == "lua.method.declaration" {
                if let Some(scope) = result.nearest_scope(fact.parent(), cancellation)? {
                    // Colon methods introduce self without a physical parameter span.
                    // Do not fabricate source evidence or bind it to an outer self.
                    result
                        .bindings
                        .entry((scope, "self"))
                        .or_default()
                        .push(Binding {
                            visible_from: fact.span().start_byte(),
                            symbol: None,
                        });
                }
                continue;
            }
            if fact.syntax_kind().as_str() != "lua.identifier.definition" {
                continue;
            }
            let Some((scope, visible_from)) = result.binding_visibility(fact, cancellation)? else {
                continue;
            };
            let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
            let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
            let name = source.get(start..end).ok_or_else(invalid_capture)?;
            if name.len() > maximum_name_bytes {
                continue;
            }
            let name = std::str::from_utf8(name).map_err(|_| invalid_capture())?;
            result
                .bindings
                .entry((scope, name))
                .or_default()
                .push(Binding {
                    visible_from,
                    symbol: symbols.get(&fact.local_id()).copied(),
                });
        }
        for bindings in result.bindings.values_mut() {
            cancellation.check()?;
            crate::runtime::sort_cancellable_by(bindings, cancellation, |left, right| {
                left.visible_from.cmp(&right.visible_from)
            })?;
        }
        Ok(result)
    }

    pub(super) fn resolve(
        &self,
        fact: &SyntaxFact,
        name: &str,
        cancellation: &Cancellation,
    ) -> Result<Option<SymbolId>, AdapterError> {
        cancellation.check()?;
        if fact.syntax_kind().as_str() != "lua.identifier.reference" {
            return Ok(None);
        }
        let mut scope = self.nearest_scope(fact.parent(), cancellation)?;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(current) = scope else {
                return Ok(None);
            };
            if let Some(bindings) = self.bindings.get(&(current, name)) {
                let after = bindings
                    .partition_point(|binding| binding.visible_from <= fact.span().start_byte());
                if let Some(index) = after.checked_sub(1) {
                    let binding = &bindings[index];
                    if index > 0 && bindings[index - 1].visible_from == binding.visible_from {
                        return Ok(None);
                    }
                    // An unavailable inner identity must not expose a same-named outer one.
                    return Ok(binding.symbol);
                }
            }
            scope = self.nearest_scope(self.fact(current)?.parent(), cancellation)?;
        }
        Err(invalid_capture())
    }

    fn binding_visibility(
        &self,
        definition: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<(u64, u64)>, AdapterError> {
        let mut parent = definition.parent();
        let mut declaration_seen = false;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            let fact = self.fact(id)?;
            let kind = fact.syntax_kind().as_str();
            if fact.kind() == SyntaxFactKind::Declaration && !declaration_seen {
                declaration_seen = true;
                match kind {
                    "lua.parameter.declaration" => {
                        return Ok(self
                            .nearest_scope(fact.parent(), cancellation)?
                            .map(|scope| (scope, definition.span().end_byte())));
                    }
                    "lua.local_function.declaration" => {
                        let own_scope = self
                            .nearest_scope(fact.parent(), cancellation)?
                            .ok_or_else(invalid_capture)?;
                        let owner =
                            self.nearest_scope(self.fact(own_scope)?.parent(), cancellation)?;
                        return Ok(owner.map(|scope| (scope, fact.span().start_byte())));
                    }
                    "lua.field_function.declaration" => return Ok(None),
                    _ => {}
                }
            }
            if kind == "lua.local_binding.scope" {
                // Lua local initializers see the previous environment; local-function
                // syntax is handled above because its binding is recursive.
                return Ok(self
                    .nearest_scope(fact.parent(), cancellation)?
                    .map(|scope| (scope, fact.span().end_byte())));
            }
            if kind == "lua.for.scope" {
                let body_start = self
                    .block_starts
                    .get(&id)
                    .copied()
                    .unwrap_or(fact.span().end_byte());
                return Ok(Some((id, body_start)));
            }
            if is_lexical_scope(fact) {
                return Ok(None);
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
            if is_lexical_scope(fact) {
                if fact.syntax_kind().as_str() == "lua.block.scope"
                    && let Some(owner) = fact.parent().map(|id| self.fact(id)).transpose()?
                    && owner.syntax_kind().as_str() == "lua.repeat.scope"
                {
                    // A repeat body's locals also cover its until condition.
                    return Ok(Some(owner.local_id()));
                }
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

fn is_lexical_scope(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Scope
        && matches!(
            fact.syntax_kind().as_str(),
            "lua.file.scope"
                | "lua.block.scope"
                | "lua.function.scope"
                | "lua.for.scope"
                | "lua.repeat.scope"
        )
}

fn invalid_capture() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("lua-binding-capture")
            .expect("built-in Lua capture diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    use rootlight_adapter_sdk::SyntaxKindLabel;
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    use super::*;

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
            SourceSpan::new(FileId::from_bytes([7; 20]), start, end)
                .expect("fixture span is valid"),
            0,
            SyntaxKindLabel::new(label).expect("fixture label is valid"),
        )
    }

    fn facts() -> Vec<SyntaxFact> {
        vec![
            fact(1, None, SyntaxFactKind::Scope, "lua.file.scope", 0, 8),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Scope,
                "lua.local_binding.scope",
                0,
                2,
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                "lua.identifier.definition",
                0,
                1,
            ),
            fact(
                4,
                Some(1),
                SyntaxFactKind::Occurrence,
                "lua.identifier.reference",
                4,
                5,
            ),
        ]
    }

    #[test]
    fn lexical_plan_is_independent_of_capture_order_and_ambiguous_ties_fail_closed() {
        let mut facts = facts();
        let symbol = SymbolId::from_bytes([9; 20]);
        let symbols = HashMap::from([(3, symbol)]);
        let cancellation = Cancellation::new();
        for _ in 0..2 {
            let plan = LuaBindings::new(&facts, b"x   x   ", &symbols, 64, &cancellation)
                .expect("complete captures plan");
            let reference = facts
                .iter()
                .find(|fact| fact.local_id() == 4)
                .expect("reference exists");
            assert_eq!(
                plan.resolve(reference, "x", &cancellation)
                    .expect("reference resolves"),
                Some(symbol)
            );
            facts.reverse();
        }
        facts.push(fact(
            5,
            Some(2),
            SyntaxFactKind::Occurrence,
            "lua.identifier.definition",
            0,
            1,
        ));
        let plan = LuaBindings::new(&facts, b"x   x   ", &symbols, 64, &cancellation)
            .expect("ambiguous captures remain representable");
        assert_eq!(
            plan.resolve(&facts[3], "x", &cancellation)
                .expect("ambiguity is bounded"),
            None
        );
    }

    #[test]
    fn invalid_capture_ancestry_is_rejected_without_unbounded_traversal() {
        let cancellation = Cancellation::new();
        for bad in [
            fact(
                3,
                Some(2),
                SyntaxFactKind::Occurrence,
                "lua.identifier.definition",
                0,
                1,
            ),
            fact(
                5,
                Some(99),
                SyntaxFactKind::Occurrence,
                "lua.identifier.definition",
                0,
                1,
            ),
            fact(
                5,
                Some(5),
                SyntaxFactKind::Occurrence,
                "lua.identifier.definition",
                0,
                1,
            ),
        ] {
            let mut facts = facts();
            facts.push(bad);
            assert!(matches!(
                LuaBindings::new(&facts, b"x   x   ", &HashMap::new(), 64, &cancellation),
                Err(AdapterError::ProviderFailed { .. })
            ));
        }
    }

    #[test]
    fn cancelled_binding_work_stops_even_for_an_empty_plan() {
        let cancellation = Cancellation::new();
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            LuaBindings::new(&[], b"", &HashMap::new(), 64, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
        let facts = facts();
        let plan = LuaBindings::new(
            &facts,
            b"x   x   ",
            &HashMap::new(),
            64,
            &Cancellation::new(),
        )
        .expect("uncancelled plan builds");
        assert!(matches!(
            plan.resolve(&facts[3], "x", &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
