//! Perl callable storage follows package names and source-ordered lexical aliases.
//! Plain declarations may fill an earlier lexical/our forward declaration; a new
//! lexical callable is not visible in its own body.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Storage<'a> {
    Package(u64, &'a str, &'a str),
    Lexical(u64),
}

#[derive(Clone, Copy)]
struct Alias<'a> {
    visible_from: u64,
    storage: Option<Storage<'a>>,
}

type Aliases<'a> = BTreeMap<(u64, &'a str), Vec<Alias<'a>>>;

pub(super) fn resolve<'a>(
    context: &Context<'a>,
    source: &'a str,
    drafts: &mut HashMap<u64, EntityDraft>,
    namespaces: &BTreeMap<(u64, &'a str), u64>,
    plan: &mut Plan,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    cancellation.check()?;
    let mut declarations = Vec::new();
    for fact in context.facts.values() {
        cancellation.check()?;
        if fact.syntax_kind().as_str() == "perl.function.declaration" {
            declarations.push(*fact);
        }
    }
    crate::runtime::sort_cancellable_by(&mut declarations, cancellation, |left, right| {
        left.span().start_byte().cmp(&right.span().start_byte())
    })?;
    let mut aliases = Aliases::new();
    let mut canonical = BTreeMap::new();
    for fact in declarations {
        cancellation.check()?;
        let Some(draft) = drafts.get(&fact.local_id()) else {
            continue;
        };
        let Some(definition) = draft.definition_local_id else {
            continue;
        };
        let Some(name) = text(source, context.fact(definition)?, limits.max_string_bytes)? else {
            continue;
        };
        let Some(scope_id) = fact.parent() else {
            return Err(invalid());
        };
        let scope = context.fact(scope_id)?;
        let lexical = match scope.syntax_kind().as_str() {
            "perl.lexical_function.scope" => true,
            "perl.function.scope" | "perl.our_function.scope" => false,
            _ => continue,
        };
        let Some(owner) = context.scope(scope.parent(), cancellation)? else {
            continue;
        };
        let alias = lexical || scope.syntax_kind().as_str() == "perl.our_function.scope";
        let storage = if name.contains("::") {
            if !alias
                && let Some((package, leaf)) = qualified_name(name, cancellation)?
                && context.package(fact, cancellation)?.is_some()
            {
                context
                    .module(fact, cancellation)?
                    .map(|root| Storage::Package(root, package, leaf))
            } else {
                None
            }
        } else if !identifier(name) {
            None
        } else if lexical {
            Some(Storage::Lexical(fact.local_id()))
        } else if !alias
            && let Some(prior) = visible(
                context,
                &aliases,
                Some(owner),
                name,
                fact.span().start_byte(),
                cancellation,
            )?
        {
            prior
        } else {
            match (
                context.module(fact, cancellation)?,
                context.package(fact, cancellation)?,
            ) {
                (Some(root), Some(package)) => Some(Storage::Package(root, package, name)),
                _ => None,
            }
        };
        if alias {
            aliases.entry((owner, name)).or_default().push(Alias {
                visible_from: fact.span().end_byte(),
                storage,
            });
        }
        let Some(storage) = storage else { continue };
        let first = *canonical.entry(storage).or_insert(fact.local_id());
        if first == fact.local_id() {
            // Perl does not overload a storage slot by signature. Written headers
            // remain separate occurrence evidence, including forward declarations.
            drafts
                .get_mut(&first)
                .ok_or_else(invalid)?
                .signature
                .clear();
            if let Storage::Package(root, package, name) = storage {
                callable_owner(
                    drafts.get_mut(&first).ok_or_else(invalid)?,
                    root,
                    package,
                    name,
                    namespaces,
                );
            }
        } else {
            // Coalesce storage identity, retaining each site's span and signature evidence.
            let original = drafts.get(&first).ok_or_else(invalid)?.clone();
            let draft = drafts.get_mut(&fact.local_id()).ok_or_else(invalid)?;
            draft.parent_entity = original.parent_entity;
            draft.scope_identity = original.scope_identity;
            draft.scope_collision_guard = original.scope_collision_guard;
            draft.lexical_module = original.lexical_module;
            draft.qualified_prefix = original.qualified_prefix;
            draft.signature = original.signature;
            draft.name = original.name;
        }
        plan.owned_functions.insert(fact.local_id());
    }
    let mut imports = BTreeMap::<(u64, &str, &str), u64>::new();
    for fact in context.facts.values() {
        cancellation.check()?;
        let word_list = fact.syntax_kind().as_str() == "perl.subs_word_list.reference";
        if !word_list && fact.syntax_kind().as_str() != "perl.subs_import.reference" {
            continue;
        }
        let Some(content) = text(source, fact, limits.max_string_bytes)? else {
            continue;
        };
        // A quoted import is one literal argument, not a whitespace-separated qw list.
        // Escapes and interpolated/dynamic names are not static binding evidence.
        if content.contains('\\') {
            continue;
        }
        if let (Some(root), Some(package)) = (
            context.module(fact, cancellation)?,
            context.package(fact, cancellation)?,
        ) {
            let mut words = content.split_ascii_whitespace();
            loop {
                cancellation.check()?;
                let name = if word_list {
                    let Some(word) = words.next() else { break };
                    word
                } else {
                    content
                };
                // Only retained callable storage can become a target in this file.
                // Bound lookup state by declarations, not the number of qw words.
                if identifier(name)
                    && name.chars().all(|ch| ch == '_' || ch.is_alphanumeric())
                    && canonical.contains_key(&Storage::Package(root, package, name))
                {
                    imports
                        .entry((root, package, name))
                        .and_modify(|start| *start = (*start).min(fact.span().start_byte()))
                        .or_insert(fact.span().start_byte());
                }
                if !word_list {
                    break;
                }
            }
        }
    }
    for fact in context.facts.values() {
        cancellation.check()?;
        let builtin = matches!(
            fact.syntax_kind().as_str(),
            "perl.builtin_function_name.reference" | "perl.importable_function_name.reference"
        );
        let code_reference = fact.syntax_kind().as_str() == "perl.code_function_name.reference";
        let bare = fact.syntax_kind().as_str() == "perl.bare_function_name.reference";
        let amper = code_reference
            || matches!(
                fact.syntax_kind().as_str(),
                "perl.amper_function_name.reference"
                    | "perl.direct_coderef_function_name.reference"
            );
        if !builtin
            && !bare
            && !amper
            && fact.syntax_kind().as_str() != "perl.static_function_name.reference"
        {
            continue;
        }
        let Some(name) = text(source, fact, limits.max_string_bytes)? else {
            continue;
        };
        let name = if amper {
            let Some(name) = name.strip_prefix('&') else {
                continue;
            };
            name
        } else {
            name
        };
        let Some(root) = context.module(fact, cancellation)? else {
            continue;
        };
        let storage = if builtin {
            // Native builtin syntax cannot see compile-time lexical pads. A visible
            // lexical callable wins even without (or over) a package subs import.
            if let Some(binding) = visible(
                context,
                &aliases,
                context.scope(fact.parent(), cancellation)?,
                name,
                fact.span().start_byte(),
                cancellation,
            )? {
                binding
            } else {
                context.package(fact, cancellation)?.and_then(|package| {
                    imports
                        .get(&(root, package, name))
                        .filter(|start| **start <= fact.span().start_byte())
                        .map(|_| Storage::Package(root, package, name))
                })
            }
        } else if name.contains("::") {
            qualified_name(name, cancellation)?
                .map(|(package, leaf)| Storage::Package(root, package, leaf))
        } else if identifier(name) {
            if let Some(binding) = visible(
                context,
                &aliases,
                context.scope(fact.parent(), cancellation)?,
                name,
                fact.span().start_byte(),
                cancellation,
            )? {
                binding
            } else {
                context
                    .package(fact, cancellation)?
                    .map(|package| Storage::Package(root, package, name))
            }
        } else {
            None
        };
        if let Some(storage) = storage
            && let Some(target) = canonical.get(&storage)
        {
            // A later package definition cannot retroactively make an earlier
            // bare term callable. A prior subs import does declare that slot.
            if bare && context.fact(*target)?.span().start_byte() > fact.span().start_byte() {
                let imported = match storage {
                    Storage::Package(root, package, name) => imports
                        .get(&(root, package, name))
                        .is_some_and(|start| *start <= fact.span().start_byte()),
                    Storage::Lexical(_) => false,
                };
                if !imported {
                    continue;
                }
            }
            plan.references.insert(fact.local_id(), *target);
            if !code_reference {
                plan.calls.insert(fact.local_id());
            }
        }
    }
    Ok(())
}

