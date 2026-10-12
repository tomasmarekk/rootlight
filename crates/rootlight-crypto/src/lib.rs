//! First-party cryptographic primitives used at Rootlight's integrity boundaries.
//!
//! [`Sha256`] implements the byte-oriented form of SHA-256 specified by
//! [FIPS 180-4](https://doi.org/10.6028/NIST.FIPS.180-4).
//!
//! ```
//! use rootlight_crypto::Sha256;
//!
//! let digest = Sha256::digest(b"abc");
//! assert_eq!(digest.len(), 32);
//! ```

#![forbid(unsafe_code)]

/// An incremental SHA-256 hasher for byte-oriented messages.
///
/// Input must contain fewer than 2^61 bytes, as required by FIPS 180-4's
/// 64-bit message-length field. The digest is always encoded as 32 bytes in
/// big-endian word order.
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffered: usize,
    total_bytes: u64,
}

// FIPS 180-4, sections 4.2.2 and 5.3.3.
const INITIAL_STATE: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const ROUND_CONSTANTS: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Sha256 {
    /// Starts a new SHA-256 computation.
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: INITIAL_STATE,
            buffer: [0; 64],
            buffered: 0,
            total_bytes: 0,
        }
    }

    /// Adds bytes to the message.
    ///
    /// # Panics
    ///
    /// Panics if the accumulated message would reach 2^61 bytes, beyond the
    /// byte-oriented SHA-256 input domain.
    pub fn update(&mut self, input: impl AsRef<[u8]>) {
        let mut input = input.as_ref();
        if input.is_empty() {
            return;
        }
        let added = u64::try_from(input.len()).expect("SHA-256 input length must fit in u64");
        self.total_bytes = self
            .total_bytes
            .checked_add(added)
            .filter(|total| *total < (1_u64 << 61))
            .expect("SHA-256 message must contain fewer than 2^61 bytes");

        if self.buffered != 0 {
            let copied = input.len().min(64 - self.buffered);
            self.buffer[self.buffered..self.buffered + copied].copy_from_slice(&input[..copied]);
            self.buffered += copied;
            input = &input[copied..];
            if self.buffered < 64 {
                return;
            }
            let block = self.buffer;
            self.compress(&block);
            self.buffered = 0;
        }

        let (blocks, remaining) = input.as_chunks::<64>();
        for block in blocks {
            self.compress(block);
        }
        self.buffer[..remaining.len()].copy_from_slice(remaining);
        self.buffered = remaining.len();
    }

    /// Completes the hash and returns its 32-byte digest.
    #[must_use]
    pub fn finalize(mut self) -> [u8; 32] {
        let message_bits = self.total_bytes * 8;
        self.buffer[self.buffered] = 0x80;
        self.buffered += 1;
        if self.buffered > 56 {
            self.buffer[self.buffered..].fill(0);
            let block = self.buffer;
            self.compress(&block);
            self.buffered = 0;
        }
        self.buffer[self.buffered..56].fill(0);
        self.buffer[56..64].copy_from_slice(&message_bits.to_be_bytes());
        let block = self.buffer;
        self.compress(&block);

        let mut digest = [0; 32];
        for (word, bytes) in self.state.iter().zip(digest.as_chunks_mut::<4>().0) {
            bytes.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }

    /// Hashes a byte-oriented message in one call.
    ///
    /// # Panics
    ///
    /// Panics if the message contains at least 2^61 bytes.
    #[must_use]
    pub fn digest(input: impl AsRef<[u8]>) -> [u8; 32] {
        let mut hasher = Self::new();
        hasher.update(input);
        hasher.finalize()
    }

    fn compress(&mut self, block: &[u8; 64]) {
        let mut schedule = [0_u32; 64];
        for (word, bytes) in schedule[..16].iter_mut().zip(block.as_chunks::<4>().0) {
            *word = u32::from_be_bytes(*bytes);
        }
        for index in 16..64 {
            let prior_15 = schedule[index - 15];
            let prior_2 = schedule[index - 2];
            let small_0 = prior_15.rotate_right(7) ^ prior_15.rotate_right(18) ^ (prior_15 >> 3);
            let small_1 = prior_2.rotate_right(17) ^ prior_2.rotate_right(19) ^ (prior_2 >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(small_0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(small_1);
        }

        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.state;
        for (&word, &constant) in schedule.iter().zip(&ROUND_CONSTANTS) {
            let big_1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ (!e & g);
            let temporary_1 = h
                .wrapping_add(big_1)
                .wrapping_add(choose)
                .wrapping_add(constant)
                .wrapping_add(word);
            let big_0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temporary_2 = big_0.wrapping_add(majority);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temporary_1);
            d = c;
            c = b;
            b = a;
            a = temporary_1.wrapping_add(temporary_2);
        }
        for (state, word) in self.state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *state = state.wrapping_add(word);
        }
    }
}

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::Sha256;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn matches_nist_single_and_double_block_examples() {
        assert_eq!(
            hex(&Sha256::digest(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(
            hex(&Sha256::digest(
                b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"
            )),
            "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1"
        );
    }

    #[test]
    fn handles_empty_input_and_padding_boundaries() {
        assert_eq!(
            hex(&Sha256::digest([])),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        for length in [1, 55, 56, 63, 64, 65, 119, 120, 127, 128] {
            let bytes: Vec<u8> = (0..length)
                .map(|value| u8::try_from(value).expect("fixture byte fits in u8"))
                .collect();
            let expected = Sha256::digest(&bytes);
            for split in 0..=length {
                let mut hasher = Sha256::new();
                hasher.update(&bytes[..split]);
                hasher.update(&bytes[split..]);
                assert_eq!(
                    hasher.finalize(),
                    expected,
                    "length={length}, split={split}"
                );
            }
        }
    }

    #[test]
    fn matches_independent_padding_boundary_digests() {
        // Independently generated with Python's hashlib.sha256, including both
        // sides of the one-block and two-block padding boundaries.
        let cases = [
            (
                55,
                "463eb28e72f82e0a96c0a4cc53690c571281131f672aa229e0d45ae59b598b59",
            ),
            (
                56,
                "da2ae4d6b36748f2a318f23e7ab1dfdf45acdc9d049bd80e59de82a60895f562",
            ),
            (
                63,
                "29af2686fd53374a36b0846694cc342177e428d1647515f078784d69cdb9e488",
            ),
            (
                64,
                "fdeab9acf3710362bd2658cdc9a29e8f9c757fcf9811603a8c447cd1d9151108",
            ),
            (
                65,
                "4bfd2c8b6f1eec7a2afeb48b934ee4b2694182027e6d0fc075074f2fabb31781",
            ),
            (
                119,
                "da18797ed7c3a777f0847f429724a2d8cd5138e6ed2895c3fa1a6d39d18f7ec6",
            ),
            (
                120,
                "f52b23db1fbb6ded89ef42a23ce0c8922c45f25c50b568a93bf1c075420bbb7c",
            ),
            (
                127,
                "92ca0fa6651ee2f97b884b7246a562fa71250fedefe5ebf270d31c546bfea976",
            ),
            (
                128,
                "471fb943aa23c511f6f72f8d1652d9c880cfa392ad80503120547703e56a2be5",
            ),
        ];
        for (length, expected) in cases {
            let bytes: Vec<u8> = (0..length)
                .map(|value| u8::try_from(value).expect("fixture byte fits in u8"))
                .collect();
            assert_eq!(hex(&Sha256::digest(bytes)), expected, "length={length}");
        }
    }

    #[test]
    fn matches_million_a_reference() {
        let mut hasher = Sha256::new();
        let chunk = [b'a'; 1000];
        for _ in 0..1000 {
            hasher.update(chunk);
        }
        assert_eq!(
            hex(&hasher.finalize()),
            "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
        );
    }
}
