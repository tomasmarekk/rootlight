//! Real writer controls for the conservative prospective sidecar envelope.
//! Model-only metadata maxima never replace final descriptor ownership or the
//! independent sealed/reopened restore plan.

use std::fs;

use super::*;
use crate::FirstSliceService;
use rootlight_catalog::OracleWriter;
use rootlight_runtime::RuntimePaths;

fn fixture() -> (tempfile::TempDir, tempfile::TempDir, RuntimePaths) {
    let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    fs::write(
        root.path().join("source.rs"),
        "pub fn kept() -> u32 { 7 }\n",
    )
    .unwrap();
    fs::write(root.path().join("notes.txt"), "exact source note\n").unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    (storage, root, paths)
}

#[test]
fn actual_authority_packed_index_incremental_and_restore_owners_fit_the_same_reserved_envelope() {
    let (_storage, root, paths) = fixture();
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    let receipt = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    assert!(snapshot.source_files().is_empty());
    let claims = snapshot
        .document()
        .extensions
        .iter()
        .filter(|extension| extension.namespace == rootlight_ir::FILE_IDENTITY_CLAIM_NAMESPACE)
        .map(|extension| rootlight_ir::decode_file_identity_claim_envelope(extension).unwrap())
        .collect::<Vec<_>>();
    let descriptions = claims
        .iter()
        .map(|claim| {
            let file = snapshot.find_file(claim.file).unwrap();
            Source {
                claim,
                language: &file.language,
                encoding: &file.encoding,
                generated: file.generated,
            }
        })
        .collect::<Vec<_>>();
    let mut charged = 0_u64;
    let envelope = Envelope::prepare(
        snapshot.metadata(),
        &descriptions,
        &cancellation,
        &mut |bytes| {
            charged = charged.checked_add(bytes).unwrap();
            Ok(())
        },
    )
    .unwrap();
    assert!(charged > 0);
    assert!(charged < envelope.restore_bytes().unwrap());
    assert!(
        envelope.restore_bytes().unwrap()
            >= MAX_INCREMENTAL_STATE_BYTES * 8 + crate::GENERATION_MEMORY_FIXED_OVERHEAD_BYTES
    );
    let prepared = service
        .durable
        .as_ref()
        .unwrap()
        .begin_generation(receipt.repository, receipt.generation)
        .unwrap();
    let mut physical = snapshot
        .document()
        .files
        .iter()
        .map(|file| {
            let path = RelativePath::parse(Path::new(&file.path)).unwrap();
            let bytes = fs::read(root.path().join(&file.path)).unwrap();
            crate::RustSourceInput {
                snapshot: SourceSnapshot::from_persisted(
                    receipt.repository,
                    path,
                    file.id,
                    file.content_hash,
                    bytes,
                )
                .unwrap(),
                encoding: file.encoding.clone(),
                generated: file.generated,
                origins: Vec::new(),
            }
        })
        .collect::<Vec<_>>();
    physical.sort_unstable_by_key(|input| input.snapshot.file());
    let mut writer = prepared.begin_packed_source_write().unwrap();
    for input in &physical {
        writer.push(&input.snapshot).unwrap();
    }
    writer.finish().unwrap();
    let storage = prepared.source_storage.lock().unwrap().unwrap();
    assert!(storage.index_bytes.unwrap() <= envelope.packed_index);
    assert!(storage.packs.unwrap() <= u64::try_from(physical.len()).unwrap());
    let verified = IdentityVerifiedGeneration::verify_snapshot(snapshot.clone(), &context).unwrap();
    prepared
        .write_source_authority(&verified, &cancellation)
        .unwrap();
    let incremental = PreparedIncrementalState {
        baseline: service.incremental_baselines[&receipt.generation].clone(),
        inputs: service.incremental_inputs[&receipt.generation].clone(),
        evidence: service.incremental_evidence[&receipt.generation].clone(),
    };
    prepared
        .write_incremental_state(&incremental, &cancellation)
        .unwrap();
    envelope.verify_written(&prepared).unwrap();
    let actual_authority = prepared.source_authority.lock().unwrap().unwrap().bytes;
    assert!(actual_authority <= envelope.authority);
    let (reader, _, written) = OracleWriter::create_in(prepared.path())
        .unwrap()
        .seal_preserving_verified_with_read_plan(verified, &context)
        .unwrap();
    let mut reopened = None;
    let stopped = reader.read_with_admission(&context, |plan| {
        reopened = Some(plan);
        Err::<(), ()>(())
    });
    assert!(matches!(
        stopped,
        Err(OracleReadAdmissionError::Admission(()))
    ));
    assert_eq!(reopened, Some(written));
    let prospective =
        oracle_restore_memory_bytes(written, false, envelope.restore_bytes().unwrap()).unwrap();
    let actual = prepared
        .oracle_restore_memory_bytes(written, receipt.indexed_files, false)
        .unwrap();
    assert!(prospective >= actual);
}

#[test]
fn sidecar_workspace_refusal_and_foreign_source_metadata_are_strict_errors() {
    let (_storage, root, paths) = fixture();
    let cancellation = Cancellation::new();
    let mut service = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    let receipt = service
        .index_repository(root.path(), &cancellation)
        .unwrap();
    let snapshot = service
        .loaded_generation_snapshot(receipt.generation)
        .unwrap();
    let mut claim = rootlight_ir::decode_file_identity_claim_envelope(
        snapshot
            .document()
            .extensions
            .iter()
            .find(|extension| extension.namespace == rootlight_ir::FILE_IDENTITY_CLAIM_NAMESPACE)
            .unwrap(),
    )
    .unwrap();
    let file = snapshot.find_file(claim.file).unwrap();
    let refusal = crate::generation_memory_limit(0, 1024, crate::PendingGenerationMemory::Staged);
    assert_eq!(
        Envelope::prepare(
            snapshot.metadata(),
            &[Source {
                claim: &claim,
                language: &file.language,
                encoding: &file.encoding,
                generated: file.generated
            }],
            &cancellation,
            &mut |_| Err(refusal)
        )
        .err(),
        Some(refusal)
    );
    claim.repository = rootlight_ids::derive_repository(b"foreign-sidecar-source").id();
    assert!(matches!(
        Envelope::prepare(
            snapshot.metadata(),
            &[Source {
                claim: &claim,
                language: &file.language,
                encoding: &file.encoding,
                generated: file.generated
            }],
            &cancellation,
            &mut |_| Ok(())
        ),
        Err(FirstSliceError::Identity)
    ));
}
