//! Shared `.mo` suffixes need bounded written Modelica evidence.
//! These discovery controls also preserve complete encoding validation and
//! immutable physical source when language admission remains unresolved.

use super::*;
use rootlight_config::{ConfigLayer, ConfigSource};
use rootlight_ids::derive_repository;

#[test]
fn complete_within_headers_survive_bounded_prefix_truncation() {
    let original = include_bytes!("../tests/fixtures/modelica-long-header/CondDwInc.mo");
    assert_eq!(original.len(), 8262);
    assert!(has_modelica("original.mo", original));
    for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
        let source = format!(
            "within Root; model Example\n Real x;\n{}end Example;",
            " ".repeat(MAX_CLASSIFICATION_BYTES)
        );
        let bytes = encoded(&source, encoding);
        let classified = encoding::classify(&bytes, &Cancellation::new())
            .unwrap()
            .unwrap();
        assert!(classified.prefix_truncated, "{encoding}");
        let projected = classified
            .decoded_prefix
            .as_ref()
            .map_or(bytes.as_slice(), |prefix| prefix.as_bytes());
        let signals = classify_validated_prefix(
            &RelativePath::parse(Path::new("long.mo")).unwrap(),
            projected,
            classified.prefix_truncated,
            classified.decoded_prefix.is_some(),
            matches!(classified.encoding, "utf-8" | "utf-16le" | "utf-16be"),
        )
        .1;
        assert!(
            signals.iter().any(|signal| signal.language == "modelica"
                && signal.evidence == LanguageEvidence::Content),
            "{encoding}: {signals:?}"
        );
    }
}

#[test]
fn truncated_modelica_certificates_require_complete_header_tokens() {
    for prefix in [
        "within Root; model Example ",
        "within; pure function evaluate\n",
        "within 'quoted root'; model 'quoted model'\n",
        "within Root; model Example\nReal x; /* later body comment",
        "within Root; model Example\nReal x; annotation(Documentation(info=\"later body string",
    ] {
        assert!(
            modelica::has_content_hint(prefix.as_bytes(), true),
            "{prefix:?}"
        );
    }
    for prefix in [
        "within",
        "within Root",
        "within Root.; model Example ",
        "within Root; mod",
        "within Root; model Exam",
        "within Root; model 'quoted model",
        "within 'quoted root; model Example ",
        "within Root; /* model Example ",
        "within Root; // model Example ",
        "within Root; \"model Example ",
        "model Example end Example; ",
    ] {
        assert!(
            !modelica::has_content_hint(prefix.as_bytes(), true),
            "{prefix:?}"
        );
    }
    let mut outside = vec![b' '; MAX_CLASSIFICATION_BYTES];
    outside.extend_from_slice(b"within Root; model Example ");
    assert!(!has_modelica("outside.mo", &outside));
    let mut split_scalar = b"within Root; model Example\nReal x; ".to_vec();
    split_scalar.resize(MAX_CLASSIFICATION_BYTES - 1, b' ');
    split_scalar.extend_from_slice("λtail".as_bytes());
    assert!(has_modelica("split.mo", &split_scalar));
    split_scalar.push(0xff);
    assert!(!has_modelica("invalid-tail.mo", &split_scalar));
}

#[test]
fn long_modelica_headers_preserve_independent_content_and_shebang_evidence() {
    let source = format!(
        "within Root; model Example end Example;\nfunction result = other(x)\nend\n{}",
        " ".repeat(MAX_CLASSIFICATION_BYTES)
    );
    let signals = classify(
        &RelativePath::parse(Path::new("conflict.mo")).unwrap(),
        source.as_bytes(),
    )
    .1;
    for language in ["modelica", "matlab"] {
        assert!(
            signals.iter().any(|signal| signal.language == language
                && signal.evidence == LanguageEvidence::Content),
            "{signals:?}"
        );
    }
    let shebang = format!("#!/usr/bin/env python\n{source}");
    let signals = classify(
        &RelativePath::parse(Path::new("shebang.mo")).unwrap(),
        shebang.as_bytes(),
    )
    .1;
    assert!(
        signals
            .iter()
            .any(|signal| signal.language == "python"
                && signal.evidence == LanguageEvidence::Shebang)
    );
    assert!(!signals.iter().any(|signal| signal.language == "modelica"));
}

