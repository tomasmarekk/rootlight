//! Optional process-local normalized reuse with compressed-only retained ownership.
//! Payload authentication and strict decoding precede canonical identity checks.

use std::io::{self, Write};

use rootlight_cancel::Cancellation;
use rootlight_ids::{ContentHash, FileId, GenerationId};
use rootlight_ir::{
    CanonicalNormalizedFileChunk, ExtensionSupport, IrLimits, NormalizedIrDocument,
    decode_normalized_ir_document_messagepack_reader_with_checkpoint,
    generation_neutral_workspace_bytes,
};

use crate::generation_admission::GenerationAdmission;
use crate::{
    FirstSliceError, GenerationMemoryReservation, MAX_RETAINED_STRUCTURAL_ARTIFACT_BYTES,
    PendingGenerationMemory, check_cancellation, compression::gzip,
    logical_projection_staged_memory_bytes,
};

const CHECKPOINT_BYTES: usize = 64 * 1024;

/// This descriptor never retains a decoded document or a spare-capacity buffer.
#[derive(Debug)]
pub(super) struct CompressedNormalizedFileChunk {
    file: FileId,
    digest: ContentHash,
    canonical_json_bytes: usize,
    decoded_messagepack_bytes: usize,
    workspace_bytes: u64,
    encoded_digest: ContentHash,
    encoded: Box<[u8]>,
}

impl CompressedNormalizedFileChunk {
    /// The caller must admit `staged_memory_bytes` before entering this phase.
    pub(super) fn new(
        document: &NormalizedIrDocument,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        workspace_bytes: u64,
        cancellation: &Cancellation,
    ) -> Result<Self, FirstSliceError> {
        check_cancellation(cancellation)?;
        let chunk =
            CanonicalNormalizedFileChunk::new_with_checkpoint(document, limits, extensions, || {
                cancellation.check().is_ok()
            })
            .map_err(|_| rejected(cancellation))?;
        let file = chunk.file();
        let digest = chunk.digest();
        let canonical_json_bytes = chunk.encoded_bytes();
        if canonical_json_bytes > limits.max_document_bytes {
            return Err(FirstSliceError::Limits);
        }
        let document = chunk.into_document();
        let destination = BoundedBytes {
            bytes: Vec::new(),
            maximum_bytes: canonical_json_bytes.min(MAX_RETAINED_STRUCTURAL_ARTIFACT_BYTES),
        };
        let gzip =
            gzip::Writer::new(destination, cancellation).map_err(|_| rejected(cancellation))?;
        let mut writer = DecodedByteCounter {
            inner: gzip,
            bytes: 0,
            maximum_bytes: limits.max_document_bytes,
            cancellation,
        };
        rmp_serde::encode::write_named(&mut writer, &document)
            .map_err(|_| rejected(cancellation))?;
        let decoded_messagepack_bytes = writer.bytes;
        let encoded = writer
            .inner
            .finish()
            .map_err(|_| rejected(cancellation))?
            .bytes
            .into_boxed_slice();
        check_cancellation(cancellation)?;
        let mut compressed = Self {
            file,
            digest,
            canonical_json_bytes,
            decoded_messagepack_bytes,
            workspace_bytes,
            encoded_digest: ContentHash::from_bytes([0; 32]),
            encoded,
        };
        compressed.encoded_digest = compressed.payload_digest(cancellation)?;
        Ok(compressed)
    }

    /// Charges every retained payload byte, the descriptor and the Arc header.
    /// Container bookkeeping and parser accounting retain their existing policy.
    pub(super) fn accounted_bytes(&self) -> usize {
        self.encoded.len() + std::mem::size_of::<Self>() + 2 * std::mem::size_of::<usize>()
    }

    pub(super) fn reconstruction_memory_bytes(&self) -> Result<u64, FirstSliceError> {
        staged_memory_bytes(
            u64::try_from(self.canonical_json_bytes).map_err(|_| FirstSliceError::Limits)?,
            self.workspace_bytes,
        )
    }

