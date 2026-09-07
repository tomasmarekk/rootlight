//! Constructs collection-key identities from retained YAML node captures.
//! An iterative, memoized graph walk preserves aliases without expansion;
//! map order is discarded only after typed keys and values are constructed.

use super::*;
use rootlight_adapter_sdk::YamlCollectionKind;

pub(super) struct Input<'a, 'source> {
    pub(super) ordered: &'a [&'a SyntaxFact],
    pub(super) source: &'source str,
    pub(super) maximum: usize,
    pub(super) documents: &'a HashMap<u64, Option<u64>>,
    pub(super) contexts: &'a HashMap<u64, Option<YamlDocumentContext<'source>>>,
    pub(super) parents: &'a HashMap<u64, Option<SourceSpan>>,
    pub(super) owners: &'a HashMap<u64, u64>,
    pub(super) bindings: &'a super::super::Bindings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Node,
    Mapping,
    Sequence,
    Pair,
    ImplicitPair,
    Empty,
}

struct Node<'a> {
    fact: &'a SyntaxFact,
    role: Role,
    children: Vec<usize>,
    key: Option<usize>,
    alias: Option<usize>,
    tag: Option<SourceSpan>,
}

#[derive(Clone, Copy)]
struct Constructed {
    kind: Option<YamlCollectionKind>,
    digest: [u8; 32],
    content: [u8; 32],
    unknown_tag: bool,
    end: u64,
}

#[derive(Clone, Copy)]
enum Value {
    Node(Constructed),
    Pair(Constructed, Constructed),
}

#[derive(Clone, Copy)]
enum State {
    Pending,
    Visiting,
    Done(Option<Value>),
}

struct Graph<'a, 'source> {
    input: Input<'a, 'source>,
    nodes: Vec<Node<'a>>,
    keys: HashMap<u64, usize>,
    states: Vec<State>,
}

