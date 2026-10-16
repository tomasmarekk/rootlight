//! Preparation must reuse admitted parser work even when the committed cache is full.

use rootlight_adapter_sdk::{ParseCapabilities, ParseReport, ParseRequest, SyntaxFactSink};

use super::*;

struct CountedParser {
    inner: Arc<dyn StructuralPreflight>,
    calls: Arc<AtomicUsize>,
    cancel_after: Option<usize>,
    cache: Arc<Mutex<FirstSliceGenerationCache>>,
    charge_at_cancellation: Arc<AtomicUsize>,
}

impl ParseProvider for CountedParser {
    fn capabilities(&self) -> &ParseCapabilities {
        self.inner.capabilities()
    }

    fn parse(
        &self,
        request: &ParseRequest<'_>,
        sink: &mut dyn SyntaxFactSink,
        cancellation: &Cancellation,
    ) -> Result<ParseReport, AdapterError> {
        let count = self.calls.fetch_add(1, Ordering::Relaxed) + 1;
        let result = self.inner.parse(request, sink, cancellation);
        if self.cancel_after == Some(count) {
            self.charge_at_cancellation.store(
                usize::try_from(
                    self.cache
                        .lock()
                        .unwrap()
                        .prepared_artifact_reservation_bytes,
                )
                .unwrap(),
                Ordering::Relaxed,
            );
            cancellation.cancel(CancellationReason::ClientRequest);
        }
        result
    }
}

fn count_rust_parses(service: &mut FirstSliceService) -> Arc<AtomicUsize> {
    count_rust_parses_with_cancellation(service, None).0
}

fn count_rust_parses_with_cancellation(
    service: &mut FirstSliceService,
    cancel_after: Option<usize>,
) -> (Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let charge_at_cancellation = Arc::new(AtomicUsize::new(0));
    let parser = Arc::new(CountedParser {
        inner: Arc::clone(&service.structural_parsers["rust"]),
        calls: Arc::clone(&calls),
        cancel_after,
        cache: Arc::clone(&service.generation_cache),
        charge_at_cancellation: Arc::clone(&charge_at_cancellation),
    });
    let analyzer = TreeSitterAnalyzer::new_rust_structural(
        parser,
        ProducerIdentity::new(
            "counted-native-parser",
            "1.0",
            content_hash(b"test-context"),
        )
        .unwrap(),
        LanguageId::new("rust").unwrap(),
        "counted-rust-native",
        content_hash(b"counted-rust-native"),
    )
    .unwrap();
    service.analyzers.insert("rust".to_owned(), analyzer);
    (calls, charge_at_cancellation)
}

#[test]
fn preparation_reuses_complete_work_beyond_committed_cache_capacity() {
    let fixture = TempDir::new().unwrap();
    fs::write(
        fixture.path().join("a.rs"),
        "pub fn first(value: u32) -> u32 { value }\n",
    )
    .unwrap();
    fs::write(
        fixture.path().join("b.rs"),
        "pub fn second() { first(1); }\n",
    )
    .unwrap();
    let cancellation = deadline();
    let mut service = FirstSliceService::new(3).unwrap();
    service.structural_artifacts.maximum_bytes = 1;
    let calls = count_rust_parses(&mut service);
    let first = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();

    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(first.indexed_files, 2);
    assert_eq!(service.structural_artifacts.retained_bytes, 0);
    let evidence = service.incremental_evidence(first.generation).unwrap();
    assert!(!evidence.structural_cache_retained());
    assert_eq!(evidence.parsed_files(), 2);
    assert_eq!(evidence.reused_parser_artifacts(), 0);
    let files = service
        .loaded_generation_snapshot(first.generation)
        .unwrap()
        .document()
        .files
        .clone();
    for file in files {
        let reference = SourceRef::new(
            first.repository,
            first.generation,
            SourceSpan::new(file.id, 0, file.byte_length).unwrap(),
            file.content_hash,
            None,
        );
        let read = service
            .source_read_with_options_and_budget(
                first.generation,
                vec![reference.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0),
                FirstSliceBudget::default(),
                &cancellation,
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, reference);
        assert_eq!(
            read.data.chunks[0].bytes,
            fs::read(fixture.path().join(file.path)).unwrap()
        );
    }

    let no_op = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(no_op.generation, first.generation);
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    let mut baseline = FirstSliceService::new(3).unwrap();
    baseline.repositories = service.repositories.clone();
    let baseline_calls = count_rust_parses(&mut baseline);
    let rebuilt = baseline
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_eq!(baseline_calls.load(Ordering::Relaxed), 2);
    equivalence_snapshot(&service, first.generation, &cancellation)
        .compare_clean(
            &equivalence_snapshot(&baseline, rebuilt.generation, &cancellation),
            &cancellation,
        )
        .unwrap()
        .require_equivalent()
        .unwrap();
    assert_eq!(first.logical_snapshot, rebuilt.logical_snapshot);

    fs::write(
        fixture.path().join("a.rs"),
        "pub fn first(value: u32) -> u32 { value + 1 }\n",
    )
    .unwrap();
    calls.store(0, Ordering::Relaxed);
    let changed = service
        .index_repository(fixture.path(), &cancellation)
        .unwrap();
    assert_ne!(changed.generation, first.generation);
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!(service.structural_artifacts.retained_bytes, 0);
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .prepared_artifact_reservation_bytes,
        0
    );
}

