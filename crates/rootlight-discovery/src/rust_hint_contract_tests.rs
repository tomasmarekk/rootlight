//! Original-source regressions for weak Rust content attribution.
//! Exact independently pinned files exercise discovery without claiming a
//! parser, full language support, or any change to source admission.

use super::*;

fn assert_no_rust_content(path: &str, bytes: &[u8], expected_len: usize) {
    assert_eq!(bytes.len(), expected_len);
    let path = RelativePath::parse(Path::new(path)).unwrap();
    let (class, signals) = classify(&path, bytes);
    assert_eq!(class, InputClass::Source);
    assert!(
        !signals.iter().any(|signal| signal.language == "rust"),
        "{signals:?}"
    );
}

#[test]
fn exact_original_gleam_main_is_not_rust_content_evidence() {
    assert_no_rust_content(
        "unrelated/app.gleam",
        include_bytes!("../tests/fixtures/language-hints-original/shared-header.gleam"),
        72,
    );
}

#[test]
fn exact_original_hare_main_is_not_rust_content_evidence() {
    assert_no_rust_content(
        "unrelated/part.ha",
        include_bytes!("../tests/fixtures/language-hints-original/shared-header.ha"),
        968,
    );
}

#[test]
fn exact_original_mojo_main_is_not_rust_content_evidence() {
    assert_no_rust_content(
        "unrelated/test.mojo",
        include_bytes!("../tests/fixtures/language-hints-original/shared-header.mojo"),
        8095,
    );
}

#[test]
fn rust_extension_evidence_is_independent_of_a_conservative_content_hint() {
    for source in [
        b"fn main() {}".as_slice(),
        b"// pub struct Example {}",
        b"pub fn main() { io.println(\"shared\") }",
    ] {
        let (_, signals) = classify(
            &RelativePath::parse(Path::new("source.rs")).unwrap(),
            source,
        );
        assert!(
            signals.iter().any(|signal| signal.language == "rust"
                && signal.evidence == LanguageEvidence::Extension)
        );
    }
}

#[test]
fn extensionless_distinctive_rust_keeps_content_attribution() {
    for source in [
        b"fn main() { println!(\"hello\"); }".as_slice(),
        b"pub struct Example { field: u8 }",
    ] {
        let (_, signals) = classify(&RelativePath::parse(Path::new("entry")).unwrap(), source);
        assert!(signals.iter().any(
            |signal| signal.language == "rust" && signal.evidence == LanguageEvidence::Content
        ));
    }
}
