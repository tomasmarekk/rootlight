//! Prospective Oracle space refusal may retain exact sources without graph credit.

use std::{cell::RefCell, fmt::Write as _, fs};

use rootlight_cancel::CancellationReason;
use rootlight_runtime::RuntimePaths;
use rootlight_source::SourceEncoding;
use tempfile::TempDir;

use super::*;

#[derive(Clone)]
struct Space {
    available: u64,
    after_refusal: Option<u64>,
    refused: bool,
    cancel_on_refusal: Option<Cancellation>,
}

thread_local! {
    static SPACE: RefCell<Option<Space>> = const { RefCell::new(None) };
}

// Narrow the real free-space observation; never synthesize a successful admission.
pub(super) fn available_bytes(actual: u64, required: u64, minimum_free: u64) -> u64 {
    SPACE.with(|value| {
        let mut value = value.borrow_mut();
        let Some(state) = value.as_mut() else {
            return actual;
        };
        let ceiling = if state.refused {
            state.after_refusal.unwrap_or(state.available)
        } else {
            state.available
        };
        let available = actual.min(ceiling);
        if required > available.saturating_sub(minimum_free) {
            state.refused = true;
            if let Some(cancel) = &state.cancel_on_refusal {
                cancel.cancel(CancellationReason::ClientRequest);
            }
        }
        available
    })
}

fn with_space<T>(available: u64, cancel: Option<Cancellation>, operation: impl FnOnce() -> T) -> T {
    with_space_after_refusal(available, None, cancel, operation)
}

fn with_space_after_refusal<T>(
    available: u64,
    after_refusal: Option<u64>,
    cancel: Option<Cancellation>,
    operation: impl FnOnce() -> T,
) -> T {
    struct Reset(Option<Space>);
    impl Drop for Reset {
        fn drop(&mut self) {
            SPACE.with(|value| *value.borrow_mut() = self.0.take());
        }
    }
    let _reset = Reset(SPACE.with(|value| {
        value.replace(Some(Space {
            available,
            after_refusal,
            refused: false,
            cancel_on_refusal: cancel,
        }))
    }));
    operation()
}

struct Fixture {
    _storage: TempDir,
    root: TempDir,
    paths: RuntimePaths,
    sources: Vec<(String, Vec<u8>)>,
}

impl Fixture {
    fn new() -> Self {
        let storage = crate::tests::durable_test_tempdir();
        let root = crate::tests::durable_test_tempdir();
        let paths = RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime"))
            .unwrap();
        paths.prepare_owner().unwrap();
        let mut sources = Vec::new();
        for file in 0..2 {
            let mut text = String::from("// source scalar: λ; ababa\n");
            for item in 0..256 {
                writeln!(text, "pub fn written_{file}_{item}() -> u32 {{ {item} }}").unwrap();
            }
            let bytes = if file == 0 {
                text.into_bytes()
            } else {
                [b"\xef\xbb\xbf".as_slice(), text.as_bytes()].concat()
            };
            let name = format!("member_{file}.rs");
            fs::write(root.path().join(&name), &bytes).unwrap();
            sources.push((name, bytes));
        }
        Self {
            _storage: storage,
            root,
            paths,
            sources,
        }
    }

    fn service(&self) -> FirstSliceService {
        FirstSliceService::new_durable(4, self.paths.state_dir(), &Cancellation::new()).unwrap()
    }

    fn changed(&mut self) {
        self.sources[0]
            .1
            .extend_from_slice(b"// next physical revision\n");
        fs::write(
            self.root.path().join(&self.sources[0].0),
            &self.sources[0].1,
        )
        .unwrap();
    }

    fn source_bytes(&self) -> u64 {
        self.sources
            .iter()
            .map(|(_, bytes)| u64::try_from(bytes.len()).unwrap())
            .sum()
    }
}

fn checked_window(
    service: &FirstSliceService,
    fixture: &Fixture,
    full: &FirstSliceIndexReceipt,
) -> u64 {
    let source_floor = service
        .durable_staging_reservation(fixture.source_bytes())
        .unwrap();
    assert!(full.estimated_disk_bytes > source_floor);
    let limit = source_floor
        .checked_add((full.estimated_disk_bytes - source_floor) / 2)
        .unwrap();
    assert!(
        source_floor < limit && limit < full.estimated_disk_bytes,
        "real full output must exceed the source-only staging floor"
    );
    limit
        .checked_add(service.storage_policy.minimum_free_disk_bytes)
        .unwrap()
}