    #[cfg(test)]
    pub(super) fn canonical_json_bytes(&self) -> usize {
        self.canonical_json_bytes
    }

    fn payload_digest(&self, cancellation: &Cancellation) -> Result<ContentHash, FirstSliceError> {
        let mut hasher =
            blake3::Hasher::new_derive_key("rootlight.normalized-file-cache/gzip-messagepack/v1");
        hasher.update(self.file.as_bytes());
        hasher.update(self.digest.as_bytes());
        for length in [self.canonical_json_bytes, self.decoded_messagepack_bytes] {
            let length = u64::try_from(length).map_err(|_| FirstSliceError::Limits)?;
            hasher.update(&length.to_be_bytes());
        }
        hasher.update(&self.workspace_bytes.to_be_bytes());
        for bytes in self.encoded.chunks(CHECKPOINT_BYTES) {
            check_cancellation(cancellation)?;
            hasher.update(bytes);
        }
        check_cancellation(cancellation)?;
        Ok(ContentHash::from_bytes(*hasher.finalize().as_bytes()))
    }

    /// The caller must reserve reconstruction work before allocating decoder output.
    pub(super) fn rebind(
        &self,
        generation: GenerationId,
        limits: &IrLimits,
        extensions: &ExtensionSupport,
        cancellation: &Cancellation,
    ) -> Result<NormalizedIrDocument, FirstSliceError> {
        check_cancellation(cancellation)?;
        if self.encoded.len() > MAX_RETAINED_STRUCTURAL_ARTIFACT_BYTES
            || self.canonical_json_bytes > limits.max_document_bytes
            || self.decoded_messagepack_bytes > limits.max_document_bytes
            || self.payload_digest(cancellation)? != self.encoded_digest
        {
            return Err(FirstSliceError::Identity);
        }
        let reader = gzip::Reader::new(&self.encoded, self.decoded_messagepack_bytes, cancellation)
            .map_err(|_| rejected(cancellation))?;
        let document = decode_normalized_ir_document_messagepack_reader_with_checkpoint(
            reader,
            self.decoded_messagepack_bytes,
            limits,
            extensions,
            || cancellation.check().is_ok(),
        )
        .map_err(|_| rejected(cancellation))?;
        let workspace_bytes =
            generation_neutral_workspace_bytes(&document, || cancellation.check().is_ok())
                .map_err(|_| rejected(cancellation))?;
        if workspace_bytes != self.workspace_bytes {
            return Err(FirstSliceError::Identity);
        }
        let chunk = CanonicalNormalizedFileChunk::new_with_checkpoint(
            &document,
            limits,
            extensions,
            || cancellation.check().is_ok(),
        )
        .map_err(|_| rejected(cancellation))?;
        drop(document);
        if chunk.file() != self.file
            || chunk.digest() != self.digest
            || chunk.encoded_bytes() != self.canonical_json_bytes
        {
            return Err(FirstSliceError::Identity);
        }
        chunk
            .rebind_with_checkpoint(generation, limits, extensions, || {
                cancellation.check().is_ok()
            })
            .map_err(|_| rejected(cancellation))
    }
}

/// Reuse the existing three-document graph allowance and add one bounded buffer.
/// This is a conservative staging reservation, not a claim about process RSS.
pub(super) fn staged_memory_bytes(
    serialized_document_bytes: u64,
    workspace_bytes: u64,
) -> Result<u64, FirstSliceError> {
    let descriptor_bytes = descriptor_bytes()?;
    logical_projection_staged_memory_bytes(serialized_document_bytes, workspace_bytes)?
        .checked_add(serialized_document_bytes)
        .and_then(|bytes| bytes.checked_add(descriptor_bytes))
        .ok_or(FirstSliceError::Limits)
}

fn descriptor_bytes() -> Result<u64, FirstSliceError> {
    u64::try_from(
        std::mem::size_of::<CompressedNormalizedFileChunk>() + 2 * std::mem::size_of::<usize>(),
    )
    .map_err(|_| FirstSliceError::Limits)
}

