//! Full-scan comparison for browser credentials and capability digests.
//!
//! Lengths are public protocol properties; equal-length contents must not
//! reveal the position of a differing byte through an early exit.

/// Compares equal-length secret bytes without a content-dependent early exit.
#[inline(never)]
pub(crate) fn equal_secret_bytes(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut difference = 0u8;
    for (&left_byte, &right_byte) in left.iter().zip(right) {
        // Keep every iteration observable to the optimizer so it cannot
        // replace the full scan with a short-circuiting equality check.
        difference = std::hint::black_box(difference | (left_byte ^ right_byte));
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::equal_secret_bytes;

    #[test]
    fn compares_empty_and_unequal_lengths() {
        assert!(equal_secret_bytes(&[], &[]));
        assert!(!equal_secret_bytes(&[], &[0]));
        assert!(!equal_secret_bytes(&[0], &[]));
        assert!(!equal_secret_bytes(&[0, 1], &[0]));
    }

    #[test]
    fn compares_every_byte_of_token_and_service_credentials() {
        for length in [1, 32, 43, 64] {
            let expected = vec![0x5a; length];
            assert!(equal_secret_bytes(&expected, &expected));
            for index in 0..length {
                let mut candidate = expected.clone();
                candidate[index] ^= 0xff;
                assert!(!equal_secret_bytes(&expected, &candidate));
                assert!(!equal_secret_bytes(&candidate, &expected));
            }
        }
    }
}