fn assert_sources(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    sources: &[(String, Vec<u8>)],
) {
    let lease = service
        .generation_lease(receipt.generation, &Cancellation::new())
        .unwrap();
    let generation = lease.generation();
    assert_eq!(generation.file_count(), sources.len());
    for (path, bytes) in sources {
        let file = generation
            .document()
            .files
            .iter()
            .chain(
                generation
                    .source_files()
                    .entries()
                    .iter()
                    .map(|entry| entry.file()),
            )
            .find(|file| &file.path == path)
            .unwrap();
        assert_eq!(file.content_hash, content_hash(bytes));
        assert_eq!(file.byte_length, u64::try_from(bytes.len()).unwrap());
        let source = file.evidence.source.clone().unwrap();
        assert_eq!(source.generation(), receipt.generation);
        assert_eq!(source.repository(), receipt.repository);
        let read = service
            .source_read_with_options_and_budget(
                receipt.generation,
                vec![source.clone()],
                SourceReadOptions::new()
                    .with_context_lines_before(0)
                    .with_context_lines_after(0)
                    .with_encoding(SourceEncoding::Bytes),
                FirstSliceBudget::default(),
                &Cancellation::new(),
            )
            .unwrap();
        assert_eq!(read.data.chunks.len(), 1);
        assert_eq!(read.data.chunks[0].reference, source);
        assert_eq!(&read.data.chunks[0].bytes, bytes);
    }
}

fn assert_source_only(
    service: &FirstSliceService,
    receipt: &FirstSliceIndexReceipt,
    expected: usize,
) {
    assert_eq!(receipt.entities, 0);
    let lease = service
        .generation_lease(receipt.generation, &Cancellation::new())
        .unwrap();
    let generation = lease.generation();
    let document = generation.document();
    assert!(
        document.entities.is_empty()
            && document.occurrences.is_empty()
            && document.relations.is_empty()
    );
    for domain in [
        IrFactDomain::Entities,
        IrFactDomain::Occurrences,
        IrFactDomain::Relations,
    ] {
        assert!(
            document
                .coverage_records
                .iter()
                .any(|record| record.domain == domain && record.status == CoverageStatus::Unknown)
        );
    }
    let envelope = document
        .extensions
        .iter()
        .find(|entry| entry.namespace == rootlight_ir::AGGREGATE_SOURCE_RECOVERY_NAMESPACE)
        .unwrap();
    assert_eq!(envelope.version, "2");
    let cause = rootlight_ir::decode_aggregate_source_recovery_envelope(envelope).unwrap();
    let value = serde_json::to_value(cause).unwrap();
    assert_eq!(value["stage"], "oracle_output_reservation");
    assert_eq!(value["resource"], "oracle_filesystem_reservation_bytes");
    assert!(cause.observed > cause.limit);
    assert_eq!(cause.members, u64::try_from(expected).unwrap());
    assert_eq!(cause.derived_members, 0);
    assert_eq!(
        cause.source_inventory,
        generation.metadata().manifest_hash()
    );
    let file = document.files.first().unwrap();
    let public = service
        .source_file_recovery_until(receipt.generation, file.id, &Cancellation::new())
        .unwrap()
        .unwrap();
    assert_eq!(public.cause, cause);
    assert!(
        document
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("modeled bytes")
                && diagnostic.message.contains("unknown analysis"))
    );
}

#[test]
fn prospective_oracle_refusal_re_admits_exact_sources_with_a_typed_unknown_generation() {
    let mut fixture = Fixture::new();
    let mut service = fixture.service();
    let first = service
        .index_repository(fixture.root.path(), &Cancellation::new())
        .unwrap();
    assert!(first.entities > 0);
    let limit = checked_window(&service, &fixture, &first);
    fixture.changed();
    let prepared = with_space(limit, None, || {
        service.prepare_repository(fixture.root.path(), &Cancellation::new())
    })
    .unwrap();
    assert_eq!(
        service.active_generation_for(first.repository),
        Some(first.generation)
    );
    let next = service
        .publish_prepared(prepared, &Cancellation::new())
        .unwrap();
    assert_eq!(next.parent, Some(first.generation));
    assert!(
        next.estimated_disk_bytes
            >= service
                .durable_staging_reservation(fixture.source_bytes())
                .unwrap()
    );
    assert!(next.estimated_disk_bytes <= limit - service.storage_policy.minimum_free_disk_bytes);
    assert!(next.estimated_disk_bytes < first.estimated_disk_bytes);
    assert_source_only(&service, &next, fixture.sources.len());
    assert_sources(&service, &next, &fixture.sources);
    let located = service
        .code_locate(
            next.generation,
            "ababa".to_owned(),
            LocateMode::Text,
            10,
            0,
            &Cancellation::new(),
        )
        .unwrap();
    assert_eq!(located.data.hits.len(), 2);
    assert!(located.data.hits.iter().all(|hit| hit.symbol.is_none()));
    assert_eq!(
        service
            .index_repository(fixture.root.path(), &Cancellation::new())
            .unwrap(),
        next
    );
    assert_source_only(&service, &next, fixture.sources.len());
    drop(service);
    for (path, _) in &fixture.sources {
        fs::remove_file(fixture.root.path().join(path)).unwrap();
    }
    let restored = fixture.service();
    assert_sources(&restored, &next, &fixture.sources);
    assert_source_only(&restored, &next, fixture.sources.len());
}

