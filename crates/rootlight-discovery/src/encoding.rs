//! BOM-backed text admission without changing immutable source bytes.
//!
//! Only a bounded decoded prefix is used for classification. UTF-16 validity
//! is checked through the entire snapshot before any binary exclusion is waived.

use rootlight_cancel::Cancellation;
use rootlight_vfs::VfsError;

use crate::{DiscoveryError, MAX_CLASSIFICATION_BYTES};

pub(super) struct SourceClassification {
    pub(super) encoding: &'static str,
    pub(super) decoded_prefix: Option<String>,
}

pub(super) fn classify(
    content: &[u8],
    cancellation: &Cancellation,
) -> Result<Option<SourceClassification>, DiscoveryError> {
    cancellation.check()?;
    if !matches!(content, [0xff, 0xfe, ..] | [0xfe, 0xff, ..]) {
        let sample = content.get(..content.len().min(MAX_CLASSIFICATION_BYTES));
        return Ok(
            (!sample.is_some_and(|sample| sample.contains(&0))).then_some(SourceClassification {
                encoding: "utf-8",
                decoded_prefix: None,
            }),
        );
    }
    let Some(text) = rootlight_vfs::SourceText::parse(content, cancellation)? else {
        return Ok(None);
    };
    let mut prefix = String::new();
    prefix
        .try_reserve_exact(MAX_CLASSIFICATION_BYTES)
        .map_err(|_| DiscoveryError::Vfs(VfsError::MemoryUnavailable))?;
    let mut original_offset = 2_usize;
    for character in text.chars() {
        original_offset = original_offset
            .checked_add(text.encoding().character_bytes(character))
            .ok_or(DiscoveryError::IncrementalDrift)?;
        if original_offset > MAX_CLASSIFICATION_BYTES
            || prefix.len().saturating_add(character.len_utf8()) > MAX_CLASSIFICATION_BYTES
        {
            break;
        }
        prefix.push(character);
    }
    cancellation.check()?;
    Ok(Some(SourceClassification {
        encoding: text.encoding().as_str(),
        decoded_prefix: Some(prefix),
    }))
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
            assert_eq!(bytes, encoded(text, little_endian));
            let empty = classify(&encoded("", little_endian), &Cancellation::new())
                .unwrap()
                .unwrap();
            assert_eq!(empty.decoded_prefix.as_deref(), Some(""));
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
        assert!(expanded.decoded_prefix.unwrap().len() <= MAX_CLASSIFICATION_BYTES);
    }

    #[test]
    fn legacy_admission_and_cancellation_remain_explicit() {
        for content in [b"fn value() {}".as_slice(), b"invalid \xff", b""] {
            let classified = classify(content, &Cancellation::new()).unwrap().unwrap();
            assert_eq!(classified.encoding, "utf-8");
            assert!(classified.decoded_prefix.is_none());
        }
        let cancellation = Cancellation::with_deadline(std::time::Instant::now());
        assert!(classify(&encoded("hello", true), &cancellation).is_err());
    }
}
