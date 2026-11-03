//! Flat and bounded RDF contributions must produce the same complete digests.

use super::*;
use crate::{RdfStatement, SourceSpan};

struct Records {
    values: Vec<ExtensionEnvelope>,
    mutate_read: u8,
}
impl RdfDigestSource for Records {
    fn visit(
        &mut self,
        visitor: &mut dyn FnMut(ExtensionEnvelope) -> Result<(), NormalizedRebindError>,
    ) -> Result<(), NormalizedRebindError> {
        for value in &self.values {
            visitor(value.clone())?;
        }
        Ok(())
    }
    fn read(&mut self, id: FactId) -> Result<ExtensionEnvelope, NormalizedRebindError> {
        let mut value = self
            .values
            .iter()
            .find(|value| value.id == id)
            .cloned()
            .ok_or(NormalizedRebindError::IdentityRecipe)?;
        if self.mutate_read == 1 {
            value.payload.push(' ');
        }
        if self.mutate_read == 2 {
            value.id = FactId::from_bytes([229; 20]);
        }
        Ok(value)
    }
}
fn documents() -> (
    NormalizedIrDocument,
    NormalizedIrDocument,
    ExtensionEnvelope,
) {
    let base = fixture_with_lexical_extension();
    let file = &base.files[0];
    let source = SourceRef::new(
        base.repository,
        base.generation,
        SourceSpan::new(file.id, 0, 17).unwrap(),
        file.content_hash,
        None,
    );
    let spans = [
        SourceSpan::new(file.id, 0, 3).unwrap(),
        SourceSpan::new(file.id, 4, 11).unwrap(),
        SourceSpan::new(file.id, 12, 15).unwrap(),
    ];
    let statement = RdfStatement::from_ntriples(source, "_:a <urn:p> _:b .", spans).unwrap();
    let record = new_rdf_statement_envelope(base.provenance[0].id, &statement).unwrap();
    let mut flat = base.clone();
    flat.extensions.push(record.clone());
    flat =
        canonical_preserving_extensions(flat, &IrLimits::default(), &ExtensionSupport::default())
            .unwrap();
    (base, flat, record)
}

#[test]
fn streamed_rdf_matches_every_flat_neutral_digest_and_count() {
    let (base, flat, record) = documents();
    let expected = canonical_generation_neutral_digests(
        &flat,
        &IrLimits::default(),
        &ExtensionSupport::default(),
        || true,
    )
    .unwrap();
    let actual = canonical_generation_neutral_digests_with_rdf(
        &base,
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &mut Records {
            values: vec![record],
            mutate_read: 0,
        },
        1,
        || true,
    )
    .unwrap();
    assert_eq!(actual, expected);
}

#[test]
fn streamed_rdf_rejects_missing_duplicate_and_changed_second_pass() {
    let (base, _, record) = documents();
    for (values, count, mutate_read) in [
        (Vec::new(), 1, 0),
        (vec![record.clone(), record.clone()], 2, 0),
        (vec![record.clone()], 1, 1),
        (vec![record], 1, 2),
    ] {
        assert!(
            canonical_generation_neutral_digests_with_rdf(
                &base,
                &IrLimits::default(),
                &ExtensionSupport::default(),
                &mut Records {
                    values,
                    mutate_read
                },
                count,
                || true
            )
            .is_err()
        );
    }
}

#[test]
fn streamed_rdf_preserves_per_record_quota_and_interruption() {
    let (base, _, record) = documents();
    let limits = IrLimits {
        max_extension_payload_bytes: record.payload.len() - 1,
        ..IrLimits::default()
    };
    assert!(
        canonical_generation_neutral_digests_with_rdf(
            &base,
            &limits,
            &ExtensionSupport::default(),
            &mut Records {
                values: vec![record.clone()],
                mutate_read: 0
            },
            1,
            || true
        )
        .is_err()
    );
    assert_eq!(
        canonical_generation_neutral_digests_with_rdf(
            &base,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &mut Records {
                values: vec![record],
                mutate_read: 0
            },
            1,
            || false
        ),
        Err(NormalizedRebindError::Interrupted)
    );
}

