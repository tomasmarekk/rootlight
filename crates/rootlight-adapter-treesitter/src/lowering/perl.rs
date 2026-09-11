//! Perl package storage is shared by lexical our aliases and qualified reads.
//! Written declaration sites remain separate occurrences; package switches are
//! scoped source events, not nesting inferred from the spelling of a name.

use super::*;

mod functions;

use rootlight_ir::{PerlBinding, PerlCallableStorage};

pub(super) enum ProjectBinding {
    Definition(u64, PerlCallableStorage),
    Claim(PerlBinding),
}

pub(super) struct ProjectEvidence {
    pub(super) module: SourceSpan,
    pub(super) source: SourceSpan,
    pub(super) binding: ProjectBinding,
}

#[derive(Default)]
pub(super) struct Plan {
    pub(super) aliases: HashMap<u64, u64>,
    pub(super) references: HashMap<u64, u64>,
    pub(super) calls: BTreeSet<u64>,
    pub(super) owned_functions: BTreeSet<u64>,
    pub(super) project: Vec<ProjectEvidence>,
}

struct Context<'a> {
    facts: BTreeMap<u64, &'a SyntaxFact>,
    events: BTreeMap<u64, Vec<(u64, Option<&'a str>)>>,
}

pub(super) fn resolve(
    facts: &[SyntaxFact],
    source: &str,
    drafts: &mut HashMap<u64, EntityDraft>,
    complete: bool,
    strings: &mut usize,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<Plan, AdapterError> {
    cancellation.check()?;
    let mut plan = Plan::default();
    if !complete
        || !facts
            .iter()
            .any(|fact| fact.syntax_kind().as_str() == "perl.file.module")
    {
        return Ok(plan);
    }
    let mut context = Context {
        facts: BTreeMap::new(),
        events: BTreeMap::new(),
    };
    for fact in facts {
        cancellation.check()?;
        if context.facts.insert(fact.local_id(), fact).is_some() {
            return Err(invalid());
        }
    }
    let mut names = HashMap::new();
    let mut namespaces = BTreeMap::<(u64, &str), u64>::new();
    let mut namespace_sites = Vec::new();
    for fact in facts {
        cancellation.check()?;
        if fact.syntax_kind().as_str() != "perl.package.declaration" {
            continue;
        }
        let name = drafts
            .get(&fact.local_id())
            .and_then(|draft| draft.definition_local_id)
            .map(|id| {
                context
                    .fact(id)
                    .and_then(|definition| text(source, definition, limits.max_string_bytes))
            })
            .transpose()?
            .flatten();
        let name = match name {
            Some(name) => canonical_package(name, cancellation)?,
            None => None,
        };
        names.insert(fact.span(), name);
        if let (Some(name), Some(root)) = (name, context.module(fact, cancellation)?) {
            let key = (root, name);
            if namespaces.get(&key).is_none_or(|prior| {
                context
                    .facts
                    .get(prior)
                    .is_some_and(|prior| prior.span().start_byte() > fact.span().start_byte())
            }) {
                namespaces.insert(key, fact.local_id());
            }
            namespace_sites.push((fact.local_id(), root, name));
        }
    }
    for fact in facts {
        cancellation.check()?;
        let owner = match fact.syntax_kind().as_str() {
            "perl.package_block.scope" => Some(fact.local_id()),
            "perl.package_switch.scope" => context.scope(fact.parent(), cancellation)?,
            _ => continue,
        };
        if let Some(owner) = owner {
            context.events.entry(owner).or_default().push((
                fact.span().start_byte(),
                names.get(&fact.span()).copied().flatten(),
            ));
        }
    }
    for events in context.events.values_mut() {
        crate::runtime::sort_cancellable_by(events, cancellation, |left, right| {
            left.0.cmp(&right.0)
        })?;
    }
    let mut variables = BTreeMap::<(u64, &str, char, &str), u64>::new();
    let mut aliases = Vec::new();
    for fact in facts {
        cancellation.check()?;
        if fact.syntax_kind().as_str() != "perl.package_variable.declaration" {
            continue;
        }
        let Some(definition) = drafts
            .get(&fact.local_id())
            .and_then(|draft| draft.definition_local_id)
        else {
            continue;
        };
        let Some(name) = text(source, context.fact(definition)?, limits.max_string_bytes)? else {
            continue;
        };
        let Some((sigil, leaf)) = variable_name(name) else {
            continue;
        };
        let (Some(root), Some(package)) = (
            context.module(fact, cancellation)?,
            context.package(fact, cancellation)?,
        ) else {
            continue;
        };
        let key = (root, package, sigil, leaf);
        if variables.get(&key).is_none_or(|prior| {
            context
                .facts
                .get(prior)
                .is_some_and(|prior| prior.span().start_byte() > fact.span().start_byte())
        }) {
            variables.insert(key, fact.local_id());
        }
        aliases.push((definition, fact.local_id(), root, package));
    }
    // A package declaration's lexical placement does not create new storage.
    // The file module keeps independent embedded examples in separate universes.
    for (id, root, name) in namespace_sites {
        cancellation.check()?;
        let draft = drafts.get_mut(&id).ok_or_else(invalid)?;
        package_owner(draft, root);
        draft.name.clear();
        draft.name.push_str(name);
    }
    for (definition, id, root, package) in aliases {
        cancellation.check()?;
        let parent = namespaces.get(&(root, package)).copied().unwrap_or(root);
        let draft = drafts.get_mut(&id).ok_or_else(invalid)?;
        package_owner(draft, parent);
        plan.aliases.insert(definition, id);
    }
    functions::resolve(
        &context,
        source,
        drafts,
        &namespaces,
        &mut plan,
        limits,
        cancellation,
    )?;
    for fact in facts {
        cancellation.check()?;
        let Some(root) = context.module(fact, cancellation)? else {
            continue;
        };
        let binding = match fact.syntax_kind().as_str() {
            "perl.file.module" => Some(PerlBinding::ModuleContext),
            "perl.code_flow_barrier.expression" => Some(PerlBinding::DynamicWrite),
            "perl.use_module_name.reference" => {
                match text(source, fact, limits.max_string_bytes)? {
                    Some(name) => canonical_package(name, cancellation)?.map(|package| {
                        PerlBinding::ModuleLoad {
                            package: content_hash(package.as_bytes()),
                        }
                    }),
                    None => None,
                }
            }
            _ => None,
        };
        if let Some(binding) = binding {
            plan.project.push(ProjectEvidence {
                module: context.fact(root)?.span(),
                source: fact.span(),
                binding: ProjectBinding::Claim(binding),
            });
        }
    }
    refresh_layout(drafts, strings, limits, cancellation)?;
    for fact in facts {
        cancellation.check()?;
        let Some(root) = context.module(fact, cancellation)? else {
            continue;
        };
        let Some(name) = text(source, fact, limits.max_string_bytes)? else {
            continue;
        };
        if fact.syntax_kind().as_str() == "perl.package_context.reference" {
            if context.package_declaration(fact.parent(), cancellation)?
                && let Some(name) = canonical_package(name, cancellation)?
                && let Some(target) = namespaces.get(&(root, name))
            {
                plan.references.insert(fact.local_id(), *target);
            }
            continue;
        }
        let (sigil, name) = match fact.syntax_kind().as_str() {
            "perl.variable_name.reference" => {
                let Some(sigil @ ('$' | '@' | '%')) = name.chars().next() else {
                    continue;
                };
                (sigil, name.get(1..).ok_or_else(invalid)?)
            }
            "perl.array_container.reference" => ('@', name.get(1..).ok_or_else(invalid)?),
            "perl.hash_container.reference" => ('%', name.get(1..).ok_or_else(invalid)?),
            "perl.array_length.reference" => ('@', name.get(2..).ok_or_else(invalid)?),
            _ => continue,
        };
        let Some((package, leaf)) = name.rsplit_once("::") else {
            continue;
        };
        if let Some(package) = canonical_package(package, cancellation)?
            && identifier(leaf)
            && let Some(target) = variables.get(&(root, package, sigil, leaf))
        {
            plan.references.insert(fact.local_id(), *target);
        }
    }
    Ok(plan)
}

fn package_owner(draft: &mut EntityDraft, owner: u64) {
    draft.parent_entity = Some(owner);
    draft.scope_identity = None;
    draft.scope_collision_guard = None;
    draft.lexical_module = None;
    draft.qualified_prefix = None;
    draft.signature.clear();
}

pub(super) fn project_candidate(fact: &SyntaxFact) -> bool {
    matches!(
        fact.syntax_kind().as_str(),
        "perl.file.module"
            | "perl.function.declaration"
            | "perl.use_module_name.reference"
            | "perl.static_glob_write.expression"
            | "perl.dynamic_glob_write.expression"
            | "perl.code_flow_barrier.expression"
    ) || fact
        .syntax_kind()
        .as_str()
        .ends_with("function_name.reference")
        && fact.syntax_kind().as_str().starts_with("perl.")
}

fn callable_storage(package: &str, name: &str) -> PerlCallableStorage {
    PerlCallableStorage {
        package: content_hash(package.as_bytes()),
        name: content_hash(name.as_bytes()),
    }
}

fn refresh_layout(
    drafts: &mut HashMap<u64, EntityDraft>,
    strings: &mut usize,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let ids: Vec<_> = drafts
        .values()
        .filter(|draft| draft.language == "perl")
        .map(|draft| draft.local_id)
        .collect();
    let mut layouts = HashMap::<u64, (usize, usize)>::new();
    let mut chain = Vec::new();
    for id in ids {
        let mut parent = Some(id);
        while let Some(id) = parent {
            cancellation.check()?;
            if layouts.contains_key(&id) {
                break;
            }
            let draft = drafts.get(&id).ok_or_else(invalid)?;
            if draft.language != "perl" {
                layouts.insert(id, (draft.depth, draft.qualified_length));
                break;
            }
            if chain.len() == drafts.len() {
                return Err(invalid());
            }
            chain.push(id);
            parent = draft.parent_entity;
        }
        while let Some(id) = chain.pop() {
            cancellation.check()?;
            let draft = drafts.get_mut(&id).ok_or_else(invalid)?;
            let (depth, prefix) = match draft.parent_entity {
                Some(parent) => {
                    let &(depth, length) = layouts.get(&parent).ok_or_else(invalid)?;
                    (
                        depth.checked_add(1).ok_or(SinkError::AccountingOverflow)?,
                        length.checked_add(2).ok_or(SinkError::AccountingOverflow)?,
                    )
                }
                None => (
                    draft.depth,
                    draft.qualified_prefix.as_ref().map_or(Ok(0), |prefix| {
                        prefix
                            .len()
                            .checked_add(2)
                            .ok_or(SinkError::AccountingOverflow)
                    })?,
                ),
            };
            let length = prefix
                .checked_add(draft.name.len())
                .ok_or(SinkError::AccountingOverflow)?;
            require_resource_limit(ResourceKind::StringBytes, length, limits.max_string_bytes)?;
            account_string(
                strings,
                length.saturating_sub(draft.qualified_length),
                limits,
            )?;
            draft.depth = depth;
            draft.qualified_length = length;
            layouts.insert(id, (depth, length));
        }
    }
    Ok(())
}

impl<'a> Context<'a> {
    fn fact(&self, id: u64) -> Result<&'a SyntaxFact, AdapterError> {
        self.facts.get(&id).copied().ok_or_else(invalid)
    }

    fn scope(
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
            if crate::perl_bindings::is_scope(fact) {
                return Ok(Some(id));
            }
            parent = fact.parent();
        }
        Err(invalid())
    }

    fn module(
        &self,
        fact: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        let mut parent = Some(fact.local_id());
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(None);
            };
            let fact = self.fact(id)?;
            if fact.syntax_kind().as_str() == "perl.file.module" {
                return Ok(Some(id));
            }
            parent = fact.parent();
        }
        Err(invalid())
    }

    fn package(
        &self,
        fact: &SyntaxFact,
        cancellation: &Cancellation,
    ) -> Result<Option<&'a str>, AdapterError> {
        let mut scope = self.scope(fact.parent(), cancellation)?;
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = scope else {
                return Ok(None);
            };
            if let Some(events) = self.events.get(&id) {
                let end = events.partition_point(|event| event.0 <= fact.span().start_byte());
                if let Some(event) = end.checked_sub(1).and_then(|index| events.get(index)) {
                    return Ok(event.1);
                }
            }
            let owner = self.fact(id)?;
            match owner.syntax_kind().as_str() {
                "perl.file.scope" => return Ok(Some("main")),
                "perl.unsupported_context.scope" => return Ok(None),
                _ => scope = self.scope(owner.parent(), cancellation)?,
            }
        }
        Err(invalid())
    }

    fn package_declaration(
        &self,
        mut parent: Option<u64>,
        cancellation: &Cancellation,
    ) -> Result<bool, AdapterError> {
        for _ in 0..self.facts.len() {
            cancellation.check()?;
            let Some(id) = parent else {
                return Ok(false);
            };
            let fact = self.fact(id)?;
            if fact.kind() == SyntaxFactKind::Declaration {
                return Ok(fact.syntax_kind().as_str() == "perl.package.declaration");
            }
            parent = fact.parent();
        }
        Err(invalid())
    }
}

