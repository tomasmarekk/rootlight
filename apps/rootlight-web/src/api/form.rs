//! Bounded web query decoding for the API's form-encoded parameters.
//! Endpoint handlers enforce byte and pair limits before consuming this iterator.

use std::borrow::Cow;

/// Iterates nonempty form-encoded pairs in source order.
pub(crate) fn pairs(input: &str) -> impl Iterator<Item = (Cow<'_, str>, Cow<'_, str>)> {
    input
        .split('&')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let (name, value) = segment.split_once('=').unwrap_or((segment, ""));
            (decode(name), decode(value))
        })
}

fn decode(input: &str) -> Cow<'_, str> {
    if !input.as_bytes().contains(&b'+') && !input.as_bytes().contains(&b'%') {
        return Cow::Borrowed(input);
    }
    let mut bytes = Vec::new();
    let input_bytes = input.as_bytes();
    let mut position = 0;
    while let Some(&byte) = input_bytes.get(position) {
        if byte == b'+' {
            bytes.push(b' ');
            position += 1;
        } else if byte == b'%'
            && let (Some(&high), Some(&low)) =
                (input_bytes.get(position + 1), input_bytes.get(position + 2))
            && let (Some(high), Some(low)) = (hex(high), hex(low))
        {
            bytes.push(high * 16 + low);
            position += 3;
        } else {
            bytes.push(byte);
            position += 1;
        }
    }
    let decoded = match String::from_utf8(bytes) {
        Ok(decoded) => decoded,
        Err(invalid) => String::from_utf8_lossy(invalid.as_bytes()).into_owned(),
    };
    Cow::Owned(decoded)
}

fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::pairs;

    #[test]
    fn form_query_pairs_preserve_order_and_decoding() {
        let cases: &[(&str, &[(&str, &str)])] = &[
            ("", &[]),
            ("&a=1&&b=2&", &[("a", "1"), ("b", "2")]),
            (
                "flag&=value&empty=&equals=a=b",
                &[
                    ("flag", ""),
                    ("", "value"),
                    ("empty", ""),
                    ("equals", "a=b"),
                ],
            ),
            (
                "a+b=c+d&encoded=%2B%26%3D",
                &[("a b", "c d"), ("encoded", "+&=")],
            ),
            ("name=%E2%82%AC&raw=π", &[("name", "€"), ("raw", "π")]),
            (
                "invalid=%FF%C0%AF&partial=%&hex=%zz%2",
                &[("invalid", "���"), ("partial", "%"), ("hex", "%zz%2")],
            ),
            (
                "nul=%00&bom=%EF%BB%BFhello",
                &[("nul", "\0"), ("bom", "\u{feff}hello")],
            ),
            (
                "duplicate=one&duplicate=two",
                &[("duplicate", "one"), ("duplicate", "two")],
            ),
        ];
        for (input, expected_pairs) in cases {
            let actual = pairs(input)
                .map(|(name, value)| (name.into_owned(), value.into_owned()))
                .collect::<Vec<_>>();
            let expected = expected_pairs
                .iter()
                .map(|(name, value)| (name.to_string(), value.to_string()))
                .collect::<Vec<_>>();
            assert_eq!(actual, expected, "{input}");
        }
    }
}
