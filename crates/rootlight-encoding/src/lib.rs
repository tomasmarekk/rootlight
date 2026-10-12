//! First-party text encodings used at Rootlight's protocol boundaries.

#![forbid(unsafe_code)]

const BASE64_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const BASE64URL_ALPHABET: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
const HEX_ALPHABET: &[u8; 16] = b"0123456789abcdef";

/// A malformed or noncanonical encoded value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecodeError;

/// One of the fixed encodings used by Rootlight.
#[derive(Clone, Copy)]
pub struct Encoding {
    kind: EncodingKind,
}

#[derive(Clone, Copy)]
enum EncodingKind {
    Base64,
    Base64UrlNoPad,
    HexLower,
    HexLowerPermissive,
}

/// RFC 4648 Base64 with mandatory padding.
pub const BASE64: Encoding = Encoding {
    kind: EncodingKind::Base64,
};
/// RFC 4648 URL-safe Base64 without padding.
pub const BASE64URL_NOPAD: Encoding = Encoding {
    kind: EncodingKind::Base64UrlNoPad,
};
/// Lowercase hexadecimal text.
pub const HEXLOWER: Encoding = Encoding {
    kind: EncodingKind::HexLower,
};
/// Hexadecimal text accepting either letter case when decoding.
pub const HEXLOWER_PERMISSIVE: Encoding = Encoding {
    kind: EncodingKind::HexLowerPermissive,
};

impl Encoding {
    /// Encodes all input bytes into canonical text.
    #[must_use]
    pub fn encode(self, input: &[u8]) -> String {
        match self.kind {
            EncodingKind::Base64 => encode_base64(input, BASE64_ALPHABET, true),
            EncodingKind::Base64UrlNoPad => encode_base64(input, BASE64URL_ALPHABET, false),
            EncodingKind::HexLower | EncodingKind::HexLowerPermissive => encode_hex(input),
        }
    }

    /// Decodes only well-formed text with canonical trailing bits.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] for invalid length, alphabet, padding, or unused bits.
    pub fn decode(self, input: &[u8]) -> Result<Vec<u8>, DecodeError> {
        match self.kind {
            EncodingKind::Base64 => decode_base64(input, false),
            EncodingKind::Base64UrlNoPad => decode_base64(input, true),
            EncodingKind::HexLower => decode_hex(input, false),
            EncodingKind::HexLowerPermissive => decode_hex(input, true),
        }
    }

    /// Decodes into an exactly sized caller-owned buffer.
    ///
    /// # Errors
    ///
    /// Returns [`DecodeError`] for malformed input or an incorrect buffer size.
    pub fn decode_mut(self, input: &[u8], output: &mut [u8]) -> Result<usize, DecodeError> {
        let decoded = self.decode(input)?;
        if decoded.len() != output.len() {
            return Err(DecodeError);
        }
        output.copy_from_slice(&decoded);
        Ok(decoded.len())
    }
}

fn encode_hex(input: &[u8]) -> String {
    let mut output = String::with_capacity(input.len().saturating_mul(2));
    for &byte in input {
        output.push(char::from(HEX_ALPHABET[usize::from(byte >> 4)]));
        output.push(char::from(HEX_ALPHABET[usize::from(byte & 15)]));
    }
    output
}

fn decode_hex(input: &[u8], permissive: bool) -> Result<Vec<u8>, DecodeError> {
    let (chunks, remainder) = input.as_chunks::<2>();
    let mut output = Vec::with_capacity(input.len() / 2);
    for pair in chunks {
        let high = hex_digit(pair[0], permissive)?;
        let low = hex_digit(pair[1], permissive)?;
        output.push((high << 4) | low);
    }
    if !remainder.is_empty() {
        return Err(DecodeError);
    }
    Ok(output)
}

fn hex_digit(byte: u8, permissive: bool) -> Result<u8, DecodeError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' if permissive => Ok(byte - b'A' + 10),
        _ => Err(DecodeError),
    }
}

fn encode_base64(input: &[u8], alphabet: &[u8; 64], padded: bool) -> String {
    let capacity = input.len().saturating_add(2) / 3 * 4;
    let mut output = String::with_capacity(capacity);
    for chunk in input.chunks(3) {
        let value = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        output.push(char::from(alphabet[((value >> 18) & 63) as usize]));
        output.push(char::from(alphabet[((value >> 12) & 63) as usize]));
        if chunk.len() > 1 {
            output.push(char::from(alphabet[((value >> 6) & 63) as usize]));
        } else if padded {
            output.push('=');
        }
        if chunk.len() > 2 {
            output.push(char::from(alphabet[(value & 63) as usize]));
        } else if padded {
            output.push('=');
        }
    }
    output
}

