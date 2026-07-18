//! Bounded JSON decoding for untrusted MCP standard-stream frames.
//!
//! The allocation-free raw string preflight must remain before serde because
//! serde may allocate decoded escaped keys and values before visitor callbacks.
//! Visitor limit checks remain as defense in depth for every decoded value.

use std::{cell::Cell, fmt, rc::Rc};

use serde::{
    Deserialize,
    de::{DeserializeSeed, MapAccess, SeqAccess, Visitor},
};
use serde_json::{Map, Number, Value};

use crate::{BoundedReserveError, MAX_SUPPORTED_JSON_DEPTH, try_reserve_bounded};

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

    let node_kinds = RawKindScanner::new(input, limits).scan()?;
    let state = Rc::new(ParseState::new(limits, node_kinds));
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
    if state.nodes.get() != state.node_kinds.len() {
        return Err(ParseFailure::Malformed);
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RawNodeKind {
    Number,
    Other,
}

// Lexical provenance keeps real number tokens distinct from serde_json's
// map-shaped arbitrary-precision transport without trusting its private key.
struct RawKindScanner<'a> {
    input: &'a [u8],
    limits: JsonLimits,
    index: usize,
    kinds: Vec<RawNodeKind>,
}

impl<'a> RawKindScanner<'a> {
    const fn new(input: &'a [u8], limits: JsonLimits) -> Self {
        Self {
            input,
            limits,
            index: 0,
            kinds: Vec::new(),
        }
    }

    fn scan(mut self) -> Result<Vec<RawNodeKind>, ParseFailure> {
        self.scan_value(0)?;
        self.skip_whitespace();
        if self.index != self.input.len() {
            return Err(ParseFailure::Malformed);
        }
        Ok(self.kinds)
    }

    fn scan_value(&mut self, depth: usize) -> Result<(), ParseFailure> {
        self.skip_whitespace();
        if depth > self.limits.max_depth || depth > MAX_SUPPORTED_JSON_DEPTH {
            return Err(ParseFailure::Rejected(JsonIssue::Limits));
        }
        let byte = *self.input.get(self.index).ok_or(ParseFailure::Malformed)?;
        match byte {
            b'{' => {
                self.push_kind(RawNodeKind::Other)?;
                self.scan_object(depth)
            }
            b'[' => {
                self.push_kind(RawNodeKind::Other)?;
                self.scan_array(depth)
            }
            b'"' => {
                self.push_kind(RawNodeKind::Other)?;
                self.index = scan_string(self.input, self.index.saturating_add(1), usize::MAX)
                    .map_err(string_scan_failure)?;
                Ok(())
            }
            b't' => {
                self.push_kind(RawNodeKind::Other)?;
                self.scan_literal(b"true")
            }
            b'f' => {
                self.push_kind(RawNodeKind::Other)?;
                self.scan_literal(b"false")
            }
            b'n' => {
                self.push_kind(RawNodeKind::Other)?;
                self.scan_literal(b"null")
            }
            b'-' | b'0'..=b'9' => {
                self.push_kind(RawNodeKind::Number)?;
                self.scan_number()
            }
            _ => Err(ParseFailure::Malformed),
        }
    }

    fn scan_object(&mut self, depth: usize) -> Result<(), ParseFailure> {
        self.index = self.index.saturating_add(1);
        self.skip_whitespace();
        if self.input.get(self.index) == Some(&b'}') {
            self.index = self.index.saturating_add(1);
            return Ok(());
        }

        let mut properties = 0usize;
        loop {
            if properties >= self.limits.max_object_properties
                || self.input.get(self.index) != Some(&b'"')
            {
                return if properties >= self.limits.max_object_properties {
                    Err(ParseFailure::Rejected(JsonIssue::Limits))
                } else {
                    Err(ParseFailure::Malformed)
                };
            }
            self.index = scan_string(self.input, self.index.saturating_add(1), usize::MAX)
                .map_err(string_scan_failure)?;
            self.skip_whitespace();
            if self.input.get(self.index) != Some(&b':') {
                return Err(ParseFailure::Malformed);
            }
            self.index = self.index.saturating_add(1);
            properties = properties
                .checked_add(1)
                .ok_or(ParseFailure::Rejected(JsonIssue::Limits))?;
            self.scan_value(depth.saturating_add(1))?;
            self.skip_whitespace();
            match self.input.get(self.index) {
                Some(b',') => {
                    self.index = self.index.saturating_add(1);
                    self.skip_whitespace();
                }
                Some(b'}') => {
                    self.index = self.index.saturating_add(1);
                    return Ok(());
                }
                _ => return Err(ParseFailure::Malformed),
            }
        }
    }

    fn scan_array(&mut self, depth: usize) -> Result<(), ParseFailure> {
        self.index = self.index.saturating_add(1);
        self.skip_whitespace();
        if self.input.get(self.index) == Some(&b']') {
            self.index = self.index.saturating_add(1);
            return Ok(());
        }

        let mut items = 0usize;
        loop {
            if items >= self.limits.max_array_items {
                return Err(ParseFailure::Rejected(JsonIssue::Limits));
            }
            items = items
                .checked_add(1)
                .ok_or(ParseFailure::Rejected(JsonIssue::Limits))?;
            self.scan_value(depth.saturating_add(1))?;
            self.skip_whitespace();
            match self.input.get(self.index) {
                Some(b',') => {
                    self.index = self.index.saturating_add(1);
                    self.skip_whitespace();
                }
                Some(b']') => {
                    self.index = self.index.saturating_add(1);
                    return Ok(());
                }
                _ => return Err(ParseFailure::Malformed),
            }
        }
    }