fn prepared_fixture_artifact(service: &FirstSliceService) -> Arc<TreeSitterStructuralArtifact> {
    let fixture = TempDir::new().unwrap();
    let source = "pub fn retained(value: u32) -> u32 { value }\n";
    fs::write(fixture.path().join("lib.rs"), source).unwrap();
    let repository = derive_repository(b"prepared-artifact-ownership").id();
    let root = RepositoryRoot::open(repository, fixture.path()).unwrap();
    let snapshot = root
        .snapshot(
            &RelativePath::parse(Path::new("lib.rs")).unwrap(),
            source.len() as u64,
        )
        .unwrap();
    let request = structural_analysis_request(
        repository,
        GenerationId::from_bytes([41; 20]),
        &snapshot,
        false,
        "rust",
        "utf-8",
        &service.analysis_limits,
    )
    .unwrap();
    Arc::new(
        service.analyzers["rust"]
            .prepare_artifact(&request, &deadline())
            .unwrap(),
    )
}

#[test]
fn prepared_artifact_ownership_is_charged_until_its_payload_drops() {
    let service = FirstSliceService::new(2).unwrap();
    let artifact = prepared_fixture_artifact(&service);
    let file = artifact.file();
    let bytes = artifact.accounted_bytes() as u64;
    let mut prepared =
        crate::prepared_artifacts::PreparedArtifacts::new(Arc::clone(&service.generation_cache));
    prepared.retain(artifact, &deadline()).unwrap();
    let charge = service
        .generation_cache
        .lock()
        .unwrap()
        .prepared_artifact_reservation_bytes;
    assert!(charge > bytes);
    let owner = prepared.take(file).unwrap();
    assert!(prepared.take(file).is_none());
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        charge
    );
    drop(owner);
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
}

#[test]
fn prepared_artifact_refusal_preserves_the_existing_memory_limit() {
    let service = FirstSliceService::new(2).unwrap();
    let artifact = prepared_fixture_artifact(&service);
    let file = artifact.file();
    let pressure = GenerationMemoryReservation::new(
        Arc::clone(&service.generation_cache),
        GenerationId::from_bytes([42; 20]),
        MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES,
        PendingGenerationMemory::Reserved,
        false,
    )
    .unwrap();
    let mut prepared =
        crate::prepared_artifacts::PreparedArtifacts::new(Arc::clone(&service.generation_cache));
    prepared.retain(artifact, &deadline()).unwrap();
    assert!(prepared.take(file).is_none());
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .prepared_artifact_reservation_bytes,
        0
    );
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES
    );
    drop(pressure);
}

