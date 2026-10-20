//! A root dialect certificate is independent of filenames and parser availability.
//! Complete text validity and bounded classification remain separate from exact
//! source retention and from the native schema analyzer's semantic proof.

use super::*;

const DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

fn schema_signals(path: &str, source: &[u8]) -> Vec<LanguageSignal> {
    classify(&RelativePath::parse(Path::new(path)).unwrap(), source).1
}

fn admitted(signals: &[LanguageSignal]) -> bool {
    signals.iter().any(|signal| {
        signal.language == "json-schema" && signal.evidence == LanguageEvidence::Content
    })
}

#[test]
fn original_schema_has_a_content_certificate_and_keeps_generic_extension_evidence() {
    let original = include_bytes!(
        "../../rootlight-adapter-treesitter/tests/fixtures/json_schema/test-schema.json"
    );
    assert_eq!(original.len(), 5849);
    let signals = schema_signals("arbitrary.json", original);
    assert!(admitted(&signals), "{signals:?}");
    assert!(signals.iter().any(|signal| {
        signal.language == "json" && signal.evidence == LanguageEvidence::Extension
    }));
    for path in ["extensionless", "arbitrary.data", "different.rs"] {
        assert!(admitted(&schema_signals(path, original)), "{path}");
    }
}

#[test]
fn neither_schema_filename_nor_keyword_lookalikes_are_dialect_proof() {
    for source in [
        "{}",
        r#"{"properties":{"a":{}}}"#,
        r#"{"payload":{"$schema":"https://json-schema.org/draft/2020-12/schema"}}"#,
        r#"{"$schema":"http://json-schema.org/draft-07/schema#"}"#,
        r#"{"$schema":"https://example.invalid/2020-12/schema"}"#,
        r#"{"$schema":true}"#,
        r#"[{"$schema":"https://json-schema.org/draft/2020-12/schema"}]"#,
        r#"{"$schema":"https://json-schema.org/draft/2020-12/schema","$sch\u0065ma":"https://json-schema.org/draft/2020-12/schema"}"#,
    ] {
        assert!(
            !admitted(&schema_signals("named.schema.json", source.as_bytes())),
            "{source}"
        );
    }
}

#[test]
fn complete_json_data_cannot_supply_independent_code_hints_from_inside_strings() {
    let source = format!(
        r#"{{"$schema":"{DIALECT}","description":"function result = sample(x)\nclass Fake {{}}\npackage fake;\n(defun pretend () nil)","examples":[{{"properties":{{"not_a_field":{{}}}}}}]}}"#
    );
    for path in [
        "sample.json",
        "sample.cl",
        "sample.lisp",
        "source.mo",
        "source.rs",
    ] {
        let signals = schema_signals(path, source.as_bytes());
        assert!(admitted(&signals));
        assert_eq!(
            signals
                .iter()
                .filter(|signal| signal.evidence == LanguageEvidence::Content)
                .count(),
            1,
            "{signals:?}"
        );
    }
}

#[test]
fn certificates_require_one_strict_complete_json_document() {
    let valid = format!(r#"{{"$schema":"{DIALECT}"}}"#);
    for source in [
        format!("{valid} {{}}"),
        format!("// comment\n{valid}"),
        format!("{valid}\n/* comment */"),
        format!("#!/usr/bin/env python\n{valid}"),
        format!(r#"{{"$schema":"{DIALECT}",}}"#),
        format!(r#"{{"$schema":"{DIALECT}","truncated":"#),
    ] {
        assert!(
            !admitted(&schema_signals("source.json", source.as_bytes())),
            "{source}"
        );
    }
    assert!(!json_schema::has_content_hint(valid.as_bytes(), true));
    let mut long = valid.into_bytes();
    long.resize(MAX_CLASSIFICATION_BYTES + 1, b' ');
    assert!(!admitted(&schema_signals("large.json", &long)));
}

#[test]
fn schema_certificate_does_not_evaluate_or_round_json_numbers() {
    let source = format!(r#"{{"$schema":"{DIALECT}","const":1e999999}}"#);
    assert!(json_schema::has_content_hint(source.as_bytes(), false));
}

#[test]
fn encoded_discovery_certificates_require_whole_text_validity() {
    let text = format!(r#"{{"$schema":"{DIALECT}","description":"λ"}}"#);
    for little in [true, false] {
        let mut bytes = if little {
            vec![0xff, 0xfe]
        } else {
            vec![0xfe, 0xff]
        };
        for unit in text.encode_utf16() {
            bytes.extend_from_slice(&if little {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        let classified = encoding::classify(&bytes, &Cancellation::new())
            .unwrap()
            .unwrap();
        let projected = classified.decoded_prefix.as_ref().unwrap();
        let signals = classify_validated_prefix(
            &RelativePath::parse(Path::new("arbitrary.json")).unwrap(),
            projected.as_bytes(),
            classified.prefix_truncated,
            true,
            true,
        )
        .1;
        assert!(admitted(&signals), "{signals:?}");
        bytes.push(0xff);
        let classified = encoding::classify(&bytes, &Cancellation::new()).unwrap();
        assert!(classified.is_none_or(|classification| classification.encoding == "bytes"));
    }
    let mut invalid = text.into_bytes();
    invalid.push(0xff);
    assert!(!admitted(&schema_signals("invalid.json", &invalid)));
}
