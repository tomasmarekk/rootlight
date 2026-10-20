//! QML extension evidence cannot acquire MATLAB merely from its methods.

use super::*;

#[test]
fn qml_methods_do_not_create_matlab_content_evidence() {
    let source = b"import QtQuick\nItem { id: root; function method(value) { return value; } }";
    for name in ["ui/View.qml", "ui/View.QML"] {
        let path = RelativePath::parse(Path::new(name)).unwrap();
        let (_, signals) = classify(&path, source);
        assert!(signals.iter().any(
            |signal| signal.language == "qml" && signal.evidence == LanguageEvidence::Extension
        ));
        assert!(!signals.iter().any(|signal| signal.language == "matlab"));
    }
    let matlab = RelativePath::parse(Path::new("method.m")).unwrap();
    let (_, signals) = classify(
        &matlab,
        b"function value = method(input)\nvalue = input;\nend\n",
    );
    assert!(
        signals
            .iter()
            .any(|signal| signal.language == "matlab"
                && signal.evidence == LanguageEvidence::Content)
    );
}

#[test]
fn qml_extension_does_not_erase_independent_shebang_evidence_or_decode_binary() {
    let path = RelativePath::parse(Path::new("View.qml")).unwrap();
    let (_, signals) = classify(&path, b"#!/usr/bin/env python\nfunction token() {}\n");
    assert!(
        signals.iter().any(
            |signal| signal.language == "qml" && signal.evidence == LanguageEvidence::Extension
        )
    );
    assert!(
        signals
            .iter()
            .any(|signal| signal.language == "python"
                && signal.evidence == LanguageEvidence::Shebang)
    );
    let (_, signals) = classify(&path, b"\xff\xfe\0\xff");
    assert!(
        !signals
            .iter()
            .any(|signal| signal.evidence == LanguageEvidence::Content)
    );
    assert_eq!(extension_language("View.qml.backup"), None);
}
