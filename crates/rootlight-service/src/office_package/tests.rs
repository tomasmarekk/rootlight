//! Authored packages test declared ownership independently of embedded VBA syntax.
//! Stored payloads make inventory/relationship failures distinct from decompression.

use super::*;
use rootlight_cancel::CancellationReason;
use rootlight_ids::content_hash;

fn crc(bytes: &[u8]) -> u32 {
    let mut value = u32::MAX;
    for byte in bytes {
        value ^= u32::from(*byte);
        for _ in 0..8 {
            value = if value & 1 == 0 {
                value >> 1
            } else {
                (value >> 1) ^ 0xedb8_8320
            };
        }
    }
    !value
}
fn number(bytes: &mut [u8], offset: usize, width: usize, value: u64) {
    bytes[offset..offset + width].copy_from_slice(&value.to_le_bytes()[..width]);
}
fn zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut central = Vec::new();
    for (name, payload) in entries {
        let offset = bytes.len();
        let mut local = vec![0; 30];
        let mut record = vec![0; 46];
        let size = u64::try_from(payload.len()).unwrap();
        let name_len = u64::try_from(name.len()).unwrap();
        let checksum = u64::from(crc(payload));
        number(&mut local, 0, 4, 0x0403_4b50);
        number(&mut local, 4, 2, 20);
        number(&mut local, 14, 4, checksum);
        number(&mut local, 18, 4, size);
        number(&mut local, 22, 4, size);
        number(&mut local, 26, 2, name_len);
        bytes.extend_from_slice(&local);
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(payload);
        number(&mut record, 0, 4, 0x0201_4b50);
        number(&mut record, 6, 2, 20);
        number(&mut record, 16, 4, checksum);
        number(&mut record, 20, 4, size);
        number(&mut record, 24, 4, size);
        number(&mut record, 28, 2, name_len);
        number(&mut record, 42, 4, u64::try_from(offset).unwrap());
        central.extend_from_slice(&record);
        central.extend_from_slice(name.as_bytes());
    }
    let start = bytes.len();
    bytes.extend_from_slice(&central);
    let mut end = vec![0; 22];
    number(&mut end, 0, 4, 0x0605_4b50);
    number(&mut end, 8, 2, u64::try_from(entries.len()).unwrap());
    number(&mut end, 10, 2, u64::try_from(entries.len()).unwrap());
    number(&mut end, 12, 4, u64::try_from(central.len()).unwrap());
    number(&mut end, 16, 4, u64::try_from(start).unwrap());
    bytes.extend_from_slice(&end);
    bytes
}
fn relationships(body: &str) -> Vec<u8> {
    format!("<Relationships xmlns='http://schemas.openxmlformats.org/package/2006/relationships'>{body}</Relationships>").into_bytes()
}
fn relation(kind: &str, target: &str, extra: &str) -> String {
    format!("<Relationship Id='r1' Type='{kind}' Target='{target}' {extra}/>")
}
fn fixture() -> Vec<(String, Vec<u8>)> {
    vec![
        ("[Content_Types].xml".into(), format!("<Types xmlns='http://schemas.openxmlformats.org/package/2006/content-types'><Default Extension='xml' ContentType='application/xml'/><Default Extension='rels' ContentType='{RELS_TYPE}'/><Override PartName='/payload/project.dat' ContentType='{VBA_TYPE}'/></Types>").into_bytes()),
        ("_rels/.rels".into(), relationships(&relation(OFFICE_REL, "documents/main.xml", ""))),
        ("documents/main.xml".into(), b"main document bytes".to_vec()),
        ("documents/_rels/main.xml.rels".into(), relationships(&relation(VBA_REL, "../payload/project.dat", ""))),
        ("payload/project.dat".into(), b"project bytes for a subsequent compound reader".to_vec()),
    ]
}
fn read(entries: &[(String, Vec<u8>)]) -> Result<Option<OfficeVbaProject>, OfficePackageError> {
    read_vba_project(
        &zip(entries),
        OfficePackageLimits::default(),
        &Cancellation::new(),
    )
}

