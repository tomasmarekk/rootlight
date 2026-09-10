//! MATLAB lexical workspaces and source-backed variable identities.
//! Repeated writes share a binding, but member lookup and runtime function
//! values remain separate from the lexical reference proved here.

use super::*;

mod functions;

#[derive(Default)]
pub(super) struct Plan {
    pub(super) references: HashMap<u64, u64>,
    pub(super) writes: HashMap<u64, Option<u64>>,
    pub(super) calls: BTreeSet<u64>,
}

#[derive(Clone, Copy)]
struct Workspace {
    parent: Option<u64>,
    captures: bool,
}

#[derive(Clone, Copy)]
struct Binding<'a> {
    name: &'a str,
    workspace: u64,
    declaration: u64,
    isolated: bool,
    input: bool,
    header: bool,
    start: u64,
}

pub(super) fn named_scopes<'a>(
    facts: &[SyntaxFact],
    captures: &HashMap<u64, AssociatedCaptures<'_>>,
    source: &'a str,
    maximum_name_bytes: usize,
    cancellation: &Cancellation,
) -> Result<HashMap<SourceSpan, Option<&'a str>>, AdapterError> {
    let mut names = HashMap::new();
    for fact in facts {
        cancellation.check()?;
        if !matches!(
            fact.syntax_kind().as_str(),
            "matlab.function.declaration"
                | "matlab.method.declaration"
                | "matlab.constructor.declaration"
                | "matlab.class.declaration"
        ) {
            continue;
        }
        let name = captures
            .get(&fact.local_id())
            .and_then(|capture| select_unique_capture(&capture.definitions))
            .map(|definition| source_name(source, definition, maximum_name_bytes))
            .transpose()?
            .flatten();
        names
            .entry(fact.span())
            .and_modify(|name| *name = None)
            .or_insert(name);
    }
    Ok(names)
}

