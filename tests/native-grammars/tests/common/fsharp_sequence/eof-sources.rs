// Exact source/profile strings from the authenticated FCS176 and accepted15 inputs.
// Included by prospective native EOF controls; hashes are checked by the source validator.

#[derive(Debug, Clone, Copy)]
struct EofSource {
    id: &'static str,
    source: &'static str,
    signature: bool,
}

const EOF_NAMED_SOURCES: &[EofSource] = &[
    EofSource {
        id: "named-eof:single:lf:eof",
        source: "module TerminatorExample\nlet run () = ignore 11;",
        signature: false,
    },
    EofSource {
        id: "named-eof:single:lf:newline",
        source: "module TerminatorExample\nlet run () = ignore 11;\n",
        signature: false,
    },
    EofSource {
        id: "named-eof:single:crlf:eof",
        source: "module TerminatorExample\r\nlet run () = ignore 11;",
        signature: false,
    },
    EofSource {
        id: "named-eof:single:crlf:newline",
        source: "module TerminatorExample\r\nlet run () = ignore 11;\r\n",
        signature: false,
    },
    EofSource {
        id: "named-eof:double:lf:eof",
        source: "module TerminatorExample\nlet run () = ignore 11;;",
        signature: false,
    },
    EofSource {
        id: "named-eof:double:lf:newline",
        source: "module TerminatorExample\nlet run () = ignore 11;;\n",
        signature: false,
    },
    EofSource {
        id: "named-eof:double:crlf:eof",
        source: "module TerminatorExample\r\nlet run () = ignore 11;;",
        signature: false,
    },
    EofSource {
        id: "named-eof:double:crlf:newline",
        source: "module TerminatorExample\r\nlet run () = ignore 11;;\r\n",
        signature: false,
    },
];

const EOF_SELECTION_SOURCES: &[EofSource] = &[
    EofSource {
        id: "namespace-inline:single:lf:eof",
        source: "namespace TerminatorExample\nlet run () =\n    ignore 11; ignore 12; ignore 13; ()\nlet following = 30",
        signature: false,
    },
    EofSource {
        id: "namespace-inline:double:lf:eof",
        source: "namespace TerminatorExample\nlet run () =\n    ignore 11; ignore 12;; ignore 13; ()\nlet following = 30",
        signature: false,
    },
    EofSource {
        id: "signature-value-boundary:single:lf:eof",
        source: "module TerminatorExample\nval first : int;\nval following : int",
        signature: true,
    },
    EofSource {
        id: "signature-value-boundary:double:lf:eof",
        source: "module TerminatorExample\nval first : int;;\nval following : int",
        signature: true,
    },
    EofSource {
        id: "named-eof:single:lf:eof",
        source: "module TerminatorExample\nlet run () = ignore 11;",
        signature: false,
    },
    EofSource {
        id: "named-eof:double:lf:eof",
        source: "module TerminatorExample\nlet run () = ignore 11;;",
        signature: false,
    },
    EofSource {
        id: "line-comment:single:lf:eof",
        source: "module TerminatorExample\nlet run () = ignore 11; // tail\nlet following = 30",
        signature: false,
    },
    EofSource {
        id: "line-comment:double:lf:eof",
        source: "module TerminatorExample\nlet run () = ignore 11;; // tail\nlet following = 30",
        signature: false,
    },
    EofSource {
        id: "block-comment:single:lf:eof",
        source: "module TerminatorExample\nlet run () = ignore 11; (* tail (* nested *) *)\nlet following = 30",
        signature: false,
    },
    EofSource {
        id: "block-comment:double:lf:eof",
        source: "module TerminatorExample\nlet run () = ignore 11;; (* tail (* nested *) *)\nlet following = 30",
        signature: false,
    },
    EofSource {
        id: "nested-module-inline:single:lf:eof",
        source: "module TerminatorExample\nmodule Inner =\n    let run () =\n        ignore 11; ignore 12; ignore 13; ()\n    let innerFollowing = 20\nlet following = 30",
        signature: false,
    },
    EofSource {
        id: "local-binding-without-continuation:single:lf:eof",
        source: "module TerminatorExample\nlet run () =\n    let value = 11;\n    ignore 13\n    ()\nlet following = 30",
        signature: false,
    },
    EofSource {
        id: "begin-end-barrier:single:lf:eof",
        source: "module TerminatorExample\nlet run () = begin ignore 11; ignore 12 end\nlet following = 30",
        signature: false,
    },
    EofSource {
        id: "record-barrier:single:lf:eof",
        source: "module TerminatorExample\nlet run () = { First = 11; Second = 12 }\nlet following = 30",
        signature: false,
    },
    EofSource {
        id: "computation-barrier:single:lf:eof",
        source: "module TerminatorExample\nlet run () = seq { yield 11; yield 12 }\nlet following = 30",
        signature: false,
    },
];
