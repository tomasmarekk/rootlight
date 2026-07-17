//! Explicit bounded decoders for externally supplied benchmark JSON.
//!
//! Raw external inputs cross this module's byte, collection, string, digest,
//! path, and aggregate-size checks before becoming trusted model values.

use std::fmt;

use serde::{
    Deserialize,
    de::{DeserializeSeed as _, Error as _, IgnoredAny, MapAccess, SeqAccess, Visitor},
};

use crate::{
    BenchmarkCommand, BundleLimits, DatasetEntry, DatasetManifest, RESULT_BUNDLE_SCHEMA_VERSION,
    bundle::BundleError,
};

/// Decodes a strict dataset manifest under checked limits.
///
/// # Errors
///
/// Returns [`DecodeError`] for an invalid limit set, oversized input, malformed
/// JSON, unknown fields, non-canonical identifiers, digests or paths, and
/// collection or declared-byte limit violations.
pub fn decode_dataset_manifest(
    bytes: &[u8],
    limits: BundleLimits,
) -> Result<DatasetManifest, DecodeError> {
    decode_dataset_manifest_with_control(bytes, limits, None)
}

fn decode_dataset_manifest_with_control(
    bytes: &[u8],
    limits: BundleLimits,
    fail_after_reservations: Option<usize>,
) -> Result<DatasetManifest, DecodeError> {
    let limits = limits.validate().map_err(DecodeError::Limits)?;
    check_input_bytes(bytes, limits)?;
    reject_escaped_strings(bytes)?;
    preflight_borrowed_strings(bytes, limits.max_string_bytes)?;
    let entry_count = preflight_array_field(
        bytes,
        "entries",
        limits.max_manifest_entries,
        "manifest_entry_count",
    )?;
    let mut budget = DecodeBudget::new(limits, fail_after_reservations)?;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let decoded = DatasetManifestSeed {
        entry_count,
        budget: &mut budget,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| ());
    let input = decoded.map_err(|()| budget.take_seed_failure())?;
    deserializer.end().map_err(|_| DecodeError::InvalidJson)?;
    validate_string(input.schema_version, limits, StringKind::Label)?;
    if input.schema_version != RESULT_BUNDLE_SCHEMA_VERSION {
        return Err(DecodeError::InvalidSchema);
    }
    validate_string(input.dataset_id, limits, StringKind::Label)?;
    validate_string(input.revision, limits, StringKind::Text)?;
    validate_string(input.scope_rule, limits, StringKind::Label)?;
    validate_string(input.loc_counting_rule, limits, StringKind::Label)?;
    if input.entries.len() > limits.max_manifest_entries {
        return Err(DecodeError::LimitExceeded {
            resource: "manifest_entry_count",
        });
    }

    let mut total_source_bytes = 0_u64;
    let mut prior_id: Option<&str> = None;
    let mut paths = Vec::new();
    paths
        .try_reserve_exact(input.entries.len())
        .map_err(|_| DecodeError::AllocationFailed)?;
    for entry in &input.entries {
        validate_string(entry.id, limits, StringKind::Label)?;
        validate_string(entry.grammar_family, limits, StringKind::Label)?;
        validate_string(entry.language, limits, StringKind::Label)?;
        validate_relative_path(entry.relative_path, limits)?;
        validate_digest(entry.source_sha256)?;
        if prior_id.is_some_and(|prior| entry.id <= prior) {
            return Err(DecodeError::NonCanonicalOrder);
        }
        prior_id = Some(entry.id);
        paths.push(entry.relative_path);
        if entry.source_bytes > limits.max_snapshot_bytes {
            return Err(DecodeError::LimitExceeded {
                resource: "snapshot_bytes",
            });
        }
        total_source_bytes = total_source_bytes.checked_add(entry.source_bytes).ok_or(
            DecodeError::LimitExceeded {
                resource: "dataset_source_bytes",
            },
        )?;
        if total_source_bytes > limits.max_dataset_source_bytes {
            return Err(DecodeError::LimitExceeded {
                resource: "dataset_source_bytes",
            });
        }
        let maximum_lines =
            entry
                .source_bytes
                .checked_add(1)
                .ok_or(DecodeError::LimitExceeded {
                    resource: "snapshot_bytes",
                })?;
        if entry.physical_lines > maximum_lines {
            return Err(DecodeError::InvalidPhysicalLineCount);
        }
    }
    paths.sort_unstable();
    if paths.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err(DecodeError::DuplicatePath);
    }

    input.into_owned(&mut budget)
}

/// Decodes a strict normalized command document under checked limits.
///
/// # Errors
///
/// Returns [`DecodeError`] for an invalid limit set, oversized input, malformed
/// JSON, unknown fields, path-shaped arguments, or invalid run counts.
pub fn decode_benchmark_command(
    bytes: &[u8],
    limits: BundleLimits,
) -> Result<BenchmarkCommand, DecodeError> {
    decode_benchmark_command_with_control(bytes, limits, None)
}

