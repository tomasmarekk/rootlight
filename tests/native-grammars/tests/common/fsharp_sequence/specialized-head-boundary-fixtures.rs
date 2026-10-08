// Exact twelve specialized-head boundary sources qualified by ParseFile.
// The native fragment preserves source identity without equating FCS and native ASTs.

#[derive(Clone, Copy)]
enum SpecializedBoundaryDecision {
    Sibling,
    Continuation,
}

struct SpecializedBoundarySource {
    id: &'static str,
    family: SpecializedArgumentFamily,
    decision: SpecializedBoundaryDecision,
    atom: &'static str,
    source: &'static str,
}

const SPECIALIZED_BOUNDARY_SOURCES: [SpecializedBoundarySource; 12] = [
    SpecializedBoundarySource {
        id: "sibling-verbatim-string-lf-eof",
        family: SpecializedArgumentFamily::VerbatimString,
        decision: SpecializedBoundaryDecision::Sibling,
        atom: "@\"neutral\"",
        source: "module PendingArgumentHeadExample\nlet run value =\n    consume\n@\"neutral\"\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "continuation-verbatim-string-lf-eof",
        family: SpecializedArgumentFamily::VerbatimString,
        decision: SpecializedBoundaryDecision::Continuation,
        atom: "@\"neutral\"",
        source: "module PendingArgumentHeadExample\nlet run value =\n    consume\n        @\"neutral\"\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "sibling-verbatim-string-crlf-eof",
        family: SpecializedArgumentFamily::VerbatimString,
        decision: SpecializedBoundaryDecision::Sibling,
        atom: "@\"neutral\"",
        source: "module PendingArgumentHeadExample\r\nlet run value =\r\n    consume\r\n@\"neutral\"\r\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "continuation-verbatim-string-crlf-eof",
        family: SpecializedArgumentFamily::VerbatimString,
        decision: SpecializedBoundaryDecision::Continuation,
        atom: "@\"neutral\"",
        source: "module PendingArgumentHeadExample\r\nlet run value =\r\n    consume\r\n        @\"neutral\"\r\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "sibling-interpolated-string-lf-eof",
        family: SpecializedArgumentFamily::InterpolatedString,
        decision: SpecializedBoundaryDecision::Sibling,
        atom: "$\"neutral {value}\"",
        source: "module PendingArgumentHeadExample\nlet run value =\n    consume\n$\"neutral {value}\"\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "continuation-interpolated-string-lf-eof",
        family: SpecializedArgumentFamily::InterpolatedString,
        decision: SpecializedBoundaryDecision::Continuation,
        atom: "$\"neutral {value}\"",
        source: "module PendingArgumentHeadExample\nlet run value =\n    consume\n        $\"neutral {value}\"\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "sibling-interpolated-string-crlf-eof",
        family: SpecializedArgumentFamily::InterpolatedString,
        decision: SpecializedBoundaryDecision::Sibling,
        atom: "$\"neutral {value}\"",
        source: "module PendingArgumentHeadExample\r\nlet run value =\r\n    consume\r\n$\"neutral {value}\"\r\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "continuation-interpolated-string-crlf-eof",
        family: SpecializedArgumentFamily::InterpolatedString,
        decision: SpecializedBoundaryDecision::Continuation,
        atom: "$\"neutral {value}\"",
        source: "module PendingArgumentHeadExample\r\nlet run value =\r\n    consume\r\n        $\"neutral {value}\"\r\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "sibling-quotation-lf-eof",
        family: SpecializedArgumentFamily::Quotation,
        decision: SpecializedBoundaryDecision::Sibling,
        atom: "<@ value @>",
        source: "module PendingArgumentHeadExample\nlet run value =\n    consume\n<@ value @>\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "continuation-quotation-lf-eof",
        family: SpecializedArgumentFamily::Quotation,
        decision: SpecializedBoundaryDecision::Continuation,
        atom: "<@ value @>",
        source: "module PendingArgumentHeadExample\nlet run value =\n    consume\n        <@ value @>\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "sibling-quotation-crlf-eof",
        family: SpecializedArgumentFamily::Quotation,
        decision: SpecializedBoundaryDecision::Sibling,
        atom: "<@ value @>",
        source: "module PendingArgumentHeadExample\r\nlet run value =\r\n    consume\r\n<@ value @>\r\nlet following = 30",
    },
    SpecializedBoundarySource {
        id: "continuation-quotation-crlf-eof",
        family: SpecializedArgumentFamily::Quotation,
        decision: SpecializedBoundaryDecision::Continuation,
        atom: "<@ value @>",
        source: "module PendingArgumentHeadExample\r\nlet run value =\r\n    consume\r\n        <@ value @>\r\nlet following = 30",
    },
];
