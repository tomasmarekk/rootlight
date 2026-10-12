//! Early serialized-document staging admission for one generation preparation.
//!
//! Charges are conservative, monotonic allowances added to the source preflight
//! floor. They are not measurements of owned heap capacity or process RSS.

use std::io::{self, Write};

use rootlight_cancel::Cancellation;
use rootlight_ir::NormalizedIrDocument;
use serde::Serialize;

use crate::{
    FirstSliceError, FirstSliceResource, GenerationMemoryReservation,
    MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES, PendingGenerationMemory, check_cancellation,
    generation_memory_limit,
};

#[cfg(test)]
use crate::generation_admission_observation as observation;

const SERIALIZATION_CHECKPOINT_BYTES: usize = 64 * 1024;

/// A preparation-local ledger of accepted serialized staging allowances.
///
/// The caller retains the reservation guard and admits each newly retained
/// document or chunk once. Already materialized analyzer output and in-flight
/// canonicalization are outside this boundary; the ledger is not heap accounting.
#[derive(Debug)]
pub(super) struct GenerationAdmission {
    source_preflight_bytes: u64,
    charged_bytes: u64,
    reservation_bytes: u64,
}

impl GenerationAdmission {
    /// Preserves the source preflight allowance as an additive reservation floor.
    ///
    /// # Errors
    /// Returns a typed memory limit if the floor already exceeds the ceiling.
    pub(super) fn new(source_preflight_bytes: u64) -> Result<Self, FirstSliceError> {
        #[cfg(test)]
        observation::begin_admission(source_preflight_bytes);
        if source_preflight_bytes > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES {
            return Err(generation_memory_limit(
                0,
                source_preflight_bytes,
                PendingGenerationMemory::Reserved,
            ));
        }
        Ok(Self {
            source_preflight_bytes,
            charged_bytes: 0,
            reservation_bytes: source_preflight_bytes,
        })
    }

    /// Returns the cumulative accepted serialized-document and chunk allowances.
    #[cfg(test)]
    pub(super) fn charged_bytes(&self) -> u64 {
        self.charged_bytes
    }

    /// Returns this ledger's additive floor plus accepted staging allowances.
    ///
    /// Later finalization may grow the underlying reservation independently.
    pub(super) fn reservation_bytes(&self) -> u64 {
        self.reservation_bytes
    }

    /// Counts a document once without buffering JSON and reserves its exact size.
    ///
    /// # Errors
    /// Returns cancellation, typed size or memory failures, or a backing
    /// reservation failure. No partial count is committed on failure.
    pub(super) fn admit_document(
        &mut self,
        document: &NormalizedIrDocument,
        reservation: &mut GenerationMemoryReservation,
        durable_cache: bool,
        cancellation: &Cancellation,
    ) -> Result<u64, FirstSliceError> {
        self.admit_serialized(document, reservation, durable_cache, cancellation)
    }

    /// Reserves an existing exact encoded-byte count without serializing again.
    ///
    /// This remains charged if a cache later evicts a chunk: it is a monotonic
    /// preparation allowance, not a claim of unique simultaneous ownership.
    ///
    /// # Errors
    /// Returns cancellation, arithmetic overflow, a typed memory limit, or a
    /// backing reservation failure. Failed growth preserves the previous ledger.
    pub(super) fn admit_known_bytes(
        &mut self,
        bytes: u64,
        reservation: &mut GenerationMemoryReservation,
        durable_cache: bool,
        cancellation: &Cancellation,
    ) -> Result<(), FirstSliceError> {
        #[cfg(test)]
        observation::observe_known_attempt(bytes);
        check_cancellation(cancellation)?;
        let charged_bytes = self
            .charged_bytes
            .checked_add(bytes)
            .ok_or(FirstSliceError::Limits)?;
        let reservation_bytes = self
            .source_preflight_bytes
            .checked_add(charged_bytes)
            .ok_or(FirstSliceError::Limits)?;
        if reservation_bytes > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES {
            return Err(generation_memory_limit(
                0,
                reservation_bytes,
                PendingGenerationMemory::Staged,
            ));
        }
        check_cancellation(cancellation)?;
        reservation.resize(
            reservation_bytes,
            PendingGenerationMemory::Staged,
            durable_cache,
        )?;
        self.charged_bytes = charged_bytes;
        self.reservation_bytes = reservation_bytes;
        #[cfg(test)]
        observation::observe_admitted(bytes, reservation_bytes);
        Ok(())
    }

