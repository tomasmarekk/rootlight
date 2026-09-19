//! Authored logical-name and relative-target cases independent of physical archives.
//! Identifier equivalence is tested separately from URI component preservation.

use super::*;

#[test]
fn zip_mapping_decodes_unicode_but_preserves_ascii_escape_identity() {
    let part = PartName::from_zip_name(b"DIR/%CE%B1%F0%90%80%80%20file.BIN").unwrap();
    assert_eq!(part.as_str(), "/dir/α𐀀%20file.bin");
    assert_eq!(part, PartName::parse("/DIR/α𐀀%20FILE.bin").unwrap());
    assert_ne!(
        PartName::parse("/É").unwrap(),
        PartName::parse("/é").unwrap()
    );
    assert!(!format!("{part:?}").contains("file"));
    for name in [
        b"%ff.bin".as_slice(),
        b"%c0%af.bin",
        "α.bin".as_bytes(),
        b"%e2%82.bin",
    ] {
        assert_eq!(
            PartName::from_zip_name(name),
            Err(PartNameError::ZipEncoding)
        );
    }
    for name in [
        b"a/%2f.bin".as_slice(),
        b"a/%5c.bin",
        b"a/%61.bin",
        b"a/../b",
        b"a//b",
        b"/a",
        b"a/",
        b"[Content_Types].xml",
        b"a/[0].piece",
    ] {
        assert_eq!(PartName::from_zip_name(name), Err(PartNameError::Invalid));
    }
}

#[test]
fn relative_resolution_uses_source_part_not_relationship_directory() {
    let base = PartName::parse("/a/b/document.xml").unwrap();
    for (reference, expected) in [
        ("source.bin", "/a/b/source.bin"),
        ("./source.bin", "/a/b/source.bin"),
        ("../source.bin", "/a/source.bin"),
        ("../../../source.bin", "/source.bin"),
        ("/source.bin", "/source.bin"),
        ("g/../source.bin", "/a/b/source.bin"),
        ("g/./source.bin", "/a/b/g/source.bin"),
        ("./name:part", "/a/b/name:part"),
        ("?q=../#fragment", "/a/b/document.xml"),
    ] {
        assert_eq!(
            PartName::resolve(Some(&base), reference)
                .unwrap()
                .part()
                .as_str(),
            expected
        );
    }
    assert_eq!(
        PartName::resolve(None, "../../source.bin")
            .unwrap()
            .part()
            .as_str(),
        "/source.bin"
    );
    for reference in [
        "http://invalid.example/source",
        "//invalid.example/source",
        "name:part",
        "../",
        "g/..",
        "g/.",
        "g//file",
        "a\\b",
        "?bad=%zz",
        "a#b#c",
    ] {
        assert!(
            PartName::resolve(Some(&base), reference).is_err(),
            "{reference}"
        );
    }
    assert!(PartName::resolve(None, "").is_err());
}

#[test]
fn query_and_fragment_are_retained_without_selecting_other_parts() {
    let base = PartName::parse("/a/document.xml").unwrap();
    let target = PartName::resolve(Some(&base), "../data.bin?q=../other#section/../x").unwrap();
    assert_eq!(target.part().as_str(), "/data.bin");
    assert_eq!(target.query(), Some("q=../other"));
    assert_eq!(target.fragment(), Some("section/../x"));
    assert_eq!(
        PartName::resolve(Some(&base), "#").unwrap().fragment(),
        Some("")
    );
}

#[test]
fn relationship_names_are_derived_and_cannot_be_relationship_sources() {
    assert_eq!(
        PartName::relationships_for(None).unwrap().as_str(),
        "/_rels/.rels"
    );
    let part = PartName::parse("/a/main.xml").unwrap();
    let rels = PartName::relationships_for(Some(&part)).unwrap();
    assert_eq!(rels.as_str(), "/a/_rels/main.xml.rels");
    assert!(rels.is_relationships());
    assert!(PartName::relationships_for(Some(&rels)).is_err());
    assert!(
        !PartName::parse("/ordinary.rels")
            .unwrap()
            .is_relationships()
    );
    assert!(
        !PartName::parse("/_rels/item.bin")
            .unwrap()
            .is_relationships()
    );
}

#[test]
fn name_and_reference_bounds_apply_before_growth() {
    let name = format!("/{}", "a".repeat(MAX_NAME_BYTES - 1));
    let part = PartName::parse(&name).unwrap();
    assert_eq!(
        PartName::parse(&format!("{name}a")),
        Err(PartNameError::Limit)
    );
    assert_eq!(
        PartName::relationships_for(Some(&part)),
        Err(PartNameError::Limit)
    );
    assert!(matches!(
        PartName::resolve(Some(&part), &"a".repeat(MAX_NAME_BYTES + 1)),
        Err(PartNameError::Limit)
    ));
    assert_eq!(
        PartName::from_zip_name(&vec![b'a'; MAX_NAME_BYTES]),
        Err(PartNameError::Limit)
    );
}
