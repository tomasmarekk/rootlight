// Neutral shared-name consumers are prospective syntax inputs, not compiler proof.
// Each LF/CRLF source ends at the independent following binding; changed bodies
// drive one bounded edit/restoration through the unchanged native helpers.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SharedConsumerFamily {
    QualifiedPattern,
    Mutation,
    DotMember,
}

struct SharedConsumerSource {
    id: &'static str,
    family: SharedConsumerFamily,
    head: &'static str,
    source: &'static str,
    body: &'static str,
    changed_body: &'static str,
}

const SHARED_CONSUMER_SOURCES: &[SharedConsumerSource] = &[
    SharedConsumerSource {
        id: "qualified-pattern:lf:eof",
        family: SharedConsumerFamily::QualifiedPattern,
        head: "classify value",
        source: "module SharedConsumerExample\nlet classify value =\n    match value with\n    | Choice.Item payload -> payload\n    | _ -> 0\nlet following = 30",
        body: "match value with\n    | Choice.Item payload -> payload\n    | _ -> 0",
        changed_body: "match value with\n    | Choice.Item payload -> payload\n    | _ -> 1",
    },
    SharedConsumerSource {
        id: "qualified-pattern:crlf:eof",
        family: SharedConsumerFamily::QualifiedPattern,
        head: "classify value",
        source: "module SharedConsumerExample\r\nlet classify value =\r\n    match value with\r\n    | Choice.Item payload -> payload\r\n    | _ -> 0\r\nlet following = 30",
        body: "match value with\r\n    | Choice.Item payload -> payload\r\n    | _ -> 0",
        changed_body: "match value with\r\n    | Choice.Item payload -> payload\r\n    | _ -> 1",
    },
    SharedConsumerSource {
        id: "mutation:lf:eof",
        family: SharedConsumerFamily::Mutation,
        head: "update input",
        source: "module SharedConsumerExample\nlet update input =\n    current <- input\nlet following = 30",
        body: "current <- input",
        changed_body: "current <- input2",
    },
    SharedConsumerSource {
        id: "mutation:crlf:eof",
        family: SharedConsumerFamily::Mutation,
        head: "update input",
        source: "module SharedConsumerExample\r\nlet update input =\r\n    current <- input\r\nlet following = 30",
        body: "current <- input",
        changed_body: "current <- input2",
    },
    SharedConsumerSource {
        id: "dot-member:lf:eof",
        family: SharedConsumerFamily::DotMember,
        head: "read value",
        source: "module SharedConsumerExample\nlet read value =\n    (value).Member\nlet following = 30",
        body: "(value).Member",
        changed_body: "(value).Member2",
    },
    SharedConsumerSource {
        id: "dot-member:crlf:eof",
        family: SharedConsumerFamily::DotMember,
        head: "read value",
        source: "module SharedConsumerExample\r\nlet read value =\r\n    (value).Member\r\nlet following = 30",
        body: "(value).Member",
        changed_body: "(value).Member2",
    },
];