#[test]
fn declared_ownership_retains_all_five_sources_and_exact_declarations() {
    let entries = fixture();
    let input = zip(&entries);
    let bound = read_vba_project(&input, OfficePackageLimits::default(), &Cancellation::new())
        .unwrap()
        .unwrap();
    assert_eq!(bound.main_name().as_str(), "/documents/main.xml");
    assert_eq!(bound.project_name().as_str(), "/payload/project.dat");
    for (source, ordinal) in [
        (bound.types_source(), 0),
        (bound.package_relationships_source(), 1),
        (bound.main_source(), 2),
        (bound.main_relationships_source(), 3),
        (bound.project_source(), 4),
    ] {
        assert_eq!(source.ordinal(), ordinal);
        assert_eq!(source.archive_hash(), content_hash(&input));
        assert_eq!(source.bytes(), entries[ordinal].1);
    }
    assert_eq!(bound.project_relationship().kind().value(), VBA_REL);
    let raw = bound.main_relationships_source().bytes();
    assert_eq!(
        &raw[bound.project_relationship().target().original_range()],
        b"../payload/project.dat"
    );
    let raw = bound.types_source().bytes();
    assert_eq!(
        &raw[bound.project_declaration().media_type().original_range()],
        VBA_TYPE.as_bytes()
    );
    assert!(bound.non_part_entries().is_empty());
    assert!(!format!("{bound:?}").contains("project bytes"));
}

#[test]
fn relocated_case_equivalent_and_unicode_parts_keep_relationship_ownership() {
    let mut entries = fixture();
    entries[4].0 = "PAYLOAD/PROJECT.DAT".into();
    entries[1].1 = relationships(&relation(
        STRICT_OFFICE_REL,
        "/documents/./main.xml?view=1",
        "",
    ));
    entries[3].1 = relationships(&relation(VBA_REL, "../../payload/project.dat#", ""));
    entries.push(("[trash]/unused.dat".into(), b"not a logical part".to_vec()));
    entries.push(("documents/".into(), Vec::new()));
    assert_eq!(read(&entries).unwrap().unwrap().non_part_entries(), &[5, 6]);
    entries[4].0 = "payload/%CE%B1.dat".into();
    entries[0].1 = String::from_utf8(entries[0].1.clone())
        .unwrap()
        .replace("/payload/project.dat", "/payload/α.dat")
        .into_bytes();
    entries[3].1 = relationships(&relation(VBA_REL, "../payload/α.dat", ""));
    assert_eq!(
        read(&entries).unwrap().unwrap().project_name().as_str(),
        "/payload/α.dat"
    );
}

#[test]
fn missing_external_ambiguous_or_fragment_ownership_is_never_accepted() {
    for target in [
        "../payload/missing.dat",
        "//invalid.example/project.dat",
        "https://invalid.example/project.dat",
        "../payload/project.dat#section",
        "_rels/main.xml.rels",
    ] {
        let mut entries = fixture();
        entries[3].1 = relationships(&relation(VBA_REL, target, ""));
        assert!(read(&entries).is_err(), "{target}");
    }
    for ordinal in [1, 3] {
        let mut entries = fixture();
        let (kind, target) = if ordinal == 1 {
            (OFFICE_REL, "documents/main.xml")
        } else {
            (VBA_REL, "../payload/project.dat")
        };
        entries[ordinal].1 = relationships(&relation(kind, target, "TargetMode='External'"));
        assert!(read(&entries).is_err());
        let extra = relation(kind, target, "").replace("Id='r1'", "Id='r2'");
        entries[ordinal].1 = relationships(&(relation(kind, target, "") + &extra));
        assert!(read(&entries).is_err());
        entries[ordinal].1 = relationships("");
        assert!(read(&entries).is_err());
    }
    for ordinal in [0, 1, 2, 3, 4] {
        let mut entries = fixture();
        entries.remove(ordinal);
        assert!(read(&entries).is_err());
    }
}

#[test]
fn equivalent_or_derivable_names_and_interleaving_fail_explicitly() {
    for extra in [
        "payload/PROJECT.DAT",
        "PAYLOAD",
        "payload/project.dat/child",
    ] {
        let mut entries = fixture();
        entries.push((extra.into(), Vec::new()));
        assert!(read(&entries).is_err(), "{extra}");
    }
    let mut entries = fixture();
    entries.push(("[trash]/ITEM".into(), Vec::new()));
    entries.push(("[trash]/item".into(), Vec::new()));
    assert!(read(&entries).is_err());
    for piece in [
        "payload/project.dat/[0].piece",
        "[Content_Types].xml/[1].last.piece",
    ] {
        let mut entries = fixture();
        entries.push((piece.into(), Vec::new()));
        assert!(matches!(
            read(&entries),
            Err(OfficePackageError::Unsupported {
                feature: "interleaved opc streams"
            })
        ));
    }
}

