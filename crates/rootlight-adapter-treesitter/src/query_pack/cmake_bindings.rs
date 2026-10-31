//! A deliberately closed ordinary-set shape cannot redirect writes through arguments.

use std::ops::Range;

use rootlight_adapter_sdk::AdapterError;
use rootlight_cancel::Cancellation;

pub(crate) fn variable_name(bytes: &[u8]) -> bool {
    bytes
        .first()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || *byte == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

pub(crate) fn builtin_without_dispatch(name: &[u8]) -> bool {
    [b"set".as_slice(), b"message", b"unset", b"option"]
        .iter()
        .any(|builtin| name.eq_ignore_ascii_case(builtin))
}

pub(crate) fn guarded_builtin(name: &[u8]) -> bool {
    builtin_without_dispatch(name)
        || [
            b"list".as_slice(),
            b"if",
            b"elseif",
            b"else",
            b"endif",
            b"foreach",
            b"endforeach",
            b"while",
            b"endwhile",
            b"block",
            b"endblock",
        ]
        .iter()
        .any(|builtin| name.eq_ignore_ascii_case(builtin))
}

fn space(bytes: &[u8], at: &mut usize, cancel: &Cancellation) -> Result<(), AdapterError> {
    while bytes.get(*at).is_some_and(u8::is_ascii_whitespace) {
        cancel.check()?;
        *at += 1;
    }
    Ok(())
}

fn argument(
    bytes: &[u8],
    at: &mut usize,
    cancel: &Cancellation,
) -> Result<Option<(Range<usize>, bool)>, AdapterError> {
    cancel.check()?;
    let start = *at;
    if bytes.get(*at) == Some(&b'"') {
        *at += 1;
        let content = *at;
        let mut dynamic = false;
        let mut non_keyword = false;
        while let Some(byte) = bytes.get(*at).copied() {
            cancel.check()?;
            match byte {
                b'"' => {
                    let range = content..*at;
                    *at += 1;
                    return Ok(Some((range, dynamic && !non_keyword)));
                }
                b'\\' => return Ok(None),
                b'$' => {
                    if bytes.get(*at + 1) != Some(&b'{') {
                        return Ok(None);
                    }
                    let name = *at + 2;
                    *at = name;
                    while bytes
                        .get(*at)
                        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
                    {
                        cancel.check()?;
                        *at += 1;
                    }
                    if !bytes.get(name..*at).is_some_and(variable_name)
                        || bytes.get(*at) != Some(&b'}')
                    {
                        return Ok(None);
                    }
                    *at += 1;
                    dynamic = true;
                }
                _ => {
                    non_keyword |= !byte.is_ascii_uppercase() && byte != b'_';
                    *at += 1;
                }
            }
        }
        return Ok(None);
    }
    if bytes.get(*at) == Some(&b'[') {
        *at += 1;
        while bytes.get(*at) == Some(&b'=') {
            cancel.check()?;
            *at += 1;
        }
        if bytes.get(*at) != Some(&b'[') {
            return Ok(None);
        }
        let equals = *at - start - 1;
        *at += 1;
        let mut content = *at;
        if bytes.get(content..content + 2) == Some(b"\r\n") {
            content += 2;
        } else if bytes.get(content) == Some(&b'\n') {
            content += 1;
        }
        while let Some(byte) = bytes.get(*at) {
            cancel.check()?;
            if *byte == b']'
                && bytes
                    .get(*at + 1..*at + 1 + equals)
                    .is_some_and(|raw| raw.iter().all(|byte| *byte == b'='))
                && bytes.get(*at + 1 + equals) == Some(&b']')
            {
                let range = content..*at;
                *at += equals + 2;
                return Ok(Some((range, false)));
            }
            *at += 1;
        }
        return Ok(None);
    }
    while let Some(byte) = bytes.get(*at) {
        cancel.check()?;
        if byte.is_ascii_whitespace() || *byte == b')' {
            break;
        }
        if matches!(*byte, b'(' | b'\\' | b'$' | b';' | b'"' | b'#' | b'[') {
            return Ok(None);
        }
        *at += 1;
    }
    Ok((*at > start).then_some((start..*at, false)))
}

/// Independently restrict a native command to an unambiguous local normal write.
pub(crate) fn ordinary_set_name(
    bytes: &[u8],
    cancel: &Cancellation,
) -> Result<Option<Range<usize>>, AdapterError> {
    cancel.check()?;
    let mut at = 0;
    while bytes.get(at).is_some_and(|byte| byte.is_ascii_alphabetic()) {
        cancel.check()?;
        at += 1;
    }
    if !bytes
        .get(..at)
        .is_some_and(|name| name.eq_ignore_ascii_case(b"set"))
    {
        return Ok(None);
    }
    space(bytes, &mut at, cancel)?;
    if bytes.get(at) != Some(&b'(') {
        return Ok(None);
    }
    at += 1;
    space(bytes, &mut at, cancel)?;
    let Some((name, uncertain)) = argument(bytes, &mut at, cancel)? else {
        return Ok(None);
    };
    if uncertain || !bytes.get(name.clone()).is_some_and(variable_name) {
        return Ok(None);
    }
    let mut values = 0usize;
    loop {
        let before_space = at;
        space(bytes, &mut at, cancel)?;
        if bytes.get(at) == Some(&b')') {
            return Ok((at + 1 == bytes.len() && values > 0).then_some(name));
        }
        if before_space == at {
            return Ok(None);
        }
        let Some((value, uncertain)) = argument(bytes, &mut at, cancel)? else {
            return Ok(None);
        };
        if uncertain
            || bytes
                .get(value)
                .is_none_or(|value| matches!(value, b"CACHE" | b"PARENT_SCOPE"))
        {
            return Ok(None);
        }
        values += 1;
    }
}

pub(crate) fn preserves_bindings(
    bytes: &[u8],
    cancel: &Cancellation,
) -> Result<bool, AdapterError> {
    let mut at = 0;
    while bytes.get(at).is_some_and(|byte| byte.is_ascii_alphabetic()) {
        cancel.check()?;
        at += 1;
    }
    if !bytes
        .get(..at)
        .is_some_and(|name| name.eq_ignore_ascii_case(b"message"))
    {
        return Ok(false);
    }
    space(bytes, &mut at, cancel)?;
    if bytes.get(at) != Some(&b'(') {
        return Ok(false);
    }
    at += 1;
    let mut first = true;
    loop {
        let before = at;
        space(bytes, &mut at, cancel)?;
        if bytes.get(at) == Some(&b')') {
            return Ok(at + 1 == bytes.len());
        }
        if (!first && before == at) || argument(bytes, &mut at, cancel)?.is_none() {
            return Ok(false);
        }
        first = false;
    }
}