#[test]
fn sufficient_space_keeps_normal_analysis_and_clean_rebuild_can_replace_source_only_state() {
    let mut fixture = Fixture::new();
    let mut service = fixture.service();
    let first = service
        .index_repository(fixture.root.path(), &Cancellation::new())
        .unwrap();
    let limit = checked_window(&service, &fixture, &first);
    fixture.changed();
    let degraded = with_space(limit, None, || {
        service.index_repository(fixture.root.path(), &Cancellation::new())
    })
    .unwrap();
    assert_source_only(&service, &degraded, fixture.sources.len());
    let full = service
        .index_repository_with_options(
            fixture.root.path(),
            FirstSliceIndexOptions::clean_rebuild(FirstSliceIndexMode::Structural),
            &Cancellation::new(),
        )
        .unwrap();
    assert!(full.entities > 0);
    assert_ne!(full.generation, degraded.generation);
    assert_sources(&service, &full, &fixture.sources);
    let lease = service
        .generation_lease(full.generation, &Cancellation::new())
        .unwrap();
    assert!(
        !lease
            .generation()
            .document()
            .extensions
            .iter()
            .any(|entry| entry.namespace == rootlight_ir::AGGREGATE_SOURCE_RECOVERY_NAMESPACE)
    );
}

#[test]
fn deep_and_caller_tightened_analysis_do_not_convert_storage_refusal_into_recovery() {
    for mode in [FirstSliceIndexMode::Deep, FirstSliceIndexMode::Structural] {
        let mut fixture = Fixture::new();
        let mut service = fixture.service();
        let first = service
            .index_repository(fixture.root.path(), &Cancellation::new())
            .unwrap();
        let limit = checked_window(&service, &fixture, &first);
        fixture.changed();
        if mode == FirstSliceIndexMode::Structural {
            let mut limits = service.analysis_limits.clone();
            let mut ir = limits.ir().clone();
            ir.max_entities -= 1;
            let project = limits.project();
            limits = AnalysisLimits::new(
                limits.max_source_bytes(),
                limits.max_syntax_nodes(),
                limits.max_syntax_depth(),
                limits.max_embedded_ranges(),
                limits.max_reported_memory_bytes(),
                limits.syntax_stream().clone(),
                limits.ir_stream().clone(),
                ir,
            )
            .unwrap();
            if let Some(project) = project {
                limits = limits.with_project_limits(project);
            }
            service.analysis_limits = limits;
        }
        let result = with_space(limit, None, || {
            service.prepare_repository_with_mode(fixture.root.path(), mode, &Cancellation::new())
        });
        assert!(matches!(
            result,
            Err(FirstSliceError::StorageResourceExhausted {
                scope: FirstSliceStorageScope::Filesystem,
                ..
            })
        ));
        assert_eq!(
            service.active_generation_for(first.repository),
            Some(first.generation)
        );
    }
}

#[test]
fn source_insufficient_and_cancelled_retries_preserve_the_prior_generation_and_reservations() {
    let mut fixture = Fixture::new();
    let mut service = fixture.service();
    let first = service
        .index_repository(fixture.root.path(), &Cancellation::new())
        .unwrap();
    let old_sources = fixture.sources.clone();
    let limit = checked_window(&service, &fixture, &first);
    fixture.changed();
    let receipts = service.receipts.len();
    let result = with_space_after_refusal(limit, Some(0), None, || {
        service.prepare_repository(fixture.root.path(), &Cancellation::new())
    });
    assert!(
        matches!(result, Err(FirstSliceError::StorageResourceExhausted {
        scope: FirstSliceStorageScope::Filesystem, required_bytes, limit_bytes: 0, ..
    }) if required_bytes < first.estimated_disk_bytes)
    );
    let result = with_space(0, None, || {
        service.prepare_repository(fixture.root.path(), &Cancellation::new())
    });
    assert!(matches!(
        result,
        Err(FirstSliceError::StorageResourceExhausted {
            scope: FirstSliceStorageScope::Filesystem,
            ..
        })
    ));
    let cancelled = Cancellation::new();
    let result = with_space(limit, Some(cancelled.clone()), || {
        service.prepare_repository(fixture.root.path(), &cancelled)
    });
    assert!(matches!(
        result,
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(
        service.active_generation_for(first.repository),
        Some(first.generation)
    );
    assert_eq!(service.receipts.len(), receipts);
    assert_sources(&service, &first, &old_sources);
    assert_eq!(
        service
            .durable
            .as_ref()
            .unwrap()
            .storage_inventory()
            .unwrap()
            .inflight_catalog_reservation_bytes,
        0
    );
    assert!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reservations
            .is_empty()
    );
    let prepared = with_space(limit, None, || {
        service.prepare_repository(fixture.root.path(), &Cancellation::new())
    })
    .unwrap();
    let cancelled = Cancellation::new();
    cancelled.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        service.publish_prepared(prepared, &cancelled),
        Err(FirstSliceError::Cancelled(
            CancellationReason::ClientRequest
        ))
    ));
    assert_eq!(
        service.active_generation_for(first.repository),
        Some(first.generation)
    );
    assert_eq!(service.receipts.len(), receipts);
    assert_sources(&service, &first, &old_sources);
    assert_eq!(
        service
            .durable
            .as_ref()
            .unwrap()
            .storage_inventory()
            .unwrap()
            .inflight_catalog_reservation_bytes,
        0
    );
    assert!(
        service
            .generation_cache
            .lock()
            .unwrap()
            .reservations
            .is_empty()
    );
}
