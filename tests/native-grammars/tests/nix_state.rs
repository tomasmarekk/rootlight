//! Pins the stateless Nix scanner's C ABI ownership and serialization contract.
//! This isolated test does not introduce unsafe code into production crates.

use std::ffi::{c_char, c_uint, c_void};

// SAFETY: These declarations match the pinned scanner's exported C signatures.
unsafe extern "C" {
    fn tree_sitter_nix_external_scanner_create() -> *mut c_void;
    fn tree_sitter_nix_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_nix_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_nix_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

#[test]
fn stateless_scanner_neither_owns_nor_serializes_context() {
    let _: tree_sitter::Language = tree_sitter_nix::LANGUAGE.into();
    // SAFETY: The constructor requires no inputs and returns the stateless null payload.
    let payload = unsafe { tree_sitter_nix_external_scanner_create() };
    assert!(payload.is_null());
    for length in [0, 1, 2, 255, 1023, 1024] {
        let input = vec![0xff_u8; length];
        let pointer = if input.is_empty() {
            std::ptr::null()
        } else {
            input.as_ptr().cast()
        };
        // SAFETY: The pinned no-op decoder accepts its null payload; all advertised bytes live.
        unsafe {
            tree_sitter_nix_external_scanner_deserialize(
                payload,
                pointer,
                c_uint::try_from(length).unwrap(),
            );
        }
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: The scanner receives its own payload and an ABI-sized writable region.
        let written = unsafe {
            tree_sitter_nix_external_scanner_serialize(
                payload,
                guarded[1..1025].as_mut_ptr().cast(),
            )
        };
        assert_eq!(written, 0);
        assert_eq!(guarded, [0xa5; 1026]);
    }
    // SAFETY: The no-op destructor receives exactly its constructor's payload once.
    unsafe { tree_sitter_nix_external_scanner_destroy(payload) };
}
