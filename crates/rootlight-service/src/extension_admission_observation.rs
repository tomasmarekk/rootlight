//! Test-only namespace/version census at a failed required-extension byte admission.
//! An owned thread-local guard captures bounded, source-free aggregates without
//! replacing the original quota error or changing unarmed service work.

use std::{
    cell::RefCell,
    collections::BTreeMap,
    io::{self, Write},
    marker::PhantomData,
    rc::Rc,
};

use rootlight_ir::{
    ExtensionEnvelope, LEXICAL_EXTENSION_NAMESPACE, SYMBOL_IDENTITY_CLAIM_NAMESPACE,
};
use serde::Serialize;

use super::{
    Cancellation, FirstSliceError, FirstSliceResource, check_cancellation, resource_limit,
};

const MAX_OBSERVATION_ROWS: usize = 256;
const MAX_NAMESPACE_BYTES: usize = 1024;
const MAX_OBSERVATION_BYTES: usize = 256 * 1024;

struct CaptureState {
    result: Option<Result<Vec<u8>, FirstSliceError>>,
    maximum_bytes: usize,
}

thread_local! {
    static CAPTURE: RefCell<Option<CaptureState>> = const { RefCell::new(None) };
}

#[must_use]
struct CaptureGuard {
    _thread: PhantomData<Rc<()>>,
}

impl CaptureGuard {
    fn arm(maximum_bytes: usize) -> Result<Self, FirstSliceError> {
        CAPTURE.with(|slot| {
            let mut slot = slot
                .try_borrow_mut()
                .map_err(|_| FirstSliceError::Retention)?;
            if slot.is_some() || maximum_bytes > MAX_OBSERVATION_BYTES {
                return Err(FirstSliceError::Identity);
            }
            *slot = Some(CaptureState {
                result: None,
                maximum_bytes,
            });
            Ok(Self {
                _thread: PhantomData,
            })
        })
    }

    fn finish(self) -> Result<Vec<u8>, FirstSliceError> {
        CAPTURE.with(|slot| {
            slot.try_borrow_mut()
                .map_err(|_| FirstSliceError::Retention)?
                .take()
                .and_then(|state| state.result)
                .ok_or(FirstSliceError::Identity)?
        })
    }

    fn reached(&self) -> bool {
        CAPTURE.with(|slot| {
            slot.try_borrow()
                .is_ok_and(|slot| slot.as_ref().is_some_and(|state| state.result.is_some()))
        })
    }
}

impl Drop for CaptureGuard {
    fn drop(&mut self) {
        // Destruction must remain safe during unwinding or thread-local teardown.
        let _ = CAPTURE.try_with(|slot| {
            if let Ok(mut slot) = slot.try_borrow_mut() {
                slot.take();
            }
        });
    }
}

#[derive(Clone, Copy, Default, Serialize)]
struct Usage {
    envelope_count: usize,
    inner_utf8_payload_bytes: usize,
}

impl Usage {
    fn add(&mut self, other: Self) -> Result<(), FirstSliceError> {
        self.envelope_count = self
            .envelope_count
            .checked_add(other.envelope_count)
            .ok_or(FirstSliceError::Limits)?;
        self.inner_utf8_payload_bytes = self
            .inner_utf8_payload_bytes
            .checked_add(other.inner_utf8_payload_bytes)
            .ok_or(FirstSliceError::Limits)?;
        Ok(())
    }
}

#[derive(Default, Serialize)]
struct PartitionUsage {
    target: Usage,
    incoming: Usage,
    combined: Usage,
}

#[derive(Serialize)]
struct NamespaceRow<'a> {
    namespace: &'a str,
    version: &'a str,
    retention: &'static str,
    usage: PartitionUsage,
}

#[derive(Serialize)]
struct QuotaError {
    code: &'static str,
    resource: &'static str,
    observed: usize,
    limit: usize,
}

#[derive(Serialize)]
struct Observation<'a> {
    schema: &'static str,
    boundary: &'static str,
    original_error: QuotaError,
    required: PartitionUsage,
    lexical_optional: PartitionUsage,
    symbol_claim_envelopes: PartitionUsage,
    required_bytes_reconciled: bool,
    symbol_claim_envelopes_are_not_distinct_symbols: bool,
    whole_repository_observed: bool,
    source_text_included: bool,
    payload_contents_included: bool,
    product_success_qualified: bool,
    benchmark_qualified: bool,
    rows: Vec<NamespaceRow<'a>>,
}