/// Temporarily enlarges the current generation reservation without charging work
/// for every file cumulatively. Only a successfully retained payload is admitted
/// to the monotonic ledger. Optional failures preserve mandatory index admission.
/// The returned peak is nonzero only when the enlargement actually succeeded.
pub(super) fn with_optional_staging<T>(
    admission: &mut GenerationAdmission,
    reservation: &mut GenerationMemoryReservation,
    work_bytes: Result<u64, FirstSliceError>,
    durable_cache: bool,
    cancellation: &Cancellation,
    work: impl FnOnce() -> Result<T, FirstSliceError>,
    retained_bytes: impl FnOnce(&T) -> Result<u64, FirstSliceError>,
) -> Result<(Option<T>, u64), FirstSliceError> {
    #[cfg(test)]
    let _phase = crate::generation_admission_observation::enter_phase(
        crate::generation_admission_observation::Phase::OptionalChunk,
    );
    check_cancellation(cancellation)?;
    let base = admission.reservation_bytes();
    let peak = work_bytes.and_then(|bytes| base.checked_add(bytes).ok_or(FirstSliceError::Limits));
    let peak = match peak {
        Ok(peak) => peak,
        Err(error) => return optional_failure(error).map(|()| (None, 0)),
    };
    if let Err(error) = reservation.resize(peak, PendingGenerationMemory::Staged, durable_cache) {
        #[cfg(test)]
        crate::generation_admission_observation::observe_temporary_reservation(base, peak, false);
        check_cancellation(cancellation)?;
        return optional_failure(error).map(|()| (None, 0));
    }
    #[cfg(test)]
    crate::generation_admission_observation::observe_temporary_reservation(base, peak, true);
    let value = work();
    let value = match value {
        Ok(value) => value,
        Err(error) => {
            reservation.release_temporary(base)?;
            check_cancellation(cancellation)?;
            return optional_failure(error).map(|()| (None, peak));
        }
    };
    if let Err(error) = check_cancellation(cancellation) {
        drop(value);
        reservation.release_temporary(base)?;
        return Err(error);
    }
    let retained = retained_bytes(&value).and_then(|bytes| {
        admission.admit_known_bytes(bytes, reservation, durable_cache, cancellation)
    });
    if let Err(error) = retained {
        drop(value);
        reservation.release_temporary(base)?;
        check_cancellation(cancellation)?;
        return optional_failure(error).map(|()| (None, peak));
    }
    reservation.release_temporary(admission.reservation_bytes())?;
    check_cancellation(cancellation)?;
    Ok((Some(value), peak))
}

fn optional_failure(error: FirstSliceError) -> Result<(), FirstSliceError> {
    match error {
        FirstSliceError::Identity
        | FirstSliceError::Limits
        | FirstSliceError::GenerationMemoryLimit { .. } => Ok(()),
        error => Err(error),
    }
}

fn rejected(cancellation: &Cancellation) -> FirstSliceError {
    check_cancellation(cancellation)
        .err()
        .unwrap_or(FirstSliceError::Identity)
}

struct BoundedBytes {
    bytes: Vec<u8>,
    maximum_bytes: usize,
}

