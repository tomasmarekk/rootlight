//! Bind retained Sed tables to the checked, request-scoped scanner.
//! Cancellation storage remains owned until the native callback is detached.

use std::{cell::Cell, ffi::c_void, marker::PhantomData, rc::Rc};
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_gsed() -> *const ();
    fn tree_sitter_gsed_ere() -> *const ();
    fn rootlight_sed_scope_begin(limit: u64) -> u64;
    fn rootlight_sed_scope_status(token: u64) -> std::ffi::c_int;
    fn rootlight_sed_scope_work(token: u64) -> u64;
    fn rootlight_sed_scope_set_cancel(
        token: u64,
        callback: Option<extern "C" fn(*mut c_void) -> bool>,
        context: *mut c_void,
    ) -> bool;
    fn rootlight_sed_scope_finish(token: u64) -> std::ffi::c_int;
    fn rootlight_sed_scope_refuse(status: std::ffi::c_int);
}

/// The retained GNU Sed BRE entry point, generated at ABI15.
// SAFETY: this complete retained parser defines the exact GNU Sed BRE entry point.
pub const LANGUAGE_BRE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_gsed) };

/// The retained GNU Sed ERE entry point, generated at ABI15.
// SAFETY: this complete retained parser defines the exact GNU Sed ERE entry point.
pub const LANGUAGE_ERE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_gsed_ere) };

/// Exact BRE node inventory from the same retained revision as its parser.
pub const NODE_TYPES_BRE: &str = include_str!("../../upstream/src/node-types.json");

/// Exact ERE node inventory from the same retained revision as its parser.
pub const NODE_TYPES_ERE: &str = include_str!("../../upstream/gsed_ere/src/node-types.json");

/// Complete active scanner, status bridge and generated parser identity.
pub const SCANNER_CONTRACT_IDENTITY: &str = "sed-scanner-checked-state/1;wire=16/1;bre=f580c8ee7ae31e7ef805fa16ac6f9fc574ccac5ed8b9db56e8249491b3713377;ere=73064175619d6312c7f10af49269605ff0ff73fb3232eb01c087f43a17b271c4;scanner=fe32615ad4d49c6491e3232d2fa7a338c299eba8787fc3c8d9190cf2a116bd35;status=7a2936f01865b28855adb36dc58111968c394dd99a524188db7cb31f7dd80d9f;header=548ee8369da06c9bbe0dea3c4fdc8e5bba987717a3abdc0e95fc41a9c88d9c86;host-placeholder-body=ac6c689f42752579e0cf2cf23f87e7e8948fff8b8332cdc8ead55bf57bdc8838";

/// Native refusal, with strict failures taking precedence over work/capacity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScannerRefusal {
    /// A scanner or stable callback allocation was refused.
    OwnerAllocation,
    /// The fixed C scanner allocation was unavailable.
    NativeAllocationUnavailable,
    /// A regex group increment would exceed the fixed uint32 state range.
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
    /// The next external-scanner step exceeded its finite credit.
    WorkLimit,
    /// A callback panicked; no unwind crossed the native ABI.
    CallbackPanicked,
    /// The linked status bridge returned an unrecognized ABI value.
    StatusAbi(i32),
}

struct CallbackContext<T> {
    value: T,
    cancelled: fn(&T) -> bool,
    panicked: Cell<bool>,
}

fn report_callback_panic() {
    // SAFETY: this source-bound status operation only records a TLS refusal;
    // it neither polls a callback nor dereferences its possibly destroyed data.
    unsafe { rootlight_sed_scope_refuse(11) };
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
    if raw.is_null() {
        report_callback_panic();
        return true;
    }
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
pub struct ScannerScope<T: CancellationProbe> {
    token: u64,
    context: Rc<Vec<CallbackContext<T>>>,
    callback_owner: *const Vec<CallbackContext<T>>,
    _thread: PhantomData<Rc<()>>,
}

impl<T: CancellationProbe> std::fmt::Debug for ScannerScope<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ScannerScope")
            .finish_non_exhaustive()
    }
}

impl<T: CancellationProbe> ScannerScope<T> {
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
    pub fn begin(value: T, limit: u64) -> Result<Self, ScannerRefusal> {
        let mut context = Vec::new();
        context
            .try_reserve_exact(1)
            .map_err(|_| ScannerRefusal::OwnerAllocation)?;
        context.push(CallbackContext {
            value,
            cancelled: |value: &T| value.is_cancelled(),
            panicked: Cell::new(false),
        });
        // Stable Rc::new has no fallible allocation API; its owner allocation
        // is constant-size, while the generic T storage was reserved fallibly.
        let context = Rc::new(context);
        // SAFETY: the paired bridge owns a thread-local monotonically fresh token.
        let token = unsafe { rootlight_sed_scope_begin(limit) };
        if token == 0 {
            return Err(ScannerRefusal::ScopeConflict);
        }
        let callback_owner = Rc::into_raw(Rc::clone(&context));
        let scope = Self {
            token,
            context,
            callback_owner,
            _thread: PhantomData,
        };
        // SAFETY: this actual into_raw count remains owned by scope until native
        // finish detaches the callback, including installed-but-refused begin.
        let installed = unsafe {
            rootlight_sed_scope_set_cancel(
                token,
                Some(poll::<T>),
                scope.callback_owner.cast_mut().cast(),
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
        let status = unsafe { rootlight_sed_scope_finish(token) };
        self.release_native_owner();
        if self
            .context
            .first()
            .is_some_and(|context| context.panicked.get())
        {
            return Err(ScannerRefusal::CallbackPanicked);
        }
        Self::decode(status)
    }

    /// Return actual accepted external-scanner logical steps before closure.
    #[must_use]
    pub fn work(&self) -> u64 {
        // SAFETY: the thread-affine guard owns this active operation token.
        unsafe { rootlight_sed_scope_work(self.token) }
    }

    /// Poll sticky native status while retaining the owned callback.
    ///
    /// # Errors
    /// Any native or callback refusal prevents admission of partial syntax.
    pub fn status(&self) -> Result<(), ScannerRefusal> {
        // SAFETY: C retains its own Rc count until matching finish detaches it.
        Self::decode(unsafe { rootlight_sed_scope_status(self.token) })
    }

    fn decode(status: i32) -> Result<(), ScannerRefusal> {
        Err(match status {
            0 => return Ok(()),
            1 => ScannerRefusal::NativeAllocationUnavailable,
            2 => ScannerRefusal::Capacity,
            3 => ScannerRefusal::MalformedState,
            4 => ScannerRefusal::ScopeMissing,
            5 => ScannerRefusal::ScopeConflict,
            6 => ScannerRefusal::InvalidScalar,
            7 => ScannerRefusal::Cancelled,
            8 => ScannerRefusal::WorkLimit,
            11 => ScannerRefusal::CallbackPanicked,
            other => ScannerRefusal::StatusAbi(other),
        })
    }
}

impl<T: CancellationProbe> Drop for ScannerScope<T> {
    fn drop(&mut self) {
        if self.token != 0 {
            // SAFETY: the same owning thread closes this token before Vec drops.
            unsafe { rootlight_sed_scope_finish(self.token) };
            self.token = 0;
            self.release_native_owner();
        }
    }
}

/// Owned request cancellation probe; borrowed state cannot satisfy this bound.
pub trait CancellationProbe: 'static {
    /// Return whether parsing must stop; panics are contained at the native ABI.
    fn is_cancelled(&self) -> bool;
}
