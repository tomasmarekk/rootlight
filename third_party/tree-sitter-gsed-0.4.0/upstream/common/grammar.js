export const symbols = [
  "group_open",
  "group_close",
  "alternation",
  "quantifier",
  "interval_open",
  "interval_close",
  "interval_comma",
  "count",
  "bracket_open",
  "bracket_close",
  "bracket_negation",
  "range_dash",
  "pending_range_dash",
  "range_continuation",
  "regex_dot",
  "regex_anchor",
  "invalid_regex_operator",
  "unmatched_group_close",
  "orphan_interval_open",
  "invalid_range_operator",
  "incomplete_range_operator",
  "invalid_interval_character",
  "class_open",
  "collating_open",
  "equivalence_open",
  "term_open_marker",
  "term_close_marker",
  "term_close",
  "misplaced_class_start",
  "term_text",
];

// Escaped delimiters cannot form these backslash-only symbols.
const quotedSymbols = [
  "regex_boundary",
  "regex_class_escape",
  "regex_backreference",
  "invalid_interval_syntax",
  "incomplete_interval_syntax",
];

export const tokens = [
  "error_sentinel",
  ...symbols.flatMap((name) => [name, `${name}_encoded`, `${name}_escaped`]),
  ...quotedSymbols.flatMap((name) => [name, `${name}_encoded`]),
  "blank",
  "separator",
  "command_end",
  "missing_separator",
  "end_of_input",
  "boundary",
  "block_close",
  "nul_command",
  "comment",
  "directive",
  "comment_tail",
  "comment_end",
  "opaque_end",
  "number",
  "zero_number",
  "pending_zero",
  "invalid_zero",
  "address_gap",
  "first_address_gap",
  "address_modifier",
  "regex_start",
  "invalid_address_prefix",
  "pattern_start",
  "translation_start",
  "invalid_delimiter",
  "regex_end",
  "pattern_end",
  "replacement_end",
  "translation_middle",
  "translation_end",
  "unterminated_regex_eof",
  "unterminated_regex_line",
  "unterminated_operand_eof",
  "unterminated_operand_line",
  "regex_character",
  "regex_escape",
  "regex_character_escape",
  "bracket_character",
  "bracket_escape",
  "bracket_character_escape",
  "utf8_start",
  "utf8_continuation",
  "incomplete_regex_operator_encoded",
  "incomplete_group_close_encoded",
  "interval_nonempty",
  "interval_empty",
  "interval_empty_eof",
  "incomplete_interval_character_encoded",
  "incomplete_interval_character_escaped",
  "term_nonempty",
  "class_valid",
  "class_invalid",
  "class_incomplete",
  "literal",
  "escape",
  "character_escape",
  "line_continuation",
  "backreference",
  "match_reference",
  "case_conversion",
  "text_start",
  "inline_text_start",
  "shell_start",
  "inline_shell_start",
  "text_end",
  "filename",
  "label",
  "version",
  "empty_label",
  "empty_filename",
  "incomplete_escape",
  "invalid_escape",
  "invalid_control_escape",
  "incomplete_control_escape",
  "invalid_encoding",
  "global_flag",
  "print_flag",
  "evaluate_flag",
  "match_flag",
  "write_flag",
  "occurrence",
  "zero_occurrence",
  "unknown_flag",
  "pending_flag",
];

const GLOBAL_FLAG = 1;
const PRINT_FLAG = 2;
const OCCURRENCE_FLAG = 4;

const countedFlags = [
  { token: "global_flag", type: "flag", bit: GLOBAL_FLAG },
  { token: "print_flag", type: "flag", bit: PRINT_FLAG },
  { token: "occurrence", type: "number", bit: OCCURRENCE_FLAG },
];

const issue = ($, id, outcome = "invalid") =>
  field("issue", alias($[`_${outcome}_${id}_issue`], $.syntax_issue));
const invalidOrIncomplete = ($, id) =>
  choice(issue($, id), issue($, id, "incomplete"));