#[test]
fn deep_names_and_intervening_siblings_preserve_prefix_detection() {
    let mut entries = fixture();
    let deep = format!("{}leaf", "segment/".repeat(8_000));
    entries.push((deep.clone(), Vec::new()));
    assert!(read(&entries).unwrap().is_some());
    entries.push(("segment".into(), Vec::new()));
    entries.push(("segment-neighbor".into(), Vec::new()));
    assert!(matches!(
        read(&entries),
        Err(OfficePackageError::Invalid {
            reason: "part name derives from another part"
        })
    ));
    let mut entries = fixture();
    entries.push(("payload/project.dat-neighbor".into(), Vec::new()));
    entries.push(("payload/project.dat/child".into(), Vec::new()));
    assert!(matches!(
        read(&entries),
        Err(OfficePackageError::Invalid {
            reason: "part name derives from another part"
        })
    ));
}

#[test]
fn absent_declaration_is_distinct_from_an_orphaned_project() {
    let mut entries = fixture();
    entries[0].1 = String::from_utf8(entries[0].1.clone())
        .unwrap()
        .replace(VBA_TYPE, "application/octet-stream")
        .into_bytes();
    assert!(read(&entries).unwrap().is_none());
    let mut entries = fixture();
    entries.push(("payload/second.dat".into(), Vec::new()));
    entries[0].1 = String::from_utf8(entries[0].1.clone())
        .unwrap()
        .replace(
            "</Types>",
            &format!("<Override PartName='/payload/second.dat' ContentType='{VBA_TYPE}'/></Types>"),
        )
        .into_bytes();
    assert!(read(&entries).is_err());
    let mut entries = fixture();
    entries[3].1 = relationships("");
    assert!(read(&entries).is_err());
    let mut entries = fixture();
    entries[0].1 = String::from_utf8(entries[0].1.clone())
        .unwrap()
        .replace(RELS_TYPE, "text/plain")
        .into_bytes();
    assert!(read(&entries).is_err());
}

#[test]
fn metadata_and_aggregate_budgets_cancellation_and_crc_are_enforced() {
    let input = zip(&fixture());
    let cancel = Cancellation::new();
    cancel.cancel(CancellationReason::ClientRequest);
    assert!(matches!(
        read_vba_project(&input, OfficePackageLimits::default(), &cancel),
        Err(OfficePackageError::Archive(ZipExpansionError::Cancelled(_)))
    ));
    let mut limits = OfficePackageLimits::default();
    limits.metadata.bytes = 1;
    assert!(matches!(
        read_vba_project(&input, limits, &Cancellation::new()),
        Err(OfficePackageError::Metadata(MetadataError::Limit { .. }))
    ));
    let mut limits = OfficePackageLimits::default();
    limits.zip.reads = 4;
    assert!(matches!(
        read_vba_project(&input, limits, &Cancellation::new()),
        Err(OfficePackageError::Archive(ZipExpansionError::Limit {
            resource: "reads",
            ..
        }))
    ));
    let mut limits = OfficePackageLimits::default();
    limits.zip.expanded_bytes = 1;
    assert!(matches!(
        read_vba_project(&input, limits, &Cancellation::new()),
        Err(OfficePackageError::Archive(ZipExpansionError::Limit {
            resource: "expanded bytes",
            ..
        }))
    ));
    let archive =
        ZipSourceArchive::open(&input, ZipSourceLimits::default(), &Cancellation::new()).unwrap();
    let at = archive.entries()[4].payload_range().start;
    let mut corrupt = input;
    corrupt[at] ^= 1;
    assert!(matches!(
        read_vba_project(
            &corrupt,
            OfficePackageLimits::default(),
            &Cancellation::new()
        ),
        Err(OfficePackageError::Archive(_))
    ));
}
