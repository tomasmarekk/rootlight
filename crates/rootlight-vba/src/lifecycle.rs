//! Checks lifecycle declaration inventory without relying on structural captures.
//! Excluded conditional headers cannot be mistaken for an absent initializer.

use std::ops::Range;

/// Source-only initializer inventory; class identity and bodies require separate proof.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClassInitializer {
    /// No initializer procedure header occurs in the complete lexical inventory.
    Absent,
    /// One unconditional parameterless Sub header, relative to the module input.
    Header(Range<usize>),
    /// Conditional, duplicate, malformed or unsupported declaration boundaries.
    Unproven,
}

/// Inventories the class initializer's written header across a complete module.
///
/// Strings, comments, dates and continued statements cannot introduce headers.
/// Conditional initializer declarations remain unproven without a compilation
/// context, even if a parser omitted them. Unrelated conditional procedures do
/// not invalidate an unconditional initializer. The caller must reconcile the
/// result with complete class-member facts, including non-procedure conflicts.
/// This does not prove class identity, body validity or runtime success.
/// Scanning is allocation-free, linear in the input and cancellable.
///
/// # Errors
/// Propagates the caller's cancellation/work error unchanged.
pub fn class_initializer<E>(
    source: &[u8],
    check: &mut impl FnMut() -> Result<(), E>,
) -> Result<ClassInitializer, E> {
    check()?;
    let mut cursor = 0;
    let mut depth = 0_usize;
    let mut initializer = None;
    while cursor < source.len() {
        check()?;
        let Some((range, next)) = super::with_blocks::statement(source, cursor, false, check)?
        else {
            return Ok(ClassInitializer::Unproven);
        };
        cursor = next;
        let text = &source[range.clone()];
        if let Some(directive) = text.strip_prefix(b"#") {
            let (keyword, tail) = word(directive.trim_ascii_start());
            if keyword.eq_ignore_ascii_case(b"If") {
                depth += 1;
            } else if keyword.eq_ignore_ascii_case(b"Else")
                || keyword.eq_ignore_ascii_case(b"ElseIf")
            {
                if depth == 0 {
                    return Ok(ClassInitializer::Unproven);
                }
            } else if keyword.eq_ignore_ascii_case(b"End")
                && tail.trim_ascii().eq_ignore_ascii_case(b"If")
            {
                let Some(next) = depth.checked_sub(1) else {
                    return Ok(ClassInitializer::Unproven);
                };
                depth = next;
            } else if !keyword.eq_ignore_ascii_case(b"Const") {
                return Ok(ClassInitializer::Unproven);
            }
            continue;
        }
        let Some(header) = super::procedure_header(text, check)? else {
            // A split declaration cannot disappear between conditional branches.
            if super::procedure::prefix_kind(text, &mut 0, check)?.is_some() {
                return Ok(ClassInitializer::Unproven);
            }
            continue;
        };
        if header.name().eq_ignore_ascii_case(b"Class_Initialize") {
            if depth != 0 || initializer.is_some() || !super::parameterless_sub(text, check)? {
                return Ok(ClassInitializer::Unproven);
            }
            initializer = Some(range);
        }
    }
    Ok(if depth != 0 {
        ClassInitializer::Unproven
    } else {
        initializer.map_or(ClassInitializer::Absent, ClassInitializer::Header)
    })
}

fn word(source: &[u8]) -> (&[u8], &[u8]) {
    let end = source
        .iter()
        .position(|byte| !byte.is_ascii_alphabetic())
        .unwrap_or(source.len());
    source.split_at(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initializer_inventory_preserves_boundaries_and_ignores_body_text() {
        let header = "Private Sub [Class_Initialize] _\r\n ( _\n )";
        let source = format!(
            "Attribute VB_Name = \"Worker\"\n#If Choice Then\nSub Other()\nEnd Sub\n#End If\n{header} ' comment\n#If Choice Then\nDim x As Long\n#End If\nEnd Sub\n"
        );
        let expected = source.find(header).unwrap();
        assert_eq!(
            class_initializer(source.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
            ClassInitializer::Header(expected..expected + header.len())
        );
        for source in [
            "",
            "' Sub Class_Initialize()\nRem Sub Class_Initialize()\n",
            "Sub Other()\ntext = \"Sub Class_Initialize()\"\nvalue = #1/1/2000#\nEnd Sub\n",
            "#If Choice Then\nSub Other()\nEnd Sub\n#Else\nSub Alternate()\nEnd Sub\n#End If\n",
            "#Const Choice = True\nSub Other()\nEnd Sub\n",
        ] {
            assert_eq!(
                class_initializer(source.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                ClassInitializer::Absent,
                "{source}"
            );
        }
    }

    #[test]
    fn unsupported_headers_cannot_become_absence_or_unconditional_calls() {
        for source in [
            "#If Choice Then\nPrivate Sub Class_Initialize()\nEnd Sub\n#End If\n",
            "#If True Then\nPrivate Sub Class_Initialize()\nEnd Sub\n#End If\n",
            "Sub Class_Initialize(): End Sub: Sub Class_Initialize(): End Sub",
            "Function Class_Initialize() As Long\nEnd Function",
            "Sub Class_Initialize(value As Long)\nEnd Sub",
            "Private Sub\n#If Choice Then\nClass_Initialize()\n#Else\nOther()\n#End If\nEnd Sub",
            "#If Choice Then\nSub Other()\nEnd Sub",
            "#Else\nSub Other()\nEnd Sub\n#End If",
            "#End If\nSub Other()\nEnd Sub",
            "Sub Other(\nEnd Sub",
            "Sub Other()\nvalue = \"unterminated\nEnd Sub",
        ] {
            assert_eq!(
                class_initializer(source.as_bytes(), &mut || Ok::<(), ()>(())).unwrap(),
                ClassInitializer::Unproven,
                "{source}"
            );
        }
    }

    #[test]
    fn inventory_remains_cancellable_and_total_on_arbitrary_bytes() {
        for source in [
            "'".to_owned() + &"x".repeat(8192),
            "Sub Other(): End Sub\n".repeat(8192),
        ] {
            let mut calls = 0;
            assert_eq!(
                class_initializer(source.as_bytes(), &mut || {
                    calls += 1;
                    if calls == 16 {
                        Err("cancelled")
                    } else {
                        Ok(())
                    }
                }),
                Err("cancelled")
            );
            assert_eq!(calls, 16);
        }
        for byte in 0..=u8::MAX {
            let source = [b"Sub Class_Initialize()".as_slice(), &[byte], b"\nEnd Sub"].concat();
            for end in 0..=source.len() {
                if let ClassInitializer::Header(range) =
                    class_initializer(&source[..end], &mut || Ok::<(), ()>(())).unwrap()
                {
                    assert!(range.start < range.end && range.end <= end);
                }
            }
        }
    }
}