fn qualified_name<'a>(
    name: &'a str,
    cancellation: &Cancellation,
) -> Result<Option<(&'a str, &'a str)>, AdapterError> {
    let Some((package, leaf)) = name.rsplit_once("::") else {
        return Ok(None);
    };
    let Some(package) = canonical_package(package, cancellation)? else {
        return Ok(None);
    };
    Ok((package != "CORE" && identifier(leaf)).then_some((package, leaf)))
}

fn callable_owner(
    draft: &mut EntityDraft,
    root: u64,
    package: &str,
    name: &str,
    namespaces: &BTreeMap<(u64, &str), u64>,
) {
    let owner = namespaces.get(&(root, package)).copied();
    package_owner(draft, owner.unwrap_or(root));
    draft.name.clear();
    draft.name.push_str(name);
    if owner.is_none() && package != "main" {
        // A qualified declaration need not have a written package declaration.
        // A display prefix alone would merge equal leaf names from distinct packages.
        let mut identity = blake3::Hasher::new_derive_key("rootlight.perl-callable-package/1");
        identity.update(package.as_bytes());
        draft.parent_entity = None;
        draft.scope_identity = Some(*identity.finalize().as_bytes());
        draft.lexical_module = Some(root);
        draft.qualified_prefix = Some(package.to_owned());
    }
}