pub(super) fn resolve(
    facts: &[SyntaxFact],
    source: &str,
    drafts: &mut HashMap<u64, EntityDraft>,
    complete: bool,
    maximum_name_bytes: usize,
    scope_names: &HashMap<SourceSpan, Option<&str>>,
    cancellation: &Cancellation,
) -> Result<Plan, AdapterError> {
    let mut plan = Plan::default();
    if !complete
        || !facts
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "matlab.file.module")
    {
        return Ok(plan);
    }
    let mut ordered: Vec<_> = facts.iter().collect();
    crate::runtime::sort_cancellable_by(&mut ordered, cancellation, |left, right| {
        structural_syntax_fact_order(left, right)
    })?;
    let mut owners = HashMap::<u64, Option<u64>>::new();
    let mut workspaces = HashMap::<u64, Workspace>::new();
    let mut workspace_order = Vec::new();
    let mut bindings = Vec::new();
    for fact in &ordered {
        cancellation.check()?;
        let enclosing = fact
            .parent()
            .and_then(|parent| owners.get(&parent).copied().flatten());
        let kind = fact.syntax_kind().as_str();
        let workspace = if fact.kind() == SyntaxFactKind::Scope
            && matches!(
                kind,
                "matlab.file.scope"
                    | "matlab.function.scope"
                    | "matlab.method.scope"
                    | "matlab.constructor.scope"
                    | "matlab.lambda.scope"
                    | "matlab.class.scope"
            ) {
            // File/class/method boundaries do not inherit a surrounding variable
            // workspace. A lambda captures its environment; nested functions
            // inherit only another function's workspace, never the base workspace.
            let parent = if kind == "matlab.lambda.scope" {
                enclosing
            } else if kind == "matlab.function.scope" {
                enclosing.filter(|owner| workspaces.get(owner).is_some_and(|scope| scope.captures))
            } else {
                None
            };
            workspaces.insert(
                fact.local_id(),
                Workspace {
                    parent,
                    captures: !matches!(kind, "matlab.file.scope" | "matlab.class.scope"),
                },
            );
            workspace_order.push(fact.local_id());
            Some(fact.local_id())
        } else {
            enclosing
        };
        owners.insert(fact.local_id(), workspace);
        if fact.kind() != SyntaxFactKind::Declaration
            || !matches!(
                kind,
                "matlab.parameter.declaration"
                    | "matlab.output_variable.declaration"
                    | "matlab.variable.declaration"
                    | "matlab.global_variable.declaration"
                    | "matlab.persistent_variable.declaration"
            )
        {
            continue;
        }
        let Some(workspace) = workspace else { continue };
        let Some(name) = source_name(source, fact, maximum_name_bytes)? else {
            continue;
        };
        bindings.push(Binding {
            name,
            workspace,
            declaration: fact.local_id(),
            isolated: kind != "matlab.variable.declaration",
            input: kind == "matlab.parameter.declaration",
            header: matches!(
                kind,
                "matlab.parameter.declaration" | "matlab.output_variable.declaration"
            ),
            start: fact.span().start_byte(),
        });
    }
    crate::runtime::sort_cancellable_by(&mut bindings, cancellation, |left, right| {
        (
            left.workspace,
            left.name,
            !left.input,
            !left.header,
            left.start,
        )
            .cmp(&(
                right.workspace,
                right.name,
                !right.input,
                !right.header,
                right.start,
            ))
    })?;
    let mut groups = BTreeMap::<(u64, &str), Vec<&Binding<'_>>>::new();
    for binding in &bindings {
        cancellation.check()?;
        groups
            .entry((binding.workspace, binding.name))
            .or_default()
            .push(binding);
    }
    let mut representatives = BTreeMap::<(u64, &str), Option<u64>>::new();
    let mut by_workspace = BTreeMap::<u64, Vec<&str>>::new();
    for &(workspace, name) in groups.keys() {
        cancellation.check()?;
        by_workspace.entry(workspace).or_default().push(name);
    }
    for workspace in workspace_order {
        cancellation.check()?;
        let Some(names) = by_workspace.get(&workspace) else {
            continue;
        };
        for &name in names {
            cancellation.check()?;
            let group = groups.get(&(workspace, name)).ok_or_else(invalid)?;
            let first = group.first().ok_or_else(invalid)?;
            let mut input_seen = false;
            let mut output_seen = false;
            let mut ambiguous = false;
            for binding in group {
                cancellation.check()?;
                if binding.header {
                    let seen = if binding.input {
                        &mut input_seen
                    } else {
                        &mut output_seen
                    };
                    ambiguous |= std::mem::replace(seen, true);
                }
            }
            if ambiguous {
                // Duplicate formals are not repeated writes. Keep their source
                // identities separate and block fallback to an outer namesake.
                representatives.insert((workspace, name), None);
                continue;
            }
            let inherited = if group.iter().any(|binding| binding.isolated) {
                None
            } else {
                lookup(
                    workspaces.get(&workspace).and_then(|scope| scope.parent),
                    name,
                    &representatives,
                    &workspaces,
                    cancellation,
                )?
            };
            let representative = inherited.unwrap_or(first.declaration);
            representatives.insert((workspace, name), Some(representative));
            for binding in group {
                cancellation.check()?;
                if binding.declaration == representative {
                    continue;
                }
                let Some(canonical) = drafts.get(&representative).cloned() else {
                    // Missing inner identities continue to shadow outer names.
                    continue;
                };
                if let Some(draft) = drafts.get_mut(&binding.declaration) {
                    if !binding.header {
                        plan.writes.insert(binding.declaration, draft.parent_entity);
                    }
                    let local_id = draft.local_id;
                    let definition = draft.definition_local_id;
                    let span = draft.span;
                    *draft = canonical;
                    draft.local_id = local_id;
                    draft.definition_local_id = definition;
                    draft.span = span;
                }
            }
        }
    }
    let functions = functions::Functions::new(
        facts,
        source,
        &owners,
        &workspaces,
        scope_names,
        cancellation,
    )?;
    let mut function_reads = HashMap::new();
    for fact in facts {
        cancellation.check()?;
        let handle =
            fact.syntax_kind().as_str() == "matlab.unqualified_function_handle_name.reference";
        let indexed_value = fact.syntax_kind().as_str() == "matlab.indexed_value_name.reference";
        if fact.syntax_kind().as_str() != "matlab.identifier.reference" && !handle && !indexed_value
        {
            continue;
        }
        let Some(name) = source_name(source, fact, maximum_name_bytes)? else {
            continue;
        };
        let owner = owners.get(&fact.local_id()).copied().flatten();
        let variable = if handle {
            None
        } else {
            lookup_entry(owner, name, &representatives, &workspaces, cancellation)?
        };
        let target = match variable {
            Some(target) => target,
            None if indexed_value => None,
            None => functions.resolve(owner, name, cancellation)?,
        };
        if let Some(target) = target {
            plan.references.insert(fact.local_id(), target);
            if !handle && functions.is_function(target) {
                function_reads.insert((owner, fact.span().start_byte()), target);
            }
        }
    }
    for fact in facts {
        cancellation.check()?;
        if fact.syntax_kind().as_str() == "matlab.named_application.reference"
            && let Some(target) = function_reads.get(&(
                owners.get(&fact.local_id()).copied().flatten(),
                fact.span().start_byte(),
            ))
        {
            plan.references.insert(fact.local_id(), *target);
            plan.calls.insert(fact.local_id());
        }
    }
    Ok(plan)
}

