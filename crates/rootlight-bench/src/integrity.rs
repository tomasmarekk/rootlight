//! Strict fixed-artifact decoding and cross-artifact result validation.
//!
//! The same bounded wire checks protect publication and later verification so
//! checksums cannot legitimize internally contradictory benchmark evidence.

use serde::{
    Deserialize, Serialize,
    de::{DeserializeSeed as _, Error as _, MapAccess, SeqAccess, Visitor},
    ser::{SerializeMap as _, SerializeSeq as _},
};
use sha2::{Digest as _, Sha256};

use crate::bundle::{
    AGENT_TRAJECTORIES_FILE, BUILD_PROVENANCE_FILE, BundleError, COMMAND_FILE, COVERAGE_FILE,
    DATASET_MANIFEST_FILE, ENVIRONMENT_FILE, FIXED_ARTIFACTS, QUALITY_FILE, RAW_SAMPLES_FILE,
    SUMMARY_FILE, json_bytes, json_lines,
};
use crate::decode::{CollectionKind, preflight_artifact_collection};
use crate::parser::{
    ScheduledSample, build_schedule, outlier_fences, semantic_fact_eligibility,
    semantic_quality_eligibility_from_values, summarize,
};
use crate::{
    Availability, BundleLimits, DatasetManifest, EvidenceValue, MetricDistribution,
    RESULT_BUNDLE_SCHEMA_VERSION, RawSample, ResultSummary, SEMANTIC_QUALITY_RUBRIC_ID,
    SampleOutcome, decode_benchmark_command, decode_dataset_manifest,
};

const WIRE_FIELD_REJECTED: &str = "fixed artifact field set is invalid";
const WIRE_STATUS_REJECTED: &str = "fixed artifact status is invalid";

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum WireEvidence<'a, T> {
    Observed {
        value: T,
    },
    Target {
        value: T,
    },
    Unavailable {
        #[serde(borrow)]
        reason_code: &'a str,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum WireAvailability<'a> {
    Available,
    Failed {
        #[serde(borrow)]
        reason_code: &'a str,
    },
    Unavailable {
        #[serde(borrow)]
        reason_code: &'a str,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
enum WireSampleOutcome<'a> {
    Succeeded,
    Failed {
        #[serde(borrow)]
        error_code: &'a str,
    },
    TimedOut,
    Cancelled,
}

impl<'de, T> Deserialize<'de> for WireEvidence<'de, T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(WireEvidenceVisitor(std::marker::PhantomData))
    }
}

struct WireEvidenceVisitor<T>(std::marker::PhantomData<T>);

impl<'de, T> Visitor<'de> for WireEvidenceVisitor<T>
where
    T: Deserialize<'de>,
{
    type Value = WireEvidence<'de, T>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a strict fixed-artifact evidence value")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        // Canonical serialization always emits the tag first. Requiring that
        // order lets the decoder select one closed shape without buffering.
        if map.next_key::<&'de str>()? != Some("status") {
            return Err(A::Error::custom(WIRE_FIELD_REJECTED));
        }
        let status = map.next_value::<&'de str>()?;
        match status {
            "observed" => {
                require_wire_field(&mut map, "value")?;
                let value = map.next_value::<T>()?;
                reject_trailing_wire_fields(&mut map)?;
                Ok(WireEvidence::Observed { value })
            }
            "target" => {
                require_wire_field(&mut map, "value")?;
                let value = map.next_value::<T>()?;
                reject_trailing_wire_fields(&mut map)?;
                Ok(WireEvidence::Target { value })
            }
            "unavailable" => {
                require_wire_field(&mut map, "reason_code")?;
                let reason_code = map.next_value::<&'de str>()?;
                reject_trailing_wire_fields(&mut map)?;
                Ok(WireEvidence::Unavailable { reason_code })
            }
            _ => Err(A::Error::custom(WIRE_STATUS_REJECTED)),
        }
    }
}

impl<'de> Deserialize<'de> for WireAvailability<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(WireAvailabilityVisitor)
    }
}

struct WireAvailabilityVisitor;

impl<'de> Visitor<'de> for WireAvailabilityVisitor {
    type Value = WireAvailability<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a strict fixed-artifact availability value")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        if map.next_key::<&'de str>()? != Some("status") {
            return Err(A::Error::custom(WIRE_FIELD_REJECTED));
        }
        let status = map.next_value::<&'de str>()?;
        match status {
            "available" => {
                reject_trailing_wire_fields(&mut map)?;
                Ok(WireAvailability::Available)
            }
            "failed" => {
                require_wire_field(&mut map, "reason_code")?;
                let reason_code = map.next_value::<&'de str>()?;
                reject_trailing_wire_fields(&mut map)?;
                Ok(WireAvailability::Failed { reason_code })
            }
            "unavailable" => {
                require_wire_field(&mut map, "reason_code")?;
                let reason_code = map.next_value::<&'de str>()?;
                reject_trailing_wire_fields(&mut map)?;
                Ok(WireAvailability::Unavailable { reason_code })
            }
            _ => Err(A::Error::custom(WIRE_STATUS_REJECTED)),
        }
    }
}

impl<'de> Deserialize<'de> for WireSampleOutcome<'de> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(WireSampleOutcomeVisitor)
    }
}

struct WireSampleOutcomeVisitor;

impl<'de> Visitor<'de> for WireSampleOutcomeVisitor {
    type Value = WireSampleOutcome<'de>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a strict fixed-artifact sample outcome")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        if map.next_key::<&'de str>()? != Some("status") {
            return Err(A::Error::custom(WIRE_FIELD_REJECTED));
        }
        let status = map.next_value::<&'de str>()?;
        match status {
            "succeeded" => {
                reject_trailing_wire_fields(&mut map)?;
                Ok(WireSampleOutcome::Succeeded)
            }
            "failed" => {
                require_wire_field(&mut map, "error_code")?;
                let error_code = map.next_value::<&'de str>()?;
                reject_trailing_wire_fields(&mut map)?;
                Ok(WireSampleOutcome::Failed { error_code })
            }
            "timed_out" => {
                reject_trailing_wire_fields(&mut map)?;
                Ok(WireSampleOutcome::TimedOut)
            }
            "cancelled" => {
                reject_trailing_wire_fields(&mut map)?;
                Ok(WireSampleOutcome::Cancelled)
            }
            _ => Err(A::Error::custom(WIRE_STATUS_REJECTED)),
        }
    }
}

fn require_wire_field<'de, A>(map: &mut A, expected: &str) -> Result<(), A::Error>
where
    A: MapAccess<'de>,
{
    if map.next_key::<&'de str>()? == Some(expected) {
        Ok(())
    } else {
        Err(A::Error::custom(WIRE_FIELD_REJECTED))
    }
}

fn reject_trailing_wire_fields<'de, A>(map: &mut A) -> Result<(), A::Error>
where
    A: MapAccess<'de>,
{
    if map.next_key::<&'de str>()?.is_none() {
        Ok(())
    } else {
        Err(A::Error::custom(WIRE_FIELD_REJECTED))
    }
}

#[derive(Debug)]
struct FallibleVec<T>(Vec<T>);

impl<T> FallibleVec<T> {
    fn as_slice(&self) -> &[T] {
        &self.0
    }
}

impl<'de, T> Deserialize<'de> for FallibleVec<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_seq(FallibleVecVisitor(std::marker::PhantomData))
    }
}

struct FallibleVecVisitor<T>(std::marker::PhantomData<T>);

impl<'de, T> Visitor<'de> for FallibleVecVisitor<T>
where
    T: Deserialize<'de>,
{
    type Value = FallibleVec<T>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a fallibly retained JSON array")
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element()? {
            if values.len() == values.capacity() {
                try_reserve_decode(&mut values, 1).map_err(A::Error::custom)?;
            }
            values.push(value);
        }
        Ok(FallibleVec(values))
    }
}

impl<T> Serialize for FallibleVec<T>
where
    T: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut sequence = serializer.serialize_seq(Some(self.0.len()))?;
        for value in &self.0 {
            sequence.serialize_element(value)?;
        }
        sequence.end()
    }
}

#[derive(Debug)]
struct FallibleMap<'a, V>(Vec<(&'a str, V)>);