fn text<'a>(
    source: &'a str,
    fact: &SyntaxFact,
    maximum: usize,
) -> Result<Option<&'a str>, AdapterError> {
    let start = usize::try_from(fact.span().start_byte()).map_err(|_| invalid())?;
    let end = usize::try_from(fact.span().end_byte()).map_err(|_| invalid())?;
    let text = source.get(start..end).ok_or_else(invalid)?;
    Ok((text.len() <= maximum).then_some(text))
}

fn identifier(name: &str) -> bool {
    // Native identifiers may contain combining marks; ASCII-only validation
    // would discard valid source names already recognized by the grammar.
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|ch| ch == '_' || ch.is_alphabetic())
        && chars.all(|ch| {
            !ch.is_control()
                && !ch.is_whitespace()
                && !matches!(ch, ':' | '\'' | '{' | '}' | '$' | '@' | '%')
        })
}

fn package_name(name: &str) -> bool {
    name.split("::").all(identifier)
}

fn canonical_package<'a>(
    mut name: &'a str,
    cancellation: &Cancellation,
) -> Result<Option<&'a str>, AdapterError> {
    // Root qualifiers alias the main stash; a non-root `main` component does not.
    // Borrow the canonical suffix so repeated qualifiers need no new allocation.
    name = name.strip_prefix("::").unwrap_or(name);
    loop {
        cancellation.check()?;
        match name.strip_prefix("main::") {
            Some(suffix) => name = suffix,
            None => break,
        }
    }
    let name = if name.is_empty() { "main" } else { name };
    Ok(package_name(name).then_some(name))
}