fn decode_benchmark_command_with_control(
    bytes: &[u8],
    limits: BundleLimits,
    fail_after_reservations: Option<usize>,
) -> Result<BenchmarkCommand, DecodeError> {
    let limits = limits.validate().map_err(DecodeError::Limits)?;
    check_input_bytes(bytes, limits)?;
    reject_escaped_strings(bytes)?;
    preflight_borrowed_strings(bytes, limits.max_string_bytes)?;
    let argument_count = preflight_array_field(
        bytes,
        "arguments",
        limits.max_command_arguments,
        "command_argument_count",
    )?;
    if argument_count != 0 {
        return Err(DecodeError::UnsupportedArguments);
    }
    let mut budget = DecodeBudget::new(limits, fail_after_reservations)?;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let decoded = BenchmarkCommandSeed {
        budget: &mut budget,
    }
    .deserialize(&mut deserializer)
    .map_err(|_| ());
    let input = decoded.map_err(|()| budget.take_seed_failure())?;
    deserializer.end().map_err(|_| DecodeError::InvalidJson)?;
    validate_string(input.schema_version, limits, StringKind::Label)?;
    if input.schema_version != RESULT_BUNDLE_SCHEMA_VERSION {
        return Err(DecodeError::InvalidSchema);
    }
    validate_string(input.subcommand, limits, StringKind::Label)?;
    if input.subcommand != "m05-parser-evidence" {
        return Err(DecodeError::UnsupportedSubcommand);
    }
    if input.warmup_rounds == 0 || input.trial_rounds == 0 || input.timeout_ms == 0 {
        return Err(DecodeError::InvalidRunPolicy);
    }
    input.into_owned(&mut budget)
}

fn check_input_bytes(bytes: &[u8], limits: BundleLimits) -> Result<(), DecodeError> {
    if bytes.is_empty() || bytes.len() > limits.max_input_bytes {
        return Err(DecodeError::LimitExceeded {
            resource: "input_bytes",
        });
    }
    Ok(())
}

const PREFLIGHT_STRING_EXCEEDED: &str = "benchmark input string length exceeded";
const DECODE_FIELD_REJECTED: &str = "benchmark input field set is invalid";

fn preflight_borrowed_strings(bytes: &[u8], maximum: usize) -> Result<(), DecodeError> {
    let mut exceeded = false;
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let result = StringLengthSeed {
        maximum,
        exceeded: &mut exceeded,
    }
    .deserialize(&mut deserializer);
    if exceeded {
        return Err(DecodeError::LimitExceeded {
            resource: "string_bytes",
        });
    }
    result.map_err(|_| DecodeError::InvalidJson)?;
    deserializer.end().map_err(|_| DecodeError::InvalidJson)
}

struct StringLengthSeed<'a> {
    maximum: usize,
    exceeded: &'a mut bool,
}

impl<'de> serde::de::DeserializeSeed<'de> for StringLengthSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(StringLengthVisitor {
            maximum: self.maximum,
            exceeded: self.exceeded,
        })
    }
}

struct StringLengthVisitor<'a> {
    maximum: usize,
    exceeded: &'a mut bool,
}

impl<'de> Visitor<'de> for StringLengthVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("strict JSON with bounded borrowed strings")
    }

    fn visit_bool<E>(self, _value: bool) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_i64<E>(self, _value: i64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_u64<E>(self, _value: u64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_f64<E>(self, _value: f64) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(())
    }

    fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        StringLengthSeed {
            maximum: self.maximum,
            exceeded: self.exceeded,
        }
        .deserialize(deserializer)
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if value.len() > self.maximum {
            *self.exceeded = true;
            Err(E::custom(PREFLIGHT_STRING_EXCEEDED))
        } else {
            Ok(())
        }
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom("benchmark input string was not borrowed"))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(()) = sequence.next_element_seed(StringLengthSeed {
            maximum: self.maximum,
            exceeded: &mut *self.exceeded,
        })? {}
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while let Some(()) = map.next_key_seed(StringLengthSeed {
            maximum: self.maximum,
            exceeded: &mut *self.exceeded,
        })? {
            map.next_value_seed(StringLengthSeed {
                maximum: self.maximum,
                exceeded: &mut *self.exceeded,
            })?;
        }
        Ok(())
    }
}

fn preflight_array_field(
    bytes: &[u8],
    field: &'static str,
    maximum: usize,
    resource: &'static str,
) -> Result<usize, DecodeError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let count = ObjectArrayCountSeed { field }
        .deserialize(&mut deserializer)
        .map_err(|_| DecodeError::InvalidJson)?
        .ok_or(DecodeError::InvalidJson)?;
    deserializer.end().map_err(|_| DecodeError::InvalidJson)?;
    if count > maximum {
        return Err(DecodeError::LimitExceeded { resource });
    }
    Ok(count)
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CollectionKind {
    Array,
    Object,
}

pub(crate) fn preflight_artifact_collection(
    bytes: &[u8],
    path: &[&'static str],
    kind: CollectionKind,
    maximum: usize,
    resource: &'static str,
    required: bool,
) -> Result<(), BundleError> {
    let mut deserializer = serde_json::Deserializer::from_slice(bytes);
    let count = ObjectPathCountSeed { path, kind }
        .deserialize(&mut deserializer)
        .map_err(|_| BundleError::InvalidArtifactEncoding)?;
    deserializer
        .end()
        .map_err(|_| BundleError::InvalidArtifactEncoding)?;
    let count = match count {
        Some(count) => count,
        None if !required => return Ok(()),
        None => return Err(BundleError::InvalidArtifactEncoding),
    };
    if count > maximum {
        return Err(BundleError::LimitExceeded { resource });
    }
    Ok(())
}

struct ObjectPathCountSeed<'a> {
    path: &'a [&'static str],
    kind: CollectionKind,
}