impl<V> FallibleMap<'_, V> {
    fn len(&self) -> usize {
        self.0.len()
    }

    fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.0.iter().map(|(key, value)| (*key, value))
    }

    fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(key, _)| *key)
    }
}

impl<'de: 'a, 'a, V> Deserialize<'de> for FallibleMap<'a, V>
where
    V: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_map(FallibleMapVisitor(std::marker::PhantomData))
    }
}

struct FallibleMapVisitor<'a, V>(std::marker::PhantomData<(&'a str, V)>);

impl<'de: 'a, 'a, V> Visitor<'de> for FallibleMapVisitor<'a, V>
where
    V: Deserialize<'de>,
{
    type Value = FallibleMap<'a, V>;

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("a sorted fallibly retained JSON object")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(key) = map.next_key::<&'a str>()? {
            if values
                .last()
                .is_some_and(|(previous, _): &(&str, V)| *previous >= key)
            {
                return Err(A::Error::custom("JSON object keys are not canonical"));
            }
            if values.len() == values.capacity() {
                try_reserve_decode(&mut values, 1).map_err(A::Error::custom)?;
            }
            values.push((key, map.next_value()?));
        }
        Ok(FallibleMap(values))
    }
}

impl<V> Serialize for FallibleMap<'_, V>
where
    V: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

const DECODE_ALLOCATION_FAILED: &str = "fixed-artifact reservation failed";

// Serde's visitor error type erases reservation failures. The out-of-band flag
// preserves that classification without inspecting an allocated error string.
thread_local! {
    static DECODE_ALLOCATION_FAILURE: std::cell::Cell<bool> =
        const { std::cell::Cell::new(false) };
}

fn signal_decode_allocation_failure() -> &'static str {
    DECODE_ALLOCATION_FAILURE.with(|failed| failed.set(true));
    DECODE_ALLOCATION_FAILED
}

fn map_decode_result<T>(
    decode: impl FnOnce() -> Result<T, serde_json::Error>,
) -> Result<T, BundleError> {
    let previous = DECODE_ALLOCATION_FAILURE.with(|failed| failed.replace(false));
    let result = decode();
    let allocation_failed = DECODE_ALLOCATION_FAILURE.with(|failed| failed.replace(previous));
    if allocation_failed {
        Err(BundleError::AllocationFailed)
    } else {
        result.map_err(|_| BundleError::InvalidArtifactEncoding)
    }
}

fn try_reserve_decode<T>(values: &mut Vec<T>, additional: usize) -> Result<(), &'static str> {
    before_decode_reservation()?;
    values.try_reserve(additional).map_err(|_| {
        let _ = signal_decode_allocation_failure();
        DECODE_ALLOCATION_FAILED
    })
}

fn before_decode_reservation() -> Result<(), &'static str> {
    #[cfg(test)]
    DECODE_RESERVATION_FAIL_AFTER.with(|remaining| {
        if let Some(value) = remaining.get() {
            if value == 0 {
                return Err(signal_decode_allocation_failure());
            }
            remaining.set(Some(value - 1));
        }
        Ok(())
    })?;
    Ok(())
}

fn own_wire_string(value: &str) -> Result<String, BundleError> {
    before_decode_reservation().map_err(|_| BundleError::AllocationFailed)?;
    let mut owned = String::new();
    owned
        .try_reserve_exact(value.len())
        .map_err(|_| BundleError::AllocationFailed)?;
    owned.push_str(value);
    Ok(owned)
}

#[cfg(test)]
thread_local! {
    static DECODE_RESERVATION_FAIL_AFTER: std::cell::Cell<Option<usize>> =
        const { std::cell::Cell::new(None) };
}

#[cfg(test)]
pub(crate) fn set_decode_reservation_fail_after(value: Option<usize>) {
    DECODE_RESERVATION_FAIL_AFTER.with(|remaining| remaining.set(value));
    if value.is_none() {
        DECODE_ALLOCATION_FAILURE.with(|failed| failed.set(false));
    }
}

macro_rules! impl_wire_struct_deserialize {
    ($name:ident { $($field:ident: $field_type:ty),+ $(,)? }) => {
        impl<'de> Deserialize<'de> for $name<'de> {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct WireStructVisitor;

                impl<'de> Visitor<'de> for WireStructVisitor {
                    type Value = $name<'de>;

                    fn expecting(
                        &self,
                        formatter: &mut std::fmt::Formatter<'_>,
                    ) -> std::fmt::Result {
                        formatter.write_str("a strict fixed-artifact object")
                    }

                    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
                    where
                        A: MapAccess<'de>,
                    {
                        $(let mut $field: Option<$field_type> = None;)+
                        while let Some(field) = map.next_key::<&'de str>()? {
                            match field {
                                $(
                                    stringify!($field) => {
                                        if $field.is_some() {
                                            return Err(A::Error::custom(WIRE_FIELD_REJECTED));
                                        }
                                        $field = Some(map.next_value::<$field_type>()?);
                                    }
                                )+
                                _ => return Err(A::Error::custom(WIRE_FIELD_REJECTED)),
                            }
                        }
                        Ok($name {
                            $(
                                $field: $field
                                    .ok_or_else(|| A::Error::custom(WIRE_FIELD_REJECTED))?,
                            )+
                        })
                    }
                }

                deserializer.deserialize_map(WireStructVisitor)
            }
        }
    };
}

#[derive(Debug, Serialize)]
struct WireEnvironment<'a> {
    schema_version: &'a str,
    cpu_model: WireEvidence<'a, &'a str>,
    cpu_topology: WireEvidence<'a, &'a str>,
    ram_bytes: WireEvidence<'a, u64>,
    operating_system: WireEvidence<'a, &'a str>,
    kernel: WireEvidence<'a, &'a str>,
    filesystem: WireEvidence<'a, &'a str>,
    storage_device: WireEvidence<'a, &'a str>,
    power_mode: WireEvidence<'a, &'a str>,
    container_limits: WireEvidence<'a, &'a str>,
    compiler: WireEvidence<'a, &'a str>,
    binary_sha256: WireEvidence<'a, &'a str>,
    feature_profile: &'a str,
    sqlite: WireEvidence<'a, &'a str>,
    adapter_versions: WireEvidence<'a, FallibleMap<'a, &'a str>>,
    grammar_versions: WireEvidence<'a, FallibleMap<'a, &'a str>>,
    grammar_source_package_checksums: WireEvidence<'a, FallibleMap<'a, &'a str>>,
    grammar_hashes: WireEvidence<'a, FallibleMap<'a, &'a str>>,
    locale: WireEvidence<'a, &'a str>,
    background_process_policy: WireEvidence<'a, &'a str>,
    clock_source: WireEvidence<'a, &'a str>,
    process_tree_accounting: WireAvailability<'a>,
}

impl_wire_struct_deserialize!(WireEnvironment {
    schema_version: &'de str,
    cpu_model: WireEvidence<'de, &'de str>,
    cpu_topology: WireEvidence<'de, &'de str>,
    ram_bytes: WireEvidence<'de, u64>,
    operating_system: WireEvidence<'de, &'de str>,
    kernel: WireEvidence<'de, &'de str>,
    filesystem: WireEvidence<'de, &'de str>,
    storage_device: WireEvidence<'de, &'de str>,
    power_mode: WireEvidence<'de, &'de str>,
    container_limits: WireEvidence<'de, &'de str>,
    compiler: WireEvidence<'de, &'de str>,
    binary_sha256: WireEvidence<'de, &'de str>,
    feature_profile: &'de str,
    sqlite: WireEvidence<'de, &'de str>,
    adapter_versions: WireEvidence<'de, FallibleMap<'de, &'de str>>,
    grammar_versions: WireEvidence<'de, FallibleMap<'de, &'de str>>,
    grammar_source_package_checksums: WireEvidence<'de, FallibleMap<'de, &'de str>>,
    grammar_hashes: WireEvidence<'de, FallibleMap<'de, &'de str>>,
    locale: WireEvidence<'de, &'de str>,
    background_process_policy: WireEvidence<'de, &'de str>,
    clock_source: WireEvidence<'de, &'de str>,
    process_tree_accounting: WireAvailability<'de>,
});

#[derive(Debug, Serialize)]
struct WireBuildProvenance<'a> {
    schema_version: &'a str,
    source_revision: &'a str,
    binary_revision: &'a str,
    build_profile: &'a str,
    features: FallibleVec<&'a str>,
    target: &'a str,
}