const leaf = ($, token, name) => alias($[`_${token}`], $[name]);
const quotedSymbol = ($, name, type) =>
  choice(leaf($, name, type), leaf($, `${name}_encoded`, "character_escape"));
const symbol = ($, name, type) =>
  choice(quotedSymbol($, name, type), leaf($, `${name}_escaped`, "escape"));

const issueDefinitions = [
  [
    "invalid_interval_syntax",
    "invalid",
    ($) => quotedSymbol($, "invalid_interval_syntax", "escape"),
  ],
  [
    "invalid_interval_syntax",
    "incomplete",
    ($) => quotedSymbol($, "incomplete_interval_syntax", "escape"),
  ],
  [
    "empty_filename",
    "invalid",
    ($) => leaf($, "empty_filename", "filename_text"),
  ],
  [
    "invalid_control_escape",
    "invalid",
    ($) => leaf($, "invalid_control_escape", "character_escape"),
  ],
  [
    "invalid_control_escape",
    "incomplete",
    ($) => leaf($, "incomplete_control_escape", "character_escape"),
  ],
  [
    "invalid_relative_address",
    "invalid",
    ($) => field("count", leaf($, "number", "number")),
  ],
  ["empty_label", "invalid", ($) => leaf($, "empty_label", "label_text")],
  [
    "first_address_gap",
    "invalid",
    ($) => $._first_address_gap,
    "missing_address",
  ],
  ["invalid_zero_address", "invalid", ($) => leaf($, "invalid_zero", "number")],
  [
    "misplaced_zero_address",
    "invalid",
    ($) => leaf($, "zero_number", "number"),
    "invalid_zero_address",
  ],
  [
    "invalid_zero_address",
    "incomplete",
    ($) => leaf($, "pending_zero", "number"),
  ],
  ["excess_range", "invalid", ($) => $._address_tail, "excess_address"],
  [
    "forbidden_address",
    "invalid",
    ($) => field("address", $.address_clause),
    "excess_address",
  ],
  ["unknown_substitution_flag", "invalid", ($) => $._unknown_flag],
  ["unknown_substitution_flag", "incomplete", ($) => $._pending_flag],
  ...countedFlags.map(({ token, type }) => [
    `duplicate_${token}`,
    "invalid",
    ($) => leaf($, token, type),
    "duplicate_substitution_flag",
  ]),
  ["zero_occurrence", "invalid", ($) => leaf($, "zero_occurrence", "number")],
  ["zero_occurrence", "incomplete", ($) => leaf($, "pending_zero", "number")],
  [
    "duplicate_zero_occurrence",
    "invalid",
    ($) => invalidOrIncomplete($, "zero_occurrence"),
    "duplicate_substitution_flag",
  ],
  ["modifier_on_empty_regexp", "invalid", ($) => leaf($, "match_flag", "flag")],
  [
    "address_modifier_on_empty_regexp",
    "invalid",
    ($) => leaf($, "address_modifier", "address_modifier"),
    "modifier_on_empty_regexp",
  ],
  [
    "unknown_character_class",
    "invalid",
    ($) => seq($._class_invalid, alias($._class_payload, $.class_name)),
  ],
  [
    "unknown_character_class",
    "incomplete",
    ($) => seq($._class_incomplete, alias($._class_payload, $.class_name)),
  ],
  [
    "empty_interval",
    "invalid",
    ($) => $._interval_empty,
    "missing_interval_count",
  ],
  [
    "empty_interval",
    "incomplete",
    ($) => $._interval_empty_eof,
    "missing_interval_count",
  ],
  ["unmatched_closing_brace", "invalid", () => "}"],
  ["duplicate_negation", "invalid", () => "!"],
  [
    "unknown_command",
    "invalid",
    ($) => choice($._unknown_command, $._nul_command),
  ],
  ["missing_command_separator", "invalid", ($) => $._missing_separator],
  ["invalid_delimiter", "invalid", ($) => $._invalid_delimiter],
  [
    "invalid_interval_character",
    "invalid",
    ($) =>
      prec.right(repeat1(symbol($, "invalid_interval_character", "literal"))),
  ],
  [
    "invalid_interval_character",
    "incomplete",
    ($) =>
      choice(
        leaf($, "incomplete_interval_character_encoded", "character_escape"),
        leaf($, "incomplete_interval_character_escaped", "escape"),
      ),
  ],
  [
    "invalid_regex_operator",
    "invalid",
    ($) => symbol($, "invalid_regex_operator", "regex_operator"),
  ],
  [
    "invalid_regex_operator",
    "incomplete",
    ($) => leaf($, "incomplete_regex_operator_encoded", "character_escape"),
  ],
  [
    "unmatched_group_close",
    "invalid",
    ($) => symbol($, "unmatched_group_close", "group_close"),
  ],
  [
    "unmatched_group_close",
    "incomplete",
    ($) => leaf($, "incomplete_group_close_encoded", "character_escape"),
  ],
  [
    "orphan_interval",
    "invalid",
    ($) => symbol($, "orphan_interval_open", "interval_open"),
    "invalid_regex_operator",
  ],
  ["invalid_encoding", "invalid", ($) => $._invalid_encoding],
  [
    "misplaced_character_class",
    "invalid",
    ($) =>
      seq(
        symbol($, "misplaced_class_start", "compound_marker"),
        alias($._class_payload, $.class_name),
        symbol($, "term_close_marker", "compound_marker"),
      ),
  ],
  ...["invalid", "incomplete"].map((outcome) => [
    "invalid_range_endpoint",
    outcome,
    ($) => choice($.character_class, $.equivalence_class),
  ]),
  [
    "invalid_range_operator",
    "invalid",
    ($) => symbol($, "invalid_range_operator", "range_operator"),
  ],
  [
    "invalid_range_operator",
    "incomplete",
    ($) => symbol($, "incomplete_range_operator", "range_operator"),
  ],
  ["missing_escape_character", "incomplete", ($) => $._incomplete_escape],
  ["missing_escape_character", "invalid", ($) => $._invalid_escape],
  ["unterminated_regex", "incomplete", ($) => $._unterminated_regex_eof],
  ["unterminated_regex", "invalid", ($) => $._unterminated_regex_line],
  ["unterminated_operand", "incomplete", ($) => $._unterminated_operand_eof],
  ["unterminated_operand", "invalid", ($) => $._unterminated_operand_line],
  ...[
    "missing_command",
    "missing_operand",
    "missing_address",
    "missing_closing_delimiter",
    "missing_opening_delimiter",
    "missing_bracket_element",
  ].flatMap((reason) => [
    [reason, "incomplete", ($) => $._end_of_input],
    [
      reason,
      "invalid",
      ($) =>
        reason === "missing_address"
          ? choice($._boundary, $._address_gap)
          : $._boundary,
    ],
  ]),
];

