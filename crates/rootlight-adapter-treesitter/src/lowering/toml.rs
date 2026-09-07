//! Resolves TOML data ownership independently of lexical AST nesting.
//! Headers use absolute paths and the latest array-table occurrence; dotted
//! pairs and inline containers inherit their actual local data address.

use super::*;

struct Address {
    digest: [u8; 32],
    parent: Option<usize>,
    qualified: String,
    depth: usize,
}

pub(super) fn resolve(
    facts: &[SyntaxFact],
    drafts: &mut HashMap<u64, EntityDraft>,
    unsupported: &mut BTreeSet<u64>,
    strings: &mut usize,
    limits: &IrLimits,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut arena = Arena(vec![Address {
        digest: [0; 32],
        parent: None,
        qualified: String::new(),
        depth: 0,
    }]);
    let mut ordered: Vec<_> = facts.iter().collect();
    ordered.sort_by_key(|fact| (fact.span().start_byte(), fact.depth(), fact.local_id()));
    let mut contexts = HashMap::<u64, Option<usize>>::new();
    let mut arrays = HashMap::<[u8; 32], u64>::new();
    let mut positions = HashMap::<Option<u64>, u64>::new();
    let mut definitions = HashMap::<[u8; 32], u64>::new();
    let mut addresses = HashMap::<u64, usize>::new();
    let mut file_module = None;
    for (index, fact) in ordered.iter().enumerate() {
        check_periodically(index, cancellation)?;
        let label = fact.syntax_kind().as_str();
        let parent = match fact.parent() {
            Some(parent) => contexts.get(&parent).cloned().flatten(),
            None => Some(0),
        };
        if fact.kind() == SyntaxFactKind::Module {
            if let Some(module) = drafts.get_mut(&fact.local_id()) {
                module.depth = 0;
                file_module = Some(fact.local_id());
            }
            contexts.insert(fact.local_id(), Some(0));
            continue;
        }
        if label == "toml.array_element.scope" {
            let address = if let Some(parent) = parent {
                let position = positions.entry(fact.parent()).or_default();
                let address =
                    arena.extend(parent, Component::Element(*position), strings, limits)?;
                *position = position
                    .checked_add(1)
                    .ok_or(SinkError::AccountingOverflow)?;
                Some(address)
            } else {
                None
            };
            contexts.insert(fact.local_id(), address);
            continue;
        }
        let Some(draft) = drafts.get(&fact.local_id()) else {
            // A rejected declaration must poison its local container, rather
            // than silently lifting nested keys into an outer valid table.
            let context = if fact.kind() == SyntaxFactKind::Declaration {
                None
            } else {
                parent
            };
            contexts.insert(fact.local_id(), context);
            continue;
        };
        let header = matches!(
            label,
            "toml.table.declaration" | "toml.table_array_element.declaration"
        );
        let base = if header { Some(0) } else { parent };
        let Some(mut address) = base else {
            unsupported.insert(fact.local_id());
            contexts.insert(fact.local_id(), None);
            continue;
        };
        let mut parts = Segments(draft.name.as_str()).peekable();
        let mut leaf = None;
        while let Some(part) = parts.next() {
            cancellation.check()?;
            address = arena.extend(address, Component::Key(part), strings, limits)?;
            let digest = arena.get(address)?.digest;
            let terminal = parts.peek().is_none();
            if terminal && label == "toml.table_array_element.declaration" {
                let position = match arrays.get(&digest) {
                    Some(previous) => previous
                        .checked_add(1)
                        .ok_or(SinkError::AccountingOverflow)?,
                    None => 0,
                };
                arrays.insert(digest, position);
                address = arena.extend(address, Component::Element(position), strings, limits)?;
            } else if header
                && !terminal
                && let Some(position) = arrays.get(&digest)
            {
                address = arena.extend(address, Component::Element(*position), strings, limits)?;
            }
            leaf = Some(part);
        }
        let leaf = leaf.ok_or_else(|| provider_failure("treesitter-toml-key-path"))?;
        let node = arena.get(address)?;
        if let Some(previous) = definitions.insert(node.digest, fact.local_id()) {
            unsupported.insert(previous);
            unsupported.insert(fact.local_id());
        }
        account_string(strings, leaf.len(), limits)?;
        let name = leaf.to_owned();
        let draft = drafts
            .get_mut(&fact.local_id())
            .ok_or_else(|| provider_failure("treesitter-toml-draft-missing"))?;
        draft.name = name;
        draft.scope_identity = None;
        draft.scope_collision_guard = None;
        draft.data_identity = Some(node.digest);
        account_string(strings, node.qualified.len(), limits)?;
        draft.data_qualified_name = Some(node.qualified.clone());
        draft.qualified_length = node.qualified.len();
        draft.depth = node.depth;
        contexts.insert(fact.local_id(), Some(address));
        addresses.insert(fact.local_id(), address);
    }
    // Written super-table headers may follow their children. Resolve explicit
    // Contains owners after collecting addresses, then materialize by data depth.
    let mut addressed = addresses
        .iter()
        .map(|(&local, &address)| Ok((arena.get(address)?.depth, local, address)))
        .collect::<Result<Vec<_>, AdapterError>>()?;
    addressed.sort_unstable();
    for (index, (_, local, address)) in addressed.into_iter().enumerate() {
        check_periodically(index, cancellation)?;
        let mut parent = arena.get(address)?.parent;
        let mut owner = file_module;
        while let Some(address) = parent {
            cancellation.check()?;
            let address = arena.get(address)?;
            if let Some(candidate) = definitions.get(&address.digest) {
                if unsupported.contains(candidate) {
                    unsupported.insert(local);
                }
                owner = Some(*candidate);
                break;
            }
            parent = address.parent;
        }
        if let Some(draft) = drafts.get_mut(&local) {
            draft.parent_entity = owner;
        }
    }
    drafts.retain(|local, _| !unsupported.contains(local));
    Ok(())
}