impl Write for BoundedBytes {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let length = self
            .bytes
            .len()
            .checked_add(bytes.len())
            .filter(|length| *length <= self.maximum_bytes)
            .ok_or_else(|| {
                io::Error::other("optional normalized cache payload exceeds its bound")
            })?;
        if length > self.bytes.capacity() {
            let capacity = self
                .bytes
                .capacity()
                .saturating_mul(2)
                .max(length)
                .min(self.maximum_bytes);
            self.bytes
                .try_reserve_exact(capacity - self.bytes.len())
                .map_err(io::Error::other)?;
            if self.bytes.capacity() > self.maximum_bytes {
                return Err(io::Error::other(
                    "optional normalized cache capacity exceeds its bound",
                ));
            }
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct DecodedByteCounter<'a, W> {
    inner: W,
    bytes: usize,
    maximum_bytes: usize,
    cancellation: &'a Cancellation,
}

impl<W: Write> Write for DecodedByteCounter<'_, W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.cancellation.check().map_err(io::Error::other)?;
        let count = bytes.len().min(CHECKPOINT_BYTES);
        let length = self
            .bytes
            .checked_add(count)
            .filter(|length| *length <= self.maximum_bytes)
            .ok_or_else(|| {
                io::Error::other("optional normalized cache document exceeds its bound")
            })?;
        let count = self.inner.write(bytes.get(..count).ok_or_else(|| {
            io::Error::other("invalid optional normalized cache serialization length")
        })?)?;
        self.bytes = length - (bytes.len().min(CHECKPOINT_BYTES) - count);
        Ok(count)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.cancellation.check().map_err(io::Error::other)?;
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        FirstSliceIndexPreparation, FirstSliceService, MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
    };
    use rootlight_cancel::CancellationReason;
    use std::{
        cell::Cell,
        fs,
        time::{Duration, Instant},
    };
    use tempfile::TempDir;

    struct TrackedDrop<'a>(&'a Cell<usize>);

    impl Drop for TrackedDrop<'_> {
        fn drop(&mut self) {
            self.0.set(self.0.get() + 1);
        }
    }

    fn fixture() -> (
        CompressedNormalizedFileChunk,
        NormalizedIrDocument,
        Cancellation,
    ) {
        let root = TempDir::new().unwrap();
        fs::create_dir(root.path().join("src")).unwrap();
        let source = (0..80)
            .map(|index| format!("/// Repeated documentation for a generic cache fixture.\npub fn item_{index}() -> u32 {{ {index} }}\n"))
            .collect::<String>();
        fs::write(root.path().join("src/lib.rs"), source).unwrap();
        let cancellation = Cancellation::with_deadline(Instant::now() + Duration::from_secs(60));
        let service = FirstSliceService::new(2).unwrap();
        let prepared = service
            .prepare_rust_fixture(root.path(), &cancellation)
            .unwrap();
        let FirstSliceIndexPreparation::Pending(pending) = prepared else {
            panic!("initial fixture requires a generation");
        };
        let (_, entry) = pending.structural_artifacts.iter().next().unwrap();
        let cached = entry.normalized.as_ref().unwrap();
        let document = cached
            .rebind(
                pending.receipt.generation,
                service.analysis_limits.ir(),
                &service.extensions,
                &cancellation,
            )
            .unwrap();
        let workspace = generation_neutral_workspace_bytes(&document, || true).unwrap();
        let chunk = CompressedNormalizedFileChunk::new(
            &document,
            service.analysis_limits.ir(),
            &service.extensions,
            workspace,
            &cancellation,
        )
        .unwrap();
        (chunk, document, cancellation)
    }

    #[test]
    fn compressed_chunk_preserves_complete_fresh_generation_and_owned_charge() {
        let (chunk, document, cancellation) = fixture();
        let limits = IrLimits::default();
        let extensions = ExtensionSupport::default();
        let expected = CanonicalNormalizedFileChunk::new(&document, &limits, &extensions).unwrap();
        let generation = GenerationId::from_bytes([71; 20]);
        assert_eq!(
            chunk
                .rebind(generation, &limits, &extensions, &cancellation)
                .unwrap(),
            expected.rebind(generation, &limits, &extensions).unwrap(),
        );
        assert_eq!(
            chunk.accounted_bytes(),
            chunk.encoded.len()
                + std::mem::size_of::<CompressedNormalizedFileChunk>()
                + 2 * std::mem::size_of::<usize>()
        );
        assert!(chunk.accounted_bytes() * 2 < chunk.canonical_json_bytes);
        assert_eq!(
            chunk.reconstruction_memory_bytes().unwrap(),
            staged_memory_bytes(
                u64::try_from(chunk.canonical_json_bytes).unwrap(),
                chunk.workspace_bytes,
            )
            .unwrap()
        );
    }

    #[test]
    fn compressed_chunk_rejects_payload_framing_and_identity_corruption() {
        let (mut chunk, _, cancellation) = fixture();
        let limits = IrLimits::default();
        let extensions = ExtensionSupport::default();
        let generation = GenerationId::from_bytes([71; 20]);
        let original = chunk.encoded.clone();
        chunk.encoded[10] ^= 1;
        assert!(
            chunk
                .rebind(generation, &limits, &extensions, &cancellation)
                .is_err()
        );
        chunk.encoded = original.clone();
        let digest = chunk.digest;
        chunk.digest = ContentHash::from_bytes([0; 32]);
        chunk.encoded_digest = chunk.payload_digest(&cancellation).unwrap();
        assert!(
            chunk
                .rebind(generation, &limits, &extensions, &cancellation)
                .is_err()
        );
        chunk.digest = digest;
        let file = chunk.file;
        chunk.file = FileId::from_bytes([0; 20]);
        chunk.encoded_digest = chunk.payload_digest(&cancellation).unwrap();
        assert!(
            chunk
                .rebind(generation, &limits, &extensions, &cancellation)
                .is_err()
        );
        chunk.file = file;
        chunk.decoded_messagepack_bytes += 1;
        chunk.encoded_digest = chunk.payload_digest(&cancellation).unwrap();
        assert!(
            chunk
                .rebind(generation, &limits, &extensions, &cancellation)
                .is_err()
        );
        chunk.decoded_messagepack_bytes -= 1;
        chunk.canonical_json_bytes -= 1;
        chunk.encoded_digest = chunk.payload_digest(&cancellation).unwrap();
        assert!(
            chunk
                .rebind(generation, &limits, &extensions, &cancellation)
                .is_err()
        );
        chunk.canonical_json_bytes += 1;
        chunk.workspace_bytes += 1;
        chunk.encoded_digest = chunk.payload_digest(&cancellation).unwrap();
        assert!(
            chunk
                .rebind(generation, &limits, &extensions, &cancellation)
                .is_err()
        );
        chunk.workspace_bytes -= 1;
        for encoded in [
            original[..original.len() - 1].to_vec(),
            [original.as_ref(), &[0]].concat(),
        ] {
            chunk.encoded = encoded.into_boxed_slice();
            chunk.encoded_digest = chunk.payload_digest(&cancellation).unwrap();
            assert!(
                chunk
                    .rebind(generation, &limits, &extensions, &cancellation)
                    .is_err()
            );
        }
    }

    #[test]
    fn compressed_chunk_cancellation_and_expanded_bounds_fail_closed() {
        let (chunk, document, cancellation) = fixture();
        let extensions = ExtensionSupport::default();
        let mut limits = IrLimits::default();
        limits.max_document_bytes = chunk.decoded_messagepack_bytes - 1;
        assert!(
            chunk
                .rebind(
                    GenerationId::from_bytes([71; 20]),
                    &limits,
                    &extensions,
                    &cancellation
                )
                .is_err()
        );
        cancellation.cancel(CancellationReason::ClientRequest);
        assert!(matches!(
            chunk.rebind(
                GenerationId::from_bytes([71; 20]),
                &IrLimits::default(),
                &extensions,
                &cancellation
            ),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));
        assert!(matches!(
            CompressedNormalizedFileChunk::new(
                &document,
                &IrLimits::default(),
                &extensions,
                chunk.workspace_bytes,
                &cancellation
            ),
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));
        let mut destination = BoundedBytes {
            bytes: Vec::new(),
            maximum_bytes: 3,
        };
        destination.write_all(b"abc").unwrap();
        assert!(destination.write_all(b"d").is_err());
        assert_eq!(destination.bytes, b"abc");
    }

    #[test]
    fn optional_chunk_retained_size_failure_drops_value_and_releases_peak() {
        let service = FirstSliceService::new(2).unwrap();
        let generation = GenerationId::from_bytes([71; 20]);
        let floor = 64 * 1024_u64;
        let mut admission = GenerationAdmission::new(floor).unwrap();
        let mut reservation = service
            .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
            .unwrap();
        let cancellation = Cancellation::new();
        admission
            .admit_known_bytes(3, &mut reservation, false, &cancellation)
            .unwrap();
        let base = admission.reservation_bytes();
        let drops = Cell::new(0);
        let retained_calls = Cell::new(0);
        let (value, peak) = with_optional_staging(
            &mut admission,
            &mut reservation,
            Ok(128),
            false,
            &cancellation,
            || {
                assert_eq!(
                    service.generation_cache.lock().unwrap().reservations[&generation],
                    base + 128
                );
                Ok(TrackedDrop(&drops))
            },
            |_| {
                retained_calls.set(retained_calls.get() + 1);
                assert_eq!(drops.get(), 0);
                Err(FirstSliceError::Identity)
            },
        )
        .unwrap();
        assert!(value.is_none());
        assert_eq!(peak, base + 128);
        assert_eq!(retained_calls.get(), 1);
        assert_eq!(drops.get(), 1);
        assert_eq!(admission.charged_bytes(), 3);
        assert_eq!(admission.reservation_bytes(), base);
        assert_eq!(
            service.generation_cache.lock().unwrap().reservations[&generation],
            base
        );
        drop(reservation);
        assert!(
            !service
                .generation_cache
                .lock()
                .unwrap()
                .reservations
                .contains_key(&generation)
        );
    }

    #[test]
    fn optional_chunk_charge_failure_drops_value_and_releases_peak() {
        let service = FirstSliceService::new(2).unwrap();
        let generation = GenerationId::from_bytes([71; 20]);
        let floor = 64 * 1024_u64;
        let mut admission = GenerationAdmission::new(floor).unwrap();
        let mut reservation = service
            .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
            .unwrap();
        let cancellation = Cancellation::new();
        admission
            .admit_known_bytes(3, &mut reservation, false, &cancellation)
            .unwrap();
        let base = admission.reservation_bytes();
        let drops = Cell::new(0);
        let retained_calls = Cell::new(0);
        let (value, peak) = with_optional_staging(
            &mut admission,
            &mut reservation,
            Ok(128),
            false,
            &cancellation,
            || {
                assert_eq!(
                    service.generation_cache.lock().unwrap().reservations[&generation],
                    base + 128
                );
                Ok(TrackedDrop(&drops))
            },
            |_| {
                retained_calls.set(retained_calls.get() + 1);
                assert_eq!(drops.get(), 0);
                Ok(MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES)
            },
        )
        .unwrap();
        assert!(value.is_none());
        assert_eq!(peak, base + 128);
        assert_eq!(retained_calls.get(), 1);
        assert_eq!(drops.get(), 1);
        assert_eq!(admission.charged_bytes(), 3);
        assert_eq!(admission.reservation_bytes(), base);
        assert_eq!(
            service.generation_cache.lock().unwrap().reservations[&generation],
            base
        );
        drop(reservation);
        assert!(
            !service
                .generation_cache
                .lock()
                .unwrap()
                .reservations
                .contains_key(&generation)
        );
    }

    #[test]
    fn optional_chunk_successful_work_cancellation_is_terminal() {
        let service = FirstSliceService::new(2).unwrap();
        let generation = GenerationId::from_bytes([71; 20]);
        let floor = 64 * 1024_u64;
        let mut admission = GenerationAdmission::new(floor).unwrap();
        let mut reservation = service
            .reserve_generation_memory(generation, floor, PendingGenerationMemory::Reserved)
            .unwrap();
        let cancellation = Cancellation::new();
        admission
            .admit_known_bytes(3, &mut reservation, false, &cancellation)
            .unwrap();
        let base = admission.reservation_bytes();
        let drops = Cell::new(0);
        let retained_calls = Cell::new(0);
        let result = with_optional_staging(
            &mut admission,
            &mut reservation,
            Ok(128),
            false,
            &cancellation,
            || {
                assert_eq!(
                    service.generation_cache.lock().unwrap().reservations[&generation],
                    base + 128
                );
                cancellation.cancel(CancellationReason::ClientRequest);
                Ok(TrackedDrop(&drops))
            },
            |_| {
                retained_calls.set(retained_calls.get() + 1);
                Ok(3)
            },
        );
        assert!(matches!(
            result,
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));
        assert_eq!(retained_calls.get(), 0);
        assert_eq!(drops.get(), 1);
        assert_eq!(admission.charged_bytes(), 3);
        assert_eq!(admission.reservation_bytes(), base);
        assert_eq!(
            service.generation_cache.lock().unwrap().reservations[&generation],
            base
        );
        drop(reservation);
        assert!(
            !service
                .generation_cache
                .lock()
                .unwrap()
                .reservations
                .contains_key(&generation)
        );
    }

    #[test]
    fn optional_chunk_work_reserves_a_peak_without_accumulating_temporary_bytes() {
        let service = FirstSliceService::new(2).unwrap();
        let generation = GenerationId::from_bytes([71; 20]);
        let base = 64 * 1024_u64;
        let mut admission = GenerationAdmission::new(base).unwrap();
        let mut reservation = service
            .reserve_generation_memory(generation, base, PendingGenerationMemory::Reserved)
            .unwrap();
        let cancellation = Cancellation::new();
        let assert_reservation = |bytes| {
            assert_eq!(
                service
                    .generation_cache
                    .lock()
                    .unwrap()
                    .reservations
                    .get(&generation),
                Some(&bytes)
            );
        };
        let (value, peak) = with_optional_staging(
            &mut admission,
            &mut reservation,
            Err(FirstSliceError::Limits),
            false,
            &cancellation,
            || -> Result<Vec<u8>, FirstSliceError> { panic!("rejected sizing cannot enter work") },
            |_| Ok(3),
        )
        .unwrap();
        assert!(value.is_none());
        assert_eq!(peak, 0);
        assert_reservation(base);
        let (value, peak) = with_optional_staging(
            &mut admission,
            &mut reservation,
            Ok(MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES),
            false,
            &cancellation,
            || -> Result<Vec<u8>, FirstSliceError> { panic!("rejected peak cannot enter work") },
            |_| Ok(3),
        )
        .unwrap();
        assert!(value.is_none());
        assert_eq!(peak, 0);
        assert_reservation(base);
        let (value, peak) = with_optional_staging(
            &mut admission,
            &mut reservation,
            Ok(128),
            false,
            &cancellation,
            || -> Result<Vec<u8>, FirstSliceError> { Err(FirstSliceError::Identity) },
            |_| Ok(3),
        )
        .unwrap();
        assert!(value.is_none());
        assert_eq!(peak, base + 128);
        assert_reservation(base);
        let (value, peak) = with_optional_staging(
            &mut admission,
            &mut reservation,
            Ok(128),
            false,
            &cancellation,
            || Ok(vec![1, 2, 3]),
            |value| u64::try_from(value.len()).map_err(|_| FirstSliceError::Limits),
        )
        .unwrap();
        assert_eq!(value, Some(vec![1, 2, 3]));
        assert_eq!(peak, base + 128);
        assert_eq!(admission.charged_bytes(), 3);
        assert_reservation(base + 3);
        let result = with_optional_staging(
            &mut admission,
            &mut reservation,
            Ok(128),
            false,
            &cancellation,
            || -> Result<Vec<u8>, FirstSliceError> {
                cancellation.cancel(CancellationReason::ClientRequest);
                Err(FirstSliceError::Cancelled(
                    CancellationReason::ClientRequest,
                ))
            },
            |_| Ok(3),
        );
        assert!(matches!(
            result,
            Err(FirstSliceError::Cancelled(
                CancellationReason::ClientRequest
            ))
        ));
        assert_eq!(admission.charged_bytes(), 3);
        assert_reservation(base + 3);
    }
}