function defineIssueRules(definitions) {
  const rules = {};
  for (const [id, outcome, rule, reason = id] of definitions) {
    const name = `_${outcome}_${id}`;
    rules[`${name}_reason`] = rule;
    rules[`${name}_outcome`] = ($) => alias($[`${name}_reason`], $[reason]);
    rules[`${name}_issue`] = ($) =>
      alias($[`${name}_outcome`], $[`${outcome}_syntax`]);
  }
  return rules;
}

const verb = ($, ...spellings) =>
  field(
    "name",
    alias(
      spellings.length === 1 ? spellings[0] : choice(...spellings),
      $.command_name,
    ),
  );
const commandItems = ($) => [
  $.command,
  $.command_separator,
  $._command_layout,
  $.comment,
  issue($, "invalid_encoding"),
];
const addressOrNegation = ($, address) =>
  choice(
    seq(
      address,
      optional(
        seq(optional($._command_decode_issues), field("negation", $.negation)),
      ),
    ),
    field("negation", $.negation),
  );
const excess = ($) => repeat(issue($, "excess_range"));
const step = ($, number) =>
  optional(
    seq(alias("~", $.address_operator), optional(field("step", number))),
  );
const relative = ($, count) =>
  prec.right(seq(alias(choice("+", "~"), $.address_operator), optional(count)));