    fn admit_serialized<T: Serialize>(
        &mut self,
        value: &T,
        reservation: &mut GenerationMemoryReservation,
        durable_cache: bool,
        cancellation: &Cancellation,
    ) -> Result<u64, FirstSliceError> {
        #[cfg(test)]
        let _document_charge = observation::enter_document_charge();
        let bytes = serialized_bytes(value, self.reservation_bytes, cancellation)?;
        self.admit_known_bytes(bytes, reservation, durable_cache, cancellation)?;
        Ok(bytes)
    }
}

struct JsonByteCounter<'a> {
    bytes: u64,
    admitted_bytes: u64,
    cancellation: &'a Cancellation,
    failure: Option<FirstSliceError>,
}

impl<'a> JsonByteCounter<'a> {
    fn new(admitted_bytes: u64, cancellation: &'a Cancellation) -> Self {
        #[cfg(test)]
        observation::serialization_start(admitted_bytes);
        Self {
            bytes: 0,
            admitted_bytes,
            cancellation,
            failure: None,
        }
    }

    fn fail<T>(&mut self, failure: FirstSliceError) -> io::Result<T> {
        #[cfg(test)]
        observation::serialization_failed(&failure);
        self.failure = Some(failure);
        Err(io::Error::other("generation staging count stopped"))
    }

    fn checkpoint(&mut self) -> io::Result<()> {
        if let Some(failure) = self.failure {
            return self.fail(failure);
        }
        if let Err(failure) = check_cancellation(self.cancellation) {
            return self.fail(failure);
        }
        Ok(())
    }
}

impl Write for JsonByteCounter<'_> {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.checkpoint()?;
        let length = buffer.len().min(SERIALIZATION_CHECKPOINT_BYTES);
        let Ok(length_bytes) = u64::try_from(length) else {
            #[cfg(test)]
            observation::serialization_rejected(None, None);
            return self.fail(FirstSliceError::Limits);
        };
        let Some(bytes) = self.bytes.checked_add(length_bytes) else {
            #[cfg(test)]
            observation::serialization_rejected(None, Some(length_bytes));
            return self.fail(FirstSliceError::Limits);
        };
        let Some(observed) = self.admitted_bytes.checked_add(bytes) else {
            #[cfg(test)]
            observation::serialization_rejected(Some(bytes), Some(length_bytes));
            return self.fail(FirstSliceError::Limits);
        };
        if observed > MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES {
            #[cfg(test)]
            observation::serialization_rejected(Some(bytes), Some(length_bytes));
            return self.fail(generation_memory_limit(
                0,
                observed,
                PendingGenerationMemory::Staged,
            ));
        }
        self.bytes = bytes;
        #[cfg(test)]
        observation::serialization_prefix(bytes);
        Ok(length)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.checkpoint()
    }
}

