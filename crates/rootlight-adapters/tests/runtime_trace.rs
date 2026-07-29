//! Public-boundary tests for the bounded local runtime-trace overlay.
//!
//! Fixtures prove exact generation binding, endpoint validation, deterministic
//! deduplication, source-free failures, quotas, and cooperative cancellation.

use std::error::Error;

use rootlight_adapters::{
    RUNTIME_TRACE_SCHEMA_VERSION, RuntimeTraceImportError, RuntimeTraceImportRequest,
    RuntimeTraceLimits, RuntimeTraceRelationKind, RuntimeTraceResource, import_runtime_trace,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{
    FactId, GenerationId, RepositoryId, SymbolId, content_hash, derive_repository,
};
use rootlight_ir::{
    AnalysisTier, EntityKind, EntityRecord, EntityVisibility, FactEvidence, NormalizedIrDocument,
    ProducerKind, RelationPredicate,
};
use serde_json::{Value, json};

#[test]
fn reordered_input_and_exact_duplicates_produce_one_canonical_overlay() {
    let fixture = Fixture::new();
    let calls = record("calls", fixture.first, fixture.second, 2);
    let writes = record("writes", fixture.second, fixture.first, 3);
    let first_trace = fixture.trace(vec![writes.clone(), calls.clone(), calls.clone()]);
    let second_trace = fixture.trace(vec![calls, writes]);

    let first = fixture.import(&first_trace).expect("first trace imports");
    let second = fixture
        .import(&second_trace)
        .expect("reordered trace imports");

    assert_eq!(first, second);
    assert_eq!(first.repository(), fixture.repository);
    assert_eq!(first.generation(), fixture.generation);
    assert_eq!(first.relations().len(), 2);
    assert_eq!(first.total_observations(), 5);
    assert_eq!(
        first.provenance().producer_kind(),
        ProducerKind::RuntimeTrace
    );
    assert_eq!(first.provenance().producer().name(), "fixture-tracer");
    assert_eq!(first.provenance().producer().version(), "1.2.3");
    assert_eq!(
        first.provenance().binary_digest(),
        content_hash(b"fixture-tracer-binary")
    );
    assert_eq!(
        first.provenance().trace_hash(),
        second.provenance().trace_hash()
    );
    assert_eq!(first.relations()[0].kind(), RuntimeTraceRelationKind::Calls);
    assert_eq!(
        first.relations()[0].kind().predicate(),
        RelationPredicate::Calls
    );
    assert_eq!(first.relations()[0].subject(), fixture.first);
    assert_eq!(first.relations()[0].object(), fixture.second);
    assert_eq!(first.relations()[0].count(), 2);
    assert_eq!(
        first.relations()[1].kind(),
        RuntimeTraceRelationKind::Writes
    );
}

#[test]
fn unknown_symbols_and_inconsistent_static_entities_are_rejected() {
    let fixture = Fixture::new();
    let unknown = SymbolId::from_bytes([99; 20]);
    let trace = fixture.trace(vec![record("calls", fixture.first, unknown, 1)]);
    assert_eq!(
        fixture.import(&trace),
        Err(RuntimeTraceImportError::UnknownSymbol)
    );

    let mut invalid = fixture.document.clone();
    invalid.entities[0].generation = GenerationId::from_bytes([45; 20]);
    let trace = fixture.trace(vec![record("calls", fixture.first, fixture.second, 1)]);
    assert_eq!(
        import(
            &trace,
            fixture.repository,
            fixture.generation,
            &invalid,
            RuntimeTraceLimits::default(),
            &Cancellation::new(),
        ),
        Err(RuntimeTraceImportError::InvalidGeneration)
    );

    let mut duplicate = fixture.document.clone();
    duplicate.entities.push(duplicate.entities[0].clone());
    assert_eq!(
        import(
            &trace,
            fixture.repository,
            fixture.generation,
            &duplicate,
            RuntimeTraceLimits::default(),
            &Cancellation::new(),
        ),
        Err(RuntimeTraceImportError::InvalidGeneration)
    );
}

#[test]
fn repository_and_generation_bindings_reject_stale_traces_and_documents() {
    let fixture = Fixture::new();
    let relation = record("calls", fixture.first, fixture.second, 1);
    let mut stale_trace = fixture.trace_value(vec![relation.clone()]);
    stale_trace["generation"] = Value::String(GenerationId::from_bytes([66; 20]).to_string());
    assert_eq!(
        fixture.import(&encode(stale_trace)),
        Err(RuntimeTraceImportError::StaleGeneration)
    );

    let other_repository = derive_repository(b"other-repository").id();
    let mut foreign_trace = fixture.trace_value(vec![relation.clone()]);
    foreign_trace["repository"] = Value::String(other_repository.to_string());
    assert_eq!(
        fixture.import(&encode(foreign_trace)),
        Err(RuntimeTraceImportError::RepositoryMismatch)
    );

    let mut stale_document = fixture.document.clone();
    stale_document.generation = GenerationId::from_bytes([77; 20]);
    let valid_trace = fixture.trace(vec![relation]);
    assert_eq!(
        import(
            &valid_trace,
            fixture.repository,
            fixture.generation,
            &stale_document,
            RuntimeTraceLimits::default(),
            &Cancellation::new(),
        ),
        Err(RuntimeTraceImportError::StaleGeneration)
    );
}

#[test]
fn cancellation_prevents_trace_materialization() {
    let fixture = Fixture::new();
    let trace = fixture.trace(vec![record("calls", fixture.first, fixture.second, 1)]);
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);

    let error = import(
        &trace,
        fixture.repository,
        fixture.generation,
        &fixture.document,
        RuntimeTraceLimits::default(),
        &cancellation,
    )
    .expect_err("cancelled import fails");
    let RuntimeTraceImportError::Cancelled(cancelled) = error else {
        panic!("expected cancellation error");
    };
    assert_eq!(cancelled.reason(), CancellationReason::ClientRequest);
}

