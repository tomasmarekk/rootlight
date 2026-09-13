//! Collects complete native Groovy call, value and static-import lookup inputs.
//! Reuses the lowering fact index; import declarations are emitted once per file,
//! while argument prefixes follow their retained call scope rather than text parsing.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact};
use rootlight_cancel::Cancellation;
use rootlight_ids::content_hash;
use rootlight_ir::{GroovyCallArguments, GroovyCallLookup, GroovyStaticImport, SourceSpan};

use crate::groovy_bindings::GroovyBindings;

/// Native contexts collected only after the whole file has parsed completely.
#[derive(Default)]
pub(super) struct Plan {
    pub(super) calls: BTreeMap<SourceSpan, GroovyCallLookup>,
    pub(super) imports: BTreeMap<SourceSpan, Option<GroovyStaticImport>>,
    pub(super) values: BTreeMap<SourceSpan, rootlight_ir::GroovyValueLookup>,
}

#[derive(Default)]
struct Call<'a> {
    name: Option<&'a SyntaxFact>,
    arguments: GroovyCallArguments,
    argument_ends: BTreeSet<u64>,
}

/// Collects bounded native lookup evidence without choosing a target declaration.
///
/// # Errors
/// Returns cancellation, inconsistent capture ancestry/duplicates or invalid source spans.
pub(super) fn collect(
    facts: &[SyntaxFact],
    by_id: &HashMap<u64, &SyntaxFact>,
    bindings: &GroovyBindings<'_>,
    source: &[u8],
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Plan, AdapterError> {
    cancellation.check()?;
    let mut calls = BTreeMap::<u64, Call<'_>>::new();
    let mut aliases = BTreeMap::new();
    let mut values = BTreeMap::new();
    for fact in facts {
        cancellation.check()?;
        let value_context = match fact.syntax_kind().as_str() {
            "groovy.value_read.expression" => Some((true, None)),
            "groovy.value_assignment.expression" => {
                Some((false, Some(rootlight_ir::GroovyValueWrite::Assignment)))
            }
            "groovy.value_compound_assignment.expression" => Some((
                false,
                Some(rootlight_ir::GroovyValueWrite::CompoundAssignment),
            )),
            "groovy.value_update.expression" => {
                Some((false, Some(rootlight_ir::GroovyValueWrite::Update)))
            }
            "groovy.value_destructuring.expression" => {
                Some((false, Some(rootlight_ir::GroovyValueWrite::Destructuring)))
            }
            _ => None,
        };
        if let Some(context) = value_context
            && values.insert(fact.span(), context).is_some()
        {
            return Err(invalid());
        }
        if fact.syntax_kind().as_str() == "groovy.implicit_call.scope" {
            calls.insert(fact.local_id(), Call::default());
        }
        if fact.syntax_kind().as_str() == "groovy.import_alias.definition"
            && let Some(owner) = owner(fact, by_id, "groovy.import.scope", cancellation)?
        {
            let name = text(fact, source)?;
            if name.len() <= maximum_name_bytes {
                aliases
                    .entry(owner.local_id())
                    .and_modify(|prior| *prior = None)
                    .or_insert(Some((fact.span(), name)));
            } else {
                aliases.insert(owner.local_id(), None);
            }
        }
    }
    for fact in facts {
        cancellation.check()?;
        let kind = fact.syntax_kind().as_str();
        let is_name = kind == "groovy.implicit_call_name.call_name";
        if !is_name && !kind.starts_with("groovy.argument_") {
            continue;
        }
        let Some(owner) = call_owner(fact, by_id, cancellation)? else {
            return Err(invalid());
        };
        let Some(call) = calls.get_mut(&owner.local_id()) else {
            continue;
        };
        if is_name {
            if call.name.replace(fact).is_some() {
                return Err(invalid());
            }
            continue;
        }
        if fact.span().file() != owner.span().file()
            || fact.span().start_byte() != owner.span().start_byte()
            || fact.span().end_byte() > owner.span().end_byte()
            || !call.argument_ends.insert(fact.span().end_byte())
        {
            return Err(invalid());
        }
        let count = match kind {
            "groovy.argument_positional_prefix.expression" => &mut call.arguments.positional,
            "groovy.argument_named_prefix.expression" => &mut call.arguments.named,
            "groovy.argument_spread_prefix.expression" => &mut call.arguments.spread,
            "groovy.argument_closure_prefix.expression" => &mut call.arguments.trailing_closures,
            _ => return Err(invalid()),
        };
        *count = count.checked_add(1).ok_or_else(invalid)?;
    }
    let mut plan = Plan::default();
    for fact in facts
        .iter()
        .filter(|fact| fact.syntax_kind().as_str() == "groovy.identifier.reference")
    {
        cancellation.check()?;
        let Some(&(read_only, write_kind)) = values.get(&fact.span()) else {
            continue;
        };
        let written = text(fact, source)?;
        if written.len() > maximum_name_bytes {
            continue;
        }
        let context = bindings.call_lookup(
            fact,
            fact,
            written,
            GroovyCallArguments::default(),
            cancellation,
        )?;
        let lookup = rootlight_ir::GroovyValueLookup {
            spelling: context.spelling,
            name_hash: context.name_hash,
            enclosing_type: context.enclosing_type,
            enclosing_callable: context.enclosing_callable,
            lexical_binding: context.lexical_binding,
            static_context: context.static_context,
            read_only,
            write_kind,
        };
        if plan.values.insert(fact.span(), lookup).is_some() {
            return Err(invalid());
        }
    }
    for (id, call) in calls {
        cancellation.check()?;
        let Some(name) = call.name else { continue };
        let written = text(name, source)?;
        if written.len() > maximum_name_bytes {
            continue;
        }
        let owner = by_id.get(&id).copied().ok_or_else(invalid)?;
        let lookup = bindings.call_lookup(owner, name, written, call.arguments, cancellation)?;
        if plan.calls.insert(owner.span(), lookup).is_some() {
            return Err(invalid());
        }
    }
    for fact in facts {
        cancellation.check()?;
        let star = match fact.syntax_kind().as_str() {
            "groovy.static_import.reference" => false,
            "groovy.static_star_import.reference" => true,
            _ => continue,
        };
        let owner = owner(fact, by_id, "groovy.import.scope", cancellation)?.ok_or_else(invalid)?;
        let import = (|| -> Result<Option<GroovyStaticImport>, AdapterError> {
            let target = rootlight_adapter_sdk::structural_captured_name_for_fact(
                "groovy",
                fact,
                text(fact, source)?,
                maximum_name_bytes,
            );
            let Some(target) = target else {
                return Ok(None);
            };
            let (owner_name, member, binding) = if star {
                (target.as_ref(), None, None)
            } else {
                let Some((owner_name, member)) = target.rsplit_once('.') else {
                    return Ok(None);
                };
                (owner_name, Some(member), Some(member))
            };
            let (alias, binding) = match aliases.get(&owner.local_id()) {
                Some(Some((span, name))) if !star => (Some(*span), Some(*name)),
                Some(_) => return Ok(None),
                None => (None, binding),
            };
            Ok(Some(GroovyStaticImport {
                name: fact.span(),
                alias,
                owner: content_hash(owner_name.as_bytes()),
                member: member.map(|name| content_hash(name.as_bytes())),
                binding: binding.map(|name| content_hash(name.as_bytes())),
                member_spelling: member.map(str::to_owned),
                binding_spelling: binding.map(str::to_owned),
            }))
        })()?;
        plan.imports
            .entry(owner.span())
            .and_modify(|prior| *prior = None)
            .or_insert(import);
    }
    Ok(plan)
}

fn text<'a>(fact: &SyntaxFact, source: &'a [u8]) -> Result<&'a str, AdapterError> {
    let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid())?;
    let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid())?;
    std::str::from_utf8(source.get(start..end).ok_or_else(invalid)?).map_err(|_| invalid())
}

