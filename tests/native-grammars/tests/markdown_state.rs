//! Direct Markdown scanner snapshot contracts outside production's unsafe boundary.
//! Guarded buffers verify complete byte-oriented restoration and failure persistence.

use std::{
    ffi::{c_char, c_uint, c_void},
    ptr::NonNull,
};

// SAFETY: These signatures match the two pinned C scanner implementations.
unsafe extern "C" {
    fn tree_sitter_markdown_external_scanner_create() -> *mut c_void;
    fn tree_sitter_markdown_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_markdown_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_markdown_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
    fn tree_sitter_markdown_inline_external_scanner_create() -> *mut c_void;
    fn tree_sitter_markdown_inline_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_markdown_inline_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_markdown_inline_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner {
    payload: NonNull<c_void>,
    inline: bool,
}

impl Scanner {
    fn new(inline: bool) -> Self {
        let _: tree_sitter::Language = tree_sitter_md::LANGUAGE.into();
        let _: tree_sitter::Language = tree_sitter_md::INLINE_LANGUAGE.into();
        // SAFETY: Constructors have no preconditions; this owner frees the allocation once.
        let pointer = unsafe {
            if inline {
                tree_sitter_markdown_inline_external_scanner_create()
            } else {
                tree_sitter_markdown_external_scanner_create()
            }
        };
        Self {
            payload: NonNull::new(pointer).unwrap(),
            inline,
        }
    }

    fn restore(&mut self, bytes: &[u8]) {
        let buffer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: The advertised byte range is live, and both patched scanners validate
        // the complete length before reading fields. Zero length admits a null buffer.
        unsafe {
            if self.inline {
                tree_sitter_markdown_inline_external_scanner_deserialize(
                    self.payload.as_ptr(),
                    buffer,
                    u32::try_from(bytes.len()).unwrap(),
                );
            } else {
                tree_sitter_markdown_external_scanner_deserialize(
                    self.payload.as_ptr(),
                    buffer,
                    u32::try_from(bytes.len()).unwrap(),
                );
            }
        }
    }

    fn bytes(&mut self) -> Vec<u8> {
        let mut guarded = [0xa5u8; 1026];
        // SAFETY: Both serializers are bounded by the 1024-byte ABI, and their byte
        // encoding permits the deliberately unaligned address after the first guard.
        let count = unsafe {
            if self.inline {
                tree_sitter_markdown_inline_external_scanner_serialize(
                    self.payload.as_ptr(),
                    guarded[1..].as_mut_ptr().cast(),
                )
            } else {
                tree_sitter_markdown_external_scanner_serialize(
                    self.payload.as_ptr(),
                    guarded[1..].as_mut_ptr().cast(),
                )
            }
        };
        assert_eq!(guarded[0], 0xa5);
        assert_eq!(guarded[1025], 0xa5);
        let count = usize::try_from(count).unwrap();
        assert!(count <= 1024);
        guarded[1..1 + count].to_vec()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: This sole owner invokes the matching destructor for a live allocation.
        unsafe {
            if self.inline {
                tree_sitter_markdown_inline_external_scanner_destroy(self.payload.as_ptr());
            } else {
                tree_sitter_markdown_external_scanner_destroy(self.payload.as_ptr());
            }
        }
    }
}

fn block_state(count: u16, width: u64) -> Vec<u8> {
    let mut bytes = vec![1, 0x13];
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend_from_slice(&(width * 4).to_le_bytes());
    bytes.push(3);
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&count.to_le_bytes());
    bytes.extend((0..count).map(|index| u8::try_from(index % 20).unwrap()));
    bytes
}

fn inline_state(width: u64) -> Vec<u8> {
    let mut bytes = vec![1, 4];
    for value in [width, width, width] {
        bytes.extend_from_slice(&value.to_le_bytes());
    }
    bytes
}

#[test]
fn markdown_snapshots_preserve_wide_counters_and_complete_stacks() {
    let mut block = Scanner::new(false);
    let mut inline = Scanner::new(true);
    for width in [0, 1, 127, 255, 256, 65535, u64::from(u32::MAX)] {
        for count in [0, 1, 127, 254, 255, 256, 1001] {
            let expected = block_state(count, width);
            block.restore(&expected);
            assert_eq!(block.bytes(), expected, "{count}:{width}");
        }
        let expected = inline_state(width);
        inline.restore(&expected);
        assert_eq!(inline.bytes(), expected);
    }
}

#[test]
fn markdown_truncated_unknown_and_oversized_snapshots_fail_closed() {
    for inline in [false, true] {
        let mut scanner = Scanner::new(inline);
        let valid = if inline {
            inline_state(257)
        } else {
            block_state(1, 257)
        };
        for length in 1..valid.len() {
            scanner.restore(&valid);
            scanner.restore(&valid[..length]);
            assert_eq!(scanner.bytes(), [0xff], "inline={inline}, length={length}");
        }
        for length in 1..=1024 {
            scanner.restore(&vec![0x5a; length]);
            assert_eq!(scanner.bytes(), [0xff]);
        }
        for malformed in [
            vec![0xff],
            vec![0; 1025],
            {
                let mut v = valid.clone();
                v.push(0);
                v
            },
            {
                let mut v = valid.clone();
                v[1] = 0x80;
                v
            },
        ] {
            scanner.restore(&malformed);
            let failure = scanner.bytes();
            assert_eq!(failure, [0xff]);
            scanner.restore(&failure);
            assert_eq!(scanner.bytes(), failure);
        }
        scanner.restore(&[]);
        let mut empty = vec![0; if inline { 26 } else { 23 }];
        empty[0] = 1;
        assert_eq!(scanner.bytes(), empty);
        scanner.restore(&valid);
        assert_eq!(scanner.bytes(), valid);
    }
}

#[test]
fn markdown_snapshot_decoders_reject_impossible_fields() {
    let mut block = Scanner::new(false);
    let baseline = block_state(1, 3);
    for index in [0, 2, 12, 20, 21, 23] {
        let mut malformed = baseline.clone();
        malformed[index] = 0xff;
        block.restore(&malformed);
        assert_eq!(block.bytes(), [0xff], "field {index}");
    }
    let mut inline = Scanner::new(true);
    for index in [0, 6, 14, 22] {
        let mut malformed = inline_state(3);
        malformed[index] = 0xff;
        inline.restore(&malformed);
        assert_eq!(inline.bytes(), [0xff], "field {index}");
    }
}
