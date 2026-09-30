//! Pins down exact-byte reuse without sharing source or semantic identities.
//! Optional capacity exhaustion remains an uncached parse, while interruption
//! never publishes a failed lexical result for another file.

use super::*;
use crate::{ResolutionError, engine::ResolutionWorkBudget};
use rootlight_cancel::Cancellation;
use rootlight_ids::content_hash;

fn ready() -> Result<(), &'static str> {
    Ok(())
}

fn repeated(bytes: &[u8]) -> RepeatedSources {
    RepeatedSources::build(
        [(content_hash(bytes), bytes), (content_hash(bytes), bytes)],
        &mut ready,
    )
    .unwrap()
}

#[test]
fn identical_bytes_borrow_successful_ranges_without_reparsing() {
    let source = b"Attribute VB_Name = \"Worker\"\r\nPrivate Sub Class_Initialize()\r\nEnd Sub\r\nPublic Sub Run()\r\nDim worker As New Worker\r\nDim other As Object\r\nSet other = worker\r\nIf worker Is Nothing Then Exit Sub\r\nEnd Sub\r\n";
    let copy = source.to_vec();
    let keys = repeated(source);
    let mut cache = LexicalCache::new(&keys);
    let expected = rootlight_vba::class_initializer(source, &mut ready).unwrap();
    let mut calls = 0;
    let first_address;
    {
        let first = cache
            .get_or_parse(
                content_hash(source),
                source,
                None,
                &mut ready,
                |check| {
                    calls += 1;
                    rootlight_vba::class_initializer(source, check)
                },
                initializer_bytes,
            )
            .unwrap();
        assert_eq!(first.as_ref(), &expected);
        assert!(matches!(first, Cow::Borrowed(_)));
        first_address = first.as_ref() as *const _;
    }
    let second = cache
        .get_or_parse(
            content_hash(source),
            &copy,
            None,
            &mut ready,
            |_| {
                calls += 1;
                panic!("identical bytes must reuse the lexical result")
            },
            initializer_bytes,
        )
        .unwrap();
    assert_eq!(second.as_ref(), &expected);
    assert_eq!(second.as_ref() as *const _, first_address);
    assert_eq!(calls, 1);

    let mut reads = LexicalCache::new(&keys);
    let expected = rootlight_vba::statement_value_reads(source, 128, 128, &mut ready).unwrap();
    assert_eq!(expected.as_ref().unwrap().len(), 2);
    let original_storage;
    {
        let first = reads
            .get_or_parse(
                content_hash(source),
                source,
                Some((128, 128)),
                &mut ready,
                |check| rootlight_vba::statement_value_reads(source, 128, 128, check),
                value_read_bytes,
            )
            .unwrap();
        assert_eq!(first.as_ref(), &expected);
        original_storage = first.as_ref().as_ref().unwrap().as_ptr();
    }
    let second = reads
        .get_or_parse(
            content_hash(source),
            &copy,
            Some((128, 128)),
            &mut ready,
            |_| panic!("read vectors must be borrowed on an exact-byte hit"),
            value_read_bytes,
        )
        .unwrap();
    assert_eq!(second.as_ref(), &expected);
    assert_eq!(second.as_ref().as_ref().unwrap().as_ptr(), original_storage);
}

#[test]
fn collisions_lengths_and_read_limits_never_reuse_another_result() {
    let first = b"Sub Class_Initialize()\nEnd Sub\n";
    let different = b"Sub Other_Initialize()\nEnd Sub\n";
    assert_eq!(first.len(), different.len());
    let hash = content_hash(first);
    let keys = RepeatedSources::build(
        [(hash, first.as_slice()), (hash, first.as_slice())],
        &mut ready,
    )
    .unwrap();
    let mut cache = LexicalCache::new(&keys);
    cache
        .get_or_parse(
            hash,
            first,
            None,
            &mut ready,
            |check| rootlight_vba::class_initializer(first, check),
            initializer_bytes,
        )
        .unwrap();
    let collision = cache
        .get_or_parse(
            hash,
            different,
            None,
            &mut ready,
            |check| rootlight_vba::class_initializer(different, check),
            initializer_bytes,
        )
        .unwrap();
    assert_eq!(collision.as_ref(), &rootlight_vba::ClassInitializer::Absent);
    assert!(matches!(collision, Cow::Owned(_)));
    assert_eq!(cache.entries.len(), 1);
    assert_eq!(
        cache
            .get_or_parse(
                hash,
                first,
                None,
                &mut ready,
                |_| panic!("collision must preserve the representative"),
                initializer_bytes
            )
            .unwrap()
            .as_ref(),
        &rootlight_vba::ClassInitializer::Header(0..22)
    );
    let shorter = &first[..first.len() - 1];
    assert!(matches!(
        cache
            .get_or_parse(
                hash,
                shorter,
                None,
                &mut ready,
                |check| rootlight_vba::class_initializer(shorter, check),
                initializer_bytes
            )
            .unwrap(),
        Cow::Owned(_)
    ));

    let source = b"Sub Run()\nvalue = left + right\nEnd Sub\n";
    let keys = repeated(source);
    let mut cache = LexicalCache::new(&keys);
    for limits in [(128, 128), (1, 128), (128, 1), (128, 128)] {
        let expected =
            rootlight_vba::statement_value_reads(source, limits.0, limits.1, &mut ready).unwrap();
        let value = cache
            .get_or_parse(
                content_hash(source),
                source,
                Some(limits),
                &mut ready,
                |check| rootlight_vba::statement_value_reads(source, limits.0, limits.1, check),
                value_read_bytes,
            )
            .unwrap();
        assert_eq!(value.as_ref(), &expected, "limits={limits:?}");
    }
    assert_eq!(cache.entries.len(), 3);
}

