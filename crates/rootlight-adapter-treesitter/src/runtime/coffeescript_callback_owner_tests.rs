//! Safe recursive callback owners must survive self-drop and refuse reentry.
//! These after-fix controls intentionally never execute the known unsafe wrapper.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
};
use tree_sitter_coffeescript::{CancellationProbe, ScannerRefusal, ScannerScope};
struct Never;
impl CancellationProbe for Never {
    fn is_cancelled(&self) -> bool {
        false
    }
}
impl CancellationProbe for Owner {
    fn is_cancelled(&self) -> bool {
        probe(self)
    }
}

#[derive(Clone, Copy)]
enum Action {
    Drop,
    Finish,
    Poll,
}

struct Owner {
    slot: Rc<RefCell<Option<ScannerScope<Owner>>>>,
    armed: Rc<Cell<bool>>,
    touches: Rc<Cell<usize>>,
    drops: Rc<Cell<usize>>,
    finish_refused: Rc<Cell<bool>>,
    begin_refused: Rc<Cell<bool>>,
    payload_drops: Arc<AtomicUsize>,
    action: Action,
    panic_on_drop: bool,
}

struct PanickingPayload(Arc<AtomicUsize>);

impl Drop for PanickingPayload {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::Relaxed);
        panic!("panic payload destructor must not run across C");
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.drops.set(self.drops.get() + 1);
        if self.panic_on_drop {
            std::panic::panic_any(PanickingPayload(Arc::clone(&self.payload_drops)));
        }
    }
}

fn probe(owner: &Owner) -> bool {
    if owner.armed.replace(false) {
        match owner.action {
            Action::Drop => {
                let guard = owner.slot.borrow_mut().take().unwrap();
                drop(guard);
            }
            Action::Finish => {
                let guard = owner.slot.borrow_mut().take().unwrap();
                owner
                    .finish_refused
                    .set(matches!(guard.finish(), Err(ScannerRefusal::ScopeConflict)));
            }
            Action::Poll => {
                let mut parser = tree_sitter::Parser::new();
                parser
                    .set_language(&tree_sitter_coffeescript::LANGUAGE.into())
                    .unwrap();
                drop(parser.parse(b"class Written\n  measure: ->\n    1\n", None));
                drop(parser);
            }
        }
        // This access was a safe-API UAF in the predecessor's sole Vec owner.
        owner.touches.set(owner.touches.get() + 1);
        owner.begin_refused.set(matches!(
            ScannerScope::begin(Never, 1_000_000),
            Err(ScannerRefusal::ScopeConflict)
        ));
        owner.touches.set(owner.touches.get() + 1);
    }
    false
}

fn run_reentrant_owner(action: Action, panic_on_drop: bool) {
    // A self-dropped operation deliberately stays conflicted on its worker;
    // worker exit, rather than another request, ends this misuse case.
    std::thread::spawn(move || {
        let slot = Rc::new(RefCell::new(None));
        let armed = Rc::new(Cell::new(false));
        let touches = Rc::new(Cell::new(0));
        let drops = Rc::new(Cell::new(0));
        let finish_refused = Rc::new(Cell::new(false));
        let begin_refused = Rc::new(Cell::new(false));
        let payload_drops = Arc::new(AtomicUsize::new(0));
        let scope = ScannerScope::begin(
            Owner {
                slot: Rc::clone(&slot),
                armed: Rc::clone(&armed),
                touches: Rc::clone(&touches),
                drops: Rc::clone(&drops),
                finish_refused: Rc::clone(&finish_refused),
                begin_refused: Rc::clone(&begin_refused),
                payload_drops: Arc::clone(&payload_drops),
                action,
                panic_on_drop,
            },
            1_000_000,
        )
        .unwrap();
        *slot.borrow_mut() = Some(scope);
        armed.set(true);
        let mut parser = tree_sitter::Parser::new();
        parser
            .set_language(&tree_sitter_coffeescript::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(b"class Written\n  measure: ->\n    1\n", None);
        drop(tree);
        drop(parser);
        if matches!(action, Action::Poll) {
            let scope = slot.borrow_mut().take().unwrap();
            assert!(matches!(scope.finish(), Err(ScannerRefusal::ScopeConflict)));
        }
        assert!(slot.borrow().is_none());
        assert_eq!(touches.get(), 2);
        assert_eq!(drops.get(), 1);
        assert!(begin_refused.get());
        if matches!(action, Action::Finish) {
            assert!(finish_refused.get());
        }
        assert_eq!(payload_drops.load(Ordering::Relaxed), 0);
        if matches!(action, Action::Poll) {
            ScannerScope::begin(Never, 1_000_000)
                .unwrap()
                .finish()
                .unwrap();
        } else {
            assert!(matches!(
                ScannerScope::begin(Never, 1_000_000),
                Err(ScannerRefusal::ScopeConflict)
            ));
        }
    })
    .join()
    .unwrap();
    ScannerScope::begin(Never, 1_000_000)
        .unwrap()
        .finish()
        .unwrap();
}

#[test]
fn safe_callback_self_drop_retains_borrowed_owner_and_refuses_new_scope() {
    run_reentrant_owner(Action::Drop, false);
}

#[test]
fn safe_callback_self_finish_cannot_launder_the_active_operation() {
    run_reentrant_owner(Action::Finish, false);
}

#[test]
fn last_callback_owner_destructor_panic_is_contained_before_return_to_c() {
    run_reentrant_owner(Action::Drop, true);
}

#[test]
fn reentrant_finish_and_panicking_owner_cannot_reopen_the_worker() {
    run_reentrant_owner(Action::Finish, true);
}

#[test]
fn safe_callback_nested_parser_poll_is_refused_without_recursion() {
    run_reentrant_owner(Action::Poll, false);
}
