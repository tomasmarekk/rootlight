//! MATLAB routing needs a complete header rather than a shared function keyword.

use std::path::Path;

use rootlight_cancel::Cancellation;
use rootlight_vfs::RelativePath;

use crate::{
    LanguageEvidence, MAX_CLASSIFICATION_BYTES, classify, classify_prefix,
    classify_validated_prefix, encoding,
};

const ORIGINAL: &[u8] =
    include_bytes!("../tests/fixtures/matlab_function_routing/SettingsUiState.qml");

fn has_matlab(source: &[u8]) -> bool {
    classify(
        &RelativePath::parse(Path::new("source.txt")).unwrap(),
        source,
    )
    .1
    .iter()
    .any(|signal| signal.language == "matlab" && signal.evidence == LanguageEvidence::Content)
}

#[test]
fn original_qml_braced_functions_do_not_supply_matlab_content() {
    assert_eq!(ORIGINAL.len(), 502);
    assert!(
        ORIGINAL
            .windows(b"function normalizeSelectedBar() {".len())
            .any(|part| part == b"function normalizeSelectedBar() {")
    );
    for path in ["source.qml", "source.js", "source.txt", "source.m"] {
        let signals = classify(&RelativePath::parse(Path::new(path)).unwrap(), ORIGINAL).1;
        assert!(
            !signals.iter().any(|signal| signal.language == "matlab"
                && signal.evidence == LanguageEvidence::Content),
            "{path}: {signals:?}"
        );
    }
}

#[test]
fn matlab_function_header_forms_keep_written_content_evidence() {
    for source in [
        "function y = measure(x)\ny = x;\nend\n",
        "function out = f()\n{1};\nout=1;\nend\n",
        "function [out] = f()\n{1};\nout=1;\nend\n",
        "function [first, second] = measure(x)\nfirst=x;second=x;\nend\n",
        "function [single] = measure(x)\nsingle=x;\nend\n",
        "function measure(x)\ndisp(x);\nend\n",
        "function bare\nend\n",
        "function bare",
        "function measure(x)",
        "function\tout = measure(value)\nout=value;\nend\n",
        "function ... output\nresult = measure(value)\nresult=value;\nend\n",
        "function measure ...\n(x)\ndisp(x);\nend\n",
        "function out = measure(first, ... input\nsecond) % comment\nout=first+second;\nend\n",
        "function measure(~, value)\ndisp(value);\nend\n",
        "function measure(x), disp(x); end\n",
    ] {
        assert!(has_matlab(source.as_bytes()), "{source:?}");
    }
}

#[test]
fn empty_matlab_output_list_requires_assignment_and_function_name() {
    for source in [
        "function [] = f()\ndisp(1);\nend\n",
        "function [] = f(value)\ndisp(value);\nend\n",
        "function [] = f()\n{1};\nend\n",
    ] {
        assert!(has_matlab(source.as_bytes()), "{source:?}");
    }
    for source in [
        "function [] f()\nend\n",
        "function [] = ()\nend\n",
        "function [] =\nend\n",
    ] {
        assert!(!has_matlab(source.as_bytes()), "{source:?}");
    }
}

#[test]
fn shared_brace_headers_and_noncode_do_not_prove_matlab() {
    for source in [
        "function measure(x) { return x; }\n",
        "function measure(x)\n{ return x; }\n",
        // This is valid MATLAB too, but the no-output header plus brace is
        // shared with JavaScript. Withholding is an ambiguity decision.
        "function measure()\n{1};\nend\n",
        "function measure(x)\n/* boundary */\n{ return x; }\n",
        "function measure(x)\n// boundary\n{ return x; }\n",
        "function measure(x = 1) { return x; }\n",
        "function measure(x: number): number { return x; }\n",
        "function *measure(x) { yield x; }\n",
        "function [out] = measure(x) { return x; }\n",
        "function wrong-name(x)\nend\n",
        "function measure(x, )\nend\n",
        "const text = \"function measure(x) {\";\n",
        "text = 'function measure(x)';\n",
        "// function out = measure(x)\n",
        "/*\nfunction out = measure(x)\n*/\n",
        "% function out = measure(x)\n",
        "%{\nfunction out = measure(x)\n%}\n",
        "\"unclosed\nfunction out = measure(x)\n",
        "/* unclosed\nfunction out = measure(x)\n",
    ] {
        assert!(!has_matlab(source.as_bytes()), "{source:?}");
    }
}

#[test]
fn matlab_function_proof_does_not_treat_a_bounded_prefix_as_eof() {
    let path = RelativePath::parse(Path::new("source.txt")).unwrap();
    for source in [
        b"function measure".as_slice(),
        b"function measure(x)",
        b"function measure(x)\n",
        b"function out = measure(x",
        b"/* function out = measure(x)",
        b"\"function out = measure(x)",
    ] {
        let signals = classify_prefix(&path, source, true, false).1;
        assert!(
            !signals.iter().any(|signal| signal.language == "matlab"),
            "{source:?}: {signals:?}"
        );
    }
    let header = b"function measure(x)";
    let mut source = vec![b' '; MAX_CLASSIFICATION_BYTES - header.len()];
    source.extend_from_slice(header);
    source.extend_from_slice(b" { return x; }\n");
    assert!(!has_matlab(&source));
    let mut outside = vec![b' '; MAX_CLASSIFICATION_BYTES];
    outside.extend_from_slice(b"function y = measure(x)\nend\n");
    assert!(!has_matlab(&outside));
}

#[test]
fn qml_and_matlab_headers_share_the_existing_validated_encoding_frontier() {
    let original = std::str::from_utf8(ORIGINAL).unwrap();
    let positive = "function y = measure(x)\ny=x;\nend\n";
    for (source, expected) in [(original, false), (positive, true)] {
        for mode in ["utf-8-bom", "utf-16le", "utf-16be"] {
            let mut bytes = if mode == "utf-8-bom" {
                vec![0xef, 0xbb, 0xbf]
            } else if mode == "utf-16le" {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            if mode == "utf-8-bom" {
                bytes.extend_from_slice(source.as_bytes());
            } else {
                for unit in source.encode_utf16() {
                    bytes.extend_from_slice(&if mode == "utf-16le" {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    });
                }
            }
            let classified = encoding::classify(&bytes, &Cancellation::new())
                .unwrap()
                .unwrap();
            let prefix = classified
                .decoded_prefix
                .as_ref()
                .map_or(bytes.as_slice(), |text| text.as_bytes());
            let signals = classify_validated_prefix(
                &RelativePath::parse(Path::new("source.txt")).unwrap(),
                prefix,
                classified.prefix_truncated,
                classified.decoded_prefix.is_some(),
                matches!(classified.encoding, "utf-8" | "utf-16le" | "utf-16be"),
            )
            .1;
            assert_eq!(
                signals.iter().any(|signal| signal.language == "matlab"
                    && signal.evidence == LanguageEvidence::Content),
                expected,
                "{mode}"
            );
        }
    }
}
