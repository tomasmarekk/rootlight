use std::{
    cell::{Cell, RefCell},
    fmt,
    rc::Rc,
};

use serde::de::{DeserializeSeed, MapAccess, SeqAccess, Visitor};
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

pub(crate) struct ParsedJson {
    pub(crate) value: Value,
    pub(crate) issue: Option<JsonIssue>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParseFailure {
    Malformed,
    MemoryUnavailable,
}

pub(crate) fn parse_bounded(input: &[u8], limits: JsonLimits) -> Result<ParsedJson, ParseFailure> {
    let state = Rc::new(ParseState::new(limits));
    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = NodeSeed {
        state: Rc::clone(&state),
        depth: 0,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| {
        if state.memory_unavailable.get() {
            ParseFailure::MemoryUnavailable
        } else {
            ParseFailure::Malformed
        }
    })?;
    deserializer.end().map_err(|_| ParseFailure::Malformed)?;
    let issue = *state.issue.borrow();
    Ok(ParsedJson { value, issue })
}

struct ParseState {
    limits: JsonLimits,
    nodes: Cell<usize>,
    issue: RefCell<Option<JsonIssue>>,
    memory_unavailable: Cell<bool>,
}

impl ParseState {
    const fn new(limits: JsonLimits) -> Self {
        Self {
            limits,
            nodes: Cell::new(0),
            issue: RefCell::new(None),
            memory_unavailable: Cell::new(false),
        }
    }

    fn count_node(&self, depth: usize) {
        let Some(nodes) = self.nodes.get().checked_add(1) else {
            self.mark(JsonIssue::Limits);
            return;
        };
        self.nodes.set(nodes);
        if nodes > self.limits.max_nodes || depth > self.limits.max_depth {
            self.mark(JsonIssue::Limits);
        }
    }

    fn check_string(&self, value: &str) {
        if value.len() > self.limits.max_string_bytes {
            self.mark(JsonIssue::Limits);
        }
    }

    fn mark(&self, issue: JsonIssue) {
        let mut current = self.issue.borrow_mut();
        if current.is_none() || matches!(issue, JsonIssue::DuplicateName) {
            *current = Some(issue);
        }
    }

    fn mark_memory_unavailable(&self) {
        self.memory_unavailable.set(true);
    }
}

struct NodeSeed {
    state: Rc<ParseState>,
    depth: usize,
}

impl<'de> DeserializeSeed<'de> for NodeSeed {
    type Value = Value;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        self.state.count_node(self.depth);
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
    fn child_seed(&self) -> NodeSeed {
        NodeSeed {
            state: Rc::clone(&self.state),
            depth: self.depth.saturating_add(1),
        }
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

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        self.state.check_string(value);
        Ok(Value::String(value.to_owned()))
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.visit_str(value)
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        self.state.check_string(&value);
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
            let bounded = size.min(self.state.limits.max_array_items.saturating_add(1));
            values.try_reserve_exact(bounded).map_err(|_| {
                self.state.mark_memory_unavailable();
                serde::de::Error::custom("JSON memory is unavailable")
            })?;
        }

        let mut item_count = 0usize;
        while let Some(value) = sequence.next_element_seed(self.child_seed())? {
            item_count = item_count.saturating_add(1);
            if item_count > self.state.limits.max_array_items {
                self.state.mark(JsonIssue::Limits);
            }
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
        let mut fields = Vec::<(String, Value)>::new();
        if let Some(size) = object.size_hint() {
            let bounded = size.min(self.state.limits.max_object_properties.saturating_add(1));
            fields.try_reserve_exact(bounded).map_err(|_| {
                self.state.mark_memory_unavailable();
                serde::de::Error::custom("JSON memory is unavailable")
            })?;
        }

        let mut property_count = 0usize;
        while let Some(key) = object.next_key::<String>()? {
            property_count = property_count.saturating_add(1);
            self.state.check_string(&key);
            if property_count > self.state.limits.max_object_properties {
                self.state.mark(JsonIssue::Limits);
            }
            let duplicate = fields.iter().any(|(existing, _)| existing == &key);
            if duplicate {
                self.state.mark(JsonIssue::DuplicateName);
            }
            let value = object.next_value_seed(self.child_seed())?;
            if !duplicate {
                if fields.len() == fields.capacity() {
                    fields.try_reserve_exact(1).map_err(|_| {
                        self.state.mark_memory_unavailable();
                        serde::de::Error::custom("JSON memory is unavailable")
                    })?;
                }
                fields.push((key, value));
            }
        }

        let mut values = Map::new();
        for (key, value) in fields {
            values.insert(key, value);
        }
        Ok(Value::Object(values))
    }
}
