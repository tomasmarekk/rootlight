//! Written Nix attribute namespaces shared by lowering and lexical resolution.
//! Native value scopes distinguish mergeable sets from evaluated expressions;
//! the first set determines recursion even when later definitions add fields.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use rootlight_adapter_sdk::{AdapterError, DiagnosticCode, SyntaxFact};
use rootlight_cancel::Cancellation;

pub(super) struct Member {
    pub(super) draft: u64,
    pub(super) definition: u64,
}

pub(super) struct Group {
    pub(super) parent: u64,
    pub(super) name: String,
    pub(super) members: Vec<Member>,
    pub(super) is_set: bool,
    pub(super) recursive: bool,
    pub(super) valid: bool,
}

#[derive(Default)]
pub(super) struct Attributes {
    pub(super) groups: BTreeMap<u64, Group>,
    pub(super) scopes: HashMap<u64, u64>,
    pub(super) values: HashMap<u64, u64>,
    pub(super) owners: BTreeSet<u64>,
    pub(super) unknown: BTreeSet<u64>,
}

pub(super) fn boundary(fact: &SyntaxFact) -> bool {
    matches!(
        fact.syntax_kind().as_str(),
        "nix.file.module"
            | "nix.function.scope"
            | "nix.let.scope"
            | "nix.attrset.scope"
            | "nix.rec_attrset.scope"
            | "nix.bound_attrset.scope"
            | "nix.bound_rec_attrset.scope"
    )
}

impl Attributes {
    pub(super) fn build(
        facts: &[SyntaxFact],
        source: &[u8],
        maximum: usize,
        cancellation: &Cancellation,
    ) -> Result<Self, AdapterError> {
        cancellation.check()?;
        let mut plan = Self::default();
        let mut by_id = HashMap::new();
        let mut paths = BTreeMap::<u64, Vec<&SyntaxFact>>::new();
        let mut definitions = HashMap::new();
        let mut ambiguous_definitions = BTreeSet::new();
        let mut sets = HashMap::new();
        for fact in facts {
            cancellation.check()?;
            if by_id.insert(fact.local_id(), fact).is_some() {
                return Err(invalid());
            }
            let Some(owner) = fact.parent() else { continue };
            match fact.syntax_kind().as_str() {
                "nix.path_segment.definition_part" | "nix.dynamic_segment.definition_part" => {
                    paths.entry(owner).or_default().push(fact);
                }
                "nix.binding_name.definition" => {
                    if definitions.insert(owner, fact).is_some() {
                        ambiguous_definitions.insert(owner);
                    }
                }
                "nix.bound_attrset.scope" | "nix.bound_rec_attrset.scope" => {
                    sets.insert(owner, fact);
                }
                _ => {}
            }
        }
        for (&owner, &definition) in &definitions {
            if by_id
                .get(&owner)
                .is_some_and(|fact| fact.syntax_kind().as_str() == "nix.variable.declaration")
            {
                paths.entry(owner).or_insert_with(|| vec![definition]);
            }
        }
        let mut order: Vec<_> = paths.keys().copied().collect();
        crate::runtime::sort_cancellable_by(&mut order, cancellation, |left, right| {
            by_id
                .get(left)
                .map(|fact| (fact.span().start_byte(), fact.depth(), fact.local_id()))
                .cmp(
                    &by_id
                        .get(right)
                        .map(|fact| (fact.span().start_byte(), fact.depth(), fact.local_id())),
                )
        })?;
        let mut keys = HashMap::<(u64, String), u64>::new();
        for owner in order {
            let mut parts = paths.remove(&owner).ok_or_else(invalid)?;
            cancellation.check()?;
            let declaration = by_id.get(&owner).ok_or_else(invalid)?;
            let Some(mut parent) = plan.nearest(declaration.parent(), &by_id, cancellation)? else {
                continue;
            };
            crate::runtime::sort_cancellable_by(&mut parts, cancellation, |left, right| {
                left.span().start_byte().cmp(&right.span().start_byte())
            })?;
            plan.owners.insert(owner);
            for (position, part) in parts.iter().enumerate() {
                cancellation.check()?;
                let start = usize::try_from(part.span().start_byte()).map_err(|_| invalid())?;
                let end = usize::try_from(part.span().end_byte()).map_err(|_| invalid())?;
                let written = std::str::from_utf8(source.get(start..end).ok_or_else(invalid)?)
                    .map_err(|_| invalid())?;
                let Some(name) = rootlight_adapter_sdk::nix_static_attribute_name(
                    written,
                    maximum,
                    cancellation,
                )?
                else {
                    if part.syntax_kind().as_str() != "nix.dynamic_segment.definition_part" {
                        plan.unknown.insert(parent);
                    }
                    plan.values.insert(owner, parent);
                    break;
                };
                let terminal = position + 1 == parts.len();
                let set = sets.get(&owner).filter(|_| terminal);
                let is_set = !terminal || set.is_some();
                let member = Member {
                    draft: if terminal { owner } else { part.local_id() },
                    definition: if terminal {
                        definitions
                            .get(&owner)
                            .map_or(part.local_id(), |fact| fact.local_id())
                    } else {
                        part.local_id()
                    },
                };
                let id = *keys
                    .entry((parent, name.to_string()))
                    .or_insert(member.draft);
                let group = plan.groups.entry(id).or_insert_with(|| Group {
                    parent,
                    name: name.into_owned(),
                    members: Vec::new(),
                    is_set,
                    recursive: set.is_some_and(|fact| {
                        fact.syntax_kind().as_str() == "nix.bound_rec_attrset.scope"
                    }),
                    valid: true,
                });
                if !group.members.is_empty() && (!group.is_set || !is_set) {
                    group.valid = false;
                }
                if terminal && ambiguous_definitions.contains(&owner) {
                    group.valid = false;
                }
                group.members.push(member);
                if terminal {
                    plan.values.insert(owner, parent);
                    if let Some(scope) = set {
                        plan.scopes.insert(scope.local_id(), id);
                    }
                } else {
                    parent = id;
                }
            }
        }
        Ok(plan)
    }

    fn nearest(
        &self,
        mut parent: Option<u64>,
        facts: &HashMap<u64, &SyntaxFact>,
        cancellation: &Cancellation,
    ) -> Result<Option<u64>, AdapterError> {
        for _ in 0..=facts.len() {
            cancellation.check()?;
            let Some(id) = parent else { return Ok(None) };
            if let Some(&scope) = self.scopes.get(&id).or_else(|| self.values.get(&id)) {
                return Ok(Some(scope));
            }
            let fact = facts.get(&id).ok_or_else(invalid)?;
            if boundary(fact) {
                return Ok(Some(id));
            }
            parent = fact.parent();
        }
        Err(invalid())
    }
}

fn invalid() -> AdapterError {
    AdapterError::ProviderFailed {
        code: DiagnosticCode::new("nix-attribute-namespace-capture")
            .expect("built-in Nix capture diagnostic is valid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancelled_namespace_planning_stops_before_empty_input() {
        let cancellation = Cancellation::new();
        cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
        assert!(matches!(
            Attributes::build(&[], b"", 64, &cancellation),
            Err(AdapterError::Cancelled { .. })
        ));
    }
}