#[test]
fn original_modelica_function_header_is_not_an_independent_matlab_declaration() {
    let original =
        include_bytes!("../../rootlight-adapter-treesitter/tests/fixtures/modelica/U_sf.mo");
    assert_eq!(original.len(), 238);
    let signals = classify(
        &RelativePath::parse(Path::new("original.mo")).unwrap(),
        original,
    )
    .1;
    assert!(signals.iter().any(
        |signal| signal.language == "modelica" && signal.evidence == LanguageEvidence::Content
    ));
    assert!(
        !signals
            .iter()
            .any(|signal| signal.language == "matlab"
                && signal.evidence == LanguageEvidence::Content),
        "{signals:?}"
    );
}

#[test]
fn modelica_ordinary_and_nested_functions_do_not_borrow_a_matlab_keyword_hint() {
    for source in [
        "within Root;\nfunction evaluate\n input Real x;\n output Real y;\nalgorithm\ny := x;\nend evaluate;",
        "function evaluate \"description\"\n input Real x;\n output Real y;\nalgorithm\ny := x;\nend evaluate;",
        "within Root;\npackage Nested\n function evaluate \"description\"\n input Real x;\n output Real y;\nalgorithm\ny := x;\nend evaluate;\nend Nested;",
        "within Root;\nmodel Example\n String text = \"function y = fake(x)\";\n // classdef Fake\nend Example;",
    ] {
        let signals = classify(
            &RelativePath::parse(Path::new("general.mo")).unwrap(),
            source.as_bytes(),
        )
        .1;
        assert!(
            signals.iter().any(|signal| signal.language == "modelica"
                && signal.evidence == LanguageEvidence::Content),
            "{source:?}: {signals:?}"
        );
        assert!(
            !signals.iter().any(|signal| signal.language == "matlab"
                && signal.evidence == LanguageEvidence::Content),
            "{source:?}: {signals:?}"
        );
    }
}

#[test]
fn distinctive_matlab_headers_remain_independent_modelica_conflicts() {
    for header in [
        "classdef Other\nend\n",
        "function y = other(x)\ny = x;\nend\n",
        "function other(x)\ndisp(x);\nend\n",
        "function [first, second] = other(x)\nfirst=x;second=x;\nend\n",
        "function [single] = other(x)\nsingle=x;\nend\n",
        "function other ...\n(x)\ndisp(x);\nend\n",
    ] {
        let source = format!("within Root;\nfunction evaluate\nend evaluate;\n{header}");
        let signals = classify(
            &RelativePath::parse(Path::new("conflict.mo")).unwrap(),
            source.as_bytes(),
        )
        .1;
        for language in ["matlab", "modelica"] {
            assert!(
                signals.iter().any(|signal| signal.language == language
                    && signal.evidence == LanguageEvidence::Content),
                "{source:?}: {signals:?}"
            );
        }
    }
    for source in [
        "function bare\nend\n",
        "function y = other(x)\nend\n",
        "classdef Other\nend\n",
    ] {
        assert_eq!(
            content_language(source.as_bytes()),
            Some("matlab"),
            "ordinary path keeps its established hint: {source:?}"
        );
        let signals = classify(
            &RelativePath::parse(Path::new("unproved.mo")).unwrap(),
            source.as_bytes(),
        )
        .1;
        assert!(signals.iter().any(|signal| signal.language == "matlab"));
        assert!(!signals.iter().any(|signal| signal.language == "modelica"));
    }
}

#[test]
fn original_modelica_function_uses_the_same_validated_encoding_frontier() {
    let original = std::str::from_utf8(include_bytes!(
        "../../rootlight-adapter-treesitter/tests/fixtures/modelica/U_sf.mo"
    ))
    .unwrap();
    for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
        let bytes = encoded(original, encoding);
        let classified = encoding::classify(&bytes, &Cancellation::new())
            .unwrap()
            .unwrap();
        let projected = classified
            .decoded_prefix
            .as_ref()
            .map_or(bytes.as_slice(), |prefix| prefix.as_bytes());
        let signals = classify_validated_prefix(
            &RelativePath::parse(Path::new("original.mo")).unwrap(),
            projected,
            classified.prefix_truncated,
            classified.decoded_prefix.is_some(),
            matches!(classified.encoding, "utf-8" | "utf-16le" | "utf-16be"),
        )
        .1;
        assert_eq!(
            signals,
            vec![LanguageSignal {
                language: "modelica".to_owned(),
                evidence: LanguageEvidence::Content
            }],
            "{encoding}"
        );
    }
    for source in [
        "within Root;\nfunction evaluate \"unterminated",
        "within Root;\nfunction evaluate\n/* unterminated",
        "within Root;\nfunction",
        "within Root;\n// function evaluate\n",
    ] {
        assert!(
            !has_modelica("unproved.mo", source.as_bytes()),
            "{source:?}"
        );
    }
    let mut truncated = original.as_bytes().to_vec();
    truncated.resize(MAX_CLASSIFICATION_BYTES + 1, b' ');
    assert!(has_modelica("truncated.mo", &truncated));
    truncated.push(0xff);
    assert!(!has_modelica("invalid.mo", &truncated));
}

