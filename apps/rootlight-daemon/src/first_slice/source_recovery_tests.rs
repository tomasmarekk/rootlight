//! Universal recovery explanations must agree across every returned source file.
//! Legacy members cannot be silently mixed into an authenticated recovery frame.

use super::*;

fn cause() -> AggregateSourceRecovery {
    AggregateSourceRecovery {
        source_inventory: ContentHash::from_bytes([8; 32]),
        members: 20,
        derived_members: 2,
        stage: AggregateCapacityStage::StructuralAppend,
        resource: AggregateCapacityResource::Occurrences,
        observed: 1001,
        limit: 1000,
        text_snapshot_bytes: 8192,
    }
}

fn member(
    id: u8,
    text_search: FirstSliceTextSearchCoverage,
) -> (FileId, Option<FirstSliceSourceRecovery>) {
    (
        FileId::from_bytes([id; 20]),
        Some(FirstSliceSourceRecovery {
            cause: cause(),
            text_search,
        }),
    )
}

#[test]
fn one_cause_preserves_every_distinct_member_disposition() {
    let result = recovery_records_to_wire(vec![
        member(1, FirstSliceTextSearchCoverage::WholeSourceProjection),
        member(2, FirstSliceTextSearchCoverage::MetadataOnlyCaptured),
        member(3, FirstSliceTextSearchCoverage::UnsupportedEncoding),
    ])
    .unwrap()
    .unwrap();
    let cause = result.cause.unwrap();
    assert_eq!(cause.members, 20);
    assert_eq!(cause.derived_members, 2);
    assert_eq!(cause.observed, 1001);
    assert_eq!(cause.limit, 1000);
    assert_eq!(
        result
            .files
            .iter()
            .map(|entry| entry.text_search)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
}

#[test]
fn ordinary_absent_and_empty_responses_make_no_aggregate_claim() {
    assert!(recovery_records_to_wire(Vec::new()).unwrap().is_none());
    assert!(
        recovery_records_to_wire(vec![(FileId::from_bytes([1; 20]), None)])
            .unwrap()
            .is_none()
    );
}

#[test]
fn partial_causes_mismatched_inventory_duplicate_and_unsorted_members_fail_closed() {
    let first = member(1, FirstSliceTextSearchCoverage::WholeSourceProjection);
    let second = member(2, FirstSliceTextSearchCoverage::MetadataOnlyCaptured);
    let mut changed = second;
    changed.1.as_mut().unwrap().cause.source_inventory = ContentHash::from_bytes([9; 32]);
    for records in [
        vec![first, (second.0, None)],
        vec![(first.0, None), second],
        vec![first, changed],
        vec![first, first],
        vec![second, first],
    ] {
        assert!(recovery_records_to_wire(records).is_err());
    }
}

#[test]
fn rejected_or_overflow_sentinel_causes_cannot_be_projected() {
    for observed in [1000, u64::MAX] {
        let mut value = member(1, FirstSliceTextSearchCoverage::WholeSourceProjection);
        value.1.as_mut().unwrap().cause.observed = observed;
        assert!(recovery_records_to_wire(vec![value]).is_err());
    }
}

#[test]
fn oracle_storage_cause_preserves_stable_wire_values_and_rejects_false_pairs() {
    let mut expected = cause();
    expected.stage = AggregateCapacityStage::OracleOutputReservation;
    expected.resource = AggregateCapacityResource::OracleFilesystemReservationBytes;
    let files = [
        (1, FirstSliceTextSearchCoverage::WholeSourceProjection),
        (2, FirstSliceTextSearchCoverage::MetadataOnlyCaptured),
        (3, FirstSliceTextSearchCoverage::UnsupportedEncoding),
    ];
    let records = files
        .map(|(id, text_search)| {
            (
                FileId::from_bytes([id; 20]),
                Some(FirstSliceSourceRecovery {
                    cause: expected,
                    text_search,
                }),
            )
        })
        .to_vec();
    let wire = recovery_records_to_wire(records.clone()).unwrap().unwrap();
    let actual = wire.cause.unwrap();
    assert_eq!(actual.stage, 3);
    assert_eq!(actual.resource, 18);
    assert_eq!(actual.members, expected.members);
    assert_eq!(actual.derived_members, expected.derived_members);
    assert_eq!(actual.observed, expected.observed);
    assert_eq!(actual.limit, expected.limit);
    assert_eq!(actual.text_snapshot_bytes, expected.text_snapshot_bytes);
    assert_eq!(
        actual.source_inventory,
        Some(content_hash_to_wire(expected.source_inventory))
    );
    for (row, (id, _)) in wire.files.iter().zip(files) {
        assert_eq!(row.file, Some(file_to_wire(FileId::from_bytes([id; 20]))));
    }
    assert_eq!(
        wire.files
            .iter()
            .map(|row| row.text_search)
            .collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    for mutation in 0..4 {
        let mut invalid = records.clone();
        for (_, value) in &mut invalid {
            let cause = &mut value.as_mut().unwrap().cause;
            match mutation {
                0 => cause.stage = AggregateCapacityStage::StructuralAppend,
                1 => cause.resource = AggregateCapacityResource::Entities,
                2 => cause.observed = cause.limit,
                _ => cause.observed = u64::MAX,
            }
        }
        assert!(recovery_records_to_wire(invalid).is_err());
    }
}