impl_wire_struct_deserialize!(WireBuildProvenance {
    schema_version: &'de str,
    source_revision: &'de str,
    binary_revision: &'de str,
    build_profile: &'de str,
    features: FallibleVec<&'de str>,
    target: &'de str,
});

#[derive(Debug, Serialize)]
struct WireMetricDistribution<'a> {
    sample_count: u64,
    p50_ns: WireEvidence<'a, u64>,
    p95_ns: WireEvidence<'a, u64>,
    p99_ns: WireEvidence<'a, u64>,
    physical_lines_per_second: WireEvidence<'a, u64>,
    files_per_second: WireEvidence<'a, u64>,
    syntax_nodes_per_second: WireEvidence<'a, u64>,
    syntax_facts_per_source_byte_ppm: WireEvidence<'a, u64>,
    outlier_count: u64,
}

impl_wire_struct_deserialize!(WireMetricDistribution {
    sample_count: u64,
    p50_ns: WireEvidence<'de, u64>,
    p95_ns: WireEvidence<'de, u64>,
    p99_ns: WireEvidence<'de, u64>,
    physical_lines_per_second: WireEvidence<'de, u64>,
    files_per_second: WireEvidence<'de, u64>,
    syntax_nodes_per_second: WireEvidence<'de, u64>,
    syntax_facts_per_source_byte_ppm: WireEvidence<'de, u64>,
    outlier_count: u64,
});

#[derive(Debug, Serialize)]
struct WireSummary<'a> {
    schema_version: &'a str,
    benchmark_id: &'a str,
    semantic_eligibility: WireAvailability<'a>,
    families: FallibleMap<'a, WireMetricDistribution<'a>>,
    failed_samples: u64,
    timed_out_samples: u64,
    cancelled_samples: u64,
    confidence_intervals: WireAvailability<'a>,
}

impl_wire_struct_deserialize!(WireSummary {
    schema_version: &'de str,
    benchmark_id: &'de str,
    semantic_eligibility: WireAvailability<'de>,
    families: FallibleMap<'de, WireMetricDistribution<'de>>,
    failed_samples: u64,
    timed_out_samples: u64,
    cancelled_samples: u64,
    confidence_intervals: WireAvailability<'de>,
});

#[derive(Debug, Serialize)]
struct WireCoverage<'a> {
    schema_version: &'a str,
    attempted_entries: u64,
    committed_entries: u64,
    skipped: FallibleMap<'a, &'a str>,
    parser_status: FallibleMap<'a, &'a str>,
}

impl_wire_struct_deserialize!(WireCoverage {
    schema_version: &'de str,
    attempted_entries: u64,
    committed_entries: u64,
    skipped: FallibleMap<'de, &'de str>,
    parser_status: FallibleMap<'de, &'de str>,
});

#[derive(Debug, Serialize)]
struct WireQuality<'a> {
    schema_version: &'a str,
    rubric_id: &'a str,
    semantic_eligibility: WireAvailability<'a>,
    precision_ppm: WireEvidence<'a, u64>,
    recall_ppm: WireEvidence<'a, u64>,
    expected_calibration_error_ppm: WireEvidence<'a, u64>,
    unsupported_cases: FallibleMap<'a, &'a str>,
}

impl_wire_struct_deserialize!(WireQuality {
    schema_version: &'de str,
    rubric_id: &'de str,
    semantic_eligibility: WireAvailability<'de>,
    precision_ppm: WireEvidence<'de, u64>,
    recall_ppm: WireEvidence<'de, u64>,
    expected_calibration_error_ppm: WireEvidence<'de, u64>,
    unsupported_cases: FallibleMap<'de, &'de str>,
});

#[derive(Debug, Serialize)]
struct WireRawSample<'a> {
    schema_version: &'a str,
    ordinal: u64,
    phase: &'a str,
    dataset_entry_id: &'a str,
    grammar_family: &'a str,
    elapsed_ns: u64,
    source_bytes: u64,
    physical_lines: u64,
    syntax_nodes: u64,
    syntax_facts: u64,
    semantic_facts: WireEvidence<'a, u64>,
    process_tree_cpu_ns: WireEvidence<'a, u64>,
    process_tree_peak_rss_bytes: WireEvidence<'a, u64>,
    outcome: WireSampleOutcome<'a>,
    is_outlier: bool,
}

impl_wire_struct_deserialize!(WireRawSample {
    schema_version: &'de str,
    ordinal: u64,
    phase: &'de str,
    dataset_entry_id: &'de str,
    grammar_family: &'de str,
    elapsed_ns: u64,
    source_bytes: u64,
    physical_lines: u64,
    syntax_nodes: u64,
    syntax_facts: u64,
    semantic_facts: WireEvidence<'de, u64>,
    process_tree_cpu_ns: WireEvidence<'de, u64>,
    process_tree_peak_rss_bytes: WireEvidence<'de, u64>,
    outcome: WireSampleOutcome<'de>,
    is_outlier: bool,
});

impl WireRawSample<'_> {
    fn into_owned(self) -> Result<RawSample, BundleError> {
        Ok(RawSample {
            schema_version: own_wire_string(self.schema_version)?,
            ordinal: self.ordinal,
            phase: own_wire_string(self.phase)?,
            dataset_entry_id: own_wire_string(self.dataset_entry_id)?,
            grammar_family: own_wire_string(self.grammar_family)?,
            elapsed_ns: self.elapsed_ns,
            source_bytes: self.source_bytes,
            physical_lines: self.physical_lines,
            syntax_nodes: self.syntax_nodes,
            syntax_facts: self.syntax_facts,
            semantic_facts: self.semantic_facts.into_owned()?,
            process_tree_cpu_ns: self.process_tree_cpu_ns.into_owned()?,
            process_tree_peak_rss_bytes: self.process_tree_peak_rss_bytes.into_owned()?,
            outcome: self.outcome.into_owned()?,
            is_outlier: self.is_outlier,
        })
    }
}

impl WireEvidence<'_, u64> {
    fn into_owned(self) -> Result<EvidenceValue<u64>, BundleError> {
        match self {
            Self::Observed { value } => Ok(EvidenceValue::Observed { value }),
            Self::Target { value } => Ok(EvidenceValue::Target { value }),
            Self::Unavailable { reason_code } => Ok(EvidenceValue::Unavailable {
                reason_code: own_wire_string(reason_code)?,
            }),
        }
    }

    fn to_owned(&self) -> Result<EvidenceValue<u64>, BundleError> {
        match self {
            Self::Observed { value } => Ok(EvidenceValue::Observed { value: *value }),
            Self::Target { value } => Ok(EvidenceValue::Target { value: *value }),
            Self::Unavailable { reason_code } => Ok(EvidenceValue::Unavailable {
                reason_code: own_wire_string(reason_code)?,
            }),
        }
    }

    fn matches(&self, expected: &EvidenceValue<u64>) -> bool {
        match (self, expected) {
            (Self::Observed { value: left }, EvidenceValue::Observed { value: right })
            | (Self::Target { value: left }, EvidenceValue::Target { value: right }) => {
                left == right
            }
            (
                Self::Unavailable { reason_code: left },
                EvidenceValue::Unavailable { reason_code: right },
            ) => *left == right,
            _ => false,
        }
    }
}

impl WireSampleOutcome<'_> {
    fn into_owned(self) -> Result<SampleOutcome, BundleError> {
        match self {
            Self::Succeeded => Ok(SampleOutcome::Succeeded),
            Self::Failed { error_code } => Ok(SampleOutcome::Failed {
                error_code: own_wire_string(error_code)?,
            }),
            Self::TimedOut => Ok(SampleOutcome::TimedOut),
            Self::Cancelled => Ok(SampleOutcome::Cancelled),
        }
    }
}

impl WireAvailability<'_> {
    fn to_owned(&self) -> Result<Availability, BundleError> {
        match self {
            Self::Available => Ok(Availability::Available),
            Self::Failed { reason_code } => Ok(Availability::Failed {
                reason_code: own_wire_string(reason_code)?,
            }),
            Self::Unavailable { reason_code } => Ok(Availability::Unavailable {
                reason_code: own_wire_string(reason_code)?,
            }),
        }
    }

    fn matches(&self, expected: &Availability) -> bool {
        match (self, expected) {
            (Self::Available, Availability::Available) => true,
            (Self::Failed { reason_code: left }, Availability::Failed { reason_code: right })
            | (
                Self::Unavailable { reason_code: left },
                Availability::Unavailable { reason_code: right },
            ) => *left == right,
            _ => false,
        }
    }
}

