use std::{cell::Cell, fmt, rc::Rc};

use serde::{
    Deserialize,
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};

use crate::{BoundedReserveError, try_reserve_bounded};

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
    parse_bounded_with_state(input, limits).map(|(value, _state)| value)
}

fn parse_bounded_with_state(
    input: &[u8],
    limits: JsonLimits,
) -> Result<(Value, Rc<ParseState>), ParseFailure> {
    match preflight_string_limits(input, limits.max_string_bytes) {
        Ok(()) => {}
        Err(StringScanFailure::Limits) => {
            return Err(ParseFailure::Rejected(JsonIssue::Limits));
        }
        Err(StringScanFailure::Malformed) => return Err(ParseFailure::Malformed),
    }

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
    Ok((value, state))
}

#[cfg(test)]
pub(crate) fn parse_bounded_with_array_growths(
    input: &[u8],
    limits: JsonLimits,
) -> Result<(Value, usize), ParseFailure> {
    parse_bounded_with_state(input, limits)
        .map(|(value, state)| (value, state.array_reservation_growths.get()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StringScanFailure {
    Limits,
    Malformed,
}

fn preflight_string_limits(input: &[u8], maximum: usize) -> Result<(), StringScanFailure> {
    let mut index = 0usize;
    while let Some(byte) = input.get(index) {
        if *byte == b'"' {
            index = scan_string(input, index.saturating_add(1), maximum)?;
        } else {
            index = index.saturating_add(1);
        }
    }
    Ok(())
}

fn scan_string(input: &[u8], mut index: usize, maximum: usize) -> Result<usize, StringScanFailure> {
    let mut decoded_bytes = 0usize;
    loop {
        let byte = *input.get(index).ok_or(StringScanFailure::Malformed)?;
        match byte {
            b'"' => return Ok(index.saturating_add(1)),
            b'\\' => {
                let escaped_index = index.checked_add(1).ok_or(StringScanFailure::Malformed)?;
                let escaped = *input
                    .get(escaped_index)
                    .ok_or(StringScanFailure::Malformed)?;
                match escaped {
                    b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' => {
                        count_decoded_bytes(&mut decoded_bytes, 1, maximum)?;
                        index = escaped_index.saturating_add(1);
                    }
                    b'u' => {
                        let quad_index = escaped_index
                            .checked_add(1)
                            .ok_or(StringScanFailure::Malformed)?;
                        let (first, next_index) = parse_hex_quad(input, quad_index)?;
                        let character = if (0xD800..=0xDBFF).contains(&first) {
                            if input.get(next_index) != Some(&b'\\')
                                || input.get(next_index.saturating_add(1)) != Some(&b'u')
                            {
                                return Err(StringScanFailure::Malformed);
                            }
                            let low_index = next_index
                                .checked_add(2)
                                .ok_or(StringScanFailure::Malformed)?;
                            let (low, after_low) = parse_hex_quad(input, low_index)?;
                            if !(0xDC00..=0xDFFF).contains(&low) {
                                return Err(StringScanFailure::Malformed);
                            }
                            index = after_low;
                            let scalar = 0x1_0000
                                + ((u32::from(first) - 0xD800) << 10)
                                + (u32::from(low) - 0xDC00);
                            char::from_u32(scalar).ok_or(StringScanFailure::Malformed)?
                        } else {
                            if (0xDC00..=0xDFFF).contains(&first) {
                                return Err(StringScanFailure::Malformed);
                            }
                            index = next_index;
                            char::from_u32(u32::from(first)).ok_or(StringScanFailure::Malformed)?
                        };
                        count_decoded_bytes(&mut decoded_bytes, character.len_utf8(), maximum)?;
                    }
                    _ => return Err(StringScanFailure::Malformed),
                }
            }
            0x00..=0x1F => return Err(StringScanFailure::Malformed),
            0x20..=0x7F => {
                count_decoded_bytes(&mut decoded_bytes, 1, maximum)?;
                index = index.saturating_add(1);
            }
            _ => {
                let width = match byte {
                    0xC2..=0xDF => 2,
                    0xE0..=0xEF => 3,
                    0xF0..=0xF4 => 4,
                    _ => return Err(StringScanFailure::Malformed),
                };
                let end = index
                    .checked_add(width)
                    .ok_or(StringScanFailure::Malformed)?;
                let encoded = input.get(index..end).ok_or(StringScanFailure::Malformed)?;
                std::str::from_utf8(encoded).map_err(|_| StringScanFailure::Malformed)?;
                count_decoded_bytes(&mut decoded_bytes, width, maximum)?;
                index = end;
            }
        }
    }
}

fn parse_hex_quad(input: &[u8], index: usize) -> Result<(u16, usize), StringScanFailure> {
    let end = index.checked_add(4).ok_or(StringScanFailure::Malformed)?;
    let digits = input.get(index..end).ok_or(StringScanFailure::Malformed)?;
    let mut value = 0u16;
    for digit in digits {
        let nibble = match digit {
            b'0'..=b'9' => *digit - b'0',
            b'a'..=b'f' => *digit - b'a' + 10,
            b'A'..=b'F' => *digit - b'A' + 10,
            _ => return Err(StringScanFailure::Malformed),
        };
        value = value
            .checked_mul(16)
            .and_then(|current| current.checked_add(u16::from(nibble)))
            .ok_or(StringScanFailure::Malformed)?;
    }
    Ok((value, end))
}

fn count_decoded_bytes(
    decoded: &mut usize,
    additional: usize,
    maximum: usize,
) -> Result<(), StringScanFailure> {
    *decoded = decoded
        .checked_add(additional)
        .ok_or(StringScanFailure::Limits)?;
    if *decoded > maximum {
        return Err(StringScanFailure::Limits);
    }
    Ok(())
}

struct ParseState {
    limits: JsonLimits,
    nodes: Cell<usize>,
    issue: Cell<Option<JsonIssue>>,
    memory_unavailable: Cell<bool>,
    #[cfg(test)]
    array_reservation_growths: Cell<usize>,
}

impl ParseState {
    const fn new(limits: JsonLimits) -> Self {
        Self {
            limits,
            nodes: Cell::new(0),
            issue: Cell::new(None),
            memory_unavailable: Cell::new(false),
            #[cfg(test)]
            array_reservation_growths: Cell::new(0),
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

    fn reserve_array<E: serde::de::Error>(
        &self,
        values: &mut Vec<Value>,
        additional: usize,
    ) -> Result<(), E> {
        let _grew = match try_reserve_bounded(values, additional, self.limits.max_array_items) {
            Ok(grew) => grew,
            Err(BoundedReserveError::Limit) => {
                return Err(self.rejection(JsonIssue::Limits));
            }
            Err(BoundedReserveError::Memory) => {
                self.mark_memory_unavailable();
                return Err(E::custom("JSON memory is unavailable"));
            }
        };
        #[cfg(test)]
        if _grew {
            self.array_reservation_growths
                .set(self.array_reservation_growths.get().saturating_add(1));
        }
        Ok(())
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
            self.state.reserve_array::<A::Error>(&mut values, bounded)?;
        }

        let mut item_count = 0usize;
        while let Some(value) = sequence
            .next_element_seed(self.child_seed(item_count < self.state.limits.max_array_items))?
        {
            item_count += 1;
            self.state.reserve_array::<A::Error>(&mut values, 1)?;
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
