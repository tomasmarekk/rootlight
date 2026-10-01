//! Reads parameter modifiers and verifies source coverage of captured declarations.
//! Array eligibility is narrower than syntactic parameter recognition: ByVal
//! and incompatible modifier combinations cannot establish an array binding.

use std::ops::Range;

use super::logical_whitespace_len;

/// Checks exact declaration-range coverage of a raw parameter-list interior.
///
/// Ranges are relative to the unchanged source between the authored parentheses.
/// Each range must be nonempty, in bounds, source-ordered and nonoverlapping.
/// Prefix and suffix gaps may contain parameter trivia only; each intermediate
/// gap must contain trivia, exactly one comma, then trivia. An empty inventory
/// succeeds only when the entire interior is trivia, including bare Rem comments.
/// Trivia shares the module parser's logical whitespace, form-feed and bare Rem
/// rules. A Rem space/tab tail is not shortened to leave closing source tokens.
///
/// This allocation-free, nonrecursive scan verifies only source conservation.
/// It does not parse declaration interiors or establish names, types, passing
/// modes, compiler validity or callable arity. Callers must supply independently
/// source-verified declaration ranges and validate their declaration semantics.
/// The callback runs before iterator advances and during all trivia scans.
///
/// # Errors
/// Propagates iterator errors and the caller's cancellation/work error unchanged.
pub fn parameter_declarations_cover<E>(
    source: &[u8],
    declarations: impl IntoIterator<Item = Result<Range<usize>, E>>,
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    check()?;
    let mut declarations = declarations.into_iter();
    let mut cursor = 0;
    let mut first = true;
    loop {
        check()?;
        let Some(declaration) = declarations.next() else {
            break;
        };
        let declaration = declaration?;
        check()?;
        if declaration.start >= declaration.end
            || declaration.end > source.len()
            || declaration.start < cursor
        {
            return Ok(false);
        }
        // Keep lexical lookahead in the full remaining source. Truncating a gap
        // at a supplied range could turn the Rem prefix of a name into a comment.
        cursor += parameter_trivia_len(source.get(cursor..).unwrap_or_default(), check)?;
        if !first {
            if source.get(cursor) != Some(&b',') {
                return Ok(false);
            }
            cursor += 1;
            cursor += parameter_trivia_len(source.get(cursor..).unwrap_or_default(), check)?;
        }
        if cursor != declaration.start {
            return Ok(false);
        }
        cursor = declaration.end;
        first = false;
    }
    let remaining = source.get(cursor..).unwrap_or_default();
    Ok(parameter_trivia_len(remaining, check)? == remaining.len())
}

/// Returns the byte length of leading trivia in an original parameter region.
///
/// Accepts space/tab-based logical continuations, form-feed and bare Rem comments.
/// ASCII-word continuations and identifier type suffixes keep Rem inside a name;
/// a space/tab Rem tail is not shortened into this bare-comment subset.
/// A non-ASCII suffix conservatively keeps the potential name unconsumed without
/// decoding it or admitting a Unicode identifier or trivia form.
/// Bare line endings and other tokens stop the scan.
///
/// Supply the full remaining original parameter-list interior or declaration,
/// never a gap truncated at an expected name or declaration boundary: lexical
/// lookahead must distinguish Rem from the prefix of Remvalue or Rem$.
/// Callers compare the returned end with their independently source-verified
/// bounds. This allocation-free primitive does not prove declaration syntax,
/// name/type identity, passing semantics or completeness of a parameter list.
/// The callback runs before scanning and throughout every trivia segment.
///
/// # Errors
/// Propagates the caller's cancellation/work error unchanged.
pub fn parameter_trivia_len<E>(
    text: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<usize, E> {
    let mut cursor = 0;
    loop {
        check()?;
        cursor += logical_whitespace_len(text.get(cursor..).unwrap_or_default(), check)?;
        if text.get(cursor) == Some(&b'\x0c') {
            cursor += 1;
            continue;
        }
        let end = cursor.saturating_add(3);
        if text
            .get(cursor..end)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"Rem"))
            && text.get(end).is_none_or(|byte| {
                byte.is_ascii()
                    && !byte.is_ascii_alphanumeric()
                    && !matches!(
                        *byte,
                        b'_' | b' ' | b'\t' | b'%' | b'&' | b'^' | b'!' | b'#' | b'@' | b'$'
                    )
            })
        {
            // A bare Rem extra may occupy an empty parameter list. A space/tab
            // suffix instead consumes the rest of the physical line, including
            // closing tokens; never reinterpret that comment as short trivia.
            cursor = end;
            continue;
        }
        return Ok(cursor);
    }
}

