//! Public-boundary tests for bounded official SCIP protobuf import.
//!
//! Fixtures verify deterministic IR, exact range conversion, explicit coverage
//! loss, source binding, malformed input rejection, and cancellation.

use protobuf::{EnumOrUnknown, Message, MessageField};
use rootlight_adapters::{
    ScipImportLimits, ScipImportRequest, ScipImportSource, ScipResource, import_scip_index,
};
use rootlight_cancel::{Cancellation, CancellationReason};
use rootlight_ids::{GenerationId, content_hash, derive_repository};
use rootlight_ir::{BuildContextIdentity, CoverageStatus, EntityKind, IrLimits, OccurrenceRole};
use scip::types::{
    Document, Index, Metadata, PositionEncoding, Relationship, SingleLineRange, SymbolInformation,
    TextEncoding, ToolInfo, symbol_information::Kind,
};

const SOURCE: &[u8] = b"fn target() {}\nfn caller() { target(); }\n";
const TARGET: &str = "rust cargo fixture 0.1.0 target().";
const CALLER: &str = "rust cargo fixture 0.1.0 caller().";

#[test]
fn official_index_imports_deterministically_with_explicit_coverage() {
    let encoded = fixture_index(SOURCE, PositionEncoding::UTF8CodeUnitOffsetFromLineStart)
        .write_to_bytes()
        .expect("fixture SCIP index encodes");
    let first = import(&encoded, SOURCE).expect("fixture SCIP index imports");
    let second = import(&encoded, SOURCE).expect("fixture SCIP index repeats");

    assert_eq!(first, second);
    assert_eq!(first.report().documents(), 1);
    assert_eq!(first.report().symbols(), 2);
    assert_eq!(first.report().occurrences(), 2);
    assert_eq!(first.report().relationships(), 1);
    assert_eq!(first.report().skipped_symbols(), 0);
    assert_eq!(first.report().skipped_occurrences(), 0);
    assert_eq!(first.report().skipped_relationships(), 0);
    assert_eq!(first.document().files.len(), 1);
    assert_eq!(first.document().entities.len(), 2);
    assert_eq!(first.document().occurrences.len(), 2);
    assert_eq!(first.document().relations.len(), 1);
    assert_eq!(first.document().extensions.len(), 3);
    assert_eq!(first.document().coverage_records.len(), 8);
    assert!(
        first
            .document()
            .coverage_records
            .iter()
            .all(|coverage| coverage.status == CoverageStatus::Complete)
    );
    assert!(
        first
            .document()
            .entities
            .iter()
            .all(|entity| entity.kind == EntityKind::Function)
    );
    assert!(
        first
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Definition)
    );
    assert!(
        first
            .document()
            .occurrences
            .iter()
            .any(|occurrence| occurrence.role == OccurrenceRole::Reference)
    );
}

