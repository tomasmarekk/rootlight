//! CODE values follow native assignment fields and resolved lexical storage identities.
//! Copies snapshot values; escapes, dynamic evaluation and cross-lifetime writes prevent
//! an exact target claim. This pass does not execute repository code.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact};
use rootlight_cancel::Cancellation;
use rootlight_ids::SymbolId;

use crate::perl_bindings::PerlBindings;

#[derive(Clone, Copy)]
enum Value {
    Function(SymbolId),
    Copy(SymbolId),
    Unknown,
}

#[derive(Clone, Copy)]
enum Event {
    Assign(SymbolId, Value),
    Call {
        fact: u64,
        variable: SymbolId,
        lifetime: u64,
        module: u64,
    },
}

struct Context<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    variables: BTreeMap<(u64, u64), (&'a SyntaxFact, SymbolId)>,
}

impl Context<'_> {
    fn enclosing(
        &self,
        fact: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<(u64, u64, bool), AdapterError> {
        let mut parent = fact.parent();
        let mut lifetime = None;
        let mut conditional = false;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else { break };
            let owner = self.facts.get(&id).ok_or_else(invalid)?;
            match owner.syntax_kind().as_str() {
                "perl.file.scope" => return Ok((id, lifetime.unwrap_or(id), conditional)),
                "perl.function.scope"
                | "perl.lexical_function.scope"
                | "perl.our_function.scope"
                | "perl.method.scope"
                | "perl.lambda.scope"
                | "perl.method_lambda.scope"
                | "perl.phaser.scope" => {
                    lifetime.get_or_insert(id);
                }
                "perl.control.scope"
                | "perl.lexical_for.scope"
                | "perl.package_for.scope"
                | "perl.unsupported_context.scope"
                | "perl.non_linear_assignment.scope" => conditional = true,
                _ => {}
            }
            parent = owner.parent();
        }
        Err(invalid())
    }

    fn variables_in(&self, fact: &SyntaxFact) -> impl Iterator<Item = (&SyntaxFact, SymbolId)> {
        self.variables
            .range((fact.span().start_byte(), 0)..=(fact.span().end_byte(), u64::MAX))
            .filter(move |((_, end), _)| *end <= fact.span().end_byte())
            .map(|(_, &(site, symbol))| (site, symbol))
    }

    fn single_variable(
        &self,
        fact: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<(&SyntaxFact, SymbolId)>, AdapterError> {
        let mut variables = self.variables_in(fact);
        cancellation.check()?;
        let first = variables.next();
        Ok(if variables.next().is_none() {
            first
        } else {
            None
        })
    }
}