pub(super) fn complete_keys(
    input: Input<'_, '_>,
    keys: &mut HashMap<u64, Option<Key>>,
    warnings: &mut Vec<(SourceSpan, &'static str)>,
    cancellation: &Cancellation,
) -> Result<(), AdapterError> {
    let mut graph = Graph::new(input, cancellation)?;
    for (id, key) in keys.iter_mut().filter(|(_, key)| key.is_none()) {
        cancellation.check()?;
        let Some(index) = graph.keys.get(id).copied() else {
            continue;
        };
        let value = match graph.construct(index, cancellation)? {
            Some(Value::Node(value)) => value,
            Some(Value::Pair(key, _))
                if graph
                    .nodes
                    .get(index)
                    .is_some_and(|node| node.role == Role::ImplicitPair) =>
            {
                key
            }
            _ => continue,
        };
        let prefix = match value.kind {
            Some(YamlCollectionKind::Mapping) => "map:",
            Some(YamlCollectionKind::Sequence) => "seq:",
            None => continue,
        };
        let name = format!(
            "{prefix}{}",
            blake3::Hash::from_bytes(value.digest).to_hex()
        );
        if name.len() > graph.input.maximum {
            continue;
        }
        let span = graph.nodes.get(index).ok_or_else(invalid)?.fact.span();
        let span =
            SourceSpan::new(span.file(), span.start_byte(), value.end).map_err(|_| invalid())?;
        if value.unknown_tag {
            warnings.push((span, "yaml-key-tag-semantics-unknown"));
        }
        *key = Some(Key { name, source: span });
    }
    Ok(())
}

impl<'a, 'source> Graph<'a, 'source> {
    fn new(input: Input<'a, 'source>, cancellation: &Cancellation) -> Result<Self, AdapterError> {
        let mut nodes: Vec<Node<'a>> = Vec::new();
        nodes
            .try_reserve(input.ordered.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        let mut nearest = HashMap::<u64, Option<usize>>::new();
        let mut node_ids = HashMap::new();
        let mut keys = HashMap::new();
        let mut key_spans = HashMap::new();
        for fact in input.ordered {
            cancellation.check()?;
            let mut parent = fact
                .parent()
                .and_then(|id| nearest.get(&id).copied().flatten());
            let role = match fact.syntax_kind().as_str() {
                "yaml.document.scope" => {
                    parent = None;
                    None
                }
                "yaml.node.scope" | "yaml.key.scope" | "yaml.sequence_element.scope" => {
                    Some(Role::Node)
                }
                "yaml.mapping.scope" => Some(Role::Mapping),
                "yaml.sequence.scope" => Some(Role::Sequence),
                "yaml.property.declaration" => Some(Role::Pair),
                "yaml.empty_key.definition" => Some(Role::Empty),
                _ => None,
            };
            if let Some(role) = role {
                let index = nodes.len();
                if let Some(parent) = parent {
                    let children = &mut nodes.get_mut(parent).ok_or_else(invalid)?.children;
                    children
                        .try_reserve(1)
                        .map_err(|_| SinkError::AllocationFailed)?;
                    children.push(index);
                }
                nodes.push(Node {
                    fact,
                    role,
                    children: Vec::new(),
                    key: None,
                    alias: None,
                    tag: None,
                });
                node_ids.insert(fact.local_id(), index);
                parent = Some(index);
            }
            nearest.insert(fact.local_id(), parent);
            if matches!(
                fact.syntax_kind().as_str(),
                "yaml.node_key.definition" | "yaml.empty_key.definition"
            ) && let Some(index) = parent
            {
                keys.insert(fact.local_id(), index);
                key_spans.insert(fact.local_id(), fact.span());
            }
            if fact.syntax_kind().as_str() == "yaml.tag.signature"
                && let Some(index) = parent
            {
                nodes.get_mut(index).ok_or_else(invalid)?.tag = Some(fact.span());
            }
        }
        for (definition, index) in &keys {
            cancellation.check()?;
            if let Some(owner) = input
                .owners
                .get(definition)
                .and_then(|owner| node_ids.get(owner))
            {
                let node = nodes.get_mut(*owner).ok_or_else(invalid)?;
                node.key = Some(*index);
                if owner == index && key_spans.get(definition) == Some(&node.fact.span()) {
                    // A flow-map shorthand captures the whole node as both
                    // property and key. Its value is null, not a self-edge.
                    node.role = Role::ImplicitPair;
                }
            }
        }
        for (alias, anchor) in &input.bindings.aliases {
            cancellation.check()?;
            if let (Some(index), Some(target)) = (
                nearest.get(alias).copied().flatten(),
                nearest.get(anchor).copied().flatten(),
            ) {
                let node = nodes.get_mut(index).ok_or_else(invalid)?;
                if matches!(node.role, Role::Node | Role::ImplicitPair) {
                    node.alias = Some(target);
                }
            }
        }
        let order: Vec<_> = nodes
            .iter()
            .map(|node| {
                (
                    node.fact.span().start_byte(),
                    node.fact.depth(),
                    node.fact.local_id(),
                )
            })
            .collect();
        for node in &mut nodes {
            crate::runtime::sort_cancellable_by(&mut node.children, cancellation, |a, b| {
                order.get(*a).cmp(&order.get(*b))
            })?;
        }
        let mut states = Vec::new();
        states
            .try_reserve_exact(nodes.len())
            .map_err(|_| SinkError::AllocationFailed)?;
        states.resize(nodes.len(), State::Pending);
        Ok(Self {
            input,
            nodes,
            keys,
            states,
        })
    }

    fn construct(
        &mut self,
        root: usize,
        cancellation: &Cancellation,
    ) -> Result<Option<Value>, AdapterError> {
        if let State::Done(value) = *self.states.get(root).ok_or_else(invalid)? {
            return Ok(value);
        }
        let mut stack = vec![(root, 0_usize)];
        *self.states.get_mut(root).ok_or_else(invalid)? = State::Visiting;
        while let Some((index, next)) = stack.last_mut() {
            cancellation.check()?;
            let current = *index;
            let node = self.nodes.get(current).ok_or_else(invalid)?;
            let dependency = if *next < node.children.len() {
                node.children.get(*next).copied()
            } else if *next == node.children.len() {
                node.alias
            } else {
                None
            };
            if let Some(child) = dependency {
                *next = next.checked_add(1).ok_or(SinkError::AccountingOverflow)?;
                match *self.states.get(child).ok_or_else(invalid)? {
                    State::Pending => {
                        *self.states.get_mut(child).ok_or_else(invalid)? = State::Visiting;
                        stack
                            .try_reserve(1)
                            .map_err(|_| SinkError::AllocationFailed)?;
                        stack.push((child, 0));
                    }
                    State::Visiting => {
                        // YAML leaves cyclic equality implementation-defined.
                        // Preserve a gap instead of inventing a traversal-order identity.
                        *self.states.get_mut(current).ok_or_else(invalid)? = State::Done(None);
                        stack.pop();
                    }
                    State::Done(_) => {}
                }
            } else {
                let value = self.evaluate(current, cancellation)?;
                *self.states.get_mut(current).ok_or_else(invalid)? = State::Done(value);
                stack.pop();
            }
        }
        Ok(match *self.states.get(root).ok_or_else(invalid)? {
            State::Done(value) => value,
            _ => None,
        })
    }

    fn value(&self, index: usize) -> Option<Value> {
        match self.states.get(index)? {
            State::Done(value) => *value,
            _ => None,
        }
    }

    fn context(&self, fact: &SyntaxFact) -> Option<&YamlDocumentContext<'source>> {
        self.input
            .documents
            .get(&fact.local_id())
            .copied()
            .flatten()
            .and_then(|id| self.input.contexts.get(&id))?
            .as_ref()
    }

    fn scalar(&self, node: &Node<'_>) -> Option<Constructed> {
        let context = self.context(node.fact)?;
        let parent = self
            .input
            .parents
            .get(&node.fact.local_id())
            .copied()
            .flatten();
        let raw = text(self.input.source, node.fact.span()).ok()?;
        let empty_item = node.fact.syntax_kind().as_str() == "yaml.sequence_element.scope"
            && raw.strip_prefix('-').is_some_and(|rest| {
                let rest = rest.trim_matches([' ', '\t', '\r', '\n']);
                rest.is_empty() || rest.starts_with('#')
            });
        let (scalar, span) = if node.role == Role::Empty || empty_item {
            (context.flow_scalar("", None)?, node.fact.span())
        } else {
            node_scalar(
                context,
                self.input.source,
                node.fact,
                parent,
                self.input.maximum,
            )?
        };
        let mut hash = blake3::Hasher::new_derive_key("rootlight.yaml-node-scalar/1");
        hash.update(scalar.name().as_bytes());
        let digest = *hash.finalize().as_bytes();
        Some(Constructed {
            kind: None,
            digest,
            content: digest,
            unknown_tag: scalar.has_unrecognized_tag(),
            end: span.end_byte(),
        })
    }

    fn null(&self, node: &Node<'_>) -> Option<Constructed> {
        let scalar = self.context(node.fact)?.flow_scalar("", None)?;
        let mut hash = blake3::Hasher::new_derive_key("rootlight.yaml-node-scalar/1");
        hash.update(scalar.name().as_bytes());
        let digest = *hash.finalize().as_bytes();
        Some(Constructed {
            kind: None,
            digest,
            content: digest,
            unknown_tag: false,
            end: node.fact.span().end_byte(),
        })
    }

    fn tagged(&self, node: &Node<'_>, mut value: Constructed) -> Option<Constructed> {
        let kind = value.kind?;
        let raw = node
            .tag
            .map(|span| text(self.input.source, span))
            .transpose()
            .ok()?;
        let tag = self.context(node.fact)?.collection_tag(kind, raw)?;
        let mut hash = blake3::Hasher::new_derive_key("rootlight.yaml-node-collection/1");
        hash.update(tag.name().as_bytes());
        hash.update(&[0]);
        hash.update(&value.content);
        value.digest = *hash.finalize().as_bytes();
        value.unknown_tag |= tag.has_unrecognized_tag();
        value.end = value.end.max(node.fact.span().end_byte());
        Some(value)
    }

    fn mapping(
        &self,
        node: &Node<'_>,
        mut pairs: Vec<(Constructed, Constructed)>,
        cancellation: &Cancellation,
    ) -> Result<Option<Constructed>, AdapterError> {
        crate::runtime::sort_cancellable_by(&mut pairs, cancellation, |a, b| {
            a.0.digest.cmp(&b.0.digest)
        })?;
        if pairs
            .windows(2)
            .any(|pair| pair.first().map(|p| p.0.digest) == pair.get(1).map(|p| p.0.digest))
        {
            return Ok(None);
        }
        let mut hash = blake3::Hasher::new_derive_key("rootlight.yaml-mapping-content/1");
        let mut unknown_tag = false;
        let mut end = node.fact.span().end_byte();
        for (key, value) in pairs {
            cancellation.check()?;
            hash.update(&key.digest);
            hash.update(&value.digest);
            unknown_tag |= key.unknown_tag || value.unknown_tag;
            end = end.max(key.end).max(value.end);
        }
        Ok(self.tagged(
            node,
            Constructed {
                kind: Some(YamlCollectionKind::Mapping),
                digest: [0; 32],
                content: *hash.finalize().as_bytes(),
                unknown_tag,
                end,
            },
        ))
    }

    fn evaluate(
        &self,
        index: usize,
        cancellation: &Cancellation,
    ) -> Result<Option<Value>, AdapterError> {
        let node = self.nodes.get(index).ok_or_else(invalid)?;
        let value = match node.role {
            Role::Empty => self.scalar(node).map(Value::Node),
            Role::Node | Role::ImplicitPair => {
                let constructed = if let Some(target) = node.alias {
                    let target_value = match self.value(target) {
                        Some(Value::Pair(key, _))
                            if self
                                .nodes
                                .get(target)
                                .is_some_and(|node| node.role == Role::ImplicitPair) =>
                        {
                            Some(Value::Node(key))
                        }
                        value => value,
                    };
                    match (node.children.is_empty(), target_value) {
                        (true, Some(Value::Node(mut value))) => {
                            value.end = node.fact.span().end_byte();
                            Some(Value::Node(value))
                        }
                        _ => None,
                    }
                } else {
                    match node.children.as_slice() {
                        [] => self.scalar(node).map(Value::Node),
                        [child] => match self.value(*child) {
                            Some(Value::Node(mut value)) => {
                                value.end = value.end.max(node.fact.span().end_byte());
                                if node.tag.is_some() {
                                    self.tagged(node, value).map(Value::Node)
                                } else {
                                    Some(Value::Node(value))
                                }
                            }
                            Some(Value::Pair(key, value)) => {
                                if self
                                    .nodes
                                    .get(*child)
                                    .is_some_and(|node| node.role == Role::ImplicitPair)
                                {
                                    Some(Value::Pair(key, value))
                                } else {
                                    self.mapping(node, vec![(key, value)], cancellation)?
                                        .map(Value::Node)
                                }
                            }
                            None => None,
                        },
                        _ => None,
                    }
                };
                if node.role == Role::ImplicitPair {
                    match (constructed, self.null(node)) {
                        (Some(Value::Node(key)), Some(value)) => Some(Value::Pair(key, value)),
                        _ => None,
                    }
                } else {
                    constructed
                }
            }
            Role::Pair => {
                let Some(key_id) = node.key else {
                    return Ok(None);
                };
                let Some(Value::Node(key)) = self.value(key_id) else {
                    return Ok(None);
                };
                let mut values = node
                    .children
                    .iter()
                    .copied()
                    .filter(|child| *child != key_id);
                let value = match (values.next(), values.next()) {
                    (None, None) => self.null(node),
                    (Some(child), None) => match self.value(child) {
                        Some(Value::Node(value)) => Some(value),
                        _ => None,
                    },
                    _ => None,
                };
                value.map(|value| Value::Pair(key, value))
            }
            Role::Mapping => {
                let mut pairs = Vec::new();
                pairs
                    .try_reserve_exact(node.children.len())
                    .map_err(|_| SinkError::AllocationFailed)?;
                for child in &node.children {
                    cancellation.check()?;
                    let Some(Value::Pair(key, value)) = self.value(*child) else {
                        return Ok(None);
                    };
                    pairs.push((key, value));
                }
                self.mapping(node, pairs, cancellation)?.map(Value::Node)
            }
            Role::Sequence => {
                let mut hash = blake3::Hasher::new_derive_key("rootlight.yaml-sequence-content/1");
                let mut unknown_tag = false;
                let mut end = node.fact.span().end_byte();
                for child in &node.children {
                    cancellation.check()?;
                    let Some(Value::Node(value)) = self.value(*child) else {
                        return Ok(None);
                    };
                    hash.update(&value.digest);
                    unknown_tag |= value.unknown_tag;
                    end = end.max(value.end);
                }
                self.tagged(
                    node,
                    Constructed {
                        kind: Some(YamlCollectionKind::Sequence),
                        digest: [0; 32],
                        content: *hash.finalize().as_bytes(),
                        unknown_tag,
                        end,
                    },
                )
                .map(Value::Node)
            }
        };
        Ok(value)
    }
}
