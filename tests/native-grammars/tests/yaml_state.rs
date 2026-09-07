//! Exercises the YAML scanner's complete, bounded serialization contract.
//! Malformed state cannot retain prior indentation or escape the caller's buffer.

use std::ffi::{c_char, c_uint, c_void};
use std::ptr::NonNull;

// SAFETY: These declarations match the pinned scanner.c. The native constructor
// and destructor own the opaque allocation; all buffers outlive their calls.
unsafe extern "C" {
    fn tree_sitter_yaml_external_scanner_create() -> *mut c_void;
    fn tree_sitter_yaml_external_scanner_destroy(payload: *mut c_void);
    fn tree_sitter_yaml_external_scanner_serialize(
        payload: *mut c_void,
        buffer: *mut c_char,
    ) -> c_uint;
    fn tree_sitter_yaml_external_scanner_deserialize(
        payload: *mut c_void,
        buffer: *const c_char,
        length: c_uint,
    );
}

struct Scanner(NonNull<c_void>);

impl Scanner {
    fn new() -> Self {
        let _: tree_sitter::Language = tree_sitter_yaml::LANGUAGE.into();
        // SAFETY: The native constructor has no preconditions; this owner alone frees its result.
        Self(
            NonNull::new(unsafe { tree_sitter_yaml_external_scanner_create() })
                .expect("native allocation"),
        )
    }

    fn restore(&mut self, bytes: &[u8]) {
        let length = u32::try_from(bytes.len()).expect("bounded input");
        let pointer = if bytes.is_empty() {
            std::ptr::null()
        } else {
            bytes.as_ptr().cast()
        };
        // SAFETY: The patched scanner checks total length before reading each
        // complete header/frame. This live slice supplies every advertised byte.
        unsafe { tree_sitter_yaml_external_scanner_deserialize(self.0.as_ptr(), pointer, length) };
    }

    fn serialized(&mut self) -> Vec<u8> {
        let mut guarded = [0xa5_u8; 1026];
        // SAFETY: The scanner admits only states fitting 1024 bytes. The offset
        // deliberately has no integer alignment requirement; encoding uses bytes.
        let length = unsafe {
            tree_sitter_yaml_external_scanner_serialize(
                self.0.as_ptr(),
                guarded[1..].as_mut_ptr().cast(),
            )
        };
        assert_eq!(guarded[0], 0xa5);
        assert_eq!(guarded[1025], 0xa5);
        let length = usize::try_from(length).unwrap();
        assert!(length <= 1024);
        guarded[1..1 + length].to_vec()
    }
}

impl Drop for Scanner {
    fn drop(&mut self) {
        // SAFETY: The sole owner destroys its still-live allocation exactly once.
        unsafe { tree_sitter_yaml_external_scanner_destroy(self.0.as_ptr()) };
    }
}

fn state(positions: [i32; 4], tab: bool, frames: &[(u8, i32)]) -> Vec<u8> {
    let mut bytes = vec![1];
    for position in positions {
        bytes.extend_from_slice(&position.to_le_bytes());
    }
    bytes.push(u8::from(tab));
    bytes.extend_from_slice(&u16::try_from(frames.len()).unwrap().to_le_bytes());
    for &(kind, column) in frames {
        bytes.push(kind);
        bytes.extend_from_slice(&column.to_le_bytes());
    }
    bytes
}

#[test]
fn yaml_wide_positions_and_full_stacks_round_trip_without_truncation() {
    let mut scanner = Scanner::new();
    let empty = state([0, 0, -1, -1], false, &[]);
    assert_eq!(scanner.serialized(), empty);
    for position in [0, 127, 128, 32_767, 32_768, 65_535, 65_536, i32::MAX] {
        for count in [0, 1, 2, 127, 128, 199, 200] {
            let frames: Vec<_> = (0..count).map(|i| (b"mqs"[i % 3], position)).collect();
            let bytes = state([position; 4], position % 2 == 0, &frames);
            scanner.restore(&bytes);
            assert_eq!(scanner.serialized(), bytes, "{position}, {count}");
            scanner.restore(&empty);
            assert_eq!(scanner.serialized(), empty);
        }
    }
    let bytes = state([0, 0, -1, -1], false, &[(b's', -1)]);
    scanner.restore(&bytes);
    assert_eq!(scanner.serialized(), bytes);
}

#[test]
fn yaml_malformed_and_truncated_states_erase_prior_context() {
    let valid = state(
        [40_000, 65_536, 39_999, 42],
        true,
        &[(b'm', 10), (b'q', 12)],
    );
    let empty = state([0, 0, -1, -1], false, &[]);
    let mut scanner = Scanner::new();
    for length in 0..valid.len() {
        scanner.restore(&valid);
        scanner.restore(&valid[..length]);
        assert_eq!(scanner.serialized(), empty, "prefix {length}");
    }
    let mut invalid = vec![valid.clone(); 6];
    invalid[0][0] = 2;
    invalid[1][17] = 2;
    invalid[2][20] = b'x';
    invalid[3][1..5].copy_from_slice(&(-1_i32).to_le_bytes());
    invalid[4][9..13].copy_from_slice(&(-2_i32).to_le_bytes());
    invalid[5][21..25].copy_from_slice(&(-1_i32).to_le_bytes());
    invalid.push(state([0; 4], false, &vec![(b'm', 1); 201]));
    invalid.push([&valid[..], &[0]].concat());
    for bytes in invalid {
        scanner.restore(&valid);
        scanner.restore(&bytes);
        assert_eq!(scanner.serialized(), empty);
    }
}

#[test]
fn yaml_adversarial_state_bytes_remain_bounded_and_reusable() {
    let mut scanner = Scanner::new();
    for length in 0..=1025 {
        for byte in [0, 1, 127, 128, 255] {
            let bytes = vec![byte; length];
            scanner.restore(&bytes);
            let encoded = scanner.serialized();
            scanner.restore(&encoded);
            assert_eq!(scanner.serialized(), encoded);
            scanner.restore(&[]);
            assert_eq!(scanner.serialized(), state([0, 0, -1, -1], false, &[]));
        }
    }
    scanner.restore(&[255]);
    assert_eq!(scanner.serialized(), [255]);
    scanner.restore(&[]);
    assert_ne!(scanner.serialized(), [255]);
}