pub(super) fn resolve(
    facts: &[SyntaxFact],
    source: &[u8],
    bindings: &PerlBindings<'_>,
    definitions: &HashMap<u64, SymbolId>,
    functions: &BTreeMap<u64, SymbolId>,
    cancellation: &Cancellation,
) -> Result<BTreeMap<u64, SymbolId>, AdapterError> {
    cancellation.check()?;
    let mut context = Context {
        facts: BTreeMap::new(),
        variables: BTreeMap::new(),
    };
    let mut lifetimes = BTreeMap::new();
    for fact in facts {
        cancellation.check()?;
        if context.facts.insert(fact.local_id(), fact).is_some() {
            return Err(invalid());
        }
    }
    for fact in facts {
        cancellation.check()?;
        if !matches!(
            fact.syntax_kind().as_str(),
            "perl.variable_name.definition" | "perl.variable_name.reference"
        ) {
            continue;
        }
        let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid())?;
        let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid())?;
        let text = std::str::from_utf8(source.get(start..end).ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        if !text.starts_with('$') {
            continue;
        }
        let symbol = if fact.syntax_kind().as_str().ends_with(".definition") {
            definitions.get(&fact.local_id()).copied()
        } else {
            bindings.resolve(fact, text, cancellation)?
        };
        if let Some(symbol) = symbol {
            if fact.syntax_kind().as_str().ends_with(".definition") {
                lifetimes.insert(symbol, context.enclosing(fact, cancellation)?.1);
            }
            context.variables.insert(
                (fact.span().start_byte(), fact.span().end_byte()),
                (fact, symbol),
            );
        }
    }
    let mut fields = BTreeMap::new();
    let mut occurrences = BTreeMap::new();
    let mut code_names = BTreeMap::new();
    let mut barriers = BTreeSet::new();
    for fact in facts {
        cancellation.check()?;
        let kind = fact.syntax_kind().as_str();
        if kind.ends_with(".expression") {
            fields.insert((fact.span().start_byte(), fact.span().end_byte()), fact);
        }
        if matches!(
            kind,
            "perl.coderef_application.reference"
                | "perl.function_name.reference"
                | "perl.amper_function_name.reference"
        ) {
            occurrences.insert(fact.span(), fact.local_id());
        }
        if kind == "perl.code_function_name.reference"
            && let Some(&target) = functions.get(&fact.local_id())
        {
            code_names.insert((fact.span().start_byte(), fact.span().end_byte()), target);
        }
        if matches!(
            kind,
            "perl.code_flow_barrier.expression"
                | "perl.block_eval_barrier.expression"
                | "perl.parsed_substitution.expression"
        ) {
            barriers.insert(context.enclosing(fact, cancellation)?.0);
        }
    }
    let mut allowed_reads = BTreeSet::new();
    let mut escaped = BTreeSet::new();
    let mut events = Vec::new();
    for fact in facts {
        cancellation.check()?;
        let kind = fact.syntax_kind().as_str();
        if matches!(
            kind,
            "perl.assignment.scope" | "perl.non_linear_assignment.scope"
        ) {
            // Join native fields by their written interval rather than incidental
            // scope-parent links, requiring one assignment target.
            let mut parts = Vec::new();
            for ((_, end), &part) in
                fields.range((fact.span().start_byte(), 0)..=(fact.span().end_byte(), u64::MAX))
            {
                cancellation.check()?;
                if *end <= fact.span().end_byte() {
                    parts.push(part);
                }
            }
            let mut targets = parts.iter().filter(|part| {
                matches!(
                    part.syntax_kind().as_str(),
                    "perl.code_target.expression"
                        | "perl.lexical_code_target.expression"
                        | "perl.unproven_code_target.expression"
                )
            });
            let Some(target) = targets.next() else {
                continue;
            };
            if targets.next().is_some() {
                for (_, symbol) in context.variables_in(fact) {
                    cancellation.check()?;
                    escaped.insert(symbol);
                }
                continue;
            }
            let Some((site, destination)) = context.single_variable(target, cancellation)? else {
                for (_, symbol) in context.variables_in(target) {
                    cancellation.check()?;
                    escaped.insert(symbol);
                }
                continue;
            };
            allowed_reads.insert(site.local_id());
            let (_, lifetime, conditional) = context.enclosing(fact, cancellation)?;
            if conditional
                || kind == "perl.non_linear_assignment.scope"
                || target.syntax_kind().as_str() == "perl.unproven_code_target.expression"
                || lifetimes.get(&destination) != Some(&lifetime)
            {
                escaped.insert(destination);
            }
            let mut value = Value::Unknown;
            for part in parts {
                cancellation.check()?;
                match part.syntax_kind().as_str() {
                    "perl.copied_code_value.expression" => {
                        if let Some((read, symbol)) = context.single_variable(part, cancellation)? {
                            allowed_reads.insert(read.local_id());
                            if lifetimes.get(&symbol) == Some(&lifetime) {
                                value = Value::Copy(symbol);
                            }
                        }
                    }
                    "perl.literal_code_value.expression" => {
                        let mut names = code_names
                            .range(
                                (part.span().start_byte(), 0)..=(part.span().end_byte(), u64::MAX),
                            )
                            .filter(|((_, end), _)| *end <= part.span().end_byte());
                        if let Some((_, &symbol)) = names.next()
                            && names.next().is_none()
                        {
                            value = Value::Function(symbol);
                        }
                    }
                    _ => {}
                }
            }
            events.push((fact.span().end_byte(), Event::Assign(destination, value)));
        } else if matches!(
            kind,
            "perl.scalar_coderef.expression" | "perl.scalar_amper.expression"
        ) {
            let offset = u64::from(kind == "perl.scalar_amper.expression");
            let start = fact
                .span()
                .start_byte()
                .checked_add(offset)
                .ok_or_else(invalid)?;
            let variable = if kind == "perl.scalar_amper.expression" {
                context
                    .variables
                    .range((start, 0)..=(start, u64::MAX))
                    .next()
                    .map(|(_, &variable)| variable)
            } else {
                // A grouped receiver need not start with its scalar. Use only the
                // native operand interval, never a resolved variable from arguments.
                let mut receiver = None;
                for (_, &field) in fields.range((start, 0)..=(start, fact.span().end_byte())) {
                    cancellation.check()?;
                    if field.syntax_kind().as_str() == "perl.scalar_coderef_receiver.expression"
                        && receiver.replace(field).is_some()
                    {
                        return Err(invalid());
                    }
                }
                if let Some(receiver) = receiver {
                    context.single_variable(receiver, cancellation)?
                } else {
                    None
                }
            };
            if let Some((read, variable)) = variable
                && let Some(&call) = occurrences.get(&fact.span())
            {
                allowed_reads.insert(read.local_id());
                let (module, lifetime, _) = context.enclosing(fact, cancellation)?;
                events.push((
                    fact.span().start_byte(),
                    Event::Call {
                        fact: call,
                        variable,
                        lifetime,
                        module,
                    },
                ));
            }
        }
    }
    // Aliasing an argument, taking a scalar reference or an unclassified write can
    // mutate the same pad without a plain assignment event. Do not guess through it.
    for &(fact, symbol) in context.variables.values() {
        cancellation.check()?;
        if fact.syntax_kind().as_str().ends_with(".reference")
            && !allowed_reads.contains(&fact.local_id())
        {
            escaped.insert(symbol);
        }
    }
    crate::runtime::sort_cancellable_by(&mut events, cancellation, |a, b| a.0.cmp(&b.0))?;
    let mut values = BTreeMap::new();
    let mut result = BTreeMap::new();
    for (_, event) in events {
        cancellation.check()?;
        match event {
            Event::Assign(destination, value) => {
                let target = if escaped.contains(&destination) {
                    None
                } else {
                    match value {
                        Value::Function(target) => Some(target),
                        Value::Copy(from) if !escaped.contains(&from) => {
                            values.get(&from).copied().flatten()
                        }
                        Value::Copy(_) | Value::Unknown => None,
                    }
                };
                values.insert(destination, target);
            }
            Event::Call {
                fact,
                variable,
                lifetime,
                module,
            } => {
                if !barriers.contains(&module)
                    && !escaped.contains(&variable)
                    && lifetimes.get(&variable) == Some(&lifetime)
                    && let Some(&Some(target)) = values.get(&variable)
                {
                    result.insert(fact, target);
                }
            }
        }
    }
    Ok(result)
}