const closingDelimiter = ($, token, reason) =>
  choice(leaf($, token, "delimiter"), invalidOrIncomplete($, reason));
const close = ($, name, type) =>
  choice(
    symbol($, name, type),
    invalidOrIncomplete($, "missing_closing_delimiter"),
  );
const compoundClose = ($, type) =>
  choice(
    seq(
      symbol($, "term_close_marker", "compound_marker"),
      close($, "term_close", type),
    ),
    invalidOrIncomplete($, "missing_closing_delimiter"),
  );
const fileOperand = ($) =>
  choice($.filename, invalidOrIncomplete($, "missing_operand"));
const fileCommand = ($, ...spellings) =>
  seq(verb($, ...spellings), field("file", fileOperand($)));
const argumentCommand = ($, ...spellings) =>
  seq(verb($, ...spellings), optional(field("argument", $.number)));
const opaqueText = ($, token, name) =>
  choice(leaf($, token, name), issue($, "invalid_encoding"));
const opaque = ($, token, name) =>
  seq(repeat1(opaqueText($, token, name)), $._opaque_end);
const requiredOpaque = ($, token, name) =>
  seq(
    choice(
      leaf($, token, name),
      issue($, `empty_${token}`),
      issue($, "invalid_encoding"),
    ),
    repeat(opaqueText($, token, name)),
    $._opaque_end,
  );
const commentLine = ($, token, name) =>
  seq(
    leaf($, token, name),
    repeat(opaqueText($, "comment_tail", "comment_text")),
    $._comment_end,
  );
const textOperand = ($, kind, body) =>
  seq(
    choice(
      leaf($, `${kind}_start`, "text_introducer"),
      $[`_inline_${kind}_start`],
    ),
    optional(body),
    $._text_end,
  );
const delimiterIssues = ($) => [
  invalidOrIncomplete($, "missing_opening_delimiter"),
  issue($, "invalid_delimiter"),
  issue($, "invalid_encoding"),
];
const escapeIssues = ($) => [
  invalidOrIncomplete($, "invalid_control_escape"),
  invalidOrIncomplete($, "missing_escape_character"),
  issue($, "invalid_encoding"),
];
const rangeEndpoint = ($, outcome) =>
  choice(
    $.bracket_character,
    $.collating_symbol,
    issue($, "invalid_range_endpoint", outcome),
    ...escapeIssues($),
  );
const characterForms = ($, prefix) => [
  leaf($, `${prefix}_character`, "literal"),
  leaf($, `${prefix}_escape`, "escape"),
  leaf($, `${prefix}_character_escape`, "character_escape"),
  seq(
    leaf($, "utf8_start", "character_escape"),
    repeat(leaf($, "utf8_continuation", "character_escape")),
  ),
];
const termPayload = ($, name) =>
  repeat1(choice(symbol($, "term_text", name), ...escapeIssues($)));
const compoundTerm = ($, opening, closing, name, content) =>
  seq(
    symbol($, opening, opening),
    symbol($, "term_open_marker", "compound_marker"),
    field(name, content),
    compoundClose($, closing),
  );
const elementTerm = ($, opening, closing) =>
  compoundTerm(
    $,
    opening,
    closing,
    "element",
    choice(
      seq($._term_nonempty, $.collating_element),
      invalidOrIncomplete($, "missing_bracket_element"),
    ),
  );
const substitutionTail = ($, flags) => [
  field("separator", closingDelimiter($, "pattern_end", "unterminated_regex")),
  optional($.replacement),
  field(
    "closing",
    closingDelimiter($, "replacement_end", "unterminated_operand"),
  ),
  optional(alias(flags, $.substitution_flags)),
];

