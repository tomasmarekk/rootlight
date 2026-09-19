//! Retains potential Office source containers before text-only classification.
//! Suffix and signature evidence identify an opaque candidate, not a valid
//! package, extracted VBA source, or permission to execute embedded content.

use std::path::Path;

pub(super) fn container_format(path: &str, content: &[u8]) -> Option<&'static str> {
    let extension = Path::new(path).extension()?.to_str()?;
    let macro_package = [
        "xlsm", "xltm", "xlam", "xlsb", "docm", "dotm", "pptm", "potm", "ppam", "ppsm", "sldm",
    ]
    .iter()
    .any(|suffix| extension.eq_ignore_ascii_case(suffix));
    if macro_package && content.starts_with(b"PK\x03\x04") {
        return Some("office-open-xml");
    }
    // Encrypted OOXML packages use a compound envelope. Preserve that evidence
    // without guessing that the encrypted payload contains readable VBA.
    let compound = macro_package
        || [
            "xls", "xlt", "xla", "doc", "dot", "ppt", "pot", "pps", "ppa", "cfb",
        ]
        .iter()
        .any(|suffix| extension.eq_ignore_ascii_case(suffix));
    (compound && content.starts_with(b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"))
        .then_some("compound-document")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suffixes_and_signatures_only_admit_opaque_container_candidates() {
        for extension in [
            "xlsm", "xltm", "xlam", "xlsb", "docm", "dotm", "pptm", "potm", "ppam", "ppsm", "sldm",
        ] {
            for extension in [extension.to_owned(), extension.to_ascii_uppercase()] {
                let path = format!("sub/document.{extension}");
                assert_eq!(
                    container_format(&path, b"PK\x03\x04"),
                    Some("office-open-xml")
                );
                assert_eq!(
                    container_format(&path, b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"),
                    Some("compound-document")
                );
                for bytes in [
                    b"PK\x03".as_slice(),
                    b"\0not an office file",
                    b"ordinary source",
                ] {
                    assert_eq!(container_format(&path, bytes), None);
                }
            }
        }
        for extension in [
            "xls", "xlt", "xla", "doc", "dot", "ppt", "pot", "pps", "ppa", "cfb",
        ] {
            let path = format!("legacy.{extension}");
            assert_eq!(
                container_format(&path, b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"),
                Some("compound-document")
            );
            assert_eq!(container_format(&path, b"PK\x03\x04"), None);
        }
        for path in [
            "document.xlsm.bak",
            "document.zip",
            "image.png",
            "program.rs",
            "plain",
            "document.xlsx",
        ] {
            assert_eq!(container_format(path, b"PK\x03\x04"), None);
            assert_eq!(
                container_format(path, b"\xd0\xcf\x11\xe0\xa1\xb1\x1a\xe1"),
                None
            );
        }
    }
}