#[test]
fn rdf_digest_id_workspace_is_checked_before_source_visit() {
    assert_eq!(rdf_digest_workspace_bytes(0), Ok(0));
    let exact = std::mem::size_of::<(FactId, FactId)>() + std::mem::size_of::<(FactId, usize)>();
    assert_eq!(
        rdf_digest_workspace_bytes(3),
        Ok(u64::try_from(exact * 3).unwrap())
    );
    assert_eq!(
        rdf_digest_workspace_bytes(usize::MAX),
        Err(NormalizedRebindError::ResourceLimit)
    );
    struct NeverVisit;
    impl RdfDigestSource for NeverVisit {
        fn visit(
            &mut self,
            _: &mut dyn FnMut(ExtensionEnvelope) -> Result<(), NormalizedRebindError>,
        ) -> Result<(), NormalizedRebindError> {
            panic!("overflow must refuse before reading source")
        }
        fn read(&mut self, _: FactId) -> Result<ExtensionEnvelope, NormalizedRebindError> {
            panic!("overflow must refuse before reread")
        }
    }
    let (base, _, _) = documents();
    assert_eq!(
        canonical_generation_neutral_digests_with_rdf(
            &base,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &mut NeverVisit,
            usize::MAX,
            || true
        ),
        Err(NormalizedRebindError::ResourceLimit)
    );
}

struct HintedRecords {
    records: Records,
    hints: Vec<(FactId, FactId)>,
    supported: bool,
    read_calls: usize,
}
impl RdfDigestSource for HintedRecords {
    fn visit_neutral_ids(
        &mut self,
        visitor: &mut dyn FnMut(FactId, FactId) -> Result<(), NormalizedRebindError>,
    ) -> Option<Result<(), NormalizedRebindError>> {
        for (original, neutral) in &self.hints {
            if let Err(error) = visitor(*original, *neutral) {
                return Some(Err(error));
            }
        }
        self.supported.then_some(Ok(()))
    }
    fn visit(
        &mut self,
        _: &mut dyn FnMut(ExtensionEnvelope) -> Result<(), NormalizedRebindError>,
    ) -> Result<(), NormalizedRebindError> {
        panic!("hints must avoid a duplicate payload pass")
    }
    fn read(&mut self, id: FactId) -> Result<ExtensionEnvelope, NormalizedRebindError> {
        self.read_calls += 1;
        self.records.read(id)
    }
}

#[test]
fn authenticated_id_hints_preserve_flat_digest_with_one_payload_read() {
    let (base, flat, record) = documents();
    let limits = IrLimits::default();
    let support = ExtensionSupport::default();
    let ids = GenerationNeutralFactIdIndex::new(&base, &limits, &support, || true).unwrap();
    let neutral = ids.rdf_statement_id(&record, || true).unwrap();
    let mut source = HintedRecords {
        hints: vec![(record.id, neutral)],
        records: Records {
            values: vec![record],
            mutate_read: 0,
        },
        supported: true,
        read_calls: 0,
    };
    let actual = canonical_generation_neutral_digests_with_rdf(
        &base,
        &limits,
        &support,
        &mut source,
        1,
        || true,
    )
    .unwrap();
    assert_eq!(
        actual,
        canonical_generation_neutral_digests(&flat, &limits, &support, || true).unwrap()
    );
    assert_eq!(source.read_calls, 1);
}

