//! Upgrade controls for host restore-admission policy independently of languages.
//! A genuine prior whole-source fallback must receive new qualification even
//! when its source bytes, grammar identity and resolver revision are unchanged.

use super::*;
use rootlight_runtime::RuntimePaths;
use std::{cell::Cell, fs};

thread_local! {
    pub(super) static PREDECESSOR_POLICY: Cell<bool> = const { Cell::new(false) };
    pub(super) static FLAGS_PREDECESSOR_POLICY: Cell<bool> = const { Cell::new(false) };
    pub(super) static SYMBOL_PREDECESSOR_POLICY: Cell<bool> = const { Cell::new(false) };
    pub(super) static TEXT_PREDECESSOR_POLICY: Cell<bool> = const { Cell::new(false) };
}

fn predecessor<T>(
    policy: &'static std::thread::LocalKey<Cell<bool>>,
    operation: impl FnOnce() -> T,
) -> T {
    struct Reset(&'static std::thread::LocalKey<Cell<bool>>, bool);
    impl Drop for Reset {
        fn drop(&mut self) {
            self.0.with(|slot| slot.set(self.1));
        }
    }
    let _reset = Reset(policy, policy.with(|slot| slot.replace(true)));
    operation()
}

#[test]
fn prior_policy_whole_source_fallback_is_requalified_before_new_policy_noop_reuse() {
    assert_prior_policy_cache_requalification(&PREDECESSOR_POLICY);
}

#[test]
fn immediate_flag_policy_fence_requalifies_unchanged_capacity_fallback_before_noop_reuse() {
    assert_prior_policy_cache_requalification(&FLAGS_PREDECESSOR_POLICY);
}

#[test]
fn immediate_symbol_policy_fence_requalifies_unchanged_capacity_fallback_before_noop_reuse() {
    assert_prior_policy_cache_requalification(&SYMBOL_PREDECESSOR_POLICY);
}

#[test]
fn skipped_text_policy_fence_requalifies_unchanged_capacity_fallback_before_noop_reuse() {
    assert_prior_policy_cache_requalification(&TEXT_PREDECESSOR_POLICY);
}

fn assert_prior_policy_cache_requalification(policy: &'static std::thread::LocalKey<Cell<bool>>) {
    let storage = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let (source, ceiling) = durable::restore_policy_predecessor_fixture();
    fs::write(root.path().join("lib.rs"), &source).unwrap();
    fs::write(root.path().join("notes.txt"), "written source note\n").unwrap();
    let paths =
        RuntimePaths::new(storage.path().join("state"), storage.path().join("runtime")).unwrap();
    paths.prepare_owner().unwrap();
    let cancellation = Cancellation::new();
    let parser = first_slice_parser_provider_hash().unwrap();
    let resolver = content_hash(RESOLVER_BINARY_SEED);
    let current_policy = FirstSliceService::new(2)
        .unwrap()
        .provider_set_hash(FirstSliceIndexMode::Structural)
        .unwrap();
    let (old, old_policy) = predecessor(policy, || {
        assert!(policy.with(Cell::get));
        let mut service =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let old_policy = service
            .provider_set_hash(FirstSliceIndexMode::Structural)
            .unwrap();
        let old = durable::with_restore_test_ceiling(ceiling, || {
            service.index_repository(root.path(), &cancellation)
        })
        .unwrap();
        let old_snapshot = service.loaded_generation_snapshot(old.generation).unwrap();
        assert!(old_snapshot.document().entities.is_empty());
        assert!(old_snapshot.document().occurrences.is_empty());
        assert!(old_snapshot.document().relations.is_empty());
        assert_eq!(old_snapshot.file_count(), 2);
        assert!(
            old_snapshot
                .document()
                .diagnostics
                .iter()
                .any(
                    |diagnostic| diagnostic.code == SOURCE_FILE_FALLBACK_DIAGNOSTIC_CODE
                        && diagnostic
                            .message
                            .contains("complete semantic oracle restore")
                )
        );
        assert_eq!(old_snapshot.metadata().provider_set_hash(), old_policy);
        (old, old_policy)
    });
    assert_ne!(current_policy, old_policy);
    assert_eq!(parser, first_slice_parser_provider_hash().unwrap());
    assert_eq!(resolver, content_hash(RESOLVER_BINARY_SEED));
    assert_eq!(fs::read(root.path().join("lib.rs")).unwrap(), source);
    predecessor(policy, || {
        // The larger test-time ceiling alone does not revisit this unchanged
        // predecessor cache. The host policy fence must cause requalification.
        let mut unchanged =
            FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
        let noop = unchanged
            .index_repository(root.path(), &cancellation)
            .unwrap();
        assert_eq!(noop.generation, old.generation);
        assert!(
            unchanged
                .loaded_generation_snapshot(noop.generation)
                .unwrap()
                .document()
                .entities
                .is_empty()
        );
    });
    let mut current = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    assert_eq!(
        current.active_generation_for(old.repository),
        Some(old.generation)
    );
    let requalified = current
        .index_repository(root.path(), &cancellation)
        .unwrap();
    assert_ne!(requalified.generation, old.generation);
    assert_eq!(requalified.parent, Some(old.generation));
    let snapshot = current
        .loaded_generation_snapshot(requalified.generation)
        .unwrap();
    assert!(!snapshot.document().entities.is_empty());
    assert!(!snapshot.document().occurrences.is_empty());
    assert_eq!(snapshot.metadata().provider_set_hash(), current_policy);
    assert_eq!(
        current
            .index_repository(root.path(), &cancellation)
            .unwrap(),
        requalified
    );
    assert_eq!(fs::read(root.path().join("lib.rs")).unwrap(), source);
    drop(current);
    let mut reopened = FirstSliceService::new_durable(2, paths.state_dir(), &cancellation).unwrap();
    assert_eq!(
        reopened.active_generation_for(old.repository),
        Some(requalified.generation)
    );
    assert!(
        !reopened
            .loaded_generation_snapshot(requalified.generation)
            .unwrap()
            .document()
            .entities
            .is_empty()
    );
    assert_eq!(
        reopened
            .index_repository(root.path(), &cancellation)
            .unwrap(),
        requalified
    );
}
