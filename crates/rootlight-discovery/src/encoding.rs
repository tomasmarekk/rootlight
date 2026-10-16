//! Strict text admission without changing immutable source bytes.
//!
//! Only a bounded prefix is used for language classification. Complete Unicode
//! validation distinguishes raw source bytes from supported text encodings.

use rootlight_cancel::Cancellation;
use rootlight_vfs::VfsError;

use crate::{DiscoveryError, MAX_CLASSIFICATION_BYTES};

pub(super) struct SourceClassification {
    pub(super) encoding: &'static str,
    pub(super) decoded_prefix: Option<String>,
    pub(super) prefix_truncated: bool,
}

pub(super) fn classify(
    content: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<SourceClassification>, DiscoveryError> {
    cancellation.check()?;
    if !matches!(content, [0xff, 0xfe, ..] | [0xfe, 0xff, ..]) {
        // Binary containers can have a NUL-free prefix; suffixes do not prove text.
        let binary_signature = content.starts_with(b"%PDF-")
            || content.starts_with(b"\x89PNG\r\n\x1a\n")
            || content.starts_with(b"PK\x03\x04")
            || content.starts_with(b"PK\x05\x06")
            || content.starts_with(b"PK\x07\x08");
        let sample = content.get(..content.len().min(MAX_CLASSIFICATION_BYTES));
        if binary_signature || sample.is_some_and(|sample| sample.contains(&0)) {
            return Ok(None);
        }
        let mut validator = rootlight_vfs::StreamingSourceTextValidator::new();
        validator.push(content, cancellation)?;
        return Ok(Some(SourceClassification {
            encoding: validator
                .finish()
                .map_or("bytes", |encoding| encoding.as_str()),
            decoded_prefix: None,
            prefix_truncated: content.len() > MAX_CLASSIFICATION_BYTES,
        }));
    }
    let Some(text) = rootlight_vfs::SourceText::parse(content, cancellation)? else {
        return Ok(None);
    };
    let mut prefix = String::new();
    prefix
        .try_reserve_exact(MAX_CLASSIFICATION_BYTES)
        .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
    let mut original_offset = 2_usize;
    // Either byte budget can stop decoding below the UTF-8 sample length;
    // downstream evidence must not mistake that prefix for a true EOF.
    let mut prefix_truncated = false;
    for character in text.chars() {
        original_offset = original_offset
            .checked_add(text.encoding().character_bytes(character))
            .ok_or(DiscoveryError::IncrementalDrift)?;
        if original_offset > MAX_CLASSIFICATION_BYTES
            || prefix.len().saturating_add(character.len_utf8()) > MAX_CLASSIFICATION_BYTES
        {
            prefix_truncated = true;
            break;
        }
        prefix.push(character);
    }
    cancellation.check()?;
    Ok(Some(SourceClassification {
        encoding: text.encoding().as_str(),
        decoded_prefix: Some(prefix),
        prefix_truncated,
    }))
}

pub(super) fn classify_streamed(
    prefix: &[u8],
    full_length: u64,
    encoding: Option<rootlight_vfs::TextEncoding>,
    contains_nul: bool,
    cancellation: &Cancellation,
) -> Result<Option<SourceClassification>, DiscoveryError> {
    cancellation.check()?;
    if contains_nul {
        return Ok(None);
    }
    let Some(encoding) = encoding else {
        return Ok(
            classify(prefix, cancellation)?.map(|_| SourceClassification {
                encoding: "bytes",
                decoded_prefix: None,
                prefix_truncated: true,
            }),
        );
    };
    // The complete stream is already valid. A bounded physical prefix may end
    // inside its final scalar, which is excluded from classification evidence.
    for removed in 0..=4.min(prefix.len()) {
        let bounded = &prefix[..prefix.len() - removed];
        let Some(text) = rootlight_vfs::SourceText::parse(bounded, cancellation)? else {
            continue;
        };
        if text.encoding() != encoding {
            continue;
        }
        let Some(mut result) = classify(bounded, cancellation)? else {
            return Ok(None);
        };
        result.prefix_truncated |= u64::try_from(bounded.len()).unwrap_or(u64::MAX) < full_length;
        return Ok(Some(result));
    }
    Err(DiscoveryError::IncrementalDrift)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(source: &str, little_endian: bool) -> Vec<u8> {
        std::iter::once(0xfeff_u16)
            .chain(source.encode_utf16())
            .flat_map(|unit| {
                if little_endian {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                }
            })
            .collect()
    }

    #[test]
    fn both_orders_validate_surrogates_and_retain_an_exact_text_prefix() {
        let text = "// λ𝄞\r\nfn value() {}\r\n";
        for (little_endian, label) in [(true, "utf-16le"), (false, "utf-16be")] {
            let bytes = encoded(text, little_endian);
            let classified = classify(&bytes, &Cancellation::new()).unwrap().unwrap();
            assert_eq!(classified.encoding, label);
            assert_eq!(classified.decoded_prefix.as_deref(), Some(text));
            assert!(!classified.prefix_truncated);
            assert_eq!(bytes, encoded(text, little_endian));
            let empty = classify(&encoded("", little_endian), &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(empty.decoded_prefix.as_deref(), Some(""));
            assert!(!empty.prefix_truncated);
        }
    }

    #[test]
    fn malformed_units_nuls_and_utf32_do_not_override_binary_exclusions() {
        for source in [
            vec![0xff, 0xfe, 0x61],
            vec![0xff, 0xfe, 0, 0xd8],
            vec![0xfe, 0xff, 0xdc, 0],
            vec![0xff, 0xfe, 0, 0xd8, 0x61, 0],
            vec![0xff, 0xfe, 0, 0, 0x61, 0, 0, 0],
            vec![0, 0, 0xfe, 0xff, 0, 0, 0, 0x61],
            encoded("before\0after", true),
            encoded("before\0after", false),
            vec![b'a', 0, b'b', 0],
        ] {
            assert!(
                classify(&source, &Cancellation::new()).unwrap().is_none(),
                "{source:?}"
            );
        }
    }

    #[test]
    fn validation_reaches_beyond_the_bounded_classification_prefix() {
        let text = "a".repeat(MAX_CLASSIFICATION_BYTES);
        for little_endian in [true, false] {
            let mut bytes = encoded(&text, little_endian);
            let classified = classify(&bytes, &Cancellation::new()).unwrap().unwrap();
            assert!(classified.prefix_truncated);
            assert_eq!(
                classified.decoded_prefix.unwrap().len(),
                (MAX_CLASSIFICATION_BYTES - 2) / 2
            );
            bytes.extend_from_slice(&if little_endian {
                0xd800_u16.to_le_bytes()
            } else {
                0xd800_u16.to_be_bytes()
            });
            assert!(classify(&bytes, &Cancellation::new()).unwrap().is_none());
        }
        let expanded = classify(
            &encoded(&"界".repeat(MAX_CLASSIFICATION_BYTES), true),
            &Cancellation::new(),
        )
        .unwrap()
        .unwrap();
        assert!(expanded.prefix_truncated);
        assert!(expanded.decoded_prefix.unwrap().len() <= MAX_CLASSIFICATION_BYTES);
    }

    #[test]
    fn bounded_prefix_completeness_distinguishes_true_eof_and_split_scalars() {
        let complete = "a".repeat((MAX_CLASSIFICATION_BYTES - 2) / 2);
        for little_endian in [true, false] {
            let classified = classify(&encoded(&complete, little_endian), &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(
                classified.decoded_prefix.as_deref(),
                Some(complete.as_str())
            );
            assert!(!classified.prefix_truncated);

            let mut incomplete = complete.clone();
            incomplete.pop();
            let expected = incomplete.clone();
            incomplete.push('𝑥');
            let classified = classify(&encoded(&incomplete, little_endian), &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(
                classified.decoded_prefix.as_deref(),
                Some(expected.as_str())
            );
            assert!(classified.prefix_truncated);
        }
    }

    #[test]
    fn streamed_classification_keeps_known_encoding_across_split_prefix_scalars() {
        let cancellation = Cancellation::new();
        let utf8 = format!("{}😀tail", "a".repeat(MAX_CLASSIFICATION_BYTES - 1));
        let classified = classify_streamed(
            &utf8.as_bytes()[..MAX_CLASSIFICATION_BYTES],
            u64::try_from(utf8.len()).unwrap(),
            Some(rootlight_vfs::TextEncoding::Utf8),
            false,
            &cancellation,
        )
        .unwrap()
        .unwrap();
        assert_eq!(classified.encoding, "utf-8");
        assert!(classified.prefix_truncated);
        for little_endian in [true, false] {
            let expected = "a".repeat((MAX_CLASSIFICATION_BYTES - 4) / 2);
            let source = encoded(&format!("{expected}😀tail"), little_endian);
            let encoding = if little_endian {
                rootlight_vfs::TextEncoding::Utf16Le
            } else {
                rootlight_vfs::TextEncoding::Utf16Be
            };
            let classified = classify_streamed(
                &source[..MAX_CLASSIFICATION_BYTES],
                u64::try_from(source.len()).unwrap(),
                Some(encoding),
                false,
                &cancellation,
            )
            .unwrap()
            .unwrap();
            assert_eq!(classified.encoding, encoding.as_str());
            assert_eq!(
                classified.decoded_prefix.as_deref(),
                Some(expected.as_str())
            );
            assert!(classified.prefix_truncated);
        }
    }

    #[test]
    fn binary_format_signatures_do_not_require_a_nul_byte() {
        for content in [
            b"%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n".as_slice(),
            b"\x89PNG\r\n\x1a\nopaque payload",
            b"PK\x03\x04opaque archive",
            b"PK\x05\x06empty archive",
            b"PK\x07\x08spanned archive",
        ] {
            assert!(!content.contains(&0));
            assert!(classify(content, &Cancellation::new()).unwrap().is_none());
        }
        for content in [
            b"let header = \"%PDF-1.7\";".as_slice(),
            b"// PK\x03\x04 is an archive signature",
            b"%PDF",
            b"PK\x03",
        ] {
            assert!(classify(content, &Cancellation::new()).unwrap().is_some());
        }
        for little_endian in [true, false] {
            assert!(
                classify(&encoded("%PDF-1.7", little_endian), &Cancellation::new())
                    .unwrap()
                    .is_some()
            );
        }
    }

    #[test]
    fn legacy_admission_and_cancellation_remain_explicit() {
        for content in [b"fn value() {}".as_slice(), b""] {
            let classified = classify(content, &Cancellation::new()).unwrap().unwrap();
            assert_eq!(classified.encoding, "utf-8");
            assert!(classified.decoded_prefix.is_none());
            assert!(!classified.prefix_truncated);
        }
        let cancellation = Cancellation::with_deadline(std::time::Instant::now());
        assert!(classify(&encoded("hello", true), &cancellation).is_err());
    }

    #[test]
    fn malformed_utf8_remains_raw_source_even_beyond_the_language_sample() {
        let mut beyond = vec![b'a'; MAX_CLASSIFICATION_BYTES + 1];
        beyond.push(0xff);
        for content in [b"invalid \xff".as_slice(), beyond.as_slice()] {
            let classified = classify(content, &Cancellation::new()).unwrap().unwrap();
            assert_eq!(classified.encoding, "bytes");
            assert!(classified.decoded_prefix.is_none());
            assert_eq!(
                classified.prefix_truncated,
                content.len() > MAX_CLASSIFICATION_BYTES
            );
        }
    }
}