impl<'de> serde::de::DeserializeSeed<'de> for ObjectPathCountSeed<'_> {
    type Value = Option<usize>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if self.path.is_empty() {
            return Err(serde::de::Error::custom("collection path is empty"));
        }
        deserializer.deserialize_map(ObjectPathCountVisitor {
            path: self.path,
            kind: self.kind,
        })
    }
}

struct ObjectPathCountVisitor<'a> {
    path: &'a [&'static str],
    kind: CollectionKind,
}

impl<'de> Visitor<'de> for ObjectPathCountVisitor<'_> {
    type Value = Option<usize>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a benchmark JSON object containing a bounded collection")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut count = None;
        while let Some(key) = map.next_key::<&str>()? {
            if key == self.path[0] {
                if count.is_some() {
                    return Err(serde::de::Error::custom(DECODE_FIELD_REJECTED));
                }
                count = if self.path.len() == 1 {
                    Some(map.next_value_seed(CollectionCountSeed { kind: self.kind })?)
                } else {
                    map.next_value_seed(ObjectPathCountSeed {
                        path: &self.path[1..],
                        kind: self.kind,
                    })?
                };
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(count)
    }
}

struct CollectionCountSeed {
    kind: CollectionKind,
}

impl<'de> serde::de::DeserializeSeed<'de> for CollectionCountSeed {
    type Value = usize;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match self.kind {
            CollectionKind::Array => deserializer.deserialize_seq(ArrayCountVisitor),
            CollectionKind::Object => deserializer.deserialize_map(MapCountVisitor),
        }
    }
}

struct MapCountVisitor;

impl<'de> Visitor<'de> for MapCountVisitor {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a benchmark JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut count = 0_usize;
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {
            count = count
                .checked_add(1)
                .ok_or_else(|| serde::de::Error::custom("object count overflow"))?;
        }
        Ok(count)
    }
}

struct ObjectArrayCountSeed {
    field: &'static str,
}

impl<'de> serde::de::DeserializeSeed<'de> for ObjectArrayCountSeed {
    type Value = Option<usize>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(ObjectArrayCountVisitor { field: self.field })
    }
}

struct ObjectArrayCountVisitor {
    field: &'static str,
}

impl<'de> Visitor<'de> for ObjectArrayCountVisitor {
    type Value = Option<usize>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a benchmark JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut count = None;
        while let Some(key) = map.next_key::<&str>()? {
            if key == self.field {
                if count.is_some() {
                    return Err(serde::de::Error::custom(DECODE_FIELD_REJECTED));
                }
                count = Some(map.next_value_seed(ArrayCountSeed)?);
            } else {
                map.next_value::<IgnoredAny>()?;
            }
        }
        Ok(count)
    }
}

struct ArrayCountSeed;

impl<'de> serde::de::DeserializeSeed<'de> for ArrayCountSeed {
    type Value = usize;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(ArrayCountVisitor)
    }
}

struct ArrayCountVisitor;

impl<'de> Visitor<'de> for ArrayCountVisitor {
    type Value = usize;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a benchmark JSON array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut count = 0_usize;
        while sequence.next_element::<IgnoredAny>()?.is_some() {
            count = count
                .checked_add(1)
                .ok_or_else(|| serde::de::Error::custom("array count overflow"))?;
        }
        Ok(count)
    }
}

#[derive(Debug, Clone, Copy)]
enum StringKind {
    Label,
    Text,
}

fn validate_string(value: &str, limits: BundleLimits, kind: StringKind) -> Result<(), DecodeError> {
    if value.is_empty()
        || value.len() > limits.max_string_bytes
        || value.chars().any(char::is_control)
    {
        return Err(DecodeError::InvalidString);
    }
    if matches!(kind, StringKind::Label)
        && !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return Err(DecodeError::InvalidString);
    }
    Ok(())
}

fn validate_relative_path(value: &str, limits: BundleLimits) -> Result<(), DecodeError> {
    validate_string(value, limits, StringKind::Text)?;
    if value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('\\')
        || value.contains("//")
        || value.split('/').any(|component| {
            component.is_empty() || matches!(component, "." | "..") || component.ends_with('.')
        })
        || value.as_bytes().get(1).is_some_and(|byte| *byte == b':')
    {
        return Err(DecodeError::InvalidRelativePath);
    }
    Ok(())
}

fn validate_digest(value: &str) -> Result<(), DecodeError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(DecodeError::InvalidDigest);
    }
    Ok(())
}

const SEED_ALLOCATION_FAILED: &str = "rootlight decode reservation failed";
const SEED_LIMIT_EXCEEDED: &str = "rootlight decoded retention limit exceeded";

struct DecodeBudget {
    retained_bytes: usize,
    retained_items: usize,
    max_retained_bytes: usize,
    max_retained_items: usize,
    reservations: usize,
    fail_after_reservations: Option<usize>,
    seed_failure: Option<SeedFailure>,
}

// Serde erases visitor error types, so the budget retains the typed failure
// without recovering it from an allocated error message.
#[derive(Clone, Copy)]
enum SeedFailure {
    Allocation,
    RetainedItems,
}