/// Checks whether a complete parameter-modifier region permits an array.
///
/// The caller supplies only the source before the parameter name. Empty input
/// means implicit ByRef. ByRef and Optional may each occur once, in either order;
/// ParamArray must occur alone. ByVal, duplicates and other tokens return false.
/// Space/tab-based logical continuations are accepted, but bare newlines and
/// comments are not. Matching is ASCII case-insensitive and allocation-free.
/// The parameter's name, empty array designator, declared type and surrounding
/// parameter-list constraints remain caller obligations.
///
/// # Errors
/// Returns the caller's error unchanged when its cancellation/work callback fails.
pub fn array_parameter_modifiers<E>(
    source: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<bool, E> {
    let mut rest = source;
    let mut by_ref = false;
    let mut optional = false;
    let mut variadic = false;
    loop {
        let whitespace = logical_whitespace_len(rest, check)?;
        rest = rest.get(whitespace..).unwrap_or_default();
        if rest.is_empty() {
            return Ok(true);
        }
        let length = if rest
            .get(..5)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"ByRef"))
        {
            if by_ref || variadic {
                return Ok(false);
            }
            by_ref = true;
            5
        } else if rest
            .get(..8)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"Optional"))
        {
            if optional || variadic {
                return Ok(false);
            }
            optional = true;
            8
        } else if rest
            .get(..10)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"ParamArray"))
        {
            if by_ref || optional || variadic {
                return Ok(false);
            }
            variadic = true;
            10
        } else {
            return Ok(false);
        };
        rest = rest.get(length..).unwrap_or_default();
        if !rest.is_empty() && !matches!(rest.first(), Some(b' ' | b'\t')) {
            return Ok(false);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cover(source: &[u8], declarations: &[Range<usize>]) -> bool {
        parameter_declarations_cover(source, declarations.iter().cloned().map(Ok), &mut || {
            Ok::<(), ()>(())
        })
        .unwrap()
    }

    #[test]
    fn declaration_coverage_conserves_complete_mixed_source_and_exact_trivia_gaps() {
        for newline in ["\n", "\r\n"] {
            let declarations = [
                "ByVal\x0c[Rem]",
                "Optional Rem\x0cByRef\x0c[label] As String=\"Rem,\"\"As\"\":)\"",
                "written As Variant",
                "Optional absent",
            ];
            let gap = format!(" Rem, _{newline} Rem\x0c");
            let source = format!("Rem\x0c{} Rem", declarations.join(&gap));
            let ranges = declarations
                .iter()
                .map(|declaration| {
                    let start = source.find(declaration).unwrap();
                    start..start + declaration.len()
                })
                .collect::<Vec<_>>();
            assert!(cover(source.as_bytes(), &ranges));
            for (range, declaration) in ranges.iter().zip(declarations) {
                assert_eq!(&source[range.clone()], declaration);
            }
            let header = format!("Sub Run({source}){newline}End Sub{newline}");
            let inventory = crate::module_inventory(
                header.as_bytes(),
                crate::ModuleLimits::new(header.len(), 2, 1).with_max_parameters(4),
                &mut || Ok::<(), ()>(()),
            )
            .unwrap();
            let interior_start = "Sub Run(".len();
            let actual_ranges = inventory.procedures()[0]
                .parameters()
                .iter()
                .map(|parameter| {
                    let range = parameter.declaration();
                    Ok(range.start - interior_start..range.end - interior_start)
                });
            assert!(
                parameter_declarations_cover(
                    source.as_bytes(),
                    actual_ranges,
                    &mut || Ok::<(), ()>(())
                )
                .unwrap()
            );
        }
        assert!(cover(b"a,b", &[0..1, 2..3]));
        assert!(cover(b"a", &[Range { start: 0, end: 1 }]));
        assert!(cover(
            b"invalid syntax [] : =",
            &[Range { start: 0, end: 21 }]
        ));
    }

    #[test]
    fn declaration_coverage_empty_inventory_requires_only_complete_parameter_trivia() {
        for source in [
            "",
            " \t\x0c",
            "Rem",
            "rEm\x0c",
            " _\nRem\x0cRem",
            "Rem\x0c _\r\n Rem",
        ] {
            assert!(cover(source.as_bytes(), &[]), "{source:?}");
        }
        for source in [
            ",",
            "Rem,",
            "Rem=1",
            "Rem ",
            "Rem\t",
            "Rem As Long",
            "Remvalue",
            "Rem$",
            "[Rem]",
            "\n",
            "\r\n",
            " _ ",
            "' comment",
            "\0",
            "\x0b",
            "Rem\x0cunknown",
            "Rem\x0c,Rem",
        ] {
            assert!(!cover(source.as_bytes(), &[]), "{source:?}");
        }
    }

    #[test]
    fn declaration_coverage_rejects_missing_commas_invalid_spans_and_uncovered_text() {
        for (source, ranges) in [
            ("ab", vec![0..1, 1..2]),
            ("a b", vec![0..1, 2..3]),
            ("a,,b", vec![0..1, 3..4]),
            ("a,Rem,b", vec![0..1, 6..7]),
            ("a,", vec![Range { start: 0, end: 1 }]),
            (",a", vec![Range { start: 1, end: 2 }]),
            ("a,x,b", vec![0..1, 4..5]),
            ("prefix a", vec![Range { start: 7, end: 8 }]),
            ("a suffix", vec![Range { start: 0, end: 1 }]),
            ("a Rem ", vec![Range { start: 0, end: 1 }]),
            ("a,b", vec![0..1, 0..1]),
            ("a,b", vec![0..2, 1..3]),
            ("a,b", vec![2..3, 0..1]),
            ("a,b", vec![Range { start: 0, end: 0 }]),
            ("a,b", vec![Range { start: 1, end: 0 }]),
            ("a,b", vec![Range { start: 0, end: 4 }]),
            (
                "a,b",
                vec![Range {
                    start: usize::MAX,
                    end: usize::MAX,
                }],
            ),
            (
                "a,b",
                vec![Range {
                    start: 0,
                    end: usize::MAX,
                }],
            ),
            ("Remvalue", vec![Range { start: 3, end: 8 }]),
            ("Rem$", vec![Range { start: 3, end: 4 }]),
            ("Rem[value]", vec![0..3, 3..10]),
        ] {
            assert!(!cover(source.as_bytes(), &ranges), "{source:?}, {ranges:?}");
        }
        assert!(cover(b"Rem[value]", &[Range { start: 3, end: 10 }]));
        assert!(cover(b"Rem\x0c[value]", &[Range { start: 4, end: 11 }]));
    }

    #[test]
    fn declaration_coverage_does_not_rescan_literal_or_bracketed_declaration_contents() {
        for declaration in [
            "Optional [Rem] = \"Rem,value,\"\"quoted\"\":)\"",
            "Optional [value] As String = \"Rem \"",
            "[Rem]",
            "Optional value = \"Rem\x0c[Name],Other\"",
        ] {
            let source = format!("Rem\x0c{declaration} Rem,\x0cnext Rem");
            let first = 4..4 + declaration.len();
            let second_start = source.find("next").unwrap();
            assert!(
                cover(source.as_bytes(), &[first, second_start..second_start + 4]),
                "{source:?}"
            );
        }
    }

    #[test]
    fn declaration_coverage_propagates_iterator_errors_without_publishing_a_prefix() {
        assert_eq!(
            parameter_declarations_cover(b"", [Err("iterator")], &mut || Ok(())),
            Err("iterator")
        );
        assert_eq!(
            parameter_declarations_cover(b"a,b", [Ok(0..1), Err("late")], &mut || Ok(())),
            Err("late")
        );
        let mut advances = 0;
        let iterator = std::iter::from_fn(|| {
            advances += 1;
            Some(Ok::<_, &str>(0..1))
        });
        assert_eq!(
            parameter_declarations_cover(b"a", iterator, &mut || Err("initial")),
            Err("initial")
        );
        assert_eq!(advances, 0);
    }

    #[test]
    fn declaration_coverage_and_public_trivia_scans_cancel_at_every_checkpoint() {
        let trivia = format!(" {}Rem\x0c", " _\r\nRem\x0c".repeat(32));
        let source = format!("{trivia}a{trivia},{trivia}b{trivia}");
        let first = source.find('a').unwrap();
        let second = source.find('b').unwrap();
        let ranges = [first..first + 1, second..second + 1];
        let mut checks = 0;
        assert!(
            parameter_declarations_cover(
                source.as_bytes(),
                ranges.iter().cloned().map(Ok),
                &mut || {
                    checks += 1;
                    Ok::<(), ()>(())
                }
            )
            .unwrap()
        );
        for cancelled_at in 1..=checks {
            let mut calls = 0;
            assert_eq!(
                parameter_declarations_cover(
                    source.as_bytes(),
                    ranges.iter().cloned().map(Ok),
                    &mut || {
                        calls += 1;
                        if calls == cancelled_at {
                            Err("stop")
                        } else {
                            Ok(())
                        }
                    }
                ),
                Err("stop"),
                "coverage checkpoint {cancelled_at}"
            );
            assert_eq!(calls, cancelled_at);
        }
        let mut checks = 0;
        assert_eq!(
            parameter_trivia_len(trivia.as_bytes(), &mut || {
                checks += 1;
                Ok::<(), ()>(())
            })
            .unwrap(),
            trivia.len()
        );
        for cancelled_at in 1..=checks {
            let mut calls = 0;
            assert_eq!(
                parameter_trivia_len(trivia.as_bytes(), &mut || {
                    calls += 1;
                    if calls == cancelled_at {
                        Err("stop")
                    } else {
                        Ok(())
                    }
                }),
                Err("stop")
            );
            assert_eq!(calls, cancelled_at);
        }
    }

    #[test]
    fn public_parameter_trivia_preserves_full_name_lookahead_and_suffixes() {
        for (source, expected) in [
            ("Rem", 3),
            ("Rem\x0c", 4),
            ("Rem[value]", 3),
            (" \tRem\x0c[value]", 6),
            ("Remvalue", 0),
            ("Rem0", 0),
            ("Rem_", 0),
            ("Rem ", 0),
            ("Rem\t", 0),
            ("Rem%", 0),
            ("Rem&", 0),
            ("Rem^", 0),
            ("Rem!", 0),
            ("Rem#", 0),
            ("Rem@", 0),
            ("Rem$", 0),
            ("Remé", 0),
            ("Remα", 0),
            ("Rem\u{00a0}", 0),
            ("Rem😀", 0),
            ("[Rem]", 0),
            (" _\r\nRem\x0c[name]", 8),
        ] {
            assert_eq!(
                parameter_trivia_len(source.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                expected,
                "{source:?}"
            );
        }
        for byte in 0..=u8::MAX {
            let source = [b'R', b'e', b'm', byte];
            let expected = if !byte.is_ascii()
                || byte.is_ascii_alphanumeric()
                || b"_ \t%&^!#@$".contains(&byte)
            {
                0
            } else if byte == b'\x0c' {
                4
            } else {
                3
            };
            assert_eq!(
                parameter_trivia_len(&source, &mut || Ok::<(), ()>(())).unwrap(),
                expected,
                "suffix byte {byte}"
            );
        }
    }

    #[test]
    fn array_modifiers_keep_order_alternatives_and_logical_lines() {
        for input in [
            "",
            " \t",
            "ByRef",
            "Optional",
            "ParamArray",
            "Optional ByRef",
            "ByRef Optional",
            " bYrEf _\r\n Optional _\n ",
            "ParamArray _\n ",
        ] {
            assert!(
                array_parameter_modifiers(input.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                "{input:?}"
            );
        }
        for input in [
            "ByVal",
            "Optional ByVal",
            "ByRef ByRef",
            "Optional Optional",
            "Optional ParamArray",
            "ParamArray Optional",
            "ParamArray ByRef",
            "ByRef ParamArray",
            "ParamArray ParamArray",
            "ByRefOptional",
            "ByRef_",
            "ByRef \n",
            "ByRef _ ",
            "ByRef ' note",
            "ByRef :",
            "ByRefX",
        ] {
            assert!(
                !array_parameter_modifiers(input.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                "{input:?}"
            );
        }
    }

    #[test]
    fn modifier_reads_propagate_cancellation_and_reject_arbitrary_tokens() {
        for input in [b"ByRef ".to_vec(), vec![b' '; 8192], b" _\r\n".repeat(8192)] {
            assert_eq!(
                array_parameter_modifiers(&input, &mut || Err("cancelled")),
                Err("cancelled")
            );
        }
        let mut calls = 0;
        assert_eq!(
            array_parameter_modifiers(&vec![b' '; 8192], &mut || {
                calls += 1;
                if calls == 5 { Err("budget") } else { Ok(()) }
            }),
            Err("budget")
        );
        assert_eq!(calls, 5);
        for byte in 0..=u8::MAX {
            let input = [b'B', b'y', b'R', b'e', b'f', byte];
            let valid = array_parameter_modifiers(&input, &mut || Ok::<(), ()>(())).unwrap();
            assert_eq!(valid, matches!(byte, b' ' | b'\t'));
        }
    }
}