struct BoundedJson<'a> {
    bytes: Vec<u8>,
    maximum: usize,
    cancellation: &'a Cancellation,
    failure: Option<FirstSliceError>,
}

impl Write for BoundedJson<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let result = (|| {
            check_cancellation(self.cancellation)?;
            let length = self
                .bytes
                .len()
                .checked_add(bytes.len())
                .ok_or(FirstSliceError::Limits)?;
            if length > self.maximum {
                return Err(resource_limit(
                    FirstSliceResource::OutputBytes,
                    length,
                    self.maximum,
                ));
            }
            self.bytes
                .try_reserve(bytes.len())
                .map_err(|_| FirstSliceError::Retention)?;
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        })();
        match result {
            Ok(length) => Ok(length),
            Err(error) => {
                self.failure = Some(error);
                Err(io::Error::other(
                    "extension admission observation is unavailable",
                ))
            }
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn encode_json(
    value: &impl Serialize,
    maximum: usize,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, FirstSliceError> {
    let mut writer = BoundedJson {
        bytes: Vec::new(),
        maximum,
        cancellation,
        failure: None,
    };
    if serde_json::to_writer(&mut writer, value).is_err() {
        return Err(writer.failure.unwrap_or(FirstSliceError::Identity));
    }
    check_cancellation(cancellation)?;
    Ok(writer.bytes)
}

fn census<'a>(
    target: &'a [ExtensionEnvelope],
    incoming: &'a [ExtensionEnvelope],
    observed_required_bytes: usize,
    limit: usize,
    maximum_bytes: usize,
    cancellation: &Cancellation,
) -> Result<Vec<u8>, FirstSliceError> {
    if observed_required_bytes <= limit {
        return Err(FirstSliceError::Identity);
    }
    let mut namespaces = BTreeMap::<(&str, &str), NamespaceRow<'a>>::new();
    for (extensions, retained) in [(target, true), (incoming, false)] {
        for extension in extensions {
            check_cancellation(cancellation)?;
            let namespace = extension.namespace.as_str();
            let version = extension.version.as_str();
            let key_bytes = namespace
                .len()
                .checked_add(version.len())
                .ok_or(FirstSliceError::Limits)?;
            if key_bytes > MAX_NAMESPACE_BYTES {
                return Err(FirstSliceError::Limits);
            }
            let key = (namespace, version);
            if namespaces.len() == MAX_OBSERVATION_ROWS && !namespaces.contains_key(&key) {
                return Err(FirstSliceError::Limits);
            }
            let row = namespaces.entry(key).or_insert_with(|| NamespaceRow {
                namespace,
                version,
                retention: if namespace == LEXICAL_EXTENSION_NAMESPACE {
                    "lexical_optional"
                } else {
                    "nonlexical_required"
                },
                usage: PartitionUsage::default(),
            });
            let usage = Usage {
                envelope_count: 1,
                inner_utf8_payload_bytes: extension.payload.len(),
            };
            let partition = if retained {
                &mut row.usage.target
            } else {
                &mut row.usage.incoming
            };
            partition.add(usage)?;
            row.usage.combined.add(usage)?;
        }
    }
    let mut required = PartitionUsage::default();
    let mut lexical_optional = PartitionUsage::default();
    let mut symbol_claim_envelopes = PartitionUsage::default();
    let mut rows = Vec::new();
    rows.try_reserve_exact(namespaces.len())
        .map_err(|_| FirstSliceError::Retention)?;
    for row in namespaces.into_values() {
        check_cancellation(cancellation)?;
        let totals = if row.namespace == LEXICAL_EXTENSION_NAMESPACE {
            &mut lexical_optional
        } else {
            &mut required
        };
        totals.target.add(row.usage.target)?;
        totals.incoming.add(row.usage.incoming)?;
        totals.combined.add(row.usage.combined)?;
        if row.namespace == SYMBOL_IDENTITY_CLAIM_NAMESPACE {
            symbol_claim_envelopes.target.add(row.usage.target)?;
            symbol_claim_envelopes.incoming.add(row.usage.incoming)?;
            symbol_claim_envelopes.combined.add(row.usage.combined)?;
        }
        rows.push(row);
    }
    if required
        .target
        .inner_utf8_payload_bytes
        .checked_add(required.incoming.inner_utf8_payload_bytes)
        != Some(observed_required_bytes)
        || required.combined.inner_utf8_payload_bytes != observed_required_bytes
    {
        return Err(FirstSliceError::Identity);
    }
    encode_json(
        &Observation {
            schema: "rootlight.failed-extension-admission-observation/2",
            boundary: "required_extension_bytes_before_optional_retention",
            original_error: QuotaError {
                code: "resource_limit",
                resource: "extension_bytes",
                observed: observed_required_bytes,
                limit,
            },
            required,
            lexical_optional,
            symbol_claim_envelopes,
            required_bytes_reconciled: true,
            symbol_claim_envelopes_are_not_distinct_symbols: true,
            whole_repository_observed: false,
            source_text_included: false,
            payload_contents_included: false,
            product_success_qualified: false,
            benchmark_qualified: false,
            rows,
        },
        maximum_bytes,
        cancellation,
    )
}

/// Captures the armed thread's first failed byte admission without returning errors.
///
/// Both aggregate and reporting failures belong to the capture guard alone. The
/// caller must return its original quota error regardless of this hook's outcome.
pub(super) fn observe(
    target: &[ExtensionEnvelope],
    incoming: &[ExtensionEnvelope],
    observed_required_bytes: usize,
    limit: usize,
    cancellation: &Cancellation,
) {
    let maximum = CAPTURE.with(|slot| {
        slot.try_borrow().ok().and_then(|slot| {
            slot.as_ref()
                .filter(|state| state.result.is_none())
                .map(|state| state.maximum_bytes)
        })
    });
    let Some(maximum) = maximum else {
        return;
    };
    let result = census(
        target,
        incoming,
        observed_required_bytes,
        limit,
        maximum,
        cancellation,
    );
    CAPTURE.with(|slot| {
        if let Ok(mut slot) = slot.try_borrow_mut()
            && let Some(state) = slot.as_mut()
            && state.result.is_none()
        {
            state.result = Some(result);
        }
    });
}

fn fixture_documents() -> (
    rootlight_ir::NormalizedIrDocument,
    rootlight_ir::NormalizedIrDocument,
) {
    use rootlight_ids::{FactId, GenerationId, derive_repository};
    use rootlight_ir::{ExtensionCriticality, FactEvidence, NormalizedIrDocument};

    let repository = derive_repository(b"extension-census-contract").id();
    let generation = GenerationId::from_bytes([1; 20]);
    let extension = |namespace: &str, payload: &str| ExtensionEnvelope {
        id: FactId::from_bytes([2; 20]),
        repository,
        generation,
        namespace: namespace.to_owned(),
        version: "1.0".to_owned(),
        criticality: ExtensionCriticality::Noncritical,
        payload: payload.to_owned(),
        provenance: FactId::from_bytes([3; 20]),
        evidence: FactEvidence {
            source: None,
            derivation: Vec::new(),
        },
    };
    let mut target = NormalizedIrDocument::empty(repository, generation);
    target.extensions = vec![
        extension("test.zeta", "é"),
        extension(SYMBOL_IDENTITY_CLAIM_NAMESPACE, "abc"),
        extension(LEXICAL_EXTENSION_NAMESPACE, "retained private text"),
    ];
    let mut incoming = NormalizedIrDocument::empty(repository, generation);
    incoming.extensions = vec![
        extension(SYMBOL_IDENTITY_CLAIM_NAMESPACE, "déf"),
        extension("test.alpha", "xyz"),
        extension(LEXICAL_EXTENSION_NAMESPACE, "incoming private text"),
    ];
    (target, incoming)
}

fn quota_failure() -> FirstSliceError {
    let (target, mut incoming) = fixture_documents();
    let original_target = target.clone();
    let original_incoming = incoming.clone();
    let state = super::DocumentAppendState::from_document(&target).unwrap();
    let original_state = state.clone();
    let mut limits = rootlight_ir::IrLimits::default();
    limits.max_total_extension_bytes = 11;
    let result = super::plan_optional_extensions(
        &target,
        &mut incoming,
        &state,
        super::DiagnosticTruncationPlan {
            truncated: 0,
            dropped_target_index: None,
            coverage: super::CoverageAdjustmentPlan::default(),
        },
        0,
        &limits,
        &Cancellation::new(),
    );
    assert_eq!(target, original_target);
    assert_eq!(incoming, original_incoming);
    assert_eq!(state, original_state);
    result
        .err()
        .expect("synthetic aggregate exceeds the byte limit")
}

#[test]
fn failed_admission_census_conserves_required_utf8_bytes_and_original_error() {
    let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
    assert_eq!(
        quota_failure(),
        resource_limit(FirstSliceResource::ExtensionBytes, 12, 11)
    );
    let bytes = guard.finish().unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert!(!text.contains("retained private text") && !text.contains("incoming private text"));
    let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        report["schema"],
        "rootlight.failed-extension-admission-observation/2"
    );
    assert_eq!(report["original_error"]["observed"], 12);
    assert_eq!(report["original_error"]["limit"], 11);
    assert_eq!(report["required"]["target"]["inner_utf8_payload_bytes"], 5);
    assert_eq!(
        report["required"]["incoming"]["inner_utf8_payload_bytes"],
        7
    );
    assert_eq!(report["required"]["combined"]["envelope_count"], 4);
    assert_eq!(
        report["required"]["combined"]["inner_utf8_payload_bytes"],
        12
    );
    assert_eq!(
        report["lexical_optional"]["target"]["inner_utf8_payload_bytes"],
        21
    );
    assert_eq!(
        report["lexical_optional"]["incoming"]["inner_utf8_payload_bytes"],
        21
    );
    assert_eq!(report["lexical_optional"]["combined"]["envelope_count"], 2);
    assert_eq!(
        report["lexical_optional"]["combined"]["inner_utf8_payload_bytes"],
        42
    );
    assert_eq!(
        report["symbol_claim_envelopes"]["combined"]["envelope_count"],
        2
    );
    assert_eq!(
        report["symbol_claim_envelopes_are_not_distinct_symbols"],
        true
    );
    assert_eq!(report["whole_repository_observed"], false);
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 4);
    for row in rows {
        for field in ["envelope_count", "inner_utf8_payload_bytes"] {
            assert_eq!(
                row["usage"]["combined"][field].as_u64().unwrap(),
                row["usage"]["target"][field].as_u64().unwrap()
                    + row["usage"]["incoming"][field].as_u64().unwrap()
            );
        }
    }
    assert!(rows.windows(2).all(|pair| {
        (pair[0]["namespace"].as_str(), pair[0]["version"].as_str())
            < (pair[1]["namespace"].as_str(), pair[1]["version"].as_str())
    }));
    assert!(rows.iter().all(|row| row["version"] == "1.0"));
    assert_eq!(
        rows.iter()
            .filter(|row| row["retention"] == "lexical_optional")
            .count(),
        1
    );
    let required_bytes = rows
        .iter()
        .filter(|row| row["retention"] == "nonlexical_required")
        .map(|row| {
            row["usage"]["combined"]["inner_utf8_payload_bytes"]
                .as_u64()
                .unwrap()
        })
        .sum::<u64>();
    assert_eq!(required_bytes, 12);
}