impl DecodeBudget {
    fn new(
        limits: BundleLimits,
        fail_after_reservations: Option<usize>,
    ) -> Result<Self, DecodeError> {
        let max_retained_bytes = usize::try_from(limits.max_total_bytes).unwrap_or(usize::MAX);
        let max_retained_items = limits
            .max_manifest_entries
            .checked_mul(4)
            .and_then(|value| value.checked_add(limits.max_command_arguments.saturating_mul(4)))
            .and_then(|value| value.checked_add(1_024))
            .ok_or(DecodeError::LimitExceeded {
                resource: "decoded_retained_items",
            })?;
        Ok(Self {
            retained_bytes: 0,
            retained_items: 0,
            max_retained_bytes,
            max_retained_items,
            reservations: 0,
            fail_after_reservations,
            seed_failure: None,
        })
    }

    fn reserve<T>(&mut self, values: &mut Vec<T>, additional: usize) -> Result<(), DecodeError> {
        if let Err(error) = self.charge_items(additional) {
            self.record_seed_failure(&error);
            return Err(error);
        }
        if let Err(error) = self.before_reservation() {
            self.record_seed_failure(&error);
            return Err(error);
        }
        values.try_reserve_exact(additional).map_err(|_| {
            self.seed_failure = Some(SeedFailure::Allocation);
            DecodeError::AllocationFailed
        })
    }

    fn own(&mut self, value: &str) -> Result<String, DecodeError> {
        self.retained_bytes = self
            .retained_bytes
            .checked_add(value.len())
            .filter(|bytes| *bytes <= self.max_retained_bytes)
            .ok_or(DecodeError::LimitExceeded {
                resource: "decoded_retained_bytes",
            })?;
        self.before_reservation()?;
        let mut owned = String::new();
        owned
            .try_reserve_exact(value.len())
            .map_err(|_| DecodeError::AllocationFailed)?;
        owned.push_str(value);
        Ok(owned)
    }

    fn charge_items(&mut self, count: usize) -> Result<(), DecodeError> {
        self.retained_items = self
            .retained_items
            .checked_add(count)
            .filter(|items| *items <= self.max_retained_items)
            .ok_or(DecodeError::LimitExceeded {
                resource: "decoded_retained_items",
            })?;
        Ok(())
    }

    fn before_reservation(&mut self) -> Result<(), DecodeError> {
        if self
            .fail_after_reservations
            .is_some_and(|failure| self.reservations == failure)
        {
            return Err(DecodeError::AllocationFailed);
        }
        self.reservations = self
            .reservations
            .checked_add(1)
            .ok_or(DecodeError::LimitExceeded {
                resource: "decoded_retained_items",
            })?;
        Ok(())
    }

    fn take_seed_failure(&mut self) -> DecodeError {
        match self.seed_failure.take() {
            Some(SeedFailure::Allocation) => DecodeError::AllocationFailed,
            Some(SeedFailure::RetainedItems) => DecodeError::LimitExceeded {
                resource: "decoded_retained_items",
            },
            None => DecodeError::InvalidJson,
        }
    }

    fn record_seed_failure(&mut self, error: &DecodeError) {
        self.seed_failure = Some(match error {
            DecodeError::AllocationFailed => SeedFailure::Allocation,
            DecodeError::LimitExceeded { .. } => SeedFailure::RetainedItems,
            _ => return,
        });
    }
}

fn reject_escaped_strings(bytes: &[u8]) -> Result<(), DecodeError> {
    if bytes.contains(&b'\\') {
        return Err(DecodeError::InvalidJson);
    }
    Ok(())
}

fn seed_error<E: serde::de::Error>(error: DecodeError) -> E {
    match error {
        DecodeError::AllocationFailed => E::custom(SEED_ALLOCATION_FAILED),
        DecodeError::LimitExceeded { .. } => E::custom(SEED_LIMIT_EXCEEDED),
        _ => E::custom("rootlight bounded decode failed"),
    }
}

struct BorrowedDatasetManifest<'a> {
    schema_version: &'a str,
    dataset_id: &'a str,
    revision: &'a str,
    scope_rule: &'a str,
    loc_counting_rule: &'a str,
    entries: Vec<BorrowedDatasetEntry<'a>>,
}

impl BorrowedDatasetManifest<'_> {
    fn into_owned(self, budget: &mut DecodeBudget) -> Result<DatasetManifest, DecodeError> {
        let mut entries = Vec::new();
        budget.reserve(&mut entries, self.entries.len())?;
        for entry in self.entries {
            entries.push(entry.into_owned(budget)?);
        }
        Ok(DatasetManifest {
            schema_version: budget.own(self.schema_version)?,
            dataset_id: budget.own(self.dataset_id)?,
            revision: budget.own(self.revision)?,
            scope_rule: budget.own(self.scope_rule)?,
            loc_counting_rule: budget.own(self.loc_counting_rule)?,
            entries,
        })
    }
}

struct BorrowedDatasetEntry<'a> {
    id: &'a str,
    grammar_family: &'a str,
    language: &'a str,
    relative_path: &'a str,
    source_sha256: &'a str,
    source_bytes: u64,
    physical_lines: u64,
    generated: bool,
}

impl<'de> Deserialize<'de> for BorrowedDatasetEntry<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(BorrowedDatasetEntryVisitor)
    }
}

struct BorrowedDatasetEntryVisitor;