#[test]
fn successful_negative_values_are_reusable_but_errors_are_not() {
    for source in [
        b"Sub Other()\nEnd Sub\n".as_slice(),
        b"#If Choice Then\nSub Class_Initialize()\nEnd Sub\n#End If\n",
    ] {
        let keys = repeated(source);
        let mut cache = LexicalCache::new(&keys);
        let expected = rootlight_vba::class_initializer(source, &mut ready).unwrap();
        assert!(!matches!(
            expected,
            rootlight_vba::ClassInitializer::Header(_)
        ));
        cache
            .get_or_parse(
                content_hash(source),
                source,
                None,
                &mut ready,
                |check| rootlight_vba::class_initializer(source, check),
                initializer_bytes,
            )
            .unwrap();
        assert_eq!(
            cache
                .get_or_parse(
                    content_hash(source),
                    source,
                    None,
                    &mut ready,
                    |_| panic!("successful negative inventory must be reused"),
                    initializer_bytes
                )
                .unwrap()
                .as_ref(),
            &expected
        );
    }
    let source = b"Sub Run()\nvalue = \"unterminated\nEnd Sub\n";
    let keys = repeated(source);
    let mut cache = LexicalCache::new(&keys);
    let value = cache
        .get_or_parse(
            content_hash(source),
            source,
            Some((128, 128)),
            &mut ready,
            |check| rootlight_vba::statement_value_reads(source, 128, 128, check),
            value_read_bytes,
        )
        .unwrap();
    assert!(value.as_ref().is_none());
    assert!(
        cache
            .get_or_parse(
                content_hash(source),
                source,
                Some((128, 128)),
                &mut ready,
                |_| panic!("successful None must be reused"),
                value_read_bytes
            )
            .unwrap()
            .as_ref()
            .is_none()
    );

    let mut errors = LexicalCache::<rootlight_vba::ClassInitializer>::new(&keys);
    for _ in 0..2 {
        assert_eq!(
            errors.get_or_parse(
                content_hash(source),
                source,
                None,
                &mut ready,
                |_| Err("interrupted parser"),
                initializer_bytes
            ),
            Err("interrupted parser")
        );
        assert!(errors.entries.is_empty());
        assert_eq!(errors.retained_result_bytes, 0);
    }
    errors
        .get_or_parse(
            content_hash(source),
            source,
            None,
            &mut ready,
            |check| rootlight_vba::class_initializer(source, check),
            initializer_bytes,
        )
        .unwrap();
    assert_eq!(errors.entries.len(), 1);
}

#[test]
fn unique_sources_and_capacity_exhaustion_parse_without_retaining() {
    let source = b"Sub Run()\nvalue = worker\nEnd Sub\n";
    let unique =
        RepeatedSources::build([(content_hash(source), source.as_slice())], &mut ready).unwrap();
    let keys = repeated(source);
    for (keys, entries, bytes) in [
        (&unique, MAX_CACHE_ENTRIES, MAX_RETAINED_RESULT_BYTES),
        (&keys, 0, MAX_RETAINED_RESULT_BYTES),
        (&keys, MAX_CACHE_ENTRIES, 0),
        (
            &keys,
            MAX_CACHE_ENTRIES,
            std::mem::size_of::<Option<Vec<rootlight_vba::ValueRead>>>(),
        ),
    ] {
        let mut cache = LexicalCache::new(keys);
        cache.maximum_entries = entries;
        cache.maximum_result_bytes = bytes;
        let mut parses = 0;
        for _ in 0..2 {
            let value = cache
                .get_or_parse(
                    content_hash(source),
                    source,
                    Some((128, 128)),
                    &mut ready,
                    |check| {
                        parses += 1;
                        rootlight_vba::statement_value_reads(source, 128, 128, check)
                    },
                    value_read_bytes,
                )
                .unwrap();
            assert_eq!(value.as_ref().as_ref().unwrap().len(), 1);
            assert!(matches!(value, Cow::Owned(_)));
        }
        assert_eq!(parses, 2);
        assert!(cache.entries.is_empty());
        assert_eq!(cache.retained_result_bytes, 0);
    }
    let sources: Vec<_> = (0..MAX_CANDIDATE_KEYS + 1)
        .map(|ordinal| ordinal.to_le_bytes())
        .collect();
    let keys = RepeatedSources::build(
        sources
            .iter()
            .map(|bytes| (content_hash(bytes), bytes.as_slice())),
        &mut ready,
    )
    .unwrap();
    assert_eq!(keys.keys.len(), MAX_CANDIDATE_KEYS);
    assert!(!keys.keys.contains_key(&SourceKey {
        hash: content_hash(sources.last().unwrap()),
        length: std::mem::size_of::<usize>()
    }));
}