fn invalid() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("perl-code-value-capture").expect("built-in diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::{SyntaxFactKind, SyntaxKindLabel};
    use rootlight_cancel::CancellationReason;
    use rootlight_ids::FileId;
    use rootlight_ir::SourceSpan;

    fn fact(id: u64, parent: Option<u64>, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            SyntaxFactKind::Scope,
            SourceSpan::new(FileId::from_bytes([7; 20]), 0, 1).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn ancestry_rejects_missing_cyclic_and_unrooted_owners() {
        for parent in [None, Some(2), Some(99)] {
            let owner = fact(2, parent, "perl.block.scope");
            let site = fact(3, Some(2), "perl.scalar_coderef.expression");
            let context = Context {
                facts: BTreeMap::from([(2, &owner), (3, &site)]),
                variables: BTreeMap::new(),
            };
            assert!(matches!(
                context.enclosing(&site, &Cancellation::new()),
                Err(AdapterError::ProviderFailed { code })
                    if code.as_str() == "perl-code-value-capture"
            ));
        }
    }

    #[test]
    fn empty_value_pass_observes_cancellation() {
        let symbols = HashMap::new();
        let bindings =
            PerlBindings::new(&[], b"", &symbols, &symbols, 64, &Cancellation::new()).unwrap();
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            resolve(&[], b"", &bindings, &symbols, &BTreeMap::new(), &cancelled),
            Err(AdapterError::Cancelled { .. })
        ));
    }

    #[test]
    fn duplicate_fact_id_is_not_silently_overwritten() {
        let symbols = HashMap::new();
        let bindings =
            PerlBindings::new(&[], b"", &symbols, &symbols, 64, &Cancellation::new()).unwrap();
        let facts = [
            fact(1, None, "perl.file.scope"),
            fact(1, None, "perl.file.scope"),
        ];
        assert!(matches!(
            resolve(&facts, b"x", &bindings, &symbols, &BTreeMap::new(), &Cancellation::new()),
            Err(AdapterError::ProviderFailed { code })
                if code.as_str() == "perl-code-value-capture"
        ));
    }
}
