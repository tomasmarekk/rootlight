// Proposed argument-token sources for a private native control fragment.
// Exact bytes require an independent compiler oracle before qualification.

#[derive(Clone, Copy)]
enum ArgumentTokenFamily {
    MultiplyPair,
    PlusPair,
    UnitAdjacent,
    UnitSpaced,
    ParenthesisPair,
    GenericConstructorComment,
    AdjacentPlainString,
    AdjacentArrayRecord,
}

struct ArgumentTokenSource {
    id: &'static str,
    family: ArgumentTokenFamily,
    targets: &'static [&'static str],
    source: &'static str,
}

const ARGUMENT_TOKEN_SOURCES: [ArgumentTokenSource; 16] = [
    ArgumentTokenSource {
        id: "multiply-pair-lf-eof",
        family: ArgumentTokenFamily::MultiplyPair,
        targets: &["consume(*)", "consume (*)"],
        source: "module ArgumentTokenExample\nlet run () =\n    ignore 101\n    consume(*)\n    consume (*)\n    ignore 103\n    ()\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "multiply-pair-crlf-eof",
        family: ArgumentTokenFamily::MultiplyPair,
        targets: &["consume(*)", "consume (*)"],
        source: "module ArgumentTokenExample\r\nlet run () =\r\n    ignore 101\r\n    consume(*)\r\n    consume (*)\r\n    ignore 103\r\n    ()\r\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "plus-pair-lf-eof",
        family: ArgumentTokenFamily::PlusPair,
        targets: &["consume(+)", "consume (+)"],
        source: "module ArgumentTokenExample\nlet run () =\n    ignore 101\n    consume(+)\n    consume (+)\n    ignore 103\n    ()\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "plus-pair-crlf-eof",
        family: ArgumentTokenFamily::PlusPair,
        targets: &["consume(+)", "consume (+)"],
        source: "module ArgumentTokenExample\r\nlet run () =\r\n    ignore 101\r\n    consume(+)\r\n    consume (+)\r\n    ignore 103\r\n    ()\r\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "unit-adjacent-lf-eof",
        family: ArgumentTokenFamily::UnitAdjacent,
        targets: &["consume()"],
        source: "module ArgumentTokenExample\nlet run () =\n    ignore 101\n    consume()\n    ignore 103\n    ()\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "unit-adjacent-crlf-eof",
        family: ArgumentTokenFamily::UnitAdjacent,
        targets: &["consume()"],
        source: "module ArgumentTokenExample\r\nlet run () =\r\n    ignore 101\r\n    consume()\r\n    ignore 103\r\n    ()\r\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "unit-spaced-lf-eof",
        family: ArgumentTokenFamily::UnitSpaced,
        targets: &["consume ()"],
        source: "module ArgumentTokenExample\nlet run () =\n    ignore 101\n    consume ()\n    ignore 103\n    ()\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "unit-spaced-crlf-eof",
        family: ArgumentTokenFamily::UnitSpaced,
        targets: &["consume ()"],
        source: "module ArgumentTokenExample\r\nlet run () =\r\n    ignore 101\r\n    consume ()\r\n    ignore 103\r\n    ()\r\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "parenthesis-pair-lf-eof",
        family: ArgumentTokenFamily::ParenthesisPair,
        targets: &["consume(value)", "consume (value)"],
        source: "module ArgumentTokenExample\nlet run () =\n    ignore 101\n    consume(value)\n    consume (value)\n    ignore 103\n    ()\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "parenthesis-pair-crlf-eof",
        family: ArgumentTokenFamily::ParenthesisPair,
        targets: &["consume(value)", "consume (value)"],
        source: "module ArgumentTokenExample\r\nlet run () =\r\n    ignore 101\r\n    consume(value)\r\n    consume (value)\r\n    ignore 103\r\n    ()\r\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "generic-constructor-comment-lf-eof",
        family: ArgumentTokenFamily::GenericConstructorComment,
        targets: &["new 'T(*c*)(value)"],
        source: "module ArgumentTokenExample\nlet run () =\n    ignore 101\n    new 'T(*c*)(value)\n    ignore 103\n    ()\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "generic-constructor-comment-crlf-eof",
        family: ArgumentTokenFamily::GenericConstructorComment,
        targets: &["new 'T(*c*)(value)"],
        source: "module ArgumentTokenExample\r\nlet run () =\r\n    ignore 101\r\n    new 'T(*c*)(value)\r\n    ignore 103\r\n    ()\r\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "adjacent-plain-string-lf-eof",
        family: ArgumentTokenFamily::AdjacentPlainString,
        targets: &["printfn\"%s\"value"],
        source: "module ArgumentTokenExample\nlet run () =\n    ignore 101\n    printfn\"%s\"value\n    ignore 103\n    ()\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "adjacent-plain-string-crlf-eof",
        family: ArgumentTokenFamily::AdjacentPlainString,
        targets: &["printfn\"%s\"value"],
        source: "module ArgumentTokenExample\r\nlet run () =\r\n    ignore 101\r\n    printfn\"%s\"value\r\n    ignore 103\r\n    ()\r\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "adjacent-array-record-lf-eof",
        family: ArgumentTokenFamily::AdjacentArrayRecord,
        targets: &["combine[|1|]", "consume{ Value = 1 }"],
        source: "module ArgumentTokenExample\nlet run () =\n    ignore 101\n    combine[|1|]\n    consume{ Value = 1 }\n    ignore 103\n    ()\nlet following = 30",
    },
    ArgumentTokenSource {
        id: "adjacent-array-record-crlf-eof",
        family: ArgumentTokenFamily::AdjacentArrayRecord,
        targets: &["combine[|1|]", "consume{ Value = 1 }"],
        source: "module ArgumentTokenExample\r\nlet run () =\r\n    ignore 101\r\n    combine[|1|]\r\n    consume{ Value = 1 }\r\n    ignore 103\r\n    ()\r\nlet following = 30",
    },
];
