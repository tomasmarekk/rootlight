//! Recognize a complete first-line Racket reader declaration from validated text.
//! The signal identifies the reader family without claiming that its custom
//! language uses base Racket syntax or executing a reader module.

use crate::MAX_CLASSIFICATION_BYTES;

pub(super) fn has_header(content: &[u8], prefix_truncated: bool) -> bool {
    let prefix = &content[..content.len().min(MAX_CLASSIFICATION_BYTES)];
    let line = match prefix.iter().position(|byte| *byte == b'\n') {
        Some(end) => &prefix[..end],
        None if !prefix_truncated && content.len() <= MAX_CLASSIFICATION_BYTES => prefix,
        None => return false,
    };
    let Ok(line) = std::str::from_utf8(line) else {
        return false;
    };
    let line = line.strip_prefix('\u{feff}').unwrap_or(line);
    let line = line.strip_suffix('\r').unwrap_or(line);
    let Some(language) = line.strip_prefix("#lang ") else {
        return false;
    };
    let name_end = language.find(char::is_whitespace).unwrap_or(language.len());
    let name = &language[..name_end];
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"+-_/".contains(&byte))
        && language[name_end..]
            .chars()
            .all(|character| !character.is_control() || character == '\t')
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DiscoveryLimits, DiscoveryPolicy, LanguageEvidence, discover};
    use rootlight_cancel::Cancellation;
    use rootlight_config::{ConfigLayer, ConfigSnapshot, ConfigSource};
    use rootlight_ids::derive_repository;
    use rootlight_vfs::RepositoryRoot;

    #[test]
    fn complete_reader_headers_are_general_and_bounded() {
        for source in [
            "#lang pollen",
            "#lang pollen\nbody",
            "#lang scribble/manual\r\nbody",
            "#lang racket/base\n",
            "#lang s-exp \"custom-reader.rkt\"\n",
            "\u{feff}#lang at-exp racket/base\n",
            "#lang custom+reader_2/sub-reader\n",
        ] {
            assert!(has_header(source.as_bytes(), false), "{source:?}");
        }
        for source in [
            "#language pollen\n",
            "#lang\tpollen\n",
            "#lang  pollen\n",
            "#lang \n",
            "#lang pollen.rkt\n",
            "#lang pollen)\n",
            " #lang pollen\n",
            "; comment\n#lang pollen\n",
            "text #lang pollen\n",
            "#lang pollen\0\n",
            "#lang pollen\rbody\n",
            "#!/usr/bin/perl\n",
        ] {
            assert!(!has_header(source.as_bytes(), false), "{source:?}");
        }
        assert!(!has_header(b"#lang pollen", true));
        assert!(has_header(b"#lang pollen\n", true));
        let mut too_long = b"#lang pollen ".to_vec();
        too_long.resize(MAX_CLASSIFICATION_BYTES + 1, b'x');
        too_long.push(b'\n');
        assert!(!has_header(&too_long, false));
        let mut split_body_scalar = b"#lang pollen\n".to_vec();
        split_body_scalar.resize(MAX_CLASSIFICATION_BYTES - 1, b'x');
        split_body_scalar.extend_from_slice("\u{1f642}".as_bytes());
        assert!(has_header(&split_body_scalar, true));
    }

    fn encoded(source: &str, encoding: &str) -> Vec<u8> {
        match encoding {
            "utf-8" => source.as_bytes().to_vec(),
            "utf-8-bom" => [b"\xef\xbb\xbf".as_slice(), source.as_bytes()].concat(),
            _ => std::iter::once(0xfeff_u16)
                .chain(source.encode_utf16())
                .flat_map(|unit| {
                    if encoding == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect(),
        }
    }

    #[test]
    fn discovery_uses_complete_encoding_validation_and_preserves_perl_evidence() {
        let root_dir = tempfile::tempdir().unwrap();
        let mut expected = Vec::new();
        for (index, encoding) in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"]
            .iter()
            .enumerate()
        {
            for (ending, newline) in [("eof", ""), ("lf", "\n"), ("crlf", "\r\n")] {
                let name = format!("reader-{index}-{ending}.pm");
                let bytes = encoded(&format!("#lang pollen{newline}"), encoding);
                std::fs::write(root_dir.path().join(&name), &bytes).unwrap();
                expected.push(name);
            }
        }
        std::fs::write(
            root_dir.path().join("plain.pm"),
            b"package Plain; use strict;\n1;\n",
        )
        .unwrap();
        let mut invalid = b"#lang pollen\n".to_vec();
        invalid.resize(MAX_CLASSIFICATION_BYTES + 8, b'x');
        invalid.push(0xff);
        std::fs::write(root_dir.path().join("invalid.pm"), invalid).unwrap();
        std::fs::write(
            root_dir.path().join("truncated.pm"),
            [
                b"#lang pollen ".as_slice(),
                &vec![b'x'; MAX_CLASSIFICATION_BYTES],
                b"\n",
            ]
            .concat(),
        )
        .unwrap();
        let repository = derive_repository(b"racket-reader-discovery").id();
        let root = RepositoryRoot::open(repository, root_dir.path()).unwrap();
        let config = ConfigSnapshot::resolve(&[ConfigLayer {
            source: ConfigSource::Defaults,
            contents: "version = \"1.0\"",
        }])
        .unwrap();
        let manifest = discover(
            &root,
            &config,
            &DiscoveryPolicy::build(Vec::new(), false).unwrap(),
            DiscoveryLimits::new(100, 16, 1024 * 1024, 100).unwrap(),
            &Cancellation::new(),
        )
        .unwrap();
        for input in &manifest.inputs {
            let racket = input.language_signals.iter().any(|signal| {
                signal.language == "racket" && signal.evidence == LanguageEvidence::Content
            });
            assert_eq!(racket, expected.contains(&input.path), "{}", input.path);
            assert!(
                input
                    .language_signals
                    .iter()
                    .any(|signal| signal.language == "perl"
                        && signal.evidence == LanguageEvidence::Extension)
            );
        }
        assert_eq!(manifest.inputs.len(), expected.len() + 3);
    }
}
