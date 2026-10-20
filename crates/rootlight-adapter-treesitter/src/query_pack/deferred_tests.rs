//! Cold query-registry ownership, cancellation and failure contracts.
//! Each fixture owns fresh cells so unrelated eager tests cannot mask accidental
//! compilation, repeated work or failure-cache behavior.

use std::sync::{
    Barrier,
    atomic::{AtomicUsize, Ordering},
};

use rootlight_cancel::CancellationReason;

use super::*;

#[test]
fn cold_catalogue_initializes_only_the_requested_factory() {
    let registry = QueryPackRegistry::catalogue();
    assert_eq!(registry.len(), 77);
    assert_eq!(registry.initialized_count(), 0);
    assert_eq!(registry.initialized_pattern_count(), 0);
    for family in [
        GrammarFamily::Python,
        GrammarFamily::TypeScript,
        GrammarFamily::Markdown,
    ] {
        assert!(registry.contains(family));
    }
    assert_eq!(registry.initialized_count(), 0);

    let cancellation = Cancellation::new();
    let python = registry
        .get_for_source(GrammarFamily::Python, "main.py", &cancellation)
        .unwrap();
    assert_eq!(registry.initialized_count(), 1);
    assert_eq!(
        registry.initialized_pattern_count(),
        python.identity_query.pattern_count()
    );
    assert!(std::ptr::eq(
        python,
        registry
            .get_for_source(GrammarFamily::Python, "other.py", &cancellation)
            .unwrap()
    ));
    assert_eq!(registry.initialized_count(), 1);

    let tsx = registry
        .get_for_source(GrammarFamily::TypeScript, "component.tsx", &cancellation)
        .unwrap();
    assert_eq!(registry.initialized_count(), 2);
    assert!(
        registry
            .entry(GrammarFamily::TypeScript)
            .unwrap()
            .compiled
            .get()
            .is_none()
    );
    assert!(
        registry
            .entry(GrammarFamily::JavaScript)
            .unwrap()
            .compiled
            .get()
            .is_none()
    );
    assert!(std::ptr::eq(
        tsx,
        registry
            .get_for_native(
                GrammarFamily::TypeScript,
                GrammarFamily::JavaScript,
                &cancellation
            )
            .unwrap()
    ));
    registry
        .get_for_source(GrammarFamily::TypeScript, "module.ts", &cancellation)
        .unwrap();
    assert_eq!(registry.initialized_count(), 3);
    assert!(
        registry
            .entry(GrammarFamily::Markdown)
            .unwrap()
            .compiled
            .get()
            .is_none()
    );
}

#[test]
fn simultaneous_callers_compile_one_shared_query() {
    let entry = QueryPackEntry::new(
        GrammarFamily::Python,
        GrammarFamily::Python,
        include_str!("../../queries/python.scm"),
    );
    let calls = AtomicUsize::new(0);
    let barrier = Barrier::new(4);
    std::thread::scope(|scope| {
        let handles: Vec<_> = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    entry
                        .load_with(&Cancellation::new(), || {
                            calls.fetch_add(1, Ordering::SeqCst);
                            QueryPack::compile(entry.family, entry.source)
                        })
                        .unwrap()
                })
            })
            .collect();
        let packs: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        assert!(packs.iter().all(|pack| std::ptr::eq(*pack, packs[0])));
    });
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[test]
fn failed_compilation_is_cached_and_remains_a_provider_failure() {
    let entry = QueryPackEntry::new(
        GrammarFamily::Python,
        GrammarFamily::Python,
        "(invalid_missing_node) @root",
    );
    for _ in 0..2 {
        let error = match entry.load(&Cancellation::new()) {
            Ok(_) => panic!("invalid built-in query must fail"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            QueryPackError::Compilation {
                family: GrammarFamily::Python,
                native: GrammarFamily::Python
            }
        ));
        assert!(
            matches!(AdapterError::from(error), AdapterError::ProviderFailed { code } if code.as_str() == "query-pack-compile")
        );
    }
    assert!(matches!(
        entry.compiled.get(),
        Some(Err(GrammarFamily::Python))
    ));
    let cached = entry.load_with(&Cancellation::new(), || {
        panic!("failed static query must not compile again")
    });
    assert!(matches!(cached, Err(QueryPackError::Compilation { .. })));
}

#[test]
fn missing_factory_is_distinct_from_compilation_and_does_not_initialize() {
    let registry = QueryPackRegistry::catalogue();
    let error = match registry.get_for_native(
        GrammarFamily::Python,
        GrammarFamily::Rust,
        &Cancellation::new(),
    ) {
        Ok(_) => panic!("unregistered query factory must fail"),
        Err(error) => error,
    };
    assert!(matches!(
        error,
        QueryPackError::Missing {
            family: GrammarFamily::Python,
            native: GrammarFamily::Rust
        }
    ));
    assert!(
        matches!(AdapterError::from(error), AdapterError::ProviderFailed { code } if code.as_str() == "query-pack-missing")
    );
    assert_eq!(registry.initialized_count(), 0);
}

#[test]
fn cancellation_before_compilation_leaves_the_cell_uninitialized() {
    let entry = QueryPackEntry::new(
        GrammarFamily::Python,
        GrammarFamily::Python,
        include_str!("../../queries/python.scm"),
    );
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::Shutdown);
    let result = entry.load_with(&cancellation, || {
        panic!("cancelled caller must not initialize")
    });
    assert!(
        matches!(result, Err(QueryPackError::Cancelled(cancelled)) if cancelled.reason() == CancellationReason::Shutdown)
    );
    assert!(entry.compiled.get().is_none());
    assert!(entry.load(&Cancellation::new()).is_ok());
}

#[test]
fn cancellation_during_compilation_wins_without_poisoning_shared_success() {
    let entry = QueryPackEntry::new(
        GrammarFamily::Python,
        GrammarFamily::Python,
        include_str!("../../queries/python.scm"),
    );
    let cancellation = Cancellation::new();
    let result = entry.load_with(&cancellation, || {
        let pack = QueryPack::compile(entry.family, entry.source);
        cancellation.cancel(CancellationReason::Shutdown);
        pack
    });
    assert!(
        matches!(result, Err(QueryPackError::Cancelled(cancelled)) if cancelled.reason() == CancellationReason::Shutdown)
    );
    assert!(matches!(entry.compiled.get(), Some(Ok(_))));
    assert!(
        entry
            .load_with(&Cancellation::new(), || panic!(
                "successful query is retained"
            ))
            .is_ok()
    );
}

#[test]
fn cancellation_after_shared_initialization_is_still_observed() {
    let entry = QueryPackEntry::new(
        GrammarFamily::Python,
        GrammarFamily::Python,
        include_str!("../../queries/python.scm"),
    );
    entry.load(&Cancellation::new()).unwrap();
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::Shutdown);
    assert!(
        matches!(entry.load(&cancellation), Err(QueryPackError::Cancelled(cancelled)) if cancelled.reason() == CancellationReason::Shutdown)
    );
}
