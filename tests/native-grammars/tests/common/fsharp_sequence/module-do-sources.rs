// Exact compiler-observed module DO inputs for the private native fragment.
// Sources and cutoffs are copied without newline normalization from fixtures.json.

struct ModuleDoSource {
    id: &'static str,
    nested: bool,
    source: &'static str,
    first_semicolon: usize,
    following_start: usize,
}

const MODULE_DO_SOURCES: [ModuleDoSource; 8] = [
    ModuleDoSource {
        id: "module-do:single:lf:eof",
        nested: false,
        source: "module DoTerminatorExample\ndo\n    ignore 11;\nlet following = 30",
        first_semicolon: 43,
        following_start: 45,
    },
    ModuleDoSource {
        id: "module-do:single:crlf:eof",
        nested: false,
        source: "module DoTerminatorExample\r\ndo\r\n    ignore 11;\r\nlet following = 30",
        first_semicolon: 45,
        following_start: 48,
    },
    ModuleDoSource {
        id: "module-do:double:lf:eof",
        nested: false,
        source: "module DoTerminatorExample\ndo\n    ignore 11;;\nlet following = 30",
        first_semicolon: 43,
        following_start: 46,
    },
    ModuleDoSource {
        id: "module-do:double:crlf:eof",
        nested: false,
        source: "module DoTerminatorExample\r\ndo\r\n    ignore 11;;\r\nlet following = 30",
        first_semicolon: 45,
        following_start: 49,
    },
    ModuleDoSource {
        id: "nested-module-do:single:lf:eof",
        nested: true,
        source: "module DoTerminatorExample\ndo\n    do\n        ignore 11;\nlet following = 30",
        first_semicolon: 54,
        following_start: 56,
    },
    ModuleDoSource {
        id: "nested-module-do:single:crlf:eof",
        nested: true,
        source: "module DoTerminatorExample\r\ndo\r\n    do\r\n        ignore 11;\r\nlet following = 30",
        first_semicolon: 57,
        following_start: 60,
    },
    ModuleDoSource {
        id: "nested-module-do:double:lf:eof",
        nested: true,
        source: "module DoTerminatorExample\ndo\n    do\n        ignore 11;;\nlet following = 30",
        first_semicolon: 54,
        following_start: 57,
    },
    ModuleDoSource {
        id: "nested-module-do:double:crlf:eof",
        nested: true,
        source: "module DoTerminatorExample\r\ndo\r\n    do\r\n        ignore 11;;\r\nlet following = 30",
        first_semicolon: 57,
        following_start: 61,
    },
];
