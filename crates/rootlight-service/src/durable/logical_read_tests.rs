//! Authoritative oracle routing never turns an RDF base into a canonical lease.

use std::fs;

use rootlight_catalog::{ORACLE_FILENAME, OracleWriter, RdfPartitionWriter};
use rootlight_runtime::RuntimePaths;
use rootlight_storage::{
    IdentityVerifiedRdfBase, VerifiedRdfPartition, new_rdf_partition_set_envelopes,
};
use tempfile::TempDir;

use super::*;
use crate::FirstSliceService;

struct Fixture {
    service: FirstSliceService,
    snapshot: GenerationSnapshot,
    receipt: FirstSliceIndexReceipt,
    paths: RuntimePaths,
    _storage: TempDir,
    _root: TempDir,
}

impl Fixture {
    fn new() -> Self {
        let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        fs::write(root.path().join("left.nt"), "_:a <urn:p> \"left\" .\n").unwrap();
        fs::write(root.path().join("right.nt"), "_:a <urn:p> \"right\" .\n").unwrap();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .unwrap();
        paths.prepare_owner().unwrap();
        let cancellation = Cancellation::new();
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let receipt = service
            .index_repository(root.path(), &cancellation)
            .unwrap();
        let snapshot = service
            .loaded_generation_snapshot(receipt.generation)
            .unwrap();
        assert!(receipt.oracle_allocated_bytes > 0);
        assert_eq!(snapshot.document().files.len(), 2);
        assert_eq!(
            snapshot
                .document()
                .extensions
                .iter()
                .filter(|record| record.namespace == rootlight_ir::RDF_STATEMENT_NAMESPACE)
                .count(),
            2
        );
        Self {
            service,
            snapshot,
            receipt,
            paths,
            _storage: storage,
            _root: root,
        }
    }

    fn directory(&self) -> PathBuf {
        self.paths
            .state_dir()
            .join("first-slice/repositories")
            .join(self.receipt.repository.to_string())
            .join(self.receipt.generation.to_string())
    }

    fn expectation(&self) -> OracleRestoreExpectation {
        OracleRestoreExpectation {
            repository: self.receipt.repository,
            generation: self.receipt.generation,
            parent: self.receipt.parent,
            allocated_bytes: self.receipt.oracle_allocated_bytes,
        }
    }

    fn remove_recovery(&self) {
        for name in [
            RECOVERY_MANIFEST_FILENAME,
            RECOVERY_SNAPSHOT_FILENAME,
            RECOVERY_SNAPSHOT_GZIP_FILENAME,
            RECOVERY_SNAPSHOT_MESSAGEPACK_GZIP_FILENAME,
        ] {
            let path = self.directory().join(name);
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }
    }

    fn make_logical(&mut self, context: &GenerationContext<'_>) {
        let limits = IrLimits::default();
        let mut document = self.snapshot.document().clone();
        let records: Vec<_> = document
            .extensions
            .iter()
            .filter(|record| record.namespace == rootlight_ir::RDF_STATEMENT_NAMESPACE)
            .cloned()
            .collect();
        document
            .extensions
            .retain(|record| record.namespace != rootlight_ir::RDF_STATEMENT_NAMESPACE);
        let parent = IdentityVerifiedGeneration::verify_with_source_files(
            self.snapshot.metadata(),
            document,
            self.snapshot.source_files().clone(),
            &limits,
            &ExtensionSupport::default(),
            context,
        )
        .unwrap();
        let mut descriptors = Vec::new();
        for file in &parent.document().files {
            let records = records
                .iter()
                .filter(|record| {
                    record
                        .evidence
                        .source
                        .as_ref()
                        .is_some_and(|source| source.span().file() == file.id)
                })
                .cloned()
                .collect();
            let partition =
                VerifiedRdfPartition::verify(&parent, file.id, records, &limits, context).unwrap();
            descriptors.push(partition.descriptor().clone());
            drop(
                RdfPartitionWriter::create_in(&self.directory(), partition.descriptor(), context)
                    .unwrap()
                    .seal(partition, &limits, context)
                    .unwrap(),
            );
        }
        assert_eq!(descriptors.len(), 2);
        let mut document = parent.document().clone();
        document
            .extensions
            .extend(new_rdf_partition_set_envelopes(&parent, &descriptors, context).unwrap());
        let base = IdentityVerifiedRdfBase::verify(
            parent.metadata(),
            document,
            self.snapshot.source_files().clone(),
            &limits,
            context,
        )
        .unwrap();
        drop(parent);
        fs::remove_file(self.directory().join(ORACLE_FILENAME)).unwrap();
        OracleWriter::create_in(&self.directory())
            .unwrap()
            .write_rdf_base(base, context)
            .unwrap();
        let reader = OracleReader::open_in(&self.directory(), context).unwrap();
        assert_eq!(
            reader.stats().layout(),
            GenerationStatsLayout::CanonicalOracleWithRdfV1
        );
        self.receipt.oracle_allocated_bytes = reader.allocated_bytes(context).unwrap();
        drop(reader);
        let path = self.directory().join(MANIFEST_FILENAME);
        let mut manifest: DurableGenerationManifest =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest.receipt.oracle_allocated_bytes = self.receipt.oracle_allocated_bytes;
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        self.service
            .receipts
            .get_mut(&self.receipt.generation)
            .unwrap()
            .oracle_allocated_bytes = self.receipt.oracle_allocated_bytes;
        self.remove_recovery();
    }
}

