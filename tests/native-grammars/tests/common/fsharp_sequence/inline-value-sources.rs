// Parameterless inline values exercise the private name alias, not function heads.
// LF/CRLF sources differ only in line endings; the body and following declaration
// remain unchanged when the bound name is renamed and restored.

struct InlineValueNameSource {
    id: &'static str,
    source: &'static str,
}

const INLINE_VALUE_NAME_SOURCES: &[InlineValueNameSource] = &[
    InlineValueNameSource {
        id: "inline-value-name:lf:eof",
        source: "module InlineValueExample\nlet inline add3 = add 3\nlet following = 30",
    },
    InlineValueNameSource {
        id: "inline-value-name:crlf:eof",
        source: "module InlineValueExample\r\nlet inline add3 = add 3\r\nlet following = 30",
    },
];
