//! Preparation-local proofs for repeated optional batch retention decisions.
//! Proofs retain no decoded children and authorize only the complete envelope
//! value that passed the existing canonical decoder.

use std::io::{self, Write};

use rootlight_cancel::Cancellation;
use rootlight_ir::ExtensionEnvelope;
use serde::Serialize;

use crate::{
    FirstSliceError, LEXICAL_EXTENSION_NAMESPACE, MAX_RETAINED_OPTIONAL_EXTENSIONS,
    SignatureBatchProfileGuard, check_cancellation, current_cancellation, increment_profile_count,
    with_analysis_profile,
};

pub(super) type Admission<'a> = Option<&'a mut dyn FnMut(u64) -> Result<(), FirstSliceError>>;

pub(super) struct PlanningContext<'cancel, 'admission> {
    pub(super) cancellation: &'cancel Cancellation,
    pub(super) admission: Admission<'admission>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Metadata {
    signature: bool,
    logical_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Proof {
    position: usize,
    next_position: usize,
    fingerprint: blake3::Hash,
    metadata: Metadata,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(super) struct Cache {
    proofs: Vec<Proof>,
    // GenerationAdmission owns a monotonic allowance, including simultaneous
    // retained and transaction vectors. Repeated clones reuse this admitted peak.
    admitted_peak: u64,
}

impl Cache {
    pub(super) fn invalidate(&mut self) {
        self.proofs.clear();
    }
}

pub(super) struct Workspace {
    cache: Option<Cache>,
    retained_capacity: usize,
    active: bool,
    growth_available: bool,
    reuse_retained: bool,
}

impl Workspace {
    pub(super) fn new(
        retained: Option<&Cache>,
        admission: &mut Admission<'_>,
        cancellation: &Cancellation,
    ) -> Result<Self, FirstSliceError> {
        check_cancellation(cancellation)?;
        let Some(retained) = retained else {
            return Ok(Self {
                cache: None,
                retained_capacity: 0,
                active: false,
                growth_available: false,
                reuse_retained: false,
            });
        };
        let mut workspace = Self {
            cache: Some(Cache {
                proofs: Vec::new(),
                admitted_peak: retained.admitted_peak,
            }),
            retained_capacity: retained.proofs.capacity(),
            active: admission.is_some(),
            growth_available: admission.is_some(),
            reuse_retained: true,
        };
        if workspace.active && !retained.proofs.is_empty() {
            if workspace.reserve(retained.proofs.len(), admission, cancellation)? {
                workspace
                    .cache
                    .as_mut()
                    .expect("enabled metadata workspace")
                    .proofs
                    .extend_from_slice(&retained.proofs);
                workspace.reuse_retained = false;
            } else {
                workspace.active = false;
            }
        } else if workspace.active {
            workspace.reuse_retained = false;
        }
        Ok(workspace)
    }

    fn reserve(
        &mut self,
        desired: usize,
        admission: &mut Admission<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, FirstSliceError> {
        check_cancellation(cancellation)?;
        let Some(cache) = &mut self.cache else {
            return Ok(false);
        };
        if cache.proofs.capacity() >= desired {
            return Ok(true);
        }
        if !self.growth_available || desired > MAX_RETAINED_OPTIONAL_EXTENSIONS {
            return Ok(false);
        }
        let old_capacity = cache.proofs.capacity();
        if !admit_peak(
            cache,
            self.retained_capacity,
            old_capacity,
            desired,
            admission,
            cancellation,
        )? {
            self.growth_available = false;
            return Ok(false);
        }
        check_cancellation(cancellation)?;
        // Keep the admitted prefix until the replacement's exact capacity is
        // also admitted. Allocation failure or excess-capacity refusal can then
        // discard only the candidate without losing already verified proofs.
        let mut candidate = Vec::new();
        if candidate.try_reserve_exact(desired).is_err()
            || candidate.capacity() > MAX_RETAINED_OPTIONAL_EXTENSIONS
        {
            self.growth_available = false;
            return Ok(false);
        }
        if !admit_peak(
            cache,
            self.retained_capacity,
            old_capacity,
            candidate.capacity(),
            admission,
            cancellation,
        )? {
            self.growth_available = false;
            return Ok(false);
        }
        check_cancellation(cancellation)?;
        candidate.extend_from_slice(&cache.proofs);
        cache.proofs = candidate;
        Ok(true)
    }

    fn metadata(
        &mut self,
        position: usize,
        extension: &ExtensionEnvelope,
        admission: &mut Admission<'_>,
        cancellation: &Cancellation,
    ) -> Result<Option<Metadata>, FirstSliceError> {
        check_cancellation(cancellation)?;
        if !self.active || extension.namespace != LEXICAL_EXTENSION_NAMESPACE {
            return Ok(None);
        }
        let signature = match extension.version.as_str() {
            rootlight_ir::LEXICAL_SIGNATURE_BATCH_VERSION => true,
            rootlight_ir::LEXICAL_SUMMARY_BATCH_VERSION => false,
            _ => return Ok(None),
        };
        let fingerprint = fingerprint(extension, cancellation)?;
        let cache = self.cache.as_mut().expect("enabled metadata workspace");
        let entry = cache
            .proofs
            .binary_search_by_key(&position, |proof| proof.position);
        if let Ok(index) = entry {
            let proof = cache.proofs[index];
            if proof.fingerprint == fingerprint {
                return Ok(Some(proof.metadata));
            }
            cache.proofs.remove(index);
        }
        let logical_count = if signature {
            let profile = SignatureBatchProfileGuard::begin(extension.payload.len());
            let children = rootlight_ir::decode_lexical_signature_batch_envelope_with_checkpoint(
                extension,
                || cancellation.check().is_ok(),
            )
            .map_err(|_| current_cancellation(cancellation).unwrap_or(FirstSliceError::Identity))?;
            profile.finish(children.len());
            u64::try_from(children.len()).map_err(|_| FirstSliceError::Limits)?
        } else {
            with_analysis_profile(|profile| {
                increment_profile_count(
                    &mut profile.append.summary_batch_classifications,
                    &mut profile.counts_complete,
                );
            });
            let children = rootlight_ir::decode_lexical_summary_batch_envelope(extension)
                .map_err(|_| FirstSliceError::Identity)?;
            u64::try_from(children.len()).map_err(|_| FirstSliceError::Limits)?
        };
        check_cancellation(cancellation)?;
        let metadata = Metadata {
            signature,
            logical_count,
        };
        let cache = self.cache.as_ref().expect("enabled metadata workspace");
        if cache.proofs.len() < MAX_RETAINED_OPTIONAL_EXTENSIONS {
            let desired = if cache.proofs.len() == cache.proofs.capacity() {
                cache
                    .proofs
                    .capacity()
                    .saturating_mul(2)
                    .clamp(4, MAX_RETAINED_OPTIONAL_EXTENSIONS)
            } else {
                cache.proofs.capacity()
            };
            if self.reserve(desired, admission, cancellation)? {
                let proofs = &mut self
                    .cache
                    .as_mut()
                    .expect("enabled metadata workspace")
                    .proofs;
                let index = proofs.partition_point(|proof| proof.position < position);
                proofs.insert(
                    index,
                    Proof {
                        position,
                        next_position: position,
                        fingerprint,
                        metadata,
                    },
                );
            }
        }
        Ok(Some(metadata))
    }

    pub(super) fn signature(
        &mut self,
        position: usize,
        extension: &ExtensionEnvelope,
        admission: &mut Admission<'_>,
        cancellation: &Cancellation,
    ) -> Result<bool, FirstSliceError> {
        self.metadata(position, extension, admission, cancellation)?
            .map_or_else(
                || crate::is_lexical_signature(extension),
                |metadata| Ok(metadata.signature),
            )
    }

    pub(super) fn logical_count(
        &mut self,
        position: usize,
        extension: &ExtensionEnvelope,
        admission: &mut Admission<'_>,
        cancellation: &Cancellation,
    ) -> Result<u64, FirstSliceError> {
        self.metadata(position, extension, admission, cancellation)?
            .map_or_else(
                || crate::optional_extension_logical_count(extension),
                |metadata| Ok(metadata.logical_count),
            )
    }

    pub(super) fn retain_source(&mut self, position: usize, next_position: Option<usize>) {
        let Some(cache) = &mut self.cache else {
            return;
        };
        if let Ok(index) = cache
            .proofs
            .binary_search_by_key(&position, |proof| proof.position)
        {
            if let Some(next_position) = next_position {
                cache.proofs[index].next_position = next_position;
            } else {
                cache.proofs.remove(index);
            }
        }
    }

    pub(super) fn commit(
        mut self,
        dropped: &[usize],
        previous_target_len: usize,
        retained: Option<Cache>,
    ) -> Option<Cache> {
        if self.reuse_retained {
            let admitted_peak = self.cache.as_ref()?.admitted_peak;
            self.cache = retained;
            self.cache.as_mut()?.admitted_peak = admitted_peak;
        }
        let cache = self.cache.as_mut()?;
        // All target removals and source retention decisions are already planned.
        // Only the successful append commit publishes the remapped proofs.
        cache.proofs.retain_mut(|proof| {
            if proof.position < previous_target_len {
                let greater = dropped.partition_point(|&index| index > proof.position);
                if dropped.get(greater) == Some(&proof.position) {
                    return false;
                }
                proof.next_position = proof.position - (dropped.len() - greater);
            }
            proof.position = proof.next_position;
            true
        });
        cache.proofs.sort_unstable_by_key(|proof| proof.position);
        self.cache
    }
}

fn admit_peak(
    cache: &mut Cache,
    retained_capacity: usize,
    old_capacity: usize,
    candidate_capacity: usize,
    admission: &mut Admission<'_>,
    cancellation: &Cancellation,
) -> Result<bool, FirstSliceError> {
    let peak = retained_capacity
        .checked_add(old_capacity)
        .and_then(|capacity| capacity.checked_add(candidate_capacity))
        .and_then(|capacity| capacity.checked_mul(size_of::<Proof>()))
        .and_then(|bytes| u64::try_from(bytes).ok())
        .ok_or(FirstSliceError::Limits)?;
    if peak <= cache.admitted_peak {
        return Ok(true);
    }
    let Some(admit) = admission.as_mut() else {
        return Ok(false);
    };
    if let Err(error) = admit(peak - cache.admitted_peak) {
        check_cancellation(cancellation)?;
        return match error {
            FirstSliceError::Limits | FirstSliceError::GenerationMemoryLimit { .. } => Ok(false),
            error => Err(error),
        };
    }
    cache.admitted_peak = peak;
    Ok(true)
}

fn fingerprint(
    extension: &ExtensionEnvelope,
    cancellation: &Cancellation,
) -> Result<blake3::Hash, FirstSliceError> {
    check_cancellation(cancellation)?;
    let mut writer = FingerprintWriter {
        hasher: blake3::Hasher::new(),
        cancellation,
    };
    writer
        .hasher
        .update(b"rootlight.optional-extension-proof.v1\0");
    extension
        .serialize(&mut rmp_serde::Serializer::new(&mut writer))
        .map_err(|_| current_cancellation(cancellation).unwrap_or(FirstSliceError::Identity))?;
    check_cancellation(cancellation)?;
    Ok(writer.hasher.finalize())
}

struct FingerprintWriter<'a> {
    hasher: blake3::Hasher,
    cancellation: &'a Cancellation,
}

impl Write for FingerprintWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        for part in bytes.chunks(64 * 1024) {
            self.cancellation
                .check()
                .map_err(|_| io::Error::other("metadata fingerprint cancelled"))?;
            self.hasher.update(part);
        }
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analysis_profile_tests::metadata_pressure_batch;
    use rootlight_ids::{FactId, GenerationId, RepositoryId};
    use rootlight_ir::{ExtensionCriticality, FactRef, SourceRef};

    fn proof_cache(extension: &ExtensionEnvelope) -> Cache {
        let empty = Cache::default();
        let mut admit = |_| Ok(());
        let mut admission: Admission<'_> = Some(&mut admit);
        let cancellation = Cancellation::new();
        let mut workspace = Workspace::new(Some(&empty), &mut admission, &cancellation).unwrap();
        workspace
            .signature(0, extension, &mut admission, &cancellation)
            .unwrap();
        workspace.commit(&[], 1, None).unwrap()
    }

    #[test]
    fn complete_value_binding_rejects_changed_headers_payload_and_evidence() {
        for signature in [false, true] {
            let original = metadata_pressure_batch(40, signature);
            let cache = proof_cache(&original);
            let original_cache = cache.clone();
            let mut mutations = Vec::new();
            let mut changed = original.clone();
            changed.payload = changed.payload.replace("batch 40", "batch 41");
            assert_eq!(changed.payload.len(), original.payload.len());
            mutations.push(changed);
            let mut changed = original.clone();
            changed.payload.push(' ');
            mutations.push(changed);
            let mut changed = original.clone();
            changed.id = FactId::from_bytes([1; 20]);
            mutations.push(changed);
            let mut changed = original.clone();
            changed.repository = RepositoryId::from_bytes([1; 16]);
            mutations.push(changed);
            let mut changed = original.clone();
            changed.generation = GenerationId::from_bytes([1; 20]);
            mutations.push(changed);
            let mut changed = original.clone();
            changed.version = "unknown".to_owned();
            mutations.push(changed);
            let mut changed = original.clone();
            changed.criticality = ExtensionCriticality::Critical;
            mutations.push(changed);
            let mut changed = original.clone();
            changed.provenance = FactId::from_bytes([1; 20]);
            mutations.push(changed);
            let mut changed = original.clone();
            changed.evidence.derivation.clear();
            mutations.push(changed);
            let mut changed = original.clone();
            changed
                .evidence
                .derivation
                .push(FactRef::File(crate::FileId::from_bytes([1; 20])));
            mutations.push(changed);
            let mut changed = original.clone();
            changed.evidence.source = None;
            mutations.push(changed);
            let mut changed = original.clone();
            let source = original.evidence.source.as_ref().unwrap();
            changed.evidence.source = Some(SourceRef::new(
                source.repository(),
                source.generation(),
                source.span(),
                crate::content_hash(b"y"),
                None,
            ));
            mutations.push(changed);
            for (case, changed) in mutations.iter().enumerate() {
                let mut admit = |_| Ok(());
                let mut admission: Admission<'_> = Some(&mut admit);
                let cancellation = Cancellation::new();
                let mut workspace =
                    Workspace::new(Some(&cache), &mut admission, &cancellation).unwrap();
                assert!(
                    matches!(
                        workspace.signature(0, changed, &mut admission, &cancellation),
                        Err(FirstSliceError::Identity)
                    ),
                    "signature={signature}, mutation={case}"
                );
                assert_eq!(cache, original_cache);
            }
            let mut changed = original.clone();
            changed.namespace = "test.changed".to_owned();
            let mut admit = |_| Ok(());
            let mut admission: Admission<'_> = Some(&mut admit);
            let cancellation = Cancellation::new();
            let mut workspace =
                Workspace::new(Some(&cache), &mut admission, &cancellation).unwrap();
            assert_eq!(
                workspace
                    .metadata(0, &changed, &mut admission, &cancellation)
                    .unwrap(),
                None
            );
            assert_eq!(cache, original_cache);
        }
    }

    #[test]
    fn exact_value_proof_survives_moves_and_revalidates_valid_replacements() {
        let extension = metadata_pressure_batch(40, true);
        let mut extensions = vec![extension];
        let cache = proof_cache(&extensions[0]);
        extensions.reserve_exact(1024);
        let mut admit = |_| Ok(());
        let mut admission: Admission<'_> = Some(&mut admit);
        let cancellation = Cancellation::new();
        let mut workspace = Workspace::new(Some(&cache), &mut admission, &cancellation).unwrap();
        let (_, profile) = crate::capture_first_slice_analysis_profile(|| {
            assert!(
                workspace
                    .signature(0, &extensions[0], &mut admission, &cancellation)
                    .unwrap()
            );
            assert_eq!(
                workspace
                    .logical_count(0, &extensions[0], &mut admission, &cancellation)
                    .unwrap(),
                2
            );
        });
        assert_eq!(profile.append.signature_batch_classifications, 0);
        extensions[0] = metadata_pressure_batch(41, false);
        assert!(
            !workspace
                .signature(0, &extensions[0], &mut admission, &cancellation)
                .unwrap()
        );
        assert_eq!(
            workspace
                .logical_count(0, &extensions[0], &mut admission, &cancellation)
                .unwrap(),
            2
        );
        let replacement = workspace.commit(&[], 1, Some(cache.clone())).unwrap();
        assert_ne!(
            replacement.proofs[0].fingerprint,
            cache.proofs[0].fingerprint
        );
    }

    #[test]
    fn actual_capacity_peak_is_charged_and_reused_with_commit_remapping() {
        let empty = Cache::default();
        let cancellation = Cancellation::new();
        let mut charged = 0_u64;
        let mut admit = |bytes| {
            charged += bytes;
            Ok(())
        };
        let mut admission: Admission<'_> = Some(&mut admit);
        let mut workspace = Workspace::new(Some(&empty), &mut admission, &cancellation).unwrap();
        let mut first_capacity = 0;
        for position in 0..6 {
            workspace
                .signature(
                    position,
                    &metadata_pressure_batch(40 + u8::try_from(position).unwrap(), true),
                    &mut admission,
                    &cancellation,
                )
                .unwrap();
            if position == 0 {
                first_capacity = workspace.cache.as_ref().unwrap().proofs.capacity();
            }
        }
        workspace.retain_source(4, Some(3));
        workspace.retain_source(5, None);
        let cache = workspace.commit(&[1], 4, None).unwrap();
        assert_eq!(
            cache
                .proofs
                .iter()
                .map(|proof| proof.position)
                .collect::<Vec<_>>(),
            vec![0, 1, 2, 3]
        );
        assert_eq!(
            charged,
            u64::try_from((first_capacity + cache.proofs.capacity()) * size_of::<Proof>()).unwrap()
        );
        assert_eq!(charged, cache.admitted_peak);
        let mut clone_charged = 0;
        let mut admit = |bytes| {
            clone_charged += bytes;
            Ok(())
        };
        let mut admission: Admission<'_> = Some(&mut admit);
        let workspace = Workspace::new(Some(&cache), &mut admission, &cancellation).unwrap();
        let working_capacity = workspace.cache.as_ref().unwrap().proofs.capacity();
        let next = workspace.commit(&[], 4, Some(cache.clone())).unwrap();
        let peak = u64::try_from((cache.proofs.capacity() + working_capacity) * size_of::<Proof>())
            .unwrap();
        assert_eq!(clone_charged, peak - cache.admitted_peak);
        assert_eq!(next.admitted_peak, peak);
        let mut admit = |bytes| panic!("unchanged peak must not charge again: {bytes}");
        let mut admission: Admission<'_> = Some(&mut admit);
        Workspace::new(Some(&next), &mut admission, &cancellation).unwrap();
        let mut invalidated = next;
        let capacity = invalidated.proofs.capacity();
        invalidated.invalidate();
        assert!(invalidated.proofs.is_empty());
        assert_eq!(invalidated.proofs.capacity(), capacity);
    }

    #[test]
    fn full_optional_cap_falls_back_without_retaining_more_proofs() {
        let extension = metadata_pressure_batch(40, true);
        let mut cache = proof_cache(&extension);
        let proof = cache.proofs[0];
        cache.proofs = Vec::with_capacity(MAX_RETAINED_OPTIONAL_EXTENSIONS);
        for position in 0..MAX_RETAINED_OPTIONAL_EXTENSIONS {
            cache.proofs.push(Proof {
                position,
                next_position: position,
                ..proof
            });
        }
        cache.admitted_peak =
            u64::try_from(cache.proofs.capacity() * size_of::<Proof>() * 2).unwrap();
        let cancellation = Cancellation::new();
        let mut admit =
            |bytes| panic!("the existing bounded clone allowance covers this operation: {bytes}");
        let mut admission: Admission<'_> = Some(&mut admit);
        let mut workspace = Workspace::new(Some(&cache), &mut admission, &cancellation).unwrap();
        assert!(
            workspace
                .signature(
                    MAX_RETAINED_OPTIONAL_EXTENSIONS,
                    &extension,
                    &mut admission,
                    &cancellation
                )
                .unwrap()
        );
        let result = workspace
            .commit(&[], MAX_RETAINED_OPTIONAL_EXTENSIONS + 1, Some(cache))
            .unwrap();
        assert_eq!(result.proofs.len(), MAX_RETAINED_OPTIONAL_EXTENSIONS);
        assert!(result.proofs.capacity() <= MAX_RETAINED_OPTIONAL_EXTENSIONS);
    }

    #[test]
    fn refused_clone_allocates_nothing_and_preserves_retained_proofs_at_commit() {
        let extension = metadata_pressure_batch(40, true);
        let cache = proof_cache(&extension);
        let cancellation = Cancellation::new();
        let mut admit = |_| {
            Err(crate::generation_memory_limit(
                0,
                u64::MAX,
                crate::PendingGenerationMemory::Staged,
            ))
        };
        let mut admission: Admission<'_> = Some(&mut admit);
        let mut workspace = Workspace::new(Some(&cache), &mut admission, &cancellation).unwrap();
        assert_eq!(workspace.cache.as_ref().unwrap().proofs.capacity(), 0);
        assert!(!workspace.active);
        assert!(
            workspace
                .signature(0, &extension, &mut admission, &cancellation)
                .unwrap()
        );
        let preserved = workspace.commit(&[], 1, Some(cache.clone())).unwrap();
        assert_eq!(preserved, cache);
    }

    #[test]
    fn refused_growth_keeps_admitted_proofs_and_invariant_errors_propagate() {
        let extension = metadata_pressure_batch(40, true);
        let cache = proof_cache(&extension);
        let cancellation = Cancellation::new();
        let mut attempts = 0;
        let mut admit = |_| {
            attempts += 1;
            if attempts > 1 {
                Err(crate::generation_memory_limit(
                    0,
                    u64::MAX,
                    crate::PendingGenerationMemory::Staged,
                ))
            } else {
                Ok(())
            }
        };
        let mut admission: Admission<'_> = Some(&mut admit);
        let mut workspace = Workspace::new(Some(&cache), &mut admission, &cancellation).unwrap();
        assert!(
            workspace
                .signature(
                    1,
                    &metadata_pressure_batch(41, true),
                    &mut admission,
                    &cancellation
                )
                .unwrap()
        );
        let (_, profile) = crate::capture_first_slice_analysis_profile(|| {
            assert!(
                workspace
                    .signature(0, &extension, &mut admission, &cancellation)
                    .unwrap()
            );
        });
        assert_eq!(profile.append.signature_batch_classifications, 0);
        let retained = workspace.commit(&[], 2, Some(cache.clone())).unwrap();
        assert_eq!(retained.proofs, cache.proofs);
        for error in [FirstSliceError::Retention, FirstSliceError::CatalogCorrupt] {
            let mut admit = |_| Err(error);
            let mut admission: Admission<'_> = Some(&mut admit);
            assert!(
                matches!(Workspace::new(Some(&cache), &mut admission, &cancellation), Err(actual) if actual == error)
            );
        }
    }
}