#[test]
fn id_hints_reject_missing_duplicate_unsorted_forged_and_reread_drift() {
    let (base, _, record) = documents();
    let limits = IrLimits::default();
    let support = ExtensionSupport::default();
    let neutral = GenerationNeutralFactIdIndex::new(&base, &limits, &support, || true)
        .unwrap()
        .rdf_statement_id(&record, || true)
        .unwrap();
    let original = record.id;
    let mut equal_neutral = vec![
        (original, neutral),
        (FactId::from_bytes([240; 20]), neutral),
    ];
    equal_neutral.sort_unstable();
    for (hints, count, mutation, supported, reads) in [
        (Vec::new(), 1, 0, true, 0),
        (
            vec![(original, neutral), (original, neutral)],
            2,
            0,
            true,
            0,
        ),
        (
            vec![
                (FactId::from_bytes([240; 20]), neutral),
                (FactId::from_bytes([239; 20]), neutral),
            ],
            2,
            0,
            true,
            0,
        ),
        (vec![(base.provenance[0].id, neutral)], 1, 0, true, 0),
        (equal_neutral, 2, 0, true, 0),
        (
            vec![(original, FactId::from_bytes([237; 20]))],
            1,
            0,
            true,
            1,
        ),
        (vec![(original, neutral)], 1, 1, true, 1),
        (vec![(original, neutral)], 1, 2, true, 1),
        (vec![(original, neutral)], 1, 0, false, 0),
    ] {
        let mut source = HintedRecords {
            hints,
            records: Records {
                values: vec![record.clone()],
                mutate_read: mutation,
            },
            supported,
            read_calls: 0,
        };
        assert!(
            canonical_generation_neutral_digests_with_rdf(
                &base,
                &limits,
                &support,
                &mut source,
                count,
                || true
            )
            .is_err()
        );
        assert_eq!(source.read_calls, reads);
    }
}

#[test]
fn hint_traversal_observes_cancellation_before_payload_read() {
    use std::cell::Cell;
    let (base, _, record) = documents();
    let flag = Cell::new(true);
    struct CancelHints<'a> {
        flag: &'a Cell<bool>,
        original: FactId,
        neutral: FactId,
    }
    impl RdfDigestSource for CancelHints<'_> {
        fn visit_neutral_ids(
            &mut self,
            visitor: &mut dyn FnMut(FactId, FactId) -> Result<(), NormalizedRebindError>,
        ) -> Option<Result<(), NormalizedRebindError>> {
            self.flag.set(false);
            Some(visitor(self.original, self.neutral))
        }
        fn visit(
            &mut self,
            _: &mut dyn FnMut(ExtensionEnvelope) -> Result<(), NormalizedRebindError>,
        ) -> Result<(), NormalizedRebindError> {
            panic!("cancelled hint traversal cannot read payload")
        }
        fn read(&mut self, _: FactId) -> Result<ExtensionEnvelope, NormalizedRebindError> {
            panic!("cancelled hint traversal cannot reread payload")
        }
    }
    let mut source = CancelHints {
        flag: &flag,
        original: record.id,
        neutral: FactId::from_bytes([237; 20]),
    };
    assert_eq!(
        canonical_generation_neutral_digests_with_rdf(
            &base,
            &IrLimits::default(),
            &ExtensionSupport::default(),
            &mut source,
            1,
            || flag.get()
        ),
        Err(NormalizedRebindError::Interrupted)
    );
}

#[test]
fn data_digest_preserves_unsupported_extension_refusal_without_authority() {
    let (mut base, _, _) = documents();
    let mut unknown = base.extensions[0].clone();
    unknown.id = FactId::from_bytes([246; 20]);
    unknown.namespace = "example.opaque".to_owned();
    unknown.criticality = crate::ExtensionCriticality::Noncritical;
    base.extensions.push(unknown);
    let limits = IrLimits::default();
    let support = ExtensionSupport::default();
    let base = canonical_preserving_extensions(base, &limits, &support).unwrap();
    let mut source = Records {
        values: Vec::new(),
        mutate_read: 0,
    };
    assert_eq!(
        canonical_generation_neutral_digests_with_rdf(
            &base,
            &limits,
            &support,
            &mut source,
            0,
            || true
        ),
        Err(NormalizedRebindError::UnsupportedExtension)
    );
}
