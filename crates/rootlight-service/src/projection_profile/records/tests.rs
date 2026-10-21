//! Compare the borrowed observer to the canonical serializer and real streams.
//! Buffered reference bytes are test-only; production retains numeric counters.

use std::io;

use super::*;

fn fixture() -> NormalizedIrDocument {
    let decoded = rootlight_ir::decode_ir_document(
        include_bytes!("../../../../../tests/fixtures/compatibility/ir/1.1/document.json"),
        &rootlight_ir::IrLimits::default(),
        &rootlight_ir::ExtensionSupport::default(),
    )
    .unwrap();
    let rootlight_ir::IrDocument::NormalizedV1_1(document) = decoded else {
        panic!("the compatibility fixture is normalized IR 1.1");
    };
    document
}

fn measured(document: &NormalizedIrDocument) -> (Vec<u8>, Observation) {
    let mut wire = Vec::new();
    let (result, profile) =
        crate::capture_first_slice_analysis_profile(|| serialize_profiled(&mut wire, document));
    result.unwrap();
    (wire, profile.last_fresh_projection_records.unwrap())
}

fn assert_category<T: Serialize>(category: Category, name: &str, records: &[T]) {
    let sizes: Vec<u64> = records
        .iter()
        .map(|record| u64::try_from(serde_json::to_vec(record).unwrap().len()).unwrap())
        .collect();
    assert_eq!(category.category, name);
    assert_eq!(category.records, u64::try_from(records.len()).unwrap());
    assert_eq!(category.serialized_record_bytes, sizes.iter().sum::<u64>());
    assert_eq!(
        category.maximum_serialized_record_bytes,
        sizes.into_iter().max().unwrap_or(0)
    );
}

#[test]
fn empty_wire_has_complete_zero_categories_and_exact_envelope() {
    let fixture = fixture();
    let document = NormalizedIrDocument::empty(fixture.repository, fixture.generation);
    let expected = serde_json::to_vec(&document).unwrap();
    let (wire, observation) = measured(&document);
    assert_eq!(wire, expected);
    assert_eq!(
        observation.document_bytes,
        u64::try_from(wire.len()).unwrap()
    );
    assert_eq!(observation.records, 0);
    assert_eq!(observation.serialized_record_bytes, 0);
    assert_eq!(observation.maximum_serialized_record_bytes, 0);
    assert_eq!(observation.envelope_bytes, observation.document_bytes);
    for (category, name) in observation.categories.into_iter().zip(CATEGORIES) {
        assert_eq!(category.category, name);
        assert_eq!(category.records, 0);
        assert_eq!(category.serialized_record_bytes, 0);
        assert_eq!(category.maximum_serialized_record_bytes, 0);
    }
}

#[test]
fn nested_unicode_records_preserve_wire_and_each_category_maximum() {
    let mut document = fixture();
    document.entities[0].canonical_name = "unicode_λ_🙂".to_owned();
    let source = document.provenance[0].input_sources[0].clone();
    document.provenance[0].input_sources.push(source);
    document.extensions[0].payload = r#"{"nested":["λ",{"value":"🙂"}]}"#.to_owned();
    let mut diagnostic = document.diagnostics[0].clone();
    diagnostic.message =
        "a longer unicode diagnostic λ with \"quotes\", \\ and a newline\n".to_owned();
    document.diagnostics.push(diagnostic);
    let expected = serde_json::to_vec(&document).unwrap();
    let (wire, observation) = measured(&document);
    assert_eq!(wire, expected);
    assert_eq!(
        observation.document_bytes,
        u64::try_from(wire.len()).unwrap()
    );
    macro_rules! category {
        ($index:literal, $field:ident) => {
            assert_category(
                observation.categories[$index],
                stringify!($field),
                &document.$field,
            );
        };
    }
    category!(0, files);
    category!(1, entities);
    category!(2, occurrences);
    category!(3, relations);
    category!(4, provenance);
    category!(5, source_mappings);
    category!(6, coverage_records);
    category!(7, skipped_regions);
    category!(8, diagnostics);
    category!(9, extensions);
    assert_eq!(observation.records, 11);
    assert_eq!(
        observation.serialized_record_bytes,
        observation
            .categories
            .iter()
            .map(|category| category.serialized_record_bytes)
            .sum::<u64>()
    );
    assert_eq!(
        observation.maximum_serialized_record_bytes,
        observation
            .categories
            .iter()
            .map(|category| category.maximum_serialized_record_bytes)
            .max()
            .unwrap()
    );
    assert_eq!(
        observation.serialized_record_bytes + observation.envelope_bytes,
        observation.document_bytes
    );
    assert!(observation.envelope_bytes > 0);
    assert!(
        observation.categories[8].serialized_record_bytes
            > observation.categories[8].maximum_serialized_record_bytes
    );
    let numeric = serde_json::to_string(&observation).unwrap();
    for payload in [
        "unicode_λ_🙂",
        "src/lib.rs",
        "a longer unicode diagnostic",
        "nested",
    ] {
        assert!(!numeric.contains(payload));
    }
}