const DECODE_BYTES_EXCEEDED: &str = "fixed decoded byte retention exceeded";
const DECODE_ITEMS_EXCEEDED: &str = "fixed decoded item retention exceeded";
const DECODE_STRING_EXCEEDED: &str = "fixed decoded string length exceeded";

struct FixedDecodeBudget {
    retained_bytes: u64,
    retained_items: u64,
    max_retained_bytes: u64,
    max_retained_items: u64,
    max_string_bytes: usize,
}

impl FixedDecodeBudget {
    const fn new(limits: BundleLimits) -> Self {
        Self {
            retained_bytes: 0,
            retained_items: 0,
            max_retained_bytes: limits.max_total_bytes,
            max_retained_items: limits.max_total_bytes,
            max_string_bytes: limits.max_string_bytes,
        }
    }

    fn inspect_artifact(&mut self, name: &str, bytes: &[u8]) -> Result<(), BundleError> {
        if matches!(name, RAW_SAMPLES_FILE | AGENT_TRAJECTORIES_FILE) {
            if bytes.is_empty() {
                return Ok(());
            }
            if !bytes.ends_with(b"\n") {
                return Err(BundleError::InvalidArtifactEncoding);
            }
            for line in bytes[..bytes.len() - 1].split(|byte| *byte == b'\n') {
                self.inspect_json(line)?;
            }
            return Ok(());
        }
        if bytes.len() <= 1 || !bytes.ends_with(b"\n") {
            return Err(BundleError::InvalidArtifactEncoding);
        }
        self.inspect_json(&bytes[..bytes.len() - 1])
    }

    fn inspect_json(&mut self, bytes: &[u8]) -> Result<(), BundleError> {
        // Borrowed wire decoding deliberately rejects escapes so serde never
        // needs an attacker-sized scratch string before our fallible budget.
        if bytes.contains(&b'\\') {
            return Err(BundleError::InvalidArtifactEncoding);
        }
        let mut deserializer = serde_json::Deserializer::from_slice(bytes);
        BudgetSeed { budget: self }
            .deserialize(&mut deserializer)
            .map_err(map_budget_error)?;
        deserializer
            .end()
            .map_err(|_| BundleError::InvalidArtifactEncoding)
    }

    fn charge_bytes<E: serde::de::Error>(&mut self, bytes: usize) -> Result<(), E> {
        if bytes > self.max_string_bytes {
            return Err(E::custom(DECODE_STRING_EXCEEDED));
        }
        let bytes = u64::try_from(bytes).map_err(|_| E::custom(DECODE_BYTES_EXCEEDED))?;
        self.retained_bytes = self
            .retained_bytes
            .checked_add(bytes)
            .filter(|retained| *retained <= self.max_retained_bytes)
            .ok_or_else(|| E::custom(DECODE_BYTES_EXCEEDED))?;
        Ok(())
    }

    fn charge_item<E: serde::de::Error>(&mut self) -> Result<(), E> {
        self.retained_items = self
            .retained_items
            .checked_add(1)
            .filter(|retained| *retained <= self.max_retained_items)
            .ok_or_else(|| E::custom(DECODE_ITEMS_EXCEEDED))?;
        Ok(())
    }
}

fn map_budget_error(error: serde_json::Error) -> BundleError {
    let message = error.to_string();
    if message.starts_with(DECODE_BYTES_EXCEEDED) {
        BundleError::LimitExceeded {
            resource: "decoded_retained_bytes",
        }
    } else if message.starts_with(DECODE_ITEMS_EXCEEDED) {
        BundleError::LimitExceeded {
            resource: "decoded_retained_items",
        }
    } else if message.starts_with(DECODE_STRING_EXCEEDED) {
        BundleError::LimitExceeded {
            resource: "string_bytes",
        }
    } else {
        BundleError::InvalidArtifactEncoding
    }
}

struct BudgetSeed<'a> {
    budget: &'a mut FixedDecodeBudget,
}

impl<'de> serde::de::DeserializeSeed<'de> for BudgetSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_any(BudgetVisitor {
            budget: self.budget,
        })
    }
}

struct BudgetVisitor<'a> {
    budget: &'a mut FixedDecodeBudget,
}

impl<'de> Visitor<'de> for BudgetVisitor<'_> {
    type Value = ();

    fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("strict JSON with borrowed strings")
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
        BudgetSeed {
            budget: self.budget,
        }
        .deserialize(deserializer)
    }

    fn visit_borrowed_str<E>(self, value: &'de str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.budget.charge_bytes::<E>(value.len())
    }

    fn visit_str<E>(self, _value: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        Err(E::custom("fixed artifact string was not borrowed"))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        while let Some(()) = sequence.next_element_seed(BudgetSeed {
            budget: &mut *self.budget,
        })? {
            self.budget.charge_item::<A::Error>()?;
        }
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        while let Some(()) = map.next_key_seed(BudgetSeed {
            budget: &mut *self.budget,
        })? {
            map.next_value_seed(BudgetSeed {
                budget: &mut *self.budget,
            })?;
            self.budget.charge_item::<A::Error>()?;
        }
        Ok(())
    }
}

#[derive(Debug)]
struct FixedArtifacts<'a> {
    environment: WireEnvironment<'a>,
    dataset_manifest: DatasetManifest,
    build_provenance: WireBuildProvenance<'a>,
    schedule: Vec<ScheduledSample>,
    raw_samples: Vec<RawSample>,
    summary: WireSummary<'a>,
    coverage: WireCoverage<'a>,
    quality: WireQuality<'a>,
}

pub(crate) fn is_fixed_artifact(relative: &str) -> bool {
    FIXED_ARTIFACTS.contains(&relative)
}

pub(crate) trait FixedArtifactSource {
    fn artifact_bytes(&self, name: &str) -> Option<&[u8]>;
}

pub(crate) fn validate_fixed_artifacts<S>(
    artifacts: &S,
    limits: BundleLimits,
) -> Result<(), BundleError>
where
    S: FixedArtifactSource + ?Sized,
{
    let fixed = decode_fixed_artifacts(artifacts, limits)?;
    validate_fixed_bundle(&fixed, limits)
}