fn tree_bytes(directory: &Path) -> BTreeMap<PathBuf, ContentHash> {
    let mut result = BTreeMap::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                result.insert(path.clone(), content_hash_bytes(&fs::read(path).unwrap()));
            }
        }
    }
    result
}

#[test]
fn logical_oracle_refuses_canonical_restore_and_lease_without_mutation() {
    let mut fixture = Fixture::new();
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    fixture.make_logical(&context);
    let directory = fixture.directory();
    let before = tree_bytes(&directory);
    let restore = |expectation| {
        restore_oracle_generation(
            &directory,
            expectation,
            fixture.snapshot.source_files().clone(),
            &context,
            &cancellation,
            None,
        )
    };
    assert_eq!(
        restore(fixture.expectation()).err().unwrap(),
        FirstSliceError::LogicalGenerationRequiresReader
    );
    for mismatch in 0..4 {
        let mut expected = fixture.expectation();
        match mismatch {
            0 => expected.repository = RepositoryId::from_bytes([0; 16]),
            1 => expected.generation = GenerationId::from_bytes([0; 20]),
            2 => expected.parent = Some(GenerationId::from_bytes([0; 20])),
            _ => expected.allocated_bytes -= 1,
        }
        assert_eq!(
            restore(expected).err().unwrap(),
            FirstSliceError::CatalogCorrupt
        );
    }
    {
        let mut cache = fixture.service.generation_cache.lock().unwrap();
        assert!(
            cache
                .generations
                .unload(fixture.receipt.generation)
                .unwrap()
        );
    }
    assert_eq!(
        fixture
            .service
            .generation_lease(fixture.receipt.generation, &cancellation)
            .err()
            .unwrap(),
        FirstSliceError::LogicalGenerationRequiresReader
    );
    let cache = fixture.service.generation_cache.lock().unwrap();
    assert!(!cache.generations.is_loaded(fixture.receipt.generation));
    assert!(cache.reservations.is_empty());
    drop(cache);
    assert_eq!(
        fixture
            .service
            .durable
            .as_ref()
            .unwrap()
            .restore_active_repository(fixture.receipt.repository, &cancellation)
            .err()
            .unwrap(),
        FirstSliceError::LogicalGenerationRequiresReader
    );
    assert!(directory.exists());
    assert_eq!(tree_bytes(&directory), before);
    assert!(OracleReader::open_in(&directory, &context).is_ok());
}

#[test]
fn canonical_oracle_preserves_materialization_admission_and_infallible_lease() {
    let fixture = Fixture::new();
    fixture.remove_recovery();
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());
    let (verified, allocated) = restore_oracle_generation(
        &fixture.directory(),
        fixture.expectation(),
        fixture.snapshot.source_files().clone(),
        &context,
        &cancellation,
        None,
    )
    .unwrap();
    assert_eq!(verified.document(), fixture.snapshot.document());
    assert_eq!(allocated, fixture.receipt.oracle_allocated_bytes);
    let mut reservation = crate::RestoredMemoryReservation::new(
        Arc::clone(&fixture.service.generation_cache),
        0,
        true,
    )
    .unwrap();
    let mut wrong_scope = fixture.expectation();
    wrong_scope.repository = RepositoryId::from_bytes([0; 16]);
    let refused = restore_capacity_tests::with_ceiling(1, || {
        restore_oracle_generation(
            &fixture.directory(),
            wrong_scope,
            fixture.snapshot.source_files().clone(),
            &context,
            &cancellation,
            Some((&mut reservation, 0)),
        )
    });
    assert!(matches!(
        refused,
        Err(FirstSliceError::GenerationMemoryLimit { .. })
    ));
    drop(reservation);
    assert!(
        fixture
            .service
            .generation_cache
            .lock()
            .unwrap()
            .generations
            .unload(fixture.receipt.generation)
            .unwrap()
    );
    let lease = fixture
        .service
        .generation_lease(fixture.receipt.generation, &cancellation)
        .unwrap();
    assert_eq!(lease.generation().document(), fixture.snapshot.document());
    assert_eq!(
        crate::map_catalog_error_kind(
            rootlight_catalog::CatalogErrorKind::PartitionedMaterializationRequired,
            &cancellation
        ),
        FirstSliceError::LogicalGenerationRequiresReader
    );
}
