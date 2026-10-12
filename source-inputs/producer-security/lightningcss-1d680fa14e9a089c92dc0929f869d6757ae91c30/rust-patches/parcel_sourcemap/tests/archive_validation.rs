//! Exercises the checked archive boundary without constructing unchecked references.
//! Logical maps and JSON stay compatible while malformed or legacy cache bytes fail closed.

use parcel_sourcemap::{OriginalLocation, SourceMap, SourceMapErrorType};
use rkyv::util::AlignedVec;

const FORMAT: &[u8; 16] = b"PSMAP-RKYV-0.8\0\0";

fn fixture() -> SourceMap {
    let mut map = SourceMap::new("/project");
    let source = map.add_source("/project/src/example.js");
    map.set_source_content(source as usize, "export const message = 'hello';\n")
        .unwrap();
    let name = map.add_name("message");
    map.add_mapping(
        0,
        12,
        Some(OriginalLocation::new(0, 13, source, Some(name))),
    );
    map.add_mapping(0, 0, Some(OriginalLocation::new(0, 0, source, None)));
    map.add_mapping(2, 1, None);
    map
}

fn encode(map: &SourceMap) -> AlignedVec {
    let mut output = AlignedVec::new();
    map.to_buffer(&mut output).unwrap();
    output
}

fn assert_buffer_error(bytes: &[u8]) {
    let error = SourceMap::from_buffer("/project", bytes).unwrap_err();
    assert!(matches!(error.error_type, SourceMapErrorType::BufferError));
}

#[test]
fn round_trip_preserves_mappings_sources_names_and_vlq() {
    let mut original = fixture();
    let bytes = encode(&original);
    assert_eq!(&bytes[..FORMAT.len()], FORMAT);
    let mut restored = SourceMap::from_buffer("/restored", &bytes).unwrap();
    assert_eq!(restored.project_root, "/restored");
    assert_eq!(restored.get_mappings(), original.get_mappings());
    assert_eq!(restored.get_sources(), original.get_sources());
    assert_eq!(
        restored.get_sources_content(),
        original.get_sources_content()
    );
    assert_eq!(restored.get_names(), original.get_names());
    let mut expected = Vec::new();
    let mut actual = Vec::new();
    original.write_vlq(&mut expected).unwrap();
    restored.write_vlq(&mut actual).unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn output_reuse_replaces_previous_bytes_and_empty_maps_round_trip() {
    let mut bytes = encode(&fixture());
    SourceMap::new("/").to_buffer(&mut bytes).unwrap();
    let empty = SourceMap::from_buffer("/", &bytes).unwrap();
    assert!(empty.get_sources().is_empty());
    assert!(empty.get_names().is_empty());
    assert!(empty.get_mappings().is_empty());
    assert_eq!(bytes.as_slice(), encode(&SourceMap::new("/")).as_slice());
}

#[test]
fn unaligned_caller_slices_are_copied_before_validation() {
    let original = fixture();
    let bytes = encode(&original);
    let mut shifted = AlignedVec::<16>::new();
    shifted.push(0);
    shifted.extend_from_slice(&bytes);
    assert_ne!((shifted[1..].as_ptr() as usize) % 16, 0);
    let restored = SourceMap::from_buffer("/project", &shifted[1..]).unwrap();
    assert_eq!(restored.get_mappings(), original.get_mappings());
    assert_eq!(
        restored.get_sources_content(),
        original.get_sources_content()
    );
}

#[test]
fn missing_unknown_and_unversioned_formats_are_rejected() {
    let bytes = encode(&fixture());
    assert_buffer_error(&[]);
    assert_buffer_error(&bytes[FORMAT.len()..]);
    let mut unknown = bytes.to_vec();
    unknown[12] = b'7';
    assert_buffer_error(&unknown);
    assert_buffer_error(b"legacy rkyv archive without a format marker");
}

#[test]
fn truncated_and_corrupt_archives_return_buffer_errors() {
    let bytes = encode(&fixture());
    for length in 0..bytes.len() {
        assert_buffer_error(&bytes[..length]);
    }
    let mut corrupted = bytes.to_vec();
    corrupted[FORMAT.len()..].fill(0xff);
    assert_buffer_error(&corrupted);
}

#[test]
fn invalid_archived_boolean_is_rejected_by_byte_validation() {
    use parcel_sourcemap::ArchivedSourceMapInner;
    use rkyv::rancor::Error;

    let mut bytes = encode(&fixture());
    let boolean_offset = {
        let archived =
            rkyv::access::<ArchivedSourceMapInner, Error>(&bytes[FORMAT.len()..]).unwrap();
        let boolean = &archived.mapping_lines[0].is_sorted;
        (std::ptr::from_ref(boolean) as usize) - (bytes.as_ptr() as usize)
    };
    bytes[boolean_offset] = 2;
    assert_buffer_error(&bytes);
}

#[cfg(feature = "json")]
#[test]
fn json_and_data_url_output_remain_logically_unchanged() {
    let mut original = fixture();
    let bytes = encode(&original);
    let mut restored = SourceMap::from_buffer("/project", &bytes).unwrap();
    assert_eq!(
        restored.to_json(None).unwrap(),
        original.to_json(None).unwrap()
    );
    assert_eq!(
        restored.to_data_url(None).unwrap(),
        original.to_data_url(None).unwrap()
    );
}
