//! Source-backed Nix attribute paths over written value dependencies.
//! Explicit dependency frames terminate alias cycles without evaluating code;
//! each selected component retains its own native reference and definition.

use super::{
    AdapterError, BTreeMap, BTreeSet, Cancellation, HashMap, NixBindings, SymbolId, SyntaxFact,
    invalid_capture, static_binding_name,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Value {
    Namespace(u64),
    Group(u64),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Task {
    Expression(u64),
    Group(u64),
    Component(u64, usize),
}

enum Step {
    Need(Task),
    Done(Option<Value>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum SelectionKind {
    Path,
    Inherited,
}

enum Owner {
    Declaration,
    Selection(SelectionKind),
}

struct Selection<'a> {
    kind: SelectionKind,
    base: Option<u64>,
    parts: Vec<&'a SyntaxFact>,
}

struct Plan<'a, 'facts> {
    bindings: &'a NixBindings<'facts>,
    source: &'facts [u8],
    selections: BTreeMap<u64, Selection<'facts>>,
    selection_ranges: HashMap<(u64, u64), u64>,
    set_ranges: HashMap<(u64, u64), u64>,
    expressions: HashMap<u64, Option<u64>>,
    inherited: HashMap<u64, Option<u64>>,
    inherited_selections: HashMap<u64, Option<Task>>,
    children: HashMap<u64, BTreeMap<&'a str, u64>>,
    targets: HashMap<u64, SymbolId>,
    symbol_groups: HashMap<SymbolId, Option<u64>>,
}

pub(super) fn resolve<'a>(
    bindings: &NixBindings<'a>,
    source: &'a [u8],
    symbols: &HashMap<u64, SymbolId>,
    cancellation: &Cancellation,
) -> Result<HashMap<u64, SymbolId>, AdapterError> {
    cancellation.check()?;
    if !bindings.facts.values().any(|fact| {
        matches!(
            fact.syntax_kind().as_str(),
            "nix.selected_attribute.reference" | "nix.inherited_attribute.reference"
        )
    }) {
        return Ok(HashMap::new());
    }
    let mut plan = Plan {
        bindings,
        source,
        selections: BTreeMap::new(),
        selection_ranges: HashMap::new(),
        set_ranges: HashMap::new(),
        expressions: HashMap::new(),
        inherited: HashMap::new(),
        inherited_selections: HashMap::new(),
        children: HashMap::new(),
        targets: HashMap::new(),
        symbol_groups: HashMap::new(),
    };
    for (&id, group) in &bindings.attributes.groups {
        cancellation.check()?;
        plan.children
            .entry(group.parent)
            .or_default()
            .insert(&group.name, id);
        let mut targets = group
            .members
            .iter()
            .map(|member| symbols.get(&member.definition).copied());
        if let Some(Some(symbol)) = targets.next()
            && group.valid
            && targets.all(|target| target == Some(symbol))
        {
            plan.targets.insert(id, symbol);
            plan.symbol_groups
                .entry(symbol)
                .and_modify(|target| {
                    if *target != Some(id) {
                        *target = None;
                    }
                })
                .or_insert(Some(id));
        }
    }
    for fact in bindings.facts.values() {
        cancellation.check()?;
        let label = fact.syntax_kind().as_str();
        if let Some(kind) = selection_kind(label) {
            plan.selections.insert(
                fact.local_id(),
                Selection {
                    kind,
                    base: None,
                    parts: Vec::new(),
                },
            );
            if plan
                .selection_ranges
                .insert(range(fact), fact.local_id())
                .is_some()
            {
                return Err(invalid_capture());
            }
        } else if matches!(
            label,
            "nix.attrset.scope"
                | "nix.rec_attrset.scope"
                | "nix.bound_attrset.scope"
                | "nix.bound_rec_attrset.scope"
        ) && plan
            .set_ranges
            .insert(range(fact), fact.local_id())
            .is_some()
        {
            return Err(invalid_capture());
        }
    }
    for fact in bindings.facts.values() {
        cancellation.check()?;
        let label = fact.syntax_kind().as_str();
        if label.starts_with("nix.binding_value_") {
            if let Some(owner) = plan.owner(fact, Owner::Declaration, cancellation)? {
                plan.expressions
                    .entry(owner)
                    .and_modify(|value| *value = None)
                    .or_insert(Some(fact.local_id()));
            }
        } else if label.starts_with("nix.selection_base_") || label.starts_with("nix.inherit_base_")
        {
            let kind = if label.starts_with("nix.inherit_base_") {
                SelectionKind::Inherited
            } else {
                SelectionKind::Path
            };
            if let Some(owner) = plan.owner(fact, Owner::Selection(kind), cancellation)? {
                let selection = plan
                    .selections
                    .get_mut(&owner)
                    .ok_or_else(invalid_capture)?;
                if selection.base.replace(fact.local_id()).is_some() {
                    return Err(invalid_capture());
                }
            }
        } else if matches!(
            label,
            "nix.selected_attribute.reference" | "nix.inherited_attribute.reference"
        ) {
            let kind = if label == "nix.inherited_attribute.reference" {
                SelectionKind::Inherited
            } else {
                SelectionKind::Path
            };
            if let Some(owner) = plan.owner(fact, Owner::Selection(kind), cancellation)? {
                plan.selections
                    .get_mut(&owner)
                    .ok_or_else(invalid_capture)?
                    .parts
                    .push(fact);
            }
        } else if label == "nix.inherited_name.reference"
            && let Some(owner) = plan.owner(fact, Owner::Declaration, cancellation)?
        {
            plan.inherited
                .entry(owner)
                .and_modify(|value| *value = None)
                .or_insert(Some(fact.local_id()));
        }
    }
    for selection in plan.selections.values_mut() {
        crate::runtime::sort_cancellable_by(&mut selection.parts, cancellation, |a, b| {
            a.span().start_byte().cmp(&b.span().start_byte())
        })?;
        for pair in selection.parts.windows(2) {
            cancellation.check()?;
            if let [left, right] = pair
                && left.span().end_byte() > right.span().start_byte()
            {
                return Err(invalid_capture());
            }
        }
    }
    for (&scope, selection) in &plan.selections {
        cancellation.check()?;
        if selection.kind == SelectionKind::Inherited {
            for (index, part) in selection.parts.iter().enumerate() {
                if let Some(owner) = plan.owner(part, Owner::Declaration, cancellation)? {
                    plan.inherited_selections
                        .entry(owner)
                        .and_modify(|value| *value = None)
                        .or_insert(Some(Task::Component(scope, index)));
                }
            }
        }
    }
    let mut cache = BTreeMap::<Task, Option<Value>>::new();
    let mut result = HashMap::new();
    for (&owner, selection) in &plan.selections {
        for (index, part) in selection.parts.iter().enumerate() {
            cancellation.check()?;
            if let Some(Value::Group(group)) =
                plan.evaluate(Task::Component(owner, index), &mut cache, cancellation)?
                && let Some(&symbol) = plan.targets.get(&group)
            {
                result.insert(part.local_id(), symbol);
            }
        }
    }
    Ok(result)
}

impl Plan<'_, '_> {
    fn owner(
        &self,
        fact: &SyntaxFact,
        wanted: Owner,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        let mut parent = fact.parent();
        for _ in 0..self.bindings.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else { return Ok(None) };
            let owner = self.bindings.fact(id)?;
            if let Owner::Selection(kind) = wanted {
                // A selected expression can itself be the base of another
                // selection; its own equal-range scope is not that outer owner.
                if selection_kind(owner.syntax_kind().as_str()) == Some(kind)
                    && range(owner) != range(fact)
                {
                    return Ok(Some(id));
                }
            } else if owner.syntax_kind().as_str().starts_with("nix.")
                && owner.syntax_kind().as_str().ends_with(".declaration")
            {
                return Ok(Some(id));
            }
            parent = owner.parent();
        }
        Err(invalid_capture())
    }

    fn written<'s>(&'s self, fact: &SyntaxFact) -> Result<&'s str, AdapterError> {
        let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid_capture())?;
        let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid_capture())?;
        std::str::from_utf8(self.source.get(start..end).ok_or_else(invalid_capture)?)
            .map_err(|_| invalid_capture())
    }

    fn variable(
        &self,
        fact: &SyntaxFact,
        inherited: bool,
        cancellation: &Cancellation,
    ) -> Result<Option<Value>, AdapterError> {
        let Some(name) = static_binding_name(
            self.written(fact)?,
            self.bindings.maximum_name_bytes,
            cancellation,
        )?
        else {
            return Ok(None);
        };
        let symbol = self.bindings.lookup(fact, &name, inherited, cancellation)?;
        Ok(symbol
            .and_then(|symbol| self.symbol_groups.get(&symbol).copied().flatten())
            .map(Value::Group))
    }

    fn evaluate(
        &self,
        root: Task,
        cache: &mut BTreeMap<Task, Option<Value>>,
        cancellation: &Cancellation,
    ) -> Result<Option<Value>, AdapterError> {
        cancellation.check()?;
        if let Some(value) = cache.get(&root) {
            return Ok(*value);
        }
        let mut stack = vec![root];
        let mut active = BTreeSet::from([root]);
        while let Some(&task) = stack.last() {
            cancellation.check()?;
            if cache.contains_key(&task) {
                stack.pop();
                active.remove(&task);
                continue;
            }
            match self.step(task, cache, cancellation)? {
                Step::Done(value) => {
                    cache.insert(task, value);
                }
                Step::Need(dependency) => {
                    if !active.insert(dependency) {
                        // Cyclic aliases cannot prove a concrete namespace.
                        cache.insert(dependency, None);
                    } else {
                        stack
                            .try_reserve(1)
                            .map_err(|_| rootlight_adapter_sdk::SinkError::AllocationFailed)?;
                        stack.push(dependency);
                    }
                }
            }
        }
        Ok(cache.get(&root).copied().flatten())
    }

    fn step(
        &self,
        task: Task,
        cache: &BTreeMap<Task, Option<Value>>,
        cancellation: &Cancellation,
    ) -> Result<Step, AdapterError> {
        match task {
            Task::Expression(id) => {
                let fact = self.bindings.fact(id)?;
                let label = fact.syntax_kind().as_str();
                if label.ends_with("_variable.expression") {
                    return Ok(Step::Done(self.variable(fact, false, cancellation)?));
                }
                if label.ends_with("_set.expression") {
                    return Ok(Step::Done(self.set_ranges.get(&range(fact)).map(|id| {
                        Value::Namespace(
                            self.bindings
                                .attributes
                                .scopes
                                .get(id)
                                .copied()
                                .unwrap_or(*id),
                        )
                    })));
                }
                if label.ends_with("_selection.expression")
                    && let Some(&scope) = self.selection_ranges.get(&range(fact))
                    && let Some(selection) = self.selections.get(&scope)
                    && let Some(index) = selection.parts.len().checked_sub(1)
                {
                    return Ok(dependency(Task::Component(scope, index), cache));
                }
                Ok(Step::Done(None))
            }
            Task::Group(id) => {
                let group = self
                    .bindings
                    .attributes
                    .groups
                    .get(&id)
                    .ok_or_else(invalid_capture)?;
                if !group.valid {
                    return Ok(Step::Done(None));
                }
                if group.is_set {
                    return Ok(Step::Done(Some(Value::Namespace(id))));
                }
                let Some(member) = group.members.first() else {
                    return Ok(Step::Done(None));
                };
                let value = if let Some(Some(expression)) = self.expressions.get(&member.draft) {
                    let dependency = Task::Expression(*expression);
                    let Some(value) = cache.get(&dependency) else {
                        return Ok(Step::Need(dependency));
                    };
                    *value
                } else if let Some(Some(selected)) = self.inherited_selections.get(&member.draft) {
                    let Some(value) = cache.get(selected) else {
                        return Ok(Step::Need(*selected));
                    };
                    *value
                } else if let Some(Some(reference)) = self.inherited.get(&member.draft) {
                    self.variable(self.bindings.fact(*reference)?, true, cancellation)?
                } else {
                    None
                };
                Ok(match value {
                    Some(Value::Group(group)) => dependency(Task::Group(group), cache),
                    value => Step::Done(value),
                })
            }
            Task::Component(scope, index) => {
                let selection = self.selections.get(&scope).ok_or_else(invalid_capture)?;
                // Inherited names are siblings selecting the same base, not
                // successive components of an attribute path.
                let prerequisite = if selection.kind == SelectionKind::Path
                    && let Some(previous) = index.checked_sub(1)
                {
                    Task::Component(scope, previous)
                } else if let Some(base) = selection.base {
                    Task::Expression(base)
                } else {
                    return Ok(Step::Done(None));
                };
                let Some(mut value) = cache.get(&prerequisite).copied() else {
                    return Ok(Step::Need(prerequisite));
                };
                if let Some(Value::Group(group)) = value {
                    let prerequisite = Task::Group(group);
                    let Some(namespace) = cache.get(&prerequisite) else {
                        return Ok(Step::Need(prerequisite));
                    };
                    value = *namespace;
                }
                let Some(Value::Namespace(namespace)) = value else {
                    return Ok(Step::Done(None));
                };
                if self.bindings.unmodeled_scopes.contains(&namespace) {
                    return Ok(Step::Done(None));
                }
                let part = selection.parts.get(index).ok_or_else(invalid_capture)?;
                let Some(name) = static_binding_name(
                    self.written(part)?,
                    self.bindings.maximum_name_bytes,
                    cancellation,
                )?
                else {
                    return Ok(Step::Done(None));
                };
                Ok(Step::Done(
                    self.children
                        .get(&namespace)
                        .and_then(|children| children.get(name.as_ref()))
                        .copied()
                        .filter(|id| {
                            self.bindings
                                .attributes
                                .groups
                                .get(id)
                                .is_some_and(|group| group.valid)
                        })
                        .map(Value::Group),
                ))
            }
        }
    }
}

fn dependency(task: Task, cache: &BTreeMap<Task, Option<Value>>) -> Step {
    cache
        .get(&task)
        .map_or(Step::Need(task), |value| Step::Done(*value))
}

fn selection_kind(label: &str) -> Option<SelectionKind> {
    match label {
        "nix.selection.scope" => Some(SelectionKind::Path),
        "nix.inherit_from.scope" => Some(SelectionKind::Inherited),
        _ => None,
    }
}

fn range(fact: &SyntaxFact) -> (u64, u64) {
    (fact.span().start_byte(), fact.span().end_byte())
}