fn decode_fixed_artifacts<'a, S>(
    artifacts: &'a S,
    limits: BundleLimits,
) -> Result<FixedArtifacts<'a>, BundleError>
where
    S: FixedArtifactSource + ?Sized,
{
    let mut decode_budget = FixedDecodeBudget::new(limits);
    for name in FIXED_ARTIFACTS {
        let bytes = fixed_bytes(artifacts, name)?;
        decode_budget.inspect_artifact(name, bytes)?;
    }
    let manifest_bytes = fixed_bytes(artifacts, DATASET_MANIFEST_FILE)?;
    validate_json_bytes(manifest_bytes, limits)?;
    let dataset_manifest =
        decode_dataset_manifest(manifest_bytes, limits).map_err(map_decode_error)?;
    validate_canonical_json(manifest_bytes, &dataset_manifest, limits)?;
    let command_bytes = fixed_bytes(artifacts, COMMAND_FILE)?;
    validate_json_bytes(command_bytes, limits)?;
    let command = decode_benchmark_command(command_bytes, limits).map_err(map_decode_error)?;
    validate_canonical_json(command_bytes, &command, limits)?;
    let schedule = build_schedule(
        dataset_manifest.entries.len(),
        command.warmup_rounds,
        command.trial_rounds,
        command.seed,
        limits.max_raw_samples,
    )
    .map_err(map_parser_integrity_error)?;
    for field in [
        "adapter_versions",
        "grammar_versions",
        "grammar_source_package_checksums",
        "grammar_hashes",
    ] {
        preflight_fixed_collection(
            artifacts,
            ENVIRONMENT_FILE,
            &[field, "value"],
            CollectionKind::Object,
            limits.max_manifest_entries,
            "evidence_map_entry_count",
            false,
            limits,
        )?;
    }
    preflight_fixed_collection(
        artifacts,
        BUILD_PROVENANCE_FILE,
        &["features"],
        CollectionKind::Array,
        limits.max_command_arguments,
        "feature_count",
        true,
        limits,
    )?;
    preflight_fixed_collection(
        artifacts,
        SUMMARY_FILE,
        &["families"],
        CollectionKind::Object,
        limits.max_manifest_entries,
        "summary_family_count",
        true,
        limits,
    )?;
    for field in ["skipped", "parser_status"] {
        preflight_fixed_collection(
            artifacts,
            COVERAGE_FILE,
            &[field],
            CollectionKind::Object,
            limits.max_manifest_entries,
            "coverage_entry_count",
            true,
            limits,
        )?;
    }
    preflight_fixed_collection(
        artifacts,
        QUALITY_FILE,
        &["unsupported_cases"],
        CollectionKind::Object,
        limits.max_manifest_entries,
        "unsupported_case_count",
        true,
        limits,
    )?;
    decode_agent_trajectories(fixed_bytes(artifacts, AGENT_TRAJECTORIES_FILE)?, limits)?;
    Ok(FixedArtifacts {
        environment: decode_json(fixed_bytes(artifacts, ENVIRONMENT_FILE)?, limits)?,
        dataset_manifest,
        build_provenance: decode_json(fixed_bytes(artifacts, BUILD_PROVENANCE_FILE)?, limits)?,
        raw_samples: decode_raw_samples(
            fixed_bytes(artifacts, RAW_SAMPLES_FILE)?,
            schedule.len(),
            limits,
            "raw_sample_count",
        )?,
        schedule,
        summary: decode_json(fixed_bytes(artifacts, SUMMARY_FILE)?, limits)?,
        coverage: decode_json(fixed_bytes(artifacts, COVERAGE_FILE)?, limits)?,
        quality: decode_json(fixed_bytes(artifacts, QUALITY_FILE)?, limits)?,
    })
}

#[allow(clippy::too_many_arguments)]
fn preflight_fixed_collection<S>(
    artifacts: &S,
    artifact: &str,
    path: &[&'static str],
    kind: CollectionKind,
    maximum: usize,
    resource: &'static str,
    required: bool,
    limits: BundleLimits,
) -> Result<(), BundleError>
where
    S: FixedArtifactSource + ?Sized,
{
    let bytes = fixed_bytes(artifacts, artifact)?;
    validate_json_bytes(bytes, limits)?;
    preflight_artifact_collection(
        &bytes[..bytes.len() - 1],
        path,
        kind,
        maximum,
        resource,
        required,
    )
}

fn fixed_bytes<'a, S>(artifacts: &'a S, name: &str) -> Result<&'a [u8], BundleError>
where
    S: FixedArtifactSource + ?Sized,
{
    artifacts
        .artifact_bytes(name)
        .ok_or(BundleError::ArtifactSetMismatch)
}

fn decode_json<'a, T>(bytes: &'a [u8], limits: BundleLimits) -> Result<T, BundleError>
where
    T: Deserialize<'a> + Serialize,
{
    validate_json_bytes(bytes, limits)?;
    let value = map_decode_result(|| serde_json::from_slice(&bytes[..bytes.len() - 1]))?;
    validate_canonical_json(bytes, &value, limits)?;
    Ok(value)
}

