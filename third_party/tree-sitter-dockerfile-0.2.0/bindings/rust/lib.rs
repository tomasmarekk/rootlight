//! Bind retained Dockerfile tables to the checked, request-scoped scanner.
//! Cancellation storage remains owned until the native callback is detached.

use std::{cell::Cell, ffi::c_void, marker::PhantomData, rc::Rc};
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_dockerfile() -> *const ();
    fn rootlight_dockerfile_scope_begin() -> u64;
    fn rootlight_dockerfile_scope_set_cancel(
        token: u64,
        callback: Option<extern "C" fn(*mut c_void) -> bool>,
        context: *mut c_void,
    ) -> bool;
    fn rootlight_dockerfile_scope_finish(token: u64) -> std::ffi::c_int;
    fn rootlight_dockerfile_scope_refuse(status: std::ffi::c_int);
}

/// The retained generated ABI-14 Dockerfile entry point.
// SAFETY: the retained generated parser defines this exact language entry point.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_dockerfile) };

/// Native node inventory from the same actual generator invocation as the tables.
pub const NODE_TYPES: &str = include_str!("../../generated/src/node-types.json");

/// Complete active scanner, status bridge and generated parser identity.
pub const SCANNER_CONTRACT_IDENTITY: &str = "dockerfile-scanner-checked-state/2;parser=95a06be957b58f1121ddf523cc8361d8a8c9f256ce0953c62c67bebbe1727612;scanner=76f04e79df9e21d82fbb0b98460dbe549db44ffbcf10373827e3175255d70595;status=07b250b0eff3dbae6bf194084560632321a5bd00d6b04fd0c27f5631a9219ac0;status-unit=2df17e68c0c3e4e718963c4f37bd7e9f38b2394a947b88de47347e49a707ec06";

/// First native refusal, or a strict wrapper lifecycle failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScannerRefusal {
    /// A scanner or stable callback allocation was refused.
    Allocation,
    /// The complete scanner state exceeds its 1024-byte serialized format.
    Capacity,
    /// Restored native scanner state was malformed or incomplete.
    MalformedState,
    /// A native callback ran outside its owning synchronous scope.
    ScopeMissing,
    /// Nested, exhausted or mismatched scope ownership was refused.
    ScopeConflict,
    /// Native input exposed a value outside the Unicode scalar contract.
    InvalidScalar,
    /// The installed cancellation callback requested a stop.
    Cancelled,
    /// A written heredoc marker requires unsupported scanner semantics.
    UnsupportedMarker,
    /// A callback panicked; no unwind crossed the native ABI.
    CallbackPanicked,
    /// The linked status bridge returned an unrecognized ABI value.
    Unknown,
}

struct CallbackContext<T> {
    value: T,
    cancelled: fn(&T) -> bool,
    panicked: Cell<bool>,
}

fn report_callback_panic() {
    // SAFETY: this source-bound status operation only records a TLS refusal;
    // it neither polls a callback nor dereferences its possibly destroyed data.
    unsafe { rootlight_dockerfile_scope_refuse(9) };
}

fn release_callback_owner<T>(owner: Rc<Vec<CallbackContext<T>>>, cancelled: bool) -> bool {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(owner))) {
        Ok(()) => cancelled,
        Err(payload) => {
            report_callback_panic();
            // A panic payload may itself panic in Drop; do not run it across C.
            std::mem::forget(payload);
            true
        }
    }
}

extern "C" fn poll<T>(raw: *mut c_void) -> bool {
    if raw.is_null() { report_callback_panic(); return true; }
    let pointer = raw.cast::<Vec<CallbackContext<T>>>();
    // SAFETY: begin installed this exact Rc::into_raw-owned C count on the same
    // thread. A non-reentrant detach precedes releasing that count. Retain an
    // independent temporary before arbitrary user code can finish/drop its guard.
    let owner = unsafe {
        Rc::increment_strong_count(pointer);
        Rc::from_raw(pointer)
    };
    let Some(context) = owner.first() else {
        report_callback_panic();
        return release_callback_owner(owner, true);
    };
    let cancelled = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        (context.cancelled)(&context.value)
    })) {
        Ok(cancelled) => cancelled,
        Err(payload) => {
            context.panicked.set(true);
            report_callback_panic();
            std::mem::forget(payload);
            true
        }
    };
    // No context access follows the final owner release: T::drop may run here.
    release_callback_owner(owner, cancelled)
}

