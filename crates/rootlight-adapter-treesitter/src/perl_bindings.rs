//! Perl lexical pads derived from native declaration and statement scopes.
//! Source identities describe bindings, not runtime values or package aliases;
//! unavailable inner identities still prevent fallback to an outer namesake.

use std::{
    borrow::Cow,
    collections::{BTreeMap, HashMap},
};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact, SyntaxFactKind};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;

#[derive(Clone, Copy)]
struct Binding {
    visible_from: u64,
    symbol: Option<SymbolId>,
}

pub(super) struct PerlBindings<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    bindings: BTreeMap<(u64, &'a str), Vec<Binding>>,
    body_starts: BTreeMap<u64, u64>,
    maximum_name_bytes: usize,
}

impl<'a> PerlBindings<'a> {
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
            bindings: BTreeMap::new(),
            body_starts: BTreeMap::new(),
            maximum_name_bytes,
        };
        for fact in facts {
            cancellation.check()?;
            if result.facts.insert(fact.local_id(), fact).is_some() {
                return Err(invalid());
            }
            if fact.syntax_kind().as_str() == "perl.block.scope"
                && let Some(parent) = fact.parent()
            {
                result
                    .body_starts
                    .entry(parent)
                    .and_modify(|start| *start = (*start).min(fact.span().start_byte()))
                    .or_insert(fact.span().start_byte());
            }
        }
        for fact in facts {
            cancellation.check()?;
            if matches!(
                fact.syntax_kind().as_str(),
                "perl.method.scope" | "perl.method_lambda.scope"
            ) {
                // Method self has no written parameter; do not invent its source
                // or bind a use to a surrounding lexical with the same name.
                result
                    .bindings
                    .entry((fact.local_id(), "$self"))
                    .or_default()
                    .push(Binding {
                        visible_from: fact.span().start_byte(),
                        symbol: None,
                    });
            }
            if fact.syntax_kind().as_str() != "perl.variable_name.definition" {
                continue;
            }
            let Some((scope, visible_from, lexical)) = result.visibility(fact, cancellation)?
            else {
                continue;
            };
            let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid())?;
            let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid())?;
            let name = std::str::from_utf8(source.get(start..end).ok_or_else(invalid)?)
                .map_err(|_| invalid())?;
            if name.len() > maximum_name_bytes || !unqualified_name(name) {
                continue;
            }
            result
                .bindings
                .entry((scope, name))
                .or_default()
                .push(Binding {
                    visible_from,
                    symbol: lexical
                        .then(|| symbols.get(&fact.local_id()).copied())
                        .flatten(),
                });
        }
        for bindings in result.bindings.values_mut() {
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
        if name.len() > self.maximum_name_bytes {
            return Ok(None);
        }
        let name = match fact.syntax_kind().as_str() {
            "perl.variable_name.reference" => Cow::Borrowed(name),
            "perl.array_container.reference" => {
                Cow::Owned(format!("@{}", name.get(1..).ok_or_else(invalid)?))
            }
            "perl.hash_container.reference" => {
                Cow::Owned(format!("%{}", name.get(1..).ok_or_else(invalid)?))
            }
            "perl.array_length.reference" => {
                Cow::Owned(format!("@{}", name.get(2..).ok_or_else(invalid)?))
            }
            _ => return Ok(None),
        };
        if !unqualified_name(&name) {
            return Ok(None);
        }
        let mut scope = self.nearest_scope(fact.parent(), cancellation)?;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = scope else {
                return Ok(None);
            };
            if let Some(bindings) = self.bindings.get(&(id, name.as_ref())) {
                let after = bindings
                    .partition_point(|binding| binding.visible_from <= fact.span().start_byte());
                if let Some(index) = after.checked_sub(1) {
                    let binding = bindings.get(index).ok_or_else(invalid)?;
                    if index
                        .checked_sub(1)
                        .and_then(|index| bindings.get(index))
                        .is_some_and(|prior| prior.visible_from == binding.visible_from)
                    {
                        return Ok(None);
                    }
                    return Ok(binding.symbol);
                }
            }
            let owner = self.fact(id)?;
            if matches!(
                owner.syntax_kind().as_str(),
                "perl.file.scope" | "perl.unsupported_context.scope"
            ) {
                return Ok(None);
            }
            scope = self.nearest_scope(owner.parent(), cancellation)?;
        }
        Err(invalid())
    }

    fn visibility(
        &self,
        definition: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<(u64, u64, bool)>, AdapterError> {
        let mut parent = definition.parent();
        let mut lexical = None;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            let fact = self.fact(id)?;
            match fact.syntax_kind().as_str() {
                "perl.parameter.declaration" => {
                    return Ok(self
                        .nearest_scope(fact.parent(), cancellation)?
                        .map(|scope| (scope, fact.span().end_byte(), true)));
                }
                "perl.lexical_variable.declaration" => lexical = Some(true),
                "perl.package_variable.declaration" | "perl.field.declaration" => {
                    // Package aliases and fields do not have proven lexical targets.
                    lexical = Some(false);
                }
                "perl.statement.scope" if lexical.is_some() => {
                    // Existing lexical reads survive the whole statement, including
                    // an our initializer. Its immediate strict-vars exemption does
                    // not replace an already visible lexical binding.
                    return Ok(self
                        .nearest_scope(fact.parent(), cancellation)?
                        .map(|scope| (scope, fact.span().end_byte(), lexical == Some(true))));
                }
                "perl.lexical_for.scope" | "perl.package_for.scope" if lexical.is_some() => {
                    return Ok(Some((
                        id,
                        self.body_starts
                            .get(&id)
                            .copied()
                            .unwrap_or(fact.span().end_byte()),
                        fact.syntax_kind().as_str() == "perl.lexical_for.scope",
                    )));
                }
                _ if is_scope(fact) => return Ok(None),
                _ => {}
            }
            parent = fact.parent();
        }
        Err(invalid())
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
        Err(invalid())
    }

    fn fact(&self, id: u64) -> Result<&'a SyntaxFact, AdapterError> {
        self.facts.get(&id).copied().ok_or_else(invalid)
    }
}