impl<'de> Visitor<'de> for BorrowedDatasetEntryVisitor {
    type Value = BorrowedDatasetEntry<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a strict borrowed dataset entry")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut id = None;
        let mut grammar_family = None;
        let mut language = None;
        let mut relative_path = None;
        let mut source_sha256 = None;
        let mut source_bytes = None;
        let mut physical_lines = None;
        let mut generated = None;
        while let Some(key) = map.next_key::<&'de str>()? {
            match key {
                "id" => set_field(&mut id, map.next_value::<&'de str>()?)?,
                "grammar_family" => {
                    set_field(&mut grammar_family, map.next_value::<&'de str>()?)?;
                }
                "language" => set_field(&mut language, map.next_value::<&'de str>()?)?,
                "relative_path" => {
                    set_field(&mut relative_path, map.next_value::<&'de str>()?)?;
                }
                "source_sha256" => {
                    set_field(&mut source_sha256, map.next_value::<&'de str>()?)?;
                }
                "source_bytes" => set_field(&mut source_bytes, map.next_value()?)?,
                "physical_lines" => set_field(&mut physical_lines, map.next_value()?)?,
                "generated" => set_field(&mut generated, map.next_value()?)?,
                _ => return Err(A::Error::custom(DECODE_FIELD_REJECTED)),
            }
        }
        Ok(BorrowedDatasetEntry {
            id: require_field(id)?,
            grammar_family: require_field(grammar_family)?,
            language: require_field(language)?,
            relative_path: require_field(relative_path)?,
            source_sha256: require_field(source_sha256)?,
            source_bytes: require_field(source_bytes)?,
            physical_lines: require_field(physical_lines)?,
            generated: require_field(generated)?,
        })
    }
}

impl BorrowedDatasetEntry<'_> {
    fn into_owned(self, budget: &mut DecodeBudget) -> Result<DatasetEntry, DecodeError> {
        Ok(DatasetEntry {
            id: budget.own(self.id)?,
            grammar_family: budget.own(self.grammar_family)?,
            language: budget.own(self.language)?,
            relative_path: budget.own(self.relative_path)?,
            source_sha256: budget.own(self.source_sha256)?,
            source_bytes: self.source_bytes,
            physical_lines: self.physical_lines,
            generated: self.generated,
        })
    }
}

struct DatasetManifestSeed<'a> {
    entry_count: usize,
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for DatasetManifestSeed<'_> {
    type Value = BorrowedDatasetManifest<'de>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(DatasetManifestVisitor {
            entry_count: self.entry_count,
            budget: self.budget,
        })
    }
}

struct DatasetManifestVisitor<'a> {
    entry_count: usize,
    budget: &'a mut DecodeBudget,
}

impl<'de> Visitor<'de> for DatasetManifestVisitor<'_> {
    type Value = BorrowedDatasetManifest<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a strict benchmark dataset manifest")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut schema_version = None;
        let mut dataset_id = None;
        let mut revision = None;
        let mut scope_rule = None;
        let mut loc_counting_rule = None;
        let mut entries = None;
        while let Some(key) = map.next_key::<&str>()? {
            match key {
                "schema_version" => {
                    set_field(&mut schema_version, map.next_value::<&str>()?)?;
                }
                "dataset_id" => {
                    set_field(&mut dataset_id, map.next_value::<&str>()?)?;
                }
                "revision" => {
                    set_field(&mut revision, map.next_value::<&str>()?)?;
                }
                "scope_rule" => {
                    set_field(&mut scope_rule, map.next_value::<&str>()?)?;
                }
                "loc_counting_rule" => {
                    set_field(&mut loc_counting_rule, map.next_value::<&str>()?)?;
                }
                "entries" => {
                    if entries.is_some() {
                        return Err(A::Error::custom(DECODE_FIELD_REJECTED));
                    }
                    entries = Some(map.next_value_seed(DatasetEntriesSeed {
                        count: self.entry_count,
                        budget: self.budget,
                    })?);
                }
                _ => return Err(A::Error::custom(DECODE_FIELD_REJECTED)),
            }
        }
        Ok(BorrowedDatasetManifest {
            schema_version: require_field(schema_version)?,
            dataset_id: require_field(dataset_id)?,
            revision: require_field(revision)?,
            scope_rule: require_field(scope_rule)?,
            loc_counting_rule: require_field(loc_counting_rule)?,
            entries: require_field(entries)?,
        })
    }
}

struct DatasetEntriesSeed<'a> {
    count: usize,
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for DatasetEntriesSeed<'_> {
    type Value = Vec<BorrowedDatasetEntry<'de>>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(DatasetEntriesVisitor {
            count: self.count,
            budget: self.budget,
        })
    }
}

struct DatasetEntriesVisitor<'a> {
    count: usize,
    budget: &'a mut DecodeBudget,
}

impl<'de> Visitor<'de> for DatasetEntriesVisitor<'_> {
    type Value = Vec<BorrowedDatasetEntry<'de>>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a bounded array of dataset entries")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut entries = Vec::new();
        self.budget
            .reserve(&mut entries, self.count)
            .map_err(seed_error::<A::Error>)?;
        while let Some(entry) = sequence.next_element()? {
            if entries.len() == self.count {
                return Err(A::Error::custom(
                    "dataset entry count changed after preflight",
                ));
            }
            entries.push(entry);
        }
        if entries.len() != self.count {
            return Err(A::Error::custom(
                "dataset entry count changed after preflight",
            ));
        }
        Ok(entries)
    }
}

struct BorrowedBenchmarkCommand<'a> {
    schema_version: &'a str,
    subcommand: &'a str,
    seed: u64,
    warmup_rounds: u32,
    trial_rounds: u32,
    timeout_ms: u64,
}