#[test]
fn required_document_admission_evicts_queued_prepared_artifacts() {
    let service = FirstSliceService::new(2).unwrap();
    let artifact = prepared_fixture_artifact(&service);
    let file = artifact.file();
    let mut prepared =
        crate::prepared_artifacts::PreparedArtifacts::new(Arc::clone(&service.generation_cache));
    prepared.retain(artifact, &deadline()).unwrap();
    let charge = service
        .generation_cache
        .lock()
        .unwrap()
        .prepared_artifact_reservation_bytes;
    let generation = GenerationId::from_bytes([43; 20]);
    let document =
        NormalizedIrDocument::empty(derive_repository(b"required-document").id(), generation);
    let document_bytes = normalized_document_serialized_bytes(&document).unwrap();
    assert!(charge > document_bytes);
    let mut reservation = GenerationMemoryReservation::new(
        Arc::clone(&service.generation_cache),
        generation,
        0,
        PendingGenerationMemory::Reserved,
        false,
    )
    .unwrap();
    let pressure = GenerationMemoryReservation::new(
        Arc::clone(&service.generation_cache),
        GenerationId::from_bytes([44; 20]),
        MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES - charge - document_bytes / 2,
        PendingGenerationMemory::Reserved,
        false,
    )
    .unwrap();
    let mut admission = generation_admission::GenerationAdmission::new(0).unwrap();
    let actual = prepared
        .admit_document(
            &mut admission,
            &document,
            &mut reservation,
            false,
            &deadline(),
        )
        .unwrap();
    assert_eq!(actual, document_bytes);
    assert_eq!(admission.charged_bytes(), document_bytes);
    assert!(prepared.take(file).is_none());
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .prepared_artifact_reservation_bytes,
        0
    );
    drop(pressure);
    drop(reservation);
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
}

#[test]
fn optional_preparation_yields_before_unloading_a_resident_generation() {
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("lib.rs"), "pub fn resident() {}\n").unwrap();
    let mut service = FirstSliceService::new(2).unwrap();
    let current = service
        .index_repository(fixture.path(), &deadline())
        .unwrap();
    let resident_bytes = service
        .generation_cache
        .lock()
        .unwrap()
        .resident_logical_charge_bytes()
        .unwrap();
    let artifact = prepared_fixture_artifact(&service);
    let file = artifact.file();
    let mut prepared =
        crate::prepared_artifacts::PreparedArtifacts::new(Arc::clone(&service.generation_cache));
    prepared.retain(artifact, &deadline()).unwrap();
    let charge = service
        .generation_cache
        .lock()
        .unwrap()
        .prepared_artifact_reservation_bytes;
    let generation = GenerationId::from_bytes([45; 20]);
    let document = NormalizedIrDocument::empty(current.repository, generation);
    let document_bytes = normalized_document_serialized_bytes(&document).unwrap();
    assert!(charge > document_bytes && resident_bytes > document_bytes);
    let mut reservation = GenerationMemoryReservation::new(
        Arc::clone(&service.generation_cache),
        generation,
        0,
        PendingGenerationMemory::Reserved,
        false,
    )
    .unwrap();
    let pressure = GenerationMemoryReservation::new(
        Arc::clone(&service.generation_cache),
        GenerationId::from_bytes([46; 20]),
        MAX_FIRST_SLICE_GENERATION_MEMORY_BYTES - resident_bytes - charge - document_bytes / 2,
        PendingGenerationMemory::Reserved,
        false,
    )
    .unwrap();
    let mut admission = generation_admission::GenerationAdmission::new(0).unwrap();
    assert_eq!(
        prepared
            .admit_document(
                &mut admission,
                &document,
                &mut reservation,
                true,
                &deadline()
            )
            .unwrap(),
        document_bytes
    );
    assert!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .generations
            .is_loaded(current.generation)
    );
    assert!(prepared.take(file).is_none());
    drop(pressure);
    drop(reservation);
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
}

#[test]
fn cancelled_preparation_releases_previously_admitted_artifacts() {
    let fixture = TempDir::new().unwrap();
    fs::write(fixture.path().join("a.rs"), "pub fn first() {}\n").unwrap();
    fs::write(fixture.path().join("b.rs"), "pub fn second() {}\n").unwrap();
    let cancellation = deadline();
    let mut service = FirstSliceService::new(2).unwrap();
    service.structural_artifacts.maximum_bytes = 1;
    let (calls, charge_at_cancellation) =
        count_rust_parses_with_cancellation(&mut service, Some(2));
    assert!(matches!(
        service.index_repository(fixture.path(), &cancellation),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert!(charge_at_cancellation.load(Ordering::Relaxed) > 0);
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .prepared_artifact_reservation_bytes,
        0
    );
    assert_eq!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reserved_memory_bytes()
            .unwrap(),
        0
    );
    assert!(service.active_by_repository.is_empty());
}
