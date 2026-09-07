//! Assigns source-occurrence addresses to YAML structural facts.
//! Documents, collection positions and duplicate keys remain distinct without
//! depending on offsets or scalar value bodies. This is not alias expansion.

use super::*;

mod bindings;
pub(super) mod names;
pub(super) use bindings::Bindings;

pub(super) struct Plan {
    pub(super) duplicates: BTreeSet<u64>,
    pub(super) aliases: HashMap<u64, u64>,
}

struct Address {
    digest: [u8; 32],
    qualified: String,
    depth: usize,
    owner: Option<u64>,
}

pub(super) fn resolve(
    facts: &[SyntaxFact],
    bindings: Bindings,
    drafts: &mut HashMap<u64, EntityDraft>,
    unsupported: &mut BTreeSet<u64>,
    strings: &mut usize,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<Plan, AdapterError> {
    let mut arena = Arena(vec![Address {
        digest: [0; 32],
        qualified: String::new(),
        depth: 0,
        owner: None,
    }]);
    let mut contexts = HashMap::<u64, Option<usize>>::new();
    let mut positions = HashMap::<(Option<u64>, &'static str), u64>::new();
    let mut members = HashMap::<([u8; 32], String), (u64, u64)>::new();
    let mut duplicates = BTreeSet::new();
    let mut ordered: Vec<_> = facts.iter().collect();
    ordered.sort_by_key(|fact| (fact.span().start_byte(), fact.depth(), fact.local_id()));

    for (index, fact) in ordered.into_iter().enumerate() {
        check_periodically(index, cancellation)?;
        let label = fact.syntax_kind().as_str();
        let parent = match fact.parent() {
            Some(parent) => contexts.get(&parent).copied().flatten(),
            None => Some(0),
        };
        if label == "yaml.anchor.declaration" && fact.kind() == SyntaxFactKind::Declaration {
            // Anchors annotate a node; they do not own or rename its data path.
            // Their serialization bindings remain valid even when a containing
            // complex key has no materializable data identity.
            if let Some(draft) = drafts.get_mut(&fact.local_id()) {
                if let Some(anchor) = bindings.anchors.get(&fact.local_id()) {
                    let document = arena.extend(
                        0,
                        Component::Position("document", anchor.document.ordinal),
                        strings,
                        limits,
                    )?;
                    let address = arena.extend(
                        document,
                        Component::Anchor(&draft.name, anchor.ordinal),
                        strings,
                        limits,
                    )?;
                    let node = arena.get(address)?;
                    draft.parent_entity = anchor.document.module;
                    draft.scope_identity = None;
                    draft.scope_collision_guard = None;
                    draft.data_identity = Some(node.digest);
                    account_string(strings, node.qualified.len(), limits)?;
                    draft.data_qualified_name = Some(node.qualified.clone());
                    draft.qualified_length = node.qualified.len();
                    draft.depth = node.depth;
                } else {
                    unsupported.insert(fact.local_id());
                }
            }
            contexts.insert(fact.local_id(), parent);
            continue;
        }
        let Some(parent) = parent else {
            if drafts.contains_key(&fact.local_id()) {
                unsupported.insert(fact.local_id());
            }
            contexts.insert(fact.local_id(), None);
            continue;
        };
        if label == "yaml.file.module" && fact.kind() == SyntaxFactKind::Module {
            let address = arena.extend(parent, Component::File, strings, limits)?;
            arena.get_mut(address)?.owner = Some(fact.local_id());
            if let Some(draft) = drafts.get_mut(&fact.local_id()) {
                draft.depth = 0;
            }
            contexts.insert(fact.local_id(), Some(address));
            continue;
        }
        if fact.kind() == SyntaxFactKind::Scope {
            let component = match label {
                "yaml.document.scope" | "yaml.sequence_element.scope" => {
                    let kind = if label == "yaml.document.scope" {
                        "document"
                    } else {
                        "element"
                    };
                    let next = positions.entry((fact.parent(), kind)).or_default();
                    let position = *next;
                    *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                    Some(Component::Position(kind, position))
                }
                "yaml.key.scope" => Some(Component::KeyNode),
                // A collection serving as an element may share one native
                // scope capture with that element. Only its data position,
                // not optional parser wrappers, belongs in the address.
                "yaml.mapping.scope"
                | "yaml.sequence.scope"
                | "yaml.value.scope"
                | "yaml.node.scope"
                | "yaml.file.scope" => None,
                _ => {
                    // Unknown ownership semantics cannot safely become an
                    // outer data path. Preserve source-bound descendant gaps.
                    contexts.insert(fact.local_id(), None);
                    continue;
                }
            };
            let address = match component {
                Some(component) => arena.extend(parent, component, strings, limits)?,
                None => parent,
            };
            contexts.insert(fact.local_id(), Some(address));
            continue;
        }
        if fact.kind() != SyntaxFactKind::Declaration {
            contexts.insert(fact.local_id(), Some(parent));
            continue;
        }
        let Some(draft) = drafts.get_mut(&fact.local_id()) else {
            // A key we cannot identify owns its complete subtree, not the next
            // identifiable ancestor. Do not lift children out of complex keys.
            contexts.insert(fact.local_id(), None);
            continue;
        };
        if label != "yaml.property.declaration" {
            unsupported.insert(fact.local_id());
            contexts.insert(fact.local_id(), None);
            continue;
        }
        let parent_node = arena.get(parent)?;
        account_string(strings, draft.name.len(), limits)?;
        let occurrence = members
            .entry((parent_node.digest, draft.name.clone()))
            .or_insert((0, fact.local_id()));
        let ordinal = occurrence.0;
        if ordinal != 0 {
            duplicates.insert(occurrence.1);
            duplicates.insert(fact.local_id());
        }
        occurrence.0 = ordinal
            .checked_add(1)
            .ok_or(SinkError::AccountingOverflow)?;
        let owner = parent_node.owner;
        let address = arena.extend(
            parent,
            Component::Member(&draft.name, ordinal),
            strings,
            limits,
        )?;
        let node = arena.get_mut(address)?;
        node.owner = Some(fact.local_id());
        draft.parent_entity = owner;
        draft.scope_identity = None;
        draft.scope_collision_guard = None;
        draft.data_identity = Some(node.digest);
        account_string(strings, node.qualified.len(), limits)?;
        draft.data_qualified_name = Some(node.qualified.clone());
        draft.qualified_length = node.qualified.len();
        draft.depth = node.depth;
        contexts.insert(fact.local_id(), Some(address));
    }
    drafts.retain(|local, _| !unsupported.contains(local));
    Ok(Plan {
        duplicates,
        aliases: bindings.aliases,
    })
}

enum Component<'a> {
    File,
    Position(&'static str, u64),
    KeyNode,
    Member(&'a str, u64),
    Anchor(&'a str, u64),
}

// Arena indices keep shared scope context cheap and destruction non-recursive.
struct Arena(Vec<Address>);

impl Arena {
    fn get(&self, index: usize) -> Result<&Address, AdapterError> {
        self.0
            .get(index)
            .ok_or_else(|| provider_failure("treesitter-yaml-address-missing"))
    }

    fn get_mut(&mut self, index: usize) -> Result<&mut Address, AdapterError> {
        self.0
            .get_mut(index)
            .ok_or_else(|| provider_failure("treesitter-yaml-address-missing"))
    }

    fn extend(
        &mut self,
        parent: usize,
        component: Component<'_>,
        strings: &mut usize,
        limits: &IrLimits,
    ) -> Result<usize, AdapterError> {
        let parent = self.get(parent)?;
        let display;
        let (kind, name, ordinal, suffix) = match component {
            Component::File => ("file", "", 0, ""),
            Component::KeyNode => ("key-node", "", 0, ".key"),
            Component::Position(kind, ordinal) => {
                display = if kind == "document" {
                    format!("document[{ordinal}]")
                } else {
                    format!("[{ordinal}]")
                };
                (kind, "", ordinal, display.as_str())
            }
            Component::Member(name, ordinal) => {
                let separator = if parent.qualified.is_empty() { "" } else { "." };
                let extra = if ordinal == 0 {
                    String::new()
                } else {
                    format!("#occurrence[{ordinal}]")
                };
                let length = name
                    .len()
                    .checked_add(separator.len())
                    .and_then(|length| length.checked_add(extra.len()))
                    .ok_or(SinkError::AccountingOverflow)?;
                require_resource_limit(ResourceKind::StringBytes, length, limits.max_string_bytes)?;
                display = format!("{separator}{name}{extra}");
                ("member", name, ordinal, display.as_str())
            }
            Component::Anchor(name, ordinal) => {
                let extra = format!("#occurrence[{ordinal}]");
                let length = name
                    .len()
                    .checked_add(2)
                    .and_then(|length| length.checked_add(extra.len()))
                    .ok_or(SinkError::AccountingOverflow)?;
                require_resource_limit(ResourceKind::StringBytes, length, limits.max_string_bytes)?;
                display = format!(".&{name}{extra}");
                ("anchor", name, ordinal, display.as_str())
            }
        };
        let length = parent
            .qualified
            .len()
            .checked_add(suffix.len())
            .ok_or(SinkError::AccountingOverflow)?;
        require_resource_limit(ResourceKind::StringBytes, length, limits.max_string_bytes)?;
        account_string(strings, length, limits)?;
        let mut qualified = String::new();
        qualified
            .try_reserve_exact(length)
            .map_err(|_| SinkError::AllocationFailed)?;
        qualified.push_str(&parent.qualified);
        qualified.push_str(suffix);
        let mut hasher = blake3::Hasher::new_derive_key("rootlight.yaml-data-occurrence/1");
        hasher.update(&parent.digest);
        hash_sized_bytes(&mut hasher, kind.as_bytes())?;
        hash_sized_bytes(&mut hasher, name.as_bytes())?;
        hasher.update(&ordinal.to_be_bytes());
        let node = Address {
            digest: *hasher.finalize().as_bytes(),
            qualified,
            depth: parent
                .depth
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?,
            owner: parent.owner,
        };
        self.0
            .try_reserve(1)
            .map_err(|_| SinkError::AllocationFailed)?;
        let index = self.0.len();
        self.0.push(node);
        Ok(index)
    }
}
