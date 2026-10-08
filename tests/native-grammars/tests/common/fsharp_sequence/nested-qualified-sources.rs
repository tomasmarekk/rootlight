// Nested qualified callee literals share the existing strict CalleeIdentitySource contract.
// They extend prefix depth only; compiler/native acceptance is still unobserved.

const NESTED_QUALIFIED_CALLEE_SOURCES: &[CalleeIdentitySource] = &[
    CalleeIdentitySource {
        id: "nested-qualified-operator:prefix-2:lf:single:eof",
        callee: "Library.Nested.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.Nested.(+) 11;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-operator:prefix-2:lf:double:eof",
        callee: "Library.Nested.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.Nested.(+) 11;;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-operator:prefix-2:crlf:single:eof",
        callee: "Library.Nested.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.Nested.(+) 11;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-operator:prefix-2:crlf:double:eof",
        callee: "Library.Nested.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.Nested.(+) 11;;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-operator:prefix-3:lf:single:eof",
        callee: "Library.Nested.Inner.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.Nested.Inner.(+) 11;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-operator:prefix-3:lf:double:eof",
        callee: "Library.Nested.Inner.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.Nested.Inner.(+) 11;;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-operator:prefix-3:crlf:single:eof",
        callee: "Library.Nested.Inner.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.Nested.Inner.(+) 11;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-operator:prefix-3:crlf:double:eof",
        callee: "Library.Nested.Inner.(+)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.Nested.Inner.(+) 11;;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-active-pattern:prefix-2:lf:single:eof",
        callee: "Library.Nested.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.Nested.(|Hit|Miss|) 11;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-active-pattern:prefix-2:lf:double:eof",
        callee: "Library.Nested.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.Nested.(|Hit|Miss|) 11;;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-active-pattern:prefix-2:crlf:single:eof",
        callee: "Library.Nested.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.Nested.(|Hit|Miss|) 11;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-active-pattern:prefix-2:crlf:double:eof",
        callee: "Library.Nested.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.Nested.(|Hit|Miss|) 11;;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-active-pattern:prefix-3:lf:single:eof",
        callee: "Library.Nested.Inner.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.Nested.Inner.(|Hit|Miss|) 11;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-active-pattern:prefix-3:lf:double:eof",
        callee: "Library.Nested.Inner.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\nlet run () = Library.Nested.Inner.(|Hit|Miss|) 11;;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-active-pattern:prefix-3:crlf:single:eof",
        callee: "Library.Nested.Inner.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.Nested.Inner.(|Hit|Miss|) 11;",
    },
    CalleeIdentitySource {
        id: "nested-qualified-active-pattern:prefix-3:crlf:double:eof",
        callee: "Library.Nested.Inner.(|Hit|Miss|)",
        direct_kind: None,
        source: "module CalleeIdentityExample\r\nlet run () = Library.Nested.Inner.(|Hit|Miss|) 11;;",
    },
];
