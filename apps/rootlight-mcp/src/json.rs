use std::{cell::Cell, fmt, rc::Rc};

use serde::{
    Deserialize,
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};

#[derive(Debug, Clone, Copy)]
pub(crate) struct JsonLimits {
    pub(crate) max_depth: usize,
    pub(crate) max_string_bytes: usize,
    pub(crate) max_object_properties: usize,
    pub(crate) max_array_items: usize,
    pub(crate) max_nodes: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum JsonIssue {
    Limits,
    DuplicateName,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseFailure {
    Malformed,
    Rejected(JsonIssue),
    MemoryUnavailable,
}

pub(crate) fn parse_bounded(input: &[u8], limits: JsonLimits) -> Result<Value, ParseFailure> {
    let state = Rc::new(ParseState::new(limits));
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = NodeSeed {
        state: Rc::clone(&state),
        depth: 0,
        allowed: true,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| {
        if state.memory_unavailable.get() {
            ParseFailure::MemoryUnavailable
        } else if let Some(issue) = state.issue.get() {
            ParseFailure::Rejected(issue)
        } else {
            ParseFailure::Malformed
        }
    })?;
    deserializer.end().map_err(|_| ParseFailure::Malformed)?;
    Ok(value)
}

struct ParseState {
    limits: JsonLimits,
    nodes: Cell<usize>,
    issue: Cell<Option<JsonIssue>>,
    memory_unavailable: Cell<bool>,
}

impl ParseState {
    const fn new(limits: JsonLimits) -> Self {
        Self {
            limits,
            nodes: Cell::new(0),
            issue: Cell::new(None),
            memory_unavailable: Cell::new(false),
        }
    }

    fn count_node<E: serde::de::Error>(&self, depth: usize) -> Result<(), E> {
        let Some(nodes) = self.nodes.get().checked_add(1) else {
            return Err(self.rejection(JsonIssue::Limits));
        };
        if nodes > self.limits.max_nodes || depth > self.limits.max_depth {
            return Err(self.rejection(JsonIssue::Limits));
        }
        self.nodes.set(nodes);
        Ok(())
    }

    fn check_string<E: serde::de::Error>(&self, value: &str) -> Result<(), E> {
        if value.len() > self.limits.max_string_bytes {
            return Err(self.rejection(JsonIssue::Limits));
        }
        Ok(())
    }

    fn rejection<E: serde::de::Error>(&self, issue: JsonIssue) -> E {
        self.issue.set(Some(issue));
        let message = match issue {
            JsonIssue::Limits => "JSON limits exceeded",
            JsonIssue::DuplicateName => "duplicate JSON object name",
        };
        E::custom(message)
    }

    fn mark_memory_unavailable(&self) {
        self.memory_unavailable.set(true);
    }
}

struct NodeSeed {
    state: Rc<ParseState>,
    depth: usize,
    allowed: bool,
}

impl<'de> DeserializeSeed<'de> for NodeSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if !self.allowed {
            return Err(self.state.rejection(JsonIssue::Limits));
        }
        self.state.count_node::<D::Error>(self.depth)?;
        deserializer.deserialize_any(NodeVisitor {
            state: self.state,
            depth: self.depth,
        })
    }
}

struct NodeVisitor {
    state: Rc<ParseState>,
    depth: usize,
}

impl NodeVisitor {
    fn child_seed(&self, allowed: bool) -> NodeSeed {
        NodeSeed {
            state: Rc::clone(&self.state),
            depth: self.depth.saturating_add(1),
            allowed,
        }
    }
}

struct ObjectKeySeed {
    state: Rc<ParseState>,
    allowed: bool,
}

impl<'de> DeserializeSeed<'de> for ObjectKeySeed {
    type Value = String;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if !self.allowed {
            return Err(self.state.rejection(JsonIssue::Limits));
        }
        let value = String::deserialize(deserializer)?;
        self.state.check_string::<D::Error>(&value)?;
        Ok(value)
    }
}

impl<'de> Visitor<'de> for NodeVisitor {
    type Value = Value;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(Value::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(Value::Number(Number::from(value)))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Number::from_f64(value)
            .map(Value::Number)
            .ok_or_else(|| E::custom("non-finite JSON number"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.state.check_string::<E>(value)?;
        Ok(Value::String(value.to_owned()))
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_str(value)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.state.check_string::<E>(&value)?;
        Ok(Value::String(value))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        if let Some(size) = sequence.size_hint() {
            let bounded = size.min(self.state.limits.max_array_items);
            values.try_reserve_exact(bounded).map_err(|_| {
                self.state.mark_memory_unavailable();
                serde::de::Error::custom("JSON memory is unavailable")
            })?;
        }

        let mut item_count = 0usize;
        while let Some(value) = sequence
            .next_element_seed(self.child_seed(item_count < self.state.limits.max_array_items))?
        {
            item_count += 1;
            if values.len() == values.capacity() {
                values.try_reserve_exact(1).map_err(|_| {
                    self.state.mark_memory_unavailable();
                    serde::de::Error::custom("JSON memory is unavailable")
                })?;
            }
            values.push(value);
        }
        Ok(Value::Array(values))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        let mut property_count = 0usize;
        while let Some(key) = object.next_key_seed(ObjectKeySeed {
            state: Rc::clone(&self.state),
            allowed: property_count < self.state.limits.max_object_properties,
        })? {
            property_count += 1;
            if values.contains_key(&key) {
                return Err(self.state.rejection(JsonIssue::DuplicateName));
            }
            let value = object.next_value_seed(self.child_seed(true))?;
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