fn serialized_bytes<T: Serialize>(
    value: &T,
    admitted_bytes: u64,
    cancellation: &Cancellation,
) -> Result<u64, FirstSliceError> {
    check_cancellation(cancellation)?;
    let mut counter = JsonByteCounter::new(admitted_bytes, cancellation);
    if let Err(error) = serde_json::to_writer(&mut counter, value) {
        if let Some(failure) = counter.failure {
            return Err(failure);
        }
        check_cancellation(cancellation)?;
        return Err(
            if error.io_error_kind() == Some(io::ErrorKind::OutOfMemory) {
                FirstSliceError::ResourceUnavailable {
                    resource: FirstSliceResource::MemoryBytes,
                }
            } else {
                FirstSliceError::Limits
            },
        );
    }
    if let Some(failure) = counter.failure {
        return Err(failure);
    }
    check_cancellation(cancellation)?;
    #[cfg(test)]
    observation::serialization_complete(counter.bytes);
    Ok(counter.bytes)
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use rootlight_cancel::CancellationReason;
    use rootlight_ids::{GenerationId, RepositoryId};
    use serde::ser::SerializeSeq;

    use super::*;
    use crate::FirstSliceService;

    struct CountedValue<'a> {
        calls: &'a Cell<usize>,
        value: &'a serde_json::Value,
    }

    impl Serialize for CountedValue<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.calls.set(self.calls.get() + 1);
            self.value.serialize(serializer)
        }
    }

    struct RejectedValue;

    impl Serialize for RejectedValue {
        fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(serde::ser::Error::custom("synthetic serialization failure"))
        }
    }

    struct CancellingValue<'a> {
        cancellation: &'a Cancellation,
        after_serialization: bool,
    }

    impl Serialize for CancellingValue<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            let mut sequence = serializer.serialize_seq(Some(2))?;
            sequence.serialize_element(&1_u8)?;
            if !self.after_serialization {
                self.cancellation.cancel(CancellationReason::ClientRequest);
            }
            sequence.serialize_element(&2_u8)?;
            let result = sequence.end();
            if self.after_serialization {
                self.cancellation.cancel(CancellationReason::ClientRequest);
            }
            result
        }
    }

    #[test]
    fn serialized_staging_count_is_exact_single_pass_and_additive() {
        let service = FirstSliceService::new(2).expect("service initializes");
        let generation = GenerationId::from_bytes([1; 20]);
        let floor = 100_u64;
        let mut reservation = service
            .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
            .expect("source preflight reserves");
        let mut admission = GenerationAdmission::new(floor).expect("floor is bounded");
        let cancellation = Cancellation::new();
        let value = serde_json::json!({"name": "\u{03b4}\"\\\n", "rows": [1, 2, 3]});
        let expected = u64::try_from(
            serde_json::to_vec(&value)
                .expect("fixture serializes")
                .len(),
        )
        .expect("fixture size fits");
        let calls = Cell::new(0);
        let counted = CountedValue {
            calls: &calls,
            value: &value,
        };
        assert_eq!(
            admission.admit_serialized(&counted, &mut reservation, false, &cancellation),
            Ok(expected)
        );
        assert_eq!(calls.get(), 1);

        let document = NormalizedIrDocument::empty(RepositoryId::from_bytes([2; 16]), generation);
        let document_bytes = u64::try_from(
            serde_json::to_vec(&document)
                .expect("document serializes")
                .len(),
        )
        .expect("document size fits");
        assert_eq!(
            admission.admit_document(&document, &mut reservation, false, &cancellation),
            Ok(document_bytes)
        );
        admission
            .admit_known_bytes(29, &mut reservation, false, &cancellation)
            .expect("known chunk bytes reserve without serialization");
        assert_eq!(admission.charged_bytes(), expected + document_bytes + 29);
        assert_eq!(
            admission.reservation_bytes(),
            floor + expected + document_bytes + 29
        );
        assert_eq!(
            service
                .generation_cache
                .lock()
                .expect("cache remains available")
                .reservations
                .get(&generation),
            Some(&admission.reservation_bytes())
        );
    }

    #[test]
    fn failed_serialization_cancellation_and_overflow_do_not_commit() {
        let service = FirstSliceService::new(2).expect("service initializes");
        let generation = GenerationId::from_bytes([3; 20]);
        let floor = 100_u64;
        let mut reservation = service
            .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
            .expect("source preflight reserves");
        let mut admission = GenerationAdmission::new(floor).expect("floor is bounded");
        let cancellation = Cancellation::new();
        assert_eq!(
            admission.admit_serialized(&RejectedValue, &mut reservation, false, &cancellation),
            Err(FirstSliceError::Limits)
        );
        for after_serialization in [false, true] {
            let cancelled = Cancellation::new();
            assert_eq!(
                admission.admit_serialized(
                    &CancellingValue {
                        cancellation: &cancelled,
                        after_serialization,
                    },
                    &mut reservation,
                    false,
                    &cancelled,
                ),
                Err(FirstSliceError::Cancelled(
                    CancellationReason::ClientRequest
                ))
            );
        }
        let cancelled = Cancellation::new();
        cancelled.cancel(CancellationReason::ClientRequest);
        let calls = Cell::new(0);
        let value = serde_json::json!("unvisited");
        assert_eq!(
            admission.admit_serialized(
                &CountedValue {
                    calls: &calls,
                    value: &value,
                },
                &mut reservation,
                false,
                &cancelled,
            ),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        );
        assert_eq!(calls.get(), 0);

        let mut counter = JsonByteCounter::new(0, &cancellation);
        let buffer = vec![0; SERIALIZATION_CHECKPOINT_BYTES * 2];
        assert_eq!(
            counter.write(&buffer).expect("first chunk counts"),
            SERIALIZATION_CHECKPOINT_BYTES
        );
        assert_eq!(
            counter.bytes,
            u64::try_from(SERIALIZATION_CHECKPOINT_BYTES).unwrap()
        );
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(counter.write(&buffer).is_err());
        assert_eq!(
            counter.failure,
            Some(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        );
        assert_eq!(
            counter.bytes,
            u64::try_from(SERIALIZATION_CHECKPOINT_BYTES).unwrap()
        );

        let cancellation = Cancellation::new();
        let mut overflow = JsonByteCounter::new(u64::MAX, &cancellation);
        assert!(overflow.write(&[1]).is_err());
        assert_eq!(overflow.failure, Some(FirstSliceError::Limits));
        assert_eq!(overflow.bytes, 0);
        assert_eq!(
            admission.admit_known_bytes(u64::MAX, &mut reservation, false, &cancellation),
            Err(FirstSliceError::Limits)
        );
        assert!(matches!(
            admission.admit_known_bytes(
                MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
                &mut reservation,
                false,
                &cancellation,
            ),
            Err(FirstSliceError::GenerationMemoryLimit { observed, .. })
                if observed == MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES + floor
        ));
        assert_eq!(admission.charged_bytes(), 0);
        assert_eq!(admission.reservation_bytes(), floor);
        assert_eq!(
            service
                .generation_cache
                .lock()
                .expect("cache remains available")
                .reservations
                .get(&generation),
            Some(&floor)
        );
    }

    #[test]
    fn rejected_staging_growth_preserves_reservation_owners() {
        let service = FirstSliceService::new(2).expect("service initializes");
        let floor = MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES / 2;
        let first_id = GenerationId::from_bytes([4; 20]);
        let second_id = GenerationId::from_bytes([5; 20]);
        let mut first = service
            .reserve_generation_memory(first_id, floor, PendingGenerationMemory::Reserved)
            .expect("first source preflight reserves");
        let mut second = service
            .reserve_generation_memory(second_id, floor, PendingGenerationMemory::Reserved)
            .expect("second source preflight reserves");
        let mut admission = GenerationAdmission::new(floor).expect("floor is bounded");
        assert!(matches!(
            admission.admit_known_bytes(1, &mut first, false, &Cancellation::new()),
            Err(FirstSliceError::GenerationMemoryLimit { observed, .. })
                if observed == MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES + 1
        ));
        assert_eq!(admission.charged_bytes(), 0);
        assert_eq!(admission.reservation_bytes(), floor);
        {
            let cache = service
                .generation_cache
                .lock()
                .expect("cache remains available");
            assert_eq!(cache.reservations.get(&first_id), Some(&floor));
            assert_eq!(cache.reservations.get(&second_id), Some(&floor));
        }
        drop(first);
        {
            let cache = service
                .generation_cache
                .lock()
                .expect("cache remains available");
            assert!(!cache.reservations.contains_key(&first_id));
            assert_eq!(cache.reservations.get(&second_id), Some(&floor));
        }
        second
            .resize(
                MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
                PendingGenerationMemory::Staged,
                false,
            )
            .expect("only the released owner becomes available");
        drop(second);
        assert!(
            service
                .generation_cache
                .lock()
                .expect("cache remains available")
                .reservations
                .is_empty()
        );
    }

    #[test]
    fn failed_prefix_observation_preserves_original_error_and_uncommitted_bytes() {
        let guard = observation::CaptureGuard::arm(observation::MAX_OBSERVATION_BYTES).unwrap();
        let _phase = observation::enter_phase(observation::Phase::FileDocument);
        let floor = MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES - 3;
        let admission = GenerationAdmission::new(floor).unwrap();
        let cancellation = Cancellation::new();
        let mut counter = JsonByteCounter::new(admission.reservation_bytes(), &cancellation);
        assert_eq!(counter.write(&[1, 2]).unwrap(), 2);
        assert!(counter.write(&[3, 4]).is_err());
        let original = generation_memory_limit(
            0,
            MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES + 1,
            PendingGenerationMemory::Staged,
        );
        assert_eq!(counter.failure, Some(original));
        assert_eq!(counter.bytes, 2);
        assert_eq!(admission.charged_bytes(), 0);
        assert!(!guard.reached());
        observation::observe_failure(&original);
        let bytes = guard.finish(&cancellation).unwrap();
        let report: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(report["failure_phase"], "file_document");
        assert_eq!(
            report["costs"]["last_serialization"]["accepted_prefix_bytes"],
            2
        );
        assert_eq!(
            report["costs"]["last_serialization"]["attempted_prefix_bytes"],
            4
        );
        assert_eq!(
            report["costs"]["last_serialization"]["rejected_write_bytes"],
            2
        );
        assert_eq!(report["costs"]["admitted_documents"]["count"], 0);
        assert_eq!(
            report["original_error"]["memory_limit"]["observed"],
            MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES + 1
        );
        assert_eq!(counter.failure, Some(original));
    }

    #[test]
    fn successful_census_and_failed_growth_match_the_transactional_ledger() {
        let service = FirstSliceService::new(2).unwrap();
        let generation = GenerationId::from_bytes([6; 20]);
        let floor = 100;
        let mut reservation = service
            .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
            .unwrap();
        let guard = observation::CaptureGuard::arm(observation::MAX_OBSERVATION_BYTES).unwrap();
        let mut admission = GenerationAdmission::new(floor).unwrap();
        let document = NormalizedIrDocument::empty(RepositoryId::from_bytes([7; 16]), generation);
        let document_bytes = admission
            .admit_document(&document, &mut reservation, false, &Cancellation::new())
            .unwrap();
        {
            let _phase = observation::enter_phase(observation::Phase::OptionalChunk);
            admission
                .admit_known_bytes(7, &mut reservation, false, &Cancellation::new())
                .unwrap();
        }
        let before = admission.reservation_bytes();
        let original = admission
            .admit_known_bytes(u64::MAX, &mut reservation, false, &Cancellation::new())
            .unwrap_err();
        assert_eq!(original, FirstSliceError::Limits);
        assert_eq!(admission.reservation_bytes(), before);
        observation::observe_failure(&original);
        let report: serde_json::Value =
            serde_json::from_slice(&guard.finish(&Cancellation::new()).unwrap()).unwrap();
        assert_eq!(
            report["costs"]["admitted_documents"]["bytes"],
            document_bytes
        );
        assert_eq!(report["costs"]["admitted_documents"]["count"], 1);
        assert_eq!(
            report["costs"]["admitted_known_chunks"],
            serde_json::json!({"count":1,"bytes":7})
        );
        assert_eq!(report["costs"]["admitted_total_bytes"], before);
        assert_eq!(report["costs"]["attempted_known_bytes"], u64::MAX);
    }

    #[test]
    fn cancelled_serialization_observation_preserves_reason_and_reservation() {
        for after_serialization in [false, true] {
            let service = FirstSliceService::new(2).unwrap();
            let generation = GenerationId::from_bytes([8; 20]);
            let floor = 100;
            let mut reservation = service
                .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
                .unwrap();
            let guard = observation::CaptureGuard::arm(observation::MAX_OBSERVATION_BYTES).unwrap();
            let _phase = observation::enter_phase(observation::Phase::FileDocument);
            let mut admission = GenerationAdmission::new(floor).unwrap();
            let cancellation = Cancellation::new();
            let original = admission
                .admit_serialized(
                    &CancellingValue {
                        cancellation: &cancellation,
                        after_serialization,
                    },
                    &mut reservation,
                    false,
                    &cancellation,
                )
                .unwrap_err();
            assert_eq!(
                original,
                FirstSliceError::Cancelled(CancellationReason::ClientRequest)
            );
            assert_eq!(admission.charged_bytes(), 0);
            assert_eq!(admission.reservation_bytes(), floor);
            assert_eq!(
                service
                    .generation_cache
                    .lock()
                    .unwrap()
                    .reservations
                    .get(&generation),
                Some(&floor)
            );
            observation::observe_failure(&original);
            let report: serde_json::Value =
                serde_json::from_slice(&guard.finish(&Cancellation::new()).unwrap()).unwrap();
            assert_eq!(report["costs"]["admitted_documents"]["count"], 0);
            assert_eq!(report["costs"]["admitted_known_bytes"]["count"], 0);
            assert_eq!(report["costs"]["admitted_total_bytes"], floor);
            assert!(report["costs"]["last_serialization"]["complete_bytes"].is_null());
            assert_eq!(report["original_error"]["code"], "cancelled");
            assert_eq!(
                original,
                FirstSliceError::Cancelled(CancellationReason::ClientRequest)
            );
        }
    }
}