impl BorrowedBenchmarkCommand<'_> {
    fn into_owned(self, budget: &mut DecodeBudget) -> Result<BenchmarkCommand, DecodeError> {
        Ok(BenchmarkCommand {
            schema_version: budget.own(self.schema_version)?,
            subcommand: budget.own(self.subcommand)?,
            arguments: Vec::new(),
            seed: self.seed,
            warmup_rounds: self.warmup_rounds,
            trial_rounds: self.trial_rounds,
            timeout_ms: self.timeout_ms,
        })
    }
}

struct BenchmarkCommandSeed<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for BenchmarkCommandSeed<'_> {
    type Value = BorrowedBenchmarkCommand<'de>;

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(BenchmarkCommandVisitor {
            budget: self.budget,
        })
    }
}

struct BenchmarkCommandVisitor<'a> {
    budget: &'a mut DecodeBudget,
}

impl<'de> Visitor<'de> for BenchmarkCommandVisitor<'_> {
    type Value = BorrowedBenchmarkCommand<'de>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a strict benchmark command")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut schema_version = None;
        let mut subcommand = None;
        let mut arguments = None;
        let mut seed = None;
        let mut warmup_rounds = None;
        let mut trial_rounds = None;
        let mut timeout_ms = None;
        while let Some(key) = map.next_key::<&str>()? {
            match key {
                "schema_version" => {
                    set_field(&mut schema_version, map.next_value::<&str>()?)?;
                }
                "subcommand" => {
                    set_field(&mut subcommand, map.next_value::<&str>()?)?;
                }
                "arguments" => {
                    if arguments.is_some() {
                        return Err(A::Error::custom(DECODE_FIELD_REJECTED));
                    }
                    map.next_value_seed(EmptySequenceSeed)?;
                    arguments = Some(());
                }
                "seed" => set_field(&mut seed, map.next_value()?)?,
                "warmup_rounds" => {
                    set_field(&mut warmup_rounds, map.next_value()?)?;
                }
                "trial_rounds" => {
                    set_field(&mut trial_rounds, map.next_value()?)?;
                }
                "timeout_ms" => {
                    set_field(&mut timeout_ms, map.next_value()?)?;
                }
                _ => return Err(A::Error::custom(DECODE_FIELD_REJECTED)),
            }
        }
        let _ = self.budget;
        require_field(arguments)?;
        Ok(BorrowedBenchmarkCommand {
            schema_version: require_field(schema_version)?,
            subcommand: require_field(subcommand)?,
            seed: require_field(seed)?,
            warmup_rounds: require_field(warmup_rounds)?,
            trial_rounds: require_field(trial_rounds)?,
            timeout_ms: require_field(timeout_ms)?,
        })
    }
}

struct EmptySequenceSeed;

impl<'de> serde::de::DeserializeSeed<'de> for EmptySequenceSeed {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(EmptySequenceVisitor)
    }
}

struct EmptySequenceVisitor;

impl<'de> Visitor<'de> for EmptySequenceVisitor {
    type Value = ();

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("an empty schema 2.0 array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        if sequence.next_element::<IgnoredAny>()?.is_some() {
            return Err(A::Error::custom("schema 2.0 array is not empty"));
        }
        Ok(())
    }
}

fn set_field<T, E: serde::de::Error>(slot: &mut Option<T>, value: T) -> Result<(), E> {
    if slot.replace(value).is_some() {
        return Err(E::custom(DECODE_FIELD_REJECTED));
    }
    Ok(())
}

fn require_field<T, E: serde::de::Error>(field: Option<T>) -> Result<T, E> {
    field.ok_or_else(|| E::custom(DECODE_FIELD_REJECTED))
}