fn visible<'a>(
    context: &Context<'a>,
    aliases: &Aliases<'a>,
    mut scope: Option<u64>,
    name: &str,
    position: u64,
    cancellation: &Cancellation,
) -> Result<Option<Option<Storage<'a>>>, AdapterError> {
    for _ in 0..context.facts.len() {
        cancellation.check()?;
        let Some(id) = scope else { return Ok(None) };
        if let Some(bindings) = aliases.get(&(id, name)) {
            let after = bindings.partition_point(|binding| binding.visible_from <= position);
            if let Some(binding) = after.checked_sub(1).and_then(|index| bindings.get(index)) {
                return Ok(Some(binding.storage));
            }
        }
        let owner = context.fact(id)?;
        if owner.syntax_kind().as_str() == "perl.unsupported_context.scope" {
            return Ok(Some(None));
        }
        scope = context.scope(owner.parent(), cancellation)?;
    }
    Err(invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rootlight_adapter_sdk::SyntaxKindLabel;

    fn scope(id: u64, parent: Option<u64>, label: &str) -> SyntaxFact {
        SyntaxFact::new(
            id,
            parent,
            SyntaxFactKind::Scope,
            SourceSpan::new(FileId::from_bytes([7; 20]), 0, 100).unwrap(),
            0,
            SyntaxKindLabel::new(label).unwrap(),
        )
    }

    #[test]
    fn callable_visibility_keeps_source_order_and_unavailable_inner_barriers() {
        let outer = scope(1, None, "perl.file.scope");
        let inner = scope(2, Some(1), "perl.block.scope");
        let context = Context {
            facts: BTreeMap::from([(1, &outer), (2, &inner)]),
            events: BTreeMap::new(),
        };
        let aliases = Aliases::from([
            (
                (1, "target"),
                vec![Alias {
                    visible_from: 0,
                    storage: Some(Storage::Lexical(3)),
                }],
            ),
            (
                (2, "target"),
                vec![
                    Alias {
                        visible_from: 20,
                        storage: None,
                    },
                    Alias {
                        visible_from: 40,
                        storage: Some(Storage::Lexical(4)),
                    },
                ],
            ),
        ]);
        for (position, expected) in [
            (19, Some(Storage::Lexical(3))),
            (20, None),
            (39, None),
            (40, Some(Storage::Lexical(4))),
        ] {
            assert!(
                visible(
                    &context,
                    &aliases,
                    Some(2),
                    "target",
                    position,
                    &Cancellation::new()
                )
                .unwrap()
                    == Some(expected)
            );
        }
        assert!(
            visible(
                &context,
                &aliases,
                Some(1),
                "target",
                50,
                &Cancellation::new()
            )
            .unwrap()
                == Some(Some(Storage::Lexical(3)))
        );
    }

    #[test]
    fn callable_visibility_rejects_cyclic_or_missing_ancestry() {
        for parent in [2, 99] {
            let inner = scope(2, Some(parent), "perl.block.scope");
            let context = Context {
                facts: BTreeMap::from([(2, &inner)]),
                events: BTreeMap::new(),
            };
            assert!(matches!(
                visible(
                    &context,
                    &Aliases::new(),
                    Some(2),
                    "target",
                    0,
                    &Cancellation::new()
                ),
                Err(AdapterError::ProviderFailed { .. })
            ));
        }
    }

    #[test]
    fn callable_planning_observes_cancellation_before_empty_work() {
        let context = Context {
            facts: BTreeMap::new(),
            events: BTreeMap::new(),
        };
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            resolve(
                &context,
                "",
                &mut HashMap::new(),
                &BTreeMap::new(),
                &mut Plan::default(),
                &IrLimits::default(),
                &cancellation
            ),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
