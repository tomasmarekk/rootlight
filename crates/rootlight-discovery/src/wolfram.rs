//! Distinctive package headers disambiguate Wolfram sources with the shared .m suffix.
//! Only the first written expression contributes a routing hint; this does not
//! validate syntax or establish structural or semantic language support.

pub(super) fn has_content_hint(source: &[u8]) -> bool {
    let Some(mut source) = after_comments(source.strip_prefix(b"\xef\xbb\xbf").unwrap_or(source))
    else {
        return false;
    };
    let (name, package) = if let Some(tail) = source.strip_prefix(b"BeginPackage") {
        (tail, true)
    } else if let Some(tail) = source.strip_prefix(b"Paclet") {
        (tail, false)
    } else {
        return false;
    };
    let Some(tail) = after_comments(name).and_then(|tail| tail.strip_prefix(b"[")) else {
        return false;
    };
    let Some(tail) = after_comments(tail) else {
        return false;
    };
    source = tail;
    if !package {
        let Some(tail) = source.strip_prefix(b"Name").and_then(after_comments) else {
            return false;
        };
        let Some(tail) = tail.strip_prefix(b"->").and_then(after_comments) else {
            return false;
        };
        source = tail;
    }
    let Some(mut source) = source.strip_prefix(b"\"") else {
        return false;
    };
    while let Some((&byte, tail)) = source.split_first() {
        match byte {
            b'"' => {
                return after_comments(tail)
                    .is_some_and(|tail| tail.starts_with(b"]") || tail.starts_with(b","));
            }
            b'\\' => {
                let Some((_, tail)) = tail.split_first() else {
                    return false;
                };
                source = tail;
            }
            _ => source = tail,
        }
    }
    false
}

fn after_comments(mut source: &[u8]) -> Option<&[u8]> {
    loop {
        source = source.trim_ascii_start();
        let Some(tail) = source.strip_prefix(b"(*") else {
            return Some(source);
        };
        source = tail;
        let mut depth = 1_usize;
        while depth != 0 {
            if let Some(tail) = source.strip_prefix(b"(*") {
                depth += 1;
                source = tail;
            } else if let Some(tail) = source.strip_prefix(b"*)") {
                depth -= 1;
                source = tail;
            } else {
                source = source.split_first()?.1;
            }
        }
    }
}