#[test]
fn hit_checks_exact_chunk_budget_and_every_interruption_boundary() {
    let source = vec![b' '; COMPARISON_CHUNK_BYTES * 2 + 1];
    let keys = repeated(&source);
    let mut cache = LexicalCache::new(&keys);
    cache
        .get_or_parse(
            content_hash(&source),
            &source,
            None,
            &mut ready,
            |_| Ok(rootlight_vba::ClassInitializer::Absent),
            initializer_bytes,
        )
        .unwrap();
    let required = 5; // Request, three bounded byte chunks, final publication check.
    for maximum in [0, required - 1, required] {
        let mut work = ResolutionWorkBudget::new(maximum);
        let result = cache.get_or_parse(
            content_hash(&source),
            &source,
            None,
            &mut || work.consume(),
            |_| panic!("a hit must not parse"),
            initializer_bytes,
        );
        if maximum == required {
            assert_eq!(
                result.unwrap().as_ref(),
                &rootlight_vba::ClassInitializer::Absent
            );
        } else {
            assert!(
                matches!(result, Err(ResolutionError::WorkLimit { maximum: limit }) if limit == maximum)
            );
        }
        assert_eq!(work.consumed(), maximum);
        assert_eq!(cache.entries.len(), 1);
    }
    for stop in 1..=required {
        let mut calls = 0;
        let result = cache.get_or_parse(
            content_hash(&source),
            &source,
            None,
            &mut || {
                calls += 1;
                if calls == stop {
                    Err("cancelled")
                } else {
                    Ok(())
                }
            },
            |_| panic!("a hit must not parse"),
            initializer_bytes,
        );
        assert_eq!(result, Err("cancelled"));
        assert_eq!(calls, stop);
        assert_eq!(cache.entries.len(), 1);
    }
    let cancellation = Cancellation::new();
    cancellation.cancel(rootlight_cancel::CancellationReason::ClientRequest);
    let mut work = ResolutionWorkBudget::new(required);
    assert!(
        cache
            .get_or_parse(
                content_hash(&source),
                &source,
                None,
                &mut || {
                    cancellation.check()?;
                    work.consume()
                },
                |_| panic!("cancelled cache must not parse"),
                initializer_bytes
            )
            .is_err()
    );
    assert_eq!(work.consumed(), 0);
}

#[test]
fn prepass_and_result_publication_require_exact_charges() {
    let bytes = b"";
    let hash = content_hash(bytes);
    for maximum in [0, 2, 3] {
        let mut work = ResolutionWorkBudget::new(maximum);
        let result = RepeatedSources::build(
            [(hash, bytes.as_slice()), (hash, bytes.as_slice())],
            &mut || work.consume(),
        );
        assert_eq!(result.is_ok(), maximum == 3);
        assert_eq!(work.consumed(), maximum);
    }
    let keys = repeated(bytes);
    for maximum in [0, 2, 3] {
        let mut cache = LexicalCache::new(&keys);
        let mut work = ResolutionWorkBudget::new(maximum);
        let result = cache.get_or_parse(
            hash,
            bytes,
            None,
            &mut || work.consume(),
            |check| {
                check()?;
                Ok(rootlight_vba::ClassInitializer::Absent)
            },
            initializer_bytes,
        );
        assert_eq!(result.is_ok(), maximum == 3);
        assert_eq!(work.consumed(), maximum);
        assert_eq!(cache.entries.len(), usize::from(maximum == 3));
    }
    for stop in 1..=3 {
        let mut cache = LexicalCache::new(&keys);
        let mut calls = 0;
        assert_eq!(
            cache.get_or_parse(
                hash,
                bytes,
                None,
                &mut || {
                    calls += 1;
                    if calls == stop {
                        Err("cancelled")
                    } else {
                        Ok(())
                    }
                },
                |check| {
                    check()?;
                    Ok(rootlight_vba::ClassInitializer::Absent)
                },
                initializer_bytes
            ),
            Err("cancelled")
        );
        assert!(cache.entries.is_empty());
        assert_eq!(cache.retained_result_bytes, 0);
    }
}
