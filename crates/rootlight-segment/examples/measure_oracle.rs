//! Measures the current segment prototype against an existing sealed oracle.
//!
//! The input remains read-only; this reports format costs before any durable
//! publication or recovery design relies on the segment representation.

use std::{env, io, path::PathBuf, time::Instant};

use rootlight_cancel::Cancellation;
use rootlight_catalog::OracleReader;
use rootlight_ir::{ExtensionSupport, IrLimits};
use rootlight_segment::{Segment, SegmentReader};
use rootlight_storage::{GenerationBudget, GenerationContext, GenerationReader};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut arguments = env::args_os().skip(1);
    let directory = arguments.next().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "expected one oracle directory")
    })?;
    if arguments.next().is_some() {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "expected one oracle directory").into(),
        );
    }
    let directory = PathBuf::from(directory).canonicalize()?;
    let cancellation = Cancellation::new();
    let context = GenerationContext::new(&cancellation, GenerationBudget::default());

    let read_start = Instant::now();
    let oracle = OracleReader::open_in(&directory, &context)?;
    let oracle_bytes = oracle.allocated_bytes(&context)?;
    let generation = oracle.read_generation(&context)?;
    let read_duration = read_start.elapsed();
    let expected_digests = generation.generation_neutral_digests(&context)?;

    let encode_start = Instant::now();
    let segment = Segment::encode(generation, oracle.stats(), &context)?;
    let encode_duration = encode_start.elapsed();
    let segment_bytes = segment.as_bytes().len();

    let open_start = Instant::now();
    let reader = SegmentReader::open(
        segment.into_bytes(),
        &IrLimits::default(),
        &ExtensionSupport::default(),
        &context,
    )?;
    let open_duration = open_start.elapsed();
    let actual_digests = reader
        .read_generation(&context)?
        .generation_neutral_digests(&context)?;
    if actual_digests != expected_digests || reader.stats() != oracle.stats() {
        return Err(
            io::Error::new(io::ErrorKind::InvalidData, "segment differs from oracle").into(),
        );
    }

    println!("oracle_bytes={oracle_bytes}");
    println!("segment_bytes={segment_bytes}");
    println!("oracle_read_micros={}", read_duration.as_micros());
    println!(
        "segment_encode_in_memory_micros={}",
        encode_duration.as_micros()
    );
    println!(
        "segment_open_in_memory_micros={}",
        open_duration.as_micros()
    );
    Ok(())
}