fn has_modelica(path: &str, source: &[u8]) -> bool {
    classify(&RelativePath::parse(Path::new(path)).unwrap(), source)
        .1
        .iter()
        .any(|signal| signal.language == "modelica" && signal.evidence == LanguageEvidence::Content)
}

#[test]
fn modelica_capability_keeps_shared_suffix_separate_from_written_proof() {
    assert_eq!(canonical_language("modelica"), Some("modelica"));
    assert_eq!(extension_language("library/PART.MO"), Some("modelica"));
    let capability = language_capabilities()
        .iter()
        .find(|row| row.language == "modelica")
        .unwrap();
    assert_eq!(capability.maximum_tier, "tier_d");
    assert_eq!(capability.analyzers, &["treesitter"]);
    assert!(capability.detectors.contains(&"content"));
    for source in [
        b"".as_slice(),
        b"// model Example end Example;\n",
        b"actor {}",
    ] {
        assert!(
            classify(
                &RelativePath::parse(Path::new("unknown.mo")).unwrap(),
                source
            )
            .1
            .iter()
            .all(|signal| signal.language != "modelica")
        );
    }
}

#[test]
fn modelica_headers_need_a_within_declaration_or_matching_written_end() {
    for source in [
        "within; model Example\n Real x;\nend Example;",
        "within Root.Sub; partial package Library\nend Library;",
        "/* license */ within Root; // location\npure function evaluate\nend evaluate;",
        "model Example\n Real x;\nend Example;",
        "encapsulated partial package Library\nend Library;",
        "expandable connector Bus\nend Bus;",
        "operator record Value\nend Value;",
        "impure function evaluate\nend evaluate;",
        "within 'quoted package'.Nested; model 'quoted model' end 'quoted model';",
        "\u{feff}within Root; model Example\r\nend Example;",
        "// comment\rwithin Root; model Example\rend Example;",
        "model Example String text = \"quoted \\\"end Other;\\\"\"; end Example;",
    ] {
        assert!(
            has_modelica("arbitrary.mo", source.as_bytes()),
            "{source:?}"
        );
        assert!(
            has_modelica("arbitrary.MO", source.as_bytes()),
            "{source:?}"
        );
        assert!(!has_modelica("arbitrary.mo.txt", source.as_bytes()));
    }
    for source in [
        "within;",
        "within Root; // no declaration",
        "within Root model Example end Example;",
        "within Root.; model Example end Example;",
        "within Root; model",
        "model Example",
        "model Example end Other;",
        "model Example end Example",
        "// model Example end Example;",
        "/* model Example end Example; */",
        "\"model Example end Example;\"",
        "model Example \"end Example;\"",
        "model Example /* end Example; */",
        "model Example /* unterminated",
        "model Example \"unterminated",
        "model Example end Example; /* unterminated",
        "within Root; modelish Example end Example;",
        "within Root; model 123;",
        "actor Example {}",
        "class Example() {}",
        "let text = \"within; model X\";",
    ] {
        assert!(
            !has_modelica("arbitrary.mo", source.as_bytes()),
            "{source:?}"
        );
    }
}

