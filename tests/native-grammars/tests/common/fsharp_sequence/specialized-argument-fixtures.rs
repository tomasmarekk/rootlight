// Exact specialized argument sources shared with the frozen ParseFile oracle.
// This private fixture fragment is included after the unchanged frontier helpers.

#[derive(Clone, Copy)]
enum SpecializedArgumentFamily {
    VerbatimString,
    InterpolatedString,
    Quotation,
}

struct SpecializedArgumentSource {
    id: &'static str,
    family: SpecializedArgumentFamily,
    source: &'static str,
}

const SPECIALIZED_ARGUMENT_SOURCES: [SpecializedArgumentSource; 6] = [
    SpecializedArgumentSource {
        id: "pending-verbatim-string-lf-eof",
        family: SpecializedArgumentFamily::VerbatimString,
        source: "module PendingArgumentHeadExample\nlet run value =\n    consume @\"neutral\"\nlet following = 30",
    },
    SpecializedArgumentSource {
        id: "pending-verbatim-string-crlf-eof",
        family: SpecializedArgumentFamily::VerbatimString,
        source: "module PendingArgumentHeadExample\r\nlet run value =\r\n    consume @\"neutral\"\r\nlet following = 30",
    },
    SpecializedArgumentSource {
        id: "pending-interpolated-string-lf-eof",
        family: SpecializedArgumentFamily::InterpolatedString,
        source: "module PendingArgumentHeadExample\nlet run value =\n    consume $\"neutral {value}\"\nlet following = 30",
    },
    SpecializedArgumentSource {
        id: "pending-interpolated-string-crlf-eof",
        family: SpecializedArgumentFamily::InterpolatedString,
        source: "module PendingArgumentHeadExample\r\nlet run value =\r\n    consume $\"neutral {value}\"\r\nlet following = 30",
    },
    SpecializedArgumentSource {
        id: "pending-quotation-lf-eof",
        family: SpecializedArgumentFamily::Quotation,
        source: "module PendingArgumentHeadExample\nlet run value =\n    consume <@ value @>\nlet following = 30",
    },
    SpecializedArgumentSource {
        id: "pending-quotation-crlf-eof",
        family: SpecializedArgumentFamily::Quotation,
        source: "module PendingArgumentHeadExample\r\nlet run value =\r\n    consume <@ value @>\r\nlet following = 30",
    },
];