#[test]
fn every_import_collection_and_count_is_bounded() {
    let fixture = Fixture::new();
    let one = record("calls", fixture.first, fixture.second, 2);
    let two = record("reads", fixture.second, fixture.first, 1);
    let trace = fixture.trace(vec![one.clone(), two]);

    assert_limit(
        &fixture,
        &trace,
        limits(1, 64 * 1024 * 1024, 10, 10, 10),
        RuntimeTraceResource::InputBytes,
    );
    assert_limit(
        &fixture,
        &trace,
        limits(16 * 1024 * 1024, 64 * 1024 * 1024, 1, 10, 10),
        RuntimeTraceResource::Records,
    );
    assert_limit(
        &fixture,
        &trace,
        limits(16 * 1024 * 1024, 64 * 1024 * 1024, 10, 1, 10),
        RuntimeTraceResource::KnownSymbols,
    );
    assert_limit(
        &fixture,
        &trace,
        limits(16 * 1024 * 1024, 64 * 1024 * 1024, 10, 10, 1),
        RuntimeTraceResource::Observations,
    );
    assert_limit(
        &fixture,
        &fixture.trace(vec![one]),
        limits(16 * 1024 * 1024, 1, 10, 10, 10),
        RuntimeTraceResource::CanonicalBytes,
    );

    assert_eq!(
        RuntimeTraceLimits::new(0, 1, 1, 1, 1),
        Err(RuntimeTraceImportError::InvalidLimit {
            resource: RuntimeTraceResource::InputBytes,
        })
    );
    assert_eq!(
        RuntimeTraceLimits::new(16 * 1024 * 1024 + 1, 64 * 1024 * 1024, 1, 1, 1,),
        Err(RuntimeTraceImportError::InvalidLimit {
            resource: RuntimeTraceResource::InputBytes,
        })
    );
}