fn decode_base64(input: &[u8], url_no_pad: bool) -> Result<Vec<u8>, DecodeError> {
    let raw = if url_no_pad {
        if input.contains(&b'=') || input.len() % 4 == 1 {
            return Err(DecodeError);
        }
        input
    } else {
        if !input.len().is_multiple_of(4) {
            return Err(DecodeError);
        }
        let padding = input.iter().rev().take_while(|&&byte| byte == b'=').count();
        if padding > 2 {
            return Err(DecodeError);
        }
        let raw = &input[..input.len() - padding];
        if (padding == 1 && raw.len() % 4 != 3)
            || (padding == 2 && raw.len() % 4 != 2)
            || (padding == 0 && !raw.len().is_multiple_of(4))
        {
            return Err(DecodeError);
        }
        raw
    };
    let output_len = raw.len().checked_mul(6).ok_or(DecodeError)? / 8;
    let mut output = Vec::with_capacity(output_len);
    let mut pending = 0_u16;
    let mut bits = 0_u8;
    for &byte in raw {
        let value = base64_digit(byte, url_no_pad)?;
        pending = (pending << 6) | u16::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(u8::try_from(pending >> bits).map_err(|_| DecodeError)?);
        }
        pending &= (1_u16 << bits) - 1;
    }
    if pending != 0 || output.len() != output_len {
        return Err(DecodeError);
    }
    Ok(output)
}

fn base64_digit(byte: u8, url_no_pad: bool) -> Result<u8, DecodeError> {
    match byte {
        b'A'..=b'Z' => Ok(byte - b'A'),
        b'a'..=b'z' => Ok(byte - b'a' + 26),
        b'0'..=b'9' => Ok(byte - b'0' + 52),
        b'-' if url_no_pad => Ok(62),
        b'_' if url_no_pad => Ok(63),
        b'+' if !url_no_pad => Ok(62),
        b'/' if !url_no_pad => Ok(63),
        _ => Err(DecodeError),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_vectors_and_canonical_rejections() {
        for (raw, standard, url) in [
            (b"".as_slice(), "", ""),
            (b"f".as_slice(), "Zg==", "Zg"),
            (b"fo".as_slice(), "Zm8=", "Zm8"),
            (b"foo".as_slice(), "Zm9v", "Zm9v"),
            (b"foob".as_slice(), "Zm9vYg==", "Zm9vYg"),
            (b"fooba".as_slice(), "Zm9vYmE=", "Zm9vYmE"),
            (b"foobar".as_slice(), "Zm9vYmFy", "Zm9vYmFy"),
            (b"\xfb\xff".as_slice(), "+/8=", "-_8"),
        ] {
            assert_eq!(BASE64.encode(raw), standard);
            assert_eq!(BASE64URL_NOPAD.encode(raw), url);
            assert_eq!(BASE64.decode(standard.as_bytes()), Ok(raw.to_vec()));
            assert_eq!(BASE64URL_NOPAD.decode(url.as_bytes()), Ok(raw.to_vec()));
        }
        for invalid in ["Zg", "Zg=", "Zh==", "Zm9v=", "Zm9v====", "Zg==\n", "-_8="] {
            assert_eq!(BASE64.decode(invalid.as_bytes()), Err(DecodeError));
        }
        for invalid in ["Zg==", "Zh", "Z", "Zm9v=", "Zg+", "Zg/"] {
            assert_eq!(BASE64URL_NOPAD.decode(invalid.as_bytes()), Err(DecodeError));
        }
    }

    #[test]
    fn hex_vectors_and_buffer_sizes() {
        assert_eq!(HEXLOWER.encode(&[0, 15, 16, 255]), "000f10ff");
        assert_eq!(HEXLOWER.decode(b"000f10ff"), Ok(vec![0, 15, 16, 255]));
        assert_eq!(HEXLOWER.decode(b"0F"), Err(DecodeError));
        let mut output = [0_u8; 2];
        assert_eq!(HEXLOWER_PERMISSIVE.decode_mut(b"aF00", &mut output), Ok(2));
        assert_eq!(output, [175, 0]);
        assert_eq!(
            HEXLOWER_PERMISSIVE.decode_mut(b"aa", &mut output),
            Err(DecodeError)
        );
        assert_eq!(HEXLOWER.decode(b"abc"), Err(DecodeError));
    }

    #[test]
    fn binary_round_trips_across_lengths() {
        let mut input = Vec::new();
        let mut next = 0_u8;
        for _ in 0..=256 {
            input.push(next.wrapping_mul(73));
            next = next.wrapping_add(1);
            for encoding in [BASE64, BASE64URL_NOPAD, HEXLOWER] {
                let encoded = encoding.encode(&input);
                assert_eq!(encoding.decode(encoded.as_bytes()), Ok(input.clone()));
            }
        }
    }
}
