//! Exercise the owned callback contract independently of product admission.
//! Native scanner geometry and wire checks live in the separate C controls.

use super::*;
use std::cell::RefCell;

struct Probe(Rc<Cell<bool>>);

impl CancellationProbe for Probe {
    fn is_cancelled(&self) -> bool {
        self.0.get()
    }
}

struct PanicProbe;

impl CancellationProbe for PanicProbe {
    fn is_cancelled(&self) -> bool {
        panic!("injected callback failure")
    }
}

#[test]
fn pre_cancel_is_strict_even_at_zero_credit() {
    assert_eq!(
        ScannerScope::begin(Probe(Rc::new(Cell::new(true))), 0).unwrap_err(),
        ScannerRefusal::Cancelled
    );
}

#[test]
fn callback_panic_does_not_cross_the_native_abi() {
    assert_eq!(
        ScannerScope::begin(PanicProbe, 0).unwrap_err(),
        ScannerRefusal::CallbackPanicked
    );
}

#[test]
fn nested_begin_cannot_reset_the_outer_refusal() {
    let outer = ScannerScope::begin(Probe(Rc::new(Cell::new(false))), 100).unwrap();
    assert_eq!(
        ScannerScope::begin(Probe(Rc::new(Cell::new(false))), 100).unwrap_err(),
        ScannerRefusal::ScopeConflict
    );
    assert_eq!(outer.finish(), Err(ScannerRefusal::ScopeConflict));
    ScannerScope::begin(Probe(Rc::new(Cell::new(false))), 100)
        .unwrap()
        .finish()
        .unwrap();
}

struct Dropped(Rc<Cell<usize>>);

impl CancellationProbe for Dropped {
    fn is_cancelled(&self) -> bool {
        false
    }
}

impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.set(self.0.get() + 1);
    }
}

#[test]
fn drop_detaches_before_releasing_the_owned_probe() {
    let drops = Rc::new(Cell::new(0));
    let scope = ScannerScope::begin(Dropped(Rc::clone(&drops)), 100).unwrap();
    assert_eq!(drops.get(), 0);
    drop(scope);
    assert_eq!(drops.get(), 1);
    ScannerScope::begin(Probe(Rc::new(Cell::new(false))), 100)
        .unwrap()
        .finish()
        .unwrap();
}

#[test]
fn native_callback_remains_owned_after_the_callers_handle_is_dropped() {
    let owner = Rc::new(Cell::new(false));
    let weak = Rc::downgrade(&owner);
    let scope = ScannerScope::begin(Probe(Rc::clone(&owner)), 100).unwrap();
    drop(owner);
    assert!(weak.upgrade().is_some());
    scope.status().unwrap();
    scope.finish().unwrap();
    assert!(weak.upgrade().is_none());
}

#[test]
fn late_cancellation_outranks_a_recorded_work_refusal() {
    unsafe extern "C" {
        fn rootlight_cuda_scope_step() -> bool;
    }
    let stop = Rc::new(Cell::new(false));
    let scope = ScannerScope::begin(Probe(Rc::clone(&stop)), 1).unwrap();
    // SAFETY: this thread owns the active scope and no native parse is running.
    unsafe {
        assert!(rootlight_cuda_scope_step());
        assert!(!rootlight_cuda_scope_step());
    }
    assert_eq!(scope.work(), 1);
    stop.set(true);
    assert_eq!(scope.finish(), Err(ScannerRefusal::Cancelled));
}

struct SelfDrop {
    slot: std::rc::Weak<RefCell<Option<ScannerScope<SelfDrop>>>>,
    drops: Rc<Cell<usize>>,
}

impl CancellationProbe for SelfDrop {
    fn is_cancelled(&self) -> bool {
        if let Some(slot) = self.slot.upgrade() {
            drop(slot.borrow_mut().take());
        }
        false
    }
}

impl Drop for SelfDrop {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
    }
}

#[test]
fn callback_can_drop_its_guard_without_dangling_context() {
    std::thread::spawn(|| {
        let slot = Rc::new(RefCell::new(None));
        let drops = Rc::new(Cell::new(0));
        let scope = ScannerScope::begin(
            SelfDrop {
                slot: Rc::downgrade(&slot),
                drops: Rc::clone(&drops),
            },
            100,
        )
        .unwrap();
        let token = scope.token;
        *slot.borrow_mut() = Some(scope);
        // SAFETY: this exact token is still active on its owning worker thread;
        // the callback deliberately drops its guard through safe RefCell access.
        let status = unsafe { rootlight_cuda_scope_status(token) };
        assert_eq!(
            ScannerScope::<SelfDrop>::decode(status),
            Err(ScannerRefusal::ScopeConflict)
        );
        assert!(slot.borrow().is_none());
        assert_eq!(drops.get(), 1);
        assert_eq!(
            ScannerScope::begin(Probe(Rc::new(Cell::new(false))), 100).unwrap_err(),
            ScannerRefusal::ScopeConflict
        );
    })
    .join()
    .unwrap();
}

#[test]
fn independent_workers_have_independent_native_scope_ownership() {
    let outer = ScannerScope::begin(Probe(Rc::new(Cell::new(false))), 100).unwrap();
    std::thread::spawn(|| {
        ScannerScope::begin(Probe(Rc::new(Cell::new(false))), 100)
            .unwrap()
            .finish()
            .unwrap();
    })
    .join()
    .unwrap();
    outer.finish().unwrap();
}