#[test]
fn relationship_flags_expand_within_the_declared_output_limit() {
    let mut index = fixture_index(SOURCE, PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    index.documents[0].symbols[1].relationships[0].is_definition = true;
    let encoded = index.write_to_bytes().expect("fixture SCIP index encodes");
    let outcome = import(&encoded, SOURCE).expect("multi-role relationship imports");

    assert_eq!(outcome.report().relationships(), 2);
    assert_eq!(outcome.document().relations.len(), 2);

    let limits = ScipImportLimits::new(
        16 * 1024 * 1024,
        4_096,
        200_000,
        1_000_000,
        1,
        16 * 1024 * 1024,
        256 * 1024 * 1024,
    )
    .expect("tighter relationship limit is valid");
    assert!(import_with_limits(&encoded, SOURCE, limits).is_err());
}

#[test]
fn utf16_and_utf32_ranges_map_to_exact_utf8_byte_spans() {
    let source = "fn 🚀target() {}\n".as_bytes();
    for (encoding, start_character, end_character) in [
        (PositionEncoding::UTF16CodeUnitOffsetFromLineStart, 5, 11),
        (PositionEncoding::UTF32CodeUnitOffsetFromLineStart, 4, 10),
    ] {
        let mut index = one_symbol_index(source, encoding);
        let occurrence = &mut index.documents[0].occurrences[0];
        occurrence.set_single_line_range(SingleLineRange {
            line: 0,
            start_character,
            end_character,
            ..Default::default()
        });
        let encoded = index.write_to_bytes().expect("fixture SCIP index encodes");
        let outcome = import(&encoded, source).expect("Unicode SCIP range imports");
        let span = outcome.document().occurrences[0].source.span();
        assert_eq!(span.start_byte(), 7);
        assert_eq!(span.end_byte(), 13);
    }
}

#[test]
fn import_rejects_malformed_mismatched_and_ambiguous_inputs() {
    assert!(import(&[0xff, 0xff], SOURCE).is_err());

    let mut mismatch = fixture_index(SOURCE, PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    mismatch.documents[0].text = "different source".to_owned();
    let encoded = mismatch.write_to_bytes().expect("mismatch fixture encodes");
    assert!(import(&encoded, SOURCE).is_err());

    let mut unspecified = fixture_index(SOURCE, PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    unspecified.documents[0].position_encoding =
        EnumOrUnknown::new(PositionEncoding::UnspecifiedPositionEncoding);
    let encoded = unspecified
        .write_to_bytes()
        .expect("unspecified fixture encodes");
    assert!(import(&encoded, SOURCE).is_err());

    let valid = fixture_index(SOURCE, PositionEncoding::UTF8CodeUnitOffsetFromLineStart)
        .write_to_bytes()
        .expect("fixture SCIP index encodes");
    let cancellation = Cancellation::new();
    cancellation.cancel(CancellationReason::ClientRequest);
    assert!(import_with_cancellation(&valid, SOURCE, &cancellation).is_err());
}

#[test]
fn unsupported_and_external_symbols_are_reported_as_bounded_loss() {
    let mut index = one_symbol_index(SOURCE, PositionEncoding::UTF8CodeUnitOffsetFromLineStart);
    index.documents[0].symbols[0].kind = EnumOrUnknown::new(Kind::UnspecifiedKind);
    index.external_symbols.push(symbol(
        "rust cargo dependency 1.0.0 external().",
        "external",
    ));
    index.documents[0].occurrences[0]
        .diagnostics
        .push(Default::default());
    let encoded = index.write_to_bytes().expect("fixture SCIP index encodes");
    let outcome = import(&encoded, SOURCE).expect("bounded SCIP index imports");

    assert_eq!(outcome.report().symbols(), 0);
    assert_eq!(outcome.report().skipped_symbols(), 2);
    assert_eq!(outcome.report().external_symbols(), 1);
    assert_eq!(outcome.report().ignored_diagnostics(), 1);
    assert_eq!(outcome.document().entities.len(), 0);
    assert_eq!(
        outcome.document().occurrences[0].role,
        OccurrenceRole::Definition
    );
    assert!(
        outcome
            .document()
            .coverage_records
            .iter()
            .any(|coverage| coverage.status == CoverageStatus::Bounded)
    );
}

#[test]
fn fixed_limits_are_visible_and_source_sets_match_exactly() {
    let limits = ScipImportLimits::default();
    assert_eq!(limits.max_index_bytes(), 16 * 1024 * 1024);
    assert_eq!(limits.max_documents(), 4_096);
    assert_eq!(limits.max_symbols(), 200_000);
    assert_eq!(limits.max_occurrences(), 1_000_000);
    assert_eq!(limits.max_relationships(), 500_000);
    assert_eq!(limits.max_source_bytes(), 16 * 1024 * 1024);
    assert_eq!(limits.max_total_source_bytes(), 256 * 1024 * 1024);
    assert_eq!(ScipResource::IndexBytes.to_string(), "index_bytes");

    let encoded = fixture_index(SOURCE, PositionEncoding::UTF8CodeUnitOffsetFromLineStart)
        .write_to_bytes()
        .expect("fixture SCIP index encodes");
    let repository = derive_repository(b"scip-import-fixture").id();
    let generation = GenerationId::from_bytes([7; 20]);
    let build_context = BuildContextIdentity::new(content_hash(b"scip-build"));
    let cancellation = Cancellation::new();
    let ir_limits = IrLimits::default();
    assert!(
        import_scip_index(
            &encoded,
            ScipImportRequest::new(
                repository,
                generation,
                build_context,
                &[],
                &ir_limits,
                &cancellation,
            ),
        )
        .is_err()
    );

    let tight_limits = ScipImportLimits::new(
        1,
        4_096,
        200_000,
        1_000_000,
        500_000,
        16 * 1024 * 1024,
        256 * 1024 * 1024,
    )
    .expect("tighter SCIP limits are accepted");
    let sources = [ScipImportSource::new("src/lib.rs", SOURCE, false)];
    assert!(
        import_scip_index(
            &encoded,
            ScipImportRequest::new(
                repository,
                generation,
                build_context,
                &sources,
                &ir_limits,
                &cancellation,
            )
            .with_limits(tight_limits),
        )
        .is_err()
    );
}

fn import(
    encoded: &[u8],
    source: &[u8],
) -> Result<rootlight_adapters::ScipImportOutcome, rootlight_adapters::ScipImportError> {
    import_with_cancellation(encoded, source, &Cancellation::new())
}

fn import_with_limits(
    encoded: &[u8],
    source: &[u8],
    limits: ScipImportLimits,
) -> Result<rootlight_adapters::ScipImportOutcome, rootlight_adapters::ScipImportError> {
    let cancellation = Cancellation::new();
    let repository = derive_repository(b"scip-import-fixture").id();
    let generation = GenerationId::from_bytes([7; 20]);
    let build_context = BuildContextIdentity::new(content_hash(b"scip-build"));
    let sources = [ScipImportSource::new("src/lib.rs", source, false)];
    let ir_limits = IrLimits::default();
    import_scip_index(
        encoded,
        ScipImportRequest::new(
            repository,
            generation,
            build_context,
            &sources,
            &ir_limits,
            &cancellation,
        )
        .with_limits(limits),
    )
}

fn import_with_cancellation(
    encoded: &[u8],
    source: &[u8],
    cancellation: &Cancellation,
) -> Result<rootlight_adapters::ScipImportOutcome, rootlight_adapters::ScipImportError> {
    let repository = derive_repository(b"scip-import-fixture").id();
    let generation = GenerationId::from_bytes([7; 20]);
    let build_context = BuildContextIdentity::new(content_hash(b"scip-build"));
    let sources = [ScipImportSource::new("src/lib.rs", source, false)];
    let ir_limits = IrLimits::default();
    import_scip_index(
        encoded,
        ScipImportRequest::new(
            repository,
            generation,
            build_context,
            &sources,
            &ir_limits,
            cancellation,
        ),
    )
}

fn fixture_index(source: &[u8], encoding: PositionEncoding) -> Index {
    let mut index = one_symbol_index(source, encoding);
    let document = &mut index.documents[0];
    document.symbols.push(symbol(CALLER, "caller"));
    document
        .symbols
        .last_mut()
        .expect("caller symbol exists")
        .relationships
        .push(Relationship {
            symbol: TARGET.to_owned(),
            is_reference: true,
            ..Default::default()
        });
    document.occurrences.push(occurrence(CALLER, 1, 3, 9, 0x1));
    document.occurrences.push(occurrence(TARGET, 1, 14, 20, 0));
    document.occurrences.remove(0);
    index
}

fn one_symbol_index(source: &[u8], encoding: PositionEncoding) -> Index {
    let mut metadata = Metadata {
        text_document_encoding: EnumOrUnknown::new(TextEncoding::UTF8),
        project_root: "file:///fixture".to_owned(),
        ..Default::default()
    };
    metadata.tool_info = MessageField::some(ToolInfo {
        name: "fixture-indexer".to_owned(),
        version: "1.0.0".to_owned(),
        ..Default::default()
    });
    let document = Document {
        language: "Rust".to_owned(),
        relative_path: "src/lib.rs".to_owned(),
        occurrences: vec![occurrence(TARGET, 0, 3, 9, 0x1)],
        symbols: vec![symbol(TARGET, "target")],
        text: String::from_utf8(source.to_vec()).expect("fixture source is UTF-8"),
        position_encoding: EnumOrUnknown::new(encoding),
        ..Default::default()
    };
    Index {
        metadata: MessageField::some(metadata),
        documents: vec![document],
        ..Default::default()
    }
}

fn symbol(identity: &str, display_name: &str) -> SymbolInformation {
    SymbolInformation {
        symbol: identity.to_owned(),
        kind: EnumOrUnknown::new(Kind::Function),
        display_name: display_name.to_owned(),
        ..Default::default()
    }
}

fn occurrence(
    symbol: &str,
    line: i32,
    start_character: i32,
    end_character: i32,
    symbol_roles: i32,
) -> scip::types::Occurrence {
    let mut occurrence = scip::types::Occurrence {
        symbol: symbol.to_owned(),
        symbol_roles,
        ..Default::default()
    };
    occurrence.set_single_line_range(SingleLineRange {
        line,
        start_character,
        end_character,
        ..Default::default()
    });
    occurrence
}