#[test]
fn modelica_prefixes_require_complete_headers_and_valid_text() {
    let mut too_long = b"within Root; model Example end Example;\n".to_vec();
    too_long.resize(MAX_CLASSIFICATION_BYTES + 1, b' ');
    assert!(has_modelica("long.mo", &too_long));
    let mut outside = vec![b' '; MAX_CLASSIFICATION_BYTES];
    outside.extend_from_slice(b"within Root; model Example end Example;");
    assert!(!has_modelica("outside.mo", &outside));
    too_long.push(0xff);
    assert!(!has_modelica("invalid.mo", &too_long));
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
fn modelica_discovery_preserves_encodings_bytes_and_unproved_sources() {
    let directory = tempfile::tempdir().unwrap();
    let mut cases = BTreeMap::new();
    for encoding in ["utf-8", "utf-8-bom", "utf-16le", "utf-16be"] {
        let source = "within Root; model Example\r\n String label = \"λ𝄞\";\r\nend Example;";
        cases.insert(format!("{encoding}.mo"), (encoded(source, encoding), true));
    }
    cases.insert(
        "comment.mo".to_owned(),
        (b"// model Example end Example;".to_vec(), false),
    );
    cases.insert(
        "motoko.mo".to_owned(),
        (
            b"actor { public func value() : async Nat { 1 }; }".to_vec(),
            false,
        ),
    );
    cases.insert("empty.mo".to_owned(), (Vec::new(), false));
    for encoding in ["utf-8", "utf-16le", "utf-16be"] {
        let source = format!(
            "within; model Example end Example;\n{}",
            " ".repeat(MAX_CLASSIFICATION_BYTES)
        );
        cases.insert(
            format!("long-{encoding}.mo"),
            (encoded(&source, encoding), true),
        );
    }
    let mut invalid = b"within; model Example end Example;\n".to_vec();
    invalid.resize(MAX_CLASSIFICATION_BYTES + 8, b' ');
    invalid.push(0xff);
    cases.insert("invalid-tail.mo".to_owned(), (invalid, false));
    for encoding in ["utf-16le", "utf-16be"] {
        let mut source = encoded("within; model Example end Example;", encoding);
        source.extend_from_slice(&if encoding == "utf-16le" {
            0xd800_u16.to_le_bytes()
        } else {
            0xd800_u16.to_be_bytes()
        });
        std::fs::write(
            directory.path().join(format!("malformed-{encoding}.mo")),
            source,
        )
        .unwrap();
    }
    for (name, (bytes, _)) in &cases {
        std::fs::write(directory.path().join(name), bytes).unwrap();
    }
    std::fs::write(
        directory.path().join("binary.mo"),
        b"within; model Example\0",
    )
    .unwrap();
    let root = RepositoryRoot::open(
        derive_repository(b"modelica-discovery-contract").id(),
        directory.path(),
    )
    .unwrap();
    let config = ConfigSnapshot::resolve(&[ConfigLayer {
        source: ConfigSource::Defaults,
        contents: "version = \"1.0\"",
    }])
    .unwrap();
    let result = discover_with_snapshots(
        &root,
        &config,
        &DiscoveryPolicy::build(Vec::new(), true).unwrap(),
        DiscoveryLimits::new(100, 16, 1024 * 1024, 100).unwrap(),
        BTreeMap::new(),
        &Cancellation::new(),
    )
    .unwrap();
    let (manifest, snapshots) = result.into_parts();
    assert_eq!(manifest.inputs.len(), cases.len());
    assert_eq!(manifest.exclusions.len(), 3);
    assert!(
        manifest
            .exclusions
            .iter()
            .all(|entry| entry.reason == ExclusionReason::Binary)
    );
    for input in &manifest.inputs {
        let (bytes, expected) = &cases[&input.path];
        assert_eq!(input.content_hash, rootlight_ids::content_hash(bytes));
        assert_eq!(input.bytes, u64::try_from(bytes.len()).unwrap());
        assert_eq!(snapshots[&input.file].content(), bytes);
        assert_eq!(
            input
                .language_signals
                .iter()
                .any(|signal| signal.language == "modelica"),
            *expected,
            "{}",
            input.path
        );
        if input.path == "invalid-tail.mo" {
            assert_eq!(input.encoding, "bytes");
        }
        if input.path == "utf-16le.mo" {
            assert_eq!(input.encoding, "utf-16le");
        }
        if input.path == "utf-16be.mo" {
            assert_eq!(input.encoding, "utf-16be");
        }
    }
}

#[test]
fn modelica_content_does_not_erase_an_independent_conflicting_signal() {
    let signals = classify(
        &RelativePath::parse(Path::new("conflict.mo")).unwrap(),
        b"within Root; model Example\npub struct Conflict { field: u8 }\nend Example;",
    )
    .1;
    for language in ["modelica", "rust"] {
        assert!(
            signals.iter().any(|signal| signal.language == language
                && signal.evidence == LanguageEvidence::Content)
        );
    }
}