fn lookup(
    workspace: Option<u64>,
    name: &str,
    representatives: &BTreeMap<(u64, &str), Option<u64>>,
    workspaces: &HashMap<u64, Workspace>,
    cancellation: &Cancellation,
) -> Result<Option<u64>, AdapterError> {
    lookup_entry(workspace, name, representatives, workspaces, cancellation).map(Option::flatten)
}

fn lookup_entry(
    mut workspace: Option<u64>,
    name: &str,
    representatives: &BTreeMap<(u64, &str), Option<u64>>,
    workspaces: &HashMap<u64, Workspace>,
    cancellation: &Cancellation,
) -> Result<Option<Option<u64>>, AdapterError> {
    for _ in 0..=workspaces.len() {
        cancellation.check()?;
        let Some(current) = workspace else {
            return Ok(None);
        };
        if let Some(target) = representatives.get(&(current, name)) {
            return Ok(Some(*target));
        }
        workspace = workspaces.get(&current).ok_or_else(invalid)?.parent;
    }
    Err(invalid())
}

fn source_name<'a>(
    source: &'a str,
    fact: &SyntaxFact,
    maximum: usize,
) -> Result<Option<&'a str>, AdapterError> {
    let text = source_text(source, fact)?;
    Ok(rootlight_adapter_sdk::structural_captured_name(
        text, maximum,
    ))
}

fn source_text<'a>(source: &'a str, fact: &SyntaxFact) -> Result<&'a str, AdapterError> {
    let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid())?;
    let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid())?;
    source.get(start..end).ok_or_else(invalid)
}

fn invalid() -> AdapterError {
    provider_failure("matlab-binding-capture")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_rejects_cyclic_workspace_ancestry() {
        let workspaces = HashMap::from([
            (
                1,
                Workspace {
                    parent: Some(2),
                    captures: true,
                },
            ),
            (
                2,
                Workspace {
                    parent: Some(1),
                    captures: true,
                },
            ),
        ]);
        assert!(
            lookup(
                Some(1),
                "missing",
                &BTreeMap::new(),
                &workspaces,
                &Cancellation::new()
            )
            .is_err()
        );
    }

    #[test]
    fn lookup_keeps_missing_inner_identities_as_shadowing_barriers() {
        let workspaces = HashMap::from([
            (
                1,
                Workspace {
                    parent: None,
                    captures: true,
                },
            ),
            (
                2,
                Workspace {
                    parent: Some(1),
                    captures: true,
                },
            ),
        ]);
        let representatives = BTreeMap::from([((1, "value"), Some(10)), ((2, "value"), Some(20))]);
        assert_eq!(
            lookup(
                Some(2),
                "value",
                &representatives,
                &workspaces,
                &Cancellation::new()
            )
            .unwrap(),
            Some(20)
        );
    }

    #[test]
    fn lookup_observes_cancellation_before_returning_a_cached_binding() {
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(
            lookup(
                Some(1),
                "value",
                &BTreeMap::from([((1, "value"), Some(10))]),
                &HashMap::new(),
                &cancellation
            )
            .is_err()
        );
    }
}