fn is_scope(fact: &SyntaxFact) -> bool {
    fact.kind() == SyntaxFactKind::Scope
        && matches!(
            fact.syntax_kind().as_str(),
            "perl.file.scope"
                | "perl.block.scope"
                | "perl.function.scope"
                | "perl.method.scope"
                | "perl.lambda.scope"
                | "perl.method_lambda.scope"
                | "perl.phaser.scope"
                | "perl.control.scope"
                | "perl.lexical_for.scope"
                | "perl.package_for.scope"
                | "perl.unsupported_context.scope"
        )
}

fn unqualified_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('$' | '@' | '%'))
        && chars
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_alphabetic())
        // Native varname captures already validate Unicode identifier characters.
        // Reject qualification/indirection without excluding combining marks.
        && chars.all(|ch| !ch.is_control() && !ch.is_whitespace() && !matches!(ch, ':' | '\'' | '{' | '}' | '$' | '@' | '%'))
}

fn invalid() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("perl-binding-capture")
            .expect("built-in Perl capture diagnostic is valid"),
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
            fact(1, None, SyntaxFactKind::Scope, "perl.file.scope", 0, 12),
            fact(
                2,
                Some(1),
                SyntaxFactKind::Scope,
                "perl.statement.scope",
                0,
                7,
            ),
            fact(
                3,
                Some(2),
                SyntaxFactKind::Declaration,
                "perl.lexical_variable.declaration",
                3,
                5,
            ),
            fact(
                4,
                Some(3),
                SyntaxFactKind::Occurrence,
                "perl.variable_name.definition",
                3,
                5,
            ),
            fact(
                5,
                Some(1),
                SyntaxFactKind::Occurrence,
                "perl.variable_name.reference",
                9,
                11,
            ),
        ]
    }

    #[test]
    fn lexical_targets_do_not_depend_on_fact_order_or_missing_symbol_fallback() {
        let mut facts = facts();
        let symbol = SymbolId::from_bytes([9; 20]);
        for symbols in [HashMap::from([(4, symbol)]), HashMap::new()] {
            for _ in 0..2 {
                let plan =
                    PerlBindings::new(&facts, b"my $x=1; $x;", &symbols, 64, &Cancellation::new())
                        .unwrap();
                let reference = facts.iter().find(|fact| fact.local_id() == 5).unwrap();
                assert_eq!(
                    plan.resolve(reference, "$x", &Cancellation::new()).unwrap(),
                    symbols.get(&4).copied()
                );
                facts.reverse();
            }
        }
    }

    #[test]
    fn invalid_or_cyclic_parent_links_fail_with_bounded_work() {
        for parent in [99, 6] {
            let mut facts = facts();
            facts.push(fact(
                6,
                Some(parent),
                SyntaxFactKind::Occurrence,
                "perl.variable_name.definition",
                3,
                5,
            ));
            assert!(matches!(
                PerlBindings::new(
                    &facts,
                    b"my $x=1; $x;",
                    &HashMap::new(),
                    64,
                    &Cancellation::new()
                ),
                Err(AdapterError::ProviderFailed { .. })
            ));
        }
    }

    #[test]
    fn cancellation_stops_empty_plans_and_reference_queries() {
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            PerlBindings::new(&[], b"", &HashMap::new(), 64, &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
        let facts = facts();
        let plan = PerlBindings::new(
            &facts,
            b"my $x=1; $x;",
            &HashMap::new(),
            64,
            &Cancellation::new(),
        )
        .unwrap();
        assert!(matches!(
            plan.resolve(&facts[4], "$x", &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