/// Own one synchronous scanner operation and its stable cancellation context.
///
/// This guard cannot cross threads. Each callback temporarily retains shared
/// ownership before lending `&T`, including when T safely finishes its own guard.
/// Forgetting leaks the owned counts and leaves the native scope occupied.
/// Reentrant finish detaches its callback but leaves the operation conflicted;
/// a self-dropped guard requires that worker thread to exit before another scope.
/// The one-element Vec reserves fallibly; its constant-size Rc owner uses stable
/// infallible allocation. This API provides no hard/accounted memory guarantee.
#[must_use = "finish the scanner scope before admitting native syntax"]
pub struct ScannerScope<T: 'static> {
    token: u64,
    context: Rc<Vec<CallbackContext<T>>>,
    callback_owner: *const Vec<CallbackContext<T>>,
    _thread: PhantomData<Rc<()>>,
}

impl<T: 'static> std::fmt::Debug for ScannerScope<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("ScannerScope").finish_non_exhaustive()
    }
}

impl<T: 'static> ScannerScope<T> {
    fn release_native_owner(&mut self) {
        let pointer = std::mem::replace(&mut self.callback_owner, std::ptr::null());
        if !pointer.is_null() {
            // SAFETY: this pairs only the C-held into_raw count, exactly once,
            // after matching finish detached the callback. The guard's separate
            // Rc still owns the context, so this release cannot destroy T.
            unsafe { drop(Rc::from_raw(pointer)) };
        }
    }

    /// Install an owned callback context on the current OS thread.
    ///
    /// # Errors
    /// Returns allocation, ownership, cancellation or callback failure without
    /// leaving a callback pointing at released storage.
    pub fn begin(value: T, cancelled: fn(&T) -> bool) -> Result<Self, ScannerRefusal> {
        let mut context = Vec::new();
        context.try_reserve_exact(1).map_err(|_| ScannerRefusal::Allocation)?;
        context.push(CallbackContext { value, cancelled, panicked: Cell::new(false) });
        // Stable Rc::new has no fallible allocation API; its owner allocation
        // is constant-size, while the generic T storage was reserved fallibly.
        let context = Rc::new(context);
        // SAFETY: the paired bridge owns a thread-local monotonically fresh token.
        let token = unsafe { rootlight_dockerfile_scope_begin() };
        if token == 0 {
            return Err(ScannerRefusal::ScopeConflict);
        }
        let callback_owner = Rc::into_raw(Rc::clone(&context));
        let scope = Self { token, context, callback_owner, _thread: PhantomData };
        // SAFETY: this actual into_raw count remains owned by scope until native
        // finish detaches the callback, including installed-but-refused begin.
        let installed = unsafe {
            rootlight_dockerfile_scope_set_cancel(
                token, Some(poll::<T>), scope.callback_owner.cast_mut().cast(),
            )
        };
        if !installed {
            return match scope.finish() {
                Err(refusal) => Err(refusal),
                Ok(()) => Err(ScannerRefusal::ScopeConflict),
            };
        }
        Ok(scope)
    }

    /// Detach the callback and return the operation's sticky native refusal.
    ///
    /// # Errors
    /// Any native or wrapper refusal prevents publication of its tree or facts.
    pub fn finish(mut self) -> Result<(), ScannerRefusal> {
        let token = std::mem::replace(&mut self.token, 0);
        // SAFETY: this thread-affine guard alone owns the nonzero token; native
        // finish clears the callback before context can be dropped.
        let status = unsafe { rootlight_dockerfile_scope_finish(token) };
        self.release_native_owner();
        if self.context.first().is_some_and(|context| context.panicked.get()) {
            return Err(ScannerRefusal::CallbackPanicked);
        }
        match status {
            0 => Ok(()),
            1 => Err(ScannerRefusal::Allocation),
            2 => Err(ScannerRefusal::Capacity),
            3 => Err(ScannerRefusal::MalformedState),
            4 => Err(ScannerRefusal::ScopeMissing),
            5 => Err(ScannerRefusal::ScopeConflict),
            6 => Err(ScannerRefusal::InvalidScalar),
            7 => Err(ScannerRefusal::Cancelled),
            8 => Err(ScannerRefusal::UnsupportedMarker),
            9 => Err(ScannerRefusal::CallbackPanicked),
            _ => Err(ScannerRefusal::Unknown),
        }
    }
}

impl<T: 'static> Drop for ScannerScope<T> {
    fn drop(&mut self) {
        if self.token != 0 {
            // SAFETY: the same owning thread closes this token before Vec drops.
            unsafe { rootlight_dockerfile_scope_finish(self.token) };
            self.token = 0;
            self.release_native_owner();
        }
    }
}

#[cfg(test)]
mod tests;