/// Strict benchmark-input decoding failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DecodeError {
    /// The supplied bundle/decode limits are invalid.
    #[error("benchmark decode limits are invalid")]
    Limits(#[source] BundleError),
    /// A bounded input resource exceeded its ceiling.
    #[error("benchmark input limit exceeded: {resource}")]
    LimitExceeded {
        /// Stable source-free resource label.
        resource: &'static str,
    },
    /// The input is not strict JSON for the expected document shape.
    #[error("benchmark input JSON is invalid")]
    InvalidJson,
    /// A bounded decoder reservation could not be satisfied.
    #[error("benchmark input allocation failed")]
    AllocationFailed,
    /// The schema version is unsupported.
    #[error("benchmark input schema is invalid")]
    InvalidSchema,
    /// A string is empty, oversized, contains controls, or is not canonical.
    #[error("benchmark input string is invalid")]
    InvalidString,
    /// A manifest digest is not canonical lowercase SHA-256.
    #[error("benchmark input digest is invalid")]
    InvalidDigest,
    /// A manifest source path is not canonical repository-relative syntax.
    #[error("benchmark input relative path is invalid")]
    InvalidRelativePath,
    /// Manifest entries are not in strictly increasing ID order.
    #[error("benchmark manifest order is invalid")]
    NonCanonicalOrder,
    /// Two manifest entries declare the same repository-relative path.
    #[error("benchmark manifest path is duplicated")]
    DuplicatePath,
    /// A physical-line count cannot fit the declared source bytes.
    #[error("benchmark manifest physical line count is invalid")]
    InvalidPhysicalLineCount,
    /// A normalized argument resembles a host path or URL.
    #[error("benchmark command contains a path-shaped value")]
    PathShapedValue,
    /// Schema 2.0 does not permit a free-form command argument channel.
    #[error("benchmark command arguments are unsupported")]
    UnsupportedArguments,
    /// Schema 2.0 supports only the closed M05 parser-evidence command.
    #[error("benchmark command subcommand is unsupported")]
    UnsupportedSubcommand,
    /// Warm-up, trial, or timeout policy is zero.
    #[error("benchmark command run policy is invalid")]
    InvalidRunPolicy,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(digest: &str, path: &str) -> Vec<u8> {
        format!(
            r#"{{
                "schema_version":"2.0",
                "dataset_id":"fixture",
                "revision":"rev-1",
                "scope_rule":"listed_entries",
                "loc_counting_rule":"physical_newlines",
                "entries":[{{
                    "id":"entry-1",
                    "grammar_family":"rust",
                    "language":"rust",
                    "relative_path":"{path}",
                    "source_sha256":"{digest}",
                    "source_bytes":4,
                    "physical_lines":1,
                    "generated":false
                }}]
            }}"#
        )
        .into_bytes()
    }

    #[test]
    fn manifest_decoder_accepts_only_canonical_lowercase_digests() {
        let lowercase = "ab".repeat(32);
        decode_dataset_manifest(&manifest(&lowercase, "src/lib.rs"), BundleLimits::default())
            .expect("canonical manifest decodes");

        let uppercase = "AB".repeat(32);
        let error =
            decode_dataset_manifest(&manifest(&uppercase, "src/lib.rs"), BundleLimits::default())
                .expect_err("uppercase digest is rejected");
        assert!(matches!(error, DecodeError::InvalidDigest));
    }

    #[test]
    fn manifest_decoder_bounds_input_entries_snapshot_and_total_bytes() {
        let digest = "ab".repeat(32);
        let bytes = manifest(&digest, "src/lib.rs");
        let limits = BundleLimits {
            max_input_bytes: bytes.len() - 1,
            ..BundleLimits::default()
        };
        assert!(matches!(
            decode_dataset_manifest(&bytes, limits),
            Err(DecodeError::LimitExceeded {
                resource: "input_bytes"
            })
        ));

        let limits = BundleLimits {
            max_manifest_entries: 1,
            ..BundleLimits::default()
        };
        let doubled = String::from_utf8(bytes)
            .expect("fixture is UTF-8")
            .replace("}]\n", "},{\"id\":\"entry-2\",\"grammar_family\":\"rust\",\"language\":\"rust\",\"relative_path\":\"src/main.rs\",\"source_sha256\":\"abababababababababababababababababababababababababababababababab\",\"source_bytes\":4,\"physical_lines\":1,\"generated\":false}]\n");
        assert!(matches!(
            decode_dataset_manifest(doubled.as_bytes(), limits),
            Err(DecodeError::LimitExceeded {
                resource: "manifest_entry_count"
            })
        ));

        let limits = BundleLimits {
            max_snapshot_bytes: 3,
            ..BundleLimits::default()
        };
        assert!(matches!(
            decode_dataset_manifest(&manifest(&digest, "src/lib.rs"), limits),
            Err(DecodeError::LimitExceeded {
                resource: "snapshot_bytes"
            })
        ));

        let limits = BundleLimits {
            max_dataset_source_bytes: 3,
            ..BundleLimits::default()
        };
        assert!(matches!(
            decode_dataset_manifest(&manifest(&digest, "src/lib.rs"), limits),
            Err(DecodeError::LimitExceeded {
                resource: "dataset_source_bytes"
            })
        ));
    }

    #[test]
    fn manifest_decoder_rejects_unknown_fields_and_noncanonical_paths() {
        let digest = "ab".repeat(32);
        let unknown = String::from_utf8(manifest(&digest, "src/lib.rs"))
            .expect("fixture is UTF-8")
            .replace("\"generated\":false", "\"generated\":false,\"extra\":true");
        let error = decode_dataset_manifest(unknown.as_bytes(), BundleLimits::default())
            .expect_err("nested unknown field is rejected");
        assert!(matches!(error, DecodeError::InvalidJson));
        assert!(!error.to_string().contains("extra"));

        assert!(matches!(
            decode_dataset_manifest(&manifest(&digest, "../outside.rs"), BundleLimits::default()),
            Err(DecodeError::InvalidRelativePath)
        ));
    }

    #[test]
    fn command_decoder_is_strict_bounded_and_source_free() {
        let valid = br#"{
            "schema_version":"2.0",
            "subcommand":"m05-parser-evidence",
            "arguments":[],
            "seed":7,
            "warmup_rounds":1,
            "trial_rounds":2,
            "timeout_ms":1000
        }"#;
        decode_benchmark_command(valid, BundleLimits::default())
            .expect("bounded source-free command decodes");

        let too_many = String::from_utf8(valid.to_vec())
            .expect("fixture is UTF-8")
            .replace("\"arguments\":[]", "\"arguments\":[\"first\",\"second\"]");
        let limits = BundleLimits {
            max_command_arguments: 1,
            ..BundleLimits::default()
        };
        assert!(matches!(
            decode_benchmark_command(too_many.as_bytes(), limits),
            Err(DecodeError::LimitExceeded {
                resource: "command_argument_count"
            })
        ));

        for value in [
            "src/lib.rs",
            "C:/source/repo",
            "https://example.invalid/source",
            "fn private_source() {}",
            "api_key_super_secret",
        ] {
            let unsupported = String::from_utf8(valid.to_vec())
                .expect("fixture is UTF-8")
                .replace("\"arguments\":[]", &format!("\"arguments\":[\"{value}\"]"));
            assert!(
                matches!(
                    decode_benchmark_command(unsupported.as_bytes(), BundleLimits::default()),
                    Err(DecodeError::UnsupportedArguments)
                ),
                "{value:?} must not cross the schema 2.0 command channel"
            );
        }

        let unsupported_subcommand = String::from_utf8(valid.to_vec())
            .expect("fixture is UTF-8")
            .replace("m05-parser-evidence", "m05-parser");
        assert!(matches!(
            decode_benchmark_command(unsupported_subcommand.as_bytes(), BundleLimits::default()),
            Err(DecodeError::UnsupportedSubcommand)
        ));

        let unknown = String::from_utf8(valid.to_vec())
            .expect("fixture is UTF-8")
            .replace("\"timeout_ms\":1000", "\"timeout_ms\":1000,\"extra\":true");
        let error = decode_benchmark_command(unknown.as_bytes(), BundleLimits::default())
            .expect_err("top-level unknown field is rejected");
        assert!(matches!(error, DecodeError::InvalidJson));
        assert!(!error.to_string().contains("extra"));
    }

    #[test]
    fn public_decoders_bound_max_input_unknown_keys_before_reservation() {
        let limits = BundleLimits {
            max_input_bytes: 4_096,
            max_string_bytes: 128,
            ..BundleLimits::default()
        };
        let command_prefix = concat!(
            "{\"schema_version\":\"2.0\",\"subcommand\":\"m05-parser-evidence\",",
            "\"arguments\":[],\"seed\":7,\"warmup_rounds\":1,\"trial_rounds\":2,",
            "\"timeout_ms\":1000,\""
        );
        let command_suffix = "\":null}";
        let command_key =
            "c".repeat(limits.max_input_bytes - command_prefix.len() - command_suffix.len());
        let command = format!("{command_prefix}{command_key}{command_suffix}");
        assert_eq!(command.len(), limits.max_input_bytes);

        let command_error =
            decode_benchmark_command_with_control(command.as_bytes(), limits, Some(0))
                .expect_err("oversized top-level key is rejected before retention");

        assert!(matches!(
            command_error,
            DecodeError::LimitExceeded {
                resource: "string_bytes"
            }
        ));
        assert!(!command_error.to_string().contains(&command_key));

        let manifest_prefix = concat!(
            "{\"schema_version\":\"2.0\",\"dataset_id\":\"fixture\",\"revision\":\"rev-1\",",
            "\"scope_rule\":\"listed_entries\",\"loc_counting_rule\":\"physical_newlines\",",
            "\"entries\":[{\"id\":\"entry-1\",\"grammar_family\":\"rust\",\"language\":\"rust\",",
            "\"relative_path\":\"src/lib.rs\",\"source_sha256\":",
            "\"abababababababababababababababababababababababababababababababab\",",
            "\"source_bytes\":4,\"physical_lines\":1,\"generated\":false,\""
        );
        let manifest_suffix = "\":null}]}";
        let manifest_key =
            "m".repeat(limits.max_input_bytes - manifest_prefix.len() - manifest_suffix.len());
        let manifest = format!("{manifest_prefix}{manifest_key}{manifest_suffix}");
        assert_eq!(manifest.len(), limits.max_input_bytes);

        let manifest_error =
            decode_dataset_manifest_with_control(manifest.as_bytes(), limits, Some(0))
                .expect_err("oversized nested key is rejected before retention");

        assert!(matches!(
            manifest_error,
            DecodeError::LimitExceeded {
                resource: "string_bytes"
            }
        ));
        assert!(!manifest_error.to_string().contains(&manifest_key));
    }

    #[test]
    fn borrowed_decoders_reject_escapes_and_report_fallible_retention() {
        let digest = "ab".repeat(32);
        let escaped_manifest = manifest(&digest, r"src\/lib.rs");
        assert!(matches!(
            decode_dataset_manifest(&escaped_manifest, BundleLimits::default()),
            Err(DecodeError::InvalidJson)
        ));

        let manifest = manifest(&digest, "src/lib.rs");
        assert!(matches!(
            decode_dataset_manifest_with_control(&manifest, BundleLimits::default(), Some(0)),
            Err(DecodeError::AllocationFailed)
        ));
        let retention_limits = BundleLimits {
            max_total_bytes: 1,
            ..BundleLimits::default()
        };
        assert!(matches!(
            decode_dataset_manifest(&manifest, retention_limits),
            Err(DecodeError::LimitExceeded {
                resource: "decoded_retained_bytes"
            })
        ));

        let command = br#"{
            "schema_version":"2.0",
            "subcommand":"m05-parser-evidence",
            "arguments":[],
            "seed":7,
            "warmup_rounds":1,
            "trial_rounds":2,
            "timeout_ms":1000
        }"#;
        assert!(matches!(
            decode_benchmark_command_with_control(command, BundleLimits::default(), Some(0)),
            Err(DecodeError::AllocationFailed)
        ));
        let escaped_command = String::from_utf8(command.to_vec())
            .expect("command fixture is UTF-8")
            .replace("evidence", r"\u0065vidence");
        assert!(matches!(
            decode_benchmark_command(escaped_command.as_bytes(), BundleLimits::default()),
            Err(DecodeError::InvalidJson)
        ));
    }
}