    fn scan_literal(&mut self, literal: &[u8]) -> Result<(), ParseFailure> {
        let end = self
            .index
            .checked_add(literal.len())
            .ok_or(ParseFailure::Malformed)?;
        if self.input.get(self.index..end) != Some(literal) {
            return Err(ParseFailure::Malformed);
        }
        self.index = end;
        Ok(())
    }

    fn scan_number(&mut self) -> Result<(), ParseFailure> {
        if self.input.get(self.index) == Some(&b'-') {
            self.index = self.index.saturating_add(1);
        }

        match self.input.get(self.index) {
            Some(b'0') => {
                self.index = self.index.saturating_add(1);
                if self.input.get(self.index).is_some_and(u8::is_ascii_digit) {
                    return Err(ParseFailure::Malformed);
                }
            }
            Some(b'1'..=b'9') => {
                self.index = self.index.saturating_add(1);
                while self.input.get(self.index).is_some_and(u8::is_ascii_digit) {
                    self.index = self.index.saturating_add(1);
                }
            }
            _ => return Err(ParseFailure::Malformed),
        }

        if self.input.get(self.index) == Some(&b'.') {
            self.index = self.index.saturating_add(1);
            let fraction_start = self.index;
            while self.input.get(self.index).is_some_and(u8::is_ascii_digit) {
                self.index = self.index.saturating_add(1);
            }
            if self.index == fraction_start {
                return Err(ParseFailure::Malformed);
            }
        }

        if matches!(self.input.get(self.index), Some(b'e' | b'E')) {
            self.index = self.index.saturating_add(1);
            if matches!(self.input.get(self.index), Some(b'+' | b'-')) {
                self.index = self.index.saturating_add(1);
            }
            let exponent_start = self.index;
            while self.input.get(self.index).is_some_and(u8::is_ascii_digit) {
                self.index = self.index.saturating_add(1);
            }
            if self.index == exponent_start {
                return Err(ParseFailure::Malformed);
            }
        }
        Ok(())
    }

    fn push_kind(&mut self, kind: RawNodeKind) -> Result<(), ParseFailure> {
        match try_reserve_bounded(&mut self.kinds, 1, self.limits.max_nodes) {
            Ok(_) => {
                self.kinds.push(kind);
                Ok(())
            }
            Err(BoundedReserveError::Limit) => Err(ParseFailure::Rejected(JsonIssue::Limits)),
            Err(BoundedReserveError::Memory) => Err(ParseFailure::MemoryUnavailable),
        }
    }

    fn skip_whitespace(&mut self) {
        while matches!(
            self.input.get(self.index),
            Some(b' ' | b'\n' | b'\r' | b'\t')
        ) {
            self.index = self.index.saturating_add(1);
        }
    }
}

const fn string_scan_failure(failure: StringScanFailure) -> ParseFailure {
    match failure {
        StringScanFailure::Limits => ParseFailure::Rejected(JsonIssue::Limits),
        StringScanFailure::Malformed => ParseFailure::Malformed,
    }
}

struct ParseState {
    limits: JsonLimits,
    node_kinds: Vec<RawNodeKind>,
    nodes: Cell<usize>,
    issue: Cell<Option<JsonIssue>>,
    memory_unavailable: Cell<bool>,
    #[cfg(test)]
    array_reservation_growths: Cell<usize>,
}

impl ParseState {
    const fn new(limits: JsonLimits, node_kinds: Vec<RawNodeKind>) -> Self {
        Self {
            limits,
            node_kinds,
            nodes: Cell::new(0),
            issue: Cell::new(None),
            memory_unavailable: Cell::new(false),
            #[cfg(test)]
            array_reservation_growths: Cell::new(0),
        }
    }

    fn enter_node<E: serde::de::Error>(&self, depth: usize) -> Result<RawNodeKind, E> {
        let Some(nodes) = self.nodes.get().checked_add(1) else {
            return Err(self.rejection(JsonIssue::Limits));
        };
        if nodes > self.limits.max_nodes || depth > self.limits.max_depth {
            return Err(self.rejection(JsonIssue::Limits));
        }
        let kind = self
            .node_kinds
            .get(nodes.saturating_sub(1))
            .copied()
            .ok_or_else(|| E::custom("raw JSON node classification is inconsistent"))?;
        self.nodes.set(nodes);
        Ok(kind)
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
        match self.state.enter_node::<D::Error>(self.depth)? {
            // Number's public Deserialize implementation understands
            // serde_json's arbitrary-precision transport representation.
            RawNodeKind::Number => Number::deserialize(deserializer).map(Value::Number),
            RawNodeKind::Other => deserializer.deserialize_any(NodeVisitor {
                state: self.state,
                depth: self.depth,
            }),
        }
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
