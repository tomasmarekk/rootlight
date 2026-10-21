//! Ownership controls for the native callback bridge.
//! Tests invoke the actual retained C status unit without production test knobs.

use super::*;
use std::sync::{Arc, atomic::{AtomicBool, Ordering}};

unsafe extern "C" {
    fn rootlight_dockerfile_scope_refuse(status: std::ffi::c_int);
    fn rootlight_dockerfile_scope_status(token: u64) -> std::ffi::c_int;
}

fn cancelled(flag: &Arc<AtomicBool>) -> bool { flag.load(Ordering::Relaxed) }

#[test]
fn moved_guard_owns_callback_until_finish() {
    let flag = Arc::new(AtomicBool::new(false));
    let scope = ScannerScope::begin(Arc::clone(&flag), cancelled).unwrap();
    let moved = std::hint::black_box(scope);
    flag.store(true, Ordering::Relaxed);
    assert_eq!(moved.finish(), Err(ScannerRefusal::Cancelled));
    ScannerScope::begin(Arc::new(AtomicBool::new(false)), cancelled).unwrap().finish().unwrap();
}

#[test]
fn already_cancelled_begin_detaches_its_context() {
    let flag = Arc::new(AtomicBool::new(true));
    assert_eq!(ScannerScope::begin(Arc::clone(&flag), cancelled).unwrap_err(), ScannerRefusal::Cancelled);
    assert_eq!(Arc::strong_count(&flag), 1);
    ScannerScope::begin((), |_| false).unwrap().finish().unwrap();
}

#[test]
fn callback_panic_is_strict_and_does_not_unwind_through_c() {
    assert_eq!(ScannerScope::begin((), |_| panic!("callback control")).unwrap_err(), ScannerRefusal::CallbackPanicked);
    ScannerScope::begin((), |_| false).unwrap().finish().unwrap();
}

#[test]
fn native_first_refusal_cannot_be_cleared_by_a_later_status() {
    let scope = ScannerScope::begin((), |_| false).unwrap();
    // SAFETY: this test owns the active native scope on this thread.
    unsafe { rootlight_dockerfile_scope_refuse(2); rootlight_dockerfile_scope_refuse(8); }
    assert_eq!(scope.finish(), Err(ScannerRefusal::Capacity));
    ScannerScope::begin((), |_| false).unwrap().finish().unwrap();
}

#[test]
fn drop_closes_scope_during_rust_unwind() {
    let flag = Arc::new(AtomicBool::new(false));
    let result = std::panic::catch_unwind({ let flag = Arc::clone(&flag); move || {
        let _scope = ScannerScope::begin(flag, cancelled).unwrap();
        panic!("owned scope unwind control");
    }});
    assert!(result.is_err());
    assert_eq!(Arc::strong_count(&flag), 1);
    ScannerScope::begin((), |_| false).unwrap().finish().unwrap();
}

#[test]
fn forgotten_guard_leaves_valid_owned_storage_and_refuses_reentry() {
    std::thread::spawn(|| {
        let flag = Arc::new(AtomicBool::new(false));
        let scope = ScannerScope::begin(Arc::clone(&flag), cancelled).unwrap();
        let token = scope.token;
        std::mem::forget(scope);
        flag.store(true, Ordering::Relaxed);
        // SAFETY: forgotten scope deliberately leaks the initialized Vec and
        // cloned flag; its callback remains valid until this worker exits.
        assert_eq!(unsafe { rootlight_dockerfile_scope_status(token) }, 7);
        assert_eq!(ScannerScope::begin((), |_| false).unwrap_err(), ScannerRefusal::ScopeConflict);
        assert_eq!(Arc::strong_count(&flag), 2);
    }).join().unwrap();
}