enum Component<'a> {
    Key(&'a str),
    Element(u64),
}

// Integer parent links avoid recursive destruction of deeply dotted key paths.
struct Arena(Vec<Address>);

impl Arena {
    fn get(&self, index: usize) -> Result<&Address, AdapterError> {
        self.0
            .get(index)
            .ok_or_else(|| provider_failure("treesitter-toml-address-missing"))
    }

    fn extend(
        &mut self,
        parent_index: usize,
        component: Component<'_>,
        strings: &mut usize,
        limits: &IrLimits,
    ) -> Result<usize, AdapterError> {
        let parent = self.get(parent_index)?;
        let index_bytes;
        let index_display;
        let (kind, identity, separator, display) = match component {
            Component::Key(part) => (
                0,
                part.as_bytes(),
                if parent.qualified.is_empty() { "" } else { "." },
                part,
            ),
            Component::Element(position) => {
                index_bytes = position.to_be_bytes();
                index_display = format!("[{position}]");
                (1, index_bytes.as_slice(), "", index_display.as_str())
            }
        };
        let length = parent
            .qualified
            .len()
            .checked_add(separator.len())
            .and_then(|length| length.checked_add(display.len()))
            .ok_or(SinkError::AccountingOverflow)?;
        require_resource_limit(ResourceKind::StringBytes, length, limits.max_string_bytes)?;
        account_string(strings, length, limits)?;
        let mut qualified = String::new();
        qualified
            .try_reserve_exact(length)
            .map_err(|_| provider_failure("treesitter-toml-address-allocation"))?;
        qualified.push_str(&parent.qualified);
        qualified.push_str(separator);
        qualified.push_str(display);
        let mut hasher = blake3::Hasher::new_derive_key("rootlight.toml-data-address/1");
        hasher.update(&parent.digest);
        hasher.update(&[kind]);
        hasher.update(identity);
        let node = Address {
            digest: *hasher.finalize().as_bytes(),
            parent: Some(parent_index),
            qualified,
            depth: parent
                .depth
                .checked_add(1)
                .ok_or(SinkError::AccountingOverflow)?,
        };
        self.0
            .try_reserve(1)
            .map_err(|_| provider_failure("treesitter-toml-address-allocation"))?;
        let index = self.0.len();
        self.0.push(node);
        Ok(index)
    }
}

// Inputs have already passed the SDK's bounded canonical-key parser. Escaped
// quotes are skipped so a quoted dot can never become a path separator.
struct Segments<'a>(&'a str);

impl<'a> Iterator for Segments<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<Self::Item> {
        let mut escaped = false;
        for (index, character) in self.0.char_indices().skip(1) {
            if escaped {
                escaped = false;
                continue;
            }
            if character == '\\' {
                escaped = true;
                continue;
            }
            if character == '"' {
                let end = index.checked_add(1)?;
                let part = self.0.get(..end)?;
                self.0 = self.0.get(end..)?.strip_prefix('.').unwrap_or("");
                return Some(part);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn data_address_storage_obeys_both_string_limits() {
        let mut arena = Arena(vec![Address {
            digest: [0; 32],
            parent: None,
            qualified: String::new(),
            depth: 0,
        }]);
        let mut limits = IrLimits::default();
        limits.max_string_bytes = 9;
        limits.max_total_string_bytes = 14;
        let mut strings = 0;
        let a = arena
            .extend(0, Component::Key("\"abc\""), &mut strings, &limits)
            .unwrap();
        let b = arena
            .extend(a, Component::Key("\"x\""), &mut strings, &limits)
            .unwrap();
        assert_eq!(arena.get(b).unwrap().qualified, "\"abc\".\"x\"");
        assert_eq!(strings, 14);
        assert!(
            arena
                .extend(a, Component::Element(0), &mut strings, &limits)
                .is_err()
        );
        assert_eq!(arena.0.len(), 3);
        limits.max_total_string_bytes = 1_000;
        assert!(
            arena
                .extend(b, Component::Key("\"y\""), &mut strings, &limits)
                .is_err()
        );
        assert_eq!(arena.0.len(), 3);
    }
}