function flagRules() {
  const rules = {};
  for (const empty of [0, 1]) {
    for (let seen = 0; seen < 8; seen++) {
      rules[`_flags_${empty}_${seen}`] = ($) => {
        const then = (next) => optional($[`_flags_${empty}_${next}`]);
        return prec.right(
          choice(
            ...countedFlags.map(({ token, type, bit }) =>
              seq(
                seen & bit
                  ? issue($, `duplicate_${token}`)
                  : leaf($, token, type),
                then(seen | bit),
              ),
            ),
            seq(leaf($, "evaluate_flag", "flag"), then(seen & ~PRINT_FLAG)),
            seq(
              empty
                ? issue($, "modifier_on_empty_regexp")
                : leaf($, "match_flag", "flag"),
              then(seen),
            ),
            seq(
              seen & OCCURRENCE_FLAG
                ? issue($, "duplicate_zero_occurrence")
                : invalidOrIncomplete($, "zero_occurrence"),
              then(seen | OCCURRENCE_FLAG),
            ),
            seq(
              choice(
                invalidOrIncomplete($, "unknown_substitution_flag"),
                issue($, "invalid_encoding"),
              ),
              then(seen),
            ),
            $.write_flag,
          ),
        );
      };
    }
  }
  return rules;
}

export default function defineGrammar(name) {
  return grammar({
    name,
    extras: ($) => [$._blank, /[ \t]/],
    conflicts: ($) => [
      [$.command_list],
      [$._zero_line_address, $._invalid_misplaced_zero_address_reason],
      [$.command, $._invalid_forbidden_address_reason],
      [$.address_clause, $._single_address_clause],
      [$.address_clause, $._invalid_excess_range_reason],
    ],
    externals: ($) => tokens.map((token) => $[`_${token}`]),

    rules: {
      script: ($) => optional(alias($._root_commands, $.command_list)),
      _root_commands: ($) => {
        const item = choice(
          ...commandItems($),
          issue($, "unmatched_closing_brace"),
        );
        return choice(seq($.directive, repeat(item)), repeat1(item));
      },
      command_list: ($) => repeat1(choice(...commandItems($))),
      command_separator: ($) => $._separator,
      _command_layout: () => token(prec(1, choice("\r", "\v", "\f"))),
      command: ($) => {
        const prefix = (rule) => seq(rule, optional($._command_decode_issues));
        const negation = optional(prefix(field("negation", $.negation)));
        return choice(
          seq(
            optional(prefix(field("address", $.address_clause))),
            negation,
            $._function,
          ),
          seq(
            optional(
              prefix(
                field(
                  "address",
                  alias($._single_address_clause, $.address_clause),
                ),
              ),
            ),
            negation,
            alias($._quit_command, $.numeric_command),
            $._command_ending,
          ),
          seq(
            optional(prefix(issue($, "forbidden_address"))),
            negation,
            $.label_command,
          ),
          prec.dynamic(
            1,
            seq(
              prefix(addressOrNegation($, issue($, "forbidden_address"))),
              $.comment,
            ),
          ),
          seq(
            prefix(
              field("address", alias($._read_zero_clause, $.address_clause)),
            ),
            alias($._read_command, $.file_command),
            $._command_ending,
          ),
          seq(
            addressOrNegation($, field("address", $.address_clause)),
            choice(
              seq(invalidOrIncomplete($, "missing_command"), $._command_ending),
              seq($._command_decode_issues, $._command_end),
            ),
          ),
        );
      },
      _command_ending: ($) =>
        seq(
          optional($._command_decode_issues),
          choice($._command_end, issue($, "missing_command_separator")),
        ),
      _command_decode_issues: ($) => repeat1(issue($, "invalid_encoding")),
      address_clause: ($) =>
        choice(
          seq($._first_address, optional(seq($._address_tail, excess($)))),
          seq(issue($, "first_address_gap"), $._address_tail, excess($)),
          seq(
            alias($._zero_address, $.address),
            alias(",", $.address_separator),
            alias($._regex_only_address, $.address),
            excess($),
          ),
        ),
      _single_address_clause: ($) =>
        choice(
          seq(
            choice(
              $._first_address,
              alias($._misplaced_zero_address, $.address),
            ),
            excess($),
          ),
          seq(issue($, "first_address_gap"), repeat1(issue($, "excess_range"))),
        ),
      _first_address: ($) =>
        choice($.address, alias($._first_relative_address, $.relative_address)),
      _first_relative_address: ($) =>
        relative(
          $,
          choice(
            field("count", leaf($, "zero_number", "number")),
            issue($, "invalid_relative_address"),
          ),
        ),
      _read_zero_clause: ($) => alias($._zero_address, $.address),
      _misplaced_zero_address: ($) =>
        alias($._misplaced_zero_line_address, $.line_address),
      _misplaced_zero_line_address: ($) =>
        seq(
          issue($, "misplaced_zero_address"),
          step($, leaf($, "zero_number", "number")),
        ),
      _address_tail: ($) =>
        seq(
          alias(",", $.address_separator),
          choice(
            $.address,
            $.relative_address,
            invalidOrIncomplete($, "missing_address"),
          ),
        ),
      address: ($) =>
        choice($.line_address, $.last_line_address, $.regex_address),
      _regex_only_address: ($) => $.regex_address,
      _zero_address: ($) => alias($._zero_line_address, $.line_address),
      _zero_line_address: ($) =>
        seq(
          field("first", leaf($, "zero_number", "number")),
          step($, leaf($, "zero_number", "number")),
        ),
      line_address: ($) =>
        choice(
          seq(field("first", leaf($, "number", "number")), step($, $.number)),
          seq(
            invalidOrIncomplete($, "invalid_zero_address"),
            step($, leaf($, "zero_number", "number")),
          ),
        ),
      last_line_address: () => "$",
      relative_address: ($) => relative($, field("count", $.number)),
      number: ($) => choice($._number, $._zero_number),
      negation: ($) => seq("!", repeat(issue($, "duplicate_negation"))),
      regex_address: ($) => {
        const closing = field(
          "closing",
          closingDelimiter($, "regex_end", "unterminated_regex"),
        );
        return choice(
          seq(
            field("opening", leaf($, "invalid_address_prefix", "delimiter")),
            choice(...delimiterIssues($)),
          ),
          seq(
            field("opening", leaf($, "regex_start", "delimiter")),
            choice(
              seq($.regular_expression, closing, repeat($.address_modifier)),
              seq(
                closing,
                repeat(issue($, "address_modifier_on_empty_regexp")),
              ),
            ),
          ),
        );
      },
      address_modifier: ($) => $._address_modifier,
      _function: ($) =>
        choice(
          $.branch_command,
          $.version_command,
          seq(
            choice(
              $.simple_command,
              $.block,
              $.substitute_command,
              $.translate_command,
              $.text_command,
              $.file_command,
              $.execute_command,
              $.numeric_command,
              issue($, "unknown_command"),
            ),
            $._command_ending,
          ),
        ),
      simple_command: ($) =>
        verb(
          $,
          "d",
          "D",
          "g",
          "G",
          "h",
          "H",
          "n",
          "N",
          "p",
          "P",
          "x",
          "=",
          "F",
          "z",
        ),
      numeric_command: ($) => argumentCommand($, "l"),
      _quit_command: ($) => argumentCommand($, "q", "Q"),
      version_command: ($) =>
        prec.right(seq(verb($, "v"), optional(field("version", $.version)))),
      version: ($) => opaque($, "version", "version_text"),
      block: ($) =>
        seq(
          field("opening", "{"),
          optional($.command_list),
          choice(
            prec.dynamic(
              2,
              seq(
                addressOrNegation($, issue($, "forbidden_address")),
                field("closing", alias($._block_close, "}")),
              ),
            ),
            field(
              "closing",
              choice(
                alias($._block_close, "}"),
                issue($, "missing_closing_delimiter", "incomplete"),
              ),
            ),
          ),
        ),
      branch_command: ($) =>
        prec.right(
          seq(verb($, "b", "t", "T"), optional(field("label", $.label))),
        ),
      label_command: ($) =>
        seq(
          verb($, ":"),
          field(
            "label",
            choice(
              alias($._required_label, $.label),
              invalidOrIncomplete($, "missing_operand"),
            ),
          ),
        ),
      file_command: ($) => fileCommand($, "r", "R", "w", "W"),
      _read_command: ($) => fileCommand($, "r"),
      label: ($) => opaque($, "label", "label_text"),
      _required_label: ($) => requiredOpaque($, "label", "label_text"),
      filename: ($) => requiredOpaque($, "filename", "filename_text"),
      shell_command: ($) =>
        repeat1(choice(leaf($, "literal", "shell_text"), $._operand_escape)),
      execute_command: ($) =>
        seq(
          verb($, "e"),
          optional(textOperand($, "shell", field("command", $.shell_command))),
        ),
      text_command: ($) =>
        seq(
          verb($, "a", "c", "i"),
          choice(
            textOperand($, "text", $.text),
            invalidOrIncomplete($, "missing_operand"),
          ),
        ),
      text: ($) => repeat1($._operand_piece),
      substitute_command: ($) =>
        prec.right(
          seq(
            verb($, "s"),
            choice(
              seq(
                field("opening", leaf($, "pattern_start", "delimiter")),
                choice(
                  seq(
                    $.regular_expression,
                    ...substitutionTail($, $._flags_0_0),
                  ),
                  seq(...substitutionTail($, $._flags_1_0)),
                ),
              ),
              ...delimiterIssues($),
            ),
          ),
        ),
      write_flag: ($) => seq(leaf($, "write_flag", "flag"), fileOperand($)),
      replacement: ($) =>
        repeat1(
          choice(
            $._operand_piece,
            leaf($, "backreference", "replacement_reference"),
            leaf($, "match_reference", "matched_text_reference"),
            leaf($, "case_conversion", "case_conversion"),
          ),
        ),
      translate_command: ($) =>
        seq(
          verb($, "y"),
          choice(
            seq(
              field("opening", leaf($, "translation_start", "delimiter")),
              optional(field("source", $.translation_string)),
              field(
                "separator",
                closingDelimiter(
                  $,
                  "translation_middle",
                  "unterminated_operand",
                ),
              ),
              optional(field("destination", $.translation_string)),
              field(
                "closing",
                closingDelimiter($, "translation_end", "unterminated_operand"),
              ),
            ),
            ...delimiterIssues($),
          ),
        ),
      translation_string: ($) => repeat1($._operand_piece),
      _operand_piece: ($) =>
        choice(leaf($, "literal", "literal"), $._operand_escape),
      _operand_escape: ($) =>
        choice(
          leaf($, "escape", "escape"),
          leaf($, "character_escape", "character_escape"),
          leaf($, "line_continuation", "line_continuation"),
          ...escapeIssues($),
        ),
      comment: ($) => commentLine($, "comment", "comment_text"),
      directive: ($) => commentLine($, "directive", "directive_text"),
      regular_expression: ($) => $.alternation,
      alternation: ($) =>
        choice(
          $.regex_branch,
          seq(
            optional($.regex_branch),
            repeat1(
              seq(
                symbol($, "alternation", "regex_operator"),
                optional($.regex_branch),
              ),
            ),
          ),
        ),
      regex_branch: ($) => repeat1(choice($.repetition, $._regex_atom)),
      repetition: ($) =>
        prec.right(
          1,
          seq(
            field("expression", $._regex_atom),
            repeat1(
              field(
                "operator",
                choice(symbol($, "quantifier", "regex_operator"), $.interval),
              ),
            ),
          ),
        ),
      _regex_atom: ($) =>
        choice(
          $.regex_character,
          $.wildcard,
          $.anchor,
          $.word_boundary,
          $.regex_character_class,
          $.backreference,
          $.group,
          $.bracket_expression,
          alias($._orphan_interval, $.interval),
          invalidOrIncomplete($, "invalid_regex_operator"),
          invalidOrIncomplete($, "unmatched_group_close"),
          ...escapeIssues($),
        ),
      regex_character: ($) =>
        choice(
          invalidOrIncomplete($, "invalid_interval_syntax"),
          ...characterForms($, "regex"),
        ),
      wildcard: ($) => symbol($, "regex_dot", "regex_operator"),
      anchor: ($) => symbol($, "regex_anchor", "regex_operator"),
      word_boundary: ($) => quotedSymbol($, "regex_boundary", "regex_operator"),
      regex_character_class: ($) =>
        quotedSymbol($, "regex_class_escape", "regex_operator"),
      backreference: ($) =>
        quotedSymbol($, "regex_backreference", "regex_reference"),
      group: ($) =>
        seq(
          field("opening", symbol($, "group_open", "group_open")),
          optional($.regular_expression),
          field("closing", close($, "group_close", "group_close")),
        ),
      interval_count: ($) => prec.right(repeat1(symbol($, "count", "number"))),
      interval: ($) =>
        seq(
          field("opening", symbol($, "interval_open", "interval_open")),
          $._interval_body,
        ),
      _orphan_interval: ($) =>
        seq(issue($, "orphan_interval"), $._interval_body),
      _interval_issue: ($) =>
        choice(
          invalidOrIncomplete($, "invalid_interval_character"),
          ...escapeIssues($),
        ),
      _interval_body: ($) => {
        const counts = (name) =>
          repeat(choice(field(name, $.interval_count), $._interval_issue));
        return seq(
          choice(
            $._interval_nonempty,
            issue($, "empty_interval"),
            issue($, "empty_interval", "incomplete"),
          ),
          counts("minimum"),
          optional(
            seq(
              symbol($, "interval_comma", "interval_separator"),
              counts("maximum"),
            ),
          ),
          field("closing", close($, "interval_close", "interval_close")),
        );
      },
      bracket_expression: ($) =>
        seq(
          field("opening", symbol($, "bracket_open", "bracket_open")),
          optional(symbol($, "bracket_negation", "regex_operator")),
          choice(
            repeat1(choice($.range, $._bracket_atom)),
            issue($, "misplaced_character_class"),
            invalidOrIncomplete($, "missing_bracket_element"),
          ),
          field("closing", close($, "bracket_close", "bracket_close")),
        ),
      _bracket_atom: ($) =>
        choice(
          $.bracket_character,
          $.character_class,
          $.collating_symbol,
          $.equivalence_class,
          ...escapeIssues($),
        ),
      bracket_character: ($) => choice(...characterForms($, "bracket")),
      range: ($) => {
        const end = field(
          "end",
          choice(
            $._range_endpoint,
            invalidOrIncomplete($, "missing_bracket_element"),
          ),
        );
        return prec.right(
          1,
          seq(
            choice(
              seq(
                field("start", $._range_endpoint),
                symbol($, "range_dash", "range_operator"),
              ),
              seq(
                field("start", $._pending_range_start),
                symbol($, "pending_range_dash", "range_operator"),
              ),
            ),
            end,
            repeat(
              seq(
                choice(
                  symbol($, "range_continuation", "range_operator"),
                  invalidOrIncomplete($, "invalid_range_operator"),
                ),
                end,
              ),
            ),
          ),
        );
      },
      _range_endpoint: ($) => rangeEndpoint($, "invalid"),
      _pending_range_start: ($) => rangeEndpoint($, "incomplete"),
      _class_payload: ($) => termPayload($, "class_name_text"),
      collating_element: ($) => termPayload($, "collating_text"),
      character_class: ($) =>
        compoundTerm(
          $,
          "class_open",
          "class_close",
          "name",
          choice(
            seq($._class_valid, alias($._class_payload, $.class_name)),
            issue($, "unknown_character_class"),
            issue($, "unknown_character_class", "incomplete"),
            invalidOrIncomplete($, "missing_bracket_element"),
          ),
        ),
      collating_symbol: ($) =>
        elementTerm($, "collating_open", "collating_close"),
      equivalence_class: ($) =>
        elementTerm($, "equivalence_open", "equivalence_close"),
      _unknown_command: () => /[^ \t\n;{}abcdeFgGhHilnNpPqQrRstTvwWxyz=:]/,
      ...flagRules(),
      ...defineIssueRules(issueDefinitions),
    },
  });
}
