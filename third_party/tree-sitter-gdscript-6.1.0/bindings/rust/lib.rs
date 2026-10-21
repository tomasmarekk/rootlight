//! Bind pinned GDScript tables to the separately reviewed checked-state scanner.
//! Scanner status must be checked even when Tree-sitter returns a clean tree.

use std::{marker::PhantomData, rc::Rc};
use tree_sitter_language::LanguageFn;

unsafe extern "C" {
    fn tree_sitter_gdscript() -> *const ();
    fn rootlight_gdscript_scope_begin() -> u64;
    fn rootlight_gdscript_scope_finish(token: u64) -> std::ffi::c_int;
}

/// The native entry point; it does not authorize parsing without a scanner scope.
// SAFETY: the paired retained parser.c defines this language function.
pub const LANGUAGE: LanguageFn = unsafe { LanguageFn::from_raw(tree_sitter_gdscript) };

/// The unchanged upstream node inventory used by the current local runtime.
pub const NODE_TYPES: &str = include_str!("../../upstream/src/node-types.json");

/// Source-bound scanner and status-control identity for cache/admission guards.
pub const SCANNER_CONTRACT_IDENTITY: &str = concat!(
    "gdscript-scanner-checked-state/1;parser=",
    "cc18c78720d6dfc8951e3132e1d3abeeb241b1c16c3ecdd54ff8551ad2017f04",
    ";scanner=",
    "9cd19de7e118dee560deb187c4114440da8ab9285121a96dd5c6f22f19b4fee1",
    ";status=",
    "899a5c5e9eec6b2579415af9b3329fe43409cff4b1e71540e4769373bbe47c63",
    ";status-unit=",
    "a95ea4ceec487e2700227ce257b92d5de8eb0bf411211c9da22f15d9eedc8e29",
    ";controls=",
    "c88665cb015b37b064c3e43352e83aee652ebb55fdc5a47a2967b9612afdd0f4",
);

/// A refusal from the native callback scope, including unrecognized ABI values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ScannerRefusal {
    /// A scanner-owned allocation was refused.
    Allocation,
    /// A finite scanner state or nesting bound was reached.
    Capacity,
    /// Scanner restoration rejected a malformed or incomplete record.
    MalformedState,
    /// A callback ran without the exact active request scope.
    ScopeMissing,
    /// Nested or mismatched scope ownership was refused.
    ScopeConflict,
    /// The linked status ABI returned a value outside this reviewed contract.
    Unknown,
}

/// Own one synchronous callback scope from parser setup through tree admission.
///
/// The guard cannot cross threads. `finish` must succeed before publishing a
/// tree or facts; Drop closes only this exact token on error or unwinding.
#[must_use = "finish the native scanner scope before admitting its parse result"]
pub struct ScannerScope {
    token: u64,
    _thread: PhantomData<Rc<()>>,
}

impl ScannerScope {
    /// Begin on the current OS thread, refusing an already active native scope.
    ///
    /// Returns `ScopeConflict` without taking ownership when the token is zero.
    pub fn begin() -> Result<Self, ScannerRefusal> {
        // SAFETY: the paired status unit defines this no-argument TLS operation.
        let token = unsafe { rootlight_gdscript_scope_begin() };
        if token == 0 {
            return Err(ScannerRefusal::ScopeConflict);
        }
        Ok(Self {
            token,
            _thread: PhantomData,
        })
    }

    /// Finish the exact owned scope and return its sticky first refusal.
    ///
    /// Success is the required admission check, not a claim of language coverage.
    pub fn finish(mut self) -> Result<(), ScannerRefusal> {
        let token = std::mem::replace(&mut self.token, 0);
        // SAFETY: only this thread-affine guard owns the nonzero token.
        match unsafe { rootlight_gdscript_scope_finish(token) } {
            0 => Ok(()),
            1 => Err(ScannerRefusal::Allocation),
            2 => Err(ScannerRefusal::Capacity),
            3 => Err(ScannerRefusal::MalformedState),
            4 => Err(ScannerRefusal::ScopeMissing),
            5 => Err(ScannerRefusal::ScopeConflict),
            _ => Err(ScannerRefusal::Unknown),
        }
    }
}

impl Drop for ScannerScope {
    fn drop(&mut self) {
        if self.token != 0 {
            // SAFETY: Drop stays on the guard's thread and closes no other token.
            unsafe { rootlight_gdscript_scope_finish(self.token) };
        }
    }
}