#[test]
fn versioned_census_separates_rows_and_sums_every_carrier_version() {
    let (mut target, mut incoming) = fixture_documents();
    incoming.extensions[0].version = "1.2".to_owned();
    let mut older = target.extensions[1].clone();
    older.version = "1.1".to_owned();
    older.payload = "λ".to_owned();
    target.extensions.push(older);
    let mut shared = incoming.extensions[0].clone();
    shared.version = "1.0".to_owned();
    shared.payload = "q".to_owned();
    incoming.extensions.push(shared);
    let mut optional = incoming.extensions[2].clone();
    optional.version = "2".to_owned();
    optional.payload = "z".to_owned();
    incoming.extensions.push(optional);

    let bytes = census(
        &target.extensions,
        &incoming.extensions,
        15,
        14,
        MAX_OBSERVATION_BYTES,
        &Cancellation::new(),
    )
    .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(report["required"]["target"]["inner_utf8_payload_bytes"], 7);
    assert_eq!(
        report["required"]["incoming"]["inner_utf8_payload_bytes"],
        8
    );
    assert_eq!(
        report["required"]["combined"]["inner_utf8_payload_bytes"],
        15
    );
    assert_eq!(
        report["lexical_optional"]["combined"]["inner_utf8_payload_bytes"],
        43
    );
    let symbols = &report["symbol_claim_envelopes"];
    assert_eq!(symbols["target"]["envelope_count"], 2);
    assert_eq!(symbols["incoming"]["envelope_count"], 2);
    assert_eq!(symbols["combined"]["envelope_count"], 4);
    assert_eq!(symbols["target"]["inner_utf8_payload_bytes"], 5);
    assert_eq!(symbols["incoming"]["inner_utf8_payload_bytes"], 5);
    assert_eq!(symbols["combined"]["inner_utf8_payload_bytes"], 10);
    let rows = report["rows"].as_array().unwrap();
    assert_eq!(rows.len(), 7);
    let symbol_rows: Vec<_> = rows
        .iter()
        .filter(|row| row["namespace"] == SYMBOL_IDENTITY_CLAIM_NAMESPACE)
        .collect();
    assert_eq!(
        symbol_rows
            .iter()
            .map(|row| row["version"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["1.0", "1.1", "1.2"]
    );
    assert_eq!(
        symbol_rows[0]["usage"]["target"]["inner_utf8_payload_bytes"],
        3
    );
    assert_eq!(
        symbol_rows[0]["usage"]["incoming"]["inner_utf8_payload_bytes"],
        1
    );
    assert_eq!(
        symbol_rows[1]["usage"]["combined"]["inner_utf8_payload_bytes"],
        2
    );
    assert_eq!(
        symbol_rows[2]["usage"]["combined"]["inner_utf8_payload_bytes"],
        4
    );
    for field in ["envelope_count", "inner_utf8_payload_bytes"] {
        for partition in ["target", "incoming", "combined"] {
            let sum = symbol_rows
                .iter()
                .map(|row| row["usage"][partition][field].as_u64().unwrap())
                .sum::<u64>();
            assert_eq!(symbols[partition][field].as_u64().unwrap(), sum);
        }
    }
}

#[test]
fn versioned_census_keys_share_the_existing_utf8_byte_bound() {
    let (mut target, _) = fixture_documents();
    target.extensions.truncate(1);
    target.extensions[0].namespace = "n".to_owned();
    target.extensions[0].version = "v".repeat(MAX_NAMESPACE_BYTES - 1);
    let cancellation = Cancellation::new();
    assert!(
        census(
            &target.extensions,
            &[],
            2,
            1,
            MAX_OBSERVATION_BYTES,
            &cancellation
        )
        .is_ok()
    );
    target.extensions[0].version.push('v');
    assert_eq!(
        census(
            &target.extensions,
            &[],
            2,
            1,
            MAX_OBSERVATION_BYTES,
            &cancellation
        ),
        Err(FirstSliceError::Limits)
    );
    target.extensions[0].version = "λ".repeat(MAX_NAMESPACE_BYTES / 2);
    assert!(target.extensions[0].version.chars().count() < MAX_NAMESPACE_BYTES);
    assert_eq!(
        census(
            &target.extensions,
            &[],
            2,
            1,
            MAX_OBSERVATION_BYTES,
            &cancellation
        ),
        Err(FirstSliceError::Limits)
    );
}

#[test]
fn versioned_census_row_bound_counts_versions_but_not_repeated_keys() {
    let (target, _) = fixture_documents();
    let prototype = target.extensions[1].clone();
    let mut extensions: Vec<_> = (0..MAX_OBSERVATION_ROWS)
        .map(|index| {
            let mut extension = prototype.clone();
            extension.version = format!("1.{index}");
            extension.payload = "x".to_owned();
            extension
        })
        .collect();
    extensions.push(extensions[0].clone());
    let cancellation = Cancellation::new();
    let bytes = census(
        &extensions,
        &[],
        MAX_OBSERVATION_ROWS + 1,
        MAX_OBSERVATION_ROWS,
        MAX_OBSERVATION_BYTES,
        &cancellation,
    )
    .unwrap();
    let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        report["rows"].as_array().unwrap().len(),
        MAX_OBSERVATION_ROWS
    );
    assert_eq!(
        report["symbol_claim_envelopes"]["combined"]["envelope_count"],
        MAX_OBSERVATION_ROWS + 1
    );
    let mut overflow = prototype;
    overflow.version = "2.0".to_owned();
    overflow.payload = "x".to_owned();
    extensions.push(overflow);
    assert_eq!(
        census(
            &extensions,
            &[],
            MAX_OBSERVATION_ROWS + 2,
            MAX_OBSERVATION_ROWS,
            MAX_OBSERVATION_BYTES,
            &cancellation,
        ),
        Err(FirstSliceError::Limits)
    );
}

#[test]
fn versioned_census_cancellation_and_output_failure_keep_the_original_error() {
    let (target, mut incoming) = fixture_documents();
    incoming.extensions[0].version = "1.2".to_owned();
    for cancelled in [false, true] {
        let original = resource_limit(FirstSliceResource::ExtensionBytes, 12, 11);
        let guard = CaptureGuard::arm(8).unwrap();
        let cancellation = Cancellation::new();
        if cancelled {
            assert!(cancellation.cancel(super::CancellationReason::ClientRequest));
        }
        observe(
            &target.extensions,
            &incoming.extensions,
            12,
            11,
            &cancellation,
        );
        let captured = guard.finish();
        if cancelled {
            assert_eq!(
                captured,
                Err(FirstSliceError::Cancelled(
                    super::CancellationReason::ClientRequest
                ))
            );
        } else {
            assert!(matches!(
                captured,
                Err(FirstSliceError::ResourceLimit {
                    resource: FirstSliceResource::OutputBytes,
                    limit: 8,
                    ..
                })
            ));
        }
        assert_eq!(
            original,
            resource_limit(FirstSliceResource::ExtensionBytes, 12, 11)
        );
        assert!(CAPTURE.with(|slot| slot.borrow().is_none()));
    }
}

#[test]
fn versioned_census_usage_rejects_count_and_payload_overflow() {
    for (mut usage, increment) in [
        (
            Usage {
                envelope_count: usize::MAX,
                inner_utf8_payload_bytes: 0,
            },
            Usage {
                envelope_count: 1,
                inner_utf8_payload_bytes: 0,
            },
        ),
        (
            Usage {
                envelope_count: 0,
                inner_utf8_payload_bytes: usize::MAX,
            },
            Usage {
                envelope_count: 0,
                inner_utf8_payload_bytes: 1,
            },
        ),
    ] {
        assert_eq!(usage.add(increment), Err(FirstSliceError::Limits));
    }
}

#[test]
fn unarmed_failure_does_not_capture_or_change_the_quota_error() {
    assert!(CAPTURE.with(|slot| slot.borrow().is_none()));
    assert_eq!(
        quota_failure(),
        resource_limit(FirstSliceResource::ExtensionBytes, 12, 11)
    );
    assert!(CAPTURE.with(|slot| slot.borrow().is_none()));
}

#[test]
fn successful_admission_never_captures_even_when_armed() {
    for armed in [false, true] {
        let guard = armed.then(|| CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap());
        let (mut target, mut incoming) = fixture_documents();
        for document in [&mut target, &mut incoming] {
            document
                .extensions
                .retain(|extension| extension.namespace != LEXICAL_EXTENSION_NAMESPACE);
        }
        let original_target = target.clone();
        let original_incoming = incoming.clone();
        let state = super::DocumentAppendState::from_document(&target).unwrap();
        let mut limits = rootlight_ir::IrLimits::default();
        limits.max_total_extension_bytes = 12;
        let plan = super::plan_optional_extensions(
            &target,
            &mut incoming,
            &state,
            super::DiagnosticTruncationPlan {
                truncated: 0,
                dropped_target_index: None,
                coverage: super::CoverageAdjustmentPlan::default(),
            },
            0,
            &limits,
            &Cancellation::new(),
        )
        .unwrap();
        assert_eq!(plan.next_state.extension_payload_bytes, 12);
        assert_eq!(target, original_target);
        assert_eq!(incoming, original_incoming);
        if let Some(guard) = guard {
            assert!(!guard.reached());
            assert_eq!(guard.finish(), Err(FirstSliceError::Identity));
        }
        assert!(CAPTURE.with(|slot| slot.borrow().is_none()));
    }
}

#[test]
fn bounded_reporting_failure_does_not_replace_the_quota_error() {
    let guard = CaptureGuard::arm(8).unwrap();
    assert_eq!(
        quota_failure(),
        resource_limit(FirstSliceResource::ExtensionBytes, 12, 11)
    );
    assert!(matches!(
        guard.finish(),
        Err(FirstSliceError::ResourceLimit {
            resource: FirstSliceResource::OutputBytes,
            limit: 8,
            ..
        })
    ));
}

#[test]
fn diagnostic_cancellation_leaves_the_original_quota_error_unchanged() {
    let original = quota_failure();
    let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
    let (target, incoming) = fixture_documents();
    let cancellation = Cancellation::new();
    assert!(cancellation.cancel(super::CancellationReason::ClientRequest));
    observe(
        &target.extensions,
        &incoming.extensions,
        12,
        11,
        &cancellation,
    );
    assert!(matches!(
        guard.finish(),
        Err(FirstSliceError::Cancelled(
            super::CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(
        original,
        resource_limit(FirstSliceResource::ExtensionBytes, 12, 11)
    );
}

#[test]
fn capture_guard_is_thread_owned_and_census_rejects_unreconciled_bytes() {
    let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
    assert!(matches!(
        CaptureGuard::arm(MAX_OBSERVATION_BYTES),
        Err(FirstSliceError::Identity)
    ));
    assert!(
        std::thread::spawn(|| CAPTURE.with(|slot| slot.borrow().is_none()))
            .join()
            .unwrap()
    );
    let (target, incoming) = fixture_documents();
    observe(
        &target.extensions,
        &incoming.extensions,
        13,
        11,
        &Cancellation::new(),
    );
    assert_eq!(guard.finish(), Err(FirstSliceError::Identity));
    drop(CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap());
    assert!(CAPTURE.with(|slot| slot.borrow().is_none()));
}

fn safe_error(error: &FirstSliceError) -> serde_json::Value {
    let kind = match error {
        FirstSliceError::AggregateCapacity(_) => "aggregate_capacity",
        FirstSliceError::Configuration => "configuration",
        FirstSliceError::RandomUnavailable => "random_unavailable",
        FirstSliceError::DeadlineRequired => "deadline_required",
        FirstSliceError::Cancelled(_) => "cancelled",
        FirstSliceError::Repository => "repository",
        FirstSliceError::FixtureShape => "fixture_shape",
        FirstSliceError::Discovery => "discovery",
        FirstSliceError::DiscoveryIncomplete { .. } => "discovery_incomplete",
        FirstSliceError::Incremental => "incremental",
        FirstSliceError::DiscoveryDrift => "discovery_drift",
        FirstSliceError::Adapter => "adapter",
        FirstSliceError::IncompleteCoverage => "incomplete_coverage",
        FirstSliceError::ProjectCoverageIncomplete { .. } => "project_coverage_incomplete",
        FirstSliceError::AdapterWallTimeLimit => "adapter_wall_time_limit",
        FirstSliceError::AdapterInputLimit => "adapter_input_limit",
        FirstSliceError::AdapterOutputLimit => "adapter_output_limit",
        FirstSliceError::AdapterMemoryLimit => "adapter_memory_limit",
        FirstSliceError::AdapterProcessFailure => "adapter_process_failure",
        FirstSliceError::Resolution => "resolution",
        FirstSliceError::Identity => "identity",
        FirstSliceError::IdentityVerification(_) => "identity_verification",
        FirstSliceError::Catalog => "catalog",
        FirstSliceError::CatalogCorrupt => "catalog_corrupt",
        FirstSliceError::CatalogMigrationRequired => "catalog_migration_required",
        FirstSliceError::Search => "search",
        FirstSliceError::Source => "source",
        FirstSliceError::SourceEncodingUnsupported => "source_encoding_unsupported",
        FirstSliceError::Query => "query",
        FirstSliceError::Sharing => "sharing",
        FirstSliceError::RuntimeTrace(_) => "runtime_trace",
        FirstSliceError::SymbolNotFound => "symbol_not_found",
        FirstSliceError::BudgetExceeded => "budget_exceeded",
        FirstSliceError::QueryTermBudget { .. } => "query_term_budget",
        FirstSliceError::RepositoryNotFound => "repository_not_found",
        FirstSliceError::GenerationNotFound => "generation_not_found",
        FirstSliceError::GenerationMismatch => "generation_mismatch",
        FirstSliceError::Retention => "retention",
        FirstSliceError::RepositoryIo => "repository_io",
        FirstSliceError::ResourceUnavailable { .. } => "resource_unavailable",
        FirstSliceError::Limits => "limits",
        FirstSliceError::ResourceLimit { .. } => "resource_limit",
        FirstSliceError::RepositoryCapacityLimit { .. } => "repository_capacity_limit",
        FirstSliceError::EstimatedResourceLimit { .. } => "estimated_resource_limit",
        FirstSliceError::GenerationMemoryLimit { .. } => "generation_memory_limit",
        FirstSliceError::InsufficientDiskSpace { .. } => "insufficient_disk_space",
        FirstSliceError::StorageResourceExhausted { .. } => "storage_resource_exhausted",
    };
    match error {
        FirstSliceError::ResourceLimit {
            resource,
            observed,
            limit,
        } => serde_json::json!({
            "kind": kind, "resource": resource.as_str(), "observed": observed, "limit": limit,
        }),
        FirstSliceError::Cancelled(reason) => serde_json::json!({ "kind": kind, "reason": reason }),
        _ => serde_json::json!({ "kind": kind, "details_redacted": true }),
    }
}

#[test]
fn diagnostic_error_summary_retains_only_safe_quota_and_cancellation_fields() {
    assert_eq!(
        safe_error(&resource_limit(FirstSliceResource::ExtensionBytes, 12, 11)),
        serde_json::json!({ "kind": "resource_limit", "resource": "extension_bytes", "observed": 12, "limit": 11 })
    );
    assert_eq!(
        safe_error(&FirstSliceError::Cancelled(
            super::CancellationReason::ClientRequest
        )),
        serde_json::json!({ "kind": "cancelled", "reason": "client_request" })
    );
    assert_eq!(
        safe_error(&FirstSliceError::Configuration),
        serde_json::json!({ "kind": "configuration", "details_redacted": true })
    );
}

#[test]
#[ignore = "requires an explicit corpus, repository identity and fresh external diagnostic output"]
fn observe_repository_failed_extension_admission() {
    use super::{FirstSliceService, canonical_repository_root, repository_path_hash};
    use rootlight_ids::RepositoryId;

    let root = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_EXTENSION_ADMISSION_ROOT")
            .expect("explicit corpus root is required"),
    );
    let output = std::path::PathBuf::from(
        std::env::var_os("ROOTLIGHT_EXTENSION_ADMISSION_OUTPUT")
            .expect("explicit diagnostic output is required"),
    );
    let repository: RepositoryId = std::env::var("ROOTLIGHT_EXTENSION_ADMISSION_REPOSITORY")
        .expect("explicit repository identity is required")
        .parse()
        .expect("repository identity is valid");
    let cancellation = Cancellation::new();
    let canonical = canonical_repository_root(&root, &cancellation).unwrap();
    let output = std::path::absolute(output).unwrap();
    let parent = std::fs::canonicalize(output.parent().expect("output has a parent")).unwrap();
    assert!(
        !parent.starts_with(&canonical),
        "diagnostic output must be outside the corpus"
    );
    let output = parent.join(output.file_name().expect("output has a file name"));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(output)
        .unwrap();
    let service = FirstSliceService::new(2).unwrap();
    service
        .restore_repository_registration(repository_path_hash(&canonical).unwrap(), repository)
        .unwrap();
    let guard = CaptureGuard::arm(MAX_OBSERVATION_BYTES).unwrap();
    let result = service.prepare_repository_with_mode(
        &canonical,
        super::FirstSliceIndexMode::Structural,
        &cancellation,
    );
    let boundary_reached = guard.reached();
    let capture = guard.finish();
    let observation = capture
        .as_ref()
        .ok()
        .map(|bytes| serde_json::from_slice::<serde_json::Value>(bytes).unwrap());
    let report = serde_json::json!({
        "schema": "rootlight.failed-extension-admission-run/2",
        "actual_result": if result.is_err() { "failed" } else { "prepared" },
        "actual_error": result.as_ref().err().map(safe_error),
        "boundary_reached": boundary_reached,
        "observation": observation,
        "observation_error": capture.as_ref().err().map(safe_error),
        "product_success_qualified": false,
        "benchmark_qualified": false,
        "whole_repository_observed": false,
        "source_text_included": false,
        "payload_contents_included": false,
    });
    // A cancelled census must not prevent persisting the already-returned service error.
    let report = encode_json(&report, 2 * MAX_OBSERVATION_BYTES, &Cancellation::new()).unwrap();
    file.write_all(&report).unwrap();
    file.sync_all().unwrap();
    assert!(service.active_generation().is_none());
    assert!(service.active_generation_for(repository).is_none());
    assert!(
        matches!(
            result,
            Err(FirstSliceError::ResourceLimit {
                resource: FirstSliceResource::ExtensionBytes,
                ..
            })
        ),
        "the service failed at a different boundary; inspect actual_error and observation_error"
    );
    assert!(
        capture.is_ok(),
        "no complete namespace census was captured; inspect diagnostic output"
    );
}
