//! Admit known source bytes separately from strict Unicode text validation.
//! A BOM is an availability marker only; it supplies no decoded content proof.

use rootlight_vfs::RelativePath;

use crate::{
    encoding::SourceClassification, extension_language, filename_language,
    is_ambiguous_basic_source, is_ambiguous_perl_prolog_source, manifest_language,
};

pub(super) fn bom_length(prefix: &[u8]) -> Option<usize> {
    match prefix {
        [0xff, 0xfe, 0, 0, ..] | [0, 0, 0xfe, 0xff, ..] => Some(4),
        [0xef, 0xbb, 0xbf, ..] => Some(3),
        [0xff, 0xfe, ..] | [0xfe, 0xff, ..] => Some(2),
        _ => None,
    }
}

pub(super) fn preserve(
    path: &RelativePath,
    prefix: &[u8],
    configured_source: bool,
) -> Option<SourceClassification> {
    let length = bom_length(prefix)?;
    if !configured_source
        && extension_language(path.as_str()).is_none()
        && !is_ambiguous_basic_source(path.as_str())
        && !is_ambiguous_perl_prolog_source(path.as_str())
        && filename_language(path.as_str()).is_none()
        && manifest_language(&path.as_str().to_ascii_lowercase()).is_none()
    {
        return None;
    }
    // A BOM must not turn an independently recognizable raw container into
    // source text. No container decoding or binary payload inspection occurs.
    let payload = &prefix[length..];
    if payload.starts_with(b"%PDF-")
        || payload.starts_with(b"\x89PNG\r\n\x1a\n")
        || payload.starts_with(b"PK\x03\x04")
        || payload.starts_with(b"PK\x05\x06")
        || payload.starts_with(b"PK\x07\x08")
    {
        return None;
    }
    Some(SourceClassification {
        encoding: "bytes",
        decoded_prefix: None,
        prefix_truncated: true,
    })
}