#[test]
fn opt_in_preserves_original_admission_count_and_opt_out_path() {
    let document = fixture();
    assert!(!crate::analysis_profile_enabled());
    let expected = crate::normalized_document_serialized_bytes(&document).unwrap();
    assert_eq!(document_serialized_bytes(&document).unwrap(), expected);
    assert!(!crate::analysis_profile_enabled());
    let (count, profile) = crate::capture_first_slice_analysis_profile(|| {
        document_serialized_bytes(&document).unwrap()
    });
    assert_eq!(count, expected);
    assert_eq!(
        profile
            .last_fresh_projection_records
            .unwrap()
            .document_bytes,
        expected
    );
    let (_, untouched) = crate::capture_first_slice_analysis_profile(|| ());
    assert!(untouched.last_fresh_projection_records.is_none());
}

struct RefusingWriter {
    bytes: Vec<u8>,
    limit: usize,
}

impl Write for RefusingWriter {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        let remaining = self.limit - self.bytes.len();
        if remaining == 0 && !buffer.is_empty() {
            return Err(io::Error::other("test writer stopped"));
        }
        let accepted = remaining.min(buffer.len());
        self.bytes.extend_from_slice(&buffer[..accepted]);
        Ok(accepted)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn partial_stream_errors_keep_exact_prefix_and_clear_prior_observation() {
    let document = fixture();
    let expected = serde_json::to_vec(&document).unwrap();
    for limit in [0, 1, expected.len() / 2, expected.len() - 1] {
        let mut normal = RefusingWriter {
            bytes: Vec::new(),
            limit,
        };
        let error = serde_json::to_writer(&mut normal, &document).unwrap_err();
        let mut observed = RefusingWriter {
            bytes: Vec::new(),
            limit,
        };
        let (result, profile) = crate::capture_first_slice_analysis_profile(|| {
            serialize_profiled(io::sink(), &document).unwrap();
            serialize_profiled(&mut observed, &document)
        });
        assert_eq!(result.unwrap_err().to_string(), error.to_string());
        assert_eq!(observed.bytes, normal.bytes);
        assert_eq!(observed.bytes, expected[..limit]);
        assert!(profile.last_fresh_projection_records.is_none());
    }
    let (_, recovered) = measured(&document);
    assert_eq!(
        recovered.document_bytes,
        u64::try_from(expected.len()).unwrap()
    );
}

#[test]
fn unwinding_before_document_completion_leaves_observation_unknown() {
    struct UnfinishedWriter;
    impl Write for UnfinishedWriter {
        fn write(&mut self, _: &[u8]) -> io::Result<usize> {
            panic!("test serialization interrupted");
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }
    let document = fixture();
    let (_, profile) = crate::capture_first_slice_analysis_profile(|| {
        serialize_profiled(io::sink(), &document).unwrap();
        let result = std::panic::catch_unwind(|| serialize_profiled(UnfinishedWriter, &document));
        assert!(result.is_err());
    });
    assert!(profile.last_fresh_projection_records.is_none());
}

#[test]
fn telemetry_overflow_keeps_one_serialization_per_record_and_valid_wire() {
    struct Once<'a>(&'a Cell<u64>);
    impl Serialize for Once<'_> {
        fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            self.0.set(self.0.get() + 1);
            serializer.serialize_str("λ")
        }
    }
    for (initial_position, initial_sizes) in [
        (
            Some(0),
            Some(RecordSizes {
                records: u64::MAX,
                ..RecordSizes::default()
            }),
        ),
        (
            Some(0),
            Some(RecordSizes {
                bytes: u64::MAX,
                ..RecordSizes::default()
            }),
        ),
        (Some(u64::MAX), Some(RecordSizes::default())),
        (None, Some(RecordSizes::default())),
    ] {
        let calls = Cell::new(0);
        let values = [Once(&calls), Once(&calls)];
        let position = Position::new(initial_position);
        let sizes: Sizes = std::array::from_fn(|_| Cell::new(Some(RecordSizes::default())));
        sizes[0].set(initial_sizes);
        let records = Records {
            values: &values,
            position: &position,
            sizes: &sizes[0],
        };
        let mut wire = Vec::new();
        serde_json::to_writer(
            ObservedWriter {
                inner: &mut wire,
                position: &position,
            },
            &records,
        )
        .unwrap();
        assert_eq!(wire, "[\"λ\",\"λ\"]".as_bytes());
        assert_eq!(calls.get(), 2);
        assert!(finish(&position, &sizes).is_none());
    }
}