fn call_owner<'a>(
    fact: &SyntaxFact,
    by_id: &HashMap<u64, &'a SyntaxFact>,
    cancellation: &Cancellation,
) -> Result<Option<&'a SyntaxFact>, AdapterError> {
    ancestor(
        fact,
        by_id,
        |fact| {
            matches!(
                fact.syntax_kind().as_str(),
                "groovy.implicit_call.scope" | "groovy.call.scope"
            )
        },
        cancellation,
    )
}

fn owner<'a>(
    fact: &SyntaxFact,
    by_id: &HashMap<u64, &'a SyntaxFact>,
    kind: &str,
    cancellation: &Cancellation,
) -> Result<Option<&'a SyntaxFact>, AdapterError> {
    ancestor(
        fact,
        by_id,
        |fact| fact.syntax_kind().as_str() == kind,
        cancellation,
    )
}

fn ancestor<'a>(
    fact: &SyntaxFact,
    by_id: &HashMap<u64, &'a SyntaxFact>,
    select: impl Fn(&SyntaxFact) -> bool,
    cancellation: &Cancellation,
) -> Result<Option<&'a SyntaxFact>, AdapterError> {
    let mut parent = fact.parent();
    for _ in 0..by_id.len() {
        cancellation.check()?;
        let Some(id) = parent else { return Ok(None) };
        let fact = by_id.get(&id).copied().ok_or_else(invalid)?;
        if select(fact) {
            return Ok(Some(fact));
        }
        parent = fact.parent();
    }
    Err(invalid())
}

fn invalid() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("groovy-call-lookup-capture")
            .expect("built-in diagnostic code is valid"),
    }
}
