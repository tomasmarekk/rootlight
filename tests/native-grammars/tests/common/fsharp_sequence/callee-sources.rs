// Exact neutral callee sources qualify canonical CST routes and edit restoration.
// Unicode is a single XID identifier interoperability case; no compiler semantics
// or previously unobserved syntax acceptance is claimed by this source table.

#[derive(Debug)]
struct CalleeIdentitySource {
    id: &'static str,
    callee: &'static str,
    direct_kind: Option<&'static str>,
    source: &'static str,
}

const CALLEE_IDENTITY_SOURCES: &[CalleeIdentitySource] = &[
    CalleeIdentitySource {
        id: "plain:lf:single:eof",
        callee: "lookup",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\nlet run () = lookup 11;",
    },
    CalleeIdentitySource {
        id: "plain:lf:double:eof",
        callee: "lookup",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\nlet run () = lookup 11;;",
    },
    CalleeIdentitySource {
        id: "plain:crlf:single:eof",
        callee: "lookup",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\r\nlet run () = lookup 11;",
    },
    CalleeIdentitySource {
        id: "plain:crlf:double:eof",
        callee: "lookup",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\r\nlet run () = lookup 11;;",
    },
    CalleeIdentitySource {
        id: "backtick:lf:single:eof",
        callee: "``lookup value``",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\nlet run () = ``lookup value`` 11;",
    },
    CalleeIdentitySource {
        id: "backtick:lf:double:eof",
        callee: "``lookup value``",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\nlet run () = ``lookup value`` 11;;",
    },
    CalleeIdentitySource {
        id: "backtick:crlf:single:eof",
        callee: "``lookup value``",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\r\nlet run () = ``lookup value`` 11;",
    },
    CalleeIdentitySource {
        id: "backtick:crlf:double:eof",
        callee: "``lookup value``",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\r\nlet run () = ``lookup value`` 11;;",
    },
    CalleeIdentitySource {
        id: "unicode:lf:single:eof",
        callee: "λ",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\nlet run () = λ 11;",
    },
    CalleeIdentitySource {
        id: "unicode:lf:double:eof",
        callee: "λ",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\nlet run () = λ 11;;",
    },
    CalleeIdentitySource {
        id: "unicode:crlf:single:eof",
        callee: "λ",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\r\nlet run () = λ 11;",
    },
    CalleeIdentitySource {
        id: "unicode:crlf:double:eof",
        callee: "λ",
        direct_kind: Some("identifier"),
        source: "module CalleeIdentityExample\r\nlet run () = λ 11;;",
    },
    CalleeIdentitySource {
        id: "dotted:lf:single:eof",
        callee: "Library.lookup",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.lookup 11;",
    },
    CalleeIdentitySource {
        id: "dotted:lf:double:eof",
        callee: "Library.lookup",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.lookup 11;;",
    },
    CalleeIdentitySource {
        id: "dotted:crlf:single:eof",
        callee: "Library.lookup",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.lookup 11;",
    },
    CalleeIdentitySource {
        id: "dotted:crlf:double:eof",
        callee: "Library.lookup",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.lookup 11;;",
    },
    CalleeIdentitySource {
        id: "nested-dotted:lf:single:eof",
        callee: "Library.Nested.lookup",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.Nested.lookup 11;",
    },
    CalleeIdentitySource {
        id: "nested-dotted:lf:double:eof",
        callee: "Library.Nested.lookup",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.Nested.lookup 11;;",
    },
    CalleeIdentitySource {
        id: "nested-dotted:crlf:single:eof",
        callee: "Library.Nested.lookup",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.Nested.lookup 11;",
    },
    CalleeIdentitySource {
        id: "nested-dotted:crlf:double:eof",
        callee: "Library.Nested.lookup",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.Nested.lookup 11;;",
    },
    CalleeIdentitySource {
        id: "operator:lf:single:eof",
        callee: "(+)",
        direct_kind: Some("op_identifier"),
        source: "module CalleeIdentityExample\nlet run () = (+) 11;",
    },
    CalleeIdentitySource {
        id: "operator:lf:double:eof",
        callee: "(+)",
        direct_kind: Some("op_identifier"),
        source: "module CalleeIdentityExample\nlet run () = (+) 11;;",
    },
    CalleeIdentitySource {
        id: "operator:crlf:single:eof",
        callee: "(+)",
        direct_kind: Some("op_identifier"),
        source: "module CalleeIdentityExample\r\nlet run () = (+) 11;",
    },
    CalleeIdentitySource {
        id: "operator:crlf:double:eof",
        callee: "(+)",
        direct_kind: Some("op_identifier"),
        source: "module CalleeIdentityExample\r\nlet run () = (+) 11;;",
    },
    CalleeIdentitySource {
        id: "qualified-operator:lf:single:eof",
        callee: "Library.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.(+) 11;",
    },
    CalleeIdentitySource {
        id: "qualified-operator:lf:double:eof",
        callee: "Library.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.(+) 11;;",
    },
    CalleeIdentitySource {
        id: "qualified-operator:crlf:single:eof",
        callee: "Library.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.(+) 11;",
    },
    CalleeIdentitySource {
        id: "qualified-operator:crlf:double:eof",
        callee: "Library.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.(+) 11;;",
    },
    CalleeIdentitySource {
        id: "active-pattern:lf:single:eof",
        callee: "(|Hit|Miss|)",
        direct_kind: Some("active_pattern"),
        source: "module CalleeIdentityExample\nlet run () = (|Hit|Miss|) 11;",
    },
    CalleeIdentitySource {
        id: "active-pattern:lf:double:eof",
        callee: "(|Hit|Miss|)",
        direct_kind: Some("active_pattern"),
        source: "module CalleeIdentityExample\nlet run () = (|Hit|Miss|) 11;;",
    },
    CalleeIdentitySource {
        id: "active-pattern:crlf:single:eof",
        callee: "(|Hit|Miss|)",
        direct_kind: Some("active_pattern"),
        source: "module CalleeIdentityExample\r\nlet run () = (|Hit|Miss|) 11;",
    },
    CalleeIdentitySource {
        id: "active-pattern:crlf:double:eof",
        callee: "(|Hit|Miss|)",
        direct_kind: Some("active_pattern"),
        source: "module CalleeIdentityExample\r\nlet run () = (|Hit|Miss|) 11;;",
    },
    CalleeIdentitySource {
        id: "qualified-active-pattern:lf:single:eof",
        callee: "Library.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.(|Hit|Miss|) 11;",
    },
    CalleeIdentitySource {
        id: "qualified-active-pattern:lf:double:eof",
        callee: "Library.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.(|Hit|Miss|) 11;;",
    },
    CalleeIdentitySource {
        id: "qualified-active-pattern:crlf:single:eof",
        callee: "Library.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.(|Hit|Miss|) 11;",
    },
    CalleeIdentitySource {
        id: "qualified-active-pattern:crlf:double:eof",
        callee: "Library.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.(|Hit|Miss|) 11;;",
    },
];