fn variable_name(name: &str) -> Option<(char, &str)> {
    let sigil = name.chars().next()?;
    let leaf = name.get(1..)?;
    (matches!(sigil, '$' | '@' | '%') && identifier(leaf)).then_some((sigil, leaf))
}

fn invalid() -> AdapterError {
    provider_failure("perl-package-capture")
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;

    fn fact(id: u64, parent: Option<u64>, label: &str, start: u64, end: u64) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            SyntaxFactKind::Scope,
            SourceSpan::new(FileId::from_bytes([7; 20]), start, end).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn package_names_normalize_only_proven_root_aliases() {
        for (written, expected) in [
            ("", Some("main")),
            ("main", Some("main")),
            ("::Cove", Some("Cove")),
            ("main::Cove", Some("Cove")),
            ("main::main::Cove", Some("Cove")),
            ("::main::Cove", Some("Cove")),
            ("Cove::main", Some("Cove::main")),
            ("::::Cove", None),
            ("main::::Cove", None),
            ("Cove::", None),
            ("Cove::9", None),
        ] {
            assert_eq!(
                canonical_package(written, &Cancellation::new()).unwrap(),
                expected,
                "{written}"
            );
        }
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        for written in ["", "Cove", "main::main::Cove"] {
            assert!(matches!(
                canonical_package(written, &cancellation),
                Err(AdapterError::Cancelled { .. })
            ));
        }
    }

    #[test]
    fn package_plans_observe_cancellation_even_without_facts() {
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        for complete in [true, false] {
            assert!(matches!(
                resolve(
                    &[],
                    "",
                    &mut HashMap::new(),
                    complete,
                    &mut 0,
                    &IrLimits::default(),
                    &cancellation
                ),
                Err(AdapterError::Cancelled { .. })
            ));
        }
    }

    #[test]
    fn package_ancestry_rejects_missing_or_cyclic_parents() {
        for parent in [1, 99] {
            let item = fact(1, Some(parent), "perl.statement.scope", 0, 0);
            let context = Context {
                facts: BTreeMap::from([(1, &item)]),
                events: BTreeMap::new(),
            };
            assert!(matches!(
                context.module(&item, &Cancellation::new()),
                Err(AdapterError::ProviderFailed { .. })
            ));
            assert!(matches!(
                context.package(&item, &Cancellation::new()),
                Err(AdapterError::ProviderFailed { .. })
            ));
        }
    }

    #[test]
    fn package_text_rejects_invalid_spans_and_respects_name_budget() {
        let item = fact(1, None, "perl.package.scope", 0, 2);
        assert_eq!(text("é", &item, 2).unwrap(), Some("é"));
        assert_eq!(text("é", &item, 1).unwrap(), None);
        for (start, end) in [(1, 2), (0, 3)] {
            let item = fact(1, None, "perl.package.scope", start, end);
            assert!(matches!(
                text("é", &item, 64),
                Err(AdapterError::ProviderFailed { .. })
            ));
        }
    }
}