#[test]
fn malformed_schema_fields_counts_and_conflicts_fail_closed() {
    let fixture = Fixture::new();
    assert_eq!(
        fixture.import(br#"{"schema":"broken""#),
        Err(RuntimeTraceImportError::MalformedTrace)
    );

    let mut unsupported = fixture.trace_value(Vec::new());
    unsupported["schema"] = Value::String("rootlight.runtime-trace/2".to_owned());
    assert_eq!(
        fixture.import(&encode(unsupported)),
        Err(RuntimeTraceImportError::UnsupportedSchema)
    );

    let mut unknown_top_level = fixture.trace_value(Vec::new());
    unknown_top_level["unexpected"] = json!(true);
    assert_eq!(
        fixture.import(&encode(unknown_top_level)),
        Err(RuntimeTraceImportError::MalformedTrace)
    );

    let mut unknown_record = record("calls", fixture.first, fixture.second, 1);
    unknown_record["unexpected"] = json!(true);
    assert_eq!(
        fixture.import(&fixture.trace(vec![unknown_record])),
        Err(RuntimeTraceImportError::MalformedTrace)
    );

    let invalid_kind = record("not_a_runtime_relation", fixture.first, fixture.second, 1);
    assert_eq!(
        fixture.import(&fixture.trace(vec![invalid_kind])),
        Err(RuntimeTraceImportError::MalformedTrace)
    );

    let zero = record("calls", fixture.first, fixture.second, 0);
    assert_eq!(
        fixture.import(&fixture.trace(vec![zero])),
        Err(RuntimeTraceImportError::InvalidObservationCount)
    );

    let first = record("calls", fixture.first, fixture.second, 1);
    let conflicting = record("calls", fixture.first, fixture.second, 2);
    assert_eq!(
        fixture.import(&fixture.trace(vec![first, conflicting])),
        Err(RuntimeTraceImportError::ConflictingRecord)
    );

    let too_many = record("calls", fixture.first, fixture.second, 1_000_000_000_001);
    assert_limit(
        &fixture,
        &fixture.trace(vec![too_many]),
        RuntimeTraceLimits::default(),
        RuntimeTraceResource::Observations,
    );
}

#[test]
fn errors_never_retain_trace_payloads_or_local_paths() {
    const SENTINEL: &str = r"C:\private\checkout\secret_payload";

    let fixture = Fixture::new();
    let mut malformed = fixture.trace_value(Vec::new());
    malformed[SENTINEL] = Value::String(SENTINEL.to_owned());
    let error = fixture
        .import(&encode(malformed))
        .expect_err("unknown field fails");
    assert_error_is_source_free(error, SENTINEL);

    let mut invalid_producer = fixture.trace_value(Vec::new());
    invalid_producer["producer"]["name"] = Value::String(SENTINEL.to_owned());
    let error = fixture
        .import(&encode(invalid_producer))
        .expect_err("path-shaped producer fails");
    assert_eq!(error, RuntimeTraceImportError::InvalidProducer);
    assert_error_is_source_free(error, SENTINEL);
}

fn assert_limit(
    fixture: &Fixture,
    trace: &[u8],
    limits: RuntimeTraceLimits,
    expected: RuntimeTraceResource,
) {
    let error = import(
        trace,
        fixture.repository,
        fixture.generation,
        &fixture.document,
        limits,
        &Cancellation::new(),
    )
    .expect_err("active limit rejects trace");
    let RuntimeTraceImportError::LimitExceeded { resource, .. } = error else {
        panic!("expected resource limit error, got {error:?}");
    };
    assert_eq!(resource, expected);
}

fn assert_error_is_source_free(error: RuntimeTraceImportError, sentinel: &str) {
    let rendered = format!("{error:?}\n{error}");
    assert!(!rendered.contains(sentinel));
    assert!(error.source().is_none());
}

fn limits(
    max_input_bytes: usize,
    max_canonical_bytes: usize,
    max_records: usize,
    max_known_symbols: usize,
    max_observations: u64,
) -> RuntimeTraceLimits {
    RuntimeTraceLimits::new(
        max_input_bytes,
        max_canonical_bytes,
        max_records,
        max_known_symbols,
        max_observations,
    )
    .expect("fixture limits stay under hard ceilings")
}

fn import(
    trace: &[u8],
    repository: RepositoryId,
    generation: GenerationId,
    document: &NormalizedIrDocument,
    limits: RuntimeTraceLimits,
    cancellation: &Cancellation,
) -> Result<rootlight_adapters::RuntimeTraceOverlay, RuntimeTraceImportError> {
    import_runtime_trace(
        trace,
        RuntimeTraceImportRequest::new(repository, generation, document, cancellation)
            .with_limits(limits),
    )
}

fn record(kind: &str, subject: SymbolId, object: SymbolId, count: u64) -> Value {
    json!({
        "kind": kind,
        "subject": subject,
        "object": object,
        "count": count,
    })
}

fn encode(value: Value) -> Vec<u8> {
    serde_json::to_vec(&value).expect("fixture JSON encodes")
}

struct Fixture {
    repository: RepositoryId,
    generation: GenerationId,
    first: SymbolId,
    second: SymbolId,
    document: NormalizedIrDocument,
}

impl Fixture {
    fn new() -> Self {
        let repository = derive_repository(b"runtime-trace-fixture").id();
        let generation = GenerationId::from_bytes([7; 20]);
        let first = SymbolId::from_bytes([1; 20]);
        let second = SymbolId::from_bytes([2; 20]);
        let mut document = NormalizedIrDocument::empty(repository, generation);
        document
            .entities
            .push(entity(repository, generation, first, "first"));
        document
            .entities
            .push(entity(repository, generation, second, "second"));
        Self {
            repository,
            generation,
            first,
            second,
            document,
        }
    }

    fn trace(&self, records: Vec<Value>) -> Vec<u8> {
        encode(self.trace_value(records))
    }

    fn trace_value(&self, records: Vec<Value>) -> Value {
        json!({
            "schema": RUNTIME_TRACE_SCHEMA_VERSION,
            "repository": self.repository,
            "generation": self.generation,
            "producer": {
                "name": "fixture-tracer",
                "version": "1.2.3",
                "configuration_hash": content_hash(b"fixture-tracer-config"),
                "binary_digest": content_hash(b"fixture-tracer-binary"),
            },
            "records": records,
        })
    }

    fn import(
        &self,
        trace: &[u8],
    ) -> Result<rootlight_adapters::RuntimeTraceOverlay, RuntimeTraceImportError> {
        import(
            trace,
            self.repository,
            self.generation,
            &self.document,
            RuntimeTraceLimits::default(),
            &Cancellation::new(),
        )
    }
}

fn entity(
    repository: RepositoryId,
    generation: GenerationId,
    id: SymbolId,
    name: &str,
) -> EntityRecord {
    EntityRecord {
        id,
        repository,
        generation,
        kind: EntityKind::Function,
        language: "rust".to_owned(),
        tier: AnalysisTier::TierC,
        canonical_name: name.to_owned(),
        display_name: name.to_owned(),
        qualified_name: name.to_owned(),
        container: None,
        visibility: EntityVisibility::Private,
        flags: Vec::new(),
        provenance: FactId::from_bytes([1; 20]),
        evidence: FactEvidence {
            source: None,
            derivation: Vec::new(),
        },
    }
}