fn decode_raw_samples(
    bytes: &[u8],
    maximum_count: usize,
    limits: BundleLimits,
    resource: &'static str,
) -> Result<Vec<RawSample>, BundleError> {
    if bytes.is_empty() {
        return Ok(Vec::new());
    }
    validate_artifact_size(bytes, limits)?;
    if !bytes.ends_with(b"\n") {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    let line_count = bytes[..bytes.len() - 1]
        .split(|byte| *byte == b'\n')
        .count();
    if line_count > maximum_count {
        return Err(BundleError::LimitExceeded { resource });
    }
    let mut values = Vec::new();
    values
        .try_reserve_exact(line_count)
        .map_err(|_| BundleError::AllocationFailed)?;
    for line in bytes[..bytes.len() - 1].split(|byte| *byte == b'\n') {
        if line.is_empty() || line.contains(&b'\r') {
            return Err(BundleError::InvalidArtifactEncoding);
        }
        if values.len() >= maximum_count {
            return Err(BundleError::LimitExceeded { resource });
        }
        let value: WireRawSample<'_> = map_decode_result(|| serde_json::from_slice(line))?;
        values.push(value.into_owned()?);
    }
    let canonical_limit =
        usize::try_from(limits.max_artifact_bytes).map_err(|_| BundleError::LimitExceeded {
            resource: "artifact_bytes",
        })?;
    if json_lines(&values, canonical_limit)? != bytes {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(values)
}

fn decode_agent_trajectories(bytes: &[u8], limits: BundleLimits) -> Result<(), BundleError> {
    if bytes.is_empty() {
        return Ok(());
    }
    validate_artifact_size(bytes, limits)?;
    if !bytes.ends_with(b"\n") {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    let lines = bytes[..bytes.len() - 1].split(|byte| *byte == b'\n');
    let line_count = lines.clone().count();
    if line_count > limits.max_agent_trajectories {
        return Err(BundleError::LimitExceeded {
            resource: "agent_trajectory_count",
        });
    }
    for line in lines {
        if line.is_empty() || line.contains(&b'\r') {
            return Err(BundleError::InvalidArtifactEncoding);
        }
        preflight_artifact_collection(
            line,
            &["tool_calls"],
            CollectionKind::Array,
            limits.max_command_arguments,
            "trajectory_tool_call_count",
            true,
        )?;
    }
    Err(BundleError::UnsupportedTrajectorySchema)
}

fn validate_canonical_json(
    bytes: &[u8],
    value: &impl serde::Serialize,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    let canonical_limit =
        usize::try_from(limits.max_artifact_bytes).map_err(|_| BundleError::LimitExceeded {
            resource: "artifact_bytes",
        })?;
    if json_bytes(value, canonical_limit)? != bytes {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn validate_json_bytes(bytes: &[u8], limits: BundleLimits) -> Result<(), BundleError> {
    validate_artifact_size(bytes, limits)?;
    if bytes.len() <= 1
        || !bytes.ends_with(b"\n")
        || bytes[..bytes.len() - 1]
            .iter()
            .any(|byte| matches!(byte, b'\n' | b'\r'))
    {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn validate_artifact_size(bytes: &[u8], limits: BundleLimits) -> Result<(), BundleError> {
    let length = u64::try_from(bytes.len()).map_err(|_| BundleError::LimitExceeded {
        resource: "artifact_bytes",
    })?;
    if length > limits.max_artifact_bytes {
        return Err(BundleError::LimitExceeded {
            resource: "artifact_bytes",
        });
    }
    Ok(())
}

fn map_decode_error(error: crate::DecodeError) -> BundleError {
    match error {
        crate::DecodeError::Limits(source) => source,
        crate::DecodeError::LimitExceeded { resource } => BundleError::LimitExceeded { resource },
        crate::DecodeError::AllocationFailed => BundleError::AllocationFailed,
        crate::DecodeError::InvalidSchema => BundleError::UnsupportedSchemaVersion,
        _ => BundleError::InvalidArtifactEncoding,
    }
}

fn map_parser_integrity_error(error: crate::ParserRunError) -> BundleError {
    match error {
        crate::ParserRunError::AllocationFailed => BundleError::AllocationFailed,
        _ => BundleError::ArtifactInvariantViolation,
    }
}

fn validate_fixed_bundle(
    fixed: &FixedArtifacts<'_>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    validate_environment(&fixed.environment)?;
    validate_manifest_revision(&fixed.dataset_manifest)?;
    validate_build_provenance(&fixed.environment, &fixed.build_provenance, limits)?;
    validate_samples(
        &fixed.dataset_manifest,
        &fixed.schedule,
        &fixed.raw_samples,
        limits,
    )?;
    validate_summary(&fixed.raw_samples, &fixed.summary, limits)?;
    validate_coverage(
        &fixed.dataset_manifest,
        &fixed.raw_samples,
        &fixed.coverage,
        limits,
    )?;
    validate_quality(&fixed.raw_samples, &fixed.summary, &fixed.quality, limits)
}

fn validate_environment(environment: &WireEnvironment<'_>) -> Result<(), BundleError> {
    validate_schema(environment.schema_version)?;
    if !matches!(
        environment.feature_profile,
        "debug" | "release" | "test" | "bench"
    ) {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    for value in [
        &environment.cpu_model,
        &environment.cpu_topology,
        &environment.kernel,
        &environment.filesystem,
        &environment.storage_device,
        &environment.power_mode,
        &environment.container_limits,
        &environment.sqlite,
        &environment.locale,
        &environment.background_process_policy,
    ] {
        validate_unavailable_environment(value)?;
    }
    validate_environment_value(&environment.operating_system, |value| {
        if matches!(value, "linux" | "macos" | "windows") {
            Ok(())
        } else {
            Err(BundleError::InvalidArtifactEncoding)
        }
    })?;
    validate_environment_value(&environment.compiler, validate_compiler_token)?;
    validate_environment_value(&environment.clock_source, |value| {
        if value == "std_instant_monotonic" {
            Ok(())
        } else {
            Err(BundleError::InvalidArtifactEncoding)
        }
    })?;
    validate_environment_scalar(&environment.ram_bytes)?;
    validate_environment_value(&environment.binary_sha256, validate_digest)?;
    validate_environment_map(
        &environment.adapter_versions,
        &["rootlight-adapter-treesitter", "tree-sitter-runtime"],
        validate_version_token,
    )?;
    validate_environment_map(
        &environment.grammar_versions,
        &["java", "javascript", "python", "rust"],
        validate_version_token,
    )?;
    validate_environment_map(
        &environment.grammar_source_package_checksums,
        &["java", "javascript", "python", "rust"],
        validate_digest,
    )?;
    validate_environment_map(
        &environment.grammar_hashes,
        &[
            "java.parser",
            "javascript.parser",
            "javascript.scanner",
            "python.parser",
            "python.scanner",
            "rust.parser",
            "rust.scanner",
        ],
        validate_digest,
    )?;
    match &environment.process_tree_accounting {
        WireAvailability::Available => Ok(()),
        WireAvailability::Unavailable { reason_code } => validate_environment_reason(reason_code),
        WireAvailability::Failed { .. } => Err(BundleError::InvalidArtifactEncoding),
    }
}

fn validate_manifest_revision(manifest: &DatasetManifest) -> Result<(), BundleError> {
    let mut hasher = Sha256::new();
    for entry in &manifest.entries {
        hash_length_prefixed(&mut hasher, entry.id.as_bytes())?;
        hash_length_prefixed(&mut hasher, entry.source_sha256.as_bytes())?;
    }
    let expected = format!("sha256:{}", hex_digest(hasher.finalize()));
    if manifest.revision != expected {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_build_provenance(
    environment: &WireEnvironment<'_>,
    provenance: &WireBuildProvenance<'_>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    validate_schema(provenance.schema_version)?;
    validate_revision(provenance.source_revision)?;
    validate_label(provenance.build_profile, limits)?;
    validate_label(provenance.target, limits)?;
    validate_sorted_wire_labels(provenance.features.as_slice(), limits)?;
    let WireEvidence::Observed {
        value: binary_sha256,
    } = environment.binary_sha256
    else {
        return Err(BundleError::ArtifactInvariantViolation);
    };
    if provenance.binary_revision.strip_prefix("sha256:") != Some(binary_sha256)
        || provenance.build_profile != environment.feature_profile
    {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_samples(
    manifest: &DatasetManifest,
    schedule: &[ScheduledSample],
    samples: &[RawSample],
    limits: BundleLimits,
) -> Result<(), BundleError> {
    if samples.len() != schedule.len() {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    for (ordinal, (sample, scheduled)) in samples.iter().zip(schedule).enumerate() {
        validate_schema(&sample.schema_version)?;
        let expected_ordinal =
            u64::try_from(ordinal).map_err(|_| BundleError::ArtifactInvariantViolation)?;
        if sample.ordinal != expected_ordinal {
            return Err(BundleError::ArtifactInvariantViolation);
        }
        validate_label(&sample.dataset_entry_id, limits)?;
        validate_label(&sample.grammar_family, limits)?;
        if !matches!(sample.phase.as_str(), "warmup" | "trial") {
            return Err(BundleError::InvalidArtifactEncoding);
        }
        let entry = manifest
            .entries
            .get(scheduled.input_index)
            .ok_or(BundleError::ArtifactInvariantViolation)?;
        if sample.phase != scheduled.phase.as_str()
            || sample.dataset_entry_id != entry.id
            || sample.grammar_family != entry.grammar_family
            || sample.source_bytes != entry.source_bytes
            || sample.physical_lines != entry.physical_lines
        {
            return Err(BundleError::ArtifactInvariantViolation);
        }
        validate_observation(&sample.semantic_facts, limits)?;
        validate_observation(&sample.process_tree_cpu_ns, limits)?;
        validate_observation(&sample.process_tree_peak_rss_bytes, limits)?;
        validate_sample_outcome(sample, limits)?;
    }
    Ok(())
}

fn validate_sample_outcome(sample: &RawSample, limits: BundleLimits) -> Result<(), BundleError> {
    match &sample.outcome {
        SampleOutcome::Succeeded => {}
        SampleOutcome::Failed { error_code } => {
            validate_reason(error_code, limits)?;
        }
        SampleOutcome::TimedOut | SampleOutcome::Cancelled => {}
    }
    if !matches!(sample.outcome, SampleOutcome::Succeeded)
        && (sample.syntax_nodes != 0
            || sample.syntax_facts != 0
            || !matches!(sample.semantic_facts, EvidenceValue::Unavailable { .. }))
    {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_summary(
    samples: &[RawSample],
    summary: &WireSummary<'_>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    validate_schema(summary.schema_version)?;
    validate_label(summary.benchmark_id, limits)?;
    validate_wire_availability(&summary.semantic_eligibility, limits)?;
    validate_wire_availability(&summary.confidence_intervals, limits)?;
    if summary.families.len() > limits.max_manifest_entries {
        return Err(BundleError::LimitExceeded {
            resource: "summary_family_count",
        });
    }
    for (family, distribution) in summary.families.iter() {
        validate_label(family, limits)?;
        validate_distribution(distribution, limits)?;
    }
    let fences = outlier_fences(samples).map_err(map_parser_integrity_error)?;
    for sample in samples {
        let expected =
            if sample.phase == "trial" && matches!(sample.outcome, SampleOutcome::Succeeded) {
                fences
                    .get(&sample.grammar_family)
                    .is_some_and(|(lower, upper)| {
                        let elapsed = u128::from(sample.elapsed_ns);
                        elapsed < *lower || elapsed > *upper
                    })
            } else {
                false
            };
        if sample.is_outlier != expected {
            return Err(BundleError::ArtifactInvariantViolation);
        }
    }
    let expected = summarize(samples, summary.semantic_eligibility.to_owned()?)
        .map_err(map_parser_integrity_error)?;
    if !summary_matches(summary, &expected) {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_distribution(
    distribution: &WireMetricDistribution<'_>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    for value in [
        &distribution.p50_ns,
        &distribution.p95_ns,
        &distribution.p99_ns,
        &distribution.physical_lines_per_second,
        &distribution.files_per_second,
        &distribution.syntax_nodes_per_second,
        &distribution.syntax_facts_per_source_byte_ppm,
    ] {
        validate_wire_observation(value, limits)?;
    }
    if distribution.outlier_count > distribution.sample_count {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn summary_matches(summary: &WireSummary<'_>, expected: &ResultSummary) -> bool {
    summary.schema_version == expected.schema_version
        && summary.benchmark_id == expected.benchmark_id
        && summary
            .semantic_eligibility
            .matches(&expected.semantic_eligibility)
        && summary.failed_samples == expected.failed_samples
        && summary.timed_out_samples == expected.timed_out_samples
        && summary.cancelled_samples == expected.cancelled_samples
        && summary
            .confidence_intervals
            .matches(&expected.confidence_intervals)
        && summary.families.len() == expected.families.len()
        && summary.families.iter().zip(&expected.families).all(
            |((left_family, left), (right_family, right))| {
                left_family == right_family && left == right
            },
        )
}

impl PartialEq<MetricDistribution> for WireMetricDistribution<'_> {
    fn eq(&self, expected: &MetricDistribution) -> bool {
        self.sample_count == expected.sample_count
            && self.p50_ns.matches(&expected.p50_ns)
            && self.p95_ns.matches(&expected.p95_ns)
            && self.p99_ns.matches(&expected.p99_ns)
            && self
                .physical_lines_per_second
                .matches(&expected.physical_lines_per_second)
            && self.files_per_second.matches(&expected.files_per_second)
            && self
                .syntax_nodes_per_second
                .matches(&expected.syntax_nodes_per_second)
            && self
                .syntax_facts_per_source_byte_ppm
                .matches(&expected.syntax_facts_per_source_byte_ppm)
            && self.outlier_count == expected.outlier_count
    }
}

fn validate_coverage(
    manifest: &DatasetManifest,
    samples: &[RawSample],
    coverage: &WireCoverage<'_>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    validate_schema(coverage.schema_version)?;
    if coverage.skipped.len() > limits.max_manifest_entries
        || coverage.parser_status.len() > limits.max_manifest_entries
    {
        return Err(BundleError::LimitExceeded {
            resource: "coverage_entry_count",
        });
    }
    if !coverage.skipped.is_empty() {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    let mut statuses = Vec::new();
    statuses
        .try_reserve_exact(manifest.entries.len())
        .map_err(|_| BundleError::AllocationFailed)?;
    statuses.extend(
        manifest
            .entries
            .iter()
            .map(|entry| (entry.id.as_str(), "succeeded")),
    );
    for sample in samples.iter().filter(|sample| sample.phase == "trial") {
        let observed = outcome_status(&sample.outcome);
        let index = statuses
            .binary_search_by_key(&sample.dataset_entry_id.as_str(), |(entry, _)| *entry)
            .map_err(|_| BundleError::ArtifactInvariantViolation)?;
        let status = &mut statuses[index].1;
        if status_severity(observed) > status_severity(status) {
            *status = observed;
        }
    }
    for (entry, status) in coverage.parser_status.iter() {
        validate_label(entry, limits)?;
        if !matches!(*status, "succeeded" | "cancelled" | "timed_out" | "failed") {
            return Err(BundleError::InvalidArtifactEncoding);
        }
    }
    let attempted =
        u64::try_from(statuses.len()).map_err(|_| BundleError::ArtifactInvariantViolation)?;
    let committed = u64::try_from(
        statuses
            .iter()
            .filter(|(_, status)| *status == "succeeded")
            .count(),
    )
    .map_err(|_| BundleError::ArtifactInvariantViolation)?;
    let statuses_match = coverage
        .parser_status
        .iter()
        .map(|(entry, status)| (entry, *status))
        .eq(statuses.iter().copied());
    if !statuses_match
        || coverage.attempted_entries != attempted
        || coverage.committed_entries != committed
    {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn outcome_status(outcome: &SampleOutcome) -> &'static str {
    match outcome {
        SampleOutcome::Succeeded => "succeeded",
        SampleOutcome::Cancelled => "cancelled",
        SampleOutcome::TimedOut => "timed_out",
        SampleOutcome::Failed { .. } => "failed",
    }
}

fn status_severity(status: &str) -> u8 {
    match status {
        "succeeded" => 0,
        "cancelled" => 1,
        "timed_out" => 2,
        "failed" => 3,
        _ => u8::MAX,
    }
}

fn validate_quality(
    samples: &[RawSample],
    summary: &WireSummary<'_>,
    quality: &WireQuality<'_>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    validate_schema(quality.schema_version)?;
    if quality.rubric_id != SEMANTIC_QUALITY_RUBRIC_ID {
        return Err(BundleError::UnsupportedRubricVersion);
    }
    validate_wire_availability(&quality.semantic_eligibility, limits)?;
    validate_wire_observation(&quality.precision_ppm, limits)?;
    validate_wire_observation(&quality.recall_ppm, limits)?;
    validate_wire_observation(&quality.expected_calibration_error_ppm, limits)?;
    if quality.unsupported_cases.len() > limits.max_manifest_entries {
        return Err(BundleError::LimitExceeded {
            resource: "unsupported_case_count",
        });
    }
    for category in quality.unsupported_cases.keys() {
        validate_label(category, limits)?;
    }
    let fact_eligibility = semantic_fact_eligibility(samples);
    let precision = quality.precision_ppm.to_owned()?;
    let recall = quality.recall_ppm.to_owned()?;
    let calibration = quality.expected_calibration_error_ppm.to_owned()?;
    let expected = semantic_quality_eligibility_from_values(
        &fact_eligibility,
        &precision,
        &recall,
        &calibration,
    );
    if !quality.semantic_eligibility.matches(&expected)
        || !summary.semantic_eligibility.matches(&expected)
    {
        return Err(BundleError::ArtifactInvariantViolation);
    }
    Ok(())
}

fn validate_environment_value(
    value: &WireEvidence<'_, &str>,
    validate: impl FnOnce(&str) -> Result<(), BundleError>,
) -> Result<(), BundleError> {
    match value {
        WireEvidence::Observed { value } => validate(value),
        WireEvidence::Unavailable { reason_code } => validate_environment_reason(reason_code),
        WireEvidence::Target { .. } => Err(BundleError::InvalidArtifactEncoding),
    }
}

fn validate_unavailable_environment<T>(value: &WireEvidence<'_, T>) -> Result<(), BundleError> {
    match value {
        WireEvidence::Unavailable { reason_code } => validate_environment_reason(reason_code),
        WireEvidence::Observed { .. } | WireEvidence::Target { .. } => {
            Err(BundleError::InvalidArtifactEncoding)
        }
    }
}

fn validate_environment_scalar<T>(value: &WireEvidence<'_, T>) -> Result<(), BundleError> {
    match value {
        WireEvidence::Observed { .. } => Ok(()),
        WireEvidence::Unavailable { reason_code } => validate_environment_reason(reason_code),
        WireEvidence::Target { .. } => Err(BundleError::InvalidArtifactEncoding),
    }
}

fn validate_environment_map(
    value: &WireEvidence<'_, FallibleMap<'_, &str>>,
    expected_keys: &[&str],
    validate_value: fn(&str) -> Result<(), BundleError>,
) -> Result<(), BundleError> {
    match value {
        WireEvidence::Observed { value } => {
            if value.len() != expected_keys.len() || !value.keys().eq(expected_keys.iter().copied())
            {
                return Err(BundleError::InvalidArtifactEncoding);
            }
            for (_, mapped) in value.iter() {
                validate_value(mapped)?;
            }
            Ok(())
        }
        WireEvidence::Unavailable { reason_code } => validate_environment_reason(reason_code),
        WireEvidence::Target { .. } => Err(BundleError::InvalidArtifactEncoding),
    }
}

fn validate_environment_reason(value: &str) -> Result<(), BundleError> {
    if matches!(
        value,
        "not_sampled"
            | "not_in_scope"
            | "host_inventory_not_collected"
            | "sqlite_not_in_scope"
            | "background_process_policy_not_recorded"
            | "platform_process_tree_sampler_not_integrated"
    ) {
        Ok(())
    } else {
        Err(BundleError::InvalidArtifactEncoding)
    }
}

fn validate_compiler_token(value: &str) -> Result<(), BundleError> {
    let release = value
        .strip_prefix("rustc-")
        .ok_or(BundleError::InvalidArtifactEncoding)?;
    validate_version_token(release)
}

fn validate_version_token(value: &str) -> Result<(), BundleError> {
    let mut components = value.split('.');
    if value.len() > 64 {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    for _ in 0..3 {
        let component = components
            .next()
            .ok_or(BundleError::InvalidArtifactEncoding)?;
        if component.is_empty()
            || !component.bytes().all(|byte| byte.is_ascii_digit())
            || (component.len() > 1 && component.starts_with('0'))
        {
            return Err(BundleError::InvalidArtifactEncoding);
        }
    }
    if components.next().is_some() {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn validate_observation<T>(
    value: &EvidenceValue<T>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    match value {
        EvidenceValue::Observed { .. } => Ok(()),
        EvidenceValue::Unavailable { reason_code } => validate_reason(reason_code, limits),
        EvidenceValue::Target { .. } => Err(BundleError::ArtifactInvariantViolation),
    }
}

fn validate_wire_observation<T>(
    value: &WireEvidence<'_, T>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    match value {
        WireEvidence::Observed { .. } => Ok(()),
        WireEvidence::Unavailable { reason_code } => validate_reason(reason_code, limits),
        WireEvidence::Target { .. } => Err(BundleError::ArtifactInvariantViolation),
    }
}

fn validate_wire_availability(
    availability: &WireAvailability<'_>,
    limits: BundleLimits,
) -> Result<(), BundleError> {
    match availability {
        WireAvailability::Available => Ok(()),
        WireAvailability::Failed { reason_code }
        | WireAvailability::Unavailable { reason_code } => validate_reason(reason_code, limits),
    }
}

fn validate_schema(schema: &str) -> Result<(), BundleError> {
    if schema != RESULT_BUNDLE_SCHEMA_VERSION {
        return Err(BundleError::UnsupportedSchemaVersion);
    }
    Ok(())
}

fn validate_sorted_wire_labels(values: &[&str], limits: BundleLimits) -> Result<(), BundleError> {
    if values.len() > limits.max_command_arguments {
        return Err(BundleError::LimitExceeded {
            resource: "feature_count",
        });
    }
    let mut prior = None;
    for value in values {
        validate_label(value, limits)?;
        if prior.is_some_and(|previous| *value <= previous) {
            return Err(BundleError::ArtifactInvariantViolation);
        }
        prior = Some(*value);
    }
    Ok(())
}

fn validate_label(value: &str, limits: BundleLimits) -> Result<(), BundleError> {
    if value.is_empty()
        || value.len() > limits.max_string_bytes
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+'))
    {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn validate_reason(value: &str, limits: BundleLimits) -> Result<(), BundleError> {
    validate_label(value, limits)
}

fn validate_digest(value: &str) -> Result<(), BundleError> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn validate_revision(value: &str) -> Result<(), BundleError> {
    if !matches!(value.len(), 40 | 64)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    {
        return Err(BundleError::InvalidArtifactEncoding);
    }
    Ok(())
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) -> Result<(), BundleError> {
    let length = u64::try_from(bytes.len()).map_err(|_| BundleError::ArtifactInvariantViolation)?;
    hasher.update(length.to_be_bytes());
    hasher.update(bytes);
    Ok(())
}

fn hex_digest(digest: impl AsRef<[u8]>) -> String {
    let mut output = String::with_capacity(digest.as_ref().len() * 2);
    for byte in digest.as_ref() {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

#[cfg(test)]
mod tests {
    use std::fmt::Write as _;

    use super::*;

    #[test]
    fn decoded_retention_budget_is_shared_and_monotonic() {
        let mut budget = FixedDecodeBudget {
            retained_bytes: 0,
            retained_items: 0,
            max_retained_bytes: 3,
            max_retained_items: 2,
            max_string_bytes: 64,
        };
        budget
            .inspect_json(br#""ab""#)
            .expect("first borrowed string fits");
        assert!(matches!(
            budget.inspect_json(br#""cd""#),
            Err(BundleError::LimitExceeded {
                resource: "decoded_retained_bytes"
            })
        ));

        let mut item_budget = FixedDecodeBudget {
            retained_bytes: 0,
            retained_items: 0,
            max_retained_bytes: 64,
            max_retained_items: 2,
            max_string_bytes: 64,
        };
        assert!(matches!(
            item_budget.inspect_json(br#"[0,1,2]"#),
            Err(BundleError::LimitExceeded {
                resource: "decoded_retained_items"
            })
        ));
    }

    #[test]
    fn borrowed_fixed_decoding_rejects_escapes_and_reports_reservation_failure() {
        let mut budget = FixedDecodeBudget {
            retained_bytes: 0,
            retained_items: 0,
            max_retained_bytes: 64,
            max_retained_items: 64,
            max_string_bytes: 64,
        };
        assert!(matches!(
            budget.inspect_json(br#""source\u002fsecret""#),
            Err(BundleError::InvalidArtifactEncoding)
        ));

        set_decode_reservation_fail_after(Some(0));
        let decoded = serde_json::from_slice::<FallibleMap<'_, u64>>(br#"{"entry":1}"#);
        set_decode_reservation_fail_after(None);
        assert!(decoded.is_err());
    }

    #[test]
    fn tagged_wire_visitors_reject_unknown_fields_without_buffering() {
        let mut flooded_availability = String::from("{\"status\":\"available\"");
        for index in 0..4_096 {
            write!(flooded_availability, ",\"unknown_{index:04}\":null")
                .expect("writing to a string succeeds");
        }
        flooded_availability.push_str("}\n");

        set_decode_reservation_fail_after(Some(0));
        assert!(matches!(
            decode_json::<WireAvailability<'_>>(
                flooded_availability.as_bytes(),
                BundleLimits::default()
            ),
            Err(BundleError::InvalidArtifactEncoding)
        ));
        DECODE_RESERVATION_FAIL_AFTER.with(|remaining| assert_eq!(remaining.get(), Some(0)));
        set_decode_reservation_fail_after(None);

        for evidence in [
            br#"{"status":"observed","value":1,"unknown":null}
"#
            .as_slice(),
            br#"{"status":"observed","status":"observed","value":1}
"#,
        ] {
            assert!(matches!(
                decode_json::<WireEvidence<'_, u64>>(evidence, BundleLimits::default()),
                Err(BundleError::InvalidArtifactEncoding)
            ));
        }
        assert!(matches!(
            decode_json::<WireSampleOutcome<'_>>(
                br#"{"status":"succeeded","unknown":null}
"#,
                BundleLimits::default()
            ),
            Err(BundleError::InvalidArtifactEncoding)
        ));
    }

    #[test]
    fn allocation_failures_survive_generic_and_raw_decode_entrypoints() {
        set_decode_reservation_fail_after(Some(0));
        let fixed = decode_json::<WireEvidence<'_, FallibleMap<'_, &'_ str>>>(
            br#"{"status":"observed","value":{"entry":"1"}}
"#,
            BundleLimits::default(),
        );
        set_decode_reservation_fail_after(None);
        assert!(matches!(fixed, Err(BundleError::AllocationFailed)));

        set_decode_reservation_fail_after(Some(0));
        let raw = decode_raw_samples(
            valid_raw_sample().as_bytes(),
            1,
            BundleLimits::default(),
            "raw_sample_count",
        );
        set_decode_reservation_fail_after(None);
        assert!(matches!(raw, Err(BundleError::AllocationFailed)));
    }

    #[test]
    fn raw_sample_outcome_rejects_unknown_field_flood_without_retention() {
        let mut outcome = String::from("{\"status\":\"succeeded\"");
        for index in 0..4_096 {
            write!(outcome, ",\"unknown_{index:04}\":null").expect("writing to a string succeeds");
        }
        outcome.push('}');
        let raw = valid_raw_sample().replace("{\"status\":\"succeeded\"}", &outcome);

        set_decode_reservation_fail_after(Some(0));
        assert!(matches!(
            decode_raw_samples(
                raw.as_bytes(),
                1,
                BundleLimits::default(),
                "raw_sample_count"
            ),
            Err(BundleError::InvalidArtifactEncoding)
        ));
        DECODE_RESERVATION_FAIL_AFTER.with(|remaining| assert_eq!(remaining.get(), Some(0)));
        set_decode_reservation_fail_after(None);
    }

    fn valid_raw_sample() -> String {
        concat!(
            "{\"schema_version\":\"2.0\",\"ordinal\":0,\"phase\":\"measured\",",
            "\"dataset_entry_id\":\"entry\",\"grammar_family\":\"rust\",\"elapsed_ns\":1,",
            "\"source_bytes\":1,\"physical_lines\":1,\"syntax_nodes\":1,\"syntax_facts\":1,",
            "\"semantic_facts\":{\"status\":\"unavailable\",\"reason_code\":\"not_measured\"},",
            "\"process_tree_cpu_ns\":{\"status\":\"unavailable\",\"reason_code\":\"not_measured\"},",
            "\"process_tree_peak_rss_bytes\":{\"status\":\"unavailable\",",
            "\"reason_code\":\"not_measured\"},\"outcome\":{\"status\":\"succeeded\"},",
            "\"is_outlier\":false}\n"
        )
        .to_owned()
    }
}
